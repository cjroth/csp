//! The deterministic fold (§5.3) — the single most important algorithm in
//! CSP. Binary left-fold of the frontier into byte-pinned synthetic fold
//! commits (§5.4). `refs/heads/main` is always the final synthetic fold
//! commit. Determinism rests on the four co-equal hard requirements of §5.4;
//! the §13.2 conformance suite exercises all four.

use crate::error::{CspError, CspResult};
use crate::merge::merge_trees;
use crate::object::{read_tree_to_files, write_tree_from_files};
use crate::object::{CommitObj, GitObject};
use crate::oid::Oid;
use crate::order::{NodeId, OrderKey};
use crate::store::Store;
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
pub fn verify_fold_commit<S: Store>(store: &mut S, oid: Oid) -> CspResult<()> {
    verify_inner(store, oid, &mut HashSet::new())
}

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

#[cfg(test)]
mod tests {
    //! §13.2 conformance: the reference fold must be **order-only
    //! associative** — `refs/heads/main` depends on *nothing* but the DAG
    //! and the strict total order (§5.3/§5.4). These are the four co-equal
    //! §5.4 hard requirements, property-tested over randomized concurrent
    //! DAGs plus hand-built golden cases. 3-way merge is non-associative, so
    //! this is the headline correctness gate, not an optimization check.
    //!
    //! `rand` (a dev-dependency; the engine deliberately ships no
    //! `proptest` — §16 "one engine everywhere") with fixed seeds keeps
    //! every run byte-reproducible.

