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
//! Cross-protocol non-stream re-render (S-codec T6): when the request is
//! non-stream AND the client wire form differs from the upstream wire form
//! (via `WireFormat::from_any_codec`, which distinguishes OpenAI Chat from
//! OpenAI Responses where `ProtocolKind` collapses them), the non-stream
//! branch accumulates the upstream body to EOS, calls
//! `upstream_codec.parse_response`, runs `CrossRender::run` through the
//! *client* codec, and substitutes the re-rendered bytes for the outbound
//! body. The `gw_codec_cross_render_nonstream_total{from,to}` metric is
//! emitted on success; failures map to `GW_CODEC_PARSE_RESPONSE` /
//! `GW_CODEC_CROSS_RENDER_FAILED` warn logs and counters.
//!
//! Response deadline (S-proxy-core T3): the entry of `run()` checks
//! `ctx.timing.start.elapsed()` against `ctx.response_deadline` and
//! terminates the stream with `GW_RESPONSE_DEADLINE_EXCEEDED` if exceeded.
//! The check runs *before* any body processing or cross-protocol re-render.

use crate::ctx::ReqCtx;
use bytes::{Bytes, BytesMut};
use gw_codec::cli::CliCodec;
use gw_codec::cross_render::{CrossRender, WireFormat};
use gw_codec::AnyCodec;
use gw_core::ir::StreamEventIR;
use gw_core::protocol::{ApiShape, ProtocolKind};
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
    check_deadline(ctx)?;

    // Track bytes shipped to the client for retry-eligibility decisions.
    if let Some(b) = body.as_ref() {
        ctx.bytes_sent_to_client
            .fetch_add(b.len() as u64, Ordering::Relaxed);
    }

    // W2-T4: on first non-empty body chunk, transition Sent -> Streaming.
    if matches!(
        ctx.vendor_exposure,
        crate::billing_lifecycle::VendorExposure::Sent
    ) && body.as_ref().map(|b| !b.is_empty()).unwrap_or(false)
    {
        crate::billing_lifecycle::transition_to(
            ctx,
            crate::billing_lifecycle::VendorExposure::Streaming,
        );
    }

    // Archive: capture raw upstream chunk before any codec rewrite. Empty
    // EOS chunks are fine; the accumulator simply doesn't grow.
    if let Some(b) = body.as_ref() {
        ctx.archive_upstream_response_body.extend_from_slice(b);
    }

    if ctx.client_shape.is_some_and(|shape| shape.is_cli()) {
        run_cli_body_filter(body, end_of_stream, ctx)?;
        return Ok(None);
    }

    // Non-stream path: accumulate the upstream body into
    // `ctx.upstream_body_accum`; on EOS, hand the buffer to
    // `upstream_codec.parse_response` so `ctx.usage` is populated from the
    // wire instead of leaving the logging filter to fall through to the
    // freeze-time estimate (ADR-B1-3 / T2-B). Decode failure increments
    // `gw_codec_parse_response_failure_total` and leaves `ctx.usage` at zero.
    //
    // Cross-protocol non-stream (S-codec T6): when the client wire form
    // differs from the upstream wire form, `handle_non_stream_chunk` also
    // re-renders the body via `CrossRender::run` and stashes the result on
    // `ctx.pending_client_body`. We suppress pre-EOS chunks (so the client
    // doesn't see partial upstream bytes) and substitute the rendered body
    // at EOS. On parse / cross-render failure, the helper repopulates
    // `ctx.upstream_body_accum` with the original bytes so the caller can
    // forward them verbatim — preserving same-protocol parity for vendor
    // error JSON.
    if !ctx.is_stream {
        let cross_protocol = is_cross_protocol(ctx);
        handle_non_stream_chunk(body.as_ref(), end_of_stream, ctx);
        if cross_protocol {
            if end_of_stream {
                if let Some(buf) = ctx.pending_client_body.take() {
                    // Happy path: cross-render produced bytes in client wire form.
                    *body = Some(buf.freeze());
                } else if let Some(orig) = ctx.upstream_body_accum.take() {
                    // Fallback (parse_response or cross-render failed): forward
                    // the original upstream bytes verbatim. Cross-protocol
                    // clients won't decode them, but vendor error envelopes /
                    // unknown response shapes are at least preserved instead
                    // of being silently zeroed out.
                    *body = Some(orig.freeze());
                } else {
                    // No body to substitute (e.g., pingora delivered EOS with no
                    // chunks). Forward whatever pingora handed us.
                }
            } else {
                // Pre-EOS: swallow the chunk; the full re-rendered body is
                // emitted once at EOS.
                *body = None;
            }
        }
        // Archive capture for non-stream path; body reflects whatever bytes
        // (substituted or upstream-verbatim) reach the client.
        if let Some(b) = body.as_ref() {
            ctx.archive_downstream_response_body.extend_from_slice(b);
        }
        return Ok(None);
    }

    // ---- Streaming path -------------------------------------------------
    // Snapshot the codec *kinds* so we can rebuild fresh zero-sized codecs
    // inside the loop without holding an immutable borrow on `ctx`.
    let Some(upstream_kind) = ctx.upstream_codec.as_ref().map(AnyCodec::kind) else {
        return Ok(None);
    };
    // Cross-protocol render (G8) requires a known client codec whose wire
    // form differs from upstream. We compare on `WireFormat` (4-way: distinguishes
    // OpenAI Chat vs OpenAI Responses) — `ProtocolKind` collapses them to 3-way
    // and would treat Chat ⇄ Responses as same-protocol, silently skipping the
    // re-render. Codec *dispatch* below still uses `kind`/`codec_for` since both
    // Chat and Responses share the same `OpenAi` ProtocolCodec impl today.
    let upstream_wire = ctx.upstream_codec.as_ref().map(WireFormat::from_any_codec);
    let client_wire = ctx.client_codec.as_ref().map(WireFormat::from_any_codec);
    let cross_protocol = matches!((client_wire, upstream_wire), (Some(cw), Some(uw)) if cw != uw);
    let client_kind = ctx.client_codec.as_ref().map(AnyCodec::kind);

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

    // Archive: capture downstream-bound bytes after all in-function mutation.
    if let Some(b) = body.as_ref() {
        ctx.archive_downstream_response_body.extend_from_slice(b);
    }

    Ok(None)
}

