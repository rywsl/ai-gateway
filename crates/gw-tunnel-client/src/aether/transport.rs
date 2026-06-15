use crate::aether::frame::{flags, Frame, MsgType};
use crate::aether::heartbeat::{HeartbeatAck, HeartbeatData};
use crate::aether::meta::{RequestMeta, ResponseMeta};
use crate::aether::redis_sink::{HeartbeatEvent, HeartbeatSink, NoopSink};
use crate::core::{StreamHandle, Transport, TransportError, TransportFuture};
use crate::resolver::ProxyNodeView;
use crate::target::StreamTarget;
use arc_swap::{ArcSwap, ArcSwapOption};
use bytes::{Buf, Bytes, BytesMut};
use dashmap::DashMap;
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use gw_core::tunnel_protocol::TunnelProtocol;
use secrecy::SecretString;
use std::io::Cursor;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub heartbeat_interval: Duration,
    pub missed_heartbeat_threshold: u32,
    pub max_streams_per_conn: u32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(30),
            missed_heartbeat_threshold: 3,
            max_streams_per_conn: 256,
        }
    }
}

pub struct WsTransport {
    inner: Arc<Inner>,
}

struct Inner {
    cfg: TransportConfig,
    _node_id: String,
    streams: DashMap<u32, StreamSlot>,
    next_stream_id: AtomicU32,
    write_tx: mpsc::Sender<Frame>,
    alive: AtomicBool,
    last_heartbeat_at: ArcSwapOption<Instant>,
    _auth_token: Option<SecretString>,
    heartbeat_session_id: ArcSwap<String>,
    heartbeat_sink: Arc<dyn HeartbeatSink>,
}

struct StreamSlot {
    body_tx_response: mpsc::Sender<Bytes>,
    meta_tx_response: Option<oneshot::Sender<ResponseMeta>>,
}

impl WsTransport {
    pub async fn attach<S>(
        cfg: TransportConfig,
        node_view: ProxyNodeView,
        upgraded_ws: S,
        heartbeat_sink: Arc<dyn HeartbeatSink>,
    ) -> Result<Self, TransportError>
    where
        S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Sink<Message, Error = tokio_tungstenite::tungstenite::Error>
            + Unpin
            + Send
            + 'static,
    {
        let (sink, stream) = upgraded_ws.split();
        let (write_tx, write_rx) = mpsc::channel::<Frame>(64);
        let inner = Arc::new(Inner {
            cfg,
            _node_id: node_view.id,
            streams: DashMap::new(),
            next_stream_id: AtomicU32::new(2),
            write_tx,
            alive: AtomicBool::new(true),
            last_heartbeat_at: ArcSwapOption::from(None),
            _auth_token: node_view.auth_token,
            heartbeat_session_id: ArcSwap::from_pointee(String::new()),
            heartbeat_sink,
        });

        tokio::spawn(write_loop(inner.clone(), sink, write_rx));
        tokio::spawn(read_loop(inner.clone(), stream));
        tokio::spawn(liveness_loop(inner.clone()));

        Ok(Self { inner })
    }

    pub fn disconnected_for_test() -> Self {
        let (write_tx, _write_rx) = mpsc::channel::<Frame>(1);
        Self {
            inner: Arc::new(Inner {
                cfg: TransportConfig::default(),
                _node_id: "test-node".to_string(),
                streams: DashMap::new(),
                next_stream_id: AtomicU32::new(2),
                write_tx,
                alive: AtomicBool::new(false),
                last_heartbeat_at: ArcSwapOption::from(None),
                _auth_token: None,
                heartbeat_session_id: ArcSwap::from_pointee(String::new()),
                heartbeat_sink: Arc::new(NoopSink),
            }),
        }
    }

    pub fn allocate_stream_id_for_test(&self) -> u32 {
        self.allocate_stream_id()
    }

    fn allocate_stream_id(&self) -> u32 {
        self.inner.next_stream_id.fetch_add(2, Ordering::AcqRel)
    }
}

