//! `StubEngine` — an in-memory `Engine` with simulated live activity.
//!
//! No real protocol, no disk, no network. It exists so the entire UI can be
//! built and demoed before `csp-core` exposes its library API (spec §13).
//! State transitions and the event cadence here are mirrored by the
//! TypeScript mock so browser-dev behaves like the eventual native build.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tokio::sync::broadcast;

use crate::api::Engine;
use crate::events::EngineEvent;
use crate::types::*;

/// Verbatim-style CSP §5.1 caveat surfaced on every clone (spec §6.2).
pub const CLONE_NODEID_WARNING: &str = "This clone forked a fresh NodeId. If you intended to \
resume an existing node identity instead, stop now and reconfigure: reusing a possibly-live \
key on two nodes violates CSP §5.1 and can break deterministic convergence.";

struct VaultRec {
    vault: Vault,
    state: SyncState,
    main_short_sha: String,
    last_activity: Option<String>,
    listener: Option<ListenerInfo>,
    peers: Vec<PeerInfo>,
    authorized: Vec<AuthorizedKey>,
    snapshots: Vec<Snapshot>,
    pending_tofu: Vec<TofuRequest>,
}

struct State {
    vaults: Vec<VaultRec>,
    identity: Identity,
    settings: AppSettings,
    tick: u64,
    seq: u64,
}

pub struct StubEngine {
    state: Mutex<State>,
    tx: broadcast::Sender<EngineEvent>,
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn sha(seed: u64) -> String {
    // Deterministic-looking 7-char short SHA for the demo.
    format!("{:07x}", (seed.wrapping_mul(2654435761)) & 0xfff_ffff)
}

fn default_settings() -> AppSettings {
    AppSettings {
        new_listener: NewListenerDefaults {
            port_strategy: PortStrategy::Auto,
            port_range_start: 51820,
            bind_scope: BindScope::Loopback,
            tofu_enabled: true,
            tls_expected: true,
        },
        behavior: AppBehavior {
            start_at_login: true,
            log_level: "info".into(),
            notifications: NotificationToggles {
                tofu: true,
                peer_connect: true,
                peer_disconnect: true,
                offline: true,
                sync_error: true,
                superseded_edit: true,
            },
        },
    }
}

fn key(fp: &str, comment: &str) -> AuthorizedKey {
    AuthorizedKey {
        fingerprint: fp.into(),
        openssh: format!("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5{} {}", &fp[7..], comment),
        comment: comment.into(),
        added_at: now(),
    }
}

fn aggregate(vaults: &[VaultRec]) -> SyncState {
    let enabled: Vec<SyncState> =
        vaults.iter().filter(|v| v.vault.enabled).map(|v| v.state).collect();
    if enabled.is_empty() {
        return SyncState::Idle;
    }
    if enabled.iter().any(|s| *s == SyncState::Attention) {
        SyncState::Attention
    } else if enabled.iter().any(|s| *s == SyncState::Syncing) {
        SyncState::Syncing
    } else if enabled.iter().all(|s| *s == SyncState::Offline) {
        SyncState::Offline
    } else if enabled.iter().any(|s| *s == SyncState::Synced) {
        SyncState::Synced
    } else {
        SyncState::Idle
    }
}

impl Default for StubEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl StubEngine {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        let vaults = vec![
            VaultRec {
                vault: Vault {
                    id: "notes".into(),
                    display_name: "Notes".into(),
                    path: "/Users/chris/Notes".into(),
                    enabled: true,
                    allow_connections: true,
                    port: 51820,
                    is_csp_vault: true,
                },
                state: SyncState::Synced,
                main_short_sha: sha(1),
                last_activity: Some(now()),
                listener: Some(ListenerInfo {
                    bound: true,
                    port: 51820,
                    bind_scope: BindScope::Lan,
                    tls_expected: true,
                }),
                peers: vec![PeerInfo {
                    fingerprint: "SHA256:ax9Qm2tLpqf0bL3kZ7vYwRn8sJ4cH1dE6gT0uI2oP".into(),
                    address: "192.168.1.42:51820".into(),
                    connected_since: now(),
                    sync_state: SyncState::Synced,
                }],
                authorized: vec![
                    key("SHA256:ax9Qm2tLpqf0bL3kZ7vYwRn8sJ4cH1dE6gT0uI2oP", "chris@laptop"),
                    key("SHA256:bk2Wp7rNvc1dF8hM4jX9yQs5tU6wA3eR0gB7nL1mZ", "chris@desktop"),
                ],
                snapshots: vec![Snapshot {
                    name: "before-cleanup".into(),
                    created_at: now(),
                    frontier_shas: vec![sha(11), sha(12)],
                }],
                pending_tofu: vec![],
            },
            VaultRec {
                vault: Vault {
                    id: "design-docs".into(),
                    display_name: "Design Docs".into(),
                    path: "/Users/chris/Work/design-docs".into(),
                    enabled: true,
                    allow_connections: false,
                    port: 51821,
                    is_csp_vault: true,
                },
                state: SyncState::Syncing,
                main_short_sha: sha(2),
                last_activity: Some(now()),
                listener: None,
                peers: vec![],
                authorized: vec![key(
                    "SHA256:cm3Xq8sOwd2eG9iN5kY0zRt6uV7xB4fS1hC8oM2nA",
                    "team-relay",
                )],
                snapshots: vec![],
                pending_tofu: vec![],
            },
            VaultRec {
                vault: Vault {
                    id: "photos-backup".into(),
                    display_name: "Photos Backup".into(),
                    path: "/Users/chris/Pictures/backup".into(),
                    enabled: true,
                    allow_connections: true,
                    port: 51822,
                    is_csp_vault: true,
                },
                state: SyncState::Offline,
                main_short_sha: sha(3),
                last_activity: None,
                listener: Some(ListenerInfo {
                    bound: true,
                    port: 51822,
                    bind_scope: BindScope::Lan,
                    tls_expected: false,
                }),
                peers: vec![],
                // Empty authorized set on purpose: lets TOFU fire (spec §8.3).
                authorized: vec![],
                snapshots: vec![],
                pending_tofu: vec![],
            },
        ];

