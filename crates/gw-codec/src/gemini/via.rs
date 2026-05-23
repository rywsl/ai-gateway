//! Hand-written `From` / `TryFrom` bridging Gemini wire structs and the IR.
//!
//! Request direction lowers `contents[]` (each holding `parts[]`) into a flat
//! `Vec<MessageIR>` and handles the `systemInstruction` separately. Parts are
//! the untagged-enum shape introduced in T4-D: text becomes content text,
//! `functionCall` projects to `tool_calls`, `functionResponse` fans out into
//! a `MessageIR::Tool`.
//!
//! Response direction adds `From<GeminiResponse> -> ResponseIR` (parse) and
//! `TryFrom<&ResponseIR> -> GeminiResponse` (render). Gemini emits a single
//! candidate per response in the gateway's surfaced traffic; multi-candidate
//! shapes (`candidateCount > 1`) are not supported by the IR — the renderer
//! emits one candidate from the last Assistant message and the parser reads
//! only `candidates[0]`.

use super::wire::{
    GeminiCandidate, GeminiContent, GeminiFunctionCall, GeminiFunctionResponse,
    GeminiGenerationConfig, GeminiPart, GeminiRequest, GeminiResponse, GeminiSystemInstruction,
    GeminiUsage,
};
use crate::common::content_to_text;
use gw_core::error::ErrorIR;
use gw_core::ir::{
    ContentIR, FinishReasonIR, MessageIR, RequestIR, ResponseIR, ToolCallIR, UsageIR,
};
use serde_json::value::RawValue;
use std::borrow::Cow;

// ─── Request direction ────────────────────────────────────────────────────

impl<'a> From<GeminiRequest<'a>> for RequestIR<'a> {
    #[inline]
    fn from(src: GeminiRequest<'a>) -> Self {
        let mut messages: Vec<MessageIR<'a>> = Vec::new();
        if let Some(sys) = src.system_instruction {
            let text = join_text_parts(&sys.parts);
            messages.push(MessageIR::System {
                content: ContentIR::Text(Cow::Owned(text)),
            });
        }
        for c in src.contents {
            lower_content(c, &mut messages);
        }
        let gc = src.generation_config.unwrap_or_default();
        RequestIR {
            model: src.model,
            messages,
            max_tokens: gc.max_output_tokens,
            temperature: gc.temperature,
            ..RequestIR::default()
        }
    }
}

impl<'a> TryFrom<&RequestIR<'a>> for GeminiRequest<'a> {
    type Error = ErrorIR<'static>;

    #[inline]
    fn try_from(ir: &RequestIR<'a>) -> Result<Self, Self::Error> {
        let mut system_instruction: Option<GeminiSystemInstruction<'a>> = None;
        let mut contents: Vec<GeminiContent<'a>> = Vec::with_capacity(ir.messages.len());
        for m in &ir.messages {
            match m {
                MessageIR::System { content } => {
                    if system_instruction.is_none() {
                        system_instruction = Some(GeminiSystemInstruction {
                            parts: vec![GeminiPart::Text {
                                text: content_to_text(content),
                            }],
                        });
                    }
                }
                MessageIR::User { content } => {
                    contents.push(GeminiContent {
                        role: Some(Cow::Borrowed("user")),
                        parts: vec![GeminiPart::Text {
                            text: content_to_text(content),
                        }],
                    });
                }
                MessageIR::Tool {
                    tool_call_id: _,
                    content,
                } => {
                    // Tool results lower to a `functionResponse` part. Gemini
                    // does not carry a per-call id — we drop `tool_call_id`.
                    // Body text → `response.content` so consumers see the
                    // free-form result string.
                    let body = content_to_text(content).into_owned();
                    contents.push(GeminiContent {
                        role: Some(Cow::Borrowed("user")),
                        parts: vec![GeminiPart::FunctionResponse {
                            function_response: GeminiFunctionResponse {
                                name: Cow::Borrowed(""),
                                response: serde_json::json!({ "content": body }),
                            },
                        }],
                    });
                }
                MessageIR::Assistant {
                    content,
                    tool_calls,
                } => {
                    let mut parts: Vec<GeminiPart<'a>> = Vec::new();
                    let text = content_to_text(content);
                    if !text.is_empty() {
                        parts.push(GeminiPart::Text { text });
                    }
                    if let Some(tcs) = tool_calls {
                        for tc in tcs {
                            parts.push(GeminiPart::FunctionCall {
                                function_call: GeminiFunctionCall {
                                    name: tc.name.clone(),
                                    args: raw_to_value(tc.arguments_json.as_ref()),
                                },
                            });
                        }
                    }
                    if parts.is_empty() {
                        // Gemini rejects an empty `parts` array; emit a
                        // single empty-text part so the schema is preserved.
                        parts.push(GeminiPart::Text {
                            text: Cow::Borrowed(""),
                        });
                    }
                    contents.push(GeminiContent {
                        role: Some(Cow::Borrowed("model")),
                        parts,
                    });
                }
            }
        }
        let generation_config = if ir.max_tokens.is_some() || ir.temperature.is_some() {
            Some(GeminiGenerationConfig {
                max_output_tokens: ir.max_tokens,
                temperature: ir.temperature,
            })
        } else {
            None
        };
        Ok(GeminiRequest {
            model: ir.model.clone(),
            contents,
            system_instruction,
            generation_config,
        })
    }
}

