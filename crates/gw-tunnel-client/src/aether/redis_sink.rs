#[async_trait::async_trait]
pub trait HeartbeatSink: Send + Sync + 'static {
    async fn push(&self, event: HeartbeatEvent);
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct HeartbeatEvent {
    pub node_id: String,
    pub heartbeat_session_id: String,
    pub heartbeat_id: u64,
    pub received_at_ms: u64,
    pub active_connections: u64,
    pub total_requests: u64,
    pub avg_latency_ms: f64,
    pub failed_requests: u64,
    pub dns_failures: u64,
    pub stream_errors: u64,
    pub proxy_version: String,
}

#[derive(Default)]
pub struct CollectingSink {
    pub events: tokio::sync::Mutex<Vec<HeartbeatEvent>>,
}

#[async_trait::async_trait]
impl HeartbeatSink for CollectingSink {
    async fn push(&self, event: HeartbeatEvent) {
        self.events.lock().await.push(event);
    }
}

pub struct NoopSink;

#[async_trait::async_trait]
impl HeartbeatSink for NoopSink {
    async fn push(&self, _event: HeartbeatEvent) {}
}
