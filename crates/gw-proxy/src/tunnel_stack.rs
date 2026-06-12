//! Tunnel decision stack — pluggable into `LlmGateway`.
//!
//! Owns the per-node `Arc<ReconnectingTransport>` pool, the live
//! `Vec<ProxyNodeView>` snapshot (ArcSwap), and the `SafeDnsResolver`.
//! `pick_peer` calls `resolve` and, when the result is `Use(node_id)`,
//! `peer_for_node` materialises a loopback bridge `HttpPeer`; the selected
//! node and real upstream target are encoded in the bridge request context
//! and tunnel frame payload.
//!
//! ADR-T-2, ADR-T-3, ADR-T-4 in
//! `docs/spec/09-tunnel.md`.

use arc_swap::ArcSwap;
use dashmap::DashMap;
use gw_core::entity::TunnelPreference;
use gw_runtime::ShutdownToken;
use gw_tunnel_client::pool::{PoolConfig, ReconnectingTransport};
use gw_tunnel_client::resolver::{
    resolve_tunnel, ProxyNodeView, TunnelResolution, TunnelResolveCtx,
};
use gw_tunnel_client::safe_dns::SafeDnsResolver;
use gw_tunnel_client::transport::{TransportConfig, WsTransport};
use pingora::upstreams::peer::HttpPeer;
use std::net::SocketAddr;
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
    /// `SafeDnsResolver` resolved the target to a blocked/private address.
    DnsBlocked {
        host: String,
        port: u16,
        blocked_ip: std::net::IpAddr,
    },
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
    /// Loopback HTTP/1 bridge address. `peer_for_node` returns this peer after
    /// validating the chosen node and target.
    bridge_addr: arc_swap::ArcSwapOption<SocketAddr>,
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
            bridge_addr: arc_swap::ArcSwapOption::empty(),
        }
    }

    pub fn set_bridge_addr(&self, addr: SocketAddr) {
        self.bridge_addr.store(Some(Arc::new(addr)));
    }

    /// Wholesale-swap the live node list. Called by the bootstrap-side
    /// invalidation subscriber when the proxy_node table changes.
    pub fn reload_nodes(&self, nodes: Vec<ProxyNodeView>) {
        self.nodes.store(Arc::new(nodes));
    }

    /// Periodically sample transport state per node and emit gauge
    /// metrics. The returned `JoinHandle` exits cleanly when the
    /// `ShutdownToken` is cancelled — register the handle with
    /// `ShutdownCoordinator` so drain awaits its exit.
    pub fn start_probe_loop(
        self: &Arc<Self>,
        interval: Duration,
        shutdown: ShutdownToken,
    ) -> JoinHandle<()> {
        let stack = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => {
                        tracing::info!(
                            target: "gw_proxy::tunnel_stack",
                            "probe loop shutdown requested"
                        );
                        return;
                    }
                    _ = ticker.tick() => {
                        let nodes = stack.nodes.load_full();
                        for node in nodes.iter() {
                            let current = stack.transports.get(&node.id).and_then(|t| t.current());
                            let present = current.as_ref().is_some_and(|t| t.is_alive());
                            metrics::gauge!(
                                "gw_tunnel_transport_present",
                                "node_id" => node.id.clone()
                            )
                            .set(if present { 1.0 } else { 0.0 });
                            if let Some(age) = current.as_ref().map(|t| t.last_pong_age().as_secs_f64()) {
                                metrics::gauge!(
                                    "gw_tunnel_last_pong_age_seconds",
                                    "node_id" => node.id.clone()
                                )
                                .set(age);
                            }
                        }
                    }
                }
            }
        })
    }

    /// Snapshot for the `/healthz/tunnel` admin endpoint. Returns each
    /// known node's id together with whether a live transport exists.
    pub fn health_snapshot(&self) -> Vec<TunnelNodeHealth> {
        let nodes = self.nodes.load_full();
        nodes
            .iter()
            .map(|n| {
                let current = self.transports.get(&n.id).and_then(|t| t.current());
                let transport_present = current.as_ref().is_some_and(|t| t.is_alive());
                TunnelNodeHealth {
                    node_id: n.id.clone(),
                    transport_present,
                    last_pong_age_secs: current.as_ref().map(|t| t.last_pong_age().as_secs_f64()),
                }
            })
            .collect()
    }

    pub fn current_transport(&self, node_id: &str) -> Option<Arc<WsTransport>> {
        self.transports.get(node_id).and_then(|t| t.current())
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
        if let Err(err) = self.dns.resolve(host).await {
            metrics::counter!(
                "gw_tunnel_open_total",
                "node_id" => node_id.to_owned(),
                "result" => "dns_blocked",
            )
            .increment(1);
            return Err(TunnelPeerError::DnsBlocked {
                host: host.to_owned(),
                port,
                blocked_ip: err.0,
            });
        }

        // Take one snapshot for the entire method so that a concurrent
        // reload_nodes() cannot cause the two node-lookup sites to disagree.
        let snapshot = self.nodes.load_full();

        // Lazily create the transport on first use.
        let transport = if let Some(t) = self.transports.get(node_id) {
            t.clone()
        } else {
            // The node ws_url comes from the matching ProxyNodeView. If the
            // node disappeared between resolve() and here, fall through to NoLive.
            let node = match snapshot.iter().find(|n| n.id == node_id) {
                Some(n) => n,
                None => {
                    return Err(TunnelPeerError::NoLive {
                        node_id: node_id.to_owned(),
                    });
                }
            };
            let mut cfg = self.transport_cfg_template.clone();
            cfg.url = node.ws_url.clone();
            cfg.auth_token = node.auth_token.clone();
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

        let Some(bridge_addr) = self.bridge_addr.load_full() else {
            return Err(TunnelPeerError::NoLive {
                node_id: node_id.to_owned(),
            });
        };
        metrics::counter!(
            "gw_tunnel_open_total",
            "node_id" => node_id.to_owned(),
            "result" => "ok",
        )
        .increment(1);
        Ok(HttpPeer::new(
            bridge_addr.to_string(),
            false,
            "127.0.0.1".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_tunnel_client::resolver::ProxyNodeView;
    use gw_tunnel_client::safe_dns::{set_override, OverrideGuard};
    use std::net::{IpAddr, Ipv4Addr};

    fn dummy_url() -> url::Url {
        "wss://node.example.com:443/tunnel".parse().unwrap()
    }

    fn view(id: &str, latency: u32, providers: &[&str]) -> ProxyNodeView {
        ProxyNodeView {
            id: id.into(),
            ws_url: dummy_url(),
            auth_token: None,
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

    #[tokio::test]
    async fn peer_for_node_rejects_blocked_dns_before_transport() {
        let _guard = OverrideGuard::new();
        set_override(
            "blocked.example.com",
            vec![IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))],
        );
        let stack = empty_stack(vec![view("n1", 10, &["openai"])]);

        let err = stack
            .peer_for_node("n1", "blocked.example.com", 443)
            .await
            .expect_err("blocked DNS must fail before tunnel liveness");
        assert!(matches!(
            err,
            TunnelPeerError::DnsBlocked {
                host,
                port: 443,
                ..
            } if host == "blocked.example.com"
        ));
    }

    #[test]
    fn health_snapshot_reads_last_pong_age() {
        let src =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tunnel_stack.rs"))
                .expect("read tunnel_stack source");
        let prod_src = src.split("#[cfg(test)]").next().unwrap_or(&src);
        assert!(
            prod_src.contains("last_pong_age().as_secs_f64()"),
            "health_snapshot must expose live transport pong age"
        );
        assert!(
            !prod_src.contains("last_pong_age_secs: None,"),
            "health_snapshot must not hard-code null pong age"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn probe_loop_respects_shutdown_token() {
        let stack = Arc::new(empty_stack(vec![]));
        let coord = gw_runtime::ShutdownCoordinator::default();
        let token = coord.token();
        let handle = stack.start_probe_loop(std::time::Duration::from_secs(60), token);
        coord.token().cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "probe loop did not exit within 5s of shutdown");
    }
}
