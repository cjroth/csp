//! §6.1/§10 end-to-end: under the **connection-level authorization model**,
//! a relay extends trust transitively through admitted connections. A
//! connects to B; C connects to B; B admits both. A primitive authored by C
//! reaches A via B's relay even though A does not locally authorize C —
//! admission was settled at the connection layer, not per-author at
//! integrate time. This is the precise inverse of the pre-revision
//! "relays-confer-no-trust" behavior, and it's what fixes the
//! multi-Obsidian-through-one-Railway bug (issues/0007 and its sibling).

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn relay_extends_trust_to_admitted_writers() {
    let mut s = Scenario::new("v-relay-trust");
    let a = s.add("A").await.unwrap();
    let bx = s.add("B").await.unwrap(); // relay (listener)
    let c = s.add("C").await.unwrap();

    let ka = s.peer(a).pubkey().await.unwrap();
    let kb = s.peer(bx).pubkey().await.unwrap();
    let kc = s.peer(c).pubkey().await.unwrap();

    // B (the relay) admits both A and C — this is the load-bearing gate.
    s.peer(bx).authorize(&ka).await.unwrap();
    s.peer(bx).authorize(&kc).await.unwrap();
    // A and C trust B (so the connector-side pinning is consistent); they
    // deliberately do NOT authorize each other.
    s.peer(a).authorize(&kb).await.unwrap();
    s.peer(c).authorize(&kb).await.unwrap();

    s.peer_mut(bx).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(bx).port.unwrap());
    s.peer_mut(a).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer_mut(c).start_watch(false, &[url]).await.unwrap();

    // Sanity: B-authored content reaches A (the easy case — A trusts B and
    // B authored it). This also covers the live-push path being awake.
    s.peer(bx).write("ok.md", "from trusted B");
    assert!(
        wait_for_content(s.peer(a), "ok.md", "from trusted B", Duration::from_secs(20)).await,
        "A must accept B-authored content"
    );

    // The headline assertion: C authors → B relays to A → A integrates,
    // even though A does NOT have C in its authorized_keys. Trust came
    // through B's admission of C at the connection layer.
    s.peer(c).write("from_c.md", "C via relay");
    assert!(
        wait_for_content(s.peer(bx), "from_c.md", "C via relay", Duration::from_secs(20)).await,
        "B must accept C-authored content (B admitted C)"
    );
    assert!(
        wait_for_content(s.peer(a), "from_c.md", "C via relay", Duration::from_secs(20)).await,
        "A must accept C-authored content relayed via B — connection-level \
         trust is transitive (§6.1/§10)"
    );

    // Symmetry: A → relay → C.
    s.peer(a).write("from_a.md", "A via relay");
    assert!(
        wait_for_content(s.peer(c), "from_a.md", "A via relay", Duration::from_secs(20)).await,
        "C must accept A-authored content relayed via B"
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
