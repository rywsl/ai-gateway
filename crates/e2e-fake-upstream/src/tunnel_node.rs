//! aether-hub mock: WS listener that decodes `MsgType::Open { target }` and
//! forwards subsequent `Data` frames to a TCP connection against `target`.
//! Used by `crates/gw-gateway/tests/e2e_tunnel.rs` (T7-B) to exercise the
//! gateway → node → real-upstream loop end-to-end without spinning up the
//! real aether-hub.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio_tungstenite::{accept_async, tungstenite::Message};

const MAGIC: [u8; 2] = [0xAE, 0x21];

#[derive(Copy, Clone)]
enum Mt {
    Open = 0x01,
    Data = 0x02,
    Close = 0x03,
    Ping = 0x04,
    Pong = 0x05,
}

fn encode(mt: Mt, stream_id: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(14 + payload.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(1); // version
    buf.push(mt as u8);
    buf.extend_from_slice(&0u16.to_be_bytes()); // flags
    buf.extend_from_slice(&stream_id.to_be_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn decode_header(buf: &[u8]) -> Option<(u8, u32, usize)> {
    if buf.len() < 14 {
        return None;
    }
    if buf[0..2] != MAGIC {
        return None;
    }
    if buf[2] != 1 {
        return None;
    }
    let mt = buf[3];
    let stream_id = u32::from_be_bytes([buf[6], buf[7], buf[8], buf[9]]);
    let length = u32::from_be_bytes([buf[10], buf[11], buf[12], buf[13]]) as usize;
    Some((mt, stream_id, length))
}

/// Start a tunnel-mode WS listener on `addr`. Blocks forever; spawn it.
pub async fn run(addr: std::net::SocketAddr) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    eprintln!("fake-upstream tunnel listener on {addr}");
    loop {
        let (sock, _peer) = listener.accept().await?;
        tokio::spawn(handle_conn(sock));
    }
}

async fn handle_conn(stream: TcpStream) {
    let ws = match accept_async(stream).await {
        Ok(w) => w,
        Err(e) => {
            eprintln!("ws handshake fail: {e}");
            return;
        }
    };
    let (sink, mut src) = ws.split();
    let sink = Arc::new(Mutex::new(sink));
    let streams: Arc<Mutex<HashMap<u32, tokio::sync::mpsc::Sender<Bytes>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    while let Some(msg) = src.next().await {
        let bin = match msg {
            Ok(Message::Binary(b)) => b,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };
        let (mt, stream_id, length) = match decode_header(&bin) {
            Some(t) => t,
            None => continue,
        };
        let end = (14 + length).min(bin.len());
        let payload = &bin[14..end];
        match mt {
            x if x == Mt::Ping as u8 => {
                let pong = encode(Mt::Pong, stream_id, payload);
                let _ = sink.lock().await.send(Message::Binary(pong)).await;
            }
            x if x == Mt::Open as u8 => {
                let target = String::from_utf8_lossy(payload).to_string();
                let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(16);
                streams.lock().await.insert(stream_id, tx);
                let sink_c = sink.clone();
                let streams_c = streams.clone();
                tokio::spawn(forward(stream_id, target, rx, sink_c, streams_c));
            }
            x if x == Mt::Data as u8 => {
                if let Some(tx) = streams.lock().await.get(&stream_id) {
                    let _ = tx.send(Bytes::copy_from_slice(payload)).await;
                }
            }
            x if x == Mt::Close as u8 => {
                streams.lock().await.remove(&stream_id);
            }
            _ => {}
        }
    }
}

type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<TcpStream>,
    Message,
>;

async fn forward(
    stream_id: u32,
    target: String,
    mut rx: tokio::sync::mpsc::Receiver<Bytes>,
    sink: Arc<Mutex<WsSink>>,
    streams: Arc<Mutex<HashMap<u32, tokio::sync::mpsc::Sender<Bytes>>>>,
) {
    let tcp = match TcpStream::connect(&target).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tunnel forward connect {target} failed: {e}");
            return;
        }
    };
    // Use tokio::io::split for owned halves so the read task can be spawned
    // without lifetime issues (tcp.split() produces borrowed halves).
    let (mut r, mut w) = tokio::io::split(tcp);

    let sink_w = sink.clone();
    let read_task = tokio::spawn(async move {
        let mut buf = [0u8; 16384];
        loop {
            let n = match r.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            let frame = encode(Mt::Data, stream_id, &buf[..n]);
            if sink_w
                .lock()
                .await
                .send(Message::Binary(frame))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    while let Some(bytes) = rx.recv().await {
        if w.write_all(&bytes).await.is_err() {
            break;
        }
    }
    drop(read_task);
    streams.lock().await.remove(&stream_id);
}
