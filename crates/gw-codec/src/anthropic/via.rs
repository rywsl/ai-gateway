//! Hand-written `From` / `TryFrom` bridging Anthropic wire structs and the IR.
//!
//! Request direction handles the content-block array shape: each
//! `AnthropicMessage` carries a `Vec<AnthropicContent>` (text / image /
//! thinking / tool_use / tool_result). On the way into the IR:
//!
//! - text blocks collapse to `ContentIR::Text` (or `ContentIR::Parts` when
//!   mixed with image blocks);
//! - tool_use blocks on an Assistant message project to `tool_calls`;
//! - tool_result blocks on a User message project to a separate
//!   `MessageIR::Tool` entry per result (Anthropic batches them inside one
//!   user turn, OpenAI surfaces them as discrete Tool messages — we keep the
//!   OpenAI shape in the IR and re-batch on the way out).
//!
//! Response direction adds `TryFrom<&ResponseIR>` (render) and `From` (parse),
//! threading thinking blocks, tool_use blocks, and `stop_reason` /
//! `usage` between Anthropic wire and `ResponseIR`.

use super::wire::{
    AnthropicContent, AnthropicMessage, AnthropicRequest, AnthropicResponse, AnthropicUsage,
};
use crate::common::content_to_text;
use gw_core::error::ErrorIR;
use gw_core::ir::{
    ContentIR, ContentPart, FinishReasonIR, ImageSource, MessageIR, RequestIR, ResponseIR,
    ResponseThinkingIR, ThinkingBlockIR, ToolCallIR, ToolUseBlockIR, UsageIR,
};
use serde_json::value::RawValue;
use smallvec::SmallVec;
use std::borrow::Cow;

/// Default max_tokens floor when the IR does not specify one. Anthropic
/// requires the field; we emit a sane ceiling rather than failing.
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ─── Request direction ────────────────────────────────────────────────────

impl<'a> From<AnthropicRequest<'a>> for RequestIR<'a> {
    #[inline]
    fn from(src: AnthropicRequest<'a>) -> Self {
        let mut messages: Vec<MessageIR<'a>> = Vec::with_capacity(src.messages.len() + 1);
        if let Some(sys) = src.system {
            messages.push(MessageIR::System {
                content: ContentIR::Text(sys),
            });
        }
        for m in src.messages {
            match m {
                AnthropicMessage::User { content } => lower_user_blocks(content, &mut messages),
                AnthropicMessage::Assistant { content } => {
                    messages.push(lower_assistant_blocks(content));
                }
            }
        }
        RequestIR {
            model: src.model,
            messages,
            stream: src.stream,
            max_tokens: Some(src.max_tokens),
            temperature: src.temperature,
            ..RequestIR::default()
        }
    }
}

