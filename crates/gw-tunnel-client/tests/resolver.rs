use gw_tunnel_client::resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};

fn nodes() -> Vec<ProxyNodeView> {
    vec![
        ProxyNodeView {
            id: "n1".into(),
            enabled: true,
            latency_ms: 30,
            allowed_provider_ids: vec!["openai".into()],
            allowed_vendor_key_ids: vec![],
        },
        ProxyNodeView {
            id: "n2".into(),
            enabled: true,
            latency_ms: 10,
            allowed_provider_ids: vec!["anthropic".into()],
            allowed_vendor_key_ids: vec![],
        },
        ProxyNodeView {
            id: "n3".into(),
            enabled: false,
            latency_ms: 5,
            allowed_provider_ids: vec!["openai".into()],
            allowed_vendor_key_ids: vec![],
        },
    ]
}

#[test]
fn picks_lowest_latency_among_allowed() {
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk1",
            preference: TunnelPreference::default(),
        },
        &nodes(),
    );
    assert_eq!(r, TunnelResolution::Use("n1".into()));
}

#[test]
fn returns_direct_when_no_match() {
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "google",
            vendor_key_id: "vk1",
            preference: TunnelPreference::default(),
        },
        &nodes(),
    );
    assert_eq!(r, TunnelResolution::Direct);
}

#[test]
fn disabled_nodes_skipped() {
    let only_disabled = vec![ProxyNodeView {
        id: "x".into(),
        enabled: false,
        latency_ms: 1,
        allowed_provider_ids: vec!["openai".into()],
        allowed_vendor_key_ids: vec![],
    }];
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk1",
            preference: TunnelPreference::default(),
        },
        &only_disabled,
    );
    assert_eq!(r, TunnelResolution::Direct);
}

#[test]
fn vendor_key_whitelist_filters() {
    let nodes = vec![ProxyNodeView {
        id: "n1".into(),
        enabled: true,
        latency_ms: 10,
        allowed_provider_ids: vec!["openai".into()],
        allowed_vendor_key_ids: vec!["vk-special".into()],
    }];
    let r = resolve_tunnel(
        &TunnelResolveCtx {
            provider_id: "openai",
            vendor_key_id: "vk-other",
            preference: TunnelPreference::default(),
        },
        &nodes,
    );
    assert_eq!(r, TunnelResolution::Direct);
}
