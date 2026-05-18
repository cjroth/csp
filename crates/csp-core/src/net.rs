//! Replication protocol (§6, §10). Persistent, reliable, ordered,
//! message-oriented, bidirectional, fully symmetric (§6.2). Git's smart
//! protocol is not used; there is no polling. Mutual ed25519 handshake over
//! a nonce transcript (§10); **per-author** authorization enforced in
//! `Vault::integrate` regardless of the relaying peer (§6.1/§10);
//! frontier-set anti-entropy catch-up (§6.4); immediate live push (§6.5);
//! full nodes relay (§6.1).
//!
//! Transport is plaintext WebSocket — CSP ships no embedded CA; terminate
//! TLS at a fronting proxy on untrusted networks (§10). Protocol-level
//! mutual auth + content integrity hold regardless of transport TLS.

#![cfg(not(target_arch = "wasm32"))]

use crate::error::{CspError, CspResult};
use crate::identity::Identity;
use crate::store::Store;
use crate::vault::Vault;
pub use crate::wire::Msg;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message as WsMsg;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Listener,
    Connector,
}

const AUTH_CTX: &[u8] = b"csp-auth-v1";

/// A running engine node: the shared vault plus an intra-process relay bus
/// that fans new object closures out to every live session (§6.1 relay).
#[derive(Clone)]
pub struct Node {
    pub vault: Arc<Mutex<Vault>>,
    bus: broadcast::Sender<Vec<Vec<u8>>>,
}

impl Node {
    pub fn new(vault: Vault) -> Node {
        let (bus, _) = broadcast::channel(4096);
        Node {
            vault: Arc::new(Mutex::new(vault)),
            bus,
        }
    }

    /// Commit any local working-tree changes (§5.6) and live-push the new
    /// primitive's closure to every connected peer (§6.5). Returns the new
    /// primitive oid hex if a commit was made.
    pub async fn commit_and_publish(&self) -> CspResult<Option<String>> {
        let mut v = self.vault.lock().await;
        if let Some(prim) = v.commit_local_changes()? {
            let raws = v.export_closure(&[prim])?;
            let _ = self.bus.send(raws);
            Ok(Some(prim.to_hex()))
        } else {
            Ok(None)
        }
    }

