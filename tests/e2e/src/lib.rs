//! End-to-end harness that spawns the **real `ctx` binary** in isolated
//! temp dirs. This is how we know the whole system actually works (§18):
//! convergence, PITR, relay, auth, and genuine git-coherence are all proven
//! against the shipped CLI, not in-process shortcuts.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tempfile::TempDir;

/// Locate (building if needed) the `ctx` binary.
pub fn ctx_bin() -> PathBuf {
    if let Ok(p) = std::env::var("CTX_BIN") {
        return PathBuf::from(p);
    }
    let manifest = env!("CARGO_MANIFEST_DIR");
    let target = Path::new(manifest)
        .ancestors()
        .nth(2)
        .unwrap()
        .join("target/debug/ctx");
    if !target.exists() {
        let st = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "ctx"])
            .status()
            .expect("cargo build -p ctx");
        assert!(st.success(), "failed to build ctx");
    }
    target
}

/// One running `ctx` node bound to its own vault dir + isolated HOME (so
/// each node has its own device identity at `$HOME/.context/id_ed25519`).
pub struct Peer {
    pub name: String,
    pub dir: TempDir,
    pub home: TempDir,
    proc: Option<Child>,
    pub port: Option<u16>,
    identity_override: Option<PathBuf>,
    /// When false (default) the harness runs plaintext (`--no-tls` + `ws://`)
    /// for speed/determinism; the dedicated TLS test opts in to the real
    /// `wss://` default path.
    use_tls: bool,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Peer {
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Force this peer to use a specific identity key file (e.g. to share
    /// one NodeId across two vault replicas — §5.1 same-NodeId case).
    pub fn set_identity(&mut self, path: PathBuf) {
        self.identity_override = Some(path);
    }

    /// Opt this peer into the real `wss://` default path (no `--no-tls`).
    pub fn enable_tls(&mut self) {
        self.use_tls = true;
    }

    /// The scheme this peer's listener serves / connectors should dial.
    pub fn scheme(&self) -> &'static str {
        if self.use_tls {
            "wss"
        } else {
            "ws"
        }
    }

    /// URL to reach this peer's listener (call after `start_watch`).
    pub fn url(&self) -> String {
        format!("{}://127.0.0.1:{}", self.scheme(), self.port.unwrap())
    }

    fn base_cmd(&self) -> Command {
        let mut c = Command::new(ctx_bin());
        c.env("HOME", self.home.path());
        c.env("CTX_DIR", self.dir.path());
        c.env("CTX_LOG", "ctx=info,csp_core=warn");
        if let Some(idp) = &self.identity_override {
            c.arg("--identity").arg(idp);
        }
        c.kill_on_drop(true);
        c
    }

    /// Run a one-shot `ctx` subcommand to completion, returning stdout.
    pub async fn run(&self, args: &[&str]) -> Result<String> {
        let out = self.base_cmd().args(args).output().await?;
        if !out.status.success() {
            bail!(
                "ctx {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub async fn run_allow_fail(&self, args: &[&str]) -> (bool, String, String) {
        let out = self.base_cmd().args(args).output().await.unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    pub async fn pubkey(&self) -> Result<String> {
        Ok(self.run(&["key"]).await?.trim().to_string())
    }

    pub async fn authorize(&self, ssh: &str) -> Result<()> {
        self.run(&["authorize", ssh]).await.map(|_| ())
    }

    pub async fn main_sha(&self) -> Result<String> {
        let json = self.run(&["status", "--json"]).await?;
        let v: serde_json::Value = serde_json::from_str(&json)?;
        Ok(v["main"].as_str().unwrap_or_default().to_string())
    }

    pub fn write(&self, rel: &str, content: &str) {
        let p = self.dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join(rel)).ok()
    }

    pub fn delete(&self, rel: &str) {
        let _ = std::fs::remove_file(self.dir.path().join(rel));
    }

    /// Atomic filesystem rename of `from` → `to` (relative to the peer's
    /// vault root). Auto-creates the destination parent. This is the path
    /// the `ctx watch` notify-based watcher sees as a real move — different
    /// from `write(new) + delete(old)` because the OS emits proper rename
    /// events.
    pub fn rename(&self, from: &str, to: &str) {
        let src = self.dir.path().join(from);
        let dst = self.dir.path().join(to);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::rename(src, dst).expect("rename");
    }

    /// Recursive rename of a directory (same semantics as `mv old/ new/`).
    /// Auto-creates the destination parent.
    pub fn rename_dir(&self, from: &str, to: &str) {
        let src = self.dir.path().join(from);
        let dst = self.dir.path().join(to);
        if let Some(parent) = dst.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::rename(src, dst).expect("rename_dir");
    }

    /// Recursive directory delete (`rm -rf`).
    pub fn delete_dir(&self, rel: &str) {
        let _ = std::fs::remove_dir_all(self.dir.path().join(rel));
    }

    /// Create a directory tree.
    pub fn mkdir(&self, rel: &str) {
        std::fs::create_dir_all(self.dir.path().join(rel)).expect("mkdir");
    }

    /// Does a path exist on this peer's working tree?
    pub fn exists(&self, rel: &str) -> bool {
        self.dir.path().join(rel).exists()
    }

    pub fn stderr_dump(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }

    /// Start the long-running `ctx watch` daemon. `listen` → act as a
    /// listener/relay; `peers` → connect outward.
    pub async fn start_watch(&mut self, listen: bool, peers: &[String]) -> Result<()> {
        self.start_watch_with(listen, peers, &[]).await
    }

    /// Like [`start_watch`] but with extra `ctx watch` flags (e.g.
    /// `--no-tofu`, `--authorized-keys …`).
    pub async fn start_watch_with(
        &mut self,
        listen: bool,
        peers: &[String],
        extra: &[&str],
    ) -> Result<()> {
        let mut c = self.base_cmd();
        c.arg("watch").arg("--debounce-ms").arg("250");
        if !self.use_tls {
            // Default suite runs plaintext for speed/determinism; the
            // dedicated TLS test opts in via `enable_tls()`.
            c.arg("--no-tls");
        }
        if listen {
            c.arg("--listen").arg("127.0.0.1:0");
        }
        for p in peers {
            c.arg("--peer").arg(p);
        }
        for e in extra {
            c.arg(e);
        }
        let mut child = c
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn ctx watch")?;

        let stderr = Arc::new(Mutex::new(Vec::new()));
        if listen {
            self.port = Some(read_listen_port(&mut child).await?);
        }
        drain(&mut child, &self.name, stderr.clone());
        self.stderr = stderr;
        self.proc = Some(child);
        Ok(())
    }

    /// Spawn an arbitrary long-running `ctx` command as a daemon (e.g.
    /// `clone … --watch`). Captures stderr; killed on drop / `stop`.
    pub async fn spawn_daemon(&mut self, args: &[&str]) -> Result<()> {
        let mut child = self
            .base_cmd()
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn ctx daemon")?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        drain(&mut child, &self.name, stderr.clone());
        self.stderr = stderr;
        self.proc = Some(child);
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(mut c) = self.proc.take() {
            let _ = c.kill().await;
        }
    }
}

fn drain(child: &mut Child, name: &str, sink: Arc<Mutex<Vec<String>>>) {
    if let Some(err) = child.stderr.take() {
        let name = name.to_string();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if std::env::var("CSP_E2E_VERBOSE").is_ok() {
                    eprintln!("[{name}] {l}");
                }
                sink.lock().unwrap().push(l);
            }
        });
    }
    if let Some(out) = child.stdout.take() {
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(_l)) = lines.next_line().await {}
        });
    }
}

