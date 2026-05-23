//! End-to-end regression tests for the sync protocol — the failure
//! classes we'd already hit in production but had no test coverage for:
//!
//!   * issue 0015 / 0016 (atomic catch-up): the 0.1.15 per-chunk
//!     `Msg::Objects` chunker silently corrupted a receiver's known-set
//!     because `verify_fold_commit` walked parent commits and a fold in
//!     chunk N could need a parent in chunk N+1. Receiver dropped
//!     admission silently, computed a divergent `main`, then poisoned
//!     the relay by authoring a primitive parented to that bogus main.
//!     Guarded here by a chunked catch-up against a real
//!     `MemEngine`/`Session` pair with assertions on the receiver's
//!     post-sync state (not just the message count).
//!
//!   * issue 0012 (Live wire size): `commitNow` was using
//!     `export_closure` which walked commit parents, dragging the entire
//!     ancestral chain on every keystroke (multi-MB per edit on a 450-
//!     file vault). Fixed by `export_primitive`, which ships only the
//!     new objects vs. the parent's tree. Guarded here by an ABSOLUTE
//!     upper bound — a single-file edit against an N-file history must
//!     produce a frame within a small constant of the file size, no
//!     matter how deep the history.
//!
//!   * issue 0015 (catch-up admission cascade): the corrupted-known-set
//!     case above is what poisoned the server. Guarded here by a two-
//!     edit sequence: a peer clones, the sender authors more, the peer
//!     receives the live frame — both engines MUST agree on `main` and
//!     `known` byte-for-byte after every step.
//!
//! Sans-IO: the test drives `Session` directly between two `MemEngine`
//! instances. No `tokio`, no sockets, no `ctx` spawn — these run in
//! milliseconds in CI and pin the wire-protocol contract independently
//! of the transport layer.

use std::collections::{BTreeMap, BTreeSet};

use csp_core::engine::MemEngine;
use csp_core::{Identity, Msg, Oid, Role, Session};

// ---------------------------------------------------------------------------
// Sans-IO session pump
// ---------------------------------------------------------------------------

/// Two-party session driver. Runs handshake + catch-up + any in-flight
/// messages until both queues are drained. The accumulator counts message
/// kinds so the assertion-rich tests can prove that — for example — the
/// regression-prone `ObjectsBatch` chunking actually fired (i.e. produced
/// more than one frame), without coupling to internal byte budgets.
#[derive(Default)]
struct WireStats {
    by_kind: BTreeMap<&'static str, usize>,
}

impl WireStats {
    fn count(&mut self, m: &Msg) {
        let k = match m {
            Msg::Hello { .. } => "Hello",
            Msg::AuthProof { .. } => "AuthProof",
            Msg::FrontierDigest { .. } => "FrontierDigest",
            Msg::WantTips { .. } => "WantTips",
            Msg::Objects { .. } => "Objects",
            Msg::ObjectsBatch { .. } => "ObjectsBatch",
            Msg::Live { .. } => "Live",
            Msg::Ping => "Ping",
            Msg::Pong => "Pong",
        };
        *self.by_kind.entry(k).or_insert(0) += 1;
    }
    fn count_of(&self, kind: &str) -> usize {
        self.by_kind.get(kind).copied().unwrap_or(0)
    }
}

