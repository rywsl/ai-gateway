//! Failure-mode integration tests (Phase 6.1, Step 2).
//!
//! These tests intentionally avoid the full Pingora harness — they exercise
//! the public APIs of the auth / codec / ledger layers directly, so they
//! run on every `cargo test --workspace` invocation without Docker.
//!
//! Mapping to the acceptance criteria from the spec:
//!
//! | Scenario | AC | Layer under test |
//! |----------|----|-----------------|
//! | `merge_permissions_returns_401_when_user_missing` | AC-023-3 | gw-auth |
//! | `hard_cap_input_tokens_413_at_boundary` | AC-024-2 | gw-auth |
//! | `hard_cap_input_tokens_passes_at_exact_cap` | AC-024-2 | gw-auth |
//! | `freeze_guard_double_release_idempotent_no_deducted_write` | ADR-004 | gw-ledger |
//! | `freeze_guard_drop_without_release_rolls_back` | ADR-004 | gw-ledger |
//! | `codec_parse_error_429_maps_to_rpm_exceeded` | AC-2.6-1 | gw-codec |
//! | `codec_parse_error_400_invalid_request_normalised` | AC-2.6-2 | gw-codec |
//! | `rate_limit_guard_emits_429_when_over_budget` | AC-024-1 | gw-auth |
//! | `error_renderer_round_trip_openai_400` | AC-2.6-2 | gw-codec |

use std::borrow::Cow;
use std::collections::HashSet;
use std::net::IpAddr;

use gw_auth::hard_cap::{check_input_tokens, HardCapCfg};
use gw_auth::ratelimit::RateLimitGuard;
use gw_auth::{merge_permissions, AuthCtx};
use gw_codec::{anthropic::Anthropic, openai::OpenAi, ProtocolCodec};
use gw_core::error::ErrorCategory;
use gw_core::ids::{ApiKeyId, UserId};
use gw_core::permissions::{MarkupChainSnapshot, MergedPermissions};
use gw_ledger::{FreezeGuard, RequestFreeze};
use gw_store::deduction::DEDUCT;
use gw_store::EntityStore;
use std::sync::atomic::Ordering;

fn client_ip() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

// ---------------------------------------------------------------------------
// AC-023-3: merge_permissions fails closed on missing user with 401.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn merge_permissions_returns_401_when_user_missing() {
    let store = EntityStore::new();
    let ctx = AuthCtx {
        user_id: UserId::from("nobody"),
        api_key_id: None,
        client_ip: client_ip(),
    };
    let err = merge_permissions(&ctx, &store, 100_000, None)
        .await
        .expect_err("expected error for missing user");
    assert_eq!(err.status, 401);
    assert!(matches!(err.category, ErrorCategory::ClientAuth));
}

// ---------------------------------------------------------------------------
// AC-024-2: ADR-024 single-request hard caps.
// ---------------------------------------------------------------------------
#[test]
fn hard_cap_input_tokens_413_at_boundary() {
    let cfg = HardCapCfg {
        max_input_tokens: 100,
        max_cost_micro: 1_000_000,
    };
    let err = check_input_tokens(101, &cfg).unwrap_err();
    assert_eq!(err.status, 413);
    assert_eq!(err.code.as_ref(), "GW_REQUEST_INPUT_TOO_LARGE");
    assert!(!err.retryable);
}

#[test]
fn hard_cap_input_tokens_passes_at_exact_cap() {
    // Boundary check: `est == cap` must pass; only `est > cap` rejects.
    let cfg = HardCapCfg {
        max_input_tokens: 100,
        max_cost_micro: 1_000_000,
    };
    assert!(check_input_tokens(100, &cfg).is_ok());
}

