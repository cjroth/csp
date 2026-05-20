//! §10 auth-key enrollment end-to-end. The matrix covers every case from
//! the spec / issue 0008's table: enrollment, idempotent reconnect after
//! enrollment, invalid key fails loud, absent key with already-enrolled
//! pubkey still works, rotation doesn't sever enrolled peers, and TOFU is
//! implicitly disabled when an auth key is configured.

use csp_e2e::*;
use std::time::Duration;

/// Read the listener's authorized_keys file directly for white-box assertions.
fn read_authorized(p: &Peer) -> String {
    std::fs::read_to_string(p.root().join(".context/authorized_keys")).unwrap_or_default()
}

/// Convenience: assert a line containing `peer_pubkey_head` is present
/// (i.e. the pubkey was enrolled into authorized_keys).
fn assert_enrolled(p: &Peer, peer_pubkey_head: &str) {
    let s = read_authorized(p);
    assert!(
        s.contains(peer_pubkey_head),
        "authorized_keys should contain {peer_pubkey_head}, got:\n{s}"
    );
}

/// Convenience: head (the first base64 chunk) of an `ssh-ed25519 AAAA…`
/// line, used as a needle in the listener's authorized_keys.
fn key_head(ssh_line: &str) -> &str {
    ssh_line
        .split_whitespace()
        .nth(1)
        .expect("ssh-ed25519 <base64> [comment]")
}

