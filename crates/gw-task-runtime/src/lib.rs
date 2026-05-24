//! Task freeze/release runtime — SUPPORTING layer.
//!
//! Provides the [`TaskFreezeRelease`] trait and a Redis-backed implementation
//! ([`RedisTaskFreezeRelease`]) for releasing frozen task balances.

pub use gw_core::ids::UserId;

/// Outcome of a task freeze release operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseOutcome {
    /// The frozen amount was successfully released.
    Released,
    /// The task had already been released (idempotent).
    AlreadyReleased,
}

/// Trait for releasing a previously frozen task balance.
#[async_trait::async_trait]
pub trait TaskFreezeRelease: Send + Sync {
    /// Release `frozen_amount` for the given user and task.
    ///
    /// Returns [`ReleaseOutcome::Released`] on first release,
    /// [`ReleaseOutcome::AlreadyReleased`] on repeated calls (idempotent).
    async fn release(
        &self,
        uid: &UserId,
        task_id: &str,
        frozen_amount: u64,
    ) -> Result<ReleaseOutcome, anyhow::Error>;
}

/// Redis-backed implementation of [`TaskFreezeRelease`].
///
/// Uses Lua scripts for atomic check-and-release semantics.
// T2 fills the implementation
pub struct RedisTaskFreezeRelease {
    _inner: (),
}
