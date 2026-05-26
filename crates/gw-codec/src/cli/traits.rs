use serde::Deserialize;
use std::borrow::Cow;

pub trait CliUsageExtractor: Send + Sync {
    fn extract_stream_usage(&self, event_type: &str, data: &[u8]) -> Option<CliUsageDelta>;

    fn extract_response_usage(&self, body: &[u8]) -> Option<CliUsageFull>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CliUsageDelta {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CliUsageFull {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct CliHints {
    pub model: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
}

#[inline]
pub fn passthrough_bytes(body: &[u8]) -> Cow<'_, [u8]> {
    Cow::Borrowed(body)
}
