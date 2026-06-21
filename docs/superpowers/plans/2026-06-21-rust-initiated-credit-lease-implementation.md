# Rust-Initiated Credit Lease Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build Rust-initiated active-user registration so the first funded normal LLM request can activate lease renewal, while Go renews leases only for recently active users in steady state.

**Architecture:** Rust admits a normal billable LLM request through the existing balance, lease, overflow, or freeze controls, then enqueues a non-blocking active-registration event to a bounded worker that atomically writes Redis liveness keys. Go keeps the existing `protected_lease` fanout payload, but changes `LeaseScheduler` selection from global funded-wallet scans to `full_scan`, `dual`, or `active_only` modes backed by Redis active-user keys, a persisted renewal zset, and a single-producer leader lock.

**Tech Stack:** Rust crates `gw-ledger`, `gw-proxy`, `gw-gateway` with `tokio`, `fred`, `sqlx`, `metrics`; Go modules `modules/billing`, `shared/rustpact`, `shared/platform/metrics` with `go-redis/v9`, `pgx/v5`, `prometheus`, `miniredis`.

## Global Constraints

- Work in the isolated worktrees: Rust `/Users/whereslow/Desktop/work/Cnewxus/.worktrees/credit-lease-rust-initiated-gateway`, Go `/Users/whereslow/Desktop/work/Cnewxus/.worktrees/credit-lease-rust-initiated-go`.
- Do not change the existing `protected_lease` stream payload shape from Go to Rust.
- Do not add Rust-to-Go HTTP or gRPC calls for lease creation.
- Do not add a synchronous Redis or Go dependency to the Rust request hot path for active registration.
- Only register after successfully admitted normal billable LLM proxy requests while `[protection].enabled = true`.
- Do not register balance rejection, daily quota rejection, API key rejection, RPM rejection, hard-cap rejection, freeze construction rejection, task endpoints, or Gemini Files endpoints.
- Rust cold Redis counter fallback must use the existing `OverflowGate` Redis balance cache and Postgres read path, or an equivalent read-only authoritative balance source.
- Redis keys are exactly `credit:active_users`, `credit:active_user:<uid>`, and `credit:lease_renew_at`.
- Rust default config values are `active_registration_ttl_secs = 300`, `active_registration_refresh_min_interval_secs = 30`, and `active_registration_queue_capacity = 8192`.
- Go `active_scan_batch_size` default is 500.
- Go scheduler modes are exactly `full_scan`, `dual`, and `active_only`; `active_only` is the steady state, `full_scan` is rollback.
- The Go credit-lease producer must skip ticks when it cannot prove it is the single leader.
- Use TDD for each task: write failing tests first, run the smallest failing test, implement, rerun, then commit.

---

## File Structure

### Rust Worktree

- Create `crates/gw-ledger/src/active_registration.rs`
  - Owns Redis key constants, `ActiveRegistrationHandle`, bounded queue, local throttle, worker loop, atomic Redis Lua write, retry behavior, and unit tests.
- Modify `crates/gw-ledger/src/lib.rs`
  - Exports `active_registration`, `ActiveRegistrationHandle`, and `ActiveRegistrationEnqueueResult`.
- Modify `crates/gw-ledger/src/protection_config.rs`
  - Adds the three active-registration config fields and default tests.
- Modify `crates/gw-ledger/src/metrics.rs`
  - Adds Rust metric-name constants used by active registration, balance fallback, and lease sweep.
- Modify `crates/gw-ledger/src/overflow.rs`
  - Exposes a read-only `fallback_balance_micro` wrapper around the existing private `balance_for` path.
- Modify `crates/gw-ledger/src/lease.rs`
  - Replaces no-op protected sweep with expiry-plus-inactivity sweep and records removals.
- Modify `crates/gw-ledger/src/ledger_sweep.rs`
  - Accepts `ProtectionConfig` so sweep uses `lease_grace_ms + active_registration_ttl_secs`.
- Modify `crates/gw-gateway/src/lib.rs`
  - Spawns the active-registration worker, stores the handle, wires the ledger sweep config, and passes the handle into proxy construction.
- Modify `crates/gw-gateway/src/config.rs` and `config/default.toml`
  - Loads and documents the new protection fields.
- Modify `crates/gw-gateway/src/metrics_catalog.rs`
  - Adds the new Rust metrics to the catalog.
- Modify `crates/gw-proxy/src/gateway.rs`
  - Stores `ActiveRegistrationHandle` in `LlmGateway` and passes it into `filters::request::run`.
- Modify `crates/gw-proxy/src/filters/request.rs`
  - Adds cold balance fallback before the step 7 rejection, registers after lease success, overflow-admitted protected failures, and cold `RequestFreeze` success.
- Modify Rust docs:
  - `docs/spec/06-billing-ledger.md`
  - `docs/workflow.md`
  - `docs/go-contracts/main.md`

### Go Worktree

- Create `shared/rustpact/credit_active.go`
  - Owns Redis active-user key constants and helper functions shared by billing code and tests.
- Modify `shared/rustpact/stream_test.go` or create `shared/rustpact/credit_active_test.go`
  - Verifies exact Redis contract strings.
- Modify `shared/platform/metrics/credit_lease.go`
  - Adds active scan and leader metrics.
- Create `modules/billing/business/lease_active_registry.go`
  - Owns active-user scanning, liveness checks, stale cleanup, wallet row filtering, and Redis renewal zset helpers.
- Create `modules/billing/business/lease_leader.go`
  - Owns single-producer Redis leadership using `SET NX PX` plus same-token refresh.
- Modify `modules/billing/business/lease_scheduler.go`
  - Adds selection mode config, leader guard, active scan integration, Redis zset due state, and keeps `full_scan` behavior for rollback.
- Modify `modules/billing/business/lease_scheduler_test.go`
  - Adds mode, active scan, stale cleanup, due zset, push failure, and leader tests.
- Modify `modules/billing/module.go`
  - Reads environment-backed scheduler mode and leader settings, wires DB batch wallet reader and leader into scheduler.
- Modify Go docs:
  - `docs/business/billing/business.md`
  - `docs/business/budgetpolicy/business.md` only if policy-facing knobs are introduced during implementation.
  - Any existing `shared/rustpact` contract docs, or add a short contract section near the constants if no doc exists.

---

### Task 1: Rust Active Registration Infrastructure

**Files:**
- Create: `crates/gw-ledger/src/active_registration.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Modify: `crates/gw-ledger/src/protection_config.rs`
- Modify: `crates/gw-ledger/src/metrics.rs`
- Test: `crates/gw-ledger/src/active_registration.rs`
- Test: `crates/gw-ledger/src/protection_config.rs`

**Interfaces:**
- Consumes: `gw_core::ids::UserId`, `fred::clients::RedisPool`, `gw_runtime::ShutdownToken`, `ProtectionConfig`.
- Produces:
  - `pub const ACTIVE_USERS_KEY: &str`
  - `pub const ACTIVE_USER_PREFIX: &str`
  - `pub fn active_user_key(uid: &UserId) -> String`
  - `pub enum ActiveRegistrationEnqueueResult { Queued, Throttled, Full, Closed, Disabled }`
  - `pub struct ActiveRegistrationHandle`
  - `impl ActiveRegistrationHandle { pub fn disabled() -> Self; pub fn try_enqueue_activity(&self, uid: &UserId, admitted_at_ms: u64) -> ActiveRegistrationEnqueueResult }`
  - `pub fn spawn_active_registration_worker(redis: RedisPool, pod_id: String, cfg: &ProtectionConfig, shutdown: ShutdownToken) -> (ActiveRegistrationHandle, tokio::task::JoinHandle<()>)`

- [ ] **Step 1: Write failing config tests**

Add this test expansion in `crates/gw-ledger/src/protection_config.rs`:

```rust
#[test]
fn default_enables_hard_mode_protection() {
    let cfg = ProtectionConfig::default();
    assert!(cfg.enabled);
    assert_eq!(cfg.lease_grace_ms, 30_000);
    assert_eq!(cfg.default_output_ceiling_tokens, 8192);
    assert_eq!(cfg.active_registration_ttl_secs, 300);
    assert_eq!(cfg.active_registration_refresh_min_interval_secs, 30);
    assert_eq!(cfg.active_registration_queue_capacity, 8192);
}

#[test]
fn deserialize_partial_overrides_keep_defaults() {
    let toml_text = r#"
        default_output_ceiling_tokens = 4096
        active_registration_refresh_min_interval_secs = 7
    "#;
    let cfg: ProtectionConfig = toml::from_str(toml_text).unwrap();
    assert!(cfg.enabled);
    assert_eq!(cfg.lease_grace_ms, 30_000);
    assert_eq!(cfg.default_output_ceiling_tokens, 4096);
    assert_eq!(cfg.active_registration_ttl_secs, 300);
    assert_eq!(cfg.active_registration_refresh_min_interval_secs, 7);
    assert_eq!(cfg.active_registration_queue_capacity, 8192);
}
```

Run:

```bash
cargo test -p gw-ledger protection_config -- --nocapture
```

Expected: FAIL with missing `active_registration_*` fields.

- [ ] **Step 2: Implement protection config fields**

Update `ProtectionConfig`:

```rust
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProtectionConfig {
    pub enabled: bool,
    pub lease_grace_ms: u64,
    pub default_output_ceiling_tokens: u32,
    /// How long Go should treat a Rust-refreshed user as active.
    pub active_registration_ttl_secs: u64,
    /// Minimum per-user interval before Rust enqueues another refresh.
    pub active_registration_refresh_min_interval_secs: u64,
    /// Bounded worker channel size. Queue overflow never rejects admitted requests.
    pub active_registration_queue_capacity: usize,
}

impl Default for ProtectionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lease_grace_ms: 30_000,
            default_output_ceiling_tokens: 8192,
            active_registration_ttl_secs: 300,
            active_registration_refresh_min_interval_secs: 30,
            active_registration_queue_capacity: 8192,
        }
    }
}
```

Run:

```bash
cargo test -p gw-ledger protection_config -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Add metric constants**

In `crates/gw-ledger/src/metrics.rs`, add:

