//! The high-level engine surface (native / full node, §16). Ties the
//! engine-owned repo (§4), the deterministic fold (§5.3), scope filtering
//! (§11), the §5.6 no-feedback-loop materialization, PITR/snapshots (§8),
//! and object integration (§6.3) together. The CLI and the net layer drive
//! this; no protocol logic lives above it.

#![cfg(not(target_arch = "wasm32"))]

use crate::error::{CspError, CspResult};
use crate::fold::{
    compute_main, frontier, genesis, parse_primitive_meta, reachable, verify_fold_commit,
};
use crate::identity::{build_primitive, parse_ssh_pubkey, verify_primitive, Identity};
use crate::object::{read_tree_to_files, write_tree_from_files, GitObject};
use crate::oid::Oid;
use crate::order::NodeId;
use crate::repo::Repo;
use crate::scope::{Scope, CONTEXTIGNORE, CONTEXT_DIR};
use crate::state::{EngineState, Snapshot};
use crate::config::VaultConfig;
use crate::store::Store;
use std::collections::{BTreeMap, BTreeSet};
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
        // NB: `.contextignore` is *synced, user-managed* scope content
        // (§11). We deliberately do NOT pre-create it: an empty init
        // artifact would look like a pending user edit on a fresh node and
        // the §5.6 no-clobber rule would defer the real synced one forever.
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
    pub fn commit_local_changes(&mut self) -> CspResult<Option<Oid>> {
        self.refresh_scope();
        let files = self.scan()?;
        let mut changed = false;
        for (p, c) in &files {
            let h = blob_hash(c);
            if self.state.materialized.get(p) != Some(&h) {
                changed = true;
                break;
            }
        }
        if !changed {
            // A removal also counts as a change.
            for p in self.state.materialized.keys() {
                if !files.contains_key(p) {
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            return Ok(None);
        }
        let tree = self.write_tree(&files)?;
        let parent = self.repo.main().unwrap_or_else(|| {
            // Should not happen (M₀ always set), but stay safe.
            genesis(&mut self.repo.store).unwrap()
        });
        let counter = self.state.next_counter();
        let prim = build_primitive(
            &self.identity,
            tree,
            parent,
            counter,
            now_unix(),
            "ctx edit",
        );
        let oid = self.repo.store.put(&prim)?;
        self.state.add_known(oid);
        self.repo.set_node_tip(&self.identity.node_id(), oid)?;
        self.recompute_and_materialize()?;
        self.state.save(&self.context)?;
        Ok(Some(oid))
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
    /// **signed** primitives whose author key is in this node's local
    /// authorized set (per-author, not per-connection — §6.1/§10).
    /// Synthetic fold commits are accepted only by recompute-verification.
    /// Returns the number of new primitives admitted.
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
        let authorized = self.authorized_node_ids()?;
        let mut admitted = 0;
        for r in raws {
            if let Ok(GitObject::Commit(c)) = GitObject::decompress_and_parse(r) {
                if let Some((counter, _node)) = parse_primitive_meta(&c) {
                    let author = match verify_primitive(&c) {
                        Ok(n) => n,
                        Err(_) => continue, // unverifiable → drop, don't forward
                    };
                    if !authorized.is_empty() && !authorized.contains(&author) {
                        continue; // unauthorized author → drop (§6.1/§10)
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

    pub fn authorized_node_ids(&self) -> CspResult<BTreeSet<NodeId>> {
        let mut set = BTreeSet::new();
        if let Ok(s) = std::fs::read_to_string(self.authorized_keys_path()) {
            for line in s.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some(n) = parse_ssh_pubkey(line) {
                    set.insert(n);
                }
            }
        }
        Ok(set)
    }

    pub fn authorize(&self, ssh_line: &str) -> CspResult<()> {
        let p = self.authorized_keys_path();
        let mut cur = std::fs::read_to_string(&p).unwrap_or_default();
        if cur.lines().any(|l| l.trim() == ssh_line.trim()) {
            return Ok(());
        }
        if !cur.is_empty() && !cur.ends_with('\n') {
            cur.push('\n');
        }
        cur.push_str(ssh_line.trim());
        cur.push('\n');
        std::fs::write(&p, cur)?;
        Ok(())
    }

    pub fn revoke(&self, ssh_line: &str) -> CspResult<()> {
        let p = self.authorized_keys_path();
        let cur = std::fs::read_to_string(&p).unwrap_or_default();
        let target = parse_ssh_pubkey(ssh_line.trim());
        let kept: Vec<&str> = cur
            .lines()
            .filter(|l| {
                let t = l.trim();
                if t.is_empty() || t.starts_with('#') {
                    return true;
                }
                match (parse_ssh_pubkey(t), target) {
                    (Some(a), Some(b)) => a != b,
                    _ => t != ssh_line.trim(),
                }
            })
            .collect();
        std::fs::write(&p, format!("{}\n", kept.join("\n")).trim_start().to_string())?;
        Ok(())
    }

    /// Bootstrap TOFU (§10): while the local authorized set is empty, the
    /// first connecting key is recorded. Disabled by `no_tofu`. Returns
    /// `true` if the key was accepted (already-trusted or just TOFU-added).
    pub fn admit_peer_tofu(&self, ssh_line: &str) -> CspResult<bool> {
        let set = self.authorized_node_ids()?;
        if let Some(n) = parse_ssh_pubkey(ssh_line) {
            if set.contains(&n) {
                return Ok(true);
            }
        }
        if set.is_empty() && !self.config.no_tofu {
            self.authorize(ssh_line)?;
            return Ok(true);
        }
        Ok(false)
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
    fn unauthorized_author_is_dropped() {
        let ta = tempdir().unwrap();
        let tb = tempdir().unwrap();
        let mut a = Vault::create(ta.path(), id(1), "v").unwrap();
        let mut b = Vault::create(tb.path(), id(2), "v").unwrap();
        // a authorizes nobody-but-itself is empty → but b authorizes only a.
        b.authorize(&id(1).to_ssh_string()).unwrap();
        // c is an unauthorized author
        let tc = tempdir().unwrap();
        let mut c = Vault::create(tc.path(), id(9), "v").unwrap();
        let _ = &mut a;
        std::fs::write(c.root().join("evil.md"), "pwned").unwrap();
        c.commit_local_changes().unwrap();
        let cx = c.export_all().unwrap();
        let admitted = b.integrate(&cx).unwrap();
        assert_eq!(admitted, 0, "unauthorized-author primitive must be dropped");
        assert!(!tb.path().join("evil.md").exists());
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
