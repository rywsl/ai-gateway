//! G6 / ADR-G6-1 — `apply_entity_event` must DEL the Redis L1 entry
//! (`entity:{ns}:{id}`) before invalidating Moka so multi-pod deployments
//! don't retain stale entries up to the L1 TTL.
//!
//! Requires Docker (testcontainers Redis + Postgres). Postgres is needed
//! only because `LoaderCtx` carries a real `PgPool`; this test never
//! reads from PG.

use fred::interfaces::{HashesInterface, KeysInterface};
use fred::types::RedisMap;
use gw_store::encrypt::KekCache;
use gw_store::loader::LoaderCtx;
use gw_store::pg_client;
use gw_store::redis_client::RedisCtx;
use gw_store::stream_consumer::apply_inline_for_test_helper;
use gw_store::EntityStore;
use std::sync::Arc;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn apply_entity_event_deletes_redis_l1() {
    // 1. Boot testcontainers Postgres + Redis, build a real LoaderCtx.
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

    let loader_ctx = LoaderCtx {
        redis: redis.clone(),
        pg,
        kek: KekCache::empty(),
    };

    // 2. Build EntityStore wired with the loader so apply_entity_event sees Redis.
    let store = Arc::new(EntityStore::with_loader(loader_ctx));

    // 3. Write `entity:apikey:ak_x` directly via fred (mimic load_entity write-back).
    let key = "entity:apikey:ak_x";
    let client = redis.pool.next();
    let map: RedisMap = vec![
        ("v".to_string(), "1".to_string()),
        ("enc".to_string(), "json".to_string()),
        ("d".to_string(), "{\"stub\":true}".to_string()),
    ]
    .into_iter()
    .collect();
    let _: () = client.hset(key, map).await.unwrap();

    // 4. assert exists == 1.
    let exists_before: i64 = client.exists(key).await.unwrap();
    assert_eq!(exists_before, 1, "precondition: entity:apikey:ak_x must exist");

    // 5. apply_entity_event(&store, "apikey", "ak_x").
    apply_inline_for_test_helper(&store, "apikey", "ak_x").await;

    // 6. assert exists == 0.
    let exists_after: i64 = client.exists(key).await.unwrap();
    assert_eq!(
        exists_after, 0,
        "apply_entity_event must DEL entity:apikey:ak_x"
    );
}
