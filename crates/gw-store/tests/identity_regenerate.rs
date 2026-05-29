//! Regression coverage for the Go-side "regenerate identity" contract.
//!
//! Requires Docker / testcontainers:
//!   cargo test -p gw-store --test identity_regenerate -- --ignored --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use fred::interfaces::KeysInterface;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

use gw_core::ids::KeyId;
use gw_core::impersonate::{claude_code, ensure_identity, IdentitySnapshot, ImpersonateProfile};
use gw_store::encrypt::{encrypt_for_test_only, KekCache};
use gw_store::loader::LoaderCtx;
use gw_store::stream_consumer::apply_inline_for_test_helper;
use gw_store::EntityStore;

const MIGRATION_0001: &str = include_str!("../migrations/0001_initial.sql");
const MIGRATION_0004: &str = include_str!("../migrations/0004_capabilities.sql");
const MIGRATION_0009: &str = include_str!("../migrations/0009_egress_impersonation.sql");

#[tokio::test]
#[ignore = "requires Docker (testcontainers Postgres + Redis)"]
async fn version_bump_after_null_reseeds_identity() {
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_node = Redis::default().start().await.expect("start redis");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let redis_port = redis_node
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");

    let pg = gw_store::pg_client::connect(
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

    let redis =
        gw_store::redis_client::RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
            .await
            .expect("redis connect");

    let key_id = "vk-regenerate-001";
    let stale_snapshot = IdentitySnapshot {
        profile_kind: ImpersonateProfile::ClaudeCodeChrome131,
        variable: claude_code::derive("vk-operator-stale"),
    };
    let stale_user_agent = user_agent(&stale_snapshot);
    let expected_fresh_user_agent = user_agent(&IdentitySnapshot {
        profile_kind: ImpersonateProfile::ClaudeCodeChrome131,
        variable: claude_code::derive(key_id),
    });
    assert_ne!(
        stale_user_agent, expected_fresh_user_agent,
        "test fixture must distinguish stale operator override from re-derived identity",
    );

    let kek_bytes = [7u8; 32];
    let kek = KekCache::new(HashMap::from([("test".to_string(), kek_bytes)]));
    let payload = serde_json::json!({
        "key_id": key_id,
        "vendor_id": "v-impersonate",
        "api_key_enc": encrypt_for_test_only("sk-regenerate", kek_bytes),
        "status": "active",
    });

    sqlx::query(
        "INSERT INTO vendor_keys (id, payload, identity_snapshot, version) VALUES ($1, $2, $3, 1)",
    )
    .bind(key_id)
    .bind(payload.clone())
    .bind(serde_json::to_value(&stale_snapshot).expect("snapshot json"))
    .execute(&pg)
    .await
    .expect("seed vendor_key");

    let store = Arc::new(EntityStore::with_loader(LoaderCtx {
        redis: redis.clone(),
        pg: pg.clone(),
        kek: kek.clone(),
    }));
    let loaded1 = store
        .vendor_key_or_load(&KeyId::from(key_id))
        .await
        .expect("initial load");
    let loaded_stale = loaded1
        .value
        .identity_snapshot
        .clone()
        .expect("seeded identity_snapshot");
    assert_eq!(loaded1.version, 1);
    assert_eq!(user_agent(&loaded_stale), stale_user_agent);

    let redis_key = format!("entity:vendor_key:{key_id}");
    let client = redis.pool.next();
    let exists_before: i64 = client
        .exists(&redis_key)
        .await
        .expect("redis exists before");
    assert_eq!(
        exists_before, 1,
        "precondition: Redis L2 contains the stale vendor_key row",
    );

    let row: (i64, Option<serde_json::Value>) = sqlx::query_as(
        "UPDATE vendor_keys \
         SET identity_snapshot = NULL, version = version + 1, updated_at = NOW() \
         WHERE id = $1 \
         RETURNING version, identity_snapshot",
    )
    .bind(key_id)
    .fetch_one(&pg)
    .await
    .expect("regenerate identity update");
    assert_eq!(row.0, 2);
    assert!(row.1.is_none());

    apply_inline_for_test_helper(&store, "vendor_key", key_id).await;

    let exists_after: i64 = client.exists(&redis_key).await.expect("redis exists after");
    assert_eq!(
        exists_after, 0,
        "entity event invalidation must delete the stale Redis L2 row",
    );

    let loaded2 = store
        .vendor_key_or_load(&KeyId::from(key_id))
        .await
        .expect("reload after regenerate");
    assert_eq!(loaded2.version, 2);
    assert!(
        loaded2.value.identity_snapshot.is_none(),
        "cache must observe the cleared identity_snapshot, not the stale operator override",
    );
    assert!(
        !Arc::ptr_eq(&loaded1, &loaded2),
        "must not reuse stale Moka Arc after entity invalidation",
    );

    let (resolved, should_persist) = ensure_identity(
        key_id,
        loaded2.value.identity_snapshot.as_ref(),
        ImpersonateProfile::ClaudeCodeChrome131,
    );
    assert!(
        should_persist,
        "cleared snapshot must cause deterministic re-derive and lazy-persist",
    );
    let fresh_snapshot = IdentitySnapshot {
        profile_kind: resolved.profile,
        variable: resolved.variable,
    };
    assert_eq!(user_agent(&fresh_snapshot), expected_fresh_user_agent);
    assert_ne!(user_agent(&fresh_snapshot), stale_user_agent);
}

fn user_agent(snapshot: &IdentitySnapshot) -> String {
    claude_code::headers(&snapshot.variable, false)
        .into_iter()
        .find_map(|(k, v)| (k == "User-Agent").then_some(v))
        .expect("User-Agent header")
}
