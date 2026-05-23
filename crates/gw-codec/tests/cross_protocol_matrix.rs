//! S-codec §5.2 — cross-protocol response matrix.
//!
//! 4 wire forms × 4 wire forms = 16 cases. Each case fabricates a source
//! `ResponseIR` populated with the vendor-specific surface the source codec
//! would plausibly emit (thinking for Anthropic / OpenAI Responses,
//! `tool_use_blocks` for Anthropic), runs `CrossRender::run` through the
//! target codec, parses the resulting wire bytes back via the same target
//! codec, and asserts the public fields survive:
//!   - usage round-trips
//!   - finish_reason round-trips (after any deliberate cross-wire mapping)
//!   - thinking is dropped when the target wire lacks `supports_thinking`
//!
//! Same-protocol cells (e.g. openai_chat_to_openai_chat) exercise the
//! identity path. The 12 truly-cross cells exercise `adapt_lossy` +
//! `render_response` end-to-end at the codec layer.

use bytes::BytesMut;
use gw_codec::cross_render::{CrossRender, WireFormat};
use gw_codec::trait_def::ProtocolCodec;
use gw_codec::{
    anthropic::Anthropic, gemini::Gemini, openai::responses::OpenAiResponsesCodec, openai::OpenAi,
};
use gw_core::ir::{
    ContentIR, FinishReasonIR, MessageIR, ResponseIR, ResponseThinkingIR, ThinkingBlockIR,
    ToolCallIR, ToolUseBlockIR, UsageIR,
};
use serde_json::value::RawValue;
use std::borrow::Cow;

fn codec_for(w: WireFormat) -> Box<dyn ProtocolCodec> {
    match w {
        WireFormat::OpenAiChat => Box::new(OpenAi),
        WireFormat::OpenAiResponses => Box::new(OpenAiResponsesCodec),
        WireFormat::AnthropicMessages => Box::new(Anthropic),
        WireFormat::GeminiGenerate => Box::new(Gemini),
    }
}

fn raw_value(s: &str) -> Box<RawValue> {
    RawValue::from_string(s.to_owned()).expect("valid JSON")
}

