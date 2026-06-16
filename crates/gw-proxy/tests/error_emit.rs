//! Unit tests for the error-emit path.
//!
//! `error_emit::send_error` writes to a Pingora `Session`. The Session struct
//! is not public-constructible (it has private fields tied to a live
//! downstream connection), so a true end-to-end test of `send_error` lives
//! in `end_to_end_openai.rs` (marked `#[ignore]` until the Pingora harness is
//! wired). Here we cover the *codec-side* contract that send_error relies on:
//! every supported codec must render an `ErrorIR` to a non-empty JSON body so
//! the proxy can never emit an empty error response.

use bytes::BytesMut;
use gw_codec::{AnyCodec, ProtocolCodec};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use std::borrow::Cow;

fn make_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 402,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_INSUFFICIENT),
        message: Cow::Borrowed("balance insufficient"),
        retryable: false,
        upstream_raw: None,
        request_id: "test-req-001",
        retry_after_secs: Some(30),
    }
}

#[test]
fn openai_render_error_is_non_empty_json() {
    let codec = gw_codec::openai::OpenAi;
    let mut buf = BytesMut::new();
    codec.render_error(&make_err(), &mut buf);
    let body = buf.freeze();
    assert!(!body.is_empty(), "render_error must produce non-empty body");
    let s = std::str::from_utf8(&body).expect("error body must be UTF-8");
    // OpenAI errors carry an `error` envelope; we only check the substring
    // here so codec internals can evolve without breaking this test.
    assert!(s.contains("error"));
    assert!(s.contains(codes::GW_QUOTA_BALANCE_INSUFFICIENT));
}

#[test]
fn anthropic_render_error_is_non_empty_json() {
    let codec = gw_codec::anthropic::Anthropic;
    let mut buf = BytesMut::new();
    codec.render_error(&make_err(), &mut buf);
    let body = buf.freeze();
    assert!(!body.is_empty());
    let s = std::str::from_utf8(&body).expect("error body must be UTF-8");
    assert!(s.contains(codes::GW_QUOTA_BALANCE_INSUFFICIENT));
}

#[test]
fn gemini_render_error_is_non_empty_json() {
    let codec = gw_codec::gemini::Gemini;
    let mut buf = BytesMut::new();
    codec.render_error(&make_err(), &mut buf);
    let body = buf.freeze();
    assert!(!body.is_empty());
    let s = std::str::from_utf8(&body).expect("error body must be UTF-8");
    assert!(s.contains(codes::GW_QUOTA_BALANCE_INSUFFICIENT));
}

#[test]
fn any_codec_dispatches_through_render_error() {
    // Confirms AnyCodec is the right dispatch type for `send_error`.
    let codec = AnyCodec::OpenAi(gw_codec::openai::OpenAi);
    let mut buf = BytesMut::new();
    codec.render_error(&make_err(), &mut buf);
    let body = buf.freeze();
    assert!(!body.is_empty());
}
