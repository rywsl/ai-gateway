# Credit Lease 防透支硬保护 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有软防 (`frozen_micro`) 之上新增"积分租约"硬上界,使低余额用户在 protected 模式下任意时刻"已下发可消费授权 ≤ 实际余额 × (1 + overcommit_ratio)",默认 `overcommit_ratio = 0` 即永不透支。

**Architecture:** Rust 持有无量纲 `credits` (i64) 池,Go 是余额/价格/汇率/阈值的唯一持有者。Go 通过 `events:counter` 推 (a) 模式切换 + lease 续约 `ns=protected_lease`,(b) 价格变更时的系数表 `ns=coef_table` + commit。Rust 在 `request_filter` 第 8.5 步(hard_cap 之前)分流到 lease 路径,LeaseGuard CAS 扣减,流结束补还差额并通过 `usage:records.credits_consumed + pod_id + coef_table_version` 把消费告知 Go。Go 据此减 outstanding ledger。TTL 由 Go 在协议负载内携带 `expires_at_ms`,Rust 仅做 fail-closed 判定。

**Tech Stack:** Rust 1.88.0 / Pingora 0.8 / `fred` (Redis) / `dashmap` / `arc_swap` / `serde` / `tokio` / `tracing`。

**Spec:** [`docs/superpowers/specs/2026-05-29-credit-lease-design.md`](../specs/2026-05-29-credit-lease-design.md)
**Go 契约:** [`docs/go-contracts/contract-change/2026-05-29-credit-lease.md`](../../go-contracts/contract-change/2026-05-29-credit-lease.md)

## 关键现状(写在前面,implementer 必读)

1. **`events:counter` stream 目前是 no-op**: `crates/gw-store/src/stream_consumer.rs:54-57` 显式注释 `counter events are observability-only; no-op`。本计划需要在 Rust 这边**从零搭** counter 事件的 ns 分发框架(Task 6 / 7)。
2. **`request_filter` 已有 step 1-10**: `crates/gw-proxy/src/filters/request.rs` 末段(约 line 1047 是 daily quota step 7a,line 1141 是 hard cap step 9,line 1158 是 RequestFreeze step 10)。新的 lease 决策点要插在 step 9 之前,且必须与 RequestFreeze 路径**互斥**:protected 模式下走 LeaseGuard,**不**再走 RequestFreeze。
3. **`ReqCtx` 在 `crates/gw-proxy/src/ctx.rs:76`**, `freeze_guard: Option<RequestFreeze>` 在 line 143。新增字段加在附近,初始化在 line 363 附近的构造函数中。
4. **`FreezeGuard` trait 已存在** (`crates/gw-ledger/src/freeze.rs`),`LeaseGuard` 按相同 RAII pattern 实现,但不需要 trait,因为 `LeaseGuard` 和 `RequestFreeze` 是互斥的两条路径。
5. **`UsageRecord` 在 `crates/gw-ledger/src/usage.rs:43`**,新增字段全部 additive omitempty,放在末尾。
6. **错误码定义在 `crates/gw-core/src/error/codes.rs`**,现有约 9-12 行/44 行/97 行/112 行,新增加在已有 quota 错误码附近(line 12 之后)。

## 文件结构

### 新建文件

| 文件 | 责任 |
|---|---|
| `crates/gw-ledger/src/lease.rs` | `LeasePool` (DashMap), `LeaseState`, `LeaseGuard` (RAII), `LeaseError` |
| `crates/gw-ledger/src/coef.rs` | `ModelCoefCache` (ArcSwap), `CoefSnapshot`, `ModelCoefs`, `StagingState`, RCU 版本切换 |
| `crates/gw-ledger/src/credit_estimator.rs` | `estimate_request_credits`, `actual_usage_credits` |
| `crates/gw-ledger/src/protection_config.rs` | `[protection]` 配置 struct (反序列化目标) |
| `crates/gw-store/src/counter_dispatch.rs` | counter stream ns 分发 trait + 实现 |
| `tests/integration/credit_lease/mod.rs` | 集成测试入口 |
| `tests/integration/credit_lease/lease_pool_test.rs` | LeasePool CAS / TTL 单元覆盖 |
| `tests/integration/credit_lease/coef_cache_test.rs` | RCU 版本切换 / 不完整提交 |
| `tests/integration/credit_lease/end_to_end_test.rs` | request_filter 集成 (mock Go push) |

### 修改文件

| 文件 | 改动 |
|---|---|
| `crates/gw-ledger/src/lib.rs` | re-export 新模块 |
| `crates/gw-ledger/src/usage.rs:43-101` | `UsageRecord` 加 `pod_id` (必填) / `credits_consumed` (Option) / `coef_table_version` (Option) |
| `crates/gw-core/src/error/codes.rs` | 加 `GW_QUOTA_LEASE_INSUFFICIENT` (402) / `GW_QUOTA_LEASE_EXPIRED` (503) |
| `crates/gw-core/src/error/classify_default.rs` | 把两个新码映射到对应 HTTP 状态 |
| `crates/gw-proxy/src/ctx.rs:76, 143, 363` | `ReqCtx` 加 `lease_guard: Option<LeaseGuard>`、`credits_consumed: Option<i64>`、`coef_table_version_at_settle: Option<u64>` |
| `crates/gw-proxy/src/filters/request.rs:1138 区` | step 8.5 新增 lease 决策分支;protected → 走 lease,不再走 step 10 RequestFreeze |
| `crates/gw-proxy/src/filters/response.rs` 或 `logging.rs` | 在 final usage 拿到后 `lease_guard.settle_actual(actual)` |
| `crates/gw-proxy/src/filters/logging.rs:110` | `lease_guard.release()` 与 `freeze_guard.release()` 并列(二选一执行) |
| `crates/gw-proxy/src/billing_lifecycle.rs` | 注入 `credits_consumed` / `coef_table_version` 到 `UsageRecord` |
| `crates/gw-store/src/stream_consumer.rs:54-57` | counter stream 不再 no-op;改为分发到 `counter_dispatch` |
| `config/default.toml` 末尾 | 新增 `[protection]` 表 |
| `crates/gw-gateway/src/metrics_catalog.rs` | 登记新 metrics |
| `crates/gw-gateway/src/health.rs` | 新增 `/healthz/credit/lease/<uid>` + `/healthz/credit/coef-table` |
| `scripts/check-invariants.sh` | 新增 `no_currency_in_lease` lint |
| `docs/architecture.md` | 在 §Workspace layout / §Configuration / §Invariants 索引新模块 |
| `docs/workflow.md` | request_filter 章节加 step 8.5 |

---

## Task 1: 新增错误码 (基础)

**Files:**
- Modify: `crates/gw-core/src/error/codes.rs`
- Modify: `crates/gw-core/src/error/classify_default.rs`

- [ ] **Step 1: 在 codes.rs 添加常量**

打开 `crates/gw-core/src/error/codes.rs`,在 line 12 (`GW_QUOTA_CONCURRENT_EXCEEDED` 之后)插入:

```rust
/// Protected-mode lease credits insufficient to cover the request's estimated cost.
/// HTTP 402. Distinct from `GW_QUOTA_BALANCE_ZERO`: user may have non-zero balance
/// but this Pod's lease share is exhausted; client may retry after next Go push.
pub const GW_QUOTA_LEASE_INSUFFICIENT: &str = "GW_QUOTA_LEASE_INSUFFICIENT";

/// Protected-mode lease has expired (now > expires_at_ms + lease_grace_ms) and
/// Go has not pushed a renewal. HTTP 503 — this is an ops signal, not a user
/// error. Triggers `gw_credit_lease_expired_total` for paging.
pub const GW_QUOTA_LEASE_EXPIRED: &str = "GW_QUOTA_LEASE_EXPIRED";
```

- [ ] **Step 2: 在 classify_default.rs 映射 HTTP 状态**

打开 `crates/gw-core/src/error/classify_default.rs`,找到现有 `GW_QUOTA_BALANCE_ZERO => 402` 这一行(grep `GW_QUOTA_BALANCE_ZERO` 定位),在同一 `match` 内追加:

```rust
GW_QUOTA_LEASE_INSUFFICIENT => 402,
GW_QUOTA_LEASE_EXPIRED => 503,
```

实际行号需 implementer 在 grep 后确认。如果该文件用枚举 / 别的结构,按同样方式追加到现有 quota 错误的分支后。

- [ ] **Step 3: 编译验证**

```bash
cargo build -p gw-core
```
Expected: 编译通过,无警告。

- [ ] **Step 4: 提交**

```bash
git add crates/gw-core/src/error/codes.rs crates/gw-core/src/error/classify_default.rs
git commit -m "feat(gw-core): add GW_QUOTA_LEASE_INSUFFICIENT / GW_QUOTA_LEASE_EXPIRED error codes"
```

---

## Task 2: 配置结构 + default.toml

**Files:**
- Create: `crates/gw-ledger/src/protection_config.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Modify: `config/default.toml`

- [ ] **Step 1: 写失败测试**

创建 `crates/gw-ledger/src/protection_config.rs` 并加测试:

```rust
//! `[protection]` config table. Rust-side only — TTL/threshold/overcommit
//! live in Go, not here, per spec §9.

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProtectionConfig {
    /// Kill switch. When false, Rust does not subscribe to `protected_lease`
    /// ns and all requests run the legacy `frozen_micro` soft-defense path.
    pub enabled: bool,
    /// Clock-skew tolerance window (ms) when checking `now > expires_at`.
    pub lease_grace_ms: u64,
    /// Fallback `max_tokens` ceiling for credit estimation when the request
    /// has no explicit `max_tokens` and the model's entity config also lacks
    /// a default. Increments `gw_credit_default_ceiling_used_total`.
    pub default_output_ceiling_tokens: u32,
}

