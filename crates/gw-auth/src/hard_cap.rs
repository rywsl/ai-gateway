//! ADR-024 single-request hard caps (pure functions).
//!
//! - `check_input_tokens` rejects requests whose estimated input size exceeds
//!   `max_input_tokens` with 413 `GW_REQUEST_INPUT_TOO_LARGE`.
//! - `check_cost` rejects requests whose estimated cost (in micro-units) exceeds
//!   `max_cost_micro` with 413 `GW_REQUEST_COST_TOO_HIGH`.

use gw_core::error::{codes, ErrorCategory, ErrorIR};
use std::borrow::Cow;

pub struct HardCapCfg {
    pub max_input_tokens: u32,
    pub max_cost_micro: u64,
}

pub fn check_input_tokens(est: u32, cfg: &HardCapCfg) -> Result<(), ErrorIR<'static>> {
    if est <= cfg.max_input_tokens {
        return Ok(());
    }
    Err(ErrorIR {
        category: ErrorCategory::ClientBadRequest,
        status: 413,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_REQUEST_INPUT_TOO_LARGE),
        message: Cow::Borrowed("input too large"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    })
}

pub fn check_cost(est_micro: u64, cfg: &HardCapCfg) -> Result<(), ErrorIR<'static>> {
    if est_micro <= cfg.max_cost_micro {
        return Ok(());
    }
    Err(ErrorIR {
        category: ErrorCategory::ClientBadRequest,
        status: 413,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_REQUEST_COST_TOO_HIGH),
        message: Cow::Borrowed("cost too high"),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: None,
    })
}
