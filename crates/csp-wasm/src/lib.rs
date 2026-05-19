//! WebAssembly bindings for csp-core. **One engine everywhere (§16):** these
//! are a thin typed surface over the *same* Rust core `ctx` uses — never a
//! reimplementation. The high-level [`WasmEngine`] is the real full engine
//! (`csp_core::MemEngine`) — it computes its own byte-identical `main` via
//! the same `compute_main`/merge/fold as `ctx` (§5.4 by construction) — and
//! it is driven by the *same* sans-IO `csp_core::Session` (§6/§10). The
//! low-level fns are retained for the cross-surface conformance vectors
//! (§18 / `test-vectors.json`).

use csp_core::engine::MaterializeOp;
use csp_core::identity::{build_primitive, ssh_pubkey_string, verify_primitive, Identity};
use csp_core::object::GitObject;
use csp_core::oid::Oid;
use csp_core::session::{Role, Session, SessionVault};
use csp_core::wire::Msg;
use csp_core::MemEngine;
use std::collections::BTreeMap;
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

fn je<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}

fn rand32() -> Vec<u8> {
    let mut n = [0u8; 32];
    getrandom::getrandom(&mut n).expect("getrandom");
    n.to_vec()
}

// ---- Low-level conformance surface (cross-surface vectors, §18) ----------

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

/// Build a **signed primitive commit** (§5.2); returns framed object bytes —
/// byte-identical to what a native node produces (§18 cross-surface).
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
    let tree = Oid::from_hex(tree_hex).map_err(je)?;
    let parent = Oid::from_hex(parent_hex).map_err(je)?;
    Ok(build_primitive(&id, tree, parent, counter, wall_time, subject).framed())
}

/// The oid of framed object bytes.
#[wasm_bindgen]
pub fn object_oid(framed: &[u8]) -> Result<String, JsError> {
    Ok(GitObject::parse_framed(framed).map_err(je)?.oid().to_hex())
}

/// Verify a primitive's in-object signature; returns the author NodeId hex
/// or throws (§6.3/§10).
#[wasm_bindgen]
pub fn verify_primitive_object(framed: &[u8]) -> Result<String, JsError> {
    match GitObject::parse_framed(framed).map_err(je)? {
        GitObject::Commit(c) => verify_primitive(&c).map(|n| n.to_hex()).map_err(je),
        _ => Err(JsError::new("not a commit object")),
    }
}

/// MessagePack-encode a wire message given as JSON (framing, §6.2/§6.6).
#[wasm_bindgen]
pub fn wire_encode(json: &str) -> Result<Vec<u8>, JsError> {
    let msg: Msg = serde_json::from_str(json).map_err(je)?;
    msg.encode().map_err(|e| JsError::new(&e))
}

/// Decode a MessagePack wire frame back to JSON.
#[wasm_bindgen]
pub fn wire_decode(bytes: &[u8]) -> Result<String, JsError> {
    let msg = Msg::decode(bytes).map_err(|e| JsError::new(&e))?;
    serde_json::to_string(&msg).map_err(je)
}

// ---- High-level: the real full engine (one core, §16) -------------------

#[derive(serde::Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum OpJson {
    Write { path: String, content: Vec<u8> },
    Remove { path: String },
    Defer { path: String },
}

#[derive(serde::Serialize)]
struct StepJson {
    out: Vec<Vec<u8>>,
    integrated: usize,
    established: bool,
}

/// The plugin-facing full engine: the *same* `MemEngine` + `Session` as
/// `ctx`, with no fs/sockets (files in / materialize ops out; the host owns
/// transport + storage). Computes its own deterministic `main`.
#[wasm_bindgen]
pub struct WasmEngine {
    inner: MemEngine,
    session: Option<Session>,
}

#[wasm_bindgen]
impl WasmEngine {
    /// `ctx init`-equivalent.
    pub fn create(seed: &[u8], vault_id: &str, name: &str) -> Result<WasmEngine, JsError> {
        let id = Identity::from_seed(&seed32(seed)?);
        Ok(WasmEngine {
            inner: MemEngine::create(id, vault_id, name).map_err(je)?,
            session: None,
        })
    }

    /// Restore from host-persisted bytes; `ignore` = newline-joined
    /// `.contextignore` + node-local exclude globs (§11).
    pub fn open(seed: &[u8], persisted: &[u8], ignore: &str) -> Result<WasmEngine, JsError> {
        let id = Identity::from_seed(&seed32(seed)?);
        let globs = ignore.lines().map(|s| s.to_string()).collect();
        Ok(WasmEngine {
            inner: MemEngine::from_bytes(id, persisted, globs).map_err(je)?,
            session: None,
        })
    }

    /// Opaque bytes the host persists via its StorageAdapter (`.context/`-
    /// equivalent, never synced — §11).
    pub fn to_bytes(&self) -> Result<Vec<u8>, JsError> {
        self.inner.to_bytes().map_err(je)
    }

    pub fn node_ssh(&self) -> String {
        SessionVault::identity_ssh(&self.inner)
    }
    pub fn node_id(&self) -> String {
        self.inner.node_id().to_hex()
    }
    pub fn vault_id(&self) -> String {
        self.inner.config.vault_id.clone()
    }
    /// Current `main` fold-commit oid hex (empty before genesis).
    pub fn main(&self) -> String {
        self.inner.main().map(|o| o.to_hex()).unwrap_or_default()
    }
    pub fn set_ignore(&mut self, ignore: &str) {
        self.inner
            .set_ignore(ignore.lines().map(|s| s.to_string()).collect());
    }
    pub fn authorize(&mut self, ssh_line: &str) {
        self.inner.authorize(ssh_line);
    }