impl<'a> TryFrom<&RequestIR<'a>> for AnthropicRequest<'a> {
    type Error = ErrorIR<'static>;

    #[inline]
    fn try_from(ir: &RequestIR<'a>) -> Result<Self, Self::Error> {
        // Anthropic separates `system` from messages and only supports a SINGLE
        // top-level `system` field. We:
        //   1. Concatenate ALL leading System messages with "\n\n" so no
        //      content is silently dropped.
        //   2. Continue extracting User/Assistant/Tool messages from after
        //      the leading System run.
        //   3. If a non-leading (mid-conversation) System message appears,
        //      we fold it into `system` too and emit a `tracing::warn!`.
        let mut system_parts: Vec<String> = Vec::new();
        let mut seen_non_system = false;
        let mut messages: Vec<AnthropicMessage<'a>> = Vec::with_capacity(ir.messages.len());
        let mut pending_user_tool_results: Vec<AnthropicContent<'a>> = Vec::new();

        let flush_tool_results =
            |bucket: &mut Vec<AnthropicContent<'a>>, out: &mut Vec<AnthropicMessage<'a>>| {
                if !bucket.is_empty() {
                    out.push(AnthropicMessage::User {
                        content: std::mem::take(bucket),
                    });
                }
            };

        for m in &ir.messages {
            match m {
                MessageIR::System { content } => {
                    if seen_non_system {
                        tracing::warn!(
                            "anthropic: mid-conversation System message folded into top-level system; conversation order may be perturbed"
                        );
                    }
                    system_parts.push(content_to_text(content).into_owned());
                }
                MessageIR::User { content } => {
                    seen_non_system = true;
                    flush_tool_results(&mut pending_user_tool_results, &mut messages);
                    messages.push(AnthropicMessage::User {
                        content: render_user_blocks(content),
                    });
                }
                MessageIR::Assistant {
                    content,
                    tool_calls,
                } => {
                    seen_non_system = true;
                    flush_tool_results(&mut pending_user_tool_results, &mut messages);
                    messages.push(AnthropicMessage::Assistant {
                        content: render_assistant_blocks(content, tool_calls.as_deref()),
                    });
                }
                MessageIR::Tool {
                    tool_call_id,
                    content,
                } => {
                    seen_non_system = true;
                    // Anthropic batches tool_result blocks inside a single
                    // user turn. Coalesce consecutive Tool messages into one
                    // User message carrying multiple tool_result blocks.
                    let body_text = content_to_text(content).into_owned();
                    pending_user_tool_results.push(AnthropicContent::ToolResult {
                        tool_use_id: tool_call_id.clone(),
                        content: Some(serde_json::Value::String(body_text)),
                        is_error: None,
                    });
                }
            }
        }
        flush_tool_results(&mut pending_user_tool_results, &mut messages);

        let system: Option<Cow<'a, str>> = if system_parts.is_empty() {
            None
        } else {
            Some(Cow::Owned(system_parts.join("\n\n")))
        };
        Ok(AnthropicRequest {
            model: ir.model.clone(),
            messages,
            system,
            max_tokens: ir.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            temperature: ir.temperature,
            stream: ir.stream,
        })
    }
}

/// Lower a User content-block array to one or more `MessageIR` entries.
/// `tool_result` blocks fan out into separate `MessageIR::Tool` entries so
/// downstream codecs (notably OpenAI) can render them as discrete Tool
/// messages.
fn lower_user_blocks<'a>(blocks: Vec<AnthropicContent<'a>>, out: &mut Vec<MessageIR<'a>>) {
    let mut text_parts: SmallVec<[ContentPart<'a>; 4]> = SmallVec::new();
    let mut tool_results: Vec<(Cow<'a, str>, Cow<'a, str>)> = Vec::new();
    let mut had_non_tool_result = false;
    for b in blocks {
        match b {
            AnthropicContent::Text { text } => {
                had_non_tool_result = true;
                text_parts.push(ContentPart::Text(text));
            }
            AnthropicContent::Image { source } => {
                had_non_tool_result = true;
                // Inspect the opaque source object for the URL or b64 payload;
                // unknown shapes are dropped.
                let kind = source.get("type").and_then(|t| t.as_str()).unwrap_or("");
                let part = match kind {
                    "base64" => {
                        let media_type = source
                            .get("media_type")
                            .and_then(|m| m.as_str())
                            .unwrap_or("application/octet-stream")
                            .to_owned();
                        let data = source
                            .get("data")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_owned();
                        Some(ContentPart::Image {
                            source: ImageSource::B64 {
                                media_type: Cow::Owned(media_type),
                                data: Cow::Owned(data),
                            },
                        })
                    }
                    "url" => {
                        let url = source
                            .get("url")
                            .and_then(|u| u.as_str())
                            .unwrap_or("")
                            .to_owned();
                        Some(ContentPart::Image {
                            source: ImageSource::Url(Cow::Owned(url)),
                        })
                    }
                    _ => None,
                };
                if let Some(p) = part {
                    text_parts.push(p);
                }
            }
            AnthropicContent::ToolResult {
                tool_use_id,
                content,
                ..
            } => {
                let body = match content {
                    Some(serde_json::Value::String(s)) => s,
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                tool_results.push((tool_use_id, Cow::Owned(body)));
            }
            // Thinking / ToolUse on a user message are not part of the
            // Anthropic surface in practice; ignore.
            AnthropicContent::Thinking { .. } | AnthropicContent::ToolUse { .. } => {}
        }
    }
    if had_non_tool_result {
        let content = match text_parts.len() {
            0 => ContentIR::Text(Cow::Borrowed("")),
            1 => match text_parts.into_iter().next().unwrap() {
                ContentPart::Text(t) => ContentIR::Text(t),
                other => {
                    let mut s: SmallVec<[ContentPart<'a>; 4]> = SmallVec::new();
                    s.push(other);
                    ContentIR::Parts(s)
                }
            },
            _ => ContentIR::Parts(text_parts),
        };
        out.push(MessageIR::User { content });
    }
    for (tool_call_id, body) in tool_results {
        out.push(MessageIR::Tool {
            tool_call_id,
            content: ContentIR::Text(body),
        });
    }
}

/// Lower an Assistant content-block array into a single `MessageIR::Assistant`
/// with optional `tool_calls`. Thinking blocks are dropped on the request
/// side (they describe a *response*, not a *request*); the IR carries
/// thinking on `ResponseIR.thinking` instead.
fn lower_assistant_blocks<'a>(blocks: Vec<AnthropicContent<'a>>) -> MessageIR<'a> {
    let mut text_buf = String::new();
    let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();
    for b in blocks {
        match b {
            AnthropicContent::Text { text } => text_buf.push_str(text.as_ref()),
            AnthropicContent::ToolUse { id, name, input } => {
                let raw = value_to_raw(input);
                tool_calls.push(ToolCallIR {
                    id,
                    name,
                    arguments_json: Cow::Owned(raw),
                });
            }
            // Image / Thinking / ToolResult are not part of an Assistant
            // request-side block in practice; ignore.
            AnthropicContent::Image { .. }
            | AnthropicContent::Thinking { .. }
            | AnthropicContent::ToolResult { .. } => {}
        }
    }
    MessageIR::Assistant {
        content: ContentIR::Text(Cow::Owned(text_buf)),
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
    }
}

