//! Stock-git-compatible object encoding (§4). Loose object framing is
//! `"<type> <len>\0<payload>"` zlib-deflated; the object id is the SHA-1 of
//! the *uncompressed* framed bytes — byte-identical to stock git, so an
//! unmodified `git` can read what the engine writes (the §18 git-coherence
//! gate). Encoding here is fully deterministic: it is load-bearing for the
//! §5.4 byte-pinned synthetic fold commit invariant.

use crate::error::{CspError, CspResult};
use crate::oid::Oid;
use sha1::{Digest, Sha1};
use std::io::{Read, Write};

pub const FILE_MODE: &str = "100644";
pub const EXEC_MODE: &str = "100755";
pub const SYMLINK_MODE: &str = "120000";
pub const TREE_MODE: &str = "40000";

/// The well-known empty-tree oid. A sanity anchor: our encoder must produce
/// exactly `4b825dc642cb6eb9a060e54bf8d69288fbee4904` for an empty tree.
pub const EMPTY_TREE_HEX: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub name: String,
    pub oid: Oid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitObj {
    pub tree: Oid,
    pub parents: Vec<Oid>,
    pub author: String,
    pub author_email: String,
    pub author_time: u64,
    pub committer: String,
    pub committer_email: String,
    pub committer_time: u64,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitObject {
    Blob(Vec<u8>),
    Tree(Vec<TreeEntry>),
    Commit(CommitObj),
}

impl GitObject {
    pub fn kind(&self) -> &'static str {
        match self {
            GitObject::Blob(_) => "blob",
            GitObject::Tree(_) => "tree",
            GitObject::Commit(_) => "commit",
        }
    }

    /// The uncompressed payload (object body without the `<type> <len>\0`
    /// frame). Deterministic for every object kind.
    pub fn payload(&self) -> Vec<u8> {
        match self {
            GitObject::Blob(b) => b.clone(),
            GitObject::Tree(entries) => {
                let mut sorted = entries.clone();
                sorted.sort_by(|a, b| tree_sort_key(a).cmp(&tree_sort_key(b)));
                let mut out = Vec::new();
                for e in &sorted {
                    out.extend_from_slice(e.mode.as_bytes());
                    out.push(b' ');
                    out.extend_from_slice(e.name.as_bytes());
                    out.push(0);
                    out.extend_from_slice(&e.oid.0);
                }
                out
            }
            GitObject::Commit(c) => {
                let mut s = String::new();
                s.push_str(&format!("tree {}\n", c.tree.to_hex()));
                for p in &c.parents {
                    s.push_str(&format!("parent {}\n", p.to_hex()));
                }
                s.push_str(&format!(
                    "author {} <{}> {} +0000\n",
                    c.author, c.author_email, c.author_time
                ));
                s.push_str(&format!(
                    "committer {} <{}> {} +0000\n",
                    c.committer, c.committer_email, c.committer_time
                ));
                s.push('\n');
                s.push_str(&c.message);
                s.into_bytes()
            }
        }
    }

    /// Framed bytes: `"<type> <len>\0<payload>"`. This is what the oid hashes
    /// and what zlib-deflates into the loose object file.
    pub fn framed(&self) -> Vec<u8> {
        let payload = self.payload();
        let mut out = format!("{} {}\0", self.kind(), payload.len()).into_bytes();
        out.extend_from_slice(&payload);
        out
    }

    pub fn oid(&self) -> Oid {
        let mut h = Sha1::new();
        h.update(self.framed());
        let digest = h.finalize();
        let mut a = [0u8; 20];
        a.copy_from_slice(&digest);
        Oid(a)
    }

    pub fn compress(&self) -> Vec<u8> {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&self.framed()).expect("zlib write");
        enc.finish().expect("zlib finish")
    }

    pub fn decompress_and_parse(compressed: &[u8]) -> CspResult<GitObject> {
        let mut dec = flate2::read::ZlibDecoder::new(compressed);
        let mut framed = Vec::new();
        dec.read_to_end(&mut framed)
            .map_err(|e| CspError::Malformed(format!("zlib inflate: {e}")))?;
        Self::parse_framed(&framed)
    }

    pub fn parse_framed(framed: &[u8]) -> CspResult<GitObject> {
        let nul = framed
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| CspError::Malformed("missing header NUL".into()))?;
        let header = std::str::from_utf8(&framed[..nul])
            .map_err(|_| CspError::Malformed("non-utf8 header".into()))?;
        let (kind, _len) = header
            .split_once(' ')
            .ok_or_else(|| CspError::Malformed("bad header".into()))?;
        let payload = &framed[nul + 1..];
        match kind {
            "blob" => Ok(GitObject::Blob(payload.to_vec())),
            "tree" => Ok(GitObject::Tree(parse_tree(payload)?)),
            "commit" => Ok(GitObject::Commit(parse_commit(payload)?)),
            other => Err(CspError::Malformed(format!("unknown object kind {other}"))),
        }
    }
}

