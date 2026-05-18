//! §4/§18 end-to-end git-coherence: the engine-owned repo lives at
//! `<scope>/.context/git` with a decoupled worktree, there is NO `.git` at
//! the scope root, an unmodified `git` can log/checkout/cat-file it, and the
//! `ctx git` read-only guard is real (data-loss-critical, §13.2).

use csp_e2e::*;

fn git(gd: &std::path::Path, wt: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = std::process::Command::new("git")
        .arg(format!("--git-dir={}", gd.display()))
        .arg(format!("--work-tree={}", wt.display()))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

#[tokio::test]
async fn engine_repo_is_genuine_stock_git() {
    let mut s = Scenario::new("v-gitcoh");
    let a = s.add("A").await.unwrap();
    s.peer(a).write("a/b/deep.md", "deep content\n");
    s.peer(a).write("top.md", "top\n");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();

    let root = s.peer(a).root().to_path_buf();
    let gd = root.join(".context/git");

    assert!(!root.join(".git").exists(), "NO .git at the scope root (§4)");
    assert!(gd.join("objects").exists(), "engine odb present");

    let (ok, head) = git(&gd, &root, &["rev-parse", "main"]);
    assert!(ok && head.trim().len() == 40, "git rev-parse main: {head}");

    let (ok, t) = git(&gd, &root, &["cat-file", "-t", head.trim()]);
    assert!(ok && t.trim() == "commit", "main is a commit object");

    let (ok, show) = git(&gd, &root, &["show", "main:a/b/deep.md"]);
    assert!(ok && show == "deep content\n", "git show nested blob: {show:?}");

    let (ok, ls) = git(&gd, &root, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(ok && ls.contains("a/b/deep.md") && ls.contains("top.md"), "ls-tree: {ls}");

    // checkout into a detached worktree elsewhere proves it is real git.
    let co = tempfile::tempdir().unwrap();
    let (ok, _) = git(
        &gd,
        co.path(),
        &["--work-tree", co.path().to_str().unwrap(), "checkout", "-f", "main", "--", "."],
    );
    assert!(ok, "unmodified git checkout of the fold head must work");
    assert_eq!(
        std::fs::read_to_string(co.path().join("top.md")).unwrap(),
        "top\n"
    );

    // The data-loss-critical guard: a mutating verb is refused.
    let (ok, _o, e) = s.peer(a).run_allow_fail(&["git", "update-ref", "refs/heads/main", head.trim()]).await;
    assert!(!ok && e.contains("refused"), "ctx git must refuse update-ref: {e}");
}