impl Default for ProtectionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lease_grace_ms: 5_000,
            default_output_ceiling_tokens: 8192,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disables_protection() {
        let cfg = ProtectionConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.lease_grace_ms, 5_000);
        assert_eq!(cfg.default_output_ceiling_tokens, 8192);
    }

    #[test]
    fn deserialize_partial_overrides_keep_defaults() {
        // figment merges atomically; we only test the serde shape here.
        let toml_text = r#"
            enabled = true
        "#;
        let cfg: ProtectionConfig = toml::from_str(toml_text).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.lease_grace_ms, 5_000); // default preserved
        assert_eq!(cfg.default_output_ceiling_tokens, 8192);
    }
}
```

- [ ] **Step 2: 在 lib.rs 暴露**

打开 `crates/gw-ledger/src/lib.rs`,在已有 `pub mod` 列表合适位置加:

```rust
pub mod protection_config;
pub use protection_config::ProtectionConfig;
```

- [ ] **Step 3: 运行测试**

```bash
cargo test -p gw-ledger protection_config -- --nocapture
```
Expected: 两个测试 PASS。

- [ ] **Step 4: 在 default.toml 追加 [protection]**

打开 `config/default.toml`,在文件末尾追加:

```toml

[protection]
# Hard overdraft protection — credit lease layer above the legacy soft defense.
# See docs/superpowers/specs/2026-05-29-credit-lease-design.md.
# Kill switch. When false, Rust ignores `events:counter ns=protected_lease`
# events and all requests use the legacy `frozen_micro` soft-defense path.
# Go-side lease TTL, low-balance threshold, and overcommit ratio are NOT in
# this table — they live in Go config, by design (spec §9).
enabled = false
# Clock-skew tolerance window (ms) when checking lease expiry.
lease_grace_ms = 5000
# Fallback `max_tokens` ceiling for credit estimation when the request has no
# explicit `max_tokens` and the model's entity config also lacks a default.
default_output_ceiling_tokens = 8192
```

- [ ] **Step 5: 编译验证**

```bash
cargo build -p gw-ledger
cargo test -p gw-ledger protection_config
```
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/gw-ledger/src/protection_config.rs crates/gw-ledger/src/lib.rs config/default.toml
git commit -m "feat(gw-ledger): add ProtectionConfig struct + [protection] table in default.toml"
```

---

## Task 3: `ModelCoefCache` 系数表缓存(RCU + staging)

**Files:**
- Create: `crates/gw-ledger/src/coef.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: 写失败测试(基础 lookup)**

创建 `crates/gw-ledger/src/coef.rs`:

```rust
//! ModelCoefCache — RCU-style versioned cache mapping model → per-dim
//! credit coefficient. Updated via Go push on `events:counter ns=coef_table`
//! + `ns=coef_table_commit`. See spec §5.1.2, §5.1.3, §6.2.
//!
//! All values are unitless `credits / token`. Rust does NOT translate to
//! USD micro or any currency — that is a strict boundary (spec §2).

use arc_swap::ArcSwap;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelCoefs {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub thinking: u64,
    pub multimodal_input: u64,
}

#[derive(Clone, Debug)]
pub struct CoefSnapshot {
    pub version: u64,
    pub by_model: HashMap<String, ModelCoefs>,
}

impl CoefSnapshot {
    pub fn empty() -> Self {
        Self {
            version: 0,
            by_model: HashMap::new(),
        }
    }

    pub fn get(&self, model_id: &str) -> Option<&ModelCoefs> {
        self.by_model.get(model_id)
    }
}

/// Staging area for an in-progress `coef_table_version` push. Entries
/// accumulate as `coef_table` events arrive; `coef_table_commit` swaps the
/// staged snapshot into `COEF_CACHE` once `model_count` is satisfied.
#[derive(Debug, Default)]
pub struct StagingState {
    pub entries: HashMap<String, ModelCoefs>,
}

pub static COEF_CACHE: Lazy<ArcSwap<CoefSnapshot>> =
    Lazy::new(|| ArcSwap::new(Arc::new(CoefSnapshot::empty())));

pub static COEF_STAGING: Lazy<DashMap<u64, StagingState>> = Lazy::new(DashMap::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_has_version_zero() {
        let snap = CoefSnapshot::empty();
        assert_eq!(snap.version, 0);
        assert!(snap.by_model.is_empty());
        assert!(snap.get("gpt-4").is_none());
    }
}
```

- [ ] **Step 2: 运行测试**

```bash
cargo test -p gw-ledger coef::tests::empty_snapshot
```
Expected: PASS。

- [ ] **Step 3: 写失败测试(staging + commit + RCU 切换)**

在 `coef.rs` 末尾的 `#[cfg(test)] mod tests` 内追加:

```rust
    #[test]
    fn apply_entry_then_commit_swaps_snapshot() {
        // Fresh-state test: clear caches between assertions.
        COEF_CACHE.store(Arc::new(CoefSnapshot::empty()));
        COEF_STAGING.clear();

        apply_coef_entry(7, "model-a", Dim::Input, 100);
        apply_coef_entry(7, "model-a", Dim::Output, 300);
        apply_coef_entry(7, "model-b", Dim::Input, 50);
        // Before commit: cache unchanged.
        assert_eq!(COEF_CACHE.load().version, 0);

        let outcome = apply_coef_commit(7, 2);
        assert_eq!(outcome, CommitOutcome::Activated);
        assert_eq!(COEF_CACHE.load().version, 7);
        assert_eq!(
            COEF_CACHE.load().get("model-a"),
            Some(&ModelCoefs {
                input: 100,
                output: 300,
                ..Default::default()
            })
        );
    }

    #[test]
    fn commit_with_incomplete_staging_records_metric_keeps_old() {
        COEF_CACHE.store(Arc::new(CoefSnapshot::empty()));
        COEF_STAGING.clear();

        apply_coef_entry(9, "model-a", Dim::Input, 100);
        // model_count = 2 but only 1 model staged.
        let outcome = apply_coef_commit(9, 2);
        assert_eq!(outcome, CommitOutcome::Incomplete { staged: 1, expected: 2 });
        assert_eq!(COEF_CACHE.load().version, 0); // unchanged
    }

    #[test]
    fn stale_commit_for_older_version_is_dropped() {
        COEF_CACHE.store(Arc::new(CoefSnapshot {
            version: 10,
            by_model: HashMap::new(),
        }));
        COEF_STAGING.clear();

        apply_coef_entry(5, "model-a", Dim::Input, 100);
        let outcome = apply_coef_commit(5, 1);
        assert_eq!(outcome, CommitOutcome::Stale { committed: 5, active: 10 });
        assert_eq!(COEF_CACHE.load().version, 10);
    }
```

- [ ] **Step 4: 运行测试以看到失败**

```bash
cargo test -p gw-ledger coef::tests
```
Expected: 三个新测试 FAIL("`apply_coef_entry` not found" / "`Dim` not found")。

- [ ] **Step 5: 实现 `Dim` / `apply_coef_entry` / `apply_coef_commit` / `CommitOutcome`**

在 `coef.rs` 中(`#[cfg(test)]` 之前)添加:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dim {
    Input,
    Output,
    CacheRead,
    CacheCreation,
    Thinking,
    MultimodalInput,
}

impl Dim {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "input" => Some(Dim::Input),
            "output" => Some(Dim::Output),
            "cache_read" => Some(Dim::CacheRead),
            "cache_creation" => Some(Dim::CacheCreation),
            "thinking" => Some(Dim::Thinking),
            "multimodal_input" => Some(Dim::MultimodalInput),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    Activated,
    Incomplete { staged: u32, expected: u32 },
    Stale { committed: u64, active: u64 },
}

/// Apply a single coef entry to the staging area for `version`.
/// Idempotent: re-applying overwrites the same field.
pub fn apply_coef_entry(version: u64, model_id: &str, dim: Dim, coef: u64) {
    let mut staging = COEF_STAGING.entry(version).or_default();
    let coefs = staging.entries.entry(model_id.to_string()).or_default();
    match dim {
        Dim::Input => coefs.input = coef,
        Dim::Output => coefs.output = coef,
        Dim::CacheRead => coefs.cache_read = coef,
        Dim::CacheCreation => coefs.cache_creation = coef,
        Dim::Thinking => coefs.thinking = coef,
        Dim::MultimodalInput => coefs.multimodal_input = coef,
    }
}

/// Commit the staged version. Activates if staged model count ≥ expected and
/// version > currently active. Returns the outcome for caller metrics/logs.
pub fn apply_coef_commit(version: u64, model_count: u32) -> CommitOutcome {
    let active_version = COEF_CACHE.load().version;
    if version <= active_version {
        // Stale commit (e.g., we already moved on). Drop staging entry.
        COEF_STAGING.remove(&version);
        return CommitOutcome::Stale {
            committed: version,
            active: active_version,
        };
    }

    let staging = match COEF_STAGING.remove(&version) {
        Some((_, s)) => s,
        None => StagingState::default(),
    };
    let staged_count = staging.entries.len() as u32;
    if staged_count < model_count {
        // Put back so a later commit with the right count can succeed.
        COEF_STAGING.insert(version, staging);
        return CommitOutcome::Incomplete {
            staged: staged_count,
            expected: model_count,
        };
    }

    let new_snapshot = CoefSnapshot {
        version,
        by_model: staging.entries,
    };
    COEF_CACHE.store(Arc::new(new_snapshot));
    CommitOutcome::Activated
}

/// Lookup the active snapshot. Returns the `Arc` directly so callers can hold
/// a stable reference even across an RCU swap.
pub fn current_snapshot() -> Arc<CoefSnapshot> {
    COEF_CACHE.load_full()
}

/// Lookup a specific model in a specific historical version (used by
/// `LeaseGuard.settle_actual` which must use the lease's coef_version).
/// Returns `None` if that version is neither active nor in staging.
pub fn lookup(version: u64, model_id: &str) -> Option<ModelCoefs> {
    let active = COEF_CACHE.load();
    if active.version == version {
        return active.by_model.get(model_id).copied();
    }
    COEF_STAGING
        .get(&version)
        .and_then(|s| s.entries.get(model_id).copied())
}
```

- [ ] **Step 6: 在 lib.rs re-export**

`crates/gw-ledger/src/lib.rs`:

```rust
pub mod coef;
pub use coef::{
    apply_coef_commit, apply_coef_entry, current_snapshot as current_coef_snapshot,
    lookup as lookup_coef, CommitOutcome, CoefSnapshot, Dim, ModelCoefs,
};
```

- [ ] **Step 7: 跑全部 coef 测试**

```bash
cargo test -p gw-ledger coef
```
Expected: 4 个测试 PASS。

- [ ] **Step 8: 提交**

```bash
git add crates/gw-ledger/src/coef.rs crates/gw-ledger/src/lib.rs
git commit -m "feat(gw-ledger): ModelCoefCache RCU + staging for protected-mode credit conversion"
```

---

## Task 4: `LeasePool` + `LeaseGuard`(核心 RAII)

**Files:**
- Create: `crates/gw-ledger/src/lease.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: 骨架 + 失败测试**

