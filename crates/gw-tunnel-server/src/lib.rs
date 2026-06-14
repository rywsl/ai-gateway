//! First-party `ailinkmax-tunnel` server.

use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use gw_tunnel_client::cnexus::frame::{
    decode_frame, encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN,
};
use serde::Serialize;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Mutex, Semaphore};
use tokio_tungstenite::tungstenite::Message;

const PROTOCOL_NAME: &str = "ailinkmax-tunnel";
const PROTOCOL_VERSION: &str = "1.0.0";

#[derive(Debug, Clone)]
pub struct TunnelServerConfig {
    pub bind: SocketAddr,
    pub auth_token: Option<String>,
    pub max_streams_per_conn: usize,
    pub node_id: String,
    pub allowed_targets: Vec<(String, u16)>,
    pub allow_private_targets: bool,
}

impl Default for TunnelServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:0".parse().unwrap(),
            auth_token: None,
            max_streams_per_conn: 256,
            node_id: "gw-tunnel-server".to_owned(),
            allowed_targets: Vec::new(),
            allow_private_targets: false,
        }
    }
}

pub struct TunnelServerHandle {
    pub local_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TunnelServerHandle {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for TunnelServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub async fn spawn(config: TunnelServerConfig) -> anyhow::Result<TunnelServerHandle> {
    let listener = TcpListener::bind(config.bind).await?;
    let local_addr = listener.local_addr()?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let cfg = Arc::new(config);
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else {
                        continue;
                    };
                    let cfg = cfg.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_ws_conn(stream, cfg).await {
                            tracing::debug!(error = ?err, "tunnel connection ended");
                        }
                    });
                }
            }
        }
    });
    Ok(TunnelServerHandle {
        local_addr,
        shutdown: Some(shutdown_tx),
        task: Some(task),
    })
}

type Ws = tokio_tungstenite::WebSocketStream<TcpStream>;
type WsSink = futures_util::stream::SplitSink<Ws, Message>;

#[derive(Clone)]
struct SharedSink(Arc<Mutex<WsSink>>);

impl SharedSink {
    async fn send_frame(&self, frame: Frame) -> anyhow::Result<()> {
        let mut buf = BytesMut::new();
        encode_frame(&frame, &mut buf);
        self.0
            .lock()
            .await
            .send(Message::Binary(buf.to_vec()))
            .await?;
        Ok(())
    }

    async fn send_error(&self, stream_id: u32, text: impl Into<Bytes>) {
        let _ = self
            .send_frame(Frame {
                msg_type: MsgType::Error,
                flags: 0,
                stream_id,
                payload: text.into(),
            })
            .await;
    }
}

#[derive(Serialize)]
struct NodeInfo<'a> {
    protocol: &'a str,
    version: &'a str,
    node_id: &'a str,
    max_streams_per_conn: usize,
    capabilities: [&'a str; 3],
}

async fn handle_ws_conn(stream: TcpStream, cfg: Arc<TunnelServerConfig>) -> anyhow::Result<()> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (sink, mut src) = ws.split();
    let sink = SharedSink(Arc::new(Mutex::new(sink)));

    if let Some(token) = cfg.auth_token.as_deref() {
        let Some(frame) = next_frame(&mut src).await? else {
            return Ok(());
        };
        if frame.msg_type != MsgType::ControlAuth || frame.payload.as_ref() != token.as_bytes() {
            sink.send_error(0, Bytes::from_static(b"authentication failed"))
                .await;
            return Ok(());
        }
    }

    let info = serde_json::to_vec(&NodeInfo {
        protocol: PROTOCOL_NAME,
        version: PROTOCOL_VERSION,
        node_id: cfg.node_id.as_str(),
        max_streams_per_conn: cfg.max_streams_per_conn,
        capabilities: ["tcp-relay", "chunked-data", "target-filter"],
    })?;
    sink.send_frame(Frame {
        msg_type: MsgType::ControlNodeInfo,
        flags: 0,
        stream_id: 0,
        payload: Bytes::from(info),
    })
    .await?;

    let permits = Arc::new(Semaphore::new(cfg.max_streams_per_conn.max(1)));
    let streams: Arc<Mutex<HashMap<u32, mpsc::Sender<Bytes>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    while let Some(frame) = next_frame(&mut src).await? {
        match frame.msg_type {
            MsgType::Ping => {
                sink.send_frame(Frame {
                    msg_type: MsgType::Pong,
                    flags: 0,
                    stream_id: frame.stream_id,
                    payload: frame.payload,
                })
                .await?;
            }
            MsgType::Open => {
                let target = match std::str::from_utf8(&frame.payload) {
                    Ok(target) => target.to_owned(),
                    Err(_) => {
                        sink.send_error(frame.stream_id, Bytes::from_static(b"target not utf-8"))
                            .await;
                        continue;
                    }
                };
                if let Err(err) =
                    validate_target(&target, &cfg.allowed_targets, cfg.allow_private_targets)
                {
                    sink.send_error(frame.stream_id, Bytes::from(err.to_string()))
                        .await;
                    continue;
                }
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    sink.send_error(frame.stream_id, Bytes::from_static(b"stream cap exceeded"))
                        .await;
                    continue;
                };
                let (tx, rx) = mpsc::channel(64);
                streams.lock().await.insert(frame.stream_id, tx);
                tokio::spawn(relay_stream(
                    frame.stream_id,
                    target,
                    rx,
                    sink.clone(),
                    streams.clone(),
                    permit,
                ));
            }
            MsgType::Data => {
                if let Some(tx) = streams.lock().await.get(&frame.stream_id).cloned() {
                    let _ = tx.send(frame.payload).await;
                } else {
                    sink.send_error(frame.stream_id, Bytes::from_static(b"unknown stream"))
                        .await;
                }
            }
            MsgType::Close | MsgType::Error => {
                streams.lock().await.remove(&frame.stream_id);
            }
            MsgType::Pong | MsgType::ControlAuth | MsgType::ControlNodeInfo => {}
        }
    }
    Ok(())
}