```rust
pub const CREDIT_ACTIVE_REGISTER_ENQUEUE_TOTAL: &str = "gw_credit_active_register_enqueue_total";
pub const CREDIT_ACTIVE_REGISTER_REDIS_TOTAL: &str = "gw_credit_active_register_redis_total";
pub const CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH: &str = "gw_credit_active_register_queue_depth";
pub const CREDIT_LEASE_SWEEP_REMOVED_TOTAL: &str = "gw_credit_lease_sweep_removed_total";
pub const CREDIT_BALANCE_FALLBACK_TOTAL: &str = "gw_credit_balance_fallback_total";
```

Run:

```bash
cargo check -p gw-ledger
```

Expected: PASS.

- [ ] **Step 4: Write failing active-registration unit tests**

Create `crates/gw-ledger/src/active_registration.rs` with the tests first. The Redis integration test uses the existing `testcontainers` Redis dependency already present in `gw-ledger`.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fred::clients::RedisPool;
    use fred::interfaces::{KeysInterface, SetsInterface};
    use fred::types::{ConnectionConfig, PerformanceConfig, ReconnectPolicy, RedisConfig};
    use gw_core::ids::UserId;
    use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
    use testcontainers_modules::redis::Redis;

    async fn redis_pool() -> (ContainerAsync<Redis>, RedisPool) {
        let container = Redis::default().start().await.expect("redis");
        let port = container.get_host_port_ipv4(6379).await.expect("redis port");
        let pool = RedisPool::new(
            RedisConfig::from_url(&format!("redis://127.0.0.1:{port}")).expect("redis config"),
            Some(PerformanceConfig::default()),
            Some(ConnectionConfig::default()),
            Some(ReconnectPolicy::new_constant(0, 100)),
            1,
        )
        .expect("redis pool");
        pool.init().await.expect("redis init");
        (container, pool)
    }

    fn uid(raw: &str) -> UserId {
        UserId::new(raw.to_string()).expect("user id")
    }

    #[test]
    fn disabled_handle_never_queues() {
        let handle = ActiveRegistrationHandle::disabled();
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-disabled"), 1234),
            ActiveRegistrationEnqueueResult::Disabled
        );
    }

    #[test]
    fn handle_throttles_same_user_inside_interval() {
        let (handle, mut rx) = ActiveRegistrationHandle::for_test("pod-a", 30_000, 1);
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-a"), 10_000),
            ActiveRegistrationEnqueueResult::Queued
        );
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-a"), 15_000),
            ActiveRegistrationEnqueueResult::Throttled
        );
        let first = rx.try_recv().expect("first event");
        assert_eq!(first.uid.as_ref(), "user-a");
        assert_eq!(first.last_seen_ms, 10_000);
        assert_eq!(first.pod_id, "pod-a");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queue_full_is_non_fatal() {
        let (handle, _rx) = ActiveRegistrationHandle::for_test("pod-a", 0, 1);
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-a"), 10_000),
            ActiveRegistrationEnqueueResult::Queued
        );
        assert_eq!(
            handle.try_enqueue_activity(&uid("user-b"), 10_001),
            ActiveRegistrationEnqueueResult::Full
        );
    }

    #[tokio::test]
    async fn write_registration_sets_membership_marker_and_ttl() {
        let (_container, pool) = redis_pool().await;
        let event = ActiveRegistrationEvent {
            uid: uid("user-live"),
            last_seen_ms: 1_700_000_000_000,
            pod_id: "pod-1".to_string(),
        };

        write_registration_once(&pool, &event, 300).await.expect("registration");

        let members: Vec<String> = pool.smembers(ACTIVE_USERS_KEY).await.expect("members");
        assert_eq!(members, vec!["user-live".to_string()]);
        let marker: String = pool
            .get(active_user_key(&event.uid))
            .await
            .expect("marker value");
        assert_eq!(marker, "1700000000000|pod-1");
        let marker_ttl: i64 = pool.ttl(active_user_key(&event.uid)).await.expect("marker ttl");
        let set_ttl: i64 = pool.ttl(ACTIVE_USERS_KEY).await.expect("set ttl");
        assert!((1..=300).contains(&marker_ttl), "marker ttl={marker_ttl}");
        assert!((1..=300).contains(&set_ttl), "set ttl={set_ttl}");
    }
}
```

Run:

```bash
cargo test -p gw-ledger active_registration -- --nocapture
```

Expected: FAIL because the module and types are not implemented.

- [ ] **Step 5: Implement `active_registration.rs`**

Use this implementation shape:

```rust
use crate::metrics::{
    CREDIT_ACTIVE_REGISTER_ENQUEUE_TOTAL, CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH,
    CREDIT_ACTIVE_REGISTER_REDIS_TOTAL,
};
use crate::ProtectionConfig;
use fred::clients::RedisPool;
use fred::interfaces::LuaInterface;
use gw_core::ids::UserId;
use gw_runtime::ShutdownToken;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

pub const ACTIVE_USERS_KEY: &str = "credit:active_users";
pub const ACTIVE_USER_PREFIX: &str = "credit:active_user:";

const ACTIVE_REGISTER_LUA: &str = r#"
redis.call('SADD', KEYS[1], ARGV[1])
redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
redis.call('EXPIRE', KEYS[1], ARGV[3])
return 1
"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveRegistrationEnqueueResult {
    Queued,
    Throttled,
    Full,
    Closed,
    Disabled,
}

#[derive(Clone, Debug)]
struct ActiveRegistrationEvent {
    uid: UserId,
    last_seen_ms: u64,
    pod_id: String,
}

#[derive(Clone)]
pub struct ActiveRegistrationHandle {
    enabled: bool,
    pod_id: Arc<str>,
    min_interval_ms: u64,
    last_enqueue_ms: Arc<dashmap::DashMap<UserId, u64>>,
    tx: Option<mpsc::Sender<ActiveRegistrationEvent>>,
}

pub fn active_user_key(uid: &UserId) -> String {
    format!("{ACTIVE_USER_PREFIX}{}", uid.as_ref())
}

impl ActiveRegistrationHandle {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            pod_id: Arc::from(""),
            min_interval_ms: 0,
            last_enqueue_ms: Arc::new(dashmap::DashMap::new()),
            tx: None,
        }
    }

    #[cfg(test)]
    fn for_test(
        pod_id: &str,
        min_interval_ms: u64,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<ActiveRegistrationEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                enabled: true,
                pod_id: Arc::from(pod_id),
                min_interval_ms,
                last_enqueue_ms: Arc::new(dashmap::DashMap::new()),
                tx: Some(tx),
            },
            rx,
        )
    }

    pub fn try_enqueue_activity(
        &self,
        uid: &UserId,
        admitted_at_ms: u64,
    ) -> ActiveRegistrationEnqueueResult {
        if !self.enabled {
            record_enqueue(ActiveRegistrationEnqueueResult::Disabled, 0);
            return ActiveRegistrationEnqueueResult::Disabled;
        }
        let Some(tx) = &self.tx else {
            record_enqueue(ActiveRegistrationEnqueueResult::Closed, 0);
            return ActiveRegistrationEnqueueResult::Closed;
        };

        if self.min_interval_ms > 0 {
            if let Some(last) = self.last_enqueue_ms.get(uid) {
                if admitted_at_ms.saturating_sub(*last) < self.min_interval_ms {
                    record_enqueue(ActiveRegistrationEnqueueResult::Throttled, tx.capacity());
                    return ActiveRegistrationEnqueueResult::Throttled;
                }
            }
        }

        let event = ActiveRegistrationEvent {
            uid: uid.clone(),
            last_seen_ms: admitted_at_ms,
            pod_id: self.pod_id.to_string(),
        };
        match tx.try_send(event) {
            Ok(()) => {
                self.last_enqueue_ms.insert(uid.clone(), admitted_at_ms);
                record_enqueue(ActiveRegistrationEnqueueResult::Queued, tx.capacity());
                ActiveRegistrationEnqueueResult::Queued
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                record_enqueue(ActiveRegistrationEnqueueResult::Full, tx.capacity());
                ActiveRegistrationEnqueueResult::Full
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                record_enqueue(ActiveRegistrationEnqueueResult::Closed, tx.capacity());
                ActiveRegistrationEnqueueResult::Closed
            }
        }
    }
}

pub fn spawn_active_registration_worker(
    redis: RedisPool,
    pod_id: String,
    cfg: &ProtectionConfig,
    shutdown: ShutdownToken,
) -> (ActiveRegistrationHandle, tokio::task::JoinHandle<()>) {
    if !cfg.enabled {
        return (ActiveRegistrationHandle::disabled(), tokio::spawn(async {}));
    }
    let capacity = cfg.active_registration_queue_capacity.max(1);
    let (tx, mut rx) = mpsc::channel(capacity);
    let handle = ActiveRegistrationHandle {
        enabled: true,
        pod_id: Arc::from(pod_id),
        min_interval_ms: cfg
            .active_registration_refresh_min_interval_secs
            .saturating_mul(1000),
        last_enqueue_ms: Arc::new(dashmap::DashMap::new()),
        tx: Some(tx),
    };
    let ttl_secs = cfg.active_registration_ttl_secs.max(1);
    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                maybe_event = rx.recv() => {
                    let Some(event) = maybe_event else { break };
                    if write_with_retry(&redis, &event, ttl_secs, &shutdown).await.is_err() {
                        metrics::counter!(
                            CREDIT_ACTIVE_REGISTER_REDIS_TOTAL,
                            "result" => "retry_exhausted"
                        ).increment(1);
                    }
                }
            }
        }
    });
    (handle, join)
}

async fn write_with_retry(
    redis: &RedisPool,
    event: &ActiveRegistrationEvent,
    ttl_secs: u64,
    shutdown: &ShutdownToken,
) -> Result<(), fred::error::RedisError> {
    let mut delay = Duration::from_millis(50);
    for attempt in 0..=4 {
        match write_registration_once(redis, event, ttl_secs).await {
            Ok(()) => {
                metrics::counter!(
                    CREDIT_ACTIVE_REGISTER_REDIS_TOTAL,
                    "result" => "success"
                ).increment(1);
                return Ok(());
            }
            Err(err) if attempt == 4 => return Err(err),
            Err(_) => {
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(delay) => {
                        delay = (delay * 2).min(Duration::from_secs(1));
                    }
                }
            }
        }
    }
    Ok(())
}

