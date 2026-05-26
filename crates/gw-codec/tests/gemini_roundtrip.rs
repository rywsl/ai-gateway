use gw_codec::gemini::Gemini;
use gw_codec::ProtocolCodec;
use gw_core::ir::{MessageIR, ReasoningConfig, StreamEventIR};
use std::borrow::Cow;

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
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        StreamEventIR::Delta { text, .. } => assert_eq!(text.as_deref().unwrap(), "hi"),
        _ => panic!("expected Delta"),
    }
}

#[test]
fn parse_stream_finish_reason() {
    let codec = Gemini;
    let chunk = b"data: {\"candidates\":[{\"finishReason\":\"STOP\"}]}\n\n";
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert_eq!(evs.len(), 1);
    assert!(matches!(evs[0], StreamEventIR::End { .. }));
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

#[test]
fn extended_generation_config_roundtrip() {
    let json = r#"{
        "contents": [{"role": "user", "parts": [{"text": "hi"}]}],
        "generationConfig": {
            "maxOutputTokens": 100,
            "temperature": 0.7,
            "topP": 0.9,
            "topK": 40,
            "stopSequences": ["END", "STOP"],
            "presencePenalty": 0.5,
            "frequencyPenalty": 0.3,
            "seed": 42,
            "thinkingConfig": {
                "thinkingBudget": 1024,
                "thinkingLevel": "medium"
            }
        }
    }"#;
    let codec = Gemini;
    let ir = codec.parse_request(json.as_bytes()).unwrap();
    assert_eq!(ir.top_p, Some(0.9));
    assert_eq!(ir.top_k, Some(40));
    assert_eq!(ir.stop.as_ref().unwrap().len(), 2);
    assert_eq!(ir.presence_penalty, Some(0.5));
    assert_eq!(ir.frequency_penalty, Some(0.3));
    assert_eq!(ir.seed, Some(42));
    let rc = ir.reasoning_config.as_ref().unwrap();
    assert_eq!(rc.budget_tokens, Some(1024));
    assert_eq!(rc.effort.as_deref(), Some("medium"));

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_request(&buf).unwrap();
    assert_eq!(ir2.top_p, ir.top_p);
    assert_eq!(ir2.top_k, ir.top_k);
    assert_eq!(ir2.stop, ir.stop);
    assert_eq!(ir2.presence_penalty, ir.presence_penalty);
    assert_eq!(ir2.frequency_penalty, ir.frequency_penalty);
    assert_eq!(ir2.seed, ir.seed);
    let rc2 = ir2.reasoning_config.as_ref().unwrap();
    assert_eq!(rc2.budget_tokens, rc.budget_tokens);
    assert_eq!(rc2.effort.as_deref(), rc.effort.as_deref());
}

#[test]
fn ir_with_extended_fields_renders_to_gemini() {
    let codec = Gemini;
    let mut ir = codec.parse_request(BASIC.as_bytes()).unwrap();
    ir.top_p = Some(0.95);
    ir.top_k = Some(50);
    ir.stop = Some(vec![Cow::Borrowed("DONE")]);
    ir.seed = Some(123);
    ir.presence_penalty = Some(0.1);
    ir.frequency_penalty = Some(0.2);
    ir.reasoning_config = Some(ReasoningConfig {
        effort: Some(Cow::Borrowed("high")),
        budget_tokens: Some(2048),
    });

    let mut buf = bytes::BytesMut::new();
    codec.render_request(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_request(&buf).unwrap();
    assert_eq!(ir2.top_p, Some(0.95));
    assert_eq!(ir2.top_k, Some(50));
    assert_eq!(ir2.seed, Some(123));
    assert_eq!(ir2.presence_penalty, Some(0.1));
    assert_eq!(ir2.frequency_penalty, Some(0.2));
    assert_eq!(ir2.stop.as_ref().unwrap(), &[Cow::Borrowed("DONE")]);
    let rc = ir2.reasoning_config.unwrap();
    assert_eq!(rc.effort.as_deref(), Some("high"));
    assert_eq!(rc.budget_tokens, Some(2048));
}

#[test]
fn parse_response_thinking_parts() {
    let codec = Gemini;
    let json = r#"{
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"text": "Let me think about this...", "thought": true, "thoughtSignature": "sig123"},
                    {"text": "The answer is 42."}
                ]
            },
            "finishReason": "STOP"
        }],
        "usageMetadata": {
            "promptTokenCount": 10,
            "candidatesTokenCount": 20,
            "thoughtsTokenCount": 15
        }
    }"#;
    let ir = codec.parse_response(json.as_bytes()).unwrap();
    let thinking = ir.thinking.as_ref().expect("thinking should be present");
    assert_eq!(thinking.blocks.len(), 1);
    assert_eq!(
        thinking.blocks[0].text.as_ref(),
        "Let me think about this..."
    );
    assert_eq!(thinking.blocks[0].signature.as_deref(), Some("sig123"));
    let last = ir.messages.last().unwrap();
    match last {
        gw_core::ir::MessageIR::Assistant { content, .. } => {
            let text = match content {
                gw_core::ir::ContentIR::Text(t) => t.as_ref(),
                _ => panic!("expected text content"),
            };
            assert_eq!(text, "The answer is 42.");
        }
        _ => panic!("expected Assistant message"),
    }
    let usage = ir.usage.as_ref().unwrap();
    assert_eq!(usage.thoughts_tokens, Some(15));
}

