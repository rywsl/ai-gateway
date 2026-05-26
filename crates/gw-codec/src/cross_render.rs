//! 跨协议响应 re-render 入口。请求侧已在 v1 实现（via.rs 双向），响应侧由本模块在
//! S-codec spec §2.3 / §3.2 中规定的流程下提供同步 IR 调整 + render dispatch。
//!
//! 流程：
//!   1. CrossRender::run 收到 source ResponseIR + target codec
//!   2. adapt_lossy 按 to.capabilities() 裁剪不可表达字段（每次裁剪触发 LossyField::drop）
//!   3. finish_reason 经 map_finish_reason 映射到目标 wire 的等价值
//!   4. target_codec.render_response(&adapted_ir, &mut buf) 渲染 wire bytes

use bytes::BytesMut;
use gw_core::error::ErrorIR;
use gw_core::ir::{FinishReasonIR, ResponseIR};

use crate::trait_def::ProtocolCodec;

/// 4 个 wire form 的标识。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFormat {
    OpenAiChat,
    OpenAiResponses,
    AnthropicMessages,
    GeminiGenerate,
}

impl WireFormat {
    pub fn label(self) -> &'static str {
        match self {
            WireFormat::OpenAiChat => "openai_chat",
            WireFormat::OpenAiResponses => "openai_responses",
            WireFormat::AnthropicMessages => "anthropic_messages",
            WireFormat::GeminiGenerate => "gemini_generate",
        }
    }

    /// Map an `AnyCodec` enum variant to its wire-form identifier.
    ///
    /// Each `AnyCodec` variant corresponds 1:1 with a [`WireFormat`]. This is the
    /// callsite the proxy uses when invoking [`CrossRender`] from the response
    /// filter — `ProtocolKind` is too coarse because it collapses OpenAI Chat
    /// and OpenAI Responses into the same family, while cross-render needs
    /// per-wire capability gates (e.g. `supports_reasoning`).
    pub fn from_any_codec(c: &crate::any::AnyCodec) -> Self {
        use crate::any::AnyCodec;
        match c {
            AnyCodec::OpenAi(_) => WireFormat::OpenAiChat,
            AnyCodec::OpenAiResponses(_) => WireFormat::OpenAiResponses,
            AnyCodec::Anthropic(_) => WireFormat::AnthropicMessages,
            AnyCodec::Gemini(_) => WireFormat::GeminiGenerate,
        }
    }

    pub fn capabilities(self) -> WireCapabilities {
        match self {
            WireFormat::OpenAiChat => WireCapabilities {
                supports_thinking: false,
                supports_reasoning: false,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: true,
                supports_function_call_parts: false,
                supports_reasoning_tokens: false,
                supports_prediction: true,
                supports_thoughts_tokens: false,
                supports_cache_creation_detail: false,
                supports_cached_tokens: true,
                supports_server_tool_usage: false,
                supports_refusal: true,
                supports_annotations: true,
                supports_server_tool_blocks: false,
                supports_redacted_thinking: false,
                supports_audio: true,
                supports_document_blocks: false,
                supports_service_tier: true,
                supports_inference_geo: false,
                supports_reasoning_config: true,
                supports_grounding: false,
                supports_citations: false,
                supports_mcp: false,
            },
            WireFormat::OpenAiResponses => WireCapabilities {
                supports_thinking: true,
                supports_reasoning: true,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: true,
                supports_function_call_parts: false,
                supports_reasoning_tokens: true,
                supports_prediction: true,
                supports_thoughts_tokens: false,
                supports_cache_creation_detail: false,
                supports_cached_tokens: true,
                supports_server_tool_usage: false,
                supports_refusal: true,
                supports_annotations: false,
                supports_server_tool_blocks: false,
                supports_redacted_thinking: false,
                supports_audio: false,
                supports_document_blocks: false,
                supports_service_tier: true,
                supports_inference_geo: false,
                supports_reasoning_config: true,
                supports_grounding: false,
                supports_citations: false,
                supports_mcp: false,
            },
            WireFormat::AnthropicMessages => WireCapabilities {
                supports_thinking: true,
                supports_reasoning: false,
                supports_tool_use_blocks: true,
                supports_tool_calls_flat: false,
                supports_function_call_parts: false,
                supports_reasoning_tokens: false,
                supports_prediction: false,
                supports_thoughts_tokens: false,
                supports_cache_creation_detail: true,
                supports_cached_tokens: true,
                supports_server_tool_usage: true,
                supports_refusal: false,
                supports_annotations: false,
                supports_server_tool_blocks: true,
                supports_redacted_thinking: true,
                supports_audio: false,
                supports_document_blocks: true,
                supports_service_tier: true,
                supports_inference_geo: true,
                supports_reasoning_config: true,
                supports_grounding: false,
                supports_citations: true,
                supports_mcp: true,
            },
            WireFormat::GeminiGenerate => WireCapabilities {
                supports_thinking: true,
                supports_reasoning: false,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: false,
                supports_function_call_parts: true,
                supports_reasoning_tokens: false,
                supports_prediction: false,
                supports_thoughts_tokens: true,
                supports_cache_creation_detail: false,
                supports_cached_tokens: false,
                supports_server_tool_usage: false,
                supports_refusal: false,
                supports_annotations: false,
                supports_server_tool_blocks: false,
                supports_redacted_thinking: false,
                supports_audio: false,
                supports_document_blocks: false,
                supports_service_tier: false,
                supports_inference_geo: false,
                supports_reasoning_config: true,
                supports_grounding: true,
                supports_citations: false,
                supports_mcp: false,
            },
        }
    }
}

