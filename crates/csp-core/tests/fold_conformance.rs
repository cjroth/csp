//! THE HEADLINE GATE (§13.2). Built and property-tested before anything
//! else. N simulated independent nodes; identical primitive sets fed in
//! shuffled / gossip orders MUST yield an identical `main` SHA *and* tree,
//! and every received synthetic fold commit MUST recursively
//! recompute-verify. If this cannot be made deterministic, the architecture
//! does not work (spec §13.2: "Non-negotiable").

use csp_core::object::{read_tree_to_files, write_tree_from_files, GitObject};
use csp_core::{
    build_primitive, compute_main, genesis, reachable, verify_fold_commit, verify_primitive,
    CommitObj, Identity, MemStore, Oid, Store,
};
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use std::collections::BTreeMap;

/// A simulated independent node: its own object store, identity, observed
/// counter, primitive set, and recomputed `main` (§5.1, §5.3).
struct Node {
    store: MemStore,
    id: Identity,
    observed: u64,
    known: Vec<Oid>,
    main: Oid,
}

impl Node {
    fn new(seed: u8) -> Node {
        let mut store = MemStore::new();
        let m0 = genesis(&mut store).unwrap();
        Node {
            store,
            id: Identity::from_seed(&[seed; 32]),
            observed: 0,
            known: Vec::new(),
            main: m0,
        }
    }

    /// Author a primitive whose working tree is `files`, parented on the
    /// fold commit this node currently holds (§5.2). Counter = max observed
    /// + 1 (§5.1). Time is derived from the counter so the object is
    /// byte-identical on every replica (replicated verbatim).
    fn edit(&mut self, files: &BTreeMap<String, Vec<u8>>) -> Oid {
        let mut put = Vec::new();
        let tree = write_tree_from_files(files, &mut |o| {
            put.push(o.clone());
            Ok(())
        })
        .unwrap();
        for o in &put {
            self.store.put(o).unwrap();
        }
        let counter = self.observed + 1;
        self.observed = counter;
        let prim = build_primitive(
            &self.id,
            tree,
            self.main,
            counter,
            1_700_000_000 + counter,
            "edit",
        );
        let oid = self.store.put(&prim).unwrap();
        self.known.push(oid);
        self.recompute();
        oid
    }

    fn recompute(&mut self) {
        self.main = compute_main(&mut self.store, &self.known).unwrap();
    }

    /// Every object reachable from this node's primitives (carries the
    /// parent fold commits → M₀, the complete-DAG invariant §5.2/§6.3).
    fn export(&self) -> Vec<Vec<u8>> {
        self.export_from(&self.known)
    }

    /// The **complete reachable closure** of `tips` (§6.4: catch-up pulls
    /// each tip's reachable closure — never an arbitrary object subset, which
    /// would violate the complete-DAG invariant §5.4(1)). Self-contained:
    /// bottoms out at M₀.
    fn export_from(&self, tips: &[Oid]) -> Vec<Vec<u8>> {
        let objs = reachable(&self.store, tips).unwrap();
        objs.into_iter()
            .map(|o| self.store.get_raw(o).unwrap())
            .collect()
    }

    /// Integrate received raw objects (content-verified on put), admit
    /// signed primitives whose signature verifies, recompute `main`.
    fn receive(&mut self, raws: &[Vec<u8>]) {
        // Object layer first so commits' trees/parents resolve.
        for r in raws {
            let _ = self.store.put_raw(r).unwrap();
        }
        for r in raws {
            if let Ok(GitObject::Commit(c)) = GitObject::decompress_and_parse(r) {
                if let Some((counter, _node)) = csp_core::fold::parse_primitive_meta(&c) {
                    let node_id = verify_primitive(&c).expect("primitive signature verifies");
                    let _ = node_id;
                    self.observed = self.observed.max(counter);
                    let oid = GitObject::Commit(c).oid();
                    if !self.known.contains(&oid) {
                        self.known.push(oid);
                    }
                }
            }
        }
        self.recompute();
    }

    fn main_tree_files(&self) -> BTreeMap<String, Vec<u8>> {
        let tree = match self.store.get(self.main).unwrap() {
            GitObject::Commit(c) => c.tree,
            _ => panic!("main is not a commit"),
        };
        read_tree_to_files(tree, &|o| self.store.get(o)).unwrap()
    }

    /// Recompute-verify every synthetic fold commit reachable from `main`
    /// down to primitives / M₀ (§5.2 / §13.2 assertion (b)).
    fn verify_all(&mut self) {
        let main = self.main;
        verify_fold_commit(&mut self.store, main).expect("main recompute-verifies");
    }
}

