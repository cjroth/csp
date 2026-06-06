//! Node-local daemon health, surfaced in `ctx status`.
//!
//! The `watch` loop reconciles and commits the working tree on a timer (and on
//! filesystem events). When a commit/publish *fails* — the canonical case is a
//! full disk wedging the object store — the old behavior was a single `warn!`
//! that scrolled past, so a *persistent* failure silently ate every offline
//! edit with nothing to show for it. We instead persist a degraded marker here
//! that a separate `ctx status` process can read.
//!
//! The marker lives at `.context/health.json` (under `.context/`, which is
//! never synced — this is per-node, not vault, state). Absent file = healthy.
//! Best-effort throughout: health reporting must never itself break the daemon.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const HEALTH_FILE: &str = "health.json";

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct Health {
    pub degraded: bool,
    /// Most recent commit error (truncated). Empty when healthy.
    pub last_error: String,
    /// Unix secs of the *first* failure in the current degraded streak.
    pub since_unix: u64,
    /// Unix secs of the most recent failure.
    pub last_unix: u64,
    /// Consecutive failures in the current streak.
    pub fail_count: u64,
}

fn file(context_dir: &Path) -> PathBuf {
    context_dir.join(HEALTH_FILE)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the current health marker, if any. `None` (or a non-degraded record)
/// means healthy.
pub fn read(context_dir: &Path) -> Option<Health> {
    let bytes = std::fs::read(file(context_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Record a failed commit/publish. Preserves `since_unix` across a streak so
/// status can show how long the node has been stuck, and bumps the count.
pub fn record_failure(context_dir: &Path, err: &str) {
    let prev = read(context_dir).filter(|h| h.degraded);
    let (since, count) = prev
        .map(|h| (h.since_unix, h.fail_count))
        .unwrap_or((now(), 0));
    let rec = Health {
        degraded: true,
        last_error: err.chars().take(500).collect(),
        since_unix: since,
        last_unix: now(),
        fail_count: count + 1,
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&rec) {
        let _ = std::fs::write(file(context_dir), bytes);
    }
}

/// Clear any degraded marker after a successful commit/publish. Idempotent.
pub fn record_success(context_dir: &Path) {
    let p = file(context_dir);
    if p.exists() {
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn absent_is_healthy() {
        let td = tempdir().unwrap();
        assert!(read(td.path()).is_none());
    }

    #[test]
    fn failure_then_success_roundtrip() {
        let td = tempdir().unwrap();
        record_failure(td.path(), "ENOSPC: No space left on device");
        let h = read(td.path()).expect("degraded record written");
        assert!(h.degraded);
        assert_eq!(h.fail_count, 1);
        assert!(h.last_error.contains("ENOSPC"));
        let since = h.since_unix;

        // A second failure preserves `since` and bumps the count.
        record_failure(td.path(), "ENOSPC again");
        let h2 = read(td.path()).unwrap();
        assert_eq!(h2.fail_count, 2);
        assert_eq!(h2.since_unix, since, "streak start must be preserved");

        // Success clears it.
        record_success(td.path());
        assert!(read(td.path()).is_none());
        // Clearing again is a no-op.
        record_success(td.path());
        assert!(read(td.path()).is_none());
    }

    #[test]
    fn long_error_is_truncated() {
        let td = tempdir().unwrap();
        record_failure(td.path(), &"x".repeat(5000));
        let h = read(td.path()).unwrap();
        assert!(h.last_error.chars().count() <= 500);
    }
}
