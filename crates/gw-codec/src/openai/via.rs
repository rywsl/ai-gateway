//! Hand-written `From` / `TryFrom` impls bridging OpenAI wire structs and
//! `gw_core::ir::*`. See `gw-codec-macros/src/lib.rs` for why these are not
//! derived for v1.

use super::wire::{
    OpenAiChoice, OpenAiFunctionCall, OpenAiMessage, OpenAiPromptTokensDetails, OpenAiRequest,
    OpenAiResponse, OpenAiTool, OpenAiToolCall, OpenAiToolFunction, OpenAiUsage,
};
use crate::common::content_to_text;
use gw_core::error::ErrorIR;
use gw_core::ir::{
    ContentIR, FinishReasonIR, MessageIR, MetadataIR, RequestIR, ResponseIR, ToolCallIR,
    ToolChoiceIR, ToolIR, UsageIR,
};
use serde_json::value::RawValue;
use std::borrow::Cow;

impl<'a> From<OpenAiRequest<'a>> for RequestIR<'a> {
    #[inline]
    fn from(src: OpenAiRequest<'a>) -> Self {
        let messages = src
            .messages
            .into_iter()
            .map(openai_msg_to_ir)
            .collect::<Vec<_>>();
        let extensions = if src.extra.is_empty() {
            None
        } else {
            let mut out: std::collections::HashMap<Cow<'a, str>, Box<RawValue>> =
                std::collections::HashMap::with_capacity(src.extra.len());
            for (k, v) in src.extra {
                out.insert(k, v);
            }
            Some(out)
        };
        let tools = src.tools.map(|ts| {
            ts.into_iter()
                .map(|t| {
                    // RawValue is required by ToolIR; use a JSON empty object when
                    // the upstream omitted the parameters schema.
                    let params = t
                        .function
                        .parameters
                        .map(|p| Cow::Owned(p) as Cow<'a, RawValue>)
                        .unwrap_or_else(|| {
                            Cow::Owned(
                                RawValue::from_string("{}".to_owned()).expect("'{}' is valid JSON"),
                            )
                        });
                    ToolIR {
                        name: t.function.name,
                        description: t.function.description,
                        parameters_jsonschema: params,
                    }
                })
                .collect()
        });
        let tool_choice = src.tool_choice.as_deref().and_then(parse_tool_choice);
        let metadata = MetadataIR {
            openai_logit_bias: src.logit_bias.map(|m| {
                m.into_iter()
                    .map(|(k, v)| (Cow::Owned(k), v))
                    .collect::<std::collections::HashMap<Cow<'a, str>, f32>>()
            }),
            ..MetadataIR::default()
        };
        RequestIR {
            model: src.model,
            messages,
            stream: src.stream,
            max_tokens: src.max_tokens,
            temperature: src.temperature,
            top_p: src.top_p,
            stop: src.stop,
            tools,
            tool_choice,
            metadata,
            extensions,
            ..RequestIR::default()
        }
    }
}

impl<'a> TryFrom<&RequestIR<'a>> for OpenAiRequest<'a> {
    type Error = ErrorIR<'static>;

    #[inline]
    fn try_from(ir: &RequestIR<'a>) -> Result<Self, Self::Error> {
        let messages = ir.messages.iter().map(ir_msg_to_openai).collect();
        let logit_bias = ir.metadata.openai_logit_bias.as_ref().map(|m| {
            m.iter()
                .map(|(k, v)| (k.to_string(), *v))
                .collect::<std::collections::HashMap<String, f32>>()
        });
        let extra = ir
            .extensions
            .as_ref()
            .map(|ext| {
                ext.iter()
                    .map(|(k, v)| (Cow::Owned(k.to_string()), v.clone()))
                    .collect::<std::collections::HashMap<Cow<'a, str>, Box<RawValue>>>()
            })
            .unwrap_or_default();
        let tools = ir.tools.as_ref().map(|ts| {
            ts.iter()
                .map(|t| OpenAiTool {
                    ty: Cow::Borrowed("function"),
                    function: OpenAiToolFunction {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: Some(
                            RawValue::from_string(t.parameters_jsonschema.get().to_owned())
                                .expect("ToolIR.parameters_jsonschema must hold valid JSON"),
                        ),
                    },
                })
                .collect()
        });
        let tool_choice = ir.tool_choice.as_ref().map(render_tool_choice);
        Ok(OpenAiRequest {
            model: ir.model.clone(),
            messages,
            stream: ir.stream,
            max_tokens: ir.max_tokens,
            temperature: ir.temperature,
            top_p: ir.top_p,
            stop: ir.stop.clone(),
            logit_bias,
            tools,
            tool_choice,
            extra,
        })
    }
}

