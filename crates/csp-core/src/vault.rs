//! The high-level engine surface (native / full node, §16). Ties the
//! engine-owned repo (§4), the deterministic fold (§5.3), scope filtering
//! (§11), the §5.6 no-feedback-loop materialization, PITR/snapshots (§8),
//! and object integration (§6.3) together. The CLI and the net layer drive
//! this; no protocol logic lives above it.

#![cfg(not(target_arch = "wasm32"))]

use crate::authkeys::{self, Expiry, KeyEntry};
use crate::config::{VaultConfig, BUILTIN_DEFAULT_TTL_DAYS};
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
use crate::repo::Repo;
use crate::scope::{canonicalize_keeps, Scope, CONTEXTIGNORE, CONTEXT_DIR};
use crate::state::{EngineState, Snapshot};
use crate::store::Store;
use fs2::FileExt;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// `VaultConfig` now lives in `crate::config` (always-on / wasm-safe model +
// TOML; on-disk file I/O `cfg`-gated there). `ctx` and the wasm SDK share
// the exact same `.context/config` bytes (§9.1).

pub struct Vault {
    root: PathBuf,
    context: PathBuf,
    repo: Repo,
    state: EngineState,
    scope: Scope,
    identity: Identity,
    pub config: VaultConfig,
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn blob_hash(content: &[u8]) -> String {
    GitObject::Blob(content.to_vec()).oid().to_hex()
}

/// Issue 0014 — UTC-iso bucket name for the `.context/orphans/<…>/<path>`
/// quarantine folder. Colon-free so NTFS / APFS both accept the path.
fn orphans_bucket(t: u64) -> String {
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

impl Vault {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn context_dir(&self) -> &Path {
        &self.context
    }
    pub fn node_id(&self) -> NodeId {
        self.identity.node_id()
    }
    pub fn identity_ssh(&self) -> String {
        self.identity.to_ssh_string()
    }
    pub fn identity_clone(&self) -> Identity {
        self.identity.clone()
    }
    /// Detached ed25519 sign (the §10 handshake transcript). Used by the
    /// sans-IO [`crate::session::Session`] via `SessionVault`.
    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.identity.sign(msg)
    }
    pub fn repo(&self) -> &Repo {
        &self.repo
    }
    pub fn vault_id(&self) -> &str {
        &self.config.vault_id
    }
    /// Human label (may be empty). Not a uniqueness guarantee — see
    /// [`VaultConfig::name`].
    pub fn name(&self) -> &str {
        &self.config.name
    }
    pub fn set_name(&mut self, name: &str) -> CspResult<()> {
        if self.config.name != name {
            self.config.name = name.to_string();
            self.save_config()?;
        }
        Ok(())
    }
    pub fn main(&self) -> Option<Oid> {
        self.repo.main()
    }

    /// `ctx init`: create a new, empty scoped vault here (§17). Genesis M₀ is
    /// the deterministic root; `refs/heads/main` starts at M₀.
    pub fn create(root: &Path, identity: Identity, vault_id: &str) -> CspResult<Vault> {
        let context = root.join(CONTEXT_DIR);
        std::fs::create_dir_all(&context)?;
        let mut repo = Repo::init(root)?;
        let m0 = genesis(&mut repo.store)?;
        repo.set_main(m0)?;
        let config = VaultConfig {
            vault_id: vault_id.to_string(),
            name: String::new(),
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
        config.save(&context)?;
        let state = EngineState {
            vault_id: vault_id.to_string(),
            ..Default::default()
        };
        state.save(&context)?;
        let ak = context.join("authorized_keys");
        if !ak.exists() {
            std::fs::write(&ak, b"")?;
        }
        // NB: `Vault::create` itself does NOT seed `.contextignore`.
        // Clone also reaches this path (`ctx clone` → `Vault::create`),
        // and there the peer's synced `.contextignore` is authoritative —
        // pre-writing locally would trip the §5.6 no-clobber rule and
        // defer the synced one. The `ctx init` CLI handler seeds a default
        // (markdown-only, [`crate::scope::DEFAULT_CONTEXTIGNORE`]) only on
        // genuine init, after this returns.
        let scope = Scope {
            include: config.include.clone(),
            ignore: Vec::new(),
            allow_binary: config.allow_binary,
        };
        Ok(Vault {
            root: root.to_path_buf(),
            context,
            repo,
            state,
            scope,
            identity,
            config,
        })
    }

    pub fn open(root: &Path, identity: Identity) -> CspResult<Vault> {
        let context = root.join(CONTEXT_DIR);
        let repo = Repo::open(root)?;
        let config = VaultConfig::load(&context)?;
        let mut state = EngineState::load(&context)?;
        // The engine repo is authoritative for the known primitive set (§5.2).
        for p in repo.scan_primitives()? {
            state.add_known(p);
            if let Ok(GitObject::Commit(c)) = repo.store.get(p) {
                if let Some((counter, _)) = parse_primitive_meta(&c) {
                    state.observe(counter);
                }
            }
        }
        state.save(&context)?;
        let scope = Scope {
            include: config.include.clone(),
            ignore: load_ignore(root, &context),
            allow_binary: config.allow_binary,
        };
        Ok(Vault {
            root: root.to_path_buf(),
            context,
            repo,
            state,
            scope,
            identity,
            config,
        })
    }

    fn refresh_scope(&mut self) {
        self.scope = Scope {
            include: self.config.include.clone(),
            ignore: load_ignore(&self.root, &self.context),
            allow_binary: self.config.allow_binary,
        };
    }

    /// Scan the working tree into a scope-filtered `path -> bytes` map
    /// (§11). `.context/` is excluded by the HARD INVARIANT in [`Scope`].
    pub fn scan(&self) -> CspResult<BTreeMap<String, Vec<u8>>> {
        let mut out = BTreeMap::new();
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                let rel = rel_path(&self.root, e.path());
                rel != CONTEXT_DIR && !rel.starts_with(&format!("{CONTEXT_DIR}/"))
            })
        {
            let entry = entry.map_err(|e| CspError::Io(e.to_string()))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = rel_path(&self.root, entry.path());
            let content = std::fs::read(entry.path())?;
            if self.scope.content_in_scope(&rel, &content) {
                out.insert(rel, content);
            }
        }
        Ok(out)
    }

