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
    // §10 channel binding: the TLS cert fingerprint both sides observed. A
    // relayed MITM that re-terminates TLS presents a different cert, so the
    // signed transcripts no longer match. Empty for plaintext `ws://`.
    t.extend_from_slice(channel_binding);
    t
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
    /// Listener-side TOFU admission of the connector key (§10). Returns
    /// `true` if the peer is admitted.
    fn admit_peer(&mut self, peer_ssh: &str) -> CspResult<bool>;
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
#[derive(Default)]
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
}

impl Session {
    pub fn new(role: Role, channel_binding: Vec<u8>, nonce: Vec<u8>) -> Session {
        Session {
            role,
            channel_binding,
            my_nonce: nonce,
            phase: Phase::AwaitHello,
        }
    }

    /// True once the mutual-auth handshake has completed (frontier
    /// advertised) — the driver subscribes to the relay bus only after this
    /// to preserve the original message ordering.
    pub fn established(&self) -> bool {
        matches!(self.phase, Phase::Established)
    }

    /// The opening frame (both sides send `Hello` immediately, §10).
    pub fn start<V: SessionVault>(&self, v: &V) -> Msg {
        Msg::Hello {
            vault_id: v.vault_id(),
            name: v.name(),
            node_ssh: v.identity_ssh(),
            nonce: self.my_nonce.clone(),
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
        let (peer_vault, peer_ssh, peer_nonce, peer_proto) = match msg {
            Msg::Hello { vault_id, node_ssh, nonce, proto, .. } => {
                (vault_id, node_ssh, nonce, proto)
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
        let script = transcript(&client_nonce, &server_nonce, &self.channel_binding);
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

        if self.role == Role::Listener && !v.admit_peer(&peer_ssh)? {
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
                step.out.push(Msg::Objects { raws });
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
