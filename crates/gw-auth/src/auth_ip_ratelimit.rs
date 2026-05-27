use dashmap::DashMap;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use gw_core::error::{codes, ErrorCategory, ErrorIR};
use std::borrow::Cow;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

struct Entry {
    limiter: Arc<DefaultDirectRateLimiter>,
    exhausted: Arc<AtomicBool>,
    sequence: u64,
}

pub struct AuthFailureIpLimiter {
    per_ip: DashMap<IpAddr, Entry>,
    rpm: NonZeroU32,
    max_entries: usize,
    next_sequence: AtomicU64,
    enabled: bool,
}

impl AuthFailureIpLimiter {
    pub fn new(rpm: u32, max_entries: usize, enabled: bool) -> Self {
        let limiter = Self {
            per_ip: DashMap::new(),
            rpm: NonZeroU32::new(rpm.max(1)).unwrap(),
            max_entries: max_entries.max(1),
            next_sequence: AtomicU64::new(0),
            enabled,
        };
        metrics::gauge!("gw_auth_ip_limit_entries_active").set(0.0);
        limiter
    }

    pub fn is_blocked(&self, ip: IpAddr) -> Result<(), ErrorIR<'static>> {
        if !self.enabled {
            return Ok(());
        }
        let (lim, flag) = {
            let Some(entry) = self.per_ip.get(&ip) else {
                return Ok(());
            };
            (entry.limiter.clone(), entry.exhausted.clone())
        };
        if flag.load(Ordering::Relaxed) {
            if lim.check().is_err() {
                metrics::counter!("gw_auth_ip_limit_total", "reason" => "auth_failure").increment(1);
                return Err(auth_failure_ip_err(ip));
            }
            flag.store(false, Ordering::Relaxed);
        }
        Ok(())
    }

    pub fn record_failure(&self, ip: IpAddr) {
        if !self.enabled {
            return;
        }
        let (lim, flag) = {
            let entry = self
                .per_ip
                .entry(ip)
                .or_insert_with(|| {
                    let q = Quota::per_minute(self.rpm);
                    Entry {
                        limiter: Arc::new(RateLimiter::direct(q)),
                        exhausted: Arc::new(AtomicBool::new(false)),
                        sequence: self.next_sequence.fetch_add(1, Ordering::Relaxed),
                    }
                });
            (entry.limiter.clone(), entry.exhausted.clone())
        };

        if lim.check().is_err() {
            flag.store(true, Ordering::Relaxed);
        }
        metrics::gauge!("gw_auth_ip_limit_entries_active").set(self.per_ip.len() as f64);
    }

    pub fn sweep(&self) -> usize {
        metrics::counter!("gw_auth_ip_limit_sweep_total").increment(1);
        let len = self.per_ip.len();
        if len <= self.max_entries {
            metrics::gauge!("gw_auth_ip_limit_entries_active").set(len as f64);
            return 0;
        }

        let remove_count = len - self.max_entries;
        let mut entries: Vec<(IpAddr, u64)> = self
            .per_ip
            .iter()
            .map(|e| (*e.key(), e.sequence))
            .collect();
        entries.sort_unstable_by_key(|(_, seq)| *seq);

        let mut removed = 0;
        for (ip, _) in entries.into_iter().take(remove_count) {
            if self.per_ip.remove(&ip).is_some() {
                removed += 1;
            }
        }
        metrics::gauge!("gw_auth_ip_limit_entries_active").set(self.per_ip.len() as f64);
        removed
    }

    pub fn len(&self) -> usize {
        self.per_ip.len()
    }

    pub fn is_empty(&self) -> bool {
        self.per_ip.is_empty()
    }
}

fn auth_failure_ip_err(ip: IpAddr) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_AUTH_IP_FAILURE_RATE_EXCEEDED),
        message: Cow::Owned(format!("ip {ip} exceeded auth failure rate limit")),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn below_threshold_allows() {
        let limiter = AuthFailureIpLimiter::new(5, 1000, true);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));

        for _ in 0..4 {
            limiter.record_failure(ip);
            assert!(limiter.is_blocked(ip).is_ok());
        }
    }

    #[test]
    fn exceeds_threshold_returns_429() {
        let limiter = AuthFailureIpLimiter::new(3, 1000, true);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));

        for _ in 0..4 {
            limiter.record_failure(ip);
        }

        let err = limiter.is_blocked(ip).unwrap_err();
        assert_eq!(err.status, 429);
        assert_eq!(err.code.as_ref(), "GW_AUTH_IP_FAILURE_RATE_EXCEEDED");
    }

    #[test]
    fn disabled_always_allows() {
        let limiter = AuthFailureIpLimiter::new(1, 1000, false);
        let ip = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3));

        for _ in 0..100 {
            limiter.record_failure(ip);
        }
        assert!(limiter.is_blocked(ip).is_ok());
    }

    #[test]
    fn different_ips_independent() {
        let limiter = AuthFailureIpLimiter::new(2, 1000, true);
        let ip_a = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10));
        let ip_b = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 11));

        for _ in 0..3 {
            limiter.record_failure(ip_a);
        }
        let err = limiter.is_blocked(ip_a).unwrap_err();
        assert_eq!(err.status, 429);

        assert!(limiter.is_blocked(ip_b).is_ok());
    }

    #[test]
    fn sweep_evicts_oldest() {
        let limiter = AuthFailureIpLimiter::new(5, 2, true);
        let ip1 = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(2, 2, 2, 2));
        let ip3 = IpAddr::V4(Ipv4Addr::new(3, 3, 3, 3));

        limiter.record_failure(ip1);
        limiter.record_failure(ip2);
        limiter.record_failure(ip3);
        assert_eq!(limiter.len(), 3);

        let removed = limiter.sweep();
        assert_eq!(removed, 1);
        assert_eq!(limiter.len(), 2);
    }

    #[test]
    fn unknown_ip_not_blocked() {
        let limiter = AuthFailureIpLimiter::new(1, 1000, true);
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1));
        assert!(limiter.is_blocked(ip).is_ok());
    }
}