fn render_user_blocks<'a>(content: &ContentIR<'a>) -> Vec<AnthropicContent<'a>> {
    match content {
        ContentIR::Text(t) => vec![AnthropicContent::Text { text: t.clone() }],
        ContentIR::Parts(parts) => {
            let mut out: Vec<AnthropicContent<'a>> = Vec::with_capacity(parts.len());
            for p in parts {
                match p {
                    ContentPart::Text(t) => out.push(AnthropicContent::Text { text: t.clone() }),
                    ContentPart::Image { source } => {
                        let value = match source {
                            ImageSource::Url(url) => serde_json::json!({
                                "type": "url",
                                "url": url.as_ref(),
                            }),
                            ImageSource::B64 { media_type, data } => serde_json::json!({
                                "type": "base64",
                                "media_type": media_type.as_ref(),
                                "data": data.as_ref(),
                            }),
                        };
                        out.push(AnthropicContent::Image { source: value });
                    }
                    // Audio / File parts: v1 does not surface them in Anthropic
                    // wire (no equivalent block) — drop.
                    ContentPart::Audio { .. } | ContentPart::File { .. } => {}
                }
            }
            if out.is_empty() {
                out.push(AnthropicContent::Text {
                    text: Cow::Borrowed(""),
                });
            }
            out
        }
    }
}

fn render_assistant_blocks<'a>(
    content: &ContentIR<'a>,
    tool_calls: Option<&[ToolCallIR<'a>]>,
) -> Vec<AnthropicContent<'a>> {
    let mut out: Vec<AnthropicContent<'a>> = Vec::new();
    let text = content_to_text(content);
    if !text.is_empty() {
        out.push(AnthropicContent::Text { text });
    }
    if let Some(tcs) = tool_calls {
        for tc in tcs {
            let input = raw_to_value(tc.arguments_json.as_ref());
            out.push(AnthropicContent::ToolUse {
                id: tc.id.clone(),
                name: tc.name.clone(),
                input,
            });
        }
    }
    if out.is_empty() {
        // Anthropic rejects an empty content array on Assistant messages;
        // emit an empty text block to stay schema-valid.
        out.push(AnthropicContent::Text {
            text: Cow::Borrowed(""),
        });
    }
    out
}

