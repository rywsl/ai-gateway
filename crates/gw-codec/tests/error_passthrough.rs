use gw_codec::error_passthrough::render_error_to_client;
use gw_core::error::{ErrorClass, PassthroughBody};
use gw_core::protocol::ProtocolKind;
use serde_json::json;

#[test]
fn anthropic_error_renders_to_openai_with_type() {
    let body = PassthroughBody {
        upstream_status: 503,
        body_json: json!({"type":"overloaded_error","message":"too busy"}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::OpenAi, &body, ErrorClass::UpstreamTransient);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["type"], "service_unavailable");
    assert_eq!(v["error"]["message"], "too busy");
}

#[test]
fn openai_401_renders_to_anthropic_authentication_error() {
    let body = PassthroughBody {
        upstream_status: 401,
        body_json: json!({"error":{"message":"invalid api key"}}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::Anthropic, &body, ErrorClass::AuthDenied);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "authentication_error");
}

#[test]
fn gemini_429_renders_with_status_field() {
    let body = PassthroughBody {
        upstream_status: 429,
        body_json: json!({"message":"too many requests"}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::Gemini,
        &body,
        ErrorClass::RateLimitedUpstream(gw_core::ratelimit::RateLimitKind::Unknown),
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["code"], 429);
    // Gemini uses uppercase status string
    assert!(v["error"]["status"].is_string());
}

#[test]
fn sse_error_emits_event_error_frame() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"error":"oops"}),
        is_sse: true,
    };
    let out = render_error_to_client(
        ProtocolKind::Anthropic,
        &body,
        ErrorClass::UpstreamTransient,
    );
    let s = std::str::from_utf8(&out).unwrap();
    assert!(s.starts_with("event: error\n"));
    assert!(s.contains("data: "));
    assert!(s.ends_with("\n\n"));
}

#[test]
fn missing_message_falls_back_to_default() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::OpenAi, &body, ErrorClass::UpstreamTransient);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(!msg.is_empty(), "fallback message should be non-empty");
}
