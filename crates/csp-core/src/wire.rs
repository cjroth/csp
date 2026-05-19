//! Wire framing & message kinds (§6.2/§6.6). Pure serde + MessagePack — no
//! tokio, no sockets — so it is part of the **reduced wasm/thin surface**
//! (§4/§7): a thin node speaks the protocol; it just never computes the
//! merge. Binary length-delimited frames are provided by the transport
//! (WebSocket); each frame is a MessagePack-encoded [`Msg`].

use serde::{Deserialize, Serialize};

/// Wire/handshake protocol version. Bump on any change to the handshake
/// transcript or message framing so version skew is reported clearly
/// instead of surfacing as an opaque signature error. v2 added §10
/// channel binding (TLS cert fingerprint in the transcript); v3 made the
/// binding **listener-advertised** (`Hello.cb`) and signed over that single
/// agreed value, so a TLS-terminating front proxy no longer desynchronizes
/// the two transcripts (§10).
pub const PROTO_VERSION: u32 = 3;

fn proto_default() -> u32 {
    // An old peer's `Hello` has no `proto` field → 0 → reported as skew.
    0
}

/// §6.6 message kinds. `FrontierDigest`/`WantTips` is the authoritative
/// reconciliation (§6.4); a scalar version vector is deliberately *not* the
/// correctness mechanism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Msg {
    Hello {
        vault_id: String,
        /// Human label (may be empty) — for display / clone-folder naming.
        #[serde(default)]
        name: String,
        node_ssh: String,
        nonce: Vec<u8>,
        /// The **listener's advertised channel binding** (§10): the SHA-256
        /// of the TLS certificate it serves, or all-zero/empty when it runs
        /// `--no-tls` behind a TLS terminator ("binding disabled"). Both
        /// sides sign the transcript over this single advertised value; the
        /// connector additionally checks it against the cert it observed.
        /// A connector's `Hello` leaves this empty (it advertises nothing).
        #[serde(default)]
        cb: Vec<u8>,
        #[serde(default = "proto_default")]
        proto: u32,
    },
    AuthProof {
        sig: Vec<u8>,
    },
    /// The un-merged primitive tip SHAs (one per concurrent lineage, §6.4).
    FrontierDigest {
        tips: Vec<String>,
    },
    WantTips {
        tips: Vec<String>,
    },
    /// Reachable closures of requested tips — verified, not trusted (§6.3).
    Objects {
        raws: Vec<Vec<u8>>,
    },
    /// A live push of new primitive closures (§6.5).
    Live {
        raws: Vec<Vec<u8>>,
    },
    Ping,
    Pong,
}

impl Msg {
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        rmp_serde::to_vec_named(self).map_err(|e| e.to_string())
    }
    pub fn decode(bytes: &[u8]) -> Result<Msg, String> {
        rmp_serde::from_slice(bytes).map_err(|e| e.to_string())
    }
}
