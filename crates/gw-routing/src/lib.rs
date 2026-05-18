//! gw-routing: 候选 Key 过滤、选择策略（8 种）、熔断器与重试执行器。
//!
//! 主要导出：
//! - [`BreakerStore`] / [`BreakerState`] —— 30s 滑动窗口 + half-open 熔断 (ADR-008)
//! - [`Strategy`] / [`StrategyAny`] / [`PickCtx`] —— 8 种策略 (ADR-009)
//! - [`RetryConfig`] / [`RetryMode`] / [`RetryExecutor`] —— 重试预算与退避 (ADR-022)

pub mod breaker;
pub mod candidates;
pub mod retry;
pub mod strategy;

pub use breaker::{BreakerState, BreakerStore};
pub use retry::{RetryConfig, RetryExecutor, RetryMode};
pub use strategy::{PickCtx, Strategy, StrategyAny};
