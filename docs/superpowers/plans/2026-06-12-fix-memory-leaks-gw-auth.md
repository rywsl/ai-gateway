# Crate 4: gw-auth — Memory Leak Fixes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace 8 unbounded in-memory maps in gw-auth with Moka caches (capacity + TTI), and wire two existing-but-dead sweep helpers into bootstrap.

**Architecture:** `RedosTracker` (`Mutex<HashMap>`) + 5 `RateLimitGuard` DashMaps + 2 `AdaptiveRpmManager` DashMaps all become `moka::sync::Cache` with capacity + idle TTI. `spawn_ip_ratelimit_sweep` (currently `#[allow(dead_code)]`) loses the attr and is called from bootstrap; a sibling `spawn_auth_failure_ip_sweep` is added and wired the same way. All sweep tasks register their `JoinHandle` with `ShutdownCoordinator`.

**Tech Stack:** Rust 1.88.0, `moka::sync::Cache` (already a workspace dep), `gw-runtime::Supervisor` (existing), `tokio`, `tracing`, `metrics`. No new workspace deps.

**Spec reference:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 4.

**Branch:** `fix/memory-leaks-gw-auth` cut from local `main` (which includes Crate 3 merge `93e3a84`).

**Test budget**: ≤ 8 new tests total (user-mandated minimalism).

---

## Critical Behavior Change to Document

> **After Moka replacement: TTI boundary creates a "fresh window".** A user who exhausts their RPM, idles longer than the TTI, then returns gets a fresh `DefaultDirectRateLimiter` instance with a clean window. Same for IP-based limiters and the adaptive RPM state. This is a deliberate semantic change documented in `docs/spec/05-rate-limit.md` and `docs/architecture.md` §Deliberate behaviours. Spec compliance review must verify both docs reflect this exactly.

---

## Pre-Implementation Findings (scope-correction)

Empirically verified on `main`:

| Item | Status | Action |
|------|--------|--------|
| `RedosTracker` (`model_permissions.rs:122`) | Confirmed unbounded `Mutex<HashMap>` | Replace with `Cache<String, Arc<TrackerEntry>>` |
| `RateLimitGuard` 5 DashMaps (`ratelimit/mod.rs:28-32`) | Confirmed all 5 unbounded | Replace each with `Cache<...>` |
| `AdaptiveRpmManager.evict()` | Event-driven only (called on `vendor_key_invalidate`), no periodic | Replace 2 DashMaps with `Cache<...>`; remove the need for periodic evict |
| `rpm_decay` Supervisor | **Already wired** (`gw-gateway/src/lib.rs:1651`) — calls `tick_decay()`, not `evict()` | No change needed (tick_decay is for confidence decay, separate concern) |
| `spawn_ip_ratelimit_sweep` (`gw-gateway/src/lib.rs:1668`) | Function exists with `#[allow(dead_code)]`, never called | Remove dead_code attr, call in bootstrap |
| `AuthFailureIpLimiter::sweep` (`auth_ip_ratelimit.rs:82`) | Method exists, no helper, no caller | Add `spawn_auth_failure_ip_sweep` helper, call in bootstrap |

Net effect: 8 cache migrations + 2 bootstrap wirings + 1 new helper function.

---

## File Structure

**Modified files:**
- `crates/gw-auth/src/model_permissions.rs` — `RedosTracker` → Moka
- `crates/gw-auth/src/ratelimit/mod.rs` — `RateLimitGuard` 5 DashMaps → Moka
- `crates/gw-auth/src/ratelimit/adaptive.rs` — `AdaptiveRpmManager` 2 DashMaps → Moka
- `crates/gw-auth/Cargo.toml` — add `moka` if not present (verify; the workspace has it)
- `crates/gw-gateway/src/lib.rs` — remove `#[allow(dead_code)]` from `spawn_ip_ratelimit_sweep`, call it from bootstrap; add `spawn_auth_failure_ip_sweep` helper and call it; register both handles with `ShutdownCoordinator`
- `docs/spec/05-rate-limit.md` — document TTI-boundary semantic change
- `docs/architecture.md` §Deliberate behaviours — entry for "fresh window after TTI"
- `docs/ROADMAP.md` — close items

