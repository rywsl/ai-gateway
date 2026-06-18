//! `KeyHealthStore` — the public face of `gw-health`. Owns the
//! `DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>` and exposes the 5
//! verbs callers need: `is_available`, `peek`, `record`, `apply_cooldown`,
//! `clear_cooldown`.

use crate::cooldown::{CircuitState, CooldownDirective, CooldownReason};
use crate::format_health::FormatHealth;
use dashmap::DashMap;
use gw_core::ids::KeyId;
use gw_core::protocol::ApiFormat;
use std::sync::atomic::Ordering;
use std::sync::Arc;

type HealthTransitionObserver = Arc<dyn Fn(&KeyId, ApiFormat, bool) + Send + Sync>;

const RETRY_AFTER_MIN_SECS: u32 = 1;
const RETRY_AFTER_MAX_SECS: u32 = 3_600;

fn current_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn health_snapshot(h: &FormatHealth, now: u64) -> HealthSnapshot {
    let (_n, rate) = h.failure_window.lock().failure_rate();
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

#[derive(Default)]
pub struct KeyHealthStore {
    pub by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>,
    transition_observers: parking_lot::RwLock<Vec<HealthTransitionObserver>>,
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

pub struct KeyHealthEntrySnapshot {
    pub key_id: KeyId,
    pub format: ApiFormat,
    pub available: bool,
    pub health: HealthSnapshot,
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

    pub fn register_transition_observer<F>(&self, observer: F)
    where
        F: Fn(&KeyId, ApiFormat, bool) + Send + Sync + 'static,
    {
        self.transition_observers.write().push(Arc::new(observer));
    }

    fn notify_transition(&self, key: &KeyId, fmt: ApiFormat, available: bool) {
        let observers = self.transition_observers.read().clone();
        for observer in observers {
            observer(key, fmt, available);
        }
    }

    pub fn is_available(&self, key: &KeyId, fmt: ApiFormat) -> bool {
        self.get_or_create(key, fmt).is_available()
    }

    pub fn peek(&self, key: &KeyId, fmt: ApiFormat) -> HealthSnapshot {
        let h = self.get_or_create(key, fmt);
        health_snapshot(&h, current_epoch_ms())
    }

    pub fn snapshot_for_pairs<'a, I>(&self, pairs: I) -> Vec<KeyHealthEntrySnapshot>
    where
        I: IntoIterator<Item = (&'a KeyId, ApiFormat)>,
    {
        let now = current_epoch_ms();
        pairs
            .into_iter()
            .map(|(key_id, format)| {
                let h = self.get_or_create(key_id, format);
                KeyHealthEntrySnapshot {
                    key_id: key_id.clone(),
                    format,
                    available: h.is_available(),
                    health: health_snapshot(&h, now),
                }
            })
            .collect()
    }

    pub fn snapshot_entries(&self) -> Vec<KeyHealthEntrySnapshot> {
        let now = current_epoch_ms();
        self.by_key_format
            .iter()
            .map(|entry| {
                let (key_id, format) = entry.key();
                let h = entry.value();
                KeyHealthEntrySnapshot {
                    key_id: key_id.clone(),
                    format: *format,
                    available: h.is_available(),
                    health: health_snapshot(h, now),
                }
            })
            .collect()
    }

    pub fn snapshot_entries_for_key(&self, key_id: &str) -> Vec<KeyHealthEntrySnapshot> {
        self.snapshot_entries()
            .into_iter()
            .filter(|entry| entry.key_id.as_ref() == key_id)
            .collect()
    }

    pub fn record(&self, key: &KeyId, fmt: ApiFormat, outcome: OutcomeKind) {
        let health = self.get_or_create(key, fmt);
        let was_available = health.is_available();
        let ok = matches!(outcome, OutcomeKind::Success);
        health.record(ok);
        if let OutcomeKind::Failure {
            http_status,
            retry_after_secs,
        } = outcome
        {
            if let Some(directive) =
                failure_payload_directive(key.as_ref(), http_status, retry_after_secs)
            {
                health.apply_cooldown(directive);
            }
        }
        let is_available = health.is_available();
        if was_available != is_available {
            self.notify_transition(key, fmt, is_available);
        }
    }

    pub fn apply_cooldown(&self, key: &KeyId, fmt: ApiFormat, d: CooldownDirective) {
        let health = self.get_or_create(key, fmt);
        let was_available = health.is_available();
        health.apply_cooldown(d);
        metrics::counter!(
            "gw_health_apply_cooldown_total",
            "reason" => d.reason.label(),
        )
        .increment(1);
        let is_available = health.is_available();
        if was_available != is_available {
            self.notify_transition(key, fmt, is_available);
        }
    }

    pub fn clear_cooldown(&self, key: &KeyId, fmt: ApiFormat) {
        let health = self.get_or_create(key, fmt);
        let was_available = health.is_available();
        health.clear_cooldown();
        let is_available = health.is_available();
        if was_available != is_available {
            self.notify_transition(key, fmt, is_available);
        }
    }

    /// Full runtime reset of the `(key, fmt)` cell — empties the failure
    /// sliding window, zeroes consecutive failures, closes the circuit, and
    /// clears any cooldown. See [`FormatHealth::reset_runtime`]. Backs the
    /// admin `reset-cost` control-plane command; distinct from
    /// [`Self::clear_cooldown`], which only lifts an explicit cooldown.
    pub fn reset_runtime(&self, key: &KeyId, fmt: ApiFormat) {
        let health = self.get_or_create(key, fmt);
        let was_available = health.is_available();
        health.reset_runtime();
        metrics::counter!("gw_health_reset_runtime_total").increment(1);
        let is_available = health.is_available();
        if was_available != is_available {
            self.notify_transition(key, fmt, is_available);
        }
    }

    /// JSON snapshot of every `(key, format)` health row, intended for the
    /// admin `/healthz/circuits` endpoint (T3-C consumer). Returns a JSON
    /// object of the shape:
    ///
    /// ```json
    /// {
    ///   "entries": [
    ///     {
    ///       "key_id": "...",
    ///       "format": "openai",
    ///       "available": true,
    ///       "circuit": "closed",
    ///       "in_cooldown": false,
    ///       "cooldown_until_ms": null,
    ///       "cooldown_reason": "none",
    ///       "consecutive_failures": 0
    ///     }
    ///   ]
    /// }
    /// ```
    ///
    /// Observability JSON only — not a wire contract. Field names may
    /// evolve; callers should not parse this in production code.
    pub fn snapshot_json(&self) -> String {
        snapshot_to_string(self.snapshot_value(None))
    }

    /// JSON snapshot filtered to a single `key_id`, used by
    /// `/healthz/key/<id>`. Same row shape as [`Self::snapshot_json`].
    pub fn key_snapshot_json(&self, key_id: &str) -> String {
        snapshot_to_string(self.snapshot_value(Some(key_id)))
    }

    fn snapshot_value(&self, key_filter: Option<&str>) -> serde_json::Value {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut entries: Vec<serde_json::Value> = Vec::new();
        for r in self.by_key_format.iter() {
            let (kid, fmt) = r.key();
            if let Some(want) = key_filter {
                if kid.as_ref() != want {
                    continue;
                }
            }
            let h = r.value();
            let cooldown_deadline = h.cooldown_until_ms.load(Ordering::Acquire);
            let in_cooldown = cooldown_deadline > now_ms;
            let circuit = CircuitState::from_u8(h.circuit.load(Ordering::Acquire));
            let reason = CooldownReason::from_u16(h.cooldown_reason.load(Ordering::Acquire));
            entries.push(serde_json::json!({
                "key_id": kid.as_ref(),
                "format": fmt.as_str(),
                "available": h.is_available(),
                "circuit": circuit_label(circuit),
                "in_cooldown": in_cooldown,
                "cooldown_until_ms": if cooldown_deadline > 0 {
                    Some(cooldown_deadline)
                } else {
                    None
                },
                "cooldown_reason": reason.label(),
                "consecutive_failures": h
                    .consecutive_failures
                    .load(Ordering::Acquire),
            }));
        }
        serde_json::json!({ "entries": entries })
    }
}

fn failure_payload_directive(
    key: &str,
    http_status: u16,
    retry_after_secs: Option<u32>,
) -> Option<CooldownDirective> {
    match http_status {
        429 => Some(CooldownDirective {
            reason: CooldownReason::Status429RateLimit,
            ttl_secs: retry_after_ttl_secs(key, retry_after_secs, 60, "status429_rate_limit"),
            reset_consecutive: false,
        }),
        500..=599 => Some(CooldownDirective {
            reason: if http_status == 529 {
                CooldownReason::Status529Overloaded
            } else {
                CooldownReason::StatusTransient5xx
            },
            ttl_secs: retry_after_ttl_secs(key, retry_after_secs, 30, "status_transient_5xx"),
            reset_consecutive: false,
        }),
        _ => None,
    }
}

fn retry_after_ttl_secs(
    key: &str,
    retry_after_secs: Option<u32>,
    default_secs: u32,
    vendor: &'static str,
) -> u32 {
    let raw = retry_after_secs.unwrap_or(default_secs);
    let clamped = raw.clamp(RETRY_AFTER_MIN_SECS, RETRY_AFTER_MAX_SECS);
    if clamped != raw {
        metrics::counter!(
            "gw_health_cooldown_clamp_total",
            "key" => key.to_string(),
            "vendor" => vendor,
        )
        .increment(1);
    }
    clamped
}

fn snapshot_to_string(v: serde_json::Value) -> String {
    // serde_json::to_string on a Value cannot fail (no custom serializers in
    // play). Falling back to a minimal empty-object literal preserves the
    // "always valid JSON" contract callers rely on.
    serde_json::to_string(&v).unwrap_or_else(|_| String::from("{\"entries\":[]}"))
}

fn circuit_label(c: CircuitState) -> &'static str {
    // Delegate to the single source of truth on the type so the admin JSON
    // and the runtime control-plane snapshot share one wire label.
    c.wire_label()
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use crate::cooldown::{CooldownDirective, CooldownReason};
    use gw_core::protocol::ProtocolKind;

    #[test]
    fn empty_snapshot_returns_valid_json() {
        let store = KeyHealthStore::default();
        let json = store.snapshot_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_object(), "snapshot must be a JSON object");
        let entries = parsed
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries array present");
        assert!(entries.is_empty(), "fresh store has no entries");
    }

    #[test]
    fn key_snapshot_filters_by_key_id() {
        let store = KeyHealthStore::default();
        let k_keep = KeyId::from("k-keep");
        let k_drop = KeyId::from("k-drop");
        // Touch both so the store has two entries; apply a cooldown to
        // `k-keep` so the row carries observable state.
        store.apply_cooldown(
            &k_keep,
            ProtocolKind::OpenAi,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 60,
                reset_consecutive: false,
            },
        );
        let _ = store.is_available(&k_drop, ProtocolKind::OpenAi);

        let json = store.key_snapshot_json("k-keep");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let entries = parsed
            .get("entries")
            .and_then(|v| v.as_array())
            .expect("entries array present");
        assert_eq!(entries.len(), 1, "exactly one row for k-keep");
        assert_eq!(entries[0]["key_id"], "k-keep");
        assert_eq!(entries[0]["in_cooldown"], true);
    }

    #[test]
    fn record_429_failure_payload_applies_retry_after_cooldown() {
        let store = KeyHealthStore::default();
        let key = KeyId::from("vk-429");
        store.record(
            &key,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 429,
                retry_after_secs: Some(23),
            },
        );
        let snap = store.peek(&key, ProtocolKind::OpenAi);
        assert!(snap.in_cooldown);
        assert_eq!(snap.cooldown_reason, CooldownReason::Status429RateLimit);
    }

    #[test]
    fn record_429_clamps_retry_after_to_one_hour() {
        let store = KeyHealthStore::default();
        let key = KeyId::from("vk-429-long");
        store.record(
            &key,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 429,
                retry_after_secs: Some(u32::MAX),
            },
        );
        let snap = store.peek(&key, ProtocolKind::OpenAi);
        let until = snap.cooldown_until_ms.expect("cooldown deadline");
        let now = current_epoch_ms();

        assert!(snap.in_cooldown);
        assert_eq!(snap.cooldown_reason, CooldownReason::Status429RateLimit);
        assert!(
            until <= now + 3_600_000 + 1_000,
            "retry_after must clamp to roughly one hour, got delta={}ms",
            until.saturating_sub(now)
        );
    }

    #[test]
    fn transition_observer_fires_when_key_becomes_unavailable() {
        let store = KeyHealthStore::default();
        let key = KeyId::from("vk-observer");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_for_observer = seen.clone();
        store.register_transition_observer(move |key_id, format, available| {
            seen_for_observer.lock().unwrap().push((
                key_id.as_ref().to_string(),
                format.as_str().to_string(),
                available,
            ));
        });

        store.apply_cooldown(
            &key,
            ProtocolKind::OpenAi,
            CooldownDirective {
                reason: CooldownReason::Status429RateLimit,
                ttl_secs: 60,
                reset_consecutive: false,
            },
        );

        assert_eq!(
            *seen.lock().unwrap(),
            vec![("vk-observer".to_string(), "openai".to_string(), false)]
        );
    }

    #[test]
    fn record_5xx_payload_uses_transient_cooldown() {
        let store = KeyHealthStore::default();
        let key = KeyId::from("vk-500");
        store.record(
            &key,
            ProtocolKind::OpenAi,
            OutcomeKind::Failure {
                http_status: 500,
                retry_after_secs: None,
            },
        );
        let snap = store.peek(&key, ProtocolKind::OpenAi);
        assert!(snap.in_cooldown);
        assert_eq!(snap.cooldown_reason, CooldownReason::StatusTransient5xx);
    }

    #[test]
    fn reset_runtime_recovers_tripped_and_cooled_key() {
        let store = KeyHealthStore::default();
        let key = KeyId::from("vk-reset");
        let fmt = ProtocolKind::OpenAi;
        // Auto-trip the circuit: >=20 samples at >50% failure rate, each 5xx
        // also stacking a long explicit cooldown.
        for _ in 0..21 {
            store.record(
                &key,
                fmt,
                OutcomeKind::Failure {
                    http_status: 500,
                    retry_after_secs: Some(3_600),
                },
            );
        }
        store.apply_cooldown(
            &key,
            fmt,
            CooldownDirective {
                reason: CooldownReason::AdminForce,
                ttl_secs: 3_600,
                reset_consecutive: false,
            },
        );
        assert!(
            !store.is_available(&key, fmt),
            "key must be sidelined after circuit trip + cooldown"
        );

        store.reset_runtime(&key, fmt);

        assert!(
            store.is_available(&key, fmt),
            "reset_runtime must make the key routable again"
        );
        let snap = store.peek(&key, fmt);
        assert!(!snap.in_cooldown);
        assert_eq!(snap.cooldown_reason, CooldownReason::None);
        assert_eq!(snap.consecutive_failures, 0);
        assert_eq!(snap.circuit, CircuitState::Closed);
        assert_eq!(snap.failure_rate_30s, 0.0);
    }

    #[test]
    fn clear_cooldown_leaves_auto_tripped_circuit_unlike_reset() {
        let store = KeyHealthStore::default();
        let key = KeyId::from("vk-clear");
        let fmt = ProtocolKind::OpenAi;
        for _ in 0..21 {
            store.record(
                &key,
                fmt,
                OutcomeKind::Failure {
                    http_status: 500,
                    retry_after_secs: None,
                },
            );
        }
        // clear_cooldown lifts only the explicit cooldown; the auto-tripped
        // circuit (and the failure window that tripped it) survive.
        store.clear_cooldown(&key, fmt);
        assert!(
            !store.is_available(&key, fmt),
            "clear_cooldown must not recover an auto-tripped circuit — reset_runtime does"
        );
    }
}
