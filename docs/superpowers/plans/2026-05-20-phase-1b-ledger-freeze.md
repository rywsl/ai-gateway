# Phase 1b — gw-ledger balance/freeze 切边 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the concrete `FreezeGuard` struct with a trait + release-only semantics; rename `BillingRecord` → `UsageRecord` (6-field `UsageSnapshot` + `MarkupChainSnapshot`, drop `cost_micro`); rename Redis stream `billing:records` → `usage:records`; remove the flusher's `INCRBY counter:user:*:balance_micro -cost` write so Rust no longer mutates `deducted_micro`. Keep the build green throughout — every API change ships with its call-site updates and tests.

**Architecture:** Five renames + one trait extraction, ordered so the build stays green:

1. New POD types land first in their authoritative crates (`gw-core::usage::UsageSnapshot`, `gw-auth::merge::MarkupChainSnapshot`) — no removals yet.
2. `gw-ledger::freeze` swaps the concrete struct for a `FreezeGuard` trait + `RequestFreeze` impl (release-only). `TaskFreeze` is a stub — its Lua release lands in Phase 6.
3. `gw-ledger::billing` becomes `gw-ledger::usage` with `UsageRecord` shaped per C-α §ADR-Cα-1 minus `cost_micro`. `MarkupChainSnapshot` is populated with all-1.0 placeholders (Phase 5 wires real values from `merge_permissions`).
4. `gw-ledger::flusher` only does `XADD usage:records` — the `INCRBY balance_micro -cost` path is deleted.
5. Call sites in `gw-proxy::filters::{request,logging}`, `gw-proxy::ctx`, and integration tests migrate to the new APIs in lockstep.
6. The four PENDING lint blocks (`no_cost_micro`, `no_billing_records_stream`, `no_freezeguard_settle`, `no_markup_product_scalar`) activate in `scripts/check-invariants.sh`.

Scope deferred to Phase 5 (per integration spec §4.1):

- Wiring real 5-tier markup values through `merge_permissions` (Bm1) — Phase 1b leaves the field shape in place with all-1.0 placeholders.
- C-γ Redis `HINCRBY frozen_micro` async path + `events:counter` consumer.
- C-β `EstimatePricing` entity (`estimate_pricing_version` field is reserved as `u64 = 0` in 1b).
- Startup-time alignment of local `DeductionState` from Redis.

Scope deferred to Phase 6: `TaskFreeze` Lua release path.

**Tech Stack:** Rust 1.86.0, tokio mpsc, fred (Redis client), serde_json. No new external dependencies.

**Authoritative references:**

- Integration spec: `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §2 表 A (A3 / A4 / A5 / A6 / A12 / A13) + §3 (C-1 / C-3 / C-7) + §4.1 Phase 1 + §4.2 Phase 1 exit gate + §5.2 lints.
- C-α: `docs/superpowers/specs/2026-05-19-billing-c-alpha-usage-record.md` (UsageRecord shape).
- C-γ: `docs/superpowers/specs/2026-05-19-billing-c-gamma-balance-split.md` §ADR-Cγ-2 (release-only) + §ADR-Cγ-6 (flusher INCRBY removal).
- Memory: `feedback-breaking-changes-ok` — no `#[deprecated]` shims, no double-write windows, one-shot rename.

---

## File map

**Created:**

- `crates/gw-core/src/usage.rs` — `UsageSnapshot` POD (6 fields per C-α §ADR-Cα-1).
- `crates/gw-ledger/src/usage.rs` — `UsageRecord`, `UsageStatus`, `UsageSource` (relocated + reshaped from `billing.rs`).

**Modified:**

