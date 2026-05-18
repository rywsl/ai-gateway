//! Hand-written `From` / `TryFrom` bridging Gemini wire structs and the IR.

use super::wire::{
    GeminiContent, GeminiGenerationConfig, GeminiPart, GeminiRequest, GeminiSystemInstruction,
};
use crate::common::content_to_text;
use gw_core::error::ErrorIR;
use gw_core::ir::{ContentIR, MessageIR, RequestIR};
use std::borrow::Cow;

impl<'a> From<GeminiRequest<'a>> for RequestIR<'a> {
    #[inline]
    fn from(src: GeminiRequest<'a>) -> Self {
        let mut messages: Vec<MessageIR<'a>> = Vec::new();
        if let Some(sys) = src.system_instruction {
            let text = join_parts(sys.parts);
            messages.push(MessageIR::System {
                content: ContentIR::Text(Cow::Owned(text)),
            });
        }
        for c in src.contents {
            let text = join_parts(c.parts);
            let content = ContentIR::Text(Cow::Owned(text));
            match c.role.as_ref() {
                "user" => messages.push(MessageIR::User { content }),
                "model" | "assistant" => messages.push(MessageIR::Assistant {
                    content,
                    tool_calls: None,
                }),
                _ => messages.push(MessageIR::User { content }),
            }
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
                            parts: vec![GeminiPart {
                                text: Some(content_to_text(content)),
                            }],
                        });
                    }
                }
                MessageIR::User { content } | MessageIR::Tool { content, .. } => {
                    // TODO(v2): emit Gemini functionResponse block instead of
                    // folding Tool messages into user text.
                    contents.push(GeminiContent {
                        role: Cow::Borrowed("user"),
                        parts: vec![GeminiPart {
                            text: Some(content_to_text(content)),
                        }],
                    });
                }
                MessageIR::Assistant { content, .. } => {
                    contents.push(GeminiContent {
                        role: Cow::Borrowed("model"),
                        parts: vec![GeminiPart {
                            text: Some(content_to_text(content)),
                        }],
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

#[inline]
fn join_parts(parts: Vec<GeminiPart<'_>>) -> String {
    let mut buf = String::new();
    for p in parts {
        if let Some(t) = p.text {
            buf.push_str(&t);
        }
    }
    buf
}
