//! Bidirectional conversion between `OpenAiResponsesResponse` (wire) and
//! `ResponseIR` (codec IR). Mirrors the OpenAI Chat conversion idiom in
//! `crates/gw-codec/src/openai/via.rs` but with the Responses output-item
//! taxonomy (`message` / `reasoning` / `function_call`).

use std::borrow::Cow;

use gw_core::error::ErrorIR;
use gw_core::ir::{
    ContentIR, FinishReasonIR, MessageIR, MetadataIR, ReasoningBlockIR, ResponseIR, ToolCallIR,
    UsageIR,
};
use serde_json::value::RawValue;

use crate::common::content_to_text;
use crate::openai::wire::{OpenAiCompletionTokensDetails, OpenAiPromptTokensDetails};

use super::wire_response::{
    IncompleteDetails, MessageContent, OpenAiResponsesResponse, ReasoningSummaryPart,
    ResponseOutputItem, ResponsesUsage,
};

// ─── parse: wire → IR ──────────────────────────────────────────────────────

impl<'a> From<OpenAiResponsesResponse<'a>> for ResponseIR<'a> {
    fn from(w: OpenAiResponsesResponse<'a>) -> Self {
        // Walk `output` accumulating message text, reasoning summary parts, and
        // function_call items. The IR carries the last assistant message; if
        // multiple `Message` items appear (rare today, future-proof) we
        // concatenate their `OutputText` content.
        let mut assistant_text = String::new();
        let mut reasoning_summary = String::new();
        let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();
        let mut saw_function_call = false;
        let mut saw_message = false;

        for item in w.output {
            match item {
                ResponseOutputItem::Message { content, .. } => {
                    saw_message = true;
                    for c in content {
                        match c {
                            MessageContent::OutputText { text } => {
                                assistant_text.push_str(text.as_ref());
                            }
                        }
                    }
                }
                ResponseOutputItem::Reasoning { summary, .. } => {
                    for (i, part) in summary.into_iter().enumerate() {
                        match part {
                            ReasoningSummaryPart::SummaryText { text } => {
                                if i > 0 {
                                    reasoning_summary.push('\n');
                                }
                                reasoning_summary.push_str(text.as_ref());
                            }
                        }
                    }
                }
                ResponseOutputItem::FunctionCall {
                    name,
                    arguments,
                    call_id,
                    id,
                    ..
                } => {
                    saw_function_call = true;
                    let args_owned = arguments.into_owned();
                    let raw = match RawValue::from_string(args_owned.clone()) {
                        Ok(rv) => rv,
                        Err(_) => {
                            // Same defensive pattern as openai/via.rs: if the
                            // upstream sent a non-JSON `arguments` string,
                            // re-encode it as a JSON string literal so the IR's
                            // RawValue invariant (always-valid JSON) holds.
                            tracing::warn!(
                                "openai responses function_call.arguments not valid JSON; \
                                 passing through as string literal"
                            );
                            let escaped = serde_json::to_string(&args_owned)
                                .expect("serde_json::to_string of &String is infallible");
                            RawValue::from_string(escaped)
                                .expect("serialized JSON string literal must be valid JSON")
                        }
                    };
                    // OpenAI Responses uses `call_id` as the cross-turn-stable
                    // identifier (the `id` field is the per-output-item id).
                    // Falling back to `id` keeps single-shot calls working when
                    // the upstream omitted `call_id`.
                    let stable_id = if !call_id.is_empty() { call_id } else { id };
                    tool_calls.push(ToolCallIR {
                        id: stable_id,
                        name,
                        arguments_json: Cow::Owned(raw),
                    });
                }
            }
        }

        // Build the IR Assistant message. Even when there's no text but there
        // are tool_calls, emit an Assistant message with empty text so the
        // downstream Chat renderer sees a structurally-valid `last_assistant`.
        let messages: Vec<MessageIR<'a>> = if saw_message || saw_function_call {
            vec![MessageIR::Assistant {
                content: ContentIR::Text(Cow::Owned(assistant_text)),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
            }]
        } else {
            Vec::new()
        };

        let reasoning = if reasoning_summary.is_empty() {
            None
        } else {
            Some(ReasoningBlockIR {
                summary: Cow::Owned(reasoning_summary),
            })
        };

        let finish_reason =
            derive_finish_reason(w.status.as_ref(), &w.incomplete_details, saw_function_call);

        let usage = w.usage.map(|u| usage_wire_to_ir(&u));

        ResponseIR {
            id: w.id,
            model: w.model,
            messages,
            finish_reason,
            usage,
            metadata: MetadataIR::default(),
            thinking: None,
            reasoning,
            tool_use_blocks: None,
        }
    }
}

