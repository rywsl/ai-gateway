//! gw-ledger: UsageRecord schema (ADR-Cα-1), FreezeGuard trait + impls
//! (ADR-004 + C-γ §ADR-Cγ-2), mpsc-backed usage-record flusher with Redis
//! pipeline, and detailed-log sink trait.

// Task 5 rewrites flusher.rs (currently references the deleted billing
// module). Until then, keep it out of the module tree to keep gw-ledger
// compiling for downstream consumers of usage::* and freeze::*.
// pub mod flusher;
pub mod freeze;
pub mod log_sink;
pub mod usage;

// pub use flusher::Ledger;  // restored in Task 5.
pub use freeze::{FreezeGuard, FreezeKind, RequestFreeze, TaskFreeze};
pub use gw_core::permissions::MarkupChainSnapshot;
pub use usage::{UsageRecord, UsageSource, UsageStatus};