    /// Listen for inbound peers and relay between them (§6.1). **HARD
    /// INVARIANT (§7):** only full nodes listen — enforced by the caller
    /// (CLI refuses `--listen` on a thin tier). `tls = Some(cfg)` serves
    /// `wss://` (the default, §17.1); `None` serves plaintext `ws://`
    /// (`--no-tls`: behind a TLS-terminating proxy, or local/trusted).
    /// `tls = Some((cfg, cert_fp))`: serve `wss://`; `cert_fp` (SHA-256 of
    /// the server cert) is the channel binding mixed into the handshake
    /// transcript (§10). `None`: plaintext `ws://`, empty binding (trusted
    /// network — acceptable per §10).
    pub async fn serve(
        &self,
        addr: SocketAddr,
        tls: Option<(Arc<rustls::ServerConfig>, [u8; 32])>,
    ) -> CspResult<(SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| CspError::Protocol(format!("bind {addr}: {e}")))?;
        let bound = listener
            .local_addr()
            .map_err(|e| CspError::Protocol(e.to_string()))?;
        let node = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _peer)) => {
                        let node = node.clone();
                        let tls = tls.clone();
                        tokio::spawn(async move {
                            let r = match tls {
                                Some((cfg, cert_fp)) => {
                                    match tokio_rustls::TlsAcceptor::from(cfg)
                                        .accept(stream)
                                        .await
                                    {
                                        Ok(s) => {
                                            accept_ws(node, s, cert_fp.to_vec()).await
                                        }
                                        Err(e) => Err(CspError::Protocol(format!(
                                            "tls accept: {e}"
                                        ))),
                                    }
                                }
                                None => accept_ws(node, stream, Vec::new()).await,
                            };
                            if let Err(e) = r {
                                tracing::debug!("session ended: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("accept error: {e}");
                        break;
                    }
                }
            }
        });
        Ok((bound, handle))
    }

    /// Connect to a listening peer and keep the session alive, re-running
    /// catch-up on every reconnect (§6.5: "no separate resync path").
    pub fn connect(&self, url: String) -> tokio::task::JoinHandle<()> {
        let node = self.clone();
        tokio::spawn(async move {
            tracing::info!("connecting to {url}");
            let mut warned = false;
            loop {
                match connect_once(&node, &url).await {
                    Ok(()) => {
                        warned = false;
                    }
                    Err(e) => {
                        // Log the first failure (and the first after a
                        // recovery) at WARN with the reason; stay quiet on
                        // repeated retries to avoid log spam.
                        if !warned {
                            tracing::warn!("connect {url} failed: {e} (retrying)");
                            warned = true;
                        } else {
                            tracing::debug!("connect {url} failed: {e}");
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        })
    }
}

/// Bootstrap probe for `ctx clone` (§17): connect, read the listener's
/// `Hello` to learn the vault id + its public key (so the fresh node can be
/// created with the right vault id and seed the source's key into its local
/// authorized set — §10), then drop the connection.
pub async fn probe(url: &str, identity: &Identity) -> CspResult<(String, String)> {
    let (mut t, _cb) = dial(url).await?;
    t.send(&Msg::Hello {
        vault_id: String::new(),
        node_ssh: identity.to_ssh_string(),
        nonce: rand_nonce(),
        proto: crate::wire::PROTO_VERSION,
    })
    .await?;
    match t.recv().await {
        Some(Msg::Hello { vault_id, node_ssh, .. }) => Ok((vault_id, node_ssh)),
        _ => Err(CspError::Protocol("probe: expected Hello".into())),
    }
}

/// Wrap a freshly accepted server-side stream (plaintext TCP *or* a
/// completed TLS stream) as a WebSocket session.
async fn accept_ws<S>(node: Node, stream: S, cb: Vec<u8>) -> CspResult<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tracing::info!("inbound connection");
    let ws = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| CspError::Protocol(format!("ws accept: {e}")))?;
    run_session(node, Transport::from_ws(ws), Role::Listener, cb).await
}

/// Dial a peer URL, doing the TLS handshake for `wss://` (accepting any
/// server cert — trust is the application-layer ed25519 handshake, §10).
/// Returns the transport and the **channel binding**: the SHA-256 of the
/// server's TLS cert (empty for plaintext `ws://`).
async fn dial(url: &str) -> CspResult<(Transport, Vec<u8>)> {
    if let Some(rest) = url.strip_prefix("wss://") {
        let authority = rest.split('/').next().unwrap_or(rest);
        let host = authority.rsplit_once(':').map(|(h, _)| h).unwrap_or(authority);
        let tcp = TcpStream::connect(authority)
            .await
            .map_err(|e| CspError::Protocol(format!("tcp connect {authority}: {e}")))?;
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| CspError::Protocol(format!("server name: {e}")))?;
        let tls = tokio_rustls::TlsConnector::from(crate::tls::client_config_accept_any())
            .connect(server_name, tcp)
            .await
            .map_err(|e| CspError::Protocol(format!("tls connect: {e}")))?;
        // Channel binding: the server cert the client actually saw (§10).
        let cb = {
            let (_io, conn) = tls.get_ref();
            conn.peer_certificates()
                .and_then(|c| c.first())
                .map(|c| crate::tls::cert_fingerprint(c.as_ref()).to_vec())
                .unwrap_or_default()
        };
        let (ws, _r) = tokio_tungstenite::client_async(url, tls)
            .await
            .map_err(|e| CspError::Protocol(format!("wss connect: {e}")))?;
        Ok((Transport::from_ws(ws), cb))
    } else {
        let (ws, _r) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| CspError::Protocol(format!("ws connect: {e}")))?;
        Ok((Transport::from_ws(ws), Vec::new()))
    }
}

async fn connect_once(node: &Node, url: &str) -> CspResult<()> {
    let (transport, cb) = dial(url).await?;
    run_session(node.clone(), transport, Role::Connector, cb).await
}

