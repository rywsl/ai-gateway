//! Detailed archive — per-surface, wire-fidelity request/response capture.
//!
//! Spec: `docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md`.
//! Each Pingora filter that completes a surface calls `DetailedArchive::try_send`
//! with a [`CapturedSurface`]; a background flusher pool encodes (raw/lz4/zstd)
//! and XADDs to `usage:detailed`.

use bytes::Bytes;
use gw_core::ids::{ApiKeyId, ModelName, RequestId, TraceId, UserId};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    DownstreamRequest,
    UpstreamRequest,
    UpstreamResponse,
    DownstreamResponse,
    Summary,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::DownstreamRequest => "downstream_request",
            Kind::UpstreamRequest => "upstream_request",
            Kind::UpstreamResponse => "upstream_response",
            Kind::DownstreamResponse => "downstream_response",
            Kind::Summary => "summary",
        }
    }
}

/// JSON payload for `kind = *_request`.
#[derive(Debug, Serialize)]
pub struct RequestPayload {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
    /// Base64-standard encoding of the raw body bytes.
    pub body_b64: String,
}

/// JSON payload for `kind = *_response`.
#[derive(Debug, Serialize)]
pub struct ResponsePayload {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_b64: String,
}

/// JSON payload for `kind = summary`. Matches the surviving fields on the
/// historical `DetailedLogRecord` (body previews removed).
#[derive(Debug, Serialize)]
pub struct SummaryPayload {
    pub user_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub model: Option<ModelName>,
    pub client_protocol: Option<String>,
    pub upstream_protocol: Option<String>,
    pub http_status: u16,
    pub error_code: Option<String>,
    pub is_stream: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub tunnel_resolution: Option<String>,
}

/// Mpsc message: a single surface captured at its trigger point.
/// The flusher pool consumes these, picks an encoding, and XADDs.
pub struct CapturedSurface {
    pub ts_ms: i64,
    pub request_id: RequestId,
    pub trace_id: TraceId,
    pub kind: Kind,
    pub payload: SurfacePayload,
}

pub enum SurfacePayload {
    Request(RequestPayload, Arc<Bytes>),
    Response(ResponsePayload, Arc<Bytes>),
    Summary(SummaryPayload),
}
