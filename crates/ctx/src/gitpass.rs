//! `ctx git` — read-only, **deny-by-default** passthrough to the
//! engine-owned repo (data-loss-critical guard). The repo is engine-owned:
//! a write reaching it is silent corruption. Only an explicit allowlist of
//! read-only subcommands runs, and only with an explicit allowlist of safe
//! read-only flags; unknown/ambiguous verbs, every mutating verb, every
//! leading global option, and any unrecognized flag are refused with a
//! pointer to the proper `ctx` command. Raw-`GIT_DIR` bypass remains
//! unsupported.

use anyhow::{bail, Result};
use std::path::Path;
use std::process::Command;

/// The exact read-only subcommand allowlist.
const READ_ONLY: &[&str] = &[
    "log", "show", "diff", "status", "blame", "cat-file", "ls-tree", "ls-files", "rev-list",
    "rev-parse", "grep", "for-each-ref", "describe", "shortlog", "reflog",
];

/// Flags that are safe across every allowed verb: they only shape how the
/// already-read history/objects are formatted or selected and never write a
/// file, spawn a process, or change config. Anything not listed here is
/// refused — including any `--flag=value` form not present, since those are
/// the vectors for writing files (`--output=`), spawning commands
/// (`--open-files-in-pager=`, `-c core.pager=!cmd`), or rewriting the repo.
const SAFE_FLAGS: &[&str] = &[
    // Output shaping / pagination.
    "--oneline",
    "--no-pager",
    "--paginate",
    "--color",
    "--no-color",
    "--no-renames",
    "--stat",
    "--numstat",
    "--shortstat",
    "--summary",
    "--name-only",
    "--name-status",
    "--raw",
    "--patch",
    "-p",
    "-u",
    "--no-patch",
    "-s",
    "--abbrev-commit",
    "--no-abbrev-commit",
    "--graph",
    "--decorate",
    "--no-decorate",
    "--parents",
    "--children",
    "--reverse",
    "--all",
    "--branches",
    "--tags",
    "--remotes",
    "--full-history",
    "--first-parent",
    "--merges",
    "--no-merges",
    "--follow",
    "--source",
    "--show-signature",
    "--cc",
    "--word-diff",
    "--function-context",
    "-w",
    "--ignore-all-space",
    "--ignore-space-change",
    "-b",
    "--text",
    "-a",
    "--binary",
    "--full-index",
    "--check",
    "--exit-code",
    "--quiet",
    "-q",
    "-v",
    "--verbose",
    "--long",
    "--short",
    "--porcelain",
    "--branch",
    "--null",
    "-z",
    "--cached",
    "--staged",
    "--stage",
    "--others",
    "--ignored",
    "--modified",
    "--deleted",
    "--unmerged",
    "--no-color-moved",
    "--objects",
    "--count",
    "--no-commit-id",
    "--root",
    "--line-number",
    "-n",
    "-l",
    "-i",
    "--ignore-case",
    "-E",
    "--extended-regexp",
    "-F",
    "--fixed-strings",
    "-P",
    "--perl-regexp",
    "-G",
    "--basic-regexp",
    "--word-regexp",
    "-c",
    "-h",
    "-H",
    "--heading",
    "--break",
    "-t",
    "-r",
    "--abbrev",
    "--verify",
    "--symbolic",
    "--symbolic-full-name",
    "--abbrev-ref",
    "--show-toplevel",
    "--git-path",
    "--is-inside-work-tree",
    "--is-bare-repository",
    "--show-prefix",
    "--show-cdup",
    "--objectname-length",
    "--all-objects",
    "--type",
    "--objects-edge",
    "--max-count",
    "--skip",
    "--no-walk",
    "--simplify-by-decoration",
    "--topo-order",
    "--date-order",
    "--author-date-order",
];

/// Flag *prefixes* that are safe in their `--flag=value` form because the
/// value is a literal/format/limit, never a path or command. Selecting only
/// these prefixes (rather than allowing any `=`-bearing flag) keeps the
/// file-writing / command-spawning vectors closed.
const SAFE_PREFIXES: &[&str] = &[
    "--format=",
    "--pretty=",
    "--date=",
    "--abbrev=",
    "--decorate=",
    "--color=",
    "--word-diff=",
    "--unified=",
    "--diff-filter=",
    "--encoding=",
    "--max-count=",
    "--skip=",
    "--since=",
    "--after=",
    "--until=",
    "--before=",
    "--author=",
    "--committer=",
    "--grep=",
    "--max-depth=",
];

/// Flags that name a file/command or otherwise enable a write/exec path on an
/// otherwise read-only verb. Listed explicitly so the refusal message can be
/// precise; the allowlist above would already reject them.
const DANGEROUS_FLAGS: &[&str] = &[
    "-O",
    "--open-files-in-pager",
    "--output",
    "--output-indicator-new",
    "--output-indicator-old",
    "--output-indicator-context",
    "-o",
    "--ext-diff",
    "--textconv",
    "--no-textconv",
    "--exec",
    "--upload-pack",
    "--receive-pack",
    "--edit",
    "--delete",
    "--force",
    "--prune",
    "--write",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
];

