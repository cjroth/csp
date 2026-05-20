//! Integration tests for the REAL engine (`CspEngine` over native
//! `csp-core`). No mocks: real vaults on real temp filesystems, real
//! ed25519 identities, real WebSocket sync between two independent nodes.
//!
//! These run on plain Linux (the engine crate has no `tauri` dep).

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use context_desktop_engine::{CspEngine, Engine, RestoreTarget};

/// Spawn an engine with isolated app-config + a distinct device identity.
async fn engine(tmp: &Path, name: &str) -> Arc<CspEngine> {
    let cfg = tmp.join(format!("cfg-{name}"));
    std::fs::create_dir_all(&cfg).unwrap();
    // Plaintext ws:// for deterministic tests (no TLS handshake variance).
    std::fs::write(
        cfg.join("config.json"),
        r#"{"vaults":[],"settings":{"startAtLogin":true,"logLevel":"info","listenByDefault":false,"noTlsByDefault":true}}"#,
    )
    .unwrap();
    let idp = tmp.join(format!("id-{name}"));
    CspEngine::new(cfg, Some(idp)).await.expect("engine new")
}

async fn wait_until<F>(secs: u64, mut cond: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    cond()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_status_identity_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), "solo").await;

    // Identity is a real ed25519 OpenSSH key.
    let id = e.get_identity().await.unwrap();
    assert!(id.openssh.starts_with("ssh-ed25519 "));
    assert!(id.fingerprint.starts_with("SHA256:"));

    // Add a brand-new local folder → init.
    let folder = tmp.path().join("solo-vault");
    let v = e
        .add_local_folder(folder.to_string_lossy().into())
        .await
        .unwrap();
    assert!(!v.is_csp_vault, "fresh dir → init, not attach");
    assert!(folder.join(".context").exists(), "vault initialised");
    assert_eq!(e.list_vaults().await.unwrap().len(), 1);

    let st = e.get_status(v.id.clone()).await.unwrap();
    assert!(matches!(
        st.state,
        context_desktop_engine::SyncState::Idle | context_desktop_engine::SyncState::Active
    ));

    // Settings round-trip + persistence.
    let mut s = e.get_settings().await.unwrap();
    s.log_level = "debug".into();
    e.set_settings(s).await.unwrap();
    assert_eq!(e.get_settings().await.unwrap().log_level, "debug");

    // Re-open the same app-config dir → folder + settings persisted.
    drop(e);
    let cfg = tmp.path().join("cfg-solo");
    let e2 = CspEngine::new(cfg, Some(tmp.path().join("id-solo")))
        .await
        .unwrap();
    assert_eq!(e2.list_vaults().await.unwrap().len(), 1);
    assert_eq!(e2.get_settings().await.unwrap().log_level, "debug");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authorize_and_revoke() {
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), "auth").await;
    let folder = tmp.path().join("auth-vault");
    let v = e
        .add_local_folder(folder.to_string_lossy().into())
        .await
        .unwrap();

    assert!(e.list_authorized(v.id.clone()).await.unwrap().is_empty());

    // A real, valid OpenSSH ed25519 line (from a throwaway identity).
    let other = engine(tmp.path(), "other").await;
    let key = other.get_identity().await.unwrap().openssh;
    e.authorize(v.id.clone(), key.clone()).await.unwrap();

    let list = e.list_authorized(v.id.clone()).await.unwrap();
    assert_eq!(list.len(), 1);
    let fp = list[0].fingerprint.clone();
    assert!(fp.starts_with("SHA256:"));

    e.revoke(v.id.clone(), fp).await.unwrap();
    assert!(e.list_authorized(v.id.clone()).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connect_address_caveat_tracks_authorized_set() {
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), "addr").await;
    let folder = tmp.path().join("addr-vault");
    let v = e
        .add_local_folder(folder.to_string_lossy().into())
        .await
        .unwrap();
    let info = e.set_allow_connections(v.id.clone(), true).await.unwrap();
    assert!(info.bound && info.port > 0 && info.scheme == "ws");

    let a = e.get_connect_address(v.id.clone()).await.unwrap();
    assert!(a.no_authorized_keys, "empty set");
    assert!(a.note.is_some(), "shows the empty-set note");
    assert!(a.address.starts_with("ws://"));

    let other = engine(tmp.path(), "addr-peer").await;
    e.authorize(v.id.clone(), other.get_identity().await.unwrap().openssh)
        .await
        .unwrap();
    let a2 = e.get_connect_address(v.id.clone()).await.unwrap();
    assert!(!a2.no_authorized_keys && a2.note.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_and_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let e = engine(tmp.path(), "snap").await;
    let folder = tmp.path().join("snap-vault");
    let v = e
        .add_local_folder(folder.to_string_lossy().into())
        .await
        .unwrap();
    let note = folder.join("note.md");

    std::fs::write(&note, "v1").unwrap();
    e.commit_now(&v.id).await.unwrap();
    e.create_snapshot(v.id.clone(), "s1".into()).await.unwrap();
    assert_eq!(e.list_snapshots(v.id.clone()).await.unwrap().len(), 1);

    std::fs::write(&note, "v2").unwrap();
    e.commit_now(&v.id).await.unwrap();

    e.restore(v.id.clone(), RestoreTarget::Named { name: "s1".into() })
        .await
        .unwrap();
    let ok = wait_until(10, || {
        std::fs::read_to_string(&note).map(|c| c == "v1").unwrap_or(false)
    })
    .await;
    assert!(ok, "restore-as-edit brings the working file back to v1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_node_real_sync_and_scope() {
    let tmp = tempfile::tempdir().unwrap();

    // Node A: serve.
    let a = engine(tmp.path(), "A").await;
    let fa = tmp.path().join("A-vault");
    let va = a
        .add_local_folder(fa.to_string_lossy().into())
        .await
        .unwrap();
    let listener = a.set_allow_connections(va.id.clone(), true).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", listener.port);

    // Node B: distinct device. A authorizes B (explicit, no TOFU).
    let b = engine(tmp.path(), "B").await;
    a.authorize(va.id.clone(), b.get_identity().await.unwrap().openssh)
        .await
        .unwrap();

    let fb = tmp.path().join("B-vault");
    let vb = b
        .clone_remote(fb.to_string_lossy().into(), url, None)
        .await
        .unwrap();

    // A → B: an edit on A appears on B.
    std::fs::write(fa.join("note.md"), "hello from A").unwrap();
    a.commit_now(&va.id).await.unwrap();
    let got = wait_until(25, || {
        std::fs::read_to_string(fb.join("note.md"))
            .map(|c| c == "hello from A")
            .unwrap_or(false)
    })
    .await;
    assert!(got, "B materialised A's edit over real WebSocket sync");

    // `.context/` is engine-owned and MUST NOT sync as a working file
    // (CSP §11 HARD INVARIANT). Put a secret under A/.context and verify
    // it never appears in B's working tree.
    std::fs::write(fa.join(".context").join("secret.txt"), "TOP SECRET").unwrap();
    std::fs::write(fa.join("after.md"), "scope-probe").unwrap();
    a.commit_now(&va.id).await.unwrap();
    let after = wait_until(20, || fb.join("after.md").exists()).await;
    assert!(after, "later edit synced");
    assert!(
        !fb.join("secret.txt").exists() && !fb.join(".context").join("secret.txt").exists(),
        ".context/ contents never cross as working files"
    );

    // Concurrent edits both sides → deterministic convergence (same main).
    std::fs::write(fa.join("a.txt"), "AAA").unwrap();
    std::fs::write(fb.join("b.txt"), "BBB").unwrap();
    a.commit_now(&va.id).await.unwrap();
    b.commit_now(&vb.id).await.unwrap();

    let converged = wait_until(30, || {
        fb.join("a.txt").exists()
            && fa.join("b.txt").exists()
            && std::fs::read_to_string(fb.join("a.txt")).unwrap_or_default() == "AAA"
            && std::fs::read_to_string(fa.join("b.txt")).unwrap_or_default() == "BBB"
    })
    .await;
    assert!(converged, "both nodes hold both concurrent edits");

    // Deterministic fold ⇒ identical `main` on both replicas.
    let ok = wait_until(15, || {
        let ma = futures_block(a.get_status(va.id.clone()));
        let mb = futures_block(b.get_status(vb.id.clone()));
        match (ma, mb) {
            (Ok(x), Ok(y)) => {
                x.main_short_sha.is_some() && x.main_short_sha == y.main_short_sha
            }
            _ => false,
        }
    })
    .await;
    assert!(ok, "deterministic convergence: A.main == B.main");
}

/// Tiny blocking bridge so the sync `wait_until` closure can call async
/// engine getters (test-only).
fn futures_block<F: std::future::Future>(f: F) -> F::Output {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(f)
    })
}