- `crates/gw-core/src/lib.rs` — register `pub mod usage`.
- `crates/gw-core/src/permissions.rs` — `MergedPermissions::markup_product: f32` → `markup_chain: MarkupChainSnapshot`.
- `crates/gw-auth/Cargo.toml` — no change (gw-auth already depends on gw-core).
- `crates/gw-auth/src/merge.rs` — define `MarkupChainSnapshot`, build it in `merge_permissions` (placeholder: all-1.0 except `product`).
- `crates/gw-auth/src/lib.rs` — re-export `MarkupChainSnapshot` via `pub use merge::MarkupChainSnapshot;`.
- `crates/gw-auth/tests/merge.rs` — assert `markup_chain.product` equals the old `markup_product` value (13.5).
- `crates/gw-auth/tests/hard_cap.rs` / `tests/ratelimit.rs` — initialize `markup_chain` instead of `markup_product`.
- `crates/gw-ledger/Cargo.toml` — add `gw-auth = { path = "../gw-auth" }`.
- `crates/gw-ledger/src/lib.rs` — `pub mod usage` (replaces `pub mod billing`), update re-exports.
- `crates/gw-ledger/src/freeze.rs` — convert concrete struct to `FreezeGuard` trait + `RequestFreeze` + `TaskFreeze`.
- `crates/gw-ledger/src/flusher.rs` — drop `INCRBY balance_micro` path, rename stream constant, mpsc carries `UsageRecord`.
- `crates/gw-ledger/tests/freeze.rs` — settle/double-settle tests → release/double-release tests; no `deducted_micro` assertions.
- `crates/gw-ledger/tests/flusher.rs` — stream name `usage:records`; balance assertion removed (no longer Rust's job).
- `crates/gw-proxy/src/ctx.rs` — `freeze_guard: Option<FreezeGuard>` → `Option<RequestFreeze>`; update use statement.
- `crates/gw-proxy/src/filters/request.rs` — `FreezeGuard::freeze(...)` → `RequestFreeze::new(...)`; update doc comments.
- `crates/gw-proxy/src/filters/logging.rs` — remove cost computation (lines 93-105); `g.settle(cost_micro)` → `g.release()`; build `UsageRecord` (not `BillingRecord`); drop `markup_product` reads, copy `merged.markup_chain`.
- `crates/gw-proxy/src/filters/mod.rs` / `src/detailed_log.rs` — doc comments referencing `BillingRecord` → `UsageRecord`.
- `crates/gw-proxy/tests/freeze_lifecycle.rs` — settle → release; drop `deducted_micro` assertions.
- `crates/gw-proxy/tests/end_to_end_openai.rs` — comment refers to `(DeductionState.deducted_micro, BillingRecord emission)` → `(DeductionState.frozen_micro, UsageRecord emission)`.
- `crates/gw-gateway/tests/e2e_failure_modes.rs` — settle → release; drop `deducted_micro` assertions; `markup_product: 1.0` → `markup_chain: MarkupChainSnapshot::placeholder()`.
- `crates/gw-gateway/src/lib.rs` — `use gw_ledger::Ledger;` (no surface change; doc comments referring to BillingRecord update).
- `CLAUDE.md` — the "Architecture: filter chain" table row for `logging` swaps "settle freeze" → "release freeze" + "emit `BillingRecord`" → "emit `UsageRecord`".
- `scripts/check-invariants.sh` — activate four PENDING lints (`no_cost_micro`, `no_billing_records_stream`, `no_freezeguard_settle`, `no_markup_product_scalar`).

**Deleted:**

- `crates/gw-ledger/src/billing.rs` (content relocated to `usage.rs`).

---

## Task 1: `gw-core::usage::UsageSnapshot`

**Files:**

- Create: `crates/gw-core/src/usage.rs`
- Modify: `crates/gw-core/src/lib.rs`

The POD lives in `gw-core` so both `gw-codec` (S4 `extract_usage_any` output, Phase 2) and `gw-ledger` (UsageRecord embedded field, Task 4) can use it without a sideways dep.

- [ ] **Step 1: Write the failing test**

Append to `crates/gw-core/src/usage.rs`:

```rust
//! Token-usage snapshot — POD shared between codec extractors and the
//! UsageRecord that the ledger flusher publishes to `usage:records`.
//!
//! Authoritative location per integration spec §3 C-7. Six dimensions cover
//! all current codecs (OpenAI / Anthropic / Gemini / Codex Responses /
//! Kiro EventStream): plain input, output, cache-read, cache-creation,
//! thinking (extended), and multimodal-derived input tokens.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub thinking_tokens: Option<u32>,
    pub multimodal_input_tokens: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_zero_none() {
        let u = UsageSnapshot::default();
        assert_eq!(u.input_tokens, 0);
        assert_eq!(u.output_tokens, 0);
        assert!(u.cache_read_input_tokens.is_none());
        assert!(u.cache_creation_input_tokens.is_none());
        assert!(u.thinking_tokens.is_none());
        assert!(u.multimodal_input_tokens.is_none());
    }

    #[test]
    fn serde_roundtrip_preserves_optional_none() {
        let u = UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        };
        let s = serde_json::to_string(&u).unwrap();
        let back: UsageSnapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(u, back);
    }
}
```

- [ ] **Step 2: Register the module in `crates/gw-core/src/lib.rs`**

Add `pub mod usage;` to the existing module list. The exact location in lib.rs is alongside the other `pub mod ids;` / `pub mod permissions;` declarations.

- [ ] **Step 3: Add `serde_json` as a dev-dependency** if not already present

Check `crates/gw-core/Cargo.toml`. If `serde_json` is not under `[dev-dependencies]`, add `serde_json.workspace = true`. (Most workspace crates already have it; verify before editing.)

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p gw-core usage:: -- --nocapture
```

Expected: 2 tests pass.

- [ ] **Step 5: Verify workspace still builds**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: `Finished` line; no errors.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-core/src/usage.rs crates/gw-core/src/lib.rs crates/gw-core/Cargo.toml
git commit -m "feat(gw-core): add UsageSnapshot POD (6-dim token usage)

Authoritative location per v2 integration spec §3 C-7. UsageSnapshot
is the shared output of codec extractors and the embedded usage field
of UsageRecord (Task 4)."
```

---

## Task 2: `MarkupChainSnapshot` + `MergedPermissions::markup_chain`

**Files:**

- Modify: `crates/gw-auth/src/merge.rs` — define `MarkupChainSnapshot`, populate it in `merge_permissions`.
- Modify: `crates/gw-auth/src/lib.rs` — re-export.
- Modify: `crates/gw-core/src/permissions.rs` — replace `markup_product: f32` with `markup_chain: MarkupChainSnapshot`.
- Modify: `crates/gw-core/Cargo.toml` — no change (the snapshot lives in gw-auth; `MergedPermissions` field uses it via `pub use`).
- Modify: `crates/gw-auth/Cargo.toml` — no change.

**Problem:** `MarkupChainSnapshot` per C-7 lives in `gw-auth::merge`, but `MergedPermissions` is in `gw-core::permissions` and `gw-core` cannot depend on `gw-auth` (C-1 layering). Resolution: define `MarkupChainSnapshot` in **`gw-core::permissions`** (not `gw-auth`) and re-export via `gw-auth::merge::MarkupChainSnapshot = gw_core::permissions::MarkupChainSnapshot`. This satisfies the C-7 contract (the qualified path `gw-auth::merge::MarkupChainSnapshot` resolves) and keeps the dep direction clean.

- [ ] **Step 1: Define `MarkupChainSnapshot` in `gw-core::permissions`**

Replace the current `crates/gw-core/src/permissions.rs` content with:

```rust
use crate::ids::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::net::IpAddr;

/// Snapshot of the 4-tier markup chain plus its scalar product.
///
/// Phase 1b: the four tier fields are populated as 1.0 placeholders by
/// `merge_permissions`; only `product` carries the real value (== the old
/// `markup_product`). Phase 5 wires the real per-tier values through.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkupChainSnapshot {
    pub user_group_markup: f32,
    pub api_key_markup: f32,
    pub vendor_markup: f32,
    pub model_markup: f32,
    /// `user_group_markup * api_key_markup * vendor_markup * model_markup`.
    pub product: f32,
}

impl MarkupChainSnapshot {
    /// All-1.0 placeholder. Used by tests and by `merge_permissions` before
    /// the per-tier values are plumbed through (Phase 5).
    pub fn placeholder() -> Self {
        Self {
            user_group_markup: 1.0,
            api_key_markup: 1.0,
            vendor_markup: 1.0,
            model_markup: 1.0,
            product: 1.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MergedPermissions {
    pub user_id: UserId,
    pub effective_user_id: UserId,
    pub client_ip: IpAddr,
    pub allowed_models: HashSet<ModelName>,
    pub allowed_channels: HashSet<KeyId>,
    pub allowed_groups: HashSet<Group>,
    pub forced_model_group: Option<Group>,
    pub rpm_limit: u32,
    pub tpm_limit: u32,
    pub max_concurrent_streams: u32,
    pub markup_chain: MarkupChainSnapshot,
}
```

- [ ] **Step 2: Re-export from `gw-auth::merge`**

Append to `crates/gw-auth/src/merge.rs` (near the top, after the `use` block):

```rust
pub use gw_core::permissions::MarkupChainSnapshot;
```

This makes `gw-auth::merge::MarkupChainSnapshot` resolve as required by C-7.

- [ ] **Step 3: Update `merge_permissions` to build the snapshot**

In `crates/gw-auth/src/merge.rs`, find the existing markup computation:

```rust
let markup = user.value.markup
    * main.value.markup
    * groups.iter().fold(1.0_f32, |a, g| a * g.value.markup)
    * api_key.as_ref().map(|a| a.value.markup).unwrap_or(1.0);
```

And the `Ok(MergedPermissions { ... markup_product: markup, })` construction at the bottom. Replace the construction's `markup_product: markup,` with:

```rust
markup_chain: MarkupChainSnapshot {
    // Phase 1b: per-tier values left as 1.0 placeholders.
    // Phase 5 (Bm1) populates these from the actual entity reads above.
    user_group_markup: 1.0,
    api_key_markup: 1.0,
    vendor_markup: 1.0,
    model_markup: 1.0,
    product: markup,
},
```

- [ ] **Step 4: Update `tests/merge.rs` assertion**

In `crates/gw-auth/tests/merge.rs`, find:

```rust
assert!((perm.markup_product - 13.5).abs() < 1e-4);
```

Replace with:

```rust
assert!((perm.markup_chain.product - 13.5).abs() < 1e-4);
```

- [ ] **Step 5: Update `tests/hard_cap.rs` and `tests/ratelimit.rs`**

Both files build a literal `MergedPermissions { ..., markup_product: 1.0 }`. Replace `markup_product: 1.0,` with `markup_chain: gw_core::permissions::MarkupChainSnapshot::placeholder(),` in each occurrence.

- [ ] **Step 6: Run the gw-auth tests to verify**

```bash
cargo test -p gw-auth 2>&1 | tail -20
```

Expected: all tests pass (3 in merge, 3 in hard_cap, 1 in ratelimit per current count).

- [ ] **Step 7: Verify workspace still builds**

```bash
cargo build --workspace 2>&1 | tail -5
```

Expected: errors will be produced from other crates (gw-proxy, gw-gateway tests) referencing `.markup_product`. **Do not fix those yet** — Task 6 + Task 7 own those call sites. The intent at this step is to confirm that `gw-core` and `gw-auth` compile cleanly on their own:

```bash
cargo build -p gw-core -p gw-auth 2>&1 | tail -5
```

Expected: clean build.

- [ ] **Step 8: Commit (do not push)**

The workspace will not be green after this commit alone — that is expected; the chain stays open until Task 7. We commit per-task so reviewers see one rename per commit, but **do not run `cargo test --workspace` until Task 7 completes**.

```bash
git add crates/gw-core/src/permissions.rs crates/gw-auth/src/merge.rs \
        crates/gw-auth/src/lib.rs crates/gw-auth/tests/merge.rs \
        crates/gw-auth/tests/hard_cap.rs crates/gw-auth/tests/ratelimit.rs
git commit -m "refactor(gw-core,gw-auth): MergedPermissions.markup_product -> markup_chain

Replaces the scalar markup_product f32 with MarkupChainSnapshot (5 fields).
Phase 1b populates per-tier values as 1.0 placeholders; product carries
the real value. Phase 5 (Bm1) plumbs real per-tier markups through
merge_permissions.

Type defined in gw-core::permissions to satisfy the C-1 dep direction;
re-exported from gw-auth::merge per integration spec §3 C-7.

Workspace will not be green until Tasks 4-7 land — call sites in
gw-ledger, gw-proxy, and gw-gateway/tests still reference the old name."
```

> **Note on intermediate red builds:** Tasks 2 through 7 each leave the workspace partially broken. The build returns green at the end of Task 7. Reviewers should evaluate the *chain* end-to-end; each individual commit is a logical step, not a green checkpoint.

---

## Task 3: `FreezeGuard` trait + `RequestFreeze` + `TaskFreeze` placeholder

**Files:**

- Modify: `crates/gw-ledger/src/freeze.rs` — concrete struct → trait + two impls.
- Modify: `crates/gw-ledger/src/lib.rs` — update re-exports.

- [ ] **Step 1: Rewrite `crates/gw-ledger/src/freeze.rs`**

Replace the entire file content with:

```rust
//! FreezeGuard — release-only RAII handle for the per-user `frozen_micro`
//! counter (ADR-004 + C-γ §ADR-Cγ-2).
//!
//! Two impls live here:
//!
//! - [`RequestFreeze`] — used by `gw-proxy` for one freeze per request.
//!   Constructed before upstream send; released (idempotent) by the
//!   `logging` filter once the request is settled, or by `Drop` on a
//!   cancellation path.
//! - [`TaskFreeze`] — placeholder for the task path. The real release
//!   (`HINCRBY task_frozen_micro -frozen`, atomic with `entity:task` status
//!   transition and `ZREM tasks:leases`) is implemented by `gw-task-runtime`
//!   in Phase 6. The 1b stub does nothing on release; constructing one is
//!   currently used only by type-system smoke tests.
//!
//! What Phase 1b deliberately does NOT do (deferred to Phase 5/6):
//!
//! - Async `HINCRBY counter:user:<uid>:frozen_micro` to Redis (C-γ
//!   §ADR-Cγ-3): in-process atomic only for now.
//! - Lua-driven Task release path (C-γ Phase 6).
//! - Writing `deducted_micro`: **never** — that's Go's exclusive write
//!   per C-γ §ADR-Cγ-1.

use gw_core::ids::UserId;
use gw_store::deduction::{DEDUCT, DeductionState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

/// Categorises a freeze for telemetry and (Phase 6) routing in the task
/// settle path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeKind {
    Request,
    Task,
}

/// Common contract for both per-request and per-task freezes.
///
/// `release` is idempotent and called both explicitly (by the proxy
/// `logging` filter / worker settle) and via `Drop` on cancellation paths.
pub trait FreezeGuard: Send + Sync {
    fn kind(&self) -> FreezeKind;
    fn freeze_amount(&self) -> u64;
    fn release(&self);
}

/// Per-request freeze. Mutates the in-process `DeductionState.frozen_micro`
/// only — Redis sync is Phase 5 (C-γ §ADR-Cγ-3) work.
pub struct RequestFreeze {
    pub uid: UserId,
    pub freeze_amount: u64,
    pub released: AtomicBool,
    pub state: Arc<DeductionState>,
}

impl RequestFreeze {
    /// Reserve `amount` micro-units against the user's `frozen_micro`.
    /// The returned guard MUST be released (explicitly or via `Drop`).
    // TODO(memory): DEDUCT entries persist for the process lifetime; periodic
    // GC of zero-state entries belongs to a follow-up under C-γ.
    pub fn new(uid: UserId, amount: u64) -> Self {
        let state = DEDUCT
            .entry(uid.clone())
            .or_insert_with(|| {
                Arc::new(DeductionState {
                    base_micro: AtomicI64::new(0),
                    deducted_micro: AtomicI64::new(0),
                    frozen_micro: AtomicI64::new(0),
                })
            })
            .clone();
        state
            .frozen_micro
            .fetch_add(amount as i64, Ordering::AcqRel);
        Self {
            uid,
            freeze_amount: amount,
            released: AtomicBool::new(false),
            state,
        }
    }
}

impl FreezeGuard for RequestFreeze {
    fn kind(&self) -> FreezeKind {
        FreezeKind::Request
    }

    fn freeze_amount(&self) -> u64 {
        self.freeze_amount
    }

    fn release(&self) {
        if self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.state
            .frozen_micro
            .fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
    }
}

impl Drop for RequestFreeze {
    fn drop(&mut self) {
        if !self.released.load(Ordering::Acquire) {
            self.state
                .frozen_micro
                .fetch_sub(self.freeze_amount as i64, Ordering::AcqRel);
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}

/// Phase 1b placeholder. The real implementation in Phase 6 calls a Lua
/// script that atomically updates `counter:user:<uid>:task_frozen_micro`,
/// the `entity:task:<id>` status field, and `tasks:leases`.
///
/// In 1b this exists only so the trait has two impls and so dependent
/// crates can name the type. Construction is restricted to `gw-task-runtime`
/// in Phase 6 — there is no public constructor here.
pub struct TaskFreeze {
    pub uid: UserId,
    pub freeze_amount: u64,
    pub released: AtomicBool,
}

impl FreezeGuard for TaskFreeze {
    fn kind(&self) -> FreezeKind {
        FreezeKind::Task
    }

    fn freeze_amount(&self) -> u64 {
        self.freeze_amount
    }

    fn release(&self) {
        // Phase 1b: no-op. Phase 6 wires the Lua-driven release path
        // (atomic settle: ZREM lease + HSET task status + HINCRBY
        // task_frozen_micro -frozen) here.
        let _ = self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire);
    }
}
```

- [ ] **Step 2: Update `crates/gw-ledger/src/lib.rs` re-exports**

Replace the existing content with:

```rust
//! gw-ledger: UsageRecord schema (ADR-Cα-1), FreezeGuard trait + impls
//! (ADR-004 + C-γ §ADR-Cγ-2), mpsc-backed usage-record flusher with Redis
//! pipeline, and detailed-log sink trait.

pub mod flusher;
pub mod freeze;
pub mod log_sink;
pub mod usage;

pub use flusher::Ledger;
pub use freeze::{FreezeGuard, FreezeKind, RequestFreeze, TaskFreeze};
pub use usage::{MarkupChainSnapshot, UsageRecord, UsageSource, UsageStatus};
```

(`MarkupChainSnapshot` re-export is for callers who want one import path; the type lives in `gw-core`.)

- [ ] **Step 3: Write the failing freeze test**

Replace `crates/gw-ledger/tests/freeze.rs` with:

```rust
//! RequestFreeze RAII semantics — release-only (C-γ §ADR-Cγ-2).
//!
//! Critically: `release` does NOT touch `deducted_micro`. Go is the sole
//! writer of `deducted_micro` (via the `usage:records` consumer + an
//! `events:counter` echo back to Rust, wired in Phase 5).

use gw_core::ids::UserId;
use gw_ledger::{FreezeGuard, RequestFreeze};
use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;

#[test]
fn freeze_then_release_zeroes_frozen() {
    let uid = UserId::from("u-freeze-release");
    let g = RequestFreeze::new(uid.clone(), 1000);
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 1000);
    g.release();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    // Critical invariant: Rust never writes deducted_micro.
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn drop_without_release_rolls_back_frozen() {
    let uid = UserId::from("u-freeze-drop");
    {
        let _g = RequestFreeze::new(uid.clone(), 500);
    }
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn double_release_is_idempotent() {
    let uid = UserId::from("u-freeze-double-release");
    let g = RequestFreeze::new(uid.clone(), 1000);
    g.release();
    g.release(); // no-op
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn release_then_drop_does_not_double_rollback() {
    let uid = UserId::from("u-freeze-release-then-drop");
    {
        let g = RequestFreeze::new(uid.clone(), 800);
        g.release();
    }
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}

#[test]
fn trait_object_release_dispatches() {
    let uid = UserId::from("u-freeze-trait-obj");
    let g: Box<dyn FreezeGuard> = Box::new(RequestFreeze::new(uid.clone(), 100));
    assert_eq!(g.freeze_amount(), 100);
    g.release();
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
}
```

- [ ] **Step 4: Create the usage.rs stub so lib.rs resolves**

`crates/gw-ledger/src/lib.rs` (rewritten in Step 2) declares `pub mod usage;`, but Task 4 owns that file. To keep the chain compiling between Task 3 and Task 4, create a placeholder `crates/gw-ledger/src/usage.rs` with the single line:

```rust
// Phase 1b Task 3: placeholder. Task 4 replaces this with UsageRecord.
```

Task 4 Step 1 overwrites this stub with the real content.

- [ ] **Step 5: Run the gw-ledger tests**

```bash
cargo test -p gw-ledger --lib 2>&1 | tail -20
cargo test -p gw-ledger --test freeze 2>&1 | tail -20
```

Expected: lib compiles cleanly (no items in the stubbed `usage` module yet); 5 freeze tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-ledger/src/freeze.rs crates/gw-ledger/src/lib.rs \
        crates/gw-ledger/src/usage.rs crates/gw-ledger/tests/freeze.rs
git commit -m "refactor(gw-ledger): FreezeGuard trait + RequestFreeze (release-only)

Drops settle(actual) per C-γ §ADR-Cγ-2: Rust no longer writes deducted_micro.
RequestFreeze releases the in-process frozen counter only. Idempotent
release + Drop rollback semantics preserved from the old concrete struct.

TaskFreeze is a Phase 1b placeholder — its real Lua-driven release path
lands in Phase 6 (gw-task-runtime).

usage.rs is a stub at this commit; Task 4 fills it with UsageRecord."
```

---

## Task 4: `gw-ledger::usage::UsageRecord`

**Files:**

- Create (replacing the Task 3 stub): `crates/gw-ledger/src/usage.rs`
- Delete: `crates/gw-ledger/src/billing.rs`
- Modify: `crates/gw-ledger/Cargo.toml` — no change needed (no new external deps; `MarkupChainSnapshot` comes from gw-core which is already a dep).

- [ ] **Step 1: Replace `crates/gw-ledger/src/usage.rs`** with:

```rust
//! UsageRecord — emitted by `gw-proxy` at request completion and consumed by
//! the ledger flusher to XADD `usage:records` (ADR-Cα-1 + ADR-Cα-3).
//!
//! Cost computation is NOT in Rust. The Go side reads `usage:records` and
//! produces the final cost record on `settled_billing`. Rust ships the
//! 6-dim UsageSnapshot, the 5-field MarkupChainSnapshot, the freeze
//! amount it actually reserved, and the EstimatePricing version it used.

use gw_core::ids::{ApiKeyId, KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use serde::Serialize;

#[derive(Debug, Serialize, Clone)]
pub struct UsageRecord {
    // --- Identity / trace ---
    pub ts_ms: u64,
    pub request_id: String,
    pub trace_id: String,

    // --- Subjects ---
    pub user_id: UserId,
    pub child_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,

    // --- Protocol path ---
    pub client_protocol: String,
    pub upstream_protocol: String,
    pub model: ModelName,
    pub vendor_id: VendorId,
    pub vendor_key_id: KeyId,

    // --- Result ---
    pub status: UsageStatus,
    pub error_code: Option<String>,
    pub http_status: u16,

    // --- Usage (6-dim) + source ---
    pub usage: UsageSnapshot,
    pub usage_source: UsageSource,

    // --- Timing ---
    pub retry_count: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub is_stream: bool,

    // --- Freeze snapshot (for Go reconciliation) ---
    pub freeze_amount_micro: u64,
    /// EstimatePricing version Rust used at freeze time. Phase 1b writes 0
    /// (placeholder); Phase 5 (C-β) sources the real version from the
    /// `entity:estimate_pricing` snapshot.
    pub estimate_pricing_version: u64,

    // --- Markup snapshot (Go re-computes cost) ---
    pub markup_chain_snapshot: MarkupChainSnapshot,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UsageStatus {
    Ok,
    #[serde(rename = "err")]
    Error,
    Cancelled,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    Upstream,
    Estimated,
    FallbackConservative,
    Partial,
}

// Re-export so callers can write `gw_ledger::usage::MarkupChainSnapshot`
// without reaching into gw-core.
pub use gw_core::permissions::MarkupChainSnapshot;
```

- [ ] **Step 2: Delete `crates/gw-ledger/src/billing.rs`**

```bash
git rm crates/gw-ledger/src/billing.rs
```

- [ ] **Step 3: Write a roundtrip test**

Append to `crates/gw-ledger/src/usage.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::permissions::MarkupChainSnapshot;

    #[test]
    fn usage_record_json_shape() {
        let rec = UsageRecord {
            ts_ms: 1700000000000,
            request_id: "req-1".into(),
            trace_id: "tr-1".into(),
            user_id: UserId::from("u1"),
            child_id: None,
            api_key_id: None,
            client_protocol: "openai".into(),
            upstream_protocol: "openai".into(),
            model: ModelName::from("gpt-4"),
            vendor_id: VendorId::from("v1"),
            vendor_key_id: KeyId::from("k1"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: UsageSnapshot {
                input_tokens: 100,
                output_tokens: 50,
                ..Default::default()
            },
            usage_source: UsageSource::Upstream,
            retry_count: 0,
            latency_ms: 250,
            ttft_ms: Some(80),
            is_stream: true,
            freeze_amount_micro: 1500,
            estimate_pricing_version: 0,
            markup_chain_snapshot: MarkupChainSnapshot::placeholder(),
        };
        let s = serde_json::to_string(&rec).unwrap();
        // Smoke: critical fields appear, cost_micro is GONE.
        assert!(s.contains("\"usage\""));
        assert!(s.contains("\"input_tokens\":100"));
        assert!(s.contains("\"markup_chain_snapshot\""));
        assert!(s.contains("\"estimate_pricing_version\":0"));
        assert!(!s.contains("cost_micro"), "Rust must not emit cost_micro");
    }
}
```

- [ ] **Step 4: Add `serde_json` to `gw-ledger` dev-deps if missing**

Check `crates/gw-ledger/Cargo.toml`. The current file already has `serde_json.workspace = true` under `[dependencies]`; no `[dev-dependencies]` entry needed (the test in the lib uses it from the main dep).

- [ ] **Step 5: Run the new lib test**

```bash
cargo test -p gw-ledger --lib usage::tests 2>&1 | tail -10
```

Expected: 1 test passes.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-ledger/src/usage.rs
git commit -m "feat(gw-ledger): UsageRecord (replaces BillingRecord)

Drops cost_micro per integration spec §2 表 A A4 — Rust no longer
computes cost. Adds UsageSnapshot (6-dim) and MarkupChainSnapshot
fields. Adds usage_source variant Partial (per C-α §ADR-Cα-4).
Reserves estimate_pricing_version u64 for Phase 5 (C-β).

Old gw-ledger::billing module deleted; serialized payload no longer
carries 'cost_micro' (asserted)."
```

---

## Task 5: `gw-ledger::flusher` — `usage:records` only, no balance INCR

**Files:**

- Modify: `crates/gw-ledger/src/flusher.rs`
- Modify: `crates/gw-ledger/tests/flusher.rs`

- [ ] **Step 1: Replace `crates/gw-ledger/src/flusher.rs`** with:

```rust
//! Usage flusher — single background task that batches `UsageRecord`s from
//! an mpsc channel and writes them to Redis in a single pipelined `XADD`
//! batch.
//!
//! Per integration spec §2 表 A A6 + C-γ §ADR-Cγ-6: the flusher only
//! appends to `usage:records`. Balance decrement is Go's job (it consumes
//! `usage:records`, computes cost, writes `events:counter`). Rust no
//! longer touches `counter:user:*:balance_micro` here.
//!
//! Batching cap is set by `batch_size`; an interval timer flushes any
//! partial buffer so latency stays bounded even at low QPS.

use crate::usage::UsageRecord;
use fred::interfaces::StreamsInterface;
use gw_store::redis_client::RedisCtx;
use tokio::sync::mpsc;
use tokio::time::{Duration, interval};

const USAGE_STREAM: &str = "usage:records";

pub struct Ledger {
    pub tx: mpsc::Sender<UsageRecord>,
}

impl Ledger {
    pub fn start(
        redis: RedisCtx,
        channel_capacity: usize,
        batch_size: usize,
        flush_interval: Duration,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageRecord>(channel_capacity);
        tokio::spawn(async move {
            let mut buf: Vec<UsageRecord> = Vec::with_capacity(batch_size);
            let mut ticker = interval(flush_interval);
            loop {
                tokio::select! {
                    maybe_rec = rx.recv() => {
                        match maybe_rec {
                            Some(rec) => {
                                buf.push(rec);
                                if buf.len() >= batch_size {
                                    flush(&redis, &mut buf).await;
                                }
                            }
                            None => {
                                if !buf.is_empty() { flush(&redis, &mut buf).await; }
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if !buf.is_empty() { flush(&redis, &mut buf).await; }
                    }
                }
            }
        });
        Self { tx }
    }

    /// Non-blocking enqueue. On a full channel we drop the record and bump
    /// the `gateway_usage_channel_full_total` counter — the alternative
    /// (blocking the proxy request task) would propagate Redis stalls into
    /// upstream latency.
    #[allow(clippy::result_unit_err)]
    pub fn try_send(&self, rec: UsageRecord) -> Result<(), ()> {
        self.tx.try_send(rec).map_err(|_| {
            metrics::counter!("gateway_usage_channel_full_total").increment(1);
        })
    }
}

async fn flush(redis: &RedisCtx, buf: &mut Vec<UsageRecord>) {
    let batch_len = buf.len();
    let client = redis.pool.next();
    let pipe = client.pipeline();
    for rec in buf.drain(..) {
        let payload = serde_json::to_string(&rec).unwrap_or_default();
        if let Err(e) = pipe
            .xadd::<(), _, _, _, _>(
                USAGE_STREAM,
                false,
                None::<()>,
                "*",
                ("payload", payload),
            )
            .await
        {
            tracing::warn!(target: "gw_ledger", error = %e, "usage flush: xadd queue failed");
            metrics::counter!("gateway_usage_queue_errors_total").increment(1);
        }
    }
    if let Err(e) = pipe.all::<()>().await {
        tracing::warn!(target: "gw_ledger", error = %e, batch_len, "usage flush RTT failed; records lost");
        metrics::counter!("gateway_usage_flush_errors_total").increment(1);
        metrics::counter!("gateway_usage_records_lost_total").increment(batch_len as u64);
    }
}
```

Note the removed `use fred::interfaces::KeysInterface;` import (no more `incr_by` call).

- [ ] **Step 2: Replace `crates/gw-ledger/tests/flusher.rs`** with:

```rust
//! Usage flusher integration test (testcontainers: Redis).
//!
//! Marked `#[ignore]` so default `cargo test` passes on machines without
//! Docker; run with `cargo test -p gw-ledger -- --ignored`.

use fred::interfaces::StreamsInterface;
use gw_core::ids::{ApiKeyId, KeyId, ModelName, UserId, VendorId};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
use gw_ledger::usage::{UsageRecord, UsageSource, UsageStatus};
use gw_ledger::Ledger;
use gw_store::redis_client::RedisCtx;
use std::time::Duration;
use testcontainers_modules::redis::Redis;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn batch_flush_writes_usage_stream_no_balance_decr() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let ledger = Ledger::start(redis.clone(), 1024, 2, Duration::from_millis(100));
    let rec = UsageRecord {
        ts_ms: 0,
        request_id: "r1".into(),
        trace_id: "t".into(),
        user_id: UserId::from("u1"),
        child_id: None,
        api_key_id: None,
        client_protocol: "openai".into(),
        upstream_protocol: "openai".into(),
        model: ModelName::from("gpt-4"),
        vendor_id: VendorId::from("v1"),
        vendor_key_id: KeyId::from("k1"),
        status: UsageStatus::Ok,
        error_code: None,
        http_status: 200,
        usage: UsageSnapshot {
            input_tokens: 100,
            output_tokens: 50,
            ..Default::default()
        },
        usage_source: UsageSource::Upstream,
        retry_count: 0,
        latency_ms: 100,
        ttft_ms: None,
        is_stream: false,
        freeze_amount_micro: 1500,
        estimate_pricing_version: 0,
        markup_chain_snapshot: MarkupChainSnapshot::placeholder(),
    };
    ledger.try_send(rec.clone()).unwrap();
    ledger.try_send(rec).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let client = redis.pool.next();
    // (1) Stream xlen reflects both records.
    let len: i64 = client.xlen("usage:records").await.unwrap();
    assert_eq!(len, 2);
    // (2) Critical invariant: Rust must not have touched balance_micro.
    use fred::interfaces::KeysInterface;
    let bal: Option<i64> = client
        .get("counter:user:u1:balance_micro")
        .await
        .unwrap();
    assert!(bal.is_none(), "Rust must not write counter:user:*:balance_micro");
}
```

- [ ] **Step 3: Build the flusher to catch type-system holes**

```bash
cargo build -p gw-ledger 2>&1 | tail -10
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-ledger/src/flusher.rs crates/gw-ledger/tests/flusher.rs
git commit -m "refactor(gw-ledger): flusher writes only usage:records

Per integration spec §2 表 A A5+A6 + C-γ §ADR-Cγ-6:

- Stream constant 'billing:records' -> 'usage:records'.
- Removed INCRBY counter:user:*:balance_micro path entirely. Balance
  decrement is now Go's responsibility (consume usage:records ->
  compute cost -> XADD events:counter back to Rust, wired Phase 5).
- mpsc channel carries UsageRecord, not BillingRecord.
- Metric renames: gateway_billing_* -> gateway_usage_*.

Integration test asserts (1) xlen('usage:records') matches the send
count and (2) counter:user:u1:balance_micro is never written by Rust."
```

---

## Task 6: gw-proxy migration — `ctx.rs` + `filters/request.rs` + `filters/logging.rs`

**Files:**

- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/request.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Modify: `crates/gw-proxy/src/filters/mod.rs` (doc comment)
- Modify: `crates/gw-proxy/src/detailed_log.rs` (doc comment)

- [ ] **Step 1: Update `crates/gw-proxy/src/ctx.rs`**

Three changes in this file:

(a) The doc comment at line 9 reads:

```rust
//! `Drop` of the embedded [`FreezeGuard`] and [`StreamSlot`] enforces the
```

Change `[FreezeGuard]` to `[RequestFreeze]` — the embedded type is the concrete impl, not the trait.

(b) The import at line 21:

```rust
use gw_ledger::FreezeGuard;
```

Becomes:

```rust
use gw_ledger::RequestFreeze;
```

(c) The struct field at line 69:

```rust
    pub freeze_guard: Option<FreezeGuard>,
```

Becomes:

```rust
    pub freeze_guard: Option<RequestFreeze>,
```

(The field name `freeze_guard` is preserved — only the type changes.)

- [ ] **Step 2: Update `crates/gw-proxy/src/filters/request.rs`**

Find the freeze guard construction block (around line 391-397):

```rust
    // ---------------------------------------------------------------------
    // 10. FreezeGuard. On drop without `settle` the destructor rolls back
    //     the frozen amount.
    // ---------------------------------------------------------------------
    use gw_ledger::FreezeGuard;
    let guard = FreezeGuard::freeze(merged.effective_user_id.clone(), freeze_estimate_micro);
    ctx.freeze_guard = Some(guard);
```

Replace with:

```rust
    // ---------------------------------------------------------------------
    // 10. RequestFreeze. On drop without `release()` the destructor rolls
    //     back the frozen amount (leak-recovered metric increments).
    // ---------------------------------------------------------------------
    use gw_ledger::RequestFreeze;
    let guard = RequestFreeze::new(merged.effective_user_id.clone(), freeze_estimate_micro);
    ctx.freeze_guard = Some(guard);
```

Find the doc comment at line 228 (currently mentions `settle` semantics):

```rust
    //     would settle the freeze at `cost_micro = 0` (no usage observed)
```

Replace with:

```rust
    //     would release the freeze (Rust never computes cost; cost lives in Go)
```

Find the `freeze_estimate` function call at line 385:

```rust
    let freeze_estimate_micro = freeze_estimate(est_input_tokens, &merged.markup_product);
```

Replace with:

```rust
    let freeze_estimate_micro = freeze_estimate(est_input_tokens, merged.markup_chain.product);
```

And the `freeze_estimate` function definition (line 406-410):

```rust
fn freeze_estimate(est_input_tokens: u32, markup: &f32) -> u64 {
    let base = est_input_tokens as u64;
    let with_markup = (base as f32 * markup.max(1.0)) as u64;
    with_markup.max(1)
}
```

Replace with:

```rust
fn freeze_estimate(est_input_tokens: u32, markup: f32) -> u64 {
    let base = est_input_tokens as u64;
    let with_markup = (base as f32 * markup.max(1.0)) as u64;
    with_markup.max(1)
}
```

(Signature changes from `&f32` to `f32` because we're passing the snapshot's scalar `product` by value, which is cheaper than borrowing.)

- [ ] **Step 3: Update `crates/gw-proxy/src/filters/logging.rs`**

This is the biggest single change. Replace the file's imports and `run` body's cost/billing section.

Replace the top-of-file imports block:

```rust
use gw_ledger::billing::{BillingRecord, BillingStatus, UsageSource};
use gw_ledger::Ledger;
```

With:

```rust
use gw_ledger::usage::{UsageRecord, UsageSource, UsageStatus};
use gw_ledger::{FreezeGuard, Ledger};
use gw_core::permissions::MarkupChainSnapshot;
use gw_core::usage::UsageSnapshot;
```

Then find the cost computation block (lines 87-108 in the current file):

```rust
    // ---------------------------------------------------------------------
    // Compute the real cost (micro) using ctx.usage + (optional) pricing.
    // The freeze guard, if present, is settled with this number. If there's
    // no pricing we charge nothing — gw-proxy defers conservative-fallback
    // accounting to gw-ledger's flusher.
    // ---------------------------------------------------------------------
    let cost_micro = if let Some(p) = ctx.pricing.as_ref() {
        let in_cost = ctx.usage.input_tokens as u64 * p.value.input_price_micro_per_token;
        let out_cost = ctx.usage.output_tokens as u64 * p.value.output_price_micro_per_token;
        let raw = in_cost + out_cost;
        let markup = ctx
            .merged_perm
            .as_ref()
            .map(|m| m.markup_product.max(1.0))
            .unwrap_or(1.0);
        ((raw as f32) * markup) as u64
    } else {
        0
    };
    if let Some(g) = ctx.freeze_guard.as_ref() {
        g.settle(cost_micro);
    }
```

Replace with:

```rust
    // ---------------------------------------------------------------------
    // Release the freeze (release-only per C-γ §ADR-Cγ-2 — Rust no longer
    // computes cost; Go consumes usage:records and computes cost). Drop
    // would do the same if we forgot, but we release explicitly here so
    // the leak-metric stays clean.
    // ---------------------------------------------------------------------
    if let Some(g) = ctx.freeze_guard.as_ref() {
        g.release();
    }
```

Find the `let rec = BillingRecord { ... }` construction block (lines 156-212) and replace `BillingRecord` with `UsageRecord`, the field set with the new shape:

```rust
        let rec = UsageRecord {
            ts_ms: now_ms,
            request_id: ctx.request_id.0.clone(),
            trace_id: ctx.trace_id.0.clone(),
            user_id: effective.clone(),
            child_id: if is_subaccount {
                Some(auth_user_id.clone())
            } else {
                None
            },
            api_key_id: ctx.api_key_id.clone(),
            client_protocol: ctx
                .client_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned())
                .unwrap_or_default(),
            upstream_protocol: ctx
                .upstream_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned())
                .unwrap_or_default(),
            model: model.clone(),
            vendor_id: picked.vendor_id.clone(),
            vendor_key_id: picked.key_id.clone(),
            status: if ok {
                UsageStatus::Ok
            } else {
                UsageStatus::Error
            },
            error_code: err.map(|e| format!("{:?}", e.etype)),
            http_status: ctx.upstream_status.unwrap_or(0),
            usage: UsageSnapshot {
                input_tokens: ctx.usage.input_tokens,
                output_tokens: ctx.usage.output_tokens,
                cache_read_input_tokens: ctx.usage.cached_tokens,
                // Phase 2 (codec S4 extract_usage_any) populates the
                // remaining three fields directly from upstream wire.
                cache_creation_input_tokens: None,
                thinking_tokens: None,
                multimodal_input_tokens: None,
            },
            usage_source: if ctx.usage.output_tokens > 0 {
                UsageSource::Upstream
            } else {
                UsageSource::Estimated
            },
            retry_count: ctx.retry_count,
            latency_ms,
            ttft_ms,
            is_stream: ctx.is_stream,
            freeze_amount_micro: ctx
                .freeze_guard
                .as_ref()
                .map(|g| g.freeze_amount)
                .unwrap_or(0),
            // Phase 5 (C-β) sources real version from entity:estimate_pricing.
            estimate_pricing_version: 0,
            markup_chain_snapshot: ctx
                .merged_perm
                .as_ref()
                .map(|m| m.markup_chain)
                .unwrap_or_else(MarkupChainSnapshot::placeholder),
        };
