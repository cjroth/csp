//! `ctx` — the Context Sync Protocol CLI (§17). A thin wrapper over
//! `csp-core`: argument parsing, process lifecycle, the filesystem watcher,
//! the listen socket. No protocol logic lives here.

mod cli;
mod gitpass;
mod idstore;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser};
use cli::{Cli, Cmd, ScopeAction};
use csp_core::net::{probe, Node};
use csp_core::Vault;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn root_dir(cli: &Cli) -> PathBuf {
    cli.dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()))
}

/// A filesystem-safe slug for folder/display use: keep `[A-Za-z0-9._-]`,
/// collapse anything else to `-`, trim. Empty if nothing usable.
fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_matches(['-', '.']).to_string()
}

/// Short, readable folder name for an opaque id (`vault-<8 hex/uuid>`).
fn short_id(vault_id: &str) -> String {
    let s = slug(vault_id);
    let head: String = s.chars().take(8).collect();
    format!("vault-{}", if head.is_empty() { "x" } else { &head })
}

/// Git-spirit name derivation: the scope directory's basename, unless it is
/// degenerate (`.`/`/`/home dir) — then empty (display falls back to id).
fn derive_name(root: &Path) -> String {
    let abs = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let home = std::env::var("HOME").ok();
    if home.as_deref() == abs.to_str() {
        return String::new();
    }
    match abs.file_name().and_then(|n| n.to_str()) {
        Some(n) if !n.is_empty() && n != "." && n != "/" => slug(n),
        _ => String::new(),
    }
}