// ─── Response direction ───────────────────────────────────────────────────

impl<'a> From<AnthropicResponse<'a>> for ResponseIR<'a> {
    fn from(w: AnthropicResponse<'a>) -> Self {
        let mut assistant_text = String::new();
        let mut thinking_blocks: Vec<ThinkingBlockIR<'a>> = Vec::new();
        let mut tool_use_blocks: Vec<ToolUseBlockIR<'a>> = Vec::new();
        let mut tool_calls: Vec<ToolCallIR<'a>> = Vec::new();

        for block in w.content {
            match block {
                AnthropicContent::Text { text } => assistant_text.push_str(text.as_ref()),
                AnthropicContent::Thinking {
                    thinking,
                    signature,
                } => {
                    thinking_blocks.push(ThinkingBlockIR {
                        text: thinking,
                        signature,
                    });
                }
                AnthropicContent::ToolUse { id, name, input } => {
                    // Materialise both the IR-flat tool_calls slot (used by
                    // OpenAI-style downstream renderers) and the
                    // Anthropic-shape tool_use_blocks slot (used to re-render
                    // back to Anthropic). The Value is converted to a
                    // RawValue once and then both slots share the JSON
                    // text via independent owned copies.
                    let raw = value_to_raw(input);
                    let raw_for_blocks = RawValue::from_string(raw.get().to_owned())
                        .expect("RawValue clone via from_string is infallible");
                    tool_calls.push(ToolCallIR {
                        id: id.clone(),
                        name: name.clone(),
                        arguments_json: Cow::Owned(raw),
                    });
                    tool_use_blocks.push(ToolUseBlockIR {
                        id,
                        name,
                        input: Cow::Owned(raw_for_blocks),
                    });
                }
                // Image / ToolResult are not valid on an Assistant response
                // block; ignore defensively.
                AnthropicContent::Image { .. } | AnthropicContent::ToolResult { .. } => {}
            }
        }

        let messages = vec![MessageIR::Assistant {
            content: ContentIR::Text(Cow::Owned(assistant_text)),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
        }];

        let finish_reason = w.stop_reason.as_deref().map(map_stop_reason_to_finish);

        let usage = w.usage.map(|u| UsageIR {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cached_tokens: u.cache_read_input_tokens,
            reasoning_tokens: None,
        });

        ResponseIR {
            id: w.id,
            model: w.model,
            messages,
            finish_reason,
            usage,
            metadata: Default::default(),
            thinking: if thinking_blocks.is_empty() {
                None
            } else {
                Some(ResponseThinkingIR {
                    blocks: thinking_blocks,
                })
            },
            reasoning: None,
            tool_use_blocks: if tool_use_blocks.is_empty() {
                None
            } else {
                Some(tool_use_blocks)
            },
        }
    }
}