#[inline]
fn openai_msg_to_ir(m: OpenAiMessage<'_>) -> MessageIR<'_> {
    match m {
        OpenAiMessage::System { content } => MessageIR::System {
            content: ContentIR::Text(content),
        },
        OpenAiMessage::User { content } => MessageIR::User {
            content: ContentIR::Text(content),
        },
        OpenAiMessage::Assistant {
            content,
            tool_calls,
        } => MessageIR::Assistant {
            content: ContentIR::Text(content.unwrap_or(Cow::Borrowed(""))),
            tool_calls: tool_calls.map(|tcs| {
                tcs.into_iter()
                    .map(|tc| {
                        let args_owned = tc.function.arguments.into_owned();
                        let raw = match RawValue::from_string(args_owned.clone()) {
                            Ok(rv) => rv,
                            Err(_) => {
                                tracing::warn!(
                                    "openai tool_call.arguments not valid JSON; passing through as string literal"
                                );
                                let escaped = serde_json::to_string(&args_owned)
                                    .expect("serde_json::to_string of &String is infallible");
                                RawValue::from_string(escaped).expect(
                                    "serialized JSON string literal must be valid JSON",
                                )
                            }
                        };
                        ToolCallIR {
                            id: tc.id,
                            name: tc.function.name,
                            arguments_json: Cow::Owned(raw),
                        }
                    })
                    .collect()
            }),
        },
        OpenAiMessage::Tool {
            tool_call_id,
            content,
        } => MessageIR::Tool {
            tool_call_id,
            content: ContentIR::Text(content),
        },
    }
}

#[inline]
fn ir_msg_to_openai<'a>(m: &MessageIR<'a>) -> OpenAiMessage<'a> {
    match m {
        MessageIR::System { content } => OpenAiMessage::System {
            content: content_to_text(content),
        },
        MessageIR::User { content } => OpenAiMessage::User {
            content: content_to_text(content),
        },
        MessageIR::Assistant {
            content,
            tool_calls,
        } => OpenAiMessage::Assistant {
            content: Some(content_to_text(content)),
            tool_calls: tool_calls.as_ref().map(|tcs| {
                tcs.iter()
                    .map(|tc| OpenAiToolCall {
                        id: tc.id.clone(),
                        ty: Cow::Borrowed("function"),
                        function: OpenAiFunctionCall {
                            name: tc.name.clone(),
                            arguments: Cow::Owned(tc.arguments_json.get().to_owned()),
                        },
                    })
                    .collect()
            }),
        },
        MessageIR::Tool {
            tool_call_id,
            content,
        } => OpenAiMessage::Tool {
            tool_call_id: tool_call_id.clone(),
            content: content_to_text(content),
        },
    }
}

/// Parse OpenAI's `tool_choice` JSON shape into `ToolChoiceIR`. Returns `None`
/// on unrecognised payloads (lossless behaviour: the original `extra`-free
/// request path simply omits a non-trivially-mappable choice).
fn parse_tool_choice(raw: &RawValue) -> Option<ToolChoiceIR<'static>> {
    let raw = raw.get();
    // First try a bare string ("auto" / "none" / "required").
    if let Ok(s) = serde_json::from_str::<String>(raw) {
        return match s.as_str() {
            "auto" => Some(ToolChoiceIR::Auto),
            "none" => Some(ToolChoiceIR::None),
            "required" => Some(ToolChoiceIR::Required),
            _ => None,
        };
    }
    // Object form: {"type":"function","function":{"name":"X"}}
    #[derive(serde::Deserialize)]
    struct FnChoice {
        function: FnName,
    }
    #[derive(serde::Deserialize)]
    struct FnName {
        name: String,
    }
    if let Ok(obj) = serde_json::from_str::<FnChoice>(raw) {
        return Some(ToolChoiceIR::Function {
            name: Cow::Owned(obj.function.name),
        });
    }
    None
}

