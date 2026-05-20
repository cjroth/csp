//! §10 per-key expiry end-to-end:
//!  - listen-start migration applies default TTL to bare lines
//!  - `expires=never` is preserved (never migrated)
//!  - expired entries are refused at admit time
//!  - an expired peer can re-enroll via a valid auth-key (TTL refresh)
//!  - `ctx auth list` / `ctx auth extend` / `ctx authorize --ttl` shapes

use csp_e2e::*;
use std::time::Duration;

fn read_authorized(p: &Peer) -> String {
    std::fs::read_to_string(p.root().join(".context/authorized_keys")).unwrap_or_default()
}

fn write_authorized(p: &Peer, content: &str) {
    let path = p.root().join(".context/authorized_keys");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

#[tokio::test]
async fn listen_start_migrates_bare_lines_idempotently() {
    let mut s = Scenario::new("v-mig-bare");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-mig-bare"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    // Manually paste a BARE line (no expires=) into A's authorized_keys,
    // simulating the operator footgun the listen-start migration guards.
    write_authorized(s.peer(a), &format!("{b_pub}\n"));
    assert!(!read_authorized(s.peer(a)).contains("expires="));

    // Start listener with a known TTL — migration kicks in.
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--default-key-ttl", "30d"])
        .await
        .unwrap();
    // Give the daemon a moment to perform the migration.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = read_authorized(s.peer(a));
    assert!(after.contains("expires="), "must add expires=; got:\n{after}");
    let before_again = after.clone();
    s.peer_mut(a).stop().await;

    // Restart: idempotent — no further change.
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--default-key-ttl", "30d"])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        before_again,
        read_authorized(s.peer(a)),
        "migration must be idempotent across restarts"
    );

    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn expires_never_is_preserved_across_listen_starts() {
    // Spec §10: `expires=never` is an explicit opt-out and must survive
    // the listen-start migration unchanged.
    let mut s = Scenario::new("v-mig-never");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-mig-never"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    let line = format!("{b_pub} expires=never\n");
    write_authorized(s.peer(a), &line);
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu", "--default-key-ttl", "30d"])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = read_authorized(s.peer(a));
    assert!(
        after.contains("expires=never"),
        "expires=never must survive listen-start; got:\n{after}"
    );
    assert!(
        !after.contains("expires=2"),
        "expires=never must NOT be rewritten to an absolute date; got:\n{after}"
    );

    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn expired_entry_is_refused_at_admit_time() {
    // An entry whose `expires=` is in the past must NOT admit the peer,
    // even though its pubkey is still in the file (left for audit).
    let mut s = Scenario::new("v-exp-refuses");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-exp-refuses"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    // Past expiry: the entry is in the file, but admit will refuse.
    write_authorized(s.peer(a), &format!("{b_pub} expires=2020-01-01\n"));
    s.peer_mut(a)
        .start_watch_with(true, &[], &["--no-tofu"])
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();

    s.peer(a).write("expired.md", "should not reach B");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        s.peer(b).read("expired.md").is_none(),
        "expired entry must refuse admission"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn expired_peer_can_reenroll_via_auth_key() {
    // The expired-then-re-enroll path: peer's pubkey is in authorized_keys
    // but expired. With a valid auth-key, admit refreshes the entry's
    // expires= and proceeds — expired peers come back through the front
    // door, not via TOFU.
    let mut s = Scenario::new("v-exp-refresh");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-exp-refresh"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    write_authorized(s.peer(a), &format!("{b_pub} expires=2020-01-01\n"));
    s.peer_mut(a)
        .start_watch_with(
            true,
            &[],
            &["--no-tofu", "--auth-key", "refresh-key"],
        )
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());

    // B reconnects WITH the auth-key — entry must be refreshed.
    s.peer_mut(b)
        .start_watch_with(
            false,
            &[url.clone()],
            &["--auth-key", "refresh-key"],
        )
        .await
        .unwrap();

    s.peer(a).write("refreshed.md", "back online");
    assert!(
        wait_for_content(s.peer(b), "refreshed.md", "back online", Duration::from_secs(15)).await,
        "expired entry must be refreshed via auth-key"
    );
    let ak = read_authorized(s.peer(a));
    assert!(
        !ak.contains("expires=2020-01-01"),
        "old expires=2020-01-01 must be replaced; got:\n{ak}"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn ctx_authorize_with_ttl_writes_expected_token() {
    // White-box: `ctx authorize <pubkey> --ttl 7d` produces an entry whose
    // expires= is 7 calendar days from today. The --ttl never form
    // produces `expires=never`.
    let mut s = Scenario::new("v-cli-ttl");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-cli-ttl"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    let out = s
        .peer(a)
        .run(&["authorize", &b_pub, "--ttl", "7d"])
        .await
        .unwrap();
    assert!(out.contains("expires="), "stdout summary should reveal expiry: {out}");
    let ak = read_authorized(s.peer(a));
    assert!(ak.contains("expires="), "file got: {ak}");

    // Re-authorize the same pubkey with never — replaces, no duplicate.
    let _ = s
        .peer(a)
        .run(&["authorize", &b_pub, "--ttl", "never"])
        .await
        .unwrap();
    let ak2 = read_authorized(s.peer(a));
    assert!(ak2.contains("expires=never"), "got: {ak2}");
    let count = ak2.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(count, 1, "re-authorize must replace, not duplicate; got:\n{ak2}");
}

#[tokio::test]
async fn ctx_auth_list_and_extend_work() {
    let mut s = Scenario::new("v-cli-list");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-cli-list"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    let _ = s
        .peer(a)
        .run(&["authorize", &b_pub, "--ttl", "5d"])
        .await
        .unwrap();
    let list_json = s.peer(a).run(&["auth", "list", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&list_json).unwrap();
    assert!(v.is_array() && !v.as_array().unwrap().is_empty());
    let first = &v[0];
    assert_eq!(first["status"], "valid");
    let remaining = first["remaining_days"].as_u64().unwrap_or(0);
    assert!(remaining >= 4 && remaining <= 5, "want ~5d, got {remaining}");

    // Extend by 100 days — remaining should jump.
    let head = b_pub.split_whitespace().nth(1).unwrap().to_string();
    let head_prefix: String = head.chars().take(20).collect();
    let _ = s
        .peer(a)
        .run(&["auth", "extend", &head_prefix, "100d"])
        .await
        .unwrap();
    let list_json = s.peer(a).run(&["auth", "list", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&list_json).unwrap();
    let remaining = v[0]["remaining_days"].as_u64().unwrap_or(0);
    assert!(remaining >= 99 && remaining <= 100, "want ~100d, got {remaining}");
}

#[tokio::test]
async fn default_key_ttl_never_disables_migration() {
    // `--default-key-ttl never` (or `0`) opts out of the default TTL —
    // listen-start migration is a no-op for bare lines under that policy.
    let mut s = Scenario::new("v-mig-disabled");
    let a = s.add("A").await.unwrap();
    let b = s.add_uninit("B").unwrap();
    let _ = s.peer(b).run_allow_fail(&["init", "--vault-id", "v-mig-disabled"]).await;
    let b_pub = s.peer(b).pubkey().await.unwrap();

    write_authorized(s.peer(a), &format!("{b_pub}\n"));
    s.peer_mut(a)
        .start_watch_with(
            true,
            &[],
            &["--no-tofu", "--default-key-ttl", "never"],
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let ak = read_authorized(s.peer(a));
    assert!(
        !ak.contains("expires="),
        "ttl never must keep bare lines bare; got:\n{ak}"
    );
    s.peer_mut(a).stop().await;
}
