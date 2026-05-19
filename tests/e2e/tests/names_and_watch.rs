//! Names / ids / `--watch` (the agreed design): default `vault_id` is an
//! opaque UUID; the human name is derived from the init directory; `ctx
//! clone` names the folder by the name (not the raw id); `clone --watch`
//! stays running and syncs without a separate `ctx watch`.

use csp_e2e::*;
use std::time::Duration;

fn looks_like_uuid(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    p.len() == 5
        && p.iter().map(|x| x.len()).collect::<Vec<_>>() == [8, 4, 4, 4, 12]
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

#[tokio::test]
async fn default_id_is_uuid_and_name_is_derived() {
    let mut s = Scenario::new("ignored");
    let a = s.add_uninit("A").unwrap();
    // `ctx init` with NO --vault-id → UUID id + name derived from the dir.
    s.peer(a).run(&["init"]).await.unwrap();

    let js = s.peer(a).run(&["status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&js).unwrap();
    let vid = v["vault_id"].as_str().unwrap();
    assert!(looks_like_uuid(vid), "default vault_id must be a UUID: {vid}");
    // The temp dir's basename is non-empty, so a name is derived.
    let name = v["name"].as_str().unwrap_or("");
    assert!(!name.is_empty(), "name should be derived from the dir");
    // It must NOT be the old pubkey-prefixed form.
    assert!(!vid.starts_with("vault-"), "id must not leak the node key");
}

#[tokio::test]
async fn init_path_arg_creates_folder_and_wins_over_ctx_cwd() {
    let mut s = Scenario::new("ignored");
    let a = s.add_uninit("A").unwrap();
    // The harness always exports CTX_DIR = peer root. A positional path is
    // the most explicit form, so it must WIN over CTX_DIR and be created
    // if missing, nested parents and all (git `init <dir>` spirit).
    let target = s.peer(a).root().join("nested/created-by-arg");
    let tgt = target.to_str().unwrap();
    s.peer(a).run(&["init", tgt]).await.unwrap();

    assert!(
        target.join(".context").exists(),
        "`ctx init <path>` must create + init the path"
    );
    assert!(
        !s.peer(a).root().join(".context").exists(),
        "positional path must win over CTX_DIR (root must stay uninited)"
    );
    // Name derives from the new folder's basename (git-spirit).
    let js = s.peer(a).run(&["--dir", tgt, "status", "--json"]).await.unwrap();
    let v: serde_json::Value = serde_json::from_str(&js).unwrap();
    assert_eq!(v["name"].as_str().unwrap_or(""), "created-by-arg");
}

#[tokio::test]
async fn clone_folder_is_named_by_vault_name_not_id() {
    let mut s = Scenario::new("ignored");
    let a = s.add_uninit("A").unwrap();
    s.peer(a)
        .run(&["init", "--name", "team-notes"])
        .await
        .unwrap();
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = s.peer(a).url();

    // Clone with NO target dir → folder named after the human name.
    let b = s.add_uninit("B").unwrap();
    s.peer(b).run(&["clone", &url]).await.unwrap();
    let folder = s.peer(b).root().join("team-notes");
    assert!(
        folder.join(".context").exists(),
        "clone must create ./team-notes/, got: {:?}",
        std::fs::read_dir(s.peer(b).root())
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
    );
    s.peer_mut(a).stop().await;
}

#[tokio::test]
async fn clone_with_watch_keeps_syncing() {
    let mut s = Scenario::new("v-cw");
    let a = s.add("A").await.unwrap();
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = s.peer(a).url();

    // One command: bootstrap + stay running as the daemon.
    let b = s.add_uninit("B").unwrap();
    s.peer_mut(b)
        .spawn_daemon(&["clone", &url, ".", "--watch"])
        .await
        .unwrap();

    s.peer(a).write("live.md", "via clone --watch");
    assert!(
        wait_for_content(s.peer(b), "live.md", "via clone --watch", Duration::from_secs(25))
            .await,
        "`clone --watch` must keep syncing without a separate `ctx watch`"
    );
    // reverse direction too
    s.peer(b).write("back.md", "from B");
    assert!(
        wait_for_content(s.peer(a), "back.md", "from B", Duration::from_secs(25)).await
    );
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
