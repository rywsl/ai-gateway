//! Retry executor (ADR-022). Centralizes the "should we retry?" decision and
//! exponential backoff with jitter.
//!
//! Retry policy summary:
//! - Non-retryable errors → no retry.
//! - Streaming responses that have already shipped bytes to the client → no
//!   retry (cannot rewind a partial SSE stream).
//! - Non-idempotent requests → retry only on `UpstreamTimeout` (it is unclear
//!   whether the upstream actually executed the side effect).
//! - Total wall-clock budget: 60s non-stream, 600s stream.
//! - Per-request attempt cap: `cross_key_max` (default 3).
//!
//! v1 limitation: `RetryConfig::mode` and `same_key_max` are accepted but not
//! enforced — the executor uses `cross_key_max` as the global attempt cap. The
//! same-key vs cross-key distinction (ADR-022) requires the caller to update
//! `mark_visited` on each new attempt; this wiring lives in gw-proxy and will
//! activate the full mode semantics in Phase 4.

use gw_core::error::{ErrorCategory, ErrorIR};
use gw_core::ids::KeyId;
use std::collections::HashSet;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub enum RetryMode {
    /// Always retry on the same key.
    SameKey,
    /// Always re-pick a different key.
    CrossKey,
    /// Up to `same_key_max` retries on the current key, then cross-key.
    Mixed,
}

#[derive(Clone, Debug)]
pub struct RetryConfig {
    pub mode: RetryMode,
    pub same_key_max: u32,
    pub cross_key_max: u32,
    pub backoff_base_ms: u32,
    pub backoff_max_ms: u32,
    pub total_budget_ms_nonstream: u32,
    pub total_budget_ms_stream: u32,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            mode: RetryMode::Mixed,
            same_key_max: 2,
            cross_key_max: 3,
            backoff_base_ms: 500,
            backoff_max_ms: 10_000,
            total_budget_ms_nonstream: 60_000,
            total_budget_ms_stream: 600_000,
        }
    }
}

pub struct RetryExecutor {
    pub config: RetryConfig,
    pub visited: HashSet<KeyId>,
    pub attempts: u32,
    pub start: Instant,
    pub idempotent: bool,
    pub is_stream: bool,
}

impl RetryExecutor {
    pub fn new(config: RetryConfig, idempotent: bool, is_stream: bool) -> Self {
        Self {
            config,
            visited: HashSet::new(),
            attempts: 0,
            start: Instant::now(),
            idempotent,
            is_stream,
        }
    }

    /// Returns `true` iff another attempt is permitted; on `true` also
    /// increments the internal attempt counter.
    pub fn should_retry(&mut self, err: &ErrorIR<'_>, bytes_sent_to_client: u64) -> bool {
        if !err.retryable {
            return false;
        }
        // Cannot rewind a partially-flushed SSE response.
        if self.is_stream && bytes_sent_to_client > 0 {
            return false;
        }
        // Non-idempotent (e.g. POST that creates side effects): only safe to
        // retry on timeout, where the upstream may never have received the body.
        if !self.idempotent && err.category != ErrorCategory::UpstreamTimeout {
            return false;
        }
        let budget = if self.is_stream {
            self.config.total_budget_ms_stream
        } else {
            self.config.total_budget_ms_nonstream
        };
        if self.start.elapsed() > Duration::from_millis(budget as u64) {
            return false;
        }
        if self.attempts >= self.config.cross_key_max {
            return false;
        }
        self.attempts += 1;
        true
    }

    /// Exponential backoff with full jitter, capped at `backoff_max_ms`.
    pub fn backoff(&self) -> Duration {
        let n = self.attempts;
        let shift = n.min(8);
        let ms = self
            .config
            .backoff_base_ms
            .saturating_mul(1u32 << shift)
            .min(self.config.backoff_max_ms);
        let half_base = self.config.backoff_base_ms / 2 + 1;
        let jitter = (rand::random::<u32>() % half_base) as u64;
        Duration::from_millis(ms as u64 + jitter)
    }

    pub fn mark_visited(&mut self, k: &KeyId) {
        self.visited.insert(k.clone());
    }

    pub fn was_visited(&self, k: &KeyId) -> bool {
        self.visited.contains(k)
    }
}
