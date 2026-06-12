//! LeasePool — per-user credit lease state for protected mode.
//! Spec §6.1.
//!
//! `LeaseGuard` mirrors `RequestFreeze` (`freeze.rs`): CAS-decrements
//! `LeaseState.credits` on acquire, refunds on settle if actual ≤ reserved,
//! and is idempotent on release/drop.

use arc_swap::ArcSwap;
use dashmap::DashMap;
use gw_core::ids::UserId;
use once_cell::sync::Lazy;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    Unprotected = 0,
    Protected = 1,
}

impl Mode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Mode::Unprotected),
            1 => Some(Mode::Protected),
            _ => None,
        }
    }

    pub fn parse_str(s: &str) -> Option<Self> {
        match s {
            "unprotected" => Some(Mode::Unprotected),
            "protected" => Some(Mode::Protected),
            _ => None,
        }
    }
}

pub struct LeaseState {
    pub credits: AtomicI64,
    pub expires_at_ms: AtomicU64,
    pub lease_id: ArcSwap<String>,
    pub mode: AtomicU8,
    pub coef_table_version: AtomicU64,
    pub last_seen_at_ms: AtomicU64,
}

impl LeaseState {
    fn new_unprotected() -> Self {
        Self {
            credits: AtomicI64::new(0),
            expires_at_ms: AtomicU64::new(0),
            lease_id: ArcSwap::new(Arc::new(String::new())),
            mode: AtomicU8::new(Mode::Unprotected as u8),
            coef_table_version: AtomicU64::new(0),
            last_seen_at_ms: AtomicU64::new(0),
        }
    }

    pub fn mode(&self) -> Mode {
        Mode::from_u8(self.mode.load(Ordering::Acquire)).unwrap_or(Mode::Unprotected)
    }

    pub fn credits(&self) -> i64 {
        self.credits.load(Ordering::Acquire)
    }
}

pub static LEASE_POOL: Lazy<DashMap<UserId, Arc<LeaseState>>> = Lazy::new(DashMap::new);

/// Idle TTL for `Unprotected` entries in `LEASE_POOL`. After transitioning to
/// `Unprotected` (via `upsert_unprotected`), an entry becomes eligible for
/// eviction once `now_ms() - last_seen_at_ms` exceeds this value. `Protected`
/// entries are never touched by the sweep.
pub const LEASE_POOL_IDLE_TTL_MS: u64 = 30 * 60 * 1000; // 30 minutes

/// Evict `LEASE_POOL` entries whose `mode == Unprotected` AND whose
/// `last_seen_at_ms` is older than `LEASE_POOL_IDLE_TTL_MS`. `Protected`
/// entries are NEVER evicted (in-flight `LeaseGuard` holds the `Arc` so
/// eviction would not corrupt active requests, but Protected entries are
/// actively used and have no idle indicator).
///
/// Returns the count of evicted entries. Called per tick by `ledger_sweep`.
pub fn sweep_unprotected() -> u64 {
    let now = now_ms();
    let mut removed: u64 = 0;
    LEASE_POOL.retain(|_uid, state| {
        if state.mode() != Mode::Unprotected {
            return true;
        }
        let last = state.last_seen_at_ms.load(Ordering::Acquire);
        if last == 0 {
            // Never recorded a transition timestamp — keep, conservative.
            return true;
        }
        if now.saturating_sub(last) > LEASE_POOL_IDLE_TTL_MS {
            removed += 1;
            return false;
        }
        true
    });
    if removed > 0 {
        metrics::counter!("gw_lease_pool_sweep_removed_total").increment(removed);
    }
    removed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseError {
    Insufficient { requested: i64, available: i64 },
    Expired,
    NoLease,
}

pub struct LeaseGuard {
    uid: UserId,
    reserved: i64,
    coef_version: u64,
    released: AtomicBool,
    state: Arc<LeaseState>,
}

impl std::fmt::Debug for LeaseGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaseGuard")
            .field("uid", &self.uid)
            .field("reserved", &self.reserved)
            .field("coef_version", &self.coef_version)
            .field("released", &self.released.load(Ordering::Relaxed))
            .finish()
    }
}

