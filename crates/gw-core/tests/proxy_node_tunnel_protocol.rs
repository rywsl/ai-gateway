use gw_core::entity::ProxyNode;
use gw_core::tunnel_protocol::TunnelProtocol;

#[test]
fn proxy_node_missing_tunnel_protocol_defaults_to_cnexus() {
    let raw = serde_json::json!({
        "id": "pn_1",
        "ws_url": "ws://127.0.0.1:7000/tunnel",
        "enabled": true,
        "latency_ms": 12,
        "allowed_provider_ids": [],
        "allowed_vendor_key_ids": []
    });

    let node: ProxyNode = serde_json::from_value(raw).unwrap();

    assert_eq!(node.tunnel_protocol, TunnelProtocol::Cnexus);
}

#[test]
fn proxy_node_accepts_aether_tunnel_protocol() {
    let raw = serde_json::json!({
        "id": "pn_1",
        "ws_url": "ws://127.0.0.1:7000/tunnel",
        "enabled": true,
        "latency_ms": 12,
        "allowed_provider_ids": [],
        "allowed_vendor_key_ids": [],
        "tunnel_protocol": "aether"
    });

    let node: ProxyNode = serde_json::from_value(raw).unwrap();

    assert_eq!(node.tunnel_protocol, TunnelProtocol::Aether);
}
