//! Three-tier deduction state + per-user circuit breaker (ADR-021).
//!
//! `DeductionState` lives in process memory (in-process counters), with periodic
//! flush to Redis via the ledger pipeline. `UserCircuit` short-circuits requests
//! when balance reaches zero or the user is banned.

use dashmap::DashMap;
use gw_core::ids::UserId;
use once_cell::sync::Lazy;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::time::Instant;

/// Grace window after `Open.until` before a USER_CIRCUIT entry becomes
/// eligible for eviction. Allows brief jitter between the circuit's
/// `until` and an upcoming consumer revalidation.
pub const USER_CIRCUIT_STALE_GRACE: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug)]
pub struct DeductionState {
    pub base_micro: AtomicI64,
    pub deducted_micro: AtomicI64,
    pub frozen_micro: AtomicI64,
}

impl DeductionState {
    /// `real = base - deducted - frozen` (in micro-units).
    pub fn real(&self) -> i64 {
        use std::sync::atomic::Ordering::Relaxed;
        self.base_micro.load(Relaxed)
            - self.deducted_micro.load(Relaxed)
            - self.frozen_micro.load(Relaxed)
    }
}

pub static DEDUCT: Lazy<DashMap<UserId, Arc<DeductionState>>> = Lazy::new(DashMap::new);

#[derive(Clone, Copy, Debug)]
pub enum UserCircuit {
    Closed,
    Open {
        until: Instant,
        reason: CircuitReason,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum CircuitReason {
    BalanceZero,
    BannedUser,
}

pub static USER_CIRCUIT: Lazy<DashMap<UserId, UserCircuit>> = Lazy::new(DashMap::new);

/// Evict `USER_CIRCUIT` entries whose `Open.until` has passed by more than
/// [`USER_CIRCUIT_STALE_GRACE`]. `Closed` entries are NOT evicted — without
/// a creation timestamp on `Closed` we cannot judge staleness, and the
/// current codebase has no `USER_CIRCUIT.insert(.., Closed)` writers.
///
/// Returns the count of evicted entries. Called per tick by [`DeductGc`].
pub fn sweep_user_circuit() -> u64 {
    let now = std::time::Instant::now();
    let mut removed: u64 = 0;
    USER_CIRCUIT.retain(|_uid, circuit| match *circuit {
        UserCircuit::Open { until, .. } => {
            if now > until + USER_CIRCUIT_STALE_GRACE {
                removed += 1;
                false
            } else {
                true
            }
        }
        UserCircuit::Closed => true,
    });
    if removed > 0 {
        metrics::counter!("gw_user_circuit_sweep_removed_total").increment(removed);
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    // USER_CIRCUIT is a global static; tests that assert counts must serialize.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn cleanup(prefix: &str) {
        let keys: Vec<_> = USER_CIRCUIT
            .iter()
            .filter(|e| e.key().as_ref().starts_with(prefix))
            .map(|e| e.key().clone())
            .collect();
        for k in keys {
            USER_CIRCUIT.remove(&k);
        }
    }

    #[test]
    fn sweep_user_circuit_evicts_expired_open() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_a_";
        cleanup(prefix);

        // Insert one Open entry whose `until` is far in the past (> STALE_GRACE).
        // The grace adds 5 min on top of `until` — pass `until = now - 10 min`.
        let key = UserId::from(format!("{prefix}expired"));
        USER_CIRCUIT.insert(
            key.clone(),
            UserCircuit::Open {
                until: Instant::now() - Duration::from_secs(600),
                reason: CircuitReason::BalanceZero,
            },
        );
        assert!(USER_CIRCUIT.contains_key(&key));

        let removed = sweep_user_circuit();

        assert_eq!(removed, 1);
        assert!(!USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_keeps_active_open() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_b_";
        cleanup(prefix);

        // Open with `until` in the future — must NOT be evicted.
        let key = UserId::from(format!("{prefix}active"));
        USER_CIRCUIT.insert(
            key.clone(),
            UserCircuit::Open {
                until: Instant::now() + Duration::from_secs(60),
                reason: CircuitReason::BalanceZero,
            },
        );

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        assert!(USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_keeps_recently_expired_open_within_grace() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_c_";
        cleanup(prefix);

        // Open with `until` slightly in the past but within STALE_GRACE (5 min).
        // until = now - 60s; grace = 5 min → still inside grace window.
        let key = UserId::from(format!("{prefix}grace"));
        USER_CIRCUIT.insert(
            key.clone(),
            UserCircuit::Open {
                until: Instant::now() - Duration::from_secs(60),
                reason: CircuitReason::BalanceZero,
            },
        );

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        assert!(USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_keeps_closed_unconditionally() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_d_";
        cleanup(prefix);

        // Closed entries are not touched. (Closed has no timestamp; without
        // writers in the codebase we cannot judge staleness — explicit policy.)
        let key = UserId::from(format!("{prefix}closed"));
        USER_CIRCUIT.insert(key.clone(), UserCircuit::Closed);

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        assert!(USER_CIRCUIT.contains_key(&key));
        cleanup(prefix);
    }

    #[test]
    fn sweep_user_circuit_on_empty_map_is_noop() {
        let _lock = SERIAL.lock().unwrap();
        let prefix = "ucsweep_e_";
        cleanup(prefix);

        let removed = sweep_user_circuit();

        assert_eq!(removed, 0);
        cleanup(prefix);
    }
}