impl Transport for WsTransport {
    fn open_stream<'a>(&'a self, target: StreamTarget) -> TransportFuture<'a> {
        Box::pin(async move {
            let StreamTarget::Http {
                method,
                url,
                headers,
                body_rx,
                body_tx_response,
                meta_tx_response,
            } = target
            else {
                return Err(TransportError::UnsupportedTarget);
            };
            if !self.alive() {
                return Err(TransportError::NotAlive);
            }

            let stream_id = self.allocate_stream_id();
            self.inner.streams.insert(
                stream_id,
                StreamSlot {
                    body_tx_response,
                    meta_tx_response: Some(meta_tx_response),
                },
            );
            let meta = RequestMeta {
                method: method.to_string(),
                url: url.to_string(),
                headers: headers.into_iter().collect(),
                timeout: 60,
            };
            let payload =
                serde_json::to_vec(&meta).map_err(|err| TransportError::Send(err.to_string()))?;
            self.inner
                .write_tx
                .send(Frame::new(
                    stream_id,
                    MsgType::RequestHeaders,
                    flags::NONE,
                    Bytes::from(payload),
                ))
                .await
                .map_err(|_| TransportError::Closed)?;
            spawn_body_pump(stream_id, body_rx, self.inner.clone());

            // PHASE 2 PR 2b TODO: tunnel_bridge will consume the response mpsc
            // channels directly; this placeholder preserves the object-safe trait.
            Ok(Box::new(EmptyTunnelStream) as StreamHandle)
        })
    }

    fn alive(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
    }

    fn last_heartbeat_age(&self) -> Duration {
        self.inner
            .last_heartbeat_at
            .load_full()
            .map(|at| at.elapsed())
            .unwrap_or(Duration::MAX)
    }

    fn active_streams(&self) -> u32 {
        self.inner.streams.len().try_into().unwrap_or(u32::MAX)
    }

    fn tunnel_protocol(&self) -> TunnelProtocol {
        TunnelProtocol::Aether
    }

    fn close(&self) {
        self.inner.alive.store(false, Ordering::Release);
    }
}

async fn write_loop<S>(inner: Arc<Inner>, mut sink: S, mut write_rx: mpsc::Receiver<Frame>)
where
    S: Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    while let Some(frame) = write_rx.recv().await {
        let mut out = BytesMut::new();
        frame.encode(&mut out);
        if sink.send(Message::Binary(out.to_vec())).await.is_err() {
            inner.alive.store(false, Ordering::Release);
            break;
        }
    }
}

async fn read_loop<S>(inner: Arc<Inner>, mut stream: S)
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut buffer = BytesMut::new();
    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(_) => {
                inner.alive.store(false, Ordering::Release);
                break;
            }
        };
        let Message::Binary(bin) = msg else {
            continue;
        };
        buffer.extend_from_slice(&bin);
        loop {
            let mut cursor = Cursor::new(&buffer[..]);
            let frame = match Frame::decode(&mut cursor) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(_) => {
                    inner.alive.store(false, Ordering::Release);
                    return;
                }
            };
            let consumed = cursor.position() as usize;
            buffer.advance(consumed);
            dispatch_frame(inner.clone(), frame).await;
        }
    }
    inner.alive.store(false, Ordering::Release);
}

