//! Gemini Generate usage extractor. Spec §5.2 row 4.

use crate::usage_extractor::{ExtractedUsage, PerDimProvenance, ProvenanceTag, UsageExtractor};
use gw_core::usage::UsageSnapshot;
use serde::Deserialize;

#[derive(Default)]
pub struct GeminiUsageExtractor;

#[derive(Deserialize, Default)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached: Option<u32>,
    #[serde(default, rename = "thoughtsTokenCount")]
    thoughts: Option<u32>,
    #[serde(default, rename = "promptTokensDetails")]
    prompt_details: Vec<ModalityCount>,
}

#[derive(Deserialize, Default)]
struct ModalityCount {
    #[serde(default)]
    modality: String,
    #[serde(default, rename = "tokenCount")]
    token_count: u32,
}

#[derive(Deserialize, Default)]
struct Root {
    #[serde(default, rename = "usageMetadata")]
    meta: Option<UsageMetadata>,
}

impl UsageExtractor for GeminiUsageExtractor {
    fn extract_response(&self, body: &[u8]) -> ExtractedUsage {
        let root: Root = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return ExtractedUsage::default(),
        };
        let Some(m) = root.meta else {
            return ExtractedUsage::default();
        };
        let multimodal = sum_non_text_modalities(&m.prompt_details);
        let tag = |present: bool| {
            if present {
                ProvenanceTag::UpstreamFinal
            } else {
                ProvenanceTag::Unknown
            }
        };
        ExtractedUsage {
            values: UsageSnapshot {
                input_tokens: m.prompt,
                output_tokens: m.candidates,
                cache_read_input_tokens: m.cached,
                cache_creation_input_tokens: None,
                thinking_tokens: m.thoughts,
                multimodal_input_tokens: multimodal,
            },
            provenance: PerDimProvenance {
                input: tag(m.prompt > 0),
                output: tag(m.candidates > 0),
                cache_read: tag(m.cached.is_some()),
                cache_creation: ProvenanceTag::Unknown,
                thinking: tag(m.thoughts.is_some()),
                multimodal_input: tag(multimodal.is_some()),
            },
            is_terminal: true,
        }
    }

    fn extract_chunk(&self, event_bytes: &[u8]) -> Option<ExtractedUsage> {
        let s = std::str::from_utf8(event_bytes).ok()?;
        let line = s.lines().find(|l| l.starts_with("data: "))?;
        let json = &line.as_bytes()[6..];
        let extracted = self.extract_response(json);
        if extracted.values.input_tokens == 0 && extracted.values.output_tokens == 0 {
            None
        } else {
            Some(extracted)
        }
    }
}

fn sum_non_text_modalities(details: &[ModalityCount]) -> Option<u32> {
    let total: u32 = details
        .iter()
        .filter(|d| !d.modality.eq_ignore_ascii_case("TEXT"))
        .map(|d| d.token_count)
        .sum();
    if total > 0 { Some(total) } else { None }
}