async fn write_registration_once(
    redis: &RedisPool,
    event: &ActiveRegistrationEvent,
    ttl_secs: u64,
) -> Result<(), fred::error::RedisError> {
    let marker_key = active_user_key(&event.uid);
    let marker_value = format!("{}|{}", event.last_seen_ms, event.pod_id);
    let _: i64 = redis
        .eval(
            ACTIVE_REGISTER_LUA,
            vec![ACTIVE_USERS_KEY, marker_key.as_str()],
            vec![event.uid.as_ref(), marker_value.as_str(), &ttl_secs.to_string()],
        )
        .await?;
    Ok(())
}

fn record_enqueue(result: ActiveRegistrationEnqueueResult, remaining_capacity: usize) {
    let result_label = match result {
        ActiveRegistrationEnqueueResult::Queued => "queued",
        ActiveRegistrationEnqueueResult::Throttled => "throttled",
        ActiveRegistrationEnqueueResult::Full => "full",
        ActiveRegistrationEnqueueResult::Closed => "closed",
        ActiveRegistrationEnqueueResult::Disabled => "disabled",
    };
    metrics::counter!(
        CREDIT_ACTIVE_REGISTER_ENQUEUE_TOTAL,
        "result" => result_label
    )
    .increment(1);
    metrics::gauge!(CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH).set(remaining_capacity as f64);
}
```

Then export in `crates/gw-ledger/src/lib.rs`:

```rust
pub mod active_registration;
pub use active_registration::{
    active_user_key, ActiveRegistrationEnqueueResult, ActiveRegistrationHandle,
};
```

Run:

```bash
cargo test -p gw-ledger active_registration -- --nocapture
cargo test -p gw-ledger protection_config -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit Task 1**

```bash
git add crates/gw-ledger/src/active_registration.rs crates/gw-ledger/src/lib.rs crates/gw-ledger/src/protection_config.rs crates/gw-ledger/src/metrics.rs
git commit -m "feat: add rust active registration worker"
```

---

### Task 2: Rust Admission Fallback and Registration Wiring

**Files:**
- Modify: `crates/gw-ledger/src/overflow.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/filters/request.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: `crates/gw-ledger/src/overflow.rs`
- Test: `crates/gw-proxy/src/filters/request.rs`

**Interfaces:**
- Consumes: Task 1 `ActiveRegistrationHandle`.
- Produces:
  - `impl OverflowGate { pub async fn fallback_balance_micro(&self, user_id: &str) -> Result<i64, OverflowError> }`
  - `fn register_admitted_activity(active_registration: &ActiveRegistrationHandle, uid: &UserId, admitted_at_ms: u64)`
  - `async fn available_balance_micro_with_fallback(...) -> AvailableBalance`

- [ ] **Step 1: Write failing `OverflowGate` balance fallback test**

In `crates/gw-ledger/src/overflow.rs`, add a unit test near existing overflow tests:

```rust
#[tokio::test]
async fn fallback_balance_micro_reuses_balance_for_path() {
    let (_redis_container, redis) = test_redis_pool().await;
    let (_pg_container, pg) = test_pg_pool().await;
    sqlx::query(
        "CREATE TABLE wallets (user_id TEXT PRIMARY KEY, recharge_balance_micros BIGINT, gift_balance_micros BIGINT, deleted_at TIMESTAMPTZ)",
    )
    .execute(&pg)
    .await
    .expect("create wallets");
    sqlx::query(
        "INSERT INTO wallets(user_id, recharge_balance_micros, gift_balance_micros, deleted_at) VALUES ($1, $2, $3, NULL)",
    )
    .bind("user-positive")
    .bind(11_i64)
    .bind(22_i64)
    .execute(&pg)
    .await
    .expect("insert wallet");

    let gate = OverflowGate::new(redis, pg, OverflowConfig::default());
    let balance = gate
        .fallback_balance_micro("user-positive")
        .await
        .expect("fallback balance");
    assert_eq!(balance, 33);
}
```

If `overflow.rs` does not already have `test_redis_pool` and `test_pg_pool` helpers, add local helpers using the same `testcontainers` pattern as Task 1 and the existing Postgres test patterns in `gw-proxy` or `gw-store`.

Run:

```bash
cargo test -p gw-ledger fallback_balance_micro_reuses_balance_for_path -- --nocapture
```

Expected: FAIL with missing method.

- [ ] **Step 2: Expose read-only fallback balance**

Add this method to `impl OverflowGate`:

```rust
pub async fn fallback_balance_micro(&self, user_id: &str) -> Result<i64, OverflowError> {
    let (balance, path) = self.balance_for(user_id).await?;
    let result = if balance > 0 { "positive" } else { "zero" };
    metrics::counter!(
        crate::metrics::CREDIT_BALANCE_FALLBACK_TOTAL,
        "result" => result,
        "path" => path
    )
    .increment(1);
    Ok(balance)
}
```

In request code, record `"error"` when this method returns an error so errors do not get hidden inside `OverflowGate`.

Run:

```bash
cargo test -p gw-ledger fallback_balance_micro_reuses_balance_for_path -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Write failing source-order tests for registration boundaries**

`request.rs` is a large integration surface. Add focused source-order tests that prove registration calls land only after admitted normal LLM exits and not inside Gemini Files/task short-circuits:

```rust
#[test]
fn active_registration_is_after_admitted_normal_paths() {
    let src = include_str!("request.rs");
    let lease_success = src.find("ctx.lease_guard = Some(guard);").expect("lease success");
    let expired_success = src.find("\"expired\"").expect("expired branch");
    let insufficient_success = src.find("\"insufficient\"").expect("insufficient branch");
    let nolease_success = src.find("\"no_lease\"").expect("no lease branch");
    let freeze_success = src.find("ctx.freeze_guard = Some(guard);").expect("freeze success");
    let register = src.match_indices("register_admitted_activity(").map(|(idx, _)| idx).collect::<Vec<_>>();

    assert!(register.iter().any(|idx| *idx > lease_success));
    assert!(register.iter().any(|idx| *idx > expired_success));
    assert!(register.iter().any(|idx| *idx > insufficient_success));
    assert!(register.iter().any(|idx| *idx > nolease_success));
    assert!(register.iter().any(|idx| *idx > freeze_success));
}

#[test]
fn active_registration_is_not_inside_task_or_gemini_files_short_circuit() {
    let src = include_str!("request.rs");
    let gemini_start = src.find("if ctx.is_gemini_files").expect("gemini files branch");
    let task_start = src.find("if ctx.is_task").expect("task branch");
    let protected_start = src.find("8.5. Credit Lease").expect("protected branch");
    let before_protected = &src[..protected_start];

    assert!(!before_protected[gemini_start..task_start].contains("register_admitted_activity("));
    assert!(!before_protected[task_start..].contains("register_admitted_activity("));
}
```

Run:

```bash
cargo test -p gw-proxy active_registration_is -- --nocapture
```

Expected: FAIL because `register_admitted_activity` is not wired.

- [ ] **Step 4: Wire `ActiveRegistrationHandle` into proxy and gateway boot**

In `crates/gw-proxy/src/gateway.rs`, add the field and constructor argument:

```rust
active_registration: gw_ledger::ActiveRegistrationHandle,
```

Pass it into the request filter:

```rust
let short_circuit = filters::request::run(
    session,
    ctx,
    &self.auth,
    &self.counters,
    &self.deduct,
    &self.protection_cfg,
    &self.overflow_gate,
    &self.active_registration,
    /* existing arguments remain in their current order after this insertion */
)
.await?;
```

In `crates/gw-gateway/src/lib.rs`, after `instance_id` and Redis are ready, spawn the worker:

```rust
let (active_registration, active_registration_handle) =
    gw_ledger::active_registration::spawn_active_registration_worker(
        (*redis.pool).clone(),
        instance_id.clone(),
        &cfg.protection,
        shutdown_token.clone(),
    );
background_handles.push(active_registration_handle);
```

Use the local vector/name that this file already uses for background tasks. If the file stores handles individually rather than in a vector, follow that existing ownership pattern and keep the join handle alive until shutdown.

Run:

```bash
cargo check -p gw-proxy -p gw-gateway
```

Expected: FAIL until `request::run` accepts the new parameter and uses it.

- [ ] **Step 5: Implement cold balance fallback and admitted-path registration**

In `crates/gw-proxy/src/filters/request.rs`, extend `run` signature:

```rust
active_registration: &gw_ledger::ActiveRegistrationHandle,
```

Replace the step 7 balance read with:

```rust
let balance = available_balance_micro_with_fallback(
    counters,
    overflow_gate,
    &merged.effective_user_id,
    !ctx.is_task && !ctx.is_gemini_files,
)
.await;
sync_local_balance_snapshot(&merged.effective_user_id, balance);
if balance.available <= 0 {
    let err = ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 402,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_BALANCE_INSUFFICIENT),
        message: Cow::Borrowed("user balance exhausted"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(30),
    };
    bail!(session, ctx, &err, &request_id)
}
```

Add the helper near `available_balance_micro`:

```rust
async fn available_balance_micro_with_fallback(
    counters: &CounterStore,
    overflow_gate: &Arc<gw_ledger::overflow::OverflowGate>,
    uid: &UserId,
    allow_fallback: bool,
) -> AvailableBalance {
    let mut balance = available_balance_micro(counters, uid).await;
    if balance.available > 0 || !allow_fallback {
        return balance;
    }
    match overflow_gate.fallback_balance_micro(uid.as_ref()).await {
        Ok(fallback) if fallback > 0 => {
            balance.base_micro = fallback;
            balance.available = fallback
                .saturating_sub(balance.deducted_micro)
                .saturating_sub(balance.frozen_micro);
            if balance.available > 0 {
                metrics::counter!(
                    gw_ledger::metrics::CREDIT_BALANCE_FALLBACK_TOTAL,
                    "result" => "positive"
                )
                .increment(1);
            } else {
                metrics::counter!(
                    gw_ledger::metrics::CREDIT_BALANCE_FALLBACK_TOTAL,
                    "result" => "zero"
                )
                .increment(1);
            }
            balance
        }
        Ok(_) => {
            metrics::counter!(
                gw_ledger::metrics::CREDIT_BALANCE_FALLBACK_TOTAL,
                "result" => "zero"
            )
            .increment(1);
            balance
        }
        Err(err) => {
            tracing::warn!(
                target: "gw_proxy::request",
                user_id = uid.as_ref(),
                error = ?err,
                "cold balance fallback failed"
            );
            metrics::counter!(
                gw_ledger::metrics::CREDIT_BALANCE_FALLBACK_TOTAL,
                "result" => "error"
            )
            .increment(1);
            balance
        }
    }
}
```

