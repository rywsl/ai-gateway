//! Regex-based ApiKey model permissions with ReDoS-resistant guardrails.
//!
//! v1 implements **compile-time** guardrails only (max pattern length + DFA
//! size limit). Runtime per-call budget enforcement (100ms) and per-ApiKey
//! disable-after-N-timeouts tracker land in T5-B as `RedosTracker`.
//!
//! The compile path is cached: `ModelPermission::new` takes a `&RegexCache`
//! and calls `cache.get_or_compile(pattern)`, so repeated calls with the same
//! pattern reuse the same `Arc<Regex>` instead of compiling from scratch.

use gw_runtime::RegexCache;
use regex::Regex;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct ModelPermission {
    regex: Arc<Regex>,
    pattern: Arc<str>,
    pub effect: PermissionEffect,
}

impl ModelPermission {
    /// Create a new `ModelPermission` using the shared `RegexCache`.
    ///
    /// The cache avoids per-request regex compilation: repeated calls with the
    /// same `pattern` string return the same `Arc<Regex>`. Compile errors are
    /// **not** cached (per `RegexCache` contract).
    pub fn new(
        cache: &RegexCache,
        pattern: &str,
        effect: PermissionEffect,
    ) -> Result<Self, RegexError> {
        // Pre-check: reject overlong patterns before hitting the cache so we
        // don't pollute it with near-limit patterns that differ by padding.
        if pattern.len() > MAX_PATTERN_LEN {
            return Err(RegexError::TooLarge(MAX_PATTERN_LEN));
        }
        let regex = cache
            .get_or_compile(pattern)
            .map_err(|e| RegexError::Compile(e.to_string()))?;
        Ok(Self {
            regex,
            pattern: Arc::from(pattern),
            effect,
        })
    }

    /// Access the compiled regex.
    pub fn regex(&self) -> &Regex {
        &self.regex
    }

    /// Access the raw pattern string.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegexError {
    #[error("pattern too large (max {0} bytes)")]
    TooLarge(usize),
    #[error("regex compile: {0}")]
    Compile(String),
}

pub const MAX_PATTERN_LEN: usize = 1024;
pub const NFA_SIZE_LIMIT: usize = 256 * 1024;
pub const DFA_SIZE_LIMIT: usize = 8 * 1024 * 1024;

/// Compile a regex pattern with size guardrails.
///
/// **Note:** Production code should prefer `ModelPermission::new` which uses
/// the shared `RegexCache`. This function remains public for the benchmark and
/// for callers that intentionally bypass the cache (e.g. one-off validation).
pub fn compile_pattern(pattern: &str) -> Result<Regex, RegexError> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(RegexError::TooLarge(MAX_PATTERN_LEN));
    }
    regex::RegexBuilder::new(pattern)
        .size_limit(NFA_SIZE_LIMIT)
        .dfa_size_limit(DFA_SIZE_LIMIT)
        .build()
        .map_err(|e| RegexError::Compile(e.to_string()))
}

/// Walk the permission list in order. Each match flips `allow`. Final value
/// of `allow` decides. Default is deny (no perms match → false).
pub fn check_model_against_permissions(model: &str, perms: &[ModelPermission]) -> bool {
    let mut allow = false;
    for p in perms {
        if p.regex.is_match(model) {
            allow = matches!(p.effect, PermissionEffect::Allow);
        }
    }
    allow
}

use moka::sync::Cache;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Per-call regex match budget. Matches over `BUDGET_MS` are recorded as
/// timeouts via [`RedosTracker::record_timeout`]; once an ApiKey accumulates
/// `TIMEOUT_THRESHOLD` timeouts, [`RedosTracker::is_disabled`] returns true
/// and [`check_with_budget`] short-circuits to `None`.
pub const BUDGET_MS: u128 = 100;

/// Number of regex-match timeouts per ApiKey before that key is auto-disabled.
const TIMEOUT_THRESHOLD: u32 = 1;

/// Default cache capacity (api-key cardinality). 100_000 covers typical
/// production loads; an LRU-evicted attacker key only loses its "disabled"
/// state after sustained idle exceeding TTI, which is acceptable for a
/// defense-in-depth tracker.
const DEFAULT_REDOS_CAPACITY: usize = 100_000;