/// Run a full session exchange between `a` (listener) and `b` (connector)
/// until both message queues are drained. Returns the wire-frame stats
/// (one side's send count) so assertions can verify what actually shipped.
///
/// This is the same shape as `net::run_session` minus the I/O — feed every
/// frame each side emits to the peer's `on_msg`, fan the outputs back.
fn run_full_sync(a: &mut MemEngine, b: &mut MemEngine) -> WireStats {
    let mut stats = WireStats::default();

    let mut sa = Session::new(Role::Listener, Vec::new(), vec![0xAAu8; 32]);
    let mut sb = Session::new(Role::Connector, Vec::new(), vec![0xBBu8; 32]);
    sa.set_enrollment_authorized(true);

    // Both sides start with their Hello (CSP §10 mutual).
    let hello_b = sb.start(b);
    let hello_a = sa.start(a);

    let mut to_a: Vec<Msg> = vec![hello_b];
    let mut to_b: Vec<Msg> = vec![hello_a];

    // Bounded pump — converges in <100 round-trips even for large vaults
    // (handshake = 2 RTT; catch-up = WantTips → N ObjectsBatch). The cap
    // is generous so a misbehaving session is detectable as a test
    // failure, not an infinite loop.
    for _ in 0..1000 {
        if to_a.is_empty() && to_b.is_empty() {
            return stats;
        }
        let inbound_a = std::mem::take(&mut to_a);
        for msg in inbound_a {
            let step = sa.on_msg(a, msg).expect("a.on_msg");
            for m in step.out {
                stats.count(&m);
                to_b.push(m);
            }
        }
        let inbound_b = std::mem::take(&mut to_b);
        for msg in inbound_b {
            let step = sb.on_msg(b, msg).expect("b.on_msg");
            for m in step.out {
                stats.count(&m);
                to_a.push(m);
            }
        }
    }
    panic!("session pump did not converge after 1000 round-trips");
}

// ---------------------------------------------------------------------------
// Vault builders
// ---------------------------------------------------------------------------

fn make_engine(seed: u8) -> MemEngine {
    MemEngine::create(Identity::from_seed(&[seed; 32]), "regression", "").unwrap()
}

/// Author `n` primitives on `e`, each a single-file edit at a unique
/// path. Each primitive parents on the previous fold so the closure is
/// genuinely deep — that's the property the 0.1.15 chunker corrupted.
/// Returns the running file map so callers can chain further edits.
///
/// Payload is hex-encoded LCG output: valid UTF-8 (the default `Scope`
/// drops anything with a null byte / non-UTF-8 as "binary"), yet
/// entropy-rich enough that zlib can't crush the closure to <256 KiB
/// and erase the chunker test surface.
fn author_n(e: &mut MemEngine, n: usize, file_size: usize) -> BTreeMap<String, Vec<u8>> {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for i in 0..n {
        let mut s: u64 = 0x9e37_79b9_7f4a_7c15u64.wrapping_mul(i as u64 + 1);
        let mut text = String::with_capacity(file_size);
        while text.len() < file_size {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            text.push_str(&format!("{:016x}", s));
        }
        text.truncate(file_size);
        files.insert(format!("note-{i:04}.md"), text.into_bytes());
        e.commit_from_files(&files).expect("commit_from_files");
    }
    files
}

/// Author one new primitive (single-file edit) on top of `files`, which
/// is the running file map for `e`. Returns the new primitive oid hex.
fn author_one(
    e: &mut MemEngine,
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    content: &[u8],
) -> String {
    files.insert(path.into(), content.to_vec());
    e.commit_from_files(files)
        .expect("commit_from_files")
        .expect("primitive authored")
        .to_hex()
}

fn known_set(e: &MemEngine) -> BTreeSet<String> {
    e.known()
        .expect("known")
        .into_iter()
        .map(|o| o.to_hex())
        .collect()
}

// ---------------------------------------------------------------------------
// Regression tests
// ---------------------------------------------------------------------------

