//! Server entry points.
//!
//! Two listeners:
//!   - **business** (`serve_business`) — fully manual on raw `TcpStream`.
//!     We parse the request head with `httparse`, match the scenario route,
//!     and dispatch either to the injection primitives in `crate::inject`
//!     (for the 6 failure variants) or to manual Static/Sse writers (also
//!     in `crate::inject`). Going full-manual lets us reuse one code path
//!     for both well-formed and broken responses; no hyper-stream rewinding
//!     is required.
//!   - **admin** (`serve_admin`) — stays on hyper. The admin endpoints are
//!     always well-formed HTTP, so hyper is fine.
//!
//! `ConnectionRefusedFirstN` is enforced in the business accept loop BEFORE
//! we read any bytes from the new socket, because connection refusal must
//! apply at TCP-handshake time (the client's `connect()` itself sees the
//! peer drop). The refuse counter is re-primed whenever the scenario is
//! swapped via `/admin/scenario`.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::inject::{self, RefuseCounter};
use crate::routes::{make_counters, match_route, HitCounters};
use crate::scenario::{ResponseSpec, Scenario};

// ── type aliases ─────────────────────────────────────────────────────────────

pub type SharedScenario = Arc<RwLock<Scenario>>;
pub type SharedCounters = Arc<RwLock<HitCounters>>;
pub type SharedWeightedHits = Arc<RwLock<crate::routes::WeightedHits>>;

// ── Business server (manual, raw TcpStream) ──────────────────────────────────

/// Run the business listener on `listener` until the task is cancelled.
///
/// `refuse` is a shared counter consulted at accept time before any bytes
/// are read from a freshly-accepted socket. Callers (typically `main` and
/// the admin handler on scenario swap) are responsible for priming it from
/// the scenario via [`prime_refuse_counter`].
pub async fn serve_business(
    listener: TcpListener,
    scenario: SharedScenario,
    counters: SharedCounters,
    weighted_hits: SharedWeightedHits,
    refuse: RefuseCounter,
) {
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[fake-upstream] business accept error: {e}");
                continue;
            }
        };

        // ── ConnectionRefusedFirstN gate ─────────────────────────────────────
        // Drop the socket BEFORE any HTTP handshake so the client sees a
        // post-accept reset (most clients surface this as a connection
        // error). We can't differentiate per-route here because we haven't
        // read the request yet — refusal applies to whichever connection
        // arrives first while the counter is non-zero.
        if refuse.take_one() {
            // Force RST so the client gets a hard error rather than a
            // clean EOF on an empty response.
            inject::rst_close(stream);
            continue;
        }

        let sc = Arc::clone(&scenario);
        let ctr = Arc::clone(&counters);
        let wh = Arc::clone(&weighted_hits);
        tokio::spawn(async move {
            if let Err(e) = handle_business_conn(stream, sc, ctr, wh).await {
                eprintln!("[fake-upstream] business conn error: {e}");
            }
        });
    }
}

