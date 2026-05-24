//! L0/L1/L2 instance_id resolver + heartbeat task.
//!
//! Spec §5.4.1. The instance_id must be stable across process restarts so the
//! startup reconciler (W4) can identify its own residuals. Resolution order:
//!   L0: GW_INSTANCE_ID env (operator override, recommended for production)
//!   L1: in-process OnceLock cache (returns cached value if already resolved)
//!   L2: Redis key `gateway:instance:<hostname>` with no TTL
//! On cold cache + cold Redis, generate a fresh UUID v4 and persist to L2.

use fred::clients::RedisPool;
use fred::interfaces::KeysInterface;
use fred::types::Expiration;
use std::sync::OnceLock;
use std::time::Duration;
use uuid::Uuid;

const ENV_VAR: &str = "GW_INSTANCE_ID";
const L2_KEY_PREFIX: &str = "gateway:instance:";

/// Heartbeat key TTL in seconds. Default 600s; spec §5.4.1.
pub const HEARTBEAT_TTL_SECS: i64 = 600;
/// Heartbeat renewal cadence (must be << HEARTBEAT_TTL_SECS so an entire
/// HEARTBEAT_INTERVAL_SECS slip doesn't expire the key).
pub const HEARTBEAT_INTERVAL_SECS: u64 = 60;

static L1_CACHE: OnceLock<String> = OnceLock::new();

/// Resolve the gateway's instance_id following the L0/L1/L2 precedence.
///
/// **Concurrency contract**: this function must be called serially from
/// gateway bootstrap (typically a single call in `LlmGateway::new` / `Wired`).
/// Concurrent first-time calls on the same hostname have a narrow race where
/// both threads may generate UUIDs and write L2 before either populates L1 —
/// callers will all observe the L1 winner's value, but L2 may settle on the
/// loser's UUID. After L1 is populated, all subsequent calls are race-free.
pub async fn resolve(pool: &RedisPool, hostname: &str) -> anyhow::Result<String> {
    if let Some(cached) = L1_CACHE.get() {
        return Ok(cached.clone());
    }

    let l2_key = format!("{}{}", L2_KEY_PREFIX, hostname);

    let id = if let Ok(env_val) = std::env::var(ENV_VAR) {
        // L0: env override. Sync into L2 so other tools observing L2 see the same value.
        let _: () = pool
            .set::<(), _, _>(&l2_key, env_val.as_str(), None, None, false)
            .await?;
        env_val
    } else {
        // L2: read existing value.
        let existing: Option<String> = pool.get(&l2_key).await?;
        match existing {
            Some(v) if !v.is_empty() => v,
            _ => {
                let new_id = Uuid::new_v4().to_string();
                let _: () = pool
                    .set::<(), _, _>(&l2_key, new_id.as_str(), None, None, false)
                    .await?;
                new_id
            }
        }
    };

    // L1: cache. If a concurrent resolver wins the race, prefer the winner's value.
    let _ = L1_CACHE.set(id.clone());
    Ok(L1_CACHE.get().cloned().unwrap_or(id))
}

pub fn current() -> Option<String> {
    L1_CACHE.get().cloned()
}

pub fn heartbeat_key(instance_id: &str) -> String {
    format!("gateway:instance:{}:heartbeat", instance_id)
}

pub async fn renew_heartbeat(pool: &RedisPool, instance_id: &str) -> anyhow::Result<()> {
    let key = heartbeat_key(instance_id);
    let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;
    let _: () = pool
        .set::<(), _, _>(
            &key,
            now_ms.to_string(),
            Some(Expiration::EX(HEARTBEAT_TTL_SECS)),
            None,
            false,
        )
        .await?;
    metrics::counter!("gw_billing_instance_heartbeat_renewals_total").increment(1);
    Ok(())
}

pub async fn spawn_heartbeat_task(pool: RedisPool, instance_id: String) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
        // Fire once at startup, then on each tick.
        loop {
            tick.tick().await;
            if let Err(e) = renew_heartbeat(&pool, &instance_id).await {
                tracing::warn!(error = ?e, instance_id = %instance_id, "instance heartbeat renewal failed");
            }
        }
    });
}

#[doc(hidden)]
pub fn __reset_for_tests() {
    // OnceLock can't be reset in stable Rust without unsafe.
    // For tests, we work around by setting an env-derived value before resolve()
    // — actual tests run in fresh processes (cargo spawns a fresh test binary per
    // file) so the OnceLock is naturally fresh. This function is a NO-OP marker
    // documenting that tests assume a fresh process.
    //
    // If you need true reset across tests in the same process, replace this
    // module's L1_CACHE with a `RwLock<Option<String>>` and clear it here.
}
