//! Device identity key storage. The private key is
//! **device-global by default** — `~/.context/id_ed25519` — *not* inside a
//! vault's `.context/`: one device may join several vaults with one key, and
//! the key must survive deleting a vault's `.context/`. A per-vault key is an
//! opt-in via `--identity` / `CTX_IDENTITY`.
//!
//! Three on-disk forms are accepted, detected by content:
//!
//!   * the native form — the 32-byte ed25519 seed as hex (mode 0600),
//!     written by `ctx` when it generates its own key;
//!   * a reused OpenSSH-format ed25519 private key (the armored
//!     `-----BEGIN OPENSSH PRIVATE KEY-----` block, e.g. a user's
//!     `~/.ssh/id_ed25519`). If it is unencrypted the seed is derived from
//!     it and the rest of the engine runs unchanged. If it is encrypted we
//!     refuse and point the operator at the agent, because we will not
//!     prompt for a passphrase;
//!   * the same key reused via a running SSH agent — the operator points
//!     `--identity` at the matching OpenSSH *public* key (or sets
//!     `CTX_SSH_AGENT=1` to pick the agent's sole ed25519 key) so the
//!     private key never enters this process.

use crate::sshagent::{ed25519_pubkey_from_blob, Agent};
use anyhow::{bail, Context, Result};
use csp_core::Identity;
use std::path::{Path, PathBuf};

pub fn default_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".context").join("id_ed25519")
}

/// What does the node sign handshakes / primitive commits with?
///
/// Most of the engine wants an owned [`Identity`] (the `Vault` holds one and
/// signs in-process), so [`InProcess`](Signer::InProcess) is the path that
/// flows everywhere today. [`Agent`](Signer::Agent) carries the public
/// identity plus a live handle to the SSH agent that holds the matching
/// private key, so a future signing seam can delegate without ever seeing
/// the secret. See [`Signer::identity`] for why the agent path cannot reach
/// every signing site yet.
pub enum Signer {
    /// The seed is in process; signing is the existing in-process ed25519.
    InProcess(Identity),
    /// The private key stays in the SSH agent; we only hold the public key
    /// and the agent handle. `key_blob` is the agent's OpenSSH key blob,
    /// used verbatim in sign requests. `key_blob`/`agent` are only consumed
    /// by the agent-backed `sign` path, which is complete but not yet
    /// reached from the engine (see [`Signer::identity`]); kept so wiring
    /// that seam later is a local change.
    #[allow(dead_code)]
    Agent {
        node: csp_core::order::NodeId,
        key_blob: Vec<u8>,
        agent: Agent,
    },
}

// Public-only Debug: the in-process backing wraps a secret seed (and
// `Identity` is intentionally not `Debug`), so we print just the node's
// public key and which backing is in use — enough for test failures /
// diagnostics, nothing sensitive.
impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Signer::InProcess(_) => "in-process",
            Signer::Agent { .. } => "ssh-agent",
        };
        write!(f, "Signer({kind}, {})", self.node_id().to_hex())
    }
}

impl Signer {
    /// Detached ed25519 signature over `msg`, by whichever backing this
    /// signer wraps. For the agent backing this is a round-trip to the
    /// agent socket; the returned bytes are the bare 64-byte signature, the
    /// same shape the in-process signer produces, so it verifies against the
    /// advertised node key with the engine's existing verifier.
    ///
    /// Not yet reached from the engine (it takes an owned in-process
    /// `Identity`); kept and tested so the agent seam is a local change.
    #[allow(dead_code)]
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>> {
        match self {
            Signer::InProcess(id) => Ok(id.sign(msg)),
            Signer::Agent { key_blob, agent, .. } => agent.sign(key_blob, msg),
        }
    }

    /// The node's public identity (stable across both backings).
    pub fn node_id(&self) -> csp_core::order::NodeId {
        match self {
            Signer::InProcess(id) => id.node_id(),
            Signer::Agent { node, .. } => *node,
        }
    }

    /// OpenSSH public-key line for this node.
    pub fn to_ssh_string(&self) -> String {
        match self {
            Signer::InProcess(id) => id.to_ssh_string(),
            Signer::Agent { node, .. } => {
                csp_core::identity::ssh_pubkey_string(node, "csp")
            }
        }
    }

