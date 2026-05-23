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
            },
            WireFormat::OpenAiResponses => WireCapabilities {
                supports_thinking: true,
                supports_reasoning: true,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: true,
                supports_function_call_parts: false,
                supports_reasoning_tokens: true,
            },
            WireFormat::AnthropicMessages => WireCapabilities {
                supports_thinking: true,
                supports_reasoning: false,
                supports_tool_use_blocks: true,
                supports_tool_calls_flat: false,
                supports_function_call_parts: false,
                supports_reasoning_tokens: false,
            },
            WireFormat::GeminiGenerate => WireCapabilities {
                supports_thinking: false,
                supports_reasoning: false,
                supports_tool_use_blocks: false,
                supports_tool_calls_flat: false,
                supports_function_call_parts: true,
                supports_reasoning_tokens: false,
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
        // tool_use_blocks 与扁平 tool_calls 是同源信息的两种 wire 形态：codec 在 render
        // 时按目标能力选用，因此 IR 层不主动裁剪 tool_use_blocks（可表达，不算 lossy）。

        if let Some(usage) = &mut ir.usage {
            if !to_caps.supports_reasoning_tokens && usage.reasoning_tokens.is_some() {
                LossyField::drop(from, to, "usage.reasoning_tokens");
                usage.reasoning_tokens = None;
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
        ContentFilter => {
            if matches!(to, WireFormat::AnthropicMessages) {
                LossyField::drop(from, to, "finish_reason.content_filter");
                Stop
            } else {
                ContentFilter
            }
        }
        Other(ref s) => {
            LossyField::drop(from, to, "finish_reason.other");
            // 不携带原始字符串到下游 wire，退化为 Stop
            let _ = s;
            Stop
        }
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
    fn adapt_lossy_matrix_thinking_drop_decisions() {
        use WireFormat::*;
        // (from, to, should_drop_thinking)
        // Pairs match spec §4.2: thinking is lossy when target lacks supports_thinking.
        let cases = [
            (AnthropicMessages, OpenAiChat, true),
            (AnthropicMessages, OpenAiResponses, false),
            (AnthropicMessages, GeminiGenerate, true),
            (OpenAiResponses, OpenAiChat, true),
            (OpenAiResponses, AnthropicMessages, false),
            (OpenAiResponses, GeminiGenerate, true),
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
}
