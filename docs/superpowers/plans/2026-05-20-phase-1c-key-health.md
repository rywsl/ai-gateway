# Phase 1c — gw-health + breaker.rs replacement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce `gw-health` business crate (per integration spec §2 表 A A2 + K-A) carrying the multi-dim `(KeyId, ApiFormat)` `KeyHealthStore`, split `gw-routing::breaker.rs`'s `in_flight` portion into `gw-routing::in_flight::InFlightStore`, delete the rest of `breaker.rs`, and migrate strategies + `gw-proxy` + `gw-gateway` to the new store pair. After this phase, `gw-routing` no longer carries any health/circuit logic — only `in_flight` for the LoadBalance hot-path.

**Architecture:** Three crate-level moves:

1. New supporting-layer crate `gw-health` owns the `(KeyId, ApiFormat) → FormatHealth` map. Each `FormatHealth` carries a 30s `SlidingWindow`, a 3-state circuit (`closed / open / half_open` per ADR-008), an explicit `cooldown_until_ms` deadline + `CooldownReason`, and a `consecutive_failures` counter. The store exposes `is_available`, `peek`, `record`, `apply_cooldown`, `clear_cooldown` per K-A §ADR-KA-3..5.
2. New module `gw-routing::in_flight` carries only the per-key `AtomicU32` counter + RAII `InFlightGuard`. `gw-routing::strategy::load_balance` and `gw-routing::candidates` switch to reading from it.
3. `gw-routing::breaker.rs` is deleted entirely. `Strategy::pick` signature changes from `(candidates, &BreakerStore, &PickCtx)` to `(candidates, &KeyHealthStore, &InFlightStore, &PickCtx)`; `PickCtx` grows an `api_format: ApiFormat` field so the health lookup can be multi-dim. All 8 strategy impls migrate in one commit.

Build-green discipline:

- T1, T2, T3 are **additive** (BreakerStore stays alive in parallel). Workspace stays green.
- T4 + T5 are the **migration chain** — Strategy trait signature change forces all 8 impls + candidates + proxy + gateway + tests + bench updates together. Workspace transiently red across T4 → T5.
- T6 deletes `breaker.rs` and is green-on-commit (only removals after T5 has migrated callers).
- T7 finalizes CLAUDE.md + lint activation + final verification.

**Tech Stack:** Rust 1.86.0, `dashmap`, `parking_lot`, `tokio`, `metrics`. No new external dependencies — `gw-health` reuses what `gw-routing::breaker` already pulled in.

**Authoritative references:**

- Integration spec: `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §2 表 A A2 + §3 C-1 (`gw-health` as business crate) + C-6 (`gw_health_*` metric prefix) + C-7 (`gw-health::KeyHealthStore` authoritative location) + §5.2 (`no_legacy_breaker` lint).
- K-A spec: `docs/superpowers/specs/2026-05-19-key-health-k-a-state-machine.md` — full data structure, ADR-KA-1 through ADR-KA-12.
- Memory: `feedback-breaking-changes-ok` — no `#[deprecated]`, no double-write window, one-shot rename.

**Phase tag note:** Integration spec §2.3 Bm2 marks `PickCtx::api_format` + `KeyHealthStore::is_available` filter as Phase 4. However, deleting `breaker.rs` (Phase 1 exit gate) forces strategies to switch to `KeyHealthStore::is_available` *now*, which forces `PickCtx::api_format` *now*. Phase 1c lands the field; Phase 4 RL-Core / R-AB build on top of it without further structural change. Add a Living Amendment to the integration spec §6 noting this re-timing.

---

## File map

**Created:**

- `crates/gw-health/Cargo.toml` — new business crate manifest.
- `crates/gw-health/src/lib.rs` — re-exports.
- `crates/gw-health/src/sliding_window.rs` — 30s `(Instant, success)` window (moved from `gw-routing::breaker::SlidingWindow`).
- `crates/gw-health/src/format_health.rs` — `FormatHealth` struct + `CircuitState` enum + 3-state CAS logic.
- `crates/gw-health/src/cooldown.rs` — `CooldownReason` (`u16` enum) + `CooldownDirective` struct + `CircuitState`.
- `crates/gw-health/src/store.rs` — `KeyHealthStore` + `HealthSnapshot` + the 5 public methods.
- `crates/gw-health/tests/key_health.rs` — unit tests for `is_available`, `record`, `apply_cooldown`, `peek`, multi-dim isolation.
- `crates/gw-routing/src/in_flight.rs` — `InFlightCounter` + `InFlightStore` + `InFlightGuard` (extracted from `breaker.rs`).
- `crates/gw-routing/tests/in_flight.rs` — minimal in-flight test (replaces the relevant assertions from `tests/breaker.rs`).

**Modified:**

- `Cargo.toml` (workspace root) — add `crates/gw-health` to `members`.
- `CLAUDE.md` — under "Crate layering rules (v2 integration)", register `gw-health` as a business-layer crate.
- `crates/gw-core/src/protocol.rs` — add `pub type ApiFormat = ProtocolKind;` alias (Bm5 v1 form).
- `crates/gw-routing/Cargo.toml` — add `gw-health = { path = "../gw-health" }`.
- `crates/gw-routing/src/lib.rs` — drop `pub mod breaker;` + `pub use breaker::*;`; add `pub mod in_flight;` + `pub use in_flight::{InFlightStore, InFlightGuard};`.
- `crates/gw-routing/src/strategy/mod.rs` — `PickCtx` adds `api_format: ApiFormat`; `Strategy::pick` signature changes to `(candidates, &KeyHealthStore, &InFlightStore, &PickCtx)`.
- `crates/gw-routing/src/strategy/{fixed_order,round_robin,load_balance,weighted_rr,cost_first,latency_first,cache_affinity,sticky}.rs` — all 8 impls migrate `is_available` calls from `BreakerStore` to `KeyHealthStore`; `LoadBalance` reads `in_flight` from `InFlightStore`.
- `crates/gw-routing/src/candidates.rs` — drop `BreakerStore` param; add `&KeyHealthStore` + `&InFlightStore` + `api_format` params; rename `breaker_in_flight_per_key` → `in_flight_per_key`.
- `crates/gw-routing/benches/strategy_pick.rs` — migrate to `KeyHealthStore` + `InFlightStore`.
- `crates/gw-routing/tests/strategies.rs` — migrate to new signatures.
- `crates/gw-proxy/Cargo.toml` — add `gw-health = { path = "../gw-health" }`.
- `crates/gw-proxy/src/ctx.rs` — `pub inflight_guard: Option<InFlightGuard>` (type unchanged but import path moves).
- `crates/gw-proxy/src/gateway.rs` — `pub breakers: Arc<BreakerStore>` → `pub health: Arc<KeyHealthStore>` + `pub in_flight: Arc<InFlightStore>`. Constructor updated.
- `crates/gw-proxy/src/filters/upstream.rs` — strategy invocation uses `&health` + `&in_flight`; PickCtx gets `api_format`; `InFlightGuard::new` takes `InFlightCounter` (sourced from `InFlightStore::get_or_create`).
- `crates/gw-proxy/src/filters/logging.rs` — `breakers.get_or_create(&pk.key_id).record(ok)` → `health.record(&pk.key_id, ctx.api_format, OutcomeKind::Success_or_Failure)`.
- `crates/gw-gateway/src/lib.rs` — bootstrap creates `Arc<KeyHealthStore>` + `Arc<InFlightStore>` instead of `Arc<BreakerStore>`. `Wired` struct field renamed.
- `crates/gw-gateway/tests/e2e_full_flow.rs` — adapt bootstrap.
- `crates/gw-proxy/tests/end_to_end_openai.rs` — adapt bootstrap.
- `scripts/check-invariants.sh` — uncomment `no_legacy_breaker` PENDING block.
- `docs/superpowers/specs/2026-05-20-v2-integration-design.md` §6 — add Living amendment for Bm2 re-timing.

**Deleted:**

- `crates/gw-routing/src/breaker.rs` — entire file (A2).
- `crates/gw-routing/tests/breaker.rs` — content split between `gw-health/tests/key_health.rs` and `gw-routing/tests/in_flight.rs`.

---

## Task 1: Create `gw-health` crate (KeyHealthStore + SlidingWindow + CooldownReason)

**Files:**

- Create: `crates/gw-health/Cargo.toml`
- Create: `crates/gw-health/src/lib.rs`
- Create: `crates/gw-health/src/sliding_window.rs`
- Create: `crates/gw-health/src/cooldown.rs`
- Create: `crates/gw-health/src/format_health.rs`
- Create: `crates/gw-health/src/store.rs`
- Create: `crates/gw-health/tests/key_health.rs`
- Modify: `Cargo.toml` (workspace root)

This task is fully additive — no existing file is touched beyond registering the crate. Workspace stays green.

- [ ] **Step 1: Add the crate to the workspace**

In `Cargo.toml` (root), find the `members = [ ... ]` list. Insert `"crates/gw-health"` in alphabetical order (between `gw-gateway` and `gw-ledger`):

```toml
members = [
    "crates/gw-core",
    "crates/gw-codec-macros",
    "crates/gw-codec",
    "crates/gw-store",
    "crates/gw-auth",
    "crates/gw-routing",
    "crates/gw-ledger",
    "crates/gw-health",
    "crates/gw-proxy",
    "crates/gw-gateway",
]
```

- [ ] **Step 2: Create `crates/gw-health/Cargo.toml`**

```toml
[package]
name = "gw-health"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
gw-core = { path = "../gw-core" }
dashmap.workspace = true
parking_lot.workspace = true
metrics.workspace = true

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "test-util"] }
```

