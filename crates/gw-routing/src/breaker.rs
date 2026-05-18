//! Circuit breaker store with 30s sliding window + half-open recovery (ADR-008).
//!
//! Each `KeyId` owns a [`BreakerState`] holding:
//! - a 30-second sliding window of `(Instant, success)` samples
//! - an atomic circuit state (closed / open / half-open)
//! - an `open_until_ms` deadline (millis since UNIX epoch) for open → half-open transition
//! - an `in_flight` counter that the proxy increments on borrow and decrements on release
//!
//! Trip rule (ADR-008): with ≥20 samples and >50% failure rate, close → open
//! and set `open_until_ms = now + 60_000`. After the deadline elapses, the next
//! `is_available()` call CAS-flips open → half-open and returns `true` to admit a
//! single probe request. Once half-open, further callers receive `false` until
//! the probe outcome resolves the circuit.
//!
//! Recovery: a successful sample recorded while half-open CAS-flips the circuit
//! back to closed. A failure while half-open will trip the breaker back to open
//! via the normal failure-rate check (if the rate threshold is still exceeded).

use dashmap::DashMap;
use gw_core::ids::KeyId;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Circuit-state encodings stored in `BreakerState::circuit`.
const CIRCUIT_CLOSED: u8 = 0;
const CIRCUIT_OPEN: u8 = 1;
const CIRCUIT_HALF_OPEN: u8 = 2;

/// Bounded 30-second sliding window of `(timestamp, success_flag)` samples.
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
            samples: VecDeque::with_capacity(256),
        }
    }

    /// Record one outcome and evict samples older than 30s.
    pub fn record(&mut self, ok: bool) {
        let now = Instant::now();
        self.samples.push_back((now, ok));
        let cutoff = now - Duration::from_secs(30);
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

pub struct BreakerState {
    pub failure_window: Mutex<SlidingWindow>,
    /// One of [`CIRCUIT_CLOSED`], [`CIRCUIT_OPEN`], [`CIRCUIT_HALF_OPEN`].
    pub circuit: AtomicU8,
    pub open_until_ms: AtomicU64,
    pub in_flight: AtomicU32,
}

impl Default for BreakerState {
    fn default() -> Self {
        Self::new()
    }
}

impl BreakerState {
    pub fn new() -> Self {
        Self {
            failure_window: Mutex::new(SlidingWindow::new()),
            circuit: AtomicU8::new(CIRCUIT_CLOSED),
            open_until_ms: AtomicU64::new(0),
            in_flight: AtomicU32::new(0),
        }
    }

    /// Record one outcome (success or failure). On ≥20 samples with >50% failure
    /// rate, trips the breaker open for 60s. On success while half-open,
    /// transitions the circuit back to closed.
    pub fn record(&self, ok: bool) {
        let mut w = self.failure_window.lock();
        w.record(ok);
        let (total, rate) = w.failure_rate();
        drop(w); // release before atomic ops
        if total >= 20 && rate > 0.5 {
            self.circuit.store(CIRCUIT_OPEN, Ordering::Release);
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            self.open_until_ms
                .store(now_ms + 60_000, Ordering::Release);
        } else if ok && self.circuit.load(Ordering::Acquire) == CIRCUIT_HALF_OPEN {
            // probe succeeded → close
            let _ = self.circuit.compare_exchange(
                CIRCUIT_HALF_OPEN,
                CIRCUIT_CLOSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    /// Returns `true` if the key may be used.
    ///
    /// - closed: always available.
    /// - open: available iff the open deadline has elapsed AND this caller wins
    ///   the CAS to claim the half-open probe slot. The CAS WINNER transitions
    ///   open → half-open and returns `true`; concurrent losers and any caller
    ///   arriving while the circuit is already half-open receive `false` (the
    ///   in-flight probe must resolve first).
    pub fn is_available(&self) -> bool {
        let c = self.circuit.load(Ordering::Acquire);
        if c == CIRCUIT_CLOSED {
            return true;
        }
        if c == CIRCUIT_HALF_OPEN {
            // Another probe is already in flight.
            return false;
        }
        // c == CIRCUIT_OPEN: check whether the deadline has elapsed.
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now_ms < self.open_until_ms.load(Ordering::Acquire) {
            return false;
        }
        // Deadline elapsed — try to claim the probe slot. Only the CAS winner
        // is admitted; concurrent callers must wait for the probe outcome.
        self.circuit
            .compare_exchange(
                CIRCUIT_OPEN,
                CIRCUIT_HALF_OPEN,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[derive(Default)]
pub struct BreakerStore {
    pub by_key: DashMap<KeyId, Arc<BreakerState>>,
}

impl BreakerStore {
    pub fn get_or_create(&self, k: &KeyId) -> Arc<BreakerState> {
        self.by_key
            .entry(k.clone())
            .or_insert_with(|| Arc::new(BreakerState::new()))
            .clone()
    }

    pub fn in_flight(&self, k: &KeyId) -> u32 {
        self.get_or_create(k).in_flight.load(Ordering::Relaxed)
    }
}

/// RAII increment of a [`BreakerState::in_flight`] counter. Incremented on
/// `new`, decremented on `Drop`. The proxy holds this on `ReqCtx` so the
/// counter releases on every termination path (success, error, cancel).
///
/// The counter feeds two consumers:
/// 1. [`crate::strategy::load_balance::LoadBalance`] — picks the candidate
///    with the lowest in-flight count.
/// 2. [`crate::candidates::get_candidate_keys`] — drops candidates whose
///    in-flight is at or above the per-key cap.
///
/// Without this guard both consumers see `0` for every key and the
/// LoadBalance strategy degenerates to "always pick the first candidate"
/// while the per-key cap is dead code.
pub struct InFlightGuard {
    state: Arc<BreakerState>,
}

impl InFlightGuard {
    /// Increment the in-flight counter on the supplied `BreakerState`. The
    /// matching decrement runs on `Drop`. Use `Ordering::AcqRel` because the
    /// counter is read with `Ordering::Relaxed` from both
    /// `LoadBalance::pick` and `get_candidate_keys`, and we want each
    /// observation of an increment to be paired with the prior decrement of
    /// the previous owner on the same thread (per-thread happens-before).
    pub fn new(state: Arc<BreakerState>) -> Self {
        state.in_flight.fetch_add(1, Ordering::AcqRel);
        Self { state }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.state.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}