impl std::fmt::Display for WireFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WireCapabilities {
    pub supports_thinking: bool,
    pub supports_reasoning: bool,
    pub supports_tool_use_blocks: bool,
    pub supports_tool_calls_flat: bool,
    pub supports_function_call_parts: bool,
    pub supports_reasoning_tokens: bool,
    pub supports_prediction: bool,
    pub supports_thoughts_tokens: bool,
    pub supports_cache_creation_detail: bool,
    pub supports_cached_tokens: bool,
    pub supports_server_tool_usage: bool,
    pub supports_refusal: bool,
    pub supports_annotations: bool,
    pub supports_server_tool_blocks: bool,
    pub supports_redacted_thinking: bool,
    pub supports_audio: bool,
    pub supports_document_blocks: bool,
    pub supports_service_tier: bool,
    pub supports_inference_geo: bool,
    pub supports_reasoning_config: bool,
    pub supports_grounding: bool,
    pub supports_citations: bool,
    pub supports_mcp: bool,
}

/// 单点入口：打印 ERROR log + 增 metric。调用方负责实际从 IR 上裁剪字段。
pub struct LossyField;

impl LossyField {
    pub fn drop(from: WireFormat, to: WireFormat, field: &'static str) {
        tracing::error!(
            from = %from,
            to = %to,
            field,
            "cross-render dropped vendor-specific field; \
             check codec/adapter mapping if this is unexpected"
        );
        metrics::counter!(
            "gw_codec_cross_render_lossy_total",
            "from" => from.label(),
            "to" => to.label(),
            "field" => field
        )
        .increment(1);
    }
}

pub struct CrossRender<'a> {
    pub from: WireFormat,
    pub to: WireFormat,
    pub source: ResponseIR<'a>,
}

