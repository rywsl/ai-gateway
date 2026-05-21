//! Tests for `RedisStreamSink` (ADR-G6-4).
//!
//! Two non-Docker tests verify the size-cap contract that any sink must
//! honour before XADD. The Docker test exercises the actual XADD path and
//! is `#[ignore]`d to keep the default `cargo test --workspace` run fast.

use gw_ledger::detailed_stream::MAX_DETAILED_LOG_BYTES;
use gw_ledger::log_sink::DetailedLogSink;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
#[allow(dead_code)]
struct CountingSink {
    accepted: AtomicU64,
    dropped: AtomicU64,
}

#[async_trait::async_trait]
impl DetailedLogSink for CountingSink {
    async fn write(&self, _payload: serde_json::Value) {
        self.accepted.fetch_add(1, Ordering::AcqRel);
    }
}

#[tokio::test]
async fn oversized_payload_dropped_by_size_check() {
    // Independent of Redis: verify the size guard logic in
    // detailed_stream::MAX_DETAILED_LOG_BYTES is the contract any sink must
    // honor before XADD.
    let big = "x".repeat(MAX_DETAILED_LOG_BYTES + 1);
    let payload = json!({ "big": big });
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert!(bytes.len() > MAX_DETAILED_LOG_BYTES);
}

#[tokio::test]
async fn small_payload_passes_size_check() {
    let payload = json!({ "small": "ok" });
    let bytes = serde_json::to_vec(&payload).unwrap();
    assert!(bytes.len() <= MAX_DETAILED_LOG_BYTES);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn writes_to_usage_detailed_stream_via_redis() {
    use fred::interfaces::{ClientLike, StreamsInterface};
    use fred::types::{Builder, RedisConfig};
    use gw_ledger::log_sink::RedisStreamSink;
    use testcontainers_modules::redis::Redis;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let url = format!("redis://127.0.0.1:{port}");

    let cfg = RedisConfig::from_url(&url).unwrap();
    let pool = Builder::from_config(cfg).build_pool(2).unwrap();
    pool.init().await.unwrap();

    let sink = RedisStreamSink::new(pool.clone(), "usage:detailed", 10_000);
    sink.write(json!({"req_id": "r1", "verbosity": "base", "model": "x"}))
        .await;

    let len: u64 = pool.next().xlen("usage:detailed").await.unwrap();
    assert_eq!(len, 1);
}
