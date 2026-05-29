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
    GeminiCandidate, GeminiContent, GeminiExecutableCode, GeminiFunctionCall,
    GeminiFunctionResponse, GeminiGenerationConfig, GeminiPart, GeminiRequest, GeminiResponse,
    GeminiSystemInstruction, GeminiThinkingConfig, GeminiUsage,
};
use crate::common::{content_to_text, internal_err};
use gw_core::error::ErrorIR;
use gw_core::ir::{
    ContentIR, FinishReasonIR, MessageIR, RequestIR, ResponseFormatIR, ResponseIR,
    ResponseThinkingIR, ServerToolBlockIR, ThinkingBlockIR, ToolCallIR, UsageIR,
};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;

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
        let reasoning_config = gc.thinking_config.map(|tc| gw_core::ir::ReasoningConfig {
            effort: tc.thinking_level.map(|l| Cow::Owned(l.into_owned())),
            budget_tokens: tc.thinking_budget,
        });
        RequestIR {
            model: src.model,
            messages,
            max_tokens: gc.max_output_tokens,
            temperature: gc.temperature,
            top_p: gc.top_p,
            top_k: gc.top_k,
            stop: gc.stop_sequences,
            seed: gc.seed,
            presence_penalty: gc.presence_penalty,
            frequency_penalty: gc.frequency_penalty,
            reasoning_config,
            response_format: gemini_response_format_to_ir(
                gc.response_mime_type.as_deref(),
                gc.response_schema,
            ),
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
        // Audit 2026-05-30 §3.5: Gemini's functionResponse REQUIRES a `name` (the
        // originating function). Map each prior tool_call id -> its function name
        // so a MessageIR::Tool result can correlate back, instead of emitting an
        // empty name. One pass over the messages; later Tool arms look up by id.
        let tool_id_to_name: HashMap<Cow<'a, str>, Cow<'a, str>> = ir
            .messages
            .iter()
            .filter_map(|m| match m {
                MessageIR::Assistant {
                    tool_calls: Some(tcs),
                    ..
                } => Some(tcs.iter()),
                _ => None,
            })
            .flatten()
            .map(|tc| (tc.id.clone(), tc.name.clone()))
            .collect();
        for m in &ir.messages {
            match m {
                MessageIR::System { content } => {
                    if system_instruction.is_none() {
                        system_instruction = Some(GeminiSystemInstruction {
                            parts: vec![GeminiPart::Text {
                                text: content_to_text(content),
                                thought: None,
                                thought_signature: None,
                            }],
                        });
                    }
                }
                MessageIR::Developer { content } => {
                    let text = content_to_text(content);
                    if let Some(system_instruction) = system_instruction.as_mut() {
                        system_instruction.parts.push(GeminiPart::Text {
                            text,
                            thought: None,
                            thought_signature: None,
                        });
                    } else {
                        system_instruction = Some(GeminiSystemInstruction {
                            parts: vec![GeminiPart::Text {
                                text,
                                thought: None,
                                thought_signature: None,
                            }],
                        });
                    }
                }
                MessageIR::User { content } => {
                    contents.push(GeminiContent {
                        role: Some(Cow::Borrowed("user")),
                        parts: vec![GeminiPart::Text {
                            text: content_to_text(content),
                            thought: None,
                            thought_signature: None,
                        }],
                    });
                }
                MessageIR::Tool {
                    tool_call_id,
                    content,
                } => {
                    // Tool results lower to a `functionResponse` part. Gemini's
                    // functionResponse.name is required and must be the
                    // originating function name — resolve it from the recorded
                    // tool_call id (audit 2026-05-30 §3.5). Fall back to the id
                    // itself only when no prior call is found (best-effort, e.g.
                    // a truncated history), rather than emitting an empty name.
                    let name: Cow<'a, str> = tool_id_to_name
                        .get(tool_call_id.as_ref())
                        .cloned()
                        .unwrap_or_else(|| tool_call_id.clone());
                    let body = content_to_text(content).into_owned();
                    contents.push(GeminiContent {
                        role: Some(Cow::Borrowed("user")),
                        parts: vec![GeminiPart::FunctionResponse {
                            function_response: GeminiFunctionResponse {
                                name,
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
                        parts.push(GeminiPart::Text {
                            text,
                            thought: None,
                            thought_signature: None,
                        });
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
                            thought: None,
                            thought_signature: None,
                        });
                    }
                    contents.push(GeminiContent {
                        role: Some(Cow::Borrowed("model")),
                        parts,
                    });
                }
            }
        }
        let thinking_config = ir.reasoning_config.as_ref().map(|rc| GeminiThinkingConfig {
            thinking_budget: rc.budget_tokens,
            thinking_level: rc
                .effort
                .as_ref()
                .map(|e| Cow::Owned(e.clone().into_owned())),
        });
        let (response_mime_type, response_schema) =
            ir_response_format_to_gemini(ir.response_format.as_ref());
        let has_gen_config = ir.max_tokens.is_some()
            || ir.temperature.is_some()
            || ir.top_p.is_some()
            || ir.top_k.is_some()
            || ir.stop.is_some()
            || ir.seed.is_some()
            || ir.presence_penalty.is_some()
            || ir.frequency_penalty.is_some()
            || thinking_config.is_some()
            || response_mime_type.is_some()
            || response_schema.is_some();
        let generation_config = if has_gen_config {
            Some(GeminiGenerationConfig {
                max_output_tokens: ir.max_tokens,
                temperature: ir.temperature,
                top_p: ir.top_p,
                top_k: ir.top_k,
                stop_sequences: ir.stop.clone(),
                candidate_count: None,
                presence_penalty: ir.presence_penalty,
                frequency_penalty: ir.frequency_penalty,
                seed: ir.seed,
                response_mime_type,
                response_schema,
                thinking_config,
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

fn gemini_response_format_to_ir(
    response_mime_type: Option<&str>,
    response_schema: Option<serde_json::Value>,
) -> Option<ResponseFormatIR> {
    match (response_mime_type, response_schema) {
        (_, Some(schema)) => Some(ResponseFormatIR::JsonSchema(value_to_raw(schema))),
        (Some("application/json"), None) => Some(ResponseFormatIR::JsonObject),
        (Some("text/plain"), None) => Some(ResponseFormatIR::Text),
        _ => None,
    }
}

fn ir_response_format_to_gemini(
    response_format: Option<&ResponseFormatIR>,
) -> (Option<Cow<'static, str>>, Option<serde_json::Value>) {
    match response_format {
        Some(ResponseFormatIR::Text) => (Some(Cow::Borrowed("text/plain")), None),
        Some(ResponseFormatIR::JsonObject) => (Some(Cow::Borrowed("application/json")), None),
        Some(ResponseFormatIR::JsonSchema(raw)) => (
            Some(Cow::Borrowed("application/json")),
            Some(gemini_response_schema_value(raw.as_ref())),
        ),
        None => (None, None),
    }
}

fn gemini_response_schema_value(raw: &RawValue) -> serde_json::Value {
    let value = raw_to_value(raw);
    value.get("schema").cloned().unwrap_or(value)
}

fn lower_content<'a>(c: GeminiContent<'a>, out: &mut Vec<MessageIR<'a>>) {
    let role = c.role.as_deref().unwrap_or("model");
    match role {
        "model" | "assistant" => {
            let mut text_buf = String::new();
            let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();
            for p in c.parts {
                match p {
                    GeminiPart::Text { text, .. } => text_buf.push_str(text.as_ref()),
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
                    | GeminiPart::FileData { .. }
                    | GeminiPart::ExecutableCode { .. }
                    | GeminiPart::CodeExecutionResult { .. } => {}
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
                    GeminiPart::Text { text, .. } => text_buf.push_str(text.as_ref()),
                    GeminiPart::FunctionResponse { function_response } => {
                        let body = match &function_response.response {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        function_responses.push((function_response.name, body));
                    }
                    GeminiPart::FunctionCall { .. }
                    | GeminiPart::InlineData { .. }
                    | GeminiPart::FileData { .. }
                    | GeminiPart::ExecutableCode { .. }
                    | GeminiPart::CodeExecutionResult { .. } => {}
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
        if let GeminiPart::Text { text, .. } = p {
            buf.push_str(text.as_ref());
        }
    }
    buf
}

// ─── Response direction ───────────────────────────────────────────────────

impl<'a> From<GeminiResponse<'a>> for ResponseIR<'a> {
    fn from(w: GeminiResponse<'a>) -> Self {
        let mut assistant_text = String::new();
        let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();
        let mut tool_messages: Vec<MessageIR<'a>> = Vec::new();
        let mut finish_reason: Option<FinishReasonIR> = None;
        let mut thinking_blocks: Vec<ThinkingBlockIR<'a>> = Vec::new();
        let mut server_tool_blocks: Vec<ServerToolBlockIR<'a>> = Vec::new();
        let mut grounding_metadata_val: Option<serde_json::Value> = None;

        if let Some(candidate) = w.candidates.into_iter().next() {
            grounding_metadata_val = candidate.grounding_metadata;
            if let Some(content) = candidate.content {
                for p in content.parts {
                    match p {
                        GeminiPart::Text {
                            text,
                            thought: Some(true),
                            thought_signature,
                        } => {
                            thinking_blocks.push(ThinkingBlockIR {
                                text,
                                signature: thought_signature,
                            });
                        }
                        GeminiPart::Text { text, .. } => {
                            assistant_text.push_str(text.as_ref());
                        }
                        GeminiPart::FunctionCall { function_call } => {
                            let raw = value_to_raw(function_call.args);
                            tool_calls.push(ToolCallIR {
                                id: Cow::Borrowed(""),
                                name: function_call.name,
                                arguments_json: Cow::Owned(raw),
                            });
                        }
                        GeminiPart::FunctionResponse { function_response } => {
                            let body = match &function_response.response {
                                serde_json::Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            tool_messages.push(MessageIR::Tool {
                                tool_call_id: function_response.name,
                                content: ContentIR::Text(Cow::Owned(body)),
                            });
                        }
                        GeminiPart::ExecutableCode { executable_code } => {
                            let code_json = serde_json::json!(executable_code.code.as_ref());
                            let mut extra = serde_json::Map::new();
                            extra.insert(
                                "language".to_owned(),
                                serde_json::Value::String(executable_code.language.into_owned()),
                            );
                            server_tool_blocks.push(ServerToolBlockIR {
                                kind: Cow::Borrowed("executable_code"),
                                id: Cow::Borrowed(""),
                                name: Cow::Borrowed(""),
                                input: value_to_raw(code_json),
                                extra,
                            });
                        }
                        GeminiPart::CodeExecutionResult {
                            code_execution_result,
                        } => {
                            let output_json =
                                serde_json::json!(code_execution_result.output.as_ref());
                            let mut extra = serde_json::Map::new();
                            extra.insert(
                                "outcome".to_owned(),
                                serde_json::Value::String(
                                    code_execution_result.outcome.into_owned(),
                                ),
                            );
                            server_tool_blocks.push(ServerToolBlockIR {
                                kind: Cow::Borrowed("code_execution_result"),
                                id: Cow::Borrowed(""),
                                name: Cow::Borrowed(""),
                                input: value_to_raw(output_json),
                                extra,
                            });
                        }
                        GeminiPart::InlineData { .. } | GeminiPart::FileData { .. } => {}
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
            thoughts_tokens: u.thoughts_token_count,
            ..Default::default()
        });

        // ADR-G5-1: long-running-operation responses set `name:
        // "operations/<uuid>"`; non-LRO chat responses leave it unset.
        let id = w.name.unwrap_or(Cow::Borrowed(""));

        let thinking = if thinking_blocks.is_empty() {
            None
        } else {
            Some(ResponseThinkingIR {
                blocks: thinking_blocks,
            })
        };

        let mut metadata = gw_core::ir::MetadataIR::default();
        if let Some(gm) = grounding_metadata_val {
            let ext = metadata.extensions.get_or_insert_with(serde_json::Map::new);
            ext.insert("grounding_metadata".to_owned(), gm);
        }

        ResponseIR {
            id,
            model: w.model_version.unwrap_or(Cow::Borrowed("")),
            messages,
            finish_reason,
            usage,
            metadata,
            thinking,
            reasoning: None,
            tool_use_blocks: None,
            server_tool_blocks: if server_tool_blocks.is_empty() {
                None
            } else {
                Some(server_tool_blocks)
            },
            ..Default::default()
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

        // Emit thinking blocks as Text parts with thought=true, before content.
        if let Some(thinking) = &ir.thinking {
            for block in &thinking.blocks {
                parts.push(GeminiPart::Text {
                    text: block.text.clone(),
                    thought: Some(true),
                    thought_signature: block.signature.clone(),
                });
            }
        }

        if let Some(MessageIR::Assistant {
            content: ContentIR::Text(content),
            tool_calls,
        }) = last_assistant
        {
            let text = content.clone();
            if !text.is_empty() {
                parts.push(GeminiPart::Text {
                    text,
                    thought: None,
                    thought_signature: None,
                });
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
        } else if last_assistant.is_some() {
            return Err(internal_err(
                "expected ContentIR::Text, got non-text content",
            ));
        } else if !ir.messages.is_empty() {
            return Err(internal_err(
                "expected Assistant, got non-assistant message",
            ));
        }
        if parts.is_empty() {
            parts.push(GeminiPart::Text {
                text: Cow::Borrowed(""),
                thought: None,
                thought_signature: None,
            });
        }

        // Render server tool blocks as ExecutableCode / CodeExecutionResult parts.
        if let Some(stbs) = &ir.server_tool_blocks {
            for stb in stbs {
                match stb.kind.as_ref() {
                    "executable_code" => {
                        let code = raw_to_value(&stb.input);
                        let code_str = match code {
                            serde_json::Value::String(s) => Cow::Owned(s),
                            _ => Cow::Owned(code.to_string()),
                        };
                        let language = stb
                            .extra
                            .get("language")
                            .and_then(|v| v.as_str())
                            .unwrap_or("PYTHON");
                        parts.push(GeminiPart::ExecutableCode {
                            executable_code: GeminiExecutableCode {
                                code: code_str,
                                language: Cow::Owned(language.to_owned()),
                            },
                        });
                    }
                    "code_execution_result" => {
                        let output = raw_to_value(&stb.input);
                        let output_str = match output {
                            serde_json::Value::String(s) => Cow::Owned(s),
                            _ => Cow::Owned(output.to_string()),
                        };
                        let outcome = stb
                            .extra
                            .get("outcome")
                            .and_then(|v| v.as_str())
                            .unwrap_or("OUTCOME_OK");
                        parts.push(GeminiPart::CodeExecutionResult {
                            code_execution_result: super::wire::GeminiCodeExecutionResult {
                                outcome: Cow::Owned(outcome.to_owned()),
                                output: output_str,
                            },
                        });
                    }
                    _ => {}
                }
            }
        }

        let finish_reason = ir
            .finish_reason
            .as_ref()
            .map(|fr| Cow::Borrowed(map_finish_reason_out(fr)));

        let grounding_metadata = ir
            .metadata
            .extensions
            .as_ref()
            .and_then(|ext| ext.get("grounding_metadata"))
            .cloned();

        let candidate = GeminiCandidate {
            content: Some(GeminiContent {
                role: Some(Cow::Borrowed("model")),
                parts,
            }),
            finish_reason,
            index: Some(0),
            grounding_metadata,
        };

        let usage_metadata = ir.usage.as_ref().map(|u| GeminiUsage {
            prompt_token_count: u.input_tokens,
            candidates_token_count: u.output_tokens,
            cached_content_token_count: u.cached_tokens,
            total_token_count: Some(u.input_tokens.saturating_add(u.output_tokens)),
            thoughts_token_count: u.thoughts_tokens,
            tool_use_prompt_token_count: None,
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
        FinishReasonIR::Refusal => "STOP",
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

#[cfg(test)]
mod usage_detail_tests {
    use super::*;
    use crate::gemini::codec::Gemini;
    use crate::ProtocolCodec;

    #[test]
    fn tool_use_prompt_token_count_serde_round_trip() {
        let body = br#"{
            "candidates":[{"content":{"parts":[{"text":"result"}]},"finishReason":"STOP"}],
            "usageMetadata":{
                "promptTokenCount":20,
                "candidatesTokenCount":5,
                "totalTokenCount":25,
                "toolUsePromptTokenCount":12
            }
        }"#;
        let codec = Gemini;
        let ir = codec.parse_response(body).expect("parse ok");
        let usage = ir.usage.as_ref().expect("usage present");
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 5);
        // tool_use_prompt_token_count has no IR slot — verify it parses at the wire level
        let wire: GeminiResponse<'_> = serde_json::from_slice(body).unwrap();
        assert_eq!(
            wire.usage_metadata.unwrap().tool_use_prompt_token_count,
            Some(12)
        );

        // Verify the field serialises when present
        let usage_wire = GeminiUsage {
            prompt_token_count: 20,
            candidates_token_count: 5,
            cached_content_token_count: None,
            total_token_count: Some(25),
            thoughts_token_count: None,
            tool_use_prompt_token_count: Some(12),
        };
        let json = serde_json::to_value(usage_wire).unwrap();
        assert_eq!(json["toolUsePromptTokenCount"], 12);
    }

    #[test]
    fn tool_use_prompt_token_count_absent_omitted() {
        let body = br#"{
            "candidates":[{"content":{"parts":[{"text":"ok"}]},"finishReason":"STOP"}],
            "usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":2,"totalTokenCount":5}
        }"#;
        let wire: GeminiResponse<'_> = serde_json::from_slice(body).unwrap();
        assert!(wire
            .usage_metadata
            .unwrap()
            .tool_use_prompt_token_count
            .is_none());

        let json = serde_json::to_value(wire.usage_metadata.unwrap()).unwrap();
        assert!(json.get("toolUsePromptTokenCount").is_none());
    }
}
