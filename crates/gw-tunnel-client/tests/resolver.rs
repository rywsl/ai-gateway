use gw_tunnel_client::resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};

fn dummy_url() -> url::Url {
    "wss://node.example.com:443/tunnel".parse().unwrap()
}

fn nodes() -> Vec<ProxyNodeView> {
    vec![
        ProxyNodeView {
            id: "n1".into(),
            ws_url: dummy_url(),
            auth_token: None,
            enabled: true,
            latency_ms: 30,
            allowed_provider_ids: vec!["openai".into()],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
        },
        ProxyNodeView {
            id: "n2".into(),
            ws_url: dummy_url(),
            auth_token: None,
            enabled: true,
            latency_ms: 10,
            allowed_provider_ids: vec!["anthropic".into()],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
        },
        ProxyNodeView {
            id: "n3".into(),
            ws_url: dummy_url(),
            auth_token: None,
            enabled: false,
            latency_ms: 5,
            allowed_provider_ids: vec!["openai".into()],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
        },
    ]
}

#[test]
fn picks_first_matching_without_latency_sort() {
    let pref = TunnelPreference::default();
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk1",
            preference: &pref,
        },
        &nodes(),
    );
    assert_eq!(r, TunnelResolution::Use("n1".into()));
}

#[test]
fn returns_direct_when_no_match() {
    let pref = TunnelPreference::default();
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "google",
            vendor_key_id: "vk1",
            preference: &pref,
        },
        &nodes(),
    );
    assert_eq!(r, TunnelResolution::Direct);
}

#[test]
fn disabled_nodes_skipped() {
    let only_disabled = vec![ProxyNodeView {
        id: "x".into(),
        ws_url: dummy_url(),
        auth_token: None,
        enabled: false,
        latency_ms: 1,
        allowed_provider_ids: vec!["openai".into()],
        allowed_vendor_key_ids: vec![],
        tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
    }];
    let pref = TunnelPreference::default();
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk1",
            preference: &pref,
        },
        &only_disabled,
    );
    assert_eq!(r, TunnelResolution::Direct);
}

#[test]
fn vendor_key_whitelist_filters() {
    let nodes = vec![ProxyNodeView {
        id: "n1".into(),
        ws_url: dummy_url(),
        auth_token: None,
        enabled: true,
        latency_ms: 10,
        allowed_provider_ids: vec!["openai".into()],
        allowed_vendor_key_ids: vec!["vk-special".into()],
        tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
    }];
    let pref = TunnelPreference::default();
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk-other",
            preference: &pref,
        },
        &nodes,
    );
    assert_eq!(r, TunnelResolution::Direct);
}

#[test]
fn prefer_low_latency_sorts_nodes() {
    // Two enabled openai nodes: n1 latency=30, n4 latency=5
    let mixed_nodes = vec![
        ProxyNodeView {
            id: "n1".into(),
            ws_url: dummy_url(),
            auth_token: None,
            enabled: true,
            latency_ms: 30,
            allowed_provider_ids: vec!["openai".into()],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
        },
        ProxyNodeView {
            id: "n4".into(),
            ws_url: dummy_url(),
            auth_token: None,
            enabled: true,
            latency_ms: 5,
            allowed_provider_ids: vec!["openai".into()],
            allowed_vendor_key_ids: vec![],
            tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,
        },
    ];
    let pref = TunnelPreference {
        prefer_low_latency: true,
        ..Default::default()
    };
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk1",
            preference: &pref,
        },
        &mixed_nodes,
    );
    assert_eq!(r, TunnelResolution::Use("n4".into()));
}

#[test]
fn preferred_node_ids_filters_to_subset() {
    let pref = TunnelPreference {
        preferred_node_ids: vec!["n2".to_string()].into_iter().collect(),
        ..Default::default()
    };
    // n2 is anthropic-only; using anthropic provider should hit it
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "anthropic",
            vendor_key_id: "vk1",
            preference: &pref,
        },
        &nodes(),
    );
    assert_eq!(r, TunnelResolution::Use("n2".into()));
}

#[test]
fn preferred_node_ids_none_match_returns_direct() {
    let pref = TunnelPreference {
        preferred_node_ids: vec!["nonexistent".to_string()].into_iter().collect(),
        ..Default::default()
    };
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk1",
            preference: &pref,
        },
        &nodes(),
    );
    assert_eq!(r, TunnelResolution::Direct);
}

#[test]
fn proxy_node_view_carries_tunnel_protocol() {
    use gw_core::tunnel_protocol::TunnelProtocol;

    let v = ProxyNodeView {
        id: "n".into(),
        ws_url: dummy_url(),
        auth_token: None,
        enabled: true,
        latency_ms: 10,
        allowed_provider_ids: vec!["p".into()],
        allowed_vendor_key_ids: vec![],
        tunnel_protocol: TunnelProtocol::Aether,
    };

    assert_eq!(v.tunnel_protocol, TunnelProtocol::Aether);
}
