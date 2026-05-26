//! §11 end-to-end: an explicit allowlist minus `.contextignore`; the
//! `.context/` HARD INVARIANT; text-only-by-default (binaries not synced).

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn contextignore_and_binary_and_dotcontext_excluded() {
    let mut s = Scenario::new("v-scope");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    // Synced .contextignore excludes *.secret and the build/ dir.
    s.peer(a).write(".contextignore", "*.secret\nbuild/\n");
    // Issue 0014 / commit 70a95cc: `ctx init` now seeds a markdown-only
    // default `.contextignore` on every vault. Without deleting B's local
    // copy, B's disk has the default while main wants A's version — same
    // bytes as B.materialized so the §5.6 no-clobber check isn't contended,
    // BUT the §5.1 tiebreak between same-counter primitive_a0 and
    // primitive_b0 is decided by NodeId byte order (random). Test result
    // then depends on which random NodeId is bytewise larger. Removing B's
    // seeded copy forces a clean materialize of A's version: B's scan
    // sees nothing to author for `.contextignore`, A's first primitive
    // wins by being the sole writer of that path.
    s.peer(b).delete(".contextignore");

    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    s.peer(a).write("keep.md", "synced");
    s.peer(a).write("creds.secret", "TOP SECRET");
    s.peer(a).write("build/out.o", "artifact");
    // A binary file (NUL byte) is not text → not synced by default (§11).
    std::fs::write(s.peer(a).root().join("blob.bin"), [0u8, 1, 2, 3, 255]).unwrap();

    assert!(
        wait_for_content(s.peer(b), "keep.md", "synced", Duration::from_secs(20)).await,
        "allowlisted text file must sync"
    );
    // The ignore file itself is synced (shared exclusion policy, §11).
    assert!(
        wait_for_content(
            s.peer(b),
            ".contextignore",
            "*.secret\nbuild/\n",
            Duration::from_secs(20)
        )
        .await
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(s.peer(b).read("creds.secret").is_none(), "*.secret excluded");
    assert!(s.peer(b).read("build/out.o").is_none(), "build/ excluded");
    assert!(s.peer(b).read("blob.bin").is_none(), "binary not synced by default");

    // HARD INVARIANT: nothing under .context/ ever crosses the wire.
    assert!(
        s.peer(b).read(".context/state").is_some()
            || std::path::Path::new(&s.peer(b).root().join(".context")).exists(),
        "B has its own local .context (not A's)"
    );
    let a_state = std::fs::read_to_string(s.peer(a).root().join(".context/state")).unwrap();
    let b_state = std::fs::read_to_string(s.peer(b).root().join(".context/state")).unwrap();
    assert_ne!(
        a_state, b_state,
        ".context/ is node-local and must never be replicated (§11)"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
