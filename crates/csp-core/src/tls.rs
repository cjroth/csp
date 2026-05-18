//! Self-signed TLS for the WSS transport (§10/§17.1). The listener
//! generates (and persists, under the never-synced `.context/`) a
//! self-signed cert; clients accept **any** cert at the TLS layer. CSP
//! ships **no certificate authority** (§10) — transport trust is *not* the
//! security boundary. Authentication is the ed25519 mutual-auth handshake
//! (§10), which binds the channel; TLS adds confidentiality only.
//! `--no-tls` (behind a TLS-terminating proxy / local) skips this entirely.

#![cfg(all(not(target_arch = "wasm32"), feature = "full"))]

use crate::error::{CspError, CspResult};
use rcgen::{CertificateParams, DistinguishedName, KeyPair};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};
use std::path::Path;
use std::sync::Arc;

pub const CERT_FILE: &str = "tls.crt";
pub const KEY_FILE: &str = "tls.key";

fn other<E: std::fmt::Display>(e: E) -> CspError {
    CspError::Protocol(format!("tls: {e}"))
}

/// Load a persisted DER cert/key from `context_dir` (`.context/`, never
/// synced — §11), or generate + persist a fresh self-signed pair.
pub fn load_or_generate(context_dir: &Path) -> CspResult<(Vec<u8>, Vec<u8>)> {
    let crt = context_dir.join(CERT_FILE);
    let key = context_dir.join(KEY_FILE);
    if crt.exists() && key.exists() {
        return Ok((std::fs::read(&crt)?, std::fs::read(&key)?));
    }
    let (c, k) = generate_self_signed()?;
    std::fs::create_dir_all(context_dir)?;
    write_private(&key, &k)?;
    std::fs::write(&crt, &c)?;
    Ok((c, k))
}

/// Fresh ed25519 self-signed cert. Returns DER-encoded `(cert, key)`.
pub fn generate_self_signed() -> CspResult<(Vec<u8>, Vec<u8>)> {
    let mut params =
        CertificateParams::new(vec!["csp.local".to_string()]).map_err(other)?;
    params.distinguished_name = DistinguishedName::new();
    let now = std::time::SystemTime::now();
    params.not_before = now.into();
    params.not_after =
        (now + std::time::Duration::from_secs(10 * 365 * 24 * 3600)).into();
    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519).map_err(other)?;
    let cert = params.self_signed(&key_pair).map_err(other)?;
    Ok((cert.der().to_vec(), key_pair.serialize_der()))
}

fn install_provider() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// SHA-256 of the cert DER. Bound into the handshake transcript (§10) so a
/// relayed MITM that re-terminates TLS presents a different cert → a
/// different fingerprint → the signed transcripts no longer match.
pub fn cert_fingerprint(cert_der: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(cert_der);
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&h.finalize());
    fp
}

pub fn server_config(cert_der: Vec<u8>, key_der: Vec<u8>) -> CspResult<Arc<ServerConfig>> {
    install_provider();
    let chain = vec![CertificateDer::from(cert_der)];
    let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key_der));
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(other)?;
    Ok(Arc::new(cfg))
}

/// Client config that accepts any server cert. Safe here: the trust
/// decision is the application-layer ed25519 handshake (§10), not X.509.
pub fn client_config_accept_any() -> Arc<ClientConfig> {
    install_provider();
    Arc::new(
        ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth(),
    )
}

#[derive(Debug)]
struct AcceptAny;

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _e: &CertificateDer<'_>,
        _i: &[CertificateDer<'_>],
        _n: &ServerName<'_>,
        _o: &[u8],
        _t: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &CertificateDer<'_>,
        _d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _m: &[u8],
        _c: &CertificateDer<'_>,
        _d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ED25519,
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> CspResult<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> CspResult<()> {
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_generates_and_persists() {
        let td = tempfile::tempdir().unwrap();
        let (c1, k1) = load_or_generate(td.path()).unwrap();
        assert!(!c1.is_empty() && !k1.is_empty());
        // Second call loads the SAME persisted pair.
        let (c2, _k2) = load_or_generate(td.path()).unwrap();
        assert_eq!(c1, c2);
        let _ = server_config(c1, k1).unwrap();
        let _ = client_config_accept_any();
    }
}
