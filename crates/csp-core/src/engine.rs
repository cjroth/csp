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
    compute_main, frontier, genesis, parse_primitive_meta, reachable, verify_fold_commit,
};
use crate::identity::{build_primitive, parse_ssh_pubkey, verify_primitive, Identity};
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

/// One materialize action the host must apply to its working tree (§5.6).
/// `Defer` = a contended path: leave the user's bytes (they become a
/// primitive on the next commit). The engine has already recorded the
/// last-materialized hash for `Write`/`Remove` (it assumes the host applies
/// them atomically, exactly as the native `Vault::materialize` does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterializeOp {
    Write { path: String, content: Vec<u8> },
    Remove { path: String },
    Defer { path: String },
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
        let mut changed = false;
        for (p, c) in &scoped {
            if self.state.materialized.get(p) != Some(&blob_hash(c)) {
                changed = true;
                break;
            }
        }
        if !changed {
            for p in self.state.materialized.keys() {
                if !scoped.contains_key(p) {
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return Ok(None);
        }
        let tree = self.write_tree(&scoped)?;
        let parent = match self.main {
            Some(m) => m,
            None => genesis(&mut self.store)?,
        };
        let counter = self.state.next_counter();
        let prim = build_primitive(&self.identity, tree, parent, counter, now_unix(), "ctx edit");
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
        for (p, c) in &scoped {
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
        let main = match self.main {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        let tree = match self.store.get(main)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("main is not a commit".into())),
        };
        let want = read_tree_to_files(tree, &|o| self.store.get(o))?;
        let mut ops = Vec::new();

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
}