        StubEngine {
            tx,
            state: Mutex::new(State {
                vaults,
                identity: Identity {
                    openssh: "ssh-ed25519 \
AAAAC3NzaC1lZDI1NTE5AAAAIH8sJ4cH1dE6gT0uI2oPax9Qm2tLpqf0bL3kZ7vYw chris@this-device"
                        .into(),
                    fingerprint: "SHA256:dn4Yr9tPxe3fH0jO6lZ1aSu7vW8yC5gT2iD9pN3oB".into(),
                    source: IdentitySource::DeviceGlobal,
                },
                settings: default_settings(),
                tick: 0,
                seq: 0,
            }),
        }
    }

    fn emit(&self, ev: EngineEvent) {
        // Err just means no subscribers yet — fine for a stub.
        let _ = self.tx.send(ev);
    }

    fn next_seq(state: &mut State) -> u64 {
        state.seq += 1;
        state.seq
    }

    /// The background simulation loop. Drives the OrbStack/Docker-style
    /// "alive" feel (status ticks, peer churn, superseded edits). Spawn it
    /// with whatever the host's runtime is (`tauri::async_runtime::spawn`
    /// in the app, `tokio::spawn` standalone).
    pub async fn run_simulation(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        loop {
            interval.tick().await;
            self.simulate_step();
        }
    }

    /// Convenience for standalone/tokio contexts.
    pub fn start_simulation(self: Arc<Self>) {
        tokio::spawn(self.run_simulation());
    }

