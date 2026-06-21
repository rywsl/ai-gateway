//! Centralized metric names used by the credit-lease hard-mode subsystem.

pub const CREDIT_LEASE_ACQUIRE_TOTAL: &str = "gw_credit_lease_acquire_total";
pub const CREDIT_LEASE_ESTIMATE_CREDITS: &str = "gw_credit_lease_estimate_credits";
pub const CREDIT_OVERFLOW_GATE_TOTAL: &str = "gw_credit_overflow_gate_total";
pub const CREDIT_OVERFLOW_LATENCY_MS: &str = "gw_credit_overflow_latency_ms";
pub const CREDIT_OVERFLOW_CONSUMED_WINDOW: &str = "gw_credit_overflow_consumed_window";
pub const PG_FALLBACK_TOTAL: &str = "gw_pg_fallback_total";
pub const COEF_TABLE_ACTIVE_VERSION: &str = "gw_coef_table_active_version";
pub const HEARTBEAT_SADD_TOTAL: &str = "gw_heartbeat_sadd_total";
pub const CREDIT_ACTUAL_EXCEEDS_RESERVED_TOTAL: &str = "gw_credit_actual_exceeds_reserved_total";
pub const CREDIT_ACTIVE_REGISTER_ENQUEUE_TOTAL: &str = "gw_credit_active_register_enqueue_total";
pub const CREDIT_ACTIVE_REGISTER_REDIS_TOTAL: &str = "gw_credit_active_register_redis_total";
pub const CREDIT_ACTIVE_REGISTER_QUEUE_DEPTH: &str = "gw_credit_active_register_queue_depth";
pub const CREDIT_LEASE_SWEEP_REMOVED_TOTAL: &str = "gw_credit_lease_sweep_removed_total";
pub const CREDIT_BALANCE_FALLBACK_TOTAL: &str = "gw_credit_balance_fallback_total";