```

Note the `ctx.freeze_guard.as_ref().map(|g| g.freeze_amount)` — `RequestFreeze` exposes `freeze_amount` as a `pub u64`. If you opt to keep accessor discipline via the trait, change to `g.freeze_amount()` (which the trait provides). Either works; the direct field access is simpler.

Also update the `// BillingRecord/TPM emit path...` comment at line 72 to read `// UsageRecord/TPM emit path...`. Similarly the `// BillingRecord -> ledger.try_send.` comment at line 121 becomes `// UsageRecord -> ledger.try_send.`.

- [ ] **Step 4: Update remaining doc comments**

In `crates/gw-proxy/src/filters/mod.rs` line 11:

```rust
//! - [`logging`] — settle freeze, record breaker outcome, emit BillingRecord
```

Replace `settle freeze` → `release freeze` and `BillingRecord` → `UsageRecord`.

In `crates/gw-proxy/src/detailed_log.rs` line 2:

```rust
//! the (cheaper) `BillingRecord`. v1 captures the structured fields the spec
```

Replace `BillingRecord` → `UsageRecord`.

- [ ] **Step 5: Build gw-proxy**

```bash
cargo build -p gw-proxy 2>&1 | tail -10
```

Expected: clean build. If there are warnings about unused imports (e.g., the old `BillingRecord` / `BillingStatus` use lines), remove them.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/filters/request.rs \
        crates/gw-proxy/src/filters/logging.rs crates/gw-proxy/src/filters/mod.rs \
        crates/gw-proxy/src/detailed_log.rs
