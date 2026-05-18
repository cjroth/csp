//! §5.1 end-to-end same-NodeId case: two replicas of one vault under the
//! SAME device key author concurrently. Correctness must not depend on the
//! single-writer invariant — the SHA tiebreak keeps a strict total order, so
//! convergence still holds.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn same_nodeid_concurrent_authoring_still_converges() {
    let mut s = Scenario::new("v-samenode");
    let shared_key = tempfile::TempDir::new().unwrap();
    let key_path = shared_key.path().join("id_ed25519");

    let a = s.add_uninit("A").unwrap();
    let b = s.add_uninit("B").unwrap();
    s.peer_mut(a).set_identity(key_path.clone());
    s.peer_mut(b).set_identity(key_path.clone());

    // Generate the shared key once, then init both vaults under it.
    let pubkey = s.peer(a).run(&["key"]).await.unwrap().trim().to_string();
    s.peer(a).run(&["init", "--vault-id", "v-samenode"]).await.unwrap();
    s.peer(b).run(&["init", "--vault-id", "v-samenode"]).await.unwrap();
    // Both authorize the (single, shared) key.
    s.peer(a).authorize(&pubkey).await.unwrap();
    s.peer(b).authorize(&pubkey).await.unwrap();

    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // Concurrent edits to the same path under the same NodeId.
    let m0 = s.peer(a).main_sha().await.unwrap();
    s.peer(a).write("f.md", "one");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_ne!(s.peer(a).main_sha().await.unwrap(), m0);

    s.peer(b).write("f.md", "two");
    s.peer(b).run(&["watch", "--once"]).await.unwrap();
    s.peer(b).run(&["watch", "--once", "--peer", &url]).await.unwrap();

    let sha = wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(20)).await;
    assert!(
        sha.is_some(),
        "same-NodeId concurrency must still converge (SHA tiebreak, §5.1)"
    );
    assert_eq!(s.peer(a).read("f.md"), s.peer(b).read("f.md"));
    s.peer_mut(a).stop().await;
}