创建 `crates/gw-ledger/src/lease.rs`:

```rust
//! LeasePool — per-user credit lease state for protected mode.
//! Spec §6.1.
//!
//! `LeaseGuard` is the RAII handle (mirror of `RequestFreeze`): CAS-decrements
//! `LeaseState.credits` on acquire, refunds on settle if actual ≤ reserved,
//! and is idempotent on release/drop.

use arc_swap::ArcSwap;
use dashmap::DashMap;
use gw_core::ids::UserId;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    Unprotected = 0,
    Protected = 1,
}

impl Mode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Mode::Unprotected),
            1 => Some(Mode::Protected),
            _ => None,
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "unprotected" => Some(Mode::Unprotected),
            "protected" => Some(Mode::Protected),
            _ => None,
        }
    }
}

pub struct LeaseState {
    pub credits: AtomicI64,
    pub expires_at_ms: AtomicU64,
    pub lease_id: ArcSwap<String>,
    pub mode: AtomicU8,
    pub coef_table_version: AtomicU64,
}

impl LeaseState {
    fn new_unprotected() -> Self {
        Self {
            credits: AtomicI64::new(0),
            expires_at_ms: AtomicU64::new(0),
            lease_id: ArcSwap::new(Arc::new(String::new())),
            mode: AtomicU8::new(Mode::Unprotected as u8),
            coef_table_version: AtomicU64::new(0),
        }
    }

    pub fn mode(&self) -> Mode {
        Mode::from_u8(self.mode.load(Ordering::Acquire)).unwrap_or(Mode::Unprotected)
    }

    pub fn credits(&self) -> i64 {
        self.credits.load(Ordering::Acquire)
    }
}

pub static LEASE_POOL: Lazy<DashMap<UserId, Arc<LeaseState>>> = Lazy::new(DashMap::new);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    Insufficient { requested: i64, available: i64 },
    Expired,
    NoLease,
}

pub struct LeaseGuard {
    uid: UserId,
    reserved: i64,
    coef_version: u64,
    released: AtomicBool,
    state: Arc<LeaseState>,
}

impl LeaseGuard {
    pub fn coef_version(&self) -> u64 {
        self.coef_version
    }
    pub fn reserved(&self) -> i64 {
        self.reserved
    }
    pub fn uid(&self) -> &UserId {
        &self.uid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_uid(tag: &str) -> UserId {
        UserId::from(format!("u_test_{tag}").as_str())
    }

    #[test]
    fn try_acquire_decrements_credits_atomically() {
        let uid = fresh_uid("acquire_ok");
        upsert_protected(&uid, 1000, 1, 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 300, now_ms()).unwrap();
        assert_eq!(guard.reserved(), 300);
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 700);
        drop(guard);
    }

    #[test]
    fn try_acquire_insufficient_returns_error() {
        let uid = fresh_uid("acquire_insufficient");
        upsert_protected(&uid, 100, 2, 9, now_ms() + 60_000);

        let err = LeaseGuard::try_acquire(&uid, 200, now_ms()).unwrap_err();
        assert!(matches!(err, LeaseError::Insufficient { .. }));
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 100);
    }

    #[test]
    fn try_acquire_expired_returns_error() {
        let uid = fresh_uid("acquire_expired");
        let past = now_ms().saturating_sub(10_000);
        upsert_protected(&uid, 1000, 3, 9, past);

        let err = LeaseGuard::try_acquire(&uid, 100, now_ms()).unwrap_err();
        assert!(matches!(err, LeaseError::Expired));
    }

    #[test]
    fn try_acquire_no_lease_returns_error() {
        let uid = fresh_uid("no_lease");
        let err = LeaseGuard::try_acquire(&uid, 100, now_ms()).unwrap_err();
        assert!(matches!(err, LeaseError::NoLease));
    }

    #[test]
    fn settle_actual_le_reserved_refunds_difference() {
        let uid = fresh_uid("settle_under");
        upsert_protected(&uid, 1000, 4, 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 500, now_ms()).unwrap();
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 500);
        guard.settle_actual(200);
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 800);
    }

    #[test]
    fn settle_actual_gt_reserved_does_not_refund_or_double_charge() {
        let uid = fresh_uid("settle_over");
        upsert_protected(&uid, 1000, 5, 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 300, now_ms()).unwrap();
        guard.settle_actual(500);
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 700);
    }

    #[test]
    fn release_is_idempotent() {
        let uid = fresh_uid("release_idem");
        upsert_protected(&uid, 1000, 6, 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 100, now_ms()).unwrap();
        guard.release();
        guard.release();
        guard.release();
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 900);
    }

    #[test]
    fn upsert_overwrites_existing_state_for_same_user() {
        let uid = fresh_uid("upsert_overwrite");
        upsert_protected(&uid, 1000, 7, 9, now_ms() + 60_000);
        upsert_protected(&uid, 500, 8, 9, now_ms() + 120_000);
        let state = LEASE_POOL.get(&uid).unwrap();
        assert_eq!(state.credits(), 500);
        assert_eq!(state.mode(), Mode::Protected);
        assert_eq!(**state.lease_id.load(), "lease-8");
    }

    #[test]
    fn upsert_unprotected_clears_state() {
        let uid = fresh_uid("upsert_unprotected");
        upsert_protected(&uid, 1000, 9, 9, now_ms() + 60_000);
        upsert_unprotected(&uid);
        let state = LEASE_POOL.get(&uid).unwrap();
        assert_eq!(state.mode(), Mode::Unprotected);
        assert_eq!(state.credits(), 0);
    }
}
```

- [ ] **Step 2: 跑测试看到失败**

```bash
cargo test -p gw-ledger lease::tests -- --nocapture
```
Expected: 编译失败,缺 `upsert_protected` / `upsert_unprotected` / `now_ms` / `LeaseGuard::try_acquire` / `settle_actual` / `release`。

- [ ] **Step 3: 实现 helpers + acquire/settle/release**

在 `lease.rs` 中(`#[cfg(test)]` 之前)添加:

```rust
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock pre-epoch")
        .as_millis() as u64
}

/// Upsert a user into protected mode with the given lease. Helper used by
/// the stream consumer and by tests.
pub fn upsert_protected(
    uid: &UserId,
    credits: i64,
    lease_id_suffix: u64,
    coef_table_version: u64,
    expires_at_ms: u64,
) {
    let lease_id_str = format!("lease-{lease_id_suffix}");
    let entry = LEASE_POOL.entry(uid.clone()).or_insert_with(|| {
        Arc::new(LeaseState::new_unprotected())
    });
    entry.credits.store(credits, Ordering::Release);
    entry.expires_at_ms.store(expires_at_ms, Ordering::Release);
    entry.lease_id.store(Arc::new(lease_id_str));
    entry
        .coef_table_version
        .store(coef_table_version, Ordering::Release);
    entry
        .mode
        .store(Mode::Protected as u8, Ordering::Release);
}

/// Flip the user back to unprotected mode. Wipes lease credits.
pub fn upsert_unprotected(uid: &UserId) {
    let entry = LEASE_POOL.entry(uid.clone()).or_insert_with(|| {
        Arc::new(LeaseState::new_unprotected())
    });
    entry.credits.store(0, Ordering::Release);
    entry.expires_at_ms.store(0, Ordering::Release);
    entry.lease_id.store(Arc::new(String::new()));
    entry
        .mode
        .store(Mode::Unprotected as u8, Ordering::Release);
}

impl LeaseGuard {
    /// Acquire `requested` credits from the user's lease. Returns
    /// `LeaseError::NoLease` if the user has no lease entry,
    /// `LeaseError::Expired` if the lease TTL has passed, or
    /// `LeaseError::Insufficient` if the available pool < requested.
    pub fn try_acquire(uid: &UserId, requested: i64, now_ms_val: u64) -> Result<Self, LeaseError> {
        let state = match LEASE_POOL.get(uid) {
            Some(s) => Arc::clone(&*s),
            None => return Err(LeaseError::NoLease),
        };
        if state.mode() != Mode::Protected {
            return Err(LeaseError::NoLease);
        }
        let expires_at = state.expires_at_ms.load(Ordering::Acquire);
        if now_ms_val > expires_at {
            return Err(LeaseError::Expired);
        }
        // CAS loop: ensure credits stay >= 0 after subtraction.
        loop {
            let current = state.credits.load(Ordering::Acquire);
            if current < requested {
                return Err(LeaseError::Insufficient {
                    requested,
                    available: current,
                });
            }
            let new = current - requested;
            if state
                .credits
                .compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let coef_version = state.coef_table_version.load(Ordering::Acquire);
                return Ok(LeaseGuard {
                    uid: uid.clone(),
                    reserved: requested,
                    coef_version,
                    released: AtomicBool::new(false),
                    state,
                });
            }
        }
    }

    /// Settle the request with actual credits. If actual ≤ reserved, refunds
    /// the difference. If actual > reserved, does NOT double-charge — the
    /// over-spend escapes via `usage:records.credits_consumed` (Go reconciles).
    pub fn settle_actual(&self, actual: i64) {
        let actual = actual.max(0);
        if actual >= self.reserved {
            // Nothing to refund; over-spend tracked via metric in caller.
            return;
        }
        let refund = self.reserved - actual;
        self.state.credits.fetch_add(refund, Ordering::AcqRel);
    }

    /// Idempotent release. Currently a no-op marker; `settle_actual` performs
    /// the actual refund. Drop also calls release for safety.
    pub fn release(&self) {
        let _ = self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}
```

