//! Device identity key storage (§9.1, §10). The private key is
//! **device-global by default** — `~/.context/id_ed25519` — *not* inside a
//! vault's `.context/`: one device may join several vaults with one key, and
//! the key must survive deleting a vault's `.context/`. A per-vault key is an
//! opt-in via `--identity` / `CTX_IDENTITY`. The file stores the 32-byte
//! ed25519 seed as hex (private; mode 0600).

use anyhow::{Context, Result};
use csp_core::Identity;
use std::path::{Path, PathBuf};

pub fn default_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".context").join("id_ed25519")
}

pub fn load_or_create(explicit: Option<&Path>) -> Result<(Identity, PathBuf)> {
    let path = explicit.map(|p| p.to_path_buf()).unwrap_or_else(default_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    if path.exists() {
        let hex = std::fs::read_to_string(&path)
            .with_context(|| format!("read identity {}", path.display()))?;
        let bytes = hex::decode(hex.trim()).context("identity hex")?;
        anyhow::ensure!(bytes.len() == 32, "identity must be a 32-byte seed");
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Ok((Identity::from_seed(&seed), path))
    } else {
        let id = Identity::generate();
        std::fs::write(&path, hex::encode(id.seed()))
            .with_context(|| format!("write identity {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok((id, path))
    }
}
