//! Verify the `tunnel` feature is opt-in: gw-tunnel-client is an optional dep.

#[test]
fn tunnel_feature_optional() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .unwrap();
    assert!(
        manifest.contains("gw-tunnel-client"),
        "tunnel-client not declared in manifest"
    );
    assert!(
        manifest.contains("optional = true"),
        "tunnel-client not optional"
    );
    assert!(
        manifest.contains("[features]"),
        "[features] table missing"
    );
    assert!(manifest.contains("tunnel"), "tunnel feature missing");
    assert!(
        manifest.contains("gw-proxy/tunnel"),
        "tunnel feature must forward to gw-proxy/tunnel (T5-B)"
    );
}
