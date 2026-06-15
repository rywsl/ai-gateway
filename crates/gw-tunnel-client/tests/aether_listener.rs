use gw_core::tunnel_protocol::TunnelProtocol;
use gw_tunnel_client::aether::listener::{InboundListener, ListenerError};
use gw_tunnel_client::core::Transport;
use gw_tunnel_client::registry::ProxyNodeRegistry;
use gw_tunnel_client::resolver::ProxyNodeView;
use secrecy::SecretString;
use std::sync::Arc;

#[derive(Clone)]
struct StubRegistry {
    node_id: String,
    token: String,
    protocol: TunnelProtocol,
}

impl ProxyNodeRegistry for StubRegistry {
    fn lookup_by_id(&self, id: &str) -> Option<ProxyNodeView> {
        if id != self.node_id {
            return None;
        }
        Some(ProxyNodeView {
            id: id.to_string(),
            ws_url: "wss://example.test/aether".parse().unwrap(),
            auth_token: Some(SecretString::from(self.token.clone())),
            enabled: true,
            latency_ms: 0,
            allowed_provider_ids: Vec::new(),
            allowed_vendor_key_ids: Vec::new(),
            tunnel_protocol: self.protocol,
        })
    }

    fn verify_auth_token(&self, node_id: &str, candidate: &str) -> bool {
        node_id == self.node_id && candidate == self.token
    }
}

#[test]
fn rejects_unknown_node_id() {
    let listener = listener(TunnelProtocol::Aether);
    let headers = headers(&[
        ("authorization", "Bearer ae_t"),
        ("x-node-id", "unknown"),
        ("x-node-name", "n"),
        ("x-tunnel-max-streams", "64"),
    ]);

    let result = listener.authorize(&headers);

    assert!(matches!(result, Err(ListenerError::UnknownNode)));
}

#[test]
fn rejects_non_aether_protocol() {
    let listener = listener(TunnelProtocol::Cnexus);
    let headers = valid_headers();

    let result = listener.authorize(&headers);

    assert!(matches!(result, Err(ListenerError::WrongProtocol)));
}

#[test]
fn rejects_missing_authorization() {
    let listener = listener(TunnelProtocol::Aether);
    let headers = headers(&[
        ("x-node-id", "n1"),
        ("x-node-name", "n"),
        ("x-tunnel-max-streams", "64"),
    ]);

    let result = listener.authorize(&headers);

    assert!(matches!(result, Err(ListenerError::Unauthorized)));
}

#[test]
fn rejects_bad_token() {
    let listener = listener(TunnelProtocol::Aether);
    let headers = headers(&[
        ("authorization", "Bearer wrong"),
        ("x-node-id", "n1"),
        ("x-node-name", "n"),
        ("x-tunnel-max-streams", "64"),
    ]);

    let result = listener.authorize(&headers);

    assert!(matches!(result, Err(ListenerError::BadToken)));
}

#[test]
fn accepts_valid_headers() {
    let listener = listener(TunnelProtocol::Aether);

    let result = listener.authorize(&valid_headers()).unwrap();

    assert_eq!(result.node_view.id, "n1");
    assert_eq!(result.node_name, "aether-node-1");
    assert_eq!(result.max_streams, 64);
}

#[test]
fn semaphore_rejects_when_exhausted() {
    let (tx, _rx) = tokio::sync::mpsc::channel::<(String, Arc<dyn Transport>)>(1);
    let listener = InboundListener::with_capacity(registry(TunnelProtocol::Aether), tx, 1);

    let _permit = listener.try_acquire_permit().unwrap();

    assert!(matches!(
        listener.try_acquire_permit(),
        Err(ListenerError::Saturated)
    ));
}

fn listener(protocol: TunnelProtocol) -> InboundListener {
    let (tx, _rx) = tokio::sync::mpsc::channel::<(String, Arc<dyn Transport>)>(1);
    InboundListener::new(registry(protocol), tx)
}

fn registry(protocol: TunnelProtocol) -> Arc<dyn ProxyNodeRegistry> {
    Arc::new(StubRegistry {
        node_id: "n1".into(),
        token: "ae_t".into(),
        protocol,
    })
}

fn valid_headers() -> http::HeaderMap {
    headers(&[
        ("authorization", "Bearer ae_t"),
        ("x-node-id", "n1"),
        ("x-node-name", "aether-node-1"),
        ("x-tunnel-max-streams", "64"),
    ])
}

fn headers(items: &[(&str, &str)]) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();
    for (name, value) in items {
        headers.insert(
            http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            http::HeaderValue::from_str(value).unwrap(),
        );
    }
    headers
}
