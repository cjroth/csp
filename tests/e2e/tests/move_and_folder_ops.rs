//! §18 end-to-end coverage for filesystem-level rename / folder ops.
//!
//! These tests drive TWO real `ctx watch` processes through actual `std::fs`
//! operations (rename, recursive directory rename, recursive directory delete)
//! and verify both peers converge with no duplicates left behind. The
//! existing `two_peer_sync` test treats "rename" as `write(new) + delete(old)`
//! — that's a different code path that doesn't exercise the notify watcher's
//! Rename events. The user-reported "moving files between folders is flakey
//! and sometimes duplicates the file" symptom comes from this code path
//! specifically.

use csp_e2e::*;
use std::time::Duration;

/// Set up a two-peer scenario with mutual auth, `A` as listener and `B`
/// connecting outward. Returns the peer indices `(a, b)`.
async fn two_peers(label: &str) -> (Scenario, usize, usize) {
    let mut s = Scenario::new(label);
    let a = s.add("A").await.unwrap();
    let b = s.add("B").await.unwrap();
    s.mutual_authorize().await.unwrap();
    s.peer_mut(a).start_watch(true, &[]).await.unwrap();
    let url = format!("ws://127.0.0.1:{}", s.peer(a).port.unwrap());
    s.peer_mut(b).start_watch(false, &[url]).await.unwrap();
    (s, a, b)
}

/// Tighter convergence window — long enough for the 250ms debounce + a
/// round trip, short enough to keep the suite fast.
fn t() -> Duration {
    Duration::from_secs(30)
}

// =============================================================================
// SINGLE FILE — rename in place, move across folders
// =============================================================================