impl<'a> TryFrom<&ResponseIR<'a>> for AnthropicResponse<'a> {
    type Error = ErrorIR<'static>;

    fn try_from(ir: &ResponseIR<'a>) -> Result<Self, Self::Error> {
        let mut content: Vec<AnthropicContent<'a>> = Vec::new();

        // Thinking blocks come first — Anthropic clients render in this
        // order and tooling depending on block order observes it.
        if let Some(t) = ir.thinking.as_ref() {
            for b in &t.blocks {
                content.push(AnthropicContent::Thinking {
                    thinking: b.text.clone(),
                    signature: b.signature.clone(),
                });
            }
        }

        // Last Assistant message contributes the text body + (fallback)
        // tool_calls list. Walk in reverse to pick the freshest assistant
        // turn — matches the OpenAI Chat renderer's convention.
        let last_assistant = ir
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m, MessageIR::Assistant { .. }));

        if let Some(MessageIR::Assistant {
            content: msg_content,
            tool_calls,
        }) = last_assistant
        {
            let text = content_to_text(msg_content);
            if !text.is_empty() {
                content.push(AnthropicContent::Text { text });
            }
            // Anthropic shape prefers explicit tool_use blocks. Prefer the
            // IR's `tool_use_blocks` slot when present (Anthropic-origin
            // payload), otherwise synthesise from the OpenAI-style
            // `tool_calls` on the message.
            if let Some(blocks) = ir.tool_use_blocks.as_ref() {
                for b in blocks {
                    let value = raw_to_value(b.input.as_ref());
                    content.push(AnthropicContent::ToolUse {
                        id: b.id.clone(),
                        name: b.name.clone(),
                        input: value,
                    });
                }
            } else if let Some(tcs) = tool_calls {
                for tc in tcs {
                    let value = raw_to_value(tc.arguments_json.as_ref());
                    content.push(AnthropicContent::ToolUse {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        input: value,
                    });
                }
            }
        } else if let Some(blocks) = ir.tool_use_blocks.as_ref() {
            // No Assistant message but the IR carries tool_use_blocks —
            // emit them so the renderer is symmetric with the OpenAI
            // Responses path.
            for b in blocks {
                let value = raw_to_value(b.input.as_ref());
                content.push(AnthropicContent::ToolUse {
                    id: b.id.clone(),
                    name: b.name.clone(),
                    input: value,
                });
            }
        }

        if content.is_empty() {
            content.push(AnthropicContent::Text {
                text: Cow::Borrowed(""),
            });
        }

        let stop_reason = ir
            .finish_reason
            .as_ref()
            .map(map_finish_to_stop_reason)
            .map(Cow::Borrowed);

        let usage = ir.usage.as_ref().map(|u| AnthropicUsage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cached_tokens,
            cache_creation_input_tokens: None,
        });

        Ok(AnthropicResponse {
            id: ir.id.clone(),
            ty: Cow::Borrowed("message"),
            role: Cow::Borrowed("assistant"),
            model: ir.model.clone(),
            content,
            stop_reason,
            stop_sequence: None,
            usage,
        })
    }
}

/// Map Anthropic `stop_reason` strings to `FinishReasonIR`.
pub(crate) fn map_stop_reason_to_finish(s: &str) -> FinishReasonIR {
    match s {
        "end_turn" => FinishReasonIR::Stop,
        "max_tokens" => FinishReasonIR::Length,
        "tool_use" => FinishReasonIR::ToolCalls,
        "stop_sequence" => FinishReasonIR::Stop,
        other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
    }
}

/// Map `FinishReasonIR` back to an Anthropic `stop_reason` literal. Returns
/// a static string so the wire struct can borrow it.
pub(crate) fn map_finish_to_stop_reason(fr: &FinishReasonIR) -> &'static str {
    match fr {
        FinishReasonIR::Stop => "end_turn",
        FinishReasonIR::Length => "max_tokens",
        FinishReasonIR::ToolCalls => "tool_use",
        FinishReasonIR::ContentFilter => "end_turn",
        // cross_render::map_finish_reason degrades Other → Stop before
        // render_response runs; defensively map any survivor to end_turn.
        FinishReasonIR::Other(_) => "end_turn",
    }
}

/// Convert an opaque `serde_json::Value` (parsed from a wire content block)
/// into a `RawValue` so the IR's `Cow<'a, RawValue>` slots can hold it. The
/// round-trip is lossy only on whitespace / key ordering, which is
/// acceptable for the IR contract (which carries semantic JSON, not byte
/// fidelity).
fn value_to_raw(v: serde_json::Value) -> Box<RawValue> {
    let s = serde_json::to_string(&v).expect("serializing serde_json::Value is infallible");
    RawValue::from_string(s).expect("serialized JSON value must be a valid RawValue")
}

/// Inverse of `value_to_raw` — read an IR-side `RawValue` back into a
/// `Value` for re-rendering through the internally-tagged wire enum.
fn raw_to_value(rv: &RawValue) -> serde_json::Value {
    serde_json::from_str(rv.get()).unwrap_or(serde_json::Value::Null)
}