    fn write_tree(&mut self, files: &BTreeMap<String, Vec<u8>>) -> CspResult<Oid> {
        let mut put = Vec::new();
        let root = write_tree_from_files(files, &mut |o| {
            put.push(o.clone());
            Ok(())
        })?;
        for o in &put {
            self.repo.store.put(o)?;
        }
        Ok(root)
    }

    /// §5.6 reconcile-by-content. Compare each in-scope file's current hash
    /// to its last-materialized hash; if anything genuinely changed (a user
    /// edit, not a self-write), author one signed primitive parented on the
    /// fold commit this node currently holds, recompute `main`, materialize.
    /// Returns the new primitive oid if a commit was made.
    /// Surface physically-empty in-scope directories as `<dir>/.keep`
    /// sentinels (the `ctx` host's spec-§11 duty). The engine's
    /// `canonicalize_keeps` then strips any that gained a real file.
    fn inject_empty_dir_keeps(
        &self,
        files: &mut BTreeMap<String, Vec<u8>>,
    ) -> CspResult<()> {
        for entry in walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                let rel = rel_path(&self.root, e.path());
                rel != CONTEXT_DIR && !rel.starts_with(&format!("{CONTEXT_DIR}/"))
            })
        {
            let entry = entry.map_err(|e| CspError::Io(e.to_string()))?;
            if !entry.file_type().is_dir() {
                continue;
            }
            let rel = rel_path(&self.root, entry.path());
            if rel.is_empty() || !self.scope.path_in_scope(&rel) {
                continue;
            }
            let mut rd =
                std::fs::read_dir(entry.path()).map_err(|e| CspError::Io(e.to_string()))?;
            if rd.next().is_none() {
                files.insert(format!("{rel}/.keep"), Vec::new());
            }
        }
        Ok(())
    }

    pub fn commit_local_changes(&mut self) -> CspResult<Option<Oid>> {
        // Layer 2 — explicit bootstrap mode (issue 0014). A `join`-created
        // vault blocks `commit_local_changes` until the §13 handshake reports
        // catch-up completion. Returning Ok(None) so the host doesn't lose
        // work — it just retries on the next tick.
        if self.state.bootstrap_pending {
            return Ok(None);
        }
        self.refresh_scope();
        let mut files = self.scan()?;
        self.inject_empty_dir_keeps(&mut files)?;
        let files = canonicalize_keeps(&files, &self.scope);

        // Layer 1 — pre-publish ghost-add guard (issue 0014). Classify each
        // path against the prospective parent's committed tree and its
        // ancestor closure, using `state.materialized` as the intent signal
        // to distinguish a stale on-disk file from genuine new user intent.
        //
        // Perf: the parent's tree is probed structurally one path at a time
        // (`path_present_in_tree`) — never loaded as a flat path→bytes map.
        // A 200 KiB blob in the parent tree would otherwise be read from
        // the store on every commit tick.
        let parent = self.repo.main().unwrap_or_else(|| {
            // Should not happen (M₀ always set), but stay safe.
            genesis(&mut self.repo.store).unwrap()
        });
        let parent_tree_oid = match self.repo.store.get(parent)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("main is not a commit".into())),
        };
        let mut filtered: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut readds: Vec<Oid> = Vec::new();
        let mut quarantines: Vec<(String, String)> = Vec::new();
        let ts = orphans_bucket(now_unix());
        for (path, bytes) in files.iter() {
            if path_present_in_tree(&self.repo.store, parent_tree_oid, path)? {
                filtered.insert(path.clone(), bytes.clone());
                continue;
            }
            let touch = most_recent_touch(&self.repo.store, parent, path)?;
            match touch {
                None => {
                    filtered.insert(path.clone(), bytes.clone());
                }
                Some((_, TouchKind::Add)) => {
                    filtered.insert(path.clone(), bytes.clone());
                }
                Some((delete_oid, TouchKind::Delete)) => {
                    if self.state.materialized.contains_key(path) {
                        let to = format!("{CONTEXT_DIR}/orphans/{ts}/{path}");
                        quarantines.push((path.clone(), to));
                    } else {
                        filtered.insert(path.clone(), bytes.clone());
                        if !readds.contains(&delete_oid) {
                            readds.push(delete_oid);
                        }
                    }
                }
            }
        }

        // Apply quarantine moves to disk (fail-closed: if any move fails,
        // skip that path's quarantine but still honor the delete on the
        // published tree — i.e. drop the path from `filtered`, never
        // publish the ghost-add, and never delete the source file).
        for (from, to) in &quarantines {
            let src = self.root.join(from);
            let dst = self.root.join(to);
            if let Some(parent_dir) = dst.parent() {
                if std::fs::create_dir_all(parent_dir).is_err() {
                    tracing::warn!(
                        from = %from,
                        to = %to,
                        "ghost-add quarantine failed (mkdir); leaving file in place"
                    );
                    continue;
                }
            }
            if let Err(e) = std::fs::rename(&src, &dst) {
                tracing::warn!(
                    from = %from,
                    to = %to,
                    err = %e,
                    "ghost-add quarantine failed (rename); leaving file in place"
                );
                continue;
            }
            self.state.materialized.remove(from);
        }
        if !quarantines.is_empty() {
            let count = quarantines.len();
            tracing::info!(count, "quarantined {count} stale files");
        }

        let mut changed = false;
        for (p, c) in &filtered {
            let h = blob_hash(c);
            if self.state.materialized.get(p) != Some(&h) {
                changed = true;
                break;
            }
        }
        if !changed {
            // A removal also counts as a change.
            for p in self.state.materialized.keys() {
                if !filtered.contains_key(p) {
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            if !quarantines.is_empty() {
                // Quarantines mutated `materialized` — persist that even
                // though we did not author a primitive.
                self.state.save(&self.context)?;
            }
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
        let oid = self.repo.store.put(&prim)?;
        self.state.add_known(oid);
        self.repo.set_node_tip(&self.identity.node_id(), oid)?;
        self.recompute_and_materialize()?;
        self.state.save(&self.context)?;
        Ok(Some(oid))
    }

    /// Issue 0014 — Layer 2 entry point. Mark the vault as
    /// `bootstrap_pending`: every subsequent `commit_local_changes` returns
    /// `Ok(None)` until [`Self::mark_bootstrap_complete`] is called. Wired
    /// into `ctx join` / `ctx clone`.
    pub fn mark_bootstrap_pending(&mut self) -> CspResult<()> {
        self.state.bootstrap_pending = true;
        self.state.save(&self.context)
    }

    /// Issue 0014 — Layer 2 unblock. Called by the session after a complete
    /// catch-up batch integrates; clears the bootstrap flag so authoring
    /// resumes.
    pub fn mark_bootstrap_complete(&mut self) {
        if self.state.bootstrap_pending {
            self.state.bootstrap_pending = false;
            let _ = self.state.save(&self.context);
        }
    }

    pub fn is_bootstrap_pending(&self) -> bool {
        self.state.bootstrap_pending
    }

    fn recompute_and_materialize(&mut self) -> CspResult<()> {
        let known = self.state.known_oids()?;
        let main = compute_main(&mut self.repo.store, &known)?;
        self.repo.set_main(main)?;
        self.materialize()?;
        Ok(())
    }

    /// Materialize `main`'s tree onto disk (§5.3 step 5) with the §5.6
    /// no-clobber rule: never overwrite a path whose on-disk bytes differ
    /// from its last-materialized hash *and* whose target content differs —
    /// defer it (leave the user's bytes; they become a primitive).
    pub fn materialize(&mut self) -> CspResult<()> {
        let main = match self.repo.main() {
            Some(m) => m,
            None => return Ok(()),
        };
        let tree = match self.repo.store.get(main)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("main is not a commit".into())),
        };
        let want = read_tree_to_files(tree, &|o| self.repo.store.get(o))?;

        // Remove files we previously materialized that main no longer has,
        // unless the user has since modified them on disk.
        let prev: Vec<String> = self.state.materialized.keys().cloned().collect();
        for p in prev {
            if want.contains_key(&p) {
                continue;
            }
            let abs = self.root.join(&p);
            if let Ok(disk) = std::fs::read(&abs) {
                if blob_hash(&disk) == self.state.materialized[&p] {
                    let _ = std::fs::remove_file(&abs);
                    // A folder rename is N per-file renames in the
                    // file-only model; without this the emptied old
                    // directory lingers on disk on the receiving node.
                    prune_empty_dirs(&self.root, abs.parent());
                }
            }
            self.state.materialized.remove(&p);
        }

        for (p, content) in &want {
            let abs = self.root.join(p);
            let last = self.state.materialized.get(p).cloned();
            let on_disk = std::fs::read(&abs).ok();
            if let Some(d) = &on_disk {
                let dh = blob_hash(d);
                let contended = Some(&dh) != last.as_ref();
                if contended && d != content {
                    // Pending user edit + main wants different content →
                    // DEFER (§5.6): leave the user's bytes untouched.
                    continue;
                }
                if d == content {
                    self.state.materialized.insert(p.clone(), blob_hash(content));
                    continue;
                }
            }
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = abs.with_extension("ctx-tmp");
            std::fs::write(&tmp, content)?; // atomic: temp + rename (§5.6)
            std::fs::rename(&tmp, &abs)?;
            self.state.materialized.insert(p.clone(), blob_hash(content));
        }
        self.state.save(&self.context)?;
        Ok(())
    }

    /// Integrate received raw objects (§6.3): object layer first, then admit
    /// every primitive whose author signature verifies. **Admission is
    /// connection-level (§6.1/§10), not per-primitive**: the author NodeId is
    /// not required to be in this node's `authorized_keys`. The signature
    /// check is content integrity only — a missing or invalid signature means
    /// a corrupt/forged primitive (dropped). Synthetic fold commits are
    /// admitted only by recompute-verification. Returns the number of new
    /// primitives admitted.
    pub fn integrate(&mut self, raws: &[Vec<u8>]) -> CspResult<usize> {
        for r in raws {
            // Content-addressed + hash-verified on put.
            self.repo.store.put_raw(r)?;
        }
        // "Verified, not trusted": recompute-verify every received synthetic
        // fold commit (recursively to primitives/M₀), exactly once, on
        // receipt. A peer that sends an unreproducible fold commit has an
        // invalid/forged DAG → drop the whole batch (admit nothing, relay
        // nothing); honest peers' folds always reproduce byte-identically by
        // construction.
        for r in raws {
            if let Ok(GitObject::Commit(c)) = GitObject::decompress_and_parse(r) {
                if parse_primitive_meta(&c).is_none() {
                    let oid = GitObject::Commit(c).oid();
                    if verify_fold_commit(&mut self.repo.store, oid).is_err() {
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
                if let Some((counter, _node)) = parse_primitive_meta(&c) {
                    if verify_primitive(&c).is_err() {
                        // Corrupt / forged primitive — drop and don't
                        // forward. Structural, not policy.
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
            self.recompute_and_materialize()?;
        }
        self.state.save(&self.context)?;
        Ok(admitted)
    }

    /// The frontier (un-merged primitive tip SHAs) for catch-up
    /// anti-entropy (§6.4) — small: one per concurrent lineage.
    pub fn frontier_tips(&self) -> CspResult<Vec<Oid>> {
        let known = self.state.known_oids()?;
        frontier(&self.repo.store, &known)
    }

    pub fn known(&self) -> CspResult<Vec<Oid>> {
        self.state.known_oids()
    }

    /// Raw reachable closure of `tips` (§6.4 catch-up delivery unit). Always
    /// self-contained — bottoms out at M₀, so the receiver never has a
    /// dangling parent (complete-DAG invariant §5.4(1)).
    pub fn export_closure(&self, tips: &[Oid]) -> CspResult<Vec<Vec<u8>>> {
        let set: BTreeSet<Oid> = reachable(&self.repo.store, tips)?;
        set.into_iter().map(|o| self.repo.store.get_raw(o)).collect()
    }

    pub fn export_all(&self) -> CspResult<Vec<Vec<u8>>> {
        let known = self.state.known_oids()?;
        self.export_closure(&known)
    }

    // ---- Authorization (§10) -------------------------------------------

    fn authorized_keys_path(&self) -> PathBuf {
        self.context.join("authorized_keys")
    }

    fn authorized_keys_lock_path(&self) -> PathBuf {
        self.context.join("authorized_keys.lock")
    }

    /// The configured default TTL for new `authorized_keys` entries (§10).
    /// Honors `config.default_key_ttl_days` (None → built-in 90, Some(0) →
    /// no expiry token).
    fn default_ttl_days(&self) -> u64 {
        self.config.default_key_ttl_days.unwrap_or(BUILTIN_DEFAULT_TTL_DAYS)
    }

    /// Currently-valid authorized NodeIds (expired entries are filtered out
    /// at admit time; lines remain in the file for audit).
    pub fn authorized_node_ids(&self) -> CspResult<BTreeSet<NodeId>> {
        let now = now_unix();
        let mut set = BTreeSet::new();
        if let Ok(s) = std::fs::read_to_string(self.authorized_keys_path()) {
            for e in authkeys::parse_file(&s) {
                if let Some(n) = e.node {
                    if e.expiry.is_valid(now) {
                        set.insert(n);
                    }
                }
            }
        }
        Ok(set)
    }

    /// All authorized entries (including expired). For `ctx auth list`.
    pub fn authorized_entries(&self) -> CspResult<Vec<KeyEntry>> {
        let s = std::fs::read_to_string(self.authorized_keys_path()).unwrap_or_default();
        Ok(authkeys::parse_file(&s)
            .into_iter()
            .filter(|e| e.is_key())
            .collect())
    }

    /// Take an exclusive file lock on `authorized_keys` for the duration of
    /// `f`. Ensures enrollment writes, manual `authorize`/`revoke`, and the
    /// listen-start migration don't race each other (across processes too).
    fn with_authorized_lock<F, T>(&self, f: F) -> CspResult<T>
    where
        F: FnOnce(&Path) -> CspResult<T>,
    {
        std::fs::create_dir_all(&self.context)?;
        let lockp = self.authorized_keys_lock_path();
        let lf = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lockp)?;
        FileExt::lock_exclusive(&lf)?;
        let res = f(&self.authorized_keys_path());
        let _ = FileExt::unlock(&lf);
        res
    }

    /// Atomic write of `authorized_keys` (tmp + rename) inside the file lock.
    fn write_authorized_atomic(&self, path: &Path, content: &str) -> CspResult<()> {
        let tmp = path.with_extension("ak-tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Add a key to `authorized_keys` with a specific `Expiry`. If an entry
    /// for the same NodeId already exists, its `expires=` is replaced.
    pub fn authorize_with_expiry(&self, ssh_line: &str, expiry: Expiry) -> CspResult<()> {
        let target_node = parse_ssh_pubkey(ssh_line.trim());
        self.with_authorized_lock(|p| {
            let cur = std::fs::read_to_string(p).unwrap_or_default();
            let mut entries = authkeys::parse_file(&cur);
            let new_line = authkeys::build_line(ssh_line, expiry);
            if let Some(t) = target_node {
                if let Some(i) = entries.iter().position(|e| e.node == Some(t)) {
                    // Replace existing entry with refreshed token / pubkey
                    // text, keeping the line in-place to preserve ordering.
                    entries[i] = authkeys::parse_line(&new_line);
                    self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
                    return Ok(());
                }
            } else {
                // Not a valid ssh-ed25519 line. Skip — no-op like before.
                return Ok(());
            }
            entries.push(authkeys::parse_line(&new_line));
            self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
            Ok(())
        })
    }

    /// Back-compat wrapper: add a key with no expiry token. Listen-start
    /// migration applies the default TTL on next startup, matching the
    /// spec's footgun-guard rule for manually pasted lines.
    pub fn authorize(&self, ssh_line: &str) -> CspResult<()> {
        self.authorize_with_expiry(ssh_line, Expiry::Unset)
    }

    pub fn revoke(&self, ssh_line: &str) -> CspResult<()> {
        let target = parse_ssh_pubkey(ssh_line.trim());
        self.with_authorized_lock(|p| {
            let cur = std::fs::read_to_string(p).unwrap_or_default();
            let entries = authkeys::parse_file(&cur);
            let kept: Vec<KeyEntry> = entries
                .into_iter()
                .filter(|e| match (e.node, target) {
                    (Some(a), Some(b)) => a != b,
                    _ => e.raw.trim() != ssh_line.trim(),
                })
                .collect();
            self.write_authorized_atomic(p, &authkeys::serialize(&kept))
        })
    }

    /// Extend (or re-set) the expiry on every entry whose pubkey or comment
    /// fingerprint matches `ssh_or_prefix` (matches either the full ssh-ed25519
    /// line head, or a hex prefix of the NodeId, or the `comment` text after
    /// the base64). `more_days = None` → `expires=never`. Returns the number
    /// of entries updated.
    pub fn extend_expiry(&self, ssh_or_prefix: &str, more_days: Option<u64>) -> CspResult<usize> {
        let now = now_unix();
        let needle_node = parse_ssh_pubkey(ssh_or_prefix);
        let needle = ssh_or_prefix.trim().to_ascii_lowercase();
        self.with_authorized_lock(|p| {
            let cur = std::fs::read_to_string(p).unwrap_or_default();
            let mut entries = authkeys::parse_file(&cur);
            let mut updated = 0;
            for e in entries.iter_mut() {
                let Some(n) = e.node else { continue };
                let matches = needle_node.map(|t| t == n).unwrap_or(false)
                    || n.to_hex().starts_with(&needle)
                    || e.raw.to_ascii_lowercase().contains(&needle);
                if !matches {
                    continue;
                }
                let new_expiry = match more_days {
                    None => Expiry::Never,
                    Some(d) => Expiry::At(authkeys::expiry_from_ttl_days(now, d)),
                };
                let new_line = authkeys::build_line(&e.raw, new_expiry);
                *e = authkeys::parse_line(&new_line);
                updated += 1;
            }
            if updated > 0 {
                self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
            }
            Ok(updated)
        })
    }

    /// Listen-start migration (§10): rewrite every entry without an expiry
    /// token to `expires=<today + default_ttl_days>`. Idempotent. Returns
    /// the number of entries migrated.
    pub fn migrate_default_expiry(&self) -> CspResult<usize> {
        let ttl_days = self.default_ttl_days();
        if ttl_days == 0 {
            // Operator opted out of a default TTL — nothing to apply.
            return Ok(0);
        }
        let now = now_unix();
        self.with_authorized_lock(|p| {
            let cur = std::fs::read_to_string(p).unwrap_or_default();
            let mut entries = authkeys::parse_file(&cur);
            let mut migrated = 0;
            for e in entries.iter_mut() {
                if !e.is_key() {
                    continue;
                }
                if matches!(e.expiry, Expiry::Unset) {
                    let new_line = authkeys::build_line(
                        &e.raw,
                        Expiry::At(authkeys::expiry_from_ttl_days(now, ttl_days)),
                    );
                    *e = authkeys::parse_line(&new_line);
                    migrated += 1;
                }
            }
            if migrated > 0 {
                self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
                tracing::info!(
                    "authorized_keys: applied default {ttl_days}d expiry to \
                     {migrated} entry(s)"
                );
            }
            Ok(migrated)
        })
    }

    /// Enrollment-aware connection admission (§10). Replaces the older
    /// TOFU-only `admit_peer_tofu`. Returns `true` if the peer is admitted;
    /// mutates `authorized_keys` to enroll a new peer (auth-key path),
    /// refresh an expired entry's `expires=`, or TOFU-record a first peer.
    ///
    /// - Already-authorized and not expired → accept (no-op).
    /// - `enrollment_authorized` true (caller validated an auth key) → write
    ///   or refresh entry with default TTL, accept.
    /// - `enrollment_authorized` false:
    ///   - Expired existing entry → reject (operator must extend or
    ///     re-enroll via auth key).
    ///   - Unknown peer + no auth keys configured + authorized set empty +
    ///     `!no_tofu` → TOFU enroll with default TTL.
    ///   - Otherwise → reject.
    pub fn admit_peer(&self, ssh_line: &str, enrollment_authorized: bool) -> CspResult<bool> {
        let now = now_unix();
        let peer_node = match parse_ssh_pubkey(ssh_line) {
            Some(n) => n,
            None => return Ok(false),
        };
        let ttl_days = self.default_ttl_days();
        let enroll_expiry = || -> Expiry {
            if ttl_days == 0 {
                Expiry::Unset
            } else {
                Expiry::At(authkeys::expiry_from_ttl_days(now, ttl_days))
            }
        };
        self.with_authorized_lock(|p| {
            let cur = std::fs::read_to_string(p).unwrap_or_default();
            let mut entries = authkeys::parse_file(&cur);
            // Existing entry for this pubkey?
            if let Some(i) = entries.iter().position(|e| e.node == Some(peer_node)) {
                let valid = entries[i].expiry.is_valid(now);
                if valid {
                    return Ok(true);
                }
                if enrollment_authorized {
                    let line = authkeys::build_line(ssh_line, enroll_expiry());
                    entries[i] = authkeys::parse_line(&line);
                    self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
                    tracing::info!(
                        "authorized_keys: refreshed expired entry via auth-key \
                         enrollment for peer={}",
                        &peer_node.to_hex()[..12]
                    );
                    return Ok(true);
                }
                return Ok(false);
            }
            // Unknown peer.
            if enrollment_authorized {
                let line = authkeys::build_line(ssh_line, enroll_expiry());
                entries.push(authkeys::parse_line(&line));
                self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
                tracing::info!(
                    "authorized_keys: enrolled new peer={} via auth-key",
                    &peer_node.to_hex()[..12]
                );
                return Ok(true);
            }
            // TOFU: only when the file has no key entries, no_tofu is off,
            // and no auth keys are configured (auth-keys disable TOFU).
            let any_key = entries.iter().any(|e| e.is_key());
            if !any_key && !self.config.no_tofu && self.config.auth_keys.is_empty() {
                let line = authkeys::build_line(ssh_line, enroll_expiry());
                entries.push(authkeys::parse_line(&line));
                self.write_authorized_atomic(p, &authkeys::serialize(&entries))?;
                tracing::info!(
                    "authorized_keys: TOFU-recorded first peer={}",
                    &peer_node.to_hex()[..12]
                );
                return Ok(true);
            }
            Ok(false)
        })
    }

    /// Legacy name kept so existing call sites (the wasm-side SessionVault)
    /// still compile. Equivalent to `admit_peer(ssh_line, false)`.
    pub fn admit_peer_tofu(&self, ssh_line: &str) -> CspResult<bool> {
        self.admit_peer(ssh_line, false)
    }

    // ---- Point-in-time recovery (§8) -----------------------------------

    pub fn snapshot(&mut self, name: &str) -> CspResult<()> {
        let tips = self.frontier_tips()?;
        let main = self.repo.main().ok_or_else(|| CspError::Other("no main".into()))?;
        self.repo.set_snapshot(name, main)?;
        self.state.snapshots.insert(
            name.to_string(),
            Snapshot {
                label: name.to_string(),
                frontier: tips.iter().map(|o| o.to_hex()).collect(),
                created_unix: now_unix(),
            },
        );
        self.state.save(&self.context)?;
        Ok(())
    }

    fn fold_over_subset(&mut self, subset: &[Oid]) -> CspResult<BTreeMap<String, Vec<u8>>> {
        let main = compute_main(&mut self.repo.store, subset)?;
        let tree = match self.repo.store.get(main)? {
            GitObject::Commit(c) => c.tree,
            _ => return Err(CspError::Malformed("subset main not a commit".into())),
        };
        read_tree_to_files(tree, &|o| self.repo.store.get(o))
    }

    /// Restore to a named snapshot or a unix time. Applying a restore is
    /// just editing (§8): write the historical tree, then it becomes a new
    /// primitive on this node's lineage and converges normally. The
    /// pre-restore state stays fully in history.
    pub fn restore_snapshot(&mut self, name: &str) -> CspResult<()> {
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
        let tree = self.fold_over_subset(&subset)?;
        self.apply_restore(tree)
    }

    /// Time-based restore (§8): the *set* of primitives with author-time ≤ T,
    /// folded deterministically. Approximate under author-clock skew; the
    /// logical order stays authoritative for correctness.
    pub fn restore_time(&mut self, t_unix: u64) -> CspResult<()> {
        let known = self.state.known_oids()?;
        let mut subset = Vec::new();
        for o in known {
            if let GitObject::Commit(c) = self.repo.store.get(o)? {
                if c.author_time <= t_unix {
                    subset.push(o);
                }
            }
        }
        let tree = self.fold_over_subset(&subset)?;
        self.apply_restore(tree)
    }

    fn apply_restore(&mut self, tree: BTreeMap<String, Vec<u8>>) -> CspResult<()> {
        // Write the historical tree into the working files, then commit it
        // as an ordinary edit on this node's own lineage.
        for (p, content) in &tree {
            let abs = self.root.join(p);
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&abs, content)?;
            self.state.materialized.remove(p); // force re-detect as a user edit
        }
        // Remove in-scope files not present in the restored tree.
        let cur = self.scan()?;
        for p in cur.keys() {
            if !tree.contains_key(p) {
                let abs = self.root.join(p);
                let _ = std::fs::remove_file(&abs);
                prune_empty_dirs(&self.root, abs.parent());
            }
        }
        self.commit_local_changes()?;
        Ok(())
    }

    pub fn snapshots(&self) -> &BTreeMap<String, Snapshot> {
        &self.state.snapshots
    }

    pub fn save_config(&self) -> CspResult<()> {
        self.config.save(&self.context)
    }

    /// Append a pattern to the synced `.contextignore` (§11) and refresh.
    pub fn add_ignore_pattern(&mut self, pat: &str) -> CspResult<()> {
        let p = self.root.join(CONTEXTIGNORE);
        let mut s = std::fs::read_to_string(&p).unwrap_or_default();
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(pat);
        s.push('\n');
        std::fs::write(&p, s)?;
        self.refresh_scope();
        Ok(())
    }

    pub fn add_include_pattern(&mut self, pat: &str) -> CspResult<()> {
        if !self.config.include.iter().any(|i| i == pat) {
            self.config.include.push(pat.to_string());
            self.save_config()?;
            self.refresh_scope();
        }
        Ok(())
    }

    pub fn scope_summary(&self) -> (Vec<String>, Vec<String>) {
        (self.config.include.clone(), self.scope.ignore.clone())
    }
}

fn rel_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn load_ignore(root: &Path, context: &Path) -> Vec<String> {
    let mut globs = Vec::new();
    if let Ok(s) = std::fs::read_to_string(root.join(CONTEXTIGNORE)) {
        globs.extend(s.lines().map(|l| l.to_string()));
    }
    if let Ok(s) = std::fs::read_to_string(context.join("exclude")) {
        globs.extend(s.lines().map(|l| l.to_string()));
    }
    globs
}

/// Drop now-empty ancestor directories of a just-removed file, up to (but
/// not including) `root`. Stops at the first non-empty / unreadable dir.
/// The engine models files only — a folder rename is N per-file renames —
/// so the emptied source directory must be reaped here or it lingers.
fn prune_empty_dirs(root: &Path, start: Option<&Path>) {
    let mut cur = match start {
        Some(p) => p.to_path_buf(),
        None => return,
    };
    while cur != root && cur.starts_with(root) {
        let mut rd = match std::fs::read_dir(&cur) {
            Ok(rd) => rd,
            Err(_) => break,
        };
        if rd.next().is_some() {
            break; // not empty — stop climbing
        }
        if std::fs::remove_dir(&cur).is_err() {
            break;
        }
        match cur.parent() {
            Some(parent) => cur = parent.to_path_buf(),
            None => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn id(seed: u8) -> Identity {
        Identity::from_seed(&[seed; 32])
    }

    #[test]
    fn create_commit_materialize_and_no_feedback() {
        let td = tempdir().unwrap();
        let mut v = Vault::create(td.path(), id(1), "vault-1").unwrap();
        std::fs::write(td.path().join("a.md"), "hello").unwrap();
        let p = v.commit_local_changes().unwrap();
        assert!(p.is_some(), "user edit must produce a primitive");
        // main advanced past M₀, file still present, content intact.
        assert_eq!(
            std::fs::read_to_string(td.path().join("a.md")).unwrap(),
            "hello"
        );
        // §5.6: re-running with no user change must NOT create a commit
        // (self-writes are non-events by construction).
        assert!(v.commit_local_changes().unwrap().is_none());
        assert!(v.commit_local_changes().unwrap().is_none());
    }

    #[test]
    fn two_vaults_converge_in_process() {
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let mut a = Vault::create(ta.path(), id(1), "v").unwrap();
        let mut b = Vault::create(tb.path(), id(2), "v").unwrap();
        // mutual authorization
        a.authorize(&id(2).to_ssh_string()).unwrap();
        b.authorize(&id(1).to_ssh_string()).unwrap();

        std::fs::write(ta.path().join("a.md"), "from A").unwrap();
        a.commit_local_changes().unwrap();
        std::fs::write(tb.path().join("b.md"), "from B").unwrap();
        b.commit_local_changes().unwrap();

        let ax = a.export_all().unwrap();
        let bx = b.export_all().unwrap();
        a.integrate(&bx).unwrap();
        b.integrate(&ax).unwrap();

        assert_eq!(a.main(), b.main(), "must converge to identical main SHA");
        assert_eq!(
            std::fs::read_to_string(ta.path().join("b.md")).unwrap(),
            "from B"
        );
        assert_eq!(
            std::fs::read_to_string(tb.path().join("a.md")).unwrap(),
            "from A"
        );
    }

    #[test]
    fn folder_rename_reaps_empty_source_dir_on_receiver() {
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let mut a = Vault::create(ta.path(), id(1), "v").unwrap();
        let mut b = Vault::create(tb.path(), id(2), "v").unwrap();
        a.authorize(&id(2).to_ssh_string()).unwrap();
        b.authorize(&id(1).to_ssh_string()).unwrap();

        // A creates a folder of files; B materializes them.
        std::fs::create_dir_all(ta.path().join("src")).unwrap();
        std::fs::write(ta.path().join("src/a.md"), "A").unwrap();
        std::fs::write(ta.path().join("src/b.md"), "B").unwrap();
        a.commit_local_changes().unwrap();
        b.integrate(&a.export_all().unwrap()).unwrap();
        assert!(tb.path().join("src/a.md").exists());
        assert!(tb.path().join("src/b.md").exists());

        // A renames the folder src/ → dst/ (per-file moves in the file model).
        std::fs::create_dir_all(ta.path().join("dst")).unwrap();
        std::fs::rename(ta.path().join("src/a.md"), ta.path().join("dst/a.md")).unwrap();
        std::fs::rename(ta.path().join("src/b.md"), ta.path().join("dst/b.md")).unwrap();
        let _ = std::fs::remove_dir(ta.path().join("src"));
        a.commit_local_changes().unwrap();
        b.integrate(&a.export_all().unwrap()).unwrap();

        // Receiver moved the files AND reaped the now-empty source dir.
        assert!(tb.path().join("dst/a.md").exists(), "renamed files arrive");
        assert!(tb.path().join("dst/b.md").exists());
        assert!(
            !tb.path().join("src").exists(),
            "empty source folder must not linger on the receiver"
        );
    }

    #[test]
    fn empty_dir_round_trips_and_keep_lifecycle() {
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let mut a = Vault::create(ta.path(), id(1), "v").unwrap();
        let mut b = Vault::create(tb.path(), id(2), "v").unwrap();
        a.authorize(&id(2).to_ssh_string()).unwrap();
        b.authorize(&id(1).to_ssh_string()).unwrap();

        // A: a user-created empty directory replicates as a `.keep`.
        std::fs::create_dir_all(ta.path().join("notes/empty")).unwrap();
        a.commit_local_changes().unwrap();
        b.integrate(&a.export_all().unwrap()).unwrap();
        assert!(tb.path().join("notes/empty").is_dir(), "empty dir replicates");
        assert!(tb.path().join("notes/empty/.keep").exists());

        // A: a real file lands → the sentinel is deterministically dropped.
        std::fs::write(ta.path().join("notes/empty/x.md"), "hi").unwrap();
        a.commit_local_changes().unwrap();
        b.integrate(&a.export_all().unwrap()).unwrap();
        assert!(tb.path().join("notes/empty/x.md").exists());
        assert!(
            !tb.path().join("notes/empty/.keep").exists(),
            ".keep dropped once the folder holds a real file"
        );

        // A: the file is removed → the folder is empty again → `.keep` back.
        std::fs::remove_file(ta.path().join("notes/empty/x.md")).unwrap();
        a.commit_local_changes().unwrap();
        b.integrate(&a.export_all().unwrap()).unwrap();
        assert!(tb.path().join("notes/empty").is_dir());
        assert!(
            tb.path().join("notes/empty/.keep").exists(),
            ".keep re-added when the folder is emptied"
        );
        assert!(!tb.path().join("notes/empty/x.md").exists());
    }

    #[test]
    fn unknown_author_with_valid_signature_is_admitted() {
        // New model (§6.1/§10): admission is connection-level. A primitive
        // signed by a key the receiver does not locally know — but with a
        // valid signature — is *admitted* and materialized. The trust gate
        // is the connection (`authorized_keys` on the listener); per-author
        // checks at integrate time would only re-implement that gate in a
        // more expensive form, and broke multi-writer convergence through a
        // relay because each reader had to enumerate every writer.
        let tb = tempdir().unwrap();
        let mut b = Vault::create(tb.path(), id(2), "v").unwrap();
        // b's authorized set is non-empty but does NOT include id(9).
        b.authorize(&id(1).to_ssh_string()).unwrap();
        // c authors under a key b has never heard of (id(9)).
        let tc = tempdir().unwrap();
        let mut c = Vault::create(tc.path(), id(9), "v").unwrap();
        std::fs::write(c.root().join("from-stranger.md"), "hello").unwrap();
        c.commit_local_changes().unwrap();
        let cx = c.export_all().unwrap();
        let admitted = b.integrate(&cx).unwrap();
        assert!(admitted > 0, "unknown-but-valid-signature primitive admits");
        assert_eq!(
            std::fs::read_to_string(tb.path().join("from-stranger.md")).unwrap(),
            "hello",
            "and materializes into the working tree"
        );
    }

    #[test]
    fn bad_signature_primitive_is_dropped() {
        // Content-integrity check still runs: a primitive whose signature
        // does not verify against the claimed author key is structurally
        // corrupt and dropped — even when the receiver's authorized_keys is
        // empty (no admission policy at all). This is the *only* drop case
        // for primitives under the new model (§6.3).
        let tb = tempdir().unwrap();
        let mut b = Vault::create(tb.path(), id(2), "v").unwrap();
        // Build a real, valid signed primitive (parented on a parent oid
        // that doesn't actually need to resolve — admitted=0 short-circuits
        // recompute, so the unresolved parent is never walked).
        let parent = b.main().expect("M₀ at create");
        let tree = parent; // any Oid; unused on the admit-rejection path
        let prim_obj = crate::identity::build_primitive(&id(9), tree, parent, 1, 0, "ctx edit");
        let mut commit = match prim_obj {
            GitObject::Commit(c) => c,
            _ => panic!("build_primitive must return a Commit"),
        };
        // Mutate the message after signing — the embedded signature now
        // covers the pre-tamper payload, so verify_primitive will fail.
        commit.message = commit.message.replace("ctx edit", "evil edit");
        let tampered = GitObject::Commit(commit).compress();
        let admitted = b.integrate(&[tampered]).unwrap();
        assert_eq!(admitted, 0, "tampered primitive must be dropped");
    }

    #[test]
    fn snapshot_and_restore() {
        let td = tempdir().unwrap();
        let mut v = Vault::create(td.path(), id(7), "v").unwrap();
        std::fs::write(td.path().join("doc.md"), "v1").unwrap();
        v.commit_local_changes().unwrap();
        v.snapshot("first").unwrap();
        std::fs::write(td.path().join("doc.md"), "v1\nv2-EDIT").unwrap();
        v.commit_local_changes().unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path().join("doc.md")).unwrap(),
            "v1\nv2-EDIT"
        );
        v.restore_snapshot("first").unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path().join("doc.md")).unwrap(),
            "v1",
            "restore brings back the snapshot state"
        );
    }

    #[test]
    fn git_coherence_unmodified_git_can_read_engine_repo() {
        // §18 git-coherence: an unmodified `git` must log/show the
        // engine-owned repo at <scope>/.context/git with the decoupled
        // worktree (no `.git` at the scope root — §4).
        let td = tempdir().unwrap();
        let mut v = Vault::create(td.path(), id(4), "v").unwrap();
        std::fs::write(td.path().join("hello.md"), "git coherence\n").unwrap();
        v.commit_local_changes().unwrap();
        let main = v.main().unwrap().to_hex();
        let git_dir = td.path().join(".context/git");
        assert!(
            !td.path().join(".git").exists(),
            "there must be NO .git at the scope root (§4)"
        );
        let run = |args: &[&str]| -> String {
            let out = std::process::Command::new("git")
                .arg(format!("--git-dir={}", git_dir.display()))
                .arg(format!("--work-tree={}", td.path().display()))
                .args(args)
                .output()
                .expect("git binary available");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let log = run(&["log", "--format=%H", "main"]);
        assert!(log.contains(&main), "git log main must list the fold head");
        let show = run(&["show", "main:hello.md"]);
        assert_eq!(show, "git coherence\n", "git show must read the blob");
        let cat = run(&["cat-file", "-t", &main]);
        assert_eq!(cat.trim(), "commit");
    }

    #[test]
    fn pitr_by_time() {
        let td = tempdir().unwrap();
        let mut v = Vault::create(td.path(), id(3), "v").unwrap();
        std::fs::write(td.path().join("f.md"), "early").unwrap();
        v.commit_local_changes().unwrap();
        let t_mid = now_unix();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(td.path().join("f.md"), "late").unwrap();
        v.commit_local_changes().unwrap();
        v.restore_time(t_mid).unwrap();
        assert_eq!(
            std::fs::read_to_string(td.path().join("f.md")).unwrap(),
            "early"
        );
    }
}
