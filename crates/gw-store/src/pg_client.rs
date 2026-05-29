//! PostgreSQL pool wrapper (sqlx 0.8).

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub async fn connect(url: &str, max: u32) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .min_connections(2)
        // Audit 2026-05-30 §1.12: raised 200ms -> 500ms. The old 200ms sat well
        // below the 2000ms cold-load budget, so transient pool contention
        // returned PoolTimedOut that callers mapped to None = "entity not found"
        // (a hard 401 / no-channel). 500ms absorbs brief spikes while staying
        // below the cold-load deadline, and PoolTimedOut is now counted distinctly
        // in `with_cold_load_timeout` rather than masquerading as a missing row.
        .acquire_timeout(Duration::from_millis(500))
        .idle_timeout(Some(Duration::from_secs(600)))
        .connect(url)
        .await?;
    Ok(pool)
}