git commit -m "refactor(gw-proxy): release freeze + emit UsageRecord

Removes cost computation entirely from gw-proxy/filters/logging.rs
(per C-γ §ADR-Cγ-6) — the freeze guard is now released, never settled
with an actual cost. Cost calculation belongs to Go.

UsageRecord carries the 6-dim UsageSnapshot (Phase 2 codec S4 fills
the remaining 3 dims), MarkupChainSnapshot (Phase 5 plumbs real values),
and a placeholder estimate_pricing_version=0 (Phase 5 C-β supplies real).

ctx.freeze_guard typed Option<RequestFreeze>; request.rs uses
RequestFreeze::new; freeze_estimate now takes markup as scalar f32
(read from merged.markup_chain.product)."
```

---

## Task 7: Update remaining tests (gw-proxy + gw-gateway integration)

**Files:**

- Modify: `crates/gw-proxy/tests/freeze_lifecycle.rs`
- Modify: `crates/gw-proxy/tests/end_to_end_openai.rs` (doc comment only)
- Modify: `crates/gw-gateway/tests/e2e_failure_modes.rs`
- Modify: `crates/gw-gateway/tests/e2e_full_flow.rs` (doc comments only)
- Modify: `crates/gw-gateway/src/lib.rs` (doc comments only — references to BillingRecord/Ledger flusher mention)

- [ ] **Step 1: Replace `crates/gw-proxy/tests/freeze_lifecycle.rs`** with:

```rust
//! RequestFreeze lifecycle integration tests (gw-proxy view).
//!
//! Mirrors gw-ledger's own tests but exercised through the proxy's local
//! re-export so any future refactor of `RequestFreeze` is caught here too.
//!
//! Critical invariant (C-γ §ADR-Cγ-1): release MUST NOT touch
//! deducted_micro. Go is the sole writer of that field via the
//! usage:records -> cost -> events:counter -> Rust loop (Phase 5).