fn lower_content<'a>(c: GeminiContent<'a>, out: &mut Vec<MessageIR<'a>>) {
    let role = c.role.as_deref().unwrap_or("model");
    match role {
        "model" | "assistant" => {
            let mut text_buf = String::new();
            let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();
            for p in c.parts {
                match p {
                    GeminiPart::Text { text } => text_buf.push_str(text.as_ref()),
                    GeminiPart::FunctionCall { function_call } => {
                        let raw = value_to_raw(function_call.args);
                        tool_calls.push(ToolCallIR {
                            id: Cow::Borrowed(""),
                            name: function_call.name,
                            arguments_json: Cow::Owned(raw),
                        });
                    }
                    GeminiPart::FunctionResponse { .. }
                    | GeminiPart::InlineData { .. }
                    | GeminiPart::FileData { .. } => {
                        // Not valid on an Assistant message in practice; ignore.
                    }
                }
            }
            out.push(MessageIR::Assistant {
                content: ContentIR::Text(Cow::Owned(text_buf)),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
            });
        }
        _ => {
            // "user" or unknown — collect text parts and fan out
            // functionResponse parts as discrete Tool messages so downstream
            // codecs (OpenAI / Anthropic) see them as separate messages.
            let mut text_buf = String::new();
            let mut function_responses: Vec<(Cow<'a, str>, String)> = Vec::new();
            for p in c.parts {
                match p {
                    GeminiPart::Text { text } => text_buf.push_str(text.as_ref()),
                    GeminiPart::FunctionResponse { function_response } => {
                        let body = match &function_response.response {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        function_responses.push((function_response.name, body));
                    }
                    GeminiPart::FunctionCall { .. }
                    | GeminiPart::InlineData { .. }
                    | GeminiPart::FileData { .. } => {
                        // FunctionCall on user role: silently ignore.
                        // InlineData / FileData: v2 doesn't surface multimodal
                        // through the User path of the IR; drop.
                    }
                }
            }
            if !text_buf.is_empty() || function_responses.is_empty() {
                out.push(MessageIR::User {
                    content: ContentIR::Text(Cow::Owned(text_buf)),
                });
            }
            for (name, body) in function_responses {
                out.push(MessageIR::Tool {
                    // Gemini doesn't ship a call id; use the function name
                    // as a best-effort correlation handle.
                    tool_call_id: name,
                    content: ContentIR::Text(Cow::Owned(body)),
                });
            }
        }
    }
}

#[inline]
fn join_text_parts(parts: &[GeminiPart<'_>]) -> String {
    let mut buf = String::new();
    for p in parts {
        if let GeminiPart::Text { text } = p {
            buf.push_str(text.as_ref());
        }
    }
    buf
}

// ─── Response direction ───────────────────────────────────────────────────

impl<'a> From<GeminiResponse<'a>> for ResponseIR<'a> {
    fn from(w: GeminiResponse<'a>) -> Self {
        // Gemini surfaces a single candidate in the gateway's traffic;
        // multi-candidate shapes are not modelled in the IR.
        let mut assistant_text = String::new();
        let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();
        let mut tool_messages: Vec<MessageIR<'a>> = Vec::new();
        let mut finish_reason: Option<FinishReasonIR> = None;

        if let Some(candidate) = w.candidates.into_iter().next() {
            if let Some(content) = candidate.content {
                for p in content.parts {
                    match p {
                        GeminiPart::Text { text } => assistant_text.push_str(text.as_ref()),
                        GeminiPart::FunctionCall { function_call } => {
                            let raw = value_to_raw(function_call.args);
                            tool_calls.push(ToolCallIR {
                                id: Cow::Borrowed(""),
                                name: function_call.name,
                                arguments_json: Cow::Owned(raw),
                            });
                        }
                        GeminiPart::FunctionResponse { function_response } => {
                            // A `functionResponse` block on the model side is
                            // unusual but spec-legal; surface as a Tool
                            // message for cross-protocol fidelity.
                            let body = match &function_response.response {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            tool_messages.push(MessageIR::Tool {
                                tool_call_id: function_response.name,
                                content: ContentIR::Text(Cow::Owned(body)),
                            });
                        }
                        GeminiPart::InlineData { .. } | GeminiPart::FileData { .. } => {
                            // No IR slot for response-side multimodal output yet.
                        }
                    }
                }
            }
            finish_reason = candidate.finish_reason.as_deref().map(map_finish_reason_in);
        }

        let mut messages: Vec<MessageIR<'a>> = Vec::with_capacity(1 + tool_messages.len());
        messages.push(MessageIR::Assistant {
            content: ContentIR::Text(Cow::Owned(assistant_text)),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        });
        messages.extend(tool_messages);

        let usage = w.usage_metadata.map(|u| UsageIR {
            input_tokens: u.prompt_token_count,
            output_tokens: u.candidates_token_count,
            cached_tokens: u.cached_content_token_count,
            reasoning_tokens: None,
        });

        // ADR-G5-1: long-running-operation responses set `name:
        // "operations/<uuid>"`; non-LRO chat responses leave it unset.
        let id = w.name.unwrap_or(Cow::Borrowed(""));

        ResponseIR {
            id,
            model: w.model_version.unwrap_or(Cow::Borrowed("")),
            messages,
            finish_reason,
            usage,
            metadata: Default::default(),
            thinking: None,
            reasoning: None,
            tool_use_blocks: None,
        }
    }
}

impl<'a> TryFrom<&ResponseIR<'a>> for GeminiResponse<'a> {
    type Error = ErrorIR<'static>;

    fn try_from(ir: &ResponseIR<'a>) -> Result<Self, Self::Error> {
        // Build a single candidate from the last Assistant message; Gemini's
        // surface emits one candidate by default and our IR doesn't model
        // candidateCount > 1.
        let last_assistant = ir
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m, MessageIR::Assistant { .. }));

        let mut parts: Vec<GeminiPart<'a>> = Vec::new();
        if let Some(MessageIR::Assistant {
            content,
            tool_calls,
        }) = last_assistant
        {
            let text = content_to_text(content);
            if !text.is_empty() {
                parts.push(GeminiPart::Text { text });
            }
            if let Some(tcs) = tool_calls {
                for tc in tcs {
                    parts.push(GeminiPart::FunctionCall {
                        function_call: GeminiFunctionCall {
                            name: tc.name.clone(),
                            args: raw_to_value(tc.arguments_json.as_ref()),
                        },
                    });
                }
            }
        }
        if parts.is_empty() {
            parts.push(GeminiPart::Text {
                text: Cow::Borrowed(""),
            });
        }

        let finish_reason = ir
            .finish_reason
            .as_ref()
            .map(|fr| Cow::Borrowed(map_finish_reason_out(fr)));

        let candidate = GeminiCandidate {
            content: Some(GeminiContent {
                role: Some(Cow::Borrowed("model")),
                parts,
            }),
            finish_reason,
            index: Some(0),
        };

        let usage_metadata = ir.usage.as_ref().map(|u| GeminiUsage {
            prompt_token_count: u.input_tokens,
            candidates_token_count: u.output_tokens,
            cached_content_token_count: u.cached_tokens,
            total_token_count: Some(u.input_tokens.saturating_add(u.output_tokens)),
        });

        // Surface the IR's id only if it looks like an operations/<uuid>
        // marker — ordinary chat responses have no top-level `name`.
        let name = if ir.id.starts_with("operations/") {
            Some(ir.id.clone())
        } else {
            None
        };

        Ok(GeminiResponse {
            candidates: vec![candidate],
            usage_metadata,
            model_version: if ir.model.is_empty() {
                None
            } else {
                Some(ir.model.clone())
            },
            name,
        })
    }
}

