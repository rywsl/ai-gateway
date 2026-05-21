//! Shared SSE framing helpers.
//!
//! All SSE-producing paths (`ResponseSpec::Sse` and `ResponseSpec::PartialSseEvent`)
//! funnel through these helpers so the framing format (`data: <data>\n\n`) and
//! per-event delay handling stay consistent.

use std::time::Duration;

use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::scenario::SseEvent;

/// Write a single SSE event to `w`, sleeping `event.after_ms` first.
///
/// Format: `data: <data>\n\n` (no event name, no id).
pub async fn write_event<W: AsyncWrite + Unpin>(
    w: &mut W,
    event: &SseEvent,
) -> std::io::Result<()> {
    if event.after_ms > 0 {
        tokio::time::sleep(Duration::from_millis(event.after_ms)).await;
    }
    let line = format!("data: {}\n\n", event.data);
    w.write_all(line.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// Write an SSE event WITHOUT the trailing `\n\n` boundary.
///
/// Used by `PartialSseEvent` to deliberately leave the stream in a half-event
/// state so the client sees a torn frame and a clean EOF.
pub async fn write_partial_event<W: AsyncWrite + Unpin>(
    w: &mut W,
    half_body: &str,
) -> std::io::Result<()> {
    // We write `data: <half>` then close — no `\n\n` boundary.
    let line = format!("data: {half_body}");
    w.write_all(line.as_bytes()).await?;
    w.flush().await?;
    Ok(())
}

/// Standard SSE response headers (status line + headers + `\r\n\r\n` terminator).
pub const SSE_RESPONSE_HEAD: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n";
