//! Sans-IO replication session (§6, §10) — the *one* protocol state machine,
//! shared verbatim by the native driver (`net.rs`, tokio WebSockets) and the
//! wasm/thin SDK (host-supplied transport). No tokio, no sockets, no
//! filesystem: it consumes decoded [`Msg`]s and emits [`Msg`]s plus
//! side-effect requests. Part of the reduced surface (§4/§7) — a thin node
//! speaks the exact same protocol; it just never computes the merge.
//!
//! Behaviour is byte-for-byte the pre-refactor `net.rs` handshake +
//! frontier anti-entropy (§6.4) + integrate (§6.3); the driver only does I/O.

use crate::error::{CspError, CspResult};
use crate::identity::{parse_ssh_pubkey, verify_detached};
use crate::oid::Oid;
use crate::wire::{Msg, PROTO_VERSION};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Listener,
    Connector,
}

const AUTH_CTX: &[u8] = b"csp-auth-v1";

fn transcript(client_nonce: &[u8], server_nonce: &[u8], channel_binding: &[u8]) -> Vec<u8> {
    let mut t = AUTH_CTX.to_vec();
    t.extend_from_slice(client_nonce);
    t.extend_from_slice(server_nonce);
    // §10 channel binding: the **listener-advertised** TLS cert fingerprint
    // (`Hello.cb`). Both sides sign over this one agreed value — *not* each
    // side's local view — so a benign TLS-terminating front proxy (which
    // makes the connector see the proxy's cert and the listener its own /
    // none) no longer desynchronizes the two transcripts. The connector
    // separately enforces this value against the cert it observed, as an
    // explicit check with a distinct error (see `on_hello`). Empty/all-zero
    // = "binding disabled" (`--no-tls`).
    t.extend_from_slice(channel_binding);
    t
}

/// A channel binding is "disabled" — the listener opted out via `--no-tls`
/// behind a TLS terminator (§10) — when it is empty or all-zero. In that
/// mode the connector skips the certificate comparison and trust falls back
/// to the TOFU-pinned listener identity carried in the transcript, which a
/// MITM cannot forge.
fn is_binding_disabled(cb: &[u8]) -> bool {
    cb.is_empty() || cb.iter().all(|b| *b == 0)
}

/// Everything the protocol needs from the engine. Implemented by the native
/// full `Vault` and (later) the wasm thin vault — the *same* `Session`
/// drives both, so there is exactly one protocol implementation (§16).
pub trait SessionVault {
    fn vault_id(&self) -> String;
    /// Human label (may be empty) — carried in `Hello` for display /
    /// clone-folder naming; never a uniqueness guarantee (that is vault_id).
    fn name(&self) -> String;
    fn identity_ssh(&self) -> String;
    /// Detached ed25519 sign of the handshake transcript (§10).
    fn sign(&self, msg: &[u8]) -> Vec<u8>;
    /// Un-merged primitive tips for anti-entropy (§6.4).
    fn frontier_tips(&self) -> CspResult<Vec<Oid>>;
    /// Known primitive set (the fold input set, §5.3).
    fn known(&self) -> CspResult<Vec<Oid>>;
    /// Is this object present in the local store?
    fn has(&self, o: Oid) -> bool;
    /// Raw reachable closure of `tips` (§6.4 delivery unit).
    fn export_closure(&self, tips: &[Oid]) -> CspResult<Vec<Vec<u8>>>;
    /// Integrate received raw objects (§6.3); returns new primitives admitted.
    fn integrate(&mut self, raws: &[Vec<u8>]) -> CspResult<usize>;
    /// Listener-side connection admission of the connector key (§10).
    /// Returns `true` if the peer is admitted.
    ///
    /// `enrollment_authorized` is set by the driver (the WS layer in
    /// `net.rs` on native; always `false` for outbound-only thin nodes)
    /// when the connection presented a valid pre-shared auth key in its
    /// `Authorization: Bearer …` upgrade header (or fallback form). The
    /// implementation may then enroll the peer's pubkey into the local
    /// authorized set with a default TTL — see `Vault::admit_peer` for
    /// the full decision table.
    fn admit_peer(&mut self, peer_ssh: &str, enrollment_authorized: bool) -> CspResult<bool>;
}

