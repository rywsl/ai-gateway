//! Library surface of `e2e-fake-upstream`.
//!
//! Exposes the scenario DSL, route matcher, and the two server entry points
//! (`serve_business`, `serve_admin`) so that integration tests can bind
//! ephemeral ports and drive the server directly without the fixed
//! 9000/9001 ports used by the bin.

pub mod inject;
pub mod routes;
pub mod scenario;
pub mod server;
pub mod sse;
pub mod tls;

pub use inject::RefuseCounter;
pub use scenario::{default_scenario, Scenario};
pub use server::{serve_admin, serve_business, SharedCounters, SharedScenario};
pub use tls::serve_tls;
