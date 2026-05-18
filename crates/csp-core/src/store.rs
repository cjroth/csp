//! Object stores. The merge/fold engine is store-agnostic: it only needs
//! content-addressed get/put/has. `MemStore` is used by the conformance
//! suite and the wasm thin profile; `DiskStore` is the stock-git-compatible
//! loose-object store under `<scope>/.context/git` (native, full node — §4,
//! §9.1).

use crate::error::{CspError, CspResult};
use crate::object::GitObject;
use crate::oid::Oid;
use std::collections::HashMap;

pub trait Store {
    fn get(&self, oid: Oid) -> CspResult<GitObject>;
    fn has(&self, oid: Oid) -> bool;
    fn put(&mut self, obj: &GitObject) -> CspResult<Oid>;
    /// Raw compressed loose-object bytes for the wire (§6.3). The peer
    /// recomputes the oid on receipt, so this stays content-verifiable.
    fn get_raw(&self, oid: Oid) -> CspResult<Vec<u8>> {
        Ok(self.get(oid)?.compress())
    }
    fn put_raw(&mut self, compressed: &[u8]) -> CspResult<Oid> {
        let obj = GitObject::decompress_and_parse(compressed)?;
        self.put(&obj)
    }
}

#[derive(Default, Clone)]
pub struct MemStore {
    objs: HashMap<Oid, GitObject>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.objs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.objs.is_empty()
    }
    pub fn oids(&self) -> impl Iterator<Item = &Oid> {
        self.objs.keys()
    }
}

impl Store for MemStore {
    fn get(&self, oid: Oid) -> CspResult<GitObject> {
        self.objs
            .get(&oid)
            .cloned()
            .ok_or_else(|| CspError::ObjectNotFound(oid.to_hex()))
    }
    fn has(&self, oid: Oid) -> bool {
        self.objs.contains_key(&oid)
    }
    fn put(&mut self, obj: &GitObject) -> CspResult<Oid> {
        let oid = obj.oid();
        self.objs.entry(oid).or_insert_with(|| obj.clone());
        Ok(oid)
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
mod disk {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Stock-git loose-object store: `<git_dir>/objects/ab/cdef…`. An
    /// unmodified `git --git-dir=<git_dir>` can read everything written here
    /// (§18 git-coherence). Engine-owned (§4): refs live alongside.
    pub struct DiskStore {
        git_dir: PathBuf,
    }

    impl DiskStore {
        /// Initialize a bare-style engine repo at `git_dir` with a decoupled
        /// worktree (`core.bare=false`, no `.git` at the scope root — §4).
        pub fn init(git_dir: &Path, work_tree: &Path) -> CspResult<Self> {
            fs::create_dir_all(git_dir.join("objects"))?;
            fs::create_dir_all(git_dir.join("refs/heads"))?;
            fs::create_dir_all(git_dir.join("refs/tags"))?;
            if !git_dir.join("HEAD").exists() {
                fs::write(git_dir.join("HEAD"), b"ref: refs/heads/main\n")?;
            }
            let cfg = format!(
                "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\tworktree = {}\n",
                work_tree.display()
            );
            fs::write(git_dir.join("config"), cfg)?;
            Ok(DiskStore { git_dir: git_dir.to_path_buf() })
        }

        pub fn open(git_dir: &Path) -> CspResult<Self> {
            if !git_dir.join("objects").exists() {
                return Err(CspError::Io(format!("no engine repo at {}", git_dir.display())));
            }
            Ok(DiskStore { git_dir: git_dir.to_path_buf() })
        }

        pub fn git_dir(&self) -> &Path {
            &self.git_dir
        }

        fn loose_path(&self, oid: Oid) -> PathBuf {
            let hex = oid.to_hex();
            self.git_dir
                .join("objects")
                .join(&hex[..2])
                .join(&hex[2..])
        }
    }

    impl Store for DiskStore {
        fn get(&self, oid: Oid) -> CspResult<GitObject> {
            let p = self.loose_path(oid);
            let bytes = fs::read(&p).map_err(|_| CspError::ObjectNotFound(oid.to_hex()))?;
            GitObject::decompress_and_parse(&bytes)
        }
        fn has(&self, oid: Oid) -> bool {
            self.loose_path(oid).exists()
        }
        fn put(&mut self, obj: &GitObject) -> CspResult<Oid> {
            let oid = obj.oid();
            let p = self.loose_path(oid);
            if p.exists() {
                return Ok(oid);
            }
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent)?;
            }
            // Atomic: write temp + rename so a concurrent reader never sees a
            // torn loose object.
            let tmp = p.with_extension("tmp");
            fs::write(&tmp, obj.compress())?;
            fs::rename(&tmp, &p)?;
            Ok(oid)
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use disk::DiskStore;