use gw_core::ids::UserId;
use gw_ledger::{FreezeGuard, RequestFreeze};
use gw_store::deduction::DEDUCT;
use std::sync::atomic::Ordering;

fn deducted(uid: &UserId) -> i64 {
    DEDUCT
        .get(uid)
        .map(|e| e.value().deducted_micro.load(Ordering::Acquire))
        .unwrap_or(0)
}

fn frozen(uid: &UserId) -> i64 {
    DEDUCT
        .get(uid)
        .map(|e| e.value().frozen_micro.load(Ordering::Acquire))
        .unwrap_or(0)
}

#[test]
fn release_zeros_frozen_and_leaves_deducted_alone() {
    let uid = UserId::from("freeze_proxy_release");
    let before_frozen = frozen(&uid);
    let before_deducted = deducted(&uid);

    let guard = RequestFreeze::new(uid.clone(), 1_000);
    assert_eq!(frozen(&uid), before_frozen + 1_000);

    guard.release();
    assert_eq!(frozen(&uid), before_frozen, "release must remove the freeze");
    assert_eq!(
        deducted(&uid),
        before_deducted,
        "Rust never writes deducted_micro"
    );
}

#[test]
fn drop_without_release_rolls_back_frozen() {
    let uid = UserId::from("freeze_proxy_drop");
    let before_frozen = frozen(&uid);

    {
        let _guard = RequestFreeze::new(uid.clone(), 500);
        assert_eq!(frozen(&uid), before_frozen + 500);
    }
    assert_eq!(frozen(&uid), before_frozen);
}

