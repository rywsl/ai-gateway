//! Task freeze/release runtime — SUPPORTING layer.
//!
//! Provides a Redis-backed implementation ([`RedisTaskFreezeRelease`]) of
//! [`TaskFreezeRelease`] for releasing frozen task balances.

pub mod cancel_loop;
pub mod exec_loop;
pub mod heartbeat_loop;
pub mod task_metrics;
pub mod watchdog_loop;

mod release;

pub use gw_core::ids::UserId;
pub use gw_ledger::{ReleaseOutcome, TaskFreezeRelease};
pub use release::{RedisTaskFreezeRelease, TASK_FREEZE_RELEASE_LUA};
