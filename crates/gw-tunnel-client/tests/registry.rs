use gw_tunnel_client::registry::ProxyNodeRegistry;

#[test]
fn registry_trait_is_object_safe() {
    fn accepts_object(_: &dyn ProxyNodeRegistry) {}

    let _ = accepts_object;
}