fn files(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
        .collect()
}

#[test]
fn empty_set_is_deterministic_genesis() {
    // |F| = 0 → M₀, globally identical SHA (§5.2). Two independent stores.
    let mut a = MemStore::new();
    let mut b = MemStore::new();
    let ga = genesis(&mut a).unwrap();
    let gb = genesis(&mut b).unwrap();
    assert_eq!(ga, gb, "M₀ must be globally constant");
    // Empty-tree anchor: our encoder is byte-identical to stock git (§4/§18).
    let empty = GitObject::Tree(vec![]).oid();
    assert_eq!(empty.to_hex(), csp_core::object::EMPTY_TREE_HEX);
}

#[test]
fn single_primitive_is_wrapper_fold() {
    let mut n = Node::new(1);
    n.edit(&files(&[("a.md", "hello")]));
    n.verify_all();
    assert_eq!(n.main_tree_files(), files(&[("a.md", "hello")]));
}

#[test]
fn disjoint_concurrent_edits_both_survive() {
    // A and B edit different files off M₀ concurrently → real 3-way merge,
    // both survive (§5.3, §12).
    let mut a = Node::new(1);
    let mut b = Node::new(2);
    a.edit(&files(&[("a.md", "from A")]));
    b.edit(&files(&[("b.md", "from B")]));
    let ax = a.export();
    let bx = b.export();
    a.receive(&bx);
    b.receive(&ax);
    assert_eq!(a.main, b.main, "must converge to identical main SHA");
    let want = files(&[("a.md", "from A"), ("b.md", "from B")]);
    assert_eq!(a.main_tree_files(), want);
    assert_eq!(b.main_tree_files(), want);
    a.verify_all();
    b.verify_all();
}

#[test]
fn same_region_conflict_is_deterministic_and_loser_retained() {
    // Concurrent same-region edits: one side deterministically wins by the
    // strict total order; the loser stays in history (§5.3, §12).
    let mut a = Node::new(1);
    let mut b = Node::new(2);
    let pa = a.edit(&files(&[("x.md", "AAA")]));
    let pb = b.edit(&files(&[("x.md", "BBB")]));
    let ax = a.export();
    let bx = b.export();
    a.receive(&bx);
    b.receive(&ax);
    assert_eq!(a.main, b.main, "converge");
    let winner = a.main_tree_files();
    assert!(
        winner == files(&[("x.md", "AAA")]) || winner == files(&[("x.md", "BBB")]),
        "exactly one side wins, no markers: {winner:?}"
    );
    assert_eq!(a.main_tree_files(), b.main_tree_files());
    // Both primitives (winner and loser) remain durably in history.
    assert!(a.store.has(pa) && a.store.has(pb));
    a.verify_all();
}

#[test]
fn offline_then_merge_resolves_unknown_fold_commit() {
    // A authors several edits offline (its primitives parent on fold commits
    // B never computed); B must resolve them from the transmitted objects
    // (§13.2 offline-then-merge case).
    let mut a = Node::new(7);
    let mut b = Node::new(9);
    a.edit(&files(&[("doc.md", "v1")]));
    a.edit(&files(&[("doc.md", "v1\nv2")]));
    a.edit(&files(&[("doc.md", "v1\nv2\nv3"), ("notes.md", "n")]));
    b.edit(&files(&[("other.md", "b-only")]));
    let ax = a.export();
    let bx = b.export();
    b.receive(&ax);
    a.receive(&bx);
    assert_eq!(a.main, b.main);
    let want = files(&[("doc.md", "v1\nv2\nv3"), ("notes.md", "n"), ("other.md", "b-only")]);
    assert_eq!(a.main_tree_files(), want);
    b.verify_all();
    a.verify_all();
}

#[test]
fn same_nodeid_concurrent_authoring_stays_total() {
    // Two replicas of one vault under the SAME key, equal counter, different
    // content → the SHA tiebreak keeps a strict total order; convergence
    // holds (§5.1, §13.2 same-NodeId case).
    let mut r1 = Node::new(5);
    let mut r2 = Node {
        store: {
            let mut s = MemStore::new();
            genesis(&mut s).unwrap();
            s
        },
        id: Identity::from_seed(&[5; 32]), // same NodeId
        observed: 0,
        known: Vec::new(),
        main: GitObject::Commit(CommitObj {
            tree: GitObject::Tree(vec![]).oid(),
            parents: vec![],
            author: "csp".into(),
            author_email: "csp@localhost".into(),
            author_time: 0,
            committer: "csp".into(),
            committer_email: "csp@localhost".into(),
            committer_time: 0,
            message: "csp: genesis\n".into(),
        })
        .oid(),
    };
    r1.edit(&files(&[("f.md", "one")])); // counter 1
    r2.edit(&files(&[("f.md", "two")])); // counter 1, same node
    let x1 = r1.export();
    let x2 = r2.export();
    r1.receive(&x2);
    r2.receive(&x1);
    assert_eq!(r1.main, r2.main, "same-NodeId concurrency still converges");
    r1.verify_all();
    r2.verify_all();
}

