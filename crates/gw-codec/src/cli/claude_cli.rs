use super::{CliCodec, CliHints, CliUsageDelta, CliUsageExtractor, CliUsageFull};
use gw_core::protocol::ApiShape;
use serde::Deserialize;

#[derive(Default)]
pub struct ClaudeCliCodec;

static USAGE_EXTRACTOR: ClaudeCliUsageExtractor = ClaudeCliUsageExtractor;

impl CliCodec for ClaudeCliCodec {
    fn shape(&self) -> ApiShape {
        ApiShape::ClaudeCli
    }

    fn extract_hints(&self, _path: &str, body: Option<&[u8]>) -> CliHints {
        body.and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default()
    }

    fn usage_extractor(&self) -> &dyn CliUsageExtractor {
        &USAGE_EXTRACTOR
    }

    fn ttft_event_types(&self) -> &'static [&'static str] {
        &["content_block_delta"]
    }
}

#[derive(Default)]
pub struct ClaudeCliUsageExtractor;

impl CliUsageExtractor for ClaudeCliUsageExtractor {
    fn extract_stream_usage(&self, event_type: &str, data: &[u8]) -> Option<CliUsageDelta> {
        match event_type {
            "message_start" => {
                let frame: MessageStartFrame = serde_json::from_slice(data).ok()?;
                let usage = frame.message?.usage?;
                Some(CliUsageDelta {
                    input_tokens: usage.input_tokens,
                    output_tokens: None,
                    cache_read_input_tokens: usage.cache_read_input_tokens,
                    cache_creation_input_tokens: usage.cache_creation_input_tokens,
                })
            }
            "message_delta" => {
                let frame: MessageDeltaFrame = serde_json::from_slice(data).ok()?;
                let usage = frame.usage?;
                Some(CliUsageDelta {
                    input_tokens: None,
                    output_tokens: usage.output_tokens,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                })
            }
            _ => None,
        }
    }

    fn extract_response_usage(&self, body: &[u8]) -> Option<CliUsageFull> {
        let root: ResponseRoot = serde_json::from_slice(body).ok()?;
        let usage = root.usage?;
        Some(CliUsageFull {
            input_tokens: usage.input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
            cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
        })
    }
}

#[derive(Deserialize)]
struct ResponseRoot {
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct MessageStartFrame {
    message: Option<ResponseRoot>,
}

#[derive(Deserialize)]
struct MessageDeltaFrame {
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}
