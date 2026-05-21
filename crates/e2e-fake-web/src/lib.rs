//! `e2e-fake-web` library surface.
//!
//! Exposes the modules and the axum router so integration tests (and the
//! upcoming E2-B admin API) can build the router in-process without going
//! through the `fake_web` binary.

pub mod drain;
pub mod events;
pub mod store;

use axum::{routing::get, Router};
use std::sync::Arc;
use store::Store;

/// Construct the axum router. Exposed (pub) so E2-B can extend it with
/// the admin API (entity/counter/balance/jwks/usage routes) without
/// rewriting main.
pub fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .with_state(store)
}