impl LeaseGuard {
    pub fn coef_version(&self) -> u64 {
        self.coef_version
    }

    pub fn reserved(&self) -> i64 {
        self.reserved
    }

    pub fn uid(&self) -> &UserId {
        &self.uid
    }

    /// Acquire `requested` credits from the user's lease. Returns
    /// `LeaseError::NoLease` if the user has no protected lease entry,
    /// `LeaseError::Expired` if the lease TTL plus grace has passed, or
    /// `LeaseError::Insufficient` if the available pool < requested.
    pub fn try_acquire(
        uid: &UserId,
        requested: i64,
        now_ms_val: u64,
        grace_ms: u64,
    ) -> Result<Self, LeaseError> {
        let state = match LEASE_POOL.get(uid) {
            Some(s) => Arc::clone(&*s),
            None => return Err(LeaseError::NoLease),
        };
        if state.mode() != Mode::Protected {
            return Err(LeaseError::NoLease);
        }
        let expires_at = state.expires_at_ms.load(Ordering::Acquire);
        if now_ms_val > expires_at.saturating_add(grace_ms) {
            return Err(LeaseError::Expired);
        }
        loop {
            let current = state.credits.load(Ordering::Acquire);
            if current < requested {
                return Err(LeaseError::Insufficient {
                    requested,
                    available: current,
                });
            }
            let new = current - requested;
            if state
                .credits
                .compare_exchange(current, new, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let coef_version = state.coef_table_version.load(Ordering::Acquire);
                return Ok(LeaseGuard {
                    uid: uid.clone(),
                    reserved: requested,
                    coef_version,
                    released: AtomicBool::new(false),
                    state,
                });
            }
        }
    }

    /// Settle the request with actual credits. If actual ≤ reserved, refunds
    /// the difference. If actual > reserved, does NOT double-charge — the
    /// over-spend is recorded via `usage:records.credits_consumed` (Go reconciles).
    pub fn settle_actual(&self, actual: i64) {
        let actual = actual.max(0);
        if actual >= self.reserved {
            return;
        }
        let refund = self.reserved - actual;
        self.state.credits.fetch_add(refund, Ordering::AcqRel);
    }

    /// Idempotent release marker. The actual credits decrement happened in
    /// `try_acquire`; settle handles refund; release just flips the marker
    /// so `Drop` does nothing.
    pub fn release(&self) {
        let _ = self
            .released
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire);
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

pub fn now_ms() -> u64 {
    // Audit 2026-05-30 (Wave 6b): a pre-1970 wall clock (VM/container clock reset
    // before NTP sync, manual adjustment) makes duration_since(UNIX_EPOCH) return
    // Err — the prior `.expect()` panicked the request worker on the credit-lease
    // admission path. Fall back to 0 (matches gw-routing/gw-task convention); a 0
    // timestamp makes any TTL comparison treat the lease as expired, the correct
    // fail-closed direction for a credit layer that cannot time-bound overdraft.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Upsert a user into protected mode with the given lease. Called by the
/// counter stream consumer when Go pushes `events:counter ns=protected_lease`
/// with `mode=protected`.
pub fn upsert_protected(
    uid: &UserId,
    credits: i64,
    lease_id: String,
    coef_table_version: u64,
    expires_at_ms: u64,
) {
    let entry = LEASE_POOL
        .entry(uid.clone())
        .or_insert_with(|| Arc::new(LeaseState::new_unprotected()));
    entry.credits.store(credits, Ordering::Release);
    entry.expires_at_ms.store(expires_at_ms, Ordering::Release);
    entry.lease_id.store(Arc::new(lease_id));
    entry
        .coef_table_version
        .store(coef_table_version, Ordering::Release);
    entry.mode.store(Mode::Protected as u8, Ordering::Release);
}

/// Flip the user back to unprotected mode. Wipes lease credits.
pub fn upsert_unprotected(uid: &UserId) {
    let entry = LEASE_POOL
        .entry(uid.clone())
        .or_insert_with(|| Arc::new(LeaseState::new_unprotected()));
    entry.credits.store(0, Ordering::Release);
    entry.expires_at_ms.store(0, Ordering::Release);
    entry.lease_id.store(Arc::new(String::new()));
    entry.mode.store(Mode::Unprotected as u8, Ordering::Release);
    entry.last_seen_at_ms.store(now_ms(), Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    // sweep_unprotected() touches all keys in the global LEASE_POOL, so
    // tests that assert sweep counts or entry presence must not race each
    // other.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn fresh_uid(tag: &str) -> UserId {
        UserId::from(format!("u_test_{tag}").as_str())
    }

    #[test]
    fn try_acquire_decrements_credits_atomically() {
        let uid = fresh_uid("acquire_ok");
        upsert_protected(&uid, 1000, "lease-1".into(), 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 300, now_ms(), 0).unwrap();
        assert_eq!(guard.reserved(), 300);
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 700);
        drop(guard);
    }

    #[test]
    fn try_acquire_insufficient_returns_error() {
        let uid = fresh_uid("acquire_insufficient");
        upsert_protected(&uid, 100, "lease-2".into(), 9, now_ms() + 60_000);

        let err = LeaseGuard::try_acquire(&uid, 200, now_ms(), 0).unwrap_err();
        assert!(matches!(err, LeaseError::Insufficient { .. }));
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 100);
    }

