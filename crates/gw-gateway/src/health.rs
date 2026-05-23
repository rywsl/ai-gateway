//! Minimal `/livez` + `/readyz` HTTP server.
//!
//! We deliberately avoid bringing in a full HTTP framework — this is a
//! single-purpose endpoint that the orchestrator (k8s, ECS, nomad) hits a few
//! times a second. A hand-rolled HTTP/1.1 responder on a `TcpListener` is
//! enough.
//!
//! Liveness is unconditional once the process is up; readiness is gated on
//! [`READY`] which the main thread flips to `true` after all critical
//! collaborators (KMS, Redis, PgSQL, Ledger) finish initialising AND the
//! pingora business service has been registered.
//!
//! Endpoint summary:
//!
//! - `/livez` — unconditional liveness ping.
//! - `/readyz` — gated on [`READY`] (init complete).
//! - `/healthz/circuits`, `/healthz/key/<id>`, `/healthz/resilience` —
//!   admin-only; require `Authorization: Mgmt <token>` (validated in T6-A
//!   wiring). v1 returns 401 unconditionally so production deployments that
//!   expose 8080 do not leak circuit / health internals before the mgmt
//!   token plumbing lands.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Flipped to `true` by the main thread once init succeeds. Readers use
/// `Acquire`, the writer uses `Release`.
pub static READY: AtomicBool = AtomicBool::new(false);

const OK_BODY: &[u8] = b"ok";
const NOT_READY_BODY: &[u8] = b"not ready";

/// Per-connection read timeout. Healthcheck clients send a tiny request and
/// close, so anything that takes longer than this is a slow/dead peer that we
/// should not let occupy a task indefinitely.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Long-lived observability handles consumed by the admin `/healthz/*`
/// endpoints. Built once in `bootstrap` and shared with the proxy
/// `LlmGateway` so the snapshot the operator sees matches the state the
/// request path observes.
///
/// T3-Pre constructs and threads this through `run_health_server`; T3-C
/// will replace the current blanket-401 routing block with a verify-then-
/// snapshot chain that consumes both `key_health` / `resilience` here
/// and the `store` + `mgmt_blacklist` passed alongside.
pub struct HealthState {
    pub key_health: Arc<gw_health::KeyHealthStore>,
    pub resilience: Arc<gw_proxy::resilience::ResilienceState>,
}

pub async fn run_health_server(
    port: u16,
    state: HealthState,
    store: Arc<gw_store::EntityStore>,
    mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(target: "gw_gateway::health", port, "health server listening");

    // T3-Pre: `state`, `store`, and `mgmt_blacklist` are accepted so T3-C
    // can wire the verify chain on the `/healthz/*` admin surface. v1
    // keeps the blanket-401 behaviour below; the handles are explicitly
    // held in the accept loop so subsequent commits add the verify
    // branch without changing the signature again.
    let state = Arc::new(state);
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "gw_gateway::health", error = %e, "accept failed");
                continue;
            }
        };

        let state = state.clone();
        let store = store.clone();
        let mgmt_blacklist = mgmt_blacklist.clone();
        tokio::spawn(handle_connection(stream, state, store, mgmt_blacklist));
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    _state: Arc<HealthState>,
    _store: Arc<gw_store::EntityStore>,
    _mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
) {
    // Read up to the first request line + (very minimal) headers. A
    // healthcheck client sends < 256 bytes; we cap the read at 1 KiB to keep
    // the parser allocation-free. A 5s timeout protects the task from slow
    // / blocked peers.
    let mut buf = [0u8; 1024];
    let n = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut buf)).await {
        Ok(Ok(0)) => return,
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            tracing::debug!(target: "gw_gateway::health", error = %e, "read failed");
            return;
        }
        Err(_) => {
            tracing::debug!(target: "gw_gateway::health", "read timeout");
            return;
        }
    };

    let req = &buf[..n];
    let path = match parse_path(req) {
        Some(p) => p,
        None => {
            let _ = write_response(&mut stream, 400, "Bad Request", b"").await;
            return;
        }
    };

    match path {
        "/livez" => {
            let _ = write_response(&mut stream, 200, "OK", OK_BODY).await;
        }
        "/readyz" => {
            if READY.load(Ordering::Acquire) {
                let _ = write_response(&mut stream, 200, "OK", OK_BODY).await;
            } else {
                let _ =
                    write_response(&mut stream, 503, "Service Unavailable", NOT_READY_BODY).await;
            }
        }
        path if path.starts_with("/healthz/circuits")
            || path.starts_with("/healthz/key/")
            || path == "/healthz/resilience" =>
        {
            // v1: gated behind management-token authorization (parsed by
            // gw-auth::mgmt). The real mgmt token validation lands in T6-A
            // wiring; for now we always reject so production deployments that
            // expose 8080 do not leak circuit / health internals.
            let _ = write_response(&mut stream, 401, "Unauthorized", b"mgmt token required").await;
        }
        _ => {
            let _ = write_response(&mut stream, 404, "Not Found", b"").await;
        }
    }

    let _ = stream.shutdown().await;
}

/// Parse the request path from the first line of an HTTP/1.1 request.
///
/// Accepts both `GET /path HTTP/1.1\r\n...` and the malformed-but-tolerated
/// `GET /path\r\n` (no version field). Strips a trailing `\r` left over from
/// the latter case, and trims any query string.
fn parse_path(req: &[u8]) -> Option<&str> {
    let line_end = req.iter().position(|&b| b == b'\n')?;
    let line = &req[..line_end];
    let mut parts = line.split(|&b| b == b' ');
    let _method = parts.next()?;
    let path_bytes = parts.next()?;
    let path = std::str::from_utf8(path_bytes).ok()?;
    // Strip CR if the path was the last field on the line (no version).
    let path = path.strip_suffix('\r').unwrap_or(path);
    // Strip query string — we match on path only.
    let path = path.split('?').next().unwrap_or(path);
    Some(path)
}

/// Write a minimal HTTP/1.1 response with the given status code, reason
/// phrase, and body. Headers and body are written sequentially so the body
/// bytes never have to be cloned into the same buffer as the header.
async fn write_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n",
        code = code,
        reason = reason,
        len = body.len(),
    );
    stream.write_all(header.as_bytes()).await?;
    if !body.is_empty() {
        stream.write_all(body).await?;
    }
    Ok(())
}
