//! gw-routing: 候选 Key 过滤、选择策略（8 种）、in-flight 计数。
//!
//! 主要导出：
//! - [`InFlightStore`] / [`InFlightGuard`] / [`InFlightCounter`] —— per-key
//!   in-flight counter (extracted from the retired `breaker` module per
//!   K-A ADR-KA-8).
//! - [`Strategy`] / [`StrategyAny`] / [`PickCtx`] —— 8 种策略 (ADR-009),
//!   now taking `&KeyHealthStore` + `&InFlightStore` (the retired single-store
//!   breaker is split: health → `gw-health`, in-flight → this crate).
//!
//! Retry / failover lives outside this crate. The legacy `retry.rs` was deleted
//! in Phase 1d per integration spec §2.1 A1; Phase 4 R-E introduces a dedicated
//! `gw-routing::failover` module on top of the FailoverState state machine.

pub mod candidates;
pub mod in_flight;
pub mod strategy;

pub use in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
pub use strategy::{PickCtx, Strategy, StrategyAny};
