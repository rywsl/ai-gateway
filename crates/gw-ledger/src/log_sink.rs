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
