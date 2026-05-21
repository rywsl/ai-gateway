//! gw-proxy: Pingora `ProxyHttp` orchestration layer.
//!
//! Wires together Phase 1-3 building blocks (gw-codec / gw-store / gw-auth /
//! gw-routing / gw-ledger) into the request lifecycle.
//!
//! Task 4.1 lands the type and ID skeleton — `LlmGateway`, `ReqCtx`, jemalloc
//! sampling and timeout helpers. Task 4.2 fills in the filter chain modules
//! and wires them into the `ProxyHttp` impl.

pub mod ctx;
pub mod detailed_log;
pub mod error_emit;
pub mod filters;
pub mod gateway;
pub mod mem;
pub mod resilience;
pub mod timeouts;

pub use gateway::LlmGateway;
