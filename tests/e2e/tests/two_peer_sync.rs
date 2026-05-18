//! §18 end-to-end: create / modify / delete / rename propagate between two
//! real `ctx` processes, both converge to an identical `main` SHA, and the
//! result is genuinely git-coherent with no `.git` at the scope root (§4).

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn create_modify_delete_rename_propagate_and_converge() {
    let mut s = Scenario::new("v-two");
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();

    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    // create on A → B
    s.peer(a).write("note.md", "hello from A");
    assert!(
        wait_for_content(s.peer(b), "note.md", "hello from A", Duration::from_secs(20)).await,
        "create did not propagate A→B"
    );

    // modify on B → A
    s.peer(b).write("note.md", "edited on B");
    assert!(
        wait_for_content(s.peer(a), "note.md", "edited on B", Duration::from_secs(20)).await,
        "modify did not propagate B→A"
    );

    // rename on A (new file + delete old) → B
    s.peer(a).write("renamed.md", "edited on B");
    s.peer(a).delete("note.md");
    assert!(
        wait_for_content(s.peer(b), "renamed.md", "edited on B", Duration::from_secs(20)).await,
        "rename target did not propagate"
    );
    assert!(
        wait_for_missing(s.peer(b), "note.md", Duration::from_secs(20)).await,
        "rename source delete did not propagate"
    );

    let sha = wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(20)).await;
    assert!(sha.is_some(), "peers did not converge to one main SHA");

    // git-coherence (§18): an unmodified `git` reads the engine repo.
    let gd = s.peer(a).root().join(".context/git");
    assert!(
        !s.peer(a).root().join(".git").exists(),
        "there must be NO .git at the scope root (§4)"
    );
    let out = std::process::Command::new("git")
        .arg(format!("--git-dir={}", gd.display()))
        .arg(format!("--work-tree={}", s.peer(a).root().display()))
        .args(["log", "--format=%H", "main"])
        .output()
        .unwrap();
    assert!(out.status.success(), "unmodified git must read main");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(&sha.unwrap()),
        "git log main must list the converged head"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