`gw-health` depends only on `gw-core` from the workspace — no `gw-store` (no persistence in v1 per ADR-KA-11), no other business crates.

- [ ] **Step 3: Create `crates/gw-health/src/sliding_window.rs`**

```rust
//! 30-second sliding window of `(Instant, success_flag)` samples.
//!
//! Moved verbatim from `gw-routing::breaker::SlidingWindow` per integration
//! spec §2 表 A A2. The semantics are unchanged; only the crate location
//! moves so `gw-health::FormatHealth` (multi-dim) can own one window per
//! `(KeyId, ApiFormat)` pair.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW_DURATION: Duration = Duration::from_secs(30);
const INITIAL_CAPACITY: usize = 256;

pub struct SlidingWindow {
    pub samples: VecDeque<(Instant, bool)>,
}

impl Default for SlidingWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl SlidingWindow {
    pub fn new() -> Self {
        Self {
            samples: VecDeque::with_capacity(INITIAL_CAPACITY),
        }
    }

    /// Record one outcome and evict samples older than 30s.
    pub fn record(&mut self, ok: bool) {
        let now = Instant::now();
        self.samples.push_back((now, ok));
        let cutoff = now - WINDOW_DURATION;
        while self
            .samples
            .front()
            .map(|(t, _)| *t < cutoff)
            .unwrap_or(false)
        {
            self.samples.pop_front();
        }
    }

    /// Returns `(total_samples, failure_rate_in_[0,1])`.
    pub fn failure_rate(&self) -> (usize, f32) {
        let total = self.samples.len();
        if total == 0 {
            return (0, 0.0);
        }
        let fail = self.samples.iter().filter(|(_, ok)| !ok).count();
        (total, fail as f32 / total as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn empty_window_zero_rate() {
        let w = SlidingWindow::new();
        let (n, r) = w.failure_rate();
        assert_eq!(n, 0);
        assert_eq!(r, 0.0);
    }

    #[test]
    fn mixed_record_yields_correct_rate() {
        let mut w = SlidingWindow::new();
        w.record(true);
        w.record(false);
        w.record(false);
        let (n, r) = w.failure_rate();
        assert_eq!(n, 3);
        assert!((r - (2.0 / 3.0)).abs() < 1e-4);
    }

    #[test]
    fn evicts_samples_older_than_30s() {
        // We synthesize an old sample by pushing then mutating its timestamp.
        let mut w = SlidingWindow::new();
        w.record(false);
        // Force the front to be 31s old.
        if let Some(front) = w.samples.front_mut() {
            front.0 = Instant::now() - Duration::from_secs(31);
        }
        // Recording a fresh sample triggers eviction.
        w.record(true);
        assert_eq!(w.samples.len(), 1);
        assert!(w.samples.front().unwrap().1, "fresh sample should remain");
        let _ = sleep; // suppress unused-import warning
    }
}
```

- [ ] **Step 4: Create `crates/gw-health/src/cooldown.rs`**

```rust
//! Cooldown reason enum + directive struct.
//!
//! `CooldownReason` is a `u16` encoding stored in an atomic on `FormatHealth`.
//! 16 variants today; the discriminants are stable and additive
//! (don't renumber existing entries — observability dashboards key off the
//! numeric label).
//!
//! Per K-A ADR-KA-2 / ADR-KA-5.

/// Reason for an explicit cooldown. Numeric discriminants are stable;
/// never renumber an existing variant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum CooldownReason {
    None = 0,
    Status401AccountDeactivated = 1,
    Status401Unauthorized = 2,
    Status402PaymentRequired = 3,
    Status403AccountSuspended = 4,
    Status403Forbidden = 5,
    Status400OrganizationDisabled = 6,
    Status429RateLimit = 7,
    Status529Overloaded = 8,
    StatusTransient5xx = 9,
    Status408Timeout = 10,
    UnschedulableRule = 11,
    OAuthRefreshFailed = 12,
    AccountBlocked = 13,
    AdminForce = 14,
    SlidingWindowTrip = 15,
}

impl CooldownReason {
    /// Decode a stored `u16` back to the typed enum, or [`Self::None`] if the
    /// value is unrecognised (forward compatibility — a newer process may
    /// have written a value an older binary doesn't know).
    pub fn from_u16(v: u16) -> Self {
        match v {
            1 => Self::Status401AccountDeactivated,
            2 => Self::Status401Unauthorized,
            3 => Self::Status402PaymentRequired,
            4 => Self::Status403AccountSuspended,
            5 => Self::Status403Forbidden,
            6 => Self::Status400OrganizationDisabled,
            7 => Self::Status429RateLimit,
            8 => Self::Status529Overloaded,
            9 => Self::StatusTransient5xx,
            10 => Self::Status408Timeout,
            11 => Self::UnschedulableRule,
            12 => Self::OAuthRefreshFailed,
            13 => Self::AccountBlocked,
            14 => Self::AdminForce,
            15 => Self::SlidingWindowTrip,
            _ => Self::None,
        }
    }

    pub fn as_u16(self) -> u16 {
        self as u16
    }

    /// Static label for metrics + tracing. Matches the variant name in
    /// snake_case.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Status401AccountDeactivated => "status401_account_deactivated",
            Self::Status401Unauthorized => "status401_unauthorized",
            Self::Status402PaymentRequired => "status402_payment_required",
            Self::Status403AccountSuspended => "status403_account_suspended",
            Self::Status403Forbidden => "status403_forbidden",
            Self::Status400OrganizationDisabled => "status400_organization_disabled",
            Self::Status429RateLimit => "status429_rate_limit",
            Self::Status529Overloaded => "status529_overloaded",
            Self::StatusTransient5xx => "status_transient_5xx",
            Self::Status408Timeout => "status408_timeout",
            Self::UnschedulableRule => "unschedulable_rule",
            Self::OAuthRefreshFailed => "oauth_refresh_failed",
            Self::AccountBlocked => "account_blocked",
            Self::AdminForce => "admin_force",
            Self::SlidingWindowTrip => "sliding_window_trip",
        }
    }
}

/// Directive passed to [`super::store::KeyHealthStore::apply_cooldown`].
#[derive(Clone, Copy, Debug)]
pub struct CooldownDirective {
    pub reason: CooldownReason,
    pub ttl_secs: u32,
    /// If true, clear `consecutive_failures` on apply (e.g. OAuth refresh OK,
    /// admin unblock). If false, accumulate so graded cooldown can back off.
    pub reset_consecutive: bool,
}

/// Circuit state for `FormatHealth`. Encoded as `u8` in an atomic for
/// CAS-driven open → half_open transitions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CircuitState {
    Closed = 0,
    Open = 1,
    HalfOpen = 2,
}

impl CircuitState {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Open,
            2 => Self::HalfOpen,
            _ => Self::Closed,
        }
    }

    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_reason_roundtrips_through_u16() {
        for r in [
            CooldownReason::None,
            CooldownReason::Status401AccountDeactivated,
            CooldownReason::SlidingWindowTrip,
            CooldownReason::AdminForce,
        ] {
            assert_eq!(CooldownReason::from_u16(r.as_u16()), r);
        }
    }

    #[test]
    fn unknown_u16_decodes_as_none() {
        assert_eq!(CooldownReason::from_u16(9999), CooldownReason::None);
    }

    #[test]
    fn circuit_state_roundtrips() {
        for c in [CircuitState::Closed, CircuitState::Open, CircuitState::HalfOpen] {
            assert_eq!(CircuitState::from_u8(c.as_u8()), c);
        }
    }
}
```

- [ ] **Step 5: Create `crates/gw-health/src/format_health.rs`**

