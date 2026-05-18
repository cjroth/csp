//! The deterministic fold (§5.3) — the single most important algorithm in
//! CSP. Binary left-fold of the frontier into byte-pinned synthetic fold
//! commits (§5.4). `refs/heads/main` is always the final synthetic fold
//! commit. Determinism rests on the four co-equal hard requirements of §5.4;
//! the §13.2 conformance suite exercises all four.

use crate::error::{CspError, CspResult};
#[cfg(feature = "full")]
use crate::merge::merge_trees;
#[cfg(feature = "full")]
use crate::object::{read_tree_to_files, write_tree_from_files};
use crate::object::{CommitObj, GitObject};
use crate::oid::Oid;
use crate::order::{NodeId, OrderKey};
use crate::store::Store;
#[cfg(feature = "full")]
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Fixed constant identity for every synthetic fold commit (§5.4) — never
/// the local node. Any leak of node-local state re-diverges every node.
pub const FOLD_NAME: &str = "csp";
pub const FOLD_EMAIL: &str = "csp@localhost";
/// Fixed message bytes (§5.4 "message & encoding — fixed template").
pub const GENESIS_MSG: &str = "csp: genesis\n";
pub const FOLD_MSG: &str = "csp: fold\n";

/// Parse the `(counter, NodeId)` trailer (§5.1/§5.2) out of a commit message.
/// A commit is a **primitive** iff it carries a `CSP-Node` trailer; anything
/// else (genesis, fold) is **synthetic**.
pub fn parse_primitive_meta(c: &CommitObj) -> Option<(u64, NodeId)> {
    let mut counter = None;
    let mut node = None;
    for line in c.message.lines() {
        if let Some(v) = line.strip_prefix("CSP-Counter: ") {
            counter = v.trim().parse::<u64>().ok();
        } else if let Some(v) = line.strip_prefix("CSP-Node: ") {
            node = NodeId::from_hex(v.trim());
        }
    }
    Some((counter?, node?))
}

pub fn is_primitive(c: &CommitObj) -> bool {
    parse_primitive_meta(c).is_some()
}

fn load_commit<S: Store>(store: &S, oid: Oid) -> CspResult<CommitObj> {
    match store.get(oid)? {
        GitObject::Commit(c) => Ok(c),
        _ => Err(CspError::Malformed(format!("{oid} is not a commit"))),
    }
}

/// Transitive ancestors of `start` (excluding `start`) over the real commit
/// DAG. Bottoms out at fold commits / `M₀` — which must be present (the
/// complete-DAG invariant §5.4 part 1); a dangling parent is an error.
fn ancestors<S: Store>(store: &S, start: Oid) -> CspResult<HashSet<Oid>> {
    let mut seen = HashSet::new();
    let mut stack = vec![start];
    let mut first = true;
    while let Some(o) = stack.pop() {
        if !first && !seen.insert(o) {
            continue;
        }
        let c = load_commit(store, o)?;
        if first {
            first = false;
        }
        for p in c.parents {
            if !seen.contains(&p) {
                stack.push(p);
            }
        }
    }
    Ok(seen)
}

fn order_key<S: Store>(store: &S, oid: Oid) -> CspResult<OrderKey> {
    let c = load_commit(store, oid)?;
    let (counter, node) = parse_primitive_meta(&c)
        .ok_or_else(|| CspError::FoldVerify(format!("{oid} is not a primitive")))?;
    Ok(OrderKey { counter, node, sha: oid })
}

/// Frontier (§5.3): the known primitives that are not an ancestor of any
/// other known primitive — computed over the *complete* DAG.
pub fn frontier<S: Store>(store: &S, known: &[Oid]) -> CspResult<Vec<Oid>> {
    let prims: Vec<Oid> = known
        .iter()
        .copied()
        .filter(|&o| {
            load_commit(store, o)
                .map(|c| is_primitive(&c))
                .unwrap_or(false)
        })
        .collect();
    let mut dominated: HashSet<Oid> = HashSet::new();
    let anc_cache: HashMap<Oid, HashSet<Oid>> = {
        let mut m = HashMap::new();
        for &q in &prims {
            m.insert(q, ancestors(store, q)?);
        }
        m
    };
    for &q in &prims {
        let aq = &anc_cache[&q];
        for &p in &prims {
            if p != q && aq.contains(&p) {
                dominated.insert(p);
            }
        }
    }
    let mut f: Vec<Oid> = prims.into_iter().filter(|o| !dominated.contains(o)).collect();
    // Sort by the strict total order (§5.1).
    let mut keyed: Vec<(OrderKey, Oid)> = Vec::new();
    for o in f.drain(..) {
        keyed.push((order_key(store, o)?, o));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, o)| o).collect())
}

