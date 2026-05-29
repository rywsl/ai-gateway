//! Implements `gw_store::counter_dispatch::CounterHandler` for the credit-lease
//! ns set: `protected_lease`, `coef_table`, `coef_table_commit`. Wired into
//! the global registry by `install_counter_handler()` at gateway boot.

use crate::coef::{apply_coef_commit, apply_coef_entry, CommitOutcome, Dim};
use crate::lease::{upsert_protected, upsert_unprotected, Mode};
use gw_core::ids::UserId;
use gw_store::counter_dispatch::{register_handler, CounterHandler, DispatchOutcome};
use std::collections::HashMap;

struct LedgerCounterHandler;

impl CounterHandler for LedgerCounterHandler {
    fn handle(&self, ns: &str, fields: &HashMap<String, String>) -> DispatchOutcome {
        match ns {
            "protected_lease" => handle_protected_lease(fields),
            "coef_table" => handle_coef_table(fields),
            "coef_table_commit" => handle_coef_table_commit(fields),
            _ => DispatchOutcome::UnknownNs,
        }
    }
}

fn handle_protected_lease(fields: &HashMap<String, String>) -> DispatchOutcome {
    let uid = match fields.get("user_id") {
        Some(s) => UserId::from(s.as_str()),
        None => return DispatchOutcome::InvalidFields,
    };
    let mode_str = fields.get("mode").map(String::as_str).unwrap_or("");
    let mode = match Mode::from_str(mode_str) {
        Some(m) => m,
        None => return DispatchOutcome::InvalidFields,
    };
    match mode {
        Mode::Unprotected => {
            upsert_unprotected(&uid);
            DispatchOutcome::Handled
        }
        Mode::Protected => {
            let credits = fields
                .get("lease_credits")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let expires = fields
                .get("expires_at_ms")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let version = fields
                .get("coef_table_version")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let lease_id_str = fields.get("lease_id").cloned().unwrap_or_default();
            upsert_protected(&uid, credits, lease_id_str, version, expires);
            DispatchOutcome::Handled
        }
    }
}

fn handle_coef_table(fields: &HashMap<String, String>) -> DispatchOutcome {
    let version = match fields
        .get("coef_table_version")
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(v) => v,
        None => return DispatchOutcome::InvalidFields,
    };
    let model_id = match fields.get("model_id") {
        Some(s) => s.as_str(),
        None => return DispatchOutcome::InvalidFields,
    };
    let dim = match fields.get("dim").and_then(|s| Dim::from_str(s.as_str())) {
        Some(d) => d,
        None => return DispatchOutcome::InvalidFields,
    };
    let coef_per_token = fields
        .get("coef_per_token")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    apply_coef_entry(version, model_id, dim, coef_per_token);
    DispatchOutcome::Handled
}

fn handle_coef_table_commit(fields: &HashMap<String, String>) -> DispatchOutcome {
    let version = match fields
        .get("coef_table_version")
        .and_then(|s| s.parse::<u64>().ok())
    {
        Some(v) => v,
        None => return DispatchOutcome::InvalidFields,
    };
    let model_count = fields
        .get("model_count")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    let outcome = apply_coef_commit(version, model_count);
    match outcome {
        CommitOutcome::Incomplete { staged, expected } => {
            tracing::warn!(
                target: "gw_ledger::counter_handler",
                version,
                staged,
                expected,
                "coef_table commit: staged < expected; keeping previous version"
            );
        }
        CommitOutcome::Stale { committed, active } => {
            tracing::debug!(
                target: "gw_ledger::counter_handler",
                committed,
                active,
                "coef_table commit: stale; dropping"
            );
        }
        CommitOutcome::Activated => {
            tracing::info!(
                target: "gw_ledger::counter_handler",
                version,
                "coef_table commit: activated"
            );
        }
    }
    DispatchOutcome::Handled
}

