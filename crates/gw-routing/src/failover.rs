//! Response-side failover classifier and candidate iterator.
//!
//! This module is intentionally small: proxy wiring owns request replay,
//! in-flight guard release, and final error emission. The executor only decides
//! whether a response status is safe to retry and which already-ranked vendor
//! key should be tried next.

use gw_core::entity::VendorKey;
use std::collections::HashSet;
use std::sync::Arc;

/// Classification of an upstream response for failover decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverClass {
    /// 5xx status, body not yet started → retryable
    Retryable5xx,
    /// 429 status, body not yet started → retryable
    Retryable429,
    /// Any non-retryable status (2xx, 3xx, 4xx except 429)
    NotRetryable,
    /// Body streaming has already started → cannot retry
    BodyAlreadyStarted,
}

/// Decision output from the executor.
#[derive(Debug)]
pub enum FailoverDecision {
    /// Retry with this candidate key
    Retry(Arc<VendorKey>),
    /// All candidates exhausted or max retries reached
    Exhausted,
    /// Response is not retryable (non-5xx/429, or body already started)
    NotRetryable,
}

pub fn classify_for_failover(
    status: u16,
    body_started: bool,
    retryable_codes: &HashSet<u16>,
) -> FailoverClass {
    if body_started {
        return FailoverClass::BodyAlreadyStarted;
    }

    if !retryable_codes.contains(&status) {
        return FailoverClass::NotRetryable;
    }

    if status == 429 {
        FailoverClass::Retryable429
    } else {
        FailoverClass::Retryable5xx
    }
}

pub struct FailoverExecutor {
    max_retries: u8,
}

impl Default for FailoverExecutor {
    fn default() -> Self {
        Self { max_retries: 3 }
    }
}

impl FailoverExecutor {
    pub fn new(max_retries: u8) -> Self {
        Self { max_retries }
    }

    pub fn max_retries(&self) -> u8 {
        self.max_retries
    }

    /// Given a failed key and the original candidate list, decide the next action.
    /// `attempt` is 1-indexed (1 = first retry, 2 = second retry, ...).
    /// `remaining_candidates` is the original candidate list with already-failed keys removed.
    pub fn next_candidate(
        &self,
        remaining_candidates: &[Arc<VendorKey>],
        attempt: u8,
    ) -> FailoverDecision {
        if attempt > self.max_retries {
            return FailoverDecision::Exhausted;
        }

        match remaining_candidates.first() {
            Some(key) => FailoverDecision::Retry(Arc::clone(key)),
            None => FailoverDecision::Exhausted,
        }
    }
}

/// Bump the failover metric counter. Labels are `reason` + `strategy` only — no key-level.
pub fn record_failover_metric(class: FailoverClass, strategy_name: &str) {
    let reason = match class {
        FailoverClass::Retryable5xx => "upstream_5xx",
        FailoverClass::Retryable429 => "upstream_429",
        FailoverClass::NotRetryable => "not_retryable",
        FailoverClass::BodyAlreadyStarted => "body_started",
    };
    metrics::counter!(
        "gw_routing_failover_total",
        "reason" => reason,
        "strategy" => strategy_name.to_owned(),
    )
    .increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::KeyStatus;
    use gw_core::ids::{KeyId, VendorId};
    use secrecy::SecretString;

    fn default_retryable() -> HashSet<u16> {
        [429, 500, 502, 503, 504].into_iter().collect()
    }

    fn make_test_key(key_id: &str) -> Arc<VendorKey> {
        Arc::new(VendorKey {
            key_id: KeyId::from(key_id),
            vendor_id: VendorId::from("vendor-test"),
            api_key: SecretString::from("sk-test".to_string()),
            accessible_models: vec![],
            model_mapping: Default::default(),
            accessible_endpoints: vec![],
            protocol_conversion: false,
            priority: 0,
            weight: 1,
            rpm_limit: 0,
            tpm_limit: 0,
            max_usage_count: None,
            proxy: None,
            ip_allowlist: None,
            expires_at: None,
            status: KeyStatus::Active,
            tags: vec![],
            forced_capabilities: vec![],
            exclusive_capability_match: false,
            tunnel_preference: None,
            oauth: None,
            identity_snapshot: None,
            quota_state: Default::default(),
        })
    }

    #[test]
    fn test_classify_5xx() {
        let codes = default_retryable();
        assert_eq!(
            classify_for_failover(502, false, &codes),
            FailoverClass::Retryable5xx
        );
        assert_eq!(
            classify_for_failover(500, false, &codes),
            FailoverClass::Retryable5xx
        );
        assert_eq!(
            classify_for_failover(503, false, &codes),
            FailoverClass::Retryable5xx
        );
    }

    #[test]
    fn test_classify_429() {
        let codes = default_retryable();
        assert_eq!(
            classify_for_failover(429, false, &codes),
            FailoverClass::Retryable429
        );
    }

    #[test]
    fn test_classify_body_started() {
        let codes = default_retryable();
        assert_eq!(
            classify_for_failover(502, true, &codes),
            FailoverClass::BodyAlreadyStarted
        );
        assert_eq!(
            classify_for_failover(429, true, &codes),
            FailoverClass::BodyAlreadyStarted
        );
    }

    #[test]
    fn test_classify_not_retryable() {
        let codes = default_retryable();
        assert_eq!(
            classify_for_failover(400, false, &codes),
            FailoverClass::NotRetryable
        );
        assert_eq!(
            classify_for_failover(200, false, &codes),
            FailoverClass::NotRetryable
        );
        assert_eq!(
            classify_for_failover(404, false, &codes),
            FailoverClass::NotRetryable
        );
    }

    #[test]
    fn test_classify_custom_codes() {
        let codes: HashSet<u16> = [429, 503].into_iter().collect();
        assert_eq!(
            classify_for_failover(500, false, &codes),
            FailoverClass::NotRetryable
        );
        assert_eq!(
            classify_for_failover(503, false, &codes),
            FailoverClass::Retryable5xx
        );
        assert_eq!(
            classify_for_failover(429, false, &codes),
            FailoverClass::Retryable429
        );
    }

    #[test]
    fn test_classify_501_not_in_default_set() {
        let codes = default_retryable();
        assert_eq!(
            classify_for_failover(501, false, &codes),
            FailoverClass::NotRetryable
        );
    }

    #[test]
    fn test_next_candidate_success() {
        let executor = FailoverExecutor::default();
        let candidates = vec![
            make_test_key("key-a"),
            make_test_key("key-b"),
            make_test_key("key-c"),
        ];

        match executor.next_candidate(&candidates, 1) {
            FailoverDecision::Retry(key) => assert_eq!(key.key_id.as_ref(), "key-a"),
            other => panic!("expected retry, got {other:?}"),
        }

        match executor.next_candidate(&candidates[1..], 2) {
            FailoverDecision::Retry(key) => assert_eq!(key.key_id.as_ref(), "key-b"),
            other => panic!("expected retry, got {other:?}"),
        }
    }

    #[test]
    fn test_next_candidate_exhausted_by_attempts() {
        let executor = FailoverExecutor::new(1);
        let candidates = vec![make_test_key("key-a")];

        assert!(matches!(
            executor.next_candidate(&candidates, 2),
            FailoverDecision::Exhausted
        ));
    }

    #[test]
    fn test_next_candidate_exhausted_no_candidates() {
        let executor = FailoverExecutor::default();
        let candidates = [];

        assert!(matches!(
            executor.next_candidate(&candidates, 1),
            FailoverDecision::Exhausted
        ));
    }
}