    use super::*;
    use crate::identity::{build_primitive, Identity};
    use crate::store::MemStore;
    use rand::rngs::StdRng;
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};

    fn seed_id(n: u64) -> Identity {
        let mut s = [0u8; 32];
        s[..8].copy_from_slice(&n.to_le_bytes());
        Identity::from_seed(&s)
    }

    fn map(pairs: &[(&str, &[u8])]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(p, c)| (p.to_string(), c.to_vec()))
            .collect()
    }

    fn put_tree(store: &mut MemStore, files: &BTreeMap<String, Vec<u8>>) -> Oid {
        let mut objs = Vec::new();
        let root = write_tree_from_files(files, &mut |o| {
            objs.push(o.clone());
            Ok(())
        })
        .unwrap();
        for o in &objs {
            store.put(o).unwrap();
        }
        root
    }

    /// The materialized `path -> bytes` of a commit's tree (§5.6).
    fn tree_of(store: &MemStore, commit: Oid) -> BTreeMap<String, Vec<u8>> {
        let t = match store.get(commit).unwrap() {
            GitObject::Commit(c) => c.tree,
            _ => panic!("{commit} is not a commit"),
        };
        read_tree_to_files(t, &|o| store.get(o)).unwrap()
    }

    /// All n! orderings (Heap's algorithm); only ever called for small n.
    fn perms<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
        let mut a = items.to_vec();
        let n = a.len();
        let mut out = vec![a.clone()];
        let mut c = vec![0usize; n];
        let mut i = 0;
        while i < n {
            if c[i] < i {
                if i % 2 == 0 {
                    a.swap(0, i);
                } else {
                    a.swap(c[i], i);
                }
                out.push(a.clone());
                c[i] += 1;
                i = 0;
            } else {
                c[i] = 0;
                i += 1;
            }
        }
        out
    }

    /// An independent store holding byte-identical objects — proves `main`
    /// does not depend on the `MemStore` instance or its insertion history
    /// (§5.4: "identical SHA on every node").
    fn independent_clone(src: &MemStore, roots: &[Oid]) -> MemStore {
        let mut dst = MemStore::new();
        for o in reachable(src, roots).unwrap() {
            dst.put(&src.get(o).unwrap()).unwrap();
        }
        dst
    }

    struct Sim {
        store: MemStore,
        known: Vec<Oid>,
        max_frontier: usize,
        diverged: bool,
        /// Each node's `main` after it has caught up to the full set.
        final_held: Vec<Oid>,
    }

    /// Simulate `nodes` peers concurrently authoring **signed primitives**,
    /// each parented on the synthetic fold commit it currently holds (§5.2),
    /// over partial, randomly-gossiped views (§6) — the real-world source of
    /// forks, deep fold-chains and overlapping concurrent edits. Author
    /// wall-clock is randomized precisely to prove it is *not* a fold input.
    fn simulate(seed: u64) -> Sim {
        const ROUNDS: usize = 16;
        const NODES: usize = 4;
        let files_pool = ["x.txt", "y.txt", "z.txt"];
        // Share lines so 3-way merges hit disjoint-hunk, true-conflict and
        // whole-file paths — not just add/add.
        let content_pool = [
            "alpha\nbeta\ngamma\n",
            "ALPHA\nbeta\ngamma\n",
            "alpha\nbeta\nGAMMA\n",
            "alpha\nBETA\ngamma\n",
            "alpha\nbeta\ngamma\ndelta\n",
        ];

        let mut rng = StdRng::seed_from_u64(seed);
        let mut store = MemStore::new();
        let m0 = genesis(&mut store).unwrap();

        struct N {
            id: Identity,
            counter: u64,
            held: Oid,
            work: BTreeMap<String, Vec<u8>>,
            view: Vec<Oid>,
        }
        let mut nodes: Vec<N> = (0..NODES)
            .map(|i| N {
                id: seed_id(1000 + i as u64),
                counter: 0,
                held: m0,
                work: BTreeMap::new(),
                view: Vec::new(),
            })
            .collect();

        let mut known: Vec<Oid> = Vec::new();
        let mut max_frontier = 0usize;
        let mut diverged = false;

        for _ in 0..ROUNDS {
            let n = rng.gen_range(0..NODES);

            // Receive another peer's view (anti-entropy, §6.2) and refold
            // over the now-larger — but still partial — known set.
            if rng.gen_bool(0.5) {
                let m = rng.gen_range(0..NODES);
                let incoming = nodes[m].view.clone();
                for o in incoming {
                    if !nodes[n].view.contains(&o) {
                        nodes[n].view.push(o);
                    }
                }
                let held = compute_main(&mut store, &nodes[n].view).unwrap();
                nodes[n].held = held;
                nodes[n].work = tree_of(&store, held);
            }

            // A user edit on top of whatever this node currently holds.
            let f = files_pool[rng.gen_range(0..files_pool.len())];
            if rng.gen_bool(0.2) && nodes[n].work.contains_key(f) {
                nodes[n].work.remove(f);
            } else {
                let c = content_pool[rng.gen_range(0..content_pool.len())];
                nodes[n].work.insert(f.to_string(), c.as_bytes().to_vec());
            }

            let tree = put_tree(&mut store, &nodes[n].work);
            nodes[n].counter += 1;
            let wall = rng.gen_range(0..1_000_000u64);
            let prim = build_primitive(
                &nodes[n].id,
                tree,
                nodes[n].held,
                nodes[n].counter,
                wall,
                "edit",
            );
            let oid = store.put(&prim).unwrap();
            known.push(oid);
            nodes[n].view.push(oid);

            let held = compute_main(&mut store, &nodes[n].view).unwrap();
            nodes[n].held = held;
            nodes[n].work = tree_of(&store, held);

            max_frontier = max_frontier.max(frontier(&store, &known).unwrap().len());
            let h0 = nodes[0].held;
            if nodes.iter().any(|x| x.held != h0) {
                diverged = true;
            }
        }

        known.sort();
        known.dedup();

        // Full catch-up: every node now folds the complete set.
        let mut final_held = Vec::new();
        for nd in nodes.iter_mut() {
            nd.held = compute_main(&mut store, &known).unwrap();
            final_held.push(nd.held);
        }

        Sim { store, known, max_frontier, diverged, final_held }
    }

    /// §5.4 part 3 + §5.3 headline: a produced `main`, however it was
    /// reached, must be permutation-invariant and store-independent.
    fn assert_order_only(store: &mut MemStore, known: &[Oid], seed: u64) -> Oid {
        let gold = compute_main(store, known).unwrap();

        if known.len() <= 6 {
            for p in perms(known) {
                assert_eq!(
                    compute_main(store, &p).unwrap(),
                    gold,
                    "fold not permutation-invariant (seed {seed})"
                );
            }
        } else {
            let mut r = StdRng::seed_from_u64(seed ^ 0xF01D);
            let mut p = known.to_vec();
            for _ in 0..48 {
                p.shuffle(&mut r);
                assert_eq!(
                    compute_main(store, &p).unwrap(),
                    gold,
                    "fold not permutation-invariant (seed {seed})"
                );
            }
        }

        // Same primitive set, a clean store with no shared insertion order.
        let mut indep = independent_clone(store, known);
        let mut p = known.to_vec();
        let mut r = StdRng::seed_from_u64(seed ^ 0xBEEF);
        p.shuffle(&mut r);
        assert_eq!(
            compute_main(&mut indep, &p).unwrap(),
            gold,
            "fold depends on the store instance (seed {seed})"
        );
        gold
    }

    /// §5.3 / §5.4 / §13.2 — the load-bearing property: over many random
    /// concurrent DAGs, `main` depends only on the DAG and the strict order.
    #[test]
    fn fold_depends_only_on_dag_and_strict_order() {
        let mut exercised_3way = false;
        for seed in 0..20u64 {
            let mut sim = simulate(seed);
            assert!(
                sim.known.len() >= 8,
                "degenerate generator (seed {seed})"
            );
            if sim.max_frontier >= 2 {
                exercised_3way = true;
            }
            let main = assert_order_only(&mut sim.store, &sim.known, seed);
            // What we converged on is itself a recompute-verifiable fold
            // chain down to primitives / M₀ (§5.2).
            verify_fold_commit(&mut sim.store, main).unwrap();
        }
        assert!(
            exercised_3way,
            "generator never produced a |F|>=2 frontier — the 3-way merge \
             path was never exercised; the property test is vacuous"
        );
    }

    /// §5.2 / §5.4 parts 1 & 2: nodes on partial, differently-ordered views
    /// converge to one identical `main` SHA on catch-up — no per-node "last
    /// converged" state leaks into the per-step merge base.
    #[test]
    fn partial_views_converge_independent_of_delivery_order() {
        let mut any_diverged = false;
        for seed in 0..20u64 {
            let mut sim = simulate(seed);
            any_diverged |= sim.diverged;

            let one_shot = compute_main(&mut sim.store, &sim.known).unwrap();
            for (i, h) in sim.final_held.iter().enumerate() {
                assert_eq!(
                    *h, one_shot,
                    "node {i} did not converge after full catch-up (seed {seed})"
                );
            }

            // Delivering the same set in scrambled order on a fresh store
            // reaches the identical SHA.
            let mut indep = independent_clone(&sim.store, &sim.known);
            let mut order = sim.known.clone();
            let mut r = StdRng::seed_from_u64(seed ^ 0xD00D);
            order.shuffle(&mut r);
            assert_eq!(
                compute_main(&mut indep, &order).unwrap(),
                one_shot,
                "delivery order changed the converged main (seed {seed})"
            );
        }
        assert!(
            any_diverged,
            "no run ever produced concurrent divergence — the convergence \
             test is vacuous"
        );
    }

    /// §5.2 verify + §5.4 part 4: `main` recompute-verifies, recomputing it
    /// is a fixed point, and growing the known set never rewrites a
    /// historical fold commit (content-addressed spine immutability).
    #[test]
    fn main_recompute_verifies_and_history_is_immutable() {
        for seed in 0..14u64 {
            let mut sim = simulate(seed);
            let k = sim.known.len();

            let half = &sim.known[..(k / 2).max(1)];
            let m_half = compute_main(&mut sim.store, half).unwrap();
            verify_fold_commit(&mut sim.store, m_half).unwrap();

            let m_full = compute_main(&mut sim.store, &sim.known).unwrap();
            verify_fold_commit(&mut sim.store, m_full).unwrap();

            assert_eq!(
                compute_main(&mut sim.store, &sim.known).unwrap(),
                m_full,
                "fold is not idempotent (seed {seed})"
            );
            // The earlier (subset) fold commit is still byte-reproducible
            // after the superset's objects were added.
            verify_fold_commit(&mut sim.store, m_half).unwrap();
        }
    }

    /// §5.4 part 3 — explicitly "load-bearing, not an optimization": two
    /// primitives with an *identical* `(counter, NodeId)` are ordered purely
    /// by commit SHA. The fold must stay deterministic AND both disjoint
    /// concurrent adds must survive — the spec's "get it wrong → add/add
    /// LWW, feature gutted" failure mode.
    #[test]
    fn strict_order_breaks_ties_without_add_add_lww() {
        let mut store = MemStore::new();
        let m0 = genesis(&mut store).unwrap();
        let a = seed_id(7);

        let t1 = put_tree(&mut store, &map(&[("f1.txt", b"one\n")]));
        let t2 = put_tree(&mut store, &map(&[("f2.txt", b"two\n")]));
        // Same author, same counter, same parent → equal (counter, NodeId);
        // the OrderKey differs only in the SHA tiebreaker.
        let p1 = store.put(&build_primitive(&a, t1, m0, 1, 10, "edit")).unwrap();
        let p2 = store.put(&build_primitive(&a, t2, m0, 1, 20, "edit")).unwrap();
        assert_ne!(p1, p2);

        let f = frontier(&store, &[p1, p2]).unwrap();
        assert_eq!(f.len(), 2, "both primitives are un-merged tips");
        let lo = if p1.0 < p2.0 { p1 } else { p2 };
        assert_eq!(f[0], lo, "frontier not sorted by the strict total order");

        let m_ab = compute_main(&mut store, &[p1, p2]).unwrap();
        let m_ba = compute_main(&mut store, &[p2, p1]).unwrap();
        assert_eq!(m_ab, m_ba, "tie broken by input order, not by SHA");

        let files = tree_of(&store, m_ab);
        assert_eq!(
            files.get("f1.txt"),
            Some(&b"one\n".to_vec()),
            "disjoint concurrent add dropped — regressed to add/add LWW"
        );
        assert_eq!(
            files.get("f2.txt"),
            Some(&b"two\n".to_vec()),
            "disjoint concurrent add dropped — regressed to add/add LWW"
        );
    }

    /// §5.3 headline, as a readable golden: two peers editing *different
    /// lines* of one file off a shared base — a real 3-way merge — both
    /// survive, no markers, and the result is permutation-invariant.
    #[test]
    fn disjoint_concurrent_line_edits_both_survive() {
        let mut store = MemStore::new();
        let m0 = genesis(&mut store).unwrap();
        let (a, b, c) = (seed_id(1), seed_id(2), seed_id(3));

        let tb = put_tree(&mut store, &map(&[("doc.txt", b"L1\nL2\nL3\n")]));
        let p0 = store.put(&build_primitive(&a, tb, m0, 1, 1, "base")).unwrap();
        let base_main = compute_main(&mut store, &[p0]).unwrap(); // |F|=1 wrapper

        let tbb = put_tree(
            &mut store,
            &map(&[("doc.txt", b"X1\nL2\nL3\n"), ("b.txt", b"B")]),
        );
        let pb = store
            .put(&build_primitive(&b, tbb, base_main, 1, 2, "b"))
            .unwrap();
        let tcc = put_tree(
            &mut store,
            &map(&[("doc.txt", b"L1\nL2\nX3\n"), ("c.txt", b"C")]),
        );
        let pc = store
            .put(&build_primitive(&c, tcc, base_main, 1, 3, "c"))
            .unwrap();

        let known = [p0, pb, pc];
        let main = compute_main(&mut store, &known).unwrap();
        let files = tree_of(&store, main);
        assert_eq!(
            files["doc.txt"],
            b"X1\nL2\nX3\n".to_vec(),
            "disjoint line edits did not both survive — 3-way merge \
             regressed to last-writer-wins"
        );
        assert_eq!(files["b.txt"], b"B".to_vec());
        assert_eq!(files["c.txt"], b"C".to_vec());

        for p in perms(&known) {
            assert_eq!(
                compute_main(&mut store, &p).unwrap(),
                main,
                "result depends on presentation order — non-associative fold"
            );
        }
        verify_fold_commit(&mut store, main).unwrap();
    }
}
