//! §6.5/§7 end-to-end: a node works offline-first and converges on
//! reconnect — there is no separate resync path, just catch-up again.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn offline_edits_converge_on_reconnect() {
    let mut s = Scenario::new("v-off");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // B connects, syncs an initial file, then goes offline.
    s.peer(a).write("shared.md", "v1");
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();
    assert!(wait_for_content(s.peer(b), "shared.md", "v1", Duration::from_secs(20)).await);
    s.peer_mut(b).stop().await; // B offline

    // A keeps editing while B is down.
    s.peer(a).write("shared.md", "v1\nv2-from-A");
    s.peer(a).write("a-only.md", "only on A");
    tokio::time::sleep(Duration::from_secs(2)).await;

    // B edits offline too (one-shot local commit, no peer).
    s.peer(b).write("b-only.md", "only on B");
    s.peer(b).run(&["watch", "--once"]).await.unwrap();

    // B reconnects → catch-up converges everything, no special resync.
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();
    assert!(
        wait_for_content(s.peer(b), "a-only.md", "only on A", Duration::from_secs(25)).await,
        "B did not catch up A's offline edits on reconnect"
    );
    assert!(
        wait_for_content(s.peer(a), "b-only.md", "only on B", Duration::from_secs(25)).await,
        "A did not receive B's offline edit on reconnect"
    );
    assert!(wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(25))
        .await
        .is_some());

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