async fn read_listen_port(child: &mut Child) -> Result<u16> {
    let err = child.stderr.as_mut().context("no stderr")?;
    let mut lines = BufReader::new(err).lines();
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(15), lines.next_line()).await {
            Ok(Ok(Some(l))) => {
                // `listening on <scheme>://127.0.0.1:<port>`
                if l.contains("listening on") {
                    if let Some(idx) = l.find("127.0.0.1:") {
                        let digits: String = l[idx + "127.0.0.1:".len()..]
                            .chars()
                            .take_while(|c| c.is_ascii_digit())
                            .collect();
                        if let Ok(p) = digits.parse() {
                            return Ok(p);
                        }
                    }
                }
            }
            _ => break,
        }
    }
    bail!("listener never reported a port")
}

/// A scenario: build peers, wire identities, drive `ctx`.
pub struct Scenario {
    pub vault_id: String,
    pub peers: Vec<Peer>,
}

impl Scenario {
    pub fn new(vault_id: &str) -> Self {
        Scenario {
            vault_id: vault_id.to_string(),
            peers: Vec::new(),
        }
    }

    /// Create a peer **without** running `ctx init` (so the caller can
    /// `set_identity` first). Returns its index.
    pub fn add_uninit(&mut self, name: &str) -> Result<usize> {
        let p = Peer {
            name: name.to_string(),
            dir: TempDir::new()?,
            home: TempDir::new()?,
            proc: None,
            port: None,
            identity_override: None,
            use_tls: false,
            stderr: Arc::new(Mutex::new(Vec::new())),
        };
        self.peers.push(p);
        Ok(self.peers.len() - 1)
    }

