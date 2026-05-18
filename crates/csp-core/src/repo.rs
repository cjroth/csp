//! The engine-owned, stock-git-compatible repository at
//! `<scope>/.context/git` with a decoupled worktree (§4, §9.1). Refs are
//! engine-managed: `refs/heads/main` (force-recomputed final fold commit),
//! `refs/heads/node/<NodeId>` (discovery), `refs/tags/snap/<name>`
//! (snapshots). Out-of-band writes are unsupported (§4); `ctx git` is a
//! read-only passthrough (§17). There is **never a `.git` at the scope
//! root**.

#![cfg(not(target_arch = "wasm32"))]

use crate::error::CspResult;
use crate::object::GitObject;
use crate::oid::Oid;
use crate::order::NodeId;
use crate::store::{DiskStore, Store};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Repo {
    git_dir: PathBuf,
    work_tree: PathBuf,
    pub store: DiskStore,
}

impl Repo {
    pub fn init(scope_root: &Path) -> CspResult<Repo> {
        let git_dir = scope_root.join(".context/git");
        let store = DiskStore::init(&git_dir, scope_root)?;
        Ok(Repo {
            git_dir,
            work_tree: scope_root.to_path_buf(),
            store,
        })
    }

    pub fn open(scope_root: &Path) -> CspResult<Repo> {
        let git_dir = scope_root.join(".context/git");
        let store = DiskStore::open(&git_dir)?;
        Ok(Repo {
            git_dir,
            work_tree: scope_root.to_path_buf(),
            store,
        })
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }
    pub fn work_tree(&self) -> &Path {
        &self.work_tree
    }

    fn ref_path(&self, name: &str) -> PathBuf {
        self.git_dir.join(name)
    }

    pub fn read_ref(&self, name: &str) -> Option<Oid> {
        let s = fs::read_to_string(self.ref_path(name)).ok()?;
        Oid::from_hex(s.trim()).ok()
    }

    pub fn update_ref(&self, name: &str, oid: Oid) -> CspResult<()> {
        let p = self.ref_path(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("lock");
        fs::write(&tmp, format!("{}\n", oid.to_hex()))?;
        fs::rename(&tmp, &p)?;
        Ok(())
    }

    /// Force-recompute `refs/heads/main` (CSP owns this ref — §4).
    pub fn set_main(&self, oid: Oid) -> CspResult<()> {
        self.update_ref("refs/heads/main", oid)
    }
    pub fn main(&self) -> Option<Oid> {
        self.read_ref("refs/heads/main")
    }

    /// `refs/heads/node/<NodeId>` MAY point at a node's latest primitive for
    /// discovery / `git log --all` (§5.5).
    pub fn set_node_tip(&self, node: &NodeId, oid: Oid) -> CspResult<()> {
        self.update_ref(&format!("refs/heads/node/{}", node.to_hex()), oid)
    }

    pub fn set_snapshot(&self, name: &str, oid: Oid) -> CspResult<()> {
        self.update_ref(&format!("refs/tags/snap/{name}"), oid)
    }

    pub fn list_node_tips(&self) -> Vec<Oid> {
        let dir = self.git_dir.join("refs/heads/node");
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                if let Ok(s) = fs::read_to_string(e.path()) {
                    if let Ok(o) = Oid::from_hex(s.trim()) {
                        out.push(o);
                    }
                }
            }
        }
        out
    }

    /// Scan the loose object database for every primitive commit present
    /// (robust discovery — the engine repo is engine-owned so this is the
    /// authoritative known set, independent of any trailer, §5.2).
    pub fn scan_primitives(&self) -> CspResult<Vec<Oid>> {
        let mut out = Vec::new();
        let objects = self.git_dir.join("objects");
        if let Ok(rd) = fs::read_dir(&objects) {
            for shard in rd.flatten() {
                let name = shard.file_name();
                let name = name.to_string_lossy();
                if name.len() != 2 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
                    continue;
                }
                if let Ok(files) = fs::read_dir(shard.path()) {
                    for f in files.flatten() {
                        let rest = f.file_name();
                        let hex = format!("{}{}", name, rest.to_string_lossy());
                        if let Ok(oid) = Oid::from_hex(&hex) {
                            if let Ok(GitObject::Commit(c)) = self.store.get(oid) {
                                if crate::fold::is_primitive(&c) {
                                    out.push(oid);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Reachability GC + loose-object hygiene (§9.2). Historical synthetic
    /// fold commits and primitives are reachable through the fold-chain spine
    /// from `refs/heads/main` (and snapshot tags), so a correct reachability
    /// sweep already retains them — fold commits are load-bearing history,
    /// never special-cased as garbage.
    pub fn gc(&self) -> CspResult<usize> {
        use std::collections::BTreeSet;
        let mut roots = Vec::new();
        if let Some(m) = self.main() {
            roots.push(m);
        }
        roots.extend(self.list_node_tips());
        let snap_dir = self.git_dir.join("refs/tags/snap");
        if let Ok(rd) = fs::read_dir(&snap_dir) {
            for e in rd.flatten() {
                if let Ok(s) = fs::read_to_string(e.path()) {
                    if let Ok(o) = Oid::from_hex(s.trim()) {
                        roots.push(o);
                    }
                }
            }
        }
        let live: BTreeSet<Oid> = crate::fold::reachable(&self.store, &roots)?;
        let objects = self.git_dir.join("objects");
        let mut pruned = 0;
        if let Ok(rd) = fs::read_dir(&objects) {
            for shard in rd.flatten() {
                let name = shard.file_name();
                let name = name.to_string_lossy();
                if name.len() != 2 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
                    continue;
                }
                if let Ok(files) = fs::read_dir(shard.path()) {
                    for f in files.flatten() {
                        let rest = f.file_name();
                        let hex = format!("{}{}", name, rest.to_string_lossy());
                        if let Ok(oid) = Oid::from_hex(&hex) {
                            if !live.contains(&oid) {
                                let _ = fs::remove_file(f.path());
                                pruned += 1;
                            }
                        }
                    }
                }
            }
        }
        Ok(pruned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::GitObject;

    #[test]
    fn refs_roundtrip_and_scan() {
        let td = tempfile::tempdir().unwrap();
        let mut repo = Repo::init(td.path()).unwrap();
        let blob = GitObject::Blob(b"hi".to_vec());
        let boid = repo.store.put(&blob).unwrap();
        assert!(repo.store.has(boid));
        let m0 = crate::fold::genesis(&mut repo.store).unwrap();
        repo.set_main(m0).unwrap();
        assert_eq!(repo.main(), Some(m0));
        // HEAD points at refs/heads/main (stock-git readable).
        let head = std::fs::read_to_string(repo.git_dir().join("HEAD")).unwrap();
        assert_eq!(head.trim(), "ref: refs/heads/main");
    }
}