// ---------------------------------------------------------------------------
// C-γ §ADR-Cγ-2: RequestFreeze release is idempotent and never touches
// deducted_micro.
// ---------------------------------------------------------------------------
#[test]
fn freeze_guard_double_release_idempotent_no_deducted_write() {
    let uid = UserId::from("u-failmodes-double-release");
    let g = RequestFreeze::new(uid.clone(), 1000);
    g.release();
    g.release(); // no-op
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn freeze_guard_drop_without_release_rolls_back() {
    let uid = UserId::from("u-failmodes-leak");
    {
        let _g = RequestFreeze::new(uid.clone(), 500);
    } // dropped without release
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

// ---------------------------------------------------------------------------
// AC-2.6-1: codec `parse_error` maps an *upstream* 429 to
// GW_UPSTREAM_OVERLOADED. The gateway's own per-user RPM trip uses
// GW_QUOTA_RPM_EXCEEDED (see the rate-limit guard test below); keeping the
// codes distinct lets on-call distinguish "vendor capped us" from
// "gateway capped the user".
// ---------------------------------------------------------------------------
#[test]
fn codec_parse_error_429_maps_to_upstream_overloaded() {
    let codec = OpenAi;
    let body = br#"{"error":{"message":"rate limit hit","type":"rate_limit_error"}}"#;
    let err = codec.parse_error(429, body);
    assert_eq!(err.status, 429);
    assert_eq!(err.upstream_status, Some(429));
    assert_eq!(err.code.as_ref(), "GW_UPSTREAM_OVERLOADED");
    assert!(matches!(err.category, ErrorCategory::ClientQuota));
    assert!(
        err.retryable,
        "429 must be retryable per spec §6 retry matrix"
    );
}

// ---------------------------------------------------------------------------
// AC-2.6-2: Anthropic-shaped error body normalises to ErrorIR; re-rendering
// through the OpenAI codec produces an OpenAI-style {"error":{...}} envelope.
// ---------------------------------------------------------------------------
#[test]
fn codec_parse_error_400_invalid_request_normalised() {
    // Upstream Anthropic 400 body.
    let anthropic_body =
        br#"{"type":"error","error":{"type":"invalid_request_error","message":"bad input"}}"#;
    let anthropic = Anthropic;
    let err = anthropic.parse_error(400, anthropic_body);
    assert_eq!(err.status, 400);
    assert_eq!(err.message.as_ref(), "bad input");
    assert!(matches!(err.category, ErrorCategory::ClientBadRequest));
    assert!(!err.retryable);

    // Now render the same ErrorIR through OpenAI's codec — the client only
    // speaks OpenAI, so they should see an OpenAI-shaped error envelope.
    let openai = OpenAi;
    let mut buf = bytes::BytesMut::new();
    openai.render_error(&err, &mut buf);
    let v: serde_json::Value = serde_json::from_slice(&buf).expect("render_error JSON");
    assert!(
        v.get("error").is_some(),
        "OpenAI envelope must have `error` field"
    );
    assert_eq!(v["error"]["message"], "bad input");
}

// ---------------------------------------------------------------------------
// AC-2.6-2 follow-up: a 5xx upstream body classifies as UpstreamError and
// remains retryable per the codec contract.
// ---------------------------------------------------------------------------
#[test]
fn codec_parse_error_502_classifies_upstream_and_retryable() {
    let codec = OpenAi;
    let body = br#"{"error":{"message":"bad gateway","type":"server_error"}}"#;
    let err = codec.parse_error(502, body);
    assert_eq!(err.status, 502);
    assert!(matches!(err.category, ErrorCategory::UpstreamError));
    assert!(err.retryable);
}

// ---------------------------------------------------------------------------
// AC-024-1: per-user RPM hits 429 once the budget is exhausted.
//
// We pick `rpm_limit = 60` (1/sec mean) but blast 120 calls in <100ms so we
// drain the governor's token bucket and observe the 429.
// ---------------------------------------------------------------------------
#[test]
fn rate_limit_guard_emits_429_when_over_budget() {
    let g = RateLimitGuard::new();
    let perm = MergedPermissions {
        user_id: UserId::from("u-rpm"),
        effective_user_id: UserId::from("u-rpm"),
        client_ip: client_ip(),
        allowed_models: HashSet::new(),
        allowed_channels: HashSet::new(),
        allowed_groups: HashSet::new(),
        forced_model_group: None,
        rpm_limit: 60,
        tpm_limit: 0,
        max_concurrent_streams: 100,
        markup_chain: MarkupChainSnapshot::placeholder(),
    };
    // Burst 120 immediate calls; the governor cell holds at most 60 tokens
    // and refills at 1/sec, so we must hit a `RateLimitError` somewhere
    // in this window.
    let mut saw_429 = false;
    for _ in 0..120 {
        if let Err(e) = g.check(&perm) {
            assert_eq!(e.status, 429);
            assert_eq!(e.code.as_ref(), "GW_QUOTA_RPM_EXCEEDED");
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "expected at least one 429 within a 120-burst");
}

// ---------------------------------------------------------------------------
// Round-trip: ErrorIR -> render_error (OpenAI) -> parse_error -> ErrorIR.
// Confirms the same status and message survive a render/parse cycle.
// ---------------------------------------------------------------------------
#[test]
fn error_renderer_round_trip_openai_400() {
    let codec = OpenAi;
    let original = gw_core::error::ErrorIR {
        category: ErrorCategory::ClientBadRequest,
        status: 400,
        upstream_status: Some(400),
        code: Cow::Borrowed("GW_UPSTREAM_4XX"),
        message: Cow::Borrowed("model unknown"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    };
    let mut buf = bytes::BytesMut::new();
    codec.render_error(&original, &mut buf);
    let parsed = codec.parse_error(400, &buf);
    assert_eq!(parsed.status, 400);
    assert_eq!(parsed.message.as_ref(), "model unknown");
}

// ---------------------------------------------------------------------------
// Smoke: the unused `ApiKeyId` import path must compile (regression guard for
// the AuthCtx struct shape).
// ---------------------------------------------------------------------------
#[test]
fn auth_ctx_struct_signature_smoke() {
    let _ctx = AuthCtx {
        user_id: UserId::from("u"),
        api_key_id: Some(ApiKeyId::from("ak")),
        client_ip: client_ip(),
    };
}