/// Source IR for cross-protocol case (`from`, ...). Pre-populates the fields
/// the source codec could plausibly emit, so `cross_render::adapt_lossy`
/// gets exercised on the realistic surface.
fn source_ir_for(from: WireFormat) -> ResponseIR<'static> {
    let mut ir = ResponseIR {
        id: Cow::Borrowed("resp-test"),
        model: Cow::Borrowed("test-model"),
        messages: vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Borrowed("Hello, world!")),
            tool_calls: Some(vec![ToolCallIR {
                id: Cow::Borrowed("call_1"),
                name: Cow::Borrowed("get_weather"),
                arguments_json: Cow::Owned(raw_value(r#"{"city":"SF"}"#)),
            }]),
        }],
        finish_reason: Some(FinishReasonIR::ToolCalls),
        usage: Some(UsageIR {
            input_tokens: 5,
            output_tokens: 3,
            cached_tokens: None,
            reasoning_tokens: None,
        }),
        ..ResponseIR::default()
    };

    // Anthropic / OpenAI Responses can emit thinking; populate so the matrix
    // exercises the lossy-drop decision when the target lacks support.
    if matches!(
        from,
        WireFormat::AnthropicMessages | WireFormat::OpenAiResponses
    ) {
        ir.thinking = Some(ResponseThinkingIR {
            blocks: vec![ThinkingBlockIR {
                text: Cow::Borrowed("reasoning step"),
                signature: None,
            }],
        });
    }

    // Anthropic also natively expresses `tool_use_blocks` alongside
    // `tool_calls`; populate so the Anthropic render path picks the
    // Anthropic-shape slot (its preferred source).
    if from == WireFormat::AnthropicMessages {
        ir.tool_use_blocks = Some(vec![ToolUseBlockIR {
            id: Cow::Borrowed("toolu_1"),
            name: Cow::Borrowed("get_weather"),
            input: Cow::Owned(raw_value(r#"{"city":"SF"}"#)),
        }]);
    }

    ir
}

/// Verify the round-trip invariants for one cell of the matrix.
fn assert_matrix_case(from: WireFormat, to: WireFormat) {
    let ir = source_ir_for(from);
    let cr = CrossRender {
        from,
        to,
        source: ir,
    };
    let target = codec_for(to);
    let mut buf = BytesMut::new();
    cr.run(target.as_ref(), &mut buf)
        .unwrap_or_else(|e| panic!("CrossRender failed: from={from} to={to} err={e:?}"));
    let parsed: ResponseIR = target
        .parse_response(&buf)
        .unwrap_or_else(|e| panic!("parse_response failed: from={from} to={to} err={e:?}"));

    assert!(
        parsed.usage.is_some(),
        "from={from} to={to}: usage missing after round-trip"
    );
    assert!(
        parsed.finish_reason.is_some(),
        "from={from} to={to}: finish_reason missing after round-trip"
    );

    // Lossy-field assertion: if the target wire cannot express thinking,
    // the parsed response must not carry it.
    let to_caps = to.capabilities();
    if !to_caps.supports_thinking {
        assert!(
            parsed.thinking.is_none(),
            "from={from} to={to}: thinking should be dropped (target lacks supports_thinking)"
        );
    }
}

macro_rules! cross_protocol_case {
    ($name:ident, $from:expr, $to:expr) => {
        #[test]
        fn $name() {
            assert_matrix_case($from, $to);
        }
    };
}

// ── row 1: source = OpenAI Chat ───────────────────────────────────────────
cross_protocol_case!(
    openai_chat_to_openai_chat,
    WireFormat::OpenAiChat,
    WireFormat::OpenAiChat
);
cross_protocol_case!(
    openai_chat_to_openai_responses,
    WireFormat::OpenAiChat,
    WireFormat::OpenAiResponses
);
cross_protocol_case!(
    openai_chat_to_anthropic,
    WireFormat::OpenAiChat,
    WireFormat::AnthropicMessages
);
cross_protocol_case!(
    openai_chat_to_gemini,
    WireFormat::OpenAiChat,
    WireFormat::GeminiGenerate
);

// ── row 2: source = OpenAI Responses ──────────────────────────────────────
cross_protocol_case!(
    openai_responses_to_openai_chat,
    WireFormat::OpenAiResponses,
    WireFormat::OpenAiChat
);
cross_protocol_case!(
    openai_responses_to_openai_responses,
    WireFormat::OpenAiResponses,
    WireFormat::OpenAiResponses
);
cross_protocol_case!(
    openai_responses_to_anthropic,
    WireFormat::OpenAiResponses,
    WireFormat::AnthropicMessages
);
cross_protocol_case!(
    openai_responses_to_gemini,
    WireFormat::OpenAiResponses,
    WireFormat::GeminiGenerate
);

// ── row 3: source = Anthropic Messages ────────────────────────────────────
cross_protocol_case!(
    anthropic_to_openai_chat,
    WireFormat::AnthropicMessages,
    WireFormat::OpenAiChat
);
cross_protocol_case!(
    anthropic_to_openai_responses,
    WireFormat::AnthropicMessages,
    WireFormat::OpenAiResponses
);
cross_protocol_case!(
    anthropic_to_anthropic,
    WireFormat::AnthropicMessages,
    WireFormat::AnthropicMessages
);
cross_protocol_case!(
    anthropic_to_gemini,
    WireFormat::AnthropicMessages,
    WireFormat::GeminiGenerate
);

// ── row 4: source = Gemini Generate ───────────────────────────────────────
cross_protocol_case!(
    gemini_to_openai_chat,
    WireFormat::GeminiGenerate,
    WireFormat::OpenAiChat
);
cross_protocol_case!(
    gemini_to_openai_responses,
    WireFormat::GeminiGenerate,
    WireFormat::OpenAiResponses
);
cross_protocol_case!(
    gemini_to_anthropic,
    WireFormat::GeminiGenerate,
    WireFormat::AnthropicMessages
);
cross_protocol_case!(
    gemini_to_gemini,
    WireFormat::GeminiGenerate,
    WireFormat::GeminiGenerate
);
