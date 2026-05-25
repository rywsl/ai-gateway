//! gw-ledger: UsageRecord schema (ADR-Cα-1), FreezeGuard trait + impls
//! (ADR-004 + C-γ §ADR-Cγ-2), mpsc-backed usage-record flusher with Redis
//! pipeline, and detailed-archive writer.

pub mod deduct_gc;
pub mod encoding;
pub mod flusher;
pub mod freeze;
pub mod inflight;
pub mod inflight_reaper;
pub mod inflight_reconciler;
pub mod instance_id;
pub mod usage;

pub use deduct_gc::DeductGc;
pub use flusher::Ledger;
pub use freeze::{
    FreezeGuard, FreezeKind, ReleaseOutcome, RequestFreeze, TaskFreeze, TaskFreezeRelease,
};
pub use gw_core::permissions::MarkupChainSnapshot;
pub use usage::{UsageRecord, UsageStatus, VendorExposureState};

pub mod detailed_archive;