enum Phase {
    /// Sent our `Hello`, awaiting the peer's `Hello`.
    AwaitHello,
    /// Exchanged `Hello`, sent our `AuthProof`, awaiting the peer's.
    AwaitAuth { script: Vec<u8>, peer_ssh: String },
    /// Handshake complete; frontier anti-entropy + live (§6.4/§6.5).
    Established,
}

/// Side effects the driver must perform after a [`Session::on_msg`] step:
/// frames to send, raw closures to relay to other peers (§6.1 — native
/// full-node concern; empty for a thin node), and how many primitives were
/// integrated (for logging / host materialize triggers).
#[derive(Default, Debug)]
pub struct Step {
    pub out: Vec<Msg>,
    pub relay: Vec<Vec<u8>>,
    pub integrated: usize,
}

/// The sans-IO session. `start()` → the opening `Hello`; then feed every
/// inbound [`Msg`] to [`on_msg`](Session::on_msg) and perform the returned
/// [`Step`]. Mutual auth (§10): each side signs a transcript over both
/// nonces + the channel binding; both directions verify.
pub struct Session {
    role: Role,
    channel_binding: Vec<u8>,
    my_nonce: Vec<u8>,
    phase: Phase,
    /// The peer's SSH-format pubkey (`ssh-ed25519 …`), set once the mutual
    /// auth completes (CSP §10). Hosts use this to surface a pinned-peer
    /// indicator without re-deriving it from the handshake messages.
    peer_ssh: Option<String>,
    /// Set by the driver (`net::run_session` on native) when the WS upgrade
    /// validated a pre-shared auth key (§10). Consumed in `on_auth` and
    /// passed to `SessionVault::admit_peer`. Always `false` for connectors
    /// (the listener owns the admission decision).
    enrollment_authorized: bool,
}

impl Session {
    pub fn new(role: Role, channel_binding: Vec<u8>, nonce: Vec<u8>) -> Session {
        Session {
            role,
            channel_binding,
            my_nonce: nonce,
            phase: Phase::AwaitHello,
            peer_ssh: None,
            enrollment_authorized: false,
        }
    }

    /// Mark this listener-side session as having presented a valid auth-key
    /// at the WS upgrade (§10). Must be set before `on_msg` reaches
    /// `on_auth`. No effect when the role is `Connector`.
    pub fn set_enrollment_authorized(&mut self, ok: bool) {
        self.enrollment_authorized = ok;
    }

    /// True once the mutual-auth handshake has completed (frontier
    /// advertised) — the driver subscribes to the relay bus only after this
    /// to preserve the original message ordering.
    pub fn established(&self) -> bool {
        matches!(self.phase, Phase::Established)
    }

    /// The peer's SSH-format pubkey once the handshake has completed.
    /// `None` before `established()` flips true.
    pub fn peer_ssh(&self) -> Option<&str> {
        self.peer_ssh.as_deref()
    }

    /// The opening frame (both sides send `Hello` immediately, §10).
    pub fn start<V: SessionVault>(&self, v: &V) -> Msg {
        Msg::Hello {
            vault_id: v.vault_id(),
            name: v.name(),
            node_ssh: v.identity_ssh(),
            nonce: self.my_nonce.clone(),
            // Only the listener advertises a binding (the cert it serves, or
            // empty under `--no-tls`); the connector advertises nothing —
            // it *verifies* the listener's value against what it observed.
            cb: match self.role {
                Role::Listener => self.channel_binding.clone(),
                Role::Connector => Vec::new(),
            },
            proto: PROTO_VERSION,
        }
    }

