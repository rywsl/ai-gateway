//! Rate-limit enforcement (RPM + concurrent streams).
//!
//! TPM (tokens-per-minute) is NOT enforced here — it requires the token count
//! from the upstream response and is therefore enforced by the gateway's
//! response-side filter (gw-proxy's logging filter, post-stream-completion).
//!
//! - **Request rate** (`check`): one `governor` direct rate limiter per
//!   `effective_user_id`, configured at the user's RPM. A failed `check()`
//!   maps to `GW_QUOTA_RPM_EXCEEDED`.
//! - **Concurrent streams** (`check_stream_slot`): a per-user `AtomicUsize`
//!   counter incremented on acquire and decremented when the returned
//!   [`StreamSlot`] is dropped (RAII). A `total_streams` counter tracks
//!   global concurrency for observability.

use dashmap::DashMap;
use moka::sync::Cache;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use std::borrow::Cow;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub mod adaptive;

const DEFAULT_RATE_LIMIT_TTI: std::time::Duration = std::time::Duration::from_secs(3600);
const DEFAULT_RATE_LIMIT_CAPACITY_USERS: u64 = 100_000;
const DEFAULT_RATE_LIMIT_CAPACITY_KEYS: u64 = 10_000;

pub struct RateLimitGuard {
    pub per_user: Cache<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_tpm: Cache<UserId, Arc<DefaultDirectRateLimiter>>,
    /// Live per-user concurrent-stream counter. NOT a Moka cache: TTI eviction
    /// while StreamSlots are alive would hand the next caller a fresh counter
    /// starting at 0, bypassing the concurrency cap. Bounded via RAII —
    /// `StreamSlot::Drop` removes the entry when the count returns to 0.
    /// Wrapped in `Arc` so `StreamSlot` can hold a back-reference for cleanup.
    pub per_user_streams: Arc<DashMap<UserId, Arc<AtomicUsize>>>,
    /// Per-(vendor key, enforced rpm) GCRA limiter. Tuple-keyed so that a limit
    /// change creates a fresh `(key, new_limit)` entry atomically via
    /// `get_with`; the old `(key, old_limit)` entry ages out via TTI. This
    /// removes the rebuild-race window of the prior `Cache<KeyId, (u32, _)>`
    /// shape, where two concurrent callers could each build a new limiter on
    /// limit change.
    pub per_key: Cache<(KeyId, u32), Arc<DefaultDirectRateLimiter>>,
    /// Admin-set per-vendor-key RPM override. NOT a Moka cache: TTI eviction
    /// would silently un-override keys after 1h idle. Bounded by the
    /// configured key count (small) and persists until explicit
    /// `reset_key_rpm`.
    pub per_key_overrides: DashMap<KeyId, u32>,
    pub total_streams: Arc<AtomicUsize>,
}

impl Default for RateLimitGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitGuard {
    pub fn new() -> Self {
        Self::with_tti(DEFAULT_RATE_LIMIT_TTI)
    }