#[test]
fn double_release_is_idempotent() {
    let uid = UserId::from("freeze_proxy_double");
    let before_deducted = deducted(&uid);

    let guard = RequestFreeze::new(uid.clone(), 100);
    guard.release();
    guard.release();
    assert_eq!(deducted(&uid), before_deducted);
}
```

- [ ] **Step 2: Update `crates/gw-proxy/tests/end_to_end_openai.rs` line 11 comment**

Find:

```rust
//!     (DeductionState.deducted_micro, BillingRecord emission).
```

Replace with:

```rust
//!     (DeductionState.frozen_micro, UsageRecord emission).
```

- [ ] **Step 3: Replace the relevant tests in `crates/gw-gateway/tests/e2e_failure_modes.rs`**

Replace the import line at line 32:

```rust
use gw_ledger::FreezeGuard;
```

With:

```rust
use gw_core::permissions::MarkupChainSnapshot;
use gw_ledger::{FreezeGuard, RequestFreeze};
```

(`FreezeGuard` stays — it's used by the trait-method test variant below. If the implementer chooses not to add that variant, drop `FreezeGuard` from the import to keep clippy happy.)

Find the test `freeze_guard_double_settle_idempotent` (lines 87-96) and rename/rewrite:

```rust
// ---------------------------------------------------------------------------
// C-γ §ADR-Cγ-2: RequestFreeze release is idempotent and never touches
// deducted_micro.
// ---------------------------------------------------------------------------
#[test]
fn freeze_guard_double_release_idempotent_no_deducted_write() {
    let uid = UserId::from("u-failmodes-double-release");
    let g = RequestFreeze::new(uid.clone(), 1000);
    g.release();
    g.release(); // no-op
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}
```

Find the test `freeze_guard_drop_without_settle_rolls_back` (lines 98-107) and rewrite:

```rust
#[test]
fn freeze_guard_drop_without_release_rolls_back() {
    let uid = UserId::from("u-failmodes-leak");
    {
        let _g = RequestFreeze::new(uid.clone(), 500);
    } // dropped without release
    let state = DEDUCT.get(&uid).unwrap().clone();
    assert_eq!(state.frozen_micro.load(Ordering::Relaxed), 0);
    assert_eq!(state.deducted_micro.load(Ordering::Relaxed), 0);
}
```

Find the `MergedPermissions { ..., markup_product: 1.0, }` literal in `rate_limit_guard_emits_429_when_over_budget` (line 187):

```rust
        markup_product: 1.0,
