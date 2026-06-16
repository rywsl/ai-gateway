//! Verify tunnel is default-compiled and no longer controlled by a Cargo feature.

#[test]
fn tunnel_is_default_compiled() {
    let manifest =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    assert!(
        manifest.contains("gw-tunnel-client"),
        "tunnel-client not declared in manifest"
    );
    assert!(
        !manifest.contains("optional = true"),
        "tunnel-client must not be optional"
    );
    assert!(
        !manifest.contains("[features]"),
        "gw-gateway must not define a tunnel feature"
    );
}