/// Render `ToolChoiceIR` back to OpenAI's `tool_choice` JSON shape.
fn render_tool_choice(tc: &ToolChoiceIR<'_>) -> Box<RawValue> {
    let s = match tc {
        ToolChoiceIR::Auto => "\"auto\"".to_owned(),
        ToolChoiceIR::None => "\"none\"".to_owned(),
        ToolChoiceIR::Required => "\"required\"".to_owned(),
        ToolChoiceIR::Function { name } => {
            // serialize the function name through serde_json to be JSON-safe.
            let name_lit = serde_json::to_string(name.as_ref())
                .expect("serde_json::to_string of &str is infallible");
            format!("{{\"type\":\"function\",\"function\":{{\"name\":{name_lit}}}}}")
        }
    };
    RawValue::from_string(s).expect("rendered tool_choice is valid JSON")
}

// ── Response direction ───────────────────────────────────────────────────

impl<'a> TryFrom<&ResponseIR<'a>> for OpenAiResponse<'a> {
    type Error = ErrorIR<'static>;

    fn try_from(ir: &ResponseIR<'a>) -> Result<Self, Self::Error> {
        // Walk messages in reverse to find the last Assistant — multi-assistant
        // responses are vanishingly rare in practice (multi-turn comes back as
        // a single Assistant message at the IR layer) but if one appears, last
        // wins per OpenAI semantics.
        let last_assistant = ir
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m, MessageIR::Assistant { .. }));

        // Extract content + tool_calls from the assistant message, or fall back
        // to empty content + IR's `tool_use_blocks` if no assistant present.
        let (content_text, ir_tool_calls): (Cow<'a, str>, Option<Vec<&ToolCallIR<'a>>>) =
            match last_assistant {
                Some(MessageIR::Assistant {
                    content,
                    tool_calls,
                }) => (
                    content_to_text(content),
                    tool_calls.as_ref().map(|v| v.iter().collect::<Vec<_>>()),
                ),
                _ => (Cow::Borrowed(""), None),
            };

        // If the message-level tool_calls is empty but the IR carries
        // tool_use_blocks (Anthropic-shape source), synthesise OpenAI tool_calls
        // from them.
        let wire_tool_calls: Option<Vec<OpenAiToolCall<'a>>> = if let Some(tcs) = ir_tool_calls {
            Some(
                tcs.into_iter()
                    .map(|tc| OpenAiToolCall {
                        id: tc.id.clone(),
                        ty: Cow::Borrowed("function"),
                        function: OpenAiFunctionCall {
                            name: tc.name.clone(),
                            arguments: Cow::Owned(tc.arguments_json.get().to_owned()),
                        },
                    })
                    .collect(),
            )
        } else {
            ir.tool_use_blocks.as_ref().map(|blocks| {
                blocks
                    .iter()
                    .map(|b| OpenAiToolCall {
                        id: b.id.clone(),
                        ty: Cow::Borrowed("function"),
                        function: OpenAiFunctionCall {
                            name: b.name.clone(),
                            arguments: Cow::Owned(b.input.get().to_owned()),
                        },
                    })
                    .collect()
            })
        };

        let message = OpenAiMessage::Assistant {
            content: if content_text.is_empty() && wire_tool_calls.is_some() {
                // OpenAI convention: omit `content` when the assistant returns
                // tool_calls only.
                None
            } else {
                Some(content_text)
            },
            tool_calls: wire_tool_calls,
        };

        let finish_reason = ir.finish_reason.as_ref().map(|fr| match fr {
            FinishReasonIR::Stop => Cow::Borrowed("stop"),
            FinishReasonIR::Length => Cow::Borrowed("length"),
            FinishReasonIR::ToolCalls => Cow::Borrowed("tool_calls"),
            FinishReasonIR::ContentFilter => Cow::Borrowed("content_filter"),
            // cross_render::map_finish_reason degrades Other → Stop before
            // render_response is reached; defensively map any survivor.
            FinishReasonIR::Other(_) => Cow::Borrowed("stop"),
        });

        let choice = OpenAiChoice {
            index: 0,
            message,
            finish_reason,
        };

        let usage = ir.usage.as_ref().map(|u| OpenAiUsage {
            prompt_tokens: u.input_tokens,
            completion_tokens: u.output_tokens,
            total_tokens: u.input_tokens.saturating_add(u.output_tokens),
            prompt_tokens_details: u
                .cached_tokens
                .map(|c| OpenAiPromptTokensDetails { cached_tokens: c }),
            // OpenAI Chat does not expose reasoning_tokens — adapt_lossy
            // strips them before render. Defensively leave None either way.
            completion_tokens_details: None,
        });

        Ok(OpenAiResponse {
            id: ir.id.clone(),
            object: Cow::Borrowed("chat.completion"),
            created: 0,
            model: ir.model.clone(),
            choices: vec![choice],
            usage,
            extra: std::collections::HashMap::new(),
        })
    }
}

