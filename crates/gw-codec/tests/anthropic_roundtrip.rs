use gw_codec::anthropic::Anthropic;
use gw_codec::ProtocolCodec;
use gw_core::ir::{
    AnthropicThinking, ContentIR, ContentPart, MediaSource, MessageIR, ReasoningConfig, RequestIR,
    StreamEventIR,
};
use std::borrow::Cow;

const BASIC: &str = include_str!("fixtures/anthropic_basic.json");

#[test]
fn parse_basic_request() {
    let codec = Anthropic;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    assert_eq!(ir.model.as_ref(), "claude-3-5-sonnet-20241022");
    assert_eq!(ir.max_tokens, Some(1024));
    assert_eq!(ir.temperature, Some(0.5));
    // Anthropic's top-level `system` is folded in as the first message.
    assert_eq!(ir.messages.len(), 2);
    match &ir.messages[0] {
        MessageIR::System { .. } => {}
        _ => panic!("expected first message System (from `system` field)"),
    }
    match &ir.messages[1] {
        MessageIR::User { .. } => {}
        _ => panic!("expected second message User"),
    }
}

#[test]
fn render_roundtrip_preserves_fields() {
    let codec = Anthropic;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_request(&buf).unwrap();
    assert_eq!(ir.model, ir2.model);
    assert_eq!(ir.max_tokens, ir2.max_tokens);
    assert_eq!(ir.temperature, ir2.temperature);
    assert_eq!(ir.messages.len(), ir2.messages.len());
}

#[test]
fn parse_stream_chunk_delta() {
    let codec = Anthropic;
    let chunk = b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        StreamEventIR::Delta { text, .. } => assert_eq!(text.as_deref().unwrap(), "Hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_message_stop() {
    let codec = Anthropic;
    let chunk = b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert!(evs.iter().any(|e| matches!(e, StreamEventIR::End { .. })));
}

#[test]
fn render_stream_chunk_delta_has_event_prefix() {
    let codec = Anthropic;
    let ev = StreamEventIR::Delta {
        text: Some(std::borrow::Cow::Borrowed("hi")),
        tool_call: None,
        thinking: None,
    };
    let mut buf = bytes::BytesMut::new();
    codec.render_stream_chunk(&ev, &mut buf).unwrap();
    let s = std::str::from_utf8(&buf).unwrap();
    assert!(s.starts_with("event: content_block_delta\n"));
    assert!(s.contains("\"hi\""));
}

#[test]
fn estimate_input_tokens_nonzero() {
    let codec = Anthropic;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    assert!(codec.estimate_input_tokens(&ir) >= 1);
}

#[test]
fn parse_error_400() {
    let codec = Anthropic;
    let body =
        br#"{"type":"error","error":{"type":"invalid_request_error","message":"missing model"}}"#;
    let err = codec.parse_error(400, body);
    assert_eq!(err.status, 400);
    assert_eq!(err.message.as_ref(), "missing model");
}

#[test]
fn parse_request_adaptive_thinking_display_and_effort() {
    let codec = Anthropic;
    let body = br#"{
        "model":"claude-3-7-sonnet",
        "messages":[{"role":"user","content":"hi"}],
        "max_tokens":2048,
        "thinking":{"type":"adaptive","display":"summarized"},
        "output_config":{"effort":"medium"}
    }"#;

    let ir = codec.parse_request(body).unwrap();
    let thinking = ir
        .metadata
        .anthropic_thinking
        .as_ref()
        .expect("anthropic thinking metadata");
    assert_eq!(thinking.kind.as_ref(), "adaptive");
    assert_eq!(thinking.display.as_deref(), Some("summarized"));
    assert_eq!(thinking.adaptive, Some(true));
    assert_eq!(thinking.budget_tokens, None);

    let reasoning = ir.reasoning_config.as_ref().expect("reasoning config");
    assert_eq!(reasoning.effort.as_deref(), Some("medium"));
    assert_eq!(reasoning.budget_tokens, None);
}

