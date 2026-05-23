//! Raw-socket failure-injection handlers.
//!
//! These primitives bypass hyper and operate directly on a `tokio::net::TcpStream`
//! so we can:
//!   - emit a TCP RST at a chosen byte offset (via `SO_LINGER=0` + drop),
//!   - drip-feed bytes one-per-second,
//!   - cut a stream mid-SSE-event with a clean FIN (no `\n\n` boundary).
//!
//! `ConnectionRefusedFirstN` is NOT in this module — it is handled in the
//! accept loop in `server.rs` because it must apply BEFORE we can read any
//! HTTP request to match a route. See [`RefuseCounter`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use crate::scenario::SseEvent;
use crate::sse;

/// A shared counter consumed at accept time by the business listener to
/// implement `ConnectionRefusedFirstN`. It is primed to `n` whenever a
/// scenario containing that variant becomes active, and decremented (with a
/// floor of 0) for each incoming socket that gets immediately dropped.
#[derive(Debug, Default, Clone)]
pub struct RefuseCounter {
    remaining: Arc<AtomicU32>,
    refused_total: Arc<AtomicU64>,
}

impl RefuseCounter {
    pub fn new() -> Self {
        Self {
            remaining: Arc::new(AtomicU32::new(0)),
            refused_total: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn set(&self, n: u32) {
        self.remaining.store(n, Ordering::SeqCst);
    }
    /// Returns true if we should refuse this connection.
    ///
    /// Increments the cumulative `refused_total` on a successful take so the
    /// admin endpoint can report the count even after `remaining` is drained.
    pub fn take_one(&self) -> bool {
        loop {
            let cur = self.remaining.load(Ordering::SeqCst);
            if cur == 0 {
                return false;
            }
            if self
                .remaining
                .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.refused_total.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }
    pub fn refused_total(&self) -> u64 {
        self.refused_total.load(Ordering::Relaxed)
    }
}

/// Force a TCP RST on close by setting `SO_LINGER` to zero, then dropping.
/// On Linux + macOS this produces an RST segment rather than a graceful FIN.
///
/// We go through `socket2::SockRef` because `tokio::net::TcpStream::set_linger`
/// is deprecated (it would block the runtime thread on drop). `SockRef::set_linger`
/// is the recommended path and does not move ownership.
pub fn rst_close(stream: TcpStream) {
    let sref = socket2::SockRef::from(&stream);
    if let Err(e) = sref.set_linger(Some(Duration::ZERO)) {
        eprintln!("[fake-upstream] set_linger failed: {e}");
    }
    drop(stream);
}

/// Build a minimal HTTP/1.1 status line + headers blob.
/// The caller is responsible for any body that follows.
pub fn build_head(
    status: u16,
    reason: &str,
    headers: impl IntoIterator<Item = (String, String)>,
) -> Vec<u8> {
    let mut buf = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    for (k, v) in headers {
        buf.extend_from_slice(k.as_bytes());
        buf.extend_from_slice(b": ");
        buf.extend_from_slice(v.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    buf.extend_from_slice(b"\r\n");
    buf
}

// ── Variant: TcpResetAfterHeaders ────────────────────────────────────────────

/// Write a valid status-line + headers, flush, then force a TCP RST.
pub async fn tcp_reset_after_headers(mut stream: TcpStream) -> std::io::Result<()> {
    let head = build_head(
        200,
        "OK",
        [
            ("content-type".into(), "text/event-stream".into()),
            ("cache-control".into(), "no-cache".into()),
            ("connection".into(), "close".into()),
        ],
    );
    stream.write_all(&head).await?;
    stream.flush().await?;
    // See note in `tcp_reset_mid_body`: give the kernel time to flush bytes
    // to the wire before SO_LINGER=0 discards them.
    tokio::time::sleep(Duration::from_millis(50)).await;
    rst_close(stream);
    Ok(())
}

// ── Variant: TcpResetMidBody ─────────────────────────────────────────────────

/// Write headers + exactly `after_bytes` body bytes, flush, then RST.
///
/// We declare `content-length: <after_bytes>` to make the truncation
/// observable even on framings that would otherwise just see a chunked-EOF.
/// `after_bytes` body content is a deterministic `b'A'` filler.
pub async fn tcp_reset_mid_body(mut stream: TcpStream, after_bytes: usize) -> std::io::Result<()> {
    let head = build_head(
        200,
        "OK",
        [
            ("content-type".into(), "application/octet-stream".into()),
            // Declare a body LARGER than what we'll send so the truncation
            // is unambiguously visible. (Some clients otherwise treat a
            // matching content-length close as success.)
            ("content-length".into(), format!("{}", after_bytes + 1024)),
            ("connection".into(), "close".into()),
        ],
    );
    stream.write_all(&head).await?;
    if after_bytes > 0 {
        let body = vec![b'A'; after_bytes];
        stream.write_all(&body).await?;
    }
    stream.flush().await?;
    // Give the kernel a moment to actually push these bytes to the peer.
    // `SO_LINGER=0` discards anything still in the send buffer when we drop,
    // so if we RST immediately after `flush()` returns (which only means
    // "userspace buffer emptied"), the body bytes may never reach the wire.
    // 50ms is below any reasonable test budget but well above typical
    // loopback latency.
    tokio::time::sleep(Duration::from_millis(50)).await;
    rst_close(stream);
    Ok(())
}

// ── Variant: SlowBodyBytePerSecond ───────────────────────────────────────────

/// Write headers, then drip-feed `body` at `bps` bytes per second.
///
/// `bps == 0` is treated as `bps == 1` to avoid div-by-zero. The connection
/// is closed cleanly (graceful FIN) once the body has been delivered.
pub async fn slow_body_byte_per_second(
    mut stream: TcpStream,
    body: String,
    bps: u32,
) -> std::io::Result<()> {
    let effective_bps = if bps == 0 { 1 } else { bps };
    let interval = Duration::from_secs_f64(1.0 / effective_bps as f64);

    let head = build_head(
        200,
        "OK",
        [
            ("content-type".into(), "text/plain".into()),
            ("content-length".into(), format!("{}", body.len())),
            ("connection".into(), "close".into()),
        ],
    );
    stream.write_all(&head).await?;
    stream.flush().await?;

    for (i, byte) in body.as_bytes().iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(interval).await;
        }
        stream.write_all(&[*byte]).await?;
        stream.flush().await?;
    }
    // Graceful close.
    let _ = stream.shutdown().await;
    Ok(())
}

// ── Variant: PartialSseEvent ─────────────────────────────────────────────────

/// Write a 200 text/event-stream response, then `data: <half_body>` WITHOUT
/// the trailing `\n\n`, then close cleanly (FIN, not RST).
pub async fn partial_sse_event(mut stream: TcpStream, half_body: String) -> std::io::Result<()> {
    stream.write_all(sse::SSE_RESPONSE_HEAD).await?;
    sse::write_partial_event(&mut stream, &half_body).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

// ── Variant: StatusWithRetryAfter ────────────────────────────────────────────

pub async fn status_with_retry_after(
    mut stream: TcpStream,
    status: u16,
    retry_after_secs: u32,
    body: String,
) -> std::io::Result<()> {
    let reason = reason_phrase(status);
    let head = build_head(
        status,
        reason,
        [
            ("content-type".into(), "application/json".into()),
            ("content-length".into(), format!("{}", body.len())),
            ("retry-after".into(), format!("{retry_after_secs}")),
            ("connection".into(), "close".into()),
        ],
    );
    stream.write_all(&head).await?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes()).await?;
    }
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

// ── Plain (non-injection) helpers — also raw-socket, since the business path
//    is fully manual. Mirrors E1-A's Static/Sse behaviour. ───────────────────

/// Plain HTTP response: status + headers + body (used for Static, 404, 401).
pub async fn write_static(
    mut stream: TcpStream,
    status: u16,
    headers: HashMap<String, String>,
    body: String,
    delay_ms: u64,
) -> std::io::Result<()> {
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    // Merge in a content-length so the client can frame the body.
    // Caller-supplied headers override our defaults.
    let mut hdrs: Vec<(String, String)> = headers.into_iter().collect();
    if !hdrs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        hdrs.push(("content-length".into(), format!("{}", body.len())));
    }
    if !hdrs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("connection"))
    {
        hdrs.push(("connection".into(), "close".into()));
    }

    let reason = reason_phrase(status);
    let head = build_head(status, reason, hdrs);
    stream.write_all(&head).await?;
    if !body.is_empty() {
        stream.write_all(body.as_bytes()).await?;
    }
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

/// SSE response: 200 + text/event-stream + each event framed as `data: ...\n\n`.
pub async fn write_sse(
    mut stream: TcpStream,
    delay_ms: u64,
    events: Vec<SseEvent>,
) -> std::io::Result<()> {
    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
    stream.write_all(sse::SSE_RESPONSE_HEAD).await?;
    stream.flush().await?;
    for ev in &events {
        sse::write_event(&mut stream, ev).await?;
    }
    let _ = stream.shutdown().await;
    Ok(())
}

// ── Variant: GzipStatic ───────────────────────────────────────────────────────

/// Write a static response whose body is the raw gzip bytes obtained by base64-
/// decoding `body_base64`. Adds `content-encoding: gzip` + `content-length`
/// for the *encoded* bytes so the client decompressor picks it up. Caller-
/// supplied headers (case-insensitive) override these defaults.
///
/// The variant exists so the gzip-transparent test (E4-D) can ship a real
/// gzip frame through the otherwise UTF-8-only `Static.body: String` shape.
/// Decoding base64 inside this function keeps the JSON-deserializable wire
/// format pure text.
pub async fn write_gzip_static(
    mut stream: TcpStream,
    status: u16,
    headers: HashMap<String, String>,
    body_base64: String,
    delay_ms: u64,
) -> std::io::Result<()> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    if delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }

