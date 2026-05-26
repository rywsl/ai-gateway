//! Lossy-path tests for cross_render: verify that vendor-specific fields
//! which the target wire cannot express are dropped from the rendered body.
//!
//! These tests complement the unit tests in cross_render.rs (which verify
//! `adapt_lossy` drops the field from IR) and the matrix tests in
//! cross_protocol_matrix.rs (which verify the round-trip across wire forms).
//! Here we assert the user-visible contract: the rendered wire bytes do NOT
//! contain the dropped field's content.

use bytes::BytesMut;
use gw_codec::cross_render::{CrossRender, WireFormat};
use gw_codec::trait_def::ProtocolCodec;
use gw_codec::{anthropic::Anthropic, openai::OpenAi};
use gw_core::ir::{
    AnnotationIR, ContentIR, FinishReasonIR, MessageIR, MetadataIR, ReasoningBlockIR,
    RedactedThinkingIR, ResponseIR, ResponseThinkingIR, ServerToolBlockIR, ThinkingBlockIR,
    UsageIR,
};
use std::borrow::Cow;

fn wire_string(buf: &BytesMut) -> String {
    String::from_utf8(buf.to_vec()).expect("rendered wire should be UTF-8")
}

#[test]
fn anthropic_thinking_to_openai_chat_drops_thinking_from_wire() {
    let ir = ResponseIR {
        id: Cow::Borrowed("msg_1"),
        model: Cow::Borrowed("claude-3-5-sonnet"),
        thinking: Some(ResponseThinkingIR {
            blocks: vec![ThinkingBlockIR {
                text: Cow::Borrowed("DROPPED_THINKING_CONTENT_MARKER"),
                signature: Some(Cow::Borrowed("DROPPED_SIGNATURE_MARKER")),
            }],
        }),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("answer text")),
            tool_calls: None,
        }],
        finish_reason: Some(FinishReasonIR::Stop),
        usage: Some(UsageIR {
            input_tokens: 5,
            output_tokens: 3,
            cached_tokens: None,
            reasoning_tokens: None,
            ..Default::default()
        }),
        ..ResponseIR::default()
    };

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::AnthropicMessages,
        to: WireFormat::OpenAiChat,
        source: ir,
    }
    .run(&OpenAi, &mut buf)
    .expect("cross-render succeeds");

    let wire = wire_string(&buf);
    assert!(
        !wire.contains("DROPPED_THINKING_CONTENT_MARKER"),
        "OpenAI Chat wire should not contain Anthropic thinking content: {wire}"
    );
    assert!(
        !wire.contains("DROPPED_SIGNATURE_MARKER"),
        "OpenAI Chat wire should not contain Anthropic thinking signature: {wire}"
    );
    // Sanity: the rest of the response did render
    assert!(wire.contains("answer text"));
}

#[test]
fn openai_responses_reasoning_to_anthropic_drops_reasoning_from_wire() {
    let ir = ResponseIR {
        id: Cow::Borrowed("resp_1"),
        model: Cow::Borrowed("o1"),
        reasoning: Some(ReasoningBlockIR {
            summary: Cow::Borrowed("DROPPED_REASONING_SUMMARY_MARKER"),
        }),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("answer")),
            tool_calls: None,
        }],
        finish_reason: Some(FinishReasonIR::Stop),
        usage: Some(UsageIR {
            input_tokens: 10,
            output_tokens: 5,
            cached_tokens: None,
            reasoning_tokens: Some(20),
            ..Default::default()
        }),
        ..ResponseIR::default()
    };

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::OpenAiResponses,
        to: WireFormat::AnthropicMessages,
        source: ir,
    }
    .run(&Anthropic, &mut buf)
    .expect("cross-render succeeds");

    let wire = wire_string(&buf);
    assert!(
        !wire.contains("DROPPED_REASONING_SUMMARY_MARKER"),
        "Anthropic wire should not contain OpenAI Responses reasoning summary: {wire}"
    );
    assert!(wire.contains("answer"));
}

#[test]
fn openai_responses_reasoning_tokens_to_anthropic_drops_from_usage() {
    let ir = ResponseIR {
        id: Cow::Borrowed("resp_2"),
        model: Cow::Borrowed("o1"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("brief answer")),
            tool_calls: None,
        }],
        finish_reason: Some(FinishReasonIR::Stop),
        usage: Some(UsageIR {
            input_tokens: 10,
            output_tokens: 5,
            cached_tokens: None,
            reasoning_tokens: Some(20),
            ..Default::default()
        }),
        ..ResponseIR::default()
    };

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::OpenAiResponses,
        to: WireFormat::AnthropicMessages,
        source: ir,
    }
    .run(&Anthropic, &mut buf)
    .expect("cross-render succeeds");

    // After cross-render + parse via Anthropic, the parsed usage should not
    // expose reasoning_tokens (Anthropic wire has no such field).
    let parsed: ResponseIR = Anthropic.parse_response(&buf).expect("parses");
    let usage = parsed.usage.expect("usage present");
    assert!(
        usage.reasoning_tokens.is_none(),
        "Anthropic-parsed usage should not surface reasoning_tokens"
    );
}