async fn dispatch_frame(inner: Arc<Inner>, frame: Frame) {
    match frame.msg_type {
        MsgType::ResponseHeaders => {
            if let Some(mut slot) = inner.streams.get_mut(&frame.stream_id) {
                let payload = decode_payload(&frame).unwrap_or_else(|_| frame.payload.clone());
                if let Ok(meta) = serde_json::from_slice::<ResponseMeta>(&payload) {
                    if let Some(tx) = slot.meta_tx_response.take() {
                        let _ = tx.send(meta);
                    }
                }
            }
        }
        MsgType::ResponseBody => {
            if let Some(slot) = inner.streams.get(&frame.stream_id) {
                let payload = decode_payload(&frame).unwrap_or_else(|_| frame.payload.clone());
                let _ = slot.body_tx_response.send(payload).await;
            }
        }
        MsgType::StreamEnd | MsgType::StreamError => {
            inner.streams.remove(&frame.stream_id);
        }
        MsgType::HeartbeatData => {
            if let Ok(payload) = decode_payload(&frame) {
                if let Ok(data) = serde_json::from_slice::<HeartbeatData>(&payload) {
                    handle_heartbeat(inner, data).await;
                }
            }
        }
        MsgType::GoAway => {
            inner.alive.store(false, Ordering::Release);
        }
        MsgType::Ping => {
            let _ = inner
                .write_tx
                .send(Frame::control(MsgType::Pong, frame.payload))
                .await;
        }
        MsgType::RequestHeaders | MsgType::RequestBody | MsgType::Pong | MsgType::HeartbeatAck => {}
    }
}

async fn handle_heartbeat(inner: Arc<Inner>, data: HeartbeatData) {
    inner
        .last_heartbeat_at
        .store(Some(Arc::new(Instant::now())));
    inner
        .heartbeat_session_id
        .store(Arc::new(data.heartbeat_session_id.clone()));

    if let Ok(payload) = serde_json::to_vec(&HeartbeatAck {
        heartbeat_id: data.heartbeat_id,
    }) {
        let _ = inner
            .write_tx
            .send(Frame::control(MsgType::HeartbeatAck, Bytes::from(payload)))
            .await;
    }

    let event = HeartbeatEvent {
        node_id: data.node_id,
        heartbeat_session_id: data.heartbeat_session_id,
        heartbeat_id: data.heartbeat_id,
        received_at_ms: now_ms(),
        active_connections: data.active_connections,
        total_requests: data.total_requests,
        avg_latency_ms: data.avg_latency_ms.unwrap_or(0.0),
        failed_requests: data.failed_requests,
        dns_failures: data.dns_failures,
        stream_errors: data.stream_errors,
        proxy_version: data.proxy_metadata.version,
    };
    let sink = inner.heartbeat_sink.clone();
    tokio::spawn(async move {
        sink.push(event).await;
    });
}

async fn liveness_loop(inner: Arc<Inner>) {
    let threshold = inner.cfg.missed_heartbeat_threshold.max(1);
    let mut tick = tokio::time::interval(inner.cfg.heartbeat_interval);
    loop {
        tick.tick().await;
        if !inner.alive.load(Ordering::Acquire) {
            break;
        }
        let Some(last) = inner.last_heartbeat_at.load_full() else {
            continue;
        };
        if last.elapsed() >= inner.cfg.heartbeat_interval * threshold {
            inner.alive.store(false, Ordering::Release);
            break;
        }
    }
}

fn spawn_body_pump(stream_id: u32, mut body_rx: mpsc::Receiver<Bytes>, inner: Arc<Inner>) {
    tokio::spawn(async move {
        while let Some(chunk) = body_rx.recv().await {
            let (payload, did_compress) =
                crate::aether::gzip::maybe_compress(chunk).unwrap_or((Bytes::new(), false));
            let frame_flags = if did_compress {
                flags::GZIP_COMPRESSED
            } else {
                flags::NONE
            };
            if inner
                .write_tx
                .send(Frame::new(
                    stream_id,
                    MsgType::RequestBody,
                    frame_flags,
                    payload,
                ))
                .await
                .is_err()
            {
                inner.alive.store(false, Ordering::Release);
                return;
            }
        }
        let _ = inner
            .write_tx
            .send(Frame::new(
                stream_id,
                MsgType::StreamEnd,
                flags::END_STREAM,
                Bytes::new(),
            ))
            .await;
    });
}

fn decode_payload(frame: &Frame) -> Result<Bytes, std::io::Error> {
    if frame.is_gzip() {
        crate::aether::gzip::decompress(&frame.payload)
    } else {
        Ok(frame.payload.clone())
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

struct EmptyTunnelStream;

impl AsyncRead for EmptyTunnelStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for EmptyTunnelStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Ok(0))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
