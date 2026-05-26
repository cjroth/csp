//! Sans-IO, wasm-safe engine (§16) — the *same* protocol/merge/fold/scope/
//! identity/session core as the native `vault`, but with **no filesystem and
//! no sockets**: working files are passed in, materialize ops are returned,
//! and persistence is host-managed bytes. This is what a plugin (via
//! `csp-wasm` + `@csp/sdk`) drives so it computes its **own byte-identical
//! `main`** exactly like `ctx` (§5.4 holds by construction — identical Rust).
//!
//! It reuses `fold::{compute_main,frontier,genesis,reachable}`,
//! `object::{write_tree_from_files,read_tree_to_files}`,
//! `identity::{build_primitive,verify_primitive,parse_ssh_pubkey}`,
//! `scope::Scope`, `state::EngineState`, `config::VaultConfig` — one core,
//! no reimplementation. It also implements [`SessionVault`] so the one
//! sans-IO [`crate::session::Session`] drives it verbatim, like the native
//! `Vault` does for `ctx`.

use crate::config::VaultConfig;
use crate::error::{CspError, CspResult};
use crate::fold::{
    compute_main, frontier, genesis, most_recent_touch, parse_primitive_meta,
    path_present_in_tree, reachable, verify_fold_commit, TouchKind,
};
use crate::identity::{
    build_primitive_with_readd, parse_ssh_pubkey, verify_primitive, Identity,
};
use crate::object::{read_tree_to_files, write_tree_from_files, GitObject};
use crate::oid::Oid;
use crate::order::NodeId;
use crate::scope::{canonicalize_keeps, Scope};
use crate::session::SessionVault;
use crate::state::{EngineState, Snapshot};
use crate::store::{MemStore, Store};
use std::collections::{BTreeMap, BTreeSet};

fn blob_hash(content: &[u8]) -> String {
    GitObject::Blob(content.to_vec()).oid().to_hex()
}

/// Issue 0014: the in-vault quarantine directory. Layer 1 routes ghost-add
/// files into `<vault>/.context/orphans/<utc-iso>/<path>`. The `.context/`
/// prefix is the scope HARD INVARIANT exclusion (`scope.rs`), so the
/// orphans folder is never re-ingested by the tree scan and never
/// published. Each device curates its own quarantine — orphans are not
/// synced.
pub const CONTEXT_ORPHANS_DIR: &str = ".context/orphans";

