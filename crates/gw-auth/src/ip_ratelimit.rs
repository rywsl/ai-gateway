use dashmap::DashMap;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use std::borrow::Cow;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const DEFAULT_MAX_ENTRIES: usize = 100_000;

struct IpLimiterEntry {
    limiter: Arc<DefaultDirectRateLimiter>,
    sequence: u64,
}

pub struct IpRateLimitGuard {
    per_ip: DashMap<IpAddr, IpLimiterEntry>,
    global_per_ip_rpm: NonZeroU32,
    max_entries: usize,
    next_sequence: AtomicU64,
}

impl IpRateLimitGuard {
    pub fn new(rpm: u32, max_entries: usize) -> Self {
        let guard = Self {
            per_ip: DashMap::new(),
            global_per_ip_rpm: NonZeroU32::new(rpm.max(1)).unwrap(),
            max_entries: max_entries.max(1),
            next_sequence: AtomicU64::new(0),
        };
        metrics::gauge!("gw_ip_ratelimit_entries_active").set(0.0);
        guard
    }

    pub fn with_default_cap(rpm: u32) -> Self {
        Self::new(rpm, DEFAULT_MAX_ENTRIES)
    }

    pub fn check(&self, ip: IpAddr) -> Result<(), ErrorIR<'static>> {
        metrics::counter!("gw_ip_ratelimit_check_total").increment(1);
        let lim = self
            .per_ip
            .entry(ip)
            .or_insert_with(|| {
                let q = Quota::per_minute(self.global_per_ip_rpm);
                IpLimiterEntry {
                    limiter: Arc::new(RateLimiter::direct(q)),
                    sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
                }
            })
            .limiter
            .clone();

        metrics::gauge!("gw_ip_ratelimit_entries_active").set(self.per_ip.len() as f64);

        if lim.check().is_err() {
            return Err(ip_rpm_err(ip));
        }
        Ok(())
    }

    pub fn sweep(&self) -> usize {
        metrics::counter!("gw_ip_ratelimit_sweep_total").increment(1);
        let len = self.per_ip.len();
        if len <= self.max_entries {
            metrics::gauge!("gw_ip_ratelimit_entries_active").set(len as f64);
            return 0;
        }

        let remove_count = len - self.max_entries;
        let mut entries: Vec<(IpAddr, u64)> = self
            .per_ip
            .iter()
            .map(|entry| (*entry.key(), entry.sequence))
            .collect();
        entries.sort_unstable_by_key(|(_, sequence)| *sequence);

        let mut removed = 0;
        for (ip, _) in entries.into_iter().take(remove_count) {
            if self.per_ip.remove(&ip).is_some() {
                removed += 1;
            }
        }
        metrics::gauge!("gw_ip_ratelimit_entries_active").set(self.per_ip.len() as f64);
        removed
    }

    pub fn len(&self) -> usize {
        self.per_ip.len()
    }

    pub fn is_empty(&self) -> bool {
        self.per_ip.is_empty()
    }
}

impl Default for IpRateLimitGuard {
    fn default() -> Self {
        Self::new(600, DEFAULT_MAX_ENTRIES)
    }
}

fn ip_rpm_err(ip: IpAddr) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_IP_RPM_EXCEEDED),
        message: Cow::Owned(format!("ip {ip} exceeded rpm limit")),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}
