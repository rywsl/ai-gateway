//! WebSocket transport — heartbeat + alive-tracking layer over the frame
//! protocol. Reconnect orchestration lives in `pool::ReconnectingTransport`.

use crate::frame::{decode_frame, encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN};
use bytes::{Buf, Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::tungstenite::Message;
use url::Url;

#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub url: Url,
    pub auth_token: Option<SecretString>,
    pub heartbeat_interval: Duration,
    pub missed_pong_threshold: u32,
    pub max_streams_per_conn: u32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:0/".parse().unwrap(),
            auth_token: None,
            heartbeat_interval: Duration::from_secs(30),
            missed_pong_threshold: 3,
            max_streams_per_conn: 256,
        }
    }
}

pub struct WsTransport {
    alive: Arc<AtomicBool>,
    last_pong_ms: Arc<AtomicU64>,
    next_stream_id: Arc<AtomicU32>,
    routes: Arc<Mutex<HashMap<u32, mpsc::Sender<TunnelEvent>>>>,
    stream_permits: Arc<Semaphore>,
    out_tx: mpsc::Sender<Frame>,
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("transport is not alive")]
    NotAlive,
    #[error("stream cap exceeded")]
    StreamCapExceeded,
    #[error("stream closed")]
    Closed,
    #[error("remote tunnel error: {0}")]
    Remote(String),
    #[error("send failed: {0}")]
    Send(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelEvent {
    Data(Bytes),
    Close,
    Error(String),
}

pub struct TunnelStream {
    stream_id: u32,
    out_tx: mpsc::Sender<Frame>,
    rx: Option<mpsc::Receiver<TunnelEvent>>,
    routes: Arc<Mutex<HashMap<u32, mpsc::Sender<TunnelEvent>>>>,
    permit: Option<OwnedSemaphorePermit>,
    detached: bool,
}

pub struct TunnelStreamReader {
    stream_id: u32,
    rx: mpsc::Receiver<TunnelEvent>,
    read_buf: Option<Bytes>,
    routes: Arc<Mutex<HashMap<u32, mpsc::Sender<TunnelEvent>>>>,
    _permit: OwnedSemaphorePermit,
}

pub struct TunnelStreamWriter {
    stream_id: u32,
    out_tx: mpsc::Sender<Frame>,
}

impl std::fmt::Debug for TunnelStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TunnelStream")
            .field("stream_id", &self.stream_id)
            .finish_non_exhaustive()
    }
}

impl TunnelStream {
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub async fn send_data(&self, payload: Bytes) -> Result<(), TunnelError> {
        send_data_frames(&self.out_tx, self.stream_id, payload).await
    }

    pub async fn close(&self) -> Result<(), TunnelError> {
        self.out_tx
            .send(Frame {
                msg_type: MsgType::Close,
                flags: 0,
                stream_id: self.stream_id,
                payload: Bytes::new(),
            })
            .await
            .map_err(|e| TunnelError::Send(e.to_string()))
    }

    pub async fn recv(&mut self) -> Result<TunnelEvent, TunnelError> {
        let Some(rx) = self.rx.as_mut() else {
            return Err(TunnelError::Closed);
        };
        match rx.recv().await {
            Some(TunnelEvent::Error(err)) => Err(TunnelError::Remote(err)),
            Some(event) => Ok(event),
            None => Err(TunnelError::Closed),
        }
    }

    pub fn split(mut self) -> (TunnelStreamReader, TunnelStreamWriter) {
        let rx = self.rx.take().expect("TunnelStream split once");
        let permit = self.permit.take().expect("TunnelStream split once");
        self.detached = true;
        (
            TunnelStreamReader {
                stream_id: self.stream_id,
                rx,
                read_buf: None,
                routes: self.routes.clone(),
                _permit: permit,
            },
            TunnelStreamWriter {
                stream_id: self.stream_id,
                out_tx: self.out_tx.clone(),
            },
        )
    }
}

impl Drop for TunnelStream {
    fn drop(&mut self) {
        if self.detached {
            return;
        }
        let routes = self.routes.clone();
        let stream_id = self.stream_id;
        tokio::spawn(async move {
            routes.lock().await.remove(&stream_id);
        });
    }
}

impl Drop for TunnelStreamReader {
    fn drop(&mut self) {
        let routes = self.routes.clone();
        let stream_id = self.stream_id;
        tokio::spawn(async move {
            routes.lock().await.remove(&stream_id);
        });
    }
}

