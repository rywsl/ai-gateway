//! Tests for [`gw_auth::hard_cap`].

use gw_auth::hard_cap::*;

#[test]
fn input_tokens_under_cap_pass() {
    let cfg = HardCapCfg {
        max_input_tokens: 2_000_000,
        max_cost_micro: 100_000_000,
    };
    assert!(check_input_tokens(1000, &cfg).is_ok());
}

#[test]
fn input_tokens_over_cap_fails_413() {
    let cfg = HardCapCfg {
        max_input_tokens: 100,
        max_cost_micro: 1_000_000,
    };
    let err = check_input_tokens(101, &cfg).unwrap_err();
    assert_eq!(err.status, 413);
    assert_eq!(err.code, "GW_REQUEST_INPUT_TOO_LARGE");
}

#[test]
fn cost_over_cap_fails_413() {
    let cfg = HardCapCfg {
        max_input_tokens: 100,
        max_cost_micro: 1_000,
    };
    let err = check_cost(2_000, &cfg).unwrap_err();
    assert_eq!(err.status, 413);
    assert_eq!(err.code, "GW_REQUEST_COST_TOO_HIGH");
}