- [ ] **Step 4: 在 lib.rs re-export**

```rust
pub mod lease;
pub use lease::{
    now_ms, upsert_protected, upsert_unprotected, LeaseError, LeaseGuard, LeasePool, LeaseState,
    Mode, LEASE_POOL,
};
```

(`LeasePool` 这里如果还没定义就用 `LEASE_POOL` 直接 re-export 全局静态;若觉得不雅,可以省略 `LeasePool` type alias。)

- [ ] **Step 5: 跑测试**

```bash
cargo test -p gw-ledger lease
```
Expected: 8 测试 PASS。

- [ ] **Step 6: 提交**

```bash
git add crates/gw-ledger/src/lease.rs crates/gw-ledger/src/lib.rs
git commit -m "feat(gw-ledger): LeasePool + LeaseGuard RAII for protected-mode hard limit"
```

---

## Task 5: `credit_estimator` 模块

**Files:**
- Create: `crates/gw-ledger/src/credit_estimator.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: 写失败测试**

创建 `crates/gw-ledger/src/credit_estimator.rs`:

```rust
//! Credit estimation for protected mode. Uses the per-lease `coef_version`
//! to look up `ModelCoefs` and convert tokens → credits.
//!
//! - `estimate_request_credits` runs at acquire time (2 dims: input + output)
//! - `actual_usage_credits` runs at settle time (6 dims)
//!
//! Both must use the SAME `coef_version` for one request, otherwise outstanding
//! ledger on the Go side will drift. Spec §6.3.

use crate::coef::{lookup as lookup_coef, ModelCoefs};
use gw_core::usage::UsageSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimatorError {
    UnknownModel,
    UnknownCoefVersion,
}

pub fn estimate_request_credits(
    coef_version: u64,
    model_id: &str,
    est_input_tokens: u32,
    max_output_tokens: u32,
) -> Result<i64, EstimatorError> {
    let coefs = lookup_coef(coef_version, model_id).ok_or_else(|| {
        if coef_version == 0 {
            EstimatorError::UnknownCoefVersion
        } else {
            EstimatorError::UnknownModel
        }
    })?;
    let input = (est_input_tokens as i64).saturating_mul(coefs.input as i64);
    let output = (max_output_tokens as i64).saturating_mul(coefs.output as i64);
    Ok(input.saturating_add(output))
}

pub fn actual_usage_credits(
    coef_version: u64,
    model_id: &str,
    usage: &UsageSnapshot,
) -> Result<i64, EstimatorError> {
    let c = lookup_coef(coef_version, model_id).ok_or(EstimatorError::UnknownModel)?;
    let mut total: i64 = 0;
    total = total.saturating_add((usage.input as i64).saturating_mul(c.input as i64));
    total = total.saturating_add((usage.output as i64).saturating_mul(c.output as i64));
    total = total.saturating_add((usage.cache_read as i64).saturating_mul(c.cache_read as i64));
    total = total.saturating_add((usage.cache_creation as i64).saturating_mul(c.cache_creation as i64));
    total = total.saturating_add((usage.thinking as i64).saturating_mul(c.thinking as i64));
    total = total.saturating_add(
        (usage.multimodal_input as i64).saturating_mul(c.multimodal_input as i64),
    );
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coef::{apply_coef_commit, apply_coef_entry, Dim, COEF_CACHE, COEF_STAGING};
    use std::sync::Arc;

    fn install_test_coefs() {
        COEF_CACHE.store(Arc::new(crate::coef::CoefSnapshot::empty()));
        COEF_STAGING.clear();
        apply_coef_entry(42, "model-x", Dim::Input, 100);
        apply_coef_entry(42, "model-x", Dim::Output, 200);
        apply_coef_entry(42, "model-x", Dim::CacheRead, 50);
        apply_coef_commit(42, 1);
    }

    #[test]
    fn estimate_input_and_max_output() {
        install_test_coefs();
        let c = estimate_request_credits(42, "model-x", 10, 20).unwrap();
        assert_eq!(c, 10 * 100 + 20 * 200);
    }

    #[test]
    fn estimate_unknown_model_errors() {
        install_test_coefs();
        let err = estimate_request_credits(42, "unknown", 10, 20).unwrap_err();
        assert_eq!(err, EstimatorError::UnknownModel);
    }

    #[test]
    fn actual_usage_six_dims() {
        install_test_coefs();
        let usage = UsageSnapshot {
            input: 5,
            output: 10,
            cache_read: 3,
            ..Default::default()
        };
        let c = actual_usage_credits(42, "model-x", &usage).unwrap();
        assert_eq!(c, 5 * 100 + 10 * 200 + 3 * 50);
    }

    #[test]
    fn version_zero_returns_unknown_coef_version() {
        install_test_coefs();
        let err = estimate_request_credits(0, "model-x", 1, 1).unwrap_err();
        assert_eq!(err, EstimatorError::UnknownCoefVersion);
    }
}
```

- [ ] **Step 2: 在 lib.rs re-export**

```rust
pub mod credit_estimator;
pub use credit_estimator::{actual_usage_credits, estimate_request_credits, EstimatorError};
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p gw-ledger credit_estimator
```
Expected: 4 测试 PASS。如果 `UsageSnapshot::default()` 不可用,检查 `gw_core::usage::UsageSnapshot` 是否有 `Default` 派生;若无,在测试里手工构造全 0 实例。

- [ ] **Step 4: 提交**

```bash
git add crates/gw-ledger/src/credit_estimator.rs crates/gw-ledger/src/lib.rs
git commit -m "feat(gw-ledger): credit_estimator with 2-dim reserve + 6-dim settle"
```

---

## Task 6: counter stream ns 分发框架

**Files:**
- Create: `crates/gw-store/src/counter_dispatch.rs`
- Modify: `crates/gw-store/src/stream_consumer.rs` (around line 54)
- Modify: `crates/gw-store/src/lib.rs`

- [ ] **Step 1: 写失败测试**

创建 `crates/gw-store/src/counter_dispatch.rs`:

```rust
//! Dispatch handler for `events:counter` stream events. Before this module
//! existed, counter events were a no-op (`stream_consumer.rs:54-57`).
//!
//! Each ns has its own handler. New handlers register here. The dispatcher
//! is intentionally simple — no async ownership; handlers are sync closures
//! over global state.

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    Handled,
    UnknownNs,
    InvalidFields,
}

pub fn dispatch(fields: &HashMap<String, String>) -> DispatchOutcome {
    let ns = match fields.get("ns").map(String::as_str) {
        Some(n) => n,
        None => return DispatchOutcome::InvalidFields,
    };
    match ns {
        "protected_lease" => handle_protected_lease(fields),
        "coef_table" => handle_coef_table(fields),
        "coef_table_commit" => handle_coef_table_commit(fields),
        _ => DispatchOutcome::UnknownNs,
    }
}

fn handle_protected_lease(_fields: &HashMap<String, String>) -> DispatchOutcome {
    // Implemented in Task 7.
    DispatchOutcome::Handled
}

fn handle_coef_table(_fields: &HashMap<String, String>) -> DispatchOutcome {
    // Implemented in Task 7.
    DispatchOutcome::Handled
}

fn handle_coef_table_commit(_fields: &HashMap<String, String>) -> DispatchOutcome {
    // Implemented in Task 7.
    DispatchOutcome::Handled
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn dispatch_unknown_ns_returns_unknown() {
        let f = fields(&[("ns", "what")]);
        assert_eq!(dispatch(&f), DispatchOutcome::UnknownNs);
    }

    #[test]
    fn dispatch_missing_ns_returns_invalid() {
        let f = fields(&[]);
        assert_eq!(dispatch(&f), DispatchOutcome::InvalidFields);
    }

    #[test]
    fn dispatch_protected_lease_returns_handled() {
        let f = fields(&[("ns", "protected_lease"), ("user_id", "u_1")]);
        assert_eq!(dispatch(&f), DispatchOutcome::Handled);
    }
}
```

- [ ] **Step 2: 在 lib.rs 暴露**

`crates/gw-store/src/lib.rs`:

```rust
pub mod counter_dispatch;
```

- [ ] **Step 3: 接入 stream_consumer.rs**

打开 `crates/gw-store/src/stream_consumer.rs`,定位 line 53-57(`if stream == COUNTER_STREAM`),把整段替换:

```rust
        if stream == COUNTER_STREAM {
            let outcome = crate::counter_dispatch::dispatch(fields);
            if outcome == crate::counter_dispatch::DispatchOutcome::UnknownNs {
                tracing::debug!(
                    target: "gw_store::stream_consumer",
                    ns = ?fields.get("ns"),
                    "events:counter: unknown ns; skipping"
                );
            }
            return;
        }
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p gw-store counter_dispatch
cargo build -p gw-store
```
Expected: 3 测试 PASS,build OK。

- [ ] **Step 5: 提交**

```bash
git add crates/gw-store/src/counter_dispatch.rs crates/gw-store/src/lib.rs crates/gw-store/src/stream_consumer.rs
git commit -m "feat(gw-store): replace events:counter no-op with ns dispatcher"
```

---

## Task 7: counter dispatch 实际处理 protected_lease / coef_table / commit

**Files:**
- Modify: `crates/gw-store/src/counter_dispatch.rs`
- Modify: `crates/gw-store/Cargo.toml`(若 gw-ledger 不在依赖里则加上)

- [ ] **Step 1: 检查依赖**

```bash
grep -A 1 '\[dependencies\]' crates/gw-store/Cargo.toml | head -30
grep 'gw-ledger' crates/gw-store/Cargo.toml
```

若 `gw-ledger` 不在 `crates/gw-store/Cargo.toml` 的 `[dependencies]` 中,加上:

```toml
gw-ledger = { path = "../gw-ledger" }
```

注:`gw-store` 已被 `gw-ledger` 依赖。如出现循环依赖(gw-ledger 也 deps gw-store),改用反向注册模式——在 `gw-ledger` 里暴露 register 函数,由 `gw-gateway` 启动时把 `gw-ledger` 的 handler 注入到 `gw-store::counter_dispatch`。这里假定 gw-store→gw-ledger 单向可行;若 cargo 报循环依赖,使用以下替代方案:

把 `counter_dispatch.rs` 改用 trait-object 注册:

```rust
use once_cell::sync::OnceCell;
pub trait CounterHandler: Send + Sync {
    fn handle(&self, ns: &str, fields: &std::collections::HashMap<String, String>) -> DispatchOutcome;
}
static HANDLER: OnceCell<Box<dyn CounterHandler>> = OnceCell::new();
pub fn register_handler(h: Box<dyn CounterHandler>) {
    let _ = HANDLER.set(h);
}
```

`gw-gateway` 启动时调用 `register_handler(Box::new(LedgerHandler))`,`LedgerHandler` 在 `gw-ledger` 里实现并调用 `lease::upsert_protected` / `coef::apply_coef_entry`。

- [ ] **Step 2: 写失败测试(扩展 counter_dispatch 测试)**

在 `crates/gw-store/src/counter_dispatch.rs` 的 `#[cfg(test)]` 内追加:

```rust
    #[test]
    fn protected_lease_event_upserts_lease_pool() {
        use gw_core::ids::UserId;
        use gw_ledger::{LEASE_POOL, Mode};

        let uid_str = "u_ct_lease_1";
        LEASE_POOL.remove(&UserId::from(uid_str));

        let f = fields(&[
            ("ns", "protected_lease"),
            ("user_id", uid_str),
            ("mode", "protected"),
            ("lease_credits", "5000"),
            ("lease_id", "lease-abc"),
            ("expires_at_ms", "9999999999999"),
            ("coef_table_version", "7"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(dispatch(&f), DispatchOutcome::Handled);
        let s = LEASE_POOL.get(&UserId::from(uid_str)).unwrap();
        assert_eq!(s.mode(), Mode::Protected);
        assert_eq!(s.credits(), 5000);
    }

    #[test]
    fn coef_table_event_stages_entry() {
        use gw_ledger::{apply_coef_commit, current_coef_snapshot, CommitOutcome};
        let f = fields(&[
            ("ns", "coef_table"),
            ("coef_table_version", "100"),
            ("model_id", "model-z"),
            ("dim", "input"),
            ("coef_per_token", "33"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(dispatch(&f), DispatchOutcome::Handled);

        // Until commit, snapshot version is unchanged (could be 0 or previous).
        let before_version = current_coef_snapshot().version;
        assert!(before_version < 100);

        let commit_f = fields(&[
            ("ns", "coef_table_commit"),
            ("coef_table_version", "100"),
            ("model_count", "1"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(dispatch(&commit_f), DispatchOutcome::Handled);
        assert_eq!(current_coef_snapshot().version, 100);
    }
```

- [ ] **Step 3: 实现 handlers**

替换 `counter_dispatch.rs` 中三个 `handle_*` 函数:

```rust
fn handle_protected_lease(fields: &HashMap<String, String>) -> DispatchOutcome {
    use gw_core::ids::UserId;
    use gw_ledger::Mode;

    let uid = match fields.get("user_id") {
        Some(s) => UserId::from(s.as_str()),
        None => return DispatchOutcome::InvalidFields,
    };
    let mode_str = fields.get("mode").map(String::as_str).unwrap_or("");
    let mode = match Mode::from_str(mode_str) {
        Some(m) => m,
        None => return DispatchOutcome::InvalidFields,
    };
    match mode {
        Mode::Unprotected => {
            gw_ledger::upsert_unprotected(&uid);
            DispatchOutcome::Handled
        }
        Mode::Protected => {
            let credits = fields
                .get("lease_credits")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let expires = fields
                .get("expires_at_ms")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let version = fields
                .get("coef_table_version")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            // lease_id_suffix uses a stable hash of the lease_id string for
            // debug correlation; the actual string is stored verbatim.
            let lease_id_str = fields
                .get("lease_id")
                .cloned()
                .unwrap_or_default();
            gw_ledger::lease::upsert_protected_with_id(
                &uid,
                credits,
                lease_id_str,
                version,
                expires,
            );
            DispatchOutcome::Handled
        }
    }
}

fn handle_coef_table(fields: &HashMap<String, String>) -> DispatchOutcome {
    use gw_ledger::Dim;
    let version = match fields.get("coef_table_version").and_then(|s| s.parse::<u64>().ok()) {
        Some(v) => v,
        None => return DispatchOutcome::InvalidFields,
    };
    let model_id = match fields.get("model_id") {
        Some(s) => s.as_str(),
        None => return DispatchOutcome::InvalidFields,
    };
    let dim = match fields.get("dim").and_then(|s| Dim::from_str(s)) {
        Some(d) => d,
        None => return DispatchOutcome::InvalidFields,
    };
    let coef = fields
        .get("coef_per_token")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    gw_ledger::apply_coef_entry(version, model_id, dim, coef);
    DispatchOutcome::Handled
}

fn handle_coef_table_commit(fields: &HashMap<String, String>) -> DispatchOutcome {
    let version = match fields.get("coef_table_version").and_then(|s| s.parse::<u64>().ok()) {
        Some(v) => v,
        None => return DispatchOutcome::InvalidFields,
    };
    let model_count = fields
        .get("model_count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let outcome = gw_ledger::apply_coef_commit(version, model_count);
    match outcome {
        gw_ledger::CommitOutcome::Incomplete { staged, expected } => {
            tracing::warn!(
                target: "gw_store::counter_dispatch",
                version,
                staged,
                expected,
                "coef_table commit: staged < expected; keeping previous version"
            );
        }
        gw_ledger::CommitOutcome::Stale { committed, active } => {
            tracing::debug!(
                target: "gw_store::counter_dispatch",
                committed,
                active,
                "coef_table commit: stale; dropping"
            );
        }
        gw_ledger::CommitOutcome::Activated => {}
    }
    DispatchOutcome::Handled
}
```

注意:测试假设 `gw_ledger::lease` 提供 `upsert_protected_with_id(uid, credits, lease_id_string, coef_version, expires_at_ms)`,而 Task 4 写的是 `upsert_protected(uid, credits, lease_id_suffix: u64, ...)`。**回到 `crates/gw-ledger/src/lease.rs` 把 `upsert_protected` 改为接受 `lease_id: String`**:

```rust
pub fn upsert_protected(
    uid: &UserId,
    credits: i64,
    lease_id: String,
    coef_table_version: u64,
    expires_at_ms: u64,
) {
    // ... existing body, replacing `format!("lease-{lease_id_suffix}")` with
    //     just `lease_id`.
}
```

并更新 `lease.rs` 的测试,调用 `upsert_protected(&uid, 1000, "lease-1".into(), 9, ...)`。`upsert_protected_with_id` 名称就用 `upsert_protected`,删除 dispatch handler 里的 `with_id` 后缀。

- [ ] **Step 4: 跑全部测试**

```bash
cargo test -p gw-ledger lease
cargo test -p gw-store counter_dispatch
```
Expected: 全部 PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/gw-store/src/counter_dispatch.rs crates/gw-ledger/src/lease.rs crates/gw-store/Cargo.toml
git commit -m "feat(gw-store,gw-ledger): wire counter_dispatch to LeasePool + ModelCoefCache"
```

---

## Task 8: `UsageRecord` 字段扩展

**Files:**
- Modify: `crates/gw-ledger/src/usage.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/billing_lifecycle.rs`

- [ ] **Step 1: 写失败测试(usage.rs JSON 形状)**

打开 `crates/gw-ledger/src/usage.rs`,定位 `#[cfg(test)] mod tests` 区(line 112 起)。在 `usage_record_json_shape` 测试旁追加:

```rust
    #[test]
    fn usage_record_serializes_pod_id_when_present() {
        let mut rec = make_minimal_record();
        rec.pod_id = "pod-xyz".into();
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["pod_id"], "pod-xyz");
    }

    #[test]
    fn credits_consumed_omitted_when_none() {
        let rec = make_minimal_record();
        let json = serde_json::to_value(&rec).unwrap();
        assert!(json.get("credits_consumed").is_none());
        assert!(json.get("coef_table_version").is_none());
    }

    #[test]
    fn credits_consumed_serialized_when_some() {
        let mut rec = make_minimal_record();
        rec.credits_consumed = Some(12345);
        rec.coef_table_version = Some(7);
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["credits_consumed"], 12345);
        assert_eq!(json["coef_table_version"], 7);
    }
```

并在该 `mod tests` 末尾加 helper(若已有同名,用现有 fixture 即可):

```rust
    fn make_minimal_record() -> UsageRecord {
        UsageRecord {
            ts_ms: 0,
            request_id: "r".into(),
            trace_id: "t".into(),
            user_id: gw_core::ids::UserId::from("u"),
            child_id: None,
            api_key_id: None,
            client_protocol: "openai".into(),
            upstream_protocol: "openai".into(),
            model: gw_core::ids::ModelName::from("m"),
            vendor_id: gw_core::ids::VendorId::from("v"),
            vendor_key_id: gw_core::ids::KeyId::from("vk"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: Default::default(),
            attempt_index: 0,
            vendor_exposure_state: VendorExposureState::Settled,
            usage_provenance: UsageProvenance::default(),
            latency_ms: 0,
            ttft_ms: None,
            is_stream: false,
            freeze_amount_micro: 0,
            estimate_pricing_version: 0,
            markup_chain_snapshot: Default::default(),
            operation_id: None,
            pod_id: String::new(),
            credits_consumed: None,
            coef_table_version: None,
        }
    }
```

- [ ] **Step 2: 在 `UsageRecord` 末尾添加字段**

在 `crates/gw-ledger/src/usage.rs` 的 `pub struct UsageRecord` 末尾(line 100 `operation_id` 之后)追加:

```rust

    // --- Credit lease (2026-05-29 design) ---
    /// Source Pod's `instance_id`. Allows Go to attribute outstanding credits.
    /// Always populated; empty string for code paths predating this spec.
    pub pod_id: String,

    /// Credits this request consumed against the per-Pod lease (protected mode
    /// only). When `None`, request went through the legacy `frozen_micro`
    /// soft-defense path and Go skips outstanding-ledger decrement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credits_consumed: Option<i64>,

    /// `coef_table_version` used by Rust to compute `credits_consumed`. Go must
    /// use this same version when reconciling outstanding ledger entries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coef_table_version: Option<u64>,
```