impl<'a> CrossRender<'a> {
    pub fn run(
        self,
        target_codec: &dyn ProtocolCodec,
        buf: &mut BytesMut,
    ) -> Result<(), ErrorIR<'static>> {
        let ir = self.adapt_lossy();
        target_codec.render_response(&ir, buf)
    }

    fn adapt_lossy(self) -> ResponseIR<'a> {
        let to_caps = self.to.capabilities();
        let mut ir = self.source;
        let (from, to) = (self.from, self.to);

        if !to_caps.supports_thinking && ir.thinking.is_some() {
            LossyField::drop(from, to, "thinking");
            ir.thinking = None;
        }
        if !to_caps.supports_reasoning && ir.reasoning.is_some() {
            LossyField::drop(from, to, "reasoning");
            ir.reasoning = None;
        }
        if !to_caps.supports_refusal && ir.refusal.is_some() {
            LossyField::drop(from, to, "refusal");
            ir.refusal = None;
        }
        if !to_caps.supports_annotations && ir.annotations.is_some() {
            LossyField::drop(from, to, "annotations");
            ir.annotations = None;
        }
        if !to_caps.supports_server_tool_blocks && ir.server_tool_blocks.is_some() {
            LossyField::drop(from, to, "server_tool_blocks");
            ir.server_tool_blocks = None;
        }
        if !to_caps.supports_redacted_thinking && ir.redacted_thinking.is_some() {
            LossyField::drop(from, to, "redacted_thinking");
            ir.redacted_thinking = None;
        }
        if !to_caps.supports_service_tier && ir.service_tier.is_some() {
            LossyField::drop(from, to, "service_tier");
            ir.service_tier = None;
        }
        if !to_caps.supports_audio {
            if let Some(ext) = &mut ir.metadata.extensions {
                if ext.remove("openai_audio").is_some() {
                    LossyField::drop(from, to, "audio");
                }
                if ext.is_empty() {
                    ir.metadata.extensions = None;
                }
            }
        }
        // tool_use_blocks 与扁平 tool_calls 是同源信息的两种 wire 形态：codec 在 render
        // 时按目标能力选用，因此 IR 层不主动裁剪 tool_use_blocks（可表达，不算 lossy）。

        if let Some(usage) = &mut ir.usage {
            if !to_caps.supports_cached_tokens && usage.cached_tokens.is_some() {
                LossyField::drop(from, to, "usage.cached_tokens");
                usage.cached_tokens = None;
            }
            if !to_caps.supports_reasoning_tokens && usage.reasoning_tokens.is_some() {
                LossyField::drop(from, to, "usage.reasoning_tokens");
                usage.reasoning_tokens = None;
            }
            if !to_caps.supports_audio && usage.audio_tokens_in.is_some() {
                LossyField::drop(from, to, "usage.audio_tokens_in");
                usage.audio_tokens_in = None;
            }
            if !to_caps.supports_audio && usage.audio_tokens_out.is_some() {
                LossyField::drop(from, to, "usage.audio_tokens_out");
                usage.audio_tokens_out = None;
            }
            if !to_caps.supports_prediction && usage.accepted_prediction_tokens.is_some() {
                LossyField::drop(from, to, "usage.accepted_prediction_tokens");
                usage.accepted_prediction_tokens = None;
            }
            if !to_caps.supports_prediction && usage.rejected_prediction_tokens.is_some() {
                LossyField::drop(from, to, "usage.rejected_prediction_tokens");
                usage.rejected_prediction_tokens = None;
            }
            if !to_caps.supports_thoughts_tokens && usage.thoughts_tokens.is_some() {
                LossyField::drop(from, to, "usage.thoughts_tokens");
                usage.thoughts_tokens = None;
            }
            if !to_caps.supports_cache_creation_detail && usage.cache_creation_tokens.is_some() {
                LossyField::drop(from, to, "usage.cache_creation_tokens");
                usage.cache_creation_tokens = None;
            }
            if !to_caps.supports_cache_creation_detail && usage.cache_creation_1h_tokens.is_some() {
                LossyField::drop(from, to, "usage.cache_creation_1h_tokens");
                usage.cache_creation_1h_tokens = None;
            }
            if !to_caps.supports_cache_creation_detail && usage.cache_creation_5m_tokens.is_some() {
                LossyField::drop(from, to, "usage.cache_creation_5m_tokens");
                usage.cache_creation_5m_tokens = None;
            }
            if !to_caps.supports_server_tool_usage
                && usage.server_tool_web_search_requests.is_some()
            {
                LossyField::drop(from, to, "usage.server_tool_web_search_requests");
                usage.server_tool_web_search_requests = None;
            }
            if !to_caps.supports_server_tool_usage && usage.server_tool_web_fetch_requests.is_some()
            {
                LossyField::drop(from, to, "usage.server_tool_web_fetch_requests");
                usage.server_tool_web_fetch_requests = None;
            }
            if !to_caps.supports_service_tier && usage.service_tier.is_some() {
                LossyField::drop(from, to, "usage.service_tier");
                usage.service_tier = None;
            }
            if !to_caps.supports_inference_geo && usage.inference_geo.is_some() {
                LossyField::drop(from, to, "usage.inference_geo");
                usage.inference_geo = None;
            }
        }

        if let Some(fr) = ir.finish_reason.take() {
            ir.finish_reason = Some(map_finish_reason(from, to, fr));
        }

        ir
    }
}

