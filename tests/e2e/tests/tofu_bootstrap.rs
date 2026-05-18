//! §10 end-to-end: trust-on-first-use is bounded to the empty-set window.
//! A fresh listener with an empty authorized set TOFU-accepts the first
//! connector; `--no-tofu` refuses it.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn tofu_accepts_first_then_no_tofu_refuses() {
    // ---- TOFU enabled (default): first connector is admitted. ----
    let mut s = Scenario::new("v-tofu");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap(); // clone bootstraps it
    // Deliberately NO authorize on A (empty set → TOFU window open).
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // Clone bootstraps B's vault + trust under TOFU (A records B's key on
    // first contact). It must succeed.
    let (ok, _o, e) = s.peer(b).run_allow_fail(&["clone", &url, "."]).await;
    assert!(ok, "clone under TOFU should succeed: {e}");

    // Now run B as a normal watch daemon and confirm real sync works.
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer(a).write("seed.md", "tofu works");
    assert!(
        wait_for_content(s.peer(b), "seed.md", "tofu works", Duration::from_secs(20)).await,
        "TOFU-admitted peer must sync"
    );
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;

    // ---- TOFU disabled: empty set + --no-tofu refuses the connector. ----
    let mut s2 = Scenario::new("v-notofu");
    let a2 = s2.add("A2").await.unwrap();
    let c2 = s2.add_uninit("C2").unwrap();
    s2.peer_mut(a2)
        .start_watch_with(true, &[], &["--no-tofu"])
        .await
        .unwrap();
    let url2 = format!("ws://127.0.0.1:{}", s2.peer(a2).port.unwrap());

    // clone must NOT establish trust: the connector is refused at the
    // handshake, so the bootstrap cannot catch up any content.
    let (_ok, _o2, _e2) = s2.peer(c2).run_allow_fail(&["clone", &url2, "."]).await;
    s2.peer(a2).write("blocked.md", "should not reach C2");
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        s2.peer(c2).read("blocked.md").is_none(),
        "with --no-tofu and an empty set, the connector must be refused"
    );
    s2.peer_mut(a2).stop().await;
}