fn tree_sort_key(e: &TreeEntry) -> Vec<u8> {
    // Git orders tree entries by name bytes, treating directory entries as if
    // their name had a trailing '/'. Deterministic and git-identical.
    let mut k = e.name.as_bytes().to_vec();
    if e.mode == TREE_MODE {
        k.push(b'/');
    }
    k
}

fn parse_tree(payload: &[u8]) -> CspResult<Vec<TreeEntry>> {
    let mut entries = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        let sp = payload[i..]
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| CspError::Malformed("tree entry missing space".into()))?
            + i;
        let mode = std::str::from_utf8(&payload[i..sp])
            .map_err(|_| CspError::Malformed("tree mode utf8".into()))?
            .to_string();
        let nul = payload[sp + 1..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| CspError::Malformed("tree entry missing NUL".into()))?
            + sp
            + 1;
        let name = std::str::from_utf8(&payload[sp + 1..nul])
            .map_err(|_| CspError::Malformed("tree name utf8".into()))?
            .to_string();
        let oid_start = nul + 1;
        if oid_start + 20 > payload.len() {
            return Err(CspError::Malformed("tree entry truncated oid".into()));
        }
        let mut a = [0u8; 20];
        a.copy_from_slice(&payload[oid_start..oid_start + 20]);
        entries.push(TreeEntry { mode, name, oid: Oid(a) });
        i = oid_start + 20;
    }
    Ok(entries)
}

fn parse_commit(payload: &[u8]) -> CspResult<CommitObj> {
    let text = String::from_utf8_lossy(payload);
    let mut lines = text.split('\n');
    let mut tree = None;
    let mut parents = Vec::new();
    let mut author = String::new();
    let mut author_email = String::new();
    let mut author_time = 0u64;
    let mut committer = String::new();
    let mut committer_email = String::new();
    let mut committer_time = 0u64;
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("tree ") {
            tree = Some(Oid::from_hex(rest.trim())?);
        } else if let Some(rest) = line.strip_prefix("parent ") {
            parents.push(Oid::from_hex(rest.trim())?);
        } else if let Some(rest) = line.strip_prefix("author ") {
            let (n, e, t) = parse_ident(rest)?;
            author = n;
            author_email = e;
            author_time = t;
        } else if let Some(rest) = line.strip_prefix("committer ") {
            let (n, e, t) = parse_ident(rest)?;
            committer = n;
            committer_email = e;
            committer_time = t;
        }
        // Unknown headers (e.g. gpgsig) are tolerated and ignored.
    }
    let message: String = {
        let collected: Vec<&str> = lines.collect();
        collected.join("\n")
    };
    Ok(CommitObj {
        tree: tree.ok_or_else(|| CspError::Malformed("commit missing tree".into()))?,
        parents,
        author,
        author_email,
        author_time,
        committer,
        committer_email,
        committer_time,
        message,
    })
}

