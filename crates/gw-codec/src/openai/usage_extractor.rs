//! OpenAI Chat + Responses usage extractor. Spec §5.2 rows 1-2.

use crate::usage_extractor::{ExtractedUsage, PerDimProvenance, ProvenanceTag, UsageExtractor};
use gw_core::usage::UsageSnapshot;
use serde::Deserialize;

#[derive(Clone, Copy, Debug)]
pub enum OpenAiFlavor {
    Chat,
    Responses,
}

pub struct OpenAiUsageExtractor {
    flavor: OpenAiFlavor,
}

impl OpenAiUsageExtractor {
    pub fn chat() -> Self {
        Self {
            flavor: OpenAiFlavor::Chat,
        }
    }

    pub fn responses() -> Self {
        Self {
            flavor: OpenAiFlavor::Responses,
        }
    }
}

#[derive(Deserialize, Default)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<PromptDetails>,
    #[serde(default)]
    completion_tokens_details: Option<CompletionDetails>,
}

#[derive(Deserialize, Default)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    input_tokens_details: Option<PromptDetails>,
    #[serde(default)]
    output_tokens_details: Option<CompletionDetails>,
}

#[derive(Deserialize, Default)]
struct PromptDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(Deserialize, Default)]
struct CompletionDetails {
    #[serde(default)]
    reasoning_tokens: Option<u32>,
}

#[derive(Deserialize, Default)]
struct ChatRoot {
    #[serde(default)]
    usage: Option<ChatUsage>,
}

#[derive(Deserialize, Default)]
struct ResponsesRoot {
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

impl UsageExtractor for OpenAiUsageExtractor {
    fn extract_response(&self, body: &[u8]) -> ExtractedUsage {
        match self.flavor {
            OpenAiFlavor::Chat => {
                let root: ChatRoot = match serde_json::from_slice(body) {
                    Ok(v) => v,
                    Err(_) => return ExtractedUsage::default(),
                };
                let Some(u) = root.usage else {
                    return ExtractedUsage::default();
                };
                let cache_read = u.prompt_tokens_details.and_then(|d| d.cached_tokens);
                let thinking = u.completion_tokens_details.and_then(|d| d.reasoning_tokens);
                ExtractedUsage {
                    values: UsageSnapshot {
                        input_tokens: u.prompt_tokens,
                        output_tokens: u.completion_tokens,
                        cache_read_input_tokens: cache_read,
                        cache_creation_input_tokens: None,
                        thinking_tokens: thinking,
                        multimodal_input_tokens: None,
                    },
                    provenance: provenance_from_present(
                        u.prompt_tokens > 0,
                        u.completion_tokens > 0,
                        cache_read.is_some(),
                        false,
                        thinking.is_some(),
                        false,
                    ),
                    is_terminal: true,
                }
            }
            OpenAiFlavor::Responses => {
                let root: ResponsesRoot = match serde_json::from_slice(body) {
                    Ok(v) => v,
                    Err(_) => return ExtractedUsage::default(),
                };
                let Some(u) = root.usage else {
                    return ExtractedUsage::default();
                };
                let cache_read = u.input_tokens_details.and_then(|d| d.cached_tokens);
                let thinking = u.output_tokens_details.and_then(|d| d.reasoning_tokens);
                ExtractedUsage {
                    values: UsageSnapshot {
                        input_tokens: u.input_tokens,
                        output_tokens: u.output_tokens,
                        cache_read_input_tokens: cache_read,
                        cache_creation_input_tokens: None,
                        thinking_tokens: thinking,
                        multimodal_input_tokens: None,
                    },
                    provenance: provenance_from_present(
                        u.input_tokens > 0,
                        u.output_tokens > 0,
                        cache_read.is_some(),
                        false,
                        thinking.is_some(),
                        false,
                    ),
                    is_terminal: true,
                }
            }
        }
    }

    fn extract_chunk(&self, event_bytes: &[u8]) -> Option<ExtractedUsage> {
        let trimmed = strip_sse_data_prefix(event_bytes)?;
        let extracted = self.extract_response(trimmed);
        if extracted.values.input_tokens == 0 && extracted.values.output_tokens == 0 {
            None
        } else {
            Some(extracted)
        }
    }
}

fn strip_sse_data_prefix(event: &[u8]) -> Option<&[u8]> {
    let s = std::str::from_utf8(event).ok()?;
    let line = s.lines().find(|l| l.starts_with("data: "))?;
    Some(&line.as_bytes()[6..])
}

fn provenance_from_present(
    input: bool,
    output: bool,
    cache_read: bool,
    cache_creation: bool,
    thinking: bool,
    multimodal: bool,
) -> PerDimProvenance {
    let tag = |present: bool| {
        if present {
            ProvenanceTag::UpstreamFinal
        } else {
            ProvenanceTag::Unknown
        }
    };
    PerDimProvenance {
        input: tag(input),
        output: tag(output),
        cache_read: tag(cache_read),
        cache_creation: tag(cache_creation),
        thinking: tag(thinking),
        multimodal_input: tag(multimodal),
    }
}