- [ ] **Step 3: 跑测试**

```bash
cargo test -p gw-ledger usage
```
Expected: 三个新测试 PASS,旧 `usage_record_json_shape` 仍 PASS(可能需要在旧 fixture 里补 `pod_id: ""` / `credits_consumed: None` / `coef_table_version: None` 字段)。

- [ ] **Step 4: ReqCtx 加字段**

打开 `crates/gw-proxy/src/ctx.rs`,在 `freeze_guard: Option<RequestFreeze>` 之后(line 143 附近)追加:

```rust
    /// Credit lease guard (protected mode). Mutually exclusive with `freeze_guard`:
    /// if `lease_guard.is_some()` then the request bypassed step-10 RequestFreeze.
    pub lease_guard: Option<gw_ledger::LeaseGuard>,

    /// Credits Rust observed consumed for this request, computed at settle.
    /// Injected into UsageRecord; None in unprotected mode.
    pub credits_consumed: Option<i64>,

    /// `coef_table_version` used to compute `credits_consumed`.
    pub coef_table_version_at_settle: Option<u64>,
```

在构造函数(line 363 附近)初始化:

```rust
            lease_guard: None,
            credits_consumed: None,
            coef_table_version_at_settle: None,
```

- [ ] **Step 5: 把 `ctx.credits_consumed` / `ctx.coef_table_version_at_settle` 注入 UsageRecord**

打开 `crates/gw-proxy/src/billing_lifecycle.rs`,找到构造 `UsageRecord` 的地方(grep `UsageRecord {`)。在构造体里追加(其他字段保持不变):

```rust
        pod_id: gw_ledger::instance_id::cached().unwrap_or_default(),
        credits_consumed: ctx.credits_consumed,
        coef_table_version: ctx.coef_table_version_at_settle,
```

注:`gw_ledger::instance_id::cached()` 是新增的辅助函数,从 `L1_CACHE: OnceLock<String>` 读;若尚未存在,在 `crates/gw-ledger/src/instance_id.rs` 末尾加:

```rust
/// Returns the cached instance_id without doing any I/O. None if `resolve`
/// has not been called yet (typically only true before gateway bootstrap).
pub fn cached() -> Option<String> {
    L1_CACHE.get().cloned()
}
```

- [ ] **Step 6: 跑测试**

```bash
cargo test -p gw-ledger
cargo test -p gw-proxy --lib billing_lifecycle
cargo build -p gw-proxy
```
Expected: PASS / build OK。

- [ ] **Step 7: 提交**

```bash
git add crates/gw-ledger/src/usage.rs crates/gw-ledger/src/instance_id.rs crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/billing_lifecycle.rs
git commit -m "feat(gw-ledger,gw-proxy): UsageRecord adds pod_id/credits_consumed/coef_table_version"
```

---

## Task 9: 接入 `request_filter` step 8.5(lease 决策点)

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`(在 step 9 之前插入新分支,约 line 1138-1140)

- [ ] **Step 1: 读现状,理解上下文变量**

```bash
sed -n '1135,1165p' crates/gw-proxy/src/filters/request.rs
```

确认手上有 `est_input_tokens` (u32) / `merged` (含 `effective_user_id`) / `ctx` (mut) / `bail!` 宏。

- [ ] **Step 2: 在 step 9 之前插入 step 8.5(lease 决策)**

打开 `crates/gw-proxy/src/filters/request.rs`,在 step 9 注释 `// 9. Hard caps` 之前(line 1138)插入:

```rust
    // ---------------------------------------------------------------------
    // 8.5. Credit Lease (protected mode). Spec 2026-05-29-credit-lease-design.
    //      If the user is in protected mode (Go pushed `mode=protected` via
    //      events:counter ns=protected_lease), enforce a HARD upper bound on
    //      the per-Pod credits pool instead of the legacy frozen_micro soft
    //      defense. Bypasses step 10 RequestFreeze; LeaseGuard stays on ctx.
    // ---------------------------------------------------------------------
    if protection_cfg.enabled {
        if let Some(state_ref) = gw_ledger::LEASE_POOL.get(&merged.effective_user_id) {
            if state_ref.mode() == gw_ledger::Mode::Protected {
                // Resolve max_output_tokens with 3-tier fallback.
                let max_out = ctx
                    .ir
                    .as_ref()
                    .and_then(|ir| ir.max_tokens)
                    .or_else(|| merged.model_default_max_tokens)
                    .unwrap_or_else(|| {
                        metrics::counter!(
                            "gw_credit_default_ceiling_used_total",
                            "model_id" => merged.model.0.clone()
                        )
                        .increment(1);
                        protection_cfg.default_output_ceiling_tokens
                    });

                let coef_version = state_ref.coef_table_version.load(std::sync::atomic::Ordering::Acquire);
                let credits = match gw_ledger::estimate_request_credits(
                    coef_version,
                    merged.model.0.as_str(),
                    est_input_tokens,
                    max_out,
                ) {
                    Ok(c) => c,
                    Err(_) => {
                        // No coefs yet: fail-closed.
                        let err = gw_core::error::ErrorIR::new(
                            gw_core::error::codes::GW_QUOTA_LEASE_INSUFFICIENT,
                            "credit coefficients unavailable",
                        );
                        bail!(session, ctx, &err, &request_id)
                    }
                };

                let now = gw_ledger::now_ms();
                let guard = match gw_ledger::LeaseGuard::try_acquire(
                    &merged.effective_user_id,
                    credits,
                    now + protection_cfg.lease_grace_ms,
                ) {
                    Ok(g) => g,
                    Err(gw_ledger::LeaseError::Expired) => {
                        metrics::counter!("gw_credit_lease_expired_total").increment(1);
                        let err = gw_core::error::ErrorIR::new(
                            gw_core::error::codes::GW_QUOTA_LEASE_EXPIRED,
                            "lease expired; ops should investigate Go push pipeline",
                        );
                        bail!(session, ctx, &err, &request_id)
                    }
                    Err(gw_ledger::LeaseError::Insufficient { .. }) => {
                        metrics::counter!(
                            "gw_credit_lease_acquire_total",
                            "result" => "insufficient"
                        )
                        .increment(1);
                        let err = gw_core::error::ErrorIR::new(
                            gw_core::error::codes::GW_QUOTA_LEASE_INSUFFICIENT,
                            "insufficient credits for this request",
                        );
                        bail!(session, ctx, &err, &request_id)
                    }
                    Err(gw_ledger::LeaseError::NoLease) => {
                        // Should not happen since we already checked mode above,
                        // but warm-up race possible. Fall through to soft path.
                        metrics::counter!("gw_credit_lease_warmup_pending_total").increment(1);
                        // continue to step 9/10
                        ctx.lease_guard = None;
                        ctx.merged_perm = Some(merged);
                        ctx.phase_end();
                        // skip the early return below; allow step 9/10 to run by
                        // breaking out of the if-let with no lease set.
                        // (Rust control flow: we cannot `continue` here, so just
                        //  fall through using a labeled break.)
                        return Ok(false);
                    }
                };

                metrics::counter!("gw_credit_lease_acquire_total", "result" => "ok").increment(1);
                ctx.lease_guard = Some(guard);
                ctx.merged_perm = Some(merged);
                ctx.phase_end();
                return Ok(false);
            }
        }
    }
```

**注意**: `protection_cfg: &ProtectionConfig` 必须由调用方传入(同 `hard_cap_cfg`)。implementer 需要在 `request_filter` 函数签名上加这个参数,并在 `Wired` / `LlmGateway::request_filter` 路径上把 `&self.protection_cfg` 注入。Task 9 末尾 verify 时记得把全栈 build 跑通。

`ctx.ir.max_tokens` 字段需要先确认存在;若不存在,从 `ctx.body_json` 或 `ctx.parsed_request` 取——implementer 在写代码时 grep `max_tokens` 找到现有抽取路径,沿用即可。

- [ ] **Step 3: build 验证**

```bash
cargo build -p gw-proxy
```
Expected: 可能需要修 `protection_cfg` 参数传递、`ir.max_tokens` 字段路径。修通编译后继续。

- [ ] **Step 4: 提交**

```bash
git add crates/gw-proxy/src/filters/request.rs
git commit -m "feat(gw-proxy): insert lease decision at request_filter step 8.5 (protected mode)"
```

---

## Task 10: 在 response / logging 里 settle + release

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs` 或 `logging.rs`(取 final usage 落地点)
- Modify: `crates/gw-proxy/src/filters/logging.rs:110`

- [ ] **Step 1: 找到 final usage 注入 ReqCtx 的位置**

```bash
grep -n "final_usage\|UsageSnapshot\|ctx.usage = \|on_settle" crates/gw-proxy/src/filters/response.rs crates/gw-proxy/src/filters/logging.rs crates/gw-proxy/src/billing_lifecycle.rs 2>/dev/null | head -20
```

定位 final usage 写入 ctx 的位置(可能在 `response_body_filter` 或 `billing_lifecycle::on_settle`)。

- [ ] **Step 2: 在 final usage 拿到后调用 settle**

在定位到的位置之后追加:

```rust
    if let Some(guard) = ctx.lease_guard.as_ref() {
        let version = guard.coef_version();
        match gw_ledger::actual_usage_credits(
            version,
            ctx.merged_perm.as_ref().map(|m| m.model.0.as_str()).unwrap_or(""),
            &final_usage,
        ) {
            Ok(actual) => {
                if actual > guard.reserved() {
                    metrics::counter!(
                        "gw_credit_actual_exceeds_reserved_total",
                        "model_id" => ctx.merged_perm.as_ref().map(|m| m.model.0.clone()).unwrap_or_default()
                    )
                    .increment(1);
                }
                guard.settle_actual(actual);
                ctx.credits_consumed = Some(actual);
                ctx.coef_table_version_at_settle = Some(version);
            }
            Err(e) => {
                tracing::warn!(
                    target: "gw_proxy::filters::response",
                    error = ?e,
                    coef_version = version,
                    "actual_usage_credits failed; settle skipped"
                );
            }
        }
    }