    /// Hand back an owned [`Identity`] for the parts of the engine that
    /// still require one (the `Vault` constructors, which own the key and
    /// sign in-process from it).
    ///
    /// This is exactly the seam where SSH-agent delegation stops today: the
    /// engine's `Vault` takes an owned `Identity` by value and signs both
    /// handshake transcripts and primitive commits synchronously and
    /// infallibly from it, deep inside the sans-IO session and the commit
    /// path. Threading the out-of-process, fallible agent signer through
    /// those would mean changing the `Vault`/`build_primitive`/`SessionVault`
    /// signatures across the engine crate, which is out of scope here. So an
    /// agent-backed signer can sign on its own (used directly where `ctx`
    /// owns the call), but it cannot satisfy this owned-`Identity` request
    /// and fails loudly rather than silently degrading.
    pub fn identity(&self) -> Result<Identity> {
        match self {
            Signer::InProcess(id) => Ok(id.clone()),
            Signer::Agent { .. } => bail!(
                "this key is held by the SSH agent; the engine still needs \
                 an in-process key to sign commits, so reuse the OpenSSH \
                 private key directly (unencrypted) or `ctx`'s own key"
            ),
        }
    }
}

/// Detect the on-disk form by content and produce a [`Signer`]. Creates a
/// fresh native key only when nothing exists at the resolved path.
pub fn load_or_create(explicit: Option<&Path>) -> Result<(Signer, PathBuf)> {
    let path = explicit.map(|p| p.to_path_buf()).unwrap_or_else(default_path);

    // Explicit opt-in: use the SSH agent's ed25519 key, no key file needed.
    if std::env::var("CTX_SSH_AGENT").map(|v| v != "0" && !v.is_empty()).unwrap_or(false) {
        let signer = agent_signer(explicit)?;
        return Ok((signer, path));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    if path.exists() {
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("read identity {}", path.display()))?;
        let signer = parse_identity_file(&body, explicit)?;
        Ok((signer, path))
    } else {
        let id = Identity::generate();
        std::fs::write(&path, hex::encode(id.seed()))
            .with_context(|| format!("write identity {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok((Signer::InProcess(id), path))
    }
}

/// Decide what `body` (the contents of the identity path) is and turn it
/// into a [`Signer`]. `explicit` is the path the operator pointed at, used
/// for clearer messages and to match an agent-held public key.
fn parse_identity_file(body: &str, explicit: Option<&Path>) -> Result<Signer> {
    let trimmed = body.trim();

    if trimmed.contains("BEGIN OPENSSH PRIVATE KEY") {
        return openssh_private_signer(body);
    }

    // An OpenSSH *public* key line (`ssh-ed25519 AAAA... [comment]`): the
    // private half is not here, so it must be in the agent.
    if trimmed.starts_with("ssh-ed25519 ") {
        return public_key_agent_signer(trimmed, explicit);
    }

    // Back-compat: the native bare-hex 32-byte seed.
    let bytes = hex::decode(trimmed).context("identity hex")?;
    anyhow::ensure!(bytes.len() == 32, "identity must be a 32-byte seed");
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&bytes);
    Ok(Signer::InProcess(Identity::from_seed(&seed)))
}

/// Reuse an armored OpenSSH ed25519 private key: derive its 32-byte seed so
/// the in-process engine runs unchanged. Encrypted keys are refused with a
/// pointer to the agent (we never prompt for a passphrase).
fn openssh_private_signer(armored: &str) -> Result<Signer> {
    use ssh_key::PrivateKey;
    let key = PrivateKey::from_openssh(armored.trim())
        .context("parse OpenSSH private key")?;
    if key.is_encrypted() {
        bail!(
            "the OpenSSH key is passphrase-encrypted; `ctx` will not prompt \
             for it. Load it into your SSH agent (`ssh-add <key>`) and run \
             with that key's public key as the identity, or set \
             CTX_SSH_AGENT=1"
        );
    }
    let kp = key
        .key_data()
        .ed25519()
        .context("the OpenSSH key is not ed25519 (CSP node keys are ed25519)")?;
    let seed = kp.private.to_bytes();
    Ok(Signer::InProcess(Identity::from_seed(&seed)))
}

/// The identity file is an OpenSSH *public* key — the private key lives in
/// the agent. Match it to one of the agent's loaded ed25519 keys.
fn public_key_agent_signer(pubkey_line: &str, _explicit: Option<&Path>) -> Result<Signer> {
    let node = csp_core::identity::parse_ssh_pubkey(pubkey_line)
        .context("parse OpenSSH public key line")?;
    let agent = Agent::from_env().context(
        "identity is an OpenSSH public key (private key not present) but no \
         SSH agent is running (SSH_AUTH_SOCK is unset)",
    )?;
    let blob = agent
        .ed25519_key_blobs()?
        .into_iter()
        .find(|b| ed25519_pubkey_from_blob(b).map(|pk| pk == node.0).unwrap_or(false))
        .context(
            "the SSH agent does not hold the private key for this public \
             key (run `ssh-add` for it)",
        )?;
    Ok(Signer::Agent { node, key_blob: blob, agent })
}

/// `CTX_SSH_AGENT=1` with no specific public key: use the agent's sole
/// ed25519 key (error if it holds zero or more than one, so the choice is
/// never ambiguous).
fn agent_signer(_explicit: Option<&Path>) -> Result<Signer> {
    let agent = Agent::from_env().context(
        "CTX_SSH_AGENT is set but no SSH agent is running (SSH_AUTH_SOCK \
         is unset)",
    )?;
    let mut blobs = agent.ed25519_key_blobs()?;
    match blobs.len() {
        0 => bail!("the SSH agent holds no ed25519 keys (`ssh-add` one)"),
        1 => {
            let blob = blobs.pop().unwrap();
            let pk = ed25519_pubkey_from_blob(&blob)?;
            Ok(Signer::Agent {
                node: csp_core::order::NodeId(pk),
                key_blob: blob,
                agent,
            })
        }
        n => bail!(
            "the SSH agent holds {n} ed25519 keys; point --identity / \
             CTX_IDENTITY at the OpenSSH public key you want so the choice \
             is unambiguous"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, contents: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(contents).unwrap();
        (dir, p)
    }

    /// An unencrypted OpenSSH ed25519 private key round-trips to the same
    /// NodeId / public key as the equivalent raw-hex seed.
    #[test]
    fn openssh_private_key_matches_raw_seed() {
        // ssh-keygen-equivalent fixture generated once via `ssh-key`.
        use ssh_key::private::{Ed25519Keypair, KeypairData, PrivateKey};
        use ssh_key::rand_core::OsRng;
        let kp = Ed25519Keypair::random(&mut OsRng);
        let seed = kp.private.to_bytes();
        let pk = PrivateKey::new(KeypairData::Ed25519(kp), "fixture").unwrap();
        let armored = pk.to_openssh(ssh_key::LineEnding::LF).unwrap();

        let (_d, path) = write_tmp("id_ed25519", armored.as_bytes());
        let body = std::fs::read_to_string(&path).unwrap();
        let signer = parse_identity_file(&body, Some(&path)).unwrap();

        let expected = Identity::from_seed(&seed);
        assert_eq!(signer.node_id(), expected.node_id());
        assert_eq!(signer.to_ssh_string(), expected.to_ssh_string());

        // And it actually signs with that key.
        let sig = signer.sign(b"transcript").unwrap();
        csp_core::identity::verify_detached(&signer.node_id(), b"transcript", &sig)
            .expect("agent-shaped signature must verify against node key");
    }

    /// The legacy bare-hex seed file still loads (back-compat).
    #[test]
    fn bare_hex_seed_still_loads() {
        let seed = [7u8; 32];
        let (_d, path) = write_tmp("id_ed25519", hex::encode(seed).as_bytes());
        let body = std::fs::read_to_string(&path).unwrap();
        let signer = parse_identity_file(&body, Some(&path)).unwrap();
        assert_eq!(signer.node_id(), Identity::from_seed(&seed).node_id());
        // Trailing whitespace/newline must not break it.
        let (_d2, path2) =
            write_tmp("id2", format!("{}\n", hex::encode(seed)).as_bytes());
        let body2 = std::fs::read_to_string(&path2).unwrap();
        assert_eq!(
            parse_identity_file(&body2, Some(&path2)).unwrap().node_id(),
            Identity::from_seed(&seed).node_id()
        );
    }

    /// A passphrase-encrypted OpenSSH key is refused with a clear message
    /// that points at the agent (no passphrase prompt). Fixture is a real
    /// `ssh-keygen -t ed25519 -N <pw>` key (we only need the `std`+`ed25519`
    /// features to *detect* encryption, not the `encryption` feature to
    /// decrypt it).
    #[test]
    fn encrypted_openssh_key_errors_clearly() {
        const ENCRYPTED: &str = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABA9C8IpfF
is+81H2Ny7Gog9AAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAILkUkItKmtFWz9li
bxIFsuHisiEp7QSib0EE5kzLRbD1AAAAkNVt8HrNDTL1+7OdutTfe9AahEejruZHk02szs
h/9pMbrjOtr8kK1wypO7mkQ6A3B/2Z26O5wZOT7nMgyVlmhVmv8w6cNtI7AyVmFFOltEap
TiSZIcrrGyNC3RyI95sEkbinFifbfmD+ozhnlL/INNdWiHhvItiU1lqB0LZhxgtQPo9R8B
PGOTApennYIGgoeQ==
-----END OPENSSH PRIVATE KEY-----
";
        let (_d, path) = write_tmp("id_ed25519", ENCRYPTED.as_bytes());
        let body = std::fs::read_to_string(&path).unwrap();
        let err = parse_identity_file(&body, Some(&path)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("encrypted") && msg.contains("agent"),
            "message should explain encryption + point at the agent: {msg}"
        );
    }

    /// An agent-backed signer cannot hand back an owned in-process
    /// `Identity` (that is the documented seam where delegation stops) and
    /// must fail loudly rather than degrade.
    #[test]
    fn agent_signer_refuses_in_process_identity() {
        // Constructed without a live socket; `identity()` never touches it.
        let signer = Signer::Agent {
            node: csp_core::order::NodeId([3u8; 32]),
            key_blob: Vec::new(),
            agent: Agent::for_test("/nonexistent.sock"),
        };
        let err = match signer.identity() {
            Ok(_) => panic!("agent-backed signer must not yield an Identity"),
            Err(e) => e,
        };
        assert!(format!("{err:#}").contains("SSH agent"));
        // The public identity is still available for display/auth.
        assert_eq!(signer.node_id(), csp_core::order::NodeId([3u8; 32]));
    }

    /// End-to-end through a real `ssh-agent`: spawn one, load a fresh
    /// ed25519 key, drive the hand-rolled agent client + agent-backed
    /// `Signer`, and confirm the agent's signature verifies against the
    /// advertised public key with the engine's own verifier. Skipped (not
    /// failed) where `ssh-agent`/`ssh-keygen` are unavailable so CI without
    /// them stays green.
    #[test]
    fn agent_signing_verifies_end_to_end() {
        use std::process::Command;
        let have = |b: &str| {
            Command::new(b)
                .arg("--version")
                .output()
                .map(|_| true)
                .unwrap_or(false)
        };
        // ssh-agent has no --version; probe via `-h` exit instead.
        let agent_ok = Command::new("ssh-agent").arg("-c").output().is_ok();
        if !agent_ok || !have("ssh-keygen") || !have("ssh-add") {
            eprintln!("skipping: ssh-agent/ssh-keygen/ssh-add not available");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let keyfile = dir.path().join("id_ed25519");

        let kg = Command::new("ssh-keygen")
            .args(["-t", "ed25519", "-N", "", "-C", "csp-test", "-f"])
            .arg(&keyfile)
            .output()
            .unwrap();
        assert!(kg.status.success(), "ssh-keygen: {kg:?}");

        // Start an agent, capture its SSH_AUTH_SOCK.
        let out = Command::new("ssh-agent").arg("-s").output().unwrap();
        let sh = String::from_utf8_lossy(&out.stdout);
        let sock = sh
            .lines()
            .find_map(|l| l.strip_prefix("SSH_AUTH_SOCK="))
            .and_then(|v| v.split(';').next())
            .expect("agent printed SSH_AUTH_SOCK")
            .to_string();
        let pid = sh
            .lines()
            .find_map(|l| l.strip_prefix("SSH_AGENT_PID="))
            .and_then(|v| v.split(';').next())
            .map(|s| s.to_string());

        let add = Command::new("ssh-add")
            .arg(&keyfile)
            .env("SSH_AUTH_SOCK", &sock)
            .output()
            .unwrap();
        assert!(add.status.success(), "ssh-add: {add:?}");

        // Build the agent-backed signer straight from the agent (the path
        // `CTX_SSH_AGENT=1` takes), sign, and verify.
        let agent = Agent::for_test(&sock);
        let blobs = agent.ed25519_key_blobs().unwrap();
        assert_eq!(blobs.len(), 1, "exactly the key we added");
        let pk = ed25519_pubkey_from_blob(&blobs[0]).unwrap();
        let signer = Signer::Agent {
            node: csp_core::order::NodeId(pk),
            key_blob: blobs[0].clone(),
            agent,
        };
        let msg = b"handshake transcript bytes";
        let sig = signer.sign(msg).expect("agent signs");
        csp_core::identity::verify_detached(&signer.node_id(), msg, &sig)
            .expect("agent signature must verify against advertised pubkey");

        // The advertised pubkey also matches the on-disk public key file.
        let publine = std::fs::read_to_string(keyfile.with_extension("pub"))
            .unwrap();
        let from_file =
            csp_core::identity::parse_ssh_pubkey(publine.trim()).unwrap();
        assert_eq!(signer.node_id(), from_file);

        if let Some(pid) = pid {
            let _ = Command::new("kill").arg(pid).output();
        }
    }
}