    fn simulate_step(&self) {
        let mut events: Vec<EngineEvent> = Vec::new();
        {
            let mut st = self.state.lock().unwrap();
            st.tick += 1;
            let tick = st.tick;

            for v in st.vaults.iter_mut().filter(|v| v.vault.enabled) {
                let prev = v.state;
                v.state = match (v.vault.id.as_str(), tick % 4) {
                    ("notes", 0) => SyncState::Syncing,
                    ("notes", _) => SyncState::Synced,
                    ("design-docs", 0 | 2) => SyncState::Syncing,
                    ("design-docs", _) => SyncState::Synced,
                    ("photos-backup", 1) => SyncState::Syncing,
                    ("photos-backup", 2) => SyncState::Synced,
                    ("photos-backup", _) => SyncState::Offline,
                    _ => v.state,
                };
                if v.state != prev {
                    if v.state == SyncState::Syncing {
                        v.main_short_sha = sha(tick.wrapping_add(v.vault.port as u64));
                        v.last_activity = Some(now());
                    }
                    events.push(EngineEvent::StatusTick {
                        id: v.vault.id.clone(),
                        state: v.state,
                        peers_connected: v.peers.len() as u32,
                        main_short_sha: v.main_short_sha.clone(),
                    });
                }
            }

            // Peer churn on "notes" every 4th tick.
            if tick % 4 == 0 {
                if let Some(v) = st.vaults.iter_mut().find(|v| v.vault.id == "notes") {
                    if v.peers.is_empty() {
                        let fp = "SHA256:ax9Qm2tLpqf0bL3kZ7vYwRn8sJ4cH1dE6gT0uI2oP";
                        v.peers.push(PeerInfo {
                            fingerprint: fp.into(),
                            address: "192.168.1.42:51820".into(),
                            connected_since: now(),
                            sync_state: SyncState::Synced,
                        });
                        events.push(EngineEvent::PeerConnected {
                            id: "notes".into(),
                            peer_fingerprint: fp.into(),
                        });
                    } else {
                        let fp = v.peers.remove(0).fingerprint;
                        events.push(EngineEvent::PeerDisconnected {
                            id: "notes".into(),
                            peer_fingerprint: fp,
                        });
                    }
                }
            }

            // Occasional superseded same-region edit (spec §9), purely
            // informational — the engine already resolved it.
            if tick % 9 == 0 {
                events.push(EngineEvent::SupersededEdit {
                    id: "design-docs".into(),
                    path: "architecture/overview.md".into(),
                    snapshot_hint: "auto/2026-05-17-pre-merge".into(),
                });
            }

            events.push(EngineEvent::AggregateTick { state: aggregate(&st.vaults) });
        }
        for ev in events {
            self.emit(ev);
        }
    }

    fn with_vault<T>(
        &self,
        id: &str,
        f: impl FnOnce(&mut VaultRec) -> T,
    ) -> EngineResult<T> {
        let mut st = self.state.lock().unwrap();
        let v = st
            .vaults
            .iter_mut()
            .find(|v| v.vault.id == id)
            .ok_or_else(|| EngineError::not_found(format!("no vault {id}")))?;
        Ok(f(v))
    }
}

#[async_trait]
impl Engine for StubEngine {
    fn subscribe(&self) -> broadcast::Receiver<EngineEvent> {
        self.tx.subscribe()
    }

    async fn list_vaults(&self) -> EngineResult<Vec<Vault>> {
        let st = self.state.lock().unwrap();
        Ok(st.vaults.iter().map(|v| v.vault.clone()).collect())
    }

    async fn add_local_folder(&self, path: String) -> EngineResult<Vault> {
        let mut st = self.state.lock().unwrap();
        let seq = Self::next_seq(&mut st);
        let name = path.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("folder");
        let vault = Vault {
            id: format!("vault-{seq}"),
            display_name: name.to_string(),
            path: path.clone(),
            enabled: true,
            allow_connections: false,
            port: 51820 + seq as u16,
            is_csp_vault: seq % 2 == 0,
        };
        st.vaults.push(VaultRec {
            vault: vault.clone(),
            state: SyncState::Idle,
            main_short_sha: sha(seq),
            last_activity: Some(now()),
            listener: None,
            peers: vec![],
            authorized: vec![],
            snapshots: vec![],
            pending_tofu: vec![],
        });
        drop(st);
        self.emit(EngineEvent::StatusTick {
            id: vault.id.clone(),
            state: SyncState::Idle,
            peers_connected: 0,
            main_short_sha: vault.id.clone(),
        });
        Ok(vault)
    }

