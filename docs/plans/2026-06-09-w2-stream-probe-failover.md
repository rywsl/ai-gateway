# W2 Stream Probe Failover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow routing failover for streaming upstreams that fail after upstream bytes arrive but before any client-visible SSE event or response body is committed.

**Architecture:** Split the current single `body_started` gate into explicit stream phases: `Connecting`, `Probing`, and `Streaming`. Probe bytes are inspected in `upstream_response_body_filter`, which Pingora runs before downstream response filters and writes, so malformed/error/timeout decisions can still raise a retryable upstream error without sending client-visible bytes. `response_body_filter` remains the final client-commit marker; once a valid event or body chunk is committed, existing no-retry semantics stay in force.

**Tech Stack:** Rust 1.88 workspace, Pingora 0.8 `ProxyHttp`, `gw-proxy`, `gw-routing`, `gw-gateway`, `gw-codec`, `metrics`, `cargo test`.

---

## Source Evidence

- W2 scope comes from `docs/plans/2026-06-09-remaining-roadmap-execution.md`: introduce Connecting/Probing/Streaming phases, distinguish first upstream byte from first client-committed event, classify upstream error body, malformed frame, and timeout as probe failures, emit `gw_stream_probe_*` metrics without key-level labels, and preserve no-retry after client-visible commit.
- W0 leaves R-F open: `docs/plans/2026-06-09-w0-contract-evidence.md` and `docs/spec/02-routing.md` record current failover as before-body only.
- Current `ReqCtx.body_started` is defined in `crates/gw-proxy/src/ctx.rs` as true once the first response body byte has been sent to the client, but `crates/gw-proxy/src/gateway.rs::response_body_filter` sets it at function entry with `if !ctx.body_started && body.is_some() { ctx.body_started = true; }`.
- `crates/gw-proxy/src/gateway.rs::response_filter` calls `gw_routing::classify_for_failover(status, ctx.body_started, &self.retryable_codes)`, so a partial upstream stream body seen by the body hook blocks failover even when no valid SSE event has reached the client.
- `crates/gw-proxy/src/filters/response.rs::run` currently parses stream chunks after `body_started` has already been set by `gateway.rs`; `handle_event` observes codec parse errors but returns without changing retry state.
- Pingora 0.8 runs `upstream_response_body_filter` before `response_filter` / `response_body_filter` and downstream writes. Evidence: `pingora-proxy-0.8.0/src/proxy_h1.rs::h1_response_filter` calls `upstream_filter` before downstream header/body filters; `proxy_trait.rs` exposes both `upstream_response_body_filter` and `response_body_filter`.
- The main retry loop retries errors whose `Error::retry()` remains true; `response_filter` already triggers this by constructing an `HTTPStatus` error and calling `set_retry(true)`.

## User Decision Needed

None from current evidence. The implementation path does not require a broader architecture change because `upstream_response_body_filter` can buffer/probe upstream body tasks before downstream header/body commit.

Stop and return `BLOCKED: Pingora body probe cannot safely replay` only if the execution spike in Task 2 proves one of these is true in this checkout:

- `upstream_response_body_filter` is not called before downstream response commit for the gateway's actual Pingora path.
- A retryable error raised from the upstream body probe cannot reach Pingora's main retry loop with `set_retry(true)`.
- The request body retry buffer is unavailable for this request shape, making replay unsafe after upstream body probe failure.

## Current Problem

The existing state variable conflates two different events:

- **First upstream byte:** Pingora received a body chunk from the vendor.
- **First client-committed event/body:** the gateway wrote a valid SSE event or body bytes to the downstream client.

Today `gateway.rs::response_body_filter` sets `ctx.body_started = true` before `filters::response::run` knows whether the chunk contains a complete valid SSE frame, malformed bytes, an upstream error envelope, or only a partial prefix. This means `classify_for_failover` later sees `BodyAlreadyStarted` and refuses retry even for the ROADMAP target case: upstream sent bytes, but the gateway has not committed a valid event to the client.

The fix is not to weaken post-commit behavior. It is to introduce a pre-commit probe phase where upstream bytes are buffered and inspected before they become client-visible.

## File Structure

- Create: `crates/gw-proxy/src/stream_probe.rs`
  - Own `StreamPhase`, `ProbeFailure`, `ProbeDecision`, probe buffering, SSE boundary detection, timeout clock checks, and metric emission helpers.
- Modify: `crates/gw-proxy/src/lib.rs`
  - Export `stream_probe`.
- Modify: `crates/gw-proxy/src/ctx.rs`
  - Add `stream_phase`, `client_body_committed`, `probe_started_at`, `probe_buffer`, `probe_timeout`, `pending_probe_failure`, and a retry marker such as `stream_probe_retry_pending`.
  - Keep `body_started` during the transition as a compatibility alias for final client commit only.
- Modify: `crates/gw-proxy/src/gateway.rs`
  - Add `upstream_response_body_filter` implementation.
  - Move first-client-body commit marking out of the current first line of `response_body_filter` and after response mutation has chosen non-empty downstream bytes.
  - Add a small helper mirroring the existing response-side failover retry mutation for stream-probe failures.
