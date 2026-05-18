//! §10/§17.1: the **default transport is `wss://`** (self-signed cert;
//! trust is the ed25519 handshake, not a CA). This exercises the real TLS
//! path end-to-end — clone + bidirectional sync over wss — with no
//! `--no-tls` anywhere.

use csp_e2e::*;
use std::time::Duration;

#[tokio::test]
async fn wss_default_clone_and_sync() {
    let mut s = Scenario::new("v-tls");
    let a = s.add("A").await.unwrap();
    s.peer_mut(a).enable_tls(); // no --no-tls → real wss listener
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = s.peer(a).url();
    assert!(url.starts_with("wss://"), "listener must be wss: {url}");

    // Clone over wss (client accepts the self-signed cert; trust is the
    // ed25519 mutual-auth handshake — §10).
    let b = s.add_clone("B", &url).await.unwrap();
    s.peer_mut(b).enable_tls();
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();

    s.peer(a).write("secret.md", "encrypted in transit");
    assert!(
        wait_for_content(s.peer(b), "secret.md", "encrypted in transit", Duration::from_secs(25))
            .await,
        "wss sync A→B"
    );
    s.peer(b).write("reply.md", "ack over tls");
    assert!(
        wait_for_content(s.peer(a), "reply.md", "ack over tls", Duration::from_secs(25)).await,
        "wss sync B→A"
    );
    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(b)], Duration::from_secs(25))
            .await
            .is_some()
    );
    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
