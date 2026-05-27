//! Per-(key, format) TTFT and error-rate sliding windows.

use dashmap::DashMap;
use gw_core::ids::KeyId;
use gw_core::protocol::ApiFormat;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const TTFT_WINDOW_CAPACITY: usize = 1024;
const TTFT_WINDOW_TTL: Duration = Duration::from_secs(300);
const ERROR_WINDOW_TTL: Duration = Duration::from_secs(30);
const MIN_SAMPLES_FOR_P50: usize = 5;
const P50_RECOMPUTE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct LatencyTracker {
    pub by_key_format: DashMap<(KeyId, ApiFormat), Arc<KeyLatencyState>>,
}

pub struct KeyLatencyState {
    ttft_samples: Mutex<VecDeque<(Instant, Duration)>>,
    error_samples: Mutex<VecDeque<(Instant, bool)>>,
    cached_p50_ms: AtomicU64,
    last_computed: Mutex<Option<Instant>>,
}

#[derive(Clone, Debug)]
pub struct LatencySnapshot {
    pub key_id: KeyId,
    pub format: ApiFormat,
    pub p50_ttft_ms: Option<u64>,
    pub error_rate_30s: f64,
}

impl Default for KeyLatencyState {
    fn default() -> Self {
        Self {
            ttft_samples: Mutex::new(VecDeque::new()),
            error_samples: Mutex::new(VecDeque::new()),
            cached_p50_ms: AtomicU64::new(0),
            last_computed: Mutex::new(None),
        }
    }
}

impl LatencyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_ttft(&self, key_id: &KeyId, format: ApiFormat, duration: Duration) {
        let now = Instant::now();
        let state = self.get_or_create(key_id, format);
        state.record_ttft_at(now, duration);
        state.record_error_at(now, false);
    }

    pub fn record_error(&self, key_id: &KeyId, format: ApiFormat) {
        self.get_or_create(key_id, format)
            .record_error_at(Instant::now(), true);
    }

    pub fn p50_ttft_ms(&self, key_id: &KeyId, format: ApiFormat) -> Option<u64> {
        let state = self.by_key_format.get(&(key_id.clone(), format))?.clone();
        state.p50_ttft_ms(Instant::now())
    }

    pub fn error_rate_30s(&self, key_id: &KeyId, format: ApiFormat) -> f64 {
        let Some(state) = self.by_key_format.get(&(key_id.clone(), format)) else {
            return 0.0;
        };
        state.error_rate_30s(Instant::now())
    }

    pub fn snapshot_for(&self, key_id: &KeyId, format: ApiFormat) -> LatencySnapshot {
        let now = Instant::now();
        let (p50_ttft_ms, error_rate_30s) = self
            .by_key_format
            .get(&(key_id.clone(), format))
            .map(|state| state.snapshot(now))
            .unwrap_or((None, 0.0));
        LatencySnapshot {
            key_id: key_id.clone(),
            format,
            p50_ttft_ms,
            error_rate_30s,
        }
    }

    pub fn snapshot_for_pairs<'a, I>(&self, pairs: I) -> Vec<LatencySnapshot>
    where
        I: IntoIterator<Item = (&'a KeyId, ApiFormat)>,
    {
        let now = Instant::now();
        pairs
            .into_iter()
            .map(|(key_id, format)| {
                let (p50_ttft_ms, error_rate_30s) = self
                    .by_key_format
                    .get(&(key_id.clone(), format))
                    .map(|state| state.snapshot(now))
                    .unwrap_or((None, 0.0));
                LatencySnapshot {
                    key_id: key_id.clone(),
                    format,
                    p50_ttft_ms,
                    error_rate_30s,
                }
            })
            .collect()
    }

    pub fn snapshot_existing(&self) -> Vec<LatencySnapshot> {
        let now = Instant::now();
        self.by_key_format
            .iter()
            .map(|entry| {
                let (key_id, format) = entry.key();
                let (p50_ttft_ms, error_rate_30s) = entry.value().snapshot(now);
                LatencySnapshot {
                    key_id: key_id.clone(),
                    format: *format,
                    p50_ttft_ms,
                    error_rate_30s,
                }
            })
            .collect()
    }

    pub fn evict_stale(&self, max_age: Duration) {
        let now = Instant::now();
        self.by_key_format
            .retain(|_, state| !state.all_samples_older_than(now, max_age));
    }

    fn get_or_create(&self, key_id: &KeyId, format: ApiFormat) -> Arc<KeyLatencyState> {
        self.by_key_format
            .entry((key_id.clone(), format))
            .or_insert_with(|| Arc::new(KeyLatencyState::default()))
            .clone()
    }

    #[cfg(test)]
    fn record_ttft_at(&self, key_id: &KeyId, format: ApiFormat, at: Instant, duration: Duration) {
        let state = self.get_or_create(key_id, format);
        state.record_ttft_at(at, duration);
        state.record_error_at(at, false);
    }
}

impl KeyLatencyState {
    fn record_ttft_at(&self, at: Instant, duration: Duration) {
        let mut samples = self.ttft_samples.lock().unwrap();
        samples.push_back((at, duration));
        prune_older_than(&mut samples, at, TTFT_WINDOW_TTL);
        while samples.len() > TTFT_WINDOW_CAPACITY {
            samples.pop_front();
        }
    }

    fn record_error_at(&self, at: Instant, is_error: bool) {
        let mut samples = self.error_samples.lock().unwrap();
        samples.push_back((at, is_error));
        prune_older_than(&mut samples, at, ERROR_WINDOW_TTL);
    }