    pub fn with_tti(tti: std::time::Duration) -> Self {
        Self {
            per_user: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_USERS)
                .time_to_idle(tti)
                .build(),
            per_user_tpm: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_USERS)
                .time_to_idle(tti)
                .build(),
            per_user_streams: Arc::new(DashMap::new()),
            per_key: Cache::builder()
                .max_capacity(DEFAULT_RATE_LIMIT_CAPACITY_KEYS)
                .time_to_idle(tti)
                .build(),
            per_key_overrides: DashMap::new(),
            total_streams: Arc::new(AtomicUsize::new(0)),
        }
    }

    #[cfg(test)]
    pub fn with_tti_for_test(tti: std::time::Duration) -> Self {
        Self::with_tti(tti)
    }

    #[cfg(test)]
    pub fn sync_for_test(&self) {
        self.per_user.run_pending_tasks();
        self.per_user_tpm.run_pending_tasks();
        self.per_key.run_pending_tasks();
    }

    pub fn check(&self, perm: &MergedPermissions) -> Result<RateLimitTicket, ErrorIR<'static>> {
        if perm.rpm_limit > 0 {
            let lim = self
                .per_user
                .get_with(perm.effective_user_id.clone(), || {
                    let q = Quota::per_minute(NonZeroU32::new(perm.rpm_limit).unwrap());
                    Arc::new(RateLimiter::direct(q))
                });
            if lim.check().is_err() {
                return Err(rpm_err());
            }
        }
        Ok(RateLimitTicket)
    }

    pub fn check_key(
        &self,
        key_id: &KeyId,
        configured_rpm: u32,
    ) -> Result<RateLimitTicket, ErrorIR<'static>> {
        let limit = self
            .per_key_overrides
            .get(key_id)
            .map(|v| *v)
            .unwrap_or(configured_rpm);
        if limit == 0 {
            return Ok(RateLimitTicket);
        }

        // Tuple-keyed atomic fetch-or-build. When the limit changes, a new
        // (key_id, new_limit) entry is created exactly once via get_with;
        // the prior (key_id, old_limit) entry ages out via TTI.
        let limiter = self.per_key.get_with((key_id.clone(), limit), || {
            let q = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
            Arc::new(RateLimiter::direct(q))
        });

        if limiter.check().is_err() {
            return Err(key_rpm_err());
        }
        Ok(RateLimitTicket)
    }

    pub fn override_key_rpm(&self, key_id: &KeyId, rpm: u32) {
        self.per_key_overrides.insert(key_id.clone(), rpm);
        // No need to invalidate per_key — the next check_key will get_with on
        // the new (key_id, rpm) tuple, building a fresh limiter atomically.
        // The stale (key_id, old_rpm) entry ages out via TTI.
    }

    pub fn reset_key_rpm(&self, key_id: &KeyId) {
        self.per_key_overrides.remove(key_id);
        // Stale per_key tuple entries age out via TTI.
    }

    pub fn check_stream_slot(
        &self,
        uid: &UserId,
        max: u32,
    ) -> Result<StreamSlot, ErrorIR<'static>> {
        let counter = self
            .per_user_streams
            .entry(uid.clone())
            .or_insert_with(|| Arc::new(AtomicUsize::new(0)))
            .clone();
        let n = counter.fetch_add(1, Ordering::AcqRel);
        if max > 0 && n + 1 > max as usize {
            let prev = counter.fetch_sub(1, Ordering::AcqRel);
            // If count is back to 0, try to remove the entry to keep the map
            // bounded by live concurrency only.
            if prev == 1 {
                self.per_user_streams
                    .remove_if(uid, |_, c| c.load(Ordering::Acquire) == 0);
            }
            return Err(concurrent_err());
        }
        self.total_streams.fetch_add(1, Ordering::Relaxed);
        Ok(StreamSlot {
            uid: uid.clone(),
            counter,
            total: Arc::clone(&self.total_streams),
            per_user_streams: Arc::clone(&self.per_user_streams),
        })
    }

    /// Deduct observed tokens from the user's TPM sliding-window counter.
    ///
    /// Called **after** the response has been fully sent to the client (in the
    /// `logging` filter). A failure here does NOT block the current request —
    /// it only means the *next* request from this user will be rejected with
    /// `GW_QUOTA_TPM_EXCEEDED` until the window slides.
    pub fn deduct_tpm(
        &self,
        uid: &UserId,
        perm: &MergedPermissions,
        tokens: u32,
    ) -> Result<(), ErrorIR<'static>> {
        if perm.tpm_limit == 0 {
            return Ok(());
        }
        let Some(tokens_nz) = NonZeroU32::new(tokens) else {
            return Ok(());
        };
        let lim = self
            .per_user_tpm
            .get_with(uid.clone(), || {
                let q = Quota::per_minute(NonZeroU32::new(perm.tpm_limit).unwrap());
                Arc::new(RateLimiter::direct(q))
            });
        if lim.check_n(tokens_nz).is_err() {
            return Err(tpm_err());
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct RateLimitTicket;

pub struct StreamSlot {
    pub uid: UserId,
    counter: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    /// Back-reference to the parent `RateLimitGuard::per_user_streams` so the
    /// drop can RAII-remove the entry when concurrency returns to 0.
    per_user_streams: Arc<DashMap<UserId, Arc<AtomicUsize>>>,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let prev = self.counter.fetch_sub(1, Ordering::AcqRel);
        self.total.fetch_sub(1, Ordering::Relaxed);
        // If the count just returned to 0, try to remove the entry to keep
        // the map bounded by live concurrency only. `remove_if` re-checks the
        // count under the shard lock so a concurrent `check_stream_slot` that
        // raced past us doesn't lose its entry.
        if prev == 1 {
            self.per_user_streams
                .remove_if(&self.uid, |_, c| c.load(Ordering::Acquire) == 0);
        }
    }
}

fn rpm_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_RPM_EXCEEDED),
        message: Cow::Borrowed("rpm exceeded"),
        retryable: true,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}

fn key_rpm_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_RPM_KEY_EXCEEDED),
        message: Cow::Borrowed("vendor key rpm exceeded"),
        retryable: true,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}

fn concurrent_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_CONCURRENT_EXCEEDED),
        message: Cow::Borrowed("concurrent streams exceeded"),
        retryable: true,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(5),
    }
}

fn tpm_err() -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_TPM_EXCEEDED),
        message: Cow::Borrowed("tpm exceeded"),
        retryable: true,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::permissions::MarkupChainSnapshot;
    use std::collections::HashSet;

    /// Build a `MergedPermissions` with the given `uid` and `rpm_limit`.
    fn test_merged_with_rpm(uid: UserId, rpm: u32) -> MergedPermissions {
        MergedPermissions {
            user_id: uid.clone(),
            effective_user_id: uid,
            client_ip: "127.0.0.1".parse().unwrap(),
            allowed_models: HashSet::new(),
            allowed_channels: HashSet::new(),
            allowed_groups: HashSet::new(),
            forced_model_group: None,
            rpm_limit: rpm,
            tpm_limit: 0,
            max_concurrent_streams: 0,
            markup_chain: MarkupChainSnapshot::identity(),
            model_whitelist_regex: None,
        }
    }

    #[test]
    fn rpm_user_limiter_resets_after_tti_eviction() {
        // Build a guard with a TINY TTI so the test runs fast without sleeps.
        // Production TTI is 1h; test uses 50ms via the `with_tti_for_test` constructor.
        let guard = RateLimitGuard::with_tti_for_test(std::time::Duration::from_millis(50));
        let uid = UserId::from("u-rpm-tti");

        // Exhaust RPM.
        let mut allowed = 0u32;
        for _ in 0..200 {
            let merged = test_merged_with_rpm(uid.clone(), 5);
            if guard.check(&merged).is_ok() {
                allowed += 1;
            }
        }
        assert!(allowed > 0 && allowed <= 60, "should exhaust at small RPM, got {allowed}");

        // Wait past TTI + small buffer.
        std::thread::sleep(std::time::Duration::from_millis(80));
        guard.sync_for_test();

        // The post-TTI request must see a FRESH limiter (deliberate semantic).
        let merged = test_merged_with_rpm(uid.clone(), 5);
        assert!(
            guard.check(&merged).is_ok(),
            "after TTI eviction, a returning user must get a fresh window"
        );
    }
}
