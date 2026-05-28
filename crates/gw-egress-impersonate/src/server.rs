//! Egress request Handler: reads X-Egress-* control headers, dispatches via
//! a profile-matched `wreq::Client` from the pool, and streams the response
//! back as a boxed body.
//!
//! # Header stripping
//!
//! Before forwarding, the handler removes:
//! - **Internal egress headers** (`X-Egress-Target`, `X-Egress-Profile`,
//!   `X-Egress-Proxy`) — these are gateway internals, never upstream.
//! - **Hop-by-hop headers** (`Connection`, `Keep-Alive`, `Proxy-Authenticate`,
//!   `Proxy-Authorization`, `TE`, `Trailers`, `Transfer-Encoding`, `Upgrade`) —
//!   standard HTTP/1.1 hop-by-hop list per RFC 7230 §6.1.
//! - **`Host`** — stripped so wreq can set the correct value from the target URL.
//! - **`Content-Length`** — stripped so it does not clash with wreq's chunked
//!   re-framing of the streamed request body.
//!
//! All other headers (including `Authorization`, `User-Agent`, etc.) pass
//! through verbatim.
//!
//! # Body streaming
//!
//! The inbound body (any `hyper::body::Body`) is converted to a
//! `wreq::Body` via `Body::wrap`, which accepts any `HttpBody` with a
//! compatible error type.  The outbound body is streamed back via
//! `wreq::Response::bytes_stream()` wrapped in `http_body_util::StreamBody`
//! and boxed.
//!
//! # URL construction
//!
//! URL: `{scheme}://{X-Egress-Target}{path_and_query}`.
//! The default scheme is `https`. Tests may create a `Handler` with scheme
//! `http` via `Handler::new_with_scheme` to target plain-HTTP mock upstreams.

use std::sync::Arc;

use bytes::Bytes;
use futures_util::TryStreamExt;
use gw_core::impersonate::ImpersonateProfile;
use http_body_util::{BodyExt, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::{Request, Response, StatusCode};

use crate::error::{HDR_CLASS, HDR_DETAIL, classify};
use crate::pool::{ClientPool, PoolKey};

// ── Header lists ──────────────────────────────────────────────────────────────

/// Gateway-internal control headers. Always stripped before egress.
const INTERNAL_HEADERS: &[&str] = &["x-egress-target", "x-egress-profile", "x-egress-proxy"];

/// Standard HTTP/1.1 hop-by-hop headers (RFC 7230 §6.1) plus gateway-managed
/// framing headers. Stripped on both request egress and response ingress.
///
/// `Host` is stripped so wreq can set it correctly from the target URL.
/// `Content-Length` is stripped so it does not clash with wreq's chunked
/// re-framing of the streamed body.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

fn is_internal(name: &str) -> bool {
    INTERNAL_HEADERS.contains(&name)
}

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name)
}

// ── Response body type alias ──────────────────────────────────────────────────

/// The boxed body returned by `Handler::handle`. The error type is
/// `std::io::Error` because `StreamBody` over a `bytes_stream()` of a
/// `wreq::Response` can surface IO errors, and `io::Error` is the lowest
/// common denominator that hyper callers already handle.
pub type EgressBody = BoxBody<Bytes, std::io::Error>;

// ── Handler ───────────────────────────────────────────────────────────────────

/// In-process egress dispatcher. Holds an LRU pool of `wreq::Client`
/// instances, one per `(ImpersonateProfile, Option<proxy URL>)` pair.
pub struct Handler {
    pool: Arc<ClientPool>,
    /// URL scheme to use when building the upstream URL. Production code
    /// always uses `"https"`. Tests may set `"http"` to target plain-HTTP
    /// mock upstreams without TLS.
    scheme: &'static str,
}

impl Handler {
    /// Create a Handler with `capacity` pool slots and the default `https` scheme.
    pub fn new(capacity: usize) -> Self {
        Self {
            pool: Arc::new(ClientPool::new(capacity)),
            scheme: "https",
        }
    }

    /// Create a Handler with a custom URL scheme.
    ///
    /// **Test-only.** Pass `scheme = "http"` to target plain-HTTP mock upstreams
    /// in integration tests without TLS. Production code must always use
    /// `Handler::new` which defaults to `https`.
    #[cfg(feature = "test-utils")]
    #[doc(hidden)]
    pub fn new_with_scheme(capacity: usize, scheme: &'static str) -> Self {
        Self {
            pool: Arc::new(ClientPool::new(capacity)),
            scheme,
        }
    }

    /// Create a Handler sharing an existing `ClientPool`.
    ///
    /// Used by the supervisor so that a restarted listener inherits the pool
    /// rather than creating a new one (which would lose cached clients and
    /// change the apparent capacity).
    pub(crate) fn new_with_pool(pool: Arc<ClientPool>) -> Self {
        Self { pool, scheme: "https" }
    }

