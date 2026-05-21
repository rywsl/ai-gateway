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
//! Cross-protocol re-rendering of the streaming body is wired (G8, T4-A): when
//! `ctx.client_codec.kind() != ctx.upstream_codec.kind()`, each parsed
//! `StreamEventIR` is re-rendered through the *client* codec into the
//! `ctx.pending_client_body` `BytesMut` accumulator, and the outer
//! `response_body_filter` substitutes the accumulator (frozen) for the upstream
//! chunk. Same-protocol traffic stays zero-copy — `pending_client_body` is left
//! `None` and the upstream bytes are forwarded verbatim.
//!
//! TODO(task-4.3): wire a deadline check against ctx.timing.start and
//! `Timeouts::stream_total`.

use crate::ctx::ReqCtx;
use bytes::{Bytes, BytesMut};
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
    // Snapshot the codec *kinds* so we can rebuild fresh zero-sized codecs
    // inside the loop without holding an immutable borrow on `ctx`.
    let Some(upstream_kind) = ctx.upstream_codec.as_ref().map(AnyCodec::kind) else {
        return Ok(None);
    };
    // Cross-protocol render (G8) requires a known client codec whose kind
    // differs from upstream. If `client_codec` is absent we fall back to
    // same-protocol passthrough (forward upstream bytes verbatim).
    let client_kind = ctx.client_codec.as_ref().map(AnyCodec::kind);
    let cross_protocol = matches!(client_kind, Some(ck) if ck != upstream_kind);

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
        let codec = codec_for(upstream_kind);
        handle_event(&codec, &event_bytes, ctx, cross_protocol, client_kind);
    }

    if end_of_stream && !ctx.sse_leftover.is_empty() {
        let tail: Vec<u8> = std::mem::take(&mut ctx.sse_leftover);
        let codec = codec_for(upstream_kind);
        handle_event(&codec, &tail, ctx, cross_protocol, client_kind);
    }

    // Cross-protocol substitution: if `handle_event` accumulated re-rendered
    // bytes for this chunk, freeze them and replace the outbound body. The
    // `.take()` is critical — each Pingora chunk gets a fresh accumulator, so
    // carrying old bytes forward would corrupt the stream. Same-protocol
    // traffic leaves `pending_client_body` `None` and forwards verbatim.
    if let Some(buf) = ctx.pending_client_body.take() {
        *body = Some(buf.freeze());
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

fn handle_event(
    codec: &AnyCodec,
    event_bytes: &[u8],
    ctx: &mut ReqCtx,
    cross_protocol: bool,
    client_kind: Option<ProtocolKind>,
) {
    let events = match codec.parse_stream_chunk(event_bytes) {
        Ok(evs) => evs,
        Err(e) => {
            tracing::warn!(error = ?e.code, "stream parse error");
            return;
        }
    };
    let upstream_kind = codec.kind();
    // G8 — cross-protocol render accumulator. Taken out at the start of the
    // event so every IR event re-renders INTO the same `BytesMut`; overwriting
    // it per event would silently drop all but the last (e.g. Gemini's
    // combined `[Delta, End]` → client would see only the End frame).
    let mut acc: BytesMut = ctx.pending_client_body.take().unwrap_or_default();
    let client_codec = client_kind.map(codec_for);

    // A single framed event may fan out into multiple IR events (Gemini's
    // combined `text + finishReason` final chunk decomposes into
    // `[Delta, End]`). Per-event accumulation (TTFT, usage) runs once per
    // IR event so cross-protocol render (T4-A) sees them in order.
    for ev in events {
        // Per-event accumulation (runs in both branches).
        match &ev {
            StreamEventIR::Usage(u) => {
                ctx.usage.input_tokens = u.input_tokens;
                ctx.usage.output_tokens = u.output_tokens;
                ctx.usage.cached_tokens = u.cached_tokens;
            }
            StreamEventIR::Start { .. } | StreamEventIR::Delta { .. } => {
                if ctx.timing.ttft.is_none() {
                    ctx.timing.ttft = Some(std::time::Instant::now());
                }
            }
            StreamEventIR::End { .. } | StreamEventIR::Error(_) => {}
        }

        if cross_protocol {
            // Re-render this IR event through the client codec INTO the
            // shared accumulator — never allocate a fresh buffer, never
            // overwrite.
            if let Some(cc) = client_codec.as_ref() {
                if let Err(e) = cc.render_stream_chunk(&ev, &mut acc) {
                    tracing::warn!(error = ?e.code, "cross-protocol stream render error");
                } else {
                    metrics::counter!(
                        "gw_codec_cross_render_total",
                        "from" => upstream_kind.as_str(),
                        "to" => cc.kind().as_str(),
                    )
                    .increment(1);
                }
            }
        } else {
            metrics::counter!(
                "gw_codec_passthrough_total",
                "kind" => upstream_kind.as_str(),
            )
            .increment(1);
        }
    }

    // Put the accumulator back so `run` can take+freeze it. Empty buffers are
    // not stashed: `run` would treat `Some(empty)` as a chunk substitution and
    // wrongly zero out the outbound body.
    if cross_protocol && !acc.is_empty() {
        ctx.pending_client_body = Some(acc);
    }
}
