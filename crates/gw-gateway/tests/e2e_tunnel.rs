//! Tunnel e2e storage + `TunnelStack::resolve` smoke tests (T7-B).
//!
//! Status: **v1 scaffold** — wires up testcontainers (Redis + Postgres),
//! applies the full migration chain through `0005_proxy_nodes.sql`, then
//! exercises the `EntityStore → TunnelStack::resolve` path. The full
//! Pingora → tunnel-node → fake-upstream round-trip is deferred to v2 because:
//!
//! - `TunnelStack::peer_for_node` requires a live WebSocket connection to the
//!   proxy node; testcontainer-spun fake upstream is the next layer and
//!   `e2e_full_flow.rs` defers an analogous ping to v2 for the same reason.
//! - No HTTP request traverses the gateway here. Only `TunnelStack::resolve`
//!   semantics (Direct vs Use) are exercised.
//!
//! What these tests do today:
//!   - Boot Postgres + Redis via testcontainers (gated `#[ignore]`).
//!   - Apply migrations 0001 → 0004 → 0005 in order.
//!   - Build a `LoaderCtx` + `EntityStore` and call `proxy_nodes_list_or_load`.
//!   - Construct a `TunnelStack` from the loaded node list.
//!   - Assert `TunnelStack::resolve` returns `Direct` (no nodes) or `Use(id)`
//!     (matching provider) as the seeded data dictates.
//!
//! Run with:
//!   `cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored --nocapture`.

#![cfg(feature = "tunnel")]

use std::sync::Arc;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

use gw_proxy::tunnel_stack::TunnelStack;
use gw_store::{encrypt::KekCache, loader::LoaderCtx, EntityStore};
use gw_tunnel_client::pool::PoolConfig;
use gw_tunnel_client::resolver::{ProxyNodeView, TunnelPreference, TunnelResolution};
use gw_tunnel_client::safe_dns::SafeDnsResolver;
use gw_tunnel_client::transport::TransportConfig;

const MIGRATION_0001: &str = include_str!("../../gw-store/migrations/0001_initial.sql");
const MIGRATION_0004: &str = include_str!("../../gw-store/migrations/0004_capabilities.sql");
const MIGRATION_0005: &str = include_str!("../../gw-store/migrations/0005_proxy_nodes.sql");

// ---------------------------------------------------------------------------
// Test A — empty proxy_nodes → TunnelStack::resolve returns Direct
// ---------------------------------------------------------------------------

/// Boot PG + Redis, apply migrations, confirm `proxy_nodes_list_or_load`
/// returns an empty `Vec`, then verify that `TunnelStack::resolve` returns
/// `Direct` for any (provider, key, preference). Guards spec §9 "non-breaking
/// on rollout": a gateway with tunnel enabled but no nodes seeded must not
/// alter the routing path.
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres); full pingora round-trip is v2 scope"]
async fn e2e_tunnel_constructs_when_enabled_with_no_nodes() {
    // -----------------------------------------------------------------
    // 1. Boot infrastructure.
    // -----------------------------------------------------------------
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_node = Redis::default().start().await.expect("start redis");

    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let redis_port = redis_node
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");

    let pg_url = format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres");
    let redis_url = format!("redis://127.0.0.1:{redis_port}");

    // -----------------------------------------------------------------
    // 2. Apply migrations.
    // -----------------------------------------------------------------
    let pg = gw_store::pg_client::connect(&pg_url, 4)
        .await
        .expect("pg connect");

    sqlx::raw_sql(MIGRATION_0001)
        .execute(&pg)
        .await
        .expect("apply 0001_initial");
    sqlx::raw_sql(MIGRATION_0004)
        .execute(&pg)
        .await
        .expect("apply 0004_capabilities");
    sqlx::raw_sql(MIGRATION_0005)
        .execute(&pg)
        .await
        .expect("apply 0005_proxy_nodes");

    // -----------------------------------------------------------------
    // 3. Build LoaderCtx + EntityStore.
    // -----------------------------------------------------------------
    let redis = gw_store::redis_client::RedisCtx::connect(&redis_url, 4)
        .await
        .expect("redis connect");

    let loader = LoaderCtx {
        redis: redis.clone(),
        pg: pg.clone(),
        kek: KekCache::default(),
    };
    let store = EntityStore::with_loader(loader);

    // -----------------------------------------------------------------
    // 4. Load proxy nodes — expect empty list (no rows seeded).
    // -----------------------------------------------------------------
    let nodes = store
        .proxy_nodes_list_or_load()
        .await
        .expect("proxy_nodes_list_or_load returned None (loader error)");
    assert!(
        nodes.is_empty(),
        "expected empty proxy_nodes, got {} rows",
        nodes.len()
    );

    // -----------------------------------------------------------------
    // 5. Build TunnelStack with empty node list, assert resolve → Direct.
    // -----------------------------------------------------------------
    let stack = TunnelStack::new(
        vec![],
        Arc::new(SafeDnsResolver::cloudflare_default()),
        PoolConfig::default(),
        TransportConfig::default(),
    );

    let pref = TunnelPreference::default();
    let result = stack.resolve("openai", "key-1", &pref);
    assert_eq!(
        result,
        TunnelResolution::Direct,
        "expected Direct with no nodes"
    );
}