fn seed_authorized(v: &Vault, spec: &Option<String>) -> Result<()> {
    // `--authorized-keys`/`CTX_AUTHORIZED_KEYS`: keys or a file path, merged
    // idempotently so the TOFU window never opens (§10/§17.1).
    let Some(spec) = spec else { return Ok(()) };
    let body = if Path::new(spec).exists() {
        std::fs::read_to_string(spec)?
    } else {
        spec.replace(',', "\n")
    };
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        v.authorize(line)?;
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // Operator-visible by default: connection, handshake, catch-up,
    // integrate, and commit events all log at INFO (override with
    // `--log`/`CTX_LOG`, e.g. `csp_core=debug`).
    let filter = cli
        .log
        .clone()
        .unwrap_or_else(|| "ctx=info,csp_core=info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
    if let Err(e) = run(cli).await {
        eprintln!("ctx: error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let root = root_dir(&cli);
    match &cli.cmd {
        Cmd::Init {
            path,
            vault_id,
            name,
            authorized_keys,
            watch,
        } => {
            // Positional `[path]` is the most explicit form, so it wins
            // over the global `--dir`/`CTX_CWD` (CLI precedence is
            // flag/positional > env > config, §17.1); created if missing,
            // git-init style. Omitted → the resolved `root` (--dir/env/cwd).
            let root = path.clone().unwrap_or(root);
            let (id, idpath) = idstore::load_or_create(cli.identity.as_deref())?;
            std::fs::create_dir_all(&root)?;
            // Opaque id: a fresh UUID by default (not derived from the node
            // key — it must not leak identity and is a pure equality guard).
            let vid = vault_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            // Human name: explicit, else the scope directory's basename
            // (git-spirit: the folder is the name), else empty.
            let nm = name.clone().unwrap_or_else(|| derive_name(&root));
            let mut v = Vault::create(&root, id.clone(), &vid)
                .context("create vault (already a vault here?)")?;
            v.set_name(&nm)?;
            seed_authorized(&v, authorized_keys)?;
            drop(v);
            println!(
                "initialized vault {} ({}) at {}",
                if nm.is_empty() { "<unnamed>" } else { &nm },
                vid,
                root.display()
            );
            println!("identity: {} ({})", id.to_ssh_string(), idpath.display());
            if *watch {
                let (id2, _) = idstore::load_or_create(cli.identity.as_deref())?;
                watch_run(
                    root.clone(),
                    id2,
                    None,
                    None,
                    false,
                    false,
                    None,
                    Vec::new(),
                    1000,
                    false,
                )
                .await?;
            }
        }

        Cmd::Key => {
            let (id, idpath) = idstore::load_or_create(cli.identity.as_deref())?;
            println!("{}", id.to_ssh_string());
            eprintln!("(key file: {})", idpath.display());
        }

        Cmd::Authorize { pubkey } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            let v = Vault::open(&root, id)?;
            v.authorize(pubkey)?;
            println!("authorized {}", pubkey.split_whitespace().next().unwrap_or(pubkey));
        }
        Cmd::Revoke { pubkey } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            let v = Vault::open(&root, id)?;
            v.revoke(pubkey)?;
            println!("revoked");
        }

        Cmd::Status { json } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            let v = Vault::open(&root, id.clone())?;
            let main = v.main().map(|o| o.to_hex()).unwrap_or_default();
            let tips: Vec<String> = v.frontier_tips()?.iter().map(|o| o.to_hex()).collect();
            let known = v.known()?.len();
            let auth = v.authorized_node_ids()?.len();
            if *json {
                let obj = serde_json::json!({
                    "vault_id": v.vault_id(),
                    "name": v.name(),
                    "node_id": id.node_id().to_hex(),
                    "pubkey": id.to_ssh_string(),
                    "main": main,
                    "frontier_tips": tips,
                    "known_primitives": known,
                    "authorized_keys": auth,
                    "peers": v.config.peers,
                    "listen": v.config.listen,
                    "tier": v.config.tier,
                });
                println!("{}", serde_json::to_string_pretty(&obj)?);
            } else {
                println!("vault    {}", v.vault_id());
                if !v.name().is_empty() {
                    println!("name     {}", v.name());
                }
                println!("node     {}", id.node_id().to_hex());
                println!("main     {main}");
                println!("frontier {} tip(s)", tips.len());
                println!("known    {known} primitive(s)");
                println!("authorized {auth} key(s)  tier {}", v.config.tier);
            }
        }

        Cmd::Snapshot { name } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            let mut v = Vault::open(&root, id)?;
            v.snapshot(name)?;
            println!("snapshot '{name}' created");
        }
        Cmd::Restore { target } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            let mut v = Vault::open(&root, id)?;
            if v.snapshots().contains_key(target) {
                v.restore_snapshot(target)?;
                println!("restored snapshot '{target}'");
            } else if let Ok(t) = target.parse::<u64>() {
                v.restore_time(t)?;
                println!("restored to time {t}");
            } else {
                anyhow::bail!("no snapshot '{target}' and not a unix time");
            }
        }

        Cmd::Log { args } => {
            let gd = root.join(".context/git");
            let mut a = vec!["log".to_string()];
            a.extend(args.clone());
            let code = gitpass::run(&gd, &root, &a)?;
            std::process::exit(code);
        }
        Cmd::Git { args } => {
            let gd = root.join(".context/git");
            let code = gitpass::run(&gd, &root, args)?;
            std::process::exit(code);
        }

        Cmd::Scope { action } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            let mut v = Vault::open(&root, id)?;
            match action {
                None => {
                    let (inc, ig) = v.scope_summary();
                    println!("include:");
                    for i in inc {
                        println!("  {i}");
                    }
                    println!(".contextignore:");
                    for g in ig {
                        println!("  {g}");
                    }
                }
                Some(ScopeAction::Ignore { pattern }) => {
                    v.add_ignore_pattern(pattern)?;
                    println!("ignoring '{pattern}' (synced)");
                }
                Some(ScopeAction::Include { pattern }) => {
                    v.add_include_pattern(pattern)?;
                    println!("included '{pattern}'");
                }
            }
        }

        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "ctx", &mut std::io::stdout());
        }

        Cmd::Clone {
            url,
            into,
            authorized_keys,
            watch,
        } => {
            let (id, _idpath) = idstore::load_or_create(cli.identity.as_deref())?;
            let (vault_id, vault_name, server_ssh) = probe(url, &id)
                .await
                .context("probe listener for vault id")?;
            anyhow::ensure!(!vault_id.is_empty(), "listener returned empty vault id");
            // Default folder: ./<name-or-short-id>/. `.` → current dir; an
            // explicit path → that path. Never the raw opaque id.
            let default_dir = if vault_name.is_empty() {
                short_id(&vault_id)
            } else {
                slug(&vault_name)
            };
            let target = match into.as_deref() {
                None => root.join(&default_dir),
                Some(d) if d == std::path::Path::new(".") => root.clone(),
                Some(d) if d.is_absolute() => d.to_path_buf(),
                Some(d) => root.join(d),
            };
            anyhow::ensure!(
                !target.join(".context").exists(),
                "{} is already a CSP vault (refusing to clobber)",
                target.display()
            );
            if into.is_none() {
                anyhow::ensure!(
                    !target.exists()
                        || std::fs::read_dir(&target)
                            .map(|mut e| e.next().is_none())
                            .unwrap_or(true),
                    "{} already exists and is not empty — pass an explicit dir or `.`",
                    target.display()
                );
            }
            std::fs::create_dir_all(&target)?;
            let label = if vault_name.is_empty() {
                vault_id.clone()
            } else {
                vault_name.clone()
            };
            tracing::info!("cloning '{label}' → {}", target.display());
            let mut v = Vault::create(&target, id.clone(), &vault_id)?;
            v.set_name(&vault_name)?;
            v.authorize(&server_ssh)?; // trust the bootstrap source (§10)
            seed_authorized(&v, authorized_keys)?;
            // Remember where we cloned from — like git's `origin`. A bare
            // `ctx watch` in this vault then reconnects automatically.
            if !v.config.peers.iter().any(|p| p == url) {
                v.config.peers.push(url.clone());
                v.save_config()?;
            }
            drop(v);
            tracing::info!("saved origin {url} (config)");

            if *watch {
                println!("Cloned '{label}' into {}.", target.display());
                println!("Watching origin {url} (Ctrl-C to stop)…");
                watch_run(
                    target.clone(),
                    id,
                    None,
                    None,
                    false,
                    false,
                    None,
                    Vec::new(),
                    1000,
                    false,
                )
                .await?;
            } else {
                // Bounded catch-up so `ctx clone` returns with content
                // (git-clone semantics), then exit.
                let node = Node::new(Vault::open(&target, id)?);
                let _conn = node.connect(url.clone());
                let start = Instant::now();
                let mut last = None;
                let mut stable_since = Instant::now();
                loop {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    let m = node.vault.lock().await.main();
                    if m != last {
                        last = m;
                        stable_since = Instant::now();
                    }
                    let settled = stable_since.elapsed() > Duration::from_millis(1200);
                    if (last.is_some()
                        && settled
                        && start.elapsed() > Duration::from_secs(1))
                        || start.elapsed() > Duration::from_secs(25)
                    {
                        break;
                    }
                }
                node.vault.lock().await.materialize().ok();
                println!("Cloned '{label}' into {}.", target.display());
                println!(
                    "  Next:  cd {} && ctx watch",
                    target.display()
                );
            }
        }

        Cmd::Watch {
            listen,
            port,
            no_tls,
            no_tofu,
            authorized_keys,
            peer,
            debounce_ms,
            once,
        } => {
            let (id, _) = idstore::load_or_create(cli.identity.as_deref())?;
            watch_run(
                root.clone(),
                id,
                listen.clone(),
                *port,
                *no_tls,
                *no_tofu,
                authorized_keys.clone(),
                peer.clone(),
                *debounce_ms,
                *once,
            )
            .await?;
        }
    }
    Ok(())
}

