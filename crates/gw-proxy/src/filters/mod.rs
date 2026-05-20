//! Pingora filter chain modules. Each submodule owns one phase callback and is
//! invoked from the central [`crate::gateway::LlmGateway`] implementation.
//!
//! - [`early`] — IP blocklist, global stream cap, protocol detection, debug
//!   trigger.
//! - [`request`] — auth, IR parse, permission merge, rate-limit, hard caps,
//!   freeze-estimate, circuit check.
//! - [`upstream`] — pick a vendor key, render upstream body, build `HttpPeer`,
//!   inject `Authorization`.
//! - [`response`] — incremental SSE parsing + usage accumulation.
//! - [`logging`] — release freeze, record breaker outcome, emit UsageRecord
//!   and DetailedLog.

pub mod early;
pub mod logging;
pub mod request;
pub mod response;
pub mod upstream;
