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
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use gw_core::ids::*;
use gw_core::permissions::MergedPermissions;
use std::borrow::Cow;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub mod adaptive;

pub struct RateLimitGuard {
    pub per_user: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_tpm: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_streams: DashMap<UserId, Arc<AtomicUsize>>,
    pub per_key: DashMap<KeyId, (u32, Arc<DefaultDirectRateLimiter>)>,
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
        Self {
            per_user: DashMap::new(),
            per_user_tpm: DashMap::new(),
            per_user_streams: DashMap::new(),
            per_key: DashMap::new(),
            per_key_overrides: DashMap::new(),
            total_streams: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn check(&self, perm: &MergedPermissions) -> Result<RateLimitTicket, ErrorIR<'static>> {
        if perm.rpm_limit > 0 {
            let lim = self
                .per_user
                .entry(perm.effective_user_id.clone())
                .or_insert_with(|| {
                    let q = Quota::per_minute(NonZeroU32::new(perm.rpm_limit).unwrap());
                    Arc::new(RateLimiter::direct(q))
                })
                .clone();
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

        let limiter = {
            let mut entry = self.per_key.entry(key_id.clone()).or_insert_with(|| {
                let q = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
                (limit, Arc::new(RateLimiter::direct(q)))
            });
            if entry.0 != limit {
                let q = Quota::per_minute(NonZeroU32::new(limit).unwrap_or(NonZeroU32::MIN));
                *entry = (limit, Arc::new(RateLimiter::direct(q)));
            }
            entry.1.clone()
        };

        if limiter.check().is_err() {
            return Err(key_rpm_err());
        }
        Ok(RateLimitTicket)
    }

    pub fn override_key_rpm(&self, key_id: &KeyId, rpm: u32) {
        self.per_key_overrides.insert(key_id.clone(), rpm);
        self.per_key.remove(key_id);
    }

    pub fn reset_key_rpm(&self, key_id: &KeyId) {
        self.per_key_overrides.remove(key_id);
        self.per_key.remove(key_id);
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
            counter.fetch_sub(1, Ordering::AcqRel);
            return Err(concurrent_err());
        }
        self.total_streams.fetch_add(1, Ordering::Relaxed);
        Ok(StreamSlot {
            uid: uid.clone(),
            counter,
            total: Arc::clone(&self.total_streams),
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
            .entry(uid.clone())
            .or_insert_with(|| {
                let q = Quota::per_minute(NonZeroU32::new(perm.tpm_limit).unwrap());
                Arc::new(RateLimiter::direct(q))
            })
            .clone();
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
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        self.total.fetch_sub(1, Ordering::Relaxed);
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