    /// Feed one decoded inbound frame. Pure protocol logic — identical to the
    /// pre-refactor `net.rs` `handshake`/`handle_msg`.
    pub fn on_msg<V: SessionVault>(&mut self, v: &mut V, msg: Msg) -> CspResult<Step> {
        match &self.phase {
            Phase::AwaitHello => self.on_hello(v, msg),
            Phase::AwaitAuth { .. } => self.on_auth(v, msg),
            Phase::Established => self.on_established(v, msg),
        }
    }

    /// A relayed closure from another peer (§6.1) → a `Live` push. Native
    /// full-node relay only; a thin node never calls this.
    pub fn on_relay(&self, raws: Vec<Vec<u8>>) -> Vec<Msg> {
        vec![Msg::Live { raws }]
    }

    fn on_hello<V: SessionVault>(&mut self, v: &mut V, msg: Msg) -> CspResult<Step> {
        let (peer_vault, peer_ssh, peer_nonce, peer_cb, peer_proto) = match msg {
            Msg::Hello { vault_id, node_ssh, nonce, cb, proto, .. } => {
                (vault_id, node_ssh, nonce, cb, proto)
            }
            _ => return Err(CspError::Protocol("expected Hello".into())),
        };
        if peer_proto != PROTO_VERSION {
            return Err(CspError::Protocol(format!(
                "protocol version mismatch: peer v{peer_proto} != ours v{PROTO_VERSION}"
            )));
        }
        let my_vault = v.vault_id();
        if peer_vault != my_vault {
            return Err(CspError::Protocol(format!(
                "vault id mismatch: {peer_vault} != {my_vault}"
            )));
        }
        let (client_nonce, server_nonce) = match self.role {
            Role::Connector => (self.my_nonce.clone(), peer_nonce),
            Role::Listener => (peer_nonce, self.my_nonce.clone()),
        };
        // The binding mixed into the signed transcript is the listener's
        // single advertised value (§10), so both sides sign identical bytes
        // even with a TLS-terminating proxy in front of the listener.
        let agreed_binding = match self.role {
            // The listener signs over exactly what it advertised in its own
            // `Hello` (== self.channel_binding).
            Role::Listener => self.channel_binding.clone(),
            Role::Connector => {
                let advertised = peer_cb;
                let observed = &self.channel_binding;
                if is_binding_disabled(&advertised) {
                    // Listener opted out (`--no-tls` behind a TLS
                    // terminator, e.g. Fly/Railway). Degraded: trust falls
                    // back to the pinned listener identity (the transcript
                    // also covers peer_ssh, which a MITM cannot forge).
                    advertised
                } else if observed.is_empty() {
                    // Cert unobservable here (plaintext `ws://`, or a
                    // browser WebSocket — §7). Bind to the advertised value
                    // and rely on the pinned listener identity; the cert
                    // itself cannot be checked at this layer.
                    advertised
                } else if observed.as_slice() != advertised.as_slice() {
                    // Binding advertised AND observable AND different: a
                    // re-terminating proxy or live MITM. Fail with a
                    // *distinct* error, never an opaque signature failure.
                    return Err(CspError::ChannelBinding(format!(
                        "listener advertised a TLS cert fingerprint ({} bytes) \
                         that does not match the certificate this connection \
                         observed ({} bytes) — a re-terminating proxy or MITM \
                         is in the path. If the listener is behind a trusted \
                         TLS-terminating proxy, run it with --no-tls / \
                         CTX_NO_TLS so it advertises a disabled binding",
                        advertised.len(),
                        observed.len()
                    )));
                } else {
                    // Observed == advertised: channel binding fully verified.
                    advertised
                }
            }
        };
        let script = transcript(&client_nonce, &server_nonce, &agreed_binding);
        let sig = v.sign(&script);
        self.phase = Phase::AwaitAuth { script, peer_ssh };
        Ok(Step {
            out: vec![Msg::AuthProof { sig }],
            ..Default::default()
        })
    }

