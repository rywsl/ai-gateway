//! gw-routing: 候选 Key 过滤、选择策略（8 种）、in-flight 计数与重试预算。
//!
//! 主要导出：
//! - [`InFlightStore`] / [`InFlightGuard`] / [`InFlightCounter`] —— per-key
//!   in-flight counter (extracted from the retired `breaker` module per
//!   K-A ADR-KA-8).
//! - [`Strategy`] / [`StrategyAny`] / [`PickCtx`] —— 8 种策略 (ADR-009),
//!   now taking `&KeyHealthStore` + `&InFlightStore` (the retired single-store
//!   breaker is split: health → `gw-health`, in-flight → this crate).
//! - [`RetryConfig`] / [`RetryMode`] / [`RetryExecutor`] —— 重试预算与退避
//!   (ADR-022). Phase 4 R-E replaces this with `failover.rs`.

pub mod candidates;
pub mod in_flight;
pub mod retry;
pub mod strategy;

pub use in_flight::{InFlightCounter, InFlightGuard, InFlightStore};
pub use retry::{RetryConfig, RetryExecutor, RetryMode};
pub use strategy::{PickCtx, Strategy, StrategyAny};