/// Install the credit-lease handler into gw-store's global counter dispatch.
/// Call once at gateway boot, before any streams start consuming.
pub fn install_counter_handler() {
    register_handler(Box::new(LedgerCounterHandler));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coef::{current_snapshot, COEF_CACHE, COEF_STAGING, TEST_COEF_LOCK};
    use crate::lease::{Mode, LEASE_POOL};
    use std::sync::Arc;

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn reset_globals() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_COEF_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        COEF_CACHE.store(Arc::new(crate::coef::CoefSnapshot::empty()));
        COEF_STAGING.clear();
        // Note: LEASE_POOL is keyed by uid; tests use unique uids so no reset needed.
        guard
    }

    #[test]
    fn protected_lease_handler_upserts_protected_user() {
        let uid_str = "u_handler_test_1";
        LEASE_POOL.remove(&UserId::from(uid_str));

        let h = LedgerCounterHandler;
        let f = fields(&[
            ("ns", "protected_lease"),
            ("user_id", uid_str),
            ("mode", "protected"),
            ("lease_credits", "5000"),
            ("lease_id", "lease-abc"),
            ("expires_at_ms", "9999999999999"),
            ("coef_table_version", "7"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(h.handle("protected_lease", &f), DispatchOutcome::Handled);

        let s = LEASE_POOL.get(&UserId::from(uid_str)).unwrap();
        assert_eq!(s.mode(), Mode::Protected);
        assert_eq!(s.credits(), 5000);
    }

    #[test]
    fn protected_lease_handler_unprotected_clears_state() {
        let uid_str = "u_handler_test_2";
        LEASE_POOL.remove(&UserId::from(uid_str));
        // First seed protected state.
        upsert_protected(&UserId::from(uid_str), 1000, "l1".into(), 9, 9999999999999);
        assert_eq!(
            LEASE_POOL.get(&UserId::from(uid_str)).unwrap().mode(),
            Mode::Protected
        );

        let h = LedgerCounterHandler;
        let f = fields(&[
            ("ns", "protected_lease"),
            ("user_id", uid_str),
            ("mode", "unprotected"),
            ("lease_credits", "0"),
            ("lease_id", ""),
            ("expires_at_ms", "0"),
            ("coef_table_version", "0"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(h.handle("protected_lease", &f), DispatchOutcome::Handled);
        assert_eq!(
            LEASE_POOL.get(&UserId::from(uid_str)).unwrap().mode(),
            Mode::Unprotected
        );
    }

    #[test]
    fn coef_table_event_then_commit_swaps_snapshot() {
        let _g = reset_globals();
        let h = LedgerCounterHandler;

        let entry = fields(&[
            ("ns", "coef_table"),
            ("coef_table_version", "100"),
            ("model_id", "model-z"),
            ("dim", "input"),
            ("coef_per_token", "33"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(h.handle("coef_table", &entry), DispatchOutcome::Handled);
        assert!(current_snapshot().version < 100);

        let commit = fields(&[
            ("ns", "coef_table_commit"),
            ("coef_table_version", "100"),
            ("model_count", "1"),
            ("ts_ms", "0"),
        ]);
        assert_eq!(
            h.handle("coef_table_commit", &commit),
            DispatchOutcome::Handled
        );
        assert_eq!(current_snapshot().version, 100);
    }

    #[test]
    fn unknown_ns_returns_unknown_ns() {
        let h = LedgerCounterHandler;
        let f = fields(&[("ns", "unrelated")]);
        assert_eq!(h.handle("unrelated", &f), DispatchOutcome::UnknownNs);
    }

    #[test]
    fn protected_lease_missing_user_id_invalid() {
        let h = LedgerCounterHandler;
        let f = fields(&[("ns", "protected_lease"), ("mode", "protected")]);
        assert_eq!(
            h.handle("protected_lease", &f),
            DispatchOutcome::InvalidFields
        );
    }

    #[test]
    fn coef_table_invalid_dim_invalid() {
        let h = LedgerCounterHandler;
        let f = fields(&[
            ("ns", "coef_table"),
            ("coef_table_version", "1"),
            ("model_id", "m"),
            ("dim", "garbage"),
            ("coef_per_token", "1"),
        ]);
        assert_eq!(h.handle("coef_table", &f), DispatchOutcome::InvalidFields);
    }
}