**Test files:**
- `crates/gw-auth/src/model_permissions.rs` mod tests — 1 new test (RedosTracker TTI eviction)
- `crates/gw-auth/src/ratelimit/mod.rs` mod tests — 1 new test (RPM TTI boundary — the critical "fresh window" behavior)
- `crates/gw-auth/src/ratelimit/adaptive.rs` mod tests — 1 new test (AdaptiveRpmManager LRU at capacity)
- `crates/gw-gateway/src/lib.rs` mod tests — 2 new tests (one for each sweep spawn function: shutdown-token cancellation)
- **Plus 3 conditional**: only add if behavior gaps remain uncovered

**Test budget cap: 8 new tests.** If you find yourself approaching 9+, stop and surface.

**No new workspace deps.**

---

## Task 1: Branch Setup + Verification

- [ ] **Step 1: Cut branch**

```bash
git checkout main
git status   # expect: ahead of origin, clean
git log --oneline -3   # expect 93e3a84 (Crate 3 merge) at top
git checkout -b fix/memory-leaks-gw-auth
```

- [ ] **Step 2: Verify the scope items haven't moved**

```bash
grep -n "Mutex<HashMap" crates/gw-auth/src/model_permissions.rs
```
Expected: line ~123 still uses `Mutex<HashMap<String, TrackerEntry>>`.

```bash
grep -n "DashMap" crates/gw-auth/src/ratelimit/mod.rs crates/gw-auth/src/ratelimit/adaptive.rs
```
Expected: 7 hits across the two files (5 in mod.rs at the RateLimitGuard struct + 2 in adaptive.rs at AdaptiveRpmManager).

```bash
grep -n "#\[allow(dead_code)\]" crates/gw-gateway/src/lib.rs
```
Expected: at least one hit on line 1667 above `spawn_ip_ratelimit_sweep`.

```bash
grep -n "moka" crates/gw-auth/Cargo.toml
```
If empty: moka must be added under `[dependencies]` (workspace already has it; just `moka = { workspace = true }`).

If any verification fails: STOP and surface to orchestrator.

---

## Task 2: RedosTracker → Moka cache

**Files:** `crates/gw-auth/src/model_permissions.rs`, `crates/gw-auth/Cargo.toml`

- [ ] **Step 1: Add moka to gw-auth Cargo.toml if missing**

```bash
grep "^moka" crates/gw-auth/Cargo.toml
```

If missing, add under `[dependencies]`: `moka = { workspace = true }`.

- [ ] **Step 2: Write the failing test first**

Append to the existing `mod tests` in `model_permissions.rs` (or add `#[cfg(test)] mod tests` block at end if none exists):

```rust
    use std::time::Duration;

    #[test]
    fn redos_tracker_evicts_via_lru_at_capacity() {
        let tracker = RedosTracker::with_capacity(4);
        // Push past capacity — earliest entries should LRU-evict.
        for i in 0..10 {
            let key = format!("ak-{i}");
            tracker.record_timeout(&key);
        }
        // Confirm at most `capacity` entries are retained.
        // Moka may sync lazily; force pending tasks to settle before assertion.
        tracker.sync_for_test();
        assert!(tracker.len_for_test() <= 4);
    }
```

