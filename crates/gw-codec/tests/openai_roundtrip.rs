use gw_codec::openai::OpenAi;
use gw_codec::ProtocolCodec;
use gw_core::ir::{MessageIR, StreamEventIR};

const BASIC: &str = include_str!("fixtures/openai_basic.json");

#[test]
fn parse_basic_request() {
    let codec = OpenAi;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    assert_eq!(ir.model.as_ref(), "gpt-4");
    assert!(!ir.stream);
    assert_eq!(ir.max_tokens, Some(100));
    assert_eq!(ir.temperature, Some(0.7));
    assert_eq!(ir.messages.len(), 2);
    match &ir.messages[0] {
        MessageIR::System { .. } => {}
        _ => panic!("expected first message to be System"),
    }
    match &ir.messages[1] {
        MessageIR::User { .. } => {}
        _ => panic!("expected second message to be User"),
    }
}

#[test]
fn render_roundtrip_preserves_fields() {
    let codec = OpenAi;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_request(&buf).unwrap();
    assert_eq!(ir.model, ir2.model);
    assert_eq!(ir.messages.len(), ir2.messages.len());
    assert_eq!(ir.max_tokens, ir2.max_tokens);
    assert_eq!(ir.temperature, ir2.temperature);
}

#[test]
fn parse_stream_chunk_delta() {
    let codec = OpenAi;
    let chunk = b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n";
    let e = codec.parse_stream_chunk(chunk).unwrap().unwrap();
    match e {
        StreamEventIR::Delta { text, .. } => assert_eq!(text.unwrap(), "hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_done() {
    let codec = OpenAi;
    let e = codec.parse_stream_chunk(b"data: [DONE]\n\n").unwrap().unwrap();
    assert!(matches!(e, StreamEventIR::End { .. }));
}

#[test]
fn render_stream_chunk_delta() {
    let codec = OpenAi;
    let ev = StreamEventIR::Delta {
        text: Some(std::borrow::Cow::Borrowed("hello")),
        tool_call: None,
        thinking: None,
    };
    let mut buf = bytes::BytesMut::new();
    codec.render_stream_chunk(&ev, &mut buf).unwrap();
    let s = std::str::from_utf8(&buf).unwrap();
    assert!(s.starts_with("data: "));
    assert!(s.contains("\"hello\""));
    assert!(s.ends_with("\n\n"));
}

#[test]
fn estimate_input_tokens_nonzero() {
    let codec = OpenAi;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    assert!(codec.estimate_input_tokens(&ir) >= 1);
}

#[test]
fn parse_error_400() {
    let codec = OpenAi;
    let body = br#"{"error":{"message":"bad model","type":"invalid_request_error"}}"#;
    let err = codec.parse_error(400, body);
    assert_eq!(err.status, 400);
    assert_eq!(err.upstream_status, Some(400));
    assert_eq!(err.message.as_ref(), "bad model");
    assert!(!err.retryable);
}