fn parse_ident(s: &str) -> CspResult<(String, String, u64)> {
    let lt = s
        .find('<')
        .ok_or_else(|| CspError::Malformed("ident missing <".into()))?;
    let gt = s
        .find('>')
        .ok_or_else(|| CspError::Malformed("ident missing >".into()))?;
    let name = s[..lt].trim().to_string();
    let email = s[lt + 1..gt].to_string();
    let rest = s[gt + 1..].trim();
    let time = rest
        .split_whitespace()
        .next()
        .and_then(|t| t.parse::<u64>().ok())
        .unwrap_or(0);
    Ok((name, email, time))
}

/// Build a (possibly nested) tree from a flat `path -> bytes` map and write
/// every object into `put`. Returns the root tree oid. Deterministic.
pub fn write_tree_from_files<F>(
    files: &std::collections::BTreeMap<String, Vec<u8>>,
    put: &mut F,
) -> CspResult<Oid>
where
    F: FnMut(&GitObject) -> CspResult<()>,
{
    build_tree("", files, put)
}

fn build_tree<F>(
    prefix: &str,
    files: &std::collections::BTreeMap<String, Vec<u8>>,
    put: &mut F,
) -> CspResult<Oid>
where
    F: FnMut(&GitObject) -> CspResult<()>,
{
    use std::collections::BTreeMap;
    let mut direct: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut subdirs: BTreeMap<String, ()> = BTreeMap::new();
    for path in files.keys() {
        let rel = match path.strip_prefix(prefix) {
            Some(r) => r,
            None => continue,
        };
        if rel.is_empty() {
            continue;
        }
        match rel.find('/') {
            None => {
                direct.insert(rel.to_string(), files[path].clone());
            }
            Some(idx) => {
                subdirs.insert(rel[..idx].to_string(), ());
            }
        }
    }
    let mut entries = Vec::new();
    for (name, content) in &direct {
        let blob = GitObject::Blob(content.clone());
        put(&blob)?;
        entries.push(TreeEntry {
            mode: FILE_MODE.to_string(),
            name: name.clone(),
            oid: blob.oid(),
        });
    }
    for name in subdirs.keys() {
        let child_prefix = format!("{prefix}{name}/");
        let child_oid = build_tree(&child_prefix, files, put)?;
        entries.push(TreeEntry {
            mode: TREE_MODE.to_string(),
            name: name.clone(),
            oid: child_oid,
        });
    }
    let tree = GitObject::Tree(entries);
    put(&tree)?;
    Ok(tree.oid())
}

/// Walk a tree into a flat `path -> bytes` map. Inverse of
/// [`write_tree_from_files`].
pub fn read_tree_to_files<G>(
    root: Oid,
    get: &G,
) -> CspResult<std::collections::BTreeMap<String, Vec<u8>>>
where
    G: Fn(Oid) -> CspResult<GitObject>,
{
    let mut out = std::collections::BTreeMap::new();
    walk_tree("", root, get, &mut out)?;
    Ok(out)
}

fn walk_tree<G>(
    prefix: &str,
    tree_oid: Oid,
    get: &G,
    out: &mut std::collections::BTreeMap<String, Vec<u8>>,
) -> CspResult<()>
where
    G: Fn(Oid) -> CspResult<GitObject>,
{
    let entries = match get(tree_oid)? {
        GitObject::Tree(e) => e,
        _ => return Err(CspError::Malformed(format!("{tree_oid} is not a tree"))),
    };
    for e in entries {
        let path = format!("{prefix}{}", e.name);
        if e.mode == TREE_MODE {
            walk_tree(&format!("{path}/"), e.oid, get, out)?;
        } else {
            match get(e.oid)? {
                GitObject::Blob(b) => {
                    out.insert(path, b);
                }
                _ => return Err(CspError::Malformed(format!("{} is not a blob", e.oid))),
            }
        }
    }
    Ok(())
}
