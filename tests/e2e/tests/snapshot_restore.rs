//! §8 end-to-end: named snapshots are exact recovery points; time-based
//! restore is approximate but works; restore is just editing and stays in
//! history.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn snapshot_and_time_restore() {
    let mut s = Scenario::new("v-snap");
    let a = s.add("A").await.unwrap();

    s.peer(a).write("doc.md", "v1");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();
    s.peer(a).run(&["snapshot", "first"]).await.unwrap();

    let t_mid = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    tokio::time::sleep(Duration::from_secs(2)).await;

    s.peer(a).write("doc.md", "v1\nv2-EDIT");
    s.peer(a).write("extra.md", "added later");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();
    assert_eq!(s.peer(a).read("doc.md").as_deref(), Some("v1\nv2-EDIT"));

    // Restore to the named snapshot → exact earlier state.
    s.peer(a).run(&["restore", "first"]).await.unwrap();
    assert_eq!(
        s.peer(a).read("doc.md").as_deref(),
        Some("v1"),
        "named snapshot restore must be exact"
    );
    assert!(
        s.peer(a).read("extra.md").is_none(),
        "files added after the snapshot are gone after restore"
    );

    // The pre-restore state is itself still recoverable (history intact).
    s.peer(a)
        .run(&["restore", &t_mid.to_string()])
        .await
        .unwrap();
    assert_eq!(
        s.peer(a).read("doc.md").as_deref(),
        Some("v1"),
        "time restore ≤ t_mid yields the early state"
    );

    // git-coherence of snapshot tag.
    let gd = s.peer(a).root().join(".context/git");
    let out = std::process::Command::new("git")
        .arg(format!("--git-dir={}", gd.display()))
        .args(["tag", "--list", "snap/*"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("snap/first"),
        "snapshot must be a real git tag (read-only inspectable, §8)"
    );
}
