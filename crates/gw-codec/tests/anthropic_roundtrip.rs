use gw_codec::anthropic::Anthropic;
use gw_codec::ProtocolCodec;
use gw_core::ir::{MessageIR, StreamEventIR};

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
    let chunk = b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";
    let evs = codec.parse_stream_chunk(chunk).unwrap();
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        StreamEventIR::Delta { text, .. } => assert_eq!(text.as_deref().unwrap(), "Hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_message_stop() {
    let codec = Anthropic;
    let chunk = b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let evs = codec.parse_stream_chunk(chunk).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::End { .. }));
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