#[test]
fn render_request_preserves_enabled_thinking_display_and_effort() {
    let codec = Anthropic;
    let ir = RequestIR {
        model: Cow::Borrowed("claude-3-7-sonnet"),
        messages: vec![MessageIR::User {
            content: gw_core::ir::ContentIR::Text(Cow::Borrowed("hi")),
        }],
        max_tokens: Some(1024),
        metadata: gw_core::ir::MetadataIR {
            anthropic_thinking: Some(AnthropicThinking {
                kind: Cow::Borrowed("enabled"),
                budget_tokens: Some(4096),
                display: Some(Cow::Borrowed("omitted")),
                adaptive: Some(false),
            }),
            ..Default::default()
        },
        reasoning_config: Some(ReasoningConfig {
            effort: Some(Cow::Borrowed("low")),
            budget_tokens: Some(4096),
        }),
        ..RequestIR::default()
    };

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let reparsed = codec.parse_request(&buf).unwrap();

    let thinking = reparsed
        .metadata
        .anthropic_thinking
        .as_ref()
        .expect("thinking after roundtrip");
    assert_eq!(thinking.kind.as_ref(), "enabled");
    assert_eq!(thinking.display.as_deref(), Some("omitted"));
    assert_eq!(thinking.budget_tokens, Some(4096));
    assert_eq!(thinking.adaptive, Some(false));

    let reasoning = reparsed
        .reasoning_config
        .as_ref()
        .expect("reasoning after roundtrip");
    assert_eq!(reasoning.effort.as_deref(), Some("low"));
    assert_eq!(reasoning.budget_tokens, Some(4096));
}

#[test]
fn cache_ttl_usage_round_trips_through_ir() {
    let codec = Anthropic;
    let body = br#"{
        "id":"msg_cache",
        "type":"message",
        "role":"assistant",
        "model":"claude-3-5-sonnet",
        "content":[{"type":"text","text":"cached response"}],
        "stop_reason":"end_turn",
        "usage":{
            "input_tokens":100,
            "output_tokens":50,
            "cache_read_input_tokens":30,
            "cache_creation_input_tokens":70,
            "cache_creation":{
                "ephemeral_1h_input_tokens":40,
                "ephemeral_5m_input_tokens":30
            },
            "server_tool_use":{
                "web_search_requests":3,
                "web_fetch_requests":1
            }
        }
    }"#;

    let ir = codec.parse_response(body).unwrap();
    let usage = ir.usage.as_ref().expect("usage present");
    assert_eq!(usage.input_tokens, 100);
    assert_eq!(usage.output_tokens, 50);
    assert_eq!(usage.cached_tokens, Some(30));
    assert_eq!(usage.cache_creation_tokens, Some(70));
    assert_eq!(usage.cache_creation_1h_tokens, Some(40));
    assert_eq!(usage.cache_creation_5m_tokens, Some(30));
    assert_eq!(usage.server_tool_web_search_requests, Some(3));
    assert_eq!(usage.server_tool_web_fetch_requests, Some(1));

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&buf).expect("valid JSON");

    let u = &json["usage"];
    assert_eq!(u["cache_creation_input_tokens"], 70);
    assert_eq!(u["cache_creation"]["ephemeral_1h_input_tokens"], 40);
    assert_eq!(u["cache_creation"]["ephemeral_5m_input_tokens"], 30);
    assert_eq!(u["server_tool_use"]["web_search_requests"], 3);
    assert_eq!(u["server_tool_use"]["web_fetch_requests"], 1);
}

#[test]
fn inference_geo_service_tier_round_trips_through_ir() {
    let codec = Anthropic;
    let body = br#"{
        "id":"msg_geo",
        "type":"message",
        "role":"assistant",
        "model":"claude-3-5-sonnet",
        "content":[{"type":"text","text":"hello"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":10,"output_tokens":5},
        "service_tier":"standard",
        "inference_geo":"us-east-1"
    }"#;

    let ir = codec.parse_response(body).unwrap();
    assert_eq!(ir.service_tier.as_deref(), Some("standard"));
    let usage = ir.usage.as_ref().expect("usage present");
    assert_eq!(usage.inference_geo.as_deref(), Some("us-east-1"));
    assert_eq!(usage.service_tier.as_deref(), Some("standard"));

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&buf).expect("valid JSON");
    assert_eq!(json["service_tier"].as_str(), Some("standard"));
    assert_eq!(json["inference_geo"].as_str(), Some("us-east-1"));
}

#[test]
fn request_service_tier_round_trips() {
    let codec = Anthropic;
    let body = br#"{
        "model":"claude-3-5-sonnet",
        "messages":[{"role":"user","content":"hi"}],
        "max_tokens":1024,
        "service_tier":"priority"
    }"#;

    let ir = codec.parse_request(body).unwrap();
    assert_eq!(ir.service_tier.as_deref(), Some("priority"));

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&buf).expect("valid JSON");
    assert_eq!(json["service_tier"].as_str(), Some("priority"));
}