    async fn clone_remote(&self, dest: String, url: String) -> EngineResult<CloneOutcome> {
        let mut st = self.state.lock().unwrap();
        let seq = Self::next_seq(&mut st);
        let name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("cloned-vault");
        let vault = Vault {
            id: format!("clone-{seq}"),
            display_name: name.to_string(),
            path: dest,
            enabled: true,
            allow_connections: false,
            port: 51820 + seq as u16,
            is_csp_vault: true,
        };
        st.vaults.push(VaultRec {
            vault: vault.clone(),
            state: SyncState::Syncing,
            main_short_sha: sha(seq),
            last_activity: Some(now()),
            listener: None,
            peers: vec![PeerInfo {
                fingerprint: "SHA256:remote-peer-just-cloned".into(),
                address: url.clone(),
                connected_since: now(),
                sync_state: SyncState::Syncing,
            }],
            authorized: vec![],
            snapshots: vec![],
            pending_tofu: vec![],
        });
        drop(st);
        self.emit(EngineEvent::StatusTick {
            id: vault.id.clone(),
            state: SyncState::Syncing,
            peers_connected: 1,
            main_short_sha: sha(seq),
        });
        Ok(CloneOutcome { vault, node_id_warning: Some(CLONE_NODEID_WARNING.to_string()) })
    }

    async fn remove_vault(&self, id: VaultId) -> EngineResult<()> {
        let mut st = self.state.lock().unwrap();
        let before = st.vaults.len();
        st.vaults.retain(|v| v.vault.id != id);
        if st.vaults.len() == before {
            return Err(EngineError::not_found(format!("no vault {id}")));
        }
        Ok(())
    }

    async fn set_enabled(&self, id: VaultId, on: bool) -> EngineResult<()> {
        self.with_vault(&id, |v| {
            v.vault.enabled = on;
            v.state = if on { SyncState::Syncing } else { SyncState::Idle };
            if !on {
                v.peers.clear();
            }
        })?;
        self.emit(EngineEvent::StatusTick {
            id,
            state: if on { SyncState::Syncing } else { SyncState::Idle },
            peers_connected: 0,
            main_short_sha: String::new(),
        });
        Ok(())
    }

    async fn set_allow_connections(&self, id: VaultId, on: bool) -> EngineResult<ListenerInfo> {
        let info = self.with_vault(&id, |v| {
            v.vault.allow_connections = on;
            let info = ListenerInfo {
                bound: on,
                port: v.vault.port,
                bind_scope: BindScope::Lan,
                tls_expected: true,
            };
            v.listener = if on { Some(info.clone()) } else { None };
            info
        })?;
        self.emit(EngineEvent::ListenerChanged { id, listener: info.clone() });
        Ok(info)
    }

    async fn get_connect_address(&self, id: VaultId) -> EngineResult<ConnectAddress> {
        let st = self.state.lock().unwrap();
        let v = st
            .vaults
            .iter()
            .find(|v| v.vault.id == id)
            .ok_or_else(|| EngineError::not_found(format!("no vault {id}")))?;
        let port = v.vault.port;
        let is_non_loopback = true;
        // The strong caveat applies only in the genuinely risky state: a
        // non-loopback listener whose authorized set is empty *and* where
        // TOFU is on — i.e. it would literally "trust whoever connects
        // first" (spec §8.4 / CSP §13.2). Pre-seeded keys or disabled TOFU
        // remove that specific risk, so the warning is not shown then.
        let trusts_first_comer =
            is_non_loopback && v.authorized.is_empty() && st.settings.new_listener.tofu_enabled;
        let lan_ip = "192.168.1.50".to_string();
        Ok(ConnectAddress {
            scheme: "wss".into(),
            lan_ip: lan_ip.clone(),
            port,
            address: format!("wss://{lan_ip}:{port}"),
            firewall_guidance: "macOS: the Application Firewall is per-application, not \
per-port. On first listen, accept the OS prompt to allow incoming connections for \
\"Context Desktop\". If you previously denied it, re-enable it under System Settings → \
Network → Firewall. The port itself needs no separate macOS rule."
                .into(),
            is_non_loopback,
            exposure_caveat: trusts_first_comer.then(|| {
                "This listener has an empty authorized set and TOFU is on, so it will \
trust whichever device connects first. Prefer LAN or a private overlay (VPN/Tailscale); \
pre-seed authorized keys or disable TOFU before exposing it publicly."
                    .into()
            }),
        })
    }