/// 跨协议 finish_reason 映射。Other(s) 落到任一目标都触发 lossy log。
pub fn map_finish_reason(from: WireFormat, to: WireFormat, fr: FinishReasonIR) -> FinishReasonIR {
    use FinishReasonIR::*;
    if from == to {
        return fr;
    }
    match fr {
        Stop | Length | ToolCalls => fr,
        Refusal => {
            if !to.capabilities().supports_refusal {
                LossyField::drop(from, to, "finish_reason.refusal");
                Stop
            } else {
                Refusal
            }
        }
        ContentFilter => map_content_filter_finish_reason(from, to),
        Other(ref s) if matches!(from, WireFormat::GeminiGenerate) => match s.as_ref() {
            "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "MODEL_ARMOR" => {
                map_content_filter_finish_reason(from, to)
            }
            "MALFORMED_FUNCTION_CALL" => {
                LossyField::drop(from, to, "finish_reason.other");
                Stop
            }
            _ => {
                LossyField::drop(from, to, "finish_reason.other");
                Stop
            }
        },
        Other(ref s) => {
            LossyField::drop(from, to, "finish_reason.other");
            // 不携带原始字符串到下游 wire，退化为 Stop
            let _ = s;
            Stop
        }
    }
}

fn map_content_filter_finish_reason(from: WireFormat, to: WireFormat) -> FinishReasonIR {
    if matches!(to, WireFormat::AnthropicMessages) {
        LossyField::drop(from, to, "finish_reason.content_filter");
        FinishReasonIR::Stop
    } else {
        FinishReasonIR::ContentFilter
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::ir::{ResponseIR, ResponseThinkingIR, ThinkingBlockIR};
    use std::borrow::Cow;

    pub(super) fn ir_with_thinking() -> ResponseIR<'static> {
        ResponseIR {
            thinking: Some(ResponseThinkingIR {
                blocks: vec![ThinkingBlockIR {
                    text: Cow::Borrowed("reasoning step"),
                    signature: None,
                }],
            }),
            ..ResponseIR::default()
        }
    }

    #[test]
    fn capabilities_openai_chat_lacks_thinking_and_reasoning() {
        let c = WireFormat::OpenAiChat.capabilities();
        assert!(!c.supports_thinking);
        assert!(!c.supports_reasoning);
    }

    #[test]
    fn adapt_lossy_drops_thinking_when_target_unsupports() {
        let cr = CrossRender {
            from: WireFormat::AnthropicMessages,
            to: WireFormat::OpenAiChat,
            source: ir_with_thinking(),
        };
        let adapted = cr.adapt_lossy();
        assert!(adapted.thinking.is_none());
    }

    #[test]
    fn adapt_lossy_keeps_thinking_when_target_supports() {
        let cr = CrossRender {
            from: WireFormat::AnthropicMessages,
            to: WireFormat::OpenAiResponses,
            source: ir_with_thinking(),
        };
        let adapted = cr.adapt_lossy();
        assert!(adapted.thinking.is_some());
    }

    #[test]
    fn finish_reason_other_degrades_to_stop_with_lossy() {
        let fr = FinishReasonIR::Other(Cow::Borrowed("pause_turn"));
        let mapped = map_finish_reason(WireFormat::AnthropicMessages, WireFormat::OpenAiChat, fr);
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn finish_reason_content_filter_degrades_to_stop_for_anthropic() {
        let mapped = map_finish_reason(
            WireFormat::OpenAiChat,
            WireFormat::AnthropicMessages,
            FinishReasonIR::ContentFilter,
        );
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn finish_reason_same_wire_identity() {
        let mapped = map_finish_reason(
            WireFormat::OpenAiChat,
            WireFormat::OpenAiChat,
            FinishReasonIR::ContentFilter,
        );
        assert!(matches!(mapped, FinishReasonIR::ContentFilter));
    }

    #[test]
    fn finish_reason_refusal_degrades_when_target_lacks_support() {
        let mapped = map_finish_reason(
            WireFormat::OpenAiChat,
            WireFormat::AnthropicMessages,
            FinishReasonIR::Refusal,
        );
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn finish_reason_refusal_keeps_when_target_supports() {
        let mapped = map_finish_reason(
            WireFormat::OpenAiChat,
            WireFormat::OpenAiResponses,
            FinishReasonIR::Refusal,
        );
        assert!(matches!(mapped, FinishReasonIR::Refusal));
    }

    #[test]
    fn finish_reason_gemini_specific_content_filters_map_to_content_filter() {
        for reason in ["BLOCKLIST", "PROHIBITED_CONTENT", "SPII", "MODEL_ARMOR"] {
            let mapped = map_finish_reason(
                WireFormat::GeminiGenerate,
                WireFormat::OpenAiChat,
                FinishReasonIR::Other(Cow::Borrowed(reason)),
            );
            assert!(
                matches!(mapped, FinishReasonIR::ContentFilter),
                "reason={reason}"
            );
        }
    }

    #[test]
    fn finish_reason_gemini_specific_content_filter_degrades_for_anthropic() {
        let mapped = map_finish_reason(
            WireFormat::GeminiGenerate,
            WireFormat::AnthropicMessages,
            FinishReasonIR::Other(Cow::Borrowed("MODEL_ARMOR")),
        );
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn finish_reason_gemini_malformed_function_call_degrades_to_stop() {
        let mapped = map_finish_reason(
            WireFormat::GeminiGenerate,
            WireFormat::OpenAiChat,
            FinishReasonIR::Other(Cow::Borrowed("MALFORMED_FUNCTION_CALL")),
        );
        assert!(matches!(mapped, FinishReasonIR::Stop));
    }

    #[test]
    fn adapt_lossy_matrix_thinking_drop_decisions() {
        use WireFormat::*;
        // (from, to, should_drop_thinking)
        // Pairs match spec §4.2: thinking is lossy when target lacks supports_thinking.
        let cases = [
            (AnthropicMessages, OpenAiChat, true),
            (AnthropicMessages, OpenAiResponses, false),
            (AnthropicMessages, GeminiGenerate, false),
            (OpenAiResponses, OpenAiChat, true),
            (OpenAiResponses, AnthropicMessages, false),
            (OpenAiResponses, GeminiGenerate, false),
        ];
        for (from, to, should_drop) in cases {
            let cr = CrossRender {
                from,
                to,
                source: ir_with_thinking(),
            };
            let adapted = cr.adapt_lossy();
            assert_eq!(
                adapted.thinking.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_reasoning_drop_decisions() {
        use gw_core::ir::ReasoningBlockIR;
        use std::borrow::Cow;
        use WireFormat::*;
        // Only target that supports reasoning is OpenAI Responses itself.
        let cases = [
            (OpenAiResponses, OpenAiChat, true),
            (OpenAiResponses, AnthropicMessages, true),
            (OpenAiResponses, GeminiGenerate, true),
        ];
        for (from, to, should_drop) in cases {
            let ir = gw_core::ir::ResponseIR {
                reasoning: Some(ReasoningBlockIR {
                    summary: Cow::Borrowed("test summary"),
                }),
                ..gw_core::ir::ResponseIR::default()
            };
            let cr = CrossRender {
                from,
                to,
                source: ir,
            };
            let adapted = cr.adapt_lossy();
            assert_eq!(
                adapted.reasoning.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_reasoning_tokens_drop_decisions() {
        use WireFormat::*;
        let cases = [
            (OpenAiResponses, OpenAiChat, true),
            (OpenAiResponses, AnthropicMessages, true),
            (OpenAiResponses, GeminiGenerate, true),
        ];
        for (from, to, should_drop) in cases {
            let ir = gw_core::ir::ResponseIR {
                usage: Some(gw_core::ir::UsageIR {
                    input_tokens: 5,
                    output_tokens: 3,
                    cached_tokens: None,
                    reasoning_tokens: Some(20),
                    ..Default::default()
                }),
                ..gw_core::ir::ResponseIR::default()
            };
            let cr = CrossRender {
                from,
                to,
                source: ir,
            };
            let adapted = cr.adapt_lossy();
            let dropped = adapted
                .usage
                .as_ref()
                .and_then(|u| u.reasoning_tokens)
                .is_none();
            assert_eq!(
                dropped, should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_refusal_drop_decisions() {
        for (from, to) in all_wire_pairs() {
            let ir = gw_core::ir::ResponseIR {
                refusal: Some(Cow::Borrowed("policy refusal")),
                ..gw_core::ir::ResponseIR::default()
            };
            let adapted = CrossRender {
                from,
                to,
                source: ir,
            }
            .adapt_lossy();
            let should_drop = !to.capabilities().supports_refusal;
            assert_eq!(
                adapted.refusal.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_annotations_drop_decisions() {
        for (from, to) in all_wire_pairs() {
            let ir = gw_core::ir::ResponseIR {
                annotations: Some(vec![gw_core::ir::AnnotationIR {
                    kind: Cow::Borrowed("url_citation"),
                    url: Some(Cow::Borrowed("https://example.test")),
                    title: None,
                    start_index: None,
                    end_index: None,
                }]),
                ..gw_core::ir::ResponseIR::default()
            };
            let adapted = CrossRender {
                from,
                to,
                source: ir,
            }
            .adapt_lossy();
            let should_drop = !to.capabilities().supports_annotations;
            assert_eq!(
                adapted.annotations.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_server_tool_blocks_drop_decisions() {
        for (from, to) in all_wire_pairs() {
            let ir = gw_core::ir::ResponseIR {
                server_tool_blocks: Some(vec![gw_core::ir::ServerToolBlockIR {
                    kind: Cow::Borrowed("web_search"),
                    id: Cow::Borrowed("srv_1"),
                    name: Cow::Borrowed("web_search"),
                    input: serde_json::value::RawValue::from_string(r#"{}"#.to_owned())
                        .expect("valid raw json"),
                    extra: serde_json::Map::new(),
                }]),
                ..gw_core::ir::ResponseIR::default()
            };
            let adapted = CrossRender {
                from,
                to,
                source: ir,
            }
            .adapt_lossy();
            let should_drop = !to.capabilities().supports_server_tool_blocks;
            assert_eq!(
                adapted.server_tool_blocks.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_redacted_thinking_drop_decisions() {
        for (from, to) in all_wire_pairs() {
            let ir = gw_core::ir::ResponseIR {
                redacted_thinking: Some(vec![gw_core::ir::RedactedThinkingIR {
                    data: Cow::Borrowed("opaque"),
                }]),
                ..gw_core::ir::ResponseIR::default()
            };
            let adapted = CrossRender {
                from,
                to,
                source: ir,
            }
            .adapt_lossy();
            let should_drop = !to.capabilities().supports_redacted_thinking;
            assert_eq!(
                adapted.redacted_thinking.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_service_tier_drop_decisions() {
        for (from, to) in all_wire_pairs() {
            let ir = gw_core::ir::ResponseIR {
                service_tier: Some(Cow::Borrowed("priority")),
                ..gw_core::ir::ResponseIR::default()
            };
            let adapted = CrossRender {
                from,
                to,
                source: ir,
            }
            .adapt_lossy();
            let should_drop = !to.capabilities().supports_service_tier;
            assert_eq!(
                adapted.service_tier.is_none(),
                should_drop,
                "from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    #[test]
    fn adapt_lossy_matrix_audio_drop_decisions() {
        for (from, to) in all_wire_pairs() {
            let mut extensions = serde_json::Map::new();
            extensions.insert(
                "openai_audio".to_owned(),
                serde_json::json!({
                    "id": "aud_1",
                    "data": "base64",
                    "expires_at": 123_u64,
                    "transcript": "hello",
                }),
            );
            let ir = gw_core::ir::ResponseIR {
                metadata: gw_core::ir::MetadataIR {
                    extensions: Some(extensions),
                    ..Default::default()
                },
                usage: Some(gw_core::ir::UsageIR {
                    audio_tokens_in: Some(11),
                    audio_tokens_out: Some(13),
                    ..Default::default()
                }),
                ..gw_core::ir::ResponseIR::default()
            };
            let adapted = CrossRender {
                from,
                to,
                source: ir,
            }
            .adapt_lossy();
            let should_drop = !to.capabilities().supports_audio;
            let audio_missing = adapted
                .metadata
                .extensions
                .as_ref()
                .and_then(|ext| ext.get("openai_audio"))
                .is_none();
            let audio_usage_missing = adapted
                .usage
                .as_ref()
                .is_none_or(|u| u.audio_tokens_in.is_none() && u.audio_tokens_out.is_none());
            assert_eq!(
                audio_missing, should_drop,
                "audio ext from={from} to={to} should_drop={should_drop}"
            );
            assert_eq!(
                audio_usage_missing, should_drop,
                "audio usage from={from} to={to} should_drop={should_drop}"
            );
        }
    }

    fn all_wire_pairs() -> impl Iterator<Item = (WireFormat, WireFormat)> {
        use WireFormat::*;
        const WIRES: [WireFormat; 4] = [
            OpenAiChat,
            OpenAiResponses,
            AnthropicMessages,
            GeminiGenerate,
        ];
        WIRES
            .into_iter()
            .flat_map(|from| WIRES.map(|to| (from, to)))
    }
}
