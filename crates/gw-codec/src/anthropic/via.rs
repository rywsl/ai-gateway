//! Hand-written `From` / `TryFrom` bridging Anthropic wire structs and the IR.

use super::wire::{AnthropicMessage, AnthropicRequest};
use crate::common::content_to_text;
use gw_core::error::ErrorIR;
use gw_core::ir::{ContentIR, MessageIR, RequestIR};
use std::borrow::Cow;

/// Default max_tokens floor when the IR does not specify one. Anthropic
/// requires the field; we emit a sane ceiling rather than failing.
const DEFAULT_MAX_TOKENS: u32 = 4096;

impl<'a> From<AnthropicRequest<'a>> for RequestIR<'a> {
    #[inline]
    fn from(src: AnthropicRequest<'a>) -> Self {
        let mut messages: Vec<MessageIR<'a>> = Vec::with_capacity(
            src.messages.len() + if src.system.is_some() { 1 } else { 0 },
        );
        if let Some(sys) = src.system {
            messages.push(MessageIR::System {
                content: ContentIR::Text(sys),
            });
        }
        for m in src.messages {
            messages.push(match m {
                AnthropicMessage::User { content } => MessageIR::User {
                    content: ContentIR::Text(content),
                },
                AnthropicMessage::Assistant { content } => MessageIR::Assistant {
                    content: ContentIR::Text(content),
                    tool_calls: None,
                },
            });
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
        // Anthropic separates system from messages and only supports a SINGLE
        // top-level `system` field. We:
        //   1. Concatenate ALL leading System messages with "\n\n" so no
        //      content is silently dropped.
        //   2. Continue extracting User/Assistant/Tool messages from after
        //      the leading System run.
        //   3. If a non-leading (mid-conversation) System message appears,
        //      we fold it into `system` too and emit a `tracing::warn!`.
        //      Conversation order is perturbed in this case; v1 trade-off.
        let mut system_parts: Vec<String> = Vec::new();
        let mut seen_non_system = false;
        let mut messages: Vec<AnthropicMessage<'a>> = Vec::with_capacity(ir.messages.len());
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
                    messages.push(AnthropicMessage::User {
                        content: content_to_text(content),
                    });
                }
                MessageIR::Assistant { content, .. } => {
                    seen_non_system = true;
                    messages.push(AnthropicMessage::Assistant {
                        content: content_to_text(content),
                    });
                }
                MessageIR::Tool { content, .. } => {
                    // Tool results fold into the trailing user turn in
                    // Anthropic's content blocks. v1: emit as a user turn
                    // with the tool output verbatim.
                    // TODO(v2): emit Anthropic tool_result block instead of
                    // folding into User text.
                    seen_non_system = true;
                    messages.push(AnthropicMessage::User {
                        content: content_to_text(content),
                    });
                }
            }
        }
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
