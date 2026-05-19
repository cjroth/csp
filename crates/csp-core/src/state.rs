//! Durable engine state in `<scope>/.context/state` (§5.1, §5.6, §9.1).
//! Holds the persisted logical counter (survives restart — §5.1), the
//! observed-counter high-water mark, the known primitive-commit set, and the
//! last-materialized content hash per path (the §5.6 no-feedback-loop
//! record).
//!
//! The state *model* + its pure bookkeeping (counter, known set, observe) is
//! always-on / wasm-safe — a plugin runs the identical engine and keeps the
//! same state. Only the on-disk JSON persistence (file + cross-process lock)
//! is native (`cfg`-gated); a wasm host persists the serialized bytes.

use crate::error::CspResult;
use crate::oid::Oid;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EngineState {
    /// Vault identity (shared across replicas; chosen at `ctx init`).
    pub vault_id: String,
    /// Monotonic logical counter for this node's *own* next primitive
    /// (§5.1). Durably persisted; survives restart.
    pub counter: u64,
    /// Max counter observed from any commit (own or received). A new local
    /// commit's counter = observed + 1 (§5.1).
    pub observed: u64,
    /// Known primitive-commit oids (hex). The fold input set (§5.3).
    pub known: Vec<String>,
    /// path -> last-materialized blob content hash (hex). The authoritative
    /// "what CSP put there" record (§5.6).
    pub materialized: BTreeMap<String, String>,
    /// name -> snapshot record (frontier primitive set + label, §8).
    pub snapshots: BTreeMap<String, Snapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub label: String,
    /// Frontier primitive-commit SHA set at creation (§8).
    pub frontier: Vec<String>,
    pub created_unix: u64,
}

impl EngineState {
    pub fn known_oids(&self) -> CspResult<Vec<Oid>> {
        self.known.iter().map(|h| Oid::from_hex(h)).collect()
    }

    pub fn add_known(&mut self, oid: Oid) {
        let h = oid.to_hex();
        if !self.known.contains(&h) {
            self.known.push(h);
        }
    }

    /// Counter for this node's next primitive (§5.1): max observed + 1,
    /// persisted before use so it survives restart.
    pub fn next_counter(&mut self) -> u64 {
        let c = self.observed + 1;
        self.counter = c;
        self.observed = self.observed.max(c);
        c
    }

    pub fn observe(&mut self, counter: u64) {
        self.observed = self.observed.max(counter);
    }

    /// Serialize for host-managed persistence (the wasm SDK persists these
    /// bytes via its StorageAdapter; identical JSON to the native file).
    pub fn to_bytes(&self) -> CspResult<Vec<u8>> {
        serde_json::to_vec_pretty(self)
            .map_err(|e| crate::error::CspError::Config(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> CspResult<EngineState> {
        if bytes.is_empty() {
            return Ok(EngineState::default());
        }
        serde_json::from_slice(bytes)
            .map_err(|e| crate::error::CspError::Config(format!("state parse: {e}")))
    }
}

// ---- Native on-disk persistence (the `ctx` full node) ----------------------

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
mod disk {
    use super::EngineState;
    use crate::error::CspResult;
    use std::path::{Path, PathBuf};

    /// Releases the `.context/state.lock` exclusive lock on drop.
    struct LockGuard<'a>(&'a std::fs::File);
    impl Drop for LockGuard<'_> {
        fn drop(&mut self) {
            let _ = fs2::FileExt::unlock(self.0);
        }
    }

    impl EngineState {
        pub fn path(context_dir: &Path) -> PathBuf {
            context_dir.join("state")
        }

        pub fn load(context_dir: &Path) -> CspResult<EngineState> {
            let p = Self::path(context_dir);
            if !p.exists() {
                return Ok(EngineState::default());
            }
            let bytes = std::fs::read(&p)?;
            EngineState::from_bytes(&bytes)
        }

        pub fn save(&self, context_dir: &Path) -> CspResult<()> {
            let p = Self::path(context_dir);
            // Cross-process exclusive lock: a one-shot (`ctx snapshot`/
            // `restore`) and a running `ctx watch` daemon are two writers of
            // `.context/state`. Serialize the whole load→merge→write so
            // neither clobbers the other (`.context/` is never synced, §11).
            let lock_path = context_dir.join("state.lock");
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)?;
            fs2::FileExt::lock_exclusive(&lock)?;
            let _guard = LockGuard(&lock);

            // Merge-on-save for the *additive* maps (snapshots + known set
            // are monotonic; counters take the max). Materialized hashes are
            // writer-owned (last-writer-wins is fine — worst case a re-scan).
            let mut merged = self.clone();
            if let Ok(disk) = Self::load(context_dir) {
                for (k, v) in disk.snapshots {
                    merged.snapshots.entry(k).or_insert(v);
                }
                for h in disk.known {
                    if !merged.known.contains(&h) {
                        merged.known.push(h);
                    }
                }
                merged.observed = merged.observed.max(disk.observed);
                merged.counter = merged.counter.max(disk.counter);
            }
            let tmp = p.with_extension("tmp");
            std::fs::write(&tmp, merged.to_bytes()?)?;
            std::fs::rename(&tmp, &p)?;
            Ok(())
        }
    }
}