    #[test]
    fn try_acquire_expired_returns_error() {
        let uid = fresh_uid("acquire_expired");
        let past = now_ms().saturating_sub(10_000);
        upsert_protected(&uid, 1000, "lease-3".into(), 9, past);

        let err = LeaseGuard::try_acquire(&uid, 100, now_ms(), 0).unwrap_err();
        assert!(matches!(err, LeaseError::Expired));
    }

    #[test]
    fn try_acquire_within_grace_window_succeeds() {
        let uid = fresh_uid("within_grace");
        let just_past = now_ms().saturating_sub(2_000);
        upsert_protected(&uid, 1000, "lease-grace".into(), 9, just_past);

        let guard = LeaseGuard::try_acquire(&uid, 100, now_ms(), 5_000).unwrap();

        assert_eq!(guard.reserved(), 100);
    }

    #[test]
    fn try_acquire_beyond_grace_window_expires() {
        let uid = fresh_uid("beyond_grace");
        let long_past = now_ms().saturating_sub(10_000);
        upsert_protected(&uid, 1000, "lease-grace2".into(), 9, long_past);

        let err = LeaseGuard::try_acquire(&uid, 100, now_ms(), 5_000).unwrap_err();

        assert!(matches!(err, LeaseError::Expired));
    }

    #[test]
    fn try_acquire_no_lease_returns_error() {
        let uid = fresh_uid("no_lease");
        // Do NOT upsert; user has no LeasePool entry.
        let err = LeaseGuard::try_acquire(&uid, 100, now_ms(), 0).unwrap_err();
        assert!(matches!(err, LeaseError::NoLease));
    }

    #[test]
    fn try_acquire_unprotected_user_returns_no_lease() {
        let uid = fresh_uid("unprotected_user");
        upsert_protected(&uid, 1000, "lease-x".into(), 9, now_ms() + 60_000);
        upsert_unprotected(&uid);
        let err = LeaseGuard::try_acquire(&uid, 100, now_ms(), 0).unwrap_err();
        assert!(matches!(err, LeaseError::NoLease));
    }

