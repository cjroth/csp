//! `ctx` CLI surface. Single binary exposing the full engine capability set.
//! Every deployment knob is a global option with three forms — a flag, a
//! `CTX_*` env var, and a config-file key — resolved flag > env > config
//! (the one exception is the vault locator `--dir`/`CTX_DIR`, which cannot
//! have a config key because it locates the config file itself).
//!
//! Exit codes: 0 success; 2 usage error (argument parsing); 3 no vault at
//! the target (run `ctx init`/`ctx clone`); 1 any other error.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ctx",
    about = "Context Sync Protocol — keep a scoped file set byte-identical across devices",
    version
)]
pub struct Cli {
    /// Vault/scope root, decoupled from the process working directory.
    /// (Env renamed from the misleading CTX_CWD; this is not the process
    /// cwd. Mirrors git's GIT_DIR.) Flag+env only — it locates the config
    /// file, so it has no config-file key.
    #[arg(long, env = "CTX_DIR", global = true)]
    pub dir: Option<PathBuf>,

    /// Device identity key file (default: ~/.context/id_ed25519).
    #[arg(long, env = "CTX_IDENTITY", global = true)]
    pub identity: Option<PathBuf>,

    /// Log level / filter (config key: `log`).
    #[arg(long, env = "CTX_LOG", global = true)]
    pub log: Option<String>,

    /// Serve a plaintext ws:// listener instead of the default self-signed
    /// wss:// (config key: `no_tls`). `--no-tls` = true; `--no-tls=false`
    /// explicitly overrides a config `no_tls = true`.
    #[arg(long, env = "CTX_NO_TLS", global = true, num_args = 0..=1,
          require_equals = true, default_missing_value = "true",
          value_parser = parse_bool)]
    pub no_tls: Option<bool>,

    /// Disable trust-on-first-use entirely (config key: `no_tofu`).
    /// `--no-tofu=false` explicitly overrides a config `no_tofu = true`.
    #[arg(long, env = "CTX_NO_TOFU", global = true, num_args = 0..=1,
          require_equals = true, default_missing_value = "true",
          value_parser = parse_bool)]
    pub no_tofu: Option<bool>,

    /// Auto-commit debounce in milliseconds (config key: `debounce_ms`;
    /// default 1000). `--debounce-ms` is a hidden backward-compatible alias.
    #[arg(long, alias = "debounce-ms", env = "CTX_DEBOUNCE", global = true)]
    pub debounce: Option<u64>,

    /// Accept inbound peers / relay. Bare `--listen` binds 0.0.0.0:9000
    /// (unprivileged; deliberately not 443). Override with an explicit addr,
    /// `--port`, or `PORT`. Config key: `listen`.
    #[arg(long, num_args = 0..=1, default_missing_value = "0.0.0.0:9000",
          global = true)]
    pub listen: Option<String>,

    /// Listen port; overrides the port in `--listen`/config `listen`.
    /// Managed platforms inject `PORT`.
    #[arg(long, env = "PORT", global = true)]
    pub port: Option<u16>,

    /// Public keys (newline/comma-separated, or a file path) merged into
    /// this node's local authorized_keys on startup, idempotently.
    #[arg(long, env = "CTX_AUTHORIZED_KEYS", global = true)]
    pub authorized_keys: Option<String>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

