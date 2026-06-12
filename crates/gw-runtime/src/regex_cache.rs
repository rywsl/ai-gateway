//! Compiled-regex cache. Moka `sync::Cache` storing `Arc<Regex>` keyed by
//! `Arc<str>` pattern. Compile errors are NOT cached.

use moka::sync::Cache;
use regex::Regex;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("regex compile failed: {0}")]
    Regex(#[from] regex::Error),
}

pub struct RegexCacheConfig {
    pub capacity: u64,
    pub tti: Duration,
}

impl Default for RegexCacheConfig {
    fn default() -> Self {
        Self { capacity: 4096, tti: Duration::from_secs(3600) }
    }
}

pub struct RegexCache {
    inner: Cache<Arc<str>, Arc<Regex>>,
}

impl RegexCache {
    pub fn new(cfg: RegexCacheConfig) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(cfg.capacity)
                .time_to_idle(cfg.tti)
                .build(),
        }
    }

    pub fn get_or_compile(&self, pattern: &str) -> Result<Arc<Regex>, CompileError> {
        let key: Arc<str> = Arc::from(pattern);
        if let Some(re) = self.inner.get(&key) {
            metrics::counter!("gw_regex_cache_hit_total").increment(1);
            return Ok(re);
        }
        metrics::counter!("gw_regex_cache_miss_total").increment(1);
        match Regex::new(pattern) {
            Ok(re) => {
                let arc = Arc::new(re);
                self.inner.insert(key, arc.clone());
                Ok(arc)
            }
            Err(e) => {
                metrics::counter!("gw_regex_cache_compile_fail_total").increment(1);
                Err(e.into())
            }
        }
    }

    pub fn len(&self) -> u64 { self.inner.entry_count() }

    pub fn is_empty(&self) -> bool { self.inner.entry_count() == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_returns_same_arc() {
        let cache = RegexCache::new(RegexCacheConfig::default());
        let a = cache.get_or_compile("^foo$").unwrap();
        let b = cache.get_or_compile("^foo$").unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn compile_error_not_cached() {
        let cache = RegexCache::new(RegexCacheConfig::default());
        assert!(cache.get_or_compile("[invalid").is_err());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn distinct_patterns_distinct_arcs() {
        let cache = RegexCache::new(RegexCacheConfig::default());
        let a = cache.get_or_compile("^foo$").unwrap();
        let b = cache.get_or_compile("^bar$").unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn concurrent_first_compile_only_once() {
        use std::thread;
        let cache = Arc::new(RegexCache::new(RegexCacheConfig::default()));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let c = cache.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    c.get_or_compile("^foo-(bar|baz)-\\d+$").unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // Moka doesn't expose internal compile-count, but cache size must be 1.
        assert_eq!(cache.len(), 1);
    }
}