    #[test]
    fn settle_actual_le_reserved_refunds_difference() {
        let uid = fresh_uid("settle_under");
        upsert_protected(&uid, 1000, "lease-4".into(), 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 500, now_ms(), 0).unwrap();
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 500);
        guard.settle_actual(200);
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 800);
    }

    #[test]
    fn settle_actual_gt_reserved_does_not_refund_or_double_charge() {
        let uid = fresh_uid("settle_over");
        upsert_protected(&uid, 1000, "lease-5".into(), 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 300, now_ms(), 0).unwrap();
        guard.settle_actual(500);
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 700);
    }

    #[test]
    fn release_is_idempotent() {
        let uid = fresh_uid("release_idem");
        upsert_protected(&uid, 1000, "lease-6".into(), 9, now_ms() + 60_000);

        let guard = LeaseGuard::try_acquire(&uid, 100, now_ms(), 0).unwrap();
        guard.release();
        guard.release();
        guard.release();
        // release does not refund; only settle_actual does. credits stay at 900.
        assert_eq!(LEASE_POOL.get(&uid).unwrap().credits(), 900);
    }

    #[test]
    fn upsert_overwrites_existing_state_for_same_user() {
        let uid = fresh_uid("upsert_overwrite");
        upsert_protected(&uid, 1000, "lease-7".into(), 9, now_ms() + 60_000);
        upsert_protected(&uid, 500, "lease-8".into(), 9, now_ms() + 120_000);
        let state = LEASE_POOL.get(&uid).unwrap();
        assert_eq!(state.credits(), 500);
        assert_eq!(state.mode(), Mode::Protected);
        assert_eq!(**state.lease_id.load(), "lease-8");
    }

    #[test]
    fn mode_from_str_accepts_known_strings() {
        assert_eq!(Mode::parse_str("protected"), Some(Mode::Protected));
        assert_eq!(Mode::parse_str("unprotected"), Some(Mode::Unprotected));
        assert!(Mode::parse_str("garbage").is_none());
    }

    #[test]
    fn upsert_unprotected_records_last_seen_at_ms() {
        let uid = fresh_uid("last_seen_unprotected");
        // First put into protected to create the entry.
        upsert_protected(&uid, 1000, "lease-ls".into(), 9, now_ms() + 60_000);
        // Then transition to unprotected.
        let before = now_ms();
        upsert_unprotected(&uid);
        let after = now_ms();
        let s = LEASE_POOL.get(&uid).unwrap();
        let ts = s.last_seen_at_ms.load(Ordering::Acquire);
        assert!(
            ts >= before && ts <= after + 1,
            "last_seen_at_ms ({ts}) should be in [{before}, {after}]"
        );
    }

    #[test]
    fn sweep_unprotected_evicts_idle_unprotected_entries() {
        let _lock = SERIAL.lock().unwrap();
        let uid = fresh_uid("sweep_idle");
        // Create the entry as protected, then transition to unprotected.
        upsert_protected(&uid, 1000, "lease-sw-idle".into(), 9, now_ms() + 60_000);
        upsert_unprotected(&uid);
        // Backdate last_seen by (idle_ttl + 1ms).
        let s = LEASE_POOL.get(&uid).unwrap();
        let old_ts = now_ms().saturating_sub(LEASE_POOL_IDLE_TTL_MS + 1);
        s.last_seen_at_ms.store(old_ts, Ordering::Release);
        drop(s);

        let removed = sweep_unprotected();
        assert!(removed >= 1);
        assert!(!LEASE_POOL.contains_key(&uid));
    }

    #[test]
    fn sweep_unprotected_keeps_recent_unprotected_entries() {
        let _lock = SERIAL.lock().unwrap();
        let uid = fresh_uid("sweep_recent");
        upsert_protected(&uid, 1000, "lease-sw-recent".into(), 9, now_ms() + 60_000);
        upsert_unprotected(&uid);
        // last_seen is current (just now).
        let _removed = sweep_unprotected();
        assert!(LEASE_POOL.contains_key(&uid));
        LEASE_POOL.remove(&uid);
    }

    #[test]
    fn sweep_unprotected_keeps_protected_entries() {
        let _lock = SERIAL.lock().unwrap();
        let uid = fresh_uid("sweep_protected");
        upsert_protected(&uid, 1000, "lease-sw-prot".into(), 9, now_ms() + 60_000);
        // Backdate last_seen heavily — but mode is still Protected.
        let s = LEASE_POOL.get(&uid).unwrap();
        let old_ts = now_ms().saturating_sub(LEASE_POOL_IDLE_TTL_MS + 60_000);
        s.last_seen_at_ms.store(old_ts, Ordering::Release);
        drop(s);

        let _removed = sweep_unprotected();
        assert!(
            LEASE_POOL.contains_key(&uid),
            "Protected entries must never be swept regardless of last_seen_at_ms"
        );
        LEASE_POOL.remove(&uid);
    }
}
