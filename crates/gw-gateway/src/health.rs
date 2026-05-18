//! Minimal `/livez` + `/readyz` HTTP server.
//!
//! We deliberately avoid bringing in a full HTTP framework — this is a
//! single-purpose endpoint that the orchestrator (k8s, ECS, nomad) hits a few
//! times a second. A hand-rolled HTTP/1.1 responder on a `TcpListener` is
//! enough.
//!
//! Liveness is unconditional once the process is up; readiness is gated on
//! [`READY`] which the main bootstrap flips to `true` after all critical
//! collaborators (KMS, Redis, PgSQL, Ledger) finish initialising.

use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Flipped to `true` by the bootstrap once init succeeds. Readers use
/// `Acquire`, the bootstrap writer uses `Release`.
pub static READY: AtomicBool = AtomicBool::new(false);

const OK_BODY: &[u8] = b"ok";
const NOT_READY_BODY: &[u8] = b"not ready";

pub async fn run_health_server(port: u16) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(target: "gw_gateway::health", port, "health server listening");

    loop {
        let (mut stream, _peer) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(target: "gw_gateway::health", error = %e, "accept failed");
                continue;
            }
        };

        tokio::spawn(async move {
            // Read up to the first request line + (very minimal) headers. A
            // healthcheck client sends < 256 bytes; we cap the read at 1 KiB
            // to keep the parser allocation-free.
            let mut buf = [0u8; 1024];
            let n = match stream.read(&mut buf).await {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };

            let req = &buf[..n];
            // Extract path from the first line: METHOD SP PATH SP VERSION.
            let path = match parse_path(req) {
                Some(p) => p,
                None => {
                    let _ = stream.write_all(bad_request()).await;
                    return;
                }
            };

            let resp: &[u8] = match path {
                "/livez" => http_200(OK_BODY),
                "/readyz" => {
                    if READY.load(Ordering::Acquire) {
                        http_200(OK_BODY)
                    } else {
                        http_503(NOT_READY_BODY)
                    }
                }
                _ => http_404(),
            };

            let _ = stream.write_all(resp).await;
            let _ = stream.shutdown().await;
        });
    }
}

fn parse_path(req: &[u8]) -> Option<&str> {
    let line_end = req.iter().position(|&b| b == b'\n')?;
    let line = &req[..line_end];
    let mut parts = line.split(|&b| b == b' ');
    let _method = parts.next()?;
    let path_bytes = parts.next()?;
    std::str::from_utf8(path_bytes).ok()
}

fn http_200(body: &'static [u8]) -> &'static [u8] {
    match body.len() {
        2 => b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Type: text/plain\r\n\r\nok",
        _ => b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
    }
}

fn http_503(_body: &'static [u8]) -> &'static [u8] {
    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 9\r\nContent-Type: text/plain\r\n\r\nnot ready"
}

fn http_404() -> &'static [u8] {
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
}

fn bad_request() -> &'static [u8] {
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n"
}
