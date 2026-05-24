//! Tunnel decision stack — pluggable into `LlmGateway`.
//!
//! Owns the per-node `Arc<ReconnectingTransport>` pool, the live
//! `Vec<ProxyNodeView>` snapshot (ArcSwap), and the `SafeDnsResolver`.
//! `pick_peer` calls `resolve` and, when the result is `Use(node_id)`,
//! `peer_for_node` materialises an `HttpPeer` whose socket terminates at
//! the proxy node's WS endpoint (the real upstream is encoded in the
//! tunnel frame payload).
//!
//! ADR-T-2, ADR-T-3, ADR-T-4 in
//! `docs/superpowers/specs/2026-05-22-tunnel-t-design.md`.

use arc_swap::ArcSwap;
use dashmap::DashMap;
use gw_core::entity::TunnelPreference;
use gw_tunnel_client::pool::{PoolConfig, ReconnectingTransport};
use gw_tunnel_client::resolver::{
    resolve_tunnel, ProxyNodeView, TunnelResolution, TunnelResolveCtx,
};
use gw_tunnel_client::safe_dns::SafeDnsResolver;
use gw_tunnel_client::transport::TransportConfig;
use pingora::upstreams::peer::HttpPeer;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Error returned by `peer_for_node` when the chosen transport is not yet
/// connected. Mapped to `GW_TUNNEL_NO_LIVE` / `GW_TUNNEL_TARGET_DENIED` in
/// `filters::upstream`.
#[derive(Debug)]
pub enum TunnelPeerError {
    /// `ReconnectingTransport::current()` returned `None`.
    NoLive { node_id: String },
    /// `target_filter::assert_target_allowed` rejected the host:port.
    TargetDenied { host: String, port: u16 },
}

pub struct TunnelNodeHealth {
    pub node_id: String,
    pub transport_present: bool,
    pub last_pong_age_secs: Option<f64>,
}

pub struct TunnelStack {
    /// node_id -> reconnecting transport. Created lazily on first use.
    transports: DashMap<String, Arc<ReconnectingTransport>>,
    /// Live `ProxyNodeView`s. Swapped wholesale by `reload_nodes`.
    nodes: ArcSwap<Vec<ProxyNodeView>>,
    /// DoH resolver used by probe_loop for DNS-based health checks.
    dns: Arc<SafeDnsResolver>,
    /// Pool config applied to every newly-spawned `ReconnectingTransport`.
    pool_cfg: PoolConfig,
    /// Transport config template (only `url` differs per node).
    transport_cfg_template: TransportConfig,
}

impl TunnelStack {
    pub fn new(
        nodes: Vec<ProxyNodeView>,
        dns: Arc<SafeDnsResolver>,
        pool_cfg: PoolConfig,
        transport_cfg_template: TransportConfig,
    ) -> Self {
        Self {
            transports: DashMap::new(),
            nodes: ArcSwap::from_pointee(nodes),
            dns,
            pool_cfg,
            transport_cfg_template,
        }
    }

    /// Wholesale-swap the live node list. Called by the bootstrap-side
    /// invalidation subscriber when the proxy_node table changes.
    pub fn reload_nodes(&self, nodes: Vec<ProxyNodeView>) {
        self.nodes.store(Arc::new(nodes));
    }