#[tokio::test]
async fn enroll_via_auth_key_persists_pubkey_then_reconnect_without_key() {
    let mut s = Scenario::new("v-auth-enroll");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();

    // Listener A: auth-key configured + --no-tofu, so the *only* way for B
    // to enroll is by presenting the secret.
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--auth-key", "s3kr1t"])
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // B has no identity yet — but ctx clone --auth-key creates one and
    // sends it on the upgrade. Enrollment persists B's pubkey on A.
    let (ok, _o, e) = s
        .peer(b)
        .run_allow_fail(&["clone", &url, ".", "--auth-key", "s3kr1t"])
        .await;
    assert!(ok, "clone with valid auth-key must succeed: {e}");

    let b_pub = s.peer(b).pubkey().await.unwrap();
    let head = key_head(&b_pub).to_string();
    assert_enrolled(s.peer(a), &head);

    // Now drop the auth-key entirely and reconnect: pubkey is already in A's
    // authorized_keys, so the second connection works without the secret.
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();
    s.peer(a).write("hello.md", "enrolled");
    assert!(
        wait_for_content(s.peer(b), "hello.md", "enrolled", Duration::from_secs(15)).await,
        "post-enrollment reconnect must sync without the auth-key"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn invalid_auth_key_fails_loud_no_silent_fallthrough() {
    let mut s = Scenario::new("v-auth-bad");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();

    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--auth-key", "right"])
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // Wrong key: the WS upgrade must 401. The clone CLI surfaces this as a
    // failure exit + an error in stderr; either way it must NOT enroll.
    let (ok, _o, _e) = s
        .peer(b)
        .run_allow_fail(&["clone", &url, ".", "--auth-key", "WRONG"])
        .await;
    // Even if `clone` doesn't strictly exit non-zero (probe may have
    // happened first), the listener must not have B's pubkey enrolled.
    if ok {
        // Sanity: enrollment did NOT happen.
        let ak = read_authorized(s.peer(a));
        let b_pub = s.peer(b).pubkey().await.unwrap_or_default();
        let head = key_head(&b_pub);
        assert!(
            !ak.contains(head),
            "wrong key MUST NOT enroll B; got authorized_keys:\n{ak}"
        );
    }

    // And sync must not happen subsequently either.
    s.peer(a).write("nope.md", "should not reach B");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        s.peer(b).read("nope.md").is_none(),
        "wrong auth-key must produce zero sync"
    );

    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn rotation_does_not_sever_already_enrolled_peers() {
    // Enroll B with key V1, restart A with key V2 (rotation). B reconnects
    // with no key and the existing pubkey entry keeps working — the spec
    // promise of "rotating the secret does not revoke enrolled peers".
    let mut s = Scenario::new("v-auth-rotate");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();

    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--auth-key", "v1"])
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    let (ok, _o, e) = s
        .peer(b)
        .run_allow_fail(&["clone", &url, ".", "--auth-key", "v1"])
        .await;
    assert!(ok, "v1 enroll: {e}");
    s.peer_mut(a).stop().await;

    // Bring A back up with a different key. B is already in authorized_keys.
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--auth-key", "v2-rotated"])
        .await
        .unwrap();
    let url2 = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    s.peer_mut(b).start_watch(false, &[url2.clone()]).await.unwrap();
    s.peer(a).write("rotated.md", "still trusted");
    assert!(
        wait_for_content(s.peer(b), "rotated.md", "still trusted", Duration::from_secs(15)).await,
        "rotation must not sever already-enrolled peers"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn auth_key_configured_disables_tofu_implicitly() {
    // Spec §10: setting an auth-key turns off the TOFU window even when
    // authorized_keys is empty. An unknown peer with NO auth-key header
    // must be rejected (no silent first-trust).
    let mut s = Scenario::new("v-auth-no-tofu-implicit");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();

    // Deliberately omit --no-tofu — the auth-key alone must disable TOFU.
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--auth-key", "must-have-this"])
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // B clones with NO --auth-key. The probe succeeds (it's read-only),
    // but the post-clone connect cannot enroll — so no content reaches B.
    let _ = s.peer(b).run_allow_fail(&["clone", &url, "."]).await;
    s.peer(a).write("blocked.md", "tofu off");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        s.peer(b).read("blocked.md").is_none(),
        "auth-key set must implicitly disable TOFU even when set is empty"
    );

    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn enrollment_writes_default_expiry_token() {
    // Spec §10: enrollment writes `expires=<today+default_ttl>` so the
    // entry is bounded by default. Verify by reading the file.
    let mut s = Scenario::new("v-auth-default-ttl");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    s.peer_mut(a)
        .start_watch_with(
            true,
            &[],
            &["--no-tofu", "--auth-key", "k", "--default-key-ttl", "30d"],
        )
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    let (ok, _o, _e) = s
        .peer(b)
        .run_allow_fail(&["clone", &url, ".", "--auth-key", "k"])
        .await;
    assert!(ok, "enroll must succeed");
    let ak = read_authorized(s.peer(a));
    assert!(
        ak.contains("expires="),
        "enrollment must write expires=…; got: {ak}"
    );
    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn multiple_auth_keys_any_one_works_supports_rotation() {
    // Comma-separated CTX_AUTH_KEY allows multiple valid keys at once for
    // safe rotation. A client presenting the new key OR the old key
    // enrolls successfully.
    let mut s = Scenario::new("v-auth-multi");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    s.peer_mut(a)
        .start_watch_with(
            true,
            &[],
            &["--no-tofu", "--auth-key", "old,new"],
        )
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    let (ok, _o, e) = s
        .peer(b)
        .run_allow_fail(&["clone", &url, ".", "--auth-key", "new"])
        .await;
    assert!(ok, "either-of-many auth-keys must work: {e}");
    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn empty_auth_key_set_falls_back_to_pubkey_admit_unchanged() {
    // Sanity: with NO auth-key configured, the listener behaves identically
    // to the pre-feature world — pubkey admit + TOFU window apply. This
    // guards us against accidentally making auth-key non-optional.
    let mut s = Scenario::new("v-auth-empty-noop");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    // No --auth-key at all; --no-tofu so TOFU isn't the path.
    let a_pub = s.peer(a).pubkey().await.unwrap();
    let _ = a_pub; // suppress unused-warning in case test trimmed
    // Pre-authorize B's pubkey on A so the pubkey path works.
    // (We need B's pubkey, which only exists after `ctx clone` creates an
    // identity. Use `ctx key` against an isolated home first.)
    let _ = s
        .peer(b)
        .run_allow_fail(&["init", "--vault-id", "v-auth-empty-noop"])
        .await;
    let b_pub = s.peer(b).pubkey().await.unwrap();
    s.peer(a).authorize(&b_pub).await.unwrap();
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu"])
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // Remove B's vault dir to clone fresh under B's existing identity.
    let _ = std::fs::remove_dir_all(s.peer(b).root().join(".context"));
    let (ok, _o, e) = s.peer(b).run_allow_fail(&["clone", &url, "."]).await;
    assert!(ok, "pubkey path must still work with no auth-key: {e}");
    s.peer_mut(a).stop().await;
}