impl TunnelStreamWriter {
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub async fn send_data(&self, payload: Bytes) -> Result<(), TunnelError> {
        send_data_frames(&self.out_tx, self.stream_id, payload).await
    }

    pub async fn close(&self) -> Result<(), TunnelError> {
        self.out_tx
            .send(Frame {
                msg_type: MsgType::Close,
                flags: 0,
                stream_id: self.stream_id,
                payload: Bytes::new(),
            })
            .await
            .map_err(|e| TunnelError::Send(e.to_string()))
    }
}

impl AsyncRead for TunnelStreamReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        loop {
            if let Some(mut bytes) = self.read_buf.take() {
                let n = bytes.len().min(buf.remaining());
                buf.put_slice(&bytes[..n]);
                if n < bytes.len() {
                    bytes.advance(n);
                    self.read_buf = Some(bytes);
                }
                return Poll::Ready(Ok(()));
            }
            match Pin::new(&mut self.rx).poll_recv(cx) {
                Poll::Ready(Some(TunnelEvent::Data(bytes))) => {
                    self.read_buf = Some(bytes);
                }
                Poll::Ready(Some(TunnelEvent::Close)) | Poll::Ready(None) => {
                    return Poll::Ready(Ok(()));
                }
                Poll::Ready(Some(TunnelEvent::Error(err))) => {
                    return Poll::Ready(Err(std::io::Error::new(std::io::ErrorKind::Other, err)));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl AsyncWrite for TunnelStreamWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let n = buf.len().min(MAX_PAYLOAD_LEN);
        match self.out_tx.try_send(Frame {
            msg_type: MsgType::Data,
            flags: 0,
            stream_id: self.stream_id,
            payload: Bytes::copy_from_slice(&buf[..n]),
        }) {
            Ok(()) => Poll::Ready(Ok(n)),
            Err(mpsc::error::TrySendError::Full(_)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "tunnel send queue full",
            ))),
            Err(mpsc::error::TrySendError::Closed(_)) => Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "tunnel transport closed",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let _ = self.out_tx.try_send(Frame {
            msg_type: MsgType::Close,
            flags: 0,
            stream_id: self.stream_id,
            payload: Bytes::new(),
        });
        Poll::Ready(Ok(()))
    }
}

impl WsTransport {
    pub async fn connect(cfg: TransportConfig) -> anyhow::Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(cfg.url.as_str()).await?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(64);
        let alive = Arc::new(AtomicBool::new(true));
        let last_pong_ms = Arc::new(AtomicU64::new(now_ms()));
        let missed = Arc::new(AtomicU32::new(0));
        let routes: Arc<Mutex<HashMap<u32, mpsc::Sender<TunnelEvent>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let stream_permits = Arc::new(Semaphore::new(cfg.max_streams_per_conn.max(1) as usize));

