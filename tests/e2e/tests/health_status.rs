//! `ctx status` surfaces daemon health: healthy by default, and a degraded
//! marker (what the watch loop writes when commits fail, e.g. a full disk) is
//! both reported by `status` and auto-cleared once a reconcile succeeds.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn status_reports_healthy_by_default() {
    let mut s = Scenario::new("v-health-ok");
    let b = s.add("B").await.unwrap();

    let json = s.peer(b).run(&["status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v["health"]["degraded"],
        serde_json::Value::Bool(false),
        "a fresh vault must report healthy"
    );
}

#[tokio::test]
async fn degraded_marker_is_reported_then_cleared_by_a_successful_reconcile() {
    let mut s = Scenario::new("v-health-degraded");
    let b = s.add("B").await.unwrap();

    // Simulate what the watch loop writes after a failed commit (e.g. ENOSPC).
    let health = s.peer(b).root().join(".context/health.json");
    std::fs::write(
        &health,
        serde_json::to_vec(&serde_json::json!({
            "degraded": true,
            "last_error": "ENOSPC: No space left on device",
            "since_unix": 1_000u64,
            "last_unix": 1_000u64,
            "fail_count": 7u64,
        }))
        .unwrap(),
    )
    .unwrap();

    // `status` reports the degraded state (JSON + human).
    let json = s.peer(b).run(&["status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["health"]["degraded"], serde_json::Value::Bool(true));
    assert_eq!(v["health"]["fail_count"], serde_json::json!(7));
    let human = s.peer(b).run(&["status"]).await.unwrap();
    assert!(human.contains("DEGRADED"), "human status must flag DEGRADED:\n{human}");

    // A running daemon whose reconcile succeeds must clear the marker (the
    // watch loop calls record_success on every Ok tick, including no-op ticks).
    s.peer_mut(b).start_watch(false, &[]).await.unwrap();
    let cleared = wait_until(Duration::from_secs(15), || !health.exists()).await;
    assert!(cleared, "degraded marker must be cleared once a reconcile succeeds");

    let json = s.peer(b).run(&["status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["health"]["degraded"], serde_json::Value::Bool(false));

    s.peer_mut(b).stop().await;
}
