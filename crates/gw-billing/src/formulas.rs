use gw_task::TaskKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TaskBillingConfig {
    pub base_micro: i64,
    pub vendor_multiplier: f64,
    pub estimated_duration_secs: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TaskBillingResult {
    pub actual_duration_secs: u64,
    pub succeeded: bool,
    pub vendor_multiplier: f64,
}

#[must_use]
pub fn calculate_freeze_amount(kind: TaskKind, config: &TaskBillingConfig) -> i64 {
    scaled_micro(
        config.base_micro,
        kind_multiplier(kind),
        config.vendor_multiplier,
        duration_multiplier(config.estimated_duration_secs),
    )
}

#[must_use]
pub fn calculate_settle_amount(kind: TaskKind, result: &TaskBillingResult) -> i64 {
    if !result.succeeded {
        return 0;
    }

    scaled_micro(
        settle_base_micro(kind),
        kind_multiplier(kind),
        result.vendor_multiplier,
        duration_multiplier(result.actual_duration_secs),
    )
}

const fn kind_multiplier(kind: TaskKind) -> u32 {
    match kind {
        TaskKind::Image => 3,
        TaskKind::Video => 6,
        TaskKind::Audio => 3,
        TaskKind::Batch => 2,
        TaskKind::LroResponses | TaskKind::Generic => 1,
    }
}

const fn settle_base_micro(kind: TaskKind) -> i64 {
    match kind {
        TaskKind::Image => 1_000,
        TaskKind::Video => 1_000,
        TaskKind::Audio => 750,
        TaskKind::Batch => 500,
        TaskKind::LroResponses | TaskKind::Generic => 250,
    }
}

const fn duration_multiplier(duration_secs: u64) -> u32 {
    match duration_secs {
        0..=60 => 1,
        61..=300 => 2,
        301..=900 => 3,
        _ => 4,
    }
}

fn scaled_micro(
    base_micro: i64,
    kind_factor: u32,
    vendor_factor: f64,
    duration_factor: u32,
) -> i64 {
    if base_micro <= 0 || !vendor_factor.is_finite() || vendor_factor <= 0.0 {
        return 0;
    }

    let amount =
        (base_micro as f64) * f64::from(kind_factor) * vendor_factor * f64::from(duration_factor);
    amount.round().clamp(0.0, i64::MAX as f64) as i64
}
