//! Regex-based ApiKey model permissions with ReDoS-resistant guardrails.
//!
//! v1 implements **compile-time** guardrails only (max pattern length + DFA
//! size limit). Runtime per-call budget enforcement (100ms) and per-ApiKey
//! disable-after-N-timeouts tracker land in T5-B as `RedosTracker`.

use regex::{Regex, RegexBuilder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionEffect {
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct ModelPermission {
    pub regex: Regex,
    pub raw_pattern: String,
    pub effect: PermissionEffect,
}

impl ModelPermission {
    pub fn new(pattern: &str, effect: PermissionEffect) -> Result<Self, RegexError> {
        let regex = compile_pattern(pattern)?;
        Ok(Self {
            regex,
            raw_pattern: pattern.to_string(),
            effect,
        })
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

pub fn compile_pattern(pattern: &str) -> Result<Regex, RegexError> {
    if pattern.len() > MAX_PATTERN_LEN {
        return Err(RegexError::TooLarge(MAX_PATTERN_LEN));
    }
    RegexBuilder::new(pattern)
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

use parking_lot::Mutex;
use std::collections::HashMap;
use std::time::Instant;

/// Per-call regex match budget. Matches over `BUDGET_MS` are recorded as
/// timeouts via [`RedosTracker::record_timeout`]; once an ApiKey accumulates
/// `TIMEOUT_THRESHOLD` timeouts, [`RedosTracker::is_disabled`] returns true
/// and [`check_with_budget`] short-circuits to `None`.
pub const BUDGET_MS: u128 = 100;

/// Number of regex-match timeouts per ApiKey before that key is auto-disabled.
const TIMEOUT_THRESHOLD: u32 = 10;

/// Per-ApiKey timeout counter + disable flag. Cheap interior mutability
/// (parking_lot::Mutex over a small HashMap) so production code can hold a
/// shared `&'static RedosTracker` (or an Arc) on the hot path.
#[derive(Default)]
pub struct RedosTracker {
    inner: Mutex<HashMap<String, TrackerEntry>>,
}

#[derive(Default)]
struct TrackerEntry {
    timeout_count: u32,
    disabled: bool,
}

impl RedosTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn timeout_count(&self, ak: &str) -> u32 {
        self.inner
            .lock()
            .get(ak)
            .map(|e| e.timeout_count)
            .unwrap_or(0)
    }

    pub fn is_disabled(&self, ak: &str) -> bool {
        self.inner
            .lock()
            .get(ak)
            .map(|e| e.disabled)
            .unwrap_or(false)
    }

    pub fn record_timeout(&self, ak: &str) {
        let mut m = self.inner.lock();
        let e = m.entry(ak.to_string()).or_default();
        e.timeout_count += 1;
        if e.timeout_count >= TIMEOUT_THRESHOLD {
            e.disabled = true;
        }
        metrics::counter!("gw_redos_timeout_total",
            "api_key_id" => ak.to_string())
        .increment(1);
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