```

- [ ] **Step 3: 修 logging 释放路径**

打开 `crates/gw-proxy/src/filters/logging.rs`,在 line 110 区域:

```rust
    if let Some(g) = ctx.freeze_guard.as_ref() {
        g.release();
    }
    if let Some(g) = ctx.lease_guard.as_ref() {
        g.release();
    }
```

`lease_guard` 和 `freeze_guard` 互斥(step 8.5 决定其中之一进 ctx),所以这里不会同时跑两条。

- [ ] **Step 4: build + test**

```bash
cargo build -p gw-proxy
cargo test -p gw-proxy
```
Expected: build OK,既有测试不退化。

- [ ] **Step 5: 提交**

```bash
git add crates/gw-proxy/src/filters/response.rs crates/gw-proxy/src/filters/logging.rs
git commit -m "feat(gw-proxy): LeaseGuard settle_actual + release in response/logging filters"
```

---

## Task 11: metrics 登记 + admin healthz 端点

**Files:**
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `crates/gw-gateway/src/health.rs`(添加两个 admin 端点)

- [ ] **Step 1: metrics 登记**

打开 `crates/gw-gateway/src/metrics_catalog.rs`,在合适分组下追加(按现有风格):

```rust
    metrics::describe_counter!(
        "gw_credit_lease_acquire_total",
        "Lease acquire attempts, by result (ok | insufficient)"
    );
    metrics::describe_counter!(
        "gw_credit_lease_expired_total",
        "Requests rejected because lease TTL expired"
    );
    metrics::describe_counter!(
        "gw_credit_actual_exceeds_reserved_total",
        "Settle phase observed actual credits > reserved (estimator drift signal)"
    );
    metrics::describe_counter!(
        "gw_credit_default_ceiling_used_total",
        "Times the fallback output token ceiling was used during estimation"
    );
    metrics::describe_counter!(
        "gw_credit_lease_warmup_pending_total",
        "Requests on a user not yet visible in LeasePool (Pod still catching up)"
    );
    metrics::describe_counter!(
        "gw_credit_coef_table_incomplete_total",
        "coef_table commit arrived with fewer staged models than expected"
    );
    metrics::describe_gauge!(
        "gw_credit_lease_active_users",
        "Number of users currently in protected mode"
    );
    metrics::describe_gauge!(
        "gw_credit_coef_table_version_active",
        "Currently active coef_table_version"
    );