/// The long-running sync daemon (`ctx watch`, and `--watch` on
/// init/clone). Opens the vault, optionally listens, connects to configured
/// + extra peers, runs the debounced watcher until Ctrl-C.
#[allow(clippy::too_many_arguments)]
async fn watch_run(
    root: PathBuf,
    id: csp_core::Identity,
    listen: Option<String>,
    port: Option<u16>,
    no_tls: bool,
    no_tofu: bool,
    authorized_keys: Option<String>,
    extra_peers: Vec<String>,
    debounce_ms: u64,
    once: bool,
) -> Result<()> {
    let mut v = Vault::open(&root, id).context("open vault (run `ctx init`?)")?;
    if no_tofu {
        v.config.no_tofu = true;
        v.save_config()?;
    }
    seed_authorized(&v, &authorized_keys)?;
    // §7 HARD INVARIANT: only full nodes may listen.
    if listen.is_some() && v.config.tier == "thin" {
        anyhow::bail!("a thin node must not listen/relay");
    }
    let mut peers: Vec<String> = v.config.peers.clone();
    peers.extend(extra_peers);
    let context_dir = v.context_dir().to_path_buf();
    let label = if v.name().is_empty() {
        v.vault_id().to_string()
    } else {
        format!("{} ({})", v.name(), v.vault_id())
    };
    tracing::info!(
        "vault {label} (tier {}, node {}…) — watching {}",
        v.config.tier,
        &v.node_id().to_hex()[..12],
        root.display()
    );

    let node = Node::new(v);

    if let Some(addr) = &listen {
        let mut bind: std::net::SocketAddr =
            addr.parse().context("parse --listen addr")?;
        // `--port` / `PORT` overrides the address's port (§17.1).
        if let Some(p) = port {
            bind.set_port(p);
        }
        // Default: wss with a self-signed cert (§10/§17.1) — trust is the
        // ed25519 handshake, not a CA. `--no-tls` → plaintext ws (behind a
        // TLS-terminating proxy, or local/trusted).
        let (tls_cfg, scheme) = if no_tls {
            (None, "ws")
        } else {
            let (cert, key) = csp_core::tls::load_or_generate(&context_dir)?;
            let fp = csp_core::tls::cert_fingerprint(&cert);
            (Some((csp_core::tls::server_config(cert, key)?, fp)), "wss")
        };
        let (bound, _h) = node.serve(bind, tls_cfg).await?;
        // One line, on stderr: keeps stdout clean for `--json` / scripting;
        // the e2e harness parses this line for the port.
        eprintln!("listening on {scheme}://{bound}");
        tracing::info!("listening on {scheme}://{bound} (relay enabled)");
    }

    if peers.is_empty() && listen.is_none() {
        tracing::warn!(
            "no peers and not listening — this node will only commit \
             locally. Add `--peer <url>` or `--listen`."
        );
    }
    for p in &peers {
        let _ = node.connect(p.clone());
    }

    if once {
        // One bounded sync pass for deterministic scripting/tests.
        node.commit_and_publish().await.ok();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        node.commit_and_publish().await.ok();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        return Ok(());
    }

    // Establish the watcher first, then the initial reconcile (§5.6) picks
    // up pre-existing edits.
    spawn_watcher(node.clone(), root.clone(), debounce_ms);
    node.commit_and_publish().await.ok();

    tokio::signal::ctrl_c().await.ok();
    tracing::info!("shutting down");
    Ok(())
}