Add registration helper:

```rust
fn register_admitted_activity(
    active_registration: &gw_ledger::ActiveRegistrationHandle,
    uid: &UserId,
    admitted_at_ms: u64,
) {
    let result = active_registration.try_enqueue_activity(uid, admitted_at_ms);
    tracing::debug!(
        target: "gw_proxy::request",
        user_id = uid.as_ref(),
        ?result,
        "active registration enqueue result"
    );
}
```

Call it immediately before each admitted return:

```rust
ctx.lease_guard = Some(guard);
register_admitted_activity(active_registration, &merged.effective_user_id, now);
ctx.merged_perm = Some(merged);
ctx.phase_end();
return Ok(false);
```

For overflow-admitted branches, use the same `now` from the protected section:

```rust
register_admitted_activity(active_registration, &merged.effective_user_id, now);
ctx.merged_perm = Some(merged);
ctx.phase_end();
return Ok(false);
```

For cold `RequestFreeze` success:

```rust
ctx.freeze_guard = Some(guard);
register_admitted_activity(
    active_registration,
    &merged.effective_user_id,
    gw_ledger::now_ms(),
);
ctx.merged_perm = Some(merged);
ctx.phase_end();
Ok(false)
```

Run:

```bash
cargo test -p gw-proxy active_registration_is -- --nocapture
cargo check -p gw-proxy -p gw-gateway
```

Expected: PASS.

- [ ] **Step 6: Commit Task 2**

```bash
git add crates/gw-ledger/src/overflow.rs crates/gw-proxy/src/gateway.rs crates/gw-proxy/src/filters/request.rs crates/gw-gateway/src/lib.rs
git commit -m "feat: register admitted rust credit activity"
```

---

### Task 3: Rust Lease Sweep, Config File, Metrics Catalog, and Docs

**Files:**
- Modify: `crates/gw-ledger/src/lease.rs`
- Modify: `crates/gw-ledger/src/ledger_sweep.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-gateway/src/config.rs`
- Modify: `config/default.toml`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: `docs/workflow.md`
- Modify: `docs/go-contracts/main.md`
- Test: `crates/gw-ledger/src/lease.rs`

**Interfaces:**
- Consumes: Task 1 `ProtectionConfig` active TTL fields.
- Produces:
  - `pub fn sweep_leases(now_ms: u64, lease_grace_ms: u64, active_registration_ttl_secs: u64) -> u64`
  - `pub fn sweep_cutoff_ms(expires_at_ms: u64, lease_grace_ms: u64, active_registration_ttl_secs: u64) -> u64`
  - `ledger_sweep::spawn(shutdown, protection_cfg)` or equivalent existing task signature with config.

- [ ] **Step 1: Write failing sweep tests**

Replace the current no-op sweep test with:

```rust
#[test]
fn protected_entries_are_swept_after_grace_and_active_ttl() {
    let _guard = SERIAL.lock().unwrap();
    LEASE_POOL.clear();
    let uid = fresh_uid("sweep-old-protected");
    upsert_protected(&uid, 100, "lease-old".to_string(), 7, 1_000);

    let removed_before = sweep_leases(1_000 + 30_000 + 300_000, 30_000, 300);
    assert_eq!(removed_before, 0);
    assert!(LEASE_POOL.contains_key(&uid));

    let removed_after = sweep_leases(1_000 + 30_000 + 300_000 + 1, 30_000, 300);
    assert_eq!(removed_after, 1);
    assert!(!LEASE_POOL.contains_key(&uid));
}

#[test]
fn sweep_cutoff_saturates_on_large_values() {
    assert_eq!(sweep_cutoff_ms(u64::MAX - 5, 30_000, 300), u64::MAX);
}
```

Run:

```bash
cargo test -p gw-ledger sweep_ -- --nocapture
```

Expected: FAIL because `sweep_leases` still takes no arguments and returns zero.

- [ ] **Step 2: Implement protected lease sweep**

In `lease.rs`:

```rust
pub fn sweep_cutoff_ms(
    expires_at_ms: u64,
    lease_grace_ms: u64,
    active_registration_ttl_secs: u64,
) -> u64 {
    expires_at_ms
        .saturating_add(lease_grace_ms)
        .saturating_add(active_registration_ttl_secs.saturating_mul(1000))
}

pub fn sweep_leases(
    now_ms_val: u64,
    lease_grace_ms: u64,
    active_registration_ttl_secs: u64,
) -> u64 {
    let mut removed = 0_u64;
    LEASE_POOL.retain(|_, state| {
        let expires_at = state.expires_at_ms.load(Ordering::Acquire);
        let keep = now_ms_val <= sweep_cutoff_ms(expires_at, lease_grace_ms, active_registration_ttl_secs);
        if !keep {
            removed += 1;
        }
        keep
    });
    if removed > 0 {
        metrics::counter!(crate::metrics::CREDIT_LEASE_SWEEP_REMOVED_TOTAL).increment(removed);
    }
    removed
}
```

Run:

```bash
cargo test -p gw-ledger sweep_ -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Wire sweep config**

Change `ledger_sweep::spawn` to accept config:

```rust
pub fn spawn(
    shutdown: ShutdownToken,
    protection_cfg: crate::ProtectionConfig,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                _ = tick.tick() => {
                    let removed = crate::lease::sweep_leases(
                        crate::lease::now_ms(),
                        protection_cfg.lease_grace_ms,
                        protection_cfg.active_registration_ttl_secs,
                    );
                    if removed > 0 {
                        tracing::debug!(removed, "swept expired protected leases");
                    }
                    crate::lease::protected_user_count();
                }
            }
        }
    })
}
```

In `crates/gw-gateway/src/lib.rs`, update the call:

```rust
let ledger_sweep_handle =
    gw_ledger::ledger_sweep::spawn(shutdown_token.clone(), cfg.protection.clone());
```

Run:

```bash
cargo check -p gw-ledger -p gw-gateway
```

Expected: PASS.

- [ ] **Step 4: Update config and metrics catalog**

In `config/default.toml` protection block:

```toml
[protection]
enabled = true
lease_grace_ms = 30000
default_output_ceiling_tokens = 8192
active_registration_ttl_secs = 300
active_registration_refresh_min_interval_secs = 30
active_registration_queue_capacity = 8192
```

In `crates/gw-gateway/src/metrics_catalog.rs`, add catalog entries for:

```rust
"gw_credit_active_register_enqueue_total"
"gw_credit_active_register_redis_total"
"gw_credit_active_register_queue_depth"
"gw_credit_lease_sweep_removed_total"
"gw_credit_balance_fallback_total"
```

Use the existing `MetricCatalogEntry` shape in that file and label descriptions:

```text
gw_credit_active_register_enqueue_total: result=queued|throttled|full|closed|disabled
gw_credit_active_register_redis_total: result=success|retry_exhausted
gw_credit_active_register_queue_depth: remaining bounded worker channel capacity
gw_credit_lease_sweep_removed_total: removed expired local protected leases
gw_credit_balance_fallback_total: result=positive|zero|error
```

Run:

```bash
cargo check -p gw-gateway
```

Expected: PASS.

- [ ] **Step 5: Update Rust docs**

Add a short section to `docs/spec/06-billing-ledger.md`:

```markdown
### Rust-Initiated Active Credit Lease Registration

After a normal billable LLM proxy request is admitted, Rust enqueues an active-registration event without awaiting Redis. The worker atomically writes `credit:active_users` and `credit:active_user:<uid>` so Go can renew leases for recently active funded users. Task endpoints and Gemini Files endpoints do not write active registration in this phase.
```

Add to `docs/workflow.md`:

```markdown
Credit lease activation starts from an admitted Rust request. Cold funded users may pass the Redis counter gate through the read-only `OverflowGate` balance fallback, then the active-registration worker marks the user for Go renewal.
```

Add to `docs/go-contracts/main.md`:

```markdown
Rust writes `credit:active_users` and `credit:active_user:<uid>` with marker value `<last_seen_ms>|<pod_id>`. Go treats the marker key as authoritative liveness and may remove stale set members when the marker is missing.
```

Run:

```bash
rg -n "active_registration|credit:active_users|gw_credit_active_register" docs crates/gw-gateway/src/metrics_catalog.rs config/default.toml
```

Expected: shows the new config, docs, and catalog entries.

- [ ] **Step 6: Commit Task 3**

```bash
git add crates/gw-ledger/src/lease.rs crates/gw-ledger/src/ledger_sweep.rs crates/gw-gateway/src/lib.rs crates/gw-gateway/src/config.rs config/default.toml crates/gw-gateway/src/metrics_catalog.rs docs/spec/06-billing-ledger.md docs/workflow.md docs/go-contracts/main.md
git commit -m "feat: sweep inactive rust credit leases"
```

---

### Task 4: Go Redis Contract Constants and Metrics

**Files:**
- Create: `shared/rustpact/credit_active.go`
- Create: `shared/rustpact/credit_active_test.go`
- Modify: `shared/platform/metrics/credit_lease.go`

**Interfaces:**
- Produces:
  - `const CreditActiveUsersKey = "credit:active_users"`
  - `const CreditActiveUserPrefix = "credit:active_user:"`
  - `const CreditLeaseRenewAtKey = "credit:lease_renew_at"`
  - `func CreditActiveUserKey(userID string) string`
  - Go metrics listed in the spec.

- [ ] **Step 1: Write failing Redis contract tests**

Create `shared/rustpact/credit_active_test.go`:

```go
package rustpact

import "testing"