    /// Create + `ctx init` a fresh peer.
    pub async fn add(&mut self, name: &str) -> Result<usize> {
        let p = Peer {
            name: name.to_string(),
            dir: TempDir::new()?,
            home: TempDir::new()?,
            proc: None,
            port: None,
            identity_override: None,
            use_tls: false,
            stderr: Arc::new(Mutex::new(Vec::new())),
        };
        p.run(&["init", "--vault-id", &self.vault_id]).await?;
        self.peers.push(p);
        Ok(self.peers.len() - 1)
    }

    /// Bootstrap a brand-new peer purely via `ctx clone <url> .` (no
    /// `init`) — the real device-onboarding path (§17). Returns its index.
    pub async fn add_clone(&mut self, name: &str, url: &str) -> Result<usize> {
        let p = Peer {
            name: name.to_string(),
            dir: TempDir::new()?,
            home: TempDir::new()?,
            proc: None,
            port: None,
            identity_override: None,
            use_tls: url.starts_with("wss://"),
            stderr: Arc::new(Mutex::new(Vec::new())),
        };
        p.run(&["clone", url, "."]).await?;
        self.peers.push(p);
        Ok(self.peers.len() - 1)
    }

    /// Make every peer trust every other peer (mutual authorization, §10).
    pub async fn mutual_authorize(&self) -> Result<()> {
        let mut keys = Vec::new();
        for p in &self.peers {
            keys.push(p.pubkey().await?);
        }
        for (i, p) in self.peers.iter().enumerate() {
            for (j, k) in keys.iter().enumerate() {
                if i != j {
                    p.authorize(k).await?;
                }
            }
        }
        Ok(())
    }

    pub fn peer(&self, i: usize) -> &Peer {
        &self.peers[i]
    }
    pub fn peer_mut(&mut self, i: usize) -> &mut Peer {
        &mut self.peers[i]
    }
}

/// Poll until `check` is true or timeout. Returns whether it succeeded.
pub async fn wait_until<F>(timeout: Duration, mut check: F) -> bool
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if check() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Wait until a file on `peer` has exactly `want`.
pub async fn wait_for_content(peer: &Peer, rel: &str, want: &str, timeout: Duration) -> bool {
    wait_until(timeout, || peer.read(rel).as_deref() == Some(want)).await
}

pub async fn wait_for_missing(peer: &Peer, rel: &str, timeout: Duration) -> bool {
    wait_until(timeout, || peer.read(rel).is_none()).await
}

/// Wait until every listed peer reports the same non-empty `main` SHA.
pub async fn wait_for_convergence(peers: &[&Peer], timeout: Duration) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut shas = Vec::new();
        for p in peers {
            shas.push(p.main_sha().await.unwrap_or_default());
        }
        if !shas[0].is_empty() && shas.iter().all(|s| s == &shas[0]) {
            return Some(shas[0].clone());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    None
}
