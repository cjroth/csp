//! §5.3/§12 end-to-end: disjoint concurrent edits both survive; a
//! same-region conflict resolves deterministically (no markers) with the
//! loser retained in history.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn disjoint_edits_both_survive() {
    let mut s = Scenario::new("v-disj");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    s.peer(a).write("a.md", "alpha");
    s.peer(b).write("b.md", "beta");
    assert!(wait_for_content(s.peer(b), "a.md", "alpha", Duration::from_secs(20)).await);
    assert!(wait_for_content(s.peer(a), "b.md", "beta", Duration::from_secs(20)).await);
    assert!(wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(20))
        .await
        .is_some());
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn same_region_conflict_is_deterministic_loser_in_history() {
    let mut s = Scenario::new("v-conf");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    // A listens (daemon). B edits OFFLINE, commits locally, then connects —
    // a true concurrent same-region edit off the same fold commit.
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    let m0 = s.peer(a).main_sha().await.unwrap();
    s.peer(a).write("x.md", "AAA");
    // Let A's reconcile commit its own primitive before B's arrives.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_ne!(
        s.peer(a).main_sha().await.unwrap(),
        m0,
        "A never committed its offline edit"
    );

    s.peer(b).write("x.md", "BBB");
    s.peer(b).run(&["watch", "--once"]).await.unwrap(); // commit offline
    s.peer(b)
        .run(&["watch", "--once", "--peer", &url])
        .await
        .unwrap(); // now reconcile

    let sha = wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(20)).await;
    assert!(sha.is_some(), "conflict must still converge to one SHA");

    let wa = s.peer(a).read("x.md").unwrap();
    let wb = s.peer(b).read("x.md").unwrap();
    assert_eq!(wa, wb, "both peers materialize the identical winner");
    assert!(wa == "AAA" || wa == "BBB", "exactly one side wins, no markers: {wa}");

    // The loser remains durably in history: `git log --all` lists ≥2
    // primitive ("ctx edit") commits on A.
    let gd = s.peer(a).root().join(".context/git");
    let out = std::process::Command::new("git")
        .arg(format!("--git-dir={}", gd.display()))
        .args(["log", "--all", "--format=%s"])
        .output()
        .unwrap();
    let edits = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("ctx edit"))
        .count();
    assert!(edits >= 2, "loser primitive must remain in history (got {edits})");

    s.peer_mut(a).stop().await;
}
