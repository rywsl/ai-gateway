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
        let ok = matches!(outcome, OutcomeKind::Success);
        self.get_or_create(key, fmt).record(ok);
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

fn snapshot_to_string(v: serde_json::Value) -> String {
    // serde_json::to_string on a Value cannot fail (no custom serializers in
    // play). Falling back to a minimal empty-object literal preserves the
    // "always valid JSON" contract callers rely on.
    serde_json::to_string(&v).unwrap_or_else(|_| String::from("{\"entries\":[]}"))
}

fn circuit_label(c: CircuitState) -> &'static str {
    match c {
        CircuitState::Closed => "closed",
        CircuitState::Open => "open",
        CircuitState::HalfOpen => "half_open",
    }
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
}