    let body_bytes = match STANDARD.decode(body_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            // Fall back to a 500 so the test fails loudly rather than
            // silently shipping zero bytes.
            eprintln!("[fake-upstream] gzip body_base64 decode failed: {e}");
            return write_static(
                stream,
                500,
                HashMap::from([("content-type".into(), "application/json".into())]),
                r#"{"error":"invalid body_base64 in GzipStatic"}"#.to_owned(),
                0,
            )
            .await;
        }
    };

    let mut hdrs: Vec<(String, String)> = headers.into_iter().collect();
    if !hdrs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-encoding"))
    {
        hdrs.push(("content-encoding".into(), "gzip".into()));
    }
    if !hdrs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        hdrs.push(("content-length".into(), format!("{}", body_bytes.len())));
    }
    if !hdrs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("connection"))
    {
        hdrs.push(("connection".into(), "close".into()));
    }
    if !hdrs
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        // Default to text/event-stream so the OpenAI SDK happily treats the
        // decompressed bytes as a stream; callers can override.
        hdrs.push(("content-type".into(), "text/event-stream".into()));
    }

    let reason = reason_phrase(status);
    let head = build_head(status, reason, hdrs);
    stream.write_all(&head).await?;
    if !body_bytes.is_empty() {
        stream.write_all(&body_bytes).await?;
    }
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

// ── Misc ─────────────────────────────────────────────────────────────────────

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Status",
    }
}