pub fn run_cli_body_filter(
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut ReqCtx,
) -> Result<()> {
    if let Some(chunk) = body.as_ref() {
        let shape = ctx
            .client_shape
            .filter(ApiShape::is_cli)
            .ok_or_else(|| pingora::Error::new(pingora::ErrorType::Custom("gw_not_cli_request")))?;
        let passthrough = cli_passthrough_response_chunk(shape, chunk).into_owned();
        if passthrough.as_slice() != chunk.as_ref() {
            *body = Some(Bytes::copy_from_slice(&passthrough));
        }
        if ctx.is_stream {
            scan_cli_stream_chunk(shape, &passthrough, ctx);
        } else {
            ctx.upstream_body_accum
                .get_or_insert_with(BytesMut::new)
                .extend_from_slice(&passthrough);
        }
        ctx.archive_downstream_response_body
            .extend_from_slice(&passthrough);
    }
    if end_of_stream && !ctx.is_stream {
        if let Some(buf) = ctx.upstream_body_accum.take() {
            apply_cli_full_usage(ctx.client_shape.unwrap(), &buf, ctx);
        }
    }
    Ok(())
}

fn cli_passthrough_response_chunk(shape: ApiShape, chunk: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    match shape {
        ApiShape::ClaudeCli => {
            gw_codec::cli::claude_cli::ClaudeCliCodec.passthrough_response_chunk(chunk)
        }
        ApiShape::OpenAiCli => {
            gw_codec::cli::openai_cli::OpenAiCliCodec.passthrough_response_chunk(chunk)
        }
        ApiShape::OpenAiCompact => {
            gw_codec::cli::openai_compact::OpenAiCompactCodec.passthrough_response_chunk(chunk)
        }
        ApiShape::GeminiCli => {
            gw_codec::cli::gemini_cli::GeminiCliCodec.passthrough_response_chunk(chunk)
        }
        _ => std::borrow::Cow::Borrowed(chunk),
    }
}

