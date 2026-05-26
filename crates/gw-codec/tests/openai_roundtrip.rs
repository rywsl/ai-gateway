use gw_codec::openai::OpenAi;
use gw_codec::ProtocolCodec;
use gw_core::ir::{
    ContentIR, ContentPart, ImageSource, MediaSource, MessageIR, ResponseFormatIR, StreamEventIR,
};

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
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        StreamEventIR::Delta { text, .. } => assert_eq!(text.as_deref().unwrap(), "hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_done() {
    let codec = OpenAi;
    let mut state = codec.new_stream_state();
    let evs = codec
        .parse_stream_chunk(state.as_mut(), b"data: [DONE]\n\n")
        .unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::End { .. }));
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

#[test]
fn parse_json_schema_response_format() {
    let codec = OpenAi;
    let body = br#"{
        "model":"gpt-4o-2024-08-06",
        "messages":[{"role":"user","content":"Extract event data"}],
        "response_format":{
            "type":"json_schema",
            "json_schema":{
                "name":"calendar_event",
                "description":"Calendar event payload",
                "schema":{
                    "type":"object",
                    "properties":{"name":{"type":"string"}},
                    "required":["name"],
                    "additionalProperties":false
                },
                "strict":true
            }
        }
    }"#;

    let ir = codec.parse_request(body).unwrap();
    match ir.response_format.as_ref() {
        Some(ResponseFormatIR::JsonSchema(raw)) => {
            let value: serde_json::Value = serde_json::from_str(raw.get()).unwrap();
            assert_eq!(value["name"], "calendar_event");
            assert_eq!(value["description"], "Calendar event payload");
            assert_eq!(value["strict"], true);
            assert_eq!(value["schema"]["type"], "object");
            assert_eq!(value["schema"]["properties"]["name"]["type"], "string");
        }
        other => panic!("expected JsonSchema response format, got {other:?}"),
    }

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let rendered: serde_json::Value = serde_json::from_slice(&buf).unwrap();
    assert_eq!(rendered["response_format"]["type"], "json_schema");
    assert_eq!(
        rendered["response_format"]["json_schema"]["name"],
        "calendar_event"
    );
    assert_eq!(
        rendered["response_format"]["json_schema"]["description"],
        "Calendar event payload"
    );
    assert_eq!(rendered["response_format"]["json_schema"]["strict"], true);
    assert_eq!(
        rendered["response_format"]["json_schema"]["schema"]["type"],
        "object"
    );
}