```rust
//! Per-`(KeyId, ApiFormat)` health state.
//!
//! Three concerns layered together:
//! - `SlidingWindow` for failure-rate trip (ADR-008 semantics carried over).
//! - 3-state circuit (`closed / open / half_open`) with CAS-driven probe
//!   admission.
//! - Explicit `cooldown_until_ms` deadline + `CooldownReason` for K-B / K-C
//!   directives.
//!
//! Trip rule (sliding window): ≥20 samples and >50% failure rate ⇒
//! `closed → open` for 60s. Same numbers as the retired `BreakerState`.
//!
//! Cooldown is OR'd with circuit state in `is_available`: either being
//! non-available makes the key unavailable.

use crate::cooldown::{CircuitState, CooldownDirective, CooldownReason};
use crate::sliding_window::SlidingWindow;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Threshold parameters lifted verbatim from the retired `BreakerState`.
const SAMPLE_FLOOR: usize = 20;
const FAILURE_RATE_TRIP: f32 = 0.5;
const OPEN_HOLD_MS: u64 = 60_000;

pub struct FormatHealth {
    pub failure_window: Mutex<SlidingWindow>,
    pub circuit: AtomicU8,
    pub open_until_ms: AtomicU64,
    pub cooldown_until_ms: AtomicU64,
    pub cooldown_reason: AtomicU16,
    pub consecutive_failures: AtomicU32,
}

impl Default for FormatHealth {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatHealth {
    pub fn new() -> Self {
        Self {
            failure_window: Mutex::new(SlidingWindow::new()),
            circuit: AtomicU8::new(CircuitState::Closed.as_u8()),
            open_until_ms: AtomicU64::new(0),
            cooldown_until_ms: AtomicU64::new(0),
            cooldown_reason: AtomicU16::new(CooldownReason::None.as_u16()),
            consecutive_failures: AtomicU32::new(0),
        }
    }

    /// True iff this `(key, fmt)` may be used right now.
    ///
    /// Priority (K-A ADR-KA-7):
    /// 1. Explicit cooldown overrides everything — return false if active.
    /// 2. Then circuit state:
    ///    - `closed` → true.
    ///    - `open` past the deadline → CAS-claim the half-open probe slot
    ///      and return true to the winner; concurrent callers see false.
    ///    - `open` within the deadline → false.
    ///    - `half_open` → false (probe already in flight).
    pub fn is_available(&self) -> bool {
        let now_ms = now_ms();
        if self.cooldown_until_ms.load(Ordering::Acquire) > now_ms {
            return false;
        }
        let c = CircuitState::from_u8(self.circuit.load(Ordering::Acquire));
        match c {
            CircuitState::Closed => true,
            CircuitState::HalfOpen => false,
            CircuitState::Open => {
                if now_ms < self.open_until_ms.load(Ordering::Acquire) {
                    return false;
                }
                self.circuit
                    .compare_exchange(
                        CircuitState::Open.as_u8(),
                        CircuitState::HalfOpen.as_u8(),
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
            }
        }
    }

    /// Record one outcome. Updates sliding window + consecutive_failures.
    /// May trip the circuit closed → open if the 20-sample / 50%-rate
    /// threshold is exceeded. A successful sample while half_open transitions
    /// the circuit back to closed.
    pub fn record(&self, ok: bool) {
        let mut w = self.failure_window.lock();
        w.record(ok);
        let (total, rate) = w.failure_rate();
        drop(w);

        if ok {
            self.consecutive_failures.store(0, Ordering::Release);
        } else {
            self.consecutive_failures.fetch_add(1, Ordering::AcqRel);
        }

        if total >= SAMPLE_FLOOR && rate > FAILURE_RATE_TRIP {
            self.circuit
                .store(CircuitState::Open.as_u8(), Ordering::Release);
            self.open_until_ms
                .store(now_ms() + OPEN_HOLD_MS, Ordering::Release);
            self.cooldown_reason.store(
                CooldownReason::SlidingWindowTrip.as_u16(),
                Ordering::Release,
            );
        } else if ok
            && CircuitState::from_u8(self.circuit.load(Ordering::Acquire))
                == CircuitState::HalfOpen
        {
            // Probe succeeded → close.
            let _ = self.circuit.compare_exchange(
                CircuitState::HalfOpen.as_u8(),
                CircuitState::Closed.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// Apply an explicit cooldown. `cooldown_until_ms` only grows (max with
    /// existing value) so a later directive cannot shorten an in-flight
    /// cooldown.
    pub fn apply_cooldown(&self, d: CooldownDirective) {
        let candidate = now_ms() + (d.ttl_secs as u64) * 1000;
        // CAS up to a fixed-point max.
        loop {
            let current = self.cooldown_until_ms.load(Ordering::Acquire);
            let next = current.max(candidate);
            if next == current {
                break;
            }
            if self
                .cooldown_until_ms
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        self.cooldown_reason
            .store(d.reason.as_u16(), Ordering::Release);
        if d.reset_consecutive {
            self.consecutive_failures.store(0, Ordering::Release);
        }
    }

    /// Forcibly clear the cooldown deadline. Does NOT touch the circuit or
    /// the sliding window — the caller (admin or OAuth refresh path) is
    /// asserting that the *cooldown reason* no longer applies.
    pub fn clear_cooldown(&self) {
        self.cooldown_until_ms.store(0, Ordering::Release);
        self.cooldown_reason
            .store(CooldownReason::None.as_u16(), Ordering::Release);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
```

- [ ] **Step 6: Create `crates/gw-health/src/store.rs`**

```rust
//! `KeyHealthStore` — the public face of `gw-health`. Owns the
//! `DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>` and exposes the 5
//! verbs callers need: `is_available`, `peek`, `record`, `apply_cooldown`,
//! `clear_cooldown`.

use crate::cooldown::{CircuitState, CooldownDirective, CooldownReason};
use crate::format_health::FormatHealth;
use dashmap::DashMap;
use gw_core::ids::KeyId;
use gw_core::protocol::ApiFormat;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[derive(Default)]
pub struct KeyHealthStore {
    pub by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>,
}

/// Read-only snapshot returned by [`KeyHealthStore::peek`]. Does NOT trigger
/// the half-open CAS — observation only.
pub struct HealthSnapshot {
    pub circuit: CircuitState,
    pub in_cooldown: bool,
    pub cooldown_until_ms: Option<u64>,
    pub cooldown_reason: CooldownReason,
    pub failure_rate_30s: f32,
    pub consecutive_failures: u32,
}

/// Outcome passed to [`KeyHealthStore::record`].
#[derive(Clone, Copy, Debug)]
pub enum OutcomeKind {
    Success,
    Failure {
        http_status: u16,
        retry_after_secs: Option<u32>,
    },
}

impl KeyHealthStore {
    fn get_or_create(&self, key: &KeyId, fmt: ApiFormat) -> Arc<FormatHealth> {
        self.by_key_format
            .entry((key.clone(), fmt))
            .or_insert_with(|| Arc::new(FormatHealth::new()))
            .clone()
    }

    pub fn is_available(&self, key: &KeyId, fmt: ApiFormat) -> bool {
        self.get_or_create(key, fmt).is_available()
    }

    pub fn peek(&self, key: &KeyId, fmt: ApiFormat) -> HealthSnapshot {
        let h = self.get_or_create(key, fmt);
        let (_n, rate) = h.failure_window.lock().failure_rate();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let cooldown_deadline = h.cooldown_until_ms.load(Ordering::Acquire);
        HealthSnapshot {
            circuit: CircuitState::from_u8(h.circuit.load(Ordering::Acquire)),
            in_cooldown: cooldown_deadline > now,
            cooldown_until_ms: (cooldown_deadline > 0).then_some(cooldown_deadline),
            cooldown_reason: CooldownReason::from_u16(h.cooldown_reason.load(Ordering::Acquire)),
            failure_rate_30s: rate,
            consecutive_failures: h.consecutive_failures.load(Ordering::Acquire),
        }
    }

    pub fn record(&self, key: &KeyId, fmt: ApiFormat, outcome: OutcomeKind) {
        let ok = matches!(outcome, OutcomeKind::Success);
        self.get_or_create(key, fmt).record(ok);
        // Phase 4 + K-B may layer retry_after_secs / status into the directive
        // path. v1 ignores them at the record() call site — directives come
        // through apply_cooldown() instead.
        let _ = outcome;
    }

    pub fn apply_cooldown(&self, key: &KeyId, fmt: ApiFormat, d: CooldownDirective) {
        self.get_or_create(key, fmt).apply_cooldown(d);
        metrics::counter!(
            "gw_health_apply_cooldown_total",
            "reason" => d.reason.label(),
        )
        .increment(1);
    }

    pub fn clear_cooldown(&self, key: &KeyId, fmt: ApiFormat) {
        self.get_or_create(key, fmt).clear_cooldown();
    }
}
```

- [ ] **Step 7: Create `crates/gw-health/src/lib.rs`**

```rust
//! gw-health: multi-dim KeyHealth state machine (`(KeyId, ApiFormat) →
//! FormatHealth`). Replaces `gw-routing::breaker::BreakerStore` per
//! integration spec §2 表 A A2 + K-A ADR-KA-1.
//!
//! No persistence in v1 (per K-A ADR-KA-11): each gateway replica
//! maintains its own snapshot.

pub mod cooldown;
pub mod format_health;
pub mod sliding_window;
pub mod store;

pub use cooldown::{CircuitState, CooldownDirective, CooldownReason};
pub use format_health::FormatHealth;
pub use sliding_window::SlidingWindow;
pub use store::{HealthSnapshot, KeyHealthStore, OutcomeKind};
```

- [ ] **Step 8: Add the `ApiFormat = ProtocolKind` alias in `gw-core::protocol`**

Open `crates/gw-core/src/protocol.rs`. After the existing `pub enum ProtocolKind { ... }` and its `impl`, add:

```rust
/// `ApiFormat` is a v1 alias for `ProtocolKind` per integration spec §2.3
/// Bm5 (three buckets: openai / anthropic / gemini). Future refinement
/// (e.g. `openai_chat`, `openai_responses`, `openai_cli`) may shadow this
/// alias with a distinct enum.
pub type ApiFormat = ProtocolKind;
```

- [ ] **Step 9: Create `crates/gw-health/tests/key_health.rs`**

```rust
//! KeyHealthStore behavioural tests.
//!
//! Coverage targets the K-A spec invariants:
//! - (KeyId, ApiFormat) isolation: same KeyId, different ApiFormat have
//!   independent state.
//! - is_available priority: cooldown overrides circuit.
//! - Half-open CAS admits exactly one probe.
//! - apply_cooldown monotonic — can't shorten an in-flight window.
//! - record() trips the circuit only after the 20-sample / 50%-rate floor.

use gw_core::ids::KeyId;
use gw_core::protocol::ProtocolKind;
use gw_health::cooldown::{CircuitState, CooldownDirective, CooldownReason};
use gw_health::store::{KeyHealthStore, OutcomeKind};

#[test]
fn fresh_store_is_available_for_unknown_key() {
    let s = KeyHealthStore::default();
    assert!(s.is_available(&KeyId::from("k-fresh"), ProtocolKind::OpenAi));
}

#[test]
fn cooldown_overrides_closed_circuit() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-cooldown");
    assert!(s.is_available(&k, ProtocolKind::OpenAi));

    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::Status401AccountDeactivated,
            ttl_secs: 3600,
            reset_consecutive: false,
        },
    );
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn cooldown_isolated_across_api_formats() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-multi-fmt");
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::Status429RateLimit,
            ttl_secs: 60,
            reset_consecutive: false,
        },
    );
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
    // Anthropic bucket is untouched.
    assert!(s.is_available(&k, ProtocolKind::Anthropic));
}

