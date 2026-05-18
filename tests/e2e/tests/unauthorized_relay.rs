//! §6.1/§10 end-to-end: relays confer no trust. B relays a C-authored
//! primitive to A, but A does not authorize C, so A drops it — content does
//! not gain trust transitively via the relaying peer.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn relay_does_not_launder_unauthorized_author() {
    let mut s = Scenario::new("v-unauth");
    let a = s.add("A").await.unwrap();
    let bx = s.add("B").await.unwrap(); // relay
    let c = s.add("C").await.unwrap();

    let ka = s.peer(a).pubkey().await.unwrap();
    let kb = s.peer(bx).pubkey().await.unwrap();
    let kc = s.peer(c).pubkey().await.unwrap();

    // A trusts only B. B trusts A and C. C trusts B.
    s.peer(a).authorize(&kb).await.unwrap();
    s.peer(bx).authorize(&ka).await.unwrap();
    s.peer(bx).authorize(&kc).await.unwrap();
    s.peer(c).authorize(&kb).await.unwrap();
    // (A deliberately does NOT authorize C.)

    s.peer_mut(bx).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(bx).port.unwrap());
    s.peer_mut(a).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer_mut(c).start_watch(false, &[url]).await.unwrap();

    // B authors a file A should accept (A trusts B).
    s.peer(bx).write("ok.md", "from trusted B");
    assert!(
        wait_for_content(s.peer(a), "ok.md", "from trusted B", Duration::from_secs(20)).await,
        "A should accept B-authored content"
    );

    // C authors a file; B relays it to A; A must DROP it (unauthorized).
    s.peer(c).write("evil.md", "unauthorized C");
    assert!(
        wait_for_content(s.peer(bx), "evil.md", "unauthorized C", Duration::from_secs(20)).await,
        "B (which trusts C) should accept it"
    );
    // Give the relay ample time; A must still not have it.
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        s.peer(a).read("evil.md").is_none(),
        "A must drop a C-authored primitive relayed via B (§6.1/§10)"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(bx).stop().await;
    s.peer_mut(c).stop().await;
}
