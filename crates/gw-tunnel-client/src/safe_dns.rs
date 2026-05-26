//! Safe DNS — IP blocklist for SSRF protection.
//!
//! T3-D lands the IP classifier and the BlockedIpError type. T3-H builds on
//! top of this by adding a `SafeDnsResolver` that consults DoH then runs each
//! result through `is_ip_blocked` before returning. The classifier alone is
//! valuable even without DoH (direct paths can pre-filter resolved IPs).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use arc_swap::ArcSwap;
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::{Resolver, TokioResolver};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
#[error("ip {0} is blocked")]
pub struct BlockedIpError(pub IpAddr);

pub fn is_ip_blocked(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_v4_blocked(v4),
        IpAddr::V6(v6) => is_v6_blocked(v6),
    }
}

fn is_v4_blocked(ip: Ipv4Addr) -> bool {
    if ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
    {
        return true;
    }
    let o = ip.octets();
    if o[0] == 100 && (o[1] & 0xC0) == 0x40 {
        return true;
    }
    false
}

fn is_v6_blocked(ip: Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    let s = ip.segments()[0];
    if (s & 0xFE00) == 0xFC00 {
        return true;
    }
    if (s & 0xFFC0) == 0xFE80 {
        return true;
    }
    false
}

static OVERRIDE: once_cell::sync::Lazy<ArcSwap<HashMap<String, Vec<IpAddr>>>> =
    once_cell::sync::Lazy::new(|| ArcSwap::from_pointee(HashMap::new()));

/// Test-only mutex: ensures `OverrideGuard` instances do not race against each
/// other when several tests in the same binary touch the override map in
/// parallel. Production callers of `set_override` (none yet — the override map
/// is intended for tests + targeted host-pinning) should not need to lock it.
static OVERRIDE_TEST_LOCK: once_cell::sync::Lazy<parking_lot::Mutex<()>> =
    once_cell::sync::Lazy::new(|| parking_lot::Mutex::new(()));

/// Override the resolution for a host. Subsequent `SafeDnsResolver::resolve`
/// calls return the supplied IPs (after running them through `is_ip_blocked`).
pub fn set_override(host: &str, ips: Vec<IpAddr>) {
    let mut m = (**OVERRIDE.load()).clone();
    m.insert(host.to_string(), ips);
    OVERRIDE.store(Arc::new(m));
}

/// RAII guard for tests: snapshots the override map on construction, restores
/// it on drop. Also serializes concurrent test sections via a process-global
/// mutex so multiple tests can mutate the override map without racing.
/// Use with `let _g = OverrideGuard::new();`.
pub struct OverrideGuard {
    snapshot: Arc<HashMap<String, Vec<IpAddr>>>,
    _lock: parking_lot::MutexGuard<'static, ()>,
}

impl OverrideGuard {
    pub fn new() -> Self {
        let lock = OVERRIDE_TEST_LOCK.lock();
        Self {
            snapshot: OVERRIDE.load_full(),
            _lock: lock,
        }
    }
}

impl Default for OverrideGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for OverrideGuard {
    fn drop(&mut self) {
        OVERRIDE.store(self.snapshot.clone());
    }
}

/// DoH-backed resolver with LRU cache + IP blocklist post-filter.
pub struct SafeDnsResolver {
    inner: TokioResolver,
    cache: Mutex<HashMap<String, (Instant, Vec<IpAddr>)>>,
    cache_ttl: Duration,
    cache_cap: usize,
}

impl SafeDnsResolver {
    /// Cloudflare DoH over HTTPS (1.1.1.1 / 1.0.0.1). Best default for v1.
    pub fn cloudflare_default() -> Self {
        let opts = ResolverOpts::default();
        let inner = Resolver::builder_with_config(
            ResolverConfig::cloudflare_https(),
            TokioConnectionProvider::default(),
        )
        .with_options(opts)
        .build();
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
            cache_ttl: Duration::from_secs(600),
            cache_cap: 4096,
        }
    }

    /// Resolve `host` to a list of public IP addresses.
    ///
    /// Resolution order:
    /// 1. Override map (set via `set_override`) takes priority.
    /// 2. Local LRU cache hit (within `cache_ttl`).
    /// 3. DoH lookup; result is cached and post-filtered through `is_ip_blocked`.
    ///
    /// Returns `Err(BlockedIpError)` if any resolved IP is in the blocklist —
    /// callers must treat this as a hard failure (don't fall through to a
    /// different IP; this is anti-SSRF).
    pub async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, BlockedIpError> {
        if let Some(ips) = OVERRIDE.load_full().get(host) {
            for ip in ips {
                if is_ip_blocked(*ip) {
                    return Err(BlockedIpError(*ip));
                }
            }
            return Ok(ips.clone());
        }

        // Cache check
        {
            let cache = self.cache.lock();
            if let Some((t, v)) = cache.get(host) {
                if t.elapsed() < self.cache_ttl {
                    return Ok(v.clone());
                }
            }
        }

        let lookup = self
            .inner
            .lookup_ip(host)
            .await
            .map_err(|_| BlockedIpError("0.0.0.0".parse().unwrap()))?;
        let mut out: Vec<IpAddr> = lookup.iter().collect();
        for ip in &out {
            if is_ip_blocked(*ip) {
                return Err(BlockedIpError(*ip));
            }
        }
        out.dedup();

        let mut cache = self.cache.lock();
        if cache.len() >= self.cache_cap {
            cache.clear();
        }
        cache.insert(host.to_string(), (Instant::now(), out.clone()));
        Ok(out)
    }
}
