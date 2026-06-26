//! Detailed archive — per-surface request/response capture.
//!
//! Spec: `docs/spec/06-billing-ledger.md`.
//! Each Pingora filter that completes a surface calls `DetailedArchive::try_send`
//! with a [`CapturedSurface`]; a background flusher pool encodes (raw/lz4/zstd)
//! and XADDs to `usage:detailed`. `gw-proxy` applies sensitive-filter archive
//! masking before this boundary when the global policy has enabled archive rules.

use crate::encoding::Encoding;
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
    /// True when the archive accumulator exceeded the hard cap and further
    /// bytes were dropped. Downstream consumers should treat the body as
    /// incomplete when `archive_truncated = true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archive_truncated: bool,
    /// Number of bytes dropped after the hard cap was hit. `0` when
    /// `archive_truncated` is false.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub archive_dropped: u64,
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
    pub attempt_index: u32,
    pub kind: Kind,
    pub payload: SurfacePayload,
}

pub enum SurfacePayload {
    Request(RequestPayload, Arc<Bytes>),
    Response(ResponsePayload, Arc<Bytes>),
    Summary(SummaryPayload),
}

// ── Flusher pool ─────────────────────────────────────────────────────────────

use crate::encoding::{encode, CompressionCfg};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use fred::types::{XCap, XCapKind, XCapTrim};
use tokio::sync::mpsc;

const DETAILED_ARCHIVE_WORKER_NAMES: [&str; 16] = [
    "detailed_archive_worker_0",
    "detailed_archive_worker_1",
    "detailed_archive_worker_2",
    "detailed_archive_worker_3",
    "detailed_archive_worker_4",
    "detailed_archive_worker_5",
    "detailed_archive_worker_6",
    "detailed_archive_worker_7",
    "detailed_archive_worker_8",
    "detailed_archive_worker_9",
    "detailed_archive_worker_10",
    "detailed_archive_worker_11",
    "detailed_archive_worker_12",
    "detailed_archive_worker_13",
    "detailed_archive_worker_14",
    "detailed_archive_worker_15",
];

/// Boot-time config; mirrors the `[detailed_log]` table in `config/default.toml`.
#[derive(Debug, Clone)]
pub struct DetailedArchiveCfg {
    pub stream: String,
    pub maxlen: u64,
    pub channel_capacity: usize,
    pub workers: usize,
    pub single_payload_soft_cap_bytes: usize,
    pub compression: CompressionCfg,
}

/// Sender handle held by `LlmGateway`; cheaply cloneable.
#[derive(Clone)]
pub struct DetailedArchive {
    tx: mpsc::Sender<CapturedSurface>,
}

impl DetailedArchive {
    pub fn start(
        redis: RedisPool,
        cfg: DetailedArchiveCfg,
        token: gw_runtime::ShutdownToken,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<CapturedSurface>(cfg.channel_capacity);
        let workers = cfg.workers.max(1).min(DETAILED_ARCHIVE_WORKER_NAMES.len());
        let shared_rx = std::sync::Arc::new(tokio::sync::Mutex::new(rx));
        for i in 0..workers {
            let rx = shared_rx.clone();
            let cfg = cfg.clone();
            let redis = redis.clone();
            let name = DETAILED_ARCHIVE_WORKER_NAMES[i];
            drop(gw_runtime::supervisor::supervised_spawn(
                name,
                gw_runtime::supervisor::policy_archive(),
                token.child(),
                move |_t| {
                    let rx = rx.clone();
                    let cfg = cfg.clone();
                    let redis = redis.clone();
                    async move {
                        worker(rx, redis, cfg).await;
                        Ok(())
                    }
                },
            ));
        }
        Self { tx }
    }

    /// Best-effort enqueue. On a full channel we drop + metric. Never blocks.
    pub fn try_send(&self, surface: CapturedSurface) {
        let kind_label = surface.kind.as_str();
        if self.tx.try_send(surface).is_err() {
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "channel_full",
                "kind" => kind_label,
            )
            .increment(1);
        }
    }
}

fn cap(maxlen: u64) -> XCap {
    XCap::try_from((XCapKind::MaxLen, XCapTrim::AlmostExact, maxlen))
        .expect("XCap::try_from infallible for (MaxLen, AlmostExact, u64)")
}

async fn worker(
    rx: std::sync::Arc<tokio::sync::Mutex<mpsc::Receiver<CapturedSurface>>>,
    redis: RedisPool,
    cfg: DetailedArchiveCfg,
) {
    loop {
        let surface = {
            let mut guard = rx.lock().await;
            match guard.recv().await {
                Some(s) => s,
                None => return,
            }
        };
        handle(surface, &redis, &cfg).await;
    }
}