    fn on_auth<V: SessionVault>(&mut self, v: &mut V, msg: Msg) -> CspResult<Step> {
        let (script, peer_ssh) = match &self.phase {
            Phase::AwaitAuth { script, peer_ssh } => (script.clone(), peer_ssh.clone()),
            _ => unreachable!(),
        };
        let peer_sig = match msg {
            Msg::AuthProof { sig } => sig,
            _ => return Err(CspError::Protocol("expected AuthProof".into())),
        };
        let peer_node = parse_ssh_pubkey(&peer_ssh)
            .ok_or_else(|| CspError::BadSignature("bad peer ssh key".into()))?;
        verify_detached(&peer_node, &script, &peer_sig)?;

        if self.role == Role::Listener
            && !v.admit_peer(&peer_ssh, self.enrollment_authorized)?
        {
            return Err(CspError::Unauthorized(format!(
                "peer {} not authorized",
                &peer_node.to_hex()[..12]
            )));
        }

        // Handshake done → advertise our frontier (§6.4 catch-up kickoff).
        let tips = v
            .frontier_tips()?
            .into_iter()
            .map(|o| o.to_hex())
            .collect::<Vec<_>>();
        self.peer_ssh = Some(peer_ssh);
        self.phase = Phase::Established;
        Ok(Step {
            out: vec![Msg::FrontierDigest { tips }],
            ..Default::default()
        })
    }

    fn on_established<V: SessionVault>(&mut self, v: &mut V, msg: Msg) -> CspResult<Step> {
        let mut step = Step::default();
        match msg {
            Msg::FrontierDigest { tips } => {
                let known = v.known()?;
                let mut want = Vec::new();
                for hex in tips {
                    if let Ok(o) = Oid::from_hex(&hex) {
                        if !v.has(o) || !known.contains(&o) {
                            want.push(hex);
                        }
                    }
                }
                if !want.is_empty() {
                    step.out.push(Msg::WantTips { tips: want });
                }
            }
            Msg::WantTips { tips } => {
                let oids: Vec<Oid> = tips.iter().filter_map(|h| Oid::from_hex(h).ok()).collect();
                let raws = v.export_closure(&oids)?;
                // Chunk the catch-up payload into frames the smallest
                // peer transport in the field can accept (issue 0015).
                // A 450-file vault's full ancestral closure is tens of MB
                // packed as one `Msg::Objects` — iOS WKWebView's
                // WebSocket implementation silently stalls on a frame
                // that large, manifesting as "server logs `frontier
                // advertised` then nothing for ~50 s, then `peer session
                // ended` from the TCP-level timeout". The receiver-side
                // integrate is already incremental + content-addressed
                // dedup, so splitting changes nothing semantically; only
                // the wire shape changes.
                //
                // 256 KiB per frame is comfortable for every transport
                // we've shipped against (Electron, iOS WKWebView,
                // Android WebView, the relay, the native tungstenite
                // client). It also keeps the receive-side recompute
                // amortized — recompute runs once per admitted Objects
                // frame, but each frame's known-set delta is bounded
                // and the fold cost stays linear.
                for chunk in chunk_objects(raws, CATCHUP_CHUNK_BYTES) {
                    step.out.push(Msg::Objects { raws: chunk });
                }
            }
            Msg::Objects { raws } | Msg::Live { raws } => {
                let admitted = v.integrate(&raws)?;
                step.integrated = admitted;
                if admitted > 0 {
                    // Relay onward (§6.1). Idempotent + admitted-gated, so
                    // gossip terminates. (Native full node only; the driver
                    // decides whether it has peers to relay to.)
                    step.relay = raws;
                }
            }
            Msg::Ping => step.out.push(Msg::Pong),
            Msg::Pong => {}
            Msg::Hello { .. } | Msg::AuthProof { .. } => {
                return Err(CspError::Protocol(
                    "unexpected handshake msg mid-session".into(),
                ));
            }
        }
        Ok(step)
    }
}