#[test]
fn record_under_floor_does_not_trip_circuit() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-under-floor");
    // 19 failures < 20-sample floor.
    for _ in 0..19 {
        s.record(
            &k,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 500,
                retry_after_secs: None,
            },
        );
    }
    let snap = s.peek(&k, ProtocolKind::OpenAi);
    assert_eq!(snap.circuit, CircuitState::Closed);
    assert!(s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn record_over_floor_trips_circuit_to_open() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-trip");
    // 20 failures, 100% rate ⇒ trip.
    for _ in 0..20 {
        s.record(
            &k,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 500,
                retry_after_secs: None,
            },
        );
    }
    let snap = s.peek(&k, ProtocolKind::OpenAi);
    assert_eq!(snap.circuit, CircuitState::Open);
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
    assert_eq!(
        snap.cooldown_reason,
        CooldownReason::None, // not a directive — circuit trip
    );
}

#[test]
fn apply_cooldown_is_monotonic() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-monotonic");
    // Long cooldown first.
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::AdminForce,
            ttl_secs: 3600,
            reset_consecutive: false,
        },
    );
    let snap1 = s.peek(&k, ProtocolKind::OpenAi);
    let deadline1 = snap1.cooldown_until_ms.unwrap();

    // Short cooldown next — must NOT shorten the window.
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::Status429RateLimit,
            ttl_secs: 10,
            reset_consecutive: false,
        },
    );
    let snap2 = s.peek(&k, ProtocolKind::OpenAi);
    assert_eq!(snap2.cooldown_until_ms, Some(deadline1));
}

#[test]
fn clear_cooldown_restores_availability() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-clear");
    s.apply_cooldown(
        &k,
        ProtocolKind::OpenAi,
        CooldownDirective {
            reason: CooldownReason::OAuthRefreshFailed,
            ttl_secs: 3600,
            reset_consecutive: false,
        },
    );
    assert!(!s.is_available(&k, ProtocolKind::OpenAi));
    s.clear_cooldown(&k, ProtocolKind::OpenAi);
    assert!(s.is_available(&k, ProtocolKind::OpenAi));
}

#[test]
fn consecutive_failures_resets_on_success() {
    let s = KeyHealthStore::default();
    let k = KeyId::from("k-consecutive");
    for _ in 0..5 {
        s.record(
            &k,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 500,
                retry_after_secs: None,
            },
        );
    }
    assert_eq!(s.peek(&k, ProtocolKind::OpenAi).consecutive_failures, 5);
    s.record(&k, ProtocolKind::OpenAi, OutcomeKind::Success);
    assert_eq!(s.peek(&k, ProtocolKind::OpenAi).consecutive_failures, 0);
}
```

- [ ] **Step 10: Build & test**

```bash
cargo build -p gw-health 2>&1 | tail -5
cargo build -p gw-core 2>&1 | tail -5
cargo test -p gw-health 2>&1 | tail -10
cargo clippy -p gw-health -p gw-core -- -D warnings 2>&1 | tail -5
cargo build --workspace 2>&1 | tail -5
```

Expected: all clean. The workspace still builds because `gw-routing::breaker` still exists in parallel.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml crates/gw-health crates/gw-core/src/protocol.rs
git commit -m "feat(gw-health): new business crate (KeyHealthStore + SlidingWindow)

Implements integration spec §2 表 A A2 + K-A ADR-KA-1..5: a new business-
layer crate carrying multi-dim (KeyId, ApiFormat) → FormatHealth state.

Each FormatHealth combines (1) a 30s SlidingWindow (moved verbatim from
gw-routing::breaker), (2) a 3-state CAS circuit (closed / open / half-open),
(3) an explicit cooldown_until_ms + CooldownReason, and
(4) a consecutive_failures counter for graded backoff.

KeyHealthStore exposes is_available / peek / record / apply_cooldown /
clear_cooldown. Cooldown overrides circuit per ADR-KA-7. The store is in-
process only in v1 (ADR-KA-11) — multi-replica gateways each maintain
their own view.

gw-core::protocol gains 'pub type ApiFormat = ProtocolKind' alias (Bm5).

gw-routing::breaker is NOT touched in this commit — Task 4 + 5 migrate
callers, Task 6 deletes the file."
```

---

## Task 2: `gw-routing::in_flight` module

**Files:**

- Create: `crates/gw-routing/src/in_flight.rs`
- Modify: `crates/gw-routing/src/lib.rs`
- Create: `crates/gw-routing/tests/in_flight.rs`

This task is also additive — the new module coexists with `breaker.rs`. No callers migrated yet.

- [ ] **Step 1: Create `crates/gw-routing/src/in_flight.rs`**

```rust
//! Per-key in-flight counter + RAII guard.
//!
//! Extracted from `gw-routing::breaker::InFlightGuard` per K-A ADR-KA-8.
//! Concerns:
//!
//! - `InFlightStore` owns a `DashMap<KeyId, Arc<InFlightCounter>>`.
//! - `InFlightCounter` is a single `AtomicU32`.
//! - `InFlightGuard` increments on `new`, decrements on `Drop`.
//!
//! Two consumers in `gw-routing`:
//! - `strategy::load_balance::LoadBalance` reads `count` for composite score.
//! - `candidates::get_candidate_keys` drops candidates whose `count` exceeds
//!   the per-key cap.
//!
//! Health concerns (circuit / cooldown / sliding window) live in
//! `gw-health::KeyHealthStore`. The two stores are independent.

use dashmap::DashMap;
use gw_core::ids::KeyId;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct InFlightCounter {
    pub count: AtomicU32,
}

impl Default for InFlightCounter {
    fn default() -> Self {
        Self {
            count: AtomicU32::new(0),
        }
    }
}

#[derive(Default)]
pub struct InFlightStore {
    pub by_key: DashMap<KeyId, Arc<InFlightCounter>>,
}

impl InFlightStore {
    pub fn get_or_create(&self, k: &KeyId) -> Arc<InFlightCounter> {
        self.by_key
            .entry(k.clone())
            .or_insert_with(|| Arc::new(InFlightCounter::default()))
            .clone()
    }

    pub fn count(&self, k: &KeyId) -> u32 {
        self.get_or_create(k).count.load(Ordering::Relaxed)
    }
}

/// RAII increment of an [`InFlightCounter`]. Incremented on `new`,
/// decremented on `Drop`. `gw-proxy` holds this on `ReqCtx` so the
/// counter releases on every termination path (success, error, cancel).
pub struct InFlightGuard {
    counter: Arc<InFlightCounter>,
}

impl InFlightGuard {
    /// Bump the counter; the matching decrement runs on `Drop`.
    pub fn new(counter: Arc<InFlightCounter>) -> Self {
        counter.count.fetch_add(1, Ordering::AcqRel);
        Self { counter }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.count.fetch_sub(1, Ordering::AcqRel);
    }
}
```

- [ ] **Step 2: Register the module in `crates/gw-routing/src/lib.rs`**

After the existing `pub mod breaker;` line, add `pub mod in_flight;`:

```rust
pub mod breaker;
pub mod candidates;
pub mod in_flight;
pub mod retry;
pub mod strategy;
```

Below the existing `pub use breaker::...;`, add:

```rust
pub use in_flight::{InFlightCounter, InFlightStore};
```

Keep the existing `pub use breaker::{BreakerState, BreakerStore, InFlightGuard};` line for now — Task 6 deletes it along with `breaker.rs`. But to avoid name collision (both `breaker::InFlightGuard` and `in_flight::InFlightGuard` exist) **do not** add `pub use in_flight::InFlightGuard` yet — we'll do that in Task 6 right after removing the breaker re-export.

- [ ] **Step 3: Create `crates/gw-routing/tests/in_flight.rs`**

```rust
//! Per-key in-flight counter semantics (extracted from the old breaker tests).

use gw_core::ids::KeyId;
use gw_routing::in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[test]
fn counter_starts_at_zero() {
    let s = InFlightStore::default();
    assert_eq!(s.count(&KeyId::from("k-fresh")), 0);
}

#[test]
fn guard_bumps_then_releases_on_drop() {
    let s = InFlightStore::default();
    let k = KeyId::from("k-guard");
    {
        let _g = InFlightGuard::new(s.get_or_create(&k));
        assert_eq!(s.count(&k), 1);
    }
    assert_eq!(s.count(&k), 0);
}

#[test]
fn concurrent_guards_sum_correctly() {
    let s = InFlightStore::default();
    let k = KeyId::from("k-concurrent");
    let _g1 = InFlightGuard::new(s.get_or_create(&k));
    let _g2 = InFlightGuard::new(s.get_or_create(&k));
    let _g3 = InFlightGuard::new(s.get_or_create(&k));
    assert_eq!(s.count(&k), 3);
}

#[test]
fn counter_is_a_proper_arc_handle() {
    let s = InFlightStore::default();
    let k = KeyId::from("k-arc");
    let h1 = s.get_or_create(&k);
    let h2 = s.get_or_create(&k);
    // Both handles point to the same atomic.
    assert!(Arc::ptr_eq(&h1, &h2));
    h1.count.fetch_add(5, Ordering::AcqRel);
    assert_eq!(h2.count.load(Ordering::Relaxed), 5);
    // Reset for cleanliness.
    h1.count.store(0, Ordering::Release);
    let _ = InFlightCounter::default();
}
```

- [ ] **Step 4: Verify**