/// Format a UTC-iso bucket name for the orphans folder. Colon-free so it
/// rides safely on every filesystem (NTFS and APFS disagree about colons).
/// `t` is wall-clock seconds; the value is local-only and never sync'd, so
/// using `now_unix()` is fine (no determinism constraint).
fn orphans_bucket(t: u64) -> String {
    // Naive RFC3339-ish formatter: avoids pulling `chrono` for one
    // user-visible string. Year/month/day computed via the classic
    // civil_from_days algorithm (Howard Hinnant). Stable on all targets.
    let days = (t / 86_400) as i64;
    let secs = (t % 86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    let h = secs / 3600;
    let mi = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{y:04}-{m:02}-{d:02}T{h:02}-{mi:02}-{s:02}Z")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// One materialize action the host must apply to its working tree (§5.6).
/// `Defer` = a contended path: leave the user's bytes (they become a
/// primitive on the next commit). `Quarantine` (issue 0014) = Layer 1 detected
/// a ghost-add: move the on-disk file to `.context/orphans/<utc-iso>/<path>`
/// instead of publishing it, so the DAG-deleted path stays deleted but the
/// user's bytes are preserved locally. The engine has already recorded the
/// last-materialized hash for `Write`/`Remove` (it assumes the host applies
/// them atomically, exactly as the native `Vault::materialize` does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeOp {
    Write { path: String, content: Vec<u8> },
    Remove { path: String },
    Defer { path: String },
    /// Issue 0014 — Layer 1 ghost-add quarantine. The host moves the file at
    /// `from` (in-vault) to `to` (under `.context/orphans/`). On move failure
    /// (disk full, permission, Windows path-length), the host MUST NOT delete
    /// `from` or publish the path: fail-closed by skipping the op and
    /// retrying on the next materialize tick. The orphans folder is hard-
    /// excluded from the tree scan, so it never re-publishes.
    Quarantine { from: String, to: String },
}

/// Host-persisted engine snapshot (the SDK stores these opaque bytes via its
/// StorageAdapter; `.context/`-equivalent, never synced, §11).
///
/// Serialized with MessagePack (`rmp-serde`), NOT `serde_json`: the object
/// store is `Vec<Vec<u8>>`, and `serde_json` has no binary type — it emits
/// every byte as a decimal integer in a JSON array (`[35,32,104,…]`), a ~4×
/// blow-up that `to_bytes` paid on *every* commit. MessagePack with
/// `serde_bytes` keeps the bytes as bytes (~1×) and the codec itself is far
/// faster, so the per-edit persist stall drops accordingly (issue 0011, the
/// cheap half). `from_bytes` still accepts the legacy JSON form so an
/// existing `.context/state` keeps loading.
#[derive(serde::Serialize, serde::Deserialize)]
struct Persisted {
    config_toml: String,
    #[serde(with = "serde_bytes")]
    state_json: Vec<u8>,
    authorized: String,
    /// Every loose object, raw (zlib(framed)) — the same wire form `ctx`
    /// stores on disk (§6.3). Content-addressed, so order is irrelevant.
    objects: Vec<serde_bytes::ByteBuf>,
    main: Option<String>,
}

/// Magic prefix on the MessagePack-encoded engine blob — lets `from_bytes`
/// tell the new form from a legacy `serde_json` blob (which starts with
/// `{`). Four bytes, not valid JSON, vanishingly unlikely as a MessagePack
/// false-positive because we only ever *write* blobs that carry it.
const PERSIST_MAGIC: &[u8] = b"CSP1";

/// The wasm/host-driven full engine. Holds the object store in memory; the
/// host persists [`Self::to_bytes`] and restores via [`Self::from_bytes`].
pub struct MemEngine {
    store: MemStore,
    state: EngineState,
    scope: Scope,
    identity: Identity,
    authorized: String,
    main: Option<Oid>,
    pub config: VaultConfig,
    /// The host's current working file set (`path -> raw bytes`). The host
    /// mutates this incrementally via [`Self::stage_write`] /
    /// [`Self::stage_remove`] so a single edit no longer re-ships the whole
    /// vault on every commit (issue 0009). [`Self::commit_staged`] authors
    /// from it; [`Self::materialize_staged`] keeps it in step with `main`.
    /// Not persisted — reconstructed from `main`'s committed tree on
    /// [`Self::from_bytes`]; deferred uncommitted host edits are re-staged
    /// by the host's reconcile pass (the host vault is their source of
    /// truth), so the working set never needs to outlive a process.
    working: BTreeMap<String, Vec<u8>>,
    /// Issue 0014 — Layer 1 ghost-add quarantines produced inside
    /// `commit_scoped` (paths the engine dropped from the published tree
    /// because the DAG already deleted them). Drained on the next
    /// `materialize_plan` / `materialize_staged` so the host moves the
    /// on-disk file to `.context/orphans/<utc-iso>/<path>` atomically with
    /// the rest of the post-commit reconcile. Not persisted — recomputed by
    /// the next commit if the host re-stages the same bytes.
    pending_quarantines: Vec<MaterializeOp>,
}

impl MemEngine {
    fn rebuild_scope(config: &VaultConfig, ignore: Vec<String>) -> Scope {
        Scope {
            include: config.include.clone(),
            ignore,
            allow_binary: config.allow_binary,
        }
    }

    /// `ctx init`-equivalent: a fresh vault. Genesis M₀ is the deterministic
    /// root; `main` starts at M₀ (§5.2).
    pub fn create(identity: Identity, vault_id: &str, name: &str) -> CspResult<MemEngine> {
        let mut store = MemStore::new();
        let m0 = genesis(&mut store)?;
        let config = VaultConfig {
            vault_id: vault_id.to_string(),
            name: name.to_string(),
            peers: Vec::new(),
            listen: None,
            no_tofu: false,
            no_tls: false,
            log: None,
            debounce_ms: 1000,
            allow_binary: false,
            include: vec!["**".into()],
            auth_keys: Vec::new(),
            default_key_ttl_days: None,
        };
        let state = EngineState {
            vault_id: vault_id.to_string(),
            ..Default::default()
        };
        let scope = Self::rebuild_scope(&config, Vec::new());
        Ok(MemEngine {
            store,
            state,
            scope,
            identity,
            authorized: String::new(),
            main: Some(m0),
            config,
            working: BTreeMap::new(),
            pending_quarantines: Vec::new(),
        })
    }

    /// Restore from host-persisted bytes ([`Self::to_bytes`]). Accepts both
    /// the current MessagePack form (`CSP1`-prefixed) and the legacy
    /// `serde_json` form, so an on-disk `.context/state` written by an older
    /// build still loads (issue 0011).
    pub fn from_bytes(identity: Identity, bytes: &[u8], ignore: Vec<String>) -> CspResult<MemEngine> {
        let p: Persisted = if let Some(body) = bytes.strip_prefix(PERSIST_MAGIC) {
            rmp_serde::from_slice(body)
                .map_err(|e| CspError::Config(format!("engine state parse (msgpack): {e}")))?
        } else {
            serde_json::from_slice(bytes)
                .map_err(|e| CspError::Config(format!("engine state parse (json): {e}")))?
        };
        let mut store = MemStore::new();
        for raw in &p.objects {
            store.put_raw(raw)?;
        }
        let config = VaultConfig::from_toml_str(&p.config_toml)?;
        let state = EngineState::from_bytes(&p.state_json)?;
        let main = match &p.main {
            Some(h) => Some(Oid::from_hex(h)?),
            None => None,
        };
        let scope = Self::rebuild_scope(&config, ignore);
        let mut engine = MemEngine {
            store,
            state,
            scope,
            identity,
            authorized: p.authorized,
            main,
            config,
            working: BTreeMap::new(),
            pending_quarantines: Vec::new(),
        };
        // Seed the incremental working set from the committed tree so the
        // first `commit_staged` after a restart has the right baseline (an
        // empty working set would read as "every file deleted").
        engine.working = engine.files_at_main()?;
        Ok(engine)
    }

    pub fn to_bytes(&self) -> CspResult<Vec<u8>> {
        let objects = self
            .store
            .oids()
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .map(|o| self.store.get_raw(o).map(serde_bytes::ByteBuf::from))
            .collect::<CspResult<Vec<_>>>()?;
        let p = Persisted {
            config_toml: self.config.to_toml_string()?,
            state_json: self.state.to_bytes()?,
            authorized: self.authorized.clone(),
            objects,
            main: self.main.map(|o| o.to_hex()),
        };
        // `CSP1` + MessagePack — see `Persisted`. The magic lets `from_bytes`
        // distinguish this from a legacy `serde_json` blob.
        let mut out = PERSIST_MAGIC.to_vec();
        rmp_serde::to_vec_named(&p)
            .map_err(|e| CspError::Config(e.to_string()))
            .map(|body| {
                out.extend_from_slice(&body);
                out
            })
    }

    /// Replace the `.contextignore`/exclude globs (the host reads the synced
    /// `.contextignore`; scope re-derives, §11).
    pub fn set_ignore(&mut self, ignore: Vec<String>) {
        self.scope = Self::rebuild_scope(&self.config, ignore);
    }

    /// Issue 0014 — Layer 2 entry point. Mark the engine as
    /// `bootstrap_pending`: every subsequent `commit_scoped` returns
    /// `Ok(None)` until [`Self::mark_bootstrap_complete`] is called (the
    /// §13 / [[0007]] handshake-completion edge). Call this from `ctx join`
    /// and from any state-loss recovery that reconstructs
    /// `state.materialized` from `main`'s tree.
    pub fn mark_bootstrap_pending(&mut self) {
        self.state.bootstrap_pending = true;
    }

    /// Issue 0014 — Layer 2 unblock. The catch-up handshake completed; the
    /// device's known-set covers the peer's frontier. After this Layer 1
    /// takes over and `commit_scoped` resumes authoring primitives.
    pub fn mark_bootstrap_complete(&mut self) {
        self.state.bootstrap_pending = false;
    }

    /// Whether the engine is currently in bootstrap-pending mode (issue
    /// 0014). Hosts can surface this in UI ("waiting for first catch-up")
    /// or use it to gate commit-driving callers.
    pub fn is_bootstrap_pending(&self) -> bool {
        self.state.bootstrap_pending
    }

    pub fn main(&self) -> Option<Oid> {
        self.main
    }
    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }

    fn write_tree(&mut self, files: &BTreeMap<String, Vec<u8>>) -> CspResult<Oid> {
        let mut put = Vec::new();
        let root = write_tree_from_files(files, &mut |o| {
            put.push(o.clone());
            Ok(())
        })?;
        for o in &put {
            self.store.put(o)?;
        }
        Ok(root)
    }

    /// The scope-eligible working file set of `main`'s committed tree.
    /// Used to seed `working` on restore (issue 0009).
    fn files_at_main(&mut self) -> CspResult<BTreeMap<String, Vec<u8>>> {
        let main = match self.main {
            Some(m) => m,
            None => return Ok(BTreeMap::new()),
        };
        let tree = match self.store.get(main)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("main is not a commit".into())),
        };
        read_tree_to_files(tree, &|o| self.store.get(o))
    }

    /// Incremental working-set update — record one host write. The bytes are
    /// not committed until [`Self::commit_staged`] (issue 0009).
    pub fn stage_write(&mut self, path: &str, content: Vec<u8>) {
        self.working.insert(path.to_string(), content);
    }

    /// Incremental working-set update — record one host deletion.
    pub fn stage_remove(&mut self, path: &str) {
        self.working.remove(path);
    }

    /// Read-only view of the staged working set (host-side read cache /
    /// tests).
    pub fn working_files(&self) -> &BTreeMap<String, Vec<u8>> {
        &self.working
    }

    /// §5.6 reconcile-by-content over the *staged* working set
    /// ([`Self::stage_write`] / [`Self::stage_remove`]). Semantics are
    /// identical to [`Self::commit_from_files`]; it just reads the
    /// engine-held working set instead of a freshly-shipped one.
    pub fn commit_staged(&mut self) -> CspResult<Option<Oid>> {
        // Spec §11: scope-filter real files + canonicalize directory-
        // preservation sentinels (engine-owned, deterministic → §12).
        let scoped = canonicalize_keeps(&self.working, &self.scope);
        self.commit_scoped(scoped)
    }

    /// §5.6 reconcile-by-content over the host-supplied scoped working set.
    /// `files` = the host's current scope-eligible `path -> bytes` (the host
    /// does the dir walk; scope filtering still applies here as defense).
    /// If anything genuinely changed vs. the last-materialized record, author
    /// one signed primitive parented on the held fold commit and recompute
    /// `main`. Returns the new primitive oid, or `None` (no change → no-op,
    /// self-writes are non-events by construction).
    ///
    /// Equivalent to replacing the whole staged working set then
    /// [`Self::commit_staged`] — kept for callers (and tests) that prefer
    /// the stateless whole-set form.
    pub fn commit_from_files(
        &mut self,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> CspResult<Option<Oid>> {
        self.working = files.clone();
        self.commit_staged()
    }

    /// Shared commit body — `scoped` is the already-scope-filtered set.
    fn commit_scoped(
        &mut self,
        scoped: BTreeMap<String, Vec<u8>>,
    ) -> CspResult<Option<Oid>> {
        // Layer 2 — explicit bootstrap mode (issue 0014). A `join`-created
        // engine (or a state-loss recovery) blocks `commit_scoped` until the
        // §13 handshake reports catch-up completion. Returning Ok(None) so
        // the host doesn't lose work — it just retries on the next tick.
        if self.state.bootstrap_pending {
            return Ok(None);
        }
        // Layer 1 — pre-publish ghost-add guard (issue 0014). Classify each
        // path against (a) the prospective parent's committed tree, (b) the
        // §5.1-most-recent primitive in the parent's closure that touched
        // it, and (c) `state.materialized` as the intent signal. Drop
        // ghost-add paths from the published tree and record a
        // `MaterializeOp::Quarantine` for the host's next reconcile.
        let parent = match self.main {
            Some(m) => m,
            None => genesis(&mut self.store)?,
        };
        let parent_tree_oid = match self.store.get(parent)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("main is not a commit".into())),
        };
        // Probe the parent tree one path at a time (`path_present_in_tree`)
        // rather than reading every blob into a path→bytes map up front. A
        // 200 KiB blob in the parent tree would otherwise be re-loaded on
        // EVERY commit (debounce + safety tick on the host watcher),
        // dragging the debounce window into a tail that races with rapid
        // file edits.
        let mut filtered: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut readds: Vec<Oid> = Vec::new();
        let mut quarantines: Vec<MaterializeOp> = Vec::new();
        let ts = orphans_bucket(now_unix());
        for (path, bytes) in scoped.iter() {
            if path_present_in_tree(&self.store, parent_tree_oid, path)? {
                // p ∈ tree(parent) → no closure walk needed (the parent's
                // tree already asserts the path exists; either a no-op or a
                // genuine modify, both legitimate).
                filtered.insert(path.clone(), bytes.clone());
                continue;
            }
            // p ∉ tree(parent) → potential ghost-add. Walk the closure.
            let touch = most_recent_touch(&self.store, parent, path)?;
            match touch {
                None => {
                    // Never touched in this lineage → genuine novel add.
                    filtered.insert(path.clone(), bytes.clone());
                }
                Some((_, TouchKind::Add)) => {
                    // A previous add (with no subsequent delete) wins on
                    // §5.1 ordering → re-emerged path, proceed.
                    filtered.insert(path.clone(), bytes.clone());
                }
                Some((delete_oid, TouchKind::Delete)) => {
                    // The §5.1-most-recent touch is a delete. Consult intent.
                    if self.state.materialized.contains_key(path) {
                        // This device had the path on disk before — it's
                        // stale data, not new user intent. Quarantine.
                        quarantines.push(MaterializeOp::Quarantine {
                            from: path.clone(),
                            to: format!("{CONTEXT_ORPHANS_DIR}/{ts}/{path}"),
                        });
                    } else {
                        // No prior `materialized` record for this path →
                        // user just created it fresh. Publish it AND emit a
                        // `CSP-Readd: <delete-oid>` trailer so Layer 3 on
                        // every peer exempts the primitive from the
                        // closure-only ghost-add drop.
                        filtered.insert(path.clone(), bytes.clone());
                        if !readds.contains(&delete_oid) {
                            readds.push(delete_oid);
                        }
                    }
                }
            }
        }

        // Stash quarantines for the next materialize tick and drop the
        // quarantined paths from local intent (working set + materialized)
        // so a stale on-disk file doesn't keep ricocheting through the
        // pipeline.
        if !quarantines.is_empty() {
            for op in &quarantines {
                if let MaterializeOp::Quarantine { from, .. } = op {
                    self.working.remove(from);
                    self.state.materialized.remove(from);
                }
            }
            self.pending_quarantines.extend(quarantines);
        }

        // Genuine-change detection runs on the *filtered* set, not the
        // input. A commit that contained only ghost-add paths is a no-op
        // (the quarantines are surfaced via `pending_quarantines`).
        let mut changed = false;
        for (p, c) in &filtered {
            if self.state.materialized.get(p) != Some(&blob_hash(c)) {
                changed = true;
                break;
            }
        }
        if !changed {
            for p in self.state.materialized.keys() {
                if !filtered.contains_key(p) {
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return Ok(None);
        }
        let tree = self.write_tree(&filtered)?;
        let counter = self.state.next_counter();
        let prim = build_primitive_with_readd(
            &self.identity,
            tree,
            parent,
            counter,
            now_unix(),
            "ctx edit",
            &readds,
        );
        let oid = self.store.put(&prim)?;
        self.state.add_known(oid);
        self.recompute()?;
        // Record what we just materialized (== what we committed). Without
        // this the §5.6 last-materialized set never reflects host-authored
        // commits, so `commit_from_files`'s own deletion check
        // (`for p in materialized: if !scoped.contains(p)`) can't fire when
        // the working set shrinks to empty — i.e. deleting the last file (or
        // a whole folder) never produced a removal primitive. Mirrors what
        // `materialize_plan` and the native `Vault::materialize` already do.
        self.state.materialized.clear();
        for (p, c) in &filtered {
            self.state.materialized.insert(p.clone(), blob_hash(c));
        }
        Ok(Some(oid))
    }

    fn recompute(&mut self) -> CspResult<()> {
        let known = self.state.known_oids()?;
        // The *same* deterministic fold as `ctx` (§5.3/§5.4) — now compiled
        // into wasm, so the plugin's `main` is byte-identical.
        self.main = Some(compute_main(&mut self.store, &known)?);
        Ok(())
    }

    /// Integrate received raw objects (§6.3): identical policy to the native
    /// `Vault` — content-addressed put, then admit every primitive whose
    /// author signature verifies. Admission is connection-level (§6.1/§10);
    /// signatures gate corruption only, not authorship policy.
    pub fn integrate(&mut self, raws: &[Vec<u8>]) -> CspResult<usize> {
        for r in raws {
            self.store.put_raw(r)?;
        }
        // "Verified, not trusted": recompute-verify every received synthetic
        // fold commit (recursively to primitives/M₀), once on receipt.
        // Unreproducible fold commit → drop the whole batch (admit nothing,
        // relay nothing). One engine everywhere: the wasm/SDK node runs this
        // identical check.
        for r in raws {
            if let Ok(GitObject::Commit(c)) = GitObject::decompress_and_parse(r) {
                if parse_primitive_meta(&c).is_none() {
                    let oid = GitObject::Commit(c).oid();
                    if verify_fold_commit(&mut self.store, oid).is_err() {
                        tracing::warn!(
                            oid = %oid,
                            "rejected object batch: synthetic fold commit failed \
                             recompute-verification"
                        );
                        return Ok(0);
                    }
                }
            }
        }
        let mut admitted = 0;
        for r in raws {
            if let Ok(GitObject::Commit(c)) = GitObject::decompress_and_parse(r) {
                if let Some((counter, _)) = parse_primitive_meta(&c) {
                    if verify_primitive(&c).is_err() {
                        // Corrupt / forged primitive — structural drop.
                        continue;
                    }
                    let oid = GitObject::Commit(c).oid();
                    if !self.state.known.contains(&oid.to_hex()) {
                        self.state.add_known(oid);
                        admitted += 1;
                    }
                    self.state.observe(counter);
                }
            }
        }
        if admitted > 0 {
            self.recompute()?;
        }
        Ok(admitted)
    }

    /// Compute the §5.6 no-clobber materialize plan for the current `main`.
    /// `on_disk` = the host's current bytes for every path it knows (so the
    /// engine can detect a contended pending user edit). The engine records
    /// the last-materialized hash for every `Write`/`Remove` it emits
    /// (assuming the host applies them atomically — exactly as the native
    /// `Vault::materialize` does).
    pub fn materialize_plan(
        &mut self,
        on_disk: &BTreeMap<String, Vec<u8>>,
    ) -> CspResult<Vec<MaterializeOp>> {
        // Issue 0014 — drain Layer 1's pending quarantines so the host
        // applies them in the same reconcile pass as the Write/Remove/Defer
        // plan. Done unconditionally (no `main` dependency); a join-pending
        // engine never reaches commit_scoped, so this is empty for it.
        let mut ops: Vec<MaterializeOp> = std::mem::take(&mut self.pending_quarantines);
        let main = match self.main {
            Some(m) => m,
            None => return Ok(ops),
        };
        let tree = match self.store.get(main)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("main is not a commit".into())),
        };
        let want = read_tree_to_files(tree, &|o| self.store.get(o))?;

        let prev: Vec<String> = self.state.materialized.keys().cloned().collect();
        for p in prev {
            if want.contains_key(&p) {
                continue;
            }
            if let Some(disk) = on_disk.get(&p) {
                if blob_hash(disk) == self.state.materialized[&p] {
                    ops.push(MaterializeOp::Remove { path: p.clone() });
                }
            }
            self.state.materialized.remove(&p);
        }

        for (p, content) in &want {
            let last = self.state.materialized.get(p).cloned();
            if let Some(d) = on_disk.get(p) {
                let dh = blob_hash(d);
                let contended = Some(&dh) != last.as_ref();
                if contended && d != content {
                    ops.push(MaterializeOp::Defer { path: p.clone() });
                    continue;
                }
                if d == content {
                    self.state.materialized.insert(p.clone(), blob_hash(content));
                    continue;
                }
            }
            ops.push(MaterializeOp::Write {
                path: p.clone(),
                content: content.clone(),
            });
            self.state.materialized.insert(p.clone(), blob_hash(content));
        }
        Ok(ops)
    }

    /// [`Self::materialize_plan`] against the *staged* working set, then keep
    /// `working` in step with the ops the host is about to apply: a `Write`
    /// updates the staged bytes, a `Remove` drops the entry, a `Defer` leaves
    /// the host's bytes untouched (they are already in `working`). After this
    /// the staged set mirrors what the host has on disk, so the next
    /// [`Self::commit_staged`] sees the merged tree as a non-event rather
    /// than re-authoring it (issue 0009).
    pub fn materialize_staged(&mut self) -> CspResult<Vec<MaterializeOp>> {
        let on_disk = std::mem::take(&mut self.working);
        let ops = self.materialize_plan(&on_disk);
        self.working = on_disk;
        let ops = ops?;
        for op in &ops {
            match op {
                MaterializeOp::Write { path, content } => {
                    self.working.insert(path.clone(), content.clone());
                }
                MaterializeOp::Remove { path } => {
                    self.working.remove(path);
                }
                MaterializeOp::Defer { .. } => {}
                MaterializeOp::Quarantine { from, .. } => {
                    // Layer 1 (issue 0014) already dropped this path from
                    // `working` and `state.materialized` in commit_scoped.
                    // Keep the staged set in step in case the host re-staged
                    // the same path between commit and materialize.
                    self.working.remove(from);
                }
            }
        }
        Ok(ops)
    }

    pub fn frontier_tips(&self) -> CspResult<Vec<Oid>> {
        frontier(&self.store, &self.state.known_oids()?)
    }
    pub fn known(&self) -> CspResult<Vec<Oid>> {
        self.state.known_oids()
    }
    pub fn export_closure(&self, tips: &[Oid]) -> CspResult<Vec<Vec<u8>>> {
        let set: BTreeSet<Oid> = reachable(&self.store, tips)?;
        set.into_iter().map(|o| self.store.get_raw(o)).collect()
    }

    /// Export ONLY the new objects authored by `prim_oid`: the primitive
    /// commit itself, plus every (sub-)tree and blob in its tree that is
    /// not also reachable from its parent's tree. Used by Live pushes — a
    /// connected peer already has the parent (it folded to it from its own
    /// known set), so re-shipping the ancestral closure on every edit is
    /// the live-sync-latency killer (issue 0012: a single keystroke
    /// authoring a primitive against an N-deep history was sending
    /// O(N · |tree|) bytes via `export_closure`'s reachable-walk-with-
    /// parents, megabytes per edit).
    ///
    /// Falls back gracefully for a parent-less primitive (genesis-only
    /// case): the parent-tree blob set is empty, so the whole primitive's
    /// tree is shipped.
    pub fn export_primitive(&self, prim_oid: Oid) -> CspResult<Vec<Vec<u8>>> {
        let prim_commit = match self.store.get(prim_oid)? {
            GitObject::Commit(c) => c,
            _ => return Err(CspError::Malformed("export_primitive: not a commit".into())),
        };

        // The set of oids the peer is assumed to already hold via the
        // parent: every (sub-)tree and blob reachable from the parent's
        // tree. The peer ran the same fold against the same known set, so
        // it has them.
        let mut parent_oids: BTreeSet<Oid> = BTreeSet::new();
        if let Some(&parent_oid) = prim_commit.parents.first() {
            if let GitObject::Commit(pc) = self.store.get(parent_oid)? {
                collect_tree_objects(&self.store, pc.tree, &mut parent_oids)?;
            }
        }

        // Oids reachable from the new primitive's tree.
        let mut prim_set: BTreeSet<Oid> = BTreeSet::new();
        collect_tree_objects(&self.store, prim_commit.tree, &mut prim_set)?;

        // The new objects = prim_set - parent_oids, plus the commit itself.
        // BTreeSet iteration is sorted, so the wire order is deterministic.
        let mut raws = Vec::with_capacity(prim_set.len().saturating_sub(parent_oids.len()) + 1);
        raws.push(self.store.get_raw(prim_oid)?);
        for o in prim_set.difference(&parent_oids) {
            raws.push(self.store.get_raw(*o)?);
        }
        Ok(raws)
    }

    // ---- Authorization (§10): in-memory; host persists `authorized` text --

    /// Currently-valid authorized NodeIds (expired entries filtered out).
    pub fn authorized_node_ids(&self) -> BTreeSet<NodeId> {
        let now = now_unix();
        let mut set = BTreeSet::new();
        for e in crate::authkeys::parse_file(&self.authorized) {
            if let Some(n) = e.node {
                if e.expiry.is_valid(now) {
                    set.insert(n);
                }
            }
        }
        set
    }

    fn default_ttl_days(&self) -> u64 {
        self.config
            .default_key_ttl_days
            .unwrap_or(crate::config::BUILTIN_DEFAULT_TTL_DAYS)
    }

    /// Add a pubkey to `authorized` with no expiry token (host-managed
    /// surface; equivalent to a manually pasted line). The listen-start
    /// migration on the embedding host (if any) applies a default TTL.
    pub fn authorize(&mut self, ssh_line: &str) {
        self.authorize_with_expiry(ssh_line, crate::authkeys::Expiry::Unset);
    }

    /// Add a pubkey with a specific expiry; replaces any existing entry for
    /// the same NodeId.
    pub fn authorize_with_expiry(
        &mut self,
        ssh_line: &str,
        expiry: crate::authkeys::Expiry,
    ) {
        let target = parse_ssh_pubkey(ssh_line.trim());
        let mut entries = crate::authkeys::parse_file(&self.authorized);
        let new_line = crate::authkeys::build_line(ssh_line, expiry);
        if let Some(t) = target {
            if let Some(i) = entries.iter().position(|e| e.node == Some(t)) {
                entries[i] = crate::authkeys::parse_line(&new_line);
                self.authorized = crate::authkeys::serialize(&entries);
                return;
            }
        } else {
            return;
        }
        entries.push(crate::authkeys::parse_line(&new_line));
        self.authorized = crate::authkeys::serialize(&entries);
    }

    /// Enrollment-aware admit (§10) — same decision table as `Vault::admit_peer`
    /// but in-memory: see `vault.rs` docs. `enrollment_authorized` is wired
    /// from the host's transport layer; for outbound-only thin nodes (the
    /// SDK) it is always `false`.
    pub fn admit_peer(
        &mut self,
        ssh_line: &str,
        enrollment_authorized: bool,
    ) -> CspResult<bool> {
        let now = now_unix();
        let Some(peer_node) = parse_ssh_pubkey(ssh_line) else {
            return Ok(false);
        };
        let ttl_days = self.default_ttl_days();
        let enroll_expiry = || -> crate::authkeys::Expiry {
            if ttl_days == 0 {
                crate::authkeys::Expiry::Unset
            } else {
                crate::authkeys::Expiry::At(crate::authkeys::expiry_from_ttl_days(now, ttl_days))
            }
        };
        let mut entries = crate::authkeys::parse_file(&self.authorized);
        if let Some(i) = entries.iter().position(|e| e.node == Some(peer_node)) {
            if entries[i].expiry.is_valid(now) {
                return Ok(true);
            }
            if enrollment_authorized {
                let line = crate::authkeys::build_line(ssh_line, enroll_expiry());
                entries[i] = crate::authkeys::parse_line(&line);
                self.authorized = crate::authkeys::serialize(&entries);
                return Ok(true);
            }
            return Ok(false);
        }
        if enrollment_authorized {
            let line = crate::authkeys::build_line(ssh_line, enroll_expiry());
            entries.push(crate::authkeys::parse_line(&line));
            self.authorized = crate::authkeys::serialize(&entries);
            return Ok(true);
        }
        let any_key = entries.iter().any(|e| e.is_key());
        if !any_key && !self.config.no_tofu && self.config.auth_keys.is_empty() {
            let line = crate::authkeys::build_line(ssh_line, enroll_expiry());
            entries.push(crate::authkeys::parse_line(&line));
            self.authorized = crate::authkeys::serialize(&entries);
            return Ok(true);
        }
        Ok(false)
    }

    /// Back-compat: `admit_peer(ssh, false)`.
    pub fn admit_peer_tofu(&mut self, ssh_line: &str) -> CspResult<bool> {
        self.admit_peer(ssh_line, false)
    }

    // ---- PITR (§8) -----------------------------------------------------

    pub fn snapshot(&mut self, name: &str) -> CspResult<()> {
        let tips = self.frontier_tips()?;
        self.state.snapshots.insert(
            name.to_string(),
            Snapshot {
                label: name.to_string(),
                frontier: tips.iter().map(|o| o.to_hex()).collect(),
                created_unix: now_unix(),
            },
        );
        Ok(())
    }

    fn fold_subset(&mut self, subset: &[Oid]) -> CspResult<BTreeMap<String, Vec<u8>>> {
        let m = compute_main(&mut self.store, subset)?;
        let tree = match self.store.get(m)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("subset main not a commit".into())),
        };
        read_tree_to_files(tree, &|o| self.store.get(o))
    }

    /// Restore-as-edit (§8): returns the historical tree the host writes into
    /// the working files; the host then calls [`commit_from_files`] so it
    /// becomes a normal primitive on this lineage (pre-restore state stays in
    /// history).
    pub fn restore_snapshot(&mut self, name: &str) -> CspResult<BTreeMap<String, Vec<u8>>> {
        let snap = self
            .state
            .snapshots
            .get(name)
            .cloned()
            .ok_or_else(|| CspError::Other(format!("no snapshot {name}")))?;
        let subset: Vec<Oid> = snap
            .frontier
            .iter()
            .map(|h| Oid::from_hex(h))
            .collect::<CspResult<_>>()?;
        self.fold_subset(&subset)
    }

    pub fn restore_time(&mut self, t_unix: u64) -> CspResult<BTreeMap<String, Vec<u8>>> {
        let known = self.state.known_oids()?;
        let mut subset = Vec::new();
        for o in known {
            if let GitObject::Commit(c) = self.store.get(o)? {
                if c.author_time <= t_unix {
                    subset.push(o);
                }
            }
        }
        self.fold_subset(&subset)
    }

    pub fn snapshots(&self) -> &BTreeMap<String, Snapshot> {
        &self.state.snapshots
    }
}

