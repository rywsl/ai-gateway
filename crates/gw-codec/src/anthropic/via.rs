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
    AnthropicCacheCreationDetails, AnthropicContent, AnthropicMessage, AnthropicOutputConfig,
    AnthropicRequest, AnthropicResponse, AnthropicServerToolUsage, AnthropicThinkingConfig,
    AnthropicUsage, ContainerUploadBlock, DocumentSource, ServerToolResultBlock,
    ServerToolUseBlock,
};
use crate::common::{content_to_text, internal_err};
use gw_core::error::ErrorIR;
use gw_core::ir::{
    AnthropicThinking, ContentIR, ContentPart, FinishReasonIR, ImageSource, MediaSource, MessageIR,
    ReasoningConfig, RedactedThinkingIR, RequestIR, ResponseIR, ResponseThinkingIR,
    ServerToolBlockIR, ThinkingBlockIR, ToolCallIR, ToolUseBlockIR, UsageIR,
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
        let thinking = src.thinking.as_ref().map(lower_thinking_config);
        let mut reasoning_config = thinking
            .as_ref()
            .and_then(|(_, reasoning)| reasoning.clone());
        if let Some(output_config) = src.output_config {
            let reasoning = reasoning_config.get_or_insert_with(ReasoningConfig::default);
            reasoning.effort = output_config.effort;
        }
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
            service_tier: src.service_tier,
            metadata: gw_core::ir::MetadataIR {
                anthropic_thinking: thinking.map(|(thinking, _)| thinking),
                ..Default::default()
            },
            reasoning_config,
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
                MessageIR::Developer { content } => {
                    if seen_non_system {
                        tracing::warn!(
                            "anthropic: mid-conversation Developer message folded into top-level system; conversation order may be perturbed"
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
            thinking: render_thinking_config(ir),
            output_config: render_output_config(ir),
            service_tier: ir.service_tier.clone(),
            stream: ir.stream,
        })
    }
}

fn lower_thinking_config<'a>(
    thinking: &AnthropicThinkingConfig<'a>,
) -> (AnthropicThinking<'a>, Option<ReasoningConfig<'a>>) {
    match thinking {
        AnthropicThinkingConfig::Enabled {
            budget_tokens,
            display,
        } => (
            AnthropicThinking {
                kind: Cow::Borrowed("enabled"),
                budget_tokens: Some(*budget_tokens),
                display: display.clone(),
                adaptive: Some(false),
            },
            Some(ReasoningConfig {
                effort: None,
                budget_tokens: Some(*budget_tokens),
            }),
        ),
        AnthropicThinkingConfig::Adaptive { display } => (
            AnthropicThinking {
                kind: Cow::Borrowed("adaptive"),
                budget_tokens: None,
                display: display.clone(),
                adaptive: Some(true),
            },
            None,
        ),
        AnthropicThinkingConfig::Disabled => (
            AnthropicThinking {
                kind: Cow::Borrowed("disabled"),
                budget_tokens: None,
                display: None,
                adaptive: Some(false),
            },
            None,
        ),
    }
}

fn render_thinking_config<'a>(ir: &RequestIR<'a>) -> Option<AnthropicThinkingConfig<'a>> {
    let thinking = ir.metadata.anthropic_thinking.as_ref()?;
    if thinking.adaptive == Some(true) || thinking.kind.as_ref() == "adaptive" {
        return Some(AnthropicThinkingConfig::Adaptive {
            display: thinking.display.clone(),
        });
    }
    if thinking.kind.as_ref() == "disabled" {
        return Some(AnthropicThinkingConfig::Disabled);
    }
    let budget_tokens = thinking.budget_tokens.or(ir
        .reasoning_config
        .as_ref()
        .and_then(|cfg| cfg.budget_tokens))?;
    Some(AnthropicThinkingConfig::Enabled {
        budget_tokens,
        display: thinking.display.clone(),
    })
}

