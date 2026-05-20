//! gw-ledger: UsageRecord schema (ADR-Cα-1), FreezeGuard trait + impls
//! (ADR-004 + C-γ §ADR-Cγ-2), mpsc-backed usage-record flusher with Redis
//! pipeline, and detailed-log sink trait.

mod billing; // Task 5 deletes; kept private so flusher.rs compiles.
pub mod flusher;
pub mod freeze;
pub mod log_sink;
pub mod usage;

pub use flusher::Ledger;
pub use freeze::{FreezeGuard, FreezeKind, RequestFreeze, TaskFreeze};