fn derive_finish_reason(
    status: &str,
    incomplete: &Option<IncompleteDetails<'_>>,
    saw_function_call: bool,
) -> Option<FinishReasonIR> {
    match status {
        "completed" => {
            if saw_function_call {
                Some(FinishReasonIR::ToolCalls)
            } else {
                Some(FinishReasonIR::Stop)
            }
        }
        "incomplete" => match incomplete.as_ref().map(|d| d.reason.as_ref()) {
            Some("max_output_tokens") => Some(FinishReasonIR::Length),
            Some("content_filter") => Some(FinishReasonIR::ContentFilter),
            Some(other) => Some(FinishReasonIR::Other(Cow::Owned(other.to_owned()))),
            None => Some(FinishReasonIR::Length),
        },
        "in_progress" => None,
        other => Some(FinishReasonIR::Other(Cow::Owned(other.to_owned()))),
    }
}

fn usage_wire_to_ir(u: &ResponsesUsage) -> UsageIR {
    UsageIR {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_tokens: u.input_tokens_details.map(|d| d.cached_tokens),
        reasoning_tokens: u.output_tokens_details.map(|d| d.reasoning_tokens),
    }
}

// ─── render: IR → wire ─────────────────────────────────────────────────────

impl<'a> TryFrom<&ResponseIR<'a>> for OpenAiResponsesResponse<'a> {
    type Error = ErrorIR<'static>;

    fn try_from(ir: &ResponseIR<'a>) -> Result<Self, Self::Error> {
        let mut output: Vec<ResponseOutputItem<'a>> = Vec::new();

        // Reasoning item comes first if present (OpenAI's own clients render in
        // this order, and tools depending on item ordering observe it).
        if let Some(reasoning) = ir.reasoning.as_ref() {
            let summary = if reasoning.summary.is_empty() {
                Vec::new()
            } else {
                vec![ReasoningSummaryPart::SummaryText {
                    text: reasoning.summary.clone(),
                }]
            };
            output.push(ResponseOutputItem::Reasoning {
                id: Cow::Borrowed(""),
                summary,
                encrypted_content: None,
            });
        }

        // Walk the last Assistant for content + tool_calls.
        let last_assistant = ir
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m, MessageIR::Assistant { .. }));

        if let Some(MessageIR::Assistant {
            content,
            tool_calls,
        }) = last_assistant
        {
            let text = content_to_text(content);
            output.push(ResponseOutputItem::Message {
                id: Cow::Borrowed(""),
                content: if text.is_empty() {
                    Vec::new()
                } else {
                    vec![MessageContent::OutputText { text }]
                },
                role: Cow::Borrowed("assistant"),
                status: Cow::Borrowed("completed"),
            });
            if let Some(tcs) = tool_calls {
                for tc in tcs {
                    output.push(ResponseOutputItem::FunctionCall {
                        id: Cow::Borrowed(""),
                        name: tc.name.clone(),
                        arguments: Cow::Owned(tc.arguments_json.get().to_owned()),
                        call_id: tc.id.clone(),
                        status: Cow::Borrowed("completed"),
                    });
                }
            }
        }

        // If no Assistant message but the IR carries tool_use_blocks (Anthropic
        // origin), synthesise FunctionCall items.
        if last_assistant.is_none() {
            if let Some(blocks) = ir.tool_use_blocks.as_ref() {
                for b in blocks {
                    output.push(ResponseOutputItem::FunctionCall {
                        id: Cow::Borrowed(""),
                        name: b.name.clone(),
                        arguments: Cow::Owned(b.input.get().to_owned()),
                        call_id: b.id.clone(),
                        status: Cow::Borrowed("completed"),
                    });
                }
            }
        }

        let (status, incomplete_details) = match ir.finish_reason.as_ref() {
            Some(FinishReasonIR::Stop) | Some(FinishReasonIR::ToolCalls) | None => {
                (Cow::Borrowed("completed"), None)
            }
            Some(FinishReasonIR::Length) => (
                Cow::Borrowed("incomplete"),
                Some(IncompleteDetails {
                    reason: Cow::Borrowed("max_output_tokens"),
                }),
            ),
            Some(FinishReasonIR::ContentFilter) => (
                Cow::Borrowed("incomplete"),
                Some(IncompleteDetails {
                    reason: Cow::Borrowed("content_filter"),
                }),
            ),
            // `cross_render::map_finish_reason` degrades `Other` to `Stop`
            // before render_response runs; defensively map any survivor to
            // "completed" to stay schema-valid downstream.
            Some(FinishReasonIR::Other(_)) => (Cow::Borrowed("completed"), None),
        };

        let usage = ir.usage.as_ref().map(|u| ResponsesUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            total_tokens: u.input_tokens.saturating_add(u.output_tokens),
            input_tokens_details: u
                .cached_tokens
                .map(|c| OpenAiPromptTokensDetails { cached_tokens: c }),
            output_tokens_details: u.reasoning_tokens.map(|r| OpenAiCompletionTokensDetails {
                reasoning_tokens: r,
            }),
        });

        Ok(OpenAiResponsesResponse {
            id: ir.id.clone(),
            object: Cow::Borrowed("response"),
            created_at: 0,
            model: ir.model.clone(),
            status,
            incomplete_details,
            output,
            usage,
        })
    }
}
