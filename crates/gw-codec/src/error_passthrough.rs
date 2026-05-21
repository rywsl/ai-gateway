//! Render upstream-vendor error bodies into client-protocol error envelopes.
//!
//! Each protocol expects a different error shape:
//! - OpenAI: `{"error": {"type": ..., "message": ..., "code": null}}`
//! - Anthropic: `{"type": "error", "error": {"type": ..., "message": ...}}`
//! - Gemini: `{"error": {"code": <int>, "status": ..., "message": ...}}`
//!
//! For SSE responses, wrap as `event: error\ndata: <json>\n\n`.

use bytes::Bytes;
use gw_core::error::{ErrorClass, PassthroughBody};
use gw_core::protocol::ProtocolKind;
use serde_json::{json, Value};

pub fn render_error_to_client(
    client: ProtocolKind,
    body: &PassthroughBody,
    fallback: ErrorClass,
) -> Bytes {
    let v = json_for_protocol(client, body, fallback);
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

fn json_for_protocol(p: ProtocolKind, b: &PassthroughBody, _fb: ErrorClass) -> Value {
    let typ = error_type_for(p, b.upstream_status);
    let msg = extract_message(&b.body_json);
    match p {
        ProtocolKind::OpenAi => json!({
            "error": { "type": typ, "message": msg, "code": null }
        }),
        ProtocolKind::Anthropic => json!({
            "type": "error",
            "error": { "type": typ, "message": msg }
        }),
        ProtocolKind::Gemini => json!({
            "error": { "code": b.upstream_status, "status": typ, "message": msg }
        }),
    }
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