```bash
cargo build -p gw-routing 2>&1 | tail -5
cargo test -p gw-routing --test in_flight 2>&1 | tail -10
cargo clippy -p gw-routing -- -D warnings 2>&1 | tail -5
cargo build --workspace 2>&1 | tail -5
```

Expected: clean. The old `breaker.rs` `InFlightGuard` and the new `in_flight::InFlightGuard` coexist; nothing in workspace consumes `in_flight::` yet, so the duplicate is harmless.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-routing/src/in_flight.rs crates/gw-routing/src/lib.rs \
        crates/gw-routing/tests/in_flight.rs
git commit -m "feat(gw-routing): in_flight module split from breaker.rs

Per K-A ADR-KA-8, the in_flight counter is independent of health/circuit
state and remains in gw-routing. New gw-routing::in_flight carries
InFlightCounter (single AtomicU32), InFlightStore (DashMap by KeyId),
and InFlightGuard (RAII +1/-1).

breaker.rs still exists and re-exports its own InFlightGuard for now;
Task 6 deletes breaker.rs and Task 6's lib.rs update switches the
top-level re-export to in_flight::InFlightGuard."
```

---

## Task 3: `PickCtx::api_format` field

**Files:**

- Modify: `crates/gw-routing/src/strategy/mod.rs`
- Modify: `crates/gw-routing/tests/strategies.rs`
- Modify: `crates/gw-routing/benches/strategy_pick.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs` (PickCtx construction)

`PickCtx` grows the field. Strategy impls still take `&BreakerStore` — Task 4 changes the trait signature. The field is plumbed but not yet consulted by `is_available`.

- [ ] **Step 1: Add the field to `PickCtx` in `crates/gw-routing/src/strategy/mod.rs`**

Find the existing struct:

```rust
pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
}
```

Replace with:

```rust
pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
    /// API format the strategy is selecting for. Threaded into
    /// `KeyHealthStore::is_available(key, fmt)` in Task 4 so the
    /// multi-dim health check has the format dimension. Per integration
    /// spec §2.3 Bm5, this is `ProtocolKind` in v1.
    pub api_format: gw_core::protocol::ApiFormat,
}
```

- [ ] **Step 2: Update `crates/gw-routing/tests/strategies.rs` PickCtx literals**

The three test PickCtx constructions (lines 37, 53, 74) need `api_format` added. Use `ProtocolKind::OpenAi` for all three:

```rust
let ctx = PickCtx {
    user_id: &UserId::from("u"),
    model: &ModelName::from("gpt-4"),
    conversation_id: None,
    message_hash: None,
    api_format: gw_core::protocol::ProtocolKind::OpenAi,
};
```

Apply to all 3 occurrences.

- [ ] **Step 3: Update `crates/gw-routing/benches/strategy_pick.rs`**

Lines 61 and 67 construct `PickCtx`. Add `api_format` field to both:

```rust
let ctx_no_hash = PickCtx {
    user_id: &user,
    model: &model,
    conversation_id: None,
    message_hash: None,
    api_format: gw_core::protocol::ProtocolKind::OpenAi,
};

let ctx_with_hash = PickCtx {
    user_id: &user,
    model: &model,
    conversation_id: None,
    message_hash: Some(0x1234_5678),
    api_format: gw_core::protocol::ProtocolKind::OpenAi,
};
```

- [ ] **Step 4: Update `crates/gw-proxy/src/filters/upstream.rs` PickCtx construction**

Find where `PickCtx { ... }` is built (around line 175 — search for `PickCtx {`). Add `api_format: ctx.upstream_protocol,` to the literal. (`ReqCtx` already carries `upstream_codec: Option<AnyCodec>` and via `.kind()` you can get a `ProtocolKind`. The expression to use is what the surrounding code already passes downstream as the target protocol — usually `target_protocol` is in scope at that point. Verify by reading `upstream.rs:170-205`.)

If `target_protocol` is the local variable, use:

```rust
let pick_ctx = PickCtx {
    user_id: &merged.effective_user_id,
    model: &model,
    conversation_id: convo_id.as_deref(),
    message_hash: msg_hash,
    api_format: target_protocol,
};
```

Otherwise compute it from `ctx.upstream_codec.as_ref().map(|c| c.kind()).unwrap_or(ProtocolKind::OpenAi)`. The implementer should read the file and pick the cleanest expression that matches existing patterns.

- [ ] **Step 5: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p gw-routing --test strategies 2>&1 | tail -10
cargo bench -p gw-routing --no-run 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
```

Expected: all clean. Existing strategies still take `&BreakerStore`; new field is plumbed but unused.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-routing/src/strategy/mod.rs \
        crates/gw-routing/tests/strategies.rs \
        crates/gw-routing/benches/strategy_pick.rs \
        crates/gw-proxy/src/filters/upstream.rs
git commit -m "feat(gw-routing): PickCtx.api_format field (Bm5 / Bm2 early land)

Adds api_format: ApiFormat to PickCtx so Task 4's Strategy::pick
signature change has the format dimension available for the
KeyHealthStore::is_available(key, fmt) call.

Integration spec §2.3 Bm2 had this tagged Phase 4, but deleting
breaker.rs in Phase 1c (A2) forces strategies to switch to
KeyHealthStore now, which forces the api_format field now.
A Living amendment will be appended in Task 7."
```

---

## Task 4: `Strategy::pick` signature migration + all 8 impls

**Files:**

- Modify: `crates/gw-routing/Cargo.toml`
- Modify: `crates/gw-routing/src/strategy/mod.rs`
- Modify: `crates/gw-routing/src/strategy/{fixed_order,round_robin,load_balance,weighted_rr,cost_first,latency_first,cache_affinity,sticky}.rs`
- Modify: `crates/gw-routing/src/candidates.rs`
- Modify: `crates/gw-routing/benches/strategy_pick.rs`
- Modify: `crates/gw-routing/tests/strategies.rs`

This is the migration core. **Workspace will be transiently broken downstream** (gw-proxy / gw-gateway tests still use the old signatures). Task 5 fixes those.

- [ ] **Step 1: Add `gw-health` dep to `crates/gw-routing/Cargo.toml`**

Under `[dependencies]`:

```toml
gw-health = { path = "../gw-health" }
```

- [ ] **Step 2: Update the `Strategy` trait + `StrategyAny` in `crates/gw-routing/src/strategy/mod.rs`**

Replace the entire file content with:

```rust
//! Selection strategies (ADR-009). 8 variants implementing the [`Strategy`]
//! trait, plus a dispatching [`StrategyAny`] enum used by the routing pipeline.

use crate::in_flight::InFlightStore;
use gw_core::entity::VendorKey;
use gw_core::ids::*;
use gw_core::protocol::ApiFormat;
use gw_health::KeyHealthStore;
use std::sync::Arc;

pub mod cache_affinity;
pub mod cost_first;
pub mod fixed_order;
pub mod latency_first;
pub mod load_balance;
pub mod round_robin;
pub mod sticky;
pub mod weighted_rr;

pub struct PickCtx<'a> {
    pub user_id: &'a UserId,
    pub model: &'a ModelName,
    pub conversation_id: Option<&'a str>,
    pub message_hash: Option<u64>,
    pub api_format: ApiFormat,
}

pub trait Strategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn pick<'a>(
        &self,
        candidates: &'a [Arc<VendorKey>],
        health: &KeyHealthStore,
        in_flight: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>>;
}

pub enum StrategyAny {
    FixedOrder(fixed_order::FixedOrder),
    RoundRobin(round_robin::RoundRobin),
    LoadBalance(load_balance::LoadBalance),
    WeightedRr(weighted_rr::WeightedRr),
    CostFirst(cost_first::CostFirst),
    LatencyFirst(latency_first::LatencyFirst),
    CacheAffinity(cache_affinity::CacheAffinity),
    Sticky(sticky::Sticky),
}

impl Strategy for StrategyAny {
    fn name(&self) -> &'static str {
        match self {
            StrategyAny::FixedOrder(s) => s.name(),
            StrategyAny::RoundRobin(s) => s.name(),
            StrategyAny::LoadBalance(s) => s.name(),
            StrategyAny::WeightedRr(s) => s.name(),
            StrategyAny::CostFirst(s) => s.name(),
            StrategyAny::LatencyFirst(s) => s.name(),
            StrategyAny::CacheAffinity(s) => s.name(),
            StrategyAny::Sticky(s) => s.name(),
        }
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        match self {
            StrategyAny::FixedOrder(s) => s.pick(c, h, f, ctx),
            StrategyAny::RoundRobin(s) => s.pick(c, h, f, ctx),
            StrategyAny::LoadBalance(s) => s.pick(c, h, f, ctx),
            StrategyAny::WeightedRr(s) => s.pick(c, h, f, ctx),
            StrategyAny::CostFirst(s) => s.pick(c, h, f, ctx),
            StrategyAny::LatencyFirst(s) => s.pick(c, h, f, ctx),
            StrategyAny::CacheAffinity(s) => s.pick(c, h, f, ctx),
            StrategyAny::Sticky(s) => s.pick(c, h, f, ctx),
        }
    }
}
```

- [ ] **Step 3: Migrate `crates/gw-routing/src/strategy/fixed_order.rs`**

Replace the file content with:

```rust
//! `FixedOrder` — pick the first available candidate in slice order.

use super::*;

pub struct FixedOrder;

