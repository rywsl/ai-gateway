# gw-ledger — Design-Only Gate

**Date**: 2026-06-12
**Status**: Awaiting user decision
**Parent spec**: `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 3
**Purpose**: Pin down design choices before implementer dispatch. Once user approves the picks below, a Crate 3 plan is written and the implementer (opus) executes.

---

## Pre-Implementation Findings (changes scope from original spec)

After reading `crates/gw-ledger/src/{lease,coef,backlog}.rs` and `AILinkMax-Go/modules/billing/business/outstanding.go`:

### Finding 1 — `LeaseGuard` already HAS a `Drop` impl

Original audit said "no Drop". In fact `lease.rs:179-183` has:

```rust
impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}
```

But `release()` (line 172-176) is purely a marker flip on `AtomicBool`. **Nothing in the codebase reads the `released` flag**, so it's effectively a no-op. The actual bug — credits not refunded on panic — is real; we just need to make Drop **actually refund** instead of inventing a new Drop.

### Finding 2 — `BillingBacklog` already emits both gauges

Original spec asked for `gw_billing_backlog_size_bytes` gauge. The crate already emits `gw_billing_backlog_pending_bytes` (line 75) AND `gw_billing_backlog_pending_count` (line 74). **This work item is essentially already done**. We just need to decide whether to add an "alarm" metric on top.

### Finding 3 — `COEF_STAGING` current logic handles 2 of 3 orphan classes

`apply_coef_commit` already handles:
- **Stale** (line 113-119): version ≤ active_version → removed.
- **Incomplete** (line 127-133): staged < expected → put back.

What it does NOT handle:
- **Orphan**: entries inserted by `apply_coef_entry(version, …)` where the commit event never arrives. These accumulate forever. This is the real leak.

### Finding 4 — Go side has a complete OutstandingLedger

`AILinkMax-Go/modules/billing/business/outstanding.go` already has `ApplyConsumed` / `ApplyLeaseEvent` with explicit `Orphan` / `Clamp` / `VersionMismatch` metric hooks AND a `outstandingTTLMult × leaseTTL` (15 min default) sliding TTL on the Redis hash. **If the Rust side refunds on panic, the Go-side outstanding entry remains until TTL expires, which is acceptable and already implemented.** The Go side likely doesn't need code changes — only contract-change documentation.

---

## Decision 1 — `LeaseGuard::Drop` Refund Policy

Three options. Pick one.

### Option A — Refund all `reserved` on panic, idempotent via new `settled: AtomicBool`

```rust
pub struct LeaseGuard {
    …
    settled: AtomicBool,   // NEW
    released: AtomicBool,  // existing — purpose narrowed to "user said we're done"
    state: Arc<LeaseState>,
}

