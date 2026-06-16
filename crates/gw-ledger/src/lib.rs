//! gw-ledger: UsageRecord schema (ADR-Cα-1), FreezeGuard trait + impls
//! (ADR-004 + C-γ §ADR-Cγ-2), mpsc-backed usage-record flusher with Redis
//! pipeline, and detailed-archive writer.

pub mod backlog;
pub mod deduct_gc;
pub mod dlq;
pub mod encoding;
pub mod estimate_pricing;
pub mod flusher;
pub mod freeze;
pub mod inflight;
pub mod inflight_reaper;
pub mod inflight_reconciler;
pub mod instance_id;
pub mod ledger_sweep;
pub mod usage;

pub use deduct_gc::DeductGc;
pub use flusher::{ClearKey, Ledger, LedgerItem};
pub use freeze::{
    FreezeGuard, FreezeKind, ReleaseOutcome, RequestFreeze, TaskFreeze, TaskFreezeRelease,
};
pub use gw_core::permissions::MarkupChainSnapshot;
pub use usage::{UsageRecord, UsageStatus, VendorExposureState};

pub mod detailed_archive;
pub mod protection_config;
pub use protection_config::ProtectionConfig;
pub mod coef;
pub use coef::{
    apply_coef_commit, apply_coef_entry, current_snapshot as current_coef_snapshot,
    lookup as lookup_coef, CoefSnapshot, CommitOutcome, Dim, ModelCoefs, COEF_CACHE, COEF_STAGING,
};
pub mod lease;
pub use lease::{now_ms, upsert_protected, LeaseError, LeaseGuard, LeaseState, Mode, LEASE_POOL};
pub mod credit_estimator;
pub use credit_estimator::{actual_usage_credits, estimate_request_credits, EstimatorError};
pub mod counter_handler;
pub use counter_handler::install_counter_handler;