/// Deterministic merge-base over the real DAG: the maximal common ancestor,
/// tie-broken by (committer-time desc, oid asc). Both operands are real
/// commits whose ancestry bottoms out at fold commits / `M₀` (§5.3) — never
/// per-node "last converged" state (§5.4 part 2). Merge-only → full profile.
#[cfg(feature = "full")]
fn merge_base<S: Store>(store: &S, a: Oid, b: Oid) -> CspResult<Oid> {
    let mut anc_a = ancestors(store, a)?;
    anc_a.insert(a);
    let mut anc_b = ancestors(store, b)?;
    anc_b.insert(b);
    let common: Vec<Oid> = anc_a.intersection(&anc_b).copied().collect();
    if common.is_empty() {
        return Err(CspError::FoldVerify(format!("no merge base for {a} and {b}")));
    }
    // Keep only maximal: drop any common node that is an ancestor of another
    // common node.
    let mut anc_of: HashMap<Oid, HashSet<Oid>> = HashMap::new();
    for &c in &common {
        anc_of.insert(c, ancestors(store, c)?);
    }
    let mut maximal: Vec<Oid> = Vec::new();
    for &c in &common {
        let mut is_anc_of_other = false;
        for &d in &common {
            if c != d && anc_of[&d].contains(&c) {
                is_anc_of_other = true;
                break;
            }
        }
        if !is_anc_of_other {
            maximal.push(c);
        }
    }
    let mut scored: Vec<(u64, Oid)> = Vec::new();
    for m in maximal {
        let c = load_commit(store, m)?;
        scored.push((c.committer_time, m));
    }
    scored.sort_by(|x, y| y.0.cmp(&x.0).then(x.1 .0.cmp(&y.1 .0)));
    Ok(scored[0].1)
}

fn commit_tree<S: Store>(store: &S, oid: Oid) -> CspResult<Oid> {
    Ok(load_commit(store, oid)?.tree)
}

#[cfg(feature = "full")]
fn tree_files<S: Store>(store: &S, tree: Oid) -> CspResult<BTreeMap<String, Vec<u8>>> {
    read_tree_to_files(tree, &|o| store.get(o))
}

/// The byte-pinned synthetic fold commit object (§5.4). `author`/`committer`
/// = fixed constant identity; time = max(committer time of all parents);
/// message = fixed bytes; unsigned. Fully reproducible by any node.
fn make_fold_commit<S: Store>(
    store: &mut S,
    tree: Oid,
    parents: Vec<Oid>,
    msg: &str,
) -> CspResult<Oid> {
    let mut t = 0u64;
    for &p in &parents {
        t = t.max(load_commit(store, p)?.committer_time);
    }
    let c = GitObject::Commit(CommitObj {
        tree,
        parents,
        author: FOLD_NAME.into(),
        author_email: FOLD_EMAIL.into(),
        author_time: t,
        committer: FOLD_NAME.into(),
        committer_email: FOLD_EMAIL.into(),
        committer_time: t,
        message: msg.into(),
    });
    store.put(&c)
}

/// The deterministic genesis `M₀` (§5.2): a root synthetic fold commit over
/// the empty tree, no parents, fixed identity, time 0. Globally identical
/// SHA on every node.
pub fn genesis<S: Store>(store: &mut S) -> CspResult<Oid> {
    let empty_tree = GitObject::Tree(vec![]);
    store.put(&empty_tree)?;
    let c = GitObject::Commit(CommitObj {
        tree: empty_tree.oid(),
        parents: vec![],
        author: FOLD_NAME.into(),
        author_email: FOLD_EMAIL.into(),
        author_time: 0,
        committer: FOLD_NAME.into(),
        committer_email: FOLD_EMAIL.into(),
        committer_time: 0,
        message: GENESIS_MSG.into(),
    });
    store.put(&c)
}

/// The trivial `|F|=1` self-wrapper synthetic fold commit (§5.3 / §7): tree
/// = the tip's tree, parent = `[tip]`. **Not a 3-way merge** (no
/// merge-base, no diff3, no merge engine) — deterministic and **thin-node /
/// wasm safe**. This is the only fold a thin node ever computes between
/// reconnects, keeping its offline run linear (O(1) frontier contribution).
pub fn thin_wrapper<S: Store>(store: &mut S, tip: Oid) -> CspResult<Oid> {
    let tree = commit_tree(store, tip)?;
    make_fold_commit(store, tree, vec![tip], FOLD_MSG)
}