impl Strategy for FixedOrder {
    fn name(&self) -> &'static str {
        "fixed_order"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        _f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        c.iter().find(|k| h.is_available(&k.key_id, ctx.api_format))
    }
}
```

- [ ] **Step 4: Migrate `crates/gw-routing/src/strategy/round_robin.rs`**

Read the existing file and adapt: drop the `b: &BreakerStore` parameter, add `h: &KeyHealthStore, _f: &InFlightStore`, replace any `b.get_or_create(&k.key_id).is_available()` with `h.is_available(&k.key_id, ctx.api_format)`. Keep the round-robin counter logic intact.

The implementer should preserve every line outside the breaker-availability check.

- [ ] **Step 5: Migrate `crates/gw-routing/src/strategy/load_balance.rs`**

Replace the file content with:

```rust
//! `LoadBalance` — pick the available key with the lowest composite
//! score `α·in_flight + β·avg_latency + γ·error_rate`.
//!
//! v1 simplification: only `in_flight` is observed (α=1, β=γ=0). Latency
//! and error-rate inputs are stubbed pending a `LatencyTracker` (Phase 4).

use super::*;
use std::sync::atomic::Ordering;

pub const ALPHA_IN_FLIGHT: f32 = 1.0;
pub const BETA_LATENCY: f32 = 0.0;
pub const GAMMA_ERROR_RATE: f32 = 0.0;

pub struct LoadBalance;

impl Strategy for LoadBalance {
    fn name(&self) -> &'static str {
        "load_balance"
    }

    fn pick<'a>(
        &self,
        c: &'a [Arc<VendorKey>],
        h: &KeyHealthStore,
        f: &InFlightStore,
        ctx: &PickCtx<'_>,
    ) -> Option<&'a Arc<VendorKey>> {
        let mut best: Option<(f32, &'a Arc<VendorKey>)> = None;
        for k in c {
            if !h.is_available(&k.key_id, ctx.api_format) {
                continue;
            }
            let in_flight = f
                .get_or_create(&k.key_id)
                .count
                .load(Ordering::Relaxed) as f32;
            let score = ALPHA_IN_FLIGHT * in_flight;
            match best {
                Some((bs, _)) if bs <= score => {}
                _ => best = Some((score, k)),
            }
        }
        best.map(|(_, k)| k)
    }
}
```

- [ ] **Step 6: Migrate the remaining 5 strategies (weighted_rr, cost_first, latency_first, cache_affinity, sticky)**

Each file follows the same mechanical pattern:

1. Replace `b: &BreakerStore` parameter → `h: &KeyHealthStore, _f: &InFlightStore` (or `f: &InFlightStore` if the strategy reads in_flight — only `load_balance` does).
2. Replace any `b.get_or_create(&k.key_id).is_available()` with `h.is_available(&k.key_id, ctx.api_format)`.
3. Remove any `use crate::breaker::BreakerStore;` line.

For each file, read it first, apply the mechanical changes, and verify `cargo build -p gw-routing` after each one (per-file commits are NOT needed — one commit at the end of Task 4).

Specifically:

- `weighted_rr.rs` — has the weighted RR counter logic; only the breaker filter line changes.
- `cost_first.rs` — currently has `_ctx: &PickCtx<'_>` (placeholder ctx, not yet used). Update params; the body likely just falls through to the FixedOrder pattern. Read the file before editing.
- `latency_first.rs` — same as cost_first; placeholder body. Update params.
- `cache_affinity.rs` — uses `ctx.message_hash` for hash-based dispatch. Update params; preserve the hash logic.
- `sticky.rs` — uses `ctx.conversation_id` for binding. **Two** breaker checks (lines 68 and 77) — both become `h.is_available(&k.key_id, ctx.api_format)`. Don't touch the DashMap binding logic.

- [ ] **Step 7: Migrate `crates/gw-routing/src/candidates.rs`**

Replace the file content with:

```rust
//! Candidate key filter (ADR-021).
//!
//! Given a `ModelGroup` and request context, walk every member `KeyId`, fetch
//! its `VendorKey` from the store, and admit it only if:
//! 1. `status == Active`,
//! 2. the requested `model` is in `accessible_models`,
//! 3. the target wire protocol is in `accessible_endpoints` OR the key has
//!    `protocol_conversion = true`,
//! 4. the caller's `MergedPermissions.allowed_channels` is either empty (no
//!    channel allowlist) or contains this key,
//! 5. the key's in-flight count is under the per-key cap.
//!
//! Health/circuit checks happen *later* inside `Strategy::pick` via
//! `KeyHealthStore::is_available` — candidate filtering is the cheap
//! upstream step.

use crate::in_flight::InFlightStore;
use gw_core::entity::{KeyStatus, ModelGroup, VendorKey};
use gw_core::ids::ModelName;
use gw_core::permissions::MergedPermissions;
use gw_core::protocol::ProtocolKind;
use gw_store::entity_store::EntityStore;
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;

pub async fn get_candidate_keys(
    group: &ModelGroup,
    model: &ModelName,
    target_protocol: ProtocolKind,
    perm: &MergedPermissions,
    in_flight: &InFlightStore,
    store: &EntityStore,
    in_flight_per_key: u32,
) -> Vec<Arc<VendorKey>> {
    let mut out = Vec::with_capacity(group.vendor_key_ids.len());
    for kid in &group.vendor_key_ids {
        let Some(v) = store.vendor_key_or_load(kid).await else {
            continue;
        };
        let key = &*v.value;
        if !matches!(key.status, KeyStatus::Active) {
            continue;
        }
        if !key.accessible_models.contains(model) {
            continue;
        }
        if !(key.accessible_endpoints.contains(&target_protocol) || key.protocol_conversion) {
            continue;
        }
        if !perm.allowed_channels.is_empty() && !perm.allowed_channels.contains(&key.key_id) {
            continue;
        }
        if in_flight.count(&key.key_id) >= in_flight_per_key {
            continue;
        }
        out.push(Arc::new(VendorKey {
            key_id: key.key_id.clone(),
            vendor_id: key.vendor_id.clone(),
            api_key: SecretString::from(key.api_key.expose_secret().to_owned()),
            accessible_models: key.accessible_models.clone(),
            model_mapping: key.model_mapping.clone(),
            accessible_endpoints: key.accessible_endpoints.clone(),
            protocol_conversion: key.protocol_conversion,
            priority: key.priority,
            weight: key.weight,
            rpm_limit: key.rpm_limit,
            tpm_limit: key.tpm_limit,
            max_usage_count: key.max_usage_count,
            proxy: key.proxy.clone(),
            ip_allowlist: key.ip_allowlist.clone(),
            expires_at: key.expires_at,
            status: key.status,
            tags: key.tags.clone(),
        }));
    }
    out
}
```

- [ ] **Step 8: Migrate `crates/gw-routing/tests/strategies.rs`**

Read the file. Each test currently constructs `BreakerStore::default()` and passes `&b` to `pick`. Replace with `KeyHealthStore::default()` and `InFlightStore::default()`, passing `&h, &f` to `pick`.

Imports update from `use gw_routing::breaker::BreakerStore;` to:

```rust
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
```

And the `Cargo.toml` for `gw-routing` already has `gw-health` (Step 1); add it as a dev-dep too if needed:

```toml
[dev-dependencies]
gw-health = { path = "../gw-health" }
```

(Dev-deps inherit prod-deps in Cargo so this should not be needed, but verify clippy/test run.)

- [ ] **Step 9: Migrate `crates/gw-routing/benches/strategy_pick.rs`**

Same pattern: `BreakerStore::default()` → `KeyHealthStore::default() + InFlightStore::default()`. Calls to `s.pick(black_box(&keys), black_box(&breakers), ...)` become `s.pick(black_box(&keys), black_box(&health), black_box(&in_flight), ...)`.

- [ ] **Step 10: Verify gw-routing compiles + tests pass + bench compiles**

```bash
cargo build -p gw-routing 2>&1 | tail -5
cargo test -p gw-routing --tests 2>&1 | tail -20
cargo bench -p gw-routing --no-run 2>&1 | tail -5
cargo clippy -p gw-routing -- -D warnings 2>&1 | tail -5
```

Expected: all green within `gw-routing`. `gw-proxy` and `gw-gateway` will be broken — that's expected and gets fixed in Task 5.

Do NOT run `cargo build --workspace` — it will fail.

- [ ] **Step 11: Commit**

```bash
git add crates/gw-routing/Cargo.toml \
        crates/gw-routing/src/strategy \
        crates/gw-routing/src/candidates.rs \
        crates/gw-routing/tests/strategies.rs \
        crates/gw-routing/benches/strategy_pick.rs
git commit -m "refactor(gw-routing): Strategy::pick uses KeyHealthStore + InFlightStore

Per integration spec §2 表 A A2 + K-A ADR-KA-9: the Strategy trait
signature changes from
    pick(candidates, &BreakerStore, &PickCtx)
to
    pick(candidates, &KeyHealthStore, &InFlightStore, &PickCtx).

All 8 strategy impls migrate. is_available is now multi-dim — keyed on
(KeyId, ApiFormat) read from ctx.api_format. LoadBalance reads in_flight
from InFlightStore. candidates::get_candidate_keys now takes
&InFlightStore (renamed param) instead of &BreakerStore.

breaker.rs still exists and is referenced by gw-proxy + gw-gateway —
Task 5 migrates those, Task 6 deletes the file. Workspace transiently
red until Task 5."
```

---

## Task 5: Migrate `gw-proxy` + `gw-gateway` to KeyHealthStore + InFlightStore

**Files:**