    /// Periodically sample transport state per node and emit gauge metrics.
    pub fn start_probe_loop(self: &Arc<Self>, interval: Duration) -> JoinHandle<()> {
        let stack = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                let nodes = stack.nodes.load_full();
                for node in nodes.iter() {
                    let present = stack
                        .transports
                        .get(&node.id)
                        .map(|t| t.current().is_some())
                        .unwrap_or(false);
                    metrics::gauge!(
                        "gw_tunnel_transport_present",
                        "node_id" => node.id.clone()
                    )
                    .set(if present { 1.0 } else { 0.0 });
                }
            }
        })
    }

    /// Snapshot for the `/healthz/tunnel` admin endpoint. Returns each
    /// known node's id together with whether a live transport exists.
    /// `last_pong_age_secs` is `None` until T3 adds the pong tracker.
    pub fn health_snapshot(&self) -> Vec<TunnelNodeHealth> {
        let nodes = self.nodes.load_full();
        nodes
            .iter()
            .map(|n| {
                let transport_present = self
                    .transports
                    .get(&n.id)
                    .map(|t| t.current().is_some())
                    .unwrap_or(false);
                TunnelNodeHealth {
                    node_id: n.id.clone(),
                    transport_present,
                    last_pong_age_secs: None,
                }
            })
            .collect()
    }

    /// Decide Direct vs Tunnel for a given (provider, key, preference).
    pub fn resolve(
        &self,
        provider_id: &str,
        key_id: &str,
        pref: &TunnelPreference,
    ) -> TunnelResolution {
        let nodes = self.nodes.load_full();
        let ctx = TunnelResolveCtx {
            provider_id,
            vendor_key_id: key_id,
            preference: pref,
        };
        resolve_tunnel(&ctx, &nodes)
    }

    /// Build an `HttpPeer` whose socket terminates at the given node's
    /// WS endpoint. The real upstream (`host:port`) is encoded as a
    /// `MsgType::Open` frame payload over the tunnel — Pingora itself
    /// only sees the gateway-to-node TLS connection.
    pub async fn peer_for_node(
        &self,
        node_id: &str,
        host: &str,
        port: u16,
    ) -> Result<HttpPeer, TunnelPeerError> {
        // Take one snapshot for the entire method so that a concurrent
        // reload_nodes() cannot cause the two node-lookup sites to disagree.
        let snapshot = self.nodes.load_full();

        // Lazily create the transport on first use.
        let transport = if let Some(t) = self.transports.get(node_id) {
            t.clone()
        } else {
            // The node ws_url comes from the matching ProxyNodeView. If the
            // node disappeared between resolve() and here, fall through to NoLive.
            let url = match snapshot
                .iter()
                .find(|n| n.id == node_id)
                .map(|n| n.ws_url.clone())
            {
                Some(u) => u,
                None => {
                    return Err(TunnelPeerError::NoLive {
                        node_id: node_id.to_owned(),
                    });
                }
            };
            let mut cfg = self.transport_cfg_template.clone();
            cfg.url = url;
            let t = ReconnectingTransport::start(cfg, self.pool_cfg.clone()).await;
            self.transports.insert(node_id.to_owned(), t.clone());
            t
        };

        // Cold-start case: the ReconnectingTransport was just spawned and its
        // WS handshake has not completed yet. Flow into R-E failover via
        // GW_TUNNEL_NO_LIVE per spec §4.
        if transport.current().is_none() {
            metrics::counter!(
                "gw_tunnel_open_total",
                "node_id" => node_id.to_owned(),
                "result" => "no_live",
            )
            .increment(1);
            return Err(TunnelPeerError::NoLive {
                node_id: node_id.to_owned(),
            });
        }

        // Bracket bare IPv6 literals so url::Url::parse accepts them.
        let host_for_url = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let target_url: url::Url =
            format!("https://{host_for_url}:{port}")
                .parse()
                .map_err(|_| TunnelPeerError::TargetDenied {
                    host: host.to_owned(),
                    port,
                })?;
        if gw_tunnel_client::target_filter::assert_target_allowed(&target_url).is_err() {
            metrics::counter!(
                "gw_tunnel_open_total",
                "node_id" => node_id.to_owned(),
                "result" => "target_denied",
            )
            .increment(1);
            return Err(TunnelPeerError::TargetDenied {
                host: host.to_owned(),
                port,
            });
        }

        // Build the HttpPeer whose socket terminates at the WS node.
        // Use the same snapshot taken at the top of this method.
        let view_url = snapshot
            .iter()
            .find(|n| n.id == node_id)
            .map(|n| n.ws_url.clone())
            .ok_or_else(|| TunnelPeerError::NoLive {
                node_id: node_id.to_owned(),
            })?;
        let node_host = view_url.host_str().unwrap_or("").to_string();
        let node_port = view_url.port_or_known_default().unwrap_or(443);
        let tls = view_url.scheme().eq_ignore_ascii_case("wss");
        metrics::counter!(
            "gw_tunnel_open_total",
            "node_id" => node_id.to_owned(),
            "result" => "ok",
        )
        .increment(1);
        Ok(HttpPeer::new(
            format!("{node_host}:{node_port}"),
            tls,
            node_host,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_tunnel_client::resolver::ProxyNodeView;

    fn dummy_url() -> url::Url {
        "wss://node.example.com:443/tunnel".parse().unwrap()
    }

    fn view(id: &str, latency: u32, providers: &[&str]) -> ProxyNodeView {
        ProxyNodeView {
            id: id.into(),
            ws_url: dummy_url(),
            enabled: true,
            latency_ms: latency,
            allowed_provider_ids: providers.iter().map(|s| (*s).to_string()).collect(),
            allowed_vendor_key_ids: vec![],
        }
    }

    fn empty_stack(nodes: Vec<ProxyNodeView>) -> TunnelStack {
        TunnelStack::new(
            nodes,
            Arc::new(SafeDnsResolver::cloudflare_default()),
            PoolConfig::default(),
            TransportConfig::default(),
        )
    }

    #[test]
    fn resolve_returns_direct_when_no_nodes_match_provider() {
        let stack = empty_stack(vec![view("n1", 10, &["openai"])]);
        let pref = TunnelPreference::default();
        let r = stack.resolve("anthropic", "key-x", &pref);
        assert_eq!(r, TunnelResolution::Direct);
    }

    #[test]
    fn resolve_returns_lowest_latency_when_prefer_low_latency() {
        let stack = empty_stack(vec![
            view("slow", 200, &["openai"]),
            view("fast", 10, &["openai"]),
        ]);
        let pref = TunnelPreference {
            prefer_low_latency: true,
            ..Default::default()
        };
        let r = stack.resolve("openai", "k", &pref);
        assert_eq!(r, TunnelResolution::Use("fast".into()));
    }

    #[test]
    fn resolve_honours_preferred_node_ids() {
        let stack = empty_stack(vec![view("a", 10, &["openai"]), view("b", 5, &["openai"])]);
        let pref = TunnelPreference {
            preferred_node_ids: vec!["a".to_string()].into_iter().collect(),
            ..Default::default()
        };
        let r = stack.resolve("openai", "k", &pref);
        assert_eq!(r, TunnelResolution::Use("a".into()));
    }
}
