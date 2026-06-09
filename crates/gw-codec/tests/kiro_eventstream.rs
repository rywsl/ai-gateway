use gw_codec::kiro::{
    decode_frame, encode_frame, render_thinking_tags, split_thinking_tags, EventStreamFrame, Kiro,
    KiroPayloadPart,
};
use gw_codec::{AnyCodec, ProtocolCodec};
use gw_core::ir::{
    ContentIR, FinishReasonIR, MessageIR, RequestIR, ResponseIR, ResponseThinkingIR, StreamEventIR,
    ThinkingBlockIR,
};
use gw_core::protocol::{ApiShape, ProtocolKind};
use std::borrow::Cow;

fn eventstream_headers() -> Vec<(String, String)> {
    vec![(
        "content-type".to_owned(),
        "application/vnd.amazon.eventstream".to_owned(),
    )]
}

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

#[test]
fn any_codec_detects_kiro_eventstream_from_header_and_path_only() {
    let headers = eventstream_headers();

    let codec = AnyCodec::detect("/kiro/invoke", &headers, &[])
        .expect("Kiro EventStream should be detected from feasible early inputs");

    assert!(matches!(codec, AnyCodec::Kiro(_)));
    assert_eq!(codec.kind(), ProtocolKind::Kiro);
    assert_eq!(codec.api_shape(), ApiShape::KiroEventStream);
    assert!(!codec.api_shape().is_cli());
}

#[test]
fn any_codec_does_not_detect_kiro_from_body_prefix_without_kiro_path() {
    let headers = eventstream_headers();
    let frame = encode_frame(&EventStreamFrame {
        headers: Vec::new(),
        payload: b"hello".to_vec(),
    });

    let detected = AnyCodec::detect("/v1/messages", &headers, &frame);
    assert!(
        !matches!(detected, Some(AnyCodec::Kiro(_))),
        "early Kiro detection must not rely on EventStream body prefix alone"
    );
}

#[test]
fn api_shape_marks_kiro_cli_separately_from_eventstream() {
    assert_eq!(ApiShape::KiroCli.protocol_kind(), ProtocolKind::Kiro);
    assert_eq!(ApiShape::KiroCli.format_id(), "kiro:cli");
    assert!(ApiShape::KiroCli.is_cli());
    assert_eq!(
        ApiShape::KiroEventStream.protocol_kind(),
        ProtocolKind::Kiro
    );
    assert_eq!(ApiShape::KiroEventStream.format_id(), "kiro:eventstream");
    assert!(!ApiShape::KiroEventStream.is_cli());
}

#[test]
fn kiro_protocol_codec_request_response_and_stream_contract() {
    let codec = Kiro;
    let request_frame = encode_frame(&EventStreamFrame {
        headers: Vec::new(),
        payload: b"hello request".to_vec(),
    });

    let parsed_request = codec.parse_request(&request_frame).expect("request parses");
    assert!(parsed_request.stream);
    assert_eq!(parsed_request.messages.len(), 1);
    assert!(matches!(
        &parsed_request.messages[0],
        MessageIR::User {
            content: ContentIR::Text(text),
        } if text.as_ref() == "hello request"
    ));

    let request_ir = RequestIR {
        messages: vec![MessageIR::User {
            content: ContentIR::Text(Cow::Borrowed("rendered request")),
        }],
        stream: true,
        ..RequestIR::default()
    };
    let mut rendered_request = bytes::BytesMut::new();
    codec
        .render_request(&request_ir, &mut rendered_request)
        .expect("request renders");
    let decoded_request = decode_frame(&rendered_request).expect("rendered request frame decodes");
    assert_eq!(decoded_request.payload, b"rendered request");

    let response_frame = encode_frame(&EventStreamFrame {
        headers: Vec::new(),
        payload: b"answer <thinking>reasoning</thinking>".to_vec(),
    });
    let parsed_response = codec
        .parse_response(&response_frame)
        .expect("response parses");
    assert!(matches!(
        &parsed_response.messages[0],
        MessageIR::Assistant {
            content: ContentIR::Text(text),
            ..
        } if text.as_ref() == "answer "
    ));
    assert_eq!(
        parsed_response.thinking.as_ref().unwrap().blocks[0].text,
        Cow::Borrowed("reasoning")
    );

    let response_ir = ResponseIR {
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("final answer")),
            tool_calls: None,
        }],
        thinking: Some(ResponseThinkingIR {
            blocks: vec![ThinkingBlockIR {
                text: Cow::Borrowed("hidden thought"),
                signature: None,
            }],
        }),
        finish_reason: Some(FinishReasonIR::Stop),
        ..ResponseIR::default()
    };
    let mut rendered_response = bytes::BytesMut::new();
    codec
        .render_response(&response_ir, &mut rendered_response)
        .expect("response renders");
    let decoded_response =
        decode_frame(&rendered_response).expect("rendered response frame decodes");
    assert_eq!(
        String::from_utf8(decoded_response.payload).unwrap(),
        "<thinking>hidden thought</thinking>final answer"
    );

    let stream_frame = encode_frame(&EventStreamFrame {
        headers: Vec::new(),
        payload: b"delta <thinking>stream thought</thinking>".to_vec(),
    });
    let mut state = codec.new_stream_state();
    let events = codec
        .parse_stream_chunk(state.as_mut(), &stream_frame)
        .expect("stream chunk parses");
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        StreamEventIR::Delta {
            text: Some(text),
            ..
        } if text.as_ref() == "delta "
    ));
    assert!(matches!(
        &events[1],
        StreamEventIR::ThinkingDelta { text, .. } if text.as_ref() == "stream thought"
    ));

    let mut rendered_stream = bytes::BytesMut::new();
    codec
        .render_stream_chunk(
            &StreamEventIR::ThinkingDelta {
                text: Cow::Borrowed("stream render thought"),
                signature_chunk: None,
            },
            &mut rendered_stream,
        )
        .expect("stream chunk renders");
    let decoded_stream = decode_frame(&rendered_stream).expect("rendered stream frame decodes");
    assert_eq!(
        String::from_utf8(decoded_stream.payload).unwrap(),
        "<thinking>stream render thought</thinking>"
    );
}
