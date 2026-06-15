use gw_tunnel_client::aether::redis_sink::{CollectingSink, HeartbeatEvent, HeartbeatSink};
use std::sync::Arc;

#[tokio::test]
async fn collecting_sink_records_event() {
    let sink = Arc::new(CollectingSink::default());
    let sink_trait: Arc<dyn HeartbeatSink> = sink.clone();

    sink_trait
        .push(HeartbeatEvent {
            node_id: "n".into(),
            heartbeat_session_id: "s".into(),
            heartbeat_id: 1,
            received_at_ms: 2,
            active_connections: 3,
            total_requests: 4,
            avg_latency_ms: 5.0,
            failed_requests: 6,
            dns_failures: 7,
            stream_errors: 8,
            proxy_version: "v".into(),
        })
        .await;

    assert_eq!(sink.events.lock().await.len(), 1);
}