```

Replace with:

```rust
        markup_chain: MarkupChainSnapshot::placeholder(),
```

(`MarkupChainSnapshot` was added to the imports in this step's first sub-edit.)

- [ ] **Step 4: Update `crates/gw-gateway/tests/e2e_full_flow.rs` doc comment line 138**

Find:

```rust
    //       - billing:records stream has 1 entry (XLEN == 1).
```

Replace with:

```rust
    //       - usage:records stream has 1 entry (XLEN == 1).
```

If the test body actually does `client.xlen("billing:records")`, update that too (search for it). If the test is `#[ignore]`d (it is — full Pingora harness), the change still needs to land for when the test is enabled.

- [ ] **Step 5: Update `crates/gw-gateway/src/lib.rs` doc comments**

Find references to `BillingRecord` / "ledger" in doc comments and update to `UsageRecord`. Specifically:

- Line 18 / 23 doc comments mention "Ledger flusher" — leave the name `Ledger` (the type didn't rename), only update references to the **payload** (`BillingRecord` → `UsageRecord`).

There are no Rust `BillingRecord` references in `lib.rs` body itself — only doc comments may need attention. If `grep -n 'BillingRecord' crates/gw-gateway/src/lib.rs` returns zero, this step is a no-op.

- [ ] **Step 6: Run the full workspace test suite**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: all tests green. **This is the first point where the chain commits since Task 2 must form a green workspace.**

- [ ] **Step 7: Run clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
```

Expected: no warnings (CI gate). Common issues to clean up: dead imports of `BillingStatus` / `BillingRecord` if they slipped through; the `_g = ` patterns I renamed should still trigger `unused_variables`-suppressed lints because the `_` prefix is preserved.

- [ ] **Step 8: Commit**

```bash
git add crates/gw-proxy/tests/freeze_lifecycle.rs \
        crates/gw-proxy/tests/end_to_end_openai.rs \
        crates/gw-gateway/tests/e2e_failure_modes.rs \
        crates/gw-gateway/tests/e2e_full_flow.rs \
        crates/gw-gateway/src/lib.rs
git commit -m "test: migrate freeze/billing tests to release+UsageRecord

- gw-proxy/tests/freeze_lifecycle: settle -> release; drop deducted_micro
  assertions (Go is sole writer per C-γ §ADR-Cγ-1).
- gw-gateway/tests/e2e_failure_modes: same; markup_product literal ->
  MarkupChainSnapshot::placeholder().
- gw-gateway/tests/e2e_full_flow: billing:records -> usage:records.
- Doc comment cleanups.

cargo test --workspace and cargo clippy both pass with no warnings."
```

---

## Task 8: Activate lints + CLAUDE.md filter-chain row + plan residual sweep

**Files:**

- Modify: `scripts/check-invariants.sh` — uncomment 4 PENDING blocks.
- Modify: `CLAUDE.md` — update "Architecture: filter chain" table row for `logging`.
- Modify: `docs/superpowers/plans/2026-05-19-business-split-refactor.md` — strip any residual `BillingRecord` / `cost_micro` / `settle(actual)` references (Bm6 cleanup).

- [ ] **Step 1: Activate four PENDING lints in `scripts/check-invariants.sh`**

The four blocks currently sit in the PENDING section (lines 100-110 of the current file). Move each to the active section and adjust the comment. Specifically:

Move these blocks **out of the comment block** (uncomment them) and **place them in the active section** (above line 87 — before the `===== PENDING lints ====` separator):

```bash
# ---------------------------------------------------------------------------
# no_cost_micro (§5.2) — Rust no longer computes cost.
# ---------------------------------------------------------------------------
echo "[no_cost_micro] no cost_micro field in workspace"
if grep -rnE '\bcost_micro\b' crates/ 2>/dev/null; then
  bad "cost_micro field remains"
else
  ok "no cost_micro field"
fi

# ---------------------------------------------------------------------------
# no_billing_records_stream (§5.2) — stream renamed to usage:records.
# ---------------------------------------------------------------------------
echo "[no_billing_records_stream] billing:records stream name must not appear"
if grep -rn '"billing:records"' crates/ 2>/dev/null; then
  bad "billing:records stream name remains"
else
  ok "no billing:records reference"
fi

# ---------------------------------------------------------------------------
# no_freezeguard_settle (§5.2) — FreezeGuard::release replaces settle.
# ---------------------------------------------------------------------------
echo "[no_freezeguard_settle] FreezeGuard::settle must be absent"
if grep -rnE '\.settle\(.*actual|FreezeGuard::settle\(' crates/ 2>/dev/null; then
  bad "FreezeGuard::settle remains"
else
  ok "no FreezeGuard::settle reference"
fi

# ---------------------------------------------------------------------------
# no_markup_product_scalar (§5.2) — MergedPermissions field is markup_chain.
# ---------------------------------------------------------------------------
echo "[no_markup_product_scalar] markup_product scalar must be absent"
if grep -rnE '\bmarkup_product\b' crates/ 2>/dev/null; then
  bad "markup_product scalar remains"
else
  ok "no markup_product reference"
fi
```

Delete the corresponding PENDING blocks (the lines starting with `# no_cost_micro (Phase 1b: ...)` through `# no_markup_product_scalar (...)`) — they're no longer pending.

- [ ] **Step 2: Run the lint harness to confirm green**

```bash
bash scripts/check-invariants.sh
```

Expected: all 8 active invariants now report `ok`. Exit 0. If any reports `FAIL`, that's a real leftover — fix it in the source before continuing.

- [ ] **Step 3: Update CLAUDE.md filter-chain table row for `logging`**

In `CLAUDE.md`, find the `## Architecture: filter chain` section's row for `logging`:

```
| `logging`                   | `filters::logging`      | settle freeze (ADR-004), record breaker outcome, emit `BillingRecord` + `DetailedLog` |
```

Replace with:

```
| `logging`                   | `filters::logging`      | release freeze (C-γ §ADR-Cγ-2), record breaker outcome, emit `UsageRecord` + `DetailedLog` |
```

- [ ] **Step 4: Sweep `business-split-refactor.md` for any remaining residuals**

```bash
grep -nE 'BillingRecord|cost_micro|settle\(actual|gw-billing|patch_body|markup_product' docs/superpowers/plans/2026-05-19-business-split-refactor.md
```

If any matches appear in **non-historical context** (e.g., describing current architecture rather than narrating a phase-out), edit them to use the new names. If matches are inside a "v1 残留" block already crossed out by Phase 1a's `14bde82` commit, leave them — they document the deletion.

Use judgment: matches that read "delete `BillingRecord`" or "撤销 `patch_body`" are *describing the change* and should stay. Matches that read "extend `BillingRecord` with field X" describe new work on a dead name and must be rewritten.

If nothing needs changing, this step is a no-op.

- [ ] **Step 5: Final verify**

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo fmt --all -- --check 2>&1 | tail -5
bash scripts/check-invariants.sh
```

All four must exit 0.

- [ ] **Step 6: Commit**

```bash
git add scripts/check-invariants.sh CLAUDE.md \
        docs/superpowers/plans/2026-05-19-business-split-refactor.md
git commit -m "ci+docs: activate 4 Phase 1b lints; update filter-chain doc

- scripts/check-invariants.sh: move no_cost_micro, no_billing_records_stream,
  no_freezeguard_settle, no_markup_product_scalar from PENDING to active.
  Harness now enforces 8 invariants.
- CLAUDE.md: 'Architecture: filter chain' row for logging updated to
  'release freeze (C-γ §ADR-Cγ-2), emit UsageRecord + DetailedLog'.
- business-split-refactor.md: residual sweep (Bm6 follow-up)."
```

---

## Self-review checklist (run after Task 8 commits)

After the final commit, run the harness one more time and skim each spec section against the plan:

| # | Spec / lint | Plan task | Verified? |
|---|---|---|---|
| §2 A3 `settle(actual)` removed | Task 3 + lint `no_freezeguard_settle` | grep returns 0; `cargo test --workspace` green |
| §2 A4 `BillingRecord` → `UsageRecord` + drop `cost_micro` | Task 4 + lint `no_cost_micro` | grep returns 0 |
| §2 A5 stream `billing:records` → `usage:records` | Task 5 + lint `no_billing_records_stream` | grep returns 0 |
| §2 A6 flusher `INCRBY balance_micro` route removed | Task 5 | grep `incr_by.*balance_micro` returns 0 in `gw-ledger/src/` |
| §2 A12 `deducted_micro.fetch_add` Rust write removed | Task 3 | grep `deducted_micro.fetch_add` returns 0 |
| §2 A13 `markup_product` scalar → `markup_chain` | Task 2 + lint `no_markup_product_scalar` | grep returns 0 |
| §3 C-7 `UsageSnapshot` lives in `gw-core::usage` | Task 1 | `cargo doc -p gw-core --no-deps` shows it |
| §3 C-7 `MarkupChainSnapshot` resolves as `gw-auth::merge::MarkupChainSnapshot` | Task 2 (`pub use` re-export) | `cargo check` passes import |
| Phase 1 exit gate: `cargo test --workspace` green | Task 7 | confirmed |
| Phase 1 exit gate: CI lints pass | Task 8 | `bash scripts/check-invariants.sh` exit 0 |
| CLAUDE.md filter chain mentions UsageRecord | Task 8 | grep |

If any row is unchecked, fix in a follow-up commit before declaring Phase 1b complete.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-20-phase-1b-ledger-freeze.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
