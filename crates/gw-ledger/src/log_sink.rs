//! Detailed-log sink — async trait used by `gw-gateway` to ship per-request
//! detailed logs to object storage (S3 / OpenDAL). v1 ships a `StdoutSink`
//! fallback that funnels through `tracing` so dev environments can opt in
//! without standing up a backend.

use async_trait::async_trait;

#[async_trait]
pub trait DetailedLogSink: Send + Sync {
    async fn write(&self, payload: serde_json::Value);
}

pub struct StdoutSink;

#[async_trait]
impl DetailedLogSink for StdoutSink {
    async fn write(&self, payload: serde_json::Value) {
        tracing::info!(target: "gw_detailed_log", "{}", payload);
    }
}

use crate::detailed_stream::MAX_DETAILED_LOG_BYTES;
use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use fred::types::{XCap, XCapKind, XCapTrim};

/// Redis Streams (`XADD`) backed sink for detailed logs (ADR-G6-4).
///
/// `MAXLEN ~ N` keeps the stream bounded in size; older entries fall off as
/// new ones arrive. Oversized payloads (per [`MAX_DETAILED_LOG_BYTES`]) are
/// dropped at the sink with a metric increment — DLQ for failed XADDs is
/// out of scope for v1 (C-δ).
pub struct RedisStreamSink {
    pool: RedisPool,
    stream: String,
    max_len: u64,
}

impl RedisStreamSink {
    pub fn new(pool: RedisPool, stream: &str, max_len: u64) -> Self {
        Self {
            pool,
            stream: stream.to_string(),
            max_len,
        }
    }

    fn cap(&self) -> XCap {
        // `XCap` has no `::new`; use the `TryFrom<(K, T, S)>` tuple form.
        // Inputs are concrete enums + u64, so the conversion is infallible.
        XCap::try_from((XCapKind::MaxLen, XCapTrim::AlmostExact, self.max_len))
            .expect("XCap::try_from infallible for (MaxLen, AlmostExact, u64)")
    }
}

#[async_trait]
impl DetailedLogSink for RedisStreamSink {
    async fn write(&self, payload: serde_json::Value) {
        let bytes = match serde_json::to_vec(&payload) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(target: "gw_ledger", error = %e, "detailed log serialize failed; dropping");
                metrics::counter!("gw_detailed_log_drop_total", "reason" => "serialize_failed")
                    .increment(1);
                return;
            }
        };
        if bytes.len() > MAX_DETAILED_LOG_BYTES {
            metrics::counter!("gw_detailed_log_oversized_total").increment(1);
            return;
        }
        let client = self.pool.next();
        let r: Result<String, _> = client
            .xadd(
                self.stream.as_str(),
                false,
                self.cap(),
                "*",
                ("payload", bytes),
            )
            .await;
        if let Err(e) = r {
            tracing::warn!(
                target: "gw_ledger",
                error = %e,
                stream = %self.stream,
                "detailed log XADD failed; dropping"
            );
            metrics::counter!("gw_detailed_log_drop_total", "reason" => "xadd_failed")
                .increment(1);
        }
    }
}
