//! Plain-HTTP request recorder for impersonation E2E tests.
//!
//! Drops a single-method API for tests that need to assert *what the upstream
//! actually received* — every accepted HTTP/1.1 request is parsed and pushed
//! into a shared `Vec<CapturedRequest>` so the test can inspect headers after
//! the fact.
//!
//! # Why a separate module from `common::mock_upstream`?
//!
//! The existing `mock_upstream` in `gw-egress-impersonate/tests/common/mod.rs`
//! echoes received headers as a JSON body. That works for single-shot
//! assertions but cannot answer "what did the *N*-th request look like?".
//! The recorder keeps the wire history.
//!
//! # Scope
//!
//! - Plain HTTP/1.1 only (no TLS). Callers point the upstream URL at
//!   `http://127.0.0.1:<recorder_port>`.
//! - One canned 200 response with a minimal Anthropic-shaped body so codec
//!   handling does not panic in callers that try to parse the body.
//! - Not robust against pipelined or malformed requests — only what the
//!   gateway emits, which is well-formed.

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response};
use hyper_util::rt::TokioIo;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

/// A single request the recorder captured before responding.
#[derive(Clone, Debug)]
pub struct CapturedRequest {
    pub method: Method,
    pub path: String,
    pub headers: HeaderMap<HeaderValue>,
}

impl CapturedRequest {
    /// Lowercase header lookup. Header names in HTTP/1.1 are case-insensitive;
    /// callers asserting on stainless / x-egress-* should use this rather than
    /// indexing the `HeaderMap` directly.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = HeaderName::from_bytes(name.as_bytes()).ok()?;
        self.headers.get(name)?.to_str().ok()
    }

    /// Returns true if the captured request had a header with the given
    /// (case-insensitive) name.
    pub fn has_header(&self, name: &str) -> bool {
        let Ok(name) = HeaderName::from_bytes(name.as_bytes()) else {
            return false;
        };
        self.headers.contains_key(name)
    }
}

/// Handle to a running recorder.
#[derive(Clone)]
pub struct Recorder {
    /// `127.0.0.1:<port>` the recorder is listening on. Use as
    /// `X-Egress-Target` value or the `host:port` of `Vendor.base_url`.
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
}

impl Recorder {
    /// Snapshot of every request captured so far, in arrival order.
    pub fn all_requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().expect("recorder mutex").clone()
    }

    /// The most recent captured request, or `None` if none have arrived.
    pub fn last_request(&self) -> Option<CapturedRequest> {
        self.requests
            .lock()
            .expect("recorder mutex")
            .last()
            .cloned()
    }

    /// Number of requests captured so far.
    pub fn count(&self) -> usize {
        self.requests.lock().expect("recorder mutex").len()
    }
}

/// Start the recorder on an OS-assigned loopback port.
///
/// Spawns a background tokio task that accepts plain HTTP/1.1 connections,
/// records each request, and returns a canned 200 with a minimal
/// Anthropic-shaped body. The task is detached and lives until the runtime
/// shuts down.
pub async fn start_recorder() -> Recorder {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind recorder loopback");
    let addr = listener.local_addr().expect("recorder local_addr");
    let requests: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&requests);

    tokio::spawn(async move {
        loop {
            let (stream, _peer) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                let svc = service_fn(move |req: Request<Incoming>| {
                    let store = Arc::clone(&store);
                    async move {
                        let captured = CapturedRequest {
                            method: req.method().clone(),
                            path: req
                                .uri()
                                .path_and_query()
                                .map(|pq| pq.as_str().to_owned())
                                .unwrap_or_else(|| "/".to_owned()),
                            headers: req.headers().clone(),
                        };
                        store.lock().expect("recorder mutex").push(captured);
                        Ok::<_, Infallible>(canned_response())
                    }
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    Recorder { addr, requests }
}

/// Minimal Anthropic-shaped 200 response. The body is just enough JSON to
/// not look syntactically broken to a codec sniffer; tests do not parse it.
fn canned_response() -> Response<BoxBody<Bytes, Infallible>> {
    const BODY: &str = r#"{"type":"message","role":"assistant","content":[]}"#;
    Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(BODY)).boxed())
        .expect("valid recorder response")
}