/// Handle a single business connection: parse the head, match the route,
/// dispatch to the appropriate writer.
async fn handle_business_conn(
    mut stream: TcpStream,
    scenario: SharedScenario,
    counters: SharedCounters,
    weighted_hits: SharedWeightedHits,
) -> std::io::Result<()> {
    // 1) Read until end-of-headers (\r\n\r\n) or a sanity cap.
    let (method, path, headers) = match read_head(&mut stream).await? {
        Some(parsed) => parsed,
        None => {
            // Could not parse a request line within the cap → just close.
            return Ok(());
        }
    };

    // 2) Route match + auth, holding read locks only for sync work.
    let outcome: Outcome = {
        let sc = scenario.read().await;
        let ctr = counters.read().await;

        let matched = match_route(&sc, &method, &path, &headers, &ctr);
        match matched {
            None => Outcome::NotFound,
            Some((idx, route)) => {
                if let Some(ref ac) = route.auth_check {
                    let header_name = ac.header.to_lowercase();
                    let value = headers
                        .iter()
                        .find(|(k, _)| k.as_str().eq_ignore_ascii_case(&header_name))
                        .and_then(|(_, v)| v.to_str().ok())
                        .unwrap_or("");
                    if !value.starts_with(ac.must_start_with.as_str()) {
                        Outcome::Unauthorized
                    } else {
                        Outcome::Spec(idx, route.response.clone())
                    }
                } else {
                    Outcome::Spec(idx, route.response.clone())
                }
            }
        }
    };

    // 3) Dispatch on the raw stream, locks released.
    match outcome {
        Outcome::NotFound => {
            inject::write_static(
                stream,
                404,
                std::collections::HashMap::from([(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                r#"{"error":"no matching route in scenario"}"#.to_owned(),
                0,
            )
            .await?;
        }
        Outcome::Unauthorized => {
            inject::write_static(
                stream,
                401,
                std::collections::HashMap::from([(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                r#"{"error":"upstream auth check failed"}"#.to_owned(),
                0,
            )
            .await?;
        }
        Outcome::Spec(route_idx, spec) => {
            dispatch_spec(stream, spec, route_idx, weighted_hits).await?
        }
    }
    Ok(())
}

enum Outcome {
    NotFound,
    Unauthorized,
    Spec(usize, ResponseSpec),
}

async fn dispatch_spec(
    stream: TcpStream,
    spec: ResponseSpec,
    route_idx: usize,
    weighted_hits: SharedWeightedHits,
) -> std::io::Result<()> {
    // §4.3: unwrap exactly one Weighted layer. Validator rejects nested
    // Weighted and empty variants, so this branch is total.
    let spec = match spec {
        ResponseSpec::Weighted { variants } => {
            let (vidx, resolved) = if crate::chaos::enabled() {
                crate::chaos::pick_weighted_indexed(&variants)
            } else {
                (0usize, (*variants[0].spec).clone())
            };
            // Increment the per-(route, variant) counter under read-lock
            // (the inner vec is AtomicU64, fetch_add is wait-free).
            let wh = weighted_hits.read().await;
            if let Some(per_route) = wh.get(route_idx) {
                if let Some(slot) = per_route.get(vidx) {
                    slot.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            resolved
        }
        other => other,
    };
    match spec {
        ResponseSpec::Static {
            status,
            headers,
            body,
            delay_ms,
        } => inject::write_static(stream, status, headers, body, delay_ms).await,
        ResponseSpec::Sse { delay_ms, events } => inject::write_sse(stream, delay_ms, events).await,
        ResponseSpec::TcpResetAfterHeaders => inject::tcp_reset_after_headers(stream).await,
        ResponseSpec::TcpResetMidBody { after_bytes } => {
            inject::tcp_reset_mid_body(stream, after_bytes).await
        }
        ResponseSpec::ConnectionRefusedFirstN { .. } => {
            // Refusal is enforced in the accept loop. If a connection got
            // this far the counter was already exhausted — fall through
            // with a normal 200 acknowledgement so the path isn't a hole.
            inject::write_static(
                stream,
                200,
                std::collections::HashMap::from([(
                    "content-type".into(),
                    "application/json".into(),
                )]),
                r#"{"ok":true,"note":"refuse counter exhausted"}"#.to_owned(),
                0,
            )
            .await
        }
        ResponseSpec::SlowBodyBytePerSecond { body, bps } => {
            inject::slow_body_byte_per_second(stream, body, bps).await
        }
        ResponseSpec::PartialSseEvent { half_body } => {
            inject::partial_sse_event(stream, half_body).await
        }
        ResponseSpec::StatusWithRetryAfter {
            status,
            retry_after_secs,
            body,
        } => inject::status_with_retry_after(stream, status, retry_after_secs, body).await,
        ResponseSpec::GzipStatic {
            status,
            headers,
            body_base64,
            delay_ms,
        } => inject::write_gzip_static(stream, status, headers, body_base64, delay_ms).await,
        ResponseSpec::Weighted { .. } => {
            // Validator forbids nested Weighted (§4.3); reaching this arm
            // is a bug. Log loudly and close the connection so the test
            // notices instead of silently hanging.
            eprintln!("[fake-upstream] BUG: nested Weighted slipped past validator");
            Ok(())
        }
    }
}

/// Read the request head (request line + headers) from `stream`, returning
/// `(method, path, headers)`. Reads at most ~16 KiB and waits at most 5 s
/// in total before giving up (a slow / stuck client must not park the task
/// indefinitely, leaking the fd + Arcs across a test run).
async fn read_head(stream: &mut TcpStream) -> std::io::Result<Option<(String, String, HeaderMap)>> {
    const MAX_HEAD: usize = 16 * 1024;
    const HEAD_READ_TIMEOUT: Duration = Duration::from_secs(5);

    let read_fut = async {
        let mut buf = Vec::with_capacity(2048);
        let mut tmp = [0u8; 1024];
        // `search_from` tracks where the next CRLFCRLF scan should begin.
        // Each iteration only scans new bytes plus a 3-byte overlap to
        // catch a CRLFCRLF that straddles a read boundary.
        let mut search_from: usize = 0;
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                return Ok::<_, std::io::Error>(None); // Peer closed.
            }
            let prev_len = buf.len();
            buf.extend_from_slice(&tmp[..n]);
            let scan_start = prev_len.saturating_sub(3).max(search_from);
            if buf[scan_start..].windows(4).any(|w| w == b"\r\n\r\n") {
                break Ok(Some(buf));
            }
            search_from = buf.len();
            if buf.len() > MAX_HEAD {
                return Ok(None);
            }
        }
    };

    let buf = match timeout(HEAD_READ_TIMEOUT, read_fut).await {
        Ok(Ok(Some(b))) => b,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            // Slow / stuck client. Drop the connection silently.
            return Ok(None);
        }
    };

    let mut header_storage = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut header_storage);
    let status = match req.parse(&buf) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    if !status.is_complete() {
        return Ok(None);
    }
    let method = req.method.unwrap_or("").to_owned();
    let path = req.path.unwrap_or("/").to_owned();
    let mut headers = HeaderMap::new();
    for h in req.headers.iter() {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(h.name.as_bytes()),
            HeaderValue::from_bytes(h.value),
        ) {
            headers.append(name, val);
        }
    }
    Ok(Some((method, path, headers)))
}

// ── Refuse-counter priming ───────────────────────────────────────────────────

/// Inspect the active scenario and prime `refuse` to the largest declared
/// `ConnectionRefusedFirstN { n }` value (or 0 if none present).
pub fn prime_refuse_counter(scenario: &Scenario, refuse: &RefuseCounter) {
    let mut max_n: u32 = 0;
    for route in &scenario.routes {
        if let ResponseSpec::ConnectionRefusedFirstN { n } = route.response {
            if n > max_n {
                max_n = n;
            }
        }
    }
    refuse.set(max_n);
}

// ── Admin server (hyper) ─────────────────────────────────────────────────────

fn full_body(b: impl Into<Bytes>) -> BoxBody<Bytes, Infallible> {
    Full::new(b.into()).boxed()
}

fn json_response(status: StatusCode, body: &str) -> Response<BoxBody<Bytes, Infallible>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(full_body(body.to_owned()))
        .expect("valid response")
}

/// Run the admin listener on `listener` until cancelled.
pub async fn serve_admin(
    listener: TcpListener,
    scenario: SharedScenario,
    counters: SharedCounters,
    weighted_hits: SharedWeightedHits,
    refuse: RefuseCounter,
) {
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[fake-upstream] admin accept error: {e}");
                continue;
            }
        };
        let io = TokioIo::new(stream);
        let sc = Arc::clone(&scenario);
        let ctr = Arc::clone(&counters);
        let wh = Arc::clone(&weighted_hits);
        let refuse = refuse.clone();
        tokio::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(move |req| {
                        handle_admin(
                            req,
                            Arc::clone(&sc),
                            Arc::clone(&ctr),
                            Arc::clone(&wh),
                            refuse.clone(),
                        )
                    }),
                )
                .await
            {
                eprintln!("[fake-upstream] admin connection error: {e}");
            }
        });
    }
}

