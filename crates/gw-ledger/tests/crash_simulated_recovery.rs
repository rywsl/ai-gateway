//! Integration: register an inflight key, build a UsageRecord but DO NOT
//! let the flusher run (simulates pre-XADD crash), then explicitly invoke
//! the startup reconciler and assert a CrashFallback record lands.
//!
//! This is the single-process surrogate for the full kill-and-restart
//! scenario covered by the soak harness in Task 20 (`make e2e-soak-kill`).
//! It validates invariant I2: "record lost ⇒ inflight key remains ⇒
//! reconciler recovers."
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with:
//!   cargo test -p gw-ledger --test crash_simulated_recovery -- --ignored

use fred::interfaces::KeysInterface;
use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::flusher::{ClearKey, LedgerItem};
use gw_ledger::inflight::{InflightLedger, InflightRegistration};
use gw_ledger::inflight_reconciler;
use gw_ledger::usage::{UsageRecord, UsageStatus, VendorExposureState};
use gw_ledger::Ledger;
use gw_store::redis_client::RedisCtx;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

fn fresh_usage_record(req_id: &str, attempt: u32) -> UsageRecord {
    UsageRecord {
        ts_ms: 0,
        request_id: req_id.to_string(),
        trace_id: String::new(),
        user_id: UserId::from("u"),
        child_id: None,
        api_key_id: None,
        client_protocol: String::new(),
        upstream_protocol: String::new(),
        model: ModelName::from("m"),
        vendor_id: VendorId::from("v"),
        vendor_key_id: KeyId::from("k"),
        status: UsageStatus::Ok,
        error_code: None,
        http_status: 200,
        usage: UsageSnapshot::default(),
        attempt_index: attempt,
        vendor_exposure_state: VendorExposureState::Settled,
        usage_provenance: Default::default(),
        latency_ms: 0,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 0,
        estimate_pricing_version: 0,
        markup_chain_snapshot: MarkupChainSnapshot::identity(),
        operation_id: None,
    }
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn reconciler_emits_crashfallback_for_un_emitted_request() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-crash".to_string();

    // 1. Register an inflight key (as upstream_request_filter would).
    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    inflight
        .register(&InflightRegistration {
            request_id: "req-crashed".to_string(),
            attempt_index: 0,
            user_id: "u".to_string(),
            vendor_key_id: "vk".to_string(),
            freeze_amount: 0,
            prompt_estimate_tokens: 42,
        })
        .await
        .unwrap();

    // 2. Build a Ledger with a paused ticker — emit will queue into mpsc but
    //    the flusher will never fire within the test (3600 s interval).
    let ledger = Ledger::start(
        redis.clone(),
        instance.clone(),
        16,
        16,
        Duration::from_secs(3600), // ticker effectively disabled
    );

    // 3. Emit a record with clear: Some — under normal flow this would clear
    //    the inflight key after flush. We never let it flush.
    ledger
        .emit(LedgerItem {
            record: fresh_usage_record("req-crashed", 0),
            clear: Some(ClearKey {
                request_id: "req-crashed".to_string(),
                attempt_index: 0,
            }),
        })
        .await;

    // 4. Verify the inflight key is still there (flush never happened).
    let key = InflightLedger::test_key(&instance, "req-crashed", 0);
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        1,
        "inflight key must still exist before simulated restart"
    );

    // 5. Simulate restart: drop the Ledger (the queued record dies with it;
    //    the inflight key survives in Redis).
    drop(ledger);

    // 6. Run the startup reconciler — it should scan the residual inflight key
    //    and emit a CrashFallback UsageRecord per spec §4 crash matrix.
    let emitted_count = Arc::new(AtomicUsize::new(0));
    let emitted_count_for_cb = emitted_count.clone();

    let recovered = inflight_reconciler::run(
        &redis.pool,
        &instance,
        move |rec: UsageRecord| -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> {
            let counter = emitted_count_for_cb.clone();
            Box::pin(async move {
                assert_eq!(
                    rec.vendor_exposure_state,
                    VendorExposureState::CrashFallback,
                    "reconciler must emit CrashFallback state"
                );
                assert_eq!(
                    rec.request_id, "req-crashed",
                    "reconciler must preserve request_id from inflight key"
                );
                assert_eq!(
                    rec.usage.input_tokens, 42,
                    "reconciler uses prompt_estimate_tokens as input_tokens"
                );
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        },
    )
    .await
    .unwrap();

    assert_eq!(recovered, 1, "reconciler returned count = 1");
    assert_eq!(
        emitted_count.load(Ordering::SeqCst),
        1,
        "callback invoked exactly once"
    );

    // 7. Reconciler DELs the key after successful emit (per spec §4 + reconciler impl).
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        0,
        "reconciler must DEL the inflight key after CrashFallback emit"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn reconciler_recovers_both_attempts_after_retry_crash() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-retry-crash".to_string();

    // Simulate: attempt 0 registered, retry-spawn enqueued attempt-0 record but
    // didn't run; attempt 1 registered and also enqueued. Crash. Reconciler
    // should emit CrashFallback for BOTH attempts (spec §4 crash matrix, retry path).
    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    for attempt in 0..=1u32 {
        inflight
            .register(&InflightRegistration {
                request_id: "req-retry-crashed".to_string(),
                attempt_index: attempt,
                user_id: "u".to_string(),
                vendor_key_id: "vk".to_string(),
                freeze_amount: 0,
                prompt_estimate_tokens: 42,
            })
            .await
            .unwrap();
    }

    // Both inflight keys are present; no Ledger was used — both are residual from
    // the simulated crash. Reconciler must scan and emit CrashFallback for each.
    let emitted_count = Arc::new(AtomicUsize::new(0));
    let emitted_attempts: Arc<std::sync::Mutex<Vec<u32>>> = Default::default();
    let emitted_count_for_cb = emitted_count.clone();
    let emitted_attempts_for_cb = emitted_attempts.clone();

    let recovered = inflight_reconciler::run(
        &redis.pool,
        &instance,
        move |rec: UsageRecord| -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> {
            let counter = emitted_count_for_cb.clone();
            let attempts = emitted_attempts_for_cb.clone();
            Box::pin(async move {
                assert_eq!(
                    rec.vendor_exposure_state,
                    VendorExposureState::CrashFallback,
                    "reconciler must emit CrashFallback state"
                );
                assert_eq!(
                    rec.request_id, "req-retry-crashed",
                    "reconciler must preserve request_id from inflight key"
                );
                attempts.lock().unwrap().push(rec.attempt_index);
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        },
    )
    .await
    .unwrap();

    assert_eq!(
        recovered, 2,
        "reconciler returned count = 2 (one per attempt)"
    );
    assert_eq!(
        emitted_count.load(Ordering::SeqCst),
        2,
        "callback invoked exactly twice"
    );

    let mut got = emitted_attempts.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, vec![0, 1], "both attempt indices recovered");

    // Both inflight keys DEL'd after successful emit (spec §4 + reconciler impl).
    for attempt in 0..=1u32 {
        let key = InflightLedger::test_key(&instance, "req-retry-crashed", attempt);
        assert_eq!(
            redis.pool.exists::<u64, _>(&key).await.unwrap(),
            0,
            "reconciler must DEL inflight key for attempt {attempt} after CrashFallback emit"
        );
    }
}
