//! `ctx git` — read-only, **deny-by-default** passthrough to the
//! engine-owned repo (§17, §13.2 data-loss-critical guard). The repo is
//! engine-owned (§4): a write reaching it is silent corruption. Only an
//! explicit allowlist of read-only subcommands runs; unknown/ambiguous
//! verbs and every mutating verb are refused with a pointer to the proper
//! `ctx` command. Raw-`GIT_DIR` bypass remains unsupported (§4).

use anyhow::{bail, Result};
use std::path::Path;
use std::process::Command;

/// The exact read-only allowlist from §17.
const READ_ONLY: &[&str] = &[
    "log", "show", "diff", "status", "blame", "cat-file", "ls-tree", "ls-files", "rev-list",
    "rev-parse", "grep", "for-each-ref", "describe", "shortlog", "reflog",
];

fn pointer(verb: &str) -> &'static str {
    match verb {
        "commit" => "edit files and let `ctx watch` auto-commit (§5.6)",
        "checkout" | "switch" | "restore" | "reset" => "use `ctx restore <name|time>` (§8)",
        "tag" | "branch" => "use `ctx snapshot <name>` to mark a point (§8)",
        "gc" | "prune" => "GC is engine-internal (§9.2)",
        "fetch" | "pull" | "push" => "replication is the realtime transport, not git (§6)",
        _ => "the engine owns this repo; there is no write workflow through git (§4)",
    }
}

/// Validate and run a `ctx git` invocation. Returns the child exit code.
pub fn run(git_dir: &Path, work_tree: &Path, args: &[String]) -> Result<i32> {
    let verb = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("ctx git: a read-only subcommand is required"))?;

    if !READ_ONLY.contains(&verb.as_str()) {
        bail!(
            "ctx git: `{verb}` is refused — the engine repo is read-only via `ctx git` ({}). \
             Allowed: {}",
            pointer(&verb),
            READ_ONLY.join(", ")
        );
    }
    // `reflog` is read-only only as `reflog show`; `reflog expire|delete` are
    // mutating.
    if verb == "reflog" {
        let sub = args.iter().filter(|a| !a.starts_with('-')).nth(1);
        if sub.map(|s| s.as_str()) != Some("show") {
            bail!("ctx git: only `reflog show` is permitted (read-only)");
        }
    }
    // Defensive: reject obviously write-capable flags even on allowed verbs.
    const WRITE_FLAGS: &[&str] = &[
        "--edit", "-d", "-D", "--delete", "--force", "--prune", "--write",
    ];
    for a in args {
        if WRITE_FLAGS.contains(&a.as_str()) {
            bail!("ctx git: write-capable flag `{a}` refused (read-only passthrough)");
        }
    }

    let status = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .arg(format!("--work-tree={}", work_tree.display()))
        .args(args)
        .status()?;
    Ok(status.code().unwrap_or(1))
}
