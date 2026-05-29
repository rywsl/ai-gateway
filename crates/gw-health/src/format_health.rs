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
//! `closed → open` for 60s. Same numbers as the retired single-dim
//! circuit (ADR-008 carried over unchanged).
//!
//! Cooldown is OR'd with circuit state in `is_available`: either being
//! non-available makes the key unavailable.

use crate::cooldown::{CircuitState, CooldownDirective, CooldownReason};
use crate::sliding_window::SlidingWindow;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Threshold parameters lifted verbatim from the retired single-dim
/// circuit (ADR-008).
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
            // Note: cooldown_reason is NOT written here. The circuit-open
            // state is separately observable via `peek().circuit`. The
            // SlidingWindowTrip reason variant is reserved for explicit
            // K-B apply_cooldown calls in Phase 3.
        } else if ok
            && CircuitState::from_u8(self.circuit.load(Ordering::Acquire)) == CircuitState::HalfOpen
        {
            // Probe succeeded → close.
            let _ = self.circuit.compare_exchange(
                CircuitState::HalfOpen.as_u8(),
                CircuitState::Closed.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        } else if !ok
            && CircuitState::from_u8(self.circuit.load(Ordering::Acquire)) == CircuitState::HalfOpen
        {
            // Audit 2026-05-30 §1.4: a FAILED half-open probe must return to Open
            // and re-arm the 60s hold. Without this transition the circuit was
            // stuck HalfOpen forever (after the 60s open hold the 30s window is
            // near-empty, so the trip branch above can't fire on one sample), and
            // is_available() returns false for HalfOpen — permanently sidelining
            // the (key, fmt) until process restart. Matches the K-A spec
            // (2026-05-19-key-health-k-a-state-machine.md §"半开失败一次就回 open").
            // Re-arm open_until_ms only on CAS success so a concurrent winner
            // doesn't double-arm.
            if self
                .circuit
                .compare_exchange(
                    CircuitState::HalfOpen.as_u8(),
                    CircuitState::Open.as_u8(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.open_until_ms
                    .store(now_ms() + OPEN_HOLD_MS, Ordering::Release);
            }
        }
    }

    /// Apply an explicit cooldown. `cooldown_until_ms` only grows (max with
    /// existing value) so a later directive cannot shorten an in-flight
    /// cooldown.
    pub fn apply_cooldown(&self, d: CooldownDirective) {
        let candidate = now_ms() + (d.ttl_secs as u64) * 1000;
        // CAS up to a fixed-point max. The reason MUST only update when the
        // deadline actually advanced — a shorter directive that lost the max
        // race must not overwrite the reason of the still-active longer one.
        let mut advanced = false;
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
                advanced = true;
                break;
            }
        }
        if advanced {
            self.cooldown_reason
                .store(d.reason.as_u16(), Ordering::Release);
        }
        // `reset_consecutive` is always honoured (admin-unblock must clear
        // the counter even when the deadline didn't grow).
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
