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
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Process-global temp-name sequence. A loose-object write stages bytes in
    /// a temp file before the atomic rename; that temp name must be unique per
    /// write so two concurrent `put`s of the *same* oid never clobber one
    /// another, and so an interrupted write (e.g. ENOSPC) never leaves a
    /// *predictably*-named artifact that a later run could mistake for live
    /// staging. `pid` separates processes; the counter separates writes within
    /// one. (Stock git uses mkstemp randomness for the same reason.)
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_tmp_name() -> String {
        let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        format!(".tmp_{}_{}", std::process::id(), n)
    }

    /// A live loose-object file is named exactly by the 38-hex oid remainder
    /// (`objects/ab/cdef…`, 40-hex oid minus the 2-hex shard). Anything else in
    /// a shard dir — a `.tmp_…` stage file, a legacy `<oid>.tmp`, a torn write
    /// — was never a referenceable object (a final name only ever appears via
    /// the atomic rename of a *complete* temp), so it is safe to delete.
    fn is_loose_object_name(name: &str) -> bool {
        name.len() == 38 && name.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Remove stray temp artifacts left in the loose-object shards by an
    /// interrupted `put` (the classic case: a write that died on ENOSPC, or a
    /// crash between staging and rename). Idempotent and best-effort: a file we
    /// cannot remove is simply skipped. Returns the count removed. Called on
    /// [`DiskStore::open`] so a daemon self-heals on restart instead of needing
    /// a human to `rm` the artifact.
    fn sweep_temps(git_dir: &Path) -> usize {
        let objects = git_dir.join("objects");
        let mut removed = 0;
        let Ok(shards) = fs::read_dir(&objects) else {
            return 0;
        };
        for shard in shards.flatten() {
            let sname = shard.file_name();
            let sname = sname.to_string_lossy();
            // Only the 2-hex object shards; never `pack/`, `info/`, etc.
            if sname.len() != 2 || !sname.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(files) = fs::read_dir(shard.path()) else {
                continue;
            };
            for f in files.flatten() {
                if !f.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let fname = f.file_name();
                if is_loose_object_name(&fname.to_string_lossy()) {
                    continue;
                }
                if fs::remove_file(f.path()).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

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
            // Self-heal: drop any stale staging artifacts a previous, possibly
            // crashed, run left behind (ENOSPC mid-write, kill between stage
            // and rename). They never carry live data, so removing them on
            // every open keeps the odb clean without operator intervention.
            let swept = sweep_temps(git_dir);
            if swept > 0 {
                tracing::debug!(removed = swept, "swept stale loose-object temp artifacts");
            }
            Ok(DiskStore { git_dir: git_dir.to_path_buf() })
        }

        pub fn git_dir(&self) -> &Path {
            &self.git_dir
        }

        /// Sweep stray loose-object staging temps (see [`sweep_temps`]). Exposed
        /// for `gc` (loose-object hygiene, §9.2) and recovery tooling; `open`
        /// already calls it.
        pub fn sweep_stale_temps(&self) -> usize {
            sweep_temps(&self.git_dir)
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
            let parent = p.parent().expect("loose path always has a shard parent");
            fs::create_dir_all(parent)?;
            // Stage to a unique temp in the same shard dir, then atomically
            // rename: a reader never sees a torn loose object, and a per-write
            // name means concurrent `put`s of the same oid can't clobber each
            // other. On *any* failure (the canonical one is ENOSPC striking
            // mid-`write`, which would otherwise strand a zero-byte temp) we
            // remove the temp before returning, so a failed write never
            // pollutes the odb with a stale artifact.
            let tmp = parent.join(unique_tmp_name());
            if let Err(e) = fs::write(&tmp, obj.compress()) {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
            if let Err(e) = fs::rename(&tmp, &p) {
                let _ = fs::remove_file(&tmp);
                return Err(e.into());
            }
            Ok(oid)
        }
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "full"))]
pub use disk::DiskStore;

#[cfg(all(test, not(target_arch = "wasm32"), feature = "full"))]
mod disk_tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn fresh_store() -> (tempfile::TempDir, DiskStore) {
        let td = tempfile::tempdir().unwrap();
        let git = td.path().join(".context/git");
        let store = DiskStore::init(&git, td.path()).unwrap();
        (td, store)
    }

    fn git_dir(td: &tempfile::TempDir) -> PathBuf {
        td.path().join(".context/git")
    }

    /// Every regular file under any 2-hex shard dir (objects + any stray temp).
    fn shard_entries(git: &Path) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        let objects = git.join("objects");
        for shard in fs::read_dir(&objects).unwrap().flatten() {
            if !shard.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let sname = shard.file_name().to_string_lossy().into_owned();
            if sname.len() != 2 || !sname.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            for f in fs::read_dir(shard.path()).unwrap().flatten() {
                if f.file_type().unwrap().is_file() {
                    out.push((f.file_name().to_string_lossy().into_owned(), f.path()));
                }
            }
        }
        out
    }

    /// Probe whether DAC permission bits are actually enforced for this
    /// process. Under root (the common CI/container case) `CAP_DAC_OVERRIDE`
    /// bypasses a read-only directory, so a perms-based fault injection would
    /// silently *succeed* instead of failing — the caller must skip that path.
    #[cfg(unix)]
    fn perms_enforced() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let td = tempfile::tempdir().unwrap();
        let d = td.path().join("ro");
        fs::create_dir(&d).unwrap();
        fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
        let blocked = fs::write(d.join("probe"), b"x").is_err();
        let _ = fs::set_permissions(&d, fs::Permissions::from_mode(0o755));
        blocked
    }

    fn is_object_name(name: &str) -> bool {
        name.len() == 38 && name.bytes().all(|b| b.is_ascii_hexdigit())
    }

    /// Plant a stray temp file in whatever shard dir exists (or shard `6a` if
    /// the odb is empty) — mimics the artifact an interrupted write strands.
    fn plant_temp(git: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let objects = git.join("objects");
        let shard = fs::read_dir(&objects)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.is_dir()
                    && p.file_name()
                        .map(|n| {
                            let n = n.to_string_lossy();
                            n.len() == 2 && n.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                        .unwrap_or(false)
            })
            .unwrap_or_else(|| objects.join("6a"));
        fs::create_dir_all(&shard).unwrap();
        let p = shard.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn put_get_roundtrip_and_idempotent() {
        let (td, mut store) = fresh_store();
        let blob = GitObject::Blob(b"hello odb".to_vec());
        let oid = store.put(&blob).unwrap();
        // Same content → same oid, second put is a no-op, exactly one file.
        let oid2 = store.put(&blob).unwrap();
        assert_eq!(oid, oid2);
        assert!(store.has(oid));
        match store.get(oid).unwrap() {
            GitObject::Blob(b) => assert_eq!(b, b"hello odb"),
            _ => panic!("wrong kind"),
        }
        let objs: Vec<_> = shard_entries(&git_dir(&td));
        assert_eq!(objs.len(), 1, "idempotent put must not duplicate");
        assert!(is_object_name(&objs[0].0), "live file must be the 38-hex oid");
    }

    /// A normal put leaves the final 38-hex object and **no** temp residue.
    #[test]
    fn successful_put_leaves_no_temp() {
        let (td, mut store) = fresh_store();
        for i in 0..16u8 {
            store.put(&GitObject::Blob(vec![i; 64])).unwrap();
        }
        for (name, _) in shard_entries(&git_dir(&td)) {
            assert!(
                is_object_name(&name),
                "unexpected non-object file in odb after put: {name}"
            );
        }
    }

    /// The customer's misdiagnosis, pinned: a pre-existing stale `.tmp` does
    /// **not** wedge the store — new objects still write and read back fine.
    #[test]
    fn stale_legacy_tmp_does_not_block_writes() {
        let (td, mut store) = fresh_store();
        let seed = store.put(&GitObject::Blob(b"seed".to_vec())).unwrap();
        // Legacy zero-byte artifact: `<38hex>.tmp`, the exact shape the old
        // `with_extension("tmp")` writer stranded on ENOSPC.
        let stale = plant_temp(&git_dir(&td), &format!("{}.tmp", "a".repeat(38)), b"");
        assert!(stale.exists());
        // Writing a brand-new object still succeeds and is readable.
        let oid = store.put(&GitObject::Blob(b"after the stale tmp".to_vec())).unwrap();
        assert!(store.has(oid));
        assert!(store.has(seed));
        assert!(matches!(store.get(oid).unwrap(), GitObject::Blob(b) if b == b"after the stale tmp"));
    }

    /// `open` sweeps both legacy `<oid>.tmp` and new `.tmp_pid_seq` artifacts,
    /// while leaving every real object untouched.
    #[test]
    fn open_sweeps_stale_temps_keeps_objects() {
        let (td, mut store) = fresh_store();
        let live = store.put(&GitObject::Blob(b"keep me".to_vec())).unwrap();
        let git = git_dir(&td);
        let legacy = plant_temp(&git, &format!("{}.tmp", "b".repeat(38)), b"");
        let staged = plant_temp(&git, ".tmp_99999_7", b"partial");
        let zero = plant_temp(&git, ".tmp_1_0", b"");
        assert!(legacy.exists() && staged.exists() && zero.exists());

        // Reopen (the daemon-restart path) → artifacts gone, object intact.
        let reopened = DiskStore::open(&git).unwrap();
        assert!(!legacy.exists(), "legacy <oid>.tmp must be swept");
        assert!(!staged.exists(), "new .tmp_ stage file must be swept");
        assert!(!zero.exists(), "zero-byte stage file must be swept");
        assert!(reopened.has(live), "real object must survive the sweep");
        for (name, _) in shard_entries(&git) {
            assert!(is_object_name(&name), "only objects must remain: {name}");
        }
        // Drop the first handle last so the temp dir outlives both stores.
        drop(store);
    }

    /// `sweep_stale_temps` is idempotent and is a no-op on a clean odb.
    #[test]
    fn sweep_is_idempotent_and_noop_when_clean() {
        let (td, mut store) = fresh_store();
        store.put(&GitObject::Blob(b"x".to_vec())).unwrap();
        assert_eq!(store.sweep_stale_temps(), 0, "clean odb → nothing to sweep");
        plant_temp(&git_dir(&td), ".tmp_7_7", b"junk");
        assert_eq!(store.sweep_stale_temps(), 1, "one artifact swept");
        assert_eq!(store.sweep_stale_temps(), 0, "second sweep is a no-op");
    }

    /// A sub-directory accidentally named like a temp inside a shard must not
    /// be removed (sweep only touches regular files).
    #[test]
    fn sweep_ignores_non_files() {
        let (td, store) = fresh_store();
        let git = git_dir(&td);
        let shard = git.join("objects/6a");
        fs::create_dir_all(shard.join(".tmp_dir_like")).unwrap();
        assert_eq!(store.sweep_stale_temps(), 0);
        assert!(shard.join(".tmp_dir_like").exists());
    }

    /// A staged write that *fails* (the ENOSPC case) must clean up after
    /// itself — no temp artifact stranded — and a later retry once the fault
    /// clears must succeed, so the daemon recovers without manual cleanup.
    /// Uses a read-only shard as a deterministic stand-in for ENOSPC; skipped
    /// where DAC perms aren't enforced (root), since the fault can't be staged.
    #[cfg(unix)]
    #[test]
    fn failed_put_leaves_clean_odb_then_recovers() {
        use std::os::unix::fs::PermissionsExt;
        if !perms_enforced() {
            eprintln!("skip failed_put_leaves_clean_odb_then_recovers: perms not enforced (root)");
            return;
        }
        let (td, mut store) = fresh_store();
        let git = git_dir(&td);
        let blob = GitObject::Blob(b"needs a writable shard".to_vec());

        // Pre-create the shard dir, then make it read-only so the staged
        // `write` fails mid-`put`.
        let shard = {
            let hex = blob.oid().to_hex();
            git.join("objects").join(&hex[..2])
        };
        fs::create_dir_all(&shard).unwrap();
        fs::set_permissions(&shard, fs::Permissions::from_mode(0o555)).unwrap();

        let err = store.put(&blob);
        assert!(err.is_err(), "put into a read-only shard must fail");
        fs::set_permissions(&shard, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            shard_entries(&git).iter().all(|(n, _)| is_object_name(n)),
            "a failed put must not strand a temp artifact"
        );

        // Clear the fault and retry → success, object readable.
        let oid = store.put(&blob).unwrap();
        assert!(store.has(oid));
        assert!(matches!(store.get(oid).unwrap(), GitObject::Blob(b) if b == b"needs a writable shard"));
    }

    /// Root-proof failure path: when the shard slot is occupied by a *file*
    /// (so the directory can't be created), `put` fails cleanly and strands no
    /// temp — independent of DAC enforcement, so it always runs.
    #[test]
    fn failed_put_does_not_strand_temp_when_shard_unusable() {
        let (td, mut store) = fresh_store();
        let git = git_dir(&td);
        let blob = GitObject::Blob(b"shard slot is a file".to_vec());
        let hex = blob.oid().to_hex();
        let shard = git.join("objects").join(&hex[..2]);
        // Occupy the shard path with a regular file: create_dir_all must fail
        // (even for root), so the put errors before staging anything.
        fs::write(&shard, b"not a directory").unwrap();

        assert!(store.put(&blob).is_err(), "put must fail when shard slot is a file");
        // No `.tmp_*` artifact anywhere under objects/.
        assert!(shard_entries(&git).is_empty());

        // Remove the obstruction → put succeeds and round-trips.
        fs::remove_file(&shard).unwrap();
        let oid = store.put(&blob).unwrap();
        assert!(matches!(store.get(oid).unwrap(), GitObject::Blob(b) if b == b"shard slot is a file"));
    }

    /// Stress: many distinct objects, interleaved with planted artifacts, end
    /// with every real object intact and zero temp residue after a sweep.
    #[test]
    fn many_objects_survive_sweep_under_noise() {
        let (td, mut store) = fresh_store();
        let git = git_dir(&td);
        let mut oids = Vec::new();
        for i in 0..200u32 {
            let o = store
                .put(&GitObject::Blob(format!("obj-{i}").into_bytes()))
                .unwrap();
            oids.push(o);
        }
        for i in 0..25 {
            plant_temp(&git, &format!(".tmp_4242_{i}"), b"noise");
        }
        let swept = store.sweep_stale_temps();
        assert_eq!(swept, 25, "exactly the planted artifacts are swept");
        for o in oids {
            assert!(store.has(o), "every real object must survive");
        }
        for (name, _) in shard_entries(&git) {
            assert!(is_object_name(&name));
        }
    }
}
