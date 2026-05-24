//! Anthropic Messages usage extractor. Spec §5.2 row 3.

use crate::usage_extractor::{ExtractedUsage, PerDimProvenance, ProvenanceTag, UsageExtractor};
use gw_core::usage::UsageSnapshot;
use serde::Deserialize;

#[derive(Default)]
pub struct AnthropicUsageExtractor;

#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Deserialize, Default)]
struct Root {
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct MessageDeltaFrame {
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct MessageStartFrame {
    #[serde(default)]
    message: Option<Root>,
}

impl UsageExtractor for AnthropicUsageExtractor {
    fn extract_response(&self, body: &[u8]) -> ExtractedUsage {
        let root: Root = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return ExtractedUsage::default(),
        };
        match root.usage {
            Some(u) => to_extracted(u, true),
            None => ExtractedUsage::default(),
        }
    }

    fn extract_chunk(&self, event_bytes: &[u8]) -> Option<ExtractedUsage> {
        let s = std::str::from_utf8(event_bytes).ok()?;
        let mut event_name = "";
        let mut data_line = "";
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                event_name = rest.trim();
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data_line = rest.trim();
            }
        }
        if data_line.is_empty() {
            return None;
        }
        match event_name {
            "message_start" => {
                let f: MessageStartFrame = serde_json::from_str(data_line).ok()?;
                let u = f.message?.usage?;
                Some(to_extracted(u, false))
            }
            "message_delta" => {
                let f: MessageDeltaFrame = serde_json::from_str(data_line).ok()?;
                let u = f.usage?;
                Some(to_extracted(u, true))
            }
            _ => None,
        }
    }
}

fn to_extracted(u: Usage, is_terminal: bool) -> ExtractedUsage {
    let tag = |present: bool| {
        if present {
            ProvenanceTag::UpstreamFinal
        } else {
            ProvenanceTag::Unknown
        }
    };
    ExtractedUsage {
        values: UsageSnapshot {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            thinking_tokens: None,
            multimodal_input_tokens: None,
        },
        provenance: PerDimProvenance {
            input: tag(u.input_tokens > 0),
            output: tag(u.output_tokens > 0),
            cache_read: tag(u.cache_read_input_tokens.is_some()),
            cache_creation: tag(u.cache_creation_input_tokens.is_some()),
            thinking: ProvenanceTag::Unknown,
            multimodal_input: ProvenanceTag::Unknown,
        },
        is_terminal,
    }
}