func TestCreditActiveRedisKeys(t *testing.T) {
	if CreditActiveUsersKey != "credit:active_users" {
		t.Fatalf("CreditActiveUsersKey = %q", CreditActiveUsersKey)
	}
	if CreditActiveUserPrefix != "credit:active_user:" {
		t.Fatalf("CreditActiveUserPrefix = %q", CreditActiveUserPrefix)
	}
	if CreditLeaseRenewAtKey != "credit:lease_renew_at" {
		t.Fatalf("CreditLeaseRenewAtKey = %q", CreditLeaseRenewAtKey)
	}
	if got := CreditActiveUserKey("u-1"); got != "credit:active_user:u-1" {
		t.Fatalf("CreditActiveUserKey = %q", got)
	}
}
```

Run:

```bash
go test ./shared/rustpact -run TestCreditActiveRedisKeys -count=1
```

Expected: FAIL with missing constants/functions.

- [ ] **Step 2: Implement Redis contract constants**

Create `shared/rustpact/credit_active.go`:

```go
package rustpact

const (
	CreditActiveUsersKey  = "credit:active_users"
	CreditActiveUserPrefix = "credit:active_user:"
	CreditLeaseRenewAtKey = "credit:lease_renew_at"
)

func CreditActiveUserKey(userID string) string {
	return CreditActiveUserPrefix + userID
}
```

Run:

```bash
go test ./shared/rustpact -run TestCreditActiveRedisKeys -count=1
```

Expected: PASS.

- [ ] **Step 3: Add Go credit lease metrics**

In `shared/platform/metrics/credit_lease.go`, add:

```go
CreditLeaseActiveScanTotal = promauto.NewCounterVec(prometheus.CounterOpts{
	Name: "credit_lease_active_scan_total",
	Help: "LeaseScheduler active registration scan events by result.",
}, []string{"result"})
CreditLeaseActiveUsers = promauto.NewGauge(prometheus.GaugeOpts{
	Name: "credit_lease_active_users",
	Help: "Live active-registration candidates seen in the current LeaseScheduler tick.",
})
CreditLeaseActiveScanDurationSeconds = promauto.NewHistogram(prometheus.HistogramOpts{
	Name:    "credit_lease_active_scan_duration_seconds",
	Help:    "Duration of LeaseScheduler active registration scan batches.",
	Buckets: prometheus.DefBuckets,
})
CreditLeaseActiveScanCursorWrapTotal = promauto.NewCounter(prometheus.CounterOpts{
	Name: "credit_lease_active_scan_cursor_wrap_total",
	Help: "Number of times LeaseScheduler active-user SSCAN cursor returned to zero.",
})
CreditLeaseLeaderTotal = promauto.NewCounterVec(prometheus.CounterOpts{
	Name: "credit_lease_leader_total",
	Help: "Credit lease producer leader acquisition result.",
}, []string{"result"})
```

Run:

```bash
go test ./shared/platform/metrics -run TestDoesNotExist -count=1
```

Expected: PASS with `testing: warning: no tests to run` if the package has no tests.

- [ ] **Step 4: Commit Task 4**

```bash
git add shared/rustpact/credit_active.go shared/rustpact/credit_active_test.go shared/platform/metrics/credit_lease.go
git commit -m "feat: add credit active redis contract"
```

---

### Task 5: Go Active Scan, Persisted Due State, and Leader

**Files:**
- Create: `modules/billing/business/lease_active_registry.go`
- Create: `modules/billing/business/lease_leader.go`
- Modify: `modules/billing/business/lease_scheduler.go`
- Modify: `modules/billing/business/lease_scheduler_test.go`

**Interfaces:**
- Consumes: Task 4 Redis key constants and Go metrics.
- Produces:
  - `type LeaseSelectionMode string`
  - `const LeaseSelectionFullScan`, `LeaseSelectionDual`, `LeaseSelectionActiveOnly`
  - `type LeaseSchedulerConfig struct { SelectionMode LeaseSelectionMode; ActiveScanBatchSize int64; LeaderRequired bool; LeaderKey string; LeaderTTL time.Duration }`
  - `func DefaultLeaseSchedulerConfig() LeaseSchedulerConfig`
  - `func (s *LeaseScheduler) WithConfig(cfg LeaseSchedulerConfig) *LeaseScheduler`
  - `func (s *LeaseScheduler) WithLeader(leader leaseProducerLeader) *LeaseScheduler`
  - `func (s *LeaseScheduler) WithActiveWalletReader(reader activeWalletReader) *LeaseScheduler`
  - `func NewRedisLeaseProducerLeader(r redis.UniversalClient, key string, ttl time.Duration) leaseProducerLeader`

- [ ] **Step 1: Write failing active-only scheduler test**

Add to `modules/billing/business/lease_scheduler_test.go`:

```go
func TestLeaseSchedulerActiveOnlyPushesLiveDueUser(t *testing.T) {
	ctx := context.Background()
	rdb := newLeaseRedis(t)
	require.NoError(t, rdb.SAdd(ctx, rustpact.CreditActiveUsersKey, "user-active").Err())
	require.NoError(t, rdb.Set(ctx, rustpact.CreditActiveUserKey("user-active"), "1000|pod-a", time.Minute).Err())

	sched := newTickScheduler(t, rdb, nil).
		WithConfig(LeaseSchedulerConfig{
			SelectionMode:      LeaseSelectionActiveOnly,
			ActiveScanBatchSize: 500,
			LeaderRequired:     false,
		}).
		WithActiveWalletReader(func(ctx context.Context, userIDs []string) (map[string]walletLeaseRow, error) {
			require.Equal(t, []string{"user-active"}, userIDs)
			return map[string]walletLeaseRow{
				"user-active": {UserID: "user-active", BalanceMicro: 1_000_000},
			}, nil
		})

	require.NoError(t, sched.Tick(ctx))
	stream := rustpact.StreamEventsCounter + ":pod:pod-a"
	entries, err := rdb.XRange(ctx, stream, "-", "+").Result()
	require.NoError(t, err)
	require.Len(t, entries, 1)
	score, err := rdb.ZScore(ctx, rustpact.CreditLeaseRenewAtKey, "user-active").Result()
	require.NoError(t, err)
	require.Greater(t, score, float64(time.Now().UnixMilli()))
}
```

Run:

```bash
go test ./modules/billing/business -run TestLeaseSchedulerActiveOnlyPushesLiveDueUser -count=1
```

Expected: FAIL because selection mode and active wallet reader are missing.

- [ ] **Step 2: Add scheduler config and mode types**

In `lease_scheduler.go`:

```go
type LeaseSelectionMode string

const (
	LeaseSelectionFullScan   LeaseSelectionMode = "full_scan"
	LeaseSelectionDual       LeaseSelectionMode = "dual"
	LeaseSelectionActiveOnly LeaseSelectionMode = "active_only"
)

type LeaseSchedulerConfig struct {
	SelectionMode       LeaseSelectionMode
	ActiveScanBatchSize int64
	LeaderRequired      bool
	LeaderKey           string
	LeaderTTL           time.Duration
}

func DefaultLeaseSchedulerConfig() LeaseSchedulerConfig {
	return LeaseSchedulerConfig{
		SelectionMode:       LeaseSelectionFullScan,
		ActiveScanBatchSize: 500,
		LeaderRequired:      true,
		LeaderKey:           "credit:lease_scheduler:leader",
		LeaderTTL:           5 * time.Second,
	}
}

type activeWalletReader func(ctx context.Context, userIDs []string) (map[string]walletLeaseRow, error)
type leaseProducerLeader interface {
	IsLeader(ctx context.Context) (bool, error)
}
```

Add fields:

```go
cfg               LeaseSchedulerConfig
activeCursor      uint64
readActiveWallets activeWalletReader
leader            leaseProducerLeader
```

Initialize:

```go
cfg: DefaultLeaseSchedulerConfig(),
```

Add methods:

```go
func (s *LeaseScheduler) WithConfig(cfg LeaseSchedulerConfig) *LeaseScheduler {
	if s == nil {
		return s
	}
	if cfg.SelectionMode == "" {
		cfg.SelectionMode = LeaseSelectionFullScan
	}
	if cfg.ActiveScanBatchSize <= 0 {
		cfg.ActiveScanBatchSize = 500
	}
	if cfg.LeaderKey == "" {
		cfg.LeaderKey = "credit:lease_scheduler:leader"
	}
	if cfg.LeaderTTL <= 0 {
		cfg.LeaderTTL = 5 * time.Second
	}
	s.cfg = cfg
	return s
}

func (s *LeaseScheduler) WithLeader(leader leaseProducerLeader) *LeaseScheduler {
	if s != nil {
		s.leader = leader
	}
	return s
}

func (s *LeaseScheduler) WithActiveWalletReader(reader activeWalletReader) *LeaseScheduler {
	if s != nil {
		s.readActiveWallets = reader
	}
	return s
}
```

Run:

```bash
go test ./modules/billing/business -run TestLeaseSchedulerActiveOnlyPushesLiveDueUser -count=1
```

Expected: FAIL until active scan is implemented.

- [ ] **Step 3: Implement active registry scan helpers**

Create `lease_active_registry.go`:

```go
package business

import (
	"context"
	"errors"
	"sort"
	"strings"
	"time"

	"github.com/redis/go-redis/v9"

	pmetrics "github.com/cnxx-tech/ailinkmax-go/shared/platform/metrics"
	"github.com/cnxx-tech/ailinkmax-go/shared/rustpact"
)

