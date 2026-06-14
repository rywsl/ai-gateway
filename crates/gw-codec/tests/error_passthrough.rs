use gw_codec::error_passthrough::render_error_to_client;
use gw_core::error::rules::{PassthroughRule, RuleEffect};
use gw_core::error::{ErrorClass, PassthroughBody};
use gw_core::protocol::ProtocolKind;
use serde_json::json;

// No rules = prior passthrough-when-enabled behavior.
const NO_RULES: &[PassthroughRule] = &[];

#[test]
fn anthropic_error_renders_to_openai_with_type() {
    let body = PassthroughBody {
        upstream_status: 503,
        body_json: json!({"type":"overloaded_error","message":"too busy"}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::OpenAi,
        &body,
        ErrorClass::UpstreamTransient,
        NO_RULES,
    );
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
    let out = render_error_to_client(
        ProtocolKind::Anthropic,
        &body,
        ErrorClass::AuthDenied,
        NO_RULES,
    );
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
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["code"], 429);
    // Gemini uses uppercase status string
    assert!(v["error"]["status"].is_string());
}

#[test]
fn kiro_error_renders_minimal_eventstream_error_payload_shape() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"message":"upstream failed"}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::Kiro,
        &body,
        ErrorClass::UpstreamTransient,
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["type"], "error");
    assert_eq!(v["code"], "service_unavailable");
    assert_eq!(v["message"], "upstream failed");
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
        NO_RULES,
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
    let out = render_error_to_client(
        ProtocolKind::OpenAi,
        &body,
        ErrorClass::UpstreamTransient,
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(!msg.is_empty(), "fallback message should be non-empty");
}

// Audit 2026-05-30 §3.8: per-pattern allow/deny over the upstream message. The
// envelope (type + status) is always rendered; a Deny rule masks only the message.
#[test]
fn deny_rule_masks_message_but_keeps_envelope() {
    let rules = [PassthroughRule {
        needle: "internal".to_string(),
        effect: RuleEffect::Deny,
    }];
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"error":{"message":"internal: secret-host db timeout"}}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::OpenAi, &body, ErrorClass::Internal, &rules);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    // Envelope preserved (status-appropriate type), message masked.
    assert_eq!(v["error"]["type"], "service_unavailable");
    assert_eq!(v["error"]["message"], "upstream error");
}

#[test]
fn allow_rule_passes_message_through() {
    let rules = [
        PassthroughRule {
            needle: "quota".to_string(),
            effect: RuleEffect::Allow,
        },
        PassthroughRule {
            needle: "".to_string(), // ignored (empty needle)
            effect: RuleEffect::Deny,
        },
    ];
    let body = PassthroughBody {
        upstream_status: 429,
        body_json: json!({"error":{"message":"quota exceeded for project"}}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::OpenAi, &body, ErrorClass::Internal, &rules);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["message"], "quota exceeded for project");
}

// --- P0 #16: stable GW_ gateway code in every error envelope ---

fn parse_sse_data(out: &[u8]) -> serde_json::Value {
    let s = std::str::from_utf8(out).unwrap();
    let data = s
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("sse frame must contain a data: line");
    serde_json::from_str(data).unwrap()
}

#[test]
fn anthropic_sse_error_includes_gateway_code() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"type":"error","error":{"message":"boom"}}),
        is_sse: true,
    };
    let out = render_error_to_client(
        ProtocolKind::Anthropic,
        &body,
        ErrorClass::UpstreamTransient,
        NO_RULES,
    );
    let v = parse_sse_data(&out);
    // Vendor shape preserved...
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "api_error");
    // ...plus a stable gateway code clients/ops can branch on.
    let code = v["error"]["code"]
        .as_str()
        .expect("error.code must be a string");
    assert!(code.starts_with("GW_"), "got {code}");
}

#[test]
fn openai_401_includes_stable_gateway_code_not_null() {
    let body = PassthroughBody {
        upstream_status: 401,
        body_json: json!({"error":{"message":"invalid api key"}}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::OpenAi,
        &body,
        ErrorClass::AuthDenied,
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(!v["error"]["code"].is_null());
    assert_eq!(v["error"]["code"], "GW_AUTH_INVALID_APIKEY");
}

#[test]
fn gemini_error_keeps_numeric_code_and_adds_gateway_code() {
    let body = PassthroughBody {
        upstream_status: 429,
        body_json: json!({"message":"too many requests"}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::Gemini,
        &body,
        ErrorClass::RateLimitedUpstream(gw_core::ratelimit::RateLimitKind::Unknown),
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    // Gemini compatibility: numeric code + uppercase status preserved.
    assert_eq!(v["error"]["code"], 429);
    assert!(v["error"]["status"].is_string());
    let gw = v["error"]["gateway_code"]
        .as_str()
        .expect("gateway_code must be a string");
    assert!(gw.starts_with("GW_"), "got {gw}");
}

#[test]
fn kiro_error_keeps_vendor_code_and_adds_gateway_code() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"message":"upstream failed"}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::Kiro,
        &body,
        ErrorClass::UpstreamTransient,
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    // Existing vendor shape preserved.
    assert_eq!(v["code"], "service_unavailable");
    let gw = v["gateway_code"]
        .as_str()
        .expect("gateway_code must be a string");
    assert!(gw.starts_with("GW_"), "got {gw}");
}

#[test]
fn denied_message_masked_but_gateway_code_present() {
    let rules = [PassthroughRule {
        needle: "internal".to_string(),
        effect: RuleEffect::Deny,
    }];
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"error":{"message":"internal: secret-host db timeout"}}),
        is_sse: false,
    };
    let out = render_error_to_client(ProtocolKind::OpenAi, &body, ErrorClass::Internal, &rules);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["message"], "upstream error");
    // Masking touches the message only — the stable code survives.
    let code = v["error"]["code"]
        .as_str()
        .expect("error.code must be a string");
    assert!(code.starts_with("GW_"), "got {code}");
}

#[test]
fn upstream_supplied_gateway_code_is_preserved() {
    let body = PassthroughBody {
        upstream_status: 500,
        body_json: json!({"error":{"message":"boom","code":"GW_UPSTREAM_OVERLOADED"}}),
        is_sse: false,
    };
    let out = render_error_to_client(
        ProtocolKind::OpenAi,
        &body,
        ErrorClass::UpstreamTransient,
        NO_RULES,
    );
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["error"]["code"], "GW_UPSTREAM_OVERLOADED");
}