fn scan_cli_stream_chunk(shape: ApiShape, chunk: &[u8], ctx: &mut ReqCtx) {
    ctx.sse_leftover.extend_from_slice(chunk);
    loop {
        let Some(idx) = find_event_boundary(&ctx.sse_leftover) else {
            break;
        };
        let event_bytes: Vec<u8> = ctx.sse_leftover.drain(..idx).collect();
        ctx.sse_leftover.drain(..2);
        handle_cli_event(shape, &event_bytes, ctx);
    }
}

fn handle_cli_event(shape: ApiShape, event_bytes: &[u8], ctx: &mut ReqCtx) {
    let (event_type, data) = split_sse_event(event_bytes);
    if cli_ttft_event_types(shape).contains(&event_type) && ctx.timing.ttft.is_none() {
        ctx.timing.ttft = Some(std::time::Instant::now());
    }
    if let Some(delta) = cli_usage_extractor(shape).extract_stream_usage(event_type, data) {
        apply_cli_delta(delta, ctx);
    }
}

fn split_sse_event(event_bytes: &[u8]) -> (&str, &[u8]) {
    let Ok(s) = std::str::from_utf8(event_bytes) else {
        return ("", event_bytes);
    };
    let mut event_type = "";
    let mut data_start = None;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim();
        } else if let Some(rest) = line.strip_prefix("data:") {
            let offset = rest.as_ptr() as usize - s.as_ptr() as usize;
            data_start = Some(offset + rest.len() - rest.trim_start().len());
            break;
        }
    }
    let data = data_start.map_or(event_bytes, |start| s[start..].trim_end().as_bytes());
    (event_type, data)
}

fn apply_cli_full_usage(shape: ApiShape, body: &[u8], ctx: &mut ReqCtx) {
    if let Some(full) = cli_usage_extractor(shape).extract_response_usage(body) {
        ctx.usage.input_tokens = full.input_tokens as u32;
        ctx.usage.output_tokens = full.output_tokens as u32;
        ctx.usage.cached_tokens = Some(full.cache_read_input_tokens as u32);
        ctx.usage.cache_creation_tokens = Some(full.cache_creation_input_tokens as u32);
    }
}

fn apply_cli_delta(delta: gw_codec::cli::CliUsageDelta, ctx: &mut ReqCtx) {
    if let Some(v) = delta.input_tokens {
        ctx.usage.input_tokens = v as u32;
    }
    if let Some(v) = delta.output_tokens {
        ctx.usage.output_tokens = v as u32;
    }
    if let Some(v) = delta.cache_read_input_tokens {
        ctx.usage.cached_tokens = Some(v as u32);
    }
    if let Some(v) = delta.cache_creation_input_tokens {
        ctx.usage.cache_creation_tokens = Some(v as u32);
    }
}

fn cli_usage_extractor(shape: ApiShape) -> &'static dyn gw_codec::cli::CliUsageExtractor {
    match shape {
        ApiShape::ClaudeCli => gw_codec::cli::claude_cli::ClaudeCliCodec.usage_extractor(),
        ApiShape::OpenAiCli => gw_codec::cli::openai_cli::OpenAiCliCodec.usage_extractor(),
        ApiShape::OpenAiCompact => {
            gw_codec::cli::openai_compact::OpenAiCompactCodec.usage_extractor()
        }
        ApiShape::GeminiCli => gw_codec::cli::gemini_cli::GeminiCliCodec.usage_extractor(),
        _ => gw_codec::cli::openai_cli::OpenAiCliCodec.usage_extractor(),
    }
}

fn cli_ttft_event_types(shape: ApiShape) -> &'static [&'static str] {
    match shape {
        ApiShape::ClaudeCli => gw_codec::cli::claude_cli::ClaudeCliCodec.ttft_event_types(),
        ApiShape::OpenAiCli => gw_codec::cli::openai_cli::OpenAiCliCodec.ttft_event_types(),
        ApiShape::OpenAiCompact => {
            gw_codec::cli::openai_compact::OpenAiCompactCodec.ttft_event_types()
        }
        ApiShape::GeminiCli => gw_codec::cli::gemini_cli::GeminiCliCodec.ttft_event_types(),
        _ => &[],
    }
}