func (s *LeaseScheduler) scanActiveRows(ctx context.Context) ([]walletLeaseRow, error) {
	if s == nil || s.r == nil || s.readActiveWallets == nil {
		return nil, nil
	}
	start := time.Now()
	defer func() {
		pmetrics.CreditLeaseActiveScanDurationSeconds.Observe(time.Since(start).Seconds())
	}()

	members, next, err := s.r.SScan(ctx, rustpact.CreditActiveUsersKey, s.activeCursor, "*", s.cfg.ActiveScanBatchSize).Result()
	if err != nil {
		pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("balance_error").Inc()
		return nil, err
	}
	s.activeCursor = next
	if next == 0 {
		pmetrics.CreditLeaseActiveScanCursorWrapTotal.Inc()
	}
	pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("scanned").Add(float64(len(members)))
	if len(members) == 0 {
		pmetrics.CreditLeaseActiveUsers.Set(0)
		return nil, nil
	}

	live := make([]string, 0, len(members))
	pipe := s.r.Pipeline()
	existsCmds := make(map[string]*redis.IntCmd, len(members))
	for _, uid := range members {
		if !validActiveUserID(uid) {
			s.removeActiveUser(ctx, uid)
			pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("malformed").Inc()
			continue
		}
		existsCmds[uid] = pipe.Exists(ctx, rustpact.CreditActiveUserKey(uid))
	}
	if _, err := pipe.Exec(ctx); err != nil && !errors.Is(err, redis.Nil) {
		return nil, err
	}
	for uid, cmd := range existsCmds {
		if cmd.Val() <= 0 {
			s.removeActiveUser(ctx, uid)
			pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("stale").Inc()
			continue
		}
		live = append(live, uid)
	}
	sort.Strings(live)
	pmetrics.CreditLeaseActiveUsers.Set(float64(len(live)))
	pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("live").Add(float64(len(live)))
	if len(live) == 0 {
		return nil, nil
	}

	rowsByUser, err := s.readActiveWallets(ctx, live)
	if err != nil {
		pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("balance_error").Inc()
		return nil, err
	}
	out := make([]walletLeaseRow, 0, len(rowsByUser))
	for _, uid := range live {
		row, ok := rowsByUser[uid]
		if !ok {
			s.removeActiveUser(ctx, uid)
			pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("missing_wallet").Inc()
			continue
		}
		if row.BalanceMicro <= 0 {
			s.removeActiveUser(ctx, uid)
			pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("zero_balance").Inc()
			continue
		}
		out = append(out, row)
	}
	return out, nil
}

func validActiveUserID(uid string) bool {
	trimmed := strings.TrimSpace(uid)
	return uid != "" && uid == trimmed && !strings.ContainsAny(uid, "\r\n\t ")
}

func (s *LeaseScheduler) removeActiveUser(ctx context.Context, uid string) {
	if s == nil || s.r == nil || uid == "" {
		return
	}
	_ = s.r.SRem(ctx, rustpact.CreditActiveUsersKey, uid).Err()
	_ = s.r.ZRem(ctx, rustpact.CreditLeaseRenewAtKey, uid).Err()
}

func (s *LeaseScheduler) isDueRedis(ctx context.Context, userID string, nowMs int64) bool {
	score, err := s.r.ZScore(ctx, rustpact.CreditLeaseRenewAtKey, userID).Result()
	if errors.Is(err, redis.Nil) {
		return true
	}
	if err != nil {
		s.logger.Warn("LeaseScheduler due score read failed; treating user as not due", "user_id", userID, "err", err)
		return false
	}
	return int64(score) <= nowMs
}

func (s *LeaseScheduler) markRenewedRedis(ctx context.Context, userID string, nextRenewAtMs int64) error {
	return s.r.ZAdd(ctx, rustpact.CreditLeaseRenewAtKey, redis.Z{
		Score:  float64(nextRenewAtMs),
		Member: userID,
	}).Err()
}
```

Run:

```bash
go test ./modules/billing/business -run TestLeaseSchedulerActiveOnlyPushesLiveDueUser -count=1
```

Expected: FAIL until `Tick` uses `scanActiveRows` and Redis due state.

- [ ] **Step 4: Update `Tick` mode selection**

Change `Tick`:

```go
func (s *LeaseScheduler) Tick(ctx context.Context) error {
	if s == nil || s.policySvc == nil {
		return nil
	}
	if s.cfg.LeaderRequired {
		if s.leader == nil {
			pmetrics.CreditLeaseLeaderTotal.WithLabelValues("unavailable").Inc()
			return nil
		}
		ok, err := s.leader.IsLeader(ctx)
		if err != nil {
			pmetrics.CreditLeaseLeaderTotal.WithLabelValues("unavailable").Inc()
			return nil
		}
		if !ok {
			pmetrics.CreditLeaseLeaderTotal.WithLabelValues("lost").Inc()
			return nil
		}
		pmetrics.CreditLeaseLeaderTotal.WithLabelValues("acquired").Inc()
	}

	policy, err := s.policySvc.GetByTier(ctx, "default")
	if err != nil {
		return fmt.Errorf("get default budget policy: %w", err)
	}
	if policy == nil || !policy.CreditLease.Enabled {
		return nil
	}

	switch s.cfg.SelectionMode {
	case LeaseSelectionActiveOnly:
		return s.tickActiveRows(ctx, policy)
	case LeaseSelectionDual:
		if err := s.tickFullScan(ctx, policy); err != nil {
			return err
		}
		return s.tickActiveRows(ctx, policy)
	default:
		return s.tickFullScan(ctx, policy)
	}
}
```

Extract existing full scan loop into `tickFullScan`, preserving `listWallets` and the in-memory `renewAt` rollback behavior:

```go
func (s *LeaseScheduler) tickFullScan(ctx context.Context, policy *contracts.BudgetPolicy) error {
	if s.listWallets == nil {
		return nil
	}
	const batchSize int64 = 500
	var offset int64
	nowMs := time.Now().UnixMilli()
	for {
		var rows []walletLeaseRow
		err := retry.Do(ctx, retry.Config{
			OpName:         "select_due_wallets",
			MaxRetries:     5,
			InitialBackoff: 100 * time.Millisecond,
			Multipliers:    []float64{1, 1, 2, 4, 8},
			TotalDeadline:  s.retryDeadline,
			Logger:         s.logger,
		}, func(ctx context.Context) error {
			var err error
			rows, err = s.listWallets(ctx, batchSize, offset)
			return err
		})
		if err != nil {
			return fmt.Errorf("list due wallets: %w", err)
		}
		pmetrics.CreditLeaseDueUsers.Set(float64(len(rows)))
		if len(rows) == 0 {
			return nil
		}
		for _, row := range rows {
			if !s.isDue(row.UserID, nowMs) {
				continue
			}
			if err := s.PushLease(ctx, row.UserID, row.BalanceMicro, policy.CreditLease, policy.Version); err != nil {
				s.logger.Error("LeaseScheduler.PushLease failed", "user_id", row.UserID, "err", err)
				continue
			}
			s.markRenewed(row.UserID, nowMs+policy.CreditLease.LeaseTTLMaxMs/2)
		}
		offset += int64(len(rows))
		if int64(len(rows)) < batchSize {
			return nil
		}
	}
}
```

Add active tick:

```go
func (s *LeaseScheduler) tickActiveRows(ctx context.Context, policy *contracts.BudgetPolicy) error {
	rows, err := s.scanActiveRows(ctx)
	if err != nil {
		return fmt.Errorf("scan active registrations: %w", err)
	}
	nowMs := time.Now().UnixMilli()
	due := 0
	for _, row := range rows {
		if !s.isDueRedis(ctx, row.UserID, nowMs) {
			pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("skipped_not_due").Inc()
			continue
		}
		due++
		if err := s.PushLease(ctx, row.UserID, row.BalanceMicro, policy.CreditLease, policy.Version); err != nil {
			s.logger.Error("LeaseScheduler.PushLease failed", "user_id", row.UserID, "err", err)
			continue
		}
		if err := s.markRenewedRedis(ctx, row.UserID, nowMs+policy.CreditLease.LeaseTTLMaxMs/2); err != nil {
			return fmt.Errorf("mark renewed active user %s: %w", row.UserID, err)
		}
		pmetrics.CreditLeaseActiveScanTotal.WithLabelValues("pushed").Inc()
	}
	pmetrics.CreditLeaseDueUsers.Set(float64(due))
	return nil
}
```

Run:

```bash
go test ./modules/billing/business -run TestLeaseSchedulerActiveOnlyPushesLiveDueUser -count=1
```

Expected: PASS.

- [ ] **Step 5: Add stale, malformed, zero balance, not-due, push-failure, and dual/full-scan tests**

Add tests:

```go
func TestLeaseSchedulerActiveOnlyCleansStaleMalformedAndZeroBalanceUsers(t *testing.T) {
	ctx := context.Background()
	rdb := newLeaseRedis(t)
	require.NoError(t, rdb.SAdd(ctx, rustpact.CreditActiveUsersKey, "live-zero", "stale", "bad user").Err())
	require.NoError(t, rdb.Set(ctx, rustpact.CreditActiveUserKey("live-zero"), "1000|pod-a", time.Minute).Err())
	require.NoError(t, rdb.Set(ctx, rustpact.CreditActiveUserKey("bad user"), "1000|pod-a", time.Minute).Err())
	require.NoError(t, rdb.ZAdd(ctx, rustpact.CreditLeaseRenewAtKey, redis.Z{Score: 1, Member: "stale"}).Err())

	sched := newTickScheduler(t, rdb, nil).
		WithConfig(LeaseSchedulerConfig{SelectionMode: LeaseSelectionActiveOnly, ActiveScanBatchSize: 500, LeaderRequired: false}).
		WithActiveWalletReader(func(ctx context.Context, userIDs []string) (map[string]walletLeaseRow, error) {
			return map[string]walletLeaseRow{"live-zero": {UserID: "live-zero", BalanceMicro: 0}}, nil
		})

	require.NoError(t, sched.Tick(ctx))
	members, err := rdb.SMembers(ctx, rustpact.CreditActiveUsersKey).Result()
	require.NoError(t, err)
	require.Empty(t, members)
	require.Equal(t, int64(0), rdb.ZCard(ctx, rustpact.CreditLeaseRenewAtKey).Val())
}

func TestLeaseSchedulerActiveOnlySkipsNotDueUser(t *testing.T) {
	ctx := context.Background()
	rdb := newLeaseRedis(t)
	require.NoError(t, rdb.SAdd(ctx, rustpact.CreditActiveUsersKey, "user-active").Err())
	require.NoError(t, rdb.Set(ctx, rustpact.CreditActiveUserKey("user-active"), "1000|pod-a", time.Minute).Err())
	require.NoError(t, rdb.ZAdd(ctx, rustpact.CreditLeaseRenewAtKey, redis.Z{
		Score: float64(time.Now().Add(time.Hour).UnixMilli()), Member: "user-active",
	}).Err())
	sched := newTickScheduler(t, rdb, nil).
		WithConfig(LeaseSchedulerConfig{SelectionMode: LeaseSelectionActiveOnly, ActiveScanBatchSize: 500, LeaderRequired: false}).
		WithActiveWalletReader(func(ctx context.Context, userIDs []string) (map[string]walletLeaseRow, error) {
			return map[string]walletLeaseRow{"user-active": {UserID: "user-active", BalanceMicro: 1_000_000}}, nil
		})
	require.NoError(t, sched.Tick(ctx))
	entries, err := rdb.XRange(ctx, rustpact.StreamEventsCounter+":pod:pod-a", "-", "+").Result()
	require.NoError(t, err)
	require.Empty(t, entries)
}