#[test]
fn absent_usage_details_omitted_from_wire() {
    let codec = Anthropic;
    let body = br#"{
        "id":"msg_basic",
        "type":"message",
        "role":"assistant",
        "model":"claude-3-5-sonnet",
        "content":[{"type":"text","text":"hi"}],
        "stop_reason":"end_turn",
        "usage":{"input_tokens":5,"output_tokens":3}
    }"#;

    let ir = codec.parse_response(body).unwrap();
    let usage = ir.usage.as_ref().expect("usage present");
    assert!(usage.cache_creation_1h_tokens.is_none());
    assert!(usage.cache_creation_5m_tokens.is_none());
    assert!(usage.server_tool_web_search_requests.is_none());
    assert!(usage.inference_geo.is_none());
    assert!(usage.service_tier.is_none());
    assert!(ir.service_tier.is_none());

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&buf).expect("valid JSON");
    assert!(json.get("service_tier").is_none());
    assert!(json.get("inference_geo").is_none());
    assert!(json["usage"].get("cache_creation").is_none());
    assert!(json["usage"].get("server_tool_use").is_none());
}

#[test]
fn document_and_search_result_request_roundtrip() {
    let codec = Anthropic;
    let body = br#"{
        "model":"claude-3-5-sonnet",
        "max_tokens":1024,
        "messages":[{"role":"user","content":[
            {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"AQID"},"title":"Spec","context":"Read carefully","cache_control":{"type":"ephemeral"}},
            {"type":"search_result","source":"https://example.test/search","title":"Search hit","content":[{"type":"text","text":"snippet"}],"cache_control":{"type":"ephemeral"}},
            {"type":"text","text":"summarize"}
        ]}]
    }"#;

    let ir = codec.parse_request(body).unwrap();
    let MessageIR::User {
        content: ContentIR::Parts(parts),
    } = &ir.messages[0]
    else {
        panic!("expected user parts");
    };
    assert!(
        matches!(&parts[0], ContentPart::File { source: MediaSource::B64 { media_type, data } } if media_type.as_ref() == "application/pdf" && data.as_ref() == "AQID")
    );
    assert!(
        matches!(&parts[1], ContentPart::Text(t) if t.contains("Search hit") && t.contains("snippet") && t.contains("https://example.test/search"))
    );

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let reparsed = codec.parse_request(&buf).unwrap();
    let MessageIR::User {
        content: ContentIR::Parts(parts2),
    } = &reparsed.messages[0]
    else {
        panic!("expected reparsed user parts");
    };
    assert!(
        matches!(&parts2[0], ContentPart::File { source: MediaSource::B64 { media_type, data } } if media_type.as_ref() == "application/pdf" && data.as_ref() == "AQID")
    );
    assert!(parts2
        .iter()
        .any(|p| matches!(p, ContentPart::Text(t) if t.contains("Search hit"))));
}

#[test]
fn redacted_thinking_stop_reason_server_tool_and_mcp_roundtrip() {
    let codec = Anthropic;
    let body = br#"{
        "id":"msg_gap","type":"message","role":"assistant","model":"claude-3-7-sonnet",
        "content":[
            {"type":"redacted_thinking","data":"opaque-redacted"},
            {"type":"server_tool_use","id":"srv_1","name":"web_search","input":{"query":"rust"},"status":"started"},
            {"type":"mcp_tool_use","id":"mcp_1","name":"read_file","input":{"path":"README.md"},"server_name":"fs"},
            {"type":"text","text":"answer"}
        ],
        "stop_reason":"pause_turn",
        "usage":{"input_tokens":9,"output_tokens":4}
    }"#;

    let ir = codec.parse_response(body).unwrap();
    assert_eq!(
        ir.redacted_thinking.as_ref().unwrap()[0].data.as_ref(),
        "opaque-redacted"
    );
    assert!(
        matches!(ir.finish_reason.as_ref(), Some(gw_core::ir::FinishReasonIR::Other(s)) if s.as_ref() == "pause_turn")
    );
    let blocks = ir.server_tool_blocks.as_ref().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].kind.as_ref(), "server_tool_use");
    assert_eq!(blocks[1].kind.as_ref(), "mcp_tool_use");
    assert_eq!(
        blocks[1].extra.get("server_name").and_then(|v| v.as_str()),
        Some("fs")
    );

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let reparsed = codec.parse_response(&buf).unwrap();
    assert_eq!(
        reparsed.redacted_thinking.as_ref().unwrap()[0]
            .data
            .as_ref(),
        "opaque-redacted"
    );
    assert!(
        matches!(reparsed.finish_reason.as_ref(), Some(gw_core::ir::FinishReasonIR::Other(s)) if s.as_ref() == "pause_turn")
    );
    assert_eq!(
        reparsed.server_tool_blocks.as_ref().unwrap()[1]
            .kind
            .as_ref(),
        "mcp_tool_use"
    );
}
