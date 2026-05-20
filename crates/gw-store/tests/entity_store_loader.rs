//! Integration: `EntityStore::user_or_load` cold-loads from PG, write-backs to
//! Redis, and the second call hits the cache. Requires Docker.

use gw_store::encrypt::KekCache;
use gw_store::{EntityStore, LoaderCtx};
use gw_store::pg_client;
use gw_store::redis_client::RedisCtx;
use gw_core::ids::UserId;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn entity_store_user_or_load_cold_loads_from_pg() {
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
        "CREATE TABLE users (
            id TEXT PRIMARY KEY,
            payload JSONB NOT NULL,
            version BIGINT NOT NULL DEFAULT 0,
            updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
            deleted_at TIMESTAMPTZ NULL
        )",
    )
    .execute(&pg)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (id, payload, version) VALUES ($1, $2::jsonb, 7)",
    )
    .bind("user-001")
    .bind(serde_json::json!({
        "user_id": "user-001",
        "tier": "free",
        "status": "active"
    }).to_string())
    .execute(&pg)
    .await
    .unwrap();

    let store = EntityStore::with_loader(LoaderCtx {
        redis,
        pg,
        kek: KekCache::empty(),
    });

    let id = UserId::from("user-001");
    let v1 = store.user_or_load(&id).await.expect("cold load should hit pg");
    assert_eq!(v1.version, 7);
    assert_eq!(v1.value.user_id.as_ref(), "user-001");

    // Second call hits the moka cache directly (Arc bumped, same instance).
    let v2 = store.user_or_load(&id).await.expect("second call hits cache");
    assert!(std::sync::Arc::ptr_eq(&v1, &v2));
}
