use gw_tunnel_client::aether::transport::WsTransport;
use gw_tunnel_client::core::Transport;

#[test]
fn ws_transport_is_a_transport() {
    fn assert_transport<T: Transport>() {}
    assert_transport::<WsTransport>();
}

#[test]
fn ws_transport_reports_aether_protocol() {
    use gw_core::tunnel_protocol::TunnelProtocol;

    let transport = WsTransport::disconnected_for_test();

    assert_eq!(transport.tunnel_protocol(), TunnelProtocol::Aether);
}

#[test]
fn next_stream_id_starts_at_2_and_steps_by_2() {
    let transport = WsTransport::disconnected_for_test();

    let first = transport.allocate_stream_id_for_test();
    let second = transport.allocate_stream_id_for_test();
    let third = transport.allocate_stream_id_for_test();

    assert_eq!((first, second, third), (2, 4, 6));
}

#[test]
fn open_stream_rejects_tcp_target() {
    use gw_tunnel_client::{StreamTarget, TransportError};

    let transport = WsTransport::disconnected_for_test();
    let result = tokio_test::block_on(transport.open_stream(StreamTarget::Tcp {
        host: "x".into(),
        port: 1,
        tls: false,
    }));

    assert!(matches!(result, Err(TransportError::UnsupportedTarget)));
}