/// Wall-clock seconds. Native uses the system clock; wasm uses the JS clock
/// via `js-sys`/the host (wasm-bindgen provides `Date.now`). Both are
/// advisory only (§5.1: the logical counter is authoritative for order).
/// Collect every (sub-)tree and blob oid reachable from `tree_oid` into
/// `out`. The tree object itself is included; nested sub-trees recurse.
/// Used by [`MemEngine::export_primitive`] to diff a new primitive's tree
/// against its parent's tree (issue 0012).
fn collect_tree_objects<S: Store>(
    store: &S,
    tree_oid: Oid,
    out: &mut BTreeSet<Oid>,
) -> CspResult<()> {
    if !out.insert(tree_oid) {
        return Ok(());
    }
    match store.get(tree_oid)? {
        GitObject::Tree(entries) => {
            for e in entries {
                match store.get(e.oid)? {
                    GitObject::Blob(_) => {
                        out.insert(e.oid);
                    }
                    GitObject::Tree(_) => {
                        collect_tree_objects(store, e.oid, out)?;
                    }
                    GitObject::Commit(_) => {} // commits never live in trees
                }
            }
        }
        GitObject::Blob(_) | GitObject::Commit(_) => {} // tree_oid wasn't a tree
    }
    Ok(())
}

fn now_unix() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
}

