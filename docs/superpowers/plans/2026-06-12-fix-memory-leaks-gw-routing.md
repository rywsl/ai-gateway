# Crate 5: gw-routing — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Bound 6 in-memory tracking structures in `gw-routing` via a new Supervisor-wrapped `routing_sweep` task (sweeps `ModelFrequencyTracker`, `ReservationGlobalView`, `LatencyTracker`, `AffinityManager` per tick), make `InFlightStore.count` non-allocating, and cap `UNKNOWN_PRESETS_WARNED` via Moka.

**Architecture:** New `gw-routing::routing_sweep::spawn(shutdown)` mirrors `gw-ledger::ledger_sweep`. Per 60s tick the body calls 4 sweep methods (2 new + 2 pre-existing). `InFlightStore.count(k)` becomes an alias for the existing `count_if_present(k)` (semantic micro-change, single callsite). `UNKNOWN_PRESETS_WARNED` switches from `LazyLock<DashSet>` to `LazyLock<Cache<String, ()>>` with `max_capacity 1000`.

**Tech Stack:** Rust 1.88.0, `gw-runtime::Supervisor` (existing), `dashmap`, `moka::sync::Cache`, `tokio`, `metrics`, `tracing`. No new workspace deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 5.

**Branch:** `fix/memory-leaks-gw-routing` cut from local `main` (includes Crate 4 merge `b84af4d`).

**Test budget**: ≤ 8 new tests total (user minimalism mandate).

---

## Pre-Implementation Findings (scope-correction)

| Item | Spec said | Empirical reality |
|------|-----------|-------------------|
| `ModelFrequencyTracker.counters` | `reservation.rs:45`, unbounded `DashMap<ModelName, Arc<ModelFrequencyCounter>>` | ✅ confirmed; needs new last-used `AtomicU64` + sweep |
| `ReservationGlobalView` 2 DashMaps | `reservation.rs:104-105`, unbounded | ✅ confirmed; `InstanceRate.ts_ms` already tracked at line 140 — sweep needs only to compare against `now_ms - threshold` |
| `InFlightStore.count()` `get_or_create` | `in_flight.rs:60` | ✅ confirmed; **but** `count_if_present` already exists at line 63 with the correct semantic. Simplest fix: make `count` delegate to `count_if_present` (single semantic), update sole caller at `candidates.rs:67` (which still works correctly with the new semantic) |
| `LatencyTracker.evict_stale()` no timer | `latency.rs:132` | ✅ confirmed; function exists; need wiring |
| `AffinityManager.sweep_expired()` no timer | `affinity.rs:76` | ✅ confirmed; function exists; need wiring |
| `UNKNOWN_PRESETS_WARNED: DashSet` no cap | `strategy/set.rs:48` | ✅ confirmed; replace with `Cache<String, ()>` cap 1000 |

Net effect: 1 new field (`ModelFrequencyCounter.last_used_at_ms`) + 2 new sweep methods + 1 new helper file (`routing_sweep.rs`) + 1 bootstrap wire + 1 semantic micro-change + 1 Moka replacement.

---

## File Structure

**Modified files:**
- `crates/gw-routing/src/in_flight.rs` — `count()` delegates to `count_if_present()`; update doc-comment
- `crates/gw-routing/src/candidates.rs` — no change (existing call still works correctly under the new semantic)
- `crates/gw-routing/src/reservation.rs` — add `ModelFrequencyCounter.last_used_at_ms: AtomicU64`; add `ModelFrequencyTracker::sweep_stale(threshold_ms)`; add `ReservationGlobalView::sweep_stale_instances(threshold_ms)`
- `crates/gw-routing/src/strategy/set.rs` — replace `DashSet` with `Cache<String, ()>`
- `crates/gw-routing/src/routing_sweep.rs` (NEW) — `pub fn spawn(shutdown: ShutdownToken) -> JoinHandle<()>` wrapping all 4 sweep calls per tick
- `crates/gw-routing/src/lib.rs` — `pub mod routing_sweep;`
- `crates/gw-gateway/src/lib.rs` — call `gw_routing::routing_sweep::spawn(shutdown_token.clone())` + register handle
- `docs/spec/02-routing.md` (note: verify file name — may be `02-routing.md` or `03-routing.md`; the spec referenced `03-routing.md` but implementers should check actual file)
- `docs/architecture.md` §Deliberate behaviours — add row for routing_sweep semantics
- `docs/ROADMAP.md` — close items

