use gw_proxy::tunnel_stack::TunnelStack;
use gw_tunnel_client::registry::ProxyNodeRegistry;

#[test]
fn tunnel_stack_implements_registry() {
    fn assert_registry<T: ProxyNodeRegistry>() {}

    assert_registry::<TunnelStack>();
}
