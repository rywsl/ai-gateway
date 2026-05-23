//! gw-ledger: UsageRecord schema (ADR-Cα-1), FreezeGuard trait + impls
//! (ADR-004 + C-γ §ADR-Cγ-2), mpsc-backed usage-record flusher with Redis
//! pipeline, and detailed-log sink trait.

pub mod detailed_stream;
pub mod encoding;
pub mod flusher;
pub mod freeze;
pub mod log_sink;
pub mod usage;

pub use flusher::Ledger;
pub use freeze::{FreezeGuard, FreezeKind, RequestFreeze, TaskFreeze};
pub use gw_core::permissions::MarkupChainSnapshot;
pub use usage::{UsageRecord, UsageSource, UsageStatus};

pub mod detailed_archive;
