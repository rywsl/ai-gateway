use gw_core::protocol::ApiShape;
use serde::Deserialize;
use std::borrow::Cow;

use super::{
    passthrough_bytes, CliCodec, CliHints, CliUsageDelta, CliUsageExtractor, CliUsageFull,
};

pub struct OpenAiCompactCodec;

pub struct OpenAiCompactUsageExtractor;

static USAGE_EXTRACTOR: OpenAiCompactUsageExtractor = OpenAiCompactUsageExtractor;

impl OpenAiCompactCodec {
    pub fn passthrough_request<'a>(&self, body: &'a [u8]) -> Cow<'a, [u8]> {
        passthrough_bytes(body)
    }

    pub fn passthrough_response_chunk<'a>(&self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        passthrough_bytes(chunk)
    }
}

impl CliCodec for OpenAiCompactCodec {
    fn shape(&self) -> ApiShape {
        ApiShape::OpenAiCompact
    }

    fn extract_hints(&self, _path: &str, body: Option<&[u8]>) -> CliHints {
        let mut hints = body
            .and_then(|b| serde_json::from_slice::<CliHints>(b).ok())
            .unwrap_or_default();
        hints.stream = Some(hints.stream.unwrap_or(false));
        hints
    }

    fn usage_extractor(&self) -> &dyn CliUsageExtractor {
        &USAGE_EXTRACTOR
    }

    fn ttft_event_types(&self) -> &'static [&'static str] {
        &["response.output_text.delta", "response.output_item.added"]
    }
}

impl CliUsageExtractor for OpenAiCompactUsageExtractor {
    fn extract_stream_usage(&self, event_type: &str, data: &[u8]) -> Option<CliUsageDelta> {
        if event_type != "response.completed" {
            return None;
        }
        usage_from_bytes(data).map(|u| CliUsageDelta {
            input_tokens: Some(u.input_tokens),
            output_tokens: Some(u.output_tokens),
            cache_read_input_tokens: Some(u.cache_read_input_tokens),
            cache_creation_input_tokens: Some(u.cache_creation_input_tokens),
        })
    }

    fn extract_response_usage(&self, body: &[u8]) -> Option<CliUsageFull> {
        usage_from_bytes(body)
    }
}

#[derive(Deserialize, Default)]
struct Root {
    #[serde(default)]
    response: Option<Response>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct Response {
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize, Default)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<InputTokenDetails>,
}

#[derive(Deserialize, Default)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}

fn usage_from_bytes(bytes: &[u8]) -> Option<CliUsageFull> {
    let root: Root = serde_json::from_slice(strip_sse_data_prefix(bytes)).ok()?;
    let usage = root.response.and_then(|r| r.usage).or(root.usage)?;
    Some(CliUsageFull {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage
            .input_tokens_details
            .map_or(0, |details| details.cached_tokens),
        cache_creation_input_tokens: 0,
    })
}

fn strip_sse_data_prefix(bytes: &[u8]) -> &[u8] {
    let Ok(s) = std::str::from_utf8(bytes) else {
        return bytes;
    };
    let Some(line) = s.lines().find(|line| line.starts_with("data:")) else {
        return bytes;
    };
    line[5..].trim_start().as_bytes()
}
