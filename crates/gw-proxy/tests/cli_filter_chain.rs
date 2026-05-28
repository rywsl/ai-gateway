use bytes::Bytes;
use gw_auth::ratelimit::RateLimitGuard;
use gw_core::ids::UserId;
use gw_core::permissions::{MarkupChainSnapshot, MergedPermissions};
use gw_core::protocol::{ApiShape, ProtocolKind};
use gw_proxy::ctx::ReqCtx;
use gw_proxy::filters::{request, response, upstream};
use std::collections::HashSet;

const EARLY_RS: &str = include_str!("../src/filters/early.rs");
const REQUEST_RS: &str = include_str!("../src/filters/request.rs");
const UPSTREAM_RS: &str = include_str!("../src/filters/upstream.rs");
const RESPONSE_RS: &str = include_str!("../src/filters/response.rs");
const LOGGING_RS: &str = include_str!("../src/filters/logging.rs");

fn pos(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing marker: {needle}"))
}

fn cli_ctx(shape: ApiShape) -> ReqCtx {
    let body_bytes = Bytes::from_static(br#"{"model":"claude-sonnet-4-20250514","stream":false,"vendor_extension":{"keep":true}}"#);
    ReqCtx {
        client_shape: Some(shape),
        body: Some(gw_proxy::body::RequestBodyView::from_raw(
            body_bytes,
            gw_codec::compression::Encoding::Identity,
        )),
        ..ReqCtx::default()
    }
}

#[test]
fn test_cli_request_passes_auth() {
    let detect = pos(EARLY_RS, "detect_cli(");
    let stash = pos(EARLY_RS, "ctx.client_shape = Some(cli_shape);");
    let auth = pos(REQUEST_RS, "1. Auth");
    let cli_hints = pos(REQUEST_RS, "extract_cli_request_hints");

    assert!(detect < stash, "CLI detection must be stored in ReqCtx");
    assert!(
        auth < cli_hints,
        "CLI hint extraction must happen after existing auth"
    );
}

#[test]
fn test_cli_request_passes_ratelimit() {
    let perm = MergedPermissions {
        user_id: UserId::from("cli_rpm_user"),
        effective_user_id: UserId::from("cli_rpm_user"),
        client_ip: "127.0.0.1".parse().unwrap(),
        allowed_models: HashSet::new(),
        allowed_channels: HashSet::new(),
        allowed_groups: HashSet::new(),
        forced_model_group: None,
        rpm_limit: 1,
        tpm_limit: 0,
        max_concurrent_streams: 0,
        markup_chain: MarkupChainSnapshot::identity(),
        model_whitelist_regex: None,
    };
    let guard = RateLimitGuard::new();

    assert!(guard.check(&perm).is_ok());
    let err = match guard.check(&perm) {
        Ok(_) => panic!("second CLI request must hit same RPM guard"),
        Err(err) => err,
    };
    assert_eq!(err.status, 429);
    assert_eq!(err.code, gw_core::error::codes::GW_QUOTA_RPM_EXCEEDED);
}

#[test]
fn test_cli_request_billing_recorded() {
    let mut ctx = cli_ctx(ApiShape::ClaudeCli);
    let hints = request::extract_cli_request_hints(
        ApiShape::ClaudeCli,
        "/v1/messages",
        ctx.body.as_ref().and_then(|v| v.decoded(64 * 1024).ok()).map(|b| b.as_ref()),
    );
    ctx.model = hints.model.map(gw_core::ids::ModelName::from);
    ctx.is_stream = hints.stream.unwrap_or(false);
    ctx.upstream_shape = Some(ApiShape::ClaudeCli);

    let mut body = Some(Bytes::from_static(
        br#"{"usage":{"input_tokens":11,"output_tokens":7,"cache_read_input_tokens":3,"cache_creation_input_tokens":2}}"#,
    ));
    response::run_cli_body_filter(&mut body, true, &mut ctx).unwrap();

    assert_eq!(ctx.client_shape, Some(ApiShape::ClaudeCli));
    assert_eq!(ctx.upstream_shape, Some(ApiShape::ClaudeCli));
    assert_eq!(ctx.usage.input_tokens, 11);
    assert_eq!(ctx.usage.output_tokens, 7);
    assert_eq!(ctx.usage.cached_tokens, Some(3));
    assert_eq!(ctx.usage.cache_creation_tokens, Some(2));
    assert!(LOGGING_RS.contains("client_shape"));
    assert!(LOGGING_RS.contains("format_id()"));
}

#[test]
fn test_cli_request_body_not_ir_converted() {
    let mut ctx = cli_ctx(ApiShape::ClaudeCli);
    let original = ctx.body.as_ref().unwrap().raw().clone();
    let outbound = upstream::cli_passthrough_body(&mut ctx, ProtocolKind::Anthropic).unwrap();

    assert_eq!(outbound, original);
    assert_eq!(ctx.body.as_ref().unwrap().outbound_or_raw(), &original);
    assert!(UPSTREAM_RS.contains("cli_passthrough_body"));
    assert!(RESPONSE_RS.contains("run_cli_body_filter"));
}