#[test]
fn comprehensive_request_gap_roundtrip() {
    let codec = OpenAi;
    let body = br#"{
        "model":"gpt-4o-audio-preview",
        "messages":[
            {"role":"developer","content":"Follow policy."},
            {"role":"user","content":[
                {"type":"text","text":"Describe this"},
                {"type":"image_url","image_url":{"url":"https://example.test/cat.png","detail":"high"}},
                {"type":"input_audio","input_audio":{"data":"UklGRg==","format":"wav"}}
            ]}
        ],
        "response_format":{"type":"json_schema","json_schema":{"name":"answer","schema":{"type":"object"},"strict":true}},
        "reasoning_effort":"medium",
        "service_tier":"flex",
        "modalities":["text","audio"],
        "audio":{"voice":"alloy","format":"wav"},
        "web_search_options":{"search_context_size":"medium"},
        "prediction":{"type":"content","content":"predicted"},
        "stream":false
    }"#;

    let ir = codec.parse_request(body).unwrap();
    assert!(matches!(ir.messages[0], MessageIR::Developer { .. }));
    match &ir.messages[1] {
        MessageIR::User {
            content: ContentIR::Parts(parts),
        } => {
            assert_eq!(parts.len(), 3);
            assert!(matches!(&parts[0], ContentPart::Text(t) if t.as_ref() == "Describe this"));
            assert!(
                matches!(&parts[1], ContentPart::Image { source: ImageSource::Url(u) } if u.as_ref() == "https://example.test/cat.png")
            );
            assert!(
                matches!(&parts[2], ContentPart::Audio { source: MediaSource::B64 { media_type, data } } if media_type.as_ref() == "audio/wav" && data.as_ref() == "UklGRg==")
            );
        }
        other => panic!("expected multimodal user parts, got {other:?}"),
    }
    assert_eq!(
        ir.reasoning_config
            .as_ref()
            .and_then(|r| r.effort.as_deref()),
        Some("medium")
    );
    assert_eq!(ir.service_tier.as_deref(), Some("flex"));
    assert_eq!(
        ir.audio_config.as_ref().and_then(|a| a.voice.as_deref()),
        Some("alloy")
    );
    assert!(ir.web_search.is_some());
    assert!(ir.prediction.is_some());
    assert!(matches!(
        ir.response_format,
        Some(ResponseFormatIR::JsonSchema(_))
    ));

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let reparsed = codec.parse_request(&buf).unwrap();
    assert!(matches!(reparsed.messages[0], MessageIR::Developer { .. }));
    assert_eq!(
        reparsed
            .reasoning_config
            .as_ref()
            .and_then(|r| r.effort.as_deref()),
        Some("medium")
    );
    assert_eq!(reparsed.service_tier.as_deref(), Some("flex"));
    assert_eq!(
        reparsed
            .audio_config
            .as_ref()
            .and_then(|a| a.format.as_deref()),
        Some("wav")
    );
    assert!(reparsed.web_search.is_some());
    assert!(reparsed.prediction.is_some());
}

#[test]
fn comprehensive_response_gap_roundtrip() {
    let codec = OpenAi;
    let body = br#"{
        "id":"chatcmpl-gap","object":"chat.completion","created":0,"model":"gpt-4o-audio-preview",
        "choices":[{"index":0,"message":{
            "role":"assistant",
            "content":null,
            "refusal":"I cannot comply.",
            "annotations":[{"type":"url_citation","url_citation":{"start_index":0,"end_index":4,"title":"Doc","url":"https://example.test/doc"}}],
            "audio":{"id":"aud_1","data":"AAAA","expires_at":123,"transcript":"spoken answer"}
        },"finish_reason":"stop"}],
        "service_tier":"default",
        "usage":{
            "prompt_tokens":20,"completion_tokens":10,"total_tokens":30,
            "prompt_tokens_details":{"cached_tokens":3,"audio_tokens":4},
            "completion_tokens_details":{"reasoning_tokens":5,"audio_tokens":6,"accepted_prediction_tokens":7,"rejected_prediction_tokens":8}
        }
    }"#;

    let ir = codec.parse_response(body).unwrap();
    assert_eq!(ir.refusal.as_deref(), Some("I cannot comply."));
    assert_eq!(
        ir.annotations.as_ref().unwrap()[0].url.as_deref(),
        Some("https://example.test/doc")
    );
    assert_eq!(ir.service_tier.as_deref(), Some("default"));
    assert!(ir
        .metadata
        .extensions
        .as_ref()
        .unwrap()
        .contains_key("openai_audio"));
    let usage = ir.usage.as_ref().unwrap();
    assert_eq!(usage.audio_tokens_in, Some(4));
    assert_eq!(usage.audio_tokens_out, Some(6));
    assert_eq!(usage.accepted_prediction_tokens, Some(7));
    assert_eq!(usage.rejected_prediction_tokens, Some(8));

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let reparsed = codec.parse_response(&buf).unwrap();
    assert_eq!(reparsed.refusal.as_deref(), Some("I cannot comply."));
    assert_eq!(
        reparsed.annotations.as_ref().unwrap()[0].title.as_deref(),
        Some("Doc")
    );
    assert!(reparsed
        .metadata
        .extensions
        .as_ref()
        .unwrap()
        .contains_key("openai_audio"));
    assert_eq!(
        reparsed.usage.as_ref().unwrap().accepted_prediction_tokens,
        Some(7)
    );
}