    /// Author a primitive from the host's scoped working set (§5.6).
    /// `files_json` = `{ "path": [byte,…], … }`. Returns the new primitive
    /// oid hex, or `null` if nothing genuinely changed.
    pub fn commit_from_files(&mut self, files_json: &str) -> Result<Option<String>, JsError> {
        let files: BTreeMap<String, Vec<u8>> = serde_json::from_str(files_json).map_err(je)?;
        Ok(self
            .inner
            .commit_from_files(&files)
            .map_err(je)?
            .map(|o| o.to_hex()))
    }

    /// The §5.6 no-clobber materialize plan for the current `main`.
    /// `on_disk_json` = the host's current bytes per known path. Returns a
    /// JSON array of `{op:"write",path,content}|{op:"remove",path}|
    /// {op:"defer",path}`.
    pub fn materialize_plan(&mut self, on_disk_json: &str) -> Result<String, JsError> {
        let on_disk: BTreeMap<String, Vec<u8>> = serde_json::from_str(on_disk_json).map_err(je)?;
        let ops: Vec<OpJson> = self
            .inner
            .materialize_plan(&on_disk)
            .map_err(je)?
            .into_iter()
            .map(|o| match o {
                MaterializeOp::Write { path, content } => OpJson::Write { path, content },
                MaterializeOp::Remove { path } => OpJson::Remove { path },
                MaterializeOp::Defer { path } => OpJson::Defer { path },
            })
            .collect();
        serde_json::to_string(&ops).map_err(je)
    }

    /// Integrate received raw object closures (§6.3). `raws_json` = JSON
    /// array of byte arrays. Returns the count of new primitives admitted.
    pub fn integrate(&mut self, raws_json: &str) -> Result<usize, JsError> {
        let raws: Vec<Vec<u8>> = serde_json::from_str(raws_json).map_err(je)?;
        self.inner.integrate(&raws).map_err(je)
    }

    pub fn frontier_tips(&self) -> Result<Vec<String>, JsError> {
        Ok(self
            .inner
            .frontier_tips()
            .map_err(je)?
            .iter()
            .map(|o| o.to_hex())
            .collect())
    }
    pub fn known(&self) -> Result<Vec<String>, JsError> {
        Ok(self
            .inner
            .known()
            .map_err(je)?
            .iter()
            .map(|o| o.to_hex())
            .collect())
    }

    /// Raw reachable closure of the given tip hexes (JSON array of byte
    /// arrays, the §6.4 delivery unit).
    pub fn export_closure(&self, tips_json: &str) -> Result<String, JsError> {
        let hexes: Vec<String> = serde_json::from_str(tips_json).map_err(je)?;
        let tips: Vec<Oid> = hexes
            .iter()
            .map(|h| Oid::from_hex(h))
            .collect::<Result<_, _>>()
            .map_err(je)?;
        let raws = self.inner.export_closure(&tips).map_err(je)?;
        serde_json::to_string(&raws).map_err(je)
    }

    // ---- The one sans-IO Session (§6/§10) — same code as `ctx` ----------

    /// Begin a session as connector (the plugin never listens — §7) and
    /// return the opening `Hello` frame bytes to send. `channel_binding` =
    /// the TLS cert fingerprint the transport observed, or empty for
    /// plaintext (§10).
    pub fn session_start(&mut self, channel_binding: Vec<u8>) -> Result<Vec<u8>, JsError> {
        let s = Session::new(Role::Connector, channel_binding, rand32());
        let hello = s.start(&self.inner);
        self.session = Some(s);
        hello.encode().map_err(|e| JsError::new(&e))
    }

    /// Feed one inbound wire frame; returns JSON
    /// `{out:[[byte…]…], integrated, established}`. The host sends `out`,
    /// then (when `integrated>0`) recomputes the materialize plan.
    pub fn session_feed(&mut self, frame: &[u8]) -> Result<String, JsError> {
        let msg = Msg::decode(frame).map_err(|e| JsError::new(&e))?;
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| JsError::new("session not started"))?;
        let step = session.on_msg(&mut self.inner, msg).map_err(je)?;
        let mut out = Vec::with_capacity(step.out.len());
        for m in &step.out {
            out.push(m.encode().map_err(|e| JsError::new(&e))?);
        }
        let established = session.established();
        serde_json::to_string(&StepJson {
            out,
            integrated: step.integrated,
            established,
        })
        .map_err(je)
    }

    // ---- PITR (§8) -----------------------------------------------------

    pub fn snapshot(&mut self, name: &str) -> Result<(), JsError> {
        self.inner.snapshot(name).map_err(je)
    }
    /// Returns the historical tree as `{ "path": [byte,…] }`; the host
    /// writes it to the working files then calls `commit_from_files`
    /// (restore-as-edit, §8).
    pub fn restore_snapshot(&mut self, name: &str) -> Result<String, JsError> {
        let tree = self.inner.restore_snapshot(name).map_err(je)?;
        serde_json::to_string(&tree).map_err(je)
    }
    pub fn restore_time(&mut self, t_unix: u64) -> Result<String, JsError> {
        let tree = self.inner.restore_time(t_unix).map_err(je)?;
        serde_json::to_string(&tree).map_err(je)
    }
    pub fn snapshots_json(&self) -> Result<String, JsError> {
        serde_json::to_string(self.inner.snapshots()).map_err(je)
    }
}