- Modify: `crates/gw-proxy/Cargo.toml`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Modify: `crates/gw-proxy/src/filters/mod.rs` (doc comment)
- Modify: `crates/gw-gateway/Cargo.toml`
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-gateway/tests/e2e_full_flow.rs`
- Modify: `crates/gw-proxy/tests/end_to_end_openai.rs`

Big task — workspace must return to green by the end. Do not commit until everything compiles + tests pass.

- [ ] **Step 1: `gw-proxy` Cargo.toml — add gw-health**

In `crates/gw-proxy/Cargo.toml`:

```toml
gw-health = { path = "../gw-health" }
```

Same in `crates/gw-gateway/Cargo.toml`.

- [ ] **Step 2: `gw-proxy/src/ctx.rs` — update InFlightGuard import**

Find:

```rust
use gw_routing::InFlightGuard;
```

Change to:

```rust
use gw_routing::in_flight::InFlightGuard;
```

(Once Task 6 lands the top-level re-export change, `gw_routing::InFlightGuard` will resolve again — keep the explicit path for now to be unambiguous during the migration.)

- [ ] **Step 3: `gw-proxy/src/gateway.rs` — swap BreakerStore for KeyHealthStore + InFlightStore**

Find the `LlmGateway` struct + constructor. The current shape has `pub breakers: Arc<BreakerStore>` and the constructor accepts `breakers: Arc<BreakerStore>`. Update:

```rust
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::RetryConfig;
// (drop the BreakerStore import)
```

The struct fields:

```rust
pub struct LlmGateway {
    pub store: Arc<EntityStore>,
    pub health: Arc<KeyHealthStore>,
    pub in_flight: Arc<InFlightStore>,
    pub ratelimit: Arc<RateLimitGuard>,
    pub ledger: Arc<Ledger>,
    pub counters: Arc<CounterStore>,
    pub retry_cfg: RetryConfig,
    pub global_rpm: u32,
    pub trust_xff: bool,
}
```

And the constructor signature mirrors that. Update all sites in gateway.rs that pass `&self.breakers` to pass `&self.health` (for strategy picking) or `&self.in_flight` (for in-flight counter).

Specifically the two existing call sites:
- `filters::upstream::pick_peer(session, ctx, &self.store, &self.breakers)` → `pick_peer(session, ctx, &self.store, &self.health, &self.in_flight)`.
- `filters::logging::run(session, err, ctx, &self.breakers, &self.ledger)` → `run(session, err, ctx, &self.health, &self.ledger)`.

- [ ] **Step 4: `gw-proxy/src/filters/upstream.rs`**

This is the major touch point. Read the file. Migration:

(a) Imports: replace

```rust
use gw_routing::{BreakerStore, InFlightGuard, PickCtx, Strategy, StrategyAny};
```

with

```rust
use gw_health::KeyHealthStore;
use gw_routing::in_flight::{InFlightGuard, InFlightStore};
use gw_routing::{PickCtx, Strategy, StrategyAny};
```

(b) The `pick_peer` function signature: change

```rust
pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    breakers: &Arc<BreakerStore>,
) -> Result<HttpPeer> {
```

to

```rust
pub async fn pick_peer(
    session: &mut Session,
    ctx: &mut ReqCtx,
    store: &Arc<EntityStore>,
    health: &Arc<KeyHealthStore>,
    in_flight: &Arc<InFlightStore>,
) -> Result<HttpPeer> {
```

(c) The call site for `get_candidate_keys` switches from `breakers` to `in_flight`:

Find:

```rust
let candidates = get_candidate_keys(
    &group,
    &model,
    target_protocol,
    &merged,
    breakers,
    ...
```

Change to:

```rust
let candidates = get_candidate_keys(
    &group,
    &model,
    target_protocol,
    &merged,
    in_flight,
    ...
```

(d) The strategy invocation:

Find:

```rust
let picked = strategy.pick(&candidates, breakers, &pick_ctx);
```

Change to:

```rust
let picked = strategy.pick(&candidates, health, in_flight, &pick_ctx);
```

(e) The InFlightGuard creation:

Find:

```rust
let breaker = breakers.get_or_create(&picked_arc.key_id);
ctx.inflight_guard = Some(InFlightGuard::new(breaker));
```

Change to:

```rust
let counter = in_flight.get_or_create(&picked_arc.key_id);
ctx.inflight_guard = Some(InFlightGuard::new(counter));
```

(f) Update doc comments referring to "breakers close" — replace with "health closes" or "circuit closes" (gw-health terminology).

- [ ] **Step 5: `gw-proxy/src/filters/logging.rs`**

(a) Imports — replace

```rust
use gw_routing::BreakerStore;
```

with

```rust
use gw_health::{KeyHealthStore, OutcomeKind};
```

(b) `run` signature — change

```rust
pub async fn run(
    _session: &mut Session,
    err: Option<&pingora::Error>,
    ctx: &mut ReqCtx,
    breakers: &Arc<BreakerStore>,
    ledger: &Arc<Ledger>,
) {
```

to

```rust
pub async fn run(
    _session: &mut Session,
    err: Option<&pingora::Error>,
    ctx: &mut ReqCtx,
    health: &Arc<KeyHealthStore>,
    ledger: &Arc<Ledger>,
) {
```

(c) Migrate the breaker `.record(ok)` call. Find:

```rust
let ok = err.is_none() && matches!(ctx.upstream_status, Some(s) if (200..400).contains(&s));
if let Some(pk) = ctx.picked_vendor_key.as_ref() {
    breakers.get_or_create(&pk.key_id).record(ok);
}
```

Change to:

```rust
let ok = err.is_none() && matches!(ctx.upstream_status, Some(s) if (200..400).contains(&s));
if let Some(pk) = ctx.picked_vendor_key.as_ref() {
    let api_fmt = ctx
        .upstream_codec
        .as_ref()
        .map(|c| c.kind())
        .unwrap_or(gw_core::protocol::ProtocolKind::OpenAi);
    let outcome = if ok {
        OutcomeKind::Success
    } else {
        OutcomeKind::Failure {
            http_status: ctx.upstream_status.unwrap_or(0),
            retry_after_secs: None,
        }
    };
    health.record(&pk.key_id, api_fmt, outcome);
}
```

(d) Update the doc comment at line 1: `record breaker outcome` → `record key-health outcome`. Similarly for `filters/mod.rs` line 11.

- [ ] **Step 6: `gw-gateway/src/lib.rs` bootstrap**

Find the bootstrap section that creates `BreakerStore`. Replace with `KeyHealthStore + InFlightStore`:

```rust
use gw_health::KeyHealthStore;
use gw_routing::in_flight::InFlightStore;
use gw_routing::RetryConfig;
// drop the BreakerStore import
```

Replace the local construction:

```rust
let breakers = Arc::new(BreakerStore::default());
```

with:

```rust
let health = Arc::new(KeyHealthStore::default());
let in_flight = Arc::new(InFlightStore::default());
```

The `Wired` struct definition:

```rust
struct Wired {
    store: Arc<EntityStore>,
    health: Arc<KeyHealthStore>,
    in_flight: Arc<InFlightStore>,
    ratelimit: Arc<RateLimitGuard>,
    ledger: Arc<Ledger>,
    counters: Arc<CounterStore>,
    global_rpm: u32,
    trust_xff: bool,
    business_port: u16,
}
```

The `LlmGateway::new` invocation passes `wired.health, wired.in_flight` instead of `wired.breakers`. Locate this around line 138 and update.

Same for the `Wired { ... }` literal construction around line 255 — `breakers,` becomes `health, in_flight,`.

- [ ] **Step 7: `gw-gateway/tests/e2e_full_flow.rs` + `gw-proxy/tests/end_to_end_openai.rs`**

Both test files construct `BreakerStore::default()` and pass it through. Mechanical fixes:

For each file:

(a) Imports: `use gw_routing::{BreakerStore, RetryConfig};` → `use gw_health::KeyHealthStore; use gw_routing::in_flight::InFlightStore; use gw_routing::RetryConfig;`

(b) Replace `let _breakers: Arc<BreakerStore> = Arc::new(BreakerStore::default());` (or similar) with:

```rust
let health: Arc<KeyHealthStore> = Arc::new(KeyHealthStore::default());
let in_flight: Arc<InFlightStore> = Arc::new(InFlightStore::default());
```

(c) Update the `LlmGateway::new(...)` call: where it currently passes `breakers.clone()`, pass `health.clone(), in_flight.clone()`.

The test's `LlmGateway::new` parameter signature is documented inline in the test (lines 60-75 of `end_to_end_openai.rs`). Update that signature comment too:

```rust
//   store, breakers, ratelimit, ledger, counters, retry_cfg, global_rpm, trust_xff,
```

→

```rust
//   store, health, in_flight, ratelimit, ledger, counters, retry_cfg, global_rpm, trust_xff,
```

- [ ] **Step 8: Build + test the full workspace**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10
```

All three MUST succeed.

If clippy fires unused-import warnings (e.g., a leftover `BreakerStore` import), remove the offending line. Common issues:

- `gw-proxy/src/filters/upstream.rs` may still import `BreakerStore` if you forgot — drop.
- `gw-gateway/src/lib.rs` may still bind `wired.breakers` — replace.
- Doc-comments that say "breakers" instead of "health" or "circuit" — rewrite.

- [ ] **Step 9: Commit**

```bash
git add crates/gw-proxy/Cargo.toml crates/gw-proxy/src \
        crates/gw-gateway/Cargo.toml crates/gw-gateway/src \
        crates/gw-gateway/tests/e2e_full_flow.rs \
        crates/gw-proxy/tests/end_to_end_openai.rs
git commit -m "refactor(gw-proxy,gw-gateway): migrate to KeyHealthStore + InFlightStore

LlmGateway now holds Arc<KeyHealthStore> + Arc<InFlightStore> instead
of Arc<BreakerStore>. The bootstrap and all integration tests follow.

filters::upstream — pick_peer signature gains both refs; PickCtx
already carries api_format from Task 3. InFlightGuard is sourced from
InFlightStore::get_or_create.

filters::logging — emits KeyHealthStore::record(key, api_format,
OutcomeKind) on every request termination. OutcomeKind::Failure
carries http_status; retry_after_secs is None in v1 (K-B layers that
in Phase 3).

breaker.rs is still on disk but no longer referenced anywhere outside
itself — Task 6 deletes it. Workspace is fully green at this commit."
```

---

## Task 6: Delete `gw-routing::breaker.rs`

**Files:**

- Delete: `crates/gw-routing/src/breaker.rs`
- Delete: `crates/gw-routing/tests/breaker.rs`
- Modify: `crates/gw-routing/src/lib.rs`

- [ ] **Step 1: Delete the files**

```bash
git rm crates/gw-routing/src/breaker.rs crates/gw-routing/tests/breaker.rs
```

- [ ] **Step 2: Update `crates/gw-routing/src/lib.rs`**

Replace the existing content with:

```rust
//! gw-routing: 候选 Key 过滤、选择策略（8 种）、in-flight 计数与重试预算。
//!
//! 主要导出：
//! - [`InFlightStore`] / [`InFlightGuard`] / [`InFlightCounter`] —— per-key
//!   in-flight counter (extracted from the retired `breaker` module per
//!   K-A ADR-KA-8).
//! - [`Strategy`] / [`StrategyAny`] / [`PickCtx`] —— 8 种策略 (ADR-009),
//!   now taking `&KeyHealthStore` + `&InFlightStore` instead of the retired
//!   `BreakerStore`.
//! - [`RetryConfig`] / [`RetryMode`] / [`RetryExecutor`] —— 重试预算与退避
//!   (ADR-022). Phase 4 R-E replaces this with `failover.rs`.

pub mod candidates;
pub mod in_flight;
pub mod retry;
pub mod strategy;

pub use in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
pub use retry::{RetryConfig, RetryExecutor, RetryMode};
pub use strategy::{PickCtx, Strategy, StrategyAny};
```

- [ ] **Step 3: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test --workspace 2>&1 | tail -20
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
cargo bench -p gw-routing --no-run 2>&1 | tail -5
```

All green.

The breaker tests that previously lived in `crates/gw-routing/tests/breaker.rs` are now obsolete — the circuit/sliding-window semantics they exercised are tested in `gw-health/tests/key_health.rs` (Task 1 step 9). The in-flight test is in `gw-routing/tests/in_flight.rs` (Task 2 step 3). No new tests needed.

- [ ] **Step 4: Run the invariants harness to confirm no leftover breaker references**

```bash
bash scripts/check-invariants.sh
```

The `no_legacy_breaker` lint is still PENDING. Task 7 activates it. But you can preview:

```bash
grep -rnE '\bBreakerState\b|\bBreakerStore\b' crates/ 2>/dev/null || echo "no legacy breaker"
```

Expected: `no legacy breaker`.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-routing/src/lib.rs
git commit -m "refactor(gw-routing): delete breaker.rs (A2)

Per integration spec §2 表 A A2: gw-routing::breaker.rs is fully retired.
Its three concerns are split:

- Health / circuit / sliding window  → gw-health::KeyHealthStore
  (multi-dim (KeyId, ApiFormat), explicit cooldown, ADR-008 trip rule)
- In-flight counter                  → gw-routing::in_flight::InFlightStore
  (single AtomicU32 per key, no state machine)
- (Old) per-key breaker tests        → covered by gw-health and
  gw-routing/tests/in_flight.rs respectively

lib.rs re-exports update to remove BreakerState / BreakerStore.
InFlightGuard remains via the in_flight module."
```

---

## Task 7: Final wrap — CLAUDE.md, lints, integration spec amendment

**Files:**

- Modify: `CLAUDE.md`
- Modify: `scripts/check-invariants.sh`
- Modify: `docs/superpowers/specs/2026-05-20-v2-integration-design.md`

- [ ] **Step 1: Update `CLAUDE.md` crate layering rules**

Find the section `## Crate layering rules (v2 integration)`. Locate the **Business layer** bullet:

```
- **Business layer** (`gw-adapter`, `gw-health`, `gw-task`): one crate per business concern. ...
```

It already lists `gw-health` — confirm the language is consistent. If `gw-health` is already mentioned, no change needed; otherwise add it in alphabetical order.

Also find the section listing `gw-routing` under the supporting layer:

```
- **Supporting layer** (`gw-core`, `gw-codec`, `gw-codec-macros`, `gw-store`, `gw-routing`, `gw-auth`, `gw-ledger`): ...
```

The wording `gw-routing      （含 in_flight，不含 health）` (or its English equivalent) should be in the C-1 dependency graph note. If the current CLAUDE.md doesn't include the parenthetical, add a sentence after the Supporting layer bullet:

```
  `gw-routing` carries `InFlightStore` only; health/circuit moved to `gw-health` in Phase 1c.
```

- [ ] **Step 2: Activate `no_legacy_breaker` in `scripts/check-invariants.sh`**

Find the PENDING block:

```bash
# # no_legacy_breaker (Phase 1c: gw-health replaces breaker.rs)
# grep -rnE '\bBreakerState\b|\bBreakerStore\b' crates/ && bad "breaker types remain outside gw-health"
```

Remove it from the PENDING comments, and add the following active check above the `===== PENDING lints` separator (placed near the other active blocks in alphabetical order):

```bash
# ---------------------------------------------------------------------------
# no_legacy_breaker (§5.2) — gw-health replaces breaker.rs.
# ---------------------------------------------------------------------------
echo "[no_legacy_breaker] BreakerState / BreakerStore must not appear"
if grep -rnE '\bBreakerState\b|\bBreakerStore\b' crates/ 2>/dev/null; then
  bad "BreakerState / BreakerStore remains"
else
  ok "no breaker types remain"
fi
```

- [ ] **Step 3: Add Living amendment to integration spec §6**

In `docs/superpowers/specs/2026-05-20-v2-integration-design.md`, find section §6 (Living amendments — currently empty). Add:

```markdown
- 2026-05-20  Bm2 re-timing: `PickCtx::api_format` + Strategy uses `KeyHealthStore::is_available(key, fmt)`
  Why: integration spec §2.3 Bm2 placed this in Phase 4, but Phase 1c (A2) deletes `gw-routing::breaker.rs`, which forces strategies onto `KeyHealthStore::is_available` immediately. Without `api_format` on `PickCtx` the multi-dim lookup has no format input. The field lands in Phase 1c rather than Phase 4.
  How to apply: no follow-up needed — the field is plumbed end-to-end as of Phase 1c. Phase 4 R-AB / R-E build on top with no further structural change.
```

- [ ] **Step 4: Final verify**

```bash
cargo test --workspace 2>&1 | tail -5
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -5
bash scripts/check-invariants.sh 2>&1 | tail -15
```

All three must exit 0. The harness should now report **9** active checks (4 from Phase 1a + 4 from Phase 1b + 1 new from Phase 1c). Note: `no_legacy_breaker` line is the new one — confirm it shows `ok`.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md scripts/check-invariants.sh \
        docs/superpowers/specs/2026-05-20-v2-integration-design.md
git commit -m "ci+docs: activate no_legacy_breaker; CLAUDE.md gw-health row; spec amendment

- scripts/check-invariants.sh: 5th Phase 1c active lint
  (no_legacy_breaker) — moved from PENDING.
- CLAUDE.md: gw-routing now noted as 'carries in_flight only;
  health/circuit moved to gw-health'.
- Integration spec §6 Living amendments: Bm2 re-timing entry — the
  PickCtx::api_format field lands in Phase 1c instead of Phase 4
  because A2 (breaker.rs deletion) forces it.

Phase 1c complete: gw-health crate live, BreakerStore retired,
9/9 invariants green."
```

---

## Self-review checklist (run after Task 7 commits)

| # | Spec / lint | Plan task | Verified? |
|---|---|---|---|
| §2 表 A A2 `breaker.rs` deletion | Task 6 | `find crates/gw-routing/src/breaker.rs` returns nothing |
| K-A ADR-KA-1 new `gw-health` crate | Task 1 | `crates/gw-health` exists, registered in workspace |
| K-A ADR-KA-2 `FormatHealth` 6-field struct | Task 1 step 5 | grep |
| K-A ADR-KA-3 `is_available` + `peek` | Task 1 step 6 | `cargo test -p gw-health key_health::cooldown_overrides_closed_circuit` |
| K-A ADR-KA-4 `record(Success/Failure)` | Task 1 step 6 | `record_under_floor_does_not_trip_circuit` + `record_over_floor_trips_circuit_to_open` |
| K-A ADR-KA-5 `apply_cooldown` monotonic | Task 1 step 9 | `apply_cooldown_is_monotonic` test |
| K-A ADR-KA-7 cooldown priority over circuit | Task 1 step 9 | `cooldown_overrides_closed_circuit` |
| K-A ADR-KA-8 in_flight in gw-routing | Task 2 | grep |
| K-A ADR-KA-9 `PickCtx.api_format` | Task 3 | grep |
| Bm5 `ApiFormat = ProtocolKind` | Task 1 step 8 | grep |
| Bm3 gw-health crate-layering registered | Task 7 step 1 | CLAUDE.md grep |
| §5.2 `no_legacy_breaker` lint active | Task 7 step 2 | harness output |
| Phase 1 exit gate: `cargo test --workspace` green | Task 5 onward | confirmed |

If any row fails, fix before declaring Phase 1c complete.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-20-phase-1c-key-health.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
