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
        // Anthropic separates system from messages: take the *first*
        // `System` message as the top-level field, drop subsequent ones (the
        // Anthropic API doesn't support multiple system prompts).
        let mut system: Option<Cow<'a, str>> = None;
        let mut messages: Vec<AnthropicMessage<'a>> = Vec::with_capacity(ir.messages.len());
        for m in &ir.messages {
            match m {
                MessageIR::System { content } => {
                    if system.is_none() {
                        system = Some(content_to_text(content));
                    }
                }
                MessageIR::User { content } => {
                    messages.push(AnthropicMessage::User {
                        content: content_to_text(content),
                    });
                }
                MessageIR::Assistant { content, .. } => {
                    messages.push(AnthropicMessage::Assistant {
                        content: content_to_text(content),
                    });
                }
                MessageIR::Tool { content, .. } => {
                    // Tool results fold into the trailing user turn in
                    // Anthropic's content blocks. v1: emit as a user turn
                    // with the tool output verbatim.
                    messages.push(AnthropicMessage::User {
                        content: content_to_text(content),
                    });
                }
            }
        }
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
