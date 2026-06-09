use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use gw_tunnel_client::frame::{decode_frame, encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN};
use gw_tunnel_client::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};
use secrecy::SecretString;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

async fn echo_pong_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(Ok(m)) = ws.next().await {
            if let Message::Binary(bin) = m {
                if let Ok(Some((f, _))) = decode_frame(&bin) {
                    if f.msg_type == MsgType::Ping {
                        let mut buf = bytes::BytesMut::new();
                        encode_frame(
                            &Frame {
                                msg_type: MsgType::Pong,
                                flags: 0,
                                stream_id: f.stream_id,
                                payload: f.payload,
                            },
                            &mut buf,
                        );
                        let _ = ws.send(Message::Binary(buf.to_vec())).await;
                    }
                }
            }
        }
    });
    (port, h)
}

async fn capture_first_frame_server() -> (u16, tokio::task::JoinHandle<Frame>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(Ok(m)) = ws.next().await {
            if let Message::Binary(bin) = m {
                if let Ok(Some((f, _))) = decode_frame(&bin) {
                    return f;
                }
            }
        }
        panic!("connection closed before first frame")
    });
    (port, h)
}

async fn capture_frames_server(count: usize) -> (u16, tokio::task::JoinHandle<Vec<Frame>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        let mut out = Vec::new();
        while let Some(Ok(m)) = ws.next().await {
            if let Message::Binary(bin) = m {
                if let Ok(Some((f, _))) = decode_frame(&bin) {
                    out.push(f);
                    if out.len() == count {
                        return out;
                    }
                }
            }
        }
        out
    });
    (port, h)
}

#[tokio::test]
async fn control_auth_is_first_frame_when_token_configured() {
    let (port, first_frame) = capture_first_frame_server().await;
    let url = format!("ws://127.0.0.1:{port}/");
    let cfg = TransportConfig {
        url: url.parse().unwrap(),
        auth_token: Some(SecretString::from("node-secret".to_string())),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        ..TransportConfig::default()
    };
    let _transport = WsTransport::connect(cfg).await.unwrap();
    let frame = first_frame.await.unwrap();
    assert_eq!(frame.msg_type, MsgType::ControlAuth);
    assert_eq!(frame.stream_id, 0);
    assert_eq!(frame.payload.as_ref(), b"node-secret");
}

#[tokio::test]
async fn ping_round_trip_keeps_alive() {
    let (port, _h) = echo_pong_server().await;
    let url = format!("ws://127.0.0.1:{port}/");
    let cfg = TransportConfig {
        url: url.parse().unwrap(),
        heartbeat_interval: Duration::from_millis(50),
        missed_pong_threshold: 3,
        ..TransportConfig::default()
    };
    let t = WsTransport::connect(cfg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(t.is_alive(), "expected alive after ping/pong cycles");
    assert!(t.last_pong_age() < Duration::from_millis(300));
    let _ = Bytes::new();
}

#[tokio::test]
async fn missed_pongs_mark_dead() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while ws.next().await.is_some() {}
    });
    let url = format!("ws://127.0.0.1:{port}/");
    let cfg = TransportConfig {
        url: url.parse().unwrap(),
        heartbeat_interval: Duration::from_millis(20),
        missed_pong_threshold: 3,
        ..TransportConfig::default()
    };
    let t = WsTransport::connect(cfg).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!t.is_alive(), "expected dead after missed PONGs");
}

#[tokio::test]
async fn send_open_increments_stream_id() {
    let (port, _h) = echo_pong_server().await;
    let url = format!("ws://127.0.0.1:{port}/");
    let cfg = TransportConfig {
        url: url.parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        ..TransportConfig::default()
    };
    let t = WsTransport::connect(cfg).await.unwrap();
    let id1 = t.send_open("https://api.x").await.unwrap();
    let id2 = t.send_open("https://api.y").await.unwrap();
    assert!(id2 > id1);
}

#[tokio::test]
async fn open_stream_routes_inbound_data_close_and_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(Ok(m)) = ws.next().await {
            let Message::Binary(bin) = m else { continue };
            let Ok(Some((f, _))) = decode_frame(&bin) else {
                continue;
            };
            if f.msg_type != MsgType::Open {
                continue;
            }
            for frame in [
                Frame {
                    msg_type: MsgType::Data,
                    flags: 0,
                    stream_id: f.stream_id,
                    payload: Bytes::from_static(b"hello"),
                },
                Frame {
                    msg_type: MsgType::Close,
                    flags: 0,
                    stream_id: f.stream_id,
                    payload: Bytes::new(),
                },
            ] {
                let mut buf = bytes::BytesMut::new();
                encode_frame(&frame, &mut buf);
                ws.send(Message::Binary(buf.to_vec())).await.unwrap();
            }
            break;
        }
    });

    let cfg = TransportConfig {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        ..TransportConfig::default()
    };
    let t = WsTransport::connect(cfg).await.unwrap();
    let mut stream = t.open_stream("example.com:443").await.unwrap();
    assert_eq!(
        stream.recv().await.unwrap(),
        TunnelEvent::Data(Bytes::from_static(b"hello"))
    );
    assert_eq!(stream.recv().await.unwrap(), TunnelEvent::Close);
}

#[tokio::test]
async fn open_stream_maps_inbound_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _h = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(Ok(Message::Binary(bin))) = ws.next().await {
            let Ok(Some((f, _))) = decode_frame(&bin) else {
                continue;
            };
            if f.msg_type == MsgType::Open {
                let mut buf = bytes::BytesMut::new();
                encode_frame(
                    &Frame {
                        msg_type: MsgType::Error,
                        flags: 0,
                        stream_id: f.stream_id,
                        payload: Bytes::from_static(b"denied"),
                    },
                    &mut buf,
                );
                ws.send(Message::Binary(buf.to_vec())).await.unwrap();
                break;
            }
        }
    });
    let t = WsTransport::connect(TransportConfig {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        ..TransportConfig::default()
    })
    .await
    .unwrap();
    let mut stream = t.open_stream("example.com:443").await.unwrap();
    let err = stream.recv().await.expect_err("remote error expected");
    assert!(matches!(err, TunnelError::Remote(msg) if msg == "denied"));
}

#[tokio::test]
async fn send_data_chunks_at_max_payload_len() {
    let payload_len = MAX_PAYLOAD_LEN + 7;
    let (port, frames) = capture_frames_server(3).await;
    let t = WsTransport::connect(TransportConfig {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        ..TransportConfig::default()
    })
    .await
    .unwrap();
    let stream_id = t.send_open("example.com:443").await.unwrap();
    t.send_data(stream_id, Bytes::from(vec![b'x'; payload_len]))
        .await
        .unwrap();
    let frames = frames.await.unwrap();
    assert_eq!(frames[0].msg_type, MsgType::Open);
    assert_eq!(frames[1].msg_type, MsgType::Data);
    assert_eq!(frames[1].payload.len(), MAX_PAYLOAD_LEN);
    assert_eq!(frames[2].msg_type, MsgType::Data);
    assert_eq!(frames[2].payload.len(), 7);
}

#[tokio::test]
async fn open_stream_enforces_client_stream_cap() {
    let (port, _h) = echo_pong_server().await;
    let t = WsTransport::connect(TransportConfig {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        max_streams_per_conn: 1,
        ..TransportConfig::default()
    })
    .await
    .unwrap();
    let _first = t.open_stream("example.com:443").await.unwrap();
    let err = t.open_stream("example.com:443").await.unwrap_err();
    assert!(matches!(err, TunnelError::StreamCapExceeded));
}
