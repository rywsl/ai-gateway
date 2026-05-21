use gw_ledger::detailed_stream::{redact_body, redact_headers, REDACT_HEADER_DENYLIST};
use serde_json::json;

#[test]
fn redact_strips_authorization_and_apikey_and_cookie() {
    let h = vec![
        ("authorization".into(), "Bearer secret".into()),
        ("x-api-key".into(), "sk-abc".into()),
        ("cookie".into(), "sid=xx".into()),
        ("user-agent".into(), "claude-cli/1.0".into()),
        ("accept-encoding".into(), "gzip, br".into()),
    ];
    let red = redact_headers(&h);
    let names: Vec<&str> = red.iter().map(|(k, _)| k.as_str()).collect();
    for d in REDACT_HEADER_DENYLIST {
        assert!(!names.contains(d), "denied header still present: {d}");
    }
    assert!(names.contains(&"user-agent"));
    assert!(names.contains(&"accept-encoding"));
}

#[test]
fn redact_passes_stainless_headers() {
    let h = vec![
        ("x-stainless-os".into(), "macos".into()),
        ("x-stainless-package-version".into(), "1.2.3".into()),
    ];
    let red = redact_headers(&h);
    assert_eq!(red.len(), 2);
}

#[test]
fn redact_body_replaces_message_content() {
    let body = json!({
        "model": "claude-3",
        "messages": [
            { "role":"user", "content": "private content very long very long very long" },
            { "role":"assistant", "tool_calls": [ { "name":"foo", "args": "x" } ] }
        ]
    });
    let red = redact_body(body.clone());
    let s = serde_json::to_string(&red).unwrap();
    assert!(!s.contains("private content"));
    assert!(s.contains("foo"), "tool_calls.name should be preserved");
    assert!(s.contains("redacted"));
    assert!(s.contains("model"));
}

#[test]
fn redact_body_replaces_inline_media() {
    let body = json!({
        "messages": [{
            "role": "user",
            "content": [
                { "type":"image", "source": { "type":"base64", "data":"AAAA", "media_type":"image/png" } }
            ]
        }]
    });
    let red = redact_body(body);
    let s = serde_json::to_string(&red).unwrap();
    assert!(!s.contains("AAAA"));
    assert!(s.contains("media:type=image/png"));
}

#[test]
fn redact_body_handles_openai_responses_input() {
    let body = json!({
        "model": "gpt-5",
        "input": [
            { "type":"input_text", "text":"secret prompt" }
        ]
    });
    let red = redact_body(body);
    let s = serde_json::to_string(&red).unwrap();
    assert!(!s.contains("secret prompt"));
    assert!(s.contains("redacted"));
}

#[test]
fn redact_body_handles_text_part_in_messages() {
    let body = json!({
        "messages": [{
            "role": "user",
            "content": [
                { "type":"text", "text":"sensitive" }
            ]
        }]
    });
    let red = redact_body(body);
    let s = serde_json::to_string(&red).unwrap();
    assert!(!s.contains("sensitive"));
    assert!(s.contains("redacted"));
}

#[test]
fn redact_body_preserves_role_field() {
    let body = json!({
        "messages": [{
            "role": "system",
            "content": "you are helpful"
        }]
    });
    let red = redact_body(body);
    let s = serde_json::to_string(&red).unwrap();
    assert!(s.contains("system"));
    assert!(!s.contains("you are helpful"));
}