    async fn list_authorized(&self, id: VaultId) -> EngineResult<Vec<AuthorizedKey>> {
        self.with_vault(&id, |v| v.authorized.clone())
    }

    async fn authorize(&self, id: VaultId, pubkey: String) -> EngineResult<()> {
        self.with_vault(&id, |v| {
            let fp = format!("SHA256:{:x}", pubkey.len().wrapping_mul(0x9E37) & 0xffffff);
            v.authorized.push(AuthorizedKey {
                fingerprint: fp,
                openssh: pubkey.clone(),
                comment: pubkey.split_whitespace().nth(2).unwrap_or("").to_string(),
                added_at: now(),
            });
        })?;
        Ok(())
    }

    async fn revoke(&self, id: VaultId, fingerprint: String) -> EngineResult<()> {
        self.with_vault(&id, |v| {
            v.authorized.retain(|k| k.fingerprint != fingerprint);
        })?;
        Ok(())
    }

    async fn respond_tofu(&self, request_id: String, allow: bool) -> EngineResult<()> {
        let mut st = self.state.lock().unwrap();
        for v in st.vaults.iter_mut() {
            if let Some(pos) = v.pending_tofu.iter().position(|t| t.request_id == request_id) {
                let req = v.pending_tofu.remove(pos);
                if allow {
                    v.authorized.push(AuthorizedKey {
                        fingerprint: req.peer_fingerprint.clone(),
                        openssh: req.peer_openssh.clone(),
                        comment: "tofu-approved".into(),
                        added_at: now(),
                    });
                }
                drop(st);
                self.emit(EngineEvent::TofuResolved { request_id, allowed: allow });
                return Ok(());
            }
        }
        Err(EngineError::not_found(format!("no pending TOFU {request_id}")))
    }

    async fn get_identity(&self) -> EngineResult<Identity> {
        Ok(self.state.lock().unwrap().identity.clone())
    }

    async fn set_identity_source(&self, src: IdentitySource) -> EngineResult<Identity> {
        let mut st = self.state.lock().unwrap();
        st.identity.source = src;
        Ok(st.identity.clone())
    }

    async fn get_settings(&self) -> EngineResult<AppSettings> {
        Ok(self.state.lock().unwrap().settings.clone())
    }

    async fn set_settings(&self, settings: AppSettings) -> EngineResult<AppSettings> {
        let mut st = self.state.lock().unwrap();
        st.settings = settings;
        Ok(st.settings.clone())
    }

    async fn create_snapshot(&self, id: VaultId, name: String) -> EngineResult<Snapshot> {
        let snap = self.with_vault(&id, |v| {
            let snap = Snapshot {
                name: name.clone(),
                created_at: now(),
                frontier_shas: vec![v.main_short_sha.clone()],
            };
            v.snapshots.push(snap.clone());
            snap
        })?;
        self.emit(EngineEvent::SnapshotCreated { id, snapshot: snap.clone() });
        Ok(snap)
    }

    async fn list_snapshots(&self, id: VaultId) -> EngineResult<Vec<Snapshot>> {
        self.with_vault(&id, |v| v.snapshots.clone())
    }

    async fn restore(&self, id: VaultId, target: RestoreTarget) -> EngineResult<()> {
        self.with_vault(&id, |v| {
            v.state = SyncState::Syncing;
            v.last_activity = Some(now());
            let _ = &target; // restore-as-edit; engine reflects convergence
        })?;
        self.emit(EngineEvent::StatusTick {
            id,
            state: SyncState::Syncing,
            peers_connected: 0,
            main_short_sha: String::new(),
        });
        Ok(())
    }