        {
            let alive = alive.clone();
            tokio::spawn(async move {
                while let Some(frame) = out_rx.recv().await {
                    let mut buf = BytesMut::new();
                    encode_frame(&frame, &mut buf);
                    if sink.send(Message::Binary(buf.to_vec())).await.is_err() {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                }
            });
        }

        if let Some(token) = cfg.auth_token.as_ref() {
            out_tx
                .send(Frame {
                    msg_type: MsgType::ControlAuth,
                    flags: 0,
                    stream_id: 0,
                    payload: Bytes::copy_from_slice(token.expose_secret().as_bytes()),
                })
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        }

        {
            let alive = alive.clone();
            let last_pong_ms = last_pong_ms.clone();
            let missed = missed.clone();
            let routes = routes.clone();
            let pong_tx = out_tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = stream.next().await {
                    let m = match msg {
                        Ok(m) => m,
                        Err(_) => {
                            alive.store(false, Ordering::Release);
                            break;
                        }
                    };
                    if let Message::Binary(bin) = m {
                        match decode_frame(&bin) {
                            Ok(Some((f, _))) => match f.msg_type {
                                MsgType::Pong => {
                                    last_pong_ms.store(now_ms(), Ordering::Release);
                                    missed.store(0, Ordering::Release);
                                }
                                MsgType::Data | MsgType::Close | MsgType::Error => {
                                    let tx = routes.lock().await.get(&f.stream_id).cloned();
                                    if let Some(tx) = tx {
                                        let event = match f.msg_type {
                                            MsgType::Data => TunnelEvent::Data(f.payload),
                                            MsgType::Close => TunnelEvent::Close,
                                            MsgType::Error => TunnelEvent::Error(
                                                String::from_utf8_lossy(&f.payload).to_string(),
                                            ),
                                            _ => unreachable!(),
                                        };
                                        let terminal = matches!(
                                            event,
                                            TunnelEvent::Close | TunnelEvent::Error(_)
                                        );
                                        let _ = tx.send(event).await;
                                        if terminal {
                                            routes.lock().await.remove(&f.stream_id);
                                        }
                                    }
                                }
                                MsgType::Ping => {
                                    // Server-originated pings are answered by the client.
                                    let _ = pong_tx
                                        .send(Frame {
                                            msg_type: MsgType::Pong,
                                            flags: 0,
                                            stream_id: f.stream_id,
                                            payload: f.payload,
                                        })
                                        .await;
                                }
                                MsgType::ControlNodeInfo | MsgType::Open | MsgType::ControlAuth => {
                                }
                            },
                            Ok(None) => {}
                            Err(_) => {
                                alive.store(false, Ordering::Release);
                                break;
                            }
                        }
                    }
                }
                alive.store(false, Ordering::Release);
            });
        }

        {
            let out_tx = out_tx.clone();
            let alive = alive.clone();
            let missed = missed.clone();
            let interval = cfg.heartbeat_interval;
            let threshold = cfg.missed_pong_threshold;
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                let mut seq: u32 = 0;
                loop {
                    tick.tick().await;
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }
                    seq = seq.wrapping_add(1);
                    let f = Frame {
                        msg_type: MsgType::Ping,
                        flags: 0,
                        stream_id: seq,
                        payload: Bytes::from_static(b"hb"),
                    };
                    if out_tx.send(f).await.is_err() {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                    let m = missed.fetch_add(1, Ordering::AcqRel) + 1;
                    if m >= threshold {
                        alive.store(false, Ordering::Release);
                        break;
                    }
                }
            });
        }

        Ok(Self {
            alive,
            last_pong_ms,
            next_stream_id: Arc::new(AtomicU32::new(1)),
            routes,
            stream_permits,
            out_tx,
        })
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    pub fn last_pong_age(&self) -> Duration {
        Duration::from_millis(now_ms().saturating_sub(self.last_pong_ms.load(Ordering::Acquire)))
    }

    pub async fn send_open(&self, target: &str) -> anyhow::Result<u32> {
        let id = self.next_stream_id.fetch_add(1, Ordering::AcqRel);
        self.send_open_with_id(id, target)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(id)
    }

    pub async fn open_stream(&self, target: &str) -> Result<TunnelStream, TunnelError> {
        if !self.is_alive() {
            return Err(TunnelError::NotAlive);
        }
        let permit = self
            .stream_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| TunnelError::StreamCapExceeded)?;
        let id = self.next_stream_id.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = mpsc::channel(64);
        self.routes.lock().await.insert(id, tx);
        if let Err(err) = self.send_open_with_id(id, target).await {
            self.routes.lock().await.remove(&id);
            return Err(err);
        }
        Ok(TunnelStream {
            stream_id: id,
            out_tx: self.out_tx.clone(),
            rx: Some(rx),
            routes: self.routes.clone(),
            permit: Some(permit),
            detached: false,
        })
    }

    async fn send_open_with_id(&self, id: u32, target: &str) -> Result<(), TunnelError> {
        let f = Frame {
            msg_type: MsgType::Open,
            flags: 0,
            stream_id: id,
            payload: Bytes::copy_from_slice(target.as_bytes()),
        };
        self.out_tx
            .send(f)
            .await
            .map_err(|e| TunnelError::Send(e.to_string()))
    }

    pub async fn send_data(&self, stream_id: u32, payload: Bytes) -> anyhow::Result<()> {
        send_data_frames(&self.out_tx, stream_id, payload)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

async fn send_data_frames(
    out_tx: &mpsc::Sender<Frame>,
    stream_id: u32,
    payload: Bytes,
) -> Result<(), TunnelError> {
    if payload.len() <= MAX_PAYLOAD_LEN {
        return out_tx
            .send(Frame {
                msg_type: MsgType::Data,
                flags: 0,
                stream_id,
                payload,
            })
            .await
            .map_err(|e| TunnelError::Send(e.to_string()));
    }
    for chunk in payload.chunks(MAX_PAYLOAD_LEN) {
        out_tx
            .send(Frame {
                msg_type: MsgType::Data,
                flags: 0,
                stream_id,
                payload: Bytes::copy_from_slice(chunk),
            })
            .await
            .map_err(|e| TunnelError::Send(e.to_string()))?;
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