impl LeaseGuard {
    pub fn settle_actual(&self, actual: i64) {
        if self.settled.swap(true, Ordering::AcqRel) {
            return; // already settled, idempotent
        }
        let actual = actual.max(0);
        if actual < self.reserved {
            self.state.credits.fetch_add(self.reserved - actual, Ordering::AcqRel);
        }
        // actual >= reserved: no refund, no double-charge (current behavior)
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if !self.settled.load(Ordering::Acquire) {
            // Panic / early exit: refund FULL reserved.
            self.state.credits.fetch_add(self.reserved, Ordering::AcqRel);
            metrics::counter!("gw_lease_panic_recovered_total").increment(1);
        }
    }
}
```

**Pros**:
- Mirrors `RequestFreeze::Drop` pattern (already in the codebase). Familiar.
- Single new bit of state (`AtomicBool`). Minimal complexity.
- `settle_actual` correctness preserved (over-spend still recorded via `usage:records`).
- Idempotent: multiple `settle_actual` calls do not double-refund. Multiple `Drop` is impossible (Rust enforces).
- Go side needs no code change — orphan TTL handles the residual outstanding entry.

**Cons**:
- Adds 1 byte per LeaseGuard.

**Recommended.**

### Option B — Refund only unsettled portion via tracking `actual` separately

Track `actual_settled: AtomicI64` (init = `i64::MIN` sentinel for "not settled"). Drop refunds `reserved - max(actual, 0)` if not settled.

**Pros**:
- Slightly more explicit about "settled with how much".

**Cons**:
- Wider state field, more confusing logic. The same effect can be achieved by Option A + reading metric streams downstream.
- Adds another AtomicI64 per LeaseGuard (8 bytes).
- No practical benefit over A given that settle_actual itself already does the refund math.

### Option C — No refund; emit metric and let Go reconcile

Drop emits `gw_lease_panic_leak_total` but does NOT refund. Go-side OutstandingLedger lets the orphan TTL clean up; user's lease pool in Rust stays in the depleted state until next protected_lease push from Go.

**Pros**:
- Minimum Rust-side code change (just metric).

**Cons**:
- Credits are temporarily lost in Rust memory until Go pushes a refresh — depending on Go's push cadence (event-driven, not periodic), a panicked user might be falsely rejected for minutes.
- Violates "no hidden issues" — a panicked request silently consumes credits the user has not actually used.
- Asymmetric with `RequestFreeze::Drop` (which refunds locally).

---

## Decision 2 — `LEASE_POOL` Eviction Policy

Currently never evicted. `upsert_unprotected` sets fields to zero but the `Arc<LeaseState>` entry stays.

### Option A — Sweep `Unprotected` entries past idle TTL

Add a sweep loop (use `gw-runtime::Supervisor`, similar to Crate 2's DeductGc). The loop removes entries where `mode == Unprotected` AND `lease_id is empty` AND some `last_seen_at` was longer than `LEASE_POOL_IDLE_TTL` (e.g. 30 min).

Requires adding `last_seen_at_ms: AtomicU64` to `LeaseState` (touched on `upsert_*` and on `try_acquire`).

**Pros**: Bounded eviction policy; matches Crate 2 pattern.
**Cons**: Adds 8 bytes per entry; adds a sweep loop.

**Recommended.**

### Option B — Evict on next `upsert_*` only, no sweep

When `upsert_*` is called for user U, check if any other entries can be removed based on something (e.g. count > threshold → evict LRU). Lazy eviction.

**Cons**: User churn is silent until pressure; doesn't really bound the map. Reject.

### Option C — Do not evict in Crate 3

Defer to a future iteration. Acceptable if you judge the leak rate is too low to matter (LEASE_POOL only grows by 1 per protected user; bounded by total user count, not per-request).

**Trade-off**: simpler PR, but a documented gap.

---

## Decision 3 — `COEF_STAGING` Orphan Eviction

Need to remove staged entries that will never receive a commit.

### Option A — Version-based: evict where `version < active_version - SAFETY_MARGIN`

Fold into the same sweep loop as LEASE_POOL (or DeductGc — but cross-crate). Example: if active_version is 100 and SAFETY_MARGIN is 10, anything ≤ 90 is removed.

**Pros**: Deterministic; no wall-clock dependency; matches the existing `version <= active_version` check in `apply_coef_commit` line 113.
**Cons**: If Go bursts many versions in quick succession, SAFETY_MARGIN needs to be ≥ inflight depth. Probably 10–100 is safe.

**Recommended.**

### Option B — Wall-clock TTL: evict staging entries older than N minutes

**Cons**: Wall-clock dependency. The codebase already has a workaround for system clock issues (`lease.rs:186-196`).

---

## Decision 4 — `BillingBacklog` Observability Hardening

Existing gauges (`pending_bytes`, `pending_count`, `high_watermark_count` field but unexposed). Audit asked for an upper-bound observation.

### Option A — Add `gw_billing_backlog_high_watermark` gauge

Expose existing `high_watermark_count` as a gauge in `push()`. Then add a simple `gw_billing_backlog_over_threshold_total` counter that increments when pending_count crosses a configurable threshold (e.g. 10_000 envelopes).

**Pros**: Minimal change; observable signal for the must-deliver invariant under stress.
**Cons**: Adds a configurable threshold.

**Recommended.**

### Option B — No change

The existing two gauges + Go-side alerting via Redis stream lag are sufficient.

**Trade-off**: simpler PR; defers the audit's request.

---

## Decision 5 — Go-side Change Style

Per parent spec, gw-ledger work includes Go-side companion changes. Given Findings 1+4 (Rust does its own refund; Go's OutstandingLedger already handles orphans via TTL), the Go-side scope reduces dramatically.

### Option A — Doc-only (recommended)

Write `docs/go-contracts/contract-change/2026-06-12-leaseguard-drop.md` describing:
- Rust LeaseGuard::Drop now refunds reserved credits on panic
- Emits `gw_lease_panic_recovered_total` for observability
- Go-side `OutstandingLedger`'s existing Orphan branch handles the remaining outstanding entries via TTL
- No Go code changes required

### Option B — Active orphan scan in Go

Add a periodic scan in Go that detects entries past their `expires_at_ms + grace` and proactively `HDEL`s them, instead of relying on Redis sliding TTL.

**Cons**: Duplicates work TTL already does. Not recommended unless we have evidence orphans are a real operational pain.

### Option C — Schema extension: add `panic_recovered` flag

Add a `:panic_recovered` field on the outstanding hash, set by Rust when its Drop refund fires, consumed by Go for forensic auditing.

**Cons**: Cross-process schema coupling for a low-frequency event.

---

## Summary of Recommended Picks

| Decision | Recommended |
|----------|-------------|
| 1 LeaseGuard::Drop refund policy | **A** (Drop refunds full reserved, idempotent via `settled` AtomicBool) |
| 2 LEASE_POOL eviction | **A** (sweep loop, idle-TTL-based, Supervisor-wrapped) |
| 3 COEF_STAGING orphan eviction | **A** (`version < active_version - SAFETY_MARGIN`) |
| 4 BillingBacklog observability | **A** (high_watermark gauge + over_threshold counter) |
| 5 Go-side change | **A** (doc-only; existing Go logic handles residuals) |

If you approve all 5 recommendations, the Crate 3 plan is short:
- 4 small Rust changes (LeaseGuard::Drop refactor, LEASE_POOL sweep, COEF_STAGING sweep, BillingBacklog gauges)
- 1 Go contract-change doc
- Standard docs sync
- The bounce-prone "Go code change" is removed from scope entirely

If you reject any, I'll re-plan with the substitute. Open questions / counter-proposals welcome.

---

**Awaiting decision.** Reply with one of:
- "All A" → I proceed to write the plan
- "1: B, others A" → swap that decision and proceed
- "Discuss X" → we talk before committing
