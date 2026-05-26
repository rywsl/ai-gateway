use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, StreamsInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig, RedisValue};
use gw_task::{
    redis_contract::RedisTaskContract, TaskContract, TaskId, TaskKind, TaskRecord, TaskStatus,
};
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

async fn make_pool(url: &str) -> RedisPool {
    let cfg = RedisConfig::from_url(url).expect("parse redis url");
    let pool = RedisPool::new(
        cfg,
        Some(PerformanceConfig::default()),
        Some(ConnectionConfig::default()),
        Some(ReconnectPolicy::default()),
        1,
    )
    .expect("RedisPool");
    pool.init().await.expect("connect");
    pool
}

async fn docker_pool() -> RedisPool {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let pool = make_pool(&format!("redis://127.0.0.1:{port}")).await;
    std::mem::forget(node);
    pool
}

fn sample_record() -> TaskRecord {
    let now = 1_700_000_000_000;
    TaskRecord {
        id: TaskId::new_v4(),
        schema_version: 1,
        kind: TaskKind::Generic,
        status: TaskStatus::Queued,
        user_id: "user-1".to_string(),
        child_id: None,
        api_key_id: "key-1".to_string(),
        client_protocol: "openai".to_string(),
        vendor_id: "vendor-1".to_string(),
        vendor_key_hint: Some("vk-1".to_string()),
        model: Some("model-1".to_string()),
        created_at_ms: now,
        updated_at_ms: now,
        submitted_ts_ms: now,
        expires_at_ms: now + 86_400_000,
        claimed_ts_ms: None,
        ended_ts_ms: None,
        lease_until_ms: Some(now + 60_000),
        worker_id: None,
        trace_id: Some("trace-1".to_string()),
        request_id: Some("req-1".to_string()),
        provider_task_id: None,
        provider_first_response_ms: None,
        input_payload: serde_json::json!({"prompt":"hello"}),
        output_payload: None,
        error_message: None,
        frozen_micro: 123,
        actual_micro: None,
        duration_seconds: None,
        billing_record_id: None,
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn test_submit_stores_and_queues() {
    let pool = docker_pool().await;
    let contract = RedisTaskContract::new(pool.clone()).await.unwrap();
    let record = sample_record();

    contract.submit(record.clone()).await.unwrap();

    let hash: std::collections::HashMap<String, String> = pool
        .next()
        .hgetall(format!("entity:task:{}", record.id))
        .await
        .unwrap();
    assert_eq!(hash.get("id"), Some(&record.id.to_string()));
    assert_eq!(hash.get("schema_version"), Some(&"1".to_string()));
    assert_eq!(hash.get("status"), Some(&"queued".to_string()));
    assert_eq!(
        hash.get("input_payload"),
        Some(&serde_json::json!({"prompt":"hello"}).to_string())
    );

    let entries: RedisValue = pool
        .next()
        .xrange("tasks:queue", "-", "+", None)
        .await
        .unwrap();
    assert!(!matches!(entries, RedisValue::Array(values) if values.is_empty()));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn test_load_returns_task_record() {
    let pool = docker_pool().await;
    let contract = RedisTaskContract::new(pool).await.unwrap();
    let record = sample_record();
    contract.submit(record.clone()).await.unwrap();

    let loaded = contract.load(&record.id).await.unwrap().unwrap();
    assert_eq!(loaded, record);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn test_request_cancel_xadd() {
    let pool = docker_pool().await;
    let contract = RedisTaskContract::new(pool.clone()).await.unwrap();
    let mut record = sample_record();
    record.status = TaskStatus::Running;
    contract.submit(record.clone()).await.unwrap();

    let cancelled = contract
        .request_cancel(&record.id, gw_task::CancelReason::Client)
        .await
        .unwrap();
    assert_eq!(cancelled.status, TaskStatus::Cancelling);

    let entries: RedisValue = pool
        .next()
        .xrange("tasks:cancel", "-", "+", None)
        .await
        .unwrap();
    assert!(!matches!(entries, RedisValue::Array(values) if values.is_empty()));
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn test_claim_xreadgroup() {
    let pool = docker_pool().await;
    let contract = RedisTaskContract::new(pool).await.unwrap();
    let record = sample_record();
    contract.submit(record.clone()).await.unwrap();

    let claimed = contract.claim("worker-1").await.unwrap().unwrap();
    assert_eq!(claimed.record.id, record.id);
    assert_eq!(claimed.record.status, TaskStatus::Running);
    assert_eq!(claimed.record.worker_id.as_deref(), Some("worker-1"));
}