This test requires:
- `RedosTracker::with_capacity(cap: usize) -> Self` (new constructor)
- `RedosTracker::sync_for_test() -> ()` (test helper that calls moka's `run_pending_tasks`)
- `RedosTracker::len_for_test() -> usize` (test helper that returns `entry_count`)

- [ ] **Step 3: Run and verify compile failure**

```bash
cargo test -p gw-auth --lib model_permissions::tests::redos_tracker_evicts_via_lru_at_capacity 2>&1 | tail -10
```

Expected: compile errors — `with_capacity` not defined.

- [ ] **Step 4: Implement the Moka replacement**

Replace the existing `RedosTracker` block (around lines 105-164). The new module preamble + struct + impl:

```rust
use moka::sync::Cache;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Per-call regex match budget. Matches over `BUDGET_MS` are recorded as
/// timeouts via [`RedosTracker::record_timeout`]; once an ApiKey accumulates
/// `TIMEOUT_THRESHOLD` timeouts, [`RedosTracker::is_disabled`] returns true
/// and [`check_with_budget`] short-circuits to `None`.
pub const BUDGET_MS: u128 = 100;

/// Number of regex-match timeouts per ApiKey before that key is auto-disabled.
const TIMEOUT_THRESHOLD: u32 = 10;

/// Default cache capacity (api-key cardinality). 100_000 covers typical
/// production loads; an LRU-evicted attacker key only loses its "disabled"
/// state after sustained idle exceeding TTI, which is acceptable for a
/// defense-in-depth tracker.
const DEFAULT_REDOS_CAPACITY: usize = 100_000;

/// Idle TTI for tracker entries. A disabled key that has not been seen for
/// this long is reset on re-entry (re-tested against the threshold). 1h is
/// long enough that legitimate keys are not punished; short enough that an
/// attacker pattern is not permanent.
const DEFAULT_REDOS_TTI: Duration = Duration::from_secs(3600);

/// Per-ApiKey timeout counter + disable flag. Backed by a bounded `moka::sync::Cache`
/// so the entry set is bounded by capacity (LRU) and idle TTI.
pub struct RedosTracker {
    inner: Cache<String, Arc<TrackerEntry>>,
}

#[derive(Default)]
struct TrackerEntry {
    timeout_count: AtomicU32,
    disabled: AtomicBool,
}

impl Default for RedosTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RedosTracker {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_REDOS_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(capacity as u64)
                .time_to_idle(DEFAULT_REDOS_TTI)
                .build(),
        }
    }

    pub fn timeout_count(&self, ak: &str) -> u32 {
        self.inner
            .get(&ak.to_string())
            .map(|e| e.timeout_count.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    pub fn is_disabled(&self, ak: &str) -> bool {
        self.inner
            .get(&ak.to_string())
            .map(|e| e.disabled.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    pub fn record_timeout(&self, ak: &str) {
        let entry = self
            .inner
            .get_with(ak.to_string(), || Arc::new(TrackerEntry::default()));
        let new_count = entry.timeout_count.fetch_add(1, Ordering::AcqRel) + 1;
        if new_count >= TIMEOUT_THRESHOLD {
            entry.disabled.store(true, Ordering::Release);
        }
        metrics::counter!("gw_redos_timeout_total",
            "api_key_id" => ak.to_string())
        .increment(1);
    }

    #[cfg(test)]
    pub fn sync_for_test(&self) {
        self.inner.run_pending_tasks();
    }

    #[cfg(test)]
    pub fn len_for_test(&self) -> usize {
        self.inner.run_pending_tasks();
        self.inner.entry_count() as usize
    }
}
```

Drop the old `use parking_lot::Mutex;` and `use std::collections::HashMap;` imports if they were only for `RedosTracker`.

- [ ] **Step 5: Run the test**

```bash
cargo test -p gw-auth --lib model_permissions::tests::redos_tracker_evicts_via_lru_at_capacity
```

Expected: PASS.

- [ ] **Step 6: Run all model_permissions tests for regression**

```bash
cargo test -p gw-auth --lib model_permissions::tests
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-auth/src/model_permissions.rs crates/gw-auth/Cargo.toml
git commit -m "$(cat <<'EOF'
fix(gw-auth): bound RedosTracker via Moka cache

Replaces the unbounded `Mutex<HashMap<String, TrackerEntry>>` with a
`moka::sync::Cache<String, Arc<TrackerEntry>>` (max_capacity 100_000,
time_to_idle 1h). The cache is LRU-evicted at capacity and expires
idle entries — both bound the keyspace against attacker-amplified
keys (audit 2026-06-12 P0).

TrackerEntry's counter and disable flag are now atomics so cache
reads can return a shared `Arc<TrackerEntry>` without re-locking the
cache for the increment.

Semantic change: a disabled key that idles past TTI is re-tested on
re-entry rather than staying disabled forever. Documented as
deliberate behaviour in subsequent docs commit.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: RateLimitGuard 5 DashMaps → Moka caches

**Files:** `crates/gw-auth/src/ratelimit/mod.rs`

- [ ] **Step 1: Write the critical failing test (TTI boundary)**

The behavior under test: after exhausting RPM, idling past TTI, then returning gets a fresh limiter. Append to the existing `#[cfg(test)] mod tests` (or add one if none):

```rust
    #[test]
    fn rpm_user_limiter_resets_after_tti_eviction() {
        // Build a guard with a TINY TTI so the test runs fast without sleeps.
        // Production TTI is 1h; test uses 50ms via the `with_tti` constructor.
        let guard = RateLimitGuard::with_tti_for_test(std::time::Duration::from_millis(50));
        let uid = gw_core::ids::UserId::from("u-rpm-tti");

        // Helper: call check() until it errors (exhaust). We rely on the
        // existing `check()` API — adjust to whatever it returns on success
        // vs. exhaustion in the codebase.
        let mut allowed = 0u32;
        for _ in 0..200 {
            let merged = test_merged_with_rpm(uid.clone(), 5);
            if guard.check(&merged).is_ok() {
                allowed += 1;
            }
        }
        assert!(allowed > 0 && allowed <= 60, "should exhaust at small RPM, got {allowed}");

        // Wait past TTI + small buffer.
        std::thread::sleep(std::time::Duration::from_millis(80));
        guard.sync_for_test();

        // The post-TTI request must see a FRESH limiter (deliberate semantic).
        let merged = test_merged_with_rpm(uid.clone(), 5);
        assert!(
            guard.check(&merged).is_ok(),
            "after TTI eviction, a returning user must get a fresh window"
        );
    }
```

`test_merged_with_rpm(uid, rpm)` is a test helper — write it ONCE at the top of the test module so it can be reused. Match it to the shape of `merged` that `check()` consumes; check existing tests in this file for the pattern.

If `RateLimitGuard::check` returns something different from what the snippet assumes, adapt the assertion — but the structural shape (exhaust → wait → fresh) must hold.

This SINGLE test is sufficient for the TTI boundary behavior; we do NOT add tests for each of the 5 DashMaps individually (test budget mandate).

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p gw-auth --lib ratelimit::tests::rpm_user_limiter_resets_after_tti_eviction 2>&1 | tail -10
```

Expected: compile error — `with_tti_for_test` not defined.

- [ ] **Step 3: Implement the Moka replacement**

Replace the 5 DashMap fields in `RateLimitGuard` with `Cache<...>` equivalents. The new struct:

```rust
use moka::sync::Cache;

const DEFAULT_RATE_LIMIT_TTI: std::time::Duration = std::time::Duration::from_secs(3600);
const DEFAULT_RATE_LIMIT_CAPACITY_USERS: u64 = 100_000;
const DEFAULT_RATE_LIMIT_CAPACITY_KEYS: u64 = 10_000;

pub struct RateLimitGuard {
    pub per_user: Cache<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_tpm: Cache<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_streams: Cache<UserId, Arc<AtomicUsize>>,
    pub per_key: Cache<KeyId, (u32, Arc<DefaultDirectRateLimiter>)>,
    pub per_key_overrides: Cache<KeyId, u32>,
}
```

Adjust the `new()` constructor:

```rust
impl RateLimitGuard {
    pub fn new() -> Self {
        Self::with_tti(DEFAULT_RATE_LIMIT_TTI)
    }

    pub fn with_tti(tti: std::time::Duration) -> Self {
        Self {
            per_user: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_USERS)
                .time_to_idle(tti)
                .build(),
            per_user_tpm: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_USERS)
                .time_to_idle(tti)
                .build(),
            per_user_streams: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_USERS)
                .time_to_idle(tti)
                .build(),
            per_key: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_KEYS)
                .time_to_idle(tti)
                .build(),
            per_key_overrides: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_KEYS)
                .time_to_idle(tti)
                .build(),
        }
    }

    #[cfg(test)]
    pub fn with_tti_for_test(tti: std::time::Duration) -> Self {
        Self::with_tti(tti)
    }

    #[cfg(test)]
    pub fn sync_for_test(&self) {
        self.per_user.run_pending_tasks();
        self.per_user_tpm.run_pending_tasks();
        self.per_user_streams.run_pending_tasks();
        self.per_key.run_pending_tasks();
        self.per_key_overrides.run_pending_tasks();
    }
}
```

For every site in the file where `DashMap::entry(...).or_insert_with(...)` or `DashMap::get(...)` is used on these 5 fields, replace with the Moka equivalents:
- `dashmap.entry(k).or_insert_with(|| v).value().clone()` → `cache.get_with(k, || v)`
- `dashmap.get(&k).map(|x| x.clone())` → `cache.get(&k)`
- `dashmap.insert(k, v)` → `cache.insert(k, v)`
- `dashmap.remove(&k)` → `cache.invalidate(&k)`

Read the existing call sites carefully and adapt one at a time, then build between each.

- [ ] **Step 4: Build to verify**

```bash
cargo build -p gw-auth
```

Expected: clean. If errors mention DashMap-specific methods (e.g. `.iter()`, `.len()`), adapt to Moka equivalents (`.iter()`, `.entry_count()` after `run_pending_tasks()`).

- [ ] **Step 5: Run the new TTI boundary test**

```bash
cargo test -p gw-auth --lib ratelimit::tests::rpm_user_limiter_resets_after_tti_eviction
```

Expected: PASS.

- [ ] **Step 6: Run all gw-auth tests for regression**

```bash
cargo test -p gw-auth
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-auth/src/ratelimit/mod.rs
git commit -m "$(cat <<'EOF'
fix(gw-auth): bound RateLimitGuard 5 maps via Moka

