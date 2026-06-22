use gw_task::TaskKind;
use serde::{Deserialize, Serialize};

const VENDOR_FACTOR_MICRO_SCALE: u32 = 1_000_000;

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
        TaskKind::LroResponses | TaskKind::Generic | TaskKind::Embedding => 1,
    }
}

const fn settle_base_micro(kind: TaskKind) -> i64 {
    match kind {
        TaskKind::Image => 1_000,
        TaskKind::Video => 1_000,
        TaskKind::Audio => 750,
        TaskKind::Batch => 500,
        TaskKind::LroResponses | TaskKind::Generic | TaskKind::Embedding => 250,
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

    let vendor_factor_micro = vendor_factor_micro_units(vendor_factor);
    if vendor_factor_micro == 0 {
        return 0;
    }

    let amount = i128::from(base_micro)
        .saturating_mul(i128::from(kind_factor))
        .saturating_mul(i128::from(vendor_factor_micro))
        .saturating_mul(i128::from(duration_factor));
    let scale = i128::from(VENDOR_FACTOR_MICRO_SCALE);
    let amount = amount.saturating_add(scale / 2) / scale;
    amount.clamp(0, i128::from(i64::MAX)) as i64
}

fn vendor_factor_micro_units(vendor_factor: f64) -> u32 {
    if vendor_factor.is_nan() || vendor_factor <= 0.0 {
        return 0;
    }
    let scaled = vendor_factor * f64::from(VENDOR_FACTOR_MICRO_SCALE);
    if scaled.is_infinite() {
        return u32::MAX;
    }
    if !scaled.is_finite() || scaled <= 0.0 {
        return 0;
    }
    scaled.round().clamp(0.0, f64::from(u32::MAX)) as u32
}
