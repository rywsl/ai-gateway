//! Full happy-path e2e test scaffold (Phase 6.1, Step 1).
//!
//! Status: **v1 scaffold** — wires up testcontainers (Redis + Postgres)
//! to assert the storage layer is reachable and the seed SQL applies
//! cleanly, then verifies that the `LlmGateway::new` constructor accepts
//! the wired-up collaborator set. The full Pingora + reqwest + wiremock
//! round-trip is deferred to v2 because:
//!
//! - `pingora::server::Server::run_forever` is `-> !` and consumes the
//!   server, so the same-process pattern requires a dedicated OS thread
//!   plus a one-shot bind-port handshake that we have not yet built.
//! - wiremock 0.6 publishes a Rust 2021 crate that compiles fine but the
//!   `http` 1.x types it surfaces conflict with `pingora 0.8`'s pinned
//!   `http 0.2` deps in the *binary* link path — the test target avoids
//!   that conflict by not depending on pingora here.
//!
//! What this test does today:
//!   - Boots Postgres + Redis via testcontainers (gated `#[ignore]`).
//!   - Applies `crates/gw-store/migrations/0001_initial.sql`.
//!   - Applies `tests/fixtures/seed.sql`.
//!   - Confirms one row per table reads back via sqlx.
//!   - Constructs an in-memory `EntityStore` and asserts the `LlmGateway`
//!     constructor signature remains stable (compile-only smoke).
//!
//! Run with: `cargo test -p gw-gateway --test e2e_full_flow -- --ignored --nocapture`.

use sqlx::Row;
use std::sync::Arc;
use std::time::Duration;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::{postgres::Postgres, redis::Redis};

use gw_auth::ratelimit::RateLimitGuard;
use gw_health::KeyHealthStore;
use gw_ledger::Ledger;
use gw_proxy::LlmGateway;
use gw_routing::in_flight::InFlightStore;
use gw_store::{CounterStore, EntityStore};

const MIGRATION: &str = include_str!("../../gw-store/migrations/0001_initial.sql");
const SEED_SQL: &str = include_str!("fixtures/seed.sql");

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis + Postgres); full pingora round-trip is v2 scope"]
async fn e2e_storage_seeded_and_gateway_constructs() {
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
    // 2. Apply schema + seed.
    // -----------------------------------------------------------------
    let pg = gw_store::pg_client::connect(&pg_url, 4)
        .await
        .expect("pg connect");

    sqlx::raw_sql(MIGRATION)
        .execute(&pg)
        .await
        .expect("apply migration");
    sqlx::raw_sql(SEED_SQL)
        .execute(&pg)
        .await
        .expect("apply seed");

    // Sanity-check: every seeded table has the expected row count.
    for (table, expected) in [
        ("users", 1_i64),
        ("vendors", 1),
        ("vendor_keys", 1),
        ("model_groups", 1),
        ("pricings", 1),
    ] {
        // String interp is safe here — `table` is a compile-time constant
        // sourced from a fixed list above, not user input.
        let sql = format!("SELECT COUNT(*) FROM {table}");
        let row = sqlx::query(&sql)
            .fetch_one(&pg)
            .await
            .unwrap_or_else(|e| panic!("count {table}: {e}"));
        let count: i64 = row.get(0);
        assert_eq!(count, expected, "{table} row count mismatch");
    }

    // -----------------------------------------------------------------
    // 3. Connect Redis and build the in-memory collaborators.
    // -----------------------------------------------------------------
    let redis = gw_store::redis_client::RedisCtx::connect(&redis_url, 4)
        .await
        .expect("redis connect");

    let store: Arc<EntityStore> = Arc::new(EntityStore::new());
    let health: Arc<KeyHealthStore> = Arc::new(KeyHealthStore::default());
    let in_flight: Arc<InFlightStore> = Arc::new(InFlightStore::default());
    let ratelimit: Arc<RateLimitGuard> = Arc::new(RateLimitGuard::new());
    let ledger: Arc<Ledger> = Arc::new(Ledger::start(
        redis.clone(),
        128,
        8,
        Duration::from_millis(100),
    ));
    let counters: Arc<CounterStore> = Arc::new(CounterStore {
        redis: redis.clone(),
    });
    let mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist> = Arc::new(
        gw_auth::jwt_blacklist::RedisJwtBlacklist::new((*redis.pool).clone()),
    );
    let resilience: Arc<gw_proxy::resilience::ResilienceState> =
        Arc::new(gw_proxy::resilience::ResilienceState::default());
    let detailed_archive = Arc::new(gw_ledger::detailed_archive::DetailedArchive::start(
        (*redis.pool).clone(),
        gw_ledger::detailed_archive::DetailedArchiveCfg {
            stream: "usage:detailed".to_string(),
            maxlen: 1_000,
            channel_capacity: 64,
            workers: 1,
            single_payload_soft_cap_bytes: 1024 * 1024,
            compression: gw_ledger::encoding::CompressionCfg {
                thresholds: gw_ledger::encoding::TierThresholds {
                    small_threshold_bytes: 1024,
                    medium_threshold_bytes: 65_536,
                },
                zstd_level: 3,
            },
        },
    ));

    // -----------------------------------------------------------------
    // 4. Construct LlmGateway. This is the "smoke" assertion: the
    //    constructor signature stays in sync with `gw-gateway::run()`.
    // -----------------------------------------------------------------
    let _gw = LlmGateway::new(
        store.clone(),
        health.clone(),
        in_flight.clone(),
        Arc::new(gw_routing::LatencyTracker::new()),
        ratelimit.clone(),
        ledger.clone(),
        counters.clone(),
        100_000,
        false,
        None,
        Arc::new(gw_routing::strategy::set::StrategySet::new_default()),
        gw_routing::FailoverExecutor::default(),
        None,
        mgmt_blacklist,
        resilience,
        #[cfg(feature = "tunnel")]
        None,
        detailed_archive,
    );

    // -----------------------------------------------------------------
    // 5. v2 TODO: bind a wiremock-like upstream + Pingora listener on
    //    random ports, send a reqwest POST to /v1/chat/completions, and
    //    assert:
    //       - 200 response body conforms to OpenAI ChatCompletion shape.
    //       - usage:records stream has 1 entry (XLEN == 1).
    //       - DEDUCT.frozen_micro == 0 (settled).
    //       - response headers carry X-Request-Id + X-Gateway-Version.
    //
    //    Tracking item: see commit message for the gateway harness work.
}
