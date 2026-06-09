//! gw-routing: 候选 Key 过滤、选择策略（8 种）、in-flight 计数、API-key
//! affinity 管理。
//!
//! 主要导出：
//! - [`InFlightStore`] / [`InFlightGuard`] / [`InFlightCounter`] —— per-key
//!   in-flight counter (extracted from the retired `breaker` module per
//!   K-A ADR-KA-8).
//! - [`Strategy`] / [`StrategyAny`] / [`PickCtx`] —— 8 种策略 (ADR-009),
//!   now taking `&KeyHealthStore` + `&InFlightStore` (the retired single-store
//!   breaker is split: health → `gw-health`, in-flight → this crate).
//! - [`AffinityManager`] —— request-external api-key → vendor-key affinity
//!   bindings, separate from strategy-local `Sticky`/`CacheAffinity` state.
//!
//! Response-side retry / failover lives in [`failover`]. The legacy `retry.rs`
//! was deleted in Phase 1d per integration spec §2.1 A1; proxy wiring consumes
//! `gw-routing::failover` without re-running auth, quota, or strategy picks.

pub mod affinity;
pub mod candidates;
pub mod failover;
pub mod in_flight;
pub mod latency;
pub mod pipeline;
pub mod reservation;
pub mod reservation_gate;
pub mod strategy;

pub use affinity::{AffinityDimension, AffinityManager, AffinityRecord};
pub use failover::{classify_for_failover, FailoverClass, FailoverDecision, FailoverExecutor};
pub use in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
pub use latency::LatencyTracker;
pub use reservation::{GlobalModelView, ModelFrequencyTracker, ReservationGlobalView};
pub use strategy::{PickCtx, RankedStrategy, Strategy, StrategyAny};
