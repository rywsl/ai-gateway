//! Hand-written `From` / `TryFrom` impls bridging OpenAI wire structs and
//! `gw_core::ir::*`. See `gw-codec-macros/src/lib.rs` for why these are not
//! derived for v1.

use super::wire::{OpenAiFunctionCall, OpenAiMessage, OpenAiRequest, OpenAiToolCall};
use crate::common::content_to_text;
use gw_core::error::ErrorIR;
use gw_core::ir::{ContentIR, MessageIR, MetadataIR, RequestIR, ToolCallIR};
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
            // Owned-key map; the keys are already owned `String`.
            let mut out: std::collections::HashMap<Cow<'a, str>, Box<RawValue>> =
                std::collections::HashMap::with_capacity(src.extra.len());
            for (k, v) in src.extra {
                out.insert(Cow::Owned(k), v);
            }
            Some(out)
        };
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
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect::<std::collections::HashMap<String, Box<RawValue>>>()
            })
            .unwrap_or_default();
        Ok(OpenAiRequest {
            model: ir.model.clone(),
            messages,
            stream: ir.stream,
            max_tokens: ir.max_tokens,
            temperature: ir.temperature,
            top_p: ir.top_p,
            stop: ir.stop.clone(),
            logit_bias,
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
                        // Arguments are a JSON string — wrap them in RawValue
                        // (owned, since the source `Cow` may have been borrowed
                        // from the request body).
                        let args_owned = tc.function.arguments.into_owned();
                        let raw = RawValue::from_string(args_owned).unwrap_or_else(|_| {
                            // Fallback: RawValue::from_string requires the
                            // string to be valid JSON. If the upstream sent a
                            // non-JSON arguments value, wrap it as a JSON
                            // string literal.
                            RawValue::from_string("\"\"".to_owned()).unwrap()
                        });
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