    /// Dispatch an egress request.
    ///
    /// Reads `X-Egress-Target` and `X-Egress-Profile` (both required) and
    /// optionally `X-Egress-Proxy`, then strips internal + hop-by-hop headers
    /// and forwards to the upstream via the pooled `wreq::Client`.
    ///
    /// Returns a synthetic error response (400/502/504) for any failure;
    /// never panics.
    pub async fn handle<B>(&self, req: Request<B>) -> Response<EgressBody>
    where
        B: hyper::body::Body<Data = Bytes> + Send + Sync + Unpin + 'static,
        B::Error: std::error::Error + Send + Sync + 'static,
    {
        // ── 1. Extract control headers ────────────────────────────────────
        let headers = req.headers();

        let target = match headers.get("x-egress-target") {
            Some(v) => match v.to_str() {
                Ok(s) => s.to_owned(),
                Err(_) => return bad_request("X-Egress-Target value is not valid UTF-8"),
            },
            None => return bad_request("X-Egress-Target header is required"),
        };

        let profile_str = match headers.get("x-egress-profile") {
            Some(v) => match v.to_str() {
                Ok(s) => s.to_owned(),
                Err(_) => return bad_request("X-Egress-Profile value is not valid UTF-8"),
            },
            None => return bad_request("X-Egress-Profile header is required"),
        };

        let profile: ImpersonateProfile = match profile_str.parse() {
            Ok(p) => p,
            Err(e) => return bad_request(&format!("invalid X-Egress-Profile: {e}")),
        };

        let proxy: Option<url::Url> = match headers.get("x-egress-proxy") {
            Some(v) => match v.to_str() {
                Ok(s) => match s.parse::<url::Url>() {
                    Ok(u) => Some(u),
                    Err(e) => return bad_request(&format!("invalid X-Egress-Proxy URL: {e}")),
                },
                Err(_) => return bad_request("X-Egress-Proxy value is not valid UTF-8"),
            },
            None => None,
        };

        // ── 2. Build upstream URL ─────────────────────────────────────────
        let path_and_query = req
            .uri()
            .path_and_query()
            .map(|pq| pq.as_str())
            .unwrap_or("/");
        let url = format!("{}://{}{}", self.scheme, target, path_and_query);

        // ── 3. Get or create the pooled wreq client ───────────────────────
        let key = PoolKey { profile, proxy };
        let client = match self.pool.get_or_create(key).await {
            Ok(c) => c,
            Err(e) => return upstream_error_response(502, "unknown", &e.to_string()),
        };

        // ── 4. Build the wreq request: method + stripped headers + body ───
        let method = req.method().clone();
        let mut wreq_builder = client.request(method, &url);

        // Copy headers, skipping internal and hop-by-hop entries.
        for (name, value) in req.headers() {
            let n = name.as_str();
            if !is_internal(n) && !is_hop_by_hop(n) {
                wreq_builder = wreq_builder.header(name.clone(), value.clone());
            }
        }

        // Stream the request body. `Body::wrap` accepts any `HttpBody` with
        // `B::Error: Into<BoxError>`. We map the body error into a `Box<dyn
        // Error + Send + Sync>` via the `BodyExt::map_err` adapter so the
        // bound is satisfied without importing wreq's internal `BoxError`.
        let body_stream = req
            .into_body()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>);
        let wreq_body = wreq::Body::wrap(body_stream);
        wreq_builder = wreq_builder.body(wreq_body);

        // ── 5. Execute the request ────────────────────────────────────────
        let wreq_resp = match wreq_builder.send().await {
            Ok(r) => r,
            Err(e) => {
                let class = classify(&e);
                let detail_str = e.to_string();
                let detail = truncate_256(&detail_str);
                return upstream_error_response(
                    class.status_code(),
                    class.as_str(),
                    detail,
                );
            }
        };

        // ── 6. Build the response ─────────────────────────────────────────
        let status = wreq_resp.status();
        let resp_headers = wreq_resp.headers().clone();

        // Stream response body via bytes_stream → StreamBody → BoxBody.
        let byte_stream = wreq_resp
            .bytes_stream()
            .map_ok(Frame::data)
            .map_err(std::io::Error::other);
        let boxed_body: EgressBody = StreamBody::new(byte_stream).boxed();

        let mut response = Response::builder().status(status);

        // Copy non-hop-by-hop response headers.
        for (name, value) in &resp_headers {
            if !is_hop_by_hop(name.as_str()) {
                response = response.header(name.clone(), value.clone());
            }
        }

        response.body(boxed_body).expect("valid response")
    }
}

// ── Error helpers ─────────────────────────────────────────────────────────────

fn empty_body() -> EgressBody {
    use http_body_util::Empty;
    Empty::<Bytes>::new()
        .map_err(|e: std::convert::Infallible| match e {})
        .boxed()
}

fn bad_request(detail: &str) -> Response<EgressBody> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header(HDR_CLASS, "bad_request")
        .header(HDR_DETAIL, truncate_256(detail))
        .body(empty_body())
        .expect("valid bad_request response")
}

fn upstream_error_response(status: u16, class: &str, detail: &str) -> Response<EgressBody> {
    Response::builder()
        .status(status)
        .header(HDR_CLASS, class)
        .header(HDR_DETAIL, truncate_256(detail))
        .body(empty_body())
        .expect("valid error response")
}