fn render_output_config<'a>(ir: &RequestIR<'a>) -> Option<AnthropicOutputConfig<'a>> {
    let effort = ir
        .reasoning_config
        .as_ref()
        .and_then(|cfg| cfg.effort.clone());
    effort.as_ref()?;
    Some(AnthropicOutputConfig {
        effort,
        format: None,
    })
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
            AnthropicContent::Document {
                source,
                title,
                context,
                ..
            } => {
                had_non_tool_result = true;
                let part = match source {
                    DocumentSource::Base64 { media_type, data } => ContentPart::File {
                        source: MediaSource::B64 { media_type, data },
                    },
                    DocumentSource::Url { url } => ContentPart::File {
                        source: MediaSource::Url(url),
                    },
                    DocumentSource::Text { text } => {
                        let mut buf = String::new();
                        if let Some(t) = title {
                            buf.push_str(t.as_ref());
                            buf.push_str("\n\n");
                        }
                        if let Some(c) = context {
                            buf.push_str(c.as_ref());
                            buf.push_str("\n\n");
                        }
                        buf.push_str(text.as_ref());
                        ContentPart::Text(Cow::Owned(buf))
                    }
                    DocumentSource::Content { content } => {
                        let mut buf = String::new();
                        if let Some(t) = title {
                            buf.push_str(t.as_ref());
                            buf.push_str("\n\n");
                        }
                        if let Some(c) = context {
                            buf.push_str(c.as_ref());
                            buf.push_str("\n\n");
                        }
                        for block in content {
                            if let AnthropicContent::Text { text } = block {
                                buf.push_str(text.as_ref());
                            }
                        }
                        ContentPart::Text(Cow::Owned(buf))
                    }
                };
                text_parts.push(part);
            }
            AnthropicContent::SearchResult {
                source,
                title,
                content,
                ..
            } => {
                had_non_tool_result = true;
                let mut buf = String::with_capacity(title.len() + 3);
                buf.push_str(title.as_ref());
                buf.push_str("\n\n");
                for block in content {
                    if let AnthropicContent::Text { text } = block {
                        buf.push_str(text.as_ref());
                    }
                }
                if !source.is_empty() {
                    buf.push_str("\n\nSource: ");
                    buf.push_str(source.as_ref());
                }
                text_parts.push(ContentPart::Text(Cow::Owned(buf)));
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
            AnthropicContent::Thinking { .. }
            | AnthropicContent::ToolUse { .. }
            | AnthropicContent::RedactedThinking { .. }
            | AnthropicContent::ServerToolUse(_)
            | AnthropicContent::WebSearchToolResult(_)
            | AnthropicContent::WebFetchToolResult(_)
            | AnthropicContent::CodeExecutionToolResult(_)
            | AnthropicContent::BashCodeExecutionToolResult(_)
            | AnthropicContent::TextEditorCodeExecutionToolResult(_)
            | AnthropicContent::ToolSearchToolResult(_)
            | AnthropicContent::McpToolUse(_)
            | AnthropicContent::McpToolResult(_)
            | AnthropicContent::ContainerUpload(_)
            | AnthropicContent::Unknown => {}
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
            | AnthropicContent::RedactedThinking { .. }
            | AnthropicContent::ToolResult { .. }
            | AnthropicContent::Document { .. }
            | AnthropicContent::SearchResult { .. }
            | AnthropicContent::ServerToolUse(_)
            | AnthropicContent::WebSearchToolResult(_)
            | AnthropicContent::WebFetchToolResult(_)
            | AnthropicContent::CodeExecutionToolResult(_)
            | AnthropicContent::BashCodeExecutionToolResult(_)
            | AnthropicContent::TextEditorCodeExecutionToolResult(_)
            | AnthropicContent::ToolSearchToolResult(_)
            | AnthropicContent::McpToolUse(_)
            | AnthropicContent::McpToolResult(_)
            | AnthropicContent::ContainerUpload(_)
            | AnthropicContent::Unknown => {}
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
                    // Audio parts have no Anthropic equivalent — drop.
                    ContentPart::Audio { .. } => {}
                    ContentPart::File { source } => {
                        let doc_source = match source {
                            MediaSource::B64 { media_type, data } => DocumentSource::Base64 {
                                media_type: media_type.clone(),
                                data: data.clone(),
                            },
                            MediaSource::Url(url) => DocumentSource::Url { url: url.clone() },
                        };
                        out.push(AnthropicContent::Document {
                            source: doc_source,
                            title: None,
                            context: None,
                            cache_control: None,
                        });
                    }
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
        let mut redacted_thinking_blocks: Vec<RedactedThinkingIR<'a>> = Vec::new();
        let mut tool_use_blocks: Vec<ToolUseBlockIR<'a>> = Vec::new();
        let mut server_tool_blocks: Vec<ServerToolBlockIR<'a>> = Vec::new();
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
                AnthropicContent::ServerToolUse(block) => {
                    server_tool_blocks.push(server_tool_use_to_ir("server_tool_use", block));
                }
                AnthropicContent::WebSearchToolResult(block) => {
                    server_tool_blocks
                        .push(server_tool_result_to_ir("web_search_tool_result", block));
                }
                AnthropicContent::WebFetchToolResult(block) => {
                    server_tool_blocks
                        .push(server_tool_result_to_ir("web_fetch_tool_result", block));
                }
                AnthropicContent::CodeExecutionToolResult(block) => {
                    server_tool_blocks.push(server_tool_result_to_ir(
                        "code_execution_tool_result",
                        block,
                    ));
                }
                AnthropicContent::BashCodeExecutionToolResult(block) => {
                    server_tool_blocks.push(server_tool_result_to_ir(
                        "bash_code_execution_tool_result",
                        block,
                    ));
                }
                AnthropicContent::TextEditorCodeExecutionToolResult(block) => {
                    server_tool_blocks.push(server_tool_result_to_ir(
                        "text_editor_code_execution_tool_result",
                        block,
                    ));
                }
                AnthropicContent::ToolSearchToolResult(block) => {
                    server_tool_blocks
                        .push(server_tool_result_to_ir("tool_search_tool_result", block));
                }
                AnthropicContent::McpToolUse(block) => {
                    server_tool_blocks.push(server_tool_use_to_ir("mcp_tool_use", block));
                }
                AnthropicContent::McpToolResult(block) => {
                    server_tool_blocks.push(server_tool_result_to_ir("mcp_tool_result", block));
                }
                AnthropicContent::ContainerUpload(block) => {
                    server_tool_blocks.push(container_upload_to_ir(block));
                }
                AnthropicContent::RedactedThinking { data } => {
                    redacted_thinking_blocks.push(RedactedThinkingIR { data });
                }
                AnthropicContent::Unknown => {
                    tracing::warn!("anthropic: unknown response content block skipped");
                }
                // Image / ToolResult / Document / SearchResult are not valid
                // on an Assistant response block; ignore defensively.
                AnthropicContent::Image { .. }
                | AnthropicContent::ToolResult { .. }
                | AnthropicContent::Document { .. }
                | AnthropicContent::SearchResult { .. } => {}
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

        let usage = w.usage.map(|u| {
            let (cache_creation_1h, cache_creation_5m) = u
                .cache_creation
                .map(|c| (c.ephemeral_1h_input_tokens, c.ephemeral_5m_input_tokens))
                .unwrap_or((None, None));
            let (web_search, web_fetch) = u
                .server_tool_use
                .map(|s| (s.web_search_requests, s.web_fetch_requests))
                .unwrap_or((None, None));
            UsageIR {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cached_tokens: u.cache_read_input_tokens,
                reasoning_tokens: None,
                cache_creation_tokens: u.cache_creation_input_tokens,
                cache_creation_1h_tokens: cache_creation_1h,
                cache_creation_5m_tokens: cache_creation_5m,
                server_tool_web_search_requests: web_search,
                server_tool_web_fetch_requests: web_fetch,
                ..Default::default()
            }
        });

        // Merge response-level inference_geo / service_tier into usage IR
        // (Anthropic surfaces them at response top-level, IR carries them on UsageIR).
        let usage = match (usage, &w.inference_geo, &w.service_tier) {
            (Some(mut u), geo, tier) => {
                if u.inference_geo.is_none() {
                    u.inference_geo = geo.as_ref().map(|g| Cow::Owned(g.to_string()));
                }
                if u.service_tier.is_none() {
                    u.service_tier = tier.as_ref().map(|t| Cow::Owned(t.to_string()));
                }
                Some(u)
            }
            (None, Some(geo), Some(tier)) => Some(UsageIR {
                inference_geo: Some(Cow::Owned(geo.to_string())),
                service_tier: Some(Cow::Owned(tier.to_string())),
                ..Default::default()
            }),
            (None, Some(geo), None) => Some(UsageIR {
                inference_geo: Some(Cow::Owned(geo.to_string())),
                ..Default::default()
            }),
            (None, None, Some(tier)) => Some(UsageIR {
                service_tier: Some(Cow::Owned(tier.to_string())),
                ..Default::default()
            }),
            (None, None, None) => None,
        };

        // Also set service_tier on ResponseIR itself.
        let response_service_tier = w.service_tier.map(|s| Cow::Owned(s.into_owned()));

        ResponseIR {
            id: w.id,
            model: w.model,
            messages,
            finish_reason,
            usage,
            metadata: Default::default(),
            service_tier: response_service_tier,
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
            server_tool_blocks: if server_tool_blocks.is_empty() {
                None
            } else {
                Some(server_tool_blocks)
            },
            redacted_thinking: if redacted_thinking_blocks.is_empty() {
                None
            } else {
                Some(redacted_thinking_blocks)
            },
            ..Default::default()
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

        if let Some(rt) = ir.redacted_thinking.as_ref() {
            for b in rt {
                content.push(AnthropicContent::RedactedThinking {
                    data: b.data.clone(),
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
            content: ContentIR::Text(msg_content),
            tool_calls,
        }) = last_assistant
        {
            let text = msg_content.clone();
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
        } else if last_assistant.is_some() {
            return Err(internal_err(
                "expected ContentIR::Text, got non-text content",
            ));
        } else if !ir.messages.is_empty() {
            return Err(internal_err(
                "expected Assistant, got non-assistant message",
            ));
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

        if let Some(blocks) = ir.server_tool_blocks.as_ref() {
            for b in blocks {
                if let Some(block) = render_server_tool_block(b) {
                    content.push(block);
                }
            }
        }

        if content.is_empty() {
            content.push(AnthropicContent::Text {
                text: Cow::Borrowed(""),
            });
        }

        let stop_reason = ir.finish_reason.as_ref().map(map_finish_to_stop_reason);

        let usage = ir.usage.as_ref().map(|u| {
            let cache_creation = match (u.cache_creation_1h_tokens, u.cache_creation_5m_tokens) {
                (None, None) => None,
                (h, m) => Some(AnthropicCacheCreationDetails {
                    ephemeral_1h_input_tokens: h,
                    ephemeral_5m_input_tokens: m,
                }),
            };
            let server_tool_use = match (
                u.server_tool_web_search_requests,
                u.server_tool_web_fetch_requests,
            ) {
                (None, None) => None,
                (ws, wf) => Some(AnthropicServerToolUsage {
                    web_search_requests: ws,
                    web_fetch_requests: wf,
                }),
            };
            AnthropicUsage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_read_input_tokens: u.cached_tokens,
                cache_creation_input_tokens: u.cache_creation_tokens,
                cache_creation,
                server_tool_use,
            }
        });

        let service_tier = ir
            .service_tier
            .as_ref()
            .or(ir.usage.as_ref().and_then(|u| u.service_tier.as_ref()))
            .map(|s| Cow::Owned(s.to_string()));
        let inference_geo = ir
            .usage
            .as_ref()
            .and_then(|u| u.inference_geo.as_ref())
            .map(|s| Cow::Owned(s.to_string()));

        Ok(AnthropicResponse {
            id: ir.id.clone(),
            ty: Cow::Borrowed("message"),
            role: Cow::Borrowed("assistant"),
            model: ir.model.clone(),
            content,
            stop_reason,
            stop_sequence: None,
            usage,
            service_tier,
            inference_geo,
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
        "refusal" => FinishReasonIR::Refusal,
        // Anthropic-specific stop reasons with no IR equivalent — preserve
        // as `Other` so same-protocol round-trips keep them verbatim while
        // cross-protocol renders degrade via `cross_render::map_finish_reason`.
        "pause_turn" | "model_context_window_exceeded" => {
            FinishReasonIR::Other(Cow::Owned(s.to_owned()))
        }
        other => FinishReasonIR::Other(Cow::Owned(other.to_owned())),
    }
}

/// Map `FinishReasonIR` back to an Anthropic `stop_reason` literal. For
/// known IR variants returns a static string; for `Other` preserves the
/// original vendor string so same-protocol round-trips keep it verbatim.
pub(crate) fn map_finish_to_stop_reason(fr: &FinishReasonIR) -> Cow<'static, str> {
    match fr {
        FinishReasonIR::Stop => Cow::Borrowed("end_turn"),
        FinishReasonIR::Length => Cow::Borrowed("max_tokens"),
        FinishReasonIR::ToolCalls => Cow::Borrowed("tool_use"),
        FinishReasonIR::ContentFilter => Cow::Borrowed("end_turn"),
        FinishReasonIR::Refusal => Cow::Borrowed("refusal"),
        FinishReasonIR::Other(s) => s.clone(),
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

fn server_tool_use_to_ir<'a>(
    kind: &'static str,
    block: ServerToolUseBlock<'a>,
) -> ServerToolBlockIR<'a> {
    ServerToolBlockIR {
        kind: Cow::Borrowed(kind),
        id: block.id,
        name: block.name,
        input: block.input,
        extra: block.extra,
    }
}

fn server_tool_result_to_ir<'a>(
    kind: &'static str,
    block: ServerToolResultBlock<'a>,
) -> ServerToolBlockIR<'a> {
    let input = RawValue::from_string("null".to_owned()).expect("literal null is valid JSON");
    ServerToolBlockIR {
        kind: Cow::Borrowed(kind),
        id: block.id,
        name: block.name,
        input,
        extra: block.extra,
    }
}

fn container_upload_to_ir(block: ContainerUploadBlock<'_>) -> ServerToolBlockIR<'_> {
    let input = RawValue::from_string("null".to_owned()).expect("literal null is valid JSON");
    ServerToolBlockIR {
        kind: Cow::Borrowed("container_upload"),
        id: block.file_id,
        name: Cow::Borrowed("container_upload"),
        input,
        extra: serde_json::Map::new(),
    }
}

fn render_server_tool_block<'a>(block: &ServerToolBlockIR<'a>) -> Option<AnthropicContent<'a>> {
    match block.kind.as_ref() {
        "server_tool_use" => Some(AnthropicContent::ServerToolUse(ServerToolUseBlock {
            id: block.id.clone(),
            name: block.name.clone(),
            input: RawValue::from_string(block.input.get().to_owned())
                .expect("RawValue clone via from_string is infallible"),
            extra: block.extra.clone(),
        })),
        "web_search_tool_result" => Some(AnthropicContent::WebSearchToolResult(
            render_result_block(block),
        )),
        "web_fetch_tool_result" => Some(AnthropicContent::WebFetchToolResult(render_result_block(
            block,
        ))),
        "code_execution_tool_result" => Some(AnthropicContent::CodeExecutionToolResult(
            render_result_block(block),
        )),
        "bash_code_execution_tool_result" => Some(AnthropicContent::BashCodeExecutionToolResult(
            render_result_block(block),
        )),
        "text_editor_code_execution_tool_result" => Some(
            AnthropicContent::TextEditorCodeExecutionToolResult(render_result_block(block)),
        ),
        "tool_search_tool_result" => Some(AnthropicContent::ToolSearchToolResult(
            render_result_block(block),
        )),
        "mcp_tool_use" => Some(AnthropicContent::McpToolUse(ServerToolUseBlock {
            id: block.id.clone(),
            name: block.name.clone(),
            input: RawValue::from_string(block.input.get().to_owned())
                .expect("RawValue clone via from_string is infallible"),
            extra: block.extra.clone(),
        })),
        "mcp_tool_result" => Some(AnthropicContent::McpToolResult(render_result_block(block))),
        "container_upload" => Some(AnthropicContent::ContainerUpload(ContainerUploadBlock {
            file_id: block.id.clone(),
        })),
        other => {
            tracing::warn!(
                kind = other,
                "anthropic: unknown server_tool_blocks kind skipped"
            );
            None
        }
    }
}

