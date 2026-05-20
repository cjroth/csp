//! `CspEngine` — the real `Engine`, backed by native `csp-core`.
//!
//! Mirrors the `ctx` reference integration (init/clone/watch/status/
//! snapshot/restore/authorize) but manages N vaults in one process with a
//! per-vault `notify` watcher, listener, and a status poller that
//! materialises incoming changes. No stubs.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use csp_core::net::{probe, Node};
use csp_core::Identity as CoreIdentity;
use csp_core::Vault as CoreVault;

use crate::api::Engine;
use crate::events::EngineEvent;
use crate::types::*;

// ---------------- persisted app config (spec §7) ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Meta {
    id: String,
    path: String,
    display_name: String,
    enabled: bool,
    allow_connections: bool,
    port: u16,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Persist {
    #[serde(default)]
    vaults: Vec<Meta>,
    #[serde(default)]
    settings: AppSettings,
}

struct Managed {
    node: Node,
    tasks: Vec<JoinHandle<()>>,
    listener_handle: Option<JoinHandle<()>>,
    listener: Option<ListenerInfo>,
    last_commit: Arc<Mutex<Option<String>>>,
}

impl Managed {
    fn stop(&mut self) {
        for t in self.tasks.drain(..) {
            t.abort();
        }
        if let Some(h) = self.listener_handle.take() {
            h.abort();
        }
    }
}

struct Inner {
    metas: Vec<Meta>,
    settings: AppSettings,
    running: HashMap<String, Managed>,
}

pub struct CspEngine {
    identity: CoreIdentity,
    config_path: PathBuf,
    tx: broadcast::Sender<EngineEvent>,
    inner: Mutex<Inner>,
}

// ---------------- helpers ----------------

fn default_id_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".context").join("id_ed25519")
}

/// Device-global identity, byte-for-byte the `ctx idstore` scheme:
/// 32-byte ed25519 seed, hex, one line, 0600.
fn load_or_create_identity(explicit: Option<&Path>) -> EngineResult<CoreIdentity> {
    let path = explicit.map(|p| p.to_path_buf()).unwrap_or_else(default_id_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if path.exists() {
        let hex = std::fs::read_to_string(&path)?;
        let bytes = hex::decode(hex.trim())
            .map_err(|e| EngineError::io(format!("identity hex: {e}")))?;
        if bytes.len() != 32 {
            return Err(EngineError::io("identity must be a 32-byte seed"));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok(CoreIdentity::from_seed(&seed))
    } else {
        let id = CoreIdentity::generate();
        std::fs::write(&path, hex::encode(id.seed()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(id)
    }
}

fn lan_ip() -> String {
    use std::net::UdpSocket;
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".into())
}

fn basename(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "folder".into())
}

fn ssh_fingerprint(line: &str) -> String {
    let b64 = line.split_whitespace().nth(1).unwrap_or("");
    match base64::engine::general_purpose::STANDARD.decode(b64) {
        Ok(blob) => {
            let d = Sha256::digest(&blob);
            format!(
                "SHA256:{}",
                base64::engine::general_purpose::STANDARD_NO_PAD.encode(d)
            )
        }
        Err(_) => "SHA256:?".into(),
    }
}

fn rfc3339(unix: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(unix as i64, 0)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn authorized_keys_path(root: &str) -> PathBuf {
    Path::new(root).join(".context").join("authorized_keys")
}

fn read_authorized(root: &str) -> Vec<AuthorizedKey> {
    let p = authorized_keys_path(root);
    let body = std::fs::read_to_string(&p).unwrap_or_default();
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| AuthorizedKey {
            fingerprint: ssh_fingerprint(l),
            openssh: l.to_string(),
            comment: l.split_whitespace().nth(2).unwrap_or("").to_string(),
        })
        .collect()
}

// ---------------- background tasks ----------------

fn spawn_watcher(
    node: Node,
    root: PathBuf,
    debounce_ms: u64,
    tx: broadcast::Sender<EngineEvent>,
    id: String,
    last_commit: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    use notify::{RecursiveMode, Watcher};
    let (sig_tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let root_filter = root.clone();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let touches = ev.paths.iter().any(|p| {
                let rel = p.strip_prefix(&root_filter).unwrap_or(p);
                let s = rel.to_string_lossy();
                !(s == ".context" || s.starts_with(".context/"))
            });
            if touches {
                let _ = sig_tx.send(());
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(EngineEvent::Error {
                id: Some(id),
                message: format!("watcher init: {e}"),
            });
            return tokio::spawn(async {});
        }
    };
    if let Err(e) = watcher.watch(&root, RecursiveMode::Recursive) {
        let _ = tx.send(EngineEvent::Error {
            id: Some(id),
            message: format!("watch {}: {e}", root.display()),
        });
        return tokio::spawn(async {});
    }
    tokio::spawn(async move {
        let _watcher = watcher; // keep alive
        let debounce = Duration::from_millis(debounce_ms);
        let mut safety = tokio::time::interval(Duration::from_millis(1000));
        safety.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = safety.tick() => {}
                ev = rx.recv() => {
                    if ev.is_none() { break; }
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep(debounce) => break,
                            more = rx.recv() => { if more.is_none() { return; } }
                        }
                    }
                }
            }
            match node.commit_and_publish().await {
                Ok(Some(p)) => {
                    let short = p.chars().take(12).collect::<String>();
                    *last_commit.lock().await = Some(now_rfc3339());
                    let _ = tx.send(EngineEvent::Committed {
                        id: id.clone(),
                        short_sha: short,
                    });
                    let _ = tx.send(EngineEvent::StatusTick { id: id.clone() });
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = tx.send(EngineEvent::Error {
                        id: Some(id.clone()),
                        message: format!("commit: {e}"),
                    });
                }
            }
        }
    })
}

