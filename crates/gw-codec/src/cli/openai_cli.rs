use super::{CliCodec, CliHints, CliUsageDelta, CliUsageExtractor, CliUsageFull};
use gw_core::protocol::ApiShape;
use serde::Deserialize;

#[derive(Default)]
pub struct OpenAiCliCodec;

static USAGE_EXTRACTOR: OpenAiCliUsageExtractor = OpenAiCliUsageExtractor;

impl CliCodec for OpenAiCliCodec {
    fn shape(&self) -> ApiShape {
        ApiShape::OpenAiCli
    }

    fn extract_hints(&self, _path: &str, body: Option<&[u8]>) -> CliHints {
        body.and_then(|b| serde_json::from_slice(b).ok())
            .unwrap_or_default()
    }

    fn usage_extractor(&self) -> &dyn CliUsageExtractor {
        &USAGE_EXTRACTOR
    }

    fn ttft_event_types(&self) -> &'static [&'static str] {
        &["response.output_text.delta", "response.completed"]
    }
}

#[derive(Default)]
pub struct OpenAiCliUsageExtractor;

impl CliUsageExtractor for OpenAiCliUsageExtractor {
    fn extract_stream_usage(&self, event_type: &str, data: &[u8]) -> Option<CliUsageDelta> {
        if event_type != "response.completed" {
            return None;
        }
        extract_usage(data).map(|u| CliUsageDelta {
            input_tokens: Some(u.input_tokens),
            output_tokens: Some(u.output_tokens),
            cache_read_input_tokens: Some(u.cache_read_input_tokens),
            cache_creation_input_tokens: None,
        })
    }

    fn extract_response_usage(&self, body: &[u8]) -> Option<CliUsageFull> {
        extract_usage(body)
    }
}

#[derive(Deserialize)]
struct Root {
    response: Option<Response>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Response {
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    input_tokens_details: Option<InputTokenDetails>,
}

#[derive(Deserialize)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: u64,
}

fn extract_usage(body: &[u8]) -> Option<CliUsageFull> {
    let root: Root = serde_json::from_slice(body).ok()?;
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
