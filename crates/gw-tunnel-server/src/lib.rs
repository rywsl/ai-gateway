//! First-party `ailinkmax-tunnel` server.

use anyhow::{anyhow, Context};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use gw_tunnel_client::cnexus::frame::{
    decode_frame, encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN,
};
use percent_encoding::percent_decode_str;
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

/// Upstream egress mode for tunnel-server outbound target dials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EgressMode {
    /// Dial targets directly.
    Direct,
    /// Dial targets through a SOCKS5 proxy.
    ///
    /// `resolve_at_proxy = true` is `socks5h`; hostnames are sent to the
    /// proxy. `false` is `socks5`; hostnames are resolved locally first.
    Socks5 {
        proxy_host: String,
        proxy_port: u16,
        auth: Option<(String, String)>,
        resolve_at_proxy: bool,
    },
}

impl Default for EgressMode {
    fn default() -> Self {
        Self::Direct
    }
}

#[derive(Debug, Clone)]
pub struct TunnelServerConfig {
    pub bind: SocketAddr,
    pub auth_token: Option<String>,
    pub max_streams_per_conn: usize,
    pub node_id: String,
    pub allowed_targets: Vec<(String, u16)>,
    pub allow_private_targets: bool,
    pub egress: EgressMode,
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
            egress: EgressMode::Direct,
        }
    }
}

/// Parse deployment-level SOCKS5 egress configuration.
///
/// `None` and empty strings keep direct egress. Supported URL schemes are
/// `socks5` and `socks5h`; `http`/`https` proxying is intentionally out of
/// scope for Phase 4.
pub fn parse_egress_url(value: Option<&str>) -> anyhow::Result<EgressMode> {
    let Some(raw) = value else {
        return Ok(EgressMode::Direct);
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(EgressMode::Direct);
    }

    let parsed = url::Url::parse(raw).with_context(|| format!("invalid proxy_url {raw:?}"))?;
    let resolve_at_proxy = match parsed.scheme() {
        "socks5" => false,
        "socks5h" => true,
        other => {
            return Err(anyhow!(
                "proxy_url scheme {other:?} is not supported by gw-tunnel-server"
            ))
        }
    };
    let proxy_host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("proxy_url missing host"))?
        .to_owned();
    let proxy_port = parsed.port().unwrap_or(1080);
    let auth = if parsed.username().is_empty() {
        None
    } else {
        let username = percent_decode_str(parsed.username())
            .decode_utf8_lossy()
            .into_owned();
        let password = parsed
            .password()
            .map(|password| {
                percent_decode_str(password)
                    .decode_utf8_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        Some((username, password))
    };

    Ok(EgressMode::Socks5 {
        proxy_host,
        proxy_port,
        auth,
        resolve_at_proxy,
    })
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
                    cfg.egress.clone(),
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
    egress: EgressMode,
    mut rx: mpsc::Receiver<Bytes>,
    sink: SharedSink,
    streams: Arc<Mutex<HashMap<u32, mpsc::Sender<Bytes>>>>,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let tcp = match dial_via_egress(&egress, &target).await {
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

/// Dial `target` through the configured egress mode.
#[doc(hidden)]
pub async fn dial_via_egress(egress: &EgressMode, target: &str) -> anyhow::Result<TcpStream> {
    match egress {
        EgressMode::Direct => TcpStream::connect(target)
            .await
            .with_context(|| format!("direct connect to {target}")),
        EgressMode::Socks5 {
            proxy_host,
            proxy_port,
            auth,
            resolve_at_proxy,
        } => {
            use tokio_socks::tcp::Socks5Stream;

            let proxy_addr = tokio::net::lookup_host((proxy_host.as_str(), *proxy_port))
                .await
                .with_context(|| format!("resolving proxy host {proxy_host}:{proxy_port}"))?
                .next()
                .ok_or_else(|| {
                    anyhow!("DNS yielded no addresses for proxy {proxy_host}:{proxy_port}")
                })?;

            if *resolve_at_proxy {
                let stream = match auth {
                    Some((username, password)) => {
                        Socks5Stream::connect_with_password(
                            proxy_addr,
                            target,
                            username.as_str(),
                            password.as_str(),
                        )
                        .await
                    }
                    None => Socks5Stream::connect(proxy_addr, target).await,
                }
                .with_context(|| format!("socks5h dial to {target} via {proxy_addr}"))?;
                Ok(stream.into_inner())
            } else {
                let resolved = tokio::net::lookup_host(target)
                    .await
                    .with_context(|| format!("local DNS resolve of {target}"))?
                    .next()
                    .ok_or_else(|| anyhow!("DNS yielded no addresses for {target}"))?;
                let stream = match auth {
                    Some((username, password)) => {
                        Socks5Stream::connect_with_password(
                            proxy_addr,
                            resolved,
                            username.as_str(),
                            password.as_str(),
                        )
                        .await
                    }
                    None => Socks5Stream::connect(proxy_addr, resolved).await,
                }
                .with_context(|| format!("socks5 dial to {resolved} via {proxy_addr}"))?;
                Ok(stream.into_inner())
            }
        }
    }
}

#[doc(hidden)]
pub mod testing {
    pub use super::dial_via_egress;
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
