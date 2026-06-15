use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HeartbeatData {
    pub node_id: String,
    pub heartbeat_session_id: String,
    pub heartbeat_id: u64,
    pub active_connections: u64,
    pub total_requests: u64,
    pub avg_latency_ms: Option<f64>,
    pub failed_requests: u64,
    pub dns_failures: u64,
    pub stream_errors: u64,
    pub proxy_metadata: ProxyMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ProxyMetadata {
    pub version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct HeartbeatAck {
    pub heartbeat_id: u64,
}