/// Target per-frame size for catch-up `Objects` chunking (issue 0015).
/// Set to 256 KiB — the largest payload every transport in the field
/// (iOS WKWebView, Android WebView, Electron, native tungstenite, the
/// relay) accepts comfortably; a single multi-MB frame stalls iOS's
/// WebSocket implementation indefinitely without an error path. A small
/// number of overflow bytes is tolerated (the chunker won't split a
/// single object), so individual very-large blobs still ride as their
/// own frame.
pub(crate) const CATCHUP_CHUNK_BYTES: usize = 256 * 1024;

/// Split a vector of raw objects into vectors whose summed byte length
/// stays under `limit`, packing greedily. The packing is order-preserving
/// (raws → chunks → integrate order) so the receiver's content-addressed
/// dedup behaves identically to the single-frame path. An object larger
/// than `limit` rides alone in its own frame.
pub(crate) fn chunk_objects(raws: Vec<Vec<u8>>, limit: usize) -> Vec<Vec<Vec<u8>>> {
    let mut out: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut current: Vec<Vec<u8>> = Vec::new();
    let mut current_bytes: usize = 0;
    for r in raws {
        let r_len = r.len();
        if !current.is_empty() && current_bytes + r_len > limit {
            out.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += r_len;
        current.push(r);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    /// Minimal `SessionVault` that signs with a real ed25519 key, so the
    /// transcript signatures genuinely verify when (and only when) both
    /// sides built identical transcripts.
    struct MockVault {
        id: Identity,
    }
    impl MockVault {
        fn new(seed: u8) -> MockVault {
            MockVault { id: Identity::from_seed(&[seed; 32]) }
        }
    }
    impl SessionVault for MockVault {
        fn vault_id(&self) -> String {
            "VID".into()
        }
        fn name(&self) -> String {
            String::new()
        }
        fn identity_ssh(&self) -> String {
            self.id.to_ssh_string()
        }
        fn sign(&self, msg: &[u8]) -> Vec<u8> {
            self.id.sign(msg)
        }
        fn frontier_tips(&self) -> CspResult<Vec<Oid>> {
            Ok(vec![])
        }
        fn known(&self) -> CspResult<Vec<Oid>> {
            Ok(vec![])
        }
        fn has(&self, _o: Oid) -> bool {
            false
        }
        fn export_closure(&self, _t: &[Oid]) -> CspResult<Vec<Vec<u8>>> {
            Ok(vec![])
        }
        fn integrate(&mut self, _r: &[Vec<u8>]) -> CspResult<usize> {
            Ok(0)
        }
        fn admit_peer(&mut self, _p: &str, _enrolled: bool) -> CspResult<bool> {
            Ok(true)
        }
    }

    /// Drive the two-message handshake between a connector that *observed*
    /// `conn_observed_cb` at its TLS layer and a listener that *advertises*
    /// `listener_cb` in its `Hello`. `Ok(())` iff both sides reach
    /// Established (i.e. both transcripts matched and verified).
    fn handshake(conn_observed_cb: Vec<u8>, listener_cb: Vec<u8>) -> CspResult<()> {
        let mut cv = MockVault::new(1);
        let mut lv = MockVault::new(2);
        let mut c = Session::new(Role::Connector, conn_observed_cb, vec![7u8; 32]);
        let mut l = Session::new(Role::Listener, listener_cb, vec![9u8; 32]);
        let hello_c = c.start(&cv);
        let hello_l = l.start(&lv);
        let step_c = c.on_msg(&mut cv, hello_l)?;
        let step_l = l.on_msg(&mut lv, hello_c)?;
        let auth_c = step_c.out.into_iter().next().expect("connector AuthProof");
        let auth_l = step_l.out.into_iter().next().expect("listener AuthProof");
        let sc = c.on_msg(&mut cv, auth_l)?;
        let sl = l.on_msg(&mut lv, auth_c)?;
        assert!(matches!(sc.out.first(), Some(Msg::FrontierDigest { .. })));
        assert!(matches!(sl.out.first(), Some(Msg::FrontierDigest { .. })));
        assert!(c.established() && l.established());
        Ok(())
    }

    #[test]
    fn no_tls_listener_behind_terminating_proxy_converges() {
        // The exact Railway failure: the listener runs `--no-tls` (empty /
        // disabled advertised binding) behind a TLS-terminating edge; the
        // connector dialed `wss://` and observed the *proxy's* cert.
        // Pre-fix this desynchronized the transcripts and surfaced as an
        // opaque "Verification equation was not satisfied".
        handshake(vec![0xAB; 32], Vec::new()).expect("no-tls degraded must converge");
        // An all-zero advertised binding is equivalent to empty.
        handshake(vec![0xAB; 32], vec![0u8; 32]).expect("all-zero == disabled");
    }

    #[test]
    fn end_to_end_tls_matching_cert_converges() {
        handshake(vec![7u8; 32], vec![7u8; 32]).expect("verified binding must converge");
    }

    #[test]
    fn unobservable_transport_degrades_to_identity_pin() {
        // Connector cannot read the peer cert (plaintext `ws://` or a
        // browser WebSocket, §7) but the listener advertises one: bind to
        // the advertised value, converge, rely on the pinned identity.
        handshake(Vec::new(), vec![5u8; 32]).expect("unobservable must degrade, not fail");
    }

    #[test]
    fn cert_substitution_fails_with_distinct_channel_binding_error() {
        // Observable AND advertised AND different = re-terminating proxy or
        // live MITM. MUST be a distinct ChannelBinding error — never an
        // opaque BadSignature.
        let err = handshake(vec![2u8; 32], vec![1u8; 32]).unwrap_err();
        assert!(
            matches!(err, CspError::ChannelBinding(_)),
            "expected ChannelBinding, got {err:?}"
        );
    }

    // ---- Catch-up Objects chunker (issue 0015) ----

    #[test]
    fn chunk_objects_empty_input_yields_no_frames() {
        // A peer whose frontier matches ours (nothing to send) must not
        // emit a stray empty Objects frame — the receiver would integrate
        // 0, log nothing, and silently mask a real catch-up.
        assert!(chunk_objects(vec![], 1024).is_empty());
    }

    #[test]
    fn chunk_objects_packs_greedily_under_the_limit() {
        // 3 × 100 B objects, 256 B limit → 2 frames (200 + 100). The
        // packing order is preserved so the receiver's content-addressed
        // dedup sees objects in the same sequence as the single-frame
        // path.
        let raws: Vec<Vec<u8>> = vec![vec![1u8; 100], vec![2u8; 100], vec![3u8; 100]];
        let chunks = chunk_objects(raws, 256);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 1);
        assert_eq!(chunks[0][0][0], 1);
        assert_eq!(chunks[0][1][0], 2);
        assert_eq!(chunks[1][0][0], 3);
    }

    #[test]
    fn chunk_objects_single_oversize_object_rides_alone() {
        // A 10 KB object with a 1 KB limit must NOT be split (we never
        // fragment a content-addressed object — the receiver hashes the
        // whole thing). It rides in its own frame.
        let raws = vec![vec![1u8; 100], vec![9u8; 10_000], vec![2u8; 100]];
        let chunks = chunk_objects(raws, 1024);
        // Expected: [small_a], [big], [small_b] (or [small_a, ?], depends
        // on packing semantics). We pack greedily but only flush when
        // EXISTING content would overflow — so the big object joins the
        // current chunk if alone there, then forces the next.
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), 3);
        // The big object must appear intact in exactly one chunk.
        let big_locations: Vec<usize> =
            chunks.iter().enumerate().filter_map(|(i, c)| c.iter().find(|o| o.len() == 10_000).map(|_| i)).collect();
        assert_eq!(big_locations.len(), 1);
    }

    #[test]
    fn chunk_objects_total_object_count_is_preserved() {
        // No object is dropped or duplicated — the chunker is a pure
        // re-packing.
        let raws: Vec<Vec<u8>> = (0..50).map(|i| vec![i as u8; 1000]).collect();
        let chunks = chunk_objects(raws.clone(), 4096);
        let flattened: Vec<Vec<u8>> = chunks.into_iter().flatten().collect();
        assert_eq!(flattened.len(), 50);
        // Order preserved (first byte == index).
        for (i, o) in flattened.iter().enumerate() {
            assert_eq!(o[0], i as u8, "object {i} out of order");
        }
    }

    #[test]
    fn want_tips_handler_emits_multiple_frames_for_a_big_closure() {
        // Stand in a SessionVault whose export_closure returns a closure
        // bigger than one chunk. The on_msg dispatcher must emit
        // multiple Msg::Objects frames, not one fused giant frame.
        struct ChunkyVault {
            id: Identity,
            closure: Vec<Vec<u8>>,
        }
        impl SessionVault for ChunkyVault {
            fn vault_id(&self) -> String {
                "VID".into()
            }
            fn name(&self) -> String {
                String::new()
            }
            fn identity_ssh(&self) -> String {
                self.id.to_ssh_string()
            }
            fn sign(&self, msg: &[u8]) -> Vec<u8> {
                self.id.sign(msg)
            }
            fn frontier_tips(&self) -> CspResult<Vec<Oid>> {
                Ok(vec![])
            }
            fn known(&self) -> CspResult<Vec<Oid>> {
                Ok(vec![])
            }
            fn has(&self, _o: Oid) -> bool {
                false
            }
            fn export_closure(&self, _t: &[Oid]) -> CspResult<Vec<Vec<u8>>> {
                Ok(self.closure.clone())
            }
            fn integrate(&mut self, _r: &[Vec<u8>]) -> CspResult<usize> {
                Ok(0)
            }
            fn admit_peer(&mut self, _p: &str, _enrolled: bool) -> CspResult<bool> {
                Ok(true)
            }
        }
        // 100 objects × ~8 KB each = ~800 KB closure → at 256 KB limit,
        // 3-4 chunks. Use a dummy tip hex (32 bytes hex == 64 chars).
        let closure: Vec<Vec<u8>> = (0..100).map(|i| vec![i as u8; 8 * 1024]).collect();
        let mut v = ChunkyVault {
            id: Identity::from_seed(&[1u8; 32]),
            closure,
        };
        let mut s = Session::new(Role::Connector, Vec::new(), vec![7u8; 32]);
        s.phase = Phase::Established;
        s.peer_ssh = Some(Identity::from_seed(&[2u8; 32]).to_ssh_string());
        let want = Msg::WantTips { tips: vec!["a".repeat(40)] };
        let step = s.on_msg(&mut v, want).unwrap();
        assert!(step.out.len() >= 3, "expected multi-frame Objects, got {}", step.out.len());
        for m in &step.out {
            match m {
                Msg::Objects { raws } => {
                    let bytes: usize = raws.iter().map(|r| r.len()).sum();
                    assert!(
                        bytes <= CATCHUP_CHUNK_BYTES + 8 * 1024,
                        "chunk too large: {bytes}"
                    );
                }
                other => panic!("expected Objects, got {other:?}"),
            }
        }
        // Total objects preserved across all chunks.
        let total: usize = step
            .out
            .iter()
            .map(|m| match m {
                Msg::Objects { raws } => raws.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(total, 100);
    }

    #[test]
    fn proto_skew_reports_clearly_not_as_signature_error() {
        // A stale peer (older proto) must surface as a clear
        // version-mismatch Protocol error, not an opaque signature failure.
        let mut lv = MockVault::new(2);
        let mut l = Session::new(Role::Listener, Vec::new(), vec![9u8; 32]);
        let stale = Msg::Hello {
            vault_id: "VID".into(),
            name: String::new(),
            node_ssh: Identity::from_seed(&[1u8; 32]).to_ssh_string(),
            nonce: vec![7u8; 32],
            cb: Vec::new(),
            proto: PROTO_VERSION - 1,
        };
        match l.on_msg(&mut lv, stale).unwrap_err() {
            CspError::Protocol(m) => {
                assert!(m.contains("protocol version mismatch"), "{m}")
            }
            other => panic!("expected Protocol version mismatch, got {other:?}"),
        }
    }
}