fn check_deadline(ctx: &ReqCtx) -> Result<()> {
    if let (Some(start), Some(deadline)) = (ctx.timing.start, ctx.response_deadline) {
        if start.elapsed() > deadline {
            metrics::counter!(
                "gw_response_deadline_exceeded_total",
                "stream" => if ctx.is_stream { "true" } else { "false" },
            )
            .increment(1);
            return Err(pingora::Error::new(pingora::ErrorType::Custom(
                gw_core::error::codes::GW_RESPONSE_DEADLINE_EXCEEDED,
            )));
        }
    }
    Ok(())
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
    // S-codec T2: parse_stream_chunk now takes a `&mut dyn StreamState` for
    // codec-internal cross-chunk accumulation. We `take()` the state out of
    // ctx to satisfy the borrow checker for the rest of the function, then
    // put it back at the end. `stream_state` is initialized in
    // `upstream_request_filter` (after `rewrite_request` resolves the
    // upstream codec) when `is_stream` is true; if for any reason it is
    // missing on a stream response (codec mismatch / mis-init), lazily
    // synthesise one from the upstream codec so streaming doesn't break.
    let mut state = ctx
        .stream_state
        .take()
        .unwrap_or_else(|| codec.new_stream_state());
    let events = match codec.parse_stream_chunk(state.as_mut(), event_bytes) {
        Ok(evs) => evs,
        Err(e) => {
            tracing::warn!(error = ?e.code, "stream parse error");
            ctx.stream_state = Some(state);
            return;
        }
    };
    let upstream_shape = codec.api_shape();
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
            StreamEventIR::ThinkingDelta { .. }
            | StreamEventIR::ToolCallStart { .. }
            | StreamEventIR::ToolCallDelta { .. }
            | StreamEventIR::RefusalDelta { .. }
            | StreamEventIR::CitationsDelta { .. }
            | StreamEventIR::ServerToolStart { .. }
            | StreamEventIR::ServerToolDelta { .. } => {
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

    // W1-T6: extract 3 new dims from the raw SSE event bytes. This is
    // additive to the per-IR-event `StreamEventIR::Usage` accumulation above
    // (which fills input/output/cached). The extractor adds cache_creation,
    // thinking, and multimodal from the raw JSON that the IR doesn't carry.
    if let Some(e) = extractor_for_shape(upstream_shape).extract_chunk(event_bytes) {
        apply_extracted_to_ctx(&e, ctx);
        // W2-T4: terminal chunk from extractor signals the stream is complete.
        if e.is_terminal
            && matches!(
                ctx.vendor_exposure,
                crate::billing_lifecycle::VendorExposure::Streaming
            )
        {
            crate::billing_lifecycle::transition_to(
                ctx,
                crate::billing_lifecycle::VendorExposure::Settled,
            );
        }
    }

    // Put the accumulator back so `run` can take+freeze it. Empty buffers are
    // not stashed: `run` would treat `Some(empty)` as a chunk substitution and
    // wrongly zero out the outbound body.
    if cross_protocol && !acc.is_empty() {
        ctx.pending_client_body = Some(acc);
    }

    // S-codec T2: restore the stream state for the next framed event.
    ctx.stream_state = Some(state);
}

fn extractor_for_shape(shape: ApiShape) -> Box<dyn gw_codec::usage_extractor::UsageExtractor> {
    match shape {
        ApiShape::OpenAiChat => {
            Box::new(gw_codec::openai::usage_extractor::OpenAiUsageExtractor::chat())
        }
        ApiShape::OpenAiResponses => {
            Box::new(gw_codec::openai::usage_extractor::OpenAiUsageExtractor::responses())
        }
        ApiShape::AnthropicMessages => {
            Box::new(gw_codec::anthropic::usage_extractor::AnthropicUsageExtractor)
        }
        ApiShape::GeminiGenerate => {
            Box::new(gw_codec::gemini::usage_extractor::GeminiUsageExtractor)
        }
        // CLI variants use CliUsageExtractor (separate path), not UsageExtractor.
        // This arm is unreachable on CLI shapes because the caller branches on
        // `is_cli()` before reaching here. Delegate to the base protocol as a
        // safe fallback.
        ApiShape::ClaudeCli => {
            Box::new(gw_codec::anthropic::usage_extractor::AnthropicUsageExtractor)
        }
        ApiShape::OpenAiCli | ApiShape::OpenAiCompact => {
            Box::new(gw_codec::openai::usage_extractor::OpenAiUsageExtractor::responses())
        }
        ApiShape::GeminiCli => Box::new(gw_codec::gemini::usage_extractor::GeminiUsageExtractor),
    }
}

/// Merge extracted 6-dim usage into `ctx.usage`. Fields from `parse_response`
/// (input/output/cached) are only overridden when the extractor produced a
/// non-zero / non-None value, so we never clobber wire-sourced data with zeros
/// from a partial chunk.
fn apply_extracted_to_ctx(e: &gw_codec::usage_extractor::ExtractedUsage, ctx: &mut ReqCtx) {
    if e.values.input_tokens > 0 {
        ctx.usage.input_tokens = e.values.input_tokens;
    }
    if e.values.output_tokens > 0 {
        ctx.usage.output_tokens = e.values.output_tokens;
    }
    if e.values.cache_read_input_tokens.is_some() {
        ctx.usage.cached_tokens = e.values.cache_read_input_tokens;
    }
    if e.values.cache_creation_input_tokens.is_some() {
        ctx.usage.cache_creation_tokens = e.values.cache_creation_input_tokens;
    }
    if e.values.thinking_tokens.is_some() {
        ctx.usage.thinking_tokens = e.values.thinking_tokens;
    }
    if e.values.multimodal_input_tokens.is_some() {
        ctx.usage.multimodal_input_tokens = e.values.multimodal_input_tokens;
    }
    copy_provenance(e, ctx);
}

/// Promote per-dimension provenance: only advance if the incoming tag has
/// higher confidence. Order: UpstreamFinal > UpstreamPartial > Estimated >
/// CrashFallback > Unknown.
fn copy_provenance(e: &gw_codec::usage_extractor::ExtractedUsage, ctx: &mut ReqCtx) {
    use gw_core::usage::ProvenanceTag;
    fn rank(t: ProvenanceTag) -> u8 {
        match t {
            ProvenanceTag::UpstreamFinal => 4,
            ProvenanceTag::UpstreamPartial => 3,
            ProvenanceTag::Estimated => 2,
            ProvenanceTag::CrashFallback => 1,
            ProvenanceTag::Unknown => 0,
        }
    }
    fn promote(dst: &mut ProvenanceTag, src: ProvenanceTag) {
        if rank(src) > rank(*dst) {
            *dst = src;
        }
    }
    promote(&mut ctx.usage_provenance.input, e.provenance.input);
    promote(&mut ctx.usage_provenance.output, e.provenance.output);
    promote(
        &mut ctx.usage_provenance.cache_read,
        e.provenance.cache_read,
    );
    promote(
        &mut ctx.usage_provenance.cache_creation,
        e.provenance.cache_creation,
    );
    promote(&mut ctx.usage_provenance.thinking, e.provenance.thinking);
    promote(
        &mut ctx.usage_provenance.multimodal_input,
        e.provenance.multimodal_input,
    );
}

/// True iff the non-stream response needs cross-protocol re-render — i.e.
/// the client and upstream codecs are both known and their **wire forms**
/// differ. We compare `WireFormat` (4-way) rather than `ProtocolKind` (3-way)
/// because the latter collapses OpenAI Chat into OpenAI Responses; both are
/// `ProtocolKind::OpenAi` but their wire shapes are not interchangeable.
fn is_cross_protocol(ctx: &ReqCtx) -> bool {
    match (ctx.client_codec.as_ref(), ctx.upstream_codec.as_ref()) {
        (Some(client), Some(upstream)) => {
            WireFormat::from_any_codec(client) != WireFormat::from_any_codec(upstream)
        }
        _ => false,
    }
}

/// T2-B / ADR-B1-3 — non-stream upstream body accumulator. Pingora delivers
/// the response body in chunks; we buffer them into `ctx.upstream_body_accum`
/// and only invoke `codec.parse_response` once `end_of_stream` is set so the
/// codec sees the full JSON document. The accumulator is *taken* on EOS so a
/// successful parse leaves it `None` (a stray empty follow-up chunk doesn't
/// re-parse the same body).
///
/// S-codec T6: when the request is cross-protocol (different client and
/// upstream wire forms), this helper also runs `CrossRender::run` through
/// the client codec at EOS and writes the re-rendered bytes into
/// `ctx.pending_client_body`. The caller (`run`) then substitutes those
/// bytes for the outbound body. The `gw_codec_cross_render_nonstream_total`
/// counter is incremented per successful re-render; failures (parse or
/// render) emit `GW_CODEC_PARSE_RESPONSE` / `GW_CODEC_CROSS_RENDER_FAILED`
/// warn logs + counters and leave `pending_client_body = None` so the caller
/// substitutes an empty body.
///
/// Failure semantics for usage extraction: any error from `parse_response`
/// increments the counter `gw_codec_parse_response_failure_total{codec,
/// upstream_status}` and leaves `ctx.usage` at its default zero.
pub(crate) fn handle_non_stream_chunk(
    chunk: Option<&Bytes>,
    end_of_stream: bool,
    ctx: &mut ReqCtx,
) {
    if let Some(b) = chunk {
        ctx.upstream_body_accum
            .get_or_insert_with(BytesMut::new)
            .extend_from_slice(b);
    }

    if !end_of_stream {
        return;
    }

    // EOS path: take the accumulator (leaves the field `None`) and decode.
    let Some(body) = ctx.upstream_body_accum.take() else {
        return;
    };
    let Some(codec) = ctx.upstream_codec.as_ref() else {
        // No codec means we can't parse — put the bytes back so the caller
        // can forward them verbatim. The logging filter will fall back to
        // the freeze-time estimate.
        ctx.upstream_body_accum = Some(body);
        return;
    };
    let codec_kind = codec.kind();
    let codec_shape = codec.api_shape();
    // `parse_response` borrows from `body`; both the successful `ResponseIR`
    // and the error `ErrorIR` hold that borrow, so we cannot move `body`
    // back into `ctx.upstream_body_accum` while the match arms hold the
    // result. Collect the decision in a flag and apply it after the match
    // drops everything that borrows `body`.
    let mut return_body_to_accum = false;
    {
        let parse_result = codec.parse_response(&body);
        match parse_result {
            Ok(ir) => {
                if let Some(u) = ir.usage.as_ref() {
                    ctx.usage.input_tokens = u.input_tokens;
                    ctx.usage.output_tokens = u.output_tokens;
                    ctx.usage.cached_tokens = u.cached_tokens;
                }
                // ADR-G5-1: Gemini long-running operations emit a top-level
                // `name: "operations/<uuid>"` which the codec surfaces as
                // `ResponseIR.id`. Promote it to `ctx.operation_id` only when the
                // prefix matches — chat ids like `chatcmpl-...` must NOT pollute
                // this field. The logging filter writes `ctx.operation_id` into
                // `UsageRecord.operation_id` (see filters/logging.rs).
                if ir.id.starts_with("operations/") {
                    ctx.operation_id = Some(ir.id.clone().into_owned());
                }

                // S-codec T6: cross-protocol non-stream re-render. Decoupled
                // from `usage` extraction so usage is still recorded when
                // re-render fails (mirrors the streaming path's per-event
                // accumulation). The accumulator stays consumed on success;
                // on render failure we mark `return_body_to_accum` so the
                // caller can forward the original upstream bytes verbatim
                // (matching same-protocol fallback semantics).
                if let Some(client) = ctx.client_codec.as_ref() {
                    let from = WireFormat::from_any_codec(codec);
                    let to = WireFormat::from_any_codec(client);
                    if from != to {
                        let mut out = BytesMut::with_capacity(body.len());
                        let cross = CrossRender {
                            from,
                            to,
                            source: ir,
                        };
                        match cross.run(client.as_dyn(), &mut out) {
                            Ok(()) => {
                                metrics::counter!(
                                    "gw_codec_cross_render_nonstream_total",
                                    "from" => from.label(),
                                    "to" => to.label(),
                                )
                                .increment(1);
                                ctx.pending_client_body = Some(out);
                            }
                            Err(e) => {
                                metrics::counter!(
                                    "gw_codec_cross_render_nonstream_failure_total",
                                    "from" => from.label(),
                                    "to" => to.label(),
                                )
                                .increment(1);
                                tracing::warn!(
                                    from = %from,
                                    to = %to,
                                    error = ?e.code,
                                    "{}",
                                    gw_core::error::codes::GW_CODEC_CROSS_RENDER_FAILED
                                );
                                return_body_to_accum = true;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                metrics::counter!(
                    "gw_codec_parse_response_failure_total",
                    "codec" => codec_kind.as_str(),
                    "upstream_status" => ctx.upstream_status.unwrap_or(0).to_string(),
                )
                .increment(1);
                tracing::warn!(
                    error = ?e.code,
                    "{}",
                    gw_core::error::codes::GW_CODEC_PARSE_RESPONSE
                );
                // parse_response failed (e.g., vendor error JSON body, or a
                // shape this codec can't decode). Forward the original
                // upstream bytes verbatim — cross-protocol clients see
                // upstream-shape bytes (best-effort vs an empty 200), and
                // same-protocol clients see no change from pre-T6 behaviour.
                return_body_to_accum = true;
            }
        }
    }
    // W1-T6: extract the 3 new dims (cache_creation, thinking, multimodal)
    // that `parse_response` / the IR don't surface. Always runs on EOS even
    // when `parse_response` succeeded — the extractor is a second read over
    // the same bytes and never fails fatally (parse errors return a default).
    // We skip extraction when `return_body_to_accum` is true (i.e. either
    // `parse_response` already failed, or cross-render failed but the usage
    // was already written) — those cases kept the body in the accum for the
    // caller to forward verbatim, and a second parse is wasteful.
    if !return_body_to_accum {
        let extracted = extractor_for_shape(codec_shape).extract_response(&body);
        apply_extracted_to_ctx(&extracted, ctx);
        // W2-T4: non-stream EOS with a successful parse is always terminal.
        if extracted.is_terminal
            && matches!(
                ctx.vendor_exposure,
                crate::billing_lifecycle::VendorExposure::Streaming
            )
        {
            crate::billing_lifecycle::transition_to(
                ctx,
                crate::billing_lifecycle::VendorExposure::Settled,
            );
        }
    }
    if return_body_to_accum {
        ctx.upstream_body_accum = Some(body);
    }
}

#[cfg(test)]
mod non_stream_branch_tests {
    use super::*;
    use crate::ctx::ReqCtx;
    use bytes::Bytes;
    use gw_codec::openai::OpenAi;
    use gw_codec::AnyCodec;

    fn make_ctx_non_stream() -> ReqCtx {
        // `ReqCtx::default()` builds a fully-initialised context with empty
        // IDs — fine for unit tests of the non-stream branch.
        ReqCtx {
            is_stream: false,
            upstream_codec: Some(AnyCodec::OpenAi(OpenAi)),
            ..ReqCtx::default()
        }
    }

    #[test]
    fn non_stream_eos_parses_usage_into_ctx() {
        let mut ctx = make_ctx_non_stream();
        // Chunk 1: head of body.
        let chunk1 = Bytes::from_static(
            br#"{"id":"x","object":"chat.completion","created":0,"model":"gpt","choices":[],"#,
        );
        handle_non_stream_chunk(Some(&chunk1), false, &mut ctx);
        assert!(
            ctx.upstream_body_accum.is_some(),
            "first chunk must allocate accumulator"
        );

        // Chunk 2: tail of body + EOS.
        let chunk2 = Bytes::from_static(
            br#""usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}}"#,
        );
        handle_non_stream_chunk(Some(&chunk2), true, &mut ctx);

        assert_eq!(ctx.usage.input_tokens, 7);
        assert_eq!(ctx.usage.output_tokens, 2);
        // After EOS the accum has been taken — must be None so a stray empty
        // follow-up chunk doesn't re-parse the same body.
        assert!(ctx.upstream_body_accum.is_none());
    }

    #[test]
    fn non_stream_decode_error_increments_metric() {
        let mut ctx = make_ctx_non_stream();
        let body = Bytes::from_static(b"not json");
        handle_non_stream_chunk(Some(&body), true, &mut ctx);
        // usage stays at zero — the logging filter emits Unknown provenance tags
        // when no vendor-observed dim has a value.
        assert_eq!(ctx.usage.input_tokens, 0);
        assert_eq!(ctx.usage.output_tokens, 0);
        // S-codec T6: on parse failure the helper repopulates
        // `upstream_body_accum` so the response_body_filter can forward the
        // original upstream bytes verbatim (preserves vendor error JSON
        // pass-through). The caller drains the accumulator at EOS in the
        // cross-protocol branch; same-protocol callers ignore it.
        let accum = ctx.upstream_body_accum.as_ref().expect("body preserved");
        assert_eq!(accum.as_ref(), b"not json" as &[u8]);
    }

    #[test]
    fn non_stream_eos_without_chunk_is_noop() {
        // Pingora may deliver a final `(None, end_of_stream=true)` callback
        // when the upstream body terminated on the previous chunk. The helper
        // must tolerate that: nothing to parse, nothing changes.
        let mut ctx = make_ctx_non_stream();
        handle_non_stream_chunk(None, true, &mut ctx);
        assert_eq!(ctx.usage.output_tokens, 0);
        assert!(ctx.upstream_body_accum.is_none());
    }

    #[test]
    fn non_stream_missing_codec_is_noop() {
        let mut ctx = make_ctx_non_stream();
        ctx.upstream_codec = None;
        let body = Bytes::from_static(b"{\"usage\":{}}");
        handle_non_stream_chunk(Some(&body), true, &mut ctx);
        assert_eq!(ctx.usage.output_tokens, 0);
    }

    #[test]
    fn anthropic_non_stream_eos_extracts_six_dims_into_ctx() {
        let mut ctx = ReqCtx {
            is_stream: false,
            upstream_codec: Some(AnyCodec::Anthropic(gw_codec::anthropic::Anthropic)),
            ..ReqCtx::default()
        };
        let body = Bytes::from_static(
            br#"{
            "id":"msg_1","type":"message","role":"assistant","content":[],
            "model":"claude-3-5-sonnet","stop_reason":"end_turn","stop_sequence":null,
            "usage":{"input_tokens":50,"output_tokens":25,
                     "cache_creation_input_tokens":1000,"cache_read_input_tokens":500}
        }"#,
        );
        handle_non_stream_chunk(Some(&body), true, &mut ctx);
        assert_eq!(ctx.usage.input_tokens, 50);
        assert_eq!(ctx.usage.output_tokens, 25);
        assert_eq!(ctx.usage.cached_tokens, Some(500));
        assert_eq!(ctx.usage.cache_creation_tokens, Some(1000));
        assert!(ctx.usage.thinking_tokens.is_none());
        assert!(ctx.usage.multimodal_input_tokens.is_none());
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use crate::ctx::ReqCtx;
    use std::time::{Duration, Instant};

    #[test]
    fn deadline_exceeded_returns_error() {
        let mut ctx = ReqCtx {
            is_stream: true,
            ..Default::default()
        };
        ctx.timing.start = Some(Instant::now() - Duration::from_secs(700));
        ctx.response_deadline = Some(Duration::from_secs(600));

        let err = check_deadline(&ctx).unwrap_err();
        assert!(matches!(
            err.etype,
            pingora::ErrorType::Custom("GW_RESPONSE_DEADLINE_EXCEEDED")
        ),);
    }

    #[test]
    fn deadline_not_exceeded_returns_ok() {
        let mut ctx = ReqCtx {
            is_stream: true,
            ..Default::default()
        };
        ctx.timing.start = Some(Instant::now());
        ctx.response_deadline = Some(Duration::from_secs(600));

        assert!(check_deadline(&ctx).is_ok());
    }

    #[test]
    fn no_deadline_set_returns_ok() {
        let mut ctx = ReqCtx::default();
        ctx.timing.start = Some(Instant::now() - Duration::from_secs(9999));
        ctx.response_deadline = None;

        assert!(check_deadline(&ctx).is_ok());
    }

    #[test]
    fn no_start_time_returns_ok() {
        let mut ctx = ReqCtx::default();
        ctx.timing.start = None;
        ctx.response_deadline = Some(Duration::from_secs(1));

        assert!(check_deadline(&ctx).is_ok());
    }
}