fn pointer(verb: &str) -> &'static str {
    match verb {
        "commit" => "edit files and let `ctx watch` auto-commit",
        "checkout" | "switch" | "restore" | "reset" => "use `ctx restore <name|time>`",
        "tag" | "branch" => "use `ctx snapshot <name>` to mark a point",
        "gc" | "prune" => "GC is engine-internal",
        "fetch" | "pull" | "push" => "replication is the realtime transport, not git",
        _ => "the engine owns this repo; there is no write workflow through git",
    }
}

/// True if `arg` (a token that starts with `-`) is an allowed read-only flag.
fn flag_is_safe(arg: &str) -> bool {
    if SAFE_FLAGS.contains(&arg) {
        return true;
    }
    // Combined short read flags like `-pq` or `-rt` are safe only if every
    // letter is itself an allowed single-letter read flag.
    if arg.starts_with('-')
        && !arg.starts_with("--")
        && arg.len() > 2
        && !arg.contains('=')
    {
        let ok = arg[1..]
            .chars()
            .all(|c| SAFE_FLAGS.contains(&format!("-{c}").as_str()));
        if ok {
            return true;
        }
    }
    // `--flag=value` / `--flag value` long options: only specific prefixes
    // whose value is a literal (format/date/limit), never a path or command.
    for p in SAFE_PREFIXES {
        if let Some(name) = p.strip_suffix('=') {
            if arg == name || arg.starts_with(p) {
                return true;
            }
        } else if arg == *p {
            return true;
        }
    }
    false
}

/// Validate and run a `ctx git` invocation. Returns the child exit code.
pub fn run(git_dir: &Path, work_tree: &Path, args: &[String]) -> Result<i32> {
    validate(args)?;
    let status = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .arg(format!("--work-tree={}", work_tree.display()))
        .args(args)
        .status()?;
    Ok(status.code().unwrap_or(1))
}

