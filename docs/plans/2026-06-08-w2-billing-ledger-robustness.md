# W2 Billing Ledger Robustness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Do **not** run `git add`, `git commit`, or modify W0/W1 files outside the W2 touches listed here.

**Goal:** Complete W2 from `docs/plans/2026-06-08-roadmap-implementation-program.md`: never silently lose billing records, synchronize soft-freeze counters across Pods, and replace the flat freeze estimate with an `entity:estimate_pricing`-backed conservative estimate.

**Architecture:** Implement C-delta first by moving DLQ fallback mechanics into a focused `gw-ledger::dlq` module and wiring the flusher to retry and persist failed DLQ writes. Implement C-gamma by adding a Rust-owned frozen-counter sync worker plus `events:counter ns=user_balance` handling for Go-owned balance fields. Implement C-beta by adding an `EstimatePricing` entity/cache and using it in the request freeze path while preserving the invariant that Rust never computes monetary cost.

**Tech Stack:** Rust 1.88, Tokio, fred Redis client, Moka cache, serde/serde_json, existing `gw-ledger`, `gw-store`, `gw-proxy`, `gw-gateway` crates.

---

## Scope And Assumptions

- W0/W1 are already implemented by the parent agent. Do not touch their code unless a fresh W2 compile error proves a direct dependency.
- Current W2 source gaps are in `docs/ROADMAP.md` section `## 06 billing-ledger`: C-delta, C-gamma, C-beta.
- The repository has no generic `data_dir` config. Use a new `[ledger] emergency_dlq_dir = "data/ledger"` setting as the concrete `${data_dir}` home for emergency JSONL files.
- Go-contracts already describe `events:counter ns=user_balance`, `counter:user:<uid>:frozen_micro`, and `entity:estimate_pricing:<model>`. This plan makes Rust match those contracts.
- No blocking clarification is required before implementation. If operators want a different default emergency directory, change only `config/default.toml` and `LedgerCfg` in Task 2.

## File Map

- `crates/gw-ledger/src/dlq.rs`: new focused module for DLQ envelope schema, retry item state, emergency JSONL writer, and Redis DLQ/permanent-failed helpers.
- `crates/gw-ledger/src/flusher.rs`: wire C-delta fallback chain and C-gamma frozen delta worker into the existing ledger flusher surface.
- `crates/gw-ledger/src/freeze.rs`: enqueue Rust-owned `frozen_micro` deltas on `RequestFreeze::new`, `release`, and `Drop`.
- `crates/gw-ledger/src/counter_handler.rs`: handle Go-owned `events:counter ns=user_balance` updates for `base_micro` and `deducted_micro`.
- `crates/gw-ledger/src/estimate_pricing.rs`: new C-beta type and pure conservative estimate formula.
- `crates/gw-ledger/src/usage.rs`: keep `estimate_pricing_version` as the emitted schema field; tests should assert nonzero when C-beta is active.
- `crates/gw-ledger/src/lib.rs`: export new W2 modules/APIs.
- `crates/gw-store/src/entity_store.rs`, `crates/gw-store/src/loader.rs`: add `EstimatePricing` cache and loader for `entity:estimate_pricing:<model>`.
- `crates/gw-core/src/entity.rs`: add `EstimatePricing` wire type.
- `crates/gw-proxy/src/ctx.rs`, `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/filters/logging.rs`, `crates/gw-proxy/src/billing_lifecycle.rs`: carry freeze estimate version and use estimate pricing in request admission and UsageRecord emission.
- `crates/gw-gateway/src/config.rs`, `crates/gw-gateway/src/lib.rs`, `config/default.toml`: add `ledger.emergency_dlq_dir`, pass it to `Ledger::start`, and wire estimate-pricing reader into the proxy.
- `crates/gw-gateway/src/metrics_catalog.rs`: register new W2 metrics.
- `docs/spec/06-billing-ledger.md`, `docs/spec/README.md`, `docs/go-contracts/parts/01-go-as-producer.md`, `docs/go-contracts/parts/02-go-as-consumer.md`, `docs/ROADMAP.md`: move completed W2 behavior into as-built docs.

---

### Task 1: C-delta DLQ Envelope And Emergency File Writer