// ---------------------------------------------------------------------------
// Test B — seeded proxy_nodes row → resolve returns Use(node_id)
// ---------------------------------------------------------------------------

/// Boot PG + Redis, apply migrations, INSERT one `proxy_nodes` row for
/// provider `openai`, then verify:
///   - `resolve("openai", …)` → `Use("node-test")`.
///   - `resolve("anthropic", …)` → `Direct` (provider mismatch).
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres); full pingora round-trip is v2 scope"]
async fn e2e_tunnel_resolves_use_when_node_matches_provider() {
    // -----------------------------------------------------------------
    // 1. Boot infrastructure.
    // -----------------------------------------------------------------
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_node = Redis::default().start().await.expect("start redis");

    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let redis_port = redis_node
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");

    let pg_url = format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres");
    let redis_url = format!("redis://127.0.0.1:{redis_port}");

    // -----------------------------------------------------------------
    // 2. Apply migrations.
    // -----------------------------------------------------------------
    let pg = gw_store::pg_client::connect(&pg_url, 4)
        .await
        .expect("pg connect");

    sqlx::raw_sql(MIGRATION_0001)
        .execute(&pg)
        .await
        .expect("apply 0001_initial");
    sqlx::raw_sql(MIGRATION_0004)
        .execute(&pg)
        .await
        .expect("apply 0004_capabilities");
    sqlx::raw_sql(MIGRATION_0005)
        .execute(&pg)
        .await
        .expect("apply 0005_proxy_nodes");

    // -----------------------------------------------------------------
    // 3. Seed one proxy_node row for provider "openai".
    // -----------------------------------------------------------------
    let payload = serde_json::json!({
        "id": "node-test",
        "ws_url": "ws://127.0.0.1:9001/",
        "enabled": true,
        "latency_ms": 0,
        "allowed_provider_ids": ["openai"],
        "allowed_vendor_key_ids": [],
        "auth_token_enc": null,
    });
    sqlx::query("INSERT INTO proxy_nodes (id, payload, version) VALUES ($1, $2, 1)")
        .bind("node-test")
        .bind(payload)
        .execute(&pg)
        .await
        .expect("seed proxy_node");

    // -----------------------------------------------------------------
    // 4. Build LoaderCtx + EntityStore + load nodes.
    // -----------------------------------------------------------------
    let redis = gw_store::redis_client::RedisCtx::connect(&redis_url, 4)
        .await
        .expect("redis connect");

    let loader = LoaderCtx {
        redis: redis.clone(),
        pg: pg.clone(),
        kek: KekCache::default(),
    };
    let store = EntityStore::with_loader(loader);

    let nodes = store
        .proxy_nodes_list_or_load()
        .await
        .expect("proxy_nodes_list_or_load returned None (loader error)");
    assert_eq!(nodes.len(), 1, "expected exactly 1 proxy_node row");

    // -----------------------------------------------------------------
    // 5. Map ProxyNode → ProxyNodeView and build TunnelStack.
    // -----------------------------------------------------------------
    let view: Vec<ProxyNodeView> = nodes
        .into_iter()
        .map(|n| ProxyNodeView {
            id: n.id,
            ws_url: n.ws_url,
            enabled: n.enabled,
            latency_ms: n.latency_ms,
            allowed_provider_ids: n.allowed_provider_ids,
            allowed_vendor_key_ids: n.allowed_vendor_key_ids,
        })
        .collect();

    let stack = TunnelStack::new(
        view,
        Arc::new(SafeDnsResolver::cloudflare_default()),
        PoolConfig::default(),
        TransportConfig::default(),
    );

    // -----------------------------------------------------------------
    // 6. Assert resolve semantics.
    // -----------------------------------------------------------------
    let pref = TunnelPreference::default();

    // Provider match → Use.
    let result = stack.resolve("openai", "any-key", &pref);
    assert_eq!(
        result,
        TunnelResolution::Use("node-test".to_string()),
        "expected Use(node-test) for matching provider"
    );

    // Provider mismatch → Direct.
    let result = stack.resolve("anthropic", "any-key", &pref);
    assert_eq!(
        result,
        TunnelResolution::Direct,
        "expected Direct for non-matching provider"
    );
}