/// Map Gemini's `finishReason` enum to `FinishReasonIR`.
pub(crate) fn map_finish_reason_in(s: &str) -> FinishReasonIR {
    match s {
        "STOP" => FinishReasonIR::Stop,
        "MAX_TOKENS" => FinishReasonIR::Length,
        "SAFETY" => FinishReasonIR::ContentFilter,
        other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
    }
}

/// Map `FinishReasonIR` to Gemini's wire enum. Returns a static string so
/// the wire struct can borrow it.
pub(crate) fn map_finish_reason_out(fr: &FinishReasonIR) -> &'static str {
    match fr {
        FinishReasonIR::Stop => "STOP",
        FinishReasonIR::Length => "MAX_TOKENS",
        // Gemini has no dedicated tool-call finish reason; tool use is
        // signalled by the presence of a functionCall part with `STOP`.
        FinishReasonIR::ToolCalls => "STOP",
        FinishReasonIR::ContentFilter => "SAFETY",
        // cross_render::map_finish_reason degrades Other → Stop before
        // render_response runs; defensively map any survivor to OTHER.
        FinishReasonIR::Other(_) => "OTHER",
    }
}

/// Convert an opaque `serde_json::Value` into a `RawValue` for the IR's
/// `Cow<'a, RawValue>` slots. Round-trip is lossy only on whitespace / key
/// ordering, which the IR contract tolerates.
fn value_to_raw(v: serde_json::Value) -> Box<RawValue> {
    let s = serde_json::to_string(&v).expect("serializing serde_json::Value is infallible");
    RawValue::from_string(s).expect("serialized JSON value must be a valid RawValue")
}

/// Inverse of `value_to_raw` — read an IR-side `RawValue` back into a
/// `Value` for re-rendering through the Gemini wire enum.
fn raw_to_value(rv: &RawValue) -> serde_json::Value {
    serde_json::from_str(rv.get()).unwrap_or(serde_json::Value::Null)
}