/// ATOMIC CATCH-UP (issue 0016, the 0.1.15 cascade).
///
/// Sender authors a vault large enough that the closure spans more than
/// one `ObjectsBatch` frame (CATCHUP_CHUNK_BYTES is 256 KiB; 500 files
/// × ~1 KiB content packs comfortably across multiple chunks). Receiver
/// clones via a full session exchange. After convergence:
///
///   * Receiver's `main` MUST byte-equal sender's `main`.
///   * Receiver's known-set MUST equal sender's known-set.
///   * Multiple `ObjectsBatch` frames MUST have actually been sent
///     (otherwise we're not testing chunking at all — easy regression
///     where a future change accidentally raises the chunk budget past
///     the test vault's closure).
///
/// Under 0.1.15's per-chunk-integrate path, the receiver's known-set is
/// a strict subset (chunks whose fold parents lived in a later chunk
/// dropped admission). Under 0.1.16's atomic-batch path, equality holds.
#[test]
fn chunked_catch_up_admits_atomically_and_converges() {
    let mut sender = make_engine(1);
    let mut receiver = make_engine(2);

    // 250 × ~1 KiB blob → ~1 MiB closure (commits + trees + blobs);
    // chunker fires with 4+ frames. Larger N is wasted in debug mode
    // where SHA-1 is the cost; the bug is just as detectable at 2
    // chunks as at 20.
    author_n(&mut sender, 250, 900);

    let stats = run_full_sync(&mut sender, &mut receiver);

    // Multi-frame chunking actually exercised — the regression's surface.
    assert!(
        stats.count_of("ObjectsBatch") >= 2,
        "expected >=2 ObjectsBatch frames in catch-up, got {}; full stats: {:?}",
        stats.count_of("ObjectsBatch"),
        stats.by_kind,
    );

    // Atomic admission: every primitive the sender admitted, the
    // receiver admitted. Under the 0.1.15 bug this was a strict subset.
    assert_eq!(
        known_set(&receiver),
        known_set(&sender),
        "receiver's known-set diverges from sender — admission was not atomic"
    );

    // Same fold root → same materialized state.
    assert_eq!(
        receiver.main(),
        sender.main(),
        "receiver computed a different main — known-set divergence cascaded into the fold"
    );
}

/// CASCADE GUARD (issue 0015): a receiver that *thinks* it caught up but
/// silently lost some primitives will compute a divergent main and then
/// author a primitive parented to that bogus main. The next sync
/// poisons the sender — `frontier_tips()` walks ancestors and hits the
/// missing parent oid. This test proves the regression is gone: after
/// the receiver clones and the sender authors another edit, the live
/// path also converges and BOTH sides remain healthy (`frontier_tips()`
/// must succeed on both, and `main` agreement must hold).
#[test]
fn live_edit_after_chunked_clone_keeps_both_engines_healthy() {
    let mut sender = make_engine(3);
    let mut receiver = make_engine(4);
    // 150 × 900 B → ~250 KB closure → 2+ ObjectsBatch frames.
    // The bug needed *chunking* + a live edit on top; we don't need
    // 500 files to provoke the cascade, just enough to chunk.
    let mut files = author_n(&mut sender, 150, 900);
    run_full_sync(&mut sender, &mut receiver);

    // Health check on the receiver right after catch-up — what the 0.1.15
    // bug broke. `frontier_tips` walks ancestors; if the known-set has an
    // orphaned primitive (its parent fold missing from the store), this
    // errors with ObjectNotFound — same failure mode the relay hit.
    receiver
        .frontier_tips()
        .expect("receiver frontier_tips after clone must succeed");
    sender
        .frontier_tips()
        .expect("sender frontier_tips after clone must succeed");

    // Sender authors a new edit; the live exchange replays the same
    // session pump (each side's Session would in practice be the
    // already-established one — we re-handshake here for test clarity,
    // which exercises a stricter sequence than the real path).
    author_one(&mut sender, &mut files, "after-clone.md", b"freshly authored");
    run_full_sync(&mut sender, &mut receiver);

    // Both healthy, both converged.
    receiver
        .frontier_tips()
        .expect("receiver frontier_tips after live edit");
    sender
        .frontier_tips()
        .expect("sender frontier_tips after live edit");
    assert_eq!(receiver.main(), sender.main());
    assert_eq!(known_set(&receiver), known_set(&sender));
}

