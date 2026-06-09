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

    assert_eq!(decision, ProbeDecision::Fail(ProbeFailure::MalformedFrame));
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
fn valid_first_event_commits_all_buffered_probe_bytes() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let chunk = Bytes::from_static(
        br#"data: {"choices":[{"delta":{"content":"hi"}}]}

data: {"choices":[{"delta":{"content":" again"}}]}

"#,
    );
    let decision = apply_probe_chunk(&mut state, Some(chunk.clone()), false);

    assert_eq!(decision, ProbeDecision::Commit(chunk));
    assert!(state.buffer.is_empty());
    assert_eq!(state.phase, StreamPhase::Streaming);
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

#[test]
fn pingora_upstream_body_filter_runs_before_downstream_body_filter_in_proxy_sources() {
    let proxy_src = pingora_proxy_src();
    let h1 = std::fs::read_to_string(proxy_src.join("proxy_h1.rs")).expect("pingora h1 source");
    let upstream_pos = h1
        .find(".upstream_filter(session, &mut task, ctx)")
        .expect("h1 upstream_filter call exists");
    let response_pos = h1
        .find(".response_body_filter(session, &mut data, end, ctx)")
        .expect("h1 response_body_filter call exists");
    assert!(upstream_pos < response_pos);

    let h2 = std::fs::read_to_string(proxy_src.join("proxy_h2.rs")).expect("pingora h2 source");
    let upstream_pos = h2
        .find(".upstream_filter(session, &mut task, ctx)")
        .expect("h2 upstream_filter call exists");
    let response_pos = h2
        .find(".response_body_filter(session, &mut data, eos, ctx)")
        .expect("h2 response_body_filter call exists");
    assert!(upstream_pos < response_pos);
}

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

    assert!(!can_retry_probe_failure(&ctx, ProbeFailure::MalformedFrame));
    ctx.body_started = false;
    assert!(!can_retry_probe_failure(&ctx, ProbeFailure::MalformedFrame));
}

#[test]
fn timeout_failure_label_is_stable() {
    assert_eq!(ProbeFailure::Timeout.as_label(), "timeout");
}

#[test]
fn malformed_openai_stream_event_before_commit_maps_to_probe_failure() {
    let mut state = StreamProbeState::new(Duration::from_millis(250), Instant::now());
    let decision = gw_proxy::stream_probe::probe_codec_event(
        &mut state,
        &gw_codec::AnyCodec::OpenAi(gw_codec::openai::OpenAi),
        Bytes::from_static(b"data: {not-json}\n\n"),
    );

    assert_eq!(decision, ProbeDecision::Fail(ProbeFailure::MalformedFrame));
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
