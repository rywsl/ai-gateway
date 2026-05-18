//! Per-user request-rate and concurrent-stream limiting.
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

pub struct RateLimitGuard {
    pub per_user: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    pub per_user_streams: DashMap<UserId, Arc<AtomicUsize>>,
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
            per_user_streams: DashMap::new(),
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
        Ok(RateLimitTicket { _placeholder: () })
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
}

pub struct RateLimitTicket {
    _placeholder: (),
}

pub struct StreamSlot {
    pub uid: UserId,
    pub counter: Arc<AtomicUsize>,
    pub total: Arc<AtomicUsize>,
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