Replaces the 5 unbounded DashMap fields (per_user, per_user_tpm,
per_user_streams, per_key, per_key_overrides) with bounded
`moka::sync::Cache` (max_capacity + time_to_idle). The capacity is
100_000 for user-keyed caches and 10_000 for key-keyed caches; TTI
defaults to 1 hour and is injectable via `with_tti` for testing.

Semantic change: a user (or key) that exhausts RPM and then idles
longer than TTI is given a fresh `DefaultDirectRateLimiter` on next
request. This is deliberate — bounded keyspace beats persistent
sliding-window state for protected users that disappear forever.
Documented as deliberate behaviour in subsequent docs commit.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: AdaptiveRpmManager 2 DashMaps → Moka caches

**Files:** `crates/gw-auth/src/ratelimit/adaptive.rs`

- [ ] **Step 1: Write the failing test (LRU at capacity)**

Append to the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn adaptive_state_evicts_via_lru_at_capacity() {
        let manager = AdaptiveRpmManager::with_capacity_for_test(8);
        // Insert past capacity.
        for i in 0..20 {
            let key = gw_core::ids::KeyId::from(format!("k-{i}"));
            manager.observe_429_with_kind(
                &key,
                100,
                gw_core::ratelimit::RateLimitKind::Rpm,
            );
        }
        manager.sync_for_test();
        // Confirm bounded.
        assert!(manager.state_len_for_test() <= 8);
        assert!(manager.limiters_len_for_test() <= 8);
    }