async fn next_frame(
    src: &mut futures_util::stream::SplitStream<Ws>,
) -> anyhow::Result<Option<Frame>> {
    while let Some(msg) = src.next().await {
        match msg? {
            Message::Binary(bin) => {
                if let Some((frame, _)) = decode_frame(&bin)? {
                    return Ok(Some(frame));
                }
            }
            Message::Close(_) => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

async fn relay_stream(
    stream_id: u32,
    target: String,
    mut rx: mpsc::Receiver<Bytes>,
    sink: SharedSink,
    streams: Arc<Mutex<HashMap<u32, mpsc::Sender<Bytes>>>>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let tcp = match TcpStream::connect(&target).await {
        Ok(tcp) => tcp,
        Err(err) => {
            sink.send_error(stream_id, Bytes::from(format!("connect failed: {err}")))
                .await;
            streams.lock().await.remove(&stream_id);
            return;
        }
    };
    let (mut reader, mut writer) = tokio::io::split(tcp);
    let read_sink = sink.clone();
    let read_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) => {
                    let _ = read_sink
                        .send_frame(Frame {
                            msg_type: MsgType::Close,
                            flags: 0,
                            stream_id,
                            payload: Bytes::new(),
                        })
                        .await;
                    break;
                }
                Ok(n) => {
                    for chunk in buf[..n].chunks(MAX_PAYLOAD_LEN) {
                        if read_sink
                            .send_frame(Frame {
                                msg_type: MsgType::Data,
                                flags: 0,
                                stream_id,
                                payload: Bytes::copy_from_slice(chunk),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(err) => {
                    read_sink
                        .send_error(stream_id, Bytes::from(format!("read failed: {err}")))
                        .await;
                    break;
                }
            }
        }
    });

    while let Some(bytes) = rx.recv().await {
        if writer.write_all(&bytes).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
    read_task.abort();
    streams.lock().await.remove(&stream_id);
}

#[derive(Debug, thiserror::Error)]
enum TargetError {
    #[error("target must be host:port")]
    BadShape,
    #[error("target host is blocked")]
    BlockedHost,
    #[error("target denied")]
    Denied,
}

fn validate_target(
    target: &str,
    allowed_targets: &[(String, u16)],
    allow_private_targets: bool,
) -> Result<(), TargetError> {
    let (host, port) = parse_target(target)?;
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !allow_private_targets && gw_tunnel_client::safe_dns::is_ip_blocked(ip) {
            return Err(TargetError::BlockedHost);
        }
    }
    if !allowed_targets.is_empty() && !allowed_targets.iter().any(|(h, p)| h == host && *p == port)
    {
        return Err(TargetError::Denied);
    }
    Ok(())
}

fn parse_target(target: &str) -> Result<(&str, u16), TargetError> {
    let (host, port) = target.rsplit_once(':').ok_or(TargetError::BadShape)?;
    if host.is_empty() {
        return Err(TargetError::BadShape);
    }
    let port = port.parse::<u16>().map_err(|_| TargetError::BadShape)?;
    Ok((host.trim_matches(['[', ']']), port))
}
