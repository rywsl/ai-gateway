use std::fs;

use gw_billing::{
    calculate_freeze_amount, calculate_settle_amount, TaskBillingConfig, TaskBillingResult,
};
use gw_task::TaskKind;

#[test]
fn test_freeze_amount_calculation() {
    let config = TaskBillingConfig {
        base_micro: 1_000,
        vendor_multiplier: 2.0,
        estimated_duration_secs: 90,
    };

    assert_eq!(calculate_freeze_amount(TaskKind::Generic, &config), 4_000);
    assert_eq!(calculate_freeze_amount(TaskKind::Audio, &config), 12_000);
    assert_eq!(calculate_freeze_amount(TaskKind::Video, &config), 24_000);
}

#[test]
fn test_settle_amount_calculation() {
    let succeeded = TaskBillingResult {
        actual_duration_secs: 120,
        succeeded: true,
        vendor_multiplier: 1.5,
    };
    let failed = TaskBillingResult {
        actual_duration_secs: 120,
        succeeded: false,
        vendor_multiplier: 1.5,
    };

    assert_eq!(calculate_settle_amount(TaskKind::Image, &succeeded), 9_000);
    assert_eq!(calculate_settle_amount(TaskKind::Video, &succeeded), 18_000);
    assert_eq!(calculate_settle_amount(TaskKind::Video, &failed), 0);
}

#[test]
fn test_no_cost_per_token() {
    let src = fs::read_to_string("src/formulas.rs").expect("formulas source should be readable");
    let banned = ["cost_per_token", "price_per_token", "token_price"];

    for needle in banned {
        assert!(
            !src.contains(needle),
            "gw-billing formulas must not contain {needle}"
        );
    }
}