#[test]
fn tampered_primitive_signature_rejected() {
    let mut n = Node::new(3);
    let oid = n.edit(&files(&[("a", "ok")]));
    let mut c = match n.store.get(oid).unwrap() {
        GitObject::Commit(c) => c,
        _ => unreachable!(),
    };
    verify_primitive(&c).unwrap();
    c.tree = GitObject::Tree(vec![]).oid(); // tamper
    assert!(verify_primitive(&c).is_err(), "tampered primitive must fail");
}

#[test]
fn tampered_fold_commit_fails_recompute_verify() {
    let mut a = Node::new(1);
    let mut b = Node::new(2);
    a.edit(&files(&[("a.md", "A")]));
    b.edit(&files(&[("b.md", "B")]));
    a.receive(&b.export());
    a.verify_all();
    // Forge a fold commit with a wrong tree but valid-looking parents.
    let real = match a.store.get(a.main).unwrap() {
        GitObject::Commit(c) => c,
        _ => unreachable!(),
    };
    let forged = GitObject::Commit(CommitObj {
        tree: GitObject::Tree(vec![]).oid(),
        ..real.clone()
    });
    let forged_oid = a.store.put(&forged).unwrap();
    assert!(
        verify_fold_commit(&mut a.store, forged_oid).is_err(),
        "a fold commit that does not reproduce must be rejected"
    );
}

/// Order-only associativity (§5.3): the result depends on nothing but the
/// DAG and the strict order. Many independent nodes, the same primitive set
/// delivered in many shuffled / gossip orders → one `main` SHA and tree.
#[test]
fn convergence_under_shuffled_gossip_delivery() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(0xC59_u64);
    for trial in 0..40 {
        // Build a random scenario on a set of authoring nodes.
        let n_authors = rng.gen_range(2..=5);
        let mut authors: Vec<Node> = (0..n_authors).map(|i| Node::new(i as u8 + 1)).collect();
        let n_edits = rng.gen_range(3..=14);
        // Each closure is the complete, self-contained reachable set of one
        // primitive tip (§6.4 delivery unit).
        let mut closures: Vec<Vec<Vec<u8>>> = Vec::new();
        for _ in 0..n_edits {
            let who = rng.gen_range(0..n_authors);
            // Occasionally let an author first absorb random peers' full
            // closures (concurrency with differing parent fold commits).
            if rng.gen_bool(0.5) && !closures.is_empty() {
                let mut idxs: Vec<usize> = (0..closures.len()).collect();
                idxs.shuffle(&mut rng);
                idxs.truncate(rng.gen_range(1..=idxs.len()));
                let mut delivery: Vec<Vec<u8>> = Vec::new();
                for i in idxs {
                    delivery.extend(closures[i].clone());
                }
                delivery.shuffle(&mut rng);
                authors[who].receive(&delivery);
            }
            let fname = format!("f{}.md", rng.gen_range(0..4));
            let content = format!("c{}", rng.gen_range(0..1000));
            let prim = authors[who].edit(&files(&[(&fname, &content)]));
            closures.push(authors[who].export_from(&[prim]));
        }
        let all_objs: Vec<Vec<u8>> = closures.iter().flatten().cloned().collect();
        // Reference: one node that receives every closure.
        let mut reference = Node::new(99);
        reference.receive(&all_objs);
        reference.verify_all();

        // Many fresh nodes, each fed the same complete object set in a
        // different gossip shuffle (order independence, §5.3 / §6.4).
        for s in 0..6 {
            let mut shuffled = all_objs.clone();
            let mut r = rand::rngs::StdRng::seed_from_u64(trial * 100 + s);
            shuffled.shuffle(&mut r);
            let mut node = Node::new(50);
            node.receive(&shuffled);
            node.verify_all();
            assert_eq!(
                node.main, reference.main,
                "trial {trial} shuffle {s}: main SHA diverged"
            );
            assert_eq!(
                node.main_tree_files(),
                reference.main_tree_files(),
                "trial {trial} shuffle {s}: working tree diverged"
            );
        }
    }
}