/// Idle TTI for tracker entries. A disabled key that has not been seen for
/// this long is reset on re-entry (re-tested against the threshold). 1h is
/// long enough that legitimate keys are not punished; short enough that an
/// attacker pattern is not permanent.
const DEFAULT_REDOS_TTI: Duration = Duration::from_secs(3600);

/// Per-ApiKey timeout counter + disable flag. Backed by a bounded `moka::sync::Cache`
/// so the entry set is bounded by capacity (LRU) and idle TTI.
pub struct RedosTracker {
    inner: Cache<String, Arc<TrackerEntry>>,
}

#[derive(Default)]
struct TrackerEntry {
    timeout_count: AtomicU32,
    disabled: AtomicBool,
}

impl Default for RedosTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RedosTracker {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_REDOS_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(capacity as u64)
                .time_to_idle(DEFAULT_REDOS_TTI)
                .build(),
        }
    }

    pub fn timeout_count(&self, ak: &str) -> u32 {
        self.inner
            .get(&ak.to_string())
            .map(|e| e.timeout_count.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    pub fn is_disabled(&self, ak: &str) -> bool {
        self.inner
            .get(&ak.to_string())
            .map(|e| e.disabled.load(Ordering::Acquire))
            .unwrap_or(false)
    }

    pub fn record_timeout(&self, ak: &str) {
        let entry = self
            .inner
            .get_with(ak.to_string(), || Arc::new(TrackerEntry::default()));
        let new_count = entry.timeout_count.fetch_add(1, Ordering::AcqRel) + 1;
        if new_count >= TIMEOUT_THRESHOLD {
            entry.disabled.store(true, Ordering::Release);
        }
        metrics::counter!("gw_redos_timeout_total").increment(1);
    }

    #[cfg(test)]
    pub fn sync_for_test(&self) {
        self.inner.run_pending_tasks();
    }

    #[cfg(test)]
    pub fn len_for_test(&self) -> usize {
        self.inner.run_pending_tasks();
        self.inner.entry_count() as usize
    }
}

/// Run a permission check with a per-ApiKey ReDoS budget guard.
///
/// - Returns `None` when the ApiKey has been auto-disabled (caller should reject the request).
/// - Returns `Some(matched)` otherwise. If the elapsed time exceeded `BUDGET_MS`,
///   the timeout is recorded but this call still returns the (presumably correct)
///   regex result — the next call from the same ApiKey will be the one short-circuited
///   if the threshold is crossed.
///
/// `regex` crate has no native incremental-step API; the post-check timestamp
/// is therefore observational, not preemptive. This is acceptable because:
///   1. `RegexBuilder::dfa_size_limit` already bounds DFA construction;
///   2. RE2-style matching is linear-time on input (no catastrophic backtracking
///      possible regardless of pattern shape);
///   3. The budget guard is a defense-in-depth — even with a pathological
///      input, threshold-disable bounds total damage to ~ `TIMEOUT_THRESHOLD`
///      slow calls per ApiKey.
pub fn check_with_budget(
    model: &str,
    perms: &[ModelPermission],
    api_key_id: &str,
    tracker: &RedosTracker,
) -> Option<bool> {
    if tracker.is_disabled(api_key_id) {
        return None;
    }
    let start = Instant::now();
    let result = check_model_against_permissions(model, perms);
    let elapsed = start.elapsed();
    if elapsed.as_millis() > BUDGET_MS {
        tracker.record_timeout(api_key_id);
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redos_tracker_evicts_via_lru_at_capacity() {
        let tracker = RedosTracker::with_capacity(4);
        // Push past capacity — earliest entries should LRU-evict.
        for i in 0..10 {
            let key = format!("ak-{i}");
            tracker.record_timeout(&key);
        }
        // Confirm at most `capacity` entries are retained.
        // Moka may sync lazily; force pending tasks to settle before assertion.
        tracker.sync_for_test();
        assert!(tracker.len_for_test() <= 4);
    }

    #[test]
    fn redos_tracker_disables_after_first_timeout() {
        let tracker = RedosTracker::new();

        tracker.record_timeout("ak-bad");

        assert!(tracker.is_disabled("ak-bad"));
        assert_eq!(check_with_budget("anything", &[], "ak-bad", &tracker), None);
    }
}
