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

#[test]
fn proxy_node_accepts_socks5_proxy_url() {
    let raw = serde_json::json!({
        "id": "pn_socks",
        "ws_url": "ws://127.0.0.1:9/tunnel",
        "proxy_url": "socks5h://user:pass@proxy.example.com:1080",
        "enabled": true,
        "latency_ms": 12,
        "allowed_provider_ids": ["openai"],
        "allowed_vendor_key_ids": [],
        "tunnel_protocol": "cnexus"
    });

    let node: ProxyNode = serde_json::from_value(raw).unwrap();

    assert_eq!(
        node.proxy_url.as_ref().map(url::Url::as_str),
        Some("socks5h://user:pass@proxy.example.com:1080")
    );
}

#[test]
fn proxy_node_socks5_proxy_url_does_not_require_ws_url() {
    let raw = serde_json::json!({
        "id": "pn_socks",
        "proxy_url": "socks5://proxy.example.com:1080",
        "enabled": true,
        "latency_ms": 12,
        "allowed_provider_ids": ["openai"],
        "allowed_vendor_key_ids": [],
        "tunnel_protocol": "cnexus"
    });

    let node: ProxyNode = serde_json::from_value(raw).unwrap();

    assert_eq!(
        node.proxy_url.as_ref().map(url::Url::as_str),
        Some("socks5://proxy.example.com:1080")
    );
    assert_eq!(node.ws_url.as_str(), "http://127.0.0.1:9/tunnel");
}

#[test]
fn proxy_node_empty_proxy_url_deserializes_as_none() {
    let raw = serde_json::json!({
        "id": "pn_legacy_empty",
        "ws_url": "wss://node.example.com/tunnel",
        "proxy_url": "",
        "enabled": true,
        "latency_ms": 1,
        "allowed_provider_ids": [],
        "allowed_vendor_key_ids": [],
        "tunnel_protocol": "cnexus"
    });

    let node: ProxyNode = serde_json::from_value(raw).unwrap();

    assert!(node.proxy_url.is_none());
}