#[test]
fn anthropic_finish_reason_other_to_openai_chat_degrades_to_stop() {
    let ir = ResponseIR {
        id: Cow::Borrowed("msg_pause"),
        model: Cow::Borrowed("claude-3-5-sonnet"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("answer")),
            tool_calls: None,
        }],
        // Anthropic-specific stop_reason that doesn't map to OpenAI Chat
        finish_reason: Some(FinishReasonIR::Other(Cow::Borrowed("pause_turn"))),
        usage: Some(UsageIR {
            input_tokens: 5,
            output_tokens: 3,
            cached_tokens: None,
            reasoning_tokens: None,
            ..Default::default()
        }),
        ..ResponseIR::default()
    };

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::AnthropicMessages,
        to: WireFormat::OpenAiChat,
        source: ir,
    }
    .run(&OpenAi, &mut buf)
    .expect("cross-render succeeds");

    let parsed: ResponseIR = OpenAi.parse_response(&buf).expect("parses");
    // map_finish_reason degrades Other(_) -> Stop on non-identity cross-render
    assert!(matches!(parsed.finish_reason, Some(FinishReasonIR::Stop)));

    let wire = wire_string(&buf);
    assert!(
        !wire.contains("pause_turn"),
        "OpenAI Chat wire should not carry Anthropic-specific stop_reason: {wire}"
    );
}

#[test]
fn openai_rich_response_to_gemini_drops_unsupported_lossy_fields() {
    let mut extensions = serde_json::Map::new();
    extensions.insert(
        "openai_audio".to_owned(),
        serde_json::json!({"id":"aud","data":"DROPPED_AUDIO_DATA_MARKER","expires_at":1,"transcript":"audio"}),
    );
    let ir = ResponseIR {
        id: Cow::Borrowed("chatcmpl_lossy"),
        model: Cow::Borrowed("gpt-4o"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("answer")),
            tool_calls: None,
        }],
        refusal: Some(Cow::Borrowed("DROPPED_REFUSAL_MARKER")),
        annotations: Some(vec![AnnotationIR {
            kind: Cow::Borrowed("url_citation"),
            url: Some(Cow::Borrowed("https://dropped.example")),
            title: Some(Cow::Borrowed("DROPPED_ANNOTATION_MARKER")),
            start_index: Some(0),
            end_index: Some(1),
        }]),
        service_tier: Some(Cow::Borrowed("DROPPED_SERVICE_TIER_MARKER")),
        metadata: MetadataIR {
            extensions: Some(extensions),
            ..Default::default()
        },
        usage: Some(UsageIR {
            input_tokens: 4,
            output_tokens: 2,
            audio_tokens_in: Some(9),
            audio_tokens_out: Some(10),
            service_tier: Some(Cow::Borrowed("DROPPED_USAGE_TIER")),
            ..Default::default()
        }),
        finish_reason: Some(FinishReasonIR::Refusal),
        ..ResponseIR::default()
    };

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::OpenAiChat,
        to: WireFormat::GeminiGenerate,
        source: ir,
    }
    .run(&gw_codec::gemini::Gemini, &mut buf)
    .expect("cross-render succeeds");

    let wire = wire_string(&buf);
    assert!(!wire.contains("DROPPED_REFUSAL_MARKER"));
    assert!(!wire.contains("DROPPED_ANNOTATION_MARKER"));
    assert!(!wire.contains("DROPPED_AUDIO_DATA_MARKER"));
    assert!(!wire.contains("DROPPED_SERVICE_TIER_MARKER"));
    let parsed = gw_codec::gemini::Gemini.parse_response(&buf).unwrap();
    assert!(parsed.refusal.is_none());
    assert!(parsed.annotations.is_none());
    assert!(parsed.service_tier.is_none());
    assert!(parsed.usage.as_ref().unwrap().audio_tokens_in.is_none());
}

#[test]
fn anthropic_private_blocks_to_openai_chat_are_dropped_from_wire() {
    let ir = ResponseIR {
        id: Cow::Borrowed("msg_private"),
        model: Cow::Borrowed("claude-3-7-sonnet"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("visible answer")),
            tool_calls: None,
        }],
        server_tool_blocks: Some(vec![ServerToolBlockIR {
            kind: Cow::Borrowed("mcp_tool_use"),
            id: Cow::Borrowed("mcp_1"),
            name: Cow::Borrowed("read_file"),
            input: serde_json::value::RawValue::from_string(
                r#"{"path":"DROPPED_MCP_INPUT"}"#.to_owned(),
            )
            .unwrap(),
            extra: serde_json::Map::new(),
        }]),
        redacted_thinking: Some(vec![RedactedThinkingIR {
            data: Cow::Borrowed("DROPPED_REDACTED_THINKING"),
        }]),
        finish_reason: Some(FinishReasonIR::Stop),
        usage: Some(UsageIR {
            input_tokens: 2,
            output_tokens: 1,
            ..Default::default()
        }),
        ..ResponseIR::default()
    };

    let mut buf = BytesMut::new();
    CrossRender {
        from: WireFormat::AnthropicMessages,
        to: WireFormat::OpenAiChat,
        source: ir,
    }
    .run(&OpenAi, &mut buf)
    .expect("cross-render succeeds");

    let wire = wire_string(&buf);
    assert!(wire.contains("visible answer"));
    assert!(!wire.contains("DROPPED_MCP_INPUT"));
    assert!(!wire.contains("DROPPED_REDACTED_THINKING"));
}
