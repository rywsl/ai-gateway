//! Proxy-node resolver. Selects which tunnel node (if any) to use for a given
//! provider + vendor_key.

#[derive(Debug, Clone)]
pub struct ProxyNodeView {
    pub id: String,
    pub enabled: bool,
    pub latency_ms: u32,
    pub allowed_provider_ids: Vec<String>,
    pub allowed_vendor_key_ids: Vec<String>,
}

pub struct TunnelResolveCtx<'a> {
    pub provider_id: &'a str,
    pub vendor_key_id: &'a str,
    pub preference: TunnelPreference,
}

#[derive(Debug, Clone, Default)]
pub struct TunnelPreference {
    pub prefer_low_latency: bool,
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
    if filtered.is_empty() {
        return TunnelResolution::Direct;
    }
    filtered.sort_by_key(|n| n.latency_ms);
    TunnelResolution::Use(filtered[0].id.clone())
}
