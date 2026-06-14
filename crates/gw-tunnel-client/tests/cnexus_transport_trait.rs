use bytes::BytesMut;
use gw_tunnel_client::cnexus::frame::{decode_frame, MsgType};
use gw_tunnel_client::cnexus::transport::{TransportConfig, WsTransport};
use gw_tunnel_client::{StreamTarget, Transport};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn transport_trait_open_stream_tcp_uses_cnexus_authority() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        match futures_util::StreamExt::next(&mut ws)
            .await
            .unwrap()
            .unwrap()
        {
            Message::Binary(bin) => {
                let (frame, _) = decode_frame(&bin).unwrap().unwrap();
                assert_eq!(frame.msg_type, MsgType::Open);
                assert_eq!(
                    frame.payload,
                    BytesMut::from("api.example.test:443").freeze()
                );
            }
            other => panic!("unexpected websocket message: {other:?}"),
        }
    });

    let transport = WsTransport::connect(TransportConfig {
        url: format!("ws://{addr}/").parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        ..TransportConfig::default()
    })
    .await
    .unwrap();

    let _stream = Transport::open_stream(
        &transport,
        StreamTarget::Tcp {
            host: "api.example.test".to_owned(),
            port: 443,
            tls: true,
        },
    )
    .await
    .unwrap();

    server.await.unwrap();
}