/// Accept the usual truthy/falsy spellings for bool flags/env (so
/// `CTX_NO_TLS=1` and `--no-tls=false` both work as expected).
fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(format!("expected a boolean, got `{other}`")),
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create a new, empty scoped vault and this node's key identity here.
    Init {
        /// Where to init. An explicit path is created if missing (like
        /// `git init <dir>`); `.` = current dir. Most explicit, so it wins
        /// over the global `--dir`/`CTX_DIR`; omitted falls back to those,
        /// then the current dir.
        path: Option<PathBuf>,
        /// Opaque protocol id shared by all replicas. Default: a fresh
        /// UUID (override only to deliberately share a memorable id).
        #[arg(long)]
        vault_id: Option<String>,
        /// Human label. Default: the scope directory's name.
        #[arg(long)]
        name: Option<String>,
        /// After init, stay running as the sync daemon (= `ctx watch`).
        #[arg(long)]
        watch: bool,
    },
    /// Bootstrap a new node from an existing vault served by a listener.
    Clone {
        url: String,
        /// Where to clone. Default: `./<name-or-short-id>/`. `.` = current
        /// dir; an explicit path = that path.
        into: Option<PathBuf>,
        /// After cloning, stay running as the sync daemon (= `ctx watch`),
        /// reconnecting to the cloned origin.
        #[arg(long)]
        watch: bool,
    },
    /// The primary long-running command: watch the scoped tree, sync.
    Watch {
        /// A peer URL to connect to (repeatable).
        #[arg(long)]
        peer: Vec<String>,
        /// Run a single sync pass and exit (test/scripting aid).
        #[arg(long)]
        once: bool,
    },
    /// Generate / show the node SSH key; print the public key (OpenSSH).
    Key,
    /// Add a public key to this node's local authorized_keys.
    Authorize { pubkey: String },
    /// Remove a public key from this node's local authorized_keys.
    Revoke { pubkey: String },
    /// Node identity, peers, sync state, head/main SHA.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Create a named snapshot (exact, skew-free recovery point).
    Snapshot { name: String },
    /// Restore to a named snapshot or a time (unix secs or RFC-3339).
    Restore { target: String },
    /// History (read-only; wraps the engine-owned git log).
    Log {
        /// Machine-readable JSON (one object per commit).
        #[arg(long)]
        json: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Read-only git inspection of the engine-owned repo (deny-by-default).
    Git {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Show / edit the synced scope and .contextignore.
    Scope {
        /// Machine-readable JSON.
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<ScopeAction>,
    },
    /// Emit shell completion.
    Completions {
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScopeAction {
    /// Append a pattern to the synced .contextignore.
    Ignore { pattern: String },
    /// Add an allowlist include pattern to the vault config.
    Include { pattern: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ctx").chain(args.iter().copied()))
    }

    #[test]
    fn deployment_knobs_are_global_with_expected_precedence_shapes() {
        // Absent → None, so resolution falls through to config/default.
        let c = parse(&["status"]).unwrap();
        assert_eq!(c.no_tls, None);
        assert_eq!(c.no_tofu, None);
        assert_eq!(c.debounce, None);
        assert_eq!(c.listen, None);

        // Global: accepted before *or* after the subcommand.
        assert_eq!(parse(&["--debounce", "250", "status"]).unwrap().debounce, Some(250));
        assert_eq!(parse(&["watch", "--debounce", "250"]).unwrap().debounce, Some(250));
        // Hidden back-compat alias.
        assert_eq!(parse(&["watch", "--debounce-ms", "250"]).unwrap().debounce, Some(250));

        // Bool knobs: bare = true; explicit value overrides in both
        // directions; junk is rejected (not silently true).
        assert_eq!(parse(&["--no-tls", "status"]).unwrap().no_tls, Some(true));
        assert_eq!(parse(&["--no-tls=false", "status"]).unwrap().no_tls, Some(false));
        assert_eq!(parse(&["--no-tofu=1", "status"]).unwrap().no_tofu, Some(true));
        assert!(parse(&["--no-tls=maybe", "status"]).is_err());

        // listen: bare → unprivileged default; explicit addr wins.
        assert_eq!(
            parse(&["watch", "--listen"]).unwrap().listen.as_deref(),
            Some("0.0.0.0:9000")
        );
        assert_eq!(
            parse(&["--listen", "127.0.0.1:1", "status"]).unwrap().listen.as_deref(),
            Some("127.0.0.1:1")
        );
    }
}