```

- [ ] **Step 2: 写失败测试 + 实现 healthz 端点**

打开 `crates/gw-gateway/src/health.rs`,定位现有 `/healthz/*` 路由表(`grep healthz`)。追加两个 handler 函数和路由:

```rust
async fn handle_credit_lease(uid: String) -> impl axum::response::IntoResponse {
    use gw_core::ids::UserId;
    use gw_ledger::LEASE_POOL;
    match LEASE_POOL.get(&UserId::from(uid.as_str())) {
        Some(state) => {
            let body = serde_json::json!({
                "user_id": uid,
                "mode": match state.mode() {
                    gw_ledger::Mode::Protected => "protected",
                    gw_ledger::Mode::Unprotected => "unprotected",
                },
                "credits": state.credits(),
                "expires_at_ms": state.expires_at_ms.load(std::sync::atomic::Ordering::Acquire),
                "coef_table_version": state.coef_table_version.load(std::sync::atomic::Ordering::Acquire),
                "lease_id": (*state.lease_id.load()).clone(),
            });
            (axum::http::StatusCode::OK, axum::Json(body))
        }
        None => (
            axum::http::StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": "no lease for user"})),
        ),
    }
}

async fn handle_credit_coef_table() -> impl axum::response::IntoResponse {
    let snap = gw_ledger::current_coef_snapshot();
    let body = serde_json::json!({
        "version": snap.version,
        "model_count": snap.by_model.len(),
        "models": snap.by_model.keys().take(64).collect::<Vec<_>>(),
    });
    (axum::http::StatusCode::OK, axum::Json(body))
}
```

并在 router 注册处追加:

```rust
        .route("/healthz/credit/lease/:uid", get(handle_credit_lease))
        .route("/healthz/credit/coef-table", get(handle_credit_coef_table))
```

这两个端点应与现有 token-gated `/healthz/*` 端点放在同一鉴权层下;按现有 pattern 落位即可。

- [ ] **Step 3: build**

```bash
cargo build -p gw-gateway
```
Expected: OK。

- [ ] **Step 4: 提交**

```bash
git add crates/gw-gateway/src/metrics_catalog.rs crates/gw-gateway/src/health.rs
git commit -m "feat(gw-gateway): metrics + /healthz/credit endpoints for lease + coef table"
```

---

## Task 12: 不变量 lint

**Files:**
- Modify: `scripts/check-invariants.sh`

- [ ] **Step 1: 加 `no_currency_in_lease` 规则**

打开 `scripts/check-invariants.sh`,在末尾追加(或在合适位置):

```bash
# ----------------------------------------------------------------
# no_currency_in_lease
# Credit lease layer must NOT know currency / pricing / micro-units.
# See spec 2026-05-29-credit-lease-design.md §2.
# ----------------------------------------------------------------
lint_no_currency_in_lease() {
    local out
    out=$(grep -iEn 'usd|cny|micro|cost|price' \
        crates/gw-ledger/src/lease.rs \
        crates/gw-ledger/src/coef.rs \
        crates/gw-ledger/src/credit_estimator.rs \
        crates/gw-ledger/src/protection_config.rs \
        2>/dev/null \
        | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' \
        | grep -vE '^[^:]+:[0-9]+:[[:space:]]*\*' \
        || true)
    if [ -n "$out" ]; then
        echo "✗ no_currency_in_lease: credit lease modules must not reference currency or pricing:"
        echo "$out"
        return 1
    fi
    echo "✓ no_currency_in_lease"
}

lint_no_currency_in_lease || fail=$((fail + 1))
```

注释/docstring 中提及"micro"等词是允许的(grep `-vE '//'` / `'*'` 过滤);如果项目用别的注释风格,implementer 调整正则。

- [ ] **Step 2: 跑 invariants**

```bash
bash scripts/check-invariants.sh
```
Expected: `no_currency_in_lease` PASS,总体 PASS。

- [ ] **Step 3: 提交**

```bash
git add scripts/check-invariants.sh
git commit -m "test(invariants): add no_currency_in_lease lint for credit lease modules"
```

---

## Task 13: 集成测试 — 端到端 lease 路径

**Files:**
- Create: `tests/integration/credit_lease/mod.rs`
- Create: `tests/integration/credit_lease/lease_pool_test.rs`
- Create: `tests/integration/credit_lease/coef_cache_test.rs`
- Create: `tests/integration/credit_lease/end_to_end_test.rs`
- Modify: `tests/integration/main.rs`(注册新 mod)

- [ ] **Step 1: 写端到端集成测试入口**

确认 `tests/integration/` 目录约定(implementer 看现有 `tests/integration/main.rs` / `mod.rs` 风格)。创建 `tests/integration/credit_lease/mod.rs`:

```rust
mod coef_cache_test;
mod end_to_end_test;
mod lease_pool_test;
```

- [ ] **Step 2: lease_pool_test.rs**

```rust
//! 集成测试:从 counter event payload 走完整路径,触发 LeasePool 状态改变,
//! 验证 LeaseGuard CAS / 过期 / 退还行为。

use gw_core::ids::UserId;
use gw_ledger::{
    actual_usage_credits, apply_coef_commit, apply_coef_entry, estimate_request_credits, now_ms,
    upsert_protected, upsert_unprotected, Dim, LeaseError, LeaseGuard, Mode, LEASE_POOL,
};
use gw_core::usage::UsageSnapshot;

fn install_coefs() {
    apply_coef_entry(1, "test-model", Dim::Input, 10);
    apply_coef_entry(1, "test-model", Dim::Output, 20);
    apply_coef_commit(1, 1);
}

#[test]
fn protected_user_with_sufficient_lease_can_acquire_then_settle() {
    install_coefs();
    let uid = UserId::from("u_integration_lease_1");
    upsert_protected(&uid, 1_000_000, "lease-int-1".into(), 1, now_ms() + 600_000);

    let credits = estimate_request_credits(1, "test-model", 100, 200).unwrap();
    assert_eq!(credits, 100 * 10 + 200 * 20);

    let guard = LeaseGuard::try_acquire(&uid, credits, now_ms()).unwrap();
    let pool_state = LEASE_POOL.get(&uid).unwrap();
    assert_eq!(pool_state.credits(), 1_000_000 - credits);

    let usage = UsageSnapshot {
        input: 100,
        output: 150,
        ..Default::default()
    };
    let actual = actual_usage_credits(1, "test-model", &usage).unwrap();
    guard.settle_actual(actual);
    // Refund of (reserved - actual) = (5000 - (100*10 + 150*20)) = 5000 - 4000 = 1000
    assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 1_000_000 - actual);
}

#[test]
fn protected_user_low_lease_rejects_request() {
    install_coefs();
    let uid = UserId::from("u_integration_lease_2");
    upsert_protected(&uid, 100, "lease-int-2".into(), 1, now_ms() + 600_000);

    let credits = estimate_request_credits(1, "test-model", 100, 100).unwrap();
    let err = LeaseGuard::try_acquire(&uid, credits, now_ms()).unwrap_err();
    assert!(matches!(err, LeaseError::Insufficient { .. }));
}

#[test]
fn lease_expiration_triggers_lease_error_expired() {
    install_coefs();
    let uid = UserId::from("u_integration_lease_3");
    upsert_protected(&uid, 10_000, "lease-int-3".into(), 1, now_ms() - 1);
    let err = LeaseGuard::try_acquire(&uid, 1000, now_ms()).unwrap_err();
    assert!(matches!(err, LeaseError::Expired));
}

#[test]
fn switching_back_to_unprotected_clears_state() {
    install_coefs();
    let uid = UserId::from("u_integration_lease_4");
    upsert_protected(&uid, 10_000, "lease-int-4".into(), 1, now_ms() + 600_000);
    upsert_unprotected(&uid);
    assert_eq!(LEASE_POOL.get(&uid).unwrap().mode(), Mode::Unprotected);
}
```

- [ ] **Step 3: coef_cache_test.rs**

```rust
//! 集成测试:版本切换 + 不完整提交 + stale 提交。

use gw_ledger::{
    apply_coef_commit, apply_coef_entry, current_coef_snapshot, lookup_coef, CommitOutcome, Dim,
};

#[test]
fn complete_push_swaps_active_snapshot() {
    apply_coef_entry(50, "alpha", Dim::Input, 5);
    apply_coef_entry(50, "alpha", Dim::Output, 7);
    apply_coef_entry(50, "beta", Dim::Input, 1);
    let outcome = apply_coef_commit(50, 2);
    assert!(matches!(outcome, CommitOutcome::Activated));
    assert_eq!(current_coef_snapshot().version, 50);
    assert!(lookup_coef(50, "alpha").is_some());
    assert!(lookup_coef(50, "beta").is_some());
}

#[test]
fn incomplete_commit_keeps_previous() {
    let before = current_coef_snapshot().version;
    apply_coef_entry(60, "lonely", Dim::Input, 1);
    let outcome = apply_coef_commit(60, 3);
    assert!(matches!(outcome, CommitOutcome::Incomplete { staged: 1, expected: 3 }));
    assert_eq!(current_coef_snapshot().version, before);
}

#[test]
fn stale_commit_for_older_version_is_dropped() {
    apply_coef_entry(70, "a", Dim::Input, 1);
    apply_coef_commit(70, 1);
    // Older version commit should not regress.
    apply_coef_entry(69, "old", Dim::Input, 100);
    let outcome = apply_coef_commit(69, 1);
    assert!(matches!(outcome, CommitOutcome::Stale { .. }));
    assert_eq!(current_coef_snapshot().version, 70);
}
```

- [ ] **Step 4: end_to_end_test.rs(占位 + 真实集成)**

```rust
//! 端到端集成:启动一个 in-process gateway test fixture,推一条 protected_lease
//! 事件,发请求,验证响应路径走 LeaseGuard。
//!
//! NOTE: implementer 需要参考现有 `tests/integration/<feature>.rs` 的 fixture
//! pattern。本测试占位以记录预期场景,具体实现按现有 e2e 模式落位。

#[test]
#[ignore]
fn protected_user_request_uses_lease_path() {
    // 1. Spawn gateway with config.protection.enabled = true.
    // 2. Seed Postgres with a user + api key + entity row for a model.
    // 3. XADD events:counter ns=coef_table for the model.
    // 4. XADD events:counter ns=coef_table_commit.
    // 5. XADD events:counter ns=protected_lease mode=protected with lease_credits.
    // 6. Wait for stream consumer to apply (small sleep / poll LEASE_POOL).
    // 7. POST a chat request; assert 200.
    // 8. Read usage:records latest; assert pod_id non-empty,
    //    credits_consumed > 0, coef_table_version matches push.
    // 9. Pop another request that would overshoot lease; assert 402 with
    //    GW_QUOTA_LEASE_INSUFFICIENT.
    // 10. XADD ns=protected_lease mode=unprotected; assert next request 200
    //     and credits_consumed = None in usage_records.
    unimplemented!("see comments; wire to existing e2e fixture pattern");
}
```

`#[ignore]` 让默认 `cargo test` 不跑这个 placeholder,implementer 把它写实后去掉 `ignore`。

- [ ] **Step 5: 在 `tests/integration/main.rs` 注册**

```rust
mod credit_lease;
```

(若现有 main.rs 用别的注册方式,沿用其 pattern。)

- [ ] **Step 6: 跑测试**

```bash
cargo test --test integration credit_lease
```
Expected: lease_pool_test + coef_cache_test 全 PASS,end_to_end_test 被 ignore。

- [ ] **Step 7: 提交**

```bash
git add tests/integration/credit_lease/ tests/integration/main.rs
git commit -m "test(credit_lease): integration coverage for LeasePool, CoefCache, e2e placeholder"
```

---

## Task 14: 文档同步

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`
- Modify: `CLAUDE.md`(在 capability surface 列表加一项)

- [ ] **Step 1: architecture.md 索引新模块**

打开 `docs/architecture.md`,在 §Workspace layout 表里 `gw-ledger` 一行的 "responsibilities" 字段追加:`lease/coef/credit_estimator (2026-05-29)`。在 §Configuration 表追加 `[protection]` 行,在 §Invariants 列表追加 `no_currency_in_lease`,在 §"Deliberate behaviours" 追加"protected mode fail-closed on TTL expiry"。

- [ ] **Step 2: workflow.md 加 step 8.5**

打开 `docs/workflow.md`,定位 `request_filter` step 列表,在 step 9 (Hard caps) 之前插入:

```markdown
### Step 8.5 — Credit lease (protected mode)

If `[protection].enabled` and the user has `mode=protected` in `LeasePool`:
- Compute `est_input_tokens × coefs.input + max_out × coefs.output` using
  `coef_table_version` carried in the lease.
- `LeaseGuard::try_acquire`. On `Expired` → `GW_QUOTA_LEASE_EXPIRED` (503).
  On `Insufficient` → `GW_QUOTA_LEASE_INSUFFICIENT` (402). On `NoLease`
  (warm-up race) → fall through to step 9-10 with `frozen_micro`.
- On success, `ctx.lease_guard = Some(guard)`. **Skip step 10** (RequestFreeze
  bypassed for this request).

See [`docs/superpowers/specs/2026-05-29-credit-lease-design.md`](superpowers/specs/2026-05-29-credit-lease-design.md).
```

- [ ] **Step 3: CLAUDE.md capability surface 追加一行**

在 `CLAUDE.md` 的 "Capability surface highlights" 列表里追加:

```markdown
- **Credit lease (hard overdraft protection)**: optional `[protection]` layer above the legacy soft `frozen_micro` defense. When enabled and the user's wallet balance drops below Go's configured threshold, Go pushes per-Pod credit leases via `events:counter ns=protected_lease`; Rust enforces fail-closed TTL on the lease, hard-rejects with `GW_QUOTA_LEASE_INSUFFICIENT` / `GW_QUOTA_LEASE_EXPIRED` when the lease is exhausted or stale. Outstanding ledger on the Go side guarantees `Σ outstanding ≤ balance × (1 + overcommit_ratio)`. See [`docs/superpowers/specs/2026-05-29-credit-lease-design.md`](docs/superpowers/specs/2026-05-29-credit-lease-design.md) and [`docs/go-contracts/contract-change/2026-05-29-credit-lease.md`](docs/go-contracts/contract-change/2026-05-29-credit-lease.md).
```

- [ ] **Step 4: 提交**

```bash
git add docs/architecture.md docs/workflow.md CLAUDE.md
git commit -m "docs: sync architecture/workflow/CLAUDE for credit lease layer"
```

---

## Task 15: 最终 verification

**Files:** N/A — pure verification

- [ ] **Step 1: 全量 fmt/clippy/test**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-invariants.sh
```
Expected: 全部 PASS。Clippy 出 warning 时按提示 fix。

- [ ] **Step 2: 跑 e2e-soak-smoke**

```bash
make e2e-soak-smoke
```
Expected: 软防路径(默认 `enabled = false`)不退化;不变量全过。

- [ ] **Step 3: 提交(若 step 1/2 中有 fmt/clippy 自动 fix 改动)**

```bash
git add -A
git commit -m "chore: post-lease verification fixups"
```

- [ ] **Step 4: 总览检查**

```bash
git log --oneline -20
git diff --stat origin/main
```
Expected: ~13-15 个 commit,每个 commit 都过测试。

---

## Self-Review 备注

写完计划后,检查 spec 各节是否都有对应任务:

| Spec 节 | 任务 |
|---|---|
| §1 动机 | (无需任务) |
| §2 核心理念 | Task 12 lint |
| §3 双模式 | Task 9 (request_filter 分支) |
| §4 安全不变量 | 跨进程,Go 侧实现;Rust 侧靠 Task 8 字段扩展间接配合 |
| §5.1.1 protected_lease ns | Task 6 (框架) + Task 7 (handler) |
| §5.1.2 coef_table ns | Task 7 (handler) + Task 3 (cache 实现) |
| §5.1.3 coef_table_commit | Task 7 + Task 3 |
| §5.2 Rust→Go | Task 8 (UsageRecord 字段) |
| §5.3 pod_id | Task 8 (cached instance_id helper) |
| §6.1 LeasePool/Guard | Task 4 |
| §6.2 CoefCache | Task 3 |
| §6.3 credit_estimator | Task 5 |
| §6.4 集成到 filter | Task 9 / Task 10 |
| §7 TTL fail-closed | Task 4 (try_acquire) + Task 9 (错误码) |
| §7.1 Go 推流中断 | Task 11 (metric) |
| §7.2 启动 warm-up | Task 9 (NoLease fallthrough) |
| §8 Pod 退出 | (Rust 侧无操作;契约描述于 contract-change) |
| §9 配置 | Task 2 |
| §10 契约 | Task 8 (字段) + Task 6/7 (ns 分发) |
| §11 错误码 | Task 1 |
| §12 可观测性 | Task 11 |
| §13 灰度 | (运维,不需代码任务) |
| §14 trade-off | (文档) |
| §15 不做的事 | (无需任务) |
| §16 验证 | Task 13 + Task 15 |

**已知后续优化(本计划不实现)**:
- end_to_end_test.rs 仍是 placeholder;真实 e2e 等 implementer 接通现有 `tests/integration/` fixture
- `gw_credit_lease_active_users` / `gw_credit_lease_credits_remaining` 两个 gauge 当前没有采样源,需要后台定时任务,implementer 选择按需补
- soak 加 `--protected-mode` 子模式(spec §16) ,本计划未拆为单独任务,建议在 plan 验收后另起 PR