async fn handle_admin(
    req: Request<Incoming>,
    scenario: SharedScenario,
    counters: SharedCounters,
    weighted_hits: SharedWeightedHits,
    refuse: RefuseCounter,
) -> Result<Response<BoxBody<Bytes, Infallible>>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => Ok(Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/plain")
            .body(full_body("ok"))
            .expect("valid healthz response")),

        ("POST", "/admin/scenario") => {
            let body_bytes = req
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();

            match serde_json::from_slice::<Scenario>(&body_bytes) {
                Ok(new_scenario) => {
                    if let Err(e) = crate::scenario::validate_scenario(&new_scenario) {
                        return Ok(json_response(
                            StatusCode::BAD_REQUEST,
                            &serde_json::json!({"error": e}).to_string(),
                        ));
                    }
                    let new_counters = make_counters(&new_scenario);
                    let new_weighted = crate::routes::make_weighted_counters(&new_scenario);

                    // Prime the refuse counter from the NEW scenario before
                    // we publish it, so the next accepted connection sees a
                    // consistent (scenario, refuse_n) pair.
                    prime_refuse_counter(&new_scenario, &refuse);

                    {
                        let mut sc = scenario.write().await;
                        let mut ctr = counters.write().await;
                        let mut wh = weighted_hits.write().await;
                        *sc = new_scenario;
                        *ctr = new_counters;
                        *wh = new_weighted;
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

        ("GET", "/admin/chaos-stats") => {
            let sc = scenario.read().await;
            let wh = weighted_hits.read().await;
            let mut variants_out: Vec<serde_json::Value> = Vec::new();
            for (ri, route) in sc.routes.iter().enumerate() {
                if let ResponseSpec::Weighted { variants } = &route.response {
                    for (vi, v) in variants.iter().enumerate() {
                        let hits = wh
                            .get(ri)
                            .and_then(|r| r.get(vi))
                            .map(|a| a.load(std::sync::atomic::Ordering::Relaxed))
                            .unwrap_or(0);
                        variants_out.push(serde_json::json!({
                            "route_index": ri,
                            "variant_index": vi,
                            "kind": variant_kind(&v.spec),
                            "weight": v.weight,
                            "hits": hits,
                        }));
                    }
                }
            }
            let body = serde_json::json!({
                "enabled": crate::chaos::enabled(),
                "variants": variants_out,
            })
            .to_string();
            Ok(json_response(StatusCode::OK, &body))
        }

        _ => Ok(json_response(
            StatusCode::NOT_FOUND,
            r#"{"error":"unknown admin endpoint"}"#,
        )),
    }
}

fn variant_kind(spec: &ResponseSpec) -> &'static str {
    match spec {
        ResponseSpec::Static { .. } => "static",
        ResponseSpec::Sse { .. } => "sse",
        ResponseSpec::TcpResetAfterHeaders => "tcp_reset_after_headers",
        ResponseSpec::TcpResetMidBody { .. } => "tcp_reset_mid_body",
        ResponseSpec::ConnectionRefusedFirstN { .. } => "connection_refused_first_n",
        ResponseSpec::SlowBodyBytePerSecond { .. } => "slow_body_byte_per_second",
        ResponseSpec::PartialSseEvent { .. } => "partial_sse_event",
        ResponseSpec::StatusWithRetryAfter { .. } => "status_with_retry_after",
        ResponseSpec::GzipStatic { .. } => "gzip_static",
        ResponseSpec::Weighted { .. } => "weighted",
    }
}