#[test]
fn thinking_response_roundtrip() {
    use gw_core::ir::{
        ContentIR, FinishReasonIR, MessageIR, ResponseIR, ResponseThinkingIR, ThinkingBlockIR,
        UsageIR,
    };

    let codec = Gemini;
    let ir = ResponseIR {
        id: Cow::Borrowed(""),
        model: Cow::Borrowed("gemini-2.5-pro"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("Final answer.")),
            tool_calls: None,
        }],
        finish_reason: Some(FinishReasonIR::Stop),
        thinking: Some(ResponseThinkingIR {
            blocks: vec![ThinkingBlockIR {
                text: Cow::Borrowed("reasoning step"),
                signature: Some(Cow::Borrowed("sig_abc")),
            }],
        }),
        usage: Some(UsageIR {
            input_tokens: 5,
            output_tokens: 10,
            thoughts_tokens: Some(8),
            ..Default::default()
        }),
        ..Default::default()
    };

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_response(&buf).unwrap();

    let thinking = ir2.thinking.as_ref().expect("thinking preserved");
    assert_eq!(thinking.blocks.len(), 1);
    assert_eq!(thinking.blocks[0].text.as_ref(), "reasoning step");
    assert_eq!(thinking.blocks[0].signature.as_deref(), Some("sig_abc"));

    let usage = ir2.usage.as_ref().unwrap();
    assert_eq!(usage.thoughts_tokens, Some(8));

    match ir2.messages.last().unwrap() {
        MessageIR::Assistant {
            content: ContentIR::Text(t),
            ..
        } => assert_eq!(t.as_ref(), "Final answer."),
        _ => panic!("expected Assistant"),
    }
}

#[test]
fn parse_stream_thinking_delta() {
    let codec = Gemini;
    let chunk = br#"data: {"candidates":[{"content":{"role":"model","parts":[{"text":"thinking...","thought":true}]}}]}

"#;
    let mut state = codec.new_stream_state();
    let evs = codec.parse_stream_chunk(state.as_mut(), chunk).unwrap();
    assert_eq!(evs.len(), 1);
    match &evs[0] {
        StreamEventIR::Delta { thinking, text, .. } => {
            assert!(text.is_none());
            assert_eq!(thinking.as_deref().unwrap(), "thinking...");
        }
        _ => panic!("expected Delta with thinking"),
    }
}

#[test]
fn parse_response_executable_code_and_result() {
    let codec = Gemini;
    let json = r#"{
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [
                    {"executableCode": {"code": "print('hello')", "language": "PYTHON"}},
                    {"codeExecutionResult": {"outcome": "OUTCOME_OK", "output": "hello"}},
                    {"text": "Done."}
                ]
            },
            "finishReason": "STOP"
        }]
    }"#;
    let ir = codec.parse_response(json.as_bytes()).unwrap();
    let stbs = ir.server_tool_blocks.as_ref().expect("server_tool_blocks");
    assert_eq!(stbs.len(), 2);
    assert_eq!(stbs[0].kind.as_ref(), "executable_code");
    assert_eq!(
        stbs[0].extra.get("language").unwrap().as_str().unwrap(),
        "PYTHON"
    );
    let code: String = serde_json::from_str(stbs[0].input.get()).unwrap();
    assert_eq!(code, "print('hello')");

    assert_eq!(stbs[1].kind.as_ref(), "code_execution_result");
    assert_eq!(
        stbs[1].extra.get("outcome").unwrap().as_str().unwrap(),
        "OUTCOME_OK"
    );
    let output: String = serde_json::from_str(stbs[1].input.get()).unwrap();
    assert_eq!(output, "hello");

    match ir.messages.last().unwrap() {
        gw_core::ir::MessageIR::Assistant { content, .. } => match content {
            gw_core::ir::ContentIR::Text(t) => assert_eq!(t.as_ref(), "Done."),
            _ => panic!("expected text"),
        },
        _ => panic!("expected Assistant"),
    }
}

