//! PostgreSQL pool wrapper (sqlx 0.8).

use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub async fn connect(url: &str, max: u32) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(max)
        .min_connections(2)
        .acquire_timeout(Duration::from_millis(200))
        .idle_timeout(Some(Duration::from_secs(600)))
        .connect(url)
        .await?;
    Ok(pool)
}
