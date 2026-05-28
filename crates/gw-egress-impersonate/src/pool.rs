//! wreq Client pool keyed by (profile, normalized proxy URL).
//! LRU eviction at `max_sessions`. On eviction the Arc is dropped;
//! wreq::Client has no explicit shutdown method — in-flight connections
//! are cleaned up when the last Arc reference is dropped.

use crate::profiles::emulation_for;
use gw_core::impersonate::ImpersonateProfile;
use parking_lot::Mutex;
use wreq::Client;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Instant;
use url::Url;

/// Cache key: profile + optional normalized proxy URL.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct PoolKey {
    pub profile: ImpersonateProfile,
    pub proxy: Option<Url>,
}

impl PoolKey {
    /// Normalize to ensure (profile, proxy) is canonical regardless of trailing
    /// slashes or uppercase host letters in the proxy URL.
    fn normalize(mut self) -> Self {
        if let Some(ref mut u) = self.proxy {
            if let Some(host) = u.host_str().map(|s| s.to_ascii_lowercase()) {
                let _ = u.set_host(Some(&host));
            }
            // Strip a bare trailing slash so "http://host:port/" == "http://host:port".
            if u.path() == "/" {
                u.set_path("");
            }
        }
        self
    }
}

struct Entry {
    client: Arc<Client>,
    last_used: Instant,
}

/// LRU-evicting pool of `wreq::Client` instances.
///
/// Tuned for small capacities (≤ ~32 entries). Eviction does an O(n) scan over
/// the map to find the LRU; that is acceptable at this scale but degrades for
/// large caches — swap the backing store before raising capacity past ~64.
///
/// Concurrent misses on the same key may race and build two clients; the
/// second writer overwrites the first, and the first Arc is dropped without
/// firing the `evicted` counter. wreq Clients are channel factories, so the
/// race is harmless but worth noting if pool metrics ever look short.
pub struct ClientPool {
    inner: Mutex<HashMap<PoolKey, Entry>>,
    capacity: NonZeroUsize,
}

impl ClientPool {
    /// Create a new pool with the given maximum number of live clients.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(HashMap::with_capacity(capacity.max(1))),
            capacity: NonZeroUsize::new(capacity.max(1)).unwrap(),
        }
    }

    /// Number of clients currently held in the pool.
    pub fn size(&self) -> usize {
        self.inner.lock().len()
    }

    /// Return a cached client for `key`, or create and cache a new one.
    /// When the pool is at capacity the least-recently-used entry is evicted.
    pub async fn get_or_create(&self, key: PoolKey) -> anyhow::Result<Arc<Client>> {
        let key = key.normalize();

        // Fast path: hit in pool.
        {
            let mut guard = self.inner.lock();
            if let Some(entry) = guard.get_mut(&key) {
                entry.last_used = Instant::now();
                metrics::counter!("gw_egress_client_pool_total", "event" => "hit").increment(1);
                return Ok(Arc::clone(&entry.client));
            }
        }

        // Miss: build a new client outside the lock.
        let client = build_client(&key).await?;
        let arc = Arc::new(client);

        // Insert and optionally evict the LRU entry.
        let evicted = {
            let mut guard = self.inner.lock();
            guard.insert(
                key.clone(),
                Entry { client: Arc::clone(&arc), last_used: Instant::now() },
            );
            if guard.len() > self.capacity.get() {
                let lru_key = guard
                    .iter()
                    .min_by_key(|(_, e)| e.last_used)
                    .map(|(k, _)| k.clone());
                lru_key.and_then(|k| guard.remove(&k))
            } else {
                None
            }
        };

        if let Some(_evicted_entry) = evicted {
            // wreq::Client has no explicit shutdown() method; the Arc drop here
            // will close idle pooled connections. In-flight requests complete
            // normally because the caller holds its own Arc reference.
            // TODO(wreq): revisit if a future wreq version adds Client::shutdown().
            metrics::counter!("gw_egress_client_pool_total", "event" => "evicted").increment(1);
        }
        metrics::counter!("gw_egress_client_pool_total", "event" => "miss").increment(1);
        Ok(arc)
    }
}

async fn build_client(key: &PoolKey) -> anyhow::Result<Client> {
    let mut builder = wreq::Client::builder().emulation(emulation_for(key.profile));
    if let Some(ref proxy) = key.proxy {
        builder = builder.proxy(wreq::Proxy::all(proxy.as_str())?);
    }
    Ok(builder.build()?)
}
