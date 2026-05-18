//! Unit tests for the retry executor decision matrix.

use gw_core::error::*;
use gw_routing::retry::*;
use std::borrow::Cow;

fn err(retryable: bool, cat: ErrorCategory) -> ErrorIR<'static> {
    ErrorIR {
        category: cat,
        status: 500,
        upstream_status: Some(500),
        code: Cow::Borrowed("x"),
        message: Cow::Borrowed(""),
        retryable,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    }
}

#[test]
fn non_retryable_skips() {
    let mut r = RetryExecutor::new(RetryConfig::default(), true, false);
    assert!(!r.should_retry(&err(false, ErrorCategory::ClientBadRequest), 0));
}

#[test]
fn stream_after_first_chunk_no_retry() {
    let mut r = RetryExecutor::new(RetryConfig::default(), true, true);
    // Once any bytes are flushed, we cannot rewind the SSE stream.
    assert!(!r.should_retry(&err(true, ErrorCategory::UpstreamError), 100));
}

#[test]
fn non_idempotent_only_retries_on_timeout() {
    let mut r = RetryExecutor::new(RetryConfig::default(), false, false);
    assert!(!r.should_retry(&err(true, ErrorCategory::UpstreamError), 0));
    let mut r2 = RetryExecutor::new(RetryConfig::default(), false, false);
    assert!(r2.should_retry(&err(true, ErrorCategory::UpstreamTimeout), 0));
}

#[test]
fn budget_exhaustion_stops() {
    let cfg = RetryConfig {
        total_budget_ms_nonstream: 0,
        ..Default::default()
    };
    let mut r = RetryExecutor::new(cfg, true, false);
    std::thread::sleep(std::time::Duration::from_millis(1));
    assert!(!r.should_retry(&err(true, ErrorCategory::UpstreamError), 0));
}
