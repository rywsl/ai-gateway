use dashmap::DashMap;
use fred::interfaces::StreamsInterface;
use fred::types::{RedisValue, XReadResponse};
use gw_core::ids::ModelName;
use gw_store::redis_client::RedisCtx;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::JoinHandle;

use crate::reservation_gate::ReservationGate;

pub const RESERVATION_STREAM: &str = "events:reservation";

const DEFAULT_XREAD_BLOCK_MS: u64 = 200;
const DEFAULT_XREAD_COUNT: u64 = 128;

#[derive(Debug)]
pub struct ModelFrequencyCounter {
    pub count: AtomicU64,
    pub window_start_ms: AtomicU64,
    last_snapshot_count: AtomicU64,
    pub last_used_at_ms: AtomicU64,
}

impl ModelFrequencyCounter {
    fn new(now_ms: u64) -> Self {
        Self {
            count: AtomicU64::new(0),
            window_start_ms: AtomicU64::new(now_ms),
            last_snapshot_count: AtomicU64::new(0),
            last_used_at_ms: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelRateSnapshot {
    pub model: String,
    pub rate: f64,
    pub ts_ms: u64,
}

#[derive(Default)]
pub struct ModelFrequencyTracker {
    counters: DashMap<ModelName, Arc<ModelFrequencyCounter>>,
}

impl ModelFrequencyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_pick(&self, model: &str) {
        let key = ModelName::from(model);
        let counter = self
            .counters
            .entry(key)
            .or_insert_with(|| Arc::new(ModelFrequencyCounter::new(now_ms())));
        counter.count.fetch_add(1, Ordering::Relaxed);
        counter.last_used_at_ms.store(now_ms(), Ordering::Release);
    }

    pub fn snapshot_rates(&self) -> Vec<ModelRateSnapshot> {
        self.snapshot_rates_at(now_ms())
    }

    pub fn snapshot_rates_at(&self, ts_ms: u64) -> Vec<ModelRateSnapshot> {
        self.counters
            .iter()
            .filter_map(|entry| {
                let counter = entry.value();
                let current = counter.count.load(Ordering::Relaxed);
                let previous = counter.last_snapshot_count.swap(current, Ordering::Relaxed);
                let start_ms = counter.window_start_ms.swap(ts_ms, Ordering::Relaxed);
                if ts_ms <= start_ms {
                    return None;
                }
                let delta = current.saturating_sub(previous);
                let elapsed_secs = (ts_ms - start_ms) as f64 / 1_000.0;
                Some(ModelRateSnapshot {
                    model: entry.key().as_ref().to_string(),
                    rate: delta as f64 / elapsed_secs,
                    ts_ms,
                })
            })
            .collect()
    }

    /// Evict counters whose `last_used_at_ms` is older than now - threshold_ms.
    /// Returns the count of evicted entries.
    pub fn sweep_stale(&self, threshold_ms: u64) -> u64 {
        let now = now_ms();
        let mut removed: u64 = 0;
        self.counters.retain(|_model, counter| {
            let last = counter.last_used_at_ms.load(Ordering::Acquire);
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

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalModelView {
    pub total_rate: f64,
    pub instance_count: u32,
    pub last_updated_ms: u64,
}

#[derive(Clone, Debug)]
struct InstanceRate {
    rate: f64,
    ts_ms: u64,
}

#[derive(Default)]
pub struct ReservationGlobalView {
    by_model: DashMap<ModelName, GlobalModelView>,
    by_model_instance: DashMap<(ModelName, String), InstanceRate>,
    gate: ReservationGate,
    received_event: AtomicBool,
}

impl ReservationGlobalView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_gate(gate: ReservationGate) -> Self {
        Self {
            by_model: DashMap::new(),
            by_model_instance: DashMap::new(),
            gate,
            received_event: AtomicBool::new(false),
        }
    }

    pub fn get(&self, model: &ModelName) -> Option<GlobalModelView> {
        self.by_model.get(model).map(|entry| entry.value().clone())
    }

    pub fn gate_allows(&self, model: &ModelName) -> bool {
        self.gate.allows(model, Some(self))
    }

    pub fn has_received_event(&self) -> bool {
        self.received_event.load(Ordering::Acquire)
    }

    pub fn pressure_for(&self, model: &ModelName) -> Option<f64> {
        self.get(model)
            .map(|view| reservation_pressure(view.total_rate))
    }

    pub fn apply_event(&self, event: ReservationEvent) {
        self.received_event.store(true, Ordering::Release);
        self.by_model_instance.insert(
            (event.model.clone(), event.instance_id),
            InstanceRate {
                rate: event.rate.max(0.0),
                ts_ms: event.ts_ms,
            },
        );
        self.recompute_model(&event.model);
    }

    pub fn spawn(
        self: Arc<Self>,
        redis: RedisCtx,
        tracker: Arc<ModelFrequencyTracker>,
        instance_id: String,
        interval: Duration,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            let mut last_id = "$".to_string();
            loop {
                ticker.tick().await;
                let snapshots = tracker.snapshot_rates();
                for snapshot in snapshots {
                    if let Err(err) = xadd_reservation(&redis, &instance_id, &snapshot).await {
                        tracing::warn!(
                            target: "gw_routing::reservation",
                            error = %err,
                            model = %snapshot.model,
                            "events:reservation XADD failed"
                        );
                    }
                }
                if let Err(err) = self.xread_once(&redis, &mut last_id).await {
                    tracing::warn!(
                        target: "gw_routing::reservation",
                        error = %err,
                        "events:reservation XREAD failed"
                    );
                }
            }
        })
    }

    async fn xread_once(&self, redis: &RedisCtx, last_id: &mut String) -> Result<(), String> {
        let client = redis.pool.next();
        let raw: RedisValue = client
            .xread(
                Some(DEFAULT_XREAD_COUNT),
                Some(DEFAULT_XREAD_BLOCK_MS),
                vec![RESERVATION_STREAM],
                vec![last_id.as_str()],
            )
            .await
            .map_err(|e| e.to_string())?;
        if raw.is_null() {
            return Ok(());
        }
        if let RedisValue::Array(ref values) = raw {
            if values.is_empty() {
                return Ok(());
            }
        }
        let entries: XReadResponse<String, String, String, String> = raw
            .into_xread_response()
            .map_err(|e| format!("parse xread response: {e}"))?;
        for (_stream, stream_entries) in entries {
            for (entry_id, fields) in stream_entries {
                *last_id = entry_id;
                if let Some(event) = ReservationEvent::from_fields(&fields) {
                    self.apply_event(event);
                } else {
                    tracing::warn!(
                        target: "gw_routing::reservation",
                        fields = ?fields,
                        "events:reservation entry missing required fields"
                    );
                }
            }
        }
        Ok(())
    }

    fn recompute_model(&self, model: &ModelName) {
        let mut total_rate = 0.0;
        let mut instance_count = 0_u32;
        let mut last_updated_ms = 0_u64;
        for entry in self.by_model_instance.iter() {
            if &entry.key().0 == model {
                total_rate += entry.value().rate;
                instance_count = instance_count.saturating_add(1);
                last_updated_ms = last_updated_ms.max(entry.value().ts_ms);
            }
        }
        self.by_model.insert(
            model.clone(),
            GlobalModelView {
                total_rate,
                instance_count,
                last_updated_ms,
            },
        );
    }

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

#[derive(Clone, Debug, PartialEq)]
pub struct ReservationEvent {
    pub instance_id: String,
    pub model: ModelName,
    pub rate: f64,
    pub ts_ms: u64,
}

impl ReservationEvent {
    pub fn from_fields(fields: &HashMap<String, String>) -> Option<Self> {
        Some(Self {
            instance_id: fields.get("instance_id")?.clone(),
            model: ModelName::from(fields.get("model")?.as_str()),
            rate: fields.get("rate")?.parse().ok()?,
            ts_ms: fields.get("ts")?.parse().ok()?,
        })
    }
}

async fn xadd_reservation(
    redis: &RedisCtx,
    instance_id: &str,
    snapshot: &ModelRateSnapshot,
) -> Result<(), String> {
    let fields = vec![
        ("instance_id".to_string(), instance_id.to_string()),
        ("model".to_string(), snapshot.model.clone()),
        ("rate".to_string(), snapshot.rate.to_string()),
        ("ts".to_string(), snapshot.ts_ms.to_string()),
    ];
    redis
        .pool
        .next()
        .xadd::<(), _, _, _, _>(RESERVATION_STREAM, false, None::<()>, "*", fields)
        .await
        .map_err(|e| e.to_string())
}

fn reservation_pressure(total_rate: f64) -> f64 {
    if total_rate.is_finite() {
        (total_rate / 1_000.0).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frequency_sampling_computes_rate_from_delta() {
        let tracker = ModelFrequencyTracker::new();
        for _ in 0..100 {
            tracker.record_pick("gpt-4");
        }
        let samples = tracker.snapshot_rates_at(now_ms() + 5_000);
        let sample = samples
            .iter()
            .find(|sample| sample.model == "gpt-4")
            .expect("sample present");
        assert!((sample.rate - 20.0).abs() < 1.0, "rate={}", sample.rate);
    }

    #[test]
    fn frequency_sampling_uses_delta_since_last_snapshot() {
        let tracker = ModelFrequencyTracker::new();
        tracker.record_pick("claude");
        let first_ts = now_ms() + 1_000;
        let _ = tracker.snapshot_rates_at(first_ts);
        for _ in 0..50 {
            tracker.record_pick("claude");
        }
        let samples = tracker.snapshot_rates_at(first_ts + 5_000);
        let sample = samples
            .iter()
            .find(|sample| sample.model == "claude")
            .expect("sample present");
        assert!((sample.rate - 10.0).abs() < 0.1, "rate={}", sample.rate);
    }

    #[test]
    fn multi_instance_events_aggregate_global_view() {
        let view = ReservationGlobalView::new();
        view.apply_event(ReservationEvent {
            instance_id: "a".to_string(),
            model: ModelName::from("gpt-4"),
            rate: 12.5,
            ts_ms: 100,
        });
        view.apply_event(ReservationEvent {
            instance_id: "b".to_string(),
            model: ModelName::from("gpt-4"),
            rate: 7.5,
            ts_ms: 200,
        });
        let snapshot = view.get(&ModelName::from("gpt-4")).expect("view present");
        assert_eq!(snapshot.instance_count, 2);
        assert_eq!(snapshot.total_rate, 20.0);
        assert_eq!(snapshot.last_updated_ms, 200);
    }

    #[test]
    fn model_frequency_tracker_sweeps_stale_counters() {
        let tracker = ModelFrequencyTracker::default();
        let model_old = ModelName::from("model-old");
        let model_new = ModelName::from("model-new");
        tracker.record_pick("model-old");
        tracker.record_pick("model-new");

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
        let now = now_ms();
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
}
