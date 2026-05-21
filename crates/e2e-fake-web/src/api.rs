//! Admin HTTP handlers for entity / counter / balance routes.
//!
//! All handlers operate on `AppState` and delegate to `Store`. Any error
//! from `Store` (table not in allowlist, or a transient sqlx/Redis
//! failure) is surfaced uniformly as `400 Bad Request` with the message —
//! the harness only asserts the success path, so the flavour of failure
//! does not need distinguishing here (see `err_to_400`).

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

/// Map any anyhow error from a Store call into a `400` with the message.
///
/// We don't try to distinguish "bad input" (e.g. unknown table) from
/// "transient backend failure" here because the test harness only
/// cares about the success path; a non-200 of any flavour is a failed
/// scenario, and the message gives the human enough to debug.
fn err_to_400(e: anyhow::Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

/// `POST /entity/:table/:id` — upsert an entity row. Body is the full
/// JSON payload, written verbatim into the JSONB `payload` column.
pub async fn put_entity(
    State(state): State<AppState>,
    Path((table, id)): Path<(String, String)>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    match state.store.put_entity(&table, &id, payload).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => err_to_400(e).into_response(),
    }
}

/// `DELETE /entity/:table/:id` — soft-delete an entity row.
pub async fn delete_entity(
    State(state): State<AppState>,
    Path((table, id)): Path<(String, String)>,
) -> impl IntoResponse {
    match state.store.delete_entity(&table, &id).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => err_to_400(e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct CounterBody {
    pub value: i64,
}

/// `POST /counter/:scope/:id/:field` — set
/// `counter:{scope}:{id}:{field}` to the body's `value`.
///
/// NOTE: axum path matching allows arbitrary non-`/` characters in a
/// single segment, so a field like `daily_tokens:2026-05-22` matches
/// this route fine (the colon is just data). For keys that don't
/// decompose into scope/id/field (or contain a `/`), use `/counter-raw`.
pub async fn set_counter(
    State(state): State<AppState>,
    Path((scope, id, field)): Path<(String, String, String)>,
    Json(body): Json<CounterBody>,
) -> impl IntoResponse {
    match state
        .store
        .set_counter(&scope, &id, &field, body.value)
        .await
    {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => err_to_400(e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct RawCounterBody {
    pub key: String,
    pub value: i64,
}

/// `POST /counter-raw` — escape hatch for fully-qualified counter keys
/// that don't fit the `scope/id/field` shape.
pub async fn set_counter_raw(
    State(state): State<AppState>,
    Json(body): Json<RawCounterBody>,
) -> impl IntoResponse {
    match state.store.set_raw_counter(&body.key, body.value).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => err_to_400(e).into_response(),
    }
}

#[derive(Deserialize)]
pub struct BalanceBody {
    pub micros: i64,
}

/// `POST /balance/:user_id` — convenience wrapper for
/// `counter:user:{user_id}:balance_micro`.
pub async fn set_balance(
    State(state): State<AppState>,
    Path(user_id): Path<String>,
    Json(body): Json<BalanceBody>,
) -> impl IntoResponse {
    match state.store.set_balance(&user_id, body.micros).await {
        Ok(()) => (StatusCode::OK, "ok").into_response(),
        Err(e) => err_to_400(e).into_response(),
    }
}
