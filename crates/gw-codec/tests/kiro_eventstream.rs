use gw_codec::kiro::{
    decode_frame, encode_frame, render_thinking_tags, split_thinking_tags, EventStreamFrame,
    KiroPayloadPart,
};

#[test]
fn kiro_eventstream_roundtrips_payload_with_crc() {
    let frame = EventStreamFrame {
        headers: vec![0x01, 0x02, 0x03],
        payload: br#"{"completion":"hello"}"#.to_vec(),
    };

    let encoded = encode_frame(&frame);
    let decoded = decode_frame(&encoded).expect("valid eventstream frame");

    assert_eq!(decoded.headers, frame.headers);
    assert_eq!(decoded.payload, frame.payload);
}

#[test]
fn kiro_eventstream_rejects_length_and_crc_mismatch() {
    let frame = EventStreamFrame {
        headers: Vec::new(),
        payload: b"payload".to_vec(),
    };
    let mut encoded = encode_frame(&frame);

    encoded[3] ^= 0x01;
    let err = decode_frame(&encoded).expect_err("corrupt total length should fail");
    assert!(err.to_string().contains("length"));

    let mut encoded = encode_frame(&frame);
    let last = encoded.len() - 1;
    encoded[last] ^= 0x01;
    let err = decode_frame(&encoded).expect_err("corrupt message crc should fail");
    assert!(err.to_string().contains("message crc"));
}

#[test]
fn kiro_thinking_tags_parse_and_render_roundtrip() {
    let payload = "visible <thinking>chain of thought</thinking> done";

    let parts = split_thinking_tags(payload).expect("thinking tags parse");
    assert_eq!(
        parts,
        vec![
            KiroPayloadPart::Text("visible "),
            KiroPayloadPart::Thinking("chain of thought"),
            KiroPayloadPart::Text(" done"),
        ]
    );
    assert_eq!(render_thinking_tags(&parts), payload);
}

#[test]
fn kiro_thinking_tags_reject_unclosed_block() {
    let err = split_thinking_tags("visible <thinking>unterminated")
        .expect_err("unclosed thinking block should fail");
    assert!(err.to_string().contains("unclosed"));
}
