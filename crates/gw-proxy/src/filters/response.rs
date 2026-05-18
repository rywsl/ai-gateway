//! `response_body_filter` — increment-by-chunk SSE parsing on the response
//! path.
//!
//! v1 scope: ship a scaffold that
//! 1. Caches the leftover (un-framed) bytes on `ctx.sse_leftover`.
//! 2. On each chunk, accumulates into the leftover buffer and tries to
//!    split on `\n\n` (SSE event boundary).
//! 3. For each complete event, calls `parse_stream_chunk` and—for
//!    `StreamEventIR::Usage`—updates `ctx.usage`.
//! 4. If the response is non-stream (no SSE framing), defers to a buffered
//!    `parse_response` call on the final `end_of_stream` flush.
//!
//! Cross-protocol re-rendering of the body is **not yet wired** — the v1
//! gateway only ships same-protocol passthrough on the streaming path. The
//! multi-event-per-chunk fan-out (Gemini-style combined final event) is
//! deferred to v2 (see codec trait_def doc).
//!
//! TODO(task-4.3): wire a deadline check against ctx.timing.start and
//! `Timeouts::stream_total`.

use crate::ctx::ReqCtx;
use bytes::Bytes;
use gw_codec::AnyCodec;
use gw_core::ir::StreamEventIR;
use gw_core::protocol::ProtocolKind;
use pingora::Result;
use pingora_proxy::Session;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub fn run(
    _session: &mut Session,
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut ReqCtx,
) -> Result<Option<Duration>> {
    // Track bytes shipped to the client for retry-eligibility decisions.
    if let Some(b) = body.as_ref() {
        ctx.bytes_sent_to_client
            .fetch_add(b.len() as u64, Ordering::Relaxed);
    }

    // Non-stream path: nothing to incrementally parse. Final usage will be
    // pulled from the buffered response in the logging filter once we cache
    // the full response. v1: skip.
    if !ctx.is_stream {
        return Ok(None);
    }

    // ---- Streaming path -------------------------------------------------
    // Snapshot the codec *kind* so we can rebuild a fresh zero-sized codec
    // inside the loop without holding an immutable borrow on `ctx`.
    let Some(codec_kind) = ctx.upstream_codec.as_ref().map(AnyCodec::kind) else {
        return Ok(None);
    };
    if let Some(chunk) = body.as_ref() {
        ctx.sse_leftover.extend_from_slice(chunk);
    }

    // Drain complete `\n\n`-terminated events.
    loop {
        let Some(idx) = find_event_boundary(&ctx.sse_leftover) else {
            break;
        };
        let event_bytes: Vec<u8> = ctx.sse_leftover.drain(..idx).collect();
        // Skip the framing `\n\n` itself.
        ctx.sse_leftover.drain(..2);
        let codec = codec_for(codec_kind);
        handle_event(&codec, &event_bytes, ctx);
    }

    if end_of_stream && !ctx.sse_leftover.is_empty() {
        let tail: Vec<u8> = std::mem::take(&mut ctx.sse_leftover);
        let codec = codec_for(codec_kind);
        handle_event(&codec, &tail, ctx);
    }

    Ok(None)
}

fn codec_for(k: ProtocolKind) -> AnyCodec {
    match k {
        ProtocolKind::OpenAi => AnyCodec::OpenAi(gw_codec::openai::OpenAi),
        ProtocolKind::Anthropic => AnyCodec::Anthropic(gw_codec::anthropic::Anthropic),
        ProtocolKind::Gemini => AnyCodec::Gemini(gw_codec::gemini::Gemini),
    }
}

fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn handle_event(codec: &AnyCodec, event_bytes: &[u8], ctx: &mut ReqCtx) {
    match codec.parse_stream_chunk(event_bytes) {
        Ok(Some(StreamEventIR::Usage(u))) => {
            ctx.usage.input_tokens = u.input_tokens;
            ctx.usage.output_tokens = u.output_tokens;
            ctx.usage.cached_tokens = u.cached_tokens;
        }
        Ok(Some(StreamEventIR::Start { .. })) => {
            if ctx.timing.ttft.is_none() {
                ctx.timing.ttft = Some(std::time::Instant::now());
            }
        }
        Ok(Some(StreamEventIR::Delta { .. })) => {
            if ctx.timing.ttft.is_none() {
                ctx.timing.ttft = Some(std::time::Instant::now());
            }
        }
        Ok(Some(StreamEventIR::End { .. })) | Ok(Some(StreamEventIR::Error(_))) => {}
        Ok(None) => {}
        Err(e) => {
            tracing::warn!(error = ?e.code, "stream parse error");
        }
    }
}
