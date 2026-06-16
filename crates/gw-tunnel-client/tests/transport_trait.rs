use gw_core::tunnel_protocol::TunnelProtocol;
use gw_tunnel_client::{StreamHandle, StreamTarget, Transport, TransportError};
use std::time::Duration;

struct NoopTransport;

impl Transport for NoopTransport {
    fn open_stream<'a>(&'a self, _target: StreamTarget) -> gw_tunnel_client::TransportFuture<'a> {
        Box::pin(async { Err(TransportError::NotAlive) })
    }

    fn alive(&self) -> bool {
        false
    }

    fn last_heartbeat_age(&self) -> Duration {
        Duration::from_secs(3)
    }

    fn tunnel_protocol(&self) -> TunnelProtocol {
        TunnelProtocol::Cnexus
    }

    fn close(&self) {}
}

#[test]
fn transport_trait_is_object_safe() {
    let transport = NoopTransport;
    let object: &dyn Transport = &transport;

    assert!(!object.alive());
    assert_eq!(object.last_heartbeat_age(), Duration::from_secs(3));
    assert_eq!(object.tunnel_protocol(), TunnelProtocol::Cnexus);
}

#[test]
fn stream_target_tcp_carries_authority_and_tls_hint() {
    let target = StreamTarget::Tcp {
        host: "api.example.test".to_owned(),
        port: 443,
        tls: true,
    };

    assert_eq!(
        target.tcp_authority(),
        Some("api.example.test:443".to_owned())
    );
    assert!(target.tcp_tls_hint().unwrap());
}

#[allow(dead_code)]
fn stream_handle_is_bidirectional(_handle: StreamHandle) {}