fn render_result_block<'a>(block: &ServerToolBlockIR<'a>) -> ServerToolResultBlock<'a> {
    ServerToolResultBlock {
        id: block.id.clone(),
        name: block.name.clone(),
        extra: block.extra.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::ir::ServerToolBlockIR;

    #[test]
    fn server_tool_use_response_block_round_trips_through_ir() {
        let wire: AnthropicResponse<'_> = serde_json::from_str(
            r#"{
                "id":"msg_server_tool",
                "type":"message",
                "role":"assistant",
                "model":"claude-3-5-sonnet",
                "content":[{
                    "type":"server_tool_use",
                    "id":"srvu_1",
                    "name":"web_search",
                    "input":{"query":"rust"},
                    "status":"started"
                }],
                "stop_reason":"end_turn"
            }"#,
        )
        .expect("server_tool_use parses");

        let ir = ResponseIR::from(wire);
        let blocks = ir.server_tool_blocks.as_ref().expect("server tool block");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind.as_ref(), "server_tool_use");
        assert_eq!(blocks[0].id.as_ref(), "srvu_1");
        assert_eq!(blocks[0].name.as_ref(), "web_search");
        assert_eq!(blocks[0].input.get(), r#"{"query":"rust"}"#);
        assert_eq!(
            blocks[0].extra.get("status").and_then(|v| v.as_str()),
            Some("started")
        );

        let rendered = AnthropicResponse::try_from(&ir).expect("render response");
        let rendered_json = serde_json::to_value(rendered).expect("serialize response");
        assert_eq!(
            rendered_json["content"][0]["type"].as_str(),
            Some("server_tool_use")
        );
        assert_eq!(rendered_json["content"][0]["id"].as_str(), Some("srvu_1"));
        assert_eq!(
            rendered_json["content"][0]["name"].as_str(),
            Some("web_search")
        );
        assert_eq!(rendered_json["content"][0]["input"]["query"], "rust");
        assert_eq!(rendered_json["content"][0]["status"], "started");
    }

    #[test]
    fn unknown_response_block_is_skipped() {
        let wire: AnthropicResponse<'_> = serde_json::from_str(
            r#"{
                "id":"msg_unknown",
                "type":"message",
                "role":"assistant",
                "model":"claude-3-5-sonnet",
                "content":[
                    {"type":"future_tool_result","id":"x","payload":true},
                    {"type":"text","text":"kept"}
                ],
                "stop_reason":"end_turn"
            }"#,
        )
        .expect("unknown content tag maps to Unknown");

        let ir = ResponseIR::from(wire);
        assert!(ir.server_tool_blocks.is_none());
        match ir.messages.first() {
            Some(MessageIR::Assistant {
                content: ContentIR::Text(text),
                ..
            }) => assert_eq!(text.as_ref(), "kept"),
            other => panic!("expected assistant text, got {other:?}"),
        }
    }

    #[test]
    fn renders_each_server_tool_block_kind_to_matching_anthropic_variant() {
        let kinds = [
            "server_tool_use",
            "web_search_tool_result",
            "web_fetch_tool_result",
            "code_execution_tool_result",
            "bash_code_execution_tool_result",
            "text_editor_code_execution_tool_result",
            "tool_search_tool_result",
            "mcp_tool_use",
            "mcp_tool_result",
            "container_upload",
        ];
        let blocks = kinds
            .iter()
            .map(|kind| ServerToolBlockIR {
                kind: Cow::Borrowed(*kind),
                id: Cow::Borrowed("blk_1"),
                name: Cow::Borrowed("tool_name"),
                input: RawValue::from_string(r#"{"ok":true}"#.to_owned()).unwrap(),
                extra: serde_json::Map::from_iter([(
                    "vendor".to_owned(),
                    serde_json::Value::String("opaque".to_owned()),
                )]),
            })
            .collect();

        let ir = ResponseIR {
            id: Cow::Borrowed("msg_all"),
            model: Cow::Borrowed("claude-3-5-sonnet"),
            server_tool_blocks: Some(blocks),
            ..ResponseIR::default()
        };
        let rendered = AnthropicResponse::try_from(&ir).expect("render response");
        let json = serde_json::to_value(rendered).expect("serialize response");
        let content = json["content"].as_array().expect("content array");

        for (idx, kind) in kinds.iter().enumerate() {
            assert_eq!(content[idx]["type"].as_str(), Some(*kind));
        }
    }

    #[test]
    fn document_base64_parses_to_file_ir() {
        let req: AnthropicRequest<'_> = serde_json::from_str(
            r#"{
                "model":"claude-3-5-sonnet",
                "max_tokens":1024,
                "stream":false,
                "messages":[{
                    "role":"user",
                    "content":[
                        {"type":"document","source":{"type":"base64","media_type":"application/pdf","data":"AQID"},"title":"My Doc","context":"read this"},
                        {"type":"text","text":"summarize"}
                    ]
                }]
            }"#,
        )
        .expect("document base64 parses");

        let ir = RequestIR::from(req);
        let user = ir
            .messages
            .iter()
            .find(|m| matches!(m, MessageIR::User { .. }));
        match user {
            Some(MessageIR::User {
                content: ContentIR::Parts(parts),
            }) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    ContentPart::File {
                        source: MediaSource::B64 { media_type, data },
                    } => {
                        assert_eq!(media_type.as_ref(), "application/pdf");
                        assert_eq!(data.as_ref(), "AQID");
                    }
                    other => panic!("expected File B64, got {other:?}"),
                }
                assert!(matches!(&parts[1], ContentPart::Text(t) if t.as_ref() == "summarize"));
            }
            other => panic!("expected User Parts, got {other:?}"),
        }
    }

    #[test]
    fn document_url_parses_to_file_ir() {
        let req: AnthropicRequest<'_> = serde_json::from_str(
            r#"{
                "model":"claude-3-5-sonnet",
                "max_tokens":1024,
                "stream":false,
                "messages":[{
                    "role":"user",
                    "content":[
                        {"type":"document","source":{"type":"url","url":"https://example.com/doc.pdf"}}
                    ]
                }]
            }"#,
        )
        .expect("document url parses");

        let ir = RequestIR::from(req);
        match ir.messages.first() {
            Some(MessageIR::User {
                content: ContentIR::Parts(parts),
            }) => match &parts[0] {
                ContentPart::File {
                    source: MediaSource::Url(url),
                } => {
                    assert_eq!(url.as_ref(), "https://example.com/doc.pdf");
                }
                other => panic!("expected File Url, got {other:?}"),
            },
            other => panic!("expected User Parts, got {other:?}"),
        }
    }

    #[test]
    fn document_text_source_parses_to_text_ir() {
        let req: AnthropicRequest<'_> = serde_json::from_str(
            r#"{
                "model":"claude-3-5-sonnet",
                "max_tokens":1024,
                "stream":false,
                "messages":[{
                    "role":"user",
                    "content":[
                        {"type":"document","source":{"type":"text","text":"hello world"},"title":"T","context":"C"}
                    ]
                }]
            }"#,
        )
        .expect("document text parses");

        let ir = RequestIR::from(req);
        match ir.messages.first() {
            Some(MessageIR::User {
                content: ContentIR::Text(t),
            }) => {
                assert!(t.contains("hello world"));
                assert!(t.contains("T"));
                assert!(t.contains("C"));
            }
            other => panic!("expected User Text, got {other:?}"),
        }
    }

    #[test]
    fn search_result_parses_to_text_ir() {
        let req: AnthropicRequest<'_> = serde_json::from_str(
            r#"{
                "model":"claude-3-5-sonnet",
                "max_tokens":1024,
                "stream":false,
                "messages":[{
                    "role":"user",
                    "content":[
                        {"type":"search_result","source":"https://example.com","title":"Example","content":[{"type":"text","text":"body text"}]}
                    ]
                }]
            }"#,
        )
        .expect("search_result parses");

        let ir = RequestIR::from(req);
        match ir.messages.first() {
            Some(MessageIR::User {
                content: ContentIR::Text(t),
            }) => {
                assert!(t.contains("Example"));
                assert!(t.contains("body text"));
                assert!(t.contains("https://example.com"));
            }
            other => panic!("expected User Text, got {other:?}"),
        }
    }

    #[test]
    fn file_ir_renders_to_document_block() {
        let ir = RequestIR {
            model: Cow::Borrowed("claude-3-5-sonnet"),
            messages: vec![MessageIR::User {
                content: ContentIR::Parts(smallvec::smallvec![ContentPart::File {
                    source: MediaSource::B64 {
                        media_type: Cow::Borrowed("application/pdf"),
                        data: Cow::Borrowed("AQID"),
                    },
                }]),
            }],
            max_tokens: Some(1024),
            ..RequestIR::default()
        };
        let rendered = AnthropicRequest::try_from(&ir).expect("render request");
        let json = serde_json::to_value(rendered).expect("serialize");
        let block = &json["messages"][0]["content"][0];
        assert_eq!(block["type"].as_str(), Some("document"));
        assert_eq!(block["source"]["type"].as_str(), Some("base64"));
        assert_eq!(
            block["source"]["media_type"].as_str(),
            Some("application/pdf")
        );
        assert_eq!(block["source"]["data"].as_str(), Some("AQID"));
    }

    #[test]
    fn document_with_cache_control_round_trips() {
        let json_str = r#"{
            "model":"claude-3-5-sonnet",
            "max_tokens":1024,
            "stream":false,
            "messages":[{
                "role":"user",
                "content":[
                    {"type":"document","source":{"type":"url","url":"https://example.com/doc.pdf"},"cache_control":{"type":"ephemeral"}}
                ]
            }]
        }"#;
        let req: AnthropicRequest<'_> =
            serde_json::from_str(json_str).expect("parses with cache_control");
        let json = serde_json::to_value(&req).expect("serialize");
        assert_eq!(
            json["messages"][0]["content"][0]["cache_control"]["type"].as_str(),
            Some("ephemeral")
        );
    }

    #[test]
    fn redacted_thinking_response_round_trip() {
        let wire: AnthropicResponse<'_> = serde_json::from_str(
            r#"{
                "id":"msg_rt",
                "type":"message",
                "role":"assistant",
                "model":"claude-3-5-sonnet",
                "content":[
                    {"type":"thinking","thinking":"visible thought","signature":"sig1"},
                    {"type":"redacted_thinking","data":"b64-encoded-opaque-data"},
                    {"type":"text","text":"hello"}
                ],
                "stop_reason":"end_turn",
                "usage":{"input_tokens":10,"output_tokens":5}
            }"#,
        )
        .expect("wire with redacted_thinking parses");

        let ir = ResponseIR::from(wire);

        let thinking = ir.thinking.as_ref().expect("thinking blocks present");
        assert_eq!(thinking.blocks.len(), 1);
        assert_eq!(thinking.blocks[0].text.as_ref(), "visible thought");

        let rt = ir
            .redacted_thinking
            .as_ref()
            .expect("redacted_thinking present");
        assert_eq!(rt.len(), 1);
        assert_eq!(rt[0].data.as_ref(), "b64-encoded-opaque-data");

        let rendered = AnthropicResponse::try_from(&ir).expect("render response");
        let json = serde_json::to_value(rendered).expect("serialize");
        let content = json["content"].as_array().expect("content array");

        let rt_block = content
            .iter()
            .find(|b| b["type"] == "redacted_thinking")
            .expect("redacted_thinking block in rendered output");
        assert_eq!(rt_block["data"].as_str(), Some("b64-encoded-opaque-data"));
    }

    #[test]
    fn stop_reason_pause_turn_maps_to_other() {
        let fr = map_stop_reason_to_finish("pause_turn");
        match &fr {
            FinishReasonIR::Other(s) => assert_eq!(s.as_ref(), "pause_turn"),
            other => panic!("expected Other(pause_turn), got {other:?}"),
        }
        let back = map_finish_to_stop_reason(&fr);
        assert_eq!(back.as_ref(), "pause_turn");
    }

    #[test]
    fn stop_reason_refusal_maps_to_ir_refusal() {
        let fr = map_stop_reason_to_finish("refusal");
        assert!(matches!(fr, FinishReasonIR::Refusal));
        let back = map_finish_to_stop_reason(&fr);
        assert_eq!(back.as_ref(), "refusal");
    }

    #[test]
    fn stop_reason_model_context_window_exceeded_maps_to_other() {
        let fr = map_stop_reason_to_finish("model_context_window_exceeded");
        match &fr {
            FinishReasonIR::Other(s) => assert_eq!(s.as_ref(), "model_context_window_exceeded"),
            other => panic!("expected Other, got {other:?}"),
        }
        let back = map_finish_to_stop_reason(&fr);
        assert_eq!(back.as_ref(), "model_context_window_exceeded");
    }

    #[test]
    fn mcp_tool_use_response_block_round_trips_through_ir() {
        let wire: AnthropicResponse<'_> = serde_json::from_str(
            r#"{
                "id":"msg_mcp",
                "type":"message",
                "role":"assistant",
                "model":"claude-3-5-sonnet",
                "content":[
                    {"type":"mcp_tool_use","id":"mcp_1","name":"read_file","input":{"path":"/tmp/test.txt"},"server_name":"filesystem"},
                    {"type":"mcp_tool_result","id":"mcp_r1","name":"read_file","content":"file contents here","server_name":"filesystem"}
                ],
                "stop_reason":"end_turn"
            }"#,
        )
        .expect("mcp blocks parse");

        let ir = ResponseIR::from(wire);
        let blocks = ir.server_tool_blocks.as_ref().expect("server tool blocks");
        assert_eq!(blocks.len(), 2);

        assert_eq!(blocks[0].kind.as_ref(), "mcp_tool_use");
        assert_eq!(blocks[0].id.as_ref(), "mcp_1");
        assert_eq!(blocks[0].name.as_ref(), "read_file");
        assert_eq!(blocks[0].input.get(), r#"{"path":"/tmp/test.txt"}"#);
        assert_eq!(
            blocks[0].extra.get("server_name").and_then(|v| v.as_str()),
            Some("filesystem")
        );

        assert_eq!(blocks[1].kind.as_ref(), "mcp_tool_result");
        assert_eq!(blocks[1].id.as_ref(), "mcp_r1");
        assert_eq!(blocks[1].name.as_ref(), "read_file");
        assert_eq!(
            blocks[1].extra.get("server_name").and_then(|v| v.as_str()),
            Some("filesystem")
        );

        let rendered = AnthropicResponse::try_from(&ir).expect("render response");
        let json = serde_json::to_value(rendered).expect("serialize");
        let content = json["content"].as_array().expect("content array");

        assert_eq!(content[0]["type"].as_str(), Some("mcp_tool_use"));
        assert_eq!(content[0]["id"].as_str(), Some("mcp_1"));
        assert_eq!(content[0]["input"]["path"], "/tmp/test.txt");
        assert_eq!(content[0]["server_name"], "filesystem");

        assert_eq!(content[1]["type"].as_str(), Some("mcp_tool_result"));
        assert_eq!(content[1]["id"].as_str(), Some("mcp_r1"));
    }
}
