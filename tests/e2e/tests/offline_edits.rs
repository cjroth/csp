//! §5.6 end-to-end: edits made to the working tree WHILE `ctx watch` is not
//! running must still sync once watch restarts. Correctness here rests on the
//! forced full-content reconcile on the first tick after `open` (not on
//! going-forward inotify events), so these also guard the dirty-gate perf
//! optimization against ever swallowing an offline edit.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn offline_modify_existing_file_syncs_on_plain_watch_restart() {
    let mut s = Scenario::new("v-off-mod");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();

    s.peer(a).write("shared.md", "v1");
    assert!(
        wait_for_content(s.peer(b), "shared.md", "v1", Duration::from_secs(20)).await,
        "initial sync A→B failed"
    );

    // B offline; MODIFY the existing file; restart plain `watch` (no --once).
    s.peer_mut(b).stop().await;
    s.peer(b).write("shared.md", "v2-edited-offline");
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    assert!(
        wait_for_content(s.peer(a), "shared.md", "v2-edited-offline", Duration::from_secs(25))
            .await,
        "offline modify on B did not propagate to A after watch restart"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn offline_delete_syncs_on_plain_watch_restart() {
    let mut s = Scenario::new("v-off-del");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();

    s.peer(a).write("doomed.md", "delete me");
    assert!(wait_for_content(s.peer(b), "doomed.md", "delete me", Duration::from_secs(20)).await);

    s.peer_mut(b).stop().await;
    s.peer(b).delete("doomed.md"); // delete while offline
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    assert!(
        wait_for_missing(s.peer(a), "doomed.md", Duration::from_secs(25)).await,
        "offline delete on B did not propagate to A after restart"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn offline_edit_commits_locally_without_any_peer() {
    // No listener, no peer: a plain `watch` must still COMMIT the offline edit
    // to local history (main advances), so it can push once a peer appears.
    let mut s = Scenario::new("v-off-local");
    let b = s.add("B").await.unwrap();

    s.peer_mut(b).start_watch(false, &[]).await.unwrap();
    s.peer(b).write("local.md", "first");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let sha_before = s.peer(b).main_sha().await.unwrap();
    s.peer_mut(b).stop().await;

    s.peer(b).write("local.md", "edited-offline");
    s.peer_mut(b).start_watch(false, &[]).await.unwrap();

    let mut moved = false;
    for _ in 0..60 {
        if s.peer(b).main_sha().await.unwrap_or_default() != sha_before {
            moved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(moved, "offline edit was not committed locally on watch restart");

    s.peer_mut(b).stop().await;
}
