//! Credit estimation for protected mode. Uses the per-lease `coef_version`
//! to look up `ModelCoefs` and convert tokens → credits.
//!
//! - `estimate_request_credits` runs at acquire time (2 dims: input + output)
//! - `actual_usage_credits` runs at settle time (6 dims)
//!
//! Both must use the SAME `coef_version` for one request, otherwise the
//! outstanding ledger on the Go side will drift. Spec §6.3.

use crate::coef::lookup as lookup_coef;
use gw_core::usage::UsageSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EstimatorError {
    UnknownModel,
    UnknownCoefVersion,
}

pub fn estimate_request_credits(
    coef_version: u64,
    model_id: &str,
    est_input_tokens: u32,
    max_output_tokens: u32,
) -> Result<i64, EstimatorError> {
    let err_kind = if coef_version == 0 {
        EstimatorError::UnknownCoefVersion
    } else {
        EstimatorError::UnknownModel
    };
    let coefs = lookup_coef(coef_version, model_id).ok_or(err_kind)?;
    let input = (est_input_tokens as i64).saturating_mul(coefs.input as i64);
    let output = (max_output_tokens as i64).saturating_mul(coefs.output as i64);
    Ok(input.saturating_add(output))
}

pub fn actual_usage_credits(
    coef_version: u64,
    model_id: &str,
    usage: &UsageSnapshot,
) -> Result<i64, EstimatorError> {
    let c = lookup_coef(coef_version, model_id).ok_or(EstimatorError::UnknownModel)?;
    let mut total: i64 = 0;
    total = total.saturating_add((usage.input_tokens as i64).saturating_mul(c.input as i64));
    total = total.saturating_add((usage.output_tokens as i64).saturating_mul(c.output as i64));
    total = total.saturating_add(
        (usage.cache_read_input_tokens.unwrap_or(0) as i64).saturating_mul(c.cache_read as i64),
    );
    total = total.saturating_add(
        (usage.cache_creation_input_tokens.unwrap_or(0) as i64)
            .saturating_mul(c.cache_creation as i64),
    );
    total = total.saturating_add(
        (usage.thinking_tokens.unwrap_or(0) as i64).saturating_mul(c.thinking as i64),
    );
    total = total.saturating_add(
        (usage.multimodal_input_tokens.unwrap_or(0) as i64)
            .saturating_mul(c.multimodal_input as i64),
    );
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coef::{
        apply_coef_commit, apply_coef_entry, CoefSnapshot, Dim, COEF_CACHE, COEF_STAGING,
        TEST_COEF_LOCK,
    };
    use std::sync::Arc;

    /// Serialize tests against the global COEF_CACHE / COEF_STAGING — shared
    /// with `coef::tests` and `counter_handler::tests` via `TEST_COEF_LOCK`.
    fn test_lock() -> &'static std::sync::Mutex<()> {
        &TEST_COEF_LOCK
    }

    fn install_test_coefs() {
        COEF_CACHE.store(Arc::new(CoefSnapshot::empty()));
        COEF_STAGING.clear();
        apply_coef_entry(42, "model-x", Dim::Input, 100);
        apply_coef_entry(42, "model-x", Dim::Output, 200);
        apply_coef_entry(42, "model-x", Dim::CacheRead, 50);
        apply_coef_commit(42, 1);
    }

    #[test]
    fn estimate_input_and_max_output() {
        let _g = test_lock().lock().unwrap();
        install_test_coefs();
        let c = estimate_request_credits(42, "model-x", 10, 20).unwrap();
        assert_eq!(c, 10 * 100 + 20 * 200);
    }

    #[test]
    fn estimate_unknown_model_errors() {
        let _g = test_lock().lock().unwrap();
        install_test_coefs();
        let err = estimate_request_credits(42, "unknown", 10, 20).unwrap_err();
        assert_eq!(err, EstimatorError::UnknownModel);
    }

    #[test]
    fn actual_usage_six_dims() {
        let _g = test_lock().lock().unwrap();
        install_test_coefs();
        let usage = UsageSnapshot {
            input_tokens: 5,
            output_tokens: 10,
            cache_read_input_tokens: Some(3),
            cache_creation_input_tokens: None,
            thinking_tokens: None,
            multimodal_input_tokens: None,
        };
        let c = actual_usage_credits(42, "model-x", &usage).unwrap();
        // 5*100 + 10*200 + 3*50 + 0 + 0 + 0
        assert_eq!(c, 5 * 100 + 10 * 200 + 3 * 50);
    }

    #[test]
    fn version_zero_returns_unknown_coef_version() {
        let _g = test_lock().lock().unwrap();
        install_test_coefs();
        let err = estimate_request_credits(0, "model-x", 1, 1).unwrap_err();
        assert_eq!(err, EstimatorError::UnknownCoefVersion);
    }
}