**Test files:**
- `crates/gw-routing/src/in_flight.rs` mod tests — 1 new test
- `crates/gw-routing/src/reservation.rs` mod tests — 2 new tests (ModelFrequencyTracker sweep + ReservationGlobalView instance sweep)
- `crates/gw-routing/src/strategy/set.rs` mod tests — 1 new test
- `crates/gw-routing/src/routing_sweep.rs` mod tests — 1 new test (shutdown-token cancellation)

**Total new tests: 5.** Budget remaining: 3. Add ONLY if reviewer flags coverage gap.

**No new workspace deps.**

---

## Task 1: Branch Setup + Verification

- [ ] **Step 1: Cut branch**

```bash
git checkout main
git status     # expect ahead-of-origin, clean
git log --oneline -3   # expect b84af4d (Crate 4 merge) recent
git checkout -b fix/memory-leaks-gw-routing
```

- [ ] **Step 2: Pre-implementation verification**

```bash
grep -n "count_if_present" crates/gw-routing/src/in_flight.rs
```
Expected: present at line 63.

```bash
grep -n "ts_ms" crates/gw-routing/src/reservation.rs | head -5
```
Expected: `InstanceRate.ts_ms` already exists (this is what we'll sweep against).

```bash
grep -n "last_used_at_ms\|sweep_stale\|sweep_stale_instances" crates/gw-routing/src/reservation.rs
```
Expected: empty.

```bash
grep -n "routing_sweep" crates/gw-routing/src/lib.rs
```
Expected: empty.

```bash
grep -n "Cache<String\|moka" crates/gw-routing/src/strategy/set.rs
```
Expected: empty.

If any of these fail, STOP and surface.

- [ ] **Step 3: Determine the spec doc filename**

```bash
ls docs/spec/ | grep -E "02|03" | head -5
```

Take note of the actual routing spec file name (`02-routing.md` or similar). Use that file in Task 7 docs sync.

---

## Task 2: `InFlightStore.count()` non-allocating semantic

**Files:** `crates/gw-routing/src/in_flight.rs`

- [ ] **Step 1: Failing test**

Append to existing `mod tests` (or add `#[cfg(test)] mod tests` if absent):

```rust
    use super::*;
    use gw_core::ids::KeyId;

    #[test]
    fn count_returns_zero_without_creating_entry() {
        let store = InFlightStore::default();
        let key = KeyId::from("k-never-acquired");
        // Read-only count of an unknown key.
        assert_eq!(store.count(&key), 0);
        // Critical: no entry was inserted.
        assert!(
            !store.by_key.contains_key(&key),
            "count() on unknown key must NOT create an entry"
        );
    }
```

- [ ] **Step 2: Verify it fails**

```bash
cargo test -p gw-routing --lib in_flight::tests::count_returns_zero_without_creating_entry 2>&1 | tail -10
```
Expected: test FAILS at the `contains_key` assertion (current code creates the entry).

- [ ] **Step 3: Fix `count`**

In `in_flight.rs`, change `count`:

```rust
    pub fn count(&self, k: &KeyId) -> u32 {
        self.count_if_present(k)
    }
```

Update the doc-comment at the top of the file (currently around lines 18-21) — replace the "Memory note" paragraph with:

```rust
//! `InFlightStore::count(k)` returns 0 for an unknown key without creating
//! an entry. Entries are created only by `acquire`/`get_or_create` (the
//! actual in-flight tracking path). This avoids the failure mode where a
//! read-only `count` query for an arbitrary key would otherwise allocate.
```

- [ ] **Step 4: Run test**

```bash
cargo test -p gw-routing --lib in_flight::tests::count_returns_zero_without_creating_entry
cargo test -p gw-routing --lib in_flight::tests
```
Expected: PASS.

- [ ] **Step 5: Verify single caller still works**

```bash
grep -n "in_flight.count\|in_flight\.count_if_present" crates/gw-routing/src/candidates.rs
```
Confirm `candidates.rs:67` still uses `in_flight.count(...)`. Semantic unchanged for that caller (it always reads a value; never depended on side-effect).

- [ ] **Step 6: Commit**

```bash
git add crates/gw-routing/src/in_flight.rs
git commit -m "$(cat <<'EOF'
fix(gw-routing): make InFlightStore.count non-allocating

count(k) now delegates to count_if_present(k) — returns 0 for an
unknown key without creating an entry. Audit 2026-06-12 flagged the
prior get_or_create semantic as a leak path: a query for an arbitrary
KeyId would allocate an InFlightCounter forever.

The sole production caller (candidates.rs:67) only reads the value;
it does not depend on the side-effect. Acquire-time entry creation
remains via get_or_create.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: `UNKNOWN_PRESETS_WARNED` → Moka cap 1000

**Files:** `crates/gw-routing/src/strategy/set.rs`, `crates/gw-routing/Cargo.toml`

- [ ] **Step 1: Verify moka in deps**

```bash
grep "^moka" crates/gw-routing/Cargo.toml
```
If missing: add `moka = { workspace = true }` under `[dependencies]`.

- [ ] **Step 2: Failing test**

Append to `set.rs` mod tests:

```rust
    #[test]
    fn unknown_presets_warned_caps_at_capacity() {
        // The set is a LazyLock global; insert past capacity and confirm
        // size never exceeds the cap. Use the public accessor or call
        // the warn path repeatedly via insert (current public API uses
        // .insert returning true on first time).
        for i in 0..2000 {
            // Each preset name is unique; if uncapped, all 2000 land in
            // the set.
            UNKNOWN_PRESETS_WARNED.insert(format!("unknown-preset-{i}"), ());
        }
        // run_pending_tasks() so moka's eviction settles.
        UNKNOWN_PRESETS_WARNED.run_pending_tasks();
        assert!(
            UNKNOWN_PRESETS_WARNED.entry_count() <= 1000,
            "UNKNOWN_PRESETS_WARNED should cap at 1000, got {}",
            UNKNOWN_PRESETS_WARNED.entry_count()
        );
    }
```

- [ ] **Step 3: Verify compile failure** (uses `.run_pending_tasks()` / `.entry_count()` which are Moka APIs not on DashSet)

```bash
cargo test -p gw-routing --lib strategy::set::tests::unknown_presets_warned_caps_at_capacity 2>&1 | tail -10
```

- [ ] **Step 4: Replace DashSet with Moka**

In `crates/gw-routing/src/strategy/set.rs`, change line 48 area:

```rust
use moka::sync::Cache;
use std::sync::LazyLock;

const UNKNOWN_PRESETS_WARNED_CAPACITY: u64 = 1000;

static UNKNOWN_PRESETS_WARNED: LazyLock<Cache<String, ()>> = LazyLock::new(|| {
    Cache::builder()
        .max_capacity(UNKNOWN_PRESETS_WARNED_CAPACITY)
        .build()
});
```

Find the production callsite (around line 145 per the spec):

```bash
grep -n "UNKNOWN_PRESETS_WARNED" crates/gw-routing/src/strategy/set.rs
```

Update from `UNKNOWN_PRESETS_WARNED.insert(preset.clone())` (which returns `bool` for DashSet) to:

```rust
if UNKNOWN_PRESETS_WARNED.get(&preset).is_none() {
    UNKNOWN_PRESETS_WARNED.insert(preset.clone(), ());
    // ... existing warn-once log
}
```

Adapt the conditional to whatever the existing code does (warn-once on first insertion).

- [ ] **Step 5: Run new test + regression**

```bash
cargo test -p gw-routing --lib strategy::set::tests
```
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-routing/src/strategy/set.rs crates/gw-routing/Cargo.toml
git commit -m "$(cat <<'EOF'
fix(gw-routing): cap UNKNOWN_PRESETS_WARNED via Moka (1000)

DashSet was unbounded; an attacker (or misconfigured client) feeding
distinct preset names could accumulate entries indefinitely. Replace
with moka::sync::Cache<String, ()> with max_capacity 1000. Warn-once
semantics preserved (existing log still fires only on insertion).

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `ModelFrequencyTracker` + `ReservationGlobalView` sweep methods

**Files:** `crates/gw-routing/src/reservation.rs`

- [ ] **Step 1: Failing tests**

Append two new tests to the existing `mod tests`:

```rust
    #[test]
    fn model_frequency_tracker_sweeps_stale_counters() {
        let tracker = ModelFrequencyTracker::default();
        let model_old = ModelName::from("model-old");
        let model_new = ModelName::from("model-new");
        tracker.record_pick(&model_old);
        tracker.record_pick(&model_new);

        // Backdate `model_old`'s last_used_at_ms.
        if let Some(counter) = tracker.counters.get(&model_old) {
            counter.last_used_at_ms.store(
                now_ms().saturating_sub(60_000),
                std::sync::atomic::Ordering::Release,
            );
        }

        let removed = tracker.sweep_stale(30_000);
        assert_eq!(removed, 1);
        assert!(!tracker.counters.contains_key(&model_old));
        assert!(tracker.counters.contains_key(&model_new));
    }

    #[test]
    fn reservation_global_view_sweeps_stale_instances() {
        let view = ReservationGlobalView::new();
        let model = ModelName::from("m1");
        let now = current_ts_ms();
        view.apply_event(ReservationEvent {
            instance_id: "alive".into(),
            model: model.clone(),
            rate: 1.0,
            ts_ms: now,
        });
        view.apply_event(ReservationEvent {
            instance_id: "dead".into(),
            model: model.clone(),
            rate: 1.0,
            ts_ms: now.saturating_sub(60_000),
        });

        let removed = view.sweep_stale_instances(30_000);
        assert_eq!(removed, 1);
        assert!(view
            .by_model_instance
            .contains_key(&(model.clone(), "alive".into())));
        assert!(!view
            .by_model_instance
            .contains_key(&(model.clone(), "dead".into())));
    }
```

`now_ms()` and `current_ts_ms()` — use whichever helpers already exist in the reservation tests (check existing tests with `grep -n "fn now_ms\|fn current_ts_ms\|SystemTime::now" crates/gw-routing/src/reservation.rs`). Use the same pattern. If neither exists, implement a local helper in the test module:

```rust
fn current_ts_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

- [ ] **Step 2: Verify compile failure**

```bash
cargo test -p gw-routing --lib reservation::tests 2>&1 | tail -10
```

Expected: errors about `last_used_at_ms`, `sweep_stale`, `sweep_stale_instances` not existing.

- [ ] **Step 3: Add `last_used_at_ms` to `ModelFrequencyCounter`**

Find `ModelFrequencyCounter` struct in `reservation.rs`. Add:

```rust
pub struct ModelFrequencyCounter {
    // ... existing fields ...
    pub last_used_at_ms: std::sync::atomic::AtomicU64,
}
```

In its constructor (or `Default` impl), initialize `last_used_at_ms: AtomicU64::new(0)`.

In `record_pick` (or wherever the counter is touched), append:

```rust
counter.last_used_at_ms.store(now_ms(), Ordering::Release);
```

`now_ms` — use the helper that already exists in the file, or import from `gw-ledger::lease::now_ms` if appropriate. Or replicate the inline pattern:

```rust
fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

- [ ] **Step 4: Add `sweep_stale` to `ModelFrequencyTracker`**

```rust
impl ModelFrequencyTracker {
    /// Evict counters whose `last_used_at_ms` is older than now - threshold_ms.
    /// Returns the count of evicted entries.
    pub fn sweep_stale(&self, threshold_ms: u64) -> u64 {
        let now = now_ms();
        let mut removed: u64 = 0;
        self.counters.retain(|_model, counter| {
            let last = counter.last_used_at_ms.load(std::sync::atomic::Ordering::Acquire);
            if last == 0 {
                // Never recorded — keep conservative (e.g. created but never recorded yet).
                return true;
            }
            if now.saturating_sub(last) > threshold_ms {
                removed += 1;
                return false;
            }
            true
        });
        if removed > 0 {
            metrics::counter!("gw_routing_model_frequency_sweep_removed_total").increment(removed);
        }
        removed
    }
}
```

- [ ] **Step 5: Add `sweep_stale_instances` to `ReservationGlobalView`**

```rust
impl ReservationGlobalView {
    /// Evict `by_model_instance` entries whose `ts_ms` is older than
    /// `now - threshold_ms`. Returns the count of evicted entries.
    /// `by_model` is recomputed from the remaining instances per affected
    /// model.
    pub fn sweep_stale_instances(&self, threshold_ms: u64) -> u64 {
        let now = now_ms();
        let mut removed: u64 = 0;
        let mut affected_models: std::collections::HashSet<ModelName> = Default::default();
        self.by_model_instance.retain(|(model, _instance), rate| {
            if now.saturating_sub(rate.ts_ms) > threshold_ms {
                removed += 1;
                affected_models.insert(model.clone());
                return false;
            }
            true
        });
        for model in affected_models {
            self.recompute_model(&model);
        }
        if removed > 0 {
            metrics::counter!("gw_routing_reservation_sweep_removed_total").increment(removed);
        }
        removed
    }
}
```

- [ ] **Step 6: Run the tests**

```bash
cargo test -p gw-routing --lib reservation::tests
```
Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-routing/src/reservation.rs
git commit -m "$(cat <<'EOF'
fix(gw-routing): add sweep_stale for ModelFrequencyTracker and ReservationGlobalView

ModelFrequencyCounter gains a last_used_at_ms timestamp written by
record_pick. ModelFrequencyTracker::sweep_stale evicts counters whose
last_used_at_ms is older than now - threshold_ms.

ReservationGlobalView::sweep_stale_instances uses the existing
InstanceRate.ts_ms (set by apply_event) to evict instances not heard
from since now - threshold_ms; recomputes the by_model rollup for
affected models.

Both methods emit a sweep_removed_total counter. Called per tick by
routing_sweep (next commit).

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: `routing_sweep::spawn` + bootstrap wiring

**Files:** `crates/gw-routing/src/routing_sweep.rs` (new), `crates/gw-routing/src/lib.rs`, `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: Failing test (shutdown cancellation)**

Create `crates/gw-routing/src/routing_sweep.rs` with ONLY:

```rust
//! Periodic sweep of in-memory routing state. Supervisor-wrapped.
//!
//! Sweeps:
//! - `ModelFrequencyTracker::sweep_stale`
//! - `ReservationGlobalView::sweep_stale_instances`
//! - `LatencyTracker::evict_stale`
//! - `AffinityManager::sweep_expired`

use crate::affinity::AffinityManager;
use crate::latency::LatencyTracker;
use crate::reservation::{ModelFrequencyTracker, ReservationGlobalView};
use gw_runtime::{RestartPolicy, ShutdownToken, Supervisor};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tracing::debug;

const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const STALE_THRESHOLD_MS: u64 = 3_600_000; // 1h default

pub fn spawn(
    freq: Arc<ModelFrequencyTracker>,
    view: Arc<ReservationGlobalView>,
    latency: Arc<LatencyTracker>,
    affinity: Arc<AffinityManager>,
    shutdown: ShutdownToken,
) -> JoinHandle<()> {
    Supervisor::new("routing_sweep", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let freq = freq.clone();
        let view = view.clone();
        let latency = latency.clone();
        let affinity = affinity.clone();
        async move {
            let mut tick = tokio::time::interval(SWEEP_INTERVAL);
            tick.tick().await; // skip immediate first tick
            loop {
                tokio::select! {
                    _ = tok.cancelled() => {
                        tracing::info!(
                            target: "gw_routing::routing_sweep",
                            "shutdown requested"
                        );
                        return Ok(());
                    }
                    _ = tick.tick() => {
                        let freq_removed = freq.sweep_stale(STALE_THRESHOLD_MS);
                        let view_removed = view.sweep_stale_instances(STALE_THRESHOLD_MS);
                        latency.evict_stale(Duration::from_millis(STALE_THRESHOLD_MS));
                        let affinity_removed = affinity.sweep_expired(Instant::now());
                        if freq_removed > 0 || view_removed > 0 || affinity_removed > 0 {
                            debug!(
                                target: "gw_routing::routing_sweep",
                                freq_removed,
                                view_removed,
                                affinity_removed,
                                "routing sweep tick"
                            );
                        }
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_respects_shutdown_token() {
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let handle = spawn(
            Arc::new(ModelFrequencyTracker::default()),
            Arc::new(ReservationGlobalView::default()),
            Arc::new(LatencyTracker::default()),
            Arc::new(AffinityManager::default()),
            token,
        );
        coord.token().cancel();
        let exit = tokio::time::timeout(Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "routing_sweep did not exit within 5s of shutdown");
    }
}
```

If any of `ModelFrequencyTracker::default()`, `ReservationGlobalView::default()`, `LatencyTracker::default()`, `AffinityManager::default()` don't exist or don't compile, use whatever constructor IS available. Check each first.

- [ ] **Step 2: Add module declaration**

```bash
grep -n "pub mod " crates/gw-routing/src/lib.rs | head -10
```

Add `pub mod routing_sweep;` alongside.

- [ ] **Step 3: Build + run new test**

```bash
cargo build -p gw-routing
cargo test -p gw-routing --lib routing_sweep::tests::spawn_respects_shutdown_token
```
Expected: PASS.

- [ ] **Step 4: Wire into bootstrap**

Find where the routing-related Arcs are constructed in `crates/gw-gateway/src/lib.rs`:

```bash
grep -n "ModelFrequencyTracker\|ReservationGlobalView\|LatencyTracker\|AffinityManager" crates/gw-gateway/src/lib.rs | head -20
```

Locate the bootstrap section. After all four Arcs are available, add:

```rust
let routing_sweep_handle = gw_routing::routing_sweep::spawn(
    model_frequency.clone(),
    reservation_view.clone(),
    latency_tracker.clone(),
    affinity.clone(),
    shutdown_token.clone(),
);
shutdown.register(routing_sweep_handle);
```

Match the actual variable names in lib.rs — they may differ.

- [ ] **Step 5: Build + run gw-gateway tests**

```bash
cargo build -p gw-gateway
cargo test -p gw-gateway
```
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-routing/src/routing_sweep.rs crates/gw-routing/src/lib.rs crates/gw-gateway/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(gw-routing,gw-gateway): add routing_sweep Supervisor task + wire bootstrap

New crates/gw-routing/src/routing_sweep.rs spawns a Supervisor-wrapped
60s tick that calls:
- ModelFrequencyTracker::sweep_stale
- ReservationGlobalView::sweep_stale_instances
- LatencyTracker::evict_stale (pre-existing method, finally called)
- AffinityManager::sweep_expired (pre-existing method, finally called)

Bootstrap calls spawn(...) and registers the JoinHandle on
ShutdownCoordinator alongside other Supervisor-wrapped tasks.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Docs sync

**Files:** `docs/spec/<routing-file>.md`, `docs/architecture.md`, `docs/ROADMAP.md`

- [ ] **Step 1: Locate the routing spec file**

```bash
ls docs/spec/ | grep -i routing
```

Use whatever name actually exists. The plan calls it `<routing-file>`.

- [ ] **Step 2: Update the routing spec**

Append (or insert in the appropriate sub-section):

> **Routing sweep cadence.** A `routing_sweep` background task (Supervisor-wrapped, 60s tick, ShutdownCoordinator-registered) periodically evicts:
> - `ModelFrequencyTracker.counters` entries whose `last_used_at_ms` is older than `now - 1h` (default).
> - `ReservationGlobalView.by_model_instance` entries whose `ts_ms` (set by `apply_event` from the Redis events:reservation stream) is older than `now - 1h`. `by_model` is recomputed for affected models.
> - `LatencyTracker` stale per-key state (existing `evict_stale` method, now finally called).
> - `AffinityManager` expired bindings (existing `sweep_expired` method, now finally called).
>
> **`InFlightStore::count` semantic.** `count(k)` returns 0 for an unknown key WITHOUT creating an entry. Entries are created only on the acquire path (`get_or_create`). This change is observable: a read-only `count` query no longer allocates.

- [ ] **Step 3: Update `docs/architecture.md` §Deliberate behaviours**

Append:

> **Routing sweep evicts on staleness, not capacity.** All four routing-side sweeps use age-based thresholds (default 1h) instead of LRU caps. Cardinality is bounded by configured key/model count + observed instance count from the Redis events:reservation stream; per-instance cardinality is recovered as remote instances time out. `crates/gw-routing/src/routing_sweep.rs`.

> **`InFlightStore::count` is non-allocating.** A read-only query for an unknown key returns 0 without inserting. Avoids the previous failure mode where any query for an arbitrary KeyId would create a counter entry forever. `crates/gw-routing/src/in_flight.rs`.

- [ ] **Step 4: Update `docs/ROADMAP.md`**

Append:

> - 2026-06-12 — Closed: gw-routing sweep task (`ModelFrequencyTracker`, `ReservationGlobalView`, `LatencyTracker`, `AffinityManager` all swept per 60s tick); `InFlightStore.count` non-allocating; `UNKNOWN_PRESETS_WARNED` capped via Moka. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 5.

- [ ] **Step 5: CLAUDE.md ⇄ AGENTS.md check**

```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "BYTE-IDENTICAL BODY"
```

- [ ] **Step 6: Commit docs**

```bash
git add docs/spec docs/architecture.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync spec/architecture/roadmap with routing_sweep + InFlightStore semantic

- spec/<routing>.md: document the 4-target routing_sweep, the
  InFlightStore.count non-allocating semantic.
- architecture.md §Deliberate behaviours: 2 new entries (routing sweep
  is staleness-based, InFlightStore.count non-allocating).
- ROADMAP.md: close Crate 5 items.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Completion gate (orchestrator-run, SEQUENTIAL)

- [ ] `cargo test -p gw-routing` PASS
- [ ] `cargo test -p gw-gateway` PASS
- [ ] `cargo fmt --all -- --check` (fix with `cargo fmt --all` + `chore: cargo fmt` commit if dirty)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` (sequential)
- [ ] `bash scripts/check-invariants.sh` (sequential)

---

## Task 8: Merge to local main (orchestrator)

```bash
git checkout main
git merge --no-ff fix/memory-leaks-gw-routing -m "Merge Crate 5: gw-routing sweep + InFlightStore.count semantic"
```

Then move to Crate 6 planning.

---

## Self-Review

**1. Spec coverage** — every Crate 5 item:
- `ModelFrequencyTracker.counters` → Task 4
- `ReservationGlobalView` (both maps) → Task 4
- `InFlightStore.count` semantic → Task 2
- `LatencyTracker.evict_stale` wired → Task 5
- `AffinityManager.sweep_expired` wired → Task 5
- `UNKNOWN_PRESETS_WARNED` → Task 3
- ✅ Complete.

**2. Test count** — 5 new tests planned, budget 8. Margin 3.

**3. Placeholder scan** — `<routing-file>` placeholder in Task 6 — implementer resolves via Task 1 Step 3.

**4. Type consistency** — `last_used_at_ms: AtomicU64`, `sweep_stale(threshold_ms: u64) -> u64`, `sweep_stale_instances(threshold_ms: u64) -> u64`, `routing_sweep::spawn(...)` signature matches `inflight_reaper`/`DeductGc`/`ledger_sweep` pattern.

---

## Notes for the Implementer

- This is sonnet-appropriate work; the patterns (Supervisor wrap, sweep methods, Moka cap) all mirror Crate 2/3/4 work.
- ModelFrequencyCounter may have other consumers reading fields you didn't touch — verify the new `last_used_at_ms` field doesn't break a `Default` derive or other trait impl.
- The `routing_sweep::spawn` signature takes 4 Arcs — keep them in the order specified. If the bootstrap doesn't have matching Arcs handy, that's a sign the wiring needs adjustment.
- BLOCKED triggers: pre-impl verification reveals the work is done; ModelFrequencyCounter struct layout has changed unexpectedly; bootstrap Arcs not available with matching types.
