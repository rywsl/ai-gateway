//! Entity loader integration test (testcontainers: Redis + Postgres).
//!
//! Marked `#[ignore]` so the default `cargo test` run passes on machines
//! without Docker. Run explicitly with `cargo test -p gw-store -- --ignored`.

use fred::interfaces::HashesInterface;
use fred::types::RedisMap;
use gw_core::ids::ModelName;
use gw_store::encrypt::KekCache;
use gw_store::loader::{load_entity, load_estimate_pricing, LoaderCtx};
use gw_store::pg_client;
use gw_store::redis_client::RedisCtx;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn cold_load_from_pgsql_writes_back_to_redis() {
    let redis_node = Redis::default().start().await.unwrap();
    let pg_node = Postgres::default().start().await.unwrap();
    let redis_port = redis_node.get_host_port_ipv4(6379).await.unwrap();
    let pg_port = pg_node.get_host_port_ipv4(5432).await.unwrap();

    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
        .await
        .unwrap();
    let pg = pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE t (id TEXT PRIMARY KEY, payload JSONB NOT NULL, version BIGINT NOT NULL DEFAULT 0)",
    )
    .execute(&pg)
    .await
    .unwrap();
    sqlx::query("INSERT INTO t VALUES ('u1', '{\"name\":\"alice\"}'::jsonb, 5)")
        .execute(&pg)
        .await
        .unwrap();

    let ctx = LoaderCtx {
        redis: redis.clone(),
        pg,
        kek: KekCache::empty(),
    };

    let v = load_entity(
        &ctx,
        "user",
        "u1",
        "SELECT payload, version FROM t WHERE id = $1",
        |val| -> anyhow::Result<String> {
            Ok(val
                .get("name")
                .and_then(|s| s.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing name"))?
                .to_string())
        },
    )
    .await
    .unwrap();

    assert_eq!(v.version, 5);
    assert_eq!(&*v.value, "alice");

    // Second load should hit Redis (write-back validated implicitly).
    let v2 = load_entity(
        &ctx,
        "user",
        "u1",
        "SELECT payload, version FROM t WHERE id = $1",
        |val| -> anyhow::Result<String> {
            Ok(val
                .get("name")
                .and_then(|s| s.as_str())
                .ok_or_else(|| anyhow::anyhow!("missing name"))?
                .to_string())
        },
    )
    .await
    .unwrap();
    assert_eq!(v2.version, 5);
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn estimate_pricing_prefers_go_redis_envelope_over_stale_pgsql() {
    let redis_node = Redis::default().start().await.unwrap();
    let pg_node = Postgres::default().start().await.unwrap();
    let redis_port = redis_node.get_host_port_ipv4(6379).await.unwrap();
    let pg_port = pg_node.get_host_port_ipv4(5432).await.unwrap();

    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
        .await
        .unwrap();
    let pg = pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .unwrap();

    sqlx::query(
        "CREATE TABLE estimate_pricings (id TEXT PRIMARY KEY, payload JSONB NOT NULL, version BIGINT NOT NULL DEFAULT 0, deleted_at TIMESTAMPTZ NULL)",
    )
    .execute(&pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO estimate_pricings VALUES ('gpt-4', '{\"model\":\"gpt-4\",\"conservative_per_token_micro\":1,\"safety_factor\":1.0,\"default_max_tokens\":1,\"hard_cap_tokens\":1}'::jsonb, 1, NULL)",
    )
    .execute(&pg)
    .await
    .unwrap();

    let payload = serde_json::to_vec(&serde_json::json!({
        "model": "gpt-4",
        "conservative_per_token_micro": 60,
        "safety_factor": 1.1,
        "default_max_tokens": 4096,
        "hard_cap_tokens": 200000
    }))
    .unwrap();
    let map: RedisMap = vec![
        ("v".to_string(), "35".to_string()),
        ("enc".to_string(), "json".to_string()),
        ("d".to_string(), String::from_utf8(payload).unwrap()),
    ]
    .into_iter()
    .collect();
    let _: () = redis
        .pool
        .next()
        .hset("entity:estimate_pricing:gpt-4", map)
        .await
        .unwrap();

    let ctx = LoaderCtx {
        redis,
        pg,
        kek: KekCache::empty(),
    };

    let loaded = load_estimate_pricing(&ctx, &ModelName::from("gpt-4"))
        .await
        .unwrap();

    assert_eq!(loaded.version, 35);
    assert_eq!(loaded.value.conservative_per_token_micro, 60);
    assert_eq!(loaded.value.default_max_tokens, 4096);
}
