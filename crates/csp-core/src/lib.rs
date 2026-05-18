//! csp-core — the entire Context Sync Protocol engine (§16). One codebase,
//! conditionally compiled: the merge/fold engine + on-disk odb are native /
//! full-node only; the wasm/thin profile gets object encode/decode + the
//! sync state machine + auth + framing (§4, §7). All protocol, merge, and
//! convergence logic lives here and nowhere else.

pub mod error;
pub mod fold;
pub mod identity;
pub mod object;
pub mod oid;
pub mod order;
pub mod scope;
pub mod store;
pub mod wire;

// The 3-way merge engine is native/full-node only (§4, §7, §16): compiled
// OUT of the wasm/thin profile.
#[cfg(feature = "full")]
pub mod merge;

// The on-disk odb (`repo`/`state`/`DiskStore`), the high-level `vault`, and
// the native sync driver `net` are full-node only (§4, §7, §16): native
// target *and* the `full` feature.
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod repo;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod state;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod vault;

pub use error::{CspError, CspResult};
pub use fold::{frontier, genesis, reachable, thin_wrapper};
#[cfg(feature = "full")]
pub use fold::{compute_main, verify_fold_commit};
pub use identity::{build_primitive, parse_ssh_pubkey, verify_primitive, Identity};
pub use wire::Msg;
pub use object::{CommitObj, GitObject, TreeEntry};
pub use oid::Oid;
pub use order::{NodeId, OrderKey};
pub use store::{MemStore, Store};

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod net;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub mod tls;

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use store::DiskStore;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use vault::{Vault, VaultConfig};
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use repo::Repo;
#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use net::{Node, Role};
