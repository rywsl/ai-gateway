use bytes::{Bytes, BytesMut};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamPhase {
    Connecting,
    Probing,
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    UpstreamErrorBody,
    MalformedFrame,
    Timeout,
}

impl ProbeFailure {
    pub fn as_label(self) -> &'static str {
        match self {
            ProbeFailure::UpstreamErrorBody => "upstream_error_body",
            ProbeFailure::MalformedFrame => "malformed_frame",
            ProbeFailure::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeDecision {
    NeedMore,
    Commit(Bytes),
    Fail(ProbeFailure),
    AlreadyCommitted,
}

#[derive(Debug, Clone)]
pub struct StreamProbeState {
    pub phase: StreamPhase,
    pub client_committed: bool,
    pub started_at: Instant,
    pub timeout: Duration,
    pub buffer: BytesMut,
}

impl StreamProbeState {
    pub fn new(timeout: Duration, now: Instant) -> Self {
        Self {
            phase: StreamPhase::Connecting,
            client_committed: false,
            started_at: now,
            timeout,
            buffer: BytesMut::new(),
        }
    }
}

pub fn apply_probe_chunk(
    state: &mut StreamProbeState,
    chunk: Option<Bytes>,
    end_of_stream: bool,
) -> ProbeDecision {
    if state.client_committed || matches!(state.phase, StreamPhase::Streaming) {
        return ProbeDecision::AlreadyCommitted;
    }
    state.phase = StreamPhase::Probing;
    if let Some(chunk) = chunk {
        state.buffer.extend_from_slice(&chunk);
    }
    let Some(boundary) = find_sse_boundary(&state.buffer) else {
        if end_of_stream && !state.buffer.is_empty() {
            return classify_unframed_tail(state);
        }
        return ProbeDecision::NeedMore;
    };
    match classify_event(&state.buffer[..boundary + 2]) {
        Ok(()) => {
            state.phase = StreamPhase::Streaming;
            ProbeDecision::Commit(state.buffer.split().freeze())
        }
        Err(failure) => ProbeDecision::Fail(failure),
    }
}

pub fn apply_probe_chunk_with_codec(
    state: &mut StreamProbeState,
    codec: Option<&gw_codec::AnyCodec>,
    chunk: Option<Bytes>,
    end_of_stream: bool,
) -> ProbeDecision {
    if codec.is_none() {
        return apply_probe_chunk(state, chunk, end_of_stream);
    }
    if state.client_committed || matches!(state.phase, StreamPhase::Streaming) {
        return ProbeDecision::AlreadyCommitted;
    }
    state.phase = StreamPhase::Probing;
    if let Some(chunk) = chunk {
        state.buffer.extend_from_slice(&chunk);
    }
    let Some(boundary) = find_sse_boundary(&state.buffer) else {
        if end_of_stream && !state.buffer.is_empty() {
            return classify_unframed_tail(state);
        }
        return ProbeDecision::NeedMore;
    };
    let event = state.buffer[..boundary + 2].to_vec();
    probe_codec_event(state, codec.expect("checked above"), Bytes::from(event))
}

pub fn probe_codec_event(
    state: &mut StreamProbeState,
    codec: &gw_codec::AnyCodec,
    event: Bytes,
) -> ProbeDecision {
    match crate::filters::response::probe_stream_event(codec, &event) {
        crate::filters::response::StreamParseProbe::Valid => {
            state.phase = StreamPhase::Streaming;
            ProbeDecision::Commit(state.buffer.split().freeze())
        }
        crate::filters::response::StreamParseProbe::Malformed => {
            ProbeDecision::Fail(ProbeFailure::MalformedFrame)
        }
        crate::filters::response::StreamParseProbe::UpstreamErrorBody => {
            ProbeDecision::Fail(ProbeFailure::UpstreamErrorBody)
        }
    }
}

pub fn probe_timed_out(state: &StreamProbeState, now: Instant) -> bool {
    matches!(state.phase, StreamPhase::Connecting | StreamPhase::Probing)
        && !state.client_committed
        && now.duration_since(state.started_at) > state.timeout
}

pub fn client_commit_allowed(state: &StreamProbeState) -> bool {
    matches!(state.phase, StreamPhase::Streaming)
}

pub fn mark_client_committed(ctx: &mut crate::ctx::ReqCtx) {
    ctx.client_body_committed = true;
    ctx.body_started = true;
    ctx.stream_phase = StreamPhase::Streaming;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailoverOutcome {
    Retry,
    Exhausted,
    Timeout,
    NotRetryable,
}

pub fn can_retry_probe_failure(ctx: &crate::ctx::ReqCtx, _failure: ProbeFailure) -> bool {
    ctx.is_stream && !ctx.client_body_committed && !ctx.body_started
}

pub fn probe_failover_outcome<T>(
    remaining: &[T],
    attempt: u8,
    elapsed_ms: u64,
    budget_ms: u64,
) -> ProbeFailoverOutcome {
    if elapsed_ms >= budget_ms {
        return ProbeFailoverOutcome::Timeout;
    }
    if attempt > 3 || remaining.is_empty() {
        return ProbeFailoverOutcome::Exhausted;
    }
    ProbeFailoverOutcome::Retry
}

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn classify_unframed_tail(state: &mut StreamProbeState) -> ProbeDecision {
    let buf = state.buffer.as_ref();
    if contains_error_body(buf) {
        return ProbeDecision::Fail(ProbeFailure::UpstreamErrorBody);
    }
    if classify_event(buf).is_ok() {
        state.phase = StreamPhase::Streaming;
        return ProbeDecision::Commit(state.buffer.split().freeze());
    }
    ProbeDecision::Fail(ProbeFailure::MalformedFrame)
}

fn classify_event(event: &[u8]) -> Result<(), ProbeFailure> {
    if contains_error_body(event) {
        return Err(ProbeFailure::UpstreamErrorBody);
    }
    let data = first_data_line(event).ok_or(ProbeFailure::MalformedFrame)?;
    if data == b"[DONE]" {
        return Ok(());
    }
    serde_json::from_slice::<serde_json::Value>(data)
        .map(|_| ())
        .map_err(|_| ProbeFailure::MalformedFrame)
}

fn contains_error_body(buf: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(strip_sse_data_prefix(buf)) else {
        return false;
    };
    value.get("error").is_some()
}

fn strip_sse_data_prefix(buf: &[u8]) -> &[u8] {
    first_data_line(buf).unwrap_or(buf)
}

fn first_data_line(event: &[u8]) -> Option<&[u8]> {
    for line in event.split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(rest) = line.strip_prefix(b"data:") else {
            continue;
        };
        return Some(trim_ascii(rest));
    }
    None
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    while matches!(bytes.last(), Some(b' ' | b'\t' | b'\r')) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}