/// Polls the vault: on a `main` change it materialises incoming work to
/// disk (so remote edits appear) and emits a status nudge.
fn spawn_poller(
    node: Node,
    tx: broadcast::Sender<EngineEvent>,
    id: String,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_main: Option<String> = None;
        let mut tick = tokio::time::interval(Duration::from_millis(1200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let cur = {
                let mut v = node.vault.lock().await;
                let m = v.main().map(|o| o.to_hex());
                if m != last_main {
                    // New fold-commit (often from a peer) → write files.
                    let _ = v.materialize();
                }
                m
            };
            if cur != last_main {
                last_main = cur;
                let _ = tx.send(EngineEvent::StatusTick { id: id.clone() });
                let _ = tx.send(EngineEvent::AggregateTick {
                    state: SyncState::Active,
                });
            }
        }
    })
}

// ---------------- CspEngine ----------------

impl CspEngine {
    pub async fn new(
        app_config_dir: PathBuf,
        identity_path: Option<PathBuf>,
    ) -> EngineResult<Arc<Self>> {
        std::fs::create_dir_all(&app_config_dir)?;
        let identity = load_or_create_identity(identity_path.as_deref())?;
        let config_path = app_config_dir.join("config.json");
        let persist: Persist = std::fs::read_to_string(&config_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let (tx, _) = broadcast::channel(512);
        let me = Arc::new(Self {
            identity,
            config_path,
            tx,
            inner: Mutex::new(Inner {
                metas: persist.vaults.clone(),
                settings: persist.settings,
                running: HashMap::new(),
            }),
        });
        for m in persist.vaults.into_iter().filter(|m| m.enabled) {
            if let Err(e) = me.start_managed(&m).await {
                let _ = me.tx.send(EngineEvent::Error {
                    id: Some(m.id.clone()),
                    message: format!("start {}: {e}", m.display_name),
                });
            }
        }
        Ok(me)
    }

    async fn save(&self) -> EngineResult<()> {
        let inner = self.inner.lock().await;
        let p = Persist {
            vaults: inner.metas.clone(),
            settings: inner.settings.clone(),
        };
        let json = serde_json::to_string_pretty(&p)
            .map_err(|e| EngineError::io(e.to_string()))?;
        std::fs::write(&self.config_path, json)?;
        Ok(())
    }

    async fn meta(&self, id: &str) -> EngineResult<Meta> {
        self.inner
            .lock()
            .await
            .metas
            .iter()
            .find(|m| m.id == id)
            .cloned()
            .ok_or_else(|| EngineError::not_found(format!("no vault {id}")))
    }

    async fn start_managed(&self, m: &Meta) -> EngineResult<()> {
        let root = PathBuf::from(&m.path);
        let mut v = CoreVault::open(&root, self.identity.clone())?;
        // Decided policy: explicit-authorize, never silent trust (spec §13.2).
        if !v.config.no_tofu {
            v.config.no_tofu = true;
            v.save_config()?;
        }
        let peers = v.config.peers.clone();
        let node = Node::new(v);
        let last_commit = Arc::new(Mutex::new(None));
        let mut tasks = Vec::new();
        for p in peers {
            // Managed-vault outbound: no auth-key (enrollment already
            // happened at clone time; subsequent reconnects ride the
            // pubkey already in the peer's authorized_keys, §10).
            tasks.push(node.connect(p, None));
        }
        tasks.push(spawn_watcher(
            node.clone(),
            root.clone(),
            1000,
            self.tx.clone(),
            m.id.clone(),
            last_commit.clone(),
        ));
        tasks.push(spawn_poller(node.clone(), self.tx.clone(), m.id.clone()));
        // Initial reconcile (§5.6 picks up pre-existing edits).
        if let Ok(Some(p)) = node.commit_and_publish().await {
            *last_commit.lock().await = Some(now_rfc3339());
            let _ = self.tx.send(EngineEvent::Committed {
                id: m.id.clone(),
                short_sha: p.chars().take(12).collect(),
            });
        }
        self.inner.lock().await.running.insert(
            m.id.clone(),
            Managed {
                node,
                tasks,
                listener_handle: None,
                listener: None,
                last_commit,
            },
        );
        if m.allow_connections {
            self.start_listener(&m.id).await?;
        }
        let _ = self.tx.send(EngineEvent::StatusTick { id: m.id.clone() });
        Ok(())
    }

    async fn stop_managed(&self, id: &str) {
        if let Some(mut man) = self.inner.lock().await.running.remove(id) {
            man.stop();
        }
        let _ = self.tx.send(EngineEvent::StatusTick { id: id.to_string() });
    }

    async fn no_tls(&self) -> bool {
        self.inner.lock().await.settings.no_tls_by_default
    }

    async fn start_listener(&self, id: &str) -> EngineResult<ListenerInfo> {
        let meta = self.meta(id).await?;
        let context_dir = Path::new(&meta.path).join(".context");
        let (tls, scheme) = if self.no_tls().await {
            (None, "ws")
        } else {
            let (cert, key) = csp_core::tls::load_or_generate(&context_dir)?;
            let fp = csp_core::tls::cert_fingerprint(&cert);
            (Some((csp_core::tls::server_config(cert, key)?, fp)), "wss")
        };
        let bind: SocketAddr = format!("0.0.0.0:{}", meta.port)
            .parse()
            .map_err(|e| EngineError::network(format!("bind addr: {e}")))?;
        // Don't hold `inner` across the serve() await.
        let node = self
            .inner
            .lock()
            .await
            .running
            .get(id)
            .map(|m| m.node.clone())
            .ok_or_else(|| EngineError::not_found("vault not running"))?;
        let (bound, handle) = node
            .serve(bind, tls)
            .await
            .map_err(|e| EngineError::network(e.to_string()))?;
        let info = ListenerInfo {
            bound: true,
            scheme: scheme.into(),
            port: bound.port(),
            address: format!("{scheme}://{}:{}", lan_ip(), bound.port()),
        };
        {
            let mut inner = self.inner.lock().await;
            if let Some(man) = inner.running.get_mut(id) {
                man.listener_handle = Some(handle);
                man.listener = Some(info.clone());
            }
            // Pin the OS-assigned port so the address is stable across restarts.
            if let Some(mt) = inner.metas.iter_mut().find(|x| x.id == id) {
                mt.port = bound.port();
            }
        }
        self.save().await.ok();
        Ok(info)
    }

    async fn with_vault<R>(
        &self,
        id: &str,
        f: impl FnOnce(&mut CoreVault) -> EngineResult<R>,
    ) -> EngineResult<R> {
        let node = self.inner.lock().await.running.get(id).map(|m| m.node.clone());
        if let Some(node) = node {
            let mut g = node.vault.lock().await;
            f(&mut *g)
        } else {
            let meta = self.meta(id).await?;
            let mut v = CoreVault::open(Path::new(&meta.path), self.identity.clone())?;
            f(&mut v)
        }
    }

    async fn status_of(&self, m: &Meta) -> VaultStatus {
        let (main, known, frontier, authorized, listener, last_commit) = {
            let inner = self.inner.lock().await;
            if let Some(man) = inner.running.get(&m.id) {
                let v = man.node.vault.lock().await;
                (
                    v.main().map(|o| o.to_hex()),
                    v.known().map(|k| k.len()).unwrap_or(0) as u32,
                    v.frontier_tips().map(|f| f.len()).unwrap_or(0) as u32,
                    v.authorized_node_ids().map(|a| a.len()).unwrap_or(0) as u32,
                    man.listener.clone(),
                    man.last_commit.lock().await.clone(),
                )
            } else {
                drop(inner);
                match CoreVault::open(Path::new(&m.path), self.identity.clone()) {
                    Ok(v) => (
                        v.main().map(|o| o.to_hex()),
                        v.known().map(|k| k.len()).unwrap_or(0) as u32,
                        v.frontier_tips().map(|f| f.len()).unwrap_or(0) as u32,
                        v.authorized_node_ids().map(|a| a.len()).unwrap_or(0) as u32,
                        None,
                        None,
                    ),
                    Err(_) => (None, 0, 0, 0, None, None),
                }
            }
        };
        let state = if !m.enabled {
            SyncState::Disabled
        } else if main.is_some() {
            SyncState::Active
        } else {
            SyncState::Idle
        };
        VaultStatus {
            id: m.id.clone(),
            state,
            main_short_sha: main.map(|s| s.chars().take(12).collect()),
            known_count: known,
            frontier_count: frontier,
            authorized_count: authorized,
            listener,
            configured_peers: CoreVault::open(Path::new(&m.path), self.identity.clone())
                .map(|v| v.config.peers.clone())
                .unwrap_or_default(),
            last_commit,
        }
    }

    /// Force one commit+publish (used by integration tests for determinism).
    pub async fn commit_now(&self, id: &str) -> EngineResult<Option<String>> {
        let node = self
            .inner
            .lock()
            .await
            .running
            .get(id)
            .map(|m| m.node.clone())
            .ok_or_else(|| EngineError::not_found("vault not running"))?;
        node.commit_and_publish()
            .await
            .map_err(|e| EngineError::engine(e.to_string()))
    }
}

#[async_trait]
impl Engine for CspEngine {
    fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.tx.subscribe()
    }