/// Truncate `s` to at most 256 bytes, clipping at a UTF-8 char boundary.
fn truncate_256(s: &str) -> &str {
    if s.len() <= 256 {
        return s;
    }
    let mut idx = 256;
    while !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

// ── Listener ──────────────────────────────────────────────────────────────────

use crate::handle::EgressBind;
use std::convert::Infallible;
use tokio::net::{TcpListener, UnixListener};

/// A bound, not-yet-accepting egress listener.
///
/// Created via [`Listener::bind`] and made active via [`Listener::spawn`].
/// The caller supplies a pre-built [`Handler`] (wrapped in `Arc`) so that
/// tests can inject `Handler::new_with_scheme("http")` while production code
/// passes `Handler::new(capacity)`.
pub struct Listener {
    bind: EgressBind,
    handler: Arc<Handler>,
}

impl Listener {
    /// Bind the listener to the given address and associate it with `handler`.
    ///
    /// For UDS binds: if the socket path already exists it is removed before
    /// binding (handles restarts without manual cleanup).
    pub async fn bind(bind: EgressBind, handler: Arc<Handler>) -> anyhow::Result<Self> {
        // Pre-cleanup for UDS path collisions.
        if let EgressBind::Uds(ref p) = bind {
            if p.exists() {
                std::fs::remove_file(p)?;
            }
        }
        Ok(Self { bind, handler })
    }

    /// Spawn the accept loop. Returns a [`ListenerJoinHandle`] that can be
    /// used to shut down the listener and await its task.
    pub fn spawn(self) -> ListenerJoinHandle {
        let bind = self.bind.clone();
        let handler = Arc::clone(&self.handler);
        let (tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

        let task = tokio::spawn(async move {
            match bind {
                EgressBind::Uds(path) => {
                    let listener = UnixListener::bind(&path)?;
                    tracing::info!("gw-egress-impersonate: UDS listener bound at {:?}", path);
                    loop {
                        tokio::select! {
                            biased;
                            _ = shutdown_rx.changed() => break,
                            accept = listener.accept() => match accept {
                                Ok((stream, _)) => {
                                    let h = Arc::clone(&handler);
                                    tokio::spawn(serve_conn_unix(stream, h));
                                }
                                Err(e) => tracing::warn!("UDS accept error: {e}"),
                            }
                        }
                    }
                }
                EgressBind::Tcp(addr) => {
                    let listener = TcpListener::bind(addr).await?;
                    tracing::info!(
                        "gw-egress-impersonate: TCP listener bound at {}",
                        listener.local_addr()?
                    );
                    loop {
                        tokio::select! {
                            biased;
                            _ = shutdown_rx.changed() => break,
                            accept = listener.accept() => match accept {
                                Ok((stream, peer)) => {
                                    if !peer.ip().is_loopback() {
                                        tracing::warn!(
                                            "gw-egress-impersonate: rejecting non-loopback peer {peer}"
                                        );
                                        // Drop the stream — this sends a TCP RST to the
                                        // caller. Intentional: non-loopback peers are not
                                        // permitted and should fail fast.
                                        continue;
                                    }
                                    let h = Arc::clone(&handler);
                                    tokio::spawn(serve_conn_tcp(stream, h));
                                }
                                Err(e) => tracing::warn!("TCP accept error: {e}"),
                            }
                        }
                    }
                }
            }
            anyhow::Result::<()>::Ok(())
        });

        ListenerJoinHandle { task, shutdown_tx: tx }
    }
}

// ── ListenerJoinHandle ────────────────────────────────────────────────────────

/// A handle to a spawned [`Listener`] task. Provides graceful shutdown.
pub struct ListenerJoinHandle {
    /// The listener's accept-loop task. `pub(crate)` so that `supervisor.rs`
    /// can `select!` on its completion without going through an extra channel.
    pub(crate) task: tokio::task::JoinHandle<anyhow::Result<()>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl ListenerJoinHandle {
    /// Signal the listener to stop accepting and await the task.
    pub async fn shutdown(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.task.await;
    }
}

// ── Per-connection serve helpers ──────────────────────────────────────────────

async fn serve_conn_unix(stream: tokio::net::UnixStream, handler: Arc<Handler>) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = hyper::service::service_fn(move |req| {
        let h = Arc::clone(&handler);
        async move { Ok::<_, Infallible>(h.handle(req).await) }
    });
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .await
    {
        tracing::debug!("gw-egress-impersonate: UDS conn error: {e}");
    }
}

async fn serve_conn_tcp(stream: tokio::net::TcpStream, handler: Arc<Handler>) {
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = hyper::service::service_fn(move |req| {
        let h = Arc::clone(&handler);
        async move { Ok::<_, Infallible>(h.handle(req).await) }
    });
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .await
    {
        tracing::debug!("gw-egress-impersonate: TCP conn error: {e}");
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_256_short() {
        assert_eq!(truncate_256("hello"), "hello");
    }

    #[test]
    fn truncate_256_long() {
        let s = "x".repeat(300);
        let t = truncate_256(&s);
        assert_eq!(t.len(), 256);
    }
}