func TestLeaseSchedulerDualRunsFullScanAndActiveScan(t *testing.T) {
	ctx := context.Background()
	rdb := newLeaseRedis(t)
	require.NoError(t, rdb.SAdd(ctx, rustpact.CreditActiveUsersKey, "active-user").Err())
	require.NoError(t, rdb.Set(ctx, rustpact.CreditActiveUserKey("active-user"), "1000|pod-a", time.Minute).Err())
	sched := newTickScheduler(t, rdb, []walletLeaseRow{{UserID: "full-user", BalanceMicro: 1_000_000}}).
		WithConfig(LeaseSchedulerConfig{SelectionMode: LeaseSelectionDual, ActiveScanBatchSize: 500, LeaderRequired: false}).
		WithActiveWalletReader(func(ctx context.Context, userIDs []string) (map[string]walletLeaseRow, error) {
			return map[string]walletLeaseRow{"active-user": {UserID: "active-user", BalanceMicro: 1_000_000}}, nil
		})
	require.NoError(t, sched.Tick(ctx))
	entries, err := rdb.XRange(ctx, rustpact.StreamEventsCounter+":pod:pod-a", "-", "+").Result()
	require.NoError(t, err)
	require.Len(t, entries, 2)
}
```

Run:

```bash
go test ./modules/billing/business -run 'TestLeaseScheduler(ActiveOnly|Dual)' -count=1
```

Expected: PASS.

- [ ] **Step 6: Implement and test Redis leader**

Create `lease_leader.go`:

```go
package business

import (
	"context"
	"errors"
	"time"

	"github.com/oklog/ulid/v2"
	"github.com/redis/go-redis/v9"
)

type redisLeaseProducerLeader struct {
	r     redis.UniversalClient
	key   string
	token string
	ttl   time.Duration
}

func NewRedisLeaseProducerLeader(r redis.UniversalClient, key string, ttl time.Duration) leaseProducerLeader {
	if key == "" {
		key = "credit:lease_scheduler:leader"
	}
	if ttl <= 0 {
		ttl = 5 * time.Second
	}
	return &redisLeaseProducerLeader{
		r:     r,
		key:   key,
		token: ulid.Make().String(),
		ttl:   ttl,
	}
}

func (l *redisLeaseProducerLeader) IsLeader(ctx context.Context) (bool, error) {
	if l == nil || l.r == nil {
		return false, nil
	}
	ok, err := l.r.SetNX(ctx, l.key, l.token, l.ttl).Result()
	if err != nil {
		return false, err
	}
	if ok {
		return true, nil
	}
	current, err := l.r.Get(ctx, l.key).Result()
	if errors.Is(err, redis.Nil) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if current != l.token {
		return false, nil
	}
	if err := l.r.Expire(ctx, l.key, l.ttl).Err(); err != nil {
		return false, err
	}
	return true, nil
}
```

Add test:

```go
func TestLeaseSchedulerLeaderUnavailableSkipsTick(t *testing.T) {
	ctx := context.Background()
	rdb := newLeaseRedis(t)
	sched := newTickScheduler(t, rdb, []walletLeaseRow{{UserID: "full-user", BalanceMicro: 1_000_000}}).
		WithConfig(LeaseSchedulerConfig{SelectionMode: LeaseSelectionFullScan, ActiveScanBatchSize: 500, LeaderRequired: true})

	require.NoError(t, sched.Tick(ctx))
	entries, err := rdb.XRange(ctx, rustpact.StreamEventsCounter+":pod:pod-a", "-", "+").Result()
	require.NoError(t, err)
	require.Empty(t, entries)
}

func TestRedisLeaseProducerLeaderAllowsOnlyOneToken(t *testing.T) {
	ctx := context.Background()
	rdb := newLeaseRedis(t)
	a := NewRedisLeaseProducerLeader(rdb, "credit:test:leader", time.Minute)
	b := NewRedisLeaseProducerLeader(rdb, "credit:test:leader", time.Minute)
	ok, err := a.IsLeader(ctx)
	require.NoError(t, err)
	require.True(t, ok)
	ok, err = b.IsLeader(ctx)
	require.NoError(t, err)
	require.False(t, ok)
	ok, err = a.IsLeader(ctx)
	require.NoError(t, err)
	require.True(t, ok)
}
```

Run:

```bash
go test ./modules/billing/business -run 'Test(LeaseSchedulerLeaderUnavailableSkipsTick|RedisLeaseProducerLeaderAllowsOnlyOneToken)' -count=1
```

Expected: PASS.

- [ ] **Step 7: Implement batch wallet reader**

In `lease_active_registry.go` or `lease_scheduler.go`, add:

```go
func listWalletsByUserIDs(ctx context.Context, db *pgxpool.Pool, userIDs []string) (map[string]walletLeaseRow, error) {
	if db == nil || len(userIDs) == 0 {
		return map[string]walletLeaseRow{}, nil
	}
	rows, err := db.Query(ctx, `
SELECT user_id, recharge_balance_micros + gift_balance_micros AS balance_micro
FROM wallets
WHERE status = 'active'
  AND user_id = ANY($1)`, userIDs)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	out := make(map[string]walletLeaseRow, len(userIDs))
	for rows.Next() {
		var row walletLeaseRow
		if err := rows.Scan(&row.UserID, &row.BalanceMicro); err != nil {
			return nil, err
		}
		out[row.UserID] = row
	}
	return out, rows.Err()
}
```

Update `WithTickDeps`:

```go
s.readActiveWallets = func(ctx context.Context, userIDs []string) (map[string]walletLeaseRow, error) {
	return listWalletsByUserIDs(ctx, db, userIDs)
}
```

Run:

```bash
go test ./modules/billing/business -run TestLeaseScheduler -count=1
```

Expected: PASS.

- [ ] **Step 8: Commit Task 5**

```bash
git add modules/billing/business/lease_active_registry.go modules/billing/business/lease_leader.go modules/billing/business/lease_scheduler.go modules/billing/business/lease_scheduler_test.go
git commit -m "feat: renew credit leases from active users"
```

---

### Task 6: Go Module Wiring, Environment Knobs, and Docs

**Files:**
- Modify: `modules/billing/module.go`
- Modify: `docs/business/billing/business.md`
- Modify: `docs/business/budgetpolicy/business.md` if policy-facing knobs are introduced.

**Interfaces:**
- Consumes: Task 5 `LeaseSchedulerConfig`, `NewRedisLeaseProducerLeader`.
- Produces environment contract:
  - `GO_BILLING_CREDIT_LEASE_SELECTION_MODE=full_scan|dual|active_only`
  - `GO_BILLING_CREDIT_LEASE_ACTIVE_SCAN_BATCH_SIZE=500`
  - `GO_BILLING_CREDIT_LEASE_LEADER_REQUIRED=true|false`
  - `GO_BILLING_CREDIT_LEASE_LEADER_TTL_MS=5000`

- [ ] **Step 1: Write failing config parser tests**

If `modules/billing/module_test.go` already has environment helper tests, add there; otherwise add a new test file `modules/billing/credit_lease_scheduler_config_test.go`:

```go
package billing

import (
	"testing"
	"time"

	"github.com/cnxx-tech/ailinkmax-go/modules/billing/business"
	"github.com/stretchr/testify/require"
)

func TestCreditLeaseSchedulerConfigFromEnv(t *testing.T) {
	t.Setenv("GO_BILLING_CREDIT_LEASE_SELECTION_MODE", "active_only")
	t.Setenv("GO_BILLING_CREDIT_LEASE_ACTIVE_SCAN_BATCH_SIZE", "123")
	t.Setenv("GO_BILLING_CREDIT_LEASE_LEADER_REQUIRED", "false")
	t.Setenv("GO_BILLING_CREDIT_LEASE_LEADER_TTL_MS", "9000")

	cfg := creditLeaseSchedulerConfigFromEnv()
	require.Equal(t, business.LeaseSelectionActiveOnly, cfg.SelectionMode)
	require.EqualValues(t, 123, cfg.ActiveScanBatchSize)
	require.False(t, cfg.LeaderRequired)
	require.Equal(t, 9*time.Second, cfg.LeaderTTL)
}

func TestCreditLeaseSchedulerConfigFromEnvRejectsUnknownMode(t *testing.T) {
	t.Setenv("GO_BILLING_CREDIT_LEASE_SELECTION_MODE", "unknown")
	cfg := creditLeaseSchedulerConfigFromEnv()
	require.Equal(t, business.LeaseSelectionFullScan, cfg.SelectionMode)
}
```

Run:

```bash
go test ./modules/billing -run TestCreditLeaseSchedulerConfigFromEnv -count=1
```

Expected: FAIL with missing function.

- [ ] **Step 2: Implement environment config**

In `modules/billing/module.go`, add:

```go
func creditLeaseSchedulerConfigFromEnv() business.LeaseSchedulerConfig {
	cfg := business.DefaultLeaseSchedulerConfig()
	switch mode := os.Getenv("GO_BILLING_CREDIT_LEASE_SELECTION_MODE"); mode {
	case string(business.LeaseSelectionFullScan):
		cfg.SelectionMode = business.LeaseSelectionFullScan
	case string(business.LeaseSelectionDual):
		cfg.SelectionMode = business.LeaseSelectionDual
	case string(business.LeaseSelectionActiveOnly):
		cfg.SelectionMode = business.LeaseSelectionActiveOnly
	case "":
		cfg.SelectionMode = business.LeaseSelectionFullScan
	default:
		cfg.SelectionMode = business.LeaseSelectionFullScan
	}
	cfg.ActiveScanBatchSize = int64(envInt("GO_BILLING_CREDIT_LEASE_ACTIVE_SCAN_BATCH_SIZE", int(cfg.ActiveScanBatchSize)))
	if raw := os.Getenv("GO_BILLING_CREDIT_LEASE_LEADER_REQUIRED"); raw == "false" {
		cfg.LeaderRequired = false
	}
	if ttlMs := envInt("GO_BILLING_CREDIT_LEASE_LEADER_TTL_MS", int(cfg.LeaderTTL/time.Millisecond)); ttlMs > 0 {
		cfg.LeaderTTL = time.Duration(ttlMs) * time.Millisecond
	}
	return cfg
}
```

Update `runCreditLeaseScheduler`:

```go
cfg := creditLeaseSchedulerConfigFromEnv()
scheduler := business.NewLeaseScheduler(d.Redis, activePodsFromRedis{r: d.Redis}).
	WithConfig(cfg).
	WithTickDeps(policies, d.DB)