#[tokio::test]
async fn file_rename_in_place_propagates_and_no_duplicate() {
    let (mut s, a, b) = two_peers("v-mv-rename").await;

    s.peer(a).write("note.md", "hello");
    assert!(
        wait_for_content(s.peer(b), "note.md", "hello", t()).await,
        "create did not propagate"
    );

    s.peer(a).rename("note.md", "renamed.md");

    assert!(
        wait_for_content(s.peer(b), "renamed.md", "hello", t()).await,
        "rename target did not appear on B"
    );
    assert!(
        wait_for_missing(s.peer(b), "note.md", t()).await,
        "old path should have been removed on B"
    );

    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(b)], t()).await.is_some(),
        "peers did not converge"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn file_move_across_folders_no_duplicate() {
    let (mut s, a, b) = two_peers("v-mv-cross").await;

    s.peer(a).write("Folder1/note.md", "payload");
    assert!(
        wait_for_content(s.peer(b), "Folder1/note.md", "payload", t()).await,
        "initial create did not propagate"
    );

    s.peer(a).rename("Folder1/note.md", "Folder2/note.md");

    assert!(
        wait_for_content(s.peer(b), "Folder2/note.md", "payload", t()).await,
        "moved file did not appear at new path on B"
    );
    assert!(
        wait_for_missing(s.peer(b), "Folder1/note.md", t()).await,
        "moved file LEAKED at old path on B — duplication bug"
    );

    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(b)], t()).await.is_some(),
        "peers did not converge"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn file_move_into_deeply_nested_folder() {
    let (mut s, a, b) = two_peers("v-mv-deep").await;

    s.peer(a).write("note.md", "x");
    assert!(wait_for_content(s.peer(b), "note.md", "x", t()).await);

    s.peer(a).rename("note.md", "a/b/c/d/e/deep.md");

    assert!(
        wait_for_content(s.peer(b), "a/b/c/d/e/deep.md", "x", t()).await,
        "deep destination did not propagate"
    );
    assert!(
        wait_for_missing(s.peer(b), "note.md", t()).await,
        "old path lingers"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn file_move_back_and_forth() {
    let (mut s, a, b) = two_peers("v-mv-bounce").await;

    s.peer(a).write("A/x.md", "p");
    assert!(wait_for_content(s.peer(b), "A/x.md", "p", t()).await);

    for from_to in &[("A/x.md", "B/x.md"), ("B/x.md", "A/x.md"), ("A/x.md", "B/x.md")] {
        s.peer(a).rename(from_to.0, from_to.1);
        assert!(
            wait_for_content(s.peer(b), from_to.1, "p", t()).await,
            "rename {} → {} did not converge",
            from_to.0,
            from_to.1
        );
        assert!(
            wait_for_missing(s.peer(b), from_to.0, t()).await,
            "old path {} lingers",
            from_to.0
        );
    }

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

// =============================================================================
// FOLDER RENAME (recursive) — same parent, different parent, nested
// =============================================================================

#[tokio::test]
async fn folder_rename_same_parent_propagates() {
    let (mut s, a, b) = two_peers("v-folder-rn").await;

    for n in &["a", "b", "c"] {
        s.peer(a).write(&format!("Old/{}.md", n), n);
    }
    for n in &["a", "b", "c"] {
        assert!(
            wait_for_content(s.peer(b), &format!("Old/{}.md", n), n, t()).await,
            "Old/{}.md did not propagate",
            n
        );
    }

    s.peer(a).rename_dir("Old", "New");

    for n in &["a", "b", "c"] {
        assert!(
            wait_for_content(s.peer(b), &format!("New/{}.md", n), n, t()).await,
            "New/{}.md missing on B",
            n
        );
        assert!(
            wait_for_missing(s.peer(b), &format!("Old/{}.md", n), t()).await,
            "Old/{}.md leaked on B",
            n
        );
    }

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn folder_rename_different_parent() {
    let (mut s, a, b) = two_peers("v-folder-mv").await;

    s.peer(a).write("P1/Inner/x.md", "X");
    s.peer(a).write("P1/Inner/y.md", "Y");
    assert!(wait_for_content(s.peer(b), "P1/Inner/x.md", "X", t()).await);
    assert!(wait_for_content(s.peer(b), "P1/Inner/y.md", "Y", t()).await);

    s.peer(a).rename_dir("P1/Inner", "P2/Inner");

    assert!(wait_for_content(s.peer(b), "P2/Inner/x.md", "X", t()).await);
    assert!(wait_for_content(s.peer(b), "P2/Inner/y.md", "Y", t()).await);
    assert!(wait_for_missing(s.peer(b), "P1/Inner/x.md", t()).await);
    assert!(wait_for_missing(s.peer(b), "P1/Inner/y.md", t()).await);

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn folder_rename_with_nested_subfolders() {
    let (mut s, a, b) = two_peers("v-folder-nest").await;

    s.peer(a).write("Root/top.md", "top");
    s.peer(a).write("Root/Sub/mid.md", "mid");
    s.peer(a).write("Root/Sub/Deeper/leaf.md", "leaf");
    assert!(wait_for_content(s.peer(b), "Root/Sub/Deeper/leaf.md", "leaf", t()).await);

    s.peer(a).rename_dir("Root", "Renamed");

    assert!(wait_for_content(s.peer(b), "Renamed/top.md", "top", t()).await);
    assert!(wait_for_content(s.peer(b), "Renamed/Sub/mid.md", "mid", t()).await);
    assert!(wait_for_content(s.peer(b), "Renamed/Sub/Deeper/leaf.md", "leaf", t()).await);
    assert!(wait_for_missing(s.peer(b), "Root/top.md", t()).await);
    assert!(wait_for_missing(s.peer(b), "Root/Sub/mid.md", t()).await);
    assert!(wait_for_missing(s.peer(b), "Root/Sub/Deeper/leaf.md", t()).await);

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn deep_nested_folder_rename() {
    let (mut s, a, b) = two_peers("v-folder-deep").await;

    s.peer(a).write("a/b/c/d/e/leaf.md", "deep");
    assert!(wait_for_content(s.peer(b), "a/b/c/d/e/leaf.md", "deep", t()).await);

    s.peer(a).rename_dir("a/b/c", "a/b/X");

    assert!(wait_for_content(s.peer(b), "a/b/X/d/e/leaf.md", "deep", t()).await);
    assert!(wait_for_missing(s.peer(b), "a/b/c/d/e/leaf.md", t()).await);

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn large_folder_rename_no_duplicates() {
    let (mut s, a, b) = two_peers("v-folder-big").await;

    for i in 0..40_u32 {
        s.peer(a).write(&format!("Big/f{:02}.md", i), &format!("p{}", i));
    }
    for i in 0..40_u32 {
        assert!(
            wait_for_content(s.peer(b), &format!("Big/f{:02}.md", i), &format!("p{}", i), t())
                .await,
            "Big/f{:02}.md did not propagate",
            i
        );
    }

    s.peer(a).rename_dir("Big", "Huge");

    for i in 0..40_u32 {
        assert!(
            wait_for_content(s.peer(b), &format!("Huge/f{:02}.md", i), &format!("p{}", i), t())
                .await,
            "Huge/f{:02}.md missing on B",
            i
        );
        assert!(
            wait_for_missing(s.peer(b), &format!("Big/f{:02}.md", i), t()).await,
            "Big/f{:02}.md LEAKED on B (duplication)",
            i
        );
    }

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

// =============================================================================
// FOLDER DELETE (recursive) — flat, nested, large
// =============================================================================

#[tokio::test]
async fn folder_delete_single_level() {
    let (mut s, a, b) = two_peers("v-folder-del").await;

    s.peer(a).write("D/a.md", "a");
    s.peer(a).write("D/b.md", "b");
    s.peer(a).write("D/c.md", "c");
    assert!(wait_for_content(s.peer(b), "D/c.md", "c", t()).await);

    s.peer(a).delete_dir("D");

    for n in &["a", "b", "c"] {
        assert!(
            wait_for_missing(s.peer(b), &format!("D/{}.md", n), t()).await,
            "D/{}.md did not get removed on B",
            n
        );
    }

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn folder_delete_nested_subfolders() {
    let (mut s, a, b) = two_peers("v-folder-del-nest").await;

    s.peer(a).write("Root/top.md", "t");
    s.peer(a).write("Root/Sub/mid.md", "m");
    s.peer(a).write("Root/Sub/Deeper/leaf.md", "l");
    assert!(wait_for_content(s.peer(b), "Root/Sub/Deeper/leaf.md", "l", t()).await);

    s.peer(a).delete_dir("Root");

    assert!(wait_for_missing(s.peer(b), "Root/top.md", t()).await);
    assert!(wait_for_missing(s.peer(b), "Root/Sub/mid.md", t()).await);
    assert!(wait_for_missing(s.peer(b), "Root/Sub/Deeper/leaf.md", t()).await);

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn folder_delete_preserves_siblings() {
    let (mut s, a, b) = two_peers("v-folder-del-sib").await;

    s.peer(a).write("Root/Keep/keep.md", "k");
    s.peer(a).write("Root/Gone/gone.md", "g");
    assert!(wait_for_content(s.peer(b), "Root/Keep/keep.md", "k", t()).await);
    assert!(wait_for_content(s.peer(b), "Root/Gone/gone.md", "g", t()).await);

    s.peer(a).delete_dir("Root/Gone");

    assert!(wait_for_missing(s.peer(b), "Root/Gone/gone.md", t()).await);
    // Sibling must survive.
    assert_eq!(s.peer(b).read("Root/Keep/keep.md").as_deref(), Some("k"));

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn large_folder_delete() {
    let (mut s, a, b) = two_peers("v-folder-del-big").await;

    for i in 0..30_u32 {
        s.peer(a).write(&format!("Big/f{:02}.md", i), "x");
    }
    for i in 0..30_u32 {
        assert!(
            wait_for_content(s.peer(b), &format!("Big/f{:02}.md", i), "x", t()).await,
            "Big/f{:02}.md did not propagate",
            i
        );
    }

    s.peer(a).delete_dir("Big");

    for i in 0..30_u32 {
        assert!(
            wait_for_missing(s.peer(b), &format!("Big/f{:02}.md", i), t()).await,
            "Big/f{:02}.md did not get removed",
            i
        );
    }

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

// =============================================================================
// MIXED workflows — what real users do
// =============================================================================

#[tokio::test]
async fn move_then_modify_propagates_final_content() {
    let (mut s, a, b) = two_peers("v-mv-mod").await;

    s.peer(a).write("A/note.md", "first");
    assert!(wait_for_content(s.peer(b), "A/note.md", "first", t()).await);

    s.peer(a).rename("A/note.md", "B/note.md");
    assert!(wait_for_content(s.peer(b), "B/note.md", "first", t()).await);

    s.peer(a).write("B/note.md", "second");
    assert!(wait_for_content(s.peer(b), "B/note.md", "second", t()).await);
    assert!(wait_for_missing(s.peer(b), "A/note.md", t()).await);

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn rename_folder_then_add_files() {
    let (mut s, a, b) = two_peers("v-folder-rn-add").await;

    s.peer(a).write("Original/a.md", "a");
    assert!(wait_for_content(s.peer(b), "Original/a.md", "a", t()).await);

    s.peer(a).rename_dir("Original", "Renamed");
    assert!(wait_for_content(s.peer(b), "Renamed/a.md", "a", t()).await);
    assert!(wait_for_missing(s.peer(b), "Original/a.md", t()).await);

    s.peer(a).write("Renamed/b.md", "b");
    assert!(wait_for_content(s.peer(b), "Renamed/b.md", "b", t()).await);

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn move_file_into_existing_folder_with_sibling() {
    let (mut s, a, b) = two_peers("v-mv-sib").await;

    s.peer(a).write("Dest/already.md", "existing");
    s.peer(a).write("Src/new.md", "moving");
    assert!(wait_for_content(s.peer(b), "Dest/already.md", "existing", t()).await);
    assert!(wait_for_content(s.peer(b), "Src/new.md", "moving", t()).await);

    s.peer(a).rename("Src/new.md", "Dest/new.md");

    assert!(wait_for_content(s.peer(b), "Dest/new.md", "moving", t()).await);
    assert!(wait_for_missing(s.peer(b), "Src/new.md", t()).await);
    // The sibling must not be disturbed.
    assert_eq!(s.peer(b).read("Dest/already.md").as_deref(), Some("existing"));

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}

#[tokio::test]
async fn convergence_after_a_long_move_sequence() {
    let (mut s, a, b) = two_peers("v-mv-many").await;

    s.peer(a).write("path0/note.md", "stable");
    assert!(wait_for_content(s.peer(b), "path0/note.md", "stable", t()).await);

    for step in 1..6_u32 {
        let from = format!("path{}/note.md", step - 1);
        let to = format!("path{}/note.md", step);
        s.peer(a).rename(&from, &to);
        assert!(
            wait_for_content(s.peer(b), &to, "stable", t()).await,
            "step {} did not converge",
            step
        );
        assert!(
            wait_for_missing(s.peer(b), &from, t()).await,
            "step {} leaked the old path",
            step
        );
    }

    assert!(
        wait_for_convergence(&[s.peer(a), s.peer(b)], t()).await.is_some(),
        "main SHAs did not converge after a long move chain"
    );

    s.peer_mut(a).stop().await;
    s.peer_mut(b).stop().await;
}
