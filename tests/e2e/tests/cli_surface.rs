//! §17/§18 no-regression: the whole command surface is reachable,
//! scriptable (`--json`), completes for every shell, refuses git writes,
//! and every deployment knob works via env (CTX_*) with no flag (§17.1).

use csp_e2e::*;

/// A `ctx git` refusal always carries a clear, actionable message.
fn assert_refusal_message(args: &[&str], stderr: &str) {
    assert!(
        stderr.contains("refused") || stderr.contains("is permitted"),
        "ctx {args:?} must be refused with a clear message, got: {stderr}"
    );
}

#[tokio::test]
async fn full_command_surface_is_reachable() {
    let mut s = Scenario::new("v-cli");
    let a = s.add("A").await.unwrap(); // init already exercised by add()

    // key
    let key = s.peer(a).run(&["key"]).await.unwrap();
    assert!(key.starts_with("ssh-ed25519 "), "OpenSSH pubkey: {key}");

    // authorize / revoke
    let other = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test";
    s.peer(a).run(&["authorize", other]).await.unwrap();
    s.peer(a).run(&["revoke", other]).await.unwrap();

    // status --json with the documented fields
    let js = s.peer(a).run(&["status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&js).unwrap();
    for f in [
        "vault_id",
        "node_id",
        "pubkey",
        "main",
        "frontier_tips",
        "known_primitives",
        "authorized_keys",
    ] {
        assert!(v.get(f).is_some(), "status --json missing field {f}");
    }

    // snapshot / restore
    s.peer(a).write("d.md", "one");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();
    s.peer(a).run(&["snapshot", "snap1"]).await.unwrap();
    s.peer(a).write("d.md", "two");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();
    s.peer(a).run(&["restore", "snap1"]).await.unwrap();
    assert_eq!(s.peer(a).read("d.md").as_deref(), Some("one"));

    // log (read-only wrap)
    let log = s.peer(a).run(&["log", "--format=%s"]).await.unwrap();
    assert!(log.contains("ctx edit") || log.contains("csp:"), "log: {log}");

    // git read-only allowed; mutating refused (data-loss guard §13.2)
    s.peer(a).run(&["git", "rev-parse", "main"]).await.unwrap();
    for bad in [
        vec!["git", "commit", "-m", "x"],
        vec!["git", "checkout", "main"],
        vec!["git", "reset", "--hard"],
        vec!["git", "gc"],
        vec!["git", "push"],
    ] {
        let (ok, _o, e) = s.peer(a).run_allow_fail(&bad).await;
        assert!(!ok, "ctx {bad:?} must be refused");
        assert!(e.contains("refused"), "expected refusal pointer: {e}");
    }

    // scope show / ignore / include
    s.peer(a).run(&["scope", "ignore", "*.tmp"]).await.unwrap();
    s.peer(a).run(&["scope", "include", "**"]).await.unwrap();
    let sc = s.peer(a).run(&["scope"]).await.unwrap();
    assert!(sc.contains("*.tmp"), "scope show must list ignore: {sc}");

    // completions for every required shell
    for sh in ["bash", "zsh", "fish", "powershell"] {
        let out = s.peer(a).run(&["completions", sh]).await.unwrap();
        assert!(!out.trim().is_empty(), "empty completion for {sh}");
    }
}

/// Flag-granularity data-loss guard: `ctx git` is read-only deny-by-default,
/// so a leading global option, a file-writing / command-spawning flag, or any
/// mutating verb must all be refused — while ordinary read invocations still
/// pass straight through.
#[tokio::test]
async fn git_passthrough_is_flag_conservative() {
    let mut s = Scenario::new("v-gitflags");
    let a = s.add("A").await.unwrap();

    // Seed a couple of commits so the read paths have real history to walk.
    s.peer(a).write("d.md", "one");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();
    s.peer(a).write("d.md", "two");
    s.peer(a).run(&["watch", "--once"]).await.unwrap();

    // Leading global options (config injection, dir redirection, alias /
    // protocol games) must never reach git — the subcommand must be first.
    let leading: &[&[&str]] = &[
        &["git", "-c", "x=y", "log"],
        &["git", "-c", "protocol.ext.allow=always", "log"],
        &["git", "-C", "/tmp", "log"],
        &["git", "--exec-path=/tmp", "log"],
        &["git", "--git-dir=/tmp", "log"],
        &["git", "--work-tree=/tmp", "status"],
        &["git", "--namespace=ns", "log"],
    ];
    for bad in leading {
        let (ok, _o, e) = s.peer(a).run_allow_fail(bad).await;
        assert!(!ok, "ctx {bad:?} must be refused");
        assert_refusal_message(bad, &e);
    }

    // File-writing / command-spawning flags on otherwise-allowed verbs.
    let dangerous: &[&[&str]] = &[
        &["git", "log", "--output=/tmp/x"],
        &["git", "show", "--output=/tmp/x"],
        &["git", "diff", "--output=/tmp/x"],
        &["git", "grep", "-O/bin/sh", "foo"],
        &["git", "grep", "--open-files-in-pager=/bin/sh", "foo"],
        &["git", "diff", "--ext-diff"],
        &["git", "log", "--textconv"],
        &["git", "log", "--an-unknown-flag"],
    ];
    for bad in dangerous {
        let (ok, _o, e) = s.peer(a).run_allow_fail(bad).await;
        assert!(!ok, "ctx {bad:?} must be refused");
        assert_refusal_message(bad, &e);
    }

    // Every mutating / repo-rewriting verb is refused outright.
    let mutating: &[&[&str]] = &[
        &["git", "commit", "-m", "x"],
        &["git", "checkout", "main"],
        &["git", "switch", "main"],
        &["git", "reset", "--hard"],
        &["git", "merge", "other"],
        &["git", "rebase", "main"],
        &["git", "gc"],
        &["git", "prune"],
        &["git", "update-ref", "refs/heads/x", "HEAD"],
        &["git", "apply", "p.patch"],
        &["git", "cherry-pick", "HEAD"],
        &["git", "restore", "d.md"],
        &["git", "clean", "-fd"],
        &["git", "stash"],
        &["git", "fetch"],
        &["git", "push"],
        &["git", "filter-branch"],
        &["git", "branch", "-d", "x"],
        &["git", "tag", "-d", "x"],
        &["git", "config", "user.x", "y"],
        &["git", "reflog", "expire"],
        &["git", "reflog", "delete", "HEAD@{0}"],
    ];
    for bad in mutating {
        let (ok, _o, e) = s.peer(a).run_allow_fail(bad).await;
        assert!(!ok, "ctx {bad:?} must be refused");
        assert_refusal_message(bad, &e);
    }

    // Legitimate read-only invocations still pass through unchanged.
    for good in [
        vec!["git", "log", "--oneline", "-n", "5"],
        vec!["git", "show", "HEAD"],
        vec!["git", "status"],
        vec!["git", "diff", "HEAD~1", "HEAD"],
        vec!["git", "cat-file", "-p", "HEAD"],
        vec!["git", "rev-parse", "HEAD"],
        vec!["git", "for-each-ref"],
        vec!["git", "reflog", "show"],
    ] {
        s.peer(a)
            .run(&good)
            .await
            .unwrap_or_else(|e| panic!("ctx {good:?} should pass the read-only guard: {e}"));
    }
}
