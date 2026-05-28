//! Locks ProtocolKind / ApiShape wire forms against silent drift.
//!
//! Two wire forms are intentional cross-process contract — see
//! `docs/superpowers/specs/2026-05-28-systemic-fixes-design.md` §6.

use gw_core::protocol::{ApiShape, ProtocolKind};

#[test]
fn protocol_kind_serde_wire_form_locked() {
    // Go <-> Rust entity payload JSONB contract.
    // DO NOT change these strings — Go writes them into PG.
    let cases: &[(ProtocolKind, &str)] = &[
        (ProtocolKind::OpenAi, "open_ai"),
        (ProtocolKind::Anthropic, "anthropic"),
        (ProtocolKind::Gemini, "gemini"),
    ];
    for (v, expected) in cases {
        let got = serde_json::to_value(v).unwrap();
        let got_str = got.as_str().expect("ProtocolKind serializes as string");
        assert_eq!(
            got_str, *expected,
            "{:?}: serde wire form drift; got '{}', contract baseline '{}'. \
             entity payload JSONB in PG depends on this — do NOT change.",
            v, got_str, expected,
        );
    }
}

#[test]
fn protocol_kind_as_str_wire_form_locked() {
    // events:entity stream + model_mapping.protocol TEXT column contract.
    let cases: &[(ProtocolKind, &str)] = &[
        (ProtocolKind::OpenAi, "openai"),
        (ProtocolKind::Anthropic, "anthropic"),
        (ProtocolKind::Gemini, "gemini"),
    ];
    for (v, expected) in cases {
        assert_eq!(
            v.as_str(), *expected,
            "{:?}: as_str wire form drift; events stream + model_mapping.protocol depend on this.",
            v,
        );
    }
}

#[test]
fn api_shape_serde_wire_form_locked() {
    use ApiShape::*;
    let cases: &[(ApiShape, &str)] = &[
        (OpenAiChat, "open_ai_chat"),
        (OpenAiResponses, "open_ai_responses"),
        (AnthropicMessages, "anthropic_messages"),
        (GeminiGenerate, "gemini_generate"),
        (ClaudeCli, "claude_cli"),
        (OpenAiCli, "open_ai_cli"),
        (OpenAiCompact, "open_ai_compact"),
        (GeminiCli, "gemini_cli"),
    ];
    for (v, expected) in cases {
        let got = serde_json::to_value(v).unwrap();
        let got_str = got.as_str().expect("ApiShape serializes as string");
        assert_eq!(
            got_str, *expected,
            "{:?}: ApiShape serde wire form drift; got '{}', baseline '{}'. \
             capability resolution + any persisted ApiShape JSON depend on this — do NOT change.",
            v, got_str, expected,
        );
    }
}

#[test]
fn api_shape_format_id_wire_form_locked() {
    use ApiShape::*;
    let cases: &[(ApiShape, &str)] = &[
        (OpenAiChat, "openai:chat"),
        (OpenAiResponses, "openai:responses"),
        (AnthropicMessages, "anthropic:messages"),
        (GeminiGenerate, "gemini:generate"),
        (ClaudeCli, "claude:cli"),
        (OpenAiCli, "openai:cli"),
        (OpenAiCompact, "openai:compact"),
        (GeminiCli, "gemini:cli"),
    ];
    for (v, expected) in cases {
        let got = v.format_id();
        assert_eq!(
            got, *expected,
            "{:?}: ApiShape format_id wire drift; got '{}', baseline '{}'. \
             Aether FORMAT_ID interop (events stream + capability resolver) depends on this — do NOT change.",
            v, got, expected,
        );
    }
}