if cfg.LeaderRequired {
	scheduler.WithLeader(business.NewRedisLeaseProducerLeader(d.Redis, cfg.LeaderKey, cfg.LeaderTTL))
}
log.Info("billing: credit lease scheduler enabled",
	"selection_mode", cfg.SelectionMode,
	"active_scan_batch_size", cfg.ActiveScanBatchSize,
	"leader_required", cfg.LeaderRequired,
	"leader_ttl", cfg.LeaderTTL.String(),
)
return scheduler.Run(ctx)
```

Remove the old warning that says the worker has no distributed leader election.

Run:

```bash
go test ./modules/billing -run TestCreditLeaseSchedulerConfigFromEnv -count=1
go test ./modules/billing/business -run TestLeaseScheduler -count=1
```

Expected: PASS.

- [ ] **Step 3: Update Go docs**

In `docs/business/billing/business.md`, add:

```markdown
### Credit Lease Scheduler Selection

`GO_BILLING_CREDIT_LEASE_SCHEDULER=true` starts the protected lease producer. The selector is controlled by `GO_BILLING_CREDIT_LEASE_SELECTION_MODE`:

- `full_scan`: rollback mode; scans funded active wallets as before.
- `dual`: rollout mode; runs full scan and active-registration scan.
- `active_only`: steady state; renews only users present in `credit:active_users` with a live `credit:active_user:<uid>` marker.

The scheduler uses a Redis leader key so only one Go replica pushes `protected_lease` messages. If leadership is unavailable, a tick is skipped rather than running multiple producers.
```

Add the environment table:

```markdown
| Env | Default | Meaning |
|---|---:|---|
| `GO_BILLING_CREDIT_LEASE_SELECTION_MODE` | `full_scan` | `full_scan`, `dual`, or `active_only` |
| `GO_BILLING_CREDIT_LEASE_ACTIVE_SCAN_BATCH_SIZE` | `500` | Max active users scanned per tick |
| `GO_BILLING_CREDIT_LEASE_LEADER_REQUIRED` | `true` | Skip ticks unless the process holds the Redis leader key |
| `GO_BILLING_CREDIT_LEASE_LEADER_TTL_MS` | `5000` | Redis leader lease TTL in milliseconds |
```

Run:

```bash
rg -n "GO_BILLING_CREDIT_LEASE_SELECTION_MODE|active_only|credit:active_users" docs/business modules/billing
```

Expected: shows code and docs entries.

- [ ] **Step 4: Commit Task 6**

```bash
git add modules/billing/module.go modules/billing/*credit_lease*_test.go docs/business/billing/business.md docs/business/budgetpolicy/business.md
git commit -m "feat: configure active credit lease scheduler"
```

If `docs/business/budgetpolicy/business.md` was not changed, omit it from `git add`.

---

### Task 7: Cross-Repo Verification and Delivery Notes

**Files:**
- Modify if missing coverage is found:
  - Rust docs from Task 3
  - Go docs from Task 6
- No new production interfaces.

**Interfaces:**
- Consumes: Tasks 1-6.
- Produces: verified branch state and clear rollout handoff.

- [ ] **Step 1: Run Rust focused tests**

From `/Users/whereslow/Desktop/work/Cnewxus/.worktrees/credit-lease-rust-initiated-gateway`:

```bash
cargo test -p gw-ledger protection_config active_registration sweep_ fallback_balance_micro -- --nocapture
cargo test -p gw-proxy active_registration_is -- --nocapture
cargo check -p gw-ledger -p gw-proxy -p gw-gateway
```

Expected: all tests and checks PASS.

- [ ] **Step 2: Run Go focused tests**

From `/Users/whereslow/Desktop/work/Cnewxus/.worktrees/credit-lease-rust-initiated-go`:

```bash
go test ./shared/rustpact -run TestCreditActiveRedisKeys -count=1
go test ./shared/platform/metrics -run TestDoesNotExist -count=1
go test ./modules/billing/business -run 'Test(LeaseScheduler|RedisLeaseProducerLeader)' -count=1
go test ./modules/billing -run TestCreditLeaseSchedulerConfigFromEnv -count=1
```

Expected: all tests PASS. The metrics package command may report no tests to run but must exit with status 0.

- [ ] **Step 3: Run broader package verification**

Rust:

```bash
cargo test -p gw-ledger -p gw-proxy -p gw-gateway
```

Go:

```bash
go test ./modules/billing/... ./shared/rustpact/... ./shared/platform/metrics/...
```

Expected: all tests PASS.

- [ ] **Step 4: Run placeholder and contract scans**

Rust:

```bash
bad_words='TO''DO|TB''D|implement ''later|fill in ''details'
rg -n "$bad_words|register_admitted_activity|credit:active_users|credit:active_user|gw_credit_active_register" crates docs config/default.toml
```

Expected: no placeholder wording introduced; active-registration symbols appear in implementation, tests, docs, and config.

Go:

```bash
bad_words='TO''DO|TB''D|implement ''later|fill in ''details'
rg -n "$bad_words|credit:active_users|credit:active_user|credit:lease_renew_at|GO_BILLING_CREDIT_LEASE_SELECTION_MODE" modules shared docs
```

Expected: no placeholder wording introduced; Redis contract and environment knobs appear in implementation, tests, and docs.

- [ ] **Step 5: Review diffs for user-facing behavior**

Rust:

```bash
git diff --stat HEAD~3..HEAD
git diff HEAD~3..HEAD -- crates/gw-proxy/src/filters/request.rs crates/gw-ledger/src/active_registration.rs crates/gw-ledger/src/lease.rs
```

Confirm:

- Balance rejection still returns existing 402.
- Gemini Files and task branches return before active registration.
- Registration occurs after admission, not before admission.
- Request hot path only calls `try_enqueue_activity` and does not await Redis.
- Lease sweep uses `expires_at_ms + lease_grace_ms + active_registration_ttl_secs * 1000`.

Go:

```bash
git diff --stat HEAD~3..HEAD
git diff HEAD~3..HEAD -- modules/billing/business/lease_scheduler.go modules/billing/business/lease_active_registry.go modules/billing/business/lease_leader.go modules/billing/module.go
```

Confirm:

- `full_scan` still works for rollback.
- `dual` runs full scan and active scan.
- `active_only` reads only active registrations.
- Active scan removes stale, malformed, missing-wallet, and zero-balance users from both Redis structures.
- Push failure leaves `credit:lease_renew_at` unchanged.
- Missing leader skips ticks.

- [ ] **Step 6: Commit any verification-only doc corrections**

If verification found missing docs or comments:

```bash
git add docs crates modules shared config/default.toml
git commit -m "docs: document rust initiated credit leases"
```

If no files changed:

```bash
git status --short
```

Expected: clean worktree.

- [ ] **Step 7: Prepare rollout handoff**

Record these branch heads for the user:

```bash
git -C /Users/whereslow/Desktop/work/Cnewxus/.worktrees/credit-lease-rust-initiated-gateway log --oneline -5
git -C /Users/whereslow/Desktop/work/Cnewxus/.worktrees/credit-lease-rust-initiated-go log --oneline -5
```

The handoff must state:

- Rust branch path and latest commit.
- Go branch path and latest commit.
- Verification commands run and whether they passed.
- Rollout order: Rust with Go `full_scan`, verify registrations, Go `dual`, then Go `active_only`.
- Rollback: switch Go back to `dual` or `full_scan`; Rust writes are harmless in rollback.

---

## Self-Review

### Spec Coverage

- Rust first funded normal LLM request without pre-existing lease: Task 2 cold counter fallback and RequestFreeze registration.
- Rust no sync Go call and no hot-path Redis await: Task 1 worker plus Task 2 `try_enqueue_activity`.
- Go no steady-state full funded-wallet scan: Task 5 `active_only` selection.
- Active users refresh on lease and cold paths: Task 2 registration at lease success, overflow success, and freeze success.
- Inactive users age out: Task 3 local lease sweep and Task 5 Go stale marker cleanup.
- Balance authority remains Go/Postgres: Task 2 uses `OverflowGate` read-only balance fallback, no settlement cost changes.
- Redis contract exact keys: Task 1 Rust constants and Task 4 Go constants.
- Go leader requirement: Task 5 Redis leader and Task 6 module wiring.
- Metrics and docs: Tasks 1, 3, 4, and 6.
- Tests listed in the spec: mapped across Tasks 1, 2, 3, 5, 6, and 7.

### Placeholder Scan

Run after saving this plan:

```bash
bad_words='TB''D|TO''DO|implement ''later|fill in ''details|Add ''appropriate|Write ''tests for the above|Similar ''to Task'
rg -n "$bad_words" docs/superpowers/plans/2026-06-21-rust-initiated-credit-lease-implementation.md
```

Expected: no matches.

### Type Consistency

- Rust config fields are defined in Task 1 and consumed by Tasks 1, 2, and 3 with the same names.
- `ActiveRegistrationHandle::try_enqueue_activity(&self, uid: &UserId, admitted_at_ms: u64)` is defined in Task 1 and consumed by Task 2.
- `OverflowGate::fallback_balance_micro(&self, user_id: &str)` is defined and consumed in Task 2.
- Go `LeaseSelectionMode` constants are defined in Task 5 and consumed by Tasks 5 and 6.
- Go `activeWalletReader` returns `map[string]walletLeaseRow`, defined and consumed in Task 5.
- Redis key constants are defined in Task 4 and consumed by Task 5.
