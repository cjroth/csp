//! §9.1/§9.2 end-to-end: a stale loose-object staging temp — the artifact an
//! interrupted write (the classic ENOSPC crash) strands in
//! `.context/git/objects/<shard>/` — must NOT wedge a node, and a daemon
//! restart must sweep it automatically. This pins the customer-reported crash:
//! the stale `.tmp` was a symptom, never the thing that blocks new writes, and
//! recovery should need no manual `rm`.

use csp_e2e::*;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

/// Pick an existing 2-hex object shard dir under a peer, or fall back to `6a`.
fn a_shard_dir(root: &std::path::Path) -> PathBuf {
    let objects = root.join(".context/git/objects");
    fs::read_dir(&objects)
        .ok()
        .and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| {
                    p.is_dir()
                        && p.file_name()
                            .map(|n| {
                                let n = n.to_string_lossy();
                                n.len() == 2 && n.bytes().all(|b| b.is_ascii_hexdigit())
                            })
                            .unwrap_or(false)
                })
        })
        .unwrap_or_else(|| {
            let p = objects.join("6a");
            fs::create_dir_all(&p).unwrap();
            p
        })
}

#[tokio::test]
async fn stale_tmp_does_not_wedge_node_and_is_swept_on_restart() {
    let mut s = Scenario::new("v-stale-tmp");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    // A serves; B connects and syncs an initial file so B's odb has real
    // objects (and at least one shard dir to plant into).
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url.clone()]).await.unwrap();

    s.peer(a).write("shared.md", "v1");
    assert!(
        wait_for_content(s.peer(b), "shared.md", "v1", Duration::from_secs(20)).await,
        "initial sync A→B failed"
    );

    // B goes offline; plant the artifacts a crashed write would leave behind.
    s.peer_mut(b).stop().await;
    let shard = a_shard_dir(s.peer(b).root());
    // Legacy zero-byte `<38hex>.tmp` (the old writer's ENOSPC residue)…
    let legacy = shard.join(format!("{}.tmp", "a".repeat(38)));
    fs::write(&legacy, b"").unwrap();
    // …and a new-style partial stage file.
    let staged = shard.join(".tmp_987654_3");
    fs::write(&staged, b"half-written object").unwrap();
    assert!(legacy.exists() && staged.exists());

    // Meanwhile A keeps editing while B is down.
    s.peer(a).write("shared.md", "v1\nv2-from-A");
    s.peer(a).write("a-only.md", "made while B was down");

    // B restarts: open() must sweep the stale temps, and B must still both
    // RECEIVE A's edits and PUBLISH its own (proving the store isn't wedged).
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    assert!(
        wait_for_content(s.peer(b), "a-only.md", "made while B was down", Duration::from_secs(25))
            .await,
        "B did not catch up A's edits after restart — store may be wedged"
    );

    s.peer(b).write("b-only.md", "B can still write");
    assert!(
        wait_for_content(s.peer(a), "b-only.md", "B can still write", Duration::from_secs(25))
            .await,
        "B's own commit never reached A — writes are blocked"
    );

    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(25))
            .await
            .is_some(),
        "peers did not converge after stale-temp recovery"
    );

    // The artifacts are gone — recovery needed no manual cleanup.
    assert!(!legacy.exists(), "legacy <oid>.tmp must be swept on restart");
    assert!(!staged.exists(), "partial .tmp_ stage file must be swept on restart");

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
