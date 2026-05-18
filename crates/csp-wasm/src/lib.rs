//! WebAssembly bindings for the **reduced thin-node surface** (§4, §7,
//! §13.2 wasm spike): object encode/decode, identity/auth, and wire framing
//! — **no 3-way merge, no on-disk odb/packfiles**. This is a thin typed
//! surface over the *one* Rust implementation (`csp-core`), never a
//! reimplementation (§16). A thin node speaks the protocol and authors
//! signed primitives; it never computes the multi-tip merge.

use csp_core::identity::{build_primitive, ssh_pubkey_string, verify_primitive, Identity};
use csp_core::object::GitObject;
use csp_core::oid::Oid;
use csp_core::wire::Msg;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() {
    #[cfg(target_arch = "wasm32")]
    console_error_panic_hook::set_once();
}

fn seed32(seed: &[u8]) -> Result<[u8; 32], JsError> {
    if seed.len() != 32 {
        return Err(JsError::new("seed must be 32 bytes"));
    }
    let mut a = [0u8; 32];
    a.copy_from_slice(seed);
    Ok(a)
}

/// NodeId (ed25519 public key) hex for a seed.
#[wasm_bindgen]
pub fn node_id_hex(seed: &[u8]) -> Result<String, JsError> {
    Ok(Identity::from_seed(&seed32(seed)?).node_id().to_hex())
}

/// OpenSSH `ssh-ed25519 …` public key line for `authorized_keys` (§10).
#[wasm_bindgen]
pub fn ssh_pubkey(seed: &[u8], comment: &str) -> Result<String, JsError> {
    let id = Identity::from_seed(&seed32(seed)?);
    Ok(ssh_pubkey_string(&id.node_id(), comment))
}

/// SHA-1 object id of raw blob bytes (stock-git identical — §4).
#[wasm_bindgen]
pub fn blob_oid(content: &[u8]) -> String {
    GitObject::Blob(content.to_vec()).oid().to_hex()
}

/// Build a **signed primitive commit** (§5.2) and return its framed object
/// bytes. Authored once; the signature is in the object so the SHA is
/// stable and it replicates verbatim — identical to what a native node
/// produces (cross-surface guarantee, §18).
#[wasm_bindgen]
pub fn build_primitive_object(
    seed: &[u8],
    tree_hex: &str,
    parent_hex: &str,
    counter: u64,
    wall_time: u64,
    subject: &str,
) -> Result<Vec<u8>, JsError> {
    let id = Identity::from_seed(&seed32(seed)?);
    let tree = Oid::from_hex(tree_hex).map_err(|e| JsError::new(&e.to_string()))?;
    let parent = Oid::from_hex(parent_hex).map_err(|e| JsError::new(&e.to_string()))?;
    let obj = build_primitive(&id, tree, parent, counter, wall_time, subject);
    Ok(obj.framed())
}

/// The oid of the framed object bytes.
#[wasm_bindgen]
pub fn object_oid(framed: &[u8]) -> Result<String, JsError> {
    let o = GitObject::parse_framed(framed).map_err(|e| JsError::new(&e.to_string()))?;
    Ok(o.oid().to_hex())
}

/// Verify a primitive's in-object signature; returns the author NodeId hex
/// or throws (§6.3/§10).
#[wasm_bindgen]
pub fn verify_primitive_object(framed: &[u8]) -> Result<String, JsError> {
    let obj = GitObject::parse_framed(framed).map_err(|e| JsError::new(&e.to_string()))?;
    match obj {
        GitObject::Commit(c) => verify_primitive(&c)
            .map(|n| n.to_hex())
            .map_err(|e| JsError::new(&e.to_string())),
        _ => Err(JsError::new("not a commit object")),
    }
}

/// MessagePack-encode a wire message given as JSON (framing, §6.2/§6.6).
#[wasm_bindgen]
pub fn wire_encode(json: &str) -> Result<Vec<u8>, JsError> {
    let msg: Msg = serde_json::from_str(json).map_err(|e| JsError::new(&e.to_string()))?;
    msg.encode().map_err(|e| JsError::new(&e))
}

/// Decode a MessagePack wire frame back to JSON.
#[wasm_bindgen]
pub fn wire_decode(bytes: &[u8]) -> Result<String, JsError> {
    let msg = Msg::decode(bytes).map_err(|e| JsError::new(&e))?;
    serde_json::to_string(&msg).map_err(|e| JsError::new(&e.to_string()))
}
