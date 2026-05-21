//! Replication protocol (§6, §10). Persistent, reliable, ordered,
//! message-oriented, bidirectional, fully symmetric (§6.2). Git's smart
//! protocol is not used; there is no polling. Mutual ed25519 handshake over
//! a nonce transcript (§10) gates **connection admission** against the
//! listener's local `authorized_keys` — this is the load-bearing trust gate
//! (§6.1/§10); `Vault::integrate` verifies primitive signatures for content
//! integrity but does not re-authorize per author. Frontier-set anti-entropy
//! catch-up (§6.4); immediate live push (§6.5); full nodes relay (§6.1).
//!
//! Default transport is wss:// using a self-signed certificate the listener
//! generates and persists; connectors accept any server cert at the TLS
//! layer because trust is established by the ed25519 mutual-auth handshake
//! (which binds the channel via the cert fingerprint). `--no-tls` opts a
//! listener into plaintext ws:// for running behind a TLS-terminating proxy
//! or on a trusted network. CSP ships no embedded CA: TLS adds
//! confidentiality only — protocol-level mutual auth and content integrity
//! hold regardless of transport TLS.

#![cfg(not(target_arch = "wasm32"))]

use crate::error::{CspError, CspResult};
use crate::identity::Identity;
use crate::store::Store;
use crate::vault::Vault;
pub use crate::wire::Msg;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio_tungstenite::tungstenite::handshake::server::{
    ErrorResponse, Request as WsRequest, Response as WsResponse,
};
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::Message as WsMsg;

pub use crate::session::Role;
use crate::session::{Session, SessionVault};

/// Constant-time byte-string equality (defense against timing oracles when
/// comparing auth keys at the WS upgrade).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc == 0
}

/// Extract a candidate auth-key token from a WS upgrade request, trying:
/// 1. `Authorization: Bearer <token>` (preferred — what `ctx` and Node send).
/// 2. `Sec-WebSocket-Protocol: bearer.<base64-or-raw-token>` (the
///    browser-compatible escape hatch; the spec lists this as fallback).
/// 3. `?auth_key=<token>` query parameter (last-resort fallback when even
///    setting a subprotocol is awkward).
///
/// Returns `(token, source_label)` so callers can log what they matched on
/// without leaking the token itself.
fn extract_bearer_token(req: &WsRequest) -> Option<(String, &'static str)> {
    if let Some(v) = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
    {
        // Case-insensitive on the scheme; tolerant of multiple spaces.
        let mut it = v.splitn(2, char::is_whitespace);
        let scheme = it.next().unwrap_or("");
        let token = it.next().unwrap_or("").trim();
        if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
            return Some((token.to_string(), "authorization-header"));
        }
    }
    if let Some(v) = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|h| h.to_str().ok())
    {
        for proto in v.split(',').map(str::trim) {
            if let Some(t) = proto.strip_prefix("bearer.") {
                if !t.is_empty() {
                    return Some((t.to_string(), "subprotocol"));
                }
            }
        }
    }
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if let Some(t) = pair.strip_prefix("auth_key=") {
                if !t.is_empty() {
                    let token = urldecode(t);
                    return Some((token, "query-string"));
                }
            }
        }
    }
    None
}

