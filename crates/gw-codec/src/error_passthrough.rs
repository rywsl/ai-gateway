//! Render upstream-vendor error bodies into client-protocol error envelopes.
//!
//! Each protocol expects a different error shape:
//! - OpenAI: `{"error": {"type": ..., "message": ..., "code": <GW_...>}}`
//! - Anthropic: `{"type": "error", "error": {"type": ..., "message": ..., "code": <GW_...>}}`
//! - Gemini: `{"error": {"code": <int>, "status": ..., "message": ..., "gateway_code": <GW_...>}}`
//! - Kiro: `{"type": "error", "code": <vendor type>, "message": ..., "gateway_code": <GW_...>}`
//!
//! Every envelope carries a stable `GW_...` gateway code (P0 #16) so SSE
//! `event: error` frames and non-stream errors alike give clients/ops a stable
//! field to branch on. The vendor-compatible `type`/`status`/numeric Gemini
//! `code` are preserved; the gateway code is additive.
//!
//! For SSE responses, wrap as `event: error\ndata: <json>\n\n`.

use bytes::Bytes;
use gw_core::error::rules::{message_allowed, PassthroughRule};
use gw_core::error::{codes, ErrorClass, PassthroughBody};
use gw_core::protocol::ProtocolKind;
use serde_json::{json, Value};

pub fn render_error_to_client(
    client: ProtocolKind,
    body: &PassthroughBody,
    fallback: ErrorClass,
    rules: &[PassthroughRule],
) -> Bytes {
    let v = json_for_protocol(client, body, fallback, rules);
    if body.is_sse {
        let s = format!(
            "event: error\ndata: {}\n\n",
            serde_json::to_string(&v).unwrap_or_default()
        );
        Bytes::from(s.into_bytes())
    } else {
        Bytes::from(serde_json::to_vec(&v).unwrap_or_default())
    }
}

fn json_for_protocol(
    p: ProtocolKind,
    b: &PassthroughBody,
    fb: ErrorClass,
    rules: &[PassthroughRule],
) -> Value {
    let typ = error_type_for(p, b.upstream_status);
    let gw_code = gateway_code(b, fb);
    // Audit 2026-05-30 §3.8: per-pattern allow/deny over the upstream message.
    // The envelope (type + status + stable gateway code) is always rendered; only
    // the MESSAGE is masked when a Deny rule matches, so a denied error still
    // surfaces as a clean, status-appropriate client error rather than leaking
    // internal detail.
    let raw_msg = extract_message(&b.body_json);
    let msg = if message_allowed(&raw_msg, rules) {
        raw_msg
    } else {
        metrics::counter!("gw_error_passthrough_masked_total").increment(1);
        "upstream error".to_string()
    };
    match p {
        ProtocolKind::OpenAi => json!({
            "error": { "type": typ, "message": msg, "code": gw_code }
        }),
        ProtocolKind::Anthropic => json!({
            "type": "error",
            "error": { "type": typ, "message": msg, "code": gw_code }
        }),
        ProtocolKind::Gemini => json!({
            "error": { "code": b.upstream_status, "status": typ, "message": msg, "gateway_code": gw_code }
        }),
        ProtocolKind::Kiro => json!({
            "type": "error",
            "code": typ,
            "message": msg,
            "gateway_code": gw_code
        }),
    }
}

/// Derive the stable `GW_...` gateway code for a passthrough envelope (P0 #16).
///
/// 1. Preserve an upstream-supplied gateway code if the body already carries a
///    `GW_...` string in `code` / `error.code` / `gateway_code` /
///    `error.gateway_code` (e.g. the upstream is itself a gateway).
/// 2. Otherwise map from the upstream HTTP status, falling back to the
///    [`ErrorClass`] when the status is unknown.
fn gateway_code(b: &PassthroughBody, fb: ErrorClass) -> String {
    if let Some(c) = extract_gateway_code(&b.body_json) {
        return c;
    }
    let mapped = match b.upstream_status {
        401 => codes::GW_AUTH_INVALID_APIKEY,
        429 => codes::GW_UPSTREAM_OVERLOADED,
        500..=599 => codes::GW_UPSTREAM_5XX,
        400..=499 => codes::GW_UPSTREAM_4XX,
        _ => match fb {
            ErrorClass::AuthDenied => codes::GW_AUTH_INVALID_APIKEY,
            ErrorClass::RateLimitedUpstream(_) => codes::GW_UPSTREAM_OVERLOADED,
            ErrorClass::UpstreamTransient => codes::GW_UPSTREAM_5XX,
            _ => codes::GW_GATEWAY_INTERNAL,
        },
    };
    mapped.to_string()
}

/// Scan common fields for an already-present `GW_...` gateway code.
fn extract_gateway_code(body: &Value) -> Option<String> {
    let is_gw = |v: &Value| {
        v.as_str()
            .filter(|s| s.starts_with("GW_"))
            .map(String::from)
    };
    body.get("gateway_code")
        .and_then(is_gw)
        .or_else(|| body.get("code").and_then(is_gw))
        .or_else(|| {
            body.get("error")
                .and_then(|e| e.get("gateway_code"))
                .and_then(is_gw)
        })
        .or_else(|| {
            body.get("error")
                .and_then(|e| e.get("code"))
                .and_then(is_gw)
        })
}

fn error_type_for(p: ProtocolKind, status: u16) -> &'static str {
    match (p, status) {
        (ProtocolKind::OpenAi, 401) => "invalid_request_error",
        (ProtocolKind::OpenAi, 429) => "rate_limit_exceeded",
        (ProtocolKind::OpenAi, 500..=599) => "service_unavailable",
        (ProtocolKind::OpenAi, _) => "api_error",
        (ProtocolKind::Anthropic, 401) => "authentication_error",
        (ProtocolKind::Anthropic, 429) => "rate_limit_error",
        (ProtocolKind::Anthropic, _) => "api_error",
        (ProtocolKind::Gemini, 401) => "UNAUTHENTICATED",
        (ProtocolKind::Gemini, 429) => "RESOURCE_EXHAUSTED",
        (ProtocolKind::Gemini, 500..=599) => "INTERNAL",
        (ProtocolKind::Gemini, _) => "INTERNAL",
        (ProtocolKind::Kiro, 401) => "authentication_error",
        (ProtocolKind::Kiro, 429) => "rate_limit_error",
        (ProtocolKind::Kiro, 500..=599) => "service_unavailable",
        (ProtocolKind::Kiro, _) => "api_error",
    }
}

fn extract_message(body: &Value) -> String {
    body.get("message")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .map(String::from)
        .unwrap_or_else(|| "upstream error".to_string())
}
