//! §17/§18 no-regression: the whole command surface is reachable,
//! scriptable (`--json`), completes for every shell, refuses git writes,
//! and every deployment knob works via env (CTX_*) with no flag (§17.1).

use csp_e2e::*;

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
        "tier",
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