    async fn list_vaults(&self) -> EngineResult<Vec<Vault>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .metas
            .iter()
            .map(|m| Vault {
                id: m.id.clone(),
                display_name: m.display_name.clone(),
                path: m.path.clone(),
                enabled: m.enabled,
                allow_connections: m.allow_connections,
                port: m.port,
                is_csp_vault: Path::new(&m.path).join(".context").exists(),
            })
            .collect())
    }

    async fn add_local_folder(&self, path: String) -> EngineResult<Vault> {
        let root = PathBuf::from(&path);
        std::fs::create_dir_all(&root)?;
        let is_csp = root.join(".context").exists();
        let name;
        if is_csp {
            let v = CoreVault::open(&root, self.identity.clone())?;
            let n = v.name().to_string();
            name = if n.is_empty() { basename(&path) } else { n };
        } else {
            let vault_id = uuid::Uuid::new_v4().to_string();
            name = basename(&path);
            let mut v = CoreVault::create(&root, self.identity.clone(), &vault_id)?;
            v.set_name(&name)?;
            v.config.no_tofu = true;
            v.save_config()?;
        }
        let meta = Meta {
            id: uuid::Uuid::new_v4().to_string(),
            path: path.clone(),
            display_name: name.clone(),
            enabled: true,
            allow_connections: false,
            port: 0,
        };
        self.inner.lock().await.metas.push(meta.clone());
        self.save().await?;
        self.start_managed(&meta).await?;
        let _ = self.tx.send(EngineEvent::VaultsChanged);
        Ok(Vault {
            id: meta.id,
            display_name: name,
            path,
            enabled: true,
            allow_connections: false,
            port: 0,
            is_csp_vault: is_csp,
        })
    }

    async fn clone_remote(
        &self,
        dest: String,
        url: String,
        auth_key: Option<String>,
    ) -> EngineResult<Vault> {
        let root = PathBuf::from(&dest);
        if root.join(".context").exists() {
            return Err(EngineError::engine(format!(
                "{dest} is already a CSP vault (refusing to clobber)"
            )));
        }
        let (vault_id, vname, server_ssh) =
            probe(&url, &self.identity, auth_key.as_deref())
                .await
                .map_err(|e| EngineError::network(format!("probe {url}: {e}")))?;
        std::fs::create_dir_all(&root)?;
        let name = if vname.is_empty() { basename(&dest) } else { vname };
        {
            let mut v = CoreVault::create(&root, self.identity.clone(), &vault_id)?;
            v.set_name(&name)?;
            v.authorize(&server_ssh)?;
            v.config.no_tofu = true;
            if !v.config.peers.iter().any(|p| p == &url) {
                v.config.peers.push(url.clone());
            }
            v.save_config()?;
        }
        let meta = Meta {
            id: uuid::Uuid::new_v4().to_string(),
            path: dest.clone(),
            display_name: name.clone(),
            enabled: true,
            allow_connections: false,
            port: 0,
        };
        self.inner.lock().await.metas.push(meta.clone());
        self.save().await?;
        self.start_managed(&meta).await?;
        let _ = self.tx.send(EngineEvent::VaultsChanged);
        Ok(Vault {
            id: meta.id,
            display_name: name,
            path: dest,
            enabled: true,
            allow_connections: false,
            port: 0,
            is_csp_vault: true,
        })
    }

    async fn remove_vault(&self, id: VaultId) -> EngineResult<()> {
        self.stop_managed(&id).await;
        {
            let mut inner = self.inner.lock().await;
            let before = inner.metas.len();
            inner.metas.retain(|m| m.id != id);
            if inner.metas.len() == before {
                return Err(EngineError::not_found(format!("no vault {id}")));
            }
        }
        self.save().await?;
        let _ = self.tx.send(EngineEvent::VaultsChanged);
        Ok(())
    }

    async fn set_enabled(&self, id: VaultId, on: bool) -> EngineResult<()> {
        let meta = {
            let mut inner = self.inner.lock().await;
            let m = inner
                .metas
                .iter_mut()
                .find(|m| m.id == id)
                .ok_or_else(|| EngineError::not_found(format!("no vault {id}")))?;
            m.enabled = on;
            m.clone()
        };
        self.save().await?;
        if on {
            if !self.inner.lock().await.running.contains_key(&id) {
                self.start_managed(&meta).await?;
            }
        } else {
            self.stop_managed(&id).await;
        }
        let _ = self.tx.send(EngineEvent::AggregateTick {
            state: SyncState::Idle,
        });
        Ok(())
    }

    async fn set_allow_connections(
        &self,
        id: VaultId,
        on: bool,
    ) -> EngineResult<ListenerInfo> {
        {
            let mut inner = self.inner.lock().await;
            let m = inner
                .metas
                .iter_mut()
                .find(|m| m.id == id)
                .ok_or_else(|| EngineError::not_found(format!("no vault {id}")))?;
            m.allow_connections = on;
        }
        self.save().await?;
        if on {
            let running = self.inner.lock().await.running.contains_key(&id);
            if !running {
                return Err(EngineError::unsupported(
                    "enable sync before allowing connections",
                ));
            }
            self.start_listener(&id).await
        } else {
            let scheme = {
                let mut inner = self.inner.lock().await;
                if let Some(man) = inner.running.get_mut(&id) {
                    if let Some(h) = man.listener_handle.take() {
                        h.abort();
                    }
                    man.listener = None;
                }
                if inner.settings.no_tls_by_default { "ws" } else { "wss" }
            };
            Ok(ListenerInfo {
                bound: false,
                scheme: scheme.into(),
                port: 0,
                address: String::new(),
            })
        }
    }

    async fn get_connect_address(&self, id: VaultId) -> EngineResult<ConnectAddress> {
        let meta = self.meta(&id).await?;
        let scheme = if self.no_tls().await { "ws" } else { "wss" };
        let ip = lan_ip();
        let port = {
            let inner = self.inner.lock().await;
            inner
                .running
                .get(&id)
                .and_then(|m| m.listener.as_ref().map(|l| l.port))
                .unwrap_or(meta.port)
        };
        let no_auth = read_authorized(&meta.path).is_empty();
        Ok(ConnectAddress {
            scheme: scheme.into(),
            lan_ip: ip.clone(),
            port,
            address: format!("{scheme}://{ip}:{port}"),
            firewall_guidance:
                "macOS: the Application Firewall is per-application, not per-port. \
On first listen, accept the OS prompt to allow incoming connections for \"Context \
Desktop\". If you previously denied it, re-enable it under System Settings → Network → \
Firewall. The port itself needs no separate macOS rule."
                    .into(),
            no_authorized_keys: no_auth,
            note: if no_auth {
                Some(
                    "No authorized keys yet. With trust-on-first-use disabled (the \
default), no peer can connect until you authorize its key below. Prefer LAN or a \
private overlay (VPN/Tailscale) over public port-forwarding."
                        .into(),
                )
            } else {
                None
            },
        })
    }

    async fn list_authorized(&self, id: VaultId) -> EngineResult<Vec<AuthorizedKey>> {
        let meta = self.meta(&id).await?;
        Ok(read_authorized(&meta.path))
    }

    async fn authorize(&self, id: VaultId, pubkey: String) -> EngineResult<()> {
        self.with_vault(&id, |v| {
            v.authorize(pubkey.trim())?;
            Ok(())
        })
        .await?;
        let _ = self.tx.send(EngineEvent::StatusTick { id });
        Ok(())
    }

    async fn revoke(&self, id: VaultId, fingerprint: String) -> EngineResult<()> {
        let meta = self.meta(&id).await?;
        let line = read_authorized(&meta.path)
            .into_iter()
            .find(|k| k.fingerprint == fingerprint)
            .map(|k| k.openssh)
            .ok_or_else(|| EngineError::not_found("no such authorized key"))?;
        self.with_vault(&id, |v| {
            v.revoke(&line)?;
            Ok(())
        })
        .await?;
        let _ = self.tx.send(EngineEvent::StatusTick { id });
        Ok(())
    }

    async fn get_identity(&self) -> EngineResult<Identity> {
        let openssh = self.identity.to_ssh_string();
        Ok(crate::types::Identity {
            fingerprint: ssh_fingerprint(&openssh),
            openssh,
        })
    }

    async fn get_settings(&self) -> EngineResult<AppSettings> {
        Ok(self.inner.lock().await.settings.clone())
    }

    async fn set_settings(&self, settings: AppSettings) -> EngineResult<AppSettings> {
        self.inner.lock().await.settings = settings.clone();
        self.save().await?;
        Ok(settings)
    }

    async fn create_snapshot(&self, id: VaultId, name: String) -> EngineResult<Snapshot> {
        let snap = self
            .with_vault(&id, |v| {
                v.snapshot(&name)?;
                let s = v
                    .snapshots()
                    .get(&name)
                    .ok_or_else(|| EngineError::engine("snapshot not recorded"))?;
                Ok(Snapshot {
                    name: s.label.clone(),
                    created_at: rfc3339(s.created_unix),
                    frontier: s.frontier.clone(),
                })
            })
            .await?;
        let _ = self.tx.send(EngineEvent::StatusTick { id });
        Ok(snap)
    }

    async fn list_snapshots(&self, id: VaultId) -> EngineResult<Vec<Snapshot>> {
        self.with_vault(&id, |v| {
            Ok(v.snapshots()
                .values()
                .map(|s| Snapshot {
                    name: s.label.clone(),
                    created_at: rfc3339(s.created_unix),
                    frontier: s.frontier.clone(),
                })
                .collect())
        })
        .await
    }

    async fn restore(&self, id: VaultId, target: RestoreTarget) -> EngineResult<()> {
        self.with_vault(&id, |v| {
            match target {
                RestoreTarget::Named { name } => v.restore_snapshot(&name)?,
                RestoreTarget::Time { rfc3339 } => {
                    let t = chrono::DateTime::parse_from_rfc3339(&rfc3339)
                        .map_err(|e| EngineError::engine(format!("bad time: {e}")))?
                        .timestamp() as u64;
                    v.restore_time(t)?;
                }
            }
            Ok(())
        })
        .await?;
        let _ = self.tx.send(EngineEvent::StatusTick { id });
        Ok(())
    }

    async fn get_status(&self, id: VaultId) -> EngineResult<VaultStatus> {
        let meta = self.meta(&id).await?;
        Ok(self.status_of(&meta).await)
    }

    async fn get_aggregate_status(&self) -> EngineResult<AggregateStatus> {
        let metas = self.inner.lock().await.metas.clone();
        let mut active = 0u32;
        let mut error = 0u32;
        let mut any_enabled = false;
        for m in &metas {
            let s = self.status_of(m).await;
            match s.state {
                SyncState::Active => {
                    active += 1;
                    any_enabled = true;
                }
                SyncState::Idle => any_enabled = true,
                SyncState::Error => error += 1,
                SyncState::Disabled => {}
            }
        }
        let state = if error > 0 {
            SyncState::Error
        } else if active > 0 {
            SyncState::Active
        } else if any_enabled {
            SyncState::Idle
        } else {
            SyncState::Disabled
        };
        Ok(AggregateStatus {
            state,
            vault_count: metas.len() as u32,
            active_count: active,
            error_count: error,
        })
    }
}