```

This requires `with_capacity_for_test`, `sync_for_test`, `state_len_for_test`, `limiters_len_for_test` helpers.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p gw-auth --lib ratelimit::adaptive::tests::adaptive_state_evicts_via_lru_at_capacity 2>&1 | tail -10
```

Expected: compile error.

- [ ] **Step 3: Implement the Moka replacement**

Replace the 2 DashMap fields in `AdaptiveRpmManager`:

```rust
const DEFAULT_ADAPTIVE_CAPACITY: u64 = 10_000;
const DEFAULT_ADAPTIVE_TTI: std::time::Duration = std::time::Duration::from_secs(3600);

pub struct AdaptiveRpmManager {
    pub state: moka::sync::Cache<KeyId, Arc<KeyAdaptiveState>>,
    limiters: moka::sync::Cache<KeyId, (u32, Arc<DefaultDirectRateLimiter>)>,
    cfg: AdaptiveCfg,
}
```

Adjust the constructor to take capacity / TTI:

```rust
impl AdaptiveRpmManager {
    pub fn new(cfg: AdaptiveCfg) -> Self {
        Self::with_capacity_and_tti(cfg, DEFAULT_ADAPTIVE_CAPACITY, DEFAULT_ADAPTIVE_TTI)
    }

    pub fn with_capacity_and_tti(
        cfg: AdaptiveCfg,
        capacity: u64,
        tti: std::time::Duration,
    ) -> Self {
        Self {
            state: moka::sync::Cache::builder()
                .max_capacity(capacity)
                .time_to_idle(tti)
                .build(),
            limiters: moka::sync::Cache::builder()
                .max_capacity(capacity)
                .time_to_idle(tti)
                .build(),
            cfg,
        }
    }

    #[cfg(test)]
    pub fn with_capacity_for_test(capacity: u64) -> Self {
        Self::with_capacity_and_tti(AdaptiveCfg::default(), capacity, DEFAULT_ADAPTIVE_TTI)
    }

    #[cfg(test)]
    pub fn sync_for_test(&self) {
        self.state.run_pending_tasks();
        self.limiters.run_pending_tasks();
    }

    #[cfg(test)]
    pub fn state_len_for_test(&self) -> usize {
        self.state.run_pending_tasks();
        self.state.entry_count() as usize
    }

    #[cfg(test)]
    pub fn limiters_len_for_test(&self) -> usize {
        self.limiters.run_pending_tasks();
        self.limiters.entry_count() as usize
    }
}
```

