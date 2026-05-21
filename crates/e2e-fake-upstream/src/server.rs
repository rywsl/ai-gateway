use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full, StreamBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode};
use tokio::sync::RwLock;

use crate::routes::{make_counters, match_route, HitCounters};
use crate::scenario::{ResponseSpec, Scenario};

// ── type aliases ─────────────────────────────────────────────────────────────

pub type SharedScenario = Arc<RwLock<Scenario>>;
pub type SharedCounters = Arc<RwLock<HitCounters>>;

// ── helper constructors ───────────────────────────────────────────────────────

fn full_body(b: impl Into<Bytes>) -> BoxBody<Bytes, Infallible> {
    Full::new(b.into()).boxed()
}

fn empty_body() -> BoxBody<Bytes, Infallible> {
    Empty::new().boxed()
}

fn json_response(status: StatusCode, body: &str) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full_body(body.to_owned()))
        .expect("valid response")
}

// ── main business handler ─────────────────────────────────────────────────────

/// Handle a single request on the business port (:9000).
pub async fn handle_business(
    req: Request<hyper::body::Incoming>,
    scenario: SharedScenario,
    counters: SharedCounters,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let method = req.method().as_str().to_owned();
    let path = req.uri().path().to_owned();
    let headers = req.headers().clone();

    // Drain the body (needed by some future auth checks; harmless otherwise).
    let _body_bytes = req
        .into_body()
        .collect()
        .await
        .map(|c| c.to_bytes())
        .unwrap_or_default();

    // Acquire read locks, do all synchronous work (route matching, auth check,
    // hit-counter increment), clone out the owned ResponseSpec, then DROP both
    // guards BEFORE any await.  This ensures that a delayed Static/Sse response
    // never blocks POST /admin/scenario from acquiring the write lock.
    let spec = {
        let sc = scenario.read().await;
        let ctr = counters.read().await;

        // match_route increments the hit counter for the matched route while we
        // still hold `ctr`.
        let matched = match_route(&sc, &method, &path, &headers, &ctr);

        let Some((_idx, route)) = matched else {
            return Ok(json_response(
                StatusCode::NOT_FOUND,
                r#"{"error":"no matching route in scenario"}"#,
            ));
        };

        // Auth check (before dispatching to the response handler).
        if let Some(ref ac) = route.auth_check {
            let header_name = ac.header.to_lowercase();
            let value = headers
                .iter()
                .find(|(k, _)| k.as_str().eq_ignore_ascii_case(&header_name))
                .and_then(|(_, v)| v.to_str().ok())
                .unwrap_or("");

            if !value.starts_with(ac.must_start_with.as_str()) {
                return Ok(json_response(
                    StatusCode::UNAUTHORIZED,
                    r#"{"error":"upstream auth check failed"}"#,
                ));
            }
        }

        // Clone the spec; `sc` and `ctr` are dropped when this block exits.
        route.response.clone()
    };
    // Both read locks are released here — dispatch_response may sleep/await freely.

    let response = dispatch_response(spec).await;
    Ok(response)
}

/// Dispatch to the appropriate handler based on the response spec.
async fn dispatch_response(spec: ResponseSpec) -> Response<BoxBody<Bytes, Infallible>> {
    match spec {
        ResponseSpec::Static {
            status,
            headers,
            body,
            delay_ms,
        } => handle_static(status, headers, body, delay_ms).await,

        ResponseSpec::Sse { delay_ms, events } => handle_sse(delay_ms, events).await,

        // ── E1-B variants ────────────────────────────────────────────────────
        // Not implemented until E1-B.  Return a clean 501 so the server never
        // panics.  Do NOT use todo!() / unimplemented!().
        ResponseSpec::TcpResetAfterHeaders // E1-B
        | ResponseSpec::TcpResetMidBody { .. } // E1-B
        | ResponseSpec::ConnectionRefusedFirstN { .. } // E1-B
        | ResponseSpec::SlowBodyBytePerSecond { .. } // E1-B
        | ResponseSpec::PartialSseEvent { .. } // E1-B
        | ResponseSpec::StatusWithRetryAfter { .. } => {
            // E1-B
            json_response(
                StatusCode::NOT_IMPLEMENTED,
                r#"{"error":"injection variant not implemented until E1-B"}"#,
            )
        }
    }
}

// ── Static handler ────────────────────────────────────────────────────────────

async fn handle_static(
    status: u16,
    headers: std::collections::HashMap<String, String>,
    body: String,
    delay_ms: u64,
) -> Response<BoxBody<Bytes, Infallible>> {
    if delay_ms > 0 {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
    }

    let status_code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut builder = Response::builder().status(status_code);
    for (k, v) in &headers {
        builder = builder.header(k.as_str(), v.as_str());
    }

    builder
        .body(full_body(body))
        .unwrap_or_else(|_| Response::new(empty_body()))
}

// ── SSE handler ───────────────────────────────────────────────────────────────

async fn handle_sse(
    delay_ms: u64,
    events: Vec<crate::scenario::SseEvent>,
) -> Response<BoxBody<Bytes, Infallible>> {
    // Pre-response delay (applied before the first byte is sent).
    if delay_ms > 0 {
        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
    }

    // Build a lazy async stream: each item is a hyper Frame.
    // We collect the events into an owned vec and iterate asynchronously using
    // `stream::unfold` so no extra crate is needed beyond futures-util.
    let sse_stream = stream::unfold((events, 0usize), |(events, idx)| async move {
        let event = events.get(idx)?;
        if event.after_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(event.after_ms)).await;
        }
        let frame_bytes = format!("data: {}\n\n", event.data);
        let frame: Result<Frame<Bytes>, Infallible> = Ok(Frame::data(Bytes::from(frame_bytes)));
        Some((frame, (events, idx + 1)))
    });

    let body = StreamBody::new(sse_stream).boxed();

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("transfer-encoding", "chunked")
        .body(body)
        .expect("valid SSE response")
}

// ── Admin handler ─────────────────────────────────────────────────────────────

/// Handle a single request on the admin port (:9001).
pub async fn handle_admin(
    req: Request<hyper::body::Incoming>,
    scenario: SharedScenario,
    counters: SharedCounters,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    match (method.as_str(), path.as_str()) {
        // ── GET /healthz ──────────────────────────────────────────────────────
        ("GET", "/healthz") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/plain")
            .body(full_body("ok"))
            .expect("valid healthz response")),

        // ── POST /admin/scenario ──────────────────────────────────────────────
        ("POST", "/admin/scenario") => {
            let body_bytes = req
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();

            match serde_json::from_slice::<crate::scenario::Scenario>(&body_bytes) {
                Ok(new_scenario) => {
                    // Build fresh counters sized to the new scenario before
                    // acquiring the write lock so the lock is held briefly.
                    let new_counters = make_counters(&new_scenario);

                    {
                        let mut sc = scenario.write().await;
                        let mut ctr = counters.write().await;
                        *sc = new_scenario;
                        *ctr = new_counters;
                    }

                    Ok(json_response(StatusCode::OK, r#"{"ok":true}"#))
                }
                Err(e) => Ok(json_response(
                    StatusCode::BAD_REQUEST,
                    &serde_json::json!({"error": format!("invalid scenario JSON: {e}")})
                        .to_string(),
                )),
            }
        }

        _ => Ok(json_response(
            StatusCode::NOT_FOUND,
            r#"{"error":"unknown admin endpoint"}"#,
        )),
    }
}