/// Minimal percent-decode for the `?auth_key=` fallback path. Keeps net.rs
/// dependency-free of a full URL crate — a token is opaque bytes; we only
/// need `%XX` and `+ → space`.
fn urldecode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '+' => out.push(' '),
            '%' => {
                let h: String = it.by_ref().take(2).collect();
                if let Ok(b) = u8::from_str_radix(&h, 16) {
                    out.push(b as char);
                } else {
                    out.push('%');
                    out.push_str(&h);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// 401-with-message response used when the WS upgrade fails auth. The
/// tungstenite `ErrorResponse` body is `Option<String>` — the literal
/// reason text is sent so operator logs on the client side are not
/// opaque.
fn unauthorized_response(reason: &str) -> ErrorResponse {
    use tokio_tungstenite::tungstenite::http::Response;
    let mut resp: ErrorResponse = Response::new(Some(format!(
        "auth-key check failed: {reason}"
    )));
    *resp.status_mut() = StatusCode::UNAUTHORIZED;
    resp.headers_mut().insert(
        "www-authenticate",
        HeaderValue::from_static("Bearer realm=\"csp\""),
    );
    resp
}

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

    /// Listen for inbound peers and relay between them. Listening is a
    /// native-only capability (server sockets + on-disk odb); the wasm build
    /// does not compile this module at all, so a browser/WebView node is
    /// outbound-only by construction. `tls = Some((cfg, cert_fp))`: serve
    /// `wss://` (the default); `cert_fp` (SHA-256 of the server cert) is the
    /// channel binding mixed into the handshake transcript. `None`: plaintext
    /// `ws://` (`--no-tls`: behind a TLS-terminating proxy or on a trusted
    /// network), empty channel binding.
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
        // Snapshot the configured auth-keys at serve-time (§10). Operators
        // rotating the set need to restart the listener; this keeps the WS
        // upgrade callback synchronous and free of vault-lock contention.
        let auth_keys: Arc<Vec<String>> =
            Arc::new(self.vault.lock().await.config.auth_keys.clone());
        let node = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _peer)) => {
                        let node = node.clone();
                        let tls = tls.clone();
                        let keys = auth_keys.clone();
                        tokio::spawn(async move {
                            let r = match tls {
                                Some((cfg, cert_fp)) => {
                                    match tokio_rustls::TlsAcceptor::from(cfg)
                                        .accept(stream)
                                        .await
                                    {
                                        Ok(s) => {
                                            accept_ws(node, s, cert_fp.to_vec(), keys).await
                                        }
                                        Err(e) => Err(CspError::Protocol(format!(
                                            "tls accept: {e}"
                                        ))),
                                    }
                                }
                                None => accept_ws(node, stream, Vec::new(), keys).await,
                            };
                            if let Err(e) = r {
                                // Visible by default — a session that ended
                                // with an error is exactly the moment an
                                // operator needs (peer admission rejected,
                                // bad signature, vault-id mismatch). A
                                // graceful peer-close is `Ok(())`, not Err,
                                // so we never spam this on normal hangups.
                                tracing::warn!("session ended: {e}");
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
    /// `auth_key` (§10) is sent as `Authorization: Bearer …` on the WS
    /// upgrade — needed only when the listener requires enrollment and
    /// this node is not yet in its `authorized_keys`.
    pub fn connect(&self, url: String, auth_key: Option<String>) -> tokio::task::JoinHandle<()> {
        let node = self.clone();
        tokio::spawn(async move {
            tracing::info!("connecting to {url}");
            let mut warned = false;
            loop {
                match connect_once(&node, &url, auth_key.as_deref()).await {
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
/// `Hello` to learn the vault id, its human name (for clone-folder naming),
/// and its public key (to seed the source's key into the new node's local
/// authorized set — §10), then drop the connection. Returns
/// `(vault_id, name, server_ssh)`.
pub async fn probe(
    url: &str,
    identity: &Identity,
    auth_key: Option<&str>,
) -> CspResult<(String, String, String)> {
    let (mut t, _cb) = dial(url, auth_key).await?;
    t.send(&Msg::Hello {
        vault_id: String::new(),
        name: String::new(),
        node_ssh: identity.to_ssh_string(),
        nonce: rand_nonce(),
        // Probe is a connector and never authenticates — it advertises no
        // binding and only reads the listener's `Hello` back (§17).
        cb: Vec::new(),
        proto: crate::wire::PROTO_VERSION,
    })
    .await?;
    match t.recv().await {
        Some(Msg::Hello { vault_id, name, node_ssh, .. }) => {
            Ok((vault_id, name, node_ssh))
        }
        _ => Err(CspError::Protocol("probe: expected Hello".into())),
    }
}

/// Wrap a freshly accepted server-side stream (plaintext TCP *or* a
/// completed TLS stream) as a WebSocket session, validating the
/// auth-key on the upgrade request before any protocol logic runs (§10).
///
/// `auth_keys` is the listener-snapshotted set of configured pre-shared
/// keys. Empty → auth-key auth is disabled and the upgrade is unconditional.
/// Non-empty → the request must present a matching key via
/// `Authorization: Bearer …`, `Sec-WebSocket-Protocol: bearer.<key>`, or
/// `?auth_key=<key>`; **invalid → HTTP 401 with no fall-through**; absent →
/// upgrade proceeds (the connection still has to clear the ed25519 admit
/// check, which rejects unknown unenrolled peers when keys are configured).
async fn accept_ws<S>(
    node: Node,
    stream: S,
    cb: Vec<u8>,
    auth_keys: Arc<Vec<String>>,
) -> CspResult<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tracing::info!("inbound connection");
    let enrolled_flag = Arc::new(AtomicBool::new(false));
    let enrolled_flag_cb = enrolled_flag.clone();
    let keys_for_cb = auth_keys.clone();
    let callback = move |req: &WsRequest, response: WsResponse| -> Result<WsResponse, ErrorResponse> {
        if keys_for_cb.is_empty() {
            // Auth-key auth disabled — defer entirely to ed25519 admit.
            return Ok(response);
        }
        match extract_bearer_token(req) {
            None => {
                // Header absent → still allow upgrade; the pubkey path can
                // succeed for an already-enrolled peer. An unknown peer with
                // no header will be rejected at admit time anyway.
                tracing::debug!(
                    "ws upgrade: no auth-key presented; deferring to pubkey admit"
                );
                Ok(response)
            }
            Some((token, source)) => {
                let mut matched = false;
                for k in keys_for_cb.iter() {
                    if ct_eq(token.as_bytes(), k.as_bytes()) {
                        matched = true;
                        break;
                    }
                }
                if matched {
                    // Log only a short prefix — never the full token.
                    let prefix: String = token.chars().take(4).collect();
                    tracing::info!(
                        "ws upgrade: auth-key accepted via {source} (key={prefix}…)"
                    );
                    enrolled_flag_cb.store(true, Ordering::SeqCst);
                    Ok(response)
                } else {
                    tracing::warn!(
                        "ws upgrade: auth-key REJECTED via {source} — 401"
                    );
                    Err(unauthorized_response("invalid auth key"))
                }
            }
        }
    };
    let ws = tokio_tungstenite::accept_hdr_async(stream, callback)
        .await
        .map_err(|e| CspError::Protocol(format!("ws accept: {e}")))?;
    let enrolled = enrolled_flag.load(Ordering::SeqCst);
    run_session(node, Transport::from_ws(ws), Role::Listener, cb, enrolled).await
}

/// Normalize a user-supplied peer address into a canonical `ws(s)://host:port`
/// URL. Conveniences so users can paste the obvious thing:
///
/// - **Scheme optional**: a bare `example.com` is assumed to be `wss://`
///   (secure); `https`/`http` are accepted as aliases for `wss`/`ws`.
/// - **Port optional**: when the authority has no explicit port, the scheme
///   default is supplied — `443` for `wss`, `80` for `ws`.
///
/// So `example.com`, `wss://example.com`, and `https://example.com` all
/// normalize to `wss://example.com:443`. (`TcpStream::connect` rejects a
/// port-less authority with `invalid socket address` before any DNS, so the
/// default port must be filled in here.)
fn normalize_url(url: &str) -> String {
    let trimmed = url.trim();
    let (secure, rest) = if let Some(r) = trimmed.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("ws://") {
        (false, r)
    } else if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else {
        (true, trimmed)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // An IPv6 literal carries `:` inside `[...]`; its port (if any) is the
    // `:NNNN` *after* the closing bracket. For names/IPv4 a bare `:` is a port.
    let has_port = match authority.rsplit_once(']') {
        Some((_, after)) => after.starts_with(':'),
        None => authority.contains(':'),
    };
    let scheme = if secure { "wss" } else { "ws" };
    if authority.is_empty() || has_port {
        format!("{scheme}://{authority}{path}")
    } else {
        let port = if secure { 443 } else { 80 };
        format!("{scheme}://{authority}:{port}{path}")
    }
}

/// Build a client `Request` for `url` with an optional
/// `Authorization: Bearer …` header (§10 auth-key enrollment). Reuses
/// tungstenite's URL → Request conversion so all the standard headers
/// (Host, Upgrade, Sec-WebSocket-Key, …) come out right.
fn build_client_request(
    url: &str,
    auth_key: Option<&str>,
) -> CspResult<tokio_tungstenite::tungstenite::handshake::client::Request> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut req = url
        .into_client_request()
        .map_err(|e| CspError::Protocol(format!("bad ws url {url}: {e}")))?;
    if let Some(k) = auth_key {
        let v = HeaderValue::from_str(&format!("Bearer {k}"))
            .map_err(|e| CspError::Protocol(format!("auth-key header: {e}")))?;
        req.headers_mut().insert("authorization", v);
    }
    Ok(req)
}

/// Dial a peer URL, doing the TLS handshake for `wss://` (accepting any
/// server cert — trust is the application-layer ed25519 handshake, §10).
/// Returns the transport and the **channel binding**: the SHA-256 of the
/// server's TLS cert (empty for plaintext `ws://`). When `auth_key` is set,
/// it is sent as `Authorization: Bearer …` on the WS upgrade (§10).
async fn dial(url: &str, auth_key: Option<&str>) -> CspResult<(Transport, Vec<u8>)> {
    let url = normalize_url(url);
    let url = url.as_str();
    let req = build_client_request(url, auth_key)?;
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
        let (ws, _r) = tokio_tungstenite::client_async(req, tls)
            .await
            .map_err(|e| CspError::Protocol(format!("wss connect: {e}")))?;
        Ok((Transport::from_ws(ws), cb))
    } else {
        // Plaintext: open the TCP stream ourselves so we can pass the
        // header-bearing request through `client_async` (rather than the
        // URL-only `connect_async`).
        let rest = url
            .strip_prefix("ws://")
            .ok_or_else(|| CspError::Protocol(format!("unsupported scheme: {url}")))?;
        let authority = rest.split('/').next().unwrap_or(rest);
        let tcp = TcpStream::connect(authority)
            .await
            .map_err(|e| CspError::Protocol(format!("tcp connect {authority}: {e}")))?;
        let (ws, _r) = tokio_tungstenite::client_async(req, tcp)
            .await
            .map_err(|e| CspError::Protocol(format!("ws connect: {e}")))?;
        Ok((Transport::from_ws(ws), Vec::new()))
    }
}

async fn connect_once(node: &Node, url: &str, auth_key: Option<&str>) -> CspResult<()> {
    let (transport, cb) = dial(url, auth_key).await?;
    // Connector role: enrollment_authorized is meaningless (the listener
    // owns the admission decision). Always `false` here.
    run_session(node.clone(), transport, Role::Connector, cb, false).await
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

/// Everything the protocol needs from the engine, supplied by the native
/// full `Vault`. The *one* sans-IO [`Session`] (shared with the wasm/thin
/// SDK) drives this — there is exactly one protocol implementation (§16).
impl SessionVault for Vault {
    fn vault_id(&self) -> String {
        Vault::vault_id(self).to_string()
    }
    fn name(&self) -> String {
        Vault::name(self).to_string()
    }
    fn identity_ssh(&self) -> String {
        Vault::identity_ssh(self)
    }
    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        Vault::sign(self, msg)
    }
    fn frontier_tips(&self) -> CspResult<Vec<crate::oid::Oid>> {
        Vault::frontier_tips(self)
    }
    fn known(&self) -> CspResult<Vec<crate::oid::Oid>> {
        Vault::known(self)
    }
    fn has(&self, o: crate::oid::Oid) -> bool {
        self.repo().store.has(o)
    }
    fn export_closure(&self, tips: &[crate::oid::Oid]) -> CspResult<Vec<Vec<u8>>> {
        Vault::export_closure(self, tips)
    }
    fn integrate(&mut self, raws: &[Vec<u8>]) -> CspResult<usize> {
        Vault::integrate(self, raws)
    }
    fn admit_peer(&mut self, peer_ssh: &str, enrollment_authorized: bool) -> CspResult<bool> {
        Vault::admit_peer(self, peer_ssh, enrollment_authorized)
    }
}

/// Thin tokio driver over the sans-IO [`Session`] (§6). The session owns all
/// protocol logic; this only does I/O: encode/send, recv/decode, and the
/// native full-node relay bus (§6.1). Behaviour is byte-identical to the
/// pre-refactor inline handshake + loop.
async fn run_session(
    node: Node,
    mut t: Transport,
    role: Role,
    channel_binding: Vec<u8>,
    enrollment_authorized: bool,
) -> CspResult<()> {
    let mut session = Session::new(role, channel_binding, rand_nonce());
    // Listener-only: thread the WS-upgrade auth-key result into the session
    // so `on_auth` can pass it to `Vault::admit_peer` (§10 enrollment path).
    session.set_enrollment_authorized(enrollment_authorized);

    // Both sides send `Hello` immediately (§10).
    {
        let v = node.vault.lock().await;
        t.send(&session.start(&*v)).await?;
    }

    // Drive the handshake (inbound only) until established. Subscribing to
    // the relay bus *after* the handshake preserves the original ordering
    // (no `Live` can be selected mid-handshake).
    //
    // Connection admission is the load-bearing trust gate (§6.1/§10), so the
    // handshake must complete in bounded wall time — otherwise an
    // unauthenticated peer can hold a session task open indefinitely
    // (slow-loris on tokio task + Session struct + relay-channel slot). The
    // deadline is enforced as a budget across all handshake recvs, not per-
    // recv, so a peer that drip-feeds bytes still gets dropped on time.
    const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        let recv = match tokio::time::timeout_at(deadline, t.recv()).await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!(
                    "handshake did not complete within {:?} — dropping connection",
                    HANDSHAKE_TIMEOUT
                );
                return Err(CspError::Protocol("handshake timeout".into()));
            }
        };
        let Some(msg) = recv else {
            tracing::info!("peer session ended");
            return Ok(());
        };
        let step = {
            let mut v = node.vault.lock().await;
            session.on_msg(&mut *v, msg)?
        };
        for m in &step.out {
            t.send(m).await?;
        }
        if session.established() {
            break;
        }
    }
    tracing::info!("catch-up: handshake complete, frontier advertised");

    let mut relay_rx = node.bus.subscribe();
    loop {
        tokio::select! {
            biased;
            incoming = t.recv() => {
                let Some(msg) = incoming else {
                    tracing::info!("peer session ended");
                    return Ok(());
                };
                let step = {
                    let mut v = node.vault.lock().await;
                    session.on_msg(&mut *v, msg)?
                };
                for m in &step.out {
                    t.send(m).await?;
                }
                if step.integrated > 0 {
                    let main = node
                        .vault
                        .lock()
                        .await
                        .main()
                        .map(|o| o.to_hex())
                        .unwrap_or_default();
                    tracing::info!(
                        "integrated {} new primitive(s); main={}…",
                        step.integrated,
                        &main[..main.len().min(12)]
                    );
                }
                if !step.relay.is_empty() {
                    // Relay onward (§6.1). Idempotent + admitted-gated, so
                    // gossip terminates.
                    let _ = node.bus.send(step.relay);
                }
            }
            relayed = relay_rx.recv() => {
                match relayed {
                    Ok(raws) => {
                        for m in session.on_relay(raws) {
                            t.send(&m).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use tempfile::tempdir;

    fn id(s: u8) -> Identity {
        Identity::from_seed(&[s; 32])
    }

    #[test]
    fn normalize_url_assumes_wss_and_default_port() {
        // bare domain -> wss + 443
        assert_eq!(normalize_url("example.com"), "wss://example.com:443");
        // explicit scheme, no port -> scheme default port
        assert_eq!(normalize_url("wss://example.com"), "wss://example.com:443");
        assert_eq!(normalize_url("ws://example.com"), "ws://example.com:80");
        // https/http accepted as wss/ws aliases (e.g. a pasted Railway URL)
        assert_eq!(normalize_url("https://example.com"), "wss://example.com:443");
        assert_eq!(normalize_url("http://example.com"), "ws://example.com:80");
        // explicit port preserved, scheme defaulted when absent
        assert_eq!(normalize_url("wss://192.168.1.42:51820"), "wss://192.168.1.42:51820");
        assert_eq!(normalize_url("192.168.1.42:51820"), "wss://192.168.1.42:51820");
        // path preserved, port still defaulted
        assert_eq!(normalize_url("example.com/vault"), "wss://example.com:443/vault");
        assert_eq!(normalize_url("wss://example.com:7777/v"), "wss://example.com:7777/v");
        // surrounding whitespace trimmed
        assert_eq!(normalize_url("  example.com \n"), "wss://example.com:443");
        // IPv6 literal: bracket colons are not a port
        assert_eq!(normalize_url("wss://[::1]"), "wss://[::1]:443");
        assert_eq!(normalize_url("wss://[::1]:7777"), "wss://[::1]:7777");
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
            let _ = run_session(na2, t1, Role::Connector, Vec::new(), false).await;
        });
        tokio::spawn(async move {
            let _ = run_session(nb2, t2, Role::Listener, Vec::new(), false).await;
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
    async fn no_tls_listener_behind_terminating_proxy_syncs_over_duplex() {
        // End-to-end repro of the Railway failure: the listener runs
        // `--no-tls` (empty advertised binding) behind a TLS-terminating
        // edge; the connector dialed `wss://` and observed the *proxy's*
        // cert (a non-empty fingerprint it never shares a value with).
        // Pre-fix the two transcripts desynchronized and the handshake
        // died with an opaque signature error → zero sync. It must now
        // converge (degraded binding, trust via the pinned listener id).
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let va = Vault::create(ta.path(), id(1), "v").unwrap();
        let mut vb = Vault::create(tb.path(), id(2), "v").unwrap();
        va.authorize(&id(2).to_ssh_string()).unwrap();
        vb.authorize(&id(1).to_ssh_string()).unwrap();
        std::fs::write(tb.path().join("hub.md"), "HUB").unwrap();
        vb.commit_local_changes().unwrap();

        let na = Node::new(va);
        let nb = Node::new(vb);
        let (t1, t2) = Transport::pair();
        let na2 = na.clone();
        let nb2 = nb.clone();
        // Connector "saw" the proxy's TLS cert; listener is `--no-tls`
        // (empty advertised binding).
        tokio::spawn(async move {
            let _ = run_session(na2, t1, Role::Connector, vec![0xAB; 32], false).await;
        });
        tokio::spawn(async move {
            let _ = run_session(nb2, t2, Role::Listener, Vec::new(), false).await;
        });

        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if na.vault.lock().await.main() == nb.vault.lock().await.main()
                && na.vault.lock().await.main().is_some()
            {
                break;
            }
        }
        assert_eq!(
            std::fs::read_to_string(ta.path().join("hub.md")).unwrap(),
            "HUB",
            "no-tls listener behind a TLS terminator must still sync to the connector"
        );
    }

    #[tokio::test]
    async fn mismatched_channel_binding_fails_handshake() {
        // A relayed MITM that re-terminates TLS presents a cert whose
        // fingerprint differs from the one the listener advertises in
        // `Hello`. The connector observes the binding, sees it is advertised
        // *and* mismatched, and aborts with a distinct `ChannelBinding`
        // error (never an opaque signature failure) → NO sync (§10).
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
            let _ = run_session(na2, t1, Role::Connector, vec![9u8; 32], false).await;
        });
        tokio::spawn(async move {
            let _ = run_session(nb2, t2, Role::Listener, vec![8u8; 32], false).await;
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