Replace `DashMap` operations throughout the file with Moka equivalents (same patterns as Task 3).

`AdaptiveCfg::default()` may not exist — if missing, derive a sensible default for tests (a `#[cfg(test)] impl Default for AdaptiveCfg { ... }` block).

- [ ] **Step 4: Build, run new test, run all adaptive tests**

```bash
cargo build -p gw-auth
cargo test -p gw-auth --lib ratelimit::adaptive::tests
```

Expected: clean build, all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-auth/src/ratelimit/adaptive.rs
git commit -m "$(cat <<'EOF'
fix(gw-auth): bound AdaptiveRpmManager 2 maps via Moka

Replaces the unbounded `state` and `limiters` DashMaps with bounded
`moka::sync::Cache` (max_capacity 10_000, time_to_idle 1 hour). The
event-driven `evict(key_id)` (called from gw-gateway on vendor key
invalidation) remains intact; what changes is that keys that fall
quiet beyond TTI now LRU-evict naturally instead of accumulating.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Wire IP sweep spawn functions

**Files:** `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: Write 2 failing tests**

Append to the existing `#[cfg(test)] mod` (or the most appropriate test module — check existing structure with `grep -n "mod adaptive_rpm_tests\|#\[cfg(test)\]" crates/gw-gateway/src/lib.rs`):

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn ip_ratelimit_sweep_respects_shutdown_token() {
        use gw_runtime::ShutdownCoordinator;
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let guard = std::sync::Arc::new(IpRateLimitGuard::default());
        let handle = spawn_ip_ratelimit_sweep(guard, 60, token);
        coord.token().cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "ip ratelimit sweep should exit within 5s of shutdown");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auth_failure_ip_sweep_respects_shutdown_token() {
        use gw_runtime::ShutdownCoordinator;
        let coord = ShutdownCoordinator::default();
        let token = coord.token();
        let guard = std::sync::Arc::new(AuthFailureIpLimiter::new(5, 1000, true));
        let handle = spawn_auth_failure_ip_sweep(guard, 60, token);
        coord.token().cancel();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(exit.is_ok(), "auth failure IP sweep should exit within 5s of shutdown");
    }
```

`IpRateLimitGuard::default()` and `AuthFailureIpLimiter::new(...)` — verify the signatures via `cargo check` first.

- [ ] **Step 2: Run and verify failure**

```bash
cargo test -p gw-gateway --lib auth_failure_ip_sweep_respects_shutdown_token 2>&1 | tail -10
```

Expected: compile error — `spawn_auth_failure_ip_sweep` not defined.

- [ ] **Step 3: Remove `#[allow(dead_code)]` from `spawn_ip_ratelimit_sweep`**

Find the attribute at line 1667 (or wherever it is now):

```bash
grep -n "#\[allow(dead_code)\]" crates/gw-gateway/src/lib.rs | head -5
```

Delete the attribute line above `fn spawn_ip_ratelimit_sweep(`.

- [ ] **Step 4: Add `spawn_auth_failure_ip_sweep` next to `spawn_ip_ratelimit_sweep`**

After the `spawn_ip_ratelimit_sweep` function block, insert:

```rust
fn spawn_auth_failure_ip_sweep(
    guard: Arc<AuthFailureIpLimiter>,
    sweep_interval_secs: u64,
    token: gw_runtime::ShutdownToken,
) -> tokio::task::JoinHandle<()> {
    Supervisor::new("auth_failure_ip_sweep", RestartPolicy::default(), token).spawn(move |tok| {
        let guard = guard.clone();
        let mut interval = tokio::time::interval(Duration::from_secs(sweep_interval_secs));
        async move {
            loop {
                tokio::select! {
                    _ = tok.cancelled() => return Ok(()),
                    _ = interval.tick() => {
                        let removed = guard.sweep();
                        if removed > 0 {
                            tracing::info!(
                                target: "gw_gateway",
                                removed,
                                "auth failure IP limiter sweep evicted idle entries"
                            );
                        }
                    }
                }
            }
        }
    })
}
```