/// The one sans-IO [`crate::session::Session`] drives this verbatim — same
/// protocol code as the native `Vault` path used by `ctx` (§16).
impl SessionVault for MemEngine {
    fn vault_id(&self) -> String {
        self.config.vault_id.clone()
    }
    fn name(&self) -> String {
        self.config.name.clone()
    }
    fn identity_ssh(&self) -> String {
        self.identity.to_ssh_string()
    }
    fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.identity.sign(msg)
    }
    fn frontier_tips(&self) -> CspResult<Vec<Oid>> {
        MemEngine::frontier_tips(self)
    }
    fn known(&self) -> CspResult<Vec<Oid>> {
        MemEngine::known(self)
    }
    fn has(&self, o: Oid) -> bool {
        self.store.has(o)
    }
    fn export_closure(&self, tips: &[Oid]) -> CspResult<Vec<Vec<u8>>> {
        MemEngine::export_closure(self, tips)
    }
    fn integrate(&mut self, raws: &[Vec<u8>]) -> CspResult<usize> {
        MemEngine::integrate(self, raws)
    }
    fn admit_peer(&mut self, peer_ssh: &str, enrollment_authorized: bool) -> CspResult<bool> {
        MemEngine::admit_peer(self, peer_ssh, enrollment_authorized)
    }
    fn mark_bootstrap_complete(&mut self) {
        MemEngine::mark_bootstrap_complete(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: u8) -> Identity {
        Identity::from_seed(&[s; 32])
    }
    fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(p, c)| (p.to_string(), c.as_bytes().to_vec()))
            .collect()
    }

    // ---- Persisted-blob format (issue 0011, cheap half) ----

    #[test]
    fn to_bytes_is_messagepack_and_round_trips() {
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.commit_from_files(&files(&[("a.md", "hello"), ("b.md", "world")]))
            .unwrap();
        let blob = e.to_bytes().unwrap();
        assert!(blob.starts_with(PERSIST_MAGIC), "blob carries the CSP1 magic");
        let restored = MemEngine::from_bytes(id(1), &blob, Vec::new()).unwrap();
        assert_eq!(restored.main(), e.main(), "main survives the round-trip");
        assert_eq!(restored.working_files().len(), 2);
        assert_eq!(restored.working_files().get("a.md"), Some(&b"hello".to_vec()));
    }

    #[test]
    fn from_bytes_still_loads_a_legacy_json_blob() {
        // Build a faithful legacy blob: the pre-0011 form was a bare
        // `serde_json` encoding of the same struct (no magic prefix). An
        // existing on-disk `.context/state` must keep loading.
        let mut e = MemEngine::create(id(2), "v", "").unwrap();
        e.commit_from_files(&files(&[("legacy.md", "kept")])).unwrap();
        let objects = e
            .store
            .oids()
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .map(|o| e.store.get_raw(o).map(serde_bytes::ByteBuf::from))
            .collect::<CspResult<Vec<_>>>()
            .unwrap();
        let legacy = Persisted {
            config_toml: e.config.to_toml_string().unwrap(),
            state_json: e.state.to_bytes().unwrap(),
            authorized: e.authorized.clone(),
            objects,
            main: e.main().map(|o| o.to_hex()),
        };
        let json_blob = serde_json::to_vec(&legacy).unwrap();
        assert!(!json_blob.starts_with(PERSIST_MAGIC));
        let restored = MemEngine::from_bytes(id(2), &json_blob, Vec::new()).unwrap();
        assert_eq!(restored.main(), e.main());
        assert_eq!(restored.working_files().get("legacy.md"), Some(&b"kept".to_vec()));
    }

    #[test]
    fn messagepack_blob_is_far_smaller_than_the_json_equivalent() {
        // The whole point of issue 0011's cheap half: object bytes stop
        // being JSON integer arrays. With real (compressible-but-binary)
        // object payloads the MessagePack blob must be dramatically smaller.
        let mut e = MemEngine::create(id(3), "v", "").unwrap();
        let body: String = (0..2000).map(|i| ((i % 64) as u8 + 32) as char).collect();
        e.commit_from_files(&files(&[("big.md", &body)])).unwrap();
        let mp = e.to_bytes().unwrap();
        let objects = e
            .store
            .oids()
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .map(|o| e.store.get_raw(o).map(serde_bytes::ByteBuf::from))
            .collect::<CspResult<Vec<_>>>()
            .unwrap();
        let json = serde_json::to_vec(&Persisted {
            config_toml: e.config.to_toml_string().unwrap(),
            state_json: e.state.to_bytes().unwrap(),
            authorized: e.authorized.clone(),
            objects,
            main: e.main().map(|o| o.to_hex()),
        })
        .unwrap();
        assert!(
            mp.len() * 2 < json.len(),
            "messagepack ({} B) must be <½ the json ({} B)",
            mp.len(),
            json.len()
        );
    }

    #[test]
    fn commit_materialize_and_no_feedback() {
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        let f = files(&[("a.md", "hello")]);
        assert!(e.commit_from_files(&f).unwrap().is_some());
        let ops = e.materialize_plan(&BTreeMap::new()).unwrap();
        assert!(ops.contains(&MaterializeOp::Write {
            path: "a.md".into(),
            content: b"hello".to_vec()
        }));
        // §5.6: re-commit with the same on-disk content → non-event.
        assert!(e.commit_from_files(&f).unwrap().is_none());
    }

    // ---- Incremental staging API (issue 0009) ----

    // ---- Incremental Live export (issue 0012) ----

    #[test]
    fn export_primitive_is_much_smaller_than_export_closure_on_a_deep_history() {
        // Two-window scenario in miniature: A authors many primitives, then
        // one more; for the *last* edit the Live wire form (what `commitNow`
        // sends on a steady-state edit) must be O(diff), not O(history).
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        // Seed a vault of ~30 files and ~10 edit rounds → real history depth.
        for round in 0..10 {
            let mut files = BTreeMap::new();
            for i in 0..30 {
                files.insert(format!("note-{i}.md"), format!("v{round}-content-{i}").into_bytes());
            }
            a.commit_from_files(&files).unwrap();
        }
        // The final, single-file edit on top.
        let mut files = BTreeMap::new();
        for i in 0..30 {
            files.insert(format!("note-{i}.md"), format!("v9-content-{i}").into_bytes());
        }
        files.insert("note-7.md".into(), b"the only fresh byte".to_vec());
        let prim = a.commit_from_files(&files).unwrap().expect("a real change");

        let closure = a.export_closure(&[prim]).unwrap();
        let incremental = a.export_primitive(prim).unwrap();

        let closure_bytes: usize = closure.iter().map(|r| r.len()).sum();
        let incr_bytes: usize = incremental.iter().map(|r| r.len()).sum();
        assert!(
            incr_bytes * 5 < closure_bytes,
            "incremental export ({incr_bytes} B) must be at least 5× smaller than full closure ({closure_bytes} B)"
        );
        // It must contain the primitive itself.
        assert!(!incremental.is_empty());
    }

    #[test]
    fn export_primitive_round_trips_through_integrate_against_a_synced_peer() {
        // The receiver must successfully admit a primitive shipped via
        // `export_primitive` *iff* it already has the parent (the steady-
        // state Live case: both ends folded to the same main on the prior
        // edit). After integrate, the receiver's `main` matches the sender.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        // Bootstrap B with A's history via the full closure (catch-up).
        a.commit_from_files(&files(&[("a.md", "v1"), ("b.md", "v1")])).unwrap();
        let bootstrap = a.export_closure(&a.known().unwrap()).unwrap();
        let admitted = b.integrate(&bootstrap).unwrap();
        assert!(admitted > 0, "catch-up must seed B with A's history");
        assert_eq!(a.main(), b.main(), "peers converged after catch-up");

        // Now A makes a tiny edit; the Live wire form is `export_primitive`.
        let prim = a
            .commit_from_files(&files(&[("a.md", "v2"), ("b.md", "v1")]))
            .unwrap()
            .expect("a real change");
        let live = a.export_primitive(prim).unwrap();

        // B integrates the incremental payload and ends at A's main.
        let admitted_live = b.integrate(&live).unwrap();
        assert_eq!(admitted_live, 1);
        assert_eq!(b.main(), a.main(), "peers converged via incremental Live");

        // And the materialize plan now writes the edited file.
        let plan = b.materialize_plan(&BTreeMap::new()).unwrap();
        assert!(plan.contains(&MaterializeOp::Write {
            path: "a.md".into(),
            content: b"v2".to_vec(),
        }));
    }

    #[test]
    fn export_primitive_omits_objects_already_in_the_parent_tree() {
        // The unchanged blobs from the parent's tree must NOT appear in the
        // incremental export — that's the whole point of issue 0012.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        a.commit_from_files(&files(&[("kept.md", "stable"), ("edit.md", "v1")])).unwrap();
        let parent = a.main().unwrap();
        let parent_tree_oid = match a.store.get(parent).unwrap() {
            GitObject::Commit(c) => c.tree,
            _ => panic!("parent not a commit"),
        };
        let kept_blob_oid = match a.store.get(parent_tree_oid).unwrap() {
            GitObject::Tree(t) => t
                .iter()
                .find(|e| e.name == "kept.md")
                .expect("kept entry")
                .oid,
            _ => panic!("parent tree not a tree"),
        };

        let prim = a
            .commit_from_files(&files(&[("kept.md", "stable"), ("edit.md", "v2")]))
            .unwrap()
            .unwrap();
        let raws = a.export_primitive(prim).unwrap();
        // Reconstruct the oids in the payload by hashing each raw.
        let payload_oids: BTreeSet<Oid> = raws
            .iter()
            .map(|r| {
                let obj = GitObject::decompress_and_parse(r).unwrap();
                obj.oid()
            })
            .collect();
        assert!(
            !payload_oids.contains(&kept_blob_oid),
            "unchanged blob must NOT ride along on the incremental wire form"
        );
        assert!(payload_oids.contains(&prim), "the new primitive itself rides");
    }

    #[test]
    fn staged_write_then_commit_authors_a_primitive() {
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.stage_write("a.md", b"hello".to_vec());
        let oid = e.commit_staged().unwrap();
        assert!(oid.is_some(), "a staged write must produce a primitive");
        // The staged set is the engine's working view.
        assert_eq!(e.working_files().get("a.md").map(|v| v.as_slice()), Some(&b"hello"[..]));
    }

    #[test]
    fn staged_commit_is_byte_identical_to_commit_from_files() {
        // The staging path must author the *same* primitive as the
        // whole-set path — same tree, same parent, same counter, same
        // signature → same oid. (Determinism §12; this is what keeps the
        // `ctx`-parity guarantee intact across the 0009 refactor.)
        let mut a = MemEngine::create(id(7), "v", "").unwrap();
        let mut b = MemEngine::create(id(7), "v", "").unwrap();
        let oid_whole = a
            .commit_from_files(&files(&[("x.md", "X"), ("y.md", "Y")]))
            .unwrap();
        b.stage_write("x.md", b"X".to_vec());
        b.stage_write("y.md", b"Y".to_vec());
        let oid_staged = b.commit_staged().unwrap();
        assert_eq!(oid_whole, oid_staged);
        assert_eq!(a.main(), b.main());
    }

    #[test]
    fn staged_no_change_is_a_non_event() {
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.stage_write("a.md", b"hello".to_vec());
        assert!(e.commit_staged().unwrap().is_some());
        // Re-staging identical bytes → §5.6 non-event.
        e.stage_write("a.md", b"hello".to_vec());
        assert!(e.commit_staged().unwrap().is_none());
        // commit_staged with nothing staged since → still a non-event.
        assert!(e.commit_staged().unwrap().is_none());
    }

    #[test]
    fn staged_remove_authors_a_deletion() {
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.stage_write("a.md", b"A".to_vec());
        e.stage_write("b.md", b"B".to_vec());
        e.commit_staged().unwrap();
        e.stage_remove("a.md");
        assert!(e.commit_staged().unwrap().is_some(), "a removal is a change");
        let ops = e.materialize_plan(&BTreeMap::new()).unwrap();
        assert!(ops.contains(&MaterializeOp::Write { path: "b.md".into(), content: b"B".to_vec() }));
        assert!(!ops.iter().any(|o| matches!(o, MaterializeOp::Write { path, .. } if path == "a.md")));
    }

    #[test]
    fn staged_remove_last_file_empties_the_tree() {
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.stage_write("only.md", b"x".to_vec());
        e.commit_staged().unwrap();
        e.stage_remove("only.md");
        assert!(e.commit_staged().unwrap().is_some(), "emptying the vault is a change");
        assert!(e.working_files().is_empty());
    }

    #[test]
    fn stage_write_then_remove_before_commit_collapses() {
        // Editing then deleting a brand-new path before any commit is a
        // no-op — the working set ends empty, nothing to author.
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.stage_write("scratch.md", b"tmp".to_vec());
        e.stage_remove("scratch.md");
        assert!(e.commit_staged().unwrap().is_none());
    }

    #[test]
    fn staged_write_accepts_non_utf8_bytes() {
        // `stage_write` takes raw bytes — a non-UTF8 payload must round-trip
        // (the scope's text-allowlist is the host's concern, not the
        // staging buffer's).
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        let raw = vec![0xff, 0x00, 0xfe, 0x80];
        e.stage_write("blob.bin", raw.clone());
        assert_eq!(e.working_files().get("blob.bin"), Some(&raw));
    }

    #[test]
    fn working_set_survives_restart_via_from_bytes() {
        // After a persist+restore the staged baseline is the committed tree,
        // so re-staging identical content is a non-event (no spurious commit
        // on every reload).
        let mut e = MemEngine::create(id(3), "v", "").unwrap();
        e.stage_write("keep.md", b"data".to_vec());
        e.stage_write("dir/nested.md", b"deep".to_vec());
        e.commit_staged().unwrap();
        let bytes = e.to_bytes().unwrap();

        let mut restored = MemEngine::from_bytes(id(3), &bytes, Vec::new()).unwrap();
        assert_eq!(restored.working_files().len(), 2, "working seeded from main");
        assert_eq!(restored.working_files().get("keep.md"), Some(&b"data".to_vec()));
        // No host edits since restore → commit_staged is a non-event.
        assert!(restored.commit_staged().unwrap().is_none());
        // A real edit after restore still authors.
        restored.stage_write("keep.md", b"edited".to_vec());
        assert!(restored.commit_staged().unwrap().is_some());
    }

    #[test]
    fn materialize_staged_pulls_remote_and_syncs_working() {
        // A receives B's primitive via integrate, then materialize_staged
        // must (a) emit the Write op and (b) leave `working` mirroring main
        // so the *next* commit_staged is a non-event (no echo).
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        b.stage_write("remote.md", b"from-b".to_vec());
        b.commit_staged().unwrap();
        let bx = b.export_closure(&b.known().unwrap()).unwrap();
        a.integrate(&bx).unwrap();

        let ops = a.materialize_staged().unwrap();
        assert!(ops.contains(&MaterializeOp::Write {
            path: "remote.md".into(),
            content: b"from-b".to_vec(),
        }));
        assert_eq!(a.working_files().get("remote.md"), Some(&b"from-b".to_vec()));
        // The merged tree is now the staged baseline — committing echoes nothing.
        assert!(a.commit_staged().unwrap().is_none());
    }

    #[test]
    fn materialize_staged_defers_a_contended_path() {
        // A has an uncommitted local edit to the same path B committed.
        // materialize_staged must Defer (keep A's bytes), and `working`
        // must retain A's bytes so the next commit authors *them*.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        // Shared genesis history: both commit the same base first.
        a.commit_from_files(&files(&[("shared.md", "base")])).unwrap();
        b.commit_from_files(&files(&[("shared.md", "base")])).unwrap();
        // A edits locally but does NOT commit; B commits a divergent edit.
        a.stage_write("shared.md", b"a-edit".to_vec());
        b.commit_from_files(&files(&[("shared.md", "b-edit")])).unwrap();
        let bx = b.export_closure(&b.known().unwrap()).unwrap();
        a.integrate(&bx).unwrap();

        let ops = a.materialize_staged().unwrap();
        assert!(
            ops.iter().any(|o| matches!(o, MaterializeOp::Defer { path } if path == "shared.md")),
            "a contended path must Defer, not clobber the host edit"
        );
        // `working` still holds A's uncommitted bytes.
        assert_eq!(a.working_files().get("shared.md"), Some(&b"a-edit".to_vec()));
    }

    #[test]
    fn commit_from_files_replaces_the_staged_set_wholesale() {
        // The whole-set form must drop staged paths not in the new set —
        // its documented "the working set is exactly these files" contract.
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.stage_write("old.md", b"old".to_vec());
        e.commit_from_files(&files(&[("new.md", "new")])).unwrap();
        assert!(e.working_files().get("old.md").is_none());
        assert_eq!(e.working_files().get("new.md"), Some(&b"new".to_vec()));
    }

    #[test]
    fn two_engines_converge_same_main_as_each_other() {
        // Authorization is connection-level (§6.1/§10); integrate admits any
        // primitive with a valid signature regardless of author. Neither
        // engine authorizes the other's key here — convergence must still hold.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        a.commit_from_files(&files(&[("a.md", "AAA")])).unwrap();
        b.commit_from_files(&files(&[("b.md", "BBB")])).unwrap();
        let ax = a.export_closure(&a.known().unwrap()).unwrap();
        let bx = b.export_closure(&b.known().unwrap()).unwrap();
        a.integrate(&bx).unwrap();
        b.integrate(&ax).unwrap();
        assert_eq!(a.main(), b.main(), "deterministic fold → identical main");
    }

    #[test]
    fn unknown_author_with_valid_signature_is_admitted() {
        // §6.1/§10: integrate admits primitives signed by keys the receiver
        // does not locally know — the trust gate is connection admission
        // (out-of-band here; this is the engine-only path), not per-author.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        // Non-empty authorized set on b, but DOES NOT contain id(1).
        b.authorize(&id(99).to_ssh_string());
        a.commit_from_files(&files(&[("from-stranger.md", "hello")])).unwrap();
        let ax = a.export_closure(&a.known().unwrap()).unwrap();
        let admitted = b.integrate(&ax).unwrap();
        assert!(admitted > 0, "unknown-but-valid-signature primitive must admit");
        let plan = b.materialize_plan(&BTreeMap::new()).unwrap();
        assert!(
            plan.contains(&MaterializeOp::Write {
                path: "from-stranger.md".into(),
                content: b"hello".to_vec(),
            }),
            "and the content reaches the materialize plan"
        );
    }

    #[test]
    fn bad_signature_primitive_is_dropped() {
        use crate::identity::build_primitive;
        // Content-integrity check: a primitive whose signature does not
        // verify is structurally corrupt and dropped — the only drop case
        // for primitives under §6.3.
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        let parent = b.main().expect("M₀ at create");
        let prim_obj = build_primitive(&id(9), parent, parent, 1, 0, "ctx edit");
        let mut commit = match prim_obj {
            GitObject::Commit(c) => c,
            _ => panic!("primitive must be a Commit"),
        };
        // Mutate after signing — the embedded signature no longer matches.
        commit.message = commit.message.replace("ctx edit", "evil edit");
        let tampered = GitObject::Commit(commit).compress();
        let admitted = b.integrate(&[tampered]).unwrap();
        assert_eq!(admitted, 0, "tampered primitive must be dropped");
    }

    /// "Verified, not trusted": a received synthetic fold commit that does
    /// NOT recompute byte-identically poisons the whole batch — nothing is
    /// admitted and nothing would be relayed (integrate returns 0, `main`
    /// unchanged). The untampered closure integrates normally.
    #[test]
    fn forged_fold_commit_rejects_whole_batch() {
        use crate::object::{CommitObj, GitObject};

        // Two peers author disjoint primitives, then b merges → b holds a
        // real 2-parent synthetic fold commit.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        a.authorize(&id(2).to_ssh_string());
        b.authorize(&id(1).to_ssh_string());
        a.commit_from_files(&files(&[("a.md", "AAA")])).unwrap();
        b.commit_from_files(&files(&[("b.md", "BBB")])).unwrap();
        let ax = a.export_closure(&a.known().unwrap()).unwrap();
        b.integrate(&ax).unwrap();
        // A third primitive authored on top of the merge makes the 2-parent
        // fold commit reachable from the primitive set (primitives parent on
        // the fold they were authored against).
        b.commit_from_files(&files(&[("a.md", "AAA"), ("b.md", "BBB"), ("c.md", "CCC")]))
            .unwrap();
        let bx = b.export_closure(&b.known().unwrap()).unwrap();

        // Find b's real fold commit (a non-primitive commit with parents)
        // and a primitive's tree to use as a deliberately wrong fold tree.
        let mut fold = None;
        let mut wrong_tree = None;
        for r in &bx {
            if let Ok(GitObject::Commit(c)) = GitObject::decompress_and_parse(r) {
                if parse_primitive_meta(&c).is_some() {
                    wrong_tree = Some(c.tree);
                } else if !c.parents.is_empty() {
                    fold = Some(c);
                }
            }
        }
        let fold = fold.expect("b must hold a real fold commit");
        let wrong_tree = wrong_tree.expect("a primitive tree");
        assert_ne!(fold.tree, wrong_tree, "bogus tree must differ from real");

        // Forge: same parents, wrong tree → cannot recompute to its own SHA.
        let forged = GitObject::Commit(CommitObj {
            tree: wrong_tree,
            ..fold.clone()
        });
        let forged_raw = forged.compress();

        // Control: the untampered closure integrates and moves `main`.
        let mut ctrl = MemEngine::create(id(1), "v", "").unwrap();
        ctrl.authorize(&id(2).to_ssh_string());
        ctrl.commit_from_files(&files(&[("a.md", "AAA")])).unwrap();
        let before = ctrl.main();
        assert!(ctrl.integrate(&bx).unwrap() > 0);
        assert_ne!(ctrl.main(), before, "clean batch must integrate");

        // Tampered: same closure + the forged fold commit → whole batch
        // rejected, nothing admitted, `main` frozen.
        let mut victim = MemEngine::create(id(1), "v", "").unwrap();
        victim.authorize(&id(2).to_ssh_string());
        victim.commit_from_files(&files(&[("a.md", "AAA")])).unwrap();
        let frozen = victim.main();
        let mut batch = bx.clone();
        batch.push(forged_raw);
        assert_eq!(
            victim.integrate(&batch).unwrap(),
            0,
            "a forged fold commit must reject the entire batch"
        );
        assert_eq!(victim.main(), frozen, "main must not move on a poisoned batch");
    }

    #[test]
    fn roundtrips_through_persisted_bytes() {
        let mut e = MemEngine::create(id(7), "v", "n").unwrap();
        e.commit_from_files(&files(&[("d.md", "v1")])).unwrap();
        let bytes = e.to_bytes().unwrap();
        let e2 = MemEngine::from_bytes(id(7), &bytes, Vec::new()).unwrap();
        assert_eq!(e.main(), e2.main());
        assert_eq!(e2.config.vault_id, "v");
    }

    // ---- Issue 0014: ghost-add guard + bootstrap mode ----

    /// Helper — read main's tree as a path map (host-observable). Avoids
    /// poking the private store directly from outside the engine.
    fn main_tree(e: &mut MemEngine) -> BTreeMap<String, Vec<u8>> {
        e.materialize_staged().ok();
        // After materialize_staged, working is in step with main.
        e.working_files().clone()
    }

    /// Test helper — re-compute the oid of a CommitObj so a test can match
    /// the right commit in a multi-object closure.
    fn obj_oid(c: &crate::object::CommitObj) -> Oid {
        crate::object::GitObject::Commit(c.clone()).oid()
    }

    #[test]
    fn ghost_add_quarantined_when_disk_file_is_stale() {
        // Acceptance: "A deletes foo, A↔B sync, then C (with foo on disk)
        // joins — foo ends up deleted on every device, on-disk content is
        // preserved in `.context/orphans/` on C." Simulated via two
        // MemEngines: A authors create+delete, B integrates the closure,
        // then B tries to publish a stale on-disk foo.md.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        a.commit_from_files(&files(&[("foo.md", "hello")])).unwrap();

        // B catches up to A's create, materializes → B.materialized[foo.md]
        // records the prior on-disk state (the "stale device" precondition).
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        let tips_a = a.frontier_tips().unwrap();
        let closure_a1 = a.export_closure(&tips_a).unwrap();
        b.integrate(&closure_a1).unwrap();
        let ops = b.materialize_staged().unwrap();
        assert!(
            ops.iter()
                .any(|o| matches!(o, MaterializeOp::Write { path, .. } if path == "foo.md")),
            "B should have written foo.md on first materialize"
        );
        assert_eq!(
            b.working_files().get("foo.md"),
            Some(&b"hello".to_vec()),
            "foo.md present in B's working set after first materialize"
        );

        // A deletes foo.md and republishes; B integrates → B.main has no
        // foo.md, but B.materialized still records it (we haven't run
        // materialize_staged since the delete arrived).
        a.commit_from_files(&files(&[])).unwrap();
        let closure_a2 = a.export_closure(&a.frontier_tips().unwrap()).unwrap();
        b.integrate(&closure_a2).unwrap();

        // Stale device cadence: the user (or the host's reconcile pass) tries
        // to commit foo.md back. Layer 1 must detect the ghost-add and
        // quarantine instead of publishing.
        let res = b.commit_from_files(&files(&[("foo.md", "hello")])).unwrap();
        assert!(
            res.is_none(),
            "ghost-add must not produce a new primitive (got {res:?})"
        );

        // The next materialize plan must include a Quarantine op for the
        // ghost-add path.
        let ops = b.materialize_staged().unwrap();
        let quarantined = ops.iter().any(|o| match o {
            MaterializeOp::Quarantine { from, to } => {
                from == "foo.md" && to.starts_with(".context/orphans/")
            }
            _ => false,
        });
        assert!(
            quarantined,
            "expected a Quarantine op for foo.md; got {ops:?}"
        );

        // After the dust settles, B.main agrees with A (no foo.md).
        let final_tree = main_tree(&mut b);
        assert!(
            !final_tree.contains_key("foo.md"),
            "foo.md must stay deleted on B"
        );
    }

    #[test]
    fn legitimate_readd_publishes_with_trailer() {
        // Acceptance: "User legitimately re-creates a previously-deleted
        // path on a synced device: file publishes, all peers see it."
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        a.commit_from_files(&files(&[("note.md", "v1")])).unwrap();
        a.commit_from_files(&files(&[])).unwrap(); // delete

        // After the delete, A.materialized no longer records note.md → a
        // subsequent stage_write is treated as fresh user intent. Layer 1
        // publishes a primitive carrying a CSP-Readd trailer naming the
        // delete.
        let oid = a
            .commit_from_files(&files(&[("note.md", "v2")]))
            .unwrap()
            .expect("legitimate re-add must publish a primitive");
        // Find the primitive itself in the closure — `pop()` returns the
        // last raw which may be a tree/blob, not the commit we authored.
        let c = a
            .export_closure(&[oid])
            .unwrap()
            .into_iter()
            .filter_map(|raw| {
                let obj = crate::object::GitObject::decompress_and_parse(&raw).ok()?;
                match obj {
                    crate::object::GitObject::Commit(c) if obj_oid(&c) == oid => Some(c),
                    _ => None,
                }
            })
            .next()
            .expect("re-add primitive missing from closure");
        let readds = crate::fold::parse_primitive_readds(&c);
        assert_eq!(
            readds.len(),
            1,
            "legitimate re-add must carry exactly one CSP-Readd trailer"
        );

        // Verify a peer B integrating the closure sees note.md in main
        // (Layer 3 must NOT drop a primitive whose trailer names a delete
        // in its own closure).
        let mut b = MemEngine::create(id(2), "v", "").unwrap();
        let closure = a.export_closure(&a.frontier_tips().unwrap()).unwrap();
        b.integrate(&closure).unwrap();
        let tree = main_tree(&mut b);
        assert_eq!(
            tree.get("note.md"),
            Some(&b"v2".to_vec()),
            "legitimate re-add must propagate to B"
        );
    }

    #[test]
    fn layer3_drops_primitive_missing_readd_trailer() {
        // Acceptance: "a primitive missing the readd trailer for a path
        // whose closure contains a delete is dropped by Layer 3 on every
        // peer." Construct the buggy author case directly: build a signed
        // primitive that adds a path against a parent whose closure has a
        // delete, with no trailer, and verify the fold drops it.
        let mut a = MemEngine::create(id(1), "v", "").unwrap();
        a.commit_from_files(&files(&[("foo.md", "hello")])).unwrap();
        a.commit_from_files(&files(&[])).unwrap();
        let post_delete_main = a.main().unwrap();

        // Hand-author a primitive that adds foo.md again, parented on
        // a.main, but WITHOUT a CSP-Readd trailer (simulating a pre-bump
        // SDK or a buggy author). `write_tree` is private to MemEngine;
        // the test reaches in (same module) to put the tree + blob into
        // A's store so `export_closure` later finds them.
        let id_bad = id(2);
        let bad_tree = a
            .write_tree(&files(&[("foo.md", "ghost")]))
            .expect("write_tree");
        let bad_prim = crate::identity::build_primitive(
            &id_bad,
            bad_tree,
            post_delete_main,
            a.state.observed + 1,
            0,
            "ctx edit",
        );
        // Put the bad primitive into A's store via the same put path the
        // engine uses, so `export_closure(&[bad_oid])` walks back through
        // its tree, blob and ancestors.
        let bad_oid = a.store.put(&bad_prim).unwrap();

        // A second engine B integrates a closure that includes the bad
        // primitive AND the original post-delete closure. Layer 3 must
        // drop the bad prim from the frontier so main stays at the
        // deleted state.
        let mut b = MemEngine::create(id(3), "v", "").unwrap();
        let mut batch = a.export_closure(&a.frontier_tips().unwrap()).unwrap();
        batch.extend(a.export_closure(&[bad_oid]).unwrap());
        b.integrate(&batch).unwrap();
        let tree = main_tree(&mut b);
        assert!(
            !tree.contains_key("foo.md"),
            "Layer 3 must drop a ghost-add primitive lacking the readd trailer"
        );
    }

    #[test]
    fn ctx_init_publishes_immediately() {
        // Acceptance: "ctx init on a brand-new device with files publishes
        // them immediately (Layer 2 only blocks join-mode engines)." The
        // MemEngine equivalent of init is `MemEngine::create`; it must NOT
        // set bootstrap_pending.
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        assert!(!e.is_bootstrap_pending(), "fresh init must not be pending");
        let oid = e
            .commit_from_files(&files(&[("hello.md", "world")]))
            .unwrap();
        assert!(oid.is_some(), "fresh init must publish immediately");
    }

    #[test]
    fn bootstrap_pending_blocks_commit_until_cleared() {
        // Acceptance: "blocked commits return Ok(None) so the host doesn't
        // lose work, just retries. The handshake-completion edge clears
        // bootstrap_pending; Layer 1 then takes over."
        let mut e = MemEngine::create(id(1), "v", "").unwrap();
        e.mark_bootstrap_pending();
        assert!(e.is_bootstrap_pending());
        let res = e
            .commit_from_files(&files(&[("x.md", "1")]))
            .unwrap();
        assert!(
            res.is_none(),
            "bootstrap_pending must short-circuit commit_scoped"
        );
        e.mark_bootstrap_complete();
        assert!(!e.is_bootstrap_pending());
        let res = e
            .commit_from_files(&files(&[("x.md", "1")]))
            .unwrap();
        assert!(
            res.is_some(),
            "commit must resume once bootstrap_pending is cleared"
        );
    }

    #[test]
    fn protocol_version_bumped_to_v5() {
        // Acceptance: "The §13 handshake refuses to peer with SDKs below the
        // version that emits and respects the trailer". v4 → v5 across this
        // issue. Pin the constant so a future accidental downgrade fails a
        // test, not a deploy.
        assert_eq!(crate::wire::PROTO_VERSION, 5);
    }
}
