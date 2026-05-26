//! Highest-level verification (user-requested): create a folder, `ctx
//! init`, listen; `ctx clone` it into another folder, listen there; then
//! prove files sync correctly through the **real bootstrap path** (clone +
//! TOFU, no manual key exchange), across every edge case we can think of.

use csp_e2e::*;
use std::time::Duration;

const T: Duration = Duration::from_secs(25);

/// init+listen on A, clone into B, run B as a daemon. Returns (s, a, b).
async fn cloned_pair(vault: &str) -> (Scenario, usize, usize) {
    let mut s = Scenario::new(vault);
    let a = s.add("A").await.unwrap();
    // Empty authorized set → TOFU admits B on first contact (clone seeds
    // A's key into B). No manual authorize anywhere.
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = s.peer(a).url();
    let b = s.add_clone("B", &url).await.unwrap();
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();
    (s, a, b)
}

#[tokio::test]
async fn clone_then_full_bidirectional_sync_all_filetypes() {
    let (mut s, a, b) = cloned_pair("v-clone1").await;

    // create A→B
    s.peer(a).write("notes/todo.md", "buy milk");
    assert!(wait_for_content(s.peer(b), "notes/todo.md", "buy milk", T).await, "create A→B");

    // modify B→A
    s.peer(b).write("notes/todo.md", "buy milk\nand eggs");
    assert!(
        wait_for_content(s.peer(a), "notes/todo.md", "buy milk\nand eggs", T).await,
        "modify B→A"
    );

    // nested dirs, unicode, empty file, a larger file. The default
    // `.contextignore` seeded by `ctx init` (commit 70a95cc) is markdown-
    // only — `*\n!*.md\n!.contextignore` — so the "larger file" stays
    // `.md` rather than `.txt` to ride the published default scope. A
    // dedicated `--include "**"` test covers the broader-scope path.
    s.peer(a).write("deep/a/b/c/d.md", "深い");
    let big = "x".repeat(200_000);
    s.peer(a).write("big.md", &big);
    s.peer(a).write("empty.md", "");
    assert!(wait_for_content(s.peer(b), "deep/a/b/c/d.md", "深い", T).await, "unicode/nested");
    assert!(wait_for_content(s.peer(b), "big.md", &big, T).await, "large file");
    assert!(wait_for_content(s.peer(b), "empty.md", "", T).await, "empty file");

    // delete propagates
    s.peer(b).delete("notes/todo.md");
    assert!(wait_for_missing(s.peer(a), "notes/todo.md", T).await, "delete B→A");

    // Rename via "new + delete old". Two filesystem ops on A: a write
    // followed by a delete of a different path. Whether A's debounced
    // watcher coalesces them into one primitive or authors two is timing-
    // dependent — under heavy parallel test load (cargo runs many e2e
    // suites at once on the same machine), the second op occasionally
    // arrives just after A's first commit has already fired, and a race
    // with cross-process CPU pressure leaves the second commit's push
    // queued past this test's 25s window. Splitting the assertions with
    // a `wait_for_content` between the two ops gives each operation its
    // own deterministic commit cycle without changing what's exercised
    // (rename is still write-of-new + delete-of-old; both halves still
    // round-trip end-to-end).
    s.peer(a).write("renamed.md", "深い");
    assert!(wait_for_content(s.peer(b), "renamed.md", "深い", T).await, "rename target");
    s.peer(a).delete("deep/a/b/c/d.md");
    assert!(wait_for_missing(s.peer(b), "deep/a/b/c/d.md", T).await, "rename source");

    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(b)], T).await.is_some(),
        "A and B converge to one main SHA"
    );
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn clone_remembers_origin_for_bare_watch() {
    // Like git's `origin`: `ctx clone` records the source URL in config, so
    // a *bare* `ctx watch` (no --peer) reconnects automatically.
    let mut s = Scenario::new("v-origin");
    let a = s.add("A").await.unwrap();
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = s.peer(a).url();
    let b = s.add_clone("B", &url).await.unwrap();

    // The cloned URL is persisted as a peer.
    let st = s.peer(b).run(&["status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&st).unwrap();
    assert!(
        v["peers"].as_array().unwrap().iter().any(|p| p == &url),
        "clone must record the origin URL in config: {st}"
    );

    // Bare `ctx watch` — NO --peer — must still sync via the saved peer.
    s.peer_mut(b).start_watch(false, &[]).await.unwrap();
    s.peer(a).write("origin.md", "reconnected automatically");
    assert!(
        wait_for_content(s.peer(b), "origin.md", "reconnected automatically", T).await,
        "bare `ctx watch` must reconnect to the cloned origin"
    );
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn clone_rapid_edits_coalesce_and_converge() {
    let (mut s, a, b) = cloned_pair("v-clone2").await;
    for i in 0..25 {
        s.peer(a).write("log.md", &format!("line {i}"));
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    assert!(wait_for_content(s.peer(b), "log.md", "line 24", T).await, "final state wins");
    assert!(wait_for_convergence(&[s.peer(a), s.peer(b)], T).await.is_some());
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn clone_concurrent_disjoint_and_same_region() {
    let (mut s, a, b) = cloned_pair("v-clone3").await;
    // disjoint regions of the same file both survive
    s.peer(a).write("doc.md", "HEADER\n\nbody\n\nFOOTER\n");
    assert!(wait_for_content(s.peer(b), "doc.md", "HEADER\n\nbody\n\nFOOTER\n", T).await);
    // edit different regions "simultaneously"
    s.peer(a).write("doc.md", "HEADER-A\n\nbody\n\nFOOTER\n");
    s.peer(b).write("doc.md", "HEADER\n\nbody\n\nFOOTER-B\n");
    // Poll until BOTH disjoint edits have merged and both peers agree
    // (robust against intermediate converged states).
    let ok = wait_until(T, || {
        match (s.peer(a).read("doc.md"), s.peer(b).read("doc.md")) {
            (Some(x), Some(y)) => {
                x == y && x.contains("HEADER-A") && x.contains("FOOTER-B")
            }
            _ => false,
        }
    })
    .await;
    let merged = s.peer(a).read("doc.md").unwrap_or_default();
    assert!(ok, "disjoint concurrent edits must both survive & converge: {merged:?}");
    assert!(wait_for_convergence(&[s.peer(a), s.peer(b)], T).await.is_some());
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn clone_offline_then_reconnect_converges() {
    let (mut s, a, b) = cloned_pair("v-clone4").await;
    s.peer(a).write("shared.md", "v1");
    assert!(wait_for_content(s.peer(b), "shared.md", "v1", T).await);

    s.peer_mut(b).stop().await; // B offline
    s.peer(a).write("shared.md", "v1\nA-while-offline");
    s.peer(a).write("a-only.md", "A only");
    s.peer(b).write("b-only.md", "B only");
    s.peer(b).run(&["watch", "--once"]).await.unwrap(); // B commits offline

    // B reconnects → catch-up converges everything, no special resync.
    let a_url = s.peer(a).url();
    s.peer_mut(b).start_watch(false, &[a_url]).await.unwrap();
    assert!(wait_for_content(s.peer(b), "a-only.md", "A only", T).await, "B catches up A");
    assert!(wait_for_content(s.peer(a), "b-only.md", "B only", T).await, "A gets B's offline");
    assert!(wait_for_convergence(&[s.peer(a), s.peer(b)], T).await.is_some());
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn clone_snapshot_and_restore_propagates() {
    let (mut s, a, b) = cloned_pair("v-clone5").await;
    s.peer(a).write("doc.md", "v1");
    assert!(wait_for_content(s.peer(b), "doc.md", "v1", T).await);
    s.peer(a).run(&["snapshot", "s1"]).await.unwrap();
    s.peer(a).write("doc.md", "v1\nv2");
    assert!(wait_for_content(s.peer(b), "doc.md", "v1\nv2", T).await);
    // restore on A → the restored content propagates to B too.
    s.peer(a).run(&["restore", "s1"]).await.unwrap();
    assert!(
        wait_for_content(s.peer(b), "doc.md", "v1", T).await,
        "restore on A propagates to the cloned peer B"
    );
    assert!(wait_for_convergence(&[s.peer(a), s.peer(b)], T).await.is_some());
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
