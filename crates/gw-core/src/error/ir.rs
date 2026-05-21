use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    ClientBadRequest,
    ClientAuth,
    ClientQuota,
    UpstreamError,
    UpstreamTimeout,
    UpstreamOverloaded,
    GatewayInternal,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
pub struct ErrorIR<'a> {
    pub category: ErrorCategory,
    pub status: u16,
    pub upstream_status: Option<u16>,
    pub code: Cow<'a, str>,
    pub message: Cow<'a, str>,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_raw: Option<&'a serde_json::value::RawValue>,
    pub request_id: &'a str,
    pub retry_after_secs: Option<u32>,
}
