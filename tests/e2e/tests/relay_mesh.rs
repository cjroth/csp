//! §6.1 end-to-end: a listening full node relays between peers that are not
//! directly connected. A ↔ B(relay) ↔ C — an edit on A reaches C only via
//! B's relay, and all three converge to the identical `main`.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn listener_relays_between_indirect_peers() {
    let mut s = Scenario::new("v-relay");
    let a = s.add("A").await.unwrap();
    let bx = s.add("B").await.unwrap(); // relay
    let c = s.add("C").await.unwrap();
    s.mutual_authorize().await.unwrap();

    s.peer_mut(bx).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(bx).port.unwrap());
    s.peer_mut(a).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer_mut(c).start_watch(false, &[url]).await.unwrap();

    // A → (B relay) → C
    s.peer(a).write("from_a.md", "hello C via B");
    assert!(
        wait_for_content(s.peer(c), "from_a.md", "hello C via B", Duration::from_secs(25)).await,
        "relay A→B→C failed"
    );
    // C → (B relay) → A
    s.peer(c).write("from_c.md", "hello A via B");
    assert!(
        wait_for_content(s.peer(a), "from_c.md", "hello A via B", Duration::from_secs(25)).await,
        "relay C→B→A failed"
    );

    let sha = wait_for_convergence(
        &[s.peer(a), s.peer(bx), s.peer(c)],
        Duration::from_secs(25),
    )
    .await;
    assert!(sha.is_some(), "all three nodes must converge to one main SHA");

    s.peer_mut(a).stop().await;
    s.peer_mut(bx).stop().await;
    s.peer_mut(c).stop().await;
}