/// Same guard as `run`, but capture stdout instead of inheriting it — used
/// by `ctx log --json` to post-process git output into machine-readable
/// form. The read-only allowlist is identical (it goes through `validate`).
pub fn run_captured(
    git_dir: &Path,
    work_tree: &Path,
    args: &[String],
) -> Result<(i32, String)> {
    validate(args)?;
    let out = Command::new("git")
        .arg(format!("--git-dir={}", git_dir.display()))
        .arg(format!("--work-tree={}", work_tree.display()))
        .args(args)
        .output()?;
    Ok((
        out.status.code().unwrap_or(1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    ))
}

/// The deny-by-default read-only guard, shared by `run`/`run_captured`.
fn validate(args: &[String]) -> Result<()> {
    // 1. The verb MUST be the very first token. Any leading global option
    //    (`-c key=val`, `-C <path>`, `--exec-path`, `--git-dir`,
    //    `--work-tree`, `--namespace`, alias injection, …) is refused before
    //    anything else: those rewrite where/how git runs and are a direct
    //    bypass of the engine-owned repo guard.
    let first = args
        .first()
        .ok_or_else(|| anyhow::anyhow!("ctx git: a read-only subcommand is required"))?;
    if first.starts_with('-') {
        bail!(
            "ctx git: leading option `{first}` is refused — the subcommand must come first \
             and global git options are not allowed (the engine owns this repo; there is no \
             write workflow through git). Allowed: {}",
            READ_ONLY.join(", ")
        );
    }
    let verb = first.clone();

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

    // 2. Every flag after the verb must be on the read-only allowlist.
    //    Unknown flags — and especially any `--flag=value` form that could
    //    name a file (`--output=`) or a command (`--open-files-in-pager=`,
    //    `-O<cmd>`, `-c core.pager=!cmd`) — are refused. Non-flag tokens
    //    (revisions, pathspecs) pass through unchanged. Everything after a
    //    bare `--` is a pathspec/operand, not a flag.
    let mut operands_only = false;
    for a in &args[1..] {
        if operands_only {
            continue;
        }
        if a == "--" {
            operands_only = true;
            continue;
        }
        if !a.starts_with('-') {
            continue;
        }
        // Name the well-known write/exec vectors precisely.
        let bare = a.split('=').next().unwrap_or(a);
        if DANGEROUS_FLAGS.contains(&bare)
            || (a.starts_with("-O") && a.len() > 2)
            || a.starts_with("--output")
        {
            bail!(
                "ctx git: flag `{a}` is refused — it can write a file, run a command, or \
                 redirect the repo, which is not permitted through the read-only `ctx git` \
                 passthrough (the engine owns this repo; there is no write workflow through \
                 git)"
            );
        }
        if !flag_is_safe(a) {
            bail!(
                "ctx git: flag `{a}` is refused — only an explicit allowlist of read-only \
                 flags is permitted through `ctx git` (the engine owns this repo; there is \
                 no write workflow through git)"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::run;
    use std::path::Path;

    /// Drive the guard without actually spawning git: every legitimate read
    /// is constructed so it would reach `Command`, every refusal returns an
    /// `Err` before that. We only assert on the refusal/allow decision, so
    /// for the "allowed" cases we point at a throwaway dir — git will fail to
    /// open it, but the guard has already let it through (that is the
    /// behavior under test).
    fn decide(args: &[&str]) -> Result<i32, String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        run(Path::new("/nonexistent/.git"), Path::new("/nonexistent"), &owned)
            .map_err(|e| e.to_string())
    }

    fn assert_refused(args: &[&str]) {
        match decide(args) {
            Ok(_) => panic!("expected refusal for {args:?}, but the guard allowed it"),
            Err(e) => assert!(
                e.contains("refused") || e.contains("permitted"),
                "refusal for {args:?} must carry a clear message, got: {e}"
            ),
        }
    }

    /// Allowed: the guard returns either Ok (git ran, even if it then failed
    /// against the throwaway dir → non-zero code) or a git-spawn error — but
    /// *never* one of our policy refusals.
    fn assert_allowed(args: &[&str]) {
        if let Err(e) = decide(args) {
            assert!(
                !(e.contains("refused") || e.contains("is not permitted") || e.contains("only `reflog show`")),
                "expected {args:?} to pass the guard, but it was refused: {e}"
            );
        }
    }

    #[test]
    fn leading_global_options_refused() {
        assert_refused(&["-c", "x=y", "log"]);
        assert_refused(&["-C", "/tmp", "log"]);
        assert_refused(&["--exec-path=/tmp", "log"]);
        assert_refused(&["--git-dir=/tmp", "log"]);
        assert_refused(&["--work-tree=/tmp", "status"]);
        assert_refused(&["--namespace=ns", "log"]);
        assert_refused(&["-c", "protocol.ext.allow=always", "log"]);
        assert_refused(&["--no-pager", "log"]);
    }

    #[test]
    fn file_writing_and_exec_flags_refused() {
        assert_refused(&["log", "--output=/tmp/x"]);
        assert_refused(&["show", "--output=/tmp/x"]);
        assert_refused(&["diff", "--output=/tmp/x"]);
        assert_refused(&["log", "--output-indicator-new=Z"]);
        assert_refused(&["grep", "-O/bin/sh", "foo"]);
        assert_refused(&["grep", "--open-files-in-pager=/bin/sh", "foo"]);
        assert_refused(&["diff", "--ext-diff"]);
        assert_refused(&["log", "--textconv"]);
        assert_refused(&["diff", "-o", "/tmp/x"]);
        assert_refused(&["log", "-O/tmp/order"]);
    }

    #[test]
    fn unknown_flags_refused() {
        assert_refused(&["log", "--totally-unknown"]);
        assert_refused(&["status", "--made-up=value"]);
    }

    #[test]
    fn every_mutating_verb_refused() {
        for v in [
            vec!["commit", "-m", "x"],
            vec!["checkout", "main"],
            vec!["switch", "main"],
            vec!["reset", "--hard"],
            vec!["merge", "other"],
            vec!["rebase", "main"],
            vec!["gc"],
            vec!["prune"],
            vec!["update-ref", "refs/heads/x", "HEAD"],
            vec!["apply", "p.patch"],
            vec!["cherry-pick", "HEAD"],
            vec!["restore", "f"],
            vec!["clean", "-fd"],
            vec!["stash"],
            vec!["fetch"],
            vec!["push"],
            vec!["filter-branch"],
            vec!["branch", "-d", "x"],
            vec!["tag", "-d", "x"],
            vec!["config", "user.x", "y"],
        ] {
            assert_refused(&v);
        }
    }

    #[test]
    fn reflog_restricted_to_show() {
        assert_refused(&["reflog"]);
        assert_refused(&["reflog", "expire"]);
        assert_refused(&["reflog", "delete", "HEAD@{0}"]);
        assert_allowed(&["reflog", "show"]);
    }

    #[test]
    fn legitimate_reads_pass() {
        assert_allowed(&["log", "--oneline", "-n", "5"]);
        assert_allowed(&["log", "--format=%s"]);
        assert_allowed(&["show", "HEAD"]);
        assert_allowed(&["status"]);
        assert_allowed(&["status", "--porcelain"]);
        assert_allowed(&["diff", "HEAD~1", "HEAD"]);
        assert_allowed(&["cat-file", "-p", "HEAD"]);
        assert_allowed(&["rev-parse", "HEAD"]);
        assert_allowed(&["for-each-ref"]);
        assert_allowed(&["reflog", "show"]);
        assert_allowed(&["log", "--", "some/path"]);
        assert_allowed(&["grep", "needle"]);
    }
}
