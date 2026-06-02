//! Unit tests for `gw_store::entity_store::with_cold_load_timeout`.
//!
//! Verifies the three terminal cases of the cold-load timeout wrapper:
//! - a stalled loader returns `None` once the deadline elapses;
//! - a successful `Ok(v)` is passed through as `Some(v)`;
//! - an `Err(_)` is converted to `None` (so Moka does NOT cache the failure).

use std::future::pending;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use gw_store::entity_store::with_cold_load_timeout;

#[tokio::test]
async fn timeout_returns_none_on_stall() {
    let start = Instant::now();
    let out = with_cold_load_timeout(
        "test_ns",
        Duration::from_millis(50),
        pending::<anyhow::Result<u32>>(),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(out.is_none(), "stalled loader must resolve to None");
    assert!(
        elapsed >= Duration::from_millis(50),
        "must wait at least the deadline (50ms), got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(250),
        "must not significantly exceed the deadline, got {elapsed:?}"
    );
}

#[tokio::test]
async fn timeout_passes_through_ok_value() {
    let out = with_cold_load_timeout("test_ns", Duration::from_secs(1), async {
        Ok::<u32, anyhow::Error>(42)
    })
    .await;
    assert_eq!(out, Some(42));
}

#[tokio::test]
async fn timeout_passes_through_err_as_none() {
    let out = with_cold_load_timeout("test_ns", Duration::from_secs(1), async {
        Err::<u32, anyhow::Error>(anyhow!("boom"))
    })
    .await;
    assert!(out.is_none(), "loader Err must resolve to None");
}

/// Functional-audit #12: a `PoolTimedOut` is transient pool contention, not a
/// missing row. It must resolve to `None` (so the caller does not cache or
/// serve a stale value) while taking the distinct
/// `gateway_pool_acquire_timeout_total` accounting path rather than being read
/// as a genuine "entity not found". Returning `None` here is the same terminal
/// value a real miss would yield, but the wrapper's downcast branch is what
/// keeps the two observably distinct (separate counter + log). This test pins
/// the `None` contract so a future refactor of the downcast arm cannot
/// regress a pool timeout into surfacing as an error/panic.
#[tokio::test]
async fn pool_timed_out_resolves_to_none_not_found() {
    let out = with_cold_load_timeout("test_ns", Duration::from_secs(1), async {
        Err::<u32, anyhow::Error>(anyhow::Error::new(sqlx::Error::PoolTimedOut))
    })
    .await;
    assert!(
        out.is_none(),
        "PoolTimedOut must resolve to None (transient), not surface as an error"
    );
}
