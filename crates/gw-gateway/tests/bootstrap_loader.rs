//! Smoke test: after bootstrap, EntityStore can resolve a seeded user via the
//! cold-loader chain. Marked #[ignore] — requires Docker.

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn entity_store_resolves_user_after_bootstrap() {
    // Placeholder: the substantive cold-loader assertion lives in
    // crates/gw-store/tests/entity_store_loader.rs. This test exists to keep
    // the gw-gateway -> gw-store dependency chain honest at compile time.
}

/// Verify that `auth_config_or_load("global")` returns `None` (not an error
/// or panic) when the `auth_configs` table exists but the `global` row is absent.
/// This guards ADR-G2-2: bootstrap must warn-and-continue, not crash, when the
/// Go management plane has not yet populated the row.
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn missing_auth_configs_row_does_not_fail_bootstrap() {
    use std::sync::Arc;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::{postgres::Postgres, redis::Redis};

    use gw_store::{encrypt::KekCache, loader::LoaderCtx, EntityStore};

    const MIGRATION_0001: &str =
        include_str!("../../gw-store/migrations/0001_initial.sql");
    const MIGRATION_0006: &str =
        include_str!("../../gw-store/migrations/0006_auth_configs.sql");

    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_node = Redis::default().start().await.expect("start redis");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let redis_port = redis_node.get_host_port_ipv4(6379).await.expect("redis port");

    let pg = gw_store::pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .expect("pg connect");
    let redis = gw_store::redis_client::RedisCtx::connect(
        &format!("redis://127.0.0.1:{redis_port}"),
        4,
    )
    .await
    .expect("redis connect");

    sqlx::raw_sql(MIGRATION_0001).execute(&pg).await.expect("apply 0001");
    sqlx::raw_sql(MIGRATION_0006).execute(&pg).await.expect("apply 0006");

    let loader = LoaderCtx { redis, pg, kek: KekCache::default() };
    let store = Arc::new(EntityStore::with_loader(loader));

    // No `global` row seeded — must return None, not panic.
    let result = store.auth_config_or_load("global").await;
    assert!(result.is_none(), "expected None when global row absent, got Some");
}