- Modify: `crates/gw-proxy/src/filters/response.rs`
  - Make stream parse errors return a structured result instead of silently observing and returning.
  - Let probe code call the same parser to decide whether a complete valid event exists.
- Modify: `crates/gw-routing/src/failover.rs`
  - Add stream-probe classification labels or a `FailoverClass::StreamProbeFailure` variant only if proxy metrics need the existing `record_failover_metric` path; keep routing labels strategy-level only.
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
  - Register `gw_stream_probe_total`, `gw_stream_probe_failover_total`, `gw_stream_probe_buffer_bytes`, and `gw_stream_probe_timeout_total`.
- Modify: `crates/gw-gateway/tests/metrics_catalog.rs`
  - Assert the four `gw_stream_probe_*` metrics are registered.
- Create: `crates/gw-proxy/tests/stream_probe.rs`
  - Focused pure tests for partial SSE, malformed before commit, timeout before commit, post-first-event no retry, and exhausted candidate.
- Modify: `docs/spec/02-routing.md`
  - Replace R-F ROADMAP pointer with as-built stream probe semantics after code lands.
- Modify: `docs/ROADMAP.md`
  - Close the R-F entry after verification.

## Task 1: Add Phase Model and Pure Probe Tests

**Files:**
- Create: `crates/gw-proxy/src/stream_probe.rs`
- Modify: `crates/gw-proxy/src/lib.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Create: `crates/gw-proxy/tests/stream_probe.rs`

- [ ] **Step 1: Write failing pure tests for probe state**

Create `crates/gw-proxy/tests/stream_probe.rs` with these tests:

```rust
use bytes::Bytes;
use gw_proxy::stream_probe::{
    apply_probe_chunk, client_commit_allowed, probe_timed_out, ProbeDecision, ProbeFailure,
    StreamPhase, StreamProbeState,
};
use std::time::{Duration, Instant};

#[test]
fn partial_sse_before_commit_buffers_without_retry_or_commit() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let decision = apply_probe_chunk(
        &mut state,
        Some(Bytes::from_static(b"data: {\"choices\":[")),
        false,
    );

    assert_eq!(decision, ProbeDecision::NeedMore);
    assert_eq!(state.phase, StreamPhase::Probing);
    assert!(!state.client_committed);
    assert_eq!(state.buffer.as_ref(), b"data: {\"choices\":[");
}

#[test]
fn malformed_sse_before_commit_is_retryable_probe_failure() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let decision = apply_probe_chunk(
        &mut state,
        Some(Bytes::from_static(b"data: {not-json}\n\n")),
        false,
    );

    assert_eq!(
        decision,
        ProbeDecision::Fail(ProbeFailure::MalformedFrame)
    );
    assert_eq!(state.phase, StreamPhase::Probing);
    assert!(!state.client_committed);
}

#[test]
fn upstream_error_body_before_commit_is_retryable_probe_failure() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let decision = apply_probe_chunk(
        &mut state,
        Some(Bytes::from_static(
            br#"data: {"error":{"message":"bad gateway","type":"server_error"}}"#,
        )),
        true,
    );

    assert_eq!(
        decision,
        ProbeDecision::Fail(ProbeFailure::UpstreamErrorBody)
    );
    assert_eq!(state.phase, StreamPhase::Probing);
    assert!(!state.client_committed);
}

#[test]
fn timeout_before_commit_is_retryable_probe_failure() {
    let start = Instant::now();
    let state = StreamProbeState::new(Duration::from_millis(10), start);

    assert!(probe_timed_out(&state, start + Duration::from_millis(11)));
}

#[test]
fn valid_first_event_enters_streaming_and_allows_commit() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let decision = apply_probe_chunk(
        &mut state,
        Some(Bytes::from_static(
            br#"data: {"choices":[{"delta":{"content":"hi"}}]}"#,
        )),
        true,
    );

    assert!(matches!(decision, ProbeDecision::Commit(_)));
    assert_eq!(state.phase, StreamPhase::Streaming);
    assert!(client_commit_allowed(&state));
}

