//! Startup reconciler tests. Requires Redis.

use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, HashesInterface, KeysInterface};
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};

async fn make_pool() -> RedisPool {
    let url =
        std::env::var("GW_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let cfg = RedisConfig::from_url(&url).expect("parse url");
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

#[tokio::test]
#[ignore = "requires Redis"]
async fn reconciler_emits_and_deletes_own_residuals() {
    let pool = make_pool().await;
    let inst = format!("inst-recon-{}", std::process::id());

    // Pre-write 2 residual keys under own instance_id
    for i in 0..2 {
        let key = format!("billing:inflight:{inst}:req-{i}:0");
        let fields: Vec<(&str, String)> = vec![
            ("state", "sent".to_string()),
            ("ts_sent_ms", "1700000000000".to_string()),
            ("user_id", format!("u-{i}")),
            ("vendor_key_id", "vk-1".to_string()),
            ("freeze_amount", "0".to_string()),
            ("prompt_estimate_tokens", "42".to_string()),
        ];
        let _: () = pool.hset(&key, fields).await.unwrap();
        let _: () = pool.expire(&key, 86_400).await.unwrap();
    }

    let captured: std::sync::Arc<std::sync::Mutex<Vec<gw_ledger::usage::UsageRecord>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture_clone = captured.clone();
    let emit = move |r: gw_ledger::usage::UsageRecord| {
        let c = capture_clone.clone();
        Box::pin(async move {
            c.lock().unwrap().push(r);
            Ok(())
        })
            as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
    };

    let recovered = gw_ledger::inflight_reconciler::run(&pool, &inst, emit)
        .await
        .unwrap();
    assert_eq!(recovered, 2);

    {
        let recs = captured.lock().unwrap();
        assert_eq!(recs.len(), 2);
        for r in recs.iter() {
            assert!(matches!(
                r.vendor_exposure_state,
                gw_ledger::usage::VendorExposureState::CrashFallback
            ));
            assert_eq!(r.usage.input_tokens, 42);
            assert_eq!(r.usage.output_tokens, 0);
            assert!(matches!(
                r.usage_provenance.input,
                gw_core::usage::ProvenanceTag::CrashFallback
            ));
        }
    }

    // Keys must be deleted post-emit
    for i in 0..2 {
        let key = format!("billing:inflight:{inst}:req-{i}:0");
        let exists: bool = pool.exists(&key).await.unwrap();
        assert!(!exists, "residual key {key} should be deleted");
    }
}

#[tokio::test]
#[ignore = "requires Redis"]
async fn reconciler_does_not_touch_other_instances() {
    let pool = make_pool().await;
    let inst_self = format!("inst-self-{}", std::process::id());
    let inst_other = format!("inst-other-{}", std::process::id());
    let other_key = format!("billing:inflight:{inst_other}:req-x:0");
    let fields: Vec<(&str, String)> = vec![("state", "sent".to_string())];
    let _: () = pool.hset(&other_key, fields).await.unwrap();
    let _: () = pool.expire(&other_key, 86_400).await.unwrap();

    let dummy_emit = |_: gw_ledger::usage::UsageRecord| {
        Box::pin(async { Ok(()) })
            as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>>
    };
    let recovered = gw_ledger::inflight_reconciler::run(&pool, &inst_self, dummy_emit)
        .await
        .unwrap();
    assert_eq!(recovered, 0);

    // The other instance's key must still exist
    let exists: bool = pool.exists(&other_key).await.unwrap();
    assert!(exists);

    // Cleanup
    let _: () = pool.del(&[other_key]).await.unwrap();
}
