//! T0 emit-lag watchdog — spec §5.4.2.
//!
//! Periodic task scanning own-instance `billing:inflight:*` keys. For each
//! key with `ts_settled_ms` set, checks whether (now - ts_settled_ms) exceeds
//! the T0 threshold. On exceedance fires `gw_billing_emit_lag_t0_total` —
//! does NOT delete the key (forensic preservation) and does NOT emit a
//! fallback UsageRecord (only the startup reconciler is allowed to do that).

use fred::clients::RedisPool;
use fred::interfaces::HashesInterface;
use fred::types::Scanner;
use futures::StreamExt;
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::collections::HashMap;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Spawn the periodic T0 watchdog supervised by a `Supervisor`. Returns the
/// `JoinHandle` so the caller can register it with the `ShutdownCoordinator`
/// — the task runs until cancelled via `ShutdownToken` (graceful shutdown)
/// or until the restart budget is exhausted.
///
/// The reaper has no buffered channel; on cancel it simply returns. There
/// is no drain budget to thread through.
pub fn spawn(
    pool: RedisPool,
    instance_id: String,
    interval_secs: u64,
    emit_lag_t0_secs: u64,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    Supervisor::new("inflight_reaper", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let pool = pool.clone();
        let instance_id = instance_id.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(
                            target: "gw_ledger",
                            "inflight_reaper: shutdown requested"
                        );
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        if let Err(e) = run_once(&pool, &instance_id, emit_lag_t0_secs).await {
                            tracing::warn!(error = ?e, "inflight reaper iteration failed");
                        }
                    }
                }
            }
        }
    })
}

/// Single sweep. Returns the count of stale keys detected (mostly for tests).
pub async fn run_once(
    pool: &RedisPool,
    instance_id: &str,
    emit_lag_t0_secs: u64,
) -> anyhow::Result<usize> {
    let pattern = format!("billing:inflight:{instance_id}:*");
    let mut stale = 0usize;

    // `scan` lives on `RedisClient`, not on `RedisPool`. `pool.next()` picks
    // the next client in round-robin order — for a single-server Redis this is
    // always the same client.
    let client = pool.next();
    let mut stream = client.scan(pattern, Some(100), None);
    while let Some(result) = stream.next().await {
        let mut page = result?;
        if let Some(keys) = page.take_results() {
            for rkey in keys {
                let Some(key_str) = rkey.into_string() else {
                    continue;
                };
                if check_key(pool, &key_str, emit_lag_t0_secs).await? {
                    stale += 1;
                }
            }
        }
        // Dropping `page` without calling `page.next()` is fine — the Stream
        // on the next iteration will drive the cursor forward automatically
        // (the Drop impl on ScanResult triggers the next page fetch).
    }

    Ok(stale)
}

async fn check_key(pool: &RedisPool, key: &str, emit_lag_t0_secs: u64) -> anyhow::Result<bool> {
    let hash: HashMap<String, String> = pool.hgetall(key).await?;
    let Some(ts_str) = hash.get("ts_settled_ms") else {
        return Ok(false);
    };
    let ts_settled: i64 = ts_str.parse().unwrap_or(0);
    if ts_settled <= 0 {
        return Ok(false);
    }
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;
    let lag_ms = now_ms.saturating_sub(ts_settled);
    if lag_ms > (emit_lag_t0_secs * 1000) as i64 {
        // Extract request_id for logs and attempt for the bounded metric label.
        // Key shape: billing:inflight:<instance_id>:<request_id>:<attempt_index>
        // instance_id itself may contain colons so we count from the right.
        let parts: Vec<&str> = key.split(':').collect();
        let attempt = parts.last().copied().unwrap_or("0");
        let request_id = if parts.len() >= 2 {
            parts[parts.len() - 2]
        } else {
            ""
        };
        tracing::error!(
            request_id,
            attempt_index = attempt,
            lag_ms,
            "T0: UsageRecord emit lag exceeded threshold"
        );
        metrics::counter!(
            "gw_billing_emit_lag_t0_total",
            "attempt_index" => attempt.to_string(),
        )
        .increment(1);
        Ok(true)
    } else {
        Ok(false)
    }
}
