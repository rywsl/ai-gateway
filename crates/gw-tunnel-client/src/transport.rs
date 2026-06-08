//! WebSocket transport — heartbeat + alive-tracking layer over the frame
//! protocol. Reconnect orchestration lives in `pool::ReconnectingTransport`.

use crate::frame::{decode_frame, encode_frame, Frame, MsgType};
use bytes::{Bytes, BytesMut};
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, SecretString};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
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
    out_tx: mpsc::Sender<Frame>,
}

impl WsTransport {
    pub async fn connect(cfg: TransportConfig) -> anyhow::Result<Self> {
        let (ws, _) = tokio_tungstenite::connect_async(cfg.url.as_str()).await?;
        let (mut sink, mut stream) = ws.split();
        let (out_tx, mut out_rx) = mpsc::channel::<Frame>(64);
        let alive = Arc::new(AtomicBool::new(true));
        let last_pong_ms = Arc::new(AtomicU64::new(now_ms()));
        let missed = Arc::new(AtomicU32::new(0));

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
                        if let Ok(Some((f, _))) = decode_frame(&bin) {
                            if f.msg_type == MsgType::Pong {
                                last_pong_ms.store(now_ms(), Ordering::Release);
                                missed.store(0, Ordering::Release);
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
        let f = Frame {
            msg_type: MsgType::Open,
            flags: 0,
            stream_id: id,
            payload: Bytes::copy_from_slice(target.as_bytes()),
        };
        self.out_tx
            .send(f)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        Ok(id)
    }

    pub async fn send_data(&self, stream_id: u32, payload: Bytes) -> anyhow::Result<()> {
        self.out_tx
            .send(Frame {
                msg_type: MsgType::Data,
                flags: 0,
                stream_id,
                payload,
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