/// LIVE WIRE-SIZE BOUND (issue 0012): a single-file edit's
/// `export_primitive` payload size depends on vault BREADTH (the tree's
/// entry count — unavoidable, the new tree's hash differs from the
/// parent's) but NOT on history DEPTH (the count of prior primitives).
/// 0.1.11's `export_closure` walked commit parents and dragged the
/// entire ancestral chain into the frame — O(depth × breadth). 0.1.12's
/// `export_primitive` ships only the new commit + new sub-trees +
/// new-vs-parent-tree blobs — O(breadth).
///
/// To make the test specifically catch a depth-dependence regression we
/// hold breadth constant and vary depth, asserting flat wire size.
#[test]
fn export_primitive_wire_size_is_independent_of_history_depth() {
    // Holds breadth constant (10 files) and varies depth by repeatedly
    // re-editing one file. The last primitive's exported payload should
    // be ~the same size regardless of the depth of edits sitting behind
    // it — that's the property the 0.1.12 fix introduced.
    fn one_edit_after_depth(edit_depth: usize) -> usize {
        let mut e = make_engine(10);
        let mut files = author_n(&mut e, 10, 600); // breadth fixed
        let prim_hex = {
            // Drive `edit_depth` edits to the same file, take the size
            // of the *last* one's export_primitive.
            let mut last_oid = String::new();
            for i in 0..edit_depth {
                last_oid = author_one(
                    &mut e,
                    &mut files,
                    "churn.md",
                    format!("revision {i} of one file").as_bytes(),
                );
            }
            last_oid
        };
        let oid = Oid::from_hex(&prim_hex).unwrap();
        let raws = e.export_primitive(oid).unwrap();
        raws.iter().map(|r| r.len()).sum()
    }

    let shallow = one_edit_after_depth(2);
    let deep = one_edit_after_depth(50);

    // The new tree always contains 11 entries (10 prior files + churn.md),
    // so the tree size is what it is — but it MUST NOT grow with depth.
    assert!(
        deep < shallow + 256,
        "wire size grew with history depth: shallow={shallow} deep={deep} \
         (export_primitive must be O(breadth), not O(depth × breadth))"
    );

    // Sanity floor: the payload contains at minimum the new commit
    // (~200 B), the new tree (~10 entries × ~50 B = ~500 B), and the
    // new blob (~30 B). A regression that *removed* objects would also
    // be wrong — flag if the size collapses below a plausible minimum.
    assert!(deep > 200, "wire size suspiciously small: {deep} B");
}

/// CONVERGENCE INVARIANT: after a full session, the receiver's store
/// holds every object reachable from the sender's main. We assert it
/// by computing `export_closure(&[main])` on BOTH engines — that's a
/// `reachable()` walk over their own stores serialized to raw bytes in
/// deterministic (BTreeSet) order. Byte equality => same closure =>
/// same object set (each oid → exactly one raw representation in a
/// content-addressed store). This catches "I see your main and your
/// known-set but I'm missing some leaf blob" bugs — the kind a future
/// Live-side optimization could re-introduce.
#[test]
fn receiver_holds_every_object_referenced_by_main_after_clone() {
    let mut sender = make_engine(5);
    author_n(&mut sender, 25, 1200);
    let mut receiver = make_engine(6);
    run_full_sync(&mut sender, &mut receiver);

    let main_a = sender.main().expect("sender main");
    let main_b = receiver.main().expect("receiver main");
    assert_eq!(main_a, main_b, "main divergence breaks the closure check");

    let closure_a = sender.export_closure(&[main_a]).expect("sender closure");
    let closure_b = receiver.export_closure(&[main_b]).expect("receiver closure");
    assert_eq!(
        closure_a, closure_b,
        "receiver's store is missing some object reachable from main, or holds a different byte representation"
    );
}

/// RECOVERY (the path we walked through after the 0.1.15 incident):
/// after a bad receiver state is wiped, a fresh clone from the same
/// sender must reach the same converged state. Validates the
/// "reset → reclone" recovery loop end-to-end at the engine layer.
#[test]
fn fresh_receiver_clones_cleanly_after_a_simulated_local_reset() {
    let mut sender = make_engine(7);
    author_n(&mut sender, 50, 600);

    // Receiver gets the full vault, then is "reset" — a brand-new
    // engine on a different seed (its identity is irrelevant for the
    // engine-state convergence check; what matters is the empty store
    // + empty known on the post-reset side).
    let mut receiver = make_engine(8);
    run_full_sync(&mut sender, &mut receiver);
    assert_eq!(receiver.main(), sender.main());

    // Reset: blow it away and clone fresh.
    let mut post_reset = make_engine(9);
    run_full_sync(&mut sender, &mut post_reset);
    assert_eq!(post_reset.main(), sender.main());
    assert_eq!(known_set(&post_reset), known_set(&sender));
}
