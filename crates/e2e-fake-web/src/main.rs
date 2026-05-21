//! `fake_web` — the E2E black-box stand-in for the Go control-plane.
//!
//! E2-A scope: connect to Postgres + Redis, expose `Store` to a future
//! admin API (E2-B), spawn a 1 s background drain loop that decrements
//! `counter:user:{id}:balance_micro` from `usage:records`, and serve a
//! single `GET /healthz` route so docker-compose can mark the
//! container healthy.

use e2e_fake_web::{drain, router, store::Store};
use fred::clients::RedisPool;
use fred::interfaces::ClientLike;
use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
use std::sync::Arc;
use std::time::Duration;

const PG_DEFAULT: &str = "postgres://postgres:postgres@localhost:5432/gateway";
const REDIS_DEFAULT: &str = "redis://localhost:6379";
const BIND_ADDR: &str = "0.0.0.0:7000";
const REDIS_POOL_SIZE: usize = 4;
const DRAIN_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let pg_url = std::env::var("FAKE_WEB_PG_URL").unwrap_or_else(|_| PG_DEFAULT.to_string());
    let redis_url =
        std::env::var("FAKE_WEB_REDIS_URL").unwrap_or_else(|_| REDIS_DEFAULT.to_string());

    tracing::info!(target: "e2e_fake_web", %pg_url, %redis_url, "fake_web starting");

    let pg = sqlx::postgres::PgPoolOptions::new()
        .max_connections(8)
        .connect(&pg_url)
        .await?;

    let redis_cfg = RedisConfig::from_url(&redis_url)?;
    let pool = RedisPool::new(
        redis_cfg,
        Some(PerformanceConfig::default()),
        Some(ConnectionConfig::default()),
        Some(ReconnectPolicy::default()),
        REDIS_POOL_SIZE,
    )?;
    pool.init().await?;
    let redis = Arc::new(pool);

    let store = Arc::new(Store::new(pg, redis));

    // Background drain loop. We tolerate transient drain errors (logged)
    // and never propagate them, because a corrupt `usage:records` entry
    // must not crash the control-plane stand-in.
    let drain_store = store.clone();
    tokio::spawn(async move {
        let mut cursor = String::from("0-0");
        loop {
            match drain::drain_once(&drain_store, &mut cursor).await {
                Ok(n) => {
                    tracing::debug!(
                        target: "e2e_fake_web::drain",
                        processed = n,
                        cursor = %cursor,
                        "drain cycle"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        target: "e2e_fake_web::drain",
                        error = %e,
                        "drain cycle failed"
                    );
                }
            }
            tokio::time::sleep(DRAIN_INTERVAL).await;
        }
    });

    let app = router(store);
    let listener = tokio::net::TcpListener::bind(BIND_ADDR).await?;
    tracing::info!(target: "e2e_fake_web", addr = %BIND_ADDR, "listening");
    axum::serve(listener, app).await?;
    Ok(())
}
