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
/// the two transcripts (§10). v4 introduced `Msg::ObjectsBatch` for atomic
/// chunked catch-up — v3's experimental single-`Objects` chunking (0.1.15)
/// broke catch-up admission ordering and is incompatible. v5 (issue 0014)
/// introduces (a) the `CSP-Readd: <oid>` commit-message trailer that exempts
/// legitimate re-adds from Layer 3 integrate-time ghost-add filtering, and
/// (b) Layer 3 itself — peers below v5 don't emit the trailer, so a v5 peer
/// folding their primitives would silently drop legitimate re-adds. The
/// handshake therefore refuses to peer with sub-v5 SDKs; no mixed-version
/// swarms, no fallback path.
pub const PROTO_VERSION: u32 = 5;

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
    /// Single-frame catch-up; the closure is sent atomically and the
    /// receiver integrates it in one call. Used when the closure fits
    /// comfortably in one WS frame; otherwise the sender uses
    /// [`Msg::ObjectsBatch`] (v4+).
    Objects {
        raws: Vec<Vec<u8>>,
    },
    /// Chunked catch-up payload (issue 0016, v4). The receiver accumulates
    /// `raws` from successive `ObjectsBatch` frames and integrates them as
    /// a single atomic batch when it sees `is_last: true`. Splitting
    /// `integrate` across frames is unsafe: `verify_fold_commit` walks
    /// parent commits, and a synthetic fold in chunk N whose parent is
    /// only in chunk N+1 fails verification with the wrong objects in
    /// the store — admission is dropped for that chunk, leaving an
    /// inconsistent known-set. Used by every catch-up response that
    /// closure-by-bytes exceeds a per-frame budget — iOS WKWebView's
    /// WebSocket implementation silently stalls on a multi-MB frame.
    ObjectsBatch {
        raws: Vec<Vec<u8>>,
        /// `true` on the final chunk of a batch — triggers the receiver's
        /// integrate over the accumulated buffer. `false` means "more
        /// chunks coming; just stage these bytes in your buffer".
        is_last: bool,
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
