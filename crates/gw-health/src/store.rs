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