impl<'a> From<OpenAiResponse<'a>> for ResponseIR<'a> {
    fn from(w: OpenAiResponse<'a>) -> Self {
        let first = w.choices.into_iter().next();
        let (messages, finish_reason) = match first {
            Some(choice) => {
                let fr = choice.finish_reason.as_deref().map(|s| match s {
                    "stop" => FinishReasonIR::Stop,
                    "length" => FinishReasonIR::Length,
                    "tool_calls" => FinishReasonIR::ToolCalls,
                    "content_filter" => FinishReasonIR::ContentFilter,
                    other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
                });
                let msg = openai_msg_to_ir(choice.message);
                (vec![msg], fr)
            }
            None => (Vec::new(), None),
        };

        let usage = w.usage.map(|u| UsageIR {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_tokens: u.prompt_tokens_details.map(|d| d.cached_tokens),
            reasoning_tokens: u.completion_tokens_details.map(|d| d.reasoning_tokens),
        });

        ResponseIR {
            id: w.id,
            model: w.model,
            messages,
            finish_reason,
            usage,
            metadata: MetadataIR::default(),
            // OpenAI Chat does not carry thinking / reasoning / tool_use_blocks
            // on the wire — these live exclusively on the IR side for
            // cross-protocol render from richer sources.
            thinking: None,
            reasoning: None,
            tool_use_blocks: None,
        }
    }
}

#[cfg(test)]
mod tool_choice_tests {
    //! Round-trip coverage for `parse_tool_choice` / `render_tool_choice`.
    //!
    //! T4-A introduced these as the single hand-rolled bit of the request-side
    //! `From<OpenAiRequest> -> RequestIR` mapping (everything else is mechanical
    //! Cow/RawValue plumbing). The helpers accept both the string-shorthand and
    //! the `{"type":"function","function":{"name":"..."}}` object form; these
    //! tests guard against silent regressions in either branch.
    use super::*;
    use serde_json::value::to_raw_value;

    fn raw(v: serde_json::Value) -> Box<RawValue> {
        to_raw_value(&v).expect("test JSON is valid")
    }

    #[test]
    fn tool_choice_auto_round_trip() {
        let wire_in = raw(serde_json::json!("auto"));
        let ir = parse_tool_choice(&wire_in).expect("parses Auto");
        assert!(matches!(ir, ToolChoiceIR::Auto));
        let wire_out = render_tool_choice(&ir);
        assert_eq!(wire_out.get(), "\"auto\"");
    }

    #[test]
    fn tool_choice_none_round_trip() {
        let wire_in = raw(serde_json::json!("none"));
        let ir = parse_tool_choice(&wire_in).expect("parses None");
        assert!(matches!(ir, ToolChoiceIR::None));
        let wire_out = render_tool_choice(&ir);
        assert_eq!(wire_out.get(), "\"none\"");
    }

    #[test]
    fn tool_choice_required_round_trip() {
        let wire_in = raw(serde_json::json!("required"));
        let ir = parse_tool_choice(&wire_in).expect("parses Required");
        assert!(matches!(ir, ToolChoiceIR::Required));
        let wire_out = render_tool_choice(&ir);
        assert_eq!(wire_out.get(), "\"required\"");
    }

    #[test]
    fn tool_choice_function_object_round_trip() {
        let wire_in = raw(serde_json::json!({
            "type": "function",
            "function": { "name": "get_weather" }
        }));
        let ir = parse_tool_choice(&wire_in).expect("parses Function");
        match &ir {
            ToolChoiceIR::Function { name } => assert_eq!(name.as_ref(), "get_weather"),
            other => panic!("expected ToolChoiceIR::Function, got {other:?}"),
        }
        let wire_out = render_tool_choice(&ir);
        let s = wire_out.get();
        assert!(s.contains("\"type\":\"function\""), "rendered: {s}");
        assert!(s.contains("\"name\":\"get_weather\""), "rendered: {s}");
    }
}