/// A uniform message channel. Both WebSocket and the in-process duplex
/// (tests) present the same surface; a pump task bridges the ws Stream/Sink.
pub struct Transport {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl Transport {
    /// In-process duplex pair for unit tests (no sockets).
    pub fn pair() -> (Transport, Transport) {
        let (a_tx, b_rx) = mpsc::channel(256);
        let (b_tx, a_rx) = mpsc::channel(256);
        (
            Transport { tx: a_tx, rx: a_rx },
            Transport { tx: b_tx, rx: b_rx },
        )
    }

    fn from_ws<S>(ws: S) -> Transport
    where
        S: futures_util::Stream<Item = Result<WsMsg, tokio_tungstenite::tungstenite::Error>>
            + futures_util::Sink<WsMsg, Error = tokio_tungstenite::tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        let (mut sink, mut stream) = ws.split();
        let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>(256);
        let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(256);
        // ws → out_tx
        tokio::spawn(async move {
            while let Some(m) = stream.next().await {
                match m {
                    Ok(WsMsg::Binary(b)) => {
                        if out_tx.send(b.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Ok(WsMsg::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        });
        // in_rx → ws
        tokio::spawn(async move {
            let mut in_rx = in_rx;
            while let Some(b) = in_rx.recv().await {
                if sink.send(WsMsg::Binary(b.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });
        Transport { tx: in_tx, rx: out_rx }
    }

    async fn send(&self, m: &Msg) -> CspResult<()> {
        let b = rmp_serde::to_vec_named(m).map_err(|e| CspError::Protocol(e.to_string()))?;
        self.tx
            .send(b)
            .await
            .map_err(|_| CspError::Protocol("transport closed".into()))
    }

    async fn recv(&mut self) -> Option<Msg> {
        let b = self.rx.recv().await?;
        rmp_serde::from_slice(&b).ok()
    }
}

fn rand_nonce() -> Vec<u8> {
    use rand::RngCore;
    let mut n = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut n);
    n.to_vec()
}

fn transcript(client_nonce: &[u8], server_nonce: &[u8], channel_binding: &[u8]) -> Vec<u8> {
    let mut t = AUTH_CTX.to_vec();
    t.extend_from_slice(client_nonce);
    t.extend_from_slice(server_nonce);
    // §10 channel binding: the TLS cert fingerprint both sides observed. A
    // relayed MITM that re-terminates TLS presents a different cert, so the
    // signed transcripts no longer match. Empty for plaintext `ws://`.
    t.extend_from_slice(channel_binding);
    t
}

/// Mutual authentication (§10): each side signs a transcript covering both
/// nonces; both directions verify. The listener then admits the connector
/// per its **local** authorized set (TOFU only while empty — §10),
/// independent of any relay (per-author trust is separately enforced in
/// `integrate`).
async fn handshake(
    node: &Node,
    t: &mut Transport,
    role: Role,
    channel_binding: &[u8],
) -> CspResult<()> {
    let (vault_id, my_ssh, identity): (String, String, Identity) = {
        let v = node.vault.lock().await;
        (
            v.vault_id().to_string(),
            v.identity_ssh(),
            v.identity_clone(),
        )
    };
    let my_nonce = rand_nonce();
    t.send(&Msg::Hello {
        vault_id: vault_id.clone(),
        node_ssh: my_ssh.clone(),
        nonce: my_nonce.clone(),
        proto: crate::wire::PROTO_VERSION,
    })
    .await?;
    let (peer_vault, peer_ssh, peer_nonce, peer_proto) = match t.recv().await {
        Some(Msg::Hello { vault_id, node_ssh, nonce, proto }) => {
            (vault_id, node_ssh, nonce, proto)
        }
        _ => return Err(CspError::Protocol("expected Hello".into())),
    };
    if peer_proto != crate::wire::PROTO_VERSION {
        tracing::warn!(
            "rejecting peer: protocol version mismatch (peer speaks v{peer_proto}, \
             we speak v{}) — both nodes must run the same `ctx` build; restart the \
             other side after upgrading",
            crate::wire::PROTO_VERSION
        );
        return Err(CspError::Protocol(format!(
            "protocol version mismatch: peer v{peer_proto} != ours v{}",
            crate::wire::PROTO_VERSION
        )));
    }
    if peer_vault != vault_id {
        tracing::warn!(
            "rejecting peer: vault id mismatch (theirs={peer_vault:?}, ours={vault_id:?}) \
             — both sides must `ctx init --vault-id <same>`"
        );
        return Err(CspError::Protocol(format!(
            "vault id mismatch: {peer_vault} != {vault_id}"
        )));
    }
    let (client_nonce, server_nonce) = match role {
        Role::Connector => (my_nonce.clone(), peer_nonce.clone()),
        Role::Listener => (peer_nonce.clone(), my_nonce.clone()),
    };
    let script = transcript(&client_nonce, &server_nonce, channel_binding);
    t.send(&Msg::AuthProof { sig: identity.sign(&script) }).await?;
    let peer_sig = match t.recv().await {
        Some(Msg::AuthProof { sig }) => sig,
        _ => return Err(CspError::Protocol("expected AuthProof".into())),
    };
    let peer_node = crate::identity::parse_ssh_pubkey(&peer_ssh)
        .ok_or_else(|| CspError::BadSignature("bad peer ssh key".into()))?;
    crate::identity::verify_detached(&peer_node, &script, &peer_sig)?;

    if role == Role::Listener {
        // Admission is the listener's local policy (§10). TOFU only while
        // the authorized set is empty.
        let v = node.vault.lock().await;
        if !v.admit_peer_tofu(&peer_ssh)? {
            tracing::warn!(
                "rejecting unauthorized peer {}…\n  to allow it, run on THIS node:\n  ctx authorize \"{}\"",
                &peer_node.to_hex()[..12],
                peer_ssh.trim()
            );
            return Err(CspError::Unauthorized(format!(
                "peer {} not authorized",
                &peer_node.to_hex()[..12]
            )));
        }
    }
    match role {
        Role::Listener => tracing::info!(
            "peer authenticated: {}… (vault {})",
            &peer_node.to_hex()[..12],
            vault_id
        ),
        Role::Connector => tracing::info!(
            "authenticated to listener {}… (vault {})",
            &peer_node.to_hex()[..12],
            vault_id
        ),
    }
    Ok(())
}

async fn run_session(
    node: Node,
    mut t: Transport,
    role: Role,
    channel_binding: Vec<u8>,
) -> CspResult<()> {
    handshake(&node, &mut t, role, &channel_binding).await?;

    // Catch-up kickoff: advertise our frontier (§6.4).
    let my_tips = {
        let v = node.vault.lock().await;
        v.frontier_tips()?
            .into_iter()
            .map(|o| o.to_hex())
            .collect::<Vec<_>>()
    };
    tracing::info!("catch-up: advertised {} frontier tip(s)", my_tips.len());
    t.send(&Msg::FrontierDigest { tips: my_tips }).await?;

    let mut relay_rx = node.bus.subscribe();

    loop {
        tokio::select! {
            biased;
            incoming = t.recv() => {
                let Some(msg) = incoming else {
                    tracing::info!("peer session ended");
                    return Ok(());
                };
                handle_msg(&node, &t, msg).await?;
            }
            relayed = relay_rx.recv() => {
                match relayed {
                    Ok(raws) => { t.send(&Msg::Live { raws }).await?; }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

async fn handle_msg(node: &Node, t: &Transport, msg: Msg) -> CspResult<()> {
    match msg {
        Msg::FrontierDigest { tips } => {
            let v = node.vault.lock().await;
            let mut want = Vec::new();
            for hex in tips {
                if let Ok(o) = crate::oid::Oid::from_hex(&hex) {
                    if !v.repo().store.has(o) || !v.known()?.contains(&o) {
                        want.push(hex);
                    }
                }
            }
            drop(v);
            if !want.is_empty() {
                t.send(&Msg::WantTips { tips: want }).await?;
            }
        }
        Msg::WantTips { tips } => {
            let v = node.vault.lock().await;
            let oids: Vec<_> = tips
                .iter()
                .filter_map(|h| crate::oid::Oid::from_hex(h).ok())
                .collect();
            let raws = v.export_closure(&oids)?;
            drop(v);
            t.send(&Msg::Objects { raws }).await?;
        }
        Msg::Objects { raws } | Msg::Live { raws } => {
            let (admitted, main) = {
                let mut v = node.vault.lock().await;
                let a = v.integrate(&raws)?;
                (a, v.main().map(|o| o.to_hex()).unwrap_or_default())
            };
            if admitted > 0 {
                tracing::info!(
                    "integrated {admitted} new primitive(s); main={}…",
                    &main[..main.len().min(12)]
                );
                // Relay onward (§6.1). Idempotent + admitted-gated, so
                // gossip terminates.
                let _ = node.bus.send(raws);
            }
        }
        Msg::Ping => t.send(&Msg::Pong).await?,
        Msg::Pong => {}
        Msg::Hello { .. } | Msg::AuthProof { .. } => {
            return Err(CspError::Protocol("unexpected handshake msg mid-session".into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use tempfile::tempdir;

    fn id(s: u8) -> Identity {
        Identity::from_seed(&[s; 32])
    }

    #[tokio::test]
    async fn two_nodes_converge_over_duplex() {
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let mut va = Vault::create(ta.path(), id(1), "v").unwrap();
        let mut vb = Vault::create(tb.path(), id(2), "v").unwrap();
        va.authorize(&id(2).to_ssh_string()).unwrap();
        vb.authorize(&id(1).to_ssh_string()).unwrap();
        std::fs::write(ta.path().join("a.md"), "AAA").unwrap();
        va.commit_local_changes().unwrap();
        std::fs::write(tb.path().join("b.md"), "BBB").unwrap();
        vb.commit_local_changes().unwrap();

        let na = Node::new(va);
        let nb = Node::new(vb);
        let (t1, t2) = Transport::pair();
        let na2 = na.clone();
        let nb2 = nb.clone();
        tokio::spawn(async move {
            let _ = run_session(na2, t1, Role::Connector, Vec::new()).await;
        });
        tokio::spawn(async move {
            let _ = run_session(nb2, t2, Role::Listener, Vec::new()).await;
        });

        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let am = na.vault.lock().await.main();
            let bm = nb.vault.lock().await.main();
            if am == bm && am.is_some() {
                break;
            }
        }
        let am = na.vault.lock().await.main();
        let bm = nb.vault.lock().await.main();
        assert_eq!(am, bm, "must converge to identical main");
        assert_eq!(
            std::fs::read_to_string(ta.path().join("b.md")).unwrap(),
            "BBB"
        );
        assert_eq!(
            std::fs::read_to_string(tb.path().join("a.md")).unwrap(),
            "AAA"
        );
    }

    #[tokio::test]
    async fn mismatched_channel_binding_fails_handshake() {
        // A relayed MITM that re-terminates TLS presents a different cert →
        // the two sides compute different channel bindings → the signed
        // transcripts don't match → handshake fails → NO sync (§10).
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let mut va = Vault::create(ta.path(), id(1), "v").unwrap();
        let vb = Vault::create(tb.path(), id(2), "v").unwrap();
        va.authorize(&id(2).to_ssh_string()).unwrap();
        vb.authorize(&id(1).to_ssh_string()).unwrap();
        std::fs::write(ta.path().join("a.md"), "AAA").unwrap();
        va.commit_local_changes().unwrap();

        let na = Node::new(va);
        let nb = Node::new(vb);
        let (t1, t2) = Transport::pair();
        let na2 = na.clone();
        let nb2 = nb.clone();
        // Different cert fingerprints on each side (the MITM case).
        tokio::spawn(async move {
            let _ = run_session(na2, t1, Role::Connector, vec![9u8; 32]).await;
        });
        tokio::spawn(async move {
            let _ = run_session(nb2, t2, Role::Listener, vec![8u8; 32]).await;
        });

        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if nb.vault.lock().await.main() == na.vault.lock().await.main() {
                break;
            }
        }
        assert!(
            !tb.path().join("a.md").exists(),
            "mismatched channel binding must abort the handshake — no sync"
        );
    }
}
