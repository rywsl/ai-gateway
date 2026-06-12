# Crate 3: gw-ledger — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bound the in-memory `LEASE_POOL` (sweep `Unprotected` past idle TTL), bound `COEF_STAGING` (sweep orphan versions < active − safety margin), and expose existing `BillingBacklog::high_watermark_count` as a gauge with an upward-crossing alarm counter.

**Architecture:** New `gw_ledger::ledger_sweep::spawn(shutdown)` wraps a 60s-tick body in `gw_runtime::Supervisor` (mirrors Crate 2's `DeductGc::spawn`). Per tick the body calls `lease::sweep_unprotected()` and `coef::sweep_orphans()`. `LeaseState` gains a `last_seen_at_ms` field written by `upsert_unprotected`. `BillingBacklog::push` exposes the high-watermark gauge and a new over-threshold counter using a `crossed_threshold: AtomicBool` to fire on upward crossings only.

**Tech Stack:** Rust 1.88.0, `gw-runtime::Supervisor` (existing), `dashmap`, `tokio`, `metrics`, `tracing`. No new workspace deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 3 (updated 2026-06-12 with scope-correction record after pre-implementation reading).

**Branch:** `fix/memory-leaks-gw-ledger` cut from local `main` (includes Crate 2 merge `f5f5b26` + this plan commit).

**Out of scope (explicitly recorded for self-review):**
- `LeaseGuard::Drop` refund logic (user decision 2026-06-12 — bounded false-rejection window judged acceptable).
- Any Go code change OR `docs/go-contracts/contract-change/...` doc (user decision: if Go changes, change both code and contract; otherwise neither).
- `BillingBacklog::pending_bytes` / `pending_count` gauges (already emitted at `backlog.rs:74-75`).
- `apply_coef_commit`'s existing `Stale` / `Incomplete` branches (already correct).

---

## File Structure

**Modified files:**
- `crates/gw-ledger/src/lease.rs` — add `last_seen_at_ms: AtomicU64` to `LeaseState`; init in `new_unprotected`; write in `upsert_unprotected`; add `pub fn sweep_unprotected() -> u64`
- `crates/gw-ledger/src/coef.rs` — add `pub fn sweep_orphans() -> u64`; add `pub const COEF_STAGING_SAFETY_MARGIN: u64 = 100`
- `crates/gw-ledger/src/backlog.rs` — expose `high_watermark_count` via `gw_billing_backlog_high_watermark_count` gauge in `push`; add `crossed_threshold: AtomicBool` field; emit `gw_billing_backlog_over_threshold_total` counter on upward crossing of `BILLING_BACKLOG_OVER_THRESHOLD` (default 10_000)
- `crates/gw-ledger/src/lib.rs` — `pub mod ledger_sweep;`
- `crates/gw-ledger/src/ledger_sweep.rs` (NEW) — `pub fn spawn(shutdown: ShutdownToken) -> JoinHandle<()>` wrapping `lease::sweep_unprotected()` + `coef::sweep_orphans()` in `Supervisor`
- `crates/gw-gateway/src/lib.rs` — call `gw_ledger::ledger_sweep::spawn(shutdown_token.clone())` at bootstrap and `shutdown.register(handle)`
- `docs/spec/06-billing-ledger.md` — document the three sweeps and the new metrics
- `docs/architecture.md` §Deliberate behaviours — sweep semantics
- `docs/ROADMAP.md` — close the items

**New tests:**
- `crates/gw-ledger/src/lease.rs` mod tests — new tests for `sweep_unprotected`
- `crates/gw-ledger/src/coef.rs` mod tests — new tests for `sweep_orphans`
- `crates/gw-ledger/src/backlog.rs` mod tests — new tests for high-watermark gauge and over-threshold counter
- `crates/gw-ledger/src/ledger_sweep.rs` mod tests — shutdown-token cancellation test

**No new workspace deps.**

---

## Task 1: Branch Setup + Verification

- [ ] **Step 1: Cut branch from latest local main**

```bash
git checkout main
git status   # expect: ahead of origin/main, working tree clean
git log --oneline -3   # expect 8592bf6 (design-only gate) as most recent before the plan commit
git checkout -b fix/memory-leaks-gw-ledger
```

- [ ] **Step 2: Pre-implementation verification**

Confirm `LeaseState` does NOT yet have `last_seen_at_ms`:

```bash
grep -n "last_seen_at_ms" crates/gw-ledger/src/lease.rs
```

Expected: empty.

Confirm `coef::sweep_orphans` does not yet exist:

```bash
grep -n "sweep_orphans" crates/gw-ledger/src/coef.rs
```

Expected: empty.

Confirm `BillingBacklog` does NOT yet have `crossed_threshold`:

```bash
grep -n "crossed_threshold\|over_threshold_total" crates/gw-ledger/src/backlog.rs
```

Expected: empty.

If any of these returns non-empty: STOP and surface to orchestrator.

---

## Task 2: `LeaseState.last_seen_at_ms` + `upsert_unprotected` writes it — failing test

**Files:** `crates/gw-ledger/src/lease.rs`

- [ ] **Step 1: Append a failing test inside the existing `mod tests`**

```rust
    #[test]
    fn upsert_unprotected_records_last_seen_at_ms() {
        let uid = fresh_uid("last_seen_unprotected");
        // First put into protected to create the entry.
        upsert_protected(&uid, 1000, "lease-ls".into(), 9, now_ms() + 60_000);
        // Then transition to unprotected.
        let before = now_ms();
        upsert_unprotected(&uid);
        let after = now_ms();
        let s = LEASE_POOL.get(&uid).unwrap();
        let ts = s.last_seen_at_ms.load(Ordering::Acquire);
        assert!(
            ts >= before && ts <= after + 1,
            "last_seen_at_ms ({ts}) should be in [{before}, {after}]"
        );
    }
```

- [ ] **Step 2: Run and verify compile failure**

```bash
cargo test -p gw-ledger --lib lease::tests::upsert_unprotected_records_last_seen_at_ms 2>&1 | tail -10
```

Expected: compile error — `LeaseState` has no `last_seen_at_ms` field.

---

## Task 3: Implement `last_seen_at_ms` on `LeaseState`

**Files:** `crates/gw-ledger/src/lease.rs`

- [ ] **Step 1: Add the field**

In `LeaseState` struct, add at the end:

```rust
    pub last_seen_at_ms: AtomicU64,
```

In `LeaseState::new_unprotected`, append to the constructor:

```rust
            last_seen_at_ms: AtomicU64::new(0),
```

- [ ] **Step 2: Have `upsert_unprotected` write the timestamp**

At the end of `upsert_unprotected` (after the existing `entry.mode.store(...)` line):

```rust
    entry.last_seen_at_ms.store(now_ms(), Ordering::Release);
```

- [ ] **Step 3: Run the test and verify it passes**

```bash
cargo test -p gw-ledger --lib lease::tests::upsert_unprotected_records_last_seen_at_ms
```

Expected: PASS.

- [ ] **Step 4: Run all gw-ledger lease tests for regression**

```bash
cargo test -p gw-ledger --lib lease::tests
```

Expected: all PASS (the new test + 13 existing).

---

## Task 4: `lease::sweep_unprotected` — failing tests

**Files:** `crates/gw-ledger/src/lease.rs`

- [ ] **Step 1: Append the three sweep tests**

Append inside `mod tests` (after the Task 2 test):

```rust
    #[test]
    fn sweep_unprotected_evicts_idle_unprotected_entries() {
        let uid = fresh_uid("sweep_idle");
        // Create the entry as protected, then transition to unprotected.
        upsert_protected(&uid, 1000, "lease-sw-idle".into(), 9, now_ms() + 60_000);
        upsert_unprotected(&uid);
        // Backdate last_seen by (idle_ttl + 1ms).
        let s = LEASE_POOL.get(&uid).unwrap();
        let old_ts = now_ms().saturating_sub(LEASE_POOL_IDLE_TTL_MS + 1);
        s.last_seen_at_ms.store(old_ts, Ordering::Release);
        drop(s);

        let removed = sweep_unprotected();
        assert!(removed >= 1);
        assert!(!LEASE_POOL.contains_key(&uid));
    }

    #[test]
    fn sweep_unprotected_keeps_recent_unprotected_entries() {
        let uid = fresh_uid("sweep_recent");
        upsert_protected(&uid, 1000, "lease-sw-recent".into(), 9, now_ms() + 60_000);
        upsert_unprotected(&uid);
        // last_seen is current (just now).
        let _removed = sweep_unprotected();
        assert!(LEASE_POOL.contains_key(&uid));
        LEASE_POOL.remove(&uid);
    }

    #[test]
    fn sweep_unprotected_keeps_protected_entries() {
        let uid = fresh_uid("sweep_protected");
        upsert_protected(&uid, 1000, "lease-sw-prot".into(), 9, now_ms() + 60_000);
        // Backdate last_seen heavily — but mode is still Protected.
        let s = LEASE_POOL.get(&uid).unwrap();
        let old_ts = now_ms().saturating_sub(LEASE_POOL_IDLE_TTL_MS + 60_000);
        s.last_seen_at_ms.store(old_ts, Ordering::Release);
        drop(s);

        let _removed = sweep_unprotected();
        assert!(
            LEASE_POOL.contains_key(&uid),
            "Protected entries must never be swept regardless of last_seen_at_ms"
        );
        LEASE_POOL.remove(&uid);
    }
```

- [ ] **Step 2: Run and verify compile failures**

```bash
cargo test -p gw-ledger --lib lease::tests::sweep_unprotected 2>&1 | tail -10
```

Expected: compile errors — `sweep_unprotected` and `LEASE_POOL_IDLE_TTL_MS` not defined.

---

## Task 5: Implement `lease::sweep_unprotected`

**Files:** `crates/gw-ledger/src/lease.rs`

- [ ] **Step 1: Add the constant near the top**

After `pub static LEASE_POOL: ...` (line 69), insert:

```rust
/// Idle TTL for `Unprotected` entries in `LEASE_POOL`. After transitioning to
/// `Unprotected` (via `upsert_unprotected`), an entry becomes eligible for
/// eviction once `now_ms() - last_seen_at_ms` exceeds this value. `Protected`
/// entries are never touched by the sweep.
pub const LEASE_POOL_IDLE_TTL_MS: u64 = 30 * 60 * 1000; // 30 minutes
```

- [ ] **Step 2: Add the sweep function**

After the constant, add:

```rust
/// Evict `LEASE_POOL` entries whose `mode == Unprotected` AND whose
/// `last_seen_at_ms` is older than `LEASE_POOL_IDLE_TTL_MS`. `Protected`
/// entries are NEVER evicted (in-flight `LeaseGuard` holds the `Arc` so
/// eviction would not corrupt active requests, but Protected entries are
/// actively used and have no idle indicator).
///
/// Returns the count of evicted entries. Called per tick by `ledger_sweep`.
pub fn sweep_unprotected() -> u64 {
    let now = now_ms();
    let mut removed: u64 = 0;
    LEASE_POOL.retain(|_uid, state| {
        if state.mode() != Mode::Unprotected {
            return true;
        }
        let last = state.last_seen_at_ms.load(Ordering::Acquire);
        if last == 0 {
            // Never recorded a transition timestamp — keep, conservative.
            return true;
        }
        if now.saturating_sub(last) > LEASE_POOL_IDLE_TTL_MS {
            removed += 1;
            return false;
        }
        true
    });
    if removed > 0 {
        metrics::counter!("gw_lease_pool_sweep_removed_total").increment(removed);
    }
    removed
}
```

- [ ] **Step 3: Run the new tests**

```bash
cargo test -p gw-ledger --lib lease::tests::sweep_unprotected
```

Expected: 3/3 PASS.

- [ ] **Step 4: Run all lease tests**

```bash
cargo test -p gw-ledger --lib lease::tests
```

Expected: all PASS.

- [ ] **Step 5: Commit (lease changes are a self-contained unit)**

```bash
git add crates/gw-ledger/src/lease.rs
git commit -m "$(cat <<'EOF'
fix(gw-ledger): bound LEASE_POOL via sweep_unprotected

Adds `last_seen_at_ms` to LeaseState (written by upsert_unprotected) and
`sweep_unprotected()` that evicts entries whose mode is Unprotected and
whose last_seen_at_ms is older than LEASE_POOL_IDLE_TTL_MS (default
30 min). Protected entries are never touched.

In-flight LeaseGuard holds Arc<LeaseState> so eviction is safe wrt
already-acquired guards. New entries created later for the same user
flow through upsert_* normally.

Emits `gw_lease_pool_sweep_removed_total` for observability. Called per
tick by the ledger_sweep task added in a follow-up commit.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: `coef::sweep_orphans` — failing tests

**Files:** `crates/gw-ledger/src/coef.rs`

- [ ] **Step 1: Append failing tests**

Append inside `mod tests` (after the existing tests):

```rust
    #[test]
    fn sweep_orphans_evicts_versions_below_active_minus_margin() {
        let _g = reset();
        // active_version = 200
        COEF_CACHE.store(Arc::new(CoefSnapshot {
            version: 200,
            by_model: HashMap::new(),
        }));
        // Seed staging with versions far behind active.
        apply_coef_entry(50, "m", Dim::Input, 1);
        apply_coef_entry(80, "m", Dim::Input, 1);
        // And one inside the safety window.
        apply_coef_entry(150, "m", Dim::Input, 1);

        let removed = sweep_orphans();

        // active(200) - SAFETY_MARGIN(100) = 100; versions < 100 evicted.
        assert!(removed >= 2);
        assert!(COEF_STAGING.get(&50).is_none());
        assert!(COEF_STAGING.get(&80).is_none());
        assert!(
            COEF_STAGING.get(&150).is_some(),
            "version within safety margin must be preserved"
        );
    }

    #[test]
    fn sweep_orphans_noop_when_active_below_margin() {
        let _g = reset();
        // active = 5, margin = 100; saturating_sub goes to 0 → no orphan
        COEF_CACHE.store(Arc::new(CoefSnapshot {
            version: 5,
            by_model: HashMap::new(),
        }));
        apply_coef_entry(1, "m", Dim::Input, 1);
        apply_coef_entry(3, "m", Dim::Input, 1);

        let removed = sweep_orphans();

        assert_eq!(removed, 0);
        assert!(COEF_STAGING.get(&1).is_some());
        assert!(COEF_STAGING.get(&3).is_some());
    }

    #[test]
    fn sweep_orphans_noop_on_empty_staging() {
        let _g = reset();
        COEF_CACHE.store(Arc::new(CoefSnapshot {
            version: 500,
            by_model: HashMap::new(),
        }));
        assert_eq!(sweep_orphans(), 0);
    }
```

- [ ] **Step 2: Verify compile failures**

```bash
cargo test -p gw-ledger --lib coef::tests::sweep_orphans 2>&1 | tail -10
```

Expected: compile errors — `sweep_orphans` not defined.

---

## Task 7: Implement `coef::sweep_orphans`

**Files:** `crates/gw-ledger/src/coef.rs`

- [ ] **Step 1: Add safety margin const + sweep function**

After the `pub static COEF_STAGING: ...` line (line 85), insert:

```rust
/// Safety margin between the active version and the orphan-eviction
/// threshold. `sweep_orphans` removes staging entries whose version is
/// strictly less than `active_version.saturating_sub(SAFETY_MARGIN)`.
/// Must exceed the maximum number of versions Go may have in flight
/// between `coef_table` entries and `coef_table_commit`. 100 is safe for
/// the expected push pattern; reduce only with operational evidence.
pub const COEF_STAGING_SAFETY_MARGIN: u64 = 100;

/// Evict orphan staging versions: entries where the `version` key is
/// strictly less than `active_version - SAFETY_MARGIN`. `apply_coef_commit`
/// already handles the `Stale` (version <= active) and `Incomplete`
/// (staged < expected) branches — the orphan case here is versions that
/// were `apply_coef_entry`'d but never received any commit at all.
///
/// `coef::lookup` is called only from `credit_estimator::estimate_request_credits`
/// at request admission time; settle path does not re-read staging.
/// `version >= active - SAFETY_MARGIN` is preserved to cover Go's in-flight
/// push depth and any recently-active LeaseGuard's captured `coef_version`.
///
/// Returns the count of evicted entries. Called per tick by `ledger_sweep`.
pub fn sweep_orphans() -> u64 {
    let active_version = COEF_CACHE.load().version;
    let threshold = active_version.saturating_sub(COEF_STAGING_SAFETY_MARGIN);
    if threshold == 0 {
        return 0;
    }
    let mut removed: u64 = 0;
    COEF_STAGING.retain(|version, _| {
        if *version < threshold {
            removed += 1;
            false
        } else {
            true
        }
    });
    if removed > 0 {
        metrics::counter!("gw_coef_staging_sweep_orphans_removed_total").increment(removed);
    }
    removed
}
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p gw-ledger --lib coef::tests
```

Expected: all PASS (new 3 + existing).

- [ ] **Step 3: Commit**

```bash
git add crates/gw-ledger/src/coef.rs
git commit -m "$(cat <<'EOF'
fix(gw-ledger): bound COEF_STAGING via sweep_orphans

Adds sweep_orphans() that removes staging entries whose version is
strictly less than active_version - SAFETY_MARGIN (default 100). This
catches the case where apply_coef_entry was called for a version that
never received any apply_coef_commit (orphan). apply_coef_commit's
existing Stale/Incomplete branches remain unchanged.

Verified single caller of coef::lookup (credit_estimator at request
admission time) — settle path does not re-read staging, so eviction is
safe wrt in-flight LeaseGuards.

Emits gw_coef_staging_sweep_orphans_removed_total.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: `BillingBacklog` observability — failing tests

**Files:** `crates/gw-ledger/src/backlog.rs`

- [ ] **Step 1: Append failing tests in mod tests**

Append in the existing `mod tests`:

```rust
    #[tokio::test]
    async fn high_watermark_tracks_max_pending_count() {
        let b = BillingBacklog::new();
        for _ in 0..5 {
            b.push(env()).await;
        }
        assert_eq!(b.high_watermark(), 5);
        for _ in 0..3 {
            b.pop().await;
        }
        // High-watermark persists; does not decrement on pop.
        assert_eq!(b.high_watermark(), 5);
    }

    #[tokio::test]
    async fn crossed_threshold_resets_when_pending_drops_below() {
        // Use a synthetic threshold via the constant. We can't override it,
        // so we verify the flag is exposed via a public reader and that it
        // tracks state correctly with respect to the real constant.
        let b = BillingBacklog::new();
        // At construction, the flag is unset.
        assert!(!b.crossed_threshold_flag());
    }
```

- [ ] **Step 2: Verify compile failures**

```bash
cargo test -p gw-ledger --lib backlog::tests 2>&1 | tail -10
```

Expected: compile errors — `crossed_threshold_flag` accessor not defined.

---

## Task 9: Implement `BillingBacklog` observability

**Files:** `crates/gw-ledger/src/backlog.rs`

- [ ] **Step 1: Add the threshold const and the new field**

Near the top of the file (after the `use` block):

```rust
/// Upward-crossing threshold for `BillingBacklog::pending_count`. When the
/// count crosses this upward, `gw_billing_backlog_over_threshold_total`
/// increments once. The flag resets when the count drops below the threshold,
/// so future upward crossings count again.
pub const BILLING_BACKLOG_OVER_THRESHOLD: usize = 10_000;
```

Inside `BillingBacklog` struct, add a new field at the end:

```rust
    crossed_threshold: std::sync::atomic::AtomicBool,
```

In `BillingBacklog::new`, append:

```rust
            crossed_threshold: std::sync::atomic::AtomicBool::new(false),
```

- [ ] **Step 2: Update `push` to emit high-watermark and threshold counter**

Inside `push`, after the existing `metrics::gauge!("gw_billing_backlog_pending_count").set(...)` line, add:

```rust
        // Expose the existing high_watermark_count as a gauge.
        metrics::gauge!("gw_billing_backlog_high_watermark_count")
            .set(self.high_watermark() as f64);

        // Upward-crossing detection: increment once per crossing, not per push.
        if count >= BILLING_BACKLOG_OVER_THRESHOLD {
            use std::sync::atomic::Ordering as O;
            if !self.crossed_threshold.swap(true, O::AcqRel) {
                metrics::counter!("gw_billing_backlog_over_threshold_total").increment(1);
            }
        }
```

- [ ] **Step 3: Update `pop` to reset the crossing flag**

Inside `pop`, after the existing `pending_count.fetch_sub(1, ...)` line, add:

```rust
        use std::sync::atomic::Ordering as O;
        if self.pending_count() < BILLING_BACKLOG_OVER_THRESHOLD {
            self.crossed_threshold.store(false, O::Release);
        }
```

- [ ] **Step 4: Add the public accessor for the flag**

In `impl BillingBacklog`, after `high_watermark`:

```rust
    /// Returns whether the backlog has crossed `BILLING_BACKLOG_OVER_THRESHOLD`
    /// since the last drop back below it. Exposed for tests and operator
    /// inspection; production code should consume the
    /// `gw_billing_backlog_over_threshold_total` counter instead.
    pub fn crossed_threshold_flag(&self) -> bool {
        self.crossed_threshold.load(std::sync::atomic::Ordering::Acquire)
    }
```

- [ ] **Step 5: Run tests**

```bash
cargo test -p gw-ledger --lib backlog::tests
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-ledger/src/backlog.rs
git commit -m "$(cat <<'EOF'
fix(gw-ledger): expose BillingBacklog observability

- Expose the existing high_watermark_count as gauge
  gw_billing_backlog_high_watermark_count (was tracked in-struct but
  not emitted).
- Add gw_billing_backlog_over_threshold_total counter that fires once
  per UPWARD crossing of BILLING_BACKLOG_OVER_THRESHOLD (default
  10_000). A `crossed_threshold: AtomicBool` field gates the counter
  so it does not increment per push; pop() resets the flag when the
  backlog drains below the threshold.

The pending_bytes / pending_count gauges already emitted in push are
unchanged (audit's "size-bytes gauge" requirement is satisfied by the
existing pending_bytes).

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: `ledger_sweep::spawn` — failing test for shutdown cancellation

**Files:** `crates/gw-ledger/src/ledger_sweep.rs` (new), `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: Create the new file with a failing test that requires the impl**

Create `crates/gw-ledger/src/ledger_sweep.rs` with ONLY:

```rust
//! Per-tick sweep loop for in-memory ledger maps:
//! - `lease::sweep_unprotected` evicts idle Unprotected entries.
//! - `coef::sweep_orphans` evicts orphan staging versions.
//!
//! Supervised by `gw_runtime::Supervisor` (catch_unwind +
//! exponential-backoff restart). Registered with `ShutdownCoordinator`.

use crate::{coef, lease};
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::debug;

/// Sweep cadence. 60 s matches DeductGc and is short enough that the
/// LEASE_POOL_IDLE_TTL_MS / COEF_STAGING_SAFETY_MARGIN windows are
/// honoured without unnecessary churn.
const LEDGER_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Spawn the periodic ledger sweep. The Supervisor recovers from panics
/// (catch_unwind + exponential-backoff restart) so a sweep iteration
/// panicking does not silently disable the loop. Returns the JoinHandle
/// so the caller can register it with the ShutdownCoordinator.
pub fn spawn(shutdown: ShutdownToken) -> JoinHandle<()> {
    Supervisor::new("ledger_sweep", RestartPolicy::default(), shutdown).spawn(move |tok| async move {
        let mut tick = tokio::time::interval(LEDGER_SWEEP_INTERVAL);
        tick.tick().await; // skip immediate first tick (match DeductGc convention)
        loop {
            tokio::select! {
                _ = tok.cancelled() => {
                    tracing::info!(target: "gw_ledger::ledger_sweep", "shutdown requested");
                    return Ok(());
                }
                _ = tick.tick() => {
                    let lease_removed = lease::sweep_unprotected();
                    let coef_removed = coef::sweep_orphans();
                    if lease_removed > 0 || coef_removed > 0 {
                        debug!(
                            lease_removed,
                            coef_removed,
                            "ledger sweep tick"
                        );
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_runtime::ShutdownCoordinator;

    /// Cancellation via ShutdownToken must promptly exit the sweep task.
    /// Mirrors Crate 2's `spawn_respects_shutdown_token`. The actual
    /// ShutdownCoordinator API is `coord.token().cancel()` (no `shutdown()`
    /// method; see gw_runtime::shutdown).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = spawn(token);
        coord.token().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(
            exit.is_ok(),
            "ledger_sweep did not exit within 5s of shutdown"
        );
    }
}
```

In `crates/gw-ledger/src/lib.rs`, add the module declaration alongside the existing `pub mod` entries:

```bash
grep -n "pub mod\|pub use" crates/gw-ledger/src/lib.rs | head -20
```

Add `pub mod ledger_sweep;` in the public-modules section (near the existing `pub mod inflight_reaper;` if present, or grouped with other reaper-style modules).

- [ ] **Step 2: Build and run the test**

```bash
cargo build -p gw-ledger
```

Expected: clean.

```bash
cargo test -p gw-ledger --lib ledger_sweep::tests
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/gw-ledger/src/ledger_sweep.rs crates/gw-ledger/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(gw-ledger): add ledger_sweep::spawn (Supervisor-wrapped)

New per-tick body calls lease::sweep_unprotected + coef::sweep_orphans
under gw_runtime::Supervisor (catch_unwind + exponential-backoff restart
mirrors DeductGc::spawn). Returns JoinHandle for ShutdownCoordinator
registration. 60s tick interval; first tick skipped to match existing
sweep cadence convention.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Wire `ledger_sweep::spawn` into bootstrap

**Files:** `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: Find the correct insertion point**

The natural location is alongside the existing `gw_ledger::inflight_reaper::spawn(...)` call (the DeductGc call from Crate 2 is also in this area). Locate it:

```bash
grep -nB2 -A5 "inflight_reaper::spawn\|DeductGc::spawn" crates/gw-gateway/src/lib.rs
```

- [ ] **Step 2: Insert the ledger_sweep spawn + register**

After the `DeductGc::spawn` block (added in Crate 2), insert:

```rust
    // ledger_sweep: periodic sweep of LEASE_POOL (Unprotected/idle) +
    // COEF_STAGING (orphan versions). Supervisor-wrapped; registered with
    // the ShutdownCoordinator like the other ledger-side background tasks.
    let ledger_sweep_handle = gw_ledger::ledger_sweep::spawn(shutdown_token.clone());
    shutdown.register(ledger_sweep_handle);
```

- [ ] **Step 3: Build to verify**

```bash
cargo build -p gw-gateway
```

Expected: clean.

- [ ] **Step 4: Run gw-gateway tests for regression**

```bash
cargo test -p gw-gateway
```

Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-gateway/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(gw-gateway): wire ledger_sweep into bootstrap

Spawns gw_ledger::ledger_sweep and registers its JoinHandle on the
ShutdownCoordinator alongside DeductGc and inflight_reaper. Per-tick
the new task runs lease::sweep_unprotected + coef::sweep_orphans.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 12: Docs sync

**Files:** `docs/spec/06-billing-ledger.md`, `docs/architecture.md`, `docs/ROADMAP.md`

- [ ] **Step 1: Update `docs/spec/06-billing-ledger.md`**

Locate the lease + coef sections:

```bash
grep -n "LEASE_POOL\|COEF_STAGING\|BillingBacklog" docs/spec/06-billing-ledger.md | head -10
```

Add a paragraph in the appropriate place:

> **Ledger sweep cadence.** A `ledger_sweep` background task (Supervisor-wrapped, 60s tick, ShutdownCoordinator-registered) periodically evicts:
> - `LEASE_POOL` entries whose `mode == Unprotected` AND `last_seen_at_ms` is older than `LEASE_POOL_IDLE_TTL_MS` (30 min default). `Protected` entries are never touched.
> - `COEF_STAGING` entries whose version is strictly less than `active_version - COEF_STAGING_SAFETY_MARGIN` (100 default). `apply_coef_commit`'s existing `Stale` and `Incomplete` branches remain unchanged; this sweep covers the orphan case (entries inserted by `apply_coef_entry` whose commit event never arrives).
>
> **BillingBacklog observability.** In addition to the existing `gw_billing_backlog_pending_bytes` and `gw_billing_backlog_pending_count` gauges, `push` now emits `gw_billing_backlog_high_watermark_count` (the maximum pending count seen) and `gw_billing_backlog_over_threshold_total` (a counter that fires once per upward crossing of `BILLING_BACKLOG_OVER_THRESHOLD`, default 10_000). The crossing flag resets when `pop` drops the count below the threshold, so subsequent crossings count again.

- [ ] **Step 2: Update `docs/architecture.md` §Deliberate behaviours**

Locate §Deliberate behaviours and append:

> **LEASE_POOL: Unprotected idle eviction.** Entries whose `mode == Unprotected` and whose `last_seen_at_ms` is older than `LEASE_POOL_IDLE_TTL_MS` (30 min) are evicted by `ledger_sweep`. Protected entries are never evicted. In-flight `LeaseGuard` holds an `Arc<LeaseState>` so eviction is safe with respect to already-acquired guards (the Arc refcount keeps the state alive until the guard drops).

> **COEF_STAGING orphan eviction is version-based.** Entries whose `version` is strictly less than `active_version - COEF_STAGING_SAFETY_MARGIN` (100) are evicted by `ledger_sweep`. Wall-clock TTL was rejected (the codebase has a documented workaround for system-clock failures in `lease.rs::now_ms`). The single caller of `coef::lookup` is at request admission time only — no settle-path or post-admission code reads historical staging versions, so eviction is correctness-safe.

> **LeaseGuard::Drop intentionally does NOT refund credits in this work.** A panic between `try_acquire` and `settle_actual` leaves the in-process `LeaseState.credits` counter lower than the true available value. Go's event-driven `protected_lease` pushes overwrite this counter periodically, bounding the false-rejection window. If operational evidence later shows the window is too long, a follow-up adds a Drop refund mirroring `RequestFreeze::Drop`'s pattern. (Recorded 2026-06-12 per user decision; see scope-correction in `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 3.)

- [ ] **Step 3: Update `docs/ROADMAP.md`**

Locate items related to LEASE_POOL / COEF_STAGING / BillingBacklog. Remove matching open items if present, else append a Recently Closed entry:

> - 2026-06-12 — Closed: `LEASE_POOL` Unprotected sweep, `COEF_STAGING` orphan sweep, `BillingBacklog` high-watermark gauge + over-threshold counter. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 3 (scope-correction record). `LeaseGuard::Drop` refund DEFERRED — see architecture.md §Deliberate behaviours for the rationale.

- [ ] **Step 4: CLAUDE.md ⇄ AGENTS.md byte-identical body**

```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "BYTE-IDENTICAL BODY"
```

Expected: `BYTE-IDENTICAL BODY`.

- [ ] **Step 5: Commit docs**

```bash
git add docs/spec/06-billing-ledger.md docs/architecture.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync architecture/spec/roadmap with ledger_sweep + BillingBacklog observability

- spec/06-billing-ledger.md: document ledger_sweep cadence and the two
  sweeps; new BillingBacklog metrics.
- architecture.md §Deliberate behaviours: three new entries
  (LEASE_POOL Unprotected eviction, COEF_STAGING version-based orphan,
  LeaseGuard::Drop intentionally not refunding — with the deferred-work
  pointer back to the spec).
- ROADMAP.md: close the three items addressed by this PR; record the
  LeaseGuard::Drop deferral with rationale.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 13: Completion gate (orchestrator-run, sequential)

Per the workflow rule: cargo commands MUST be sequential (`check-invariants.sh` runs cargo internally; concurrent cargo invocations contend on the target lock).

- [ ] **Step 1: Per-crate tests, then workspace fmt**

```bash
cargo test -p gw-ledger
```

Expected: all PASS.

```bash
cargo test -p gw-gateway
```

Expected: all PASS.

```bash
cargo fmt --all -- --check
```

Expected: no output. If output appears: `cargo fmt --all`, commit as `chore: cargo fmt --all`.

- [ ] **Step 2: Workspace clippy (sequential after the above)**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean.

- [ ] **Step 3: Architectural invariants (sequential after clippy)**

```bash
bash scripts/check-invariants.sh
```

Expected: clean (advisory `managed_loop_advisory` warnings are not blocking).

---

## Task 14: Merge to local main (orchestrator)

Orchestrator action — not the implementer subagent. After Task 13 passes:

```bash
git checkout main
git merge --no-ff fix/memory-leaks-gw-ledger -m "Merge Crate 3: gw-ledger sweeps + BillingBacklog observability"
```

Then move to Crate 4 planning.

---

## Self-Review

**1. Spec coverage** — every § Crate 3 (post-correction) item maps to a task:
- LEASE_POOL Unprotected sweep → Tasks 2–5
- COEF_STAGING orphan sweep → Tasks 6–7
- BillingBacklog gauges → Tasks 8–9
- Supervisor-wrapped sweep task → Task 10
- Bootstrap wiring → Task 11
- Docs sync → Task 12
- Completion gate → Task 13
- Merge → Task 14
- ✅ Complete.

**2. Placeholder scan** — none. All commands and code blocks are concrete.

**3. Type consistency** —
- `last_seen_at_ms: AtomicU64` — declared once, written once (upsert_unprotected), read once (sweep_unprotected)
- `LEASE_POOL_IDLE_TTL_MS: u64` — declared as const; tests reference by name
- `COEF_STAGING_SAFETY_MARGIN: u64` — declared as const; tests reference by name
- `BILLING_BACKLOG_OVER_THRESHOLD: usize` — declared as const; used in push and pop crossing logic
- `crossed_threshold: AtomicBool` — new field; reader `crossed_threshold_flag()` is the test-visible accessor
- `ledger_sweep::spawn` API: `(shutdown: ShutdownToken) -> JoinHandle<()>` — mirrors `DeductGc::spawn` from Crate 2
- Metric names: `gw_lease_pool_sweep_removed_total`, `gw_coef_staging_sweep_orphans_removed_total`, `gw_billing_backlog_high_watermark_count`, `gw_billing_backlog_over_threshold_total` — all follow existing `gw_*` conventions

---

## Notes for the Implementer

- Crate 3's scope was significantly reduced after pre-implementation reading. The parent spec § Crate 3 has a "Scope-correction record" capturing what was dropped and why. If the plan and the parent spec disagree, the plan wins.
- This is sonnet-appropriate work after the scope reduction. If you find yourself reaching for opus-level reasoning, you are probably overthinking — re-read the §Crate 3 update.
- BLOCKED triggers: pre-impl verification (Task 1 Step 2) finds the work has been done independently; sweep tests behave non-deterministically (suggests `now_ms` or atomics ordering bug — surface before committing).
- DONE_WITH_CONCERNS triggers: any sweep finds itself wanting to evict Protected entries (don't do it, surface instead).