- [ ] **Step 5: Wire both into bootstrap**

Find where `ip_ratelimit` is constructed (around line 823). After that block (and after wherever `auth_failure_ip_limiter` is constructed), add the two spawn calls + register both handles. Approximate location: near other `Supervisor`-wrapped spawn registrations.

Example sketch (adjust variable names to actual ones in lib.rs):

```rust
let ip_sweep_handle = spawn_ip_ratelimit_sweep(
    ip_ratelimit.clone(),
    cfg.security.ip_ratelimit_sweep_interval_secs,
    shutdown_token.clone(),
);
shutdown.register(ip_sweep_handle);

let auth_fail_sweep_handle = spawn_auth_failure_ip_sweep(
    auth_failure_ip_limiter.clone(),
    cfg.security.auth_failure_ip_sweep_interval_secs,
    shutdown_token.clone(),
);
shutdown.register(auth_fail_sweep_handle);
```

If the config doesn't have these fields, use literal defaults: `300` (5 min) for both. Surface as a DONE_WITH_CONCERNS note so the orchestrator can add config knobs in a follow-up if desired.

- [ ] **Step 6: Build and run the new tests**

```bash
cargo build -p gw-gateway
cargo test -p gw-gateway --lib ip_ratelimit_sweep_respects_shutdown_token auth_failure_ip_sweep_respects_shutdown_token
```

Expected: 2/2 PASS.

- [ ] **Step 7: Run gw-gateway full tests for regression**

```bash
cargo test -p gw-gateway
```

Expected: all PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/gw-gateway/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(gw-gateway): wire IP sweep helpers into bootstrap

Removes the `#[allow(dead_code)]` decoration on
`spawn_ip_ratelimit_sweep` and calls it from bootstrap. Adds a
sibling `spawn_auth_failure_ip_sweep` for the
`AuthFailureIpLimiter` and wires it the same way. Both
JoinHandles are registered with the `ShutdownCoordinator`.

Closes the "IP limiter sweep never runs" gap identified by the
audit. The actual sweep methods on the guards already existed.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Docs sync

**Files:** `docs/spec/05-rate-limit.md`, `docs/architecture.md`, `docs/ROADMAP.md`

- [ ] **Step 1: Update `docs/spec/05-rate-limit.md`**

Locate the RPM section:

```bash
grep -n "RateLimitGuard\|per_user\|per_key\|sliding window" docs/spec/05-rate-limit.md | head -10
```

Append (or insert near the relevant section):

> **Storage: bounded Moka cache, TTI-evicted.** All five rate-limit maps (`per_user`, `per_user_tpm`, `per_user_streams`, `per_key`, `per_key_overrides`) are stored in `moka::sync::Cache` with `max_capacity` (100_000 for user-keyed, 10_000 for key-keyed) and `time_to_idle` (1 hour). The adaptive RPM manager's `state` and `limiters` use the same construction.
>
> **Deliberate semantic at the TTI boundary**: a user (or key) that exhausts RPM, idles longer than TTI, then returns is given a FRESH `DefaultDirectRateLimiter` instance. The sliding-window state of the prior limiter is discarded. This is the trade-off for a bounded keyspace: a key that disappears forever is not punished forever. Operators concerned about this boundary can raise TTI in configuration (TODO: expose as a config knob; current default is hard-coded at 1 hour).

- [ ] **Step 2: Update `docs/architecture.md` §Deliberate behaviours**

Locate §Deliberate behaviours and add a row:

> **Rate limiters are TTI-bounded, not persistent.** RPM and TPM sliding-window state lives in `moka::sync::Cache` with `time_to_idle` (1 hour default). A user that exhausts RPM, idles past TTI, then returns receives a fresh window. The bounded keyspace beats persistent sliding-window state for users (or keys) that disappear forever. `crates/gw-auth/src/ratelimit/{mod,adaptive}.rs`, `crates/gw-auth/src/model_permissions.rs::RedosTracker`.

- [ ] **Step 3: Update `docs/ROADMAP.md`**

Append:

> - 2026-06-12 — Closed: bound `RedosTracker`, `RateLimitGuard` (5 maps), `AdaptiveRpmManager` (2 maps) with Moka caches; wire `spawn_ip_ratelimit_sweep` and new `spawn_auth_failure_ip_sweep` into bootstrap. See `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §Crate 4. Deliberate TTI-boundary semantic change recorded in §spec/05-rate-limit.md + §architecture.md.

- [ ] **Step 4: CLAUDE.md ⇄ AGENTS.md check**

```bash
diff <(tail -n +2 CLAUDE.md) <(tail -n +2 AGENTS.md) && echo "BYTE-IDENTICAL BODY"
```

Expected: `BYTE-IDENTICAL BODY`.

- [ ] **Step 5: Commit docs**

```bash
git add docs/spec/05-rate-limit.md docs/architecture.md docs/ROADMAP.md
git commit -m "$(cat <<'EOF'
docs: sync spec/architecture/roadmap with Moka rate-limit refactor

- spec/05-rate-limit.md: document Moka cache storage and the deliberate
  TTI-boundary "fresh window" semantic.
- architecture.md §Deliberate behaviours: add a row for the TTI-bounded
  rate limiters.
- ROADMAP.md: close Crate 4 items.

Co-Authored-By: Claude Sonnet 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Completion gate (orchestrator-run, SEQUENTIAL)

Per workflow rule: cargo commands MUST run serially. `check-invariants.sh` runs cargo internally; concurrent cargo invocations contend on the target lock.

- [ ] **Step 1: gw-auth tests**

```bash
cargo test -p gw-auth
```

- [ ] **Step 2: gw-gateway tests (after step 1 finishes)**

```bash
cargo test -p gw-gateway
```

- [ ] **Step 3: workspace fmt check**

```bash
cargo fmt --all -- --check
```

Fix with `cargo fmt --all` + amend if dirty.

- [ ] **Step 4: workspace clippy (sequential after fmt)**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 5: invariants (sequential after clippy)**

```bash
bash scripts/check-invariants.sh
```

---

## Task 8: Merge to local main (orchestrator)

```bash
git checkout main
git merge --no-ff fix/memory-leaks-gw-auth -m "Merge Crate 4: gw-auth Moka migration + sweep wiring"
```

Then move to Crate 5 planning.

---

## Self-Review

**1. Spec coverage** — every §Crate 4 item maps to a task:
- `RedosTracker` → Task 2
- `RateLimitGuard` 5 DashMaps → Task 3
- `AdaptiveRpmManager.evict()` (now handled by Moka TTI) → Task 4
- `spawn_ip_ratelimit_sweep` + new `spawn_auth_failure_ip_sweep` wired → Task 5
- TTI semantic doc → Task 6
- ✅ Complete.

**2. Test count** — planned **5 new tests** (1 RedosTracker + 1 RPM TTI + 1 Adaptive + 2 spawn shutdowns). Budget of 8 leaves margin for the implementer to add up to 3 more IF a sweep test in the existing IP sweep helper reveals a gap. Hard stop at 8 total.

**3. Placeholder scan** — none. All commands / code blocks are concrete.

**4. Type consistency** —
- `moka::sync::Cache<K, V>` used uniformly
- `DEFAULT_*_CAPACITY` / `DEFAULT_*_TTI` constants named consistently
- All `with_*_for_test` test helpers use `#[cfg(test)]`
- `run_pending_tasks()` is called before any `entry_count()` check (moka requires this for synchronous test assertions)

---

## Notes for the Implementer

- Moka's `sync::Cache` is the right choice (we're in mostly-sync code paths). Do NOT use `future::Cache` — that requires `.await`.
- `Cache::insert` and `Cache::invalidate` are sync; no await needed.
- For "iterate over all entries" patterns (rare in this code), call `cache.run_pending_tasks()` first then `cache.iter()`.
- The TTI test in Task 3 uses `std::thread::sleep(80ms)` — that's intentional. Moka does not honor `tokio::time::pause()` for TTI eviction; real wall time is needed.
- BLOCKED triggers: Task 5 Step 5 wiring requires identifiers (`ip_ratelimit`, `auth_failure_ip_limiter`, `cfg.security.*`) — if the names differ in lib.rs, use the actual names and note in DONE report.
- DONE_WITH_CONCERNS triggers: if the `RateLimitGuard::check` signature differs from what the test scaffold assumes; if `AdaptiveCfg::default()` does not exist and adding `Default` derive cascades to other crates.
