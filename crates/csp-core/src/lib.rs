//! csp-core — the entire Context Sync Protocol engine (§16). **One engine
//! everywhere:** the object codec, identity/auth, wire framing, the sans-IO
//! sync `Session`, *and the deterministic 3-way merge/fold* all compile to
//! every target including `wasm32` — so a plugin computes its own
//! byte-identical `main` exactly like `ctx` (§5.4 holds by construction; the
//! cross-surface conformance suite is the headline guarantee). Only
//! genuinely platform-bound pieces are `cfg`-gated: the on-disk odb
//! (`repo`/`state`/`DiskStore`), the tokio socket driver (`net`), and `tls`
//! are native-only. Tiering is *listen/relay capability + retention*, never
//! *merge capability*. All protocol/merge/convergence logic lives here and
//! nowhere else.

// The vault-config model + TOML (de)serialization is always-on / wasm-safe
// (a plugin shares the exact `.context/config` bytes with `ctx`, §9.1); only
// its on-disk file I/O is `cfg`-gated inside the module.
pub mod config;
// The sans-IO, wasm-safe full engine — the *same* protocol/merge/fold core
// as native `vault`, but files-in / materialize-ops-out (no fs, no sockets).
// What a plugin drives so it computes its own byte-identical `main` (§16).
pub mod engine;
pub mod error;
pub mod fold;
pub mod identity;
pub mod merge;
pub mod object;
pub mod oid;
pub mod order;
pub mod scope;
// The sans-IO replication session (§6, §10) — the one protocol state
// machine, shared verbatim by the native driver (`net`) and the wasm SDK.
pub mod session;
pub mod store;
pub mod wire;

// On-disk odb (`repo`/`state`/`DiskStore`), the high-level on-disk `vault`,
// the tokio socket driver (`net`), and `tls` are the only genuinely
// platform-bound pieces: native target *and* the `full` feature. The
// merge/fold engine is NOT here — it is in the always-on surface above so
// every node (incl. wasm plugins) computes its own deterministic `main`.
// The engine-state *model* + pure bookkeeping is always-on / wasm-safe (a
// plugin runs the identical engine and keeps the same state); only its
// on-disk JSON persistence is `cfg`-gated inside the module.
pub mod state;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod repo;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod vault;

pub use error::{CspError, CspResult};
pub use fold::{compute_main, frontier, genesis, reachable, thin_wrapper, verify_fold_commit};
pub use session::{Role, Session, SessionVault, Step};
pub use identity::{build_primitive, parse_ssh_pubkey, verify_primitive, Identity};
pub use wire::Msg;
pub use object::{CommitObj, GitObject, TreeEntry};
pub use oid::Oid;
pub use config::VaultConfig;
pub use engine::{MaterializeOp, MemEngine};
pub use order::{NodeId, OrderKey};
pub use state::{EngineState, Snapshot};
pub use store::{MemStore, Store};

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod net;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod tls;

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use store::DiskStore;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use vault::Vault;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use repo::Repo;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use net::Node;