/// Filesystem watcher with debounced auto-commit and §5.6 self-write
/// suppression (the suppression is content-hash based inside the engine; the
/// watcher only needs to debounce and ignore the `.context/` subtree).
fn spawn_watcher(node: Node, root: PathBuf, debounce_ms: u64) {
    use notify::{RecursiveMode, Watcher};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let root_for_filter = root.clone();
    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let touches_scope = ev.paths.iter().any(|p| {
                let rel = p.strip_prefix(&root_for_filter).unwrap_or(p);
                let s = rel.to_string_lossy();
                !(s == ".context" || s.starts_with(".context/"))
            });
            if touches_scope {
                let _ = tx.send(());
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("watcher init failed: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        tracing::error!("watch {} failed: {e}", root.display());
        return;
    }
    tokio::spawn(async move {
        // Keep the watcher alive for the task's lifetime.
        let _watcher = watcher;
        let debounce = Duration::from_millis(debounce_ms);
        // Network propagation stays push-driven (§2: no polling). This
        // low-frequency *local* reconcile is only a safety net for
        // filesystem events inotify can drop (atomic rename saves, events
        // before the watch is established). Content-hash reconcile (§5.6)
        // makes a no-change tick a non-event, so it is cheap.
        let mut safety = tokio::time::interval(Duration::from_millis(1000));
        safety.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = safety.tick() => {}
                ev = rx.recv() => {
                    if ev.is_none() { break; }
                    // Debounce: coalesce a burst into one commit.
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep(debounce) => break,
                            more = rx.recv() => { if more.is_none() { return; } }
                        }
                    }
                }
            }
            match node.commit_and_publish().await {
                Ok(Some(p)) => tracing::info!("committed {}", &p[..12]),
                Ok(None) => {}
                Err(e) => tracing::warn!("commit failed: {e}"),
            }
        }
    });
}
