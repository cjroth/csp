//! §6.1/§10 end-to-end: the literal bug scenario — two devices each `ctx
//! clone` from a common relay, then both connect back. Neither cloned device
//! has the other's key in its local `authorized_keys` (clone only seeds the
//! source's key). Under the pre-revision per-author admission model this
//! left the two clones unable to receive each other's edits: pushes reached
//! the relay, but the relay's broadcast back to either clone was dropped at
//! integrate because the author key wasn't locally known. The new model
//! (admission = connection-level; per-primitive signatures = content
//! integrity only, §6.1/§10) is exactly what unblocks this — and this test
//! pins that.
//!
//! Mirrors the user's two-Obsidian-vaults-through-one-Railway report.

use csp_e2e::*;
use std::time::Duration;

const T: Duration = Duration::from_secs(25);

#[tokio::test]
async fn two_clones_converge_through_one_relay() {
    let mut s = Scenario::new("v-two-clones");

    // The relay (R). Listens, doesn't connect anywhere.
    let r = s.add("R").await.unwrap();
    s.peer_mut(r).start_watch(true, &[]).await.unwrap();
    let url = s.peer(r).url();

    // Two devices clone from R. Each clone records R's key in its OWN
    // authorized_keys (via `ctx clone` → `v.authorize(server_ssh)` in
    // ctx/src/main.rs:418). Neither device adds the other's key.
    let a = s.add_clone("A", &url).await.unwrap();
    let b = s.add_clone("B", &url).await.unwrap();

    // R must authorize both devices to admit their connections. With the
    // relay's authorized set empty up to here, TOFU would only admit ONE
    // device (the first connect); explicit authorize is the right shape
    // for "two devices were onboarded".
    let ka = s.peer(a).pubkey().await.unwrap();
    let kb = s.peer(b).pubkey().await.unwrap();
    s.peer(r).authorize(&ka).await.unwrap();
    s.peer(r).authorize(&kb).await.unwrap();

    // Cross-check that A and B did NOT authorize each other (would mask
    // the bug-fix). Inspect each device's status.
    for &dev in &[a, b] {
        let st = s.peer(dev).run(&["status", "--json"]).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&st).unwrap();
        // After `ctx clone` the device knows exactly one key — the relay's.
        assert_eq!(
            v["authorized_keys"].as_u64(),
            Some(1),
            "clone must leave exactly one key (R) in the device's local set"
        );
    }

    s.peer_mut(a).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();

    // The bug: A authors → reaches R correctly → must also reach B.
    s.peer(a).write("from_a.md", "hello from A");
    assert!(
        wait_for_content(s.peer(r), "from_a.md", "hello from A", T).await,
        "R must integrate A's primitive (A is authorized on R)"
    );
    assert!(
        wait_for_content(s.peer(b), "from_a.md", "hello from A", T).await,
        "B must integrate A's primitive relayed via R, even though B does \
         NOT have A's key in its authorized_keys (§6.1/§10)"
    );

    // Symmetric path.
    s.peer(b).write("from_b.md", "hello from B");
    assert!(
        wait_for_content(s.peer(a), "from_b.md", "hello from B", T).await,
        "A must integrate B's primitive relayed via R"
    );

    // And the three converge to one fold.
    let sha = wait_for_convergence(&[s.peer(a), s.peer(r), s.peer(b)], T).await;
    assert!(sha.is_some(), "A, R, B must converge to one main SHA");

    s.peer_mut(a).stop().await;
    s.peer_mut(r).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn two_clones_sharing_one_device_key_converge() {
    // The "same NodeId" variant — closer to the actual user report where
    // both Obsidian vaults on one machine share `~/.context/id_ed25519`.
    // Two vault dirs, one device identity, one relay. Under the new model
    // this also converges (the per-author drop that broke it is gone, and
    // the SHA-tiebreak in the strict total order keeps concurrent authoring
    // under the same NodeId well-defined, §5.1).
    let mut s = Scenario::new("v-two-clones-same-key");

    // Set up R first, with its own key.
    let r = s.add("R").await.unwrap();
    s.peer_mut(r).start_watch(true, &[]).await.unwrap();
    let url = s.peer(r).url();

    // A and B share one device identity (the user's case: one
    // ~/.context/id_ed25519 across two Obsidian vaults). Build them with
    // a shared identity file *before* `ctx clone` would generate one.
    let shared_key = tempfile::TempDir::new().unwrap();
    let key_path = shared_key.path().join("id_ed25519");
    let a = s.add_uninit("A").unwrap();
    let b = s.add_uninit("B").unwrap();
    s.peer_mut(a).set_identity(key_path.clone());
    s.peer_mut(b).set_identity(key_path.clone());

    // Run clone on both, using the same identity.
    s.peer(a).run(&["clone", &url, "."]).await.unwrap();
    s.peer(b).run(&["clone", &url, "."]).await.unwrap();

    let shared_pubkey = s.peer(a).pubkey().await.unwrap();
    assert_eq!(
        shared_pubkey,
        s.peer(b).pubkey().await.unwrap(),
        "A and B must present the same NodeId — that's the scenario"
    );
    s.peer(r).authorize(&shared_pubkey).await.unwrap();

    s.peer_mut(a).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();

    // A and B authoring distinct files — must both reach the other replica.
    s.peer(a).write("a.md", "AAA");
    s.peer(b).write("b.md", "BBB");
    assert!(
        wait_for_content(s.peer(b), "a.md", "AAA", T).await,
        "A's primitive must reach B even though they share a NodeId"
    );
    assert!(
        wait_for_content(s.peer(a), "b.md", "BBB", T).await,
        "B's primitive must reach A even though they share a NodeId"
    );
    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(r), s.peer(b)], T)
            .await
            .is_some(),
        "all three replicas converge to identical main"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(r).stop().await;
    s.peer_mut(b).stop().await;
}
