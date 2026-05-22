//! `e2e-fake-web` library surface.
//!
//! Exposes the modules and the axum router so integration tests (and the
//! upcoming E2-B admin API) can build the router in-process without going
//! through the `fake_web` binary.

pub mod api;
pub mod drain;
pub mod events;
pub mod jwks;
pub mod soak;
pub mod store;
pub mod usage_drain;

use axum::{
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use store::Store;
use tokio::sync::Mutex;

/// Shared state carried by every admin handler.
///
/// `drain_cursor` is shared between the background drain loop in
/// `main.rs` and the `POST /usage/drain` force-drain handler so the two
/// can't double-process or diverge.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub drain_cursor: Arc<Mutex<String>>,
    pub soak: Arc<soak::SoakTable>,
}

impl AppState {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            drain_cursor: Arc::new(Mutex::new(String::from("0-0"))),
            soak: Arc::new(soak::SoakTable::new()),
        }
    }
}

/// Construct the axum router. Exposed (pub) so main.rs and tests can
/// build it. Registers the full admin API:
///
/// - `GET  /healthz`
/// - `POST /entity/{table}/{id}`, `DELETE /entity/{table}/{id}`
/// - `POST /counter/{scope}/{id}/{field}`, `POST /counter-raw`
/// - `POST /balance/{user_id}`
/// - `POST /jwks/rotate`
/// - `POST /usage/drain`, `GET /usage/stats`
/// - `POST /admin/reset`
/// - `POST /soak/window/start`
/// - `GET  /soak/window/report`
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/entity/:table/:id",
            post(api::put_entity).delete(api::delete_entity),
        )
        .route("/counter/:scope/:id/:field", post(api::set_counter))
        .route("/counter-raw", post(api::set_counter_raw))
        .route("/balance/:user_id", post(api::set_balance))
        .route("/jwks/rotate", post(jwks::rotate))
        .route("/usage/drain", post(usage_drain::force_drain))
        .route("/usage/stats", get(usage_drain::stats))
        .route("/admin/reset", post(api::admin_reset))
        .route("/soak/window/start", post(soak::start))
        .route("/soak/window/report", get(soak::report))
        .with_state(state)
}
