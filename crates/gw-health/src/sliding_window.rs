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
    }
}