#[test]
fn executable_code_roundtrip() {
    use gw_core::ir::{ContentIR, FinishReasonIR, MessageIR, ResponseIR, ServerToolBlockIR};
    use serde_json::value::RawValue;

    let codec = Gemini;
    let ir = ResponseIR {
        id: Cow::Borrowed(""),
        model: Cow::Borrowed("gemini-2.5-pro"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("Result.")),
            tool_calls: None,
        }],
        finish_reason: Some(FinishReasonIR::Stop),
        server_tool_blocks: Some(vec![
            ServerToolBlockIR {
                kind: Cow::Borrowed("executable_code"),
                id: Cow::Borrowed(""),
                name: Cow::Borrowed(""),
                input: RawValue::from_string(r#""x = 1 + 2""#.to_owned()).unwrap(),
                extra: {
                    let mut m = serde_json::Map::new();
                    m.insert(
                        "language".to_owned(),
                        serde_json::Value::String("PYTHON".to_owned()),
                    );
                    m
                },
            },
            ServerToolBlockIR {
                kind: Cow::Borrowed("code_execution_result"),
                id: Cow::Borrowed(""),
                name: Cow::Borrowed(""),
                input: RawValue::from_string(r#""3""#.to_owned()).unwrap(),
                extra: {
                    let mut m = serde_json::Map::new();
                    m.insert(
                        "outcome".to_owned(),
                        serde_json::Value::String("OUTCOME_OK".to_owned()),
                    );
                    m
                },
            },
        ]),
        ..Default::default()
    };

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_response(&buf).unwrap();

    let stbs = ir2
        .server_tool_blocks
        .as_ref()
        .expect("server_tool_blocks preserved");
    assert_eq!(stbs.len(), 2);
    assert_eq!(stbs[0].kind.as_ref(), "executable_code");
    let code: String = serde_json::from_str(stbs[0].input.get()).unwrap();
    assert_eq!(code, "x = 1 + 2");
    assert_eq!(
        stbs[0].extra.get("language").unwrap().as_str().unwrap(),
        "PYTHON"
    );

    assert_eq!(stbs[1].kind.as_ref(), "code_execution_result");
    let output: String = serde_json::from_str(stbs[1].input.get()).unwrap();
    assert_eq!(output, "3");
    assert_eq!(
        stbs[1].extra.get("outcome").unwrap().as_str().unwrap(),
        "OUTCOME_OK"
    );
}

#[test]
fn grounding_metadata_roundtrip() {
    let codec = Gemini;
    let json = r#"{
        "candidates": [{
            "content": {
                "role": "model",
                "parts": [{"text": "Grounded answer."}]
            },
            "finishReason": "STOP",
            "groundingMetadata": {
                "groundingChunks": [{"web": {"uri": "https://example.com", "title": "Example"}}],
                "groundingSupports": [{"segment": {"startIndex": 0, "endIndex": 10}}]
            }
        }]
    }"#;
    let ir = codec.parse_response(json.as_bytes()).unwrap();
    let ext = ir.metadata.extensions.as_ref().expect("extensions present");
    let gm = ext.get("grounding_metadata").expect("grounding_metadata");
    assert!(gm.get("groundingChunks").is_some());
    assert!(gm.get("groundingSupports").is_some());

    let mut buf = bytes::BytesMut::new();
    codec.render_response(&ir, &mut buf).unwrap();
    let ir2 = codec.parse_response(&buf).unwrap();

    let ext2 = ir2
        .metadata
        .extensions
        .as_ref()
        .expect("extensions preserved");
    let gm2 = ext2
        .get("grounding_metadata")
        .expect("grounding_metadata preserved");
    assert_eq!(
        gm2.get("groundingChunks")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn finish_reasons_and_tool_use_prompt_usage_roundtrip() {
    let codec = Gemini;
    let cases = [
        ("STOP", "Stop"),
        ("MAX_TOKENS", "Length"),
        ("SAFETY", "ContentFilter"),
        ("MODEL_ARMOR", "Other"),
        ("MALFORMED_FUNCTION_CALL", "Other"),
    ];

    for (reason, expected) in cases {
        let json = format!(
            r#"{{
                "candidates":[{{"content":{{"role":"model","parts":[{{"text":"ok"}}]}},"finishReason":"{reason}"}}],
                "usageMetadata":{{"promptTokenCount":2,"candidatesTokenCount":3,"toolUsePromptTokenCount":4}}
            }}"#
        );
        let ir = codec.parse_response(json.as_bytes()).unwrap();
        match (expected, ir.finish_reason.as_ref().unwrap()) {
            ("Stop", gw_core::ir::FinishReasonIR::Stop) => {}
            ("Length", gw_core::ir::FinishReasonIR::Length) => {}
            ("ContentFilter", gw_core::ir::FinishReasonIR::ContentFilter) => {}
            ("Other", gw_core::ir::FinishReasonIR::Other(s)) => assert_eq!(s.as_ref(), reason),
            (label, other) => panic!("reason={reason} expected {label}, got {other:?}"),
        }

        let mut buf = bytes::BytesMut::new();
        codec.render_response(&ir, &mut buf).unwrap();
        let reparsed = codec.parse_response(&buf).unwrap();
        assert!(reparsed.finish_reason.is_some(), "reason={reason}");
        assert_eq!(reparsed.usage.as_ref().unwrap().input_tokens, 2);
        assert_eq!(reparsed.usage.as_ref().unwrap().output_tokens, 3);
    }
}