    async fn get_status(&self, id: VaultId) -> EngineResult<VaultStatus> {
        self.with_vault(&id, |v| VaultStatus {
            id: v.vault.id.clone(),
            state: v.state,
            peers_connected: v.peers.len() as u32,
            main_short_sha: v.main_short_sha.clone(),
            last_activity: v.last_activity.clone(),
            listener: v.listener.clone(),
            peers: v.peers.clone(),
            pending_tofu: v.pending_tofu.clone(),
        })
    }

    async fn get_aggregate_status(&self) -> EngineResult<AggregateStatus> {
        let st = self.state.lock().unwrap();
        let vault_count = st.vaults.len() as u32;
        let syncing_count =
            st.vaults.iter().filter(|v| v.state == SyncState::Syncing).count() as u32;
        let attention_count =
            st.vaults.iter().filter(|v| v.state == SyncState::Attention).count() as u32;
        Ok(AggregateStatus {
            state: aggregate(&st.vaults),
            vault_count,
            syncing_count,
            attention_count,
        })
    }

    async fn dev_trigger_tofu(&self) -> EngineResult<()> {
        let req = {
            let mut st = self.state.lock().unwrap();
            let seq = Self::next_seq(&mut st);
            // Prefer a vault with an empty authorized set (genuine TOFU).
            if st.vaults.is_empty() {
                return Err(EngineError::not_found("no vaults"));
            }
            let idx = st
                .vaults
                .iter()
                .position(|v| v.authorized.is_empty())
                .unwrap_or(0);
            let v = &mut st.vaults[idx];
            let req = TofuRequest {
                request_id: format!("tofu-{seq}"),
                vault_id: v.vault.id.clone(),
                peer_fingerprint: format!("SHA256:newpeer{seq:04x}deadbeefcafef00dba5e"),
                peer_openssh: format!(
                    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5newpeer{seq:04x} unknown@peer"
                ),
                address: "192.168.1.77:51999".into(),
            };
            v.pending_tofu.push(req.clone());
            req
        };
        self.emit(EngineEvent::TofuRequested { request: req });
        Ok(())
    }

    async fn dev_trigger_superseded(&self) -> EngineResult<()> {
        let id = {
            let st = self.state.lock().unwrap();
            st.vaults.first().map(|v| v.vault.id.clone())
        }
        .ok_or_else(|| EngineError::not_found("no vaults"))?;
        self.emit(EngineEvent::SupersededEdit {
            id,
            path: "notes/2026-05-17.md".into(),
            snapshot_hint: "auto/pre-merge-2026-05-17".into(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixtures_and_roundtrip() {
        let e = StubEngine::new();
        let vaults = e.list_vaults().await.unwrap();
        assert_eq!(vaults.len(), 3);
        // photos-backup has an empty authorized set so TOFU can fire.
        let authd = e.list_authorized("photos-backup".into()).await.unwrap();
        assert!(authd.is_empty());

        let status = e.get_status("notes".into()).await.unwrap();
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("mainShortSha"));
        assert!(json.contains("\"state\":\"synced\""));

        let agg = e.get_aggregate_status().await.unwrap();
        assert_eq!(agg.vault_count, 3);
    }

    #[tokio::test]
    async fn tofu_flow() {
        let e = StubEngine::new();
        let mut rx = e.subscribe();
        e.dev_trigger_tofu().await.unwrap();
        let ev = rx.recv().await.unwrap();
        let request_id = match ev {
            EngineEvent::TofuRequested { request } => request.request_id,
            other => panic!("expected TofuRequested, got {other:?}"),
        };
        e.respond_tofu(request_id, true).await.unwrap();
        // photos-backup should now have one authorized key.
        let authd = e.list_authorized("photos-backup".into()).await.unwrap();
        assert_eq!(authd.len(), 1);
    }
}
