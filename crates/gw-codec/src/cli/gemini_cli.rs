use gw_core::protocol::ApiShape;
use serde::Deserialize;
use std::borrow::Cow;

use super::{
    passthrough_bytes, CliCodec, CliHints, CliUsageDelta, CliUsageExtractor, CliUsageFull,
};

pub struct GeminiCliCodec;

pub struct GeminiCliUsageExtractor;

static USAGE_EXTRACTOR: GeminiCliUsageExtractor = GeminiCliUsageExtractor;

impl GeminiCliCodec {
    pub fn passthrough_request<'a>(&self, body: &'a [u8]) -> Cow<'a, [u8]> {
        passthrough_bytes(body)
    }

    pub fn passthrough_response_chunk<'a>(&self, chunk: &'a [u8]) -> Cow<'a, [u8]> {
        passthrough_bytes(chunk)
    }
}

impl CliCodec for GeminiCliCodec {
    fn shape(&self) -> ApiShape {
        ApiShape::GeminiCli
    }

    fn extract_hints(&self, path: &str, _body: Option<&[u8]>) -> CliHints {
        CliHints {
            model: extract_path_model(path).map(ToOwned::to_owned),
            stream: Some(path.contains(":streamGenerateContent")),
        }
    }

    fn usage_extractor(&self) -> &dyn CliUsageExtractor {
        &USAGE_EXTRACTOR
    }

    fn ttft_event_types(&self) -> &'static [&'static str] {
        &["data"]
    }
}

impl CliUsageExtractor for GeminiCliUsageExtractor {
    fn extract_stream_usage(&self, event_type: &str, data: &[u8]) -> Option<CliUsageDelta> {
        if event_type != "data" && !event_type.is_empty() {
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
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<UsageMetadata>,
}

#[derive(Deserialize, Default)]
struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u64,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u64,
    #[serde(default, rename = "thoughtsTokenCount")]
    thoughts_token_count: u64,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: u64,
}

fn usage_from_bytes(bytes: &[u8]) -> Option<CliUsageFull> {
    let root: Root = serde_json::from_slice(strip_sse_data_prefix(bytes)).ok()?;
    let usage = root.usage_metadata?;
    Some(CliUsageFull {
        input_tokens: usage.prompt_token_count,
        output_tokens: usage.candidates_token_count + usage.thoughts_token_count,
        cache_read_input_tokens: usage.cached_content_token_count,
        cache_creation_input_tokens: 0,
    })
}

fn extract_path_model(path: &str) -> Option<&str> {
    let after_models = path.split_once("/models/")?.1;
    let without_query = after_models
        .split_once('?')
        .map_or(after_models, |(p, _)| p);
    let model = without_query
        .split_once(':')
        .map_or(without_query, |(m, _)| m);
    (!model.is_empty()).then_some(model)
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