async fn handle(surface: CapturedSurface, redis: &RedisPool, cfg: &DetailedArchiveCfg) {
    let CapturedSurface {
        ts_ms,
        request_id,
        trace_id,
        attempt_index,
        kind,
        payload,
    } = surface;
    let kind_label = kind.as_str();

    let json_bytes = match serialize_payload(payload, cfg.single_payload_soft_cap_bytes, kind_label)
    {
        Some(b) => b,
        None => return,
    };

    let pre_compress_len = json_bytes.len();
    metrics::histogram!("gw_detailed_log_pre_compress_bytes", "kind" => kind_label)
        .record(pre_compress_len as f64);

    let start = std::time::Instant::now();
    let (encoding, encoded) = match encode(json_bytes, &cfg.compression) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, "detailed archive encode failed; dropping");
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "compress_failed",
                "kind" => kind_label,
            )
            .increment(1);
            return;
        }
    };
    let elapsed = start.elapsed().as_secs_f64();
    metrics::histogram!("gw_detailed_log_compress_seconds", "encoding" => encoding.as_str())
        .record(elapsed);
    metrics::histogram!(
        "gw_detailed_log_payload_bytes",
        "kind" => kind_label,
        "encoding" => encoding.as_str(),
    )
    .record(encoded.len() as f64);

    // Payload is binary; use a separate xadd call shape if needed, but
    // for now encode as base64 string so all fields stay string-typed.
    // The consumer already expects binary in `encoded`; we keep it as bytes
    // by using the Vec<(K, V)> where V: Into<RedisValue> — fred 9.x accepts
    // Vec<(&str, Vec<u8>)> for the fields argument.
    let fields_with_payload = stream_fields(
        ts_ms,
        &request_id,
        &trace_id,
        attempt_index,
        kind,
        encoding,
        encoded.as_slice(),
    );

    let client = redis.next();
    let r: Result<String, _> = client
        .xadd(
            cfg.stream.as_str(),
            false,
            cap(cfg.maxlen),
            "*",
            fields_with_payload,
        )
        .await;
    if let Err(e) = r {
        tracing::warn!(
            target: "gw_ledger",
            error = %e,
            stream = %cfg.stream,
            "detailed archive XADD failed; dropping"
        );
        metrics::counter!(
            "gw_detailed_log_drop_total",
            "reason" => "xadd_failed",
            "kind" => kind_label,
        )
        .increment(1);
    } else {
        metrics::counter!(
            "gw_detailed_log_emit_total",
            "kind" => kind_label,
            "encoding" => encoding.as_str(),
        )
        .increment(1);
    }
}

fn stream_fields(
    ts_ms: i64,
    request_id: &RequestId,
    trace_id: &TraceId,
    attempt_index: u32,
    kind: Kind,
    encoding: Encoding,
    payload: &[u8],
) -> Vec<(&'static str, fred::types::RedisValue)> {
    vec![
        ("ts_ms", ts_ms.to_string().into()),
        ("request_id", request_id.as_ref().to_string().into()),
        ("trace_id", trace_id.as_ref().to_string().into()),
        ("attempt_index", attempt_index.to_string().into()),
        ("kind", kind.as_str().to_string().into()),
        ("encoding", encoding.as_str().to_string().into()),
        ("payload", payload.into()),
    ]
}

fn serialize_payload(
    payload: SurfacePayload,
    soft_cap: usize,
    kind_label: &'static str,
) -> Option<Vec<u8>> {
    let result = match payload {
        SurfacePayload::Request(mut req, body) => {
            req.body_b64 = B64.encode(body.as_ref());
            serde_json::to_vec(&req)
        }
        SurfacePayload::Response(mut resp, body) => {
            resp.body_b64 = B64.encode(body.as_ref());
            serde_json::to_vec(&resp)
        }
        SurfacePayload::Summary(s) => serde_json::to_vec(&s),
    };
    match result {
        Ok(v) if v.len() > soft_cap => {
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "oversize",
                "kind" => kind_label,
            )
            .increment(1);
            None
        }
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, "detailed archive serialize failed");
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "serialize_failed",
                "kind" => kind_label,
            )
            .increment(1);
            None
        }
    }
}

impl DetailedArchive {
    /// Test-only: spawn workers that immediately drain the channel slowly
    /// (no Redis call). Lets us cover the drop-on-full path without Docker.
    #[doc(hidden)]
    pub fn for_test_with_dummy_workers(cfg: DetailedArchiveCfg) -> Self {
        let (tx, mut rx) = mpsc::channel::<CapturedSurface>(cfg.channel_capacity);
        // Drain the channel slowly so the second `try_send` lands on a full
        // queue.
        drop(gw_runtime::supervisor::one_shot_spawn(
            "detailed_archive_test_drain",
            || async move {
                while let Some(_s) = rx.recv().await {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Ok(())
            },
        ));
        Self { tx }
    }
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::Encoding;

    fn field_value<'a>(
        fields: &'a [(&'static str, fred::types::RedisValue)],
        name: &str,
    ) -> Option<&'a fred::types::RedisValue> {
        fields.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
    }

    #[test]
    fn stream_fields_include_attempt_index() {
        let fields = stream_fields(
            1716345678901,
            &RequestId::from("req-1"),
            &TraceId::from("trace-1"),
            2,
            Kind::UpstreamResponse,
            Encoding::Raw,
            b"{}".as_slice(),
        );

        assert_eq!(
            field_value(&fields, "attempt_index"),
            Some(&fred::types::RedisValue::from("2"))
        );
    }
}
