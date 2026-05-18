//! `ctx` CLI surface (§17). Single binary exposing the full engine
//! capability set. Every deployment knob has all three forms — a flag, a
//! `CTX_*` env var, and a config-file key — with precedence flag > env >
//! config (§17.1).

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ctx",
    about = "Context Sync Protocol — keep a scoped file set byte-identical across devices",
    version
)]
pub struct Cli {
    /// Vault/scope root, decoupled from the process working directory
    /// (§17.1). Always resolve to the persistent volume.
    #[arg(long, env = "CTX_CWD", global = true)]
    pub dir: Option<PathBuf>,

    /// Device identity key file (default: ~/.context/id_ed25519 — §9.1/§10).
    #[arg(long, env = "CTX_IDENTITY", global = true)]
    pub identity: Option<PathBuf>,

    /// Log level / filter.
    #[arg(long, env = "CTX_LOG", global = true)]
    pub log: Option<String>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create a new, empty scoped vault and this node's key identity here.
    Init {
        #[arg(long)]
        vault_id: Option<String>,
        #[arg(long, env = "CTX_AUTHORIZED_KEYS")]
        authorized_keys: Option<String>,
    },
    /// Bootstrap a new node from an existing vault served by a listener.
    Clone {
        url: String,
        /// Where to clone. Default: `./<vault-id>/`. `.` = current dir; an
        /// explicit path = that path. (Named `into` to avoid colliding with
        /// the global `--dir`/`CTX_CWD`.)
        into: Option<PathBuf>,
        #[arg(long, env = "CTX_AUTHORIZED_KEYS")]
        authorized_keys: Option<String>,
    },
    /// The primary long-running command: watch the scoped tree, sync.
    Watch {
        /// Accept inbound peers / relay (full nodes only — §7). Bare
        /// `--listen` binds 0.0.0.0:9000 (plaintext WS — TLS is terminated
        /// by a fronting proxy, §10; not 443, which implies TLS and is
        /// privileged). Override with an explicit addr, `--port`, or `PORT`.
        #[arg(long, num_args = 0..=1, default_missing_value = "0.0.0.0:9000")]
        listen: Option<String>,
        #[arg(long, env = "PORT")]
        port: Option<u16>,
        #[arg(long, env = "CTX_NO_TLS")]
        no_tls: bool,
        #[arg(long, env = "CTX_NO_TOFU")]
        no_tofu: bool,
        #[arg(long, env = "CTX_AUTHORIZED_KEYS")]
        authorized_keys: Option<String>,
        /// A peer URL to connect to (repeatable).
        #[arg(long)]
        peer: Vec<String>,
        #[arg(long, default_value_t = 1000)]
        debounce_ms: u64,
        /// Run a single sync pass and exit (test/scripting aid).
        #[arg(long)]
        once: bool,
    },
    /// Generate / show the node SSH key; print the public key (OpenSSH).
    Key,
    /// Add a public key to this node's local authorized_keys (§10).
    Authorize { pubkey: String },
    /// Remove a public key from this node's local authorized_keys (§10).
    Revoke { pubkey: String },
    /// Node identity, peers, sync state, head/main SHA.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Create a named snapshot (exact, skew-free recovery point — §8).
    Snapshot { name: String },
    /// Restore to a named snapshot or a time (unix secs / RFC-ish) (§8).
    Restore { target: String },
    /// History (read-only; wraps the engine-owned git log — §17).
    Log {
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
