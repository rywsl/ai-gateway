//! Proxy-node resolver. Selects which tunnel node (if any) to use for a given
//! provider + vendor_key.

pub use gw_core::entity::TunnelPreference;

#[derive(Debug, Clone)]
pub struct ProxyNodeView {
    pub id: String,
    /// `ws[s]://host:port/path` — the endpoint `TunnelStack::peer_for_node`
    /// dials when building the `HttpPeer`. Mirrors `gw_core::entity::ProxyNode::ws_url`.
    pub ws_url: url::Url,
    pub auth_token: Option<secrecy::SecretString>,
    pub enabled: bool,
    pub latency_ms: u32,
    pub allowed_provider_ids: Vec<String>,
    pub allowed_vendor_key_ids: Vec<String>,
    /// Wire protocol this node speaks. Phase 1 always populates Cnexus;
    /// Phase 2 starts honoring Aether.
    pub tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol,
}

pub struct TunnelResolveCtx<'a> {
    pub provider_id: &'a str,
    pub vendor_key_id: &'a str,
    pub preference: &'a TunnelPreference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelResolution {
    Use(String),
    Direct,
}

pub fn resolve_tunnel(ctx: &TunnelResolveCtx<'_>, nodes: &[ProxyNodeView]) -> TunnelResolution {
    let mut filtered: Vec<&ProxyNodeView> = nodes
        .iter()
        .filter(|n| n.enabled)
        .filter(|n| n.allowed_provider_ids.iter().any(|p| p == ctx.provider_id))
        .filter(|n| {
            n.allowed_vendor_key_ids.is_empty()
                || n.allowed_vendor_key_ids
                    .iter()
                    .any(|k| k == ctx.vendor_key_id)
        })
        .collect();

    if !ctx.preference.preferred_node_ids.is_empty() {
        let allow: std::collections::HashSet<&str> = ctx
            .preference
            .preferred_node_ids
            .iter()
            .map(String::as_str)
            .collect();
        filtered.retain(|n| allow.contains(n.id.as_str()));
    }

    if filtered.is_empty() {
        // Caller maps require_tunnel=true to GW_TUNNEL_NO_NODE; Direct stays
        // the resolver's representation of "no preference satisfied".
        return TunnelResolution::Direct;
    }

    if ctx.preference.prefer_low_latency {
        filtered.sort_by_key(|n| n.latency_ms);
    }
    TunnelResolution::Use(filtered[0].id.clone())
}