/// Compute `refs/heads/main`: the final synthetic fold commit of the §5.3
/// binary left-fold over the frontier of `known` primitives. Every
/// intermediate `accₖ` is persisted into `store` as a real verifiable
/// object. Deterministic given the same primitive set (§5.4). The multi-tip
/// 3-way merge is the full-node engine (§4/§7) — compiled out of wasm/thin.
#[cfg(feature = "full")]
pub fn compute_main<S: Store>(store: &mut S, known: &[Oid]) -> CspResult<Oid> {
    let f = frontier(store, known)?;
    if f.is_empty() {
        return genesis(store); // |F| = 0 → M₀
    }
    if f.len() == 1 {
        // |F| = 1 → the trivial self-wrapper (§5.3, §7) — same code path a
        // thin node runs; not a 3-way merge.
        return thin_wrapper(store, f[0]);
    }
    // acc₀ = the lowest-ordered tip (a real commit).
    let mut acc = f[0];
    for &tk in &f[1..] {
        let base = merge_base(store, acc, tk)?;
        let base_tree = tree_files(store, commit_tree(store, base)?)?;
        let ours_tree = tree_files(store, commit_tree(store, acc)?)?;
        let theirs_tree = tree_files(store, commit_tree(store, tk)?)?;
        let merged = merge_trees(&base_tree, &ours_tree, &theirs_tree);
        let merged_tree = {
            let mut to_put: Vec<GitObject> = Vec::new();
            let root = write_tree_from_files(&merged, &mut |obj| {
                to_put.push(obj.clone());
                Ok(())
            })?;
            for o in &to_put {
                store.put(o)?;
            }
            root
        };
        // accₖ = synthetic fold commit: that tree, parents [accₖ₋₁, Tₖ].
        acc = make_fold_commit(store, merged_tree, vec![acc, tk], FOLD_MSG)?;
    }
    Ok(acc)
}

/// Recompute-verify a received synthetic fold commit (§5.2): recompute the
/// fold over *its own* parent list and assert the SHA matches, recursively
/// down to primitives / `M₀`. §5.4 determinism makes this exact. Returns
/// `Ok(())` for primitives and `M₀` (verified by signature / fixed identity
/// elsewhere); errors if any fold commit fails to reproduce byte-identically.
/// Recompute-verification re-runs the merge → full profile (§4/§7); a thin
/// node never recomputes folds (§7).
#[cfg(feature = "full")]
pub fn verify_fold_commit<S: Store>(store: &mut S, oid: Oid) -> CspResult<()> {
    verify_inner(store, oid, &mut HashSet::new())
}

#[cfg(feature = "full")]
fn verify_inner<S: Store>(
    store: &mut S,
    oid: Oid,
    seen: &mut HashSet<Oid>,
) -> CspResult<()> {
    if !seen.insert(oid) {
        return Ok(());
    }
    let c = load_commit(store, oid)?;
    if is_primitive(&c) {
        return Ok(());
    }
    if c.parents.is_empty() {
        // M₀ — verify it reproduces the fixed genesis identity.
        let mut probe = crate::store::MemStore::new();
        let g = genesis(&mut probe)?;
        if g != oid {
            return Err(CspError::FoldVerify(format!(
                "genesis mismatch: expected {g}, got {oid}"
            )));
        }
        return Ok(());
    }
    for &p in &c.parents {
        verify_inner(store, p, seen)?;
    }
    // Reproduce this fold commit from its own parent list.
    let recomputed = if c.parents.len() == 1 {
        let tip = c.parents[0];
        let tree = commit_tree(store, tip)?;
        make_fold_commit(store, tree, vec![tip], FOLD_MSG)?
    } else if c.parents.len() == 2 {
        let acc = c.parents[0];
        let tk = c.parents[1];
        let base = merge_base(store, acc, tk)?;
        let base_tree = tree_files(store, commit_tree(store, base)?)?;
        let ours_tree = tree_files(store, commit_tree(store, acc)?)?;
        let theirs_tree = tree_files(store, commit_tree(store, tk)?)?;
        let merged = merge_trees(&base_tree, &ours_tree, &theirs_tree);
        let mut to_put: Vec<GitObject> = Vec::new();
        let root = write_tree_from_files(&merged, &mut |obj| {
            to_put.push(obj.clone());
            Ok(())
        })?;
        for o in &to_put {
            store.put(o)?;
        }
        make_fold_commit(store, root, vec![acc, tk], FOLD_MSG)?
    } else {
        return Err(CspError::FoldVerify(format!(
            "fold commit {oid} has {} parents (expected 1 or 2)",
            c.parents.len()
        )));
    };
    if recomputed != oid {
        return Err(CspError::FoldVerify(format!(
            "fold commit {oid} does not reproduce (got {recomputed})"
        )));
    }
    Ok(())
}

/// All objects reachable from `roots` (commits → trees → blobs). Used by
/// replication (§6.3) and tests.
pub fn reachable<S: Store>(store: &S, roots: &[Oid]) -> CspResult<BTreeSet<Oid>> {
    let mut out = BTreeSet::new();
    let mut stack: Vec<Oid> = roots.to_vec();
    while let Some(o) = stack.pop() {
        if !out.insert(o) {
            continue;
        }
        match store.get(o)? {
            GitObject::Commit(c) => {
                stack.push(c.tree);
                for p in c.parents {
                    stack.push(p);
                }
            }
            GitObject::Tree(entries) => {
                for e in entries {
                    stack.push(e.oid);
                }
            }
            GitObject::Blob(_) => {}
        }
    }
    Ok(out)
}
