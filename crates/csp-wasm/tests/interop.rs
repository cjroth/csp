//! Cross-surface interop (§18): the wasm binding surface and the native
//! core must produce **byte-identical** objects, oids, and signatures —
//! proving the SDK is a typed surface over the one implementation, not a
//! divergent reimplementation. Runs natively (the bindings compile on both
//! targets); the wasm32 *compile* of the reduced surface is the §13.2 spike.

use csp_core::identity::{build_primitive as core_build, verify_primitive, Identity};
use csp_core::object::{GitObject, EMPTY_TREE_HEX};
use csp_core::{genesis, MemStore, Oid};

#[test]
fn wasm_and_native_produce_identical_primitive_sha_and_signature() {
    let seed = [7u8; 32];
    let m0 = {
        let mut s = MemStore::new();
        genesis(&mut s).unwrap()
    };
    let tree_hex = EMPTY_TREE_HEX;
    let parent_hex = m0.to_hex();

    // Built through the wasm binding surface.
    let via_wasm = csp_wasm::build_primitive_object(
        &seed,
        tree_hex,
        &parent_hex,
        42,
        1_700_000_123,
        "interop",
    )
    .unwrap();
    let wasm_oid = csp_wasm::object_oid(&via_wasm).unwrap();

    // Built through the native core directly with the same inputs.
    let id = Identity::from_seed(&seed);
    let native = core_build(
        &id,
        Oid::from_hex(tree_hex).unwrap(),
        m0,
        42,
        1_700_000_123,
        "interop",
    );
    let native_framed = native.framed();
    let native_oid = native.oid().to_hex();

    assert_eq!(
        via_wasm, native_framed,
        "wasm and native must produce byte-identical objects"
    );
    assert_eq!(wasm_oid, native_oid, "identical SHA across the surface");

    // A native node verifies the wasm-authored primitive's signature …
    assert_eq!(
        csp_wasm::verify_primitive_object(&native_framed).unwrap(),
        id.node_id().to_hex()
    );
    if let GitObject::Commit(c) = GitObject::parse_framed(&via_wasm).unwrap() {
        // … and the core verifies it directly too.
        assert_eq!(verify_primitive(&c).unwrap(), id.node_id());
    } else {
        panic!("not a commit");
    }
}

/// Emit a native→wasm interop **known-answer vector** the TS SDK bun test
/// reproduces (§18 cross-surface): native generates it, the wasm surface
/// must recompute byte-identical framed bytes / oid / author.
#[test]
fn emit_cross_surface_vector() {
    let seed = [7u8; 32];
    let m0 = {
        let mut s = MemStore::new();
        genesis(&mut s).unwrap()
    };
    let framed = csp_wasm::build_primitive_object(
        &seed,
        EMPTY_TREE_HEX,
        &m0.to_hex(),
        42,
        1_700_000_123,
        "interop",
    )
    .unwrap();
    let id = Identity::from_seed(&seed);
    let vec = serde_json::json!({
        "seed_hex": hex::encode(seed),
        "tree_hex": EMPTY_TREE_HEX,
        "parent_hex": m0.to_hex(),
        "counter": 42,
        "wall_time": 1_700_000_123u64,
        "subject": "interop",
        "expected_framed_hex": hex::encode(&framed),
        "expected_oid": csp_wasm::object_oid(&framed).unwrap(),
        "expected_node_id": id.node_id().to_hex(),
    });
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../sdks/typescript/test-vectors.json");
    std::fs::write(&out, serde_json::to_string_pretty(&vec).unwrap()).unwrap();
}

#[test]
fn wire_framing_roundtrips_across_surface() {
    // A thin node speaks the protocol: JSON → wasm wire_encode (MessagePack)
    // → native core decode must be the same message (§6.2/§6.6 framing).
    let json = r#"{"FrontierDigest":{"tips":["aa","bb"]}}"#;
    let bytes = csp_wasm::wire_encode(json).unwrap();
    let msg = csp_core::wire::Msg::decode(&bytes).unwrap();
    match msg {
        csp_core::wire::Msg::FrontierDigest { tips } => {
            assert_eq!(tips, vec!["aa".to_string(), "bb".to_string()]);
        }
        _ => panic!("wrong message kind"),
    }
    // Round-trip back out through the binding.
    let back = csp_wasm::wire_decode(&bytes).unwrap();
    assert!(back.contains("FrontierDigest"));
}