#[test]
fn post_first_event_failure_is_not_probe_retryable() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let first = apply_probe_chunk(
        &mut state,
        Some(Bytes::from_static(
            br#"data: {"choices":[{"delta":{"content":"hi"}}]}"#,
        )),
        true,
    );
    assert!(matches!(first, ProbeDecision::Commit(_)));
    state.client_committed = true;

    let second = apply_probe_chunk(
        &mut state,
        Some(Bytes::from_static(b"data: {not-json}\n\n")),
        false,
    );

    assert_eq!(second, ProbeDecision::AlreadyCommitted);
    assert_eq!(state.phase, StreamPhase::Streaming);
}
```

- [ ] **Step 2: Run tests and verify expected failure**

Run:

```bash
cargo test -p gw-proxy --test stream_probe
```

Expected: compile failure with unresolved imports for `gw_proxy::stream_probe` and missing probe types/functions.

- [ ] **Step 3: Implement the minimal phase model**

Create `crates/gw-proxy/src/stream_probe.rs` with:

```rust
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
            return classify_unframed_tail(&state.buffer);
        }
        return ProbeDecision::NeedMore;
    };
    let event = state.buffer.split_to(boundary + 2).freeze();
    match classify_event(&event) {
        Ok(()) => {
            state.phase = StreamPhase::Streaming;
            ProbeDecision::Commit(event)
        }
        Err(failure) => ProbeDecision::Fail(failure),
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

fn find_sse_boundary(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn classify_unframed_tail(buf: &[u8]) -> ProbeDecision {
    if contains_error_body(buf) {
        ProbeDecision::Fail(ProbeFailure::UpstreamErrorBody)
    } else {
        ProbeDecision::Fail(ProbeFailure::MalformedFrame)
    }
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
```

Modify `crates/gw-proxy/src/lib.rs`:

```rust
pub mod stream_probe;
```

Add fields to `ReqCtx` in `crates/gw-proxy/src/ctx.rs`:

```rust
pub stream_phase: crate::stream_probe::StreamPhase,
pub client_body_committed: bool,
pub probe_started_at: Option<std::time::Instant>,
pub probe_timeout: std::time::Duration,
pub probe_buffer: bytes::BytesMut,
pub pending_probe_failure: Option<crate::stream_probe::ProbeFailure>,
pub stream_probe_retry_pending: bool,
```

Set defaults in `ReqCtx::default()`:

```rust
stream_phase: crate::stream_probe::StreamPhase::Connecting,
client_body_committed: false,
probe_started_at: None,
probe_timeout: std::time::Duration::from_millis(750),
probe_buffer: bytes::BytesMut::new(),
pending_probe_failure: None,
stream_probe_retry_pending: false,
```

- [ ] **Step 4: Run tests and verify pass**

Run:

```bash
cargo test -p gw-proxy --test stream_probe
```

Expected: all six stream-probe pure tests pass.

## Task 2: Prove Pingora Hook Safety Before Wiring Retry

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/tests/stream_probe.rs`

- [ ] **Step 1: Add a source-order assertion test**

Append to `crates/gw-proxy/tests/stream_probe.rs`:

```rust
#[test]
fn pingora_upstream_body_filter_runs_before_downstream_body_filter_in_proxy_sources() {
    let proxy_src = pingora_proxy_src();
    let h1 = std::fs::read_to_string(proxy_src.join("proxy_h1.rs"))
        .expect("pingora h1 source");
    let upstream_pos = h1
        .find(".upstream_filter(session, &mut task, ctx)")
        .expect("h1 upstream_filter call exists");
    let response_pos = h1
        .find(".response_body_filter(session, &mut data, end, ctx)")
        .expect("h1 response_body_filter call exists");
    assert!(upstream_pos < response_pos);

    let h2 = std::fs::read_to_string(proxy_src.join("proxy_h2.rs"))
        .expect("pingora h2 source");
    let upstream_pos = h2
        .find(".upstream_filter(session, &mut task, ctx)")
        .expect("h2 upstream_filter call exists");
    let response_pos = h2
        .find(".response_body_filter(session, &mut data, eos, ctx)")
        .expect("h2 response_body_filter call exists");
    assert!(upstream_pos < response_pos);
}

fn pingora_proxy_src() -> std::path::PathBuf {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(std::path::PathBuf::from)
                .map(|home| home.join(".cargo"))
        })
        .expect("CARGO_HOME or HOME");
    let src_root = cargo_home.join("registry/src");
    for registry in std::fs::read_dir(&src_root).expect("cargo registry src") {
        let registry = registry.expect("registry entry").path();
        let candidate = registry.join("pingora-proxy-0.8.0/src");
        if candidate.join("proxy_h1.rs").exists() && candidate.join("proxy_h2.rs").exists() {
            return candidate;
        }
    }
    panic!("pingora-proxy-0.8.0 source not found under {src_root:?}");
}
```

- [ ] **Step 2: Run the hook-order test**

Run:

```bash
cargo test -p gw-proxy --test stream_probe pingora_upstream_body_filter_runs_before_downstream_body_filter_in_proxy_sources
```

Expected: PASS. If it fails because the vendored path hash differs, adjust only the test path discovery to find `pingora-proxy-0.8.0/src/proxy_h1.rs` under the local Cargo registry. If it fails because upstream filter order is different, stop with `BLOCKED: Pingora upstream body hook is not pre-commit in this checkout`.

- [ ] **Step 3: Add a no-op upstream body filter implementation**

Add this method to the `impl ProxyHttp for LlmGateway` in `crates/gw-proxy/src/gateway.rs`:

```rust
fn upstream_response_body_filter(
    &self,
    _session: &mut Session,
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut Self::CTX,
) -> Result<Option<Duration>> {
    if !ctx.is_stream {
        return Ok(None);
    }
    if ctx.probe_started_at.is_none() {
        ctx.probe_started_at = Some(std::time::Instant::now());
        ctx.stream_phase = crate::stream_probe::StreamPhase::Probing;
    }
    if end_of_stream && body.is_none() {
        metrics::counter!("gw_stream_probe_total", "outcome" => "empty_eos").increment(1);
    }
    Ok(None)
}
```

- [ ] **Step 4: Compile the gateway crate**

Run:

```bash
cargo test -p gw-proxy --lib upstream_response_body_filter
```

Expected: compile succeeds. If the method signature does not match Pingora 0.8 `ProxyHttp`, fix the signature to match `proxy_trait.rs::upstream_response_body_filter` exactly, then rerun.

## Task 3: Buffer Probe Bytes and Gate Client Commit

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/tests/stream_probe.rs`

- [ ] **Step 1: Add focused tests for client commit semantics**

Append:

```rust
#[test]
fn body_started_tracks_client_commit_not_upstream_byte() {
    let mut ctx = gw_proxy::ctx::ReqCtx {
        is_stream: true,
        stream_phase: StreamPhase::Probing,
        client_body_committed: false,
        body_started: false,
        ..Default::default()
    };
    ctx.probe_buffer.extend_from_slice(b"data: {\"partial\"");

    assert!(!ctx.body_started);
    assert!(!ctx.client_body_committed);
}

#[test]
fn mark_client_commit_sets_body_started_alias() {
    let mut ctx = gw_proxy::ctx::ReqCtx::default();
    gw_proxy::stream_probe::mark_client_committed(&mut ctx);

    assert!(ctx.client_body_committed);
    assert!(ctx.body_started);
    assert_eq!(ctx.stream_phase, StreamPhase::Streaming);
}
```

- [ ] **Step 2: Run tests and verify expected failure**

Run:

```bash
cargo test -p gw-proxy --test stream_probe body_started_tracks_client_commit mark_client_commit
```

Expected: compile failure for missing `mark_client_committed` or missing new `ReqCtx` fields if Task 1 was not completed.

- [ ] **Step 3: Add commit helper**

Add to `crates/gw-proxy/src/stream_probe.rs`:

```rust
pub fn mark_client_committed(ctx: &mut crate::ctx::ReqCtx) {
    ctx.client_body_committed = true;
    ctx.body_started = true;
    ctx.stream_phase = StreamPhase::Streaming;
}
```

- [ ] **Step 4: Change `response_body_filter` commit marking**

Replace the current early mark in `crates/gw-proxy/src/gateway.rs::response_body_filter`:

```rust
if !ctx.body_started && body.is_some() {
    ctx.body_started = true;
}
```

with a post-filter mark after `filters::response::run(...)` has returned and after passthrough substitution has decided the final downstream `body`:

```rust
let result = filters::response::run(session, body, end_of_stream, ctx);
if result.is_ok() {
    if body.as_ref().map(|b| !b.is_empty()).unwrap_or(false) {
        crate::stream_probe::mark_client_committed(ctx);
    }
}
return result;
```

Keep the existing deadline/grace code order intact by moving only the mark, not the rest of `response_body_filter`. If the function currently has logic before and after `filters::response::run`, preserve that logic and place `mark_client_committed` immediately after the final body mutation point.

- [ ] **Step 5: Run tests**

Run:

```bash
cargo test -p gw-proxy --test stream_probe body_started_tracks_client_commit mark_client_commit
```

Expected: both tests pass.

## Task 4: Convert Probe Failures Into Cross-Key Retry

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/stream_probe.rs`
- Modify: `crates/gw-routing/src/failover.rs`
- Create or modify: `crates/gw-routing/tests/failover.rs` if external integration coverage is preferred over unit-only tests

- [ ] **Step 1: Add failover decision tests for exhausted candidate**

Add to `crates/gw-proxy/tests/stream_probe.rs`:

```rust
#[test]
fn exhausted_candidate_sets_probe_failure_without_retry() {
    use gw_proxy::stream_probe::{probe_failover_outcome, ProbeFailoverOutcome};

    let remaining: Vec<std::sync::Arc<gw_core::entity::VendorKey>> = Vec::new();
    let outcome = probe_failover_outcome(&remaining, 1, 0, 30_000);

    assert_eq!(outcome, ProbeFailoverOutcome::Exhausted);
}

#[test]
fn post_first_event_no_retry_even_for_probe_failure() {
    use gw_proxy::stream_probe::{can_retry_probe_failure, ProbeFailure};

    let mut ctx = gw_proxy::ctx::ReqCtx {
        is_stream: true,
        client_body_committed: true,
        body_started: true,
        stream_phase: StreamPhase::Streaming,
        ..Default::default()
    };

    assert!(!can_retry_probe_failure(
        &ctx,
        ProbeFailure::MalformedFrame
    ));
    ctx.body_started = false;
    assert!(!can_retry_probe_failure(
        &ctx,
        ProbeFailure::MalformedFrame
    ));
}
```

- [ ] **Step 2: Run tests and verify expected failure**

Run:

```bash
cargo test -p gw-proxy --test stream_probe exhausted_candidate post_first_event_no_retry
```

Expected: compile failure for missing `ProbeFailoverOutcome`, `probe_failover_outcome`, or `can_retry_probe_failure`.

- [ ] **Step 3: Add pure retry gate helpers**

Add to `crates/gw-proxy/src/stream_probe.rs`:

```rust
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
```

- [ ] **Step 4: Wire `upstream_response_body_filter` to buffer, classify, and raise retry**

Change `LlmGateway::upstream_response_body_filter` to:

```rust
fn upstream_response_body_filter(
    &self,
    _session: &mut Session,
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut Self::CTX,
) -> Result<Option<Duration>> {
    if !ctx.is_stream || ctx.client_body_committed {
        return Ok(None);
    }

    let started_at = *ctx
        .probe_started_at
        .get_or_insert_with(std::time::Instant::now);
    ctx.stream_phase = crate::stream_probe::StreamPhase::Probing;

    let mut probe_state = crate::stream_probe::StreamProbeState {
        phase: ctx.stream_phase,
        client_committed: ctx.client_body_committed,
        started_at,
        timeout: ctx.probe_timeout,
        buffer: std::mem::take(&mut ctx.probe_buffer),
    };

    let decision = crate::stream_probe::apply_probe_chunk(
        &mut probe_state,
        body.take(),
        end_of_stream,
    );
    ctx.stream_phase = probe_state.phase;
    ctx.probe_buffer = probe_state.buffer;

    match decision {
        crate::stream_probe::ProbeDecision::NeedMore => {
            *body = None;
            Ok(None)
        }
        crate::stream_probe::ProbeDecision::Commit(bytes) => {
            ctx.stream_phase = crate::stream_probe::StreamPhase::Streaming;
            *body = Some(bytes);
            metrics::counter!("gw_stream_probe_total", "outcome" => "commit").increment(1);
            Ok(None)
        }
        crate::stream_probe::ProbeDecision::Fail(failure) => {
            ctx.pending_probe_failure = Some(failure);
            ctx.stream_probe_retry_pending = true;
            *body = None;
            metrics::counter!(
                "gw_stream_probe_total",
                "outcome" => failure.as_label(),
            )
            .increment(1);
            let mut e = Error::new(ErrorType::HTTPStatus(ctx.upstream_status.unwrap_or(502)));
            e.as_up();
            e.set_retry(true);
            Err(e)
        }
        crate::stream_probe::ProbeDecision::AlreadyCommitted => Ok(None),
    }
}
```

Then add an `error_while_proxy` override if the default Pingora implementation clears the explicit probe retry flag for non-reused upstream connections:

```rust
fn error_while_proxy(
    &self,
    peer: &HttpPeer,
    session: &mut Session,
    mut e: Box<Error>,
    ctx: &mut Self::CTX,
    client_reused: bool,
) -> Box<Error> {
    if ctx.stream_probe_retry_pending {
        e.set_retry(true);
        return e.more_context(format!("Peer: {}", peer));
    }
    let mut e = e.more_context(format!("Peer: {}", peer));
    e.retry
        .decide_reuse(client_reused && !session.as_ref().retry_buffer_truncated());
    e
}
```

- [ ] **Step 5: On retry, mutate context exactly like response-side failover**

In the same probe failure branch, before returning `Err(e)`, perform the same state transition used by `response_filter` retry:

```rust
let Some(candidates) = ctx.picked_candidates.as_ref() else {
    ctx.upstream_error_code = Some(gw_core::error::codes::GW_ROUTING_NO_FALLBACK_LEFT);
    return Ok(None);
};
let remaining = remaining_failover_candidates(candidates, ctx);
let attempt = ctx.failover_attempt.saturating_add(1);
let decision = next_failover_decision(&self.failover_executor, &remaining, attempt, ctx);
match decision {
    FailoverDecision::Retry(new_key) => {
        crate::billing_lifecycle::settle_prior_attempt_spawn(
            ctx,
            self.inflight.clone(),
            self.ledger.clone(),
        );
        if let Some(k) = ctx.picked_vendor_key.as_ref() {
            if !ctx.retry_blacklist.iter().any(|id| id == &k.key_id) {
                ctx.retry_blacklist.push(k.key_id.clone());
            }
        }
        ctx.failover_attempt = attempt;
        ctx.inflight_guard = None;
        ctx.picked_vendor_key = Some(new_key);
        ctx.picked_vendor = None;
        ctx.upstream_codec = None;
        ctx.upstream_status = None;
        ctx.body_started = false;
        ctx.client_body_committed = false;
        ctx.stream_phase = crate::stream_probe::StreamPhase::Connecting;
        ctx.probe_started_at = None;
        ctx.probe_buffer.clear();
        metrics::counter!(
            "gw_stream_probe_failover_total",
            "reason" => ctx.pending_probe_failure.unwrap().as_label(),
            "strategy" => ctx.strategy_name.unwrap_or("unknown").to_owned(),
            "outcome" => "retry",
        )
        .increment(1);
    }
    FailoverDecision::Exhausted => {
        ctx.upstream_error_code = Some(gw_core::error::codes::GW_ROUTING_NO_FALLBACK_LEFT);
        metrics::counter!(
            "gw_stream_probe_failover_total",
            "reason" => ctx.pending_probe_failure.unwrap().as_label(),
            "strategy" => ctx.strategy_name.unwrap_or("unknown").to_owned(),
            "outcome" => "exhausted",
        )
        .increment(1);
        return Ok(None);
    }
    FailoverDecision::Timeout => {
        ctx.upstream_error_code = Some(gw_core::error::codes::GW_ROUTING_FAILOVER_TIMEOUT);
        metrics::counter!(
            "gw_stream_probe_failover_total",
            "reason" => "timeout",
            "strategy" => ctx.strategy_name.unwrap_or("unknown").to_owned(),
            "outcome" => "timeout",
        )
        .increment(1);
        return Ok(None);
    }
    FailoverDecision::NotRetryable => return Ok(None),
}
```

Keep labels to `reason`, `strategy`, and `outcome`; do not add API-key, vendor-key, model, or user labels.

- [ ] **Step 6: Run focused tests**

Run:

```bash
cargo test -p gw-proxy --test stream_probe exhausted_candidate post_first_event_no_retry
cargo test -p gw-proxy --lib stream_probe
```

Expected: all stream-probe tests pass and `gw-proxy` compiles.

## Task 5: Add Probe Timeout Watchdog

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/stream_probe.rs`
- Modify: `crates/gw-proxy/tests/stream_probe.rs`

- [ ] **Step 1: Add timeout behavior test**

Append:

```rust
#[test]
fn timeout_failure_label_is_stable() {
    assert_eq!(ProbeFailure::Timeout.as_label(), "timeout");
}
```

- [ ] **Step 2: Run test and verify pass or expected failure**

Run:

```bash
cargo test -p gw-proxy --test stream_probe timeout_failure_label_is_stable
```

Expected: PASS if Task 1 is complete. If it fails, fix `ProbeFailure::as_label` before wiring timeout.

- [ ] **Step 3: Check timeout at upstream body filter entry**

At the top of the stream branch in `upstream_response_body_filter`, after setting `probe_started_at`, add:

```rust
let now = std::time::Instant::now();
let probe_state = crate::stream_probe::StreamProbeState {
    phase: ctx.stream_phase,
    client_committed: ctx.client_body_committed,
    started_at,
    timeout: ctx.probe_timeout,
    buffer: ctx.probe_buffer.clone(),
};
if crate::stream_probe::probe_timed_out(&probe_state, now) {
    ctx.pending_probe_failure = Some(crate::stream_probe::ProbeFailure::Timeout);
    ctx.stream_probe_retry_pending = true;
    metrics::counter!("gw_stream_probe_timeout_total").increment(1);
    metrics::counter!(
        "gw_stream_probe_total",
        "outcome" => "timeout",
    )
    .increment(1);
    let mut e = Error::new(ErrorType::HTTPStatus(504));
    e.as_up();
    e.set_retry(true);
    return Err(e);
}
```

Route this timeout branch through the same context mutation helper as other probe failures. Extract a private `retry_stream_probe_failure(&self, ctx, failure, status) -> Result<bool>` helper if the branch duplicates more than 20 lines from Task 4.

- [ ] **Step 4: Run timeout and compile tests**

Run:

```bash
cargo test -p gw-proxy --test stream_probe timeout
cargo test -p gw-proxy --lib upstream_response_body_filter
```

Expected: timeout unit tests pass and `gw-proxy` compiles.

## Task 6: Make Codec Parse Errors Visible to Probe

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/stream_probe.rs`
- Modify: `crates/gw-proxy/tests/stream_probe.rs`

- [ ] **Step 1: Add malformed-before-commit parser test**

Add a test that exercises the real response parser path:

```rust
#[test]
fn malformed_openai_stream_event_before_commit_maps_to_probe_failure() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let decision = gw_proxy::stream_probe::probe_codec_event(
        &mut state,
        gw_core::protocol::ProtocolKind::OpenAi,
        Bytes::from_static(b"data: {not-json}\n\n"),
    );

    assert_eq!(
        decision,
        ProbeDecision::Fail(ProbeFailure::MalformedFrame)
    );
}
```

- [ ] **Step 2: Run test and verify expected failure**

Run:

```bash
cargo test -p gw-proxy --test stream_probe malformed_openai_stream_event_before_commit_maps_to_probe_failure
```

Expected: compile failure for missing `probe_codec_event`.

- [ ] **Step 3: Expose a structured stream parse result**

In `crates/gw-proxy/src/filters/response.rs`, extract the parse-only part of `handle_event`:

```rust
pub enum StreamParseProbe {
    Valid,
    Malformed,
    UpstreamErrorBody,
}

pub fn probe_stream_event(codec: &AnyCodec, event_bytes: &[u8]) -> StreamParseProbe {
    if serde_json::from_slice::<serde_json::Value>(strip_sse_data(event_bytes))
        .ok()
        .and_then(|v| v.get("error").cloned())
        .is_some()
    {
        return StreamParseProbe::UpstreamErrorBody;
    }
    let mut state = codec.new_stream_state();
    match codec.parse_stream_chunk(state.as_mut(), event_bytes) {
        Ok(_) => StreamParseProbe::Valid,
        Err(_) => StreamParseProbe::Malformed,
    }
}
```

Use the existing local SSE data extraction helper if present; otherwise keep it private to `filters::response`.

In `crates/gw-proxy/src/stream_probe.rs`, add:

```rust
pub fn probe_codec_event(
    state: &mut StreamProbeState,
    kind: gw_core::protocol::ProtocolKind,
    event: Bytes,
) -> ProbeDecision {
    let codec = match kind {
        gw_core::protocol::ProtocolKind::OpenAi => {
            gw_codec::AnyCodec::OpenAi(gw_codec::openai::OpenAi)
        }
        gw_core::protocol::ProtocolKind::Anthropic => {
            gw_codec::AnyCodec::Anthropic(gw_codec::anthropic::Anthropic)
        }
        gw_core::protocol::ProtocolKind::Gemini => {
            gw_codec::AnyCodec::Gemini(gw_codec::gemini::Gemini)
        }
    };
    match crate::filters::response::probe_stream_event(&codec, &event) {
        crate::filters::response::StreamParseProbe::Valid => {
            state.phase = StreamPhase::Streaming;
            ProbeDecision::Commit(event)
        }
        crate::filters::response::StreamParseProbe::Malformed => {
            ProbeDecision::Fail(ProbeFailure::MalformedFrame)
        }
        crate::filters::response::StreamParseProbe::UpstreamErrorBody => {
            ProbeDecision::Fail(ProbeFailure::UpstreamErrorBody)
        }
    }
}
```

Then update `apply_probe_chunk` to call `probe_codec_event` when `ReqCtx.upstream_codec` is known. If keeping `apply_probe_chunk` pure, add a separate gateway-only path that uses `probe_codec_event` after SSE boundary detection and before committing bytes.

- [ ] **Step 4: Run parser probe tests**

Run:

```bash
cargo test -p gw-proxy --test stream_probe malformed_openai_stream_event_before_commit_maps_to_probe_failure
cargo test -p gw-proxy --lib response::tests
```

Expected: malformed-before-commit test passes; existing response filter tests still pass.

## Task 7: Register Metrics and Assert No Key-Level Labels

**Files:**
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `crates/gw-gateway/tests/metrics_catalog.rs`
- Modify: `crates/gw-proxy/src/stream_probe.rs`

- [ ] **Step 1: Add catalog tests first**

Append to `crates/gw-gateway/tests/metrics_catalog.rs`:

```rust
#[test]
fn stream_probe_metrics_registered_without_key_level_labels() {
    let names: Vec<&str> = METRICS.iter().map(|m| m.name).collect();
    for required in [
        "gw_stream_probe_total",
        "gw_stream_probe_failover_total",
        "gw_stream_probe_buffer_bytes",
        "gw_stream_probe_timeout_total",
    ] {
        assert!(names.contains(&required), "missing metric: {required}");
    }

    for metric in METRICS
        .iter()
        .filter(|m| m.name.starts_with("gw_stream_probe_"))
    {
        assert!(!metric.help.contains("key_id"));
        assert!(!metric.help.contains("vendor_key"));
        assert!(!metric.help.contains("api_key"));
        assert!(!metric.help.contains("user_id"));
    }
}
```

- [ ] **Step 2: Run test and verify expected failure**

Run:

```bash
cargo test -p gw-gateway --test metrics_catalog stream_probe_metrics_registered_without_key_level_labels
```

Expected: failure listing missing `gw_stream_probe_*` metrics.

- [ ] **Step 3: Register metrics**

Add to `METRICS` in `crates/gw-gateway/src/metrics_catalog.rs`:

```rust
MetricSpec {
    name: "gw_stream_probe_total",
    kind: MetricKind::Counter,
    help: "stream probe outcomes before client commit; labels=outcome",
},
MetricSpec {
    name: "gw_stream_probe_failover_total",
    kind: MetricKind::Counter,
    help: "stream probe failover decisions before client commit; labels=reason,strategy,outcome",
},
MetricSpec {
    name: "gw_stream_probe_buffer_bytes",
    kind: MetricKind::Histogram,
    help: "bytes buffered during stream probe before first client-visible event",
},
MetricSpec {
    name: "gw_stream_probe_timeout_total",
    kind: MetricKind::Counter,
    help: "stream probe timeout events before client commit",
},
```

Emit `gw_stream_probe_buffer_bytes` when probe commits or fails:

```rust
metrics::histogram!("gw_stream_probe_buffer_bytes").record(ctx.probe_buffer.len() as f64);
```

- [ ] **Step 4: Run metrics test**

Run:

```bash
cargo test -p gw-gateway --test metrics_catalog stream_probe_metrics_registered_without_key_level_labels
```

Expected: PASS.

## Task 8: Focused End-to-End Failure Mode Coverage

**Files:**
- Modify: `crates/gw-gateway/tests/e2e_failure_modes.rs`
- Modify: `crates/gw-proxy/tests/stream_probe.rs`

- [ ] **Step 1: Add gateway-layer assertions for the five acceptance cases**

Add pure gateway-layer tests to `crates/gw-gateway/tests/e2e_failure_modes.rs` if a full Pingora harness is still unavailable:

```rust
#[test]
fn stream_probe_acceptance_matrix_is_covered_by_proxy_tests() {
    let proxy_tests = include_str!("../../gw-proxy/tests/stream_probe.rs");
    for required in [
        "partial_sse_before_commit_buffers_without_retry_or_commit",
        "malformed_sse_before_commit_is_retryable_probe_failure",
        "timeout_before_commit_is_retryable_probe_failure",
        "post_first_event_failure_is_not_probe_retryable",
        "exhausted_candidate_sets_probe_failure_without_retry",
    ] {
        assert!(
            proxy_tests.contains(required),
            "missing stream probe acceptance test: {required}"
        );
    }
}
```

If the gateway test suite has a live local fake-upstream harness by implementation time, replace this source assertion with real HTTP tests:

```text
first upstream returns partial SSE then malformed frame, second upstream returns valid SSE -> client sees second upstream only
first upstream returns error SSE before commit, second upstream returns valid SSE -> client sees second upstream only
first upstream stalls before first event past probe_timeout, second upstream returns valid SSE -> client sees second upstream only
first upstream returns valid first event then malformed frame -> no retry, client receives stream error/truncation
only candidate returns malformed before commit -> final client error uses GW_ROUTING_NO_FALLBACK_LEFT or GW_ROUTING_FAILOVER_TIMEOUT
```

- [ ] **Step 2: Run gateway failure-mode test**

Run:

```bash
cargo test -p gw-gateway --test e2e_failure_modes stream_probe_acceptance_matrix_is_covered_by_proxy_tests
```

Expected: PASS once the focused proxy tests exist.

- [ ] **Step 3: Run the focused W2 verification set**

Run:

```bash
cargo test -p gw-routing failover
cargo test -p gw-proxy --test stream_probe
cargo test -p gw-gateway --test e2e_failure_modes
cargo test -p gw-gateway --test metrics_catalog stream_probe_metrics_registered_without_key_level_labels
```

Expected: all pass.

## Task 9: Close Routing Docs and ROADMAP

**Files:**
- Modify: `docs/spec/02-routing.md`
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Update `docs/spec/02-routing.md`**

In the Failover section, replace the current before-body-only language with:

```markdown
### Stream probe failover window

Streaming requests use explicit phases:

- `Connecting`: upstream request has been sent but no upstream response body bytes have been observed.
- `Probing`: upstream response body bytes have arrived, but no valid client-visible SSE event/body chunk has been committed downstream.
- `Streaming`: at least one valid client-visible event/body chunk has been committed downstream.

The proxy may fail over across vendor keys during `Connecting` and `Probing`. Probe retry is allowed for upstream error bodies, malformed first frames, and probe timeout, provided `client_body_committed == false`. Once `Streaming` is reached, failover keeps the existing no-retry behavior and mid-stream failures surface as stream/upstream errors for the current attempt.

Probe metrics are emitted as `gw_stream_probe_*` with low-cardinality labels only; API-key, vendor-key, model, and user labels are intentionally absent.
```

Also change the Deliberate behaviours bullet from:

```markdown
- **Failover is before-body only.** Once `body_started`, `classify_for_failover` returns `BodyAlreadyStarted` and no retry occurs, even for a 5xx/429 status.
```

to:

```markdown
- **Failover is before-client-commit only.** Header/status retries still use `classify_for_failover`; stream probe retries may also occur after upstream bytes arrive but before `client_body_committed`. Once a client-visible event/body chunk is committed, `body_started` remains true and `classify_for_failover` returns `BodyAlreadyStarted`.
```

- [ ] **Step 2: Update `docs/ROADMAP.md`**

Replace the R-F entry under `## 02 routing` with:

```markdown
### R-F stream probe (first-chunk failover window) — closed in spec
- **Landed:** streaming upstreams now use Connecting/Probing/Streaming phases. Probe failures before any client-visible event/body commit can fail over to the next ranked candidate; after the first committed event/body chunk, existing no-retry semantics remain.
- **Probe failures:** upstream error body, malformed frame, and probe timeout.
- **Metrics:** `gw_stream_probe_*` with no key-level labels.
- **Spec:** `docs/spec/02-routing.md`.
```

- [ ] **Step 3: Run spec checks**

Run:

```bash
bash scripts/check-spec.sh docs/spec/02-routing.md
rg -n "R-F stream probe|gw_stream_probe|body_started|client_body_committed" docs/spec/02-routing.md docs/ROADMAP.md
```

Expected: spec check passes; grep shows the closed R-F wording and no remaining "not built" pointer for R-F.

- [ ] **Step 4: Commit implementation and docs in suggested implementation granularity**

Implementation workers should use:

```bash
git add crates/gw-proxy crates/gw-routing crates/gw-gateway
git commit -m "feat(routing): add stream probe failover phase"
git add docs/spec/02-routing.md docs/ROADMAP.md
git commit -m "docs(routing): close R-F stream probe entry"
```

## Final Verification

- [ ] **Step 1: Run focused verification**

Run:

```bash
cargo test -p gw-routing failover
cargo test -p gw-proxy --test stream_probe
cargo test -p gw-gateway --test e2e_failure_modes
cargo test -p gw-gateway --test metrics_catalog stream_probe_metrics_registered_without_key_level_labels
bash scripts/check-spec.sh docs/spec/02-routing.md
git diff --check
```

Expected: all commands pass.

- [ ] **Step 2: Run broad gate before merge**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-invariants.sh
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f"; done
```

Expected: all commands pass.

## Submission Notes

- Plan-only worker commit for this document:

```bash
git add docs/plans/2026-06-09-w2-stream-probe-failover.md
git commit -m "docs(plan): detail stream probe failover"
```

- Suggested implementation commits after this plan is executed:

```bash
git commit -m "feat(routing): add stream probe failover phase"
git commit -m "docs(routing): close R-F stream probe entry"
```
