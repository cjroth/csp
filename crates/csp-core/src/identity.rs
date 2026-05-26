//! Node identity & primitive-commit signing (§5.1, §5.2, §10). The NodeId is
//! an ed25519 public key (OpenSSH-formatted for `authorized_keys`). Every
//! primitive commit is signed by its author key; the signature lives **in
//! the object** (a `CSP-Signature` trailer) so the SHA is stable and the
//! commit replicates verbatim (§5.2). Synthetic fold commits are unsigned
//! (§5.4).

use crate::error::{CspError, CspResult};
use crate::object::{CommitObj, GitObject};
use crate::oid::Oid;
use crate::order::NodeId;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

#[derive(Clone)]
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn generate() -> Identity {
        use rand_core::OsRng;
        Identity { signing: SigningKey::generate(&mut OsRng) }
    }

    pub fn from_seed(seed: &[u8; 32]) -> Identity {
        Identity { signing: SigningKey::from_bytes(seed) }
    }

    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn node_id(&self) -> NodeId {
        NodeId(self.signing.verifying_key().to_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> Vec<u8> {
        self.signing.sign(msg).to_bytes().to_vec()
    }

    /// OpenSSH public-key line: `ssh-ed25519 <b64> <comment>` (§10).
    pub fn to_ssh_string(&self) -> String {
        ssh_pubkey_string(&self.node_id(), "csp")
    }
}

/// OpenSSH `ssh-ed25519` public key wire format, base64-encoded (§10).
pub fn ssh_pubkey_string(node: &NodeId, comment: &str) -> String {
    let mut blob = Vec::new();
    let algo = b"ssh-ed25519";
    blob.extend_from_slice(&(algo.len() as u32).to_be_bytes());
    blob.extend_from_slice(algo);
    blob.extend_from_slice(&(node.0.len() as u32).to_be_bytes());
    blob.extend_from_slice(&node.0);
    format!(
        "ssh-ed25519 {} {}",
        base64::engine::general_purpose::STANDARD.encode(&blob),
        comment
    )
}

/// Parse an OpenSSH `ssh-ed25519 <b64> [comment]` line back to a NodeId.
pub fn parse_ssh_pubkey(line: &str) -> Option<NodeId> {
    let mut it = line.split_whitespace();
    if it.next()? != "ssh-ed25519" {
        return None;
    }
    let blob = base64::engine::general_purpose::STANDARD
        .decode(it.next()?)
        .ok()?;
    // u32 len + "ssh-ed25519" + u32 len + 32-byte key
    let n = u32::from_be_bytes(blob.get(0..4)?.try_into().ok()?) as usize;
    let key_off = 4 + n + 4;
    let key = blob.get(key_off..key_off + 32)?;
    let mut a = [0u8; 32];
    a.copy_from_slice(key);
    Some(NodeId(a))
}

fn verifying_key(node: &NodeId) -> CspResult<VerifyingKey> {
    VerifyingKey::from_bytes(&node.0).map_err(|e| CspError::BadSignature(e.to_string()))
}

/// Verify a detached ed25519 signature (handshake transcript auth, §10).
pub fn verify_detached(node: &NodeId, msg: &[u8], sig: &[u8]) -> CspResult<()> {
    let sig = Signature::from_slice(sig).map_err(|e| CspError::BadSignature(e.to_string()))?;
    verifying_key(node)?
        .verify(msg, &sig)
        .map_err(|e| CspError::BadSignature(e.to_string()))
}

const SIG_TRAILER: &str = "CSP-Signature: ";
/// Issue 0014: `CSP-Readd: <delete-prim-oid>` trailer naming the §5.1
/// most-recent delete in this primitive's closure that the publisher is
/// genuinely re-adding. Emitted automatically by Layer 1 when
/// `state.materialized` confirmed the path is fresh user intent; consumed by
/// Layer 3's integrate-time filter to exempt the primitive from drop.
pub const READD_TRAILER: &str = "CSP-Readd: ";

/// Build a **signed** primitive commit (§5.2). Parent is the synthetic fold
/// commit the author held. The signature covers the entire pre-signature
/// commit payload and is then folded into the object as the final trailer,
/// so the resulting SHA is stable and the object replicates verbatim.
#[allow(clippy::too_many_arguments)]
pub fn build_primitive(
    id: &Identity,
    tree: Oid,
    parent: Oid,
    counter: u64,
    wall_time: u64,
    subject: &str,
) -> GitObject {
    build_primitive_with_readd(id, tree, parent, counter, wall_time, subject, &[])
}

/// Like [`build_primitive`] but also emits one `CSP-Readd: <delete-prim-oid>`
/// trailer per entry in `readds` (issue 0014). The trailer is engine-emitted
/// (Layer 1 detects the legitimate re-add of a previously-deleted path) and
/// load-bearing for Layer 3 integrate-time filtering — primitives that re-add
/// a path whose closure contains a delete are dropped unless they carry a
/// trailer naming that delete. Under the trust model the trailer is a
/// structural signal, not an authenticated one (no separate signing — it is
/// covered by the ed25519 signature over the commit payload like every other
/// trailer).
#[allow(clippy::too_many_arguments)]
pub fn build_primitive_with_readd(
    id: &Identity,
    tree: Oid,
    parent: Oid,
    counter: u64,
    wall_time: u64,
    subject: &str,
    readds: &[Oid],
) -> GitObject {
    let node = id.node_id();
    let mut body = format!(
        "{subject}\n\nCSP-Counter: {counter}\nCSP-Node: {}\n",
        node.to_hex()
    );
    for r in readds {
        body.push_str(&format!("{READD_TRAILER}{}\n", r.to_hex()));
    }
    let unsigned = CommitObj {
        tree,
        parents: vec![parent],
        author: node.to_hex(),
        author_email: format!("{}@csp", &node.to_hex()[..16]),
        author_time: wall_time,
        committer: node.to_hex(),
        committer_email: format!("{}@csp", &node.to_hex()[..16]),
        committer_time: wall_time,
        message: body,
    };
    let payload = GitObject::Commit(unsigned.clone()).payload();
    let sig = base64::engine::general_purpose::STANDARD.encode(id.sign(&payload));
    let mut signed = unsigned;
    signed.message = format!("{}{SIG_TRAILER}{sig}\n", signed.message);
    GitObject::Commit(signed)
}

/// Verify a primitive commit's in-object signature against its `CSP-Node`
/// author key (§6.3/§10). Returns the authoring NodeId on success.
pub fn verify_primitive(c: &CommitObj) -> CspResult<NodeId> {
    let (_counter, node) = crate::fold::parse_primitive_meta(c)
        .ok_or_else(|| CspError::BadSignature("not a primitive (no CSP-Node)".into()))?;
    // Split the message at the final CSP-Signature trailer.
    let sig_line = c
        .message
        .lines()
        .rev()
        .find(|l| l.starts_with(SIG_TRAILER))
        .ok_or_else(|| CspError::BadSignature("missing CSP-Signature".into()))?;
    let sig_b64 = &sig_line[SIG_TRAILER.len()..];
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|e| CspError::BadSignature(e.to_string()))?;
    let sig = Signature::from_slice(&sig_bytes)
        .map_err(|e| CspError::BadSignature(e.to_string()))?;
    // Reconstruct the pre-signature payload (message with the trailer line
    // and its preceding newline removed).
    let needle = format!("{SIG_TRAILER}{}\n", sig_b64);
    let pre_msg = c
        .message
        .strip_suffix(&needle)
        .ok_or_else(|| CspError::BadSignature("signature not final trailer".into()))?;
    let unsigned = CommitObj { message: pre_msg.to_string(), ..c.clone() };
    let payload = GitObject::Commit(unsigned).payload();
    let vk = verifying_key(&node)?;
    vk.verify(&payload, &sig)
        .map_err(|e| CspError::BadSignature(e.to_string()))?;
    Ok(node)
}
