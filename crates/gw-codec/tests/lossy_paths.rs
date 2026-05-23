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
    ContentIR, FinishReasonIR, MessageIR, ReasoningBlockIR, ResponseIR, ResponseThinkingIR,
    ThinkingBlockIR, UsageIR,
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