    fn p50_ttft_ms(&self, now: Instant) -> Option<u64> {
        if self.cache_is_fresh(now) {
            return Some(self.cached_p50_ms.load(Ordering::Relaxed));
        }

        let mut samples = self.ttft_samples.lock().unwrap();
        prune_older_than(&mut samples, now, TTFT_WINDOW_TTL);
        if samples.len() < MIN_SAMPLES_FOR_P50 {
            return None;
        }

        let mut durations_ms: Vec<u64> = samples.iter().map(|(_, d)| duration_ms(*d)).collect();
        durations_ms.sort_unstable();
        let p50 = durations_ms[durations_ms.len() / 2];
        self.cached_p50_ms.store(p50, Ordering::Relaxed);
        *self.last_computed.lock().unwrap() = Some(now);
        Some(p50)
    }

    fn error_rate_30s(&self, now: Instant) -> f64 {
        let mut samples = self.error_samples.lock().unwrap();
        prune_older_than(&mut samples, now, ERROR_WINDOW_TTL);
        if samples.is_empty() {
            return 0.0;
        }
        let errors = samples.iter().filter(|(_, is_error)| *is_error).count();
        errors as f64 / samples.len() as f64
    }

    fn snapshot(&self, now: Instant) -> (Option<u64>, f64) {
        (self.p50_ttft_ms(now), self.error_rate_30s(now))
    }

    fn all_samples_older_than(&self, now: Instant, max_age: Duration) -> bool {
        let ttft_old = self
            .ttft_samples
            .lock()
            .unwrap()
            .iter()
            .all(|(at, _)| now.duration_since(*at) > max_age);
        let errors_old = self
            .error_samples
            .lock()
            .unwrap()
            .iter()
            .all(|(at, _)| now.duration_since(*at) > max_age);
        ttft_old && errors_old
    }

    fn cache_is_fresh(&self, now: Instant) -> bool {
        self.last_computed
            .lock()
            .unwrap()
            .is_some_and(|computed_at| now.duration_since(computed_at) < P50_RECOMPUTE_INTERVAL)
    }
}

fn prune_older_than<T>(samples: &mut VecDeque<(Instant, T)>, now: Instant, ttl: Duration) {
    while samples
        .front()
        .is_some_and(|(at, _)| now.duration_since(*at) > ttl)
    {
        samples.pop_front();
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::protocol::ProtocolKind;

    fn key() -> KeyId {
        KeyId::from("key-a")
    }

    #[test]
    fn test_p50_accuracy() {
        let tracker = LatencyTracker::new();
        let key = key();
        for _ in 0..50 {
            tracker.record_ttft(&key, ProtocolKind::OpenAi, Duration::from_millis(10));
        }
        for _ in 0..50 {
            tracker.record_ttft(&key, ProtocolKind::OpenAi, Duration::from_millis(20));
        }

        let p50 = tracker
            .p50_ttft_ms(&key, ProtocolKind::OpenAi)
            .expect("enough samples for p50");
        assert!((10..=20).contains(&p50), "p50={p50}");
    }

    #[test]
    fn test_stale_window_returns_none() {
        let tracker = LatencyTracker::new();
        let key = key();
        let old = Instant::now() - TTFT_WINDOW_TTL - Duration::from_secs(1);
        for _ in 0..10 {
            tracker.record_ttft_at(&key, ProtocolKind::OpenAi, old, Duration::from_millis(10));
        }

        assert_eq!(tracker.p50_ttft_ms(&key, ProtocolKind::OpenAi), None);
    }

    #[test]
    fn test_error_rate() {
        let tracker = LatencyTracker::new();
        let key = key();
        for _ in 0..7 {
            tracker.record_ttft(&key, ProtocolKind::OpenAi, Duration::from_millis(10));
        }
        for _ in 0..3 {
            tracker.record_error(&key, ProtocolKind::OpenAi);
        }

        let rate = tracker.error_rate_30s(&key, ProtocolKind::OpenAi);
        assert!((rate - 0.3).abs() < f64::EPSILON, "rate={rate}");
    }

    #[test]
    fn test_evict_stale() {
        let tracker = LatencyTracker::new();
        let key = key();
        let old = Instant::now() - Duration::from_secs(60);
        for _ in 0..10 {
            tracker.record_ttft_at(&key, ProtocolKind::OpenAi, old, Duration::from_millis(10));
        }

        tracker.evict_stale(Duration::from_secs(30));

        assert_eq!(tracker.p50_ttft_ms(&key, ProtocolKind::OpenAi), None);
        assert!(!tracker
            .by_key_format
            .contains_key(&(key, ProtocolKind::OpenAi)));
    }

    #[test]
    fn test_min_samples() {
        let tracker = LatencyTracker::new();
        let key = key();
        for _ in 0..4 {
            tracker.record_ttft(&key, ProtocolKind::OpenAi, Duration::from_millis(10));
        }

        assert_eq!(tracker.p50_ttft_ms(&key, ProtocolKind::OpenAi), None);
    }

    #[test]
    fn test_cache_recompute() {
        let tracker = LatencyTracker::new();
        let key = key();
        for _ in 0..5 {
            tracker.record_ttft(&key, ProtocolKind::OpenAi, Duration::from_millis(10));
        }
        assert_eq!(tracker.p50_ttft_ms(&key, ProtocolKind::OpenAi), Some(10));

        for _ in 0..5 {
            tracker.record_ttft(&key, ProtocolKind::OpenAi, Duration::from_millis(500));
        }

        assert_eq!(tracker.p50_ttft_ms(&key, ProtocolKind::OpenAi), Some(10));
    }
}
