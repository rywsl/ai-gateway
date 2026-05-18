//! Circuit breaker store with 30s sliding window + half-open recovery (ADR-008).
//!
//! Each `KeyId` owns a [`BreakerState`] holding:
//! - a 30-second sliding window of `(Instant, success)` samples
//! - an atomic circuit state (`0 = closed`, `1 = open`, `2 = half-open`)
//! - an `open_until_ms` deadline (millis since UNIX epoch) for open → half-open transition
//! - an `in_flight` counter that the proxy increments on borrow and decrements on release
//!
//! Trip rule (ADR-008): with ≥20 samples and >50% failure rate, close → open
//! and set `open_until_ms = now + 60_000`. After the deadline elapses, the next
//! `is_available()` call CAS-flips open → half-open and returns `true` to admit a
//! probe request.
//!
//! v1 limitation: there is no explicit half-open → closed transition on success;
//! the circuit naturally relaxes as failure samples roll off the 30s window
//! (a recorded success while half-open does NOT immediately reset `circuit`).
//! TODO(v2): explicit half-open → closed transition on first success.

use dashmap::DashMap;
use gw_core::ids::KeyId;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// `0 = closed`, `1 = open`, `2 = half-open`.
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
            circuit: AtomicU8::new(0),
            open_until_ms: AtomicU64::new(0),
            in_flight: AtomicU32::new(0),
        }
    }

    /// Record one outcome (success or failure). On ≥20 samples with >50% failure
    /// rate, trips the breaker open for 60s.
    pub fn record(&self, ok: bool) {
        let mut w = self.failure_window.lock();
        w.record(ok);
        let (total, rate) = w.failure_rate();
        if total >= 20 && rate > 0.5 {
            self.circuit.store(1, Ordering::Release);
            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            self.open_until_ms
                .store(now_ms + 60_000, Ordering::Release);
        }
    }

    /// Returns `true` if the key may be used.
    ///
    /// - closed: always available.
    /// - open: available iff the open deadline has elapsed; the call CAS-flips
    ///   the state to half-open and admits a probe.
    /// - half-open: already past the deadline; remains available so the probe
    ///   in-flight can proceed.
    pub fn is_available(&self) -> bool {
        let c = self.circuit.load(Ordering::Acquire);
        if c == 0 {
            return true;
        }
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if now_ms >= self.open_until_ms.load(Ordering::Acquire) {
            // Only flip 1 → 2; if already 2, leave alone.
            let _ = self.circuit.compare_exchange(
                1,
                2,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return true;
        }
        false
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
