//! `POST /usage/drain` — force one drain cycle. Reuses the shared
//! cursor from `AppState` so the background loop and the forced cycle
//! advance the same position.
//!
//! `GET /usage/stats` — XLEN of `usage:records`, `usage:records:dlq`,
//! and `usage:detailed`, returned as a JSON triple. XLEN of a missing
//! stream is `0`, so callers don't have to special-case fresh Redis.

use crate::AppState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

#[derive(Serialize)]
pub struct ForceDrainResponse {
    pub processed: usize,
}

pub async fn force_drain(State(state): State<AppState>) -> impl IntoResponse {
    let mut guard = state.drain_cursor.lock().await;
    match crate::drain::drain_once(&state.store, &mut guard).await {
        Ok(processed) => Json(ForceDrainResponse { processed }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
pub struct UsageStats {
    pub records_xlen: i64,
    pub dlq_xlen: i64,
    pub detailed_xlen: i64,
}

pub async fn stats(State(state): State<AppState>) -> impl IntoResponse {
    let records = state.store.xlen("usage:records").await;
    let dlq = state.store.xlen("usage:records:dlq").await;
    let detailed = state.store.xlen("usage:detailed").await;
    match (records, dlq, detailed) {
        (Ok(r), Ok(d), Ok(dt)) => Json(UsageStats {
            records_xlen: r,
            dlq_xlen: d,
            detailed_xlen: dt,
        })
        .into_response(),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}
