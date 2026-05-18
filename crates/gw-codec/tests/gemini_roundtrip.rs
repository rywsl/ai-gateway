use gw_codec::gemini::Gemini;
use gw_codec::ProtocolCodec;
use gw_core::ir::{MessageIR, StreamEventIR};

const BASIC: &str = include_str!("fixtures/gemini_basic.json");

#[test]
fn parse_basic_request() {
    let codec = Gemini;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    // Gemini doesn't carry `model` in the body — it's in the URL — so IR
    // model is empty from a wire-only parse.
    assert!(ir.model.is_empty());
    assert_eq!(ir.max_tokens, Some(256));
    assert_eq!(ir.temperature, Some(0.6));
    // System instruction + 1 user content.
    assert_eq!(ir.messages.len(), 2);
    match &ir.messages[0] {
        MessageIR::System { .. } => {}
        _ => panic!("expected first message System (from systemInstruction)"),
    }
    match &ir.messages[1] {
        MessageIR::User { .. } => {}
        _ => panic!("expected second message User"),
    }
}

#[test]
fn render_roundtrip_preserves_fields() {
    let codec = Gemini;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_request(&buf).unwrap();
    assert_eq!(ir.max_tokens, ir2.max_tokens);
    assert_eq!(ir.temperature, ir2.temperature);
    assert_eq!(ir.messages.len(), ir2.messages.len());
}

#[test]
fn parse_stream_chunk_delta() {
    let codec = Gemini;
    let chunk = b"data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hi\"}]}}]}\n\n";
    let e = codec.parse_stream_chunk(chunk).unwrap().unwrap();
    match e {
        StreamEventIR::Delta { text, .. } => assert_eq!(text.unwrap(), "hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_finish_reason() {
    let codec = Gemini;
    let chunk = b"data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";
    let e = codec.parse_stream_chunk(chunk).unwrap().unwrap();
    assert!(matches!(e, StreamEventIR::End { .. }));
}

#[test]
fn render_stream_chunk_delta() {
    let codec = Gemini;
    let ev = StreamEventIR::Delta {
        text: Some(std::borrow::Cow::Borrowed("hi")),
        tool_call: None,
        thinking: None,
    };
    let mut buf = bytes::BytesMut::new();
    codec.render_stream_chunk(&ev, &mut buf).unwrap();
    let s = std::str::from_utf8(&buf).unwrap();
    assert!(s.starts_with("data: "));
    assert!(s.contains("\"hi\""));
}

#[test]
fn estimate_input_tokens_nonzero() {
    let codec = Gemini;
    let ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    assert!(codec.estimate_input_tokens(&ir) >= 1);
}

#[test]
fn parse_error_400() {
    let codec = Gemini;
    let body = br#"{"error":{"code":400,"message":"bad request","status":"INVALID_ARGUMENT"}}"#;
    let err = codec.parse_error(400, body);
    assert_eq!(err.status, 400);
    assert_eq!(err.message.as_ref(), "bad request");
}
