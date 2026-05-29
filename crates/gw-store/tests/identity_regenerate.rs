//! EI-7 — Go-side identity regeneration clears the dedicated
//! `vendor_keys.identity_snapshot` column and bumps `version`, forcing Rust to
//! miss stale caches and re-derive on the next impersonated request.
//!
//! Requires Docker (testcontainers Redis + Postgres).

use std::collections::HashMap;
use std::sync::Arc;

use gw_core::ids::KeyId;
use gw_core::impersonate::{
    ensure_identity, IdentitySnapshot, ImpersonateProfile, ResolvedIdentity,
};
use gw_store::encrypt::{encrypt_for_test_only, KekCache};
use gw_store::identity_persist::{PgSnapshotWriter, SnapshotWriter};
use gw_store::loader::LoaderCtx;
use gw_store::redis_client::RedisCtx;
use gw_store::stream_consumer::apply_inline_for_test_helper;
use gw_store::{pg_client, EntityStore};
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

const MIGRATION_0001: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_capabilities.sql");
const MIGRATION_0009: &str = include_str!("../migrations/0009_egress_impersonation.sql");

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres)"]
async fn regenerate_identity_nulls_snapshot_bumps_version_and_rederives() {
    let redis_node = Redis::default().start().await.expect("start redis");
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_port = redis_node
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");

    let pg = pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .expect("pg connect");
    sqlx::raw_sql(MIGRATION_0001)
        .execute(&pg)
        .await
        .expect("apply 0001");
    sqlx::raw_sql(MIGRATION_0004)
        .execute(&pg)
        .await
        .expect("apply 0004");
    sqlx::raw_sql(MIGRATION_0009)
        .execute(&pg)
        .await
        .expect("apply 0009");

    let key_id = "vk-regenerate-001";
    let profile = ImpersonateProfile::ClaudeCodeChrome131;
    let kek_bytes = [9u8; 32];
    let kek = KekCache::new(HashMap::from([("test".to_string(), kek_bytes)]));
    let api_key_enc = encrypt_for_test_only("sk-regenerate", kek_bytes);
    let seeded_snapshot = snapshot_from(ensure_identity(key_id, None, profile).0);
    let payload = serde_json::json!({
        "key_id": key_id,
        "vendor_id": "v-impersonate",
        "api_key_enc": api_key_enc,
        "status": "active"
    });

    sqlx::query(
        "INSERT INTO vendor_keys (id, payload, identity_snapshot, version)
         VALUES ($1, $2, $3, 1)",
    )
    .bind(key_id)
    .bind(payload)
    .bind(serde_json::to_value(&seeded_snapshot).expect("serialize snapshot"))
    .execute(&pg)
    .await
    .expect("seed vendor_key");

    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
        .await
        .expect("redis connect");
    let store = Arc::new(EntityStore::with_loader(LoaderCtx {
        redis: redis.clone(),
        pg: pg.clone(),
        kek: kek.clone(),
    }));
    let key = KeyId::from(key_id);
    let loaded_v1 = store
        .vendor_key_or_load(&key)
        .await
        .expect("initial cold load");
    assert_eq!(loaded_v1.version, 1);
    assert!(
        loaded_v1.value.identity_snapshot.is_some(),
        "seeded non-null snapshot must load from the dedicated column"
    );

    sqlx::query("UPDATE vendor_keys SET identity_snapshot = NULL, version = 2 WHERE id = $1")
        .bind(key_id)
        .execute(&pg)
        .await
        .expect("simulate Go regenerate action");

    // Simulate the `events:entity` invalidation path Go must emit after the
    // write. This clears Redis L2 and Moka L1 through the same helper used by
    // StreamConsumer tests; without it, the stale version-1 snapshot remains
    // visible until cache TTL expiry.
    apply_inline_for_test_helper(&store, "vendor_key", key_id).await;

    let loaded_v2 = store
        .vendor_key_or_load(&key)
        .await
        .expect("reload after regenerate");
    assert_eq!(loaded_v2.version, 2);
    assert!(
        loaded_v2.value.identity_snapshot.is_none(),
        "loader must surface the regenerated storage state before ensure_identity re-derives"
    );

    let (resolved, should_persist) =
        ensure_identity(key_id, loaded_v2.value.identity_snapshot.as_ref(), profile);
    assert!(
        should_persist,
        "missing snapshot after regenerate must request lazy persist"
    );
    let rederived = snapshot_from(resolved);
    PgSnapshotWriter::new(pg.clone())
        .write_if_null(key_id, &rederived)
        .await
        .expect("lazy persist regenerated snapshot");

    let persisted: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT identity_snapshot FROM vendor_keys WHERE id = $1")
            .bind(key_id)
            .fetch_one(&pg)
            .await
            .expect("read persisted snapshot");
    assert!(
        persisted.is_some(),
        "lazy persist must write a regenerated identity_snapshot while the column is NULL"
    );
}

fn snapshot_from(resolved: ResolvedIdentity) -> IdentitySnapshot {
    IdentitySnapshot {
        profile_kind: resolved.profile,
        variable: resolved.variable,
    }
}
