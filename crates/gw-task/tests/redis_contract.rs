use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, KeysInterface, StreamsInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig, RedisValue};
use gw_task::{
    redis_contract::RedisTaskContract, ClientMode, TaskContract, TaskId, TaskKind, TaskRecord,
    TaskStatus, UpstreamMode,
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
        schema_version: 2,
        kind: TaskKind::Generic,
        status: TaskStatus::Queued,
        user_id: "user-1".to_string(),
        child_id: None,
        api_key_id: "key-1".to_string(),
        vendor_id: "vendor-1".to_string(),
        model: Some("model-1".to_string()),
        client_mode: ClientMode::Async,
        upstream_mode: UpstreamMode::AsyncLro,
        global_model_id: Some("model-1".to_string()),
        provider_model_id: Some("provider-model-1".to_string()),
        endpoint_id: Some("endpoint-1".to_string()),
        provider_key_id: Some("vk-1".to_string()),
        profile_id: Some("profile-1".to_string()),
        profile_revision_id: Some("profile-rev-1".to_string()),
        profile_version: Some(1),
        binding_id: Some("binding-1".to_string()),
        binding_revision_id: Some("binding-rev-1".to_string()),
        binding_version: Some(1),
        effective_template_hash: Some("sha256:test".to_string()),
        poll_context: None,
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
        input_payload: serde_json::json!({
            "body": {"model": "gpt-4.1-mini", "messages": [{"role": "user", "content": "hi"}]},
            "ir_wire_format": "openai:chat",
            "ir_wire_content_type": "application/json",
            "ir_wire_body": "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
        }),
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
    assert_eq!(hash.get("schema_version"), Some(&"2".to_string()));
    assert_eq!(hash.get("status"), Some(&"queued".to_string()));
    let stored_payload: serde_json::Value =
        serde_json::from_str(hash.get("input_payload").expect("input_payload hash field"))
            .expect("input_payload decodes");
    assert_eq!(
        gw_task::task_ir_wire_body(&stored_payload),
        Ok(Some(
            "{\"model\":\"gpt-4.1-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}]}"
        ))
    );
    assert_eq!(
        gw_task::task_ir_wire_format(&stored_payload),
        Some("openai:chat")
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

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn test_settle_releases_freeze_deducts_and_emits_usage() {
    let pool = docker_pool().await;
    let contract = RedisTaskContract::new(pool.clone()).await.unwrap();
    let record = sample_record();

    contract
        .freeze_task(
            &record.user_id,
            &record.id,
            record.frozen_micro,
            Some(10_000),
        )
        .await
        .unwrap();
    contract.submit(record.clone()).await.unwrap();
    contract.claim("worker-1").await.unwrap().unwrap();
    contract
        .settle(
            &record.id,
            TaskStatus::Succeeded,
            Some(serde_json::json!({"ok": true})),
            None,
            77,
        )
        .await
        .unwrap();

    let task_frozen: Option<i64> = pool
        .next()
        .get(format!("counter:user:{}:task_frozen_micro", record.user_id))
        .await
        .unwrap();
    let deducted: Option<i64> = pool
        .next()
        .get(format!("counter:user:{}:deducted_micro", record.user_id))
        .await
        .unwrap();
    let usage_len: i64 = pool.next().xlen("usage:records").await.unwrap();

    assert_eq!(task_frozen.unwrap_or_default(), 0);
    assert_eq!(deducted.unwrap_or_default(), 77);
    assert_eq!(usage_len, 1);
}