**Files:**
- Create: `crates/gw-ledger/src/dlq.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Test: inline unit tests in `crates/gw-ledger/src/dlq.rs`

- [ ] **Step 1: Write the failing tests**

Add `crates/gw-ledger/src/dlq.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;
    use crate::usage::{UsageRecord, UsageStatus, VendorExposureState};

    fn record() -> UsageRecord {
        UsageRecord {
            ts_ms: 1,
            request_id: "req-dlq-1".into(),
            trace_id: "trace-dlq-1".into(),
            user_id: UserId::from("u-dlq"),
            child_id: None,
            api_key_id: None,
            client_protocol: "openai".into(),
            upstream_protocol: "openai".into(),
            model: ModelName::from("gpt-4o"),
            vendor_id: VendorId::from("v1"),
            vendor_key_id: KeyId::from("k1"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: UsageSnapshot { input_tokens: 10, output_tokens: 5, ..Default::default() },
            attempt_index: 0,
            vendor_exposure_state: VendorExposureState::Settled,
            usage_provenance: Default::default(),
            latency_ms: 10,
            ttft_ms: None,
            is_stream: false,
            freeze_amount_micro: 100,
            estimate_pricing_version: 7,
            markup_chain_snapshot: MarkupChainSnapshot::identity(),
            operation_id: None,
            pod_id: "pod-a".into(),
            credits_consumed: None,
            coef_table_version: None,
        }
    }

    #[test]
    fn dlq_envelope_matches_go_contract_shape() {
        let envelope = DlqEnvelope::new(record(), "primary xadd failed", 3, DlqLayer::Mem, 1234);
        let json = serde_json::to_value(&envelope).unwrap();

        assert_eq!(json["dlq_meta"]["first_failure_ts_ms"], 1234);
        assert_eq!(json["dlq_meta"]["primary_failure_reason"], "primary xadd failed");
        assert_eq!(json["dlq_meta"]["retry_count_before_dlq"], 3);
        assert_eq!(json["dlq_meta"]["dlq_layer"], "mem");
        assert_eq!(json["usage_record"]["request_id"], "req-dlq-1");
        assert!(json.get("cost_micro").is_none(), "Rust DLQ must not carry cost");
    }

    #[test]
    fn emergency_writer_appends_json_lines() {
        let root = std::env::temp_dir().join(format!("gw-dlq-test-{}", uuid::Uuid::new_v4()));
        let writer = EmergencyDlqWriter::new(root.clone());

        writer.append(&DlqEnvelope::new(record(), "redis down", 2, DlqLayer::Disk, 99)).unwrap();
        writer.append(&DlqEnvelope::new(record(), "redis down", 3, DlqLayer::Disk, 100)).unwrap();

        let entries = std::fs::read_dir(&root).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(entries.len(), 1, "one JSONL file per process is enough");
        let text = std::fs::read_to_string(entries[0].path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"dlq_layer\":\"disk\""));
    }
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p gw-ledger dlq_envelope_matches_go_contract_shape
```

Expected: compile failure because `DlqEnvelope`, `DlqLayer`, and `EmergencyDlqWriter` are undefined.

- [ ] **Step 3: Implement the module**

Replace the top of `crates/gw-ledger/src/dlq.rs` with this implementation, keeping the tests from Step 1 below it:

```rust
use crate::usage::UsageRecord;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlqLayer {
    Redis,
    Mem,
    Disk,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DlqMeta {
    pub first_failure_ts_ms: u64,
    pub primary_failure_reason: String,
    pub retry_count_before_dlq: u32,
    pub dlq_layer: DlqLayer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DlqEnvelope {
    pub dlq_meta: DlqMeta,
    pub usage_record: UsageRecord,
}

impl DlqEnvelope {
    pub fn new(
        usage_record: UsageRecord,
        primary_failure_reason: impl Into<String>,
        retry_count_before_dlq: u32,
        dlq_layer: DlqLayer,
        first_failure_ts_ms: u64,
    ) -> Self {
        Self {
            dlq_meta: DlqMeta {
                first_failure_ts_ms,
                primary_failure_reason: primary_failure_reason.into(),
                retry_count_before_dlq,
                dlq_layer,
            },
            usage_record,
        }
    }
}

pub struct EmergencyDlqWriter {
    root: PathBuf,
    file: Mutex<Option<File>>,
}

impl EmergencyDlqWriter {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            file: Mutex::new(None),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn append(&self, envelope: &DlqEnvelope) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let path = self.root.join(format!(
                "gw-ledger-emergency-dlq-{}.jsonl",
                uuid::Uuid::new_v4()
            ));
            *guard = Some(OpenOptions::new().create(true).append(true).open(path)?);
        }
        let file = guard.as_mut().expect("file initialized above");
        serde_json::to_writer(&mut *file, envelope)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}
```

Update `crates/gw-ledger/src/lib.rs`:

```rust
pub mod dlq;
```

- [ ] **Step 4: Run tests to verify GREEN**

Run:

```bash
cargo test -p gw-ledger dlq_envelope_matches_go_contract_shape
cargo test -p gw-ledger emergency_writer_appends_json_lines
```

Expected: both tests pass.

---

### Task 2: C-delta Flusher Fallback Chain And Retry Worker

**Files:**
- Modify: `crates/gw-ledger/src/flusher.rs`
- Modify: `crates/gw-gateway/src/config.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `config/default.toml`
- Test: `crates/gw-ledger/tests/dlq.rs`

- [ ] **Step 1: Write the failing integration test**

Add this ignored test to `crates/gw-ledger/tests/dlq.rs`:

```rust
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn dlq_stream_failure_writes_emergency_jsonl() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4).await.unwrap();
    let client = redis.pool.next();

    let _: () = client.set("usage:records", "blocked", None, None, false).await.unwrap();
    let _: () = client.set("usage:records:dlq", "blocked", None, None, false).await.unwrap();

    let dir = std::env::temp_dir().join(format!("gw-emergency-dlq-{}", uuid::Uuid::new_v4()));
    let ledger = Ledger::start(
        redis.clone(),
        "test-instance".to_string(),
        1024,
        1,
        Duration::from_millis(50),
        dir.clone(),
    );

    ledger.try_send(LedgerItem { record: sample_record(), clear: None }).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let files = std::fs::read_dir(&dir).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(!files.is_empty(), "DLQ stream failure must leave an emergency JSONL file");
    let text = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(text.contains("\"usage_record\""));
    assert!(text.contains("\"dlq_layer\":\"disk\""));
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p gw-ledger dlq_stream_failure_writes_emergency_jsonl -- --ignored
```

Expected: compile failure because `Ledger::start` does not accept `emergency_dlq_dir`.

- [ ] **Step 3: Implement the fallback chain**

In `crates/gw-ledger/src/flusher.rs`:

- Import the new module:

```rust
use crate::dlq::{DlqEnvelope, DlqLayer, EmergencyDlqWriter};
use std::path::PathBuf;
use std::sync::Arc;
```

- Change `Ledger::start` signature to include the directory:

```rust
pub fn start(
    redis: RedisCtx,
    instance_id: String,
    channel_capacity: usize,
    batch_size: usize,
    flush_interval: Duration,
    emergency_dlq_dir: PathBuf,
) -> Self {
```

- Create one writer before spawning:

```rust
let emergency_writer = Arc::new(EmergencyDlqWriter::new(emergency_dlq_dir));
let worker_emergency_writer = emergency_writer.clone();
```

- Pass `&worker_emergency_writer` to every internal `flush` call and change `flush` to:

```rust
async fn flush(
    redis: &RedisCtx,
    instance_id: &str,
    buf: &mut Vec<LedgerItem>,
    emergency_writer: &EmergencyDlqWriter,
) {
```

- Replace every direct `dlq_xadd(...).await.is_err() { gateway_usage_records_lost_total }` with:

```rust
send_to_dlq_or_emergency(
    redis,
    emergency_writer,
    item_record,
    failure_reason,
    retry_count,
    DlqLayer::Redis,
).await;
```

- Add this helper near `dlq_xadd`:

```rust
async fn send_to_dlq_or_emergency(
    redis: &RedisCtx,
    emergency_writer: &EmergencyDlqWriter,
    record: UsageRecord,
    reason: String,
    retry_count: u32,
    layer: DlqLayer,
) {
    let first_failure_ts_ms = crate::now_ms();
    let redis_envelope = DlqEnvelope::new(
        record.clone(),
        reason.clone(),
        retry_count,
        layer,
        first_failure_ts_ms,
    );
    match dlq_xadd(redis, redis_envelope).await {
        Ok(()) => {}
        Err(e) => {
            let disk_envelope = DlqEnvelope::new(
                record,
                format!("{reason}; dlq_xadd: {e}"),
                retry_count.saturating_add(1),
                DlqLayer::Disk,
                first_failure_ts_ms,
            );
            match emergency_writer.append(&disk_envelope) {
                Ok(()) => {
                    metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "success")
                        .increment(1);
                }
                Err(err) => {
                    metrics::counter!("gw_usage_dlq_emergency_file_total", "outcome" => "error")
                        .increment(1);
                    metrics::counter!("gateway_usage_records_lost_total").increment(1);
                    tracing::error!(target: "gw_ledger", error = %err, "emergency DLQ JSONL append failed");
                }
            }
        }
    }
}
```

- Change `dlq_xadd` to accept `DlqEnvelope` and write `serde_json::to_string(&envelope)` as the `payload`.

In `crates/gw-gateway/src/config.rs`, add to `LedgerCfg`:

```rust
#[serde(default = "default_emergency_dlq_dir")]
pub emergency_dlq_dir: String,
```

Add:

```rust
fn default_emergency_dlq_dir() -> String {
    "data/ledger".to_string()
}
```

Validate it:

```rust
if self.ledger.emergency_dlq_dir.trim().is_empty() {
    anyhow::bail!("invalid config: ledger.emergency_dlq_dir must not be empty");
}
```

In `config/default.toml` under `[ledger]` add:

```toml
emergency_dlq_dir = "data/ledger"
```

In `crates/gw-gateway/src/lib.rs`, pass:

```rust
std::path::PathBuf::from(cfg.ledger.emergency_dlq_dir.clone())
```

to `Ledger::start`.

- [ ] **Step 4: Run focused checks**

Run:

```bash
cargo test -p gw-ledger dlq_stream_failure_writes_emergency_jsonl -- --ignored
cargo test -p gw-gateway config
```

Expected: ignored DLQ test passes when Docker is available; config tests pass.

---

### Task 3: C-gamma Frozen Micro Redis Sync Worker

**Files:**
- Modify: `crates/gw-ledger/src/flusher.rs`
- Modify: `crates/gw-ledger/src/freeze.rs`
- Test: `crates/gw-ledger/tests/freeze.rs`
- Test: `crates/gw-ledger/tests/flusher.rs`

- [ ] **Step 1: Write the failing unit test**

Add to `crates/gw-ledger/tests/freeze.rs`:

```rust
#[test]
fn request_freeze_enqueues_positive_and_negative_frozen_deltas() {
    gw_ledger::flusher::reset_frozen_delta_sink_for_test();
    let seen = gw_ledger::flusher::install_test_frozen_delta_sink();
    let uid = UserId::from("u-frozen-sync");

    {
        let guard = RequestFreeze::new(uid.clone(), 777);
        guard.release();
    }

    let deltas = seen.lock().unwrap().clone();
    assert_eq!(deltas, vec![(uid, 777), (UserId::from("u-frozen-sync"), -777)]);
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
cargo test -p gw-ledger request_freeze_enqueues_positive_and_negative_frozen_deltas
```

Expected: compile failure because the frozen delta sink helpers do not exist.

- [ ] **Step 3: Implement the in-process sink**

In `crates/gw-ledger/src/flusher.rs`, add:

```rust
use gw_core::ids::UserId;
use once_cell::sync::OnceCell;
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenDelta {
    pub uid: UserId,
    pub delta: i64,
}

type FrozenDeltaSink = Box<dyn Fn(FrozenDelta) + Send + Sync + 'static>;
static FROZEN_DELTA_SINK: OnceCell<FrozenDeltaSink> = OnceCell::new();

pub fn enqueue_frozen_delta(uid: UserId, delta: i64) {
    if let Some(sink) = FROZEN_DELTA_SINK.get() {
        sink(FrozenDelta { uid, delta });
    } else {
        metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "no_sink").increment(1);
    }
}

#[cfg(test)]
static TEST_FROZEN_DELTAS: once_cell::sync::Lazy<Mutex<Vec<(UserId, i64)>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(Vec::new()));

#[cfg(test)]
pub fn reset_frozen_delta_sink_for_test() {
    TEST_FROZEN_DELTAS.lock().unwrap().clear();
}

#[cfg(test)]
pub fn install_test_frozen_delta_sink() -> &'static Mutex<Vec<(UserId, i64)>> {
    let _ = FROZEN_DELTA_SINK.set(Box::new(|delta| {
        TEST_FROZEN_DELTAS
            .lock()
            .unwrap()
            .push((delta.uid, delta.delta));
    }));
    &TEST_FROZEN_DELTAS
}
```

In `crates/gw-ledger/src/freeze.rs`:

- After local `fetch_add` in `RequestFreeze::new`:

```rust
crate::flusher::enqueue_frozen_delta(uid.clone(), amount as i64);
```

- After local `fetch_sub` in `RequestFreeze::release`:

```rust
crate::flusher::enqueue_frozen_delta(self.uid.clone(), -(self.freeze_amount as i64));
```

- In `Drop` after local rollback:

```rust
crate::flusher::enqueue_frozen_delta(self.uid.clone(), -(self.freeze_amount as i64));
```

- [ ] **Step 4: Run unit tests**

Run:

```bash
cargo test -p gw-ledger request_freeze_enqueues_positive_and_negative_frozen_deltas
cargo test -p gw-ledger freeze
```

Expected: tests pass. Existing tests still assert `deducted_micro` is untouched.

- [ ] **Step 5: Wire Redis HINCRBY worker**

In `Ledger::start`, create an mpsc channel for `FrozenDelta`, install the sink, and spawn a worker:

```rust
let (frozen_tx, mut frozen_rx) = mpsc::channel::<FrozenDelta>(channel_capacity.min(8192).max(1));
let _ = FROZEN_DELTA_SINK.set(Box::new(move |delta| {
    if frozen_tx.try_send(delta).is_err() {
        metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "full").increment(1);
    } else {
        metrics::counter!("gw_frozen_delta_enqueue_total", "outcome" => "ok").increment(1);
    }
}));
let frozen_redis = redis.clone();
tokio::spawn(async move {
    while let Some(delta) = frozen_rx.recv().await {
        let key = format!("counter:user:{}:frozen_micro", delta.uid);
        let client = frozen_redis.pool.next();
        match client.incr_by::<i64, _, _>(&key, delta.delta).await {
            Ok(_) => metrics::counter!("gw_frozen_delta_redis_total", "outcome" => "ok").increment(1),
            Err(err) => {
                metrics::counter!("gw_frozen_delta_redis_total", "outcome" => "error").increment(1);
                tracing::warn!(target: "gw_ledger", error = %err, key, delta = delta.delta, "frozen_micro HINCRBY failed");
            }
        }
    }
});
```

The code uses `INCRBY` on key `counter:user:<uid>:frozen_micro`, matching `docs/go-contracts/parts/01-go-as-producer.md` section 1.4.

- [ ] **Step 6: Add and run ignored Redis test**

Add to `crates/gw-ledger/tests/flusher.rs`:

```rust
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn request_freeze_syncs_frozen_micro_to_redis() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4).await.unwrap();
    let _ledger = Ledger::start(
        redis.clone(),
        "test-instance".to_string(),
        1024,
        2,
        Duration::from_millis(100),
        std::env::temp_dir().join(format!("gw-ledger-test-{}", uuid::Uuid::new_v4())),
    );

    let uid = UserId::from("u-frozen-redis");
    let guard = gw_ledger::RequestFreeze::new(uid.clone(), 111);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = redis.pool.next();
    let frozen: i64 = client.get("counter:user:u-frozen-redis:frozen_micro").await.unwrap();
    assert_eq!(frozen, 111);

    guard.release();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let frozen: i64 = client.get("counter:user:u-frozen-redis:frozen_micro").await.unwrap();
    assert_eq!(frozen, 0);
}
```

Run:

```bash
cargo test -p gw-ledger request_freeze_syncs_frozen_micro_to_redis -- --ignored
```

Expected: passes when Docker is available.

---

### Task 4: C-gamma `events:counter ns=user_balance` Handler

**Files:**
- Modify: `crates/gw-ledger/src/counter_handler.rs`
- Test: inline unit tests in `crates/gw-ledger/src/counter_handler.rs`

- [ ] **Step 1: Write the failing tests**

Add to the existing test module in `counter_handler.rs`:

```rust
#[test]
fn user_balance_base_micro_overwrites_local_value_after() {
    let uid_str = "u_user_balance_base";
    LEASE_POOL.remove(&UserId::from(uid_str));
    gw_store::deduction::DEDUCT.remove(&UserId::from(uid_str));

    let h = LedgerCounterHandler;
    let f = fields(&[
        ("ns", "user_balance"),
        ("uid", uid_str),
        ("op", "incrby"),
        ("field", "base_micro"),
        ("delta", "500"),
        ("value_after", "1234"),
        ("ts_ms", "1"),
    ]);

    assert_eq!(h.handle("user_balance", &f), DispatchOutcome::Handled);
    let state = gw_store::deduction::DEDUCT.get(&UserId::from(uid_str)).unwrap();
    assert_eq!(state.base_micro.load(std::sync::atomic::Ordering::Acquire), 1234);
    assert_eq!(state.deducted_micro.load(std::sync::atomic::Ordering::Acquire), 0);
}

#[test]
fn user_balance_deducted_micro_overwrites_local_value_after() {
    let uid_str = "u_user_balance_deducted";
    gw_store::deduction::DEDUCT.remove(&UserId::from(uid_str));

    let h = LedgerCounterHandler;
    let f = fields(&[
        ("ns", "user_balance"),
        ("uid", uid_str),
        ("op", "incrby"),
        ("field", "deducted_micro"),
        ("delta", "99"),
        ("value_after", "456"),
        ("ts_ms", "1"),
    ]);

    assert_eq!(h.handle("user_balance", &f), DispatchOutcome::Handled);
    let state = gw_store::deduction::DEDUCT.get(&UserId::from(uid_str)).unwrap();
    assert_eq!(state.deducted_micro.load(std::sync::atomic::Ordering::Acquire), 456);
}

#[test]
fn user_balance_rejects_frozen_micro_from_go() {
    let h = LedgerCounterHandler;
    let f = fields(&[
        ("ns", "user_balance"),
        ("uid", "u_bad_frozen"),
        ("op", "incrby"),
        ("field", "frozen_micro"),
        ("delta", "1"),
        ("value_after", "1"),
        ("ts_ms", "1"),
    ]);

    assert_eq!(h.handle("user_balance", &f), DispatchOutcome::InvalidFields);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p gw-ledger user_balance_
```

Expected: tests fail because `user_balance` returns `UnknownNs`.

- [ ] **Step 3: Implement handler**

In `counter_handler.rs`:

- Update the match:

```rust
"user_balance" => handle_user_balance(fields),
```

- Add:

```rust
fn handle_user_balance(fields: &HashMap<String, String>) -> DispatchOutcome {
    let uid = match fields.get("uid").or_else(|| fields.get("user_id")) {
        Some(s) => UserId::from(s.as_str()),
        None => return DispatchOutcome::InvalidFields,
    };
    let op = fields.get("op").map(String::as_str).unwrap_or("");
    if op != "incrby" {
        return DispatchOutcome::InvalidFields;
    }
    let field = fields.get("field").map(String::as_str).unwrap_or("");
    let value_after = match fields.get("value_after").and_then(|s| s.parse::<i64>().ok()) {
        Some(v) => v,
        None => return DispatchOutcome::InvalidFields,
    };

    let state = gw_store::deduction::DEDUCT
        .entry(uid)
        .or_insert_with(|| {
            std::sync::Arc::new(gw_store::deduction::DeductionState {
                base_micro: std::sync::atomic::AtomicI64::new(0),
                deducted_micro: std::sync::atomic::AtomicI64::new(0),
                frozen_micro: std::sync::atomic::AtomicI64::new(0),
            })
        })
        .clone();

    match field {
        "base_micro" => state
            .base_micro
            .store(value_after, std::sync::atomic::Ordering::Release),
        "deducted_micro" => state
            .deducted_micro
            .store(value_after, std::sync::atomic::Ordering::Release),
        _ => return DispatchOutcome::InvalidFields,
    }
    DispatchOutcome::Handled
}
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p gw-ledger user_balance_
cargo test -p gw-store counter_dispatch
```

Expected: tests pass.

---

### Task 5: C-beta EstimatePricing Entity, Loader, And Cache

**Files:**
- Modify: `crates/gw-core/src/entity.rs`
- Modify: `crates/gw-store/src/loader.rs`
- Modify: `crates/gw-store/src/entity_store.rs`
- Test: `crates/gw-store/tests/estimate_pricing.rs`

- [ ] **Step 1: Write failing entity/store tests**

Create `crates/gw-store/tests/estimate_pricing.rs`:

```rust
use gw_core::entity::EstimatePricing;
use gw_core::ids::ModelName;
use gw_core::versioned::Versioned;
use gw_store::EntityStore;
use std::sync::Arc;

#[tokio::test]
async fn estimate_pricing_cache_returns_inserted_snapshot() {
    let store = EntityStore::new();
    let model = ModelName::from("gpt-4o");
    store.estimate_pricing.insert(
        model.clone(),
        Arc::new(Versioned {
            version: 42,
            value: Arc::new(EstimatePricing {
                model: model.clone(),
                conservative_per_token_micro: 60,
                safety_factor: 1.25,
                default_max_tokens: 4096,
                hard_cap_tokens: 200_000,
            }),
        }),
    ).await;

    let loaded = store.estimate_pricing_or_load(&model).await.unwrap();
    assert_eq!(loaded.version, 42);
    assert_eq!(loaded.value.conservative_per_token_micro, 60);
}

#[test]
fn estimate_pricing_deserializes_go_contract_payload() {
    let pricing: EstimatePricing = serde_json::from_value(serde_json::json!({
        "model": "gpt-4o",
        "conservative_per_token_micro": 60,
        "safety_factor": 1.1,
        "default_max_tokens": 4096,
        "hard_cap_tokens": 200000
    })).unwrap();

    assert_eq!(pricing.model, ModelName::from("gpt-4o"));
    assert_eq!(pricing.hard_cap_tokens, 200000);
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p gw-store estimate_pricing
```

Expected: compile failure because `EstimatePricing`, `estimate_pricing`, and `estimate_pricing_or_load` are undefined.

- [ ] **Step 3: Add entity and loader**

In `crates/gw-core/src/entity.rs` add:

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct EstimatePricing {
    pub model: ModelName,
    pub conservative_per_token_micro: u64,
    pub safety_factor: f64,
    pub default_max_tokens: u32,
    pub hard_cap_tokens: u32,
}
```

In `crates/gw-store/src/loader.rs`:

- Add `EstimatePricing` to the `use gw_core::entity::{...}` list.
- Add:

```rust
pub async fn load_estimate_pricing(
    ctx: &LoaderCtx,
    id: &ModelName,
) -> anyhow::Result<Versioned<EstimatePricing>> {
    load_entity(ctx, "estimate_pricing", id.as_ref(), "SELECT payload, version FROM estimate_pricing WHERE model = $1 AND deleted_at IS NULL", |v| {
        serde_json::from_value::<EstimatePricing>(v.clone()).map_err(Into::into)
    })
    .await
}
```

- Add `"estimate_pricing"` to the 1800s TTL arm in `ttl_for`.

In `crates/gw-store/src/entity_store.rs`:

- Import `load_estimate_pricing`.
- Add cache field:

```rust
pub estimate_pricing: Cache<ModelName, Arc<Versioned<EstimatePricing>>>,
```

- Initialize it with the same capacity/TTL shape as `pricing`.
- Add:

```rust
pub async fn estimate_pricing_or_load(
    &self,
    id: &ModelName,
) -> Option<Arc<Versioned<EstimatePricing>>> {
    if let Some(v) = self.estimate_pricing.get(id).await {
        return Some(v);
    }
    let loader = self.loader.as_ref()?.clone();
    let deadline = self.cold_load_timeout;
    self.estimate_pricing
        .try_get_with_by_ref(id, async move {
            with_cold_load_timeout(
                "estimate_pricing",
                deadline,
                load_estimate_pricing(&loader, id),
            )
            .await
            .map(Arc::new)
            .ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
        })
        .await
        .ok()
}
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p gw-store estimate_pricing
```

Expected: tests pass.

---

### Task 6: C-beta Conservative Estimate Formula

**Files:**
- Create: `crates/gw-ledger/src/estimate_pricing.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Test: inline unit tests in `crates/gw-ledger/src/estimate_pricing.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/gw-ledger/src/estimate_pricing.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::EstimatePricing;
    use gw_core::ids::ModelName;

    #[test]
    fn conservative_freeze_uses_input_plus_capped_output_and_safety_factor() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 10,
            safety_factor: 1.5,
            default_max_tokens: 100,
            hard_cap_tokens: 120,
        };

        let estimate = compute_freeze_estimate(&pricing, 20, Some(200), 1.0);

        assert_eq!(estimate, 1_800);
    }

    #[test]
    fn conservative_freeze_uses_default_output_when_request_omits_max_tokens() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 2,
            safety_factor: 1.0,
            default_max_tokens: 50,
            hard_cap_tokens: 100,
        };

        assert_eq!(compute_freeze_estimate(&pricing, 10, None, 2.0), 240);
    }
}
```

- [ ] **Step 2: Run tests to verify RED**

Run:

```bash
cargo test -p gw-ledger conservative_freeze_
```

Expected: compile failure because `compute_freeze_estimate` is undefined.

- [ ] **Step 3: Implement formula**

Above the tests add:

```rust
use gw_core::entity::EstimatePricing;

pub fn compute_freeze_estimate(
    pricing: &EstimatePricing,
    input_tokens: u32,
    request_max_tokens: Option<u32>,
    markup: f32,
) -> u64 {
    let output_cap = request_max_tokens
        .unwrap_or(pricing.default_max_tokens)
        .min(pricing.hard_cap_tokens);
    let total_tokens = u64::from(input_tokens).saturating_add(u64::from(output_cap));
    let base = total_tokens.saturating_mul(pricing.conservative_per_token_micro);
    let safety = pricing.safety_factor.max(1.0);
    let markup = f64::from(markup.max(1.0));
    ((base as f64) * safety * markup).ceil().max(1.0) as u64
}
```

In `crates/gw-ledger/src/lib.rs` add:

```rust
pub mod estimate_pricing;
pub use estimate_pricing::compute_freeze_estimate;
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p gw-ledger conservative_freeze_
```

Expected: tests pass.

---

### Task 7: C-beta Proxy Freeze Path And UsageRecord Version

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/request.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Modify: `crates/gw-proxy/src/billing_lifecycle.rs`
- Test: `crates/gw-proxy/tests/estimate_pricing.rs`
- Test: existing `crates/gw-ledger/tests/usage_schema.rs`

- [ ] **Step 1: Write failing proxy test**

Create `crates/gw-proxy/tests/estimate_pricing.rs`:

```rust
use gw_core::entity::EstimatePricing;
use gw_core::ids::ModelName;

#[test]
fn request_freeze_estimate_uses_estimate_pricing_snapshot() {
    let pricing = EstimatePricing {
        model: ModelName::from("gpt-4o"),
        conservative_per_token_micro: 10,
        safety_factor: 1.0,
        default_max_tokens: 100,
        hard_cap_tokens: 200,
    };

    let estimate = gw_proxy::filters::request::freeze_estimate_with_pricing(
        20,
        Some(50),
        1.2,
        Some((&pricing, 9)),
    );

    assert_eq!(estimate.amount_micro, 840);
    assert_eq!(estimate.estimate_pricing_version, 9);
}

#[test]
fn request_freeze_estimate_falls_back_to_flat_formula_without_snapshot() {
    let estimate = gw_proxy::filters::request::freeze_estimate_with_pricing(20, None, 1.2, None);

    assert_eq!(estimate.amount_micro, 24);
    assert_eq!(estimate.estimate_pricing_version, 0);
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p gw-proxy request_freeze_estimate_
```

Expected: compile failure because `freeze_estimate_with_pricing` is undefined or private.

- [ ] **Step 3: Add request context field**

In `crates/gw-proxy/src/ctx.rs`, add to `ReqCtx`:

```rust
pub estimate_pricing_version: u64,
```

Initialize it to `0` in `Default`/constructor code.

- [ ] **Step 4: Add pure estimate helper**

In `crates/gw-proxy/src/filters/request.rs`, replace the private flat helper with:

```rust
pub struct FreezeEstimate {
    pub amount_micro: u64,
    pub estimate_pricing_version: u64,
}

pub fn freeze_estimate_with_pricing(
    est_input_tokens: u32,
    req_max_tokens: Option<u32>,
    markup: f32,
    pricing: Option<(&gw_core::entity::EstimatePricing, u64)>,
) -> FreezeEstimate {
    match pricing {
        Some((p, version)) => FreezeEstimate {
            amount_micro: gw_ledger::compute_freeze_estimate(p, est_input_tokens, req_max_tokens, markup),
            estimate_pricing_version: version,
        },
        None => {
            let base = est_input_tokens as u64;
            let with_markup = (base as f32 * markup.max(1.0)) as u64;
            FreezeEstimate {
                amount_micro: with_markup.max(1),
                estimate_pricing_version: 0,
            }
        }
    }
}
```

- [ ] **Step 5: Wire async lookup in request filter**

In `run`, before daily quota and hard-cap calls compute:

```rust
let estimate_pricing = store
    .estimate_pricing_or_load(&model_name)
    .await
    .map(|v| (v.value.as_ref().clone(), v.version));
let estimate_pricing_ref = estimate_pricing.as_ref().map(|(p, v)| (p, *v));
let freeze_est = freeze_estimate_with_pricing(
    est_input_tokens,
    req_max_tokens,
    merged.markup_chain.product,
    estimate_pricing_ref,
);
ctx.estimate_pricing_version = freeze_est.estimate_pricing_version;
```

Use `freeze_est.amount_micro` for daily quota pre-check, hard-cap check, and `RequestFreeze::new`. Remove duplicate calls to the old flat helper.

- [ ] **Step 6: Emit version into UsageRecord**

In both `crates/gw-proxy/src/filters/logging.rs` and `crates/gw-proxy/src/billing_lifecycle.rs`, replace:

```rust
estimate_pricing_version: 0,
```

with:

```rust
estimate_pricing_version: ctx.estimate_pricing_version,
```

- [ ] **Step 7: Run tests**

Run:

```bash
cargo test -p gw-proxy request_freeze_estimate_
cargo test -p gw-proxy --locked
cargo test -p gw-ledger usage_schema
```

Expected: tests pass.

---

### Task 8: Metrics And Documentation Closure

**Files:**
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: `docs/spec/README.md`
- Modify: `docs/go-contracts/parts/01-go-as-producer.md`
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`
- Modify: `docs/ROADMAP.md`
- Test: `crates/gw-gateway/tests/metrics_catalog.rs`

- [ ] **Step 1: Write failing metrics test expectations**

In `crates/gw-gateway/tests/metrics_catalog.rs`, add these names to the existing expected metric list:

```rust
"gw_usage_dlq_emergency_file_total",
"gw_frozen_delta_enqueue_total",
"gw_frozen_delta_redis_total",
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
cargo test -p gw-gateway metrics_catalog
```

Expected: failure listing missing metrics.

- [ ] **Step 3: Register metrics**

In `crates/gw-gateway/src/metrics_catalog.rs`, add:

```rust
MetricSpec {
    name: "gw_usage_dlq_emergency_file_total",
    kind: MetricKind::Counter,
    help: "Emergency JSONL fallback writes for usage-record DLQ failures; labels=outcome(success|error)",
},
MetricSpec {
    name: "gw_frozen_delta_enqueue_total",
    kind: MetricKind::Counter,
    help: "RequestFreeze frozen_micro delta enqueue outcomes; labels=outcome(ok|full|no_sink)",
},
MetricSpec {
    name: "gw_frozen_delta_redis_total",
    kind: MetricKind::Counter,
    help: "Redis counter:user:<uid>:frozen_micro sync outcomes; labels=outcome(ok|error)",
},
```

- [ ] **Step 4: Update docs**

Make these exact documentation moves:

- In `docs/spec/06-billing-ledger.md`, update the Flusher section to describe: primary `usage:records`, Redis DLQ envelope with `dlq_meta` + `usage_record`, retry queue, emergency JSONL, and `usage:records:dlq:permanent_failed` if implemented by Task 2.
- In `docs/spec/06-billing-ledger.md`, update Freeze section to state `RequestFreeze` updates local `DEDUCT` and asynchronously `INCRBY counter:user:<uid>:frozen_micro`.
- In `docs/spec/06-billing-ledger.md`, update Freeze estimate section to state `EstimatePricing` formula and fallback behavior when the entity is absent.
- In `docs/spec/README.md`, change the `06-billing-ledger` row from `built; C-beta/gamma/delta + lease fixes in ROADMAP` to `built; remaining open items listed in ROADMAP`.
- In `docs/ROADMAP.md`, delete the C-beta, C-gamma, and C-delta entries from `## 06 billing-ledger`.
- In `docs/go-contracts/parts/01-go-as-producer.md`, confirm `entity:estimate_pricing` matches the Rust type names.
- In `docs/go-contracts/parts/02-go-as-consumer.md`, confirm DLQ `dlq_meta` field names match `DlqEnvelope`.

- [ ] **Step 5: Run documentation and metrics checks**

Run:

```bash
cargo test -p gw-gateway metrics_catalog
bash scripts/check-spec.sh docs/spec/06-billing-ledger.md
```

Expected: both pass.

---

## Final Verification For W2

After all tasks pass their focused checks, run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-invariants.sh
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f" || exit 1; done
git diff --check
```

Expected:

- `cargo fmt` exits 0.
- `cargo clippy` exits 0.
- `cargo test --workspace --locked` exits 0.
- `scripts/check-invariants.sh` exits 0; advisory warnings are acceptable only if marked advisory by the script.
- Every `docs/spec/0*.md` prints `ok`.
- `git diff --check` prints nothing and exits 0.

## Subagent Split

- Implementer A: Task 1 and Task 2 only. Write scope: `gw-ledger/src/dlq.rs`, `gw-ledger/src/flusher.rs`, gateway config/lib, `config/default.toml`, DLQ tests.
- Implementer B: Task 3 and Task 4 only. Write scope: `gw-ledger/src/flusher.rs`, `gw-ledger/src/freeze.rs`, `gw-ledger/src/counter_handler.rs`, freeze/flusher tests. Coordinate with Implementer A because both touch `flusher.rs`; do not run both writers concurrently unless one owns a separate branch/worktree.
- Implementer C: Task 5 through Task 7 only. Write scope: entity/store/estimate pricing/proxy request and usage emission.
- Implementer D: Task 8 only after A/B/C complete. Write scope: metrics catalog and docs.
- Reviewers: run spec compliance review first against `docs/ROADMAP.md ## 06 billing-ledger` and this plan; then run code-quality review for concurrency, no-cost invariant, Redis key naming, and failure-mode durability.

## Self-Review

- Spec coverage: C-delta is covered by Tasks 1, 2, and 8; C-gamma is covered by Tasks 3, 4, and 8; C-beta is covered by Tasks 5, 6, 7, and 8.
- Placeholder scan: this plan contains no `TBD`, `TODO`, or undefined follow-up task. The implementation assumes `ledger.emergency_dlq_dir = "data/ledger"` when no existing `data_dir` exists.
- Type consistency: `EstimatePricing` lives in `gw-core::entity`, is loaded by `gw-store`, computed by `gw-ledger::compute_freeze_estimate`, and consumed by `gw-proxy::filters::request::freeze_estimate_with_pricing`. `DlqEnvelope` uses `dlq_meta` and `usage_record`, matching go-contracts.
