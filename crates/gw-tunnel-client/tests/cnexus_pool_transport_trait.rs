use gw_core::tunnel_protocol::TunnelProtocol;
use gw_tunnel_client::cnexus::pool::{PoolConfig, ReconnectingTransport};
use gw_tunnel_client::cnexus::transport::TransportConfig;
use gw_tunnel_client::Transport;

#[tokio::test]
async fn reconnecting_transport_implements_protocol_transport_trait() {
    let pool =
        ReconnectingTransport::start(TransportConfig::default(), PoolConfig::default()).await;
    let object: &dyn Transport = pool.as_ref();

    assert_eq!(object.tunnel_protocol(), TunnelProtocol::Cnexus);
}
