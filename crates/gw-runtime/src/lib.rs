//! Shared runtime primitives for the gateway.
//!
//! Modules:
//! - [`supervisor`] — bounded restart for long-running tasks.
//! - [`shutdown`] — two-phase drain coordinator.
//! - [`spill_io`] — async sharded append-only files for DLQ / archive overflow.
//! - [`regex_cache`] — Moka-backed compiled-regex cache.
//! - [`health`] — `HealthHook` trait, `SystemHealthMonitor`, `BackpressureGate`.

pub mod health;
pub mod regex_cache;
pub mod shutdown;
pub mod spill_io;
pub mod supervisor;

pub use health::{Admit, BackpressureGate, HealthHook, HealthSignal, SystemHealthMonitor};
pub use regex_cache::{CompileError, RegexCache, RegexCacheConfig};
pub use shutdown::{DrainOutcome, PhaseHandle, ShutdownCoordinator, ShutdownPhase, ShutdownToken};
pub use spill_io::{SpillFile, SpillRoot, SpillRootConfig};
pub use supervisor::{RestartPolicy, Supervisor, SupervisorError};
