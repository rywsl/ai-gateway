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
//! - `/readyz` — gated on [`READY`] (init complete), AND the process is not
//!   in a shutdown phase >= Phase1AcceptStop, AND `BackpressureGate` is not
//!   active, AND no Supervisor has marked itself unhealthy. Any of these
//!   conditions causes a 503 response so k8s stops routing new traffic.
//! - `/healthz/circuits`, `/healthz/key/<id>`, `/healthz/resilience`,
//!   `/healthz/tunnel`, `/healthz/strategies` —
//!   admin-only; require `Authorization: Mgmt <token>` validated by the
//!   management-token verify chain (S-admin-tunnel). Unverified requests
//!   receive 401.

use bytes::Buf;
use fred::interfaces::StreamsInterface;
use fred::types::{XCap, XCapKind, XCapTrim};
use gw_tunnel_client::Transport;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Error as WsError;

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
/// Constructed and threaded through `run_health_server`; the `/healthz/*`
/// surface has its own management-token verify chain before rendering
/// snapshots from `key_health` / `resilience` plus the `store` and
/// `mgmt_blacklist` passed alongside.
#[derive(Clone)]
pub struct HealthState {
    pub key_health: Arc<gw_health::KeyHealthStore>,
    pub resilience: Arc<gw_proxy::resilience::ResilienceState>,
    pub latency_tracker: Arc<gw_routing::LatencyTracker>,
    pub tunnel_enabled: bool,
    pub tunnel_stack: Option<Arc<gw_proxy::tunnel_stack::TunnelStack>>,
    pub aether_listener: Option<Arc<gw_tunnel_client::aether::listener::InboundListener>>,
    pub aether_listener_path: String,
    pub aether_heartbeat_sink: Arc<dyn gw_tunnel_client::aether::redis_sink::HeartbeatSink>,
}

pub async fn run_health_server(
    port: u16,
    state: HealthState,
    store: Arc<gw_store::EntityStore>,
    mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
    phase_handle: gw_runtime::PhaseHandle,
    backpressure_state: Arc<AtomicU8>,
    supervisor_unhealthy: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(target: "gw_gateway::health", port, "health server listening");

    // `store` and `mgmt_blacklist` are consumed by the admin verify chain
    // on `/healthz/*` endpoints. The verify chain is now wired; these
    // handles are held in the accept loop for the admin surface.
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
        let phase_handle = phase_handle.clone();
        let backpressure_state = backpressure_state.clone();
        let supervisor_unhealthy = supervisor_unhealthy.clone();
        tokio::spawn(handle_connection(
            stream,
            state,
            store,
            mgmt_blacklist,
            phase_handle,
            backpressure_state,
            supervisor_unhealthy,
        ));
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    state: Arc<HealthState>,
    store: Arc<gw_store::EntityStore>,
    mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
    phase_handle: gw_runtime::PhaseHandle,
    backpressure_state: Arc<AtomicU8>,
    supervisor_unhealthy: Arc<AtomicBool>,
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
        path if state.aether_listener.is_some() && path == state.aether_listener_path => {
            if let Err(err) = handle_aether_upgrade(stream, req, &state).await {
                tracing::debug!(target: "gw_gateway::health", error = %err, "aether upgrade failed");
            }
            return;
        }
        "/livez" => {
            let _ = write_response(&mut stream, 200, "OK", OK_BODY).await;
        }
        "/readyz" => {
            if READY.load(Ordering::Acquire)
                && phase_handle.current() < gw_runtime::ShutdownPhase::Phase1AcceptStop
                && backpressure_state.load(Ordering::Acquire) == 0
                && !supervisor_unhealthy.load(Ordering::Acquire)
            {
                let _ = write_response(&mut stream, 200, "OK", OK_BODY).await;
            } else {
                let _ =
                    write_response(&mut stream, 503, "Service Unavailable", NOT_READY_BODY).await;
            }
        }
        path if path.starts_with("/healthz/circuits")
            || path.starts_with("/healthz/key/")
            || path == "/healthz/resilience"
            || path == "/healthz/tunnel"
            || path == "/healthz/strategies"
            || path.starts_with("/healthz/credit/") =>
        {
            // ADR-B2-3 + B2-5: mgmt-token gated admin endpoints. Verify
            // chain mirrors the proxy's request_filter mgmt branch
            // (T3-B); the same RedisJwtBlacklist instance is consulted
            // so revocations applied via the management plane take
            // effect on both surfaces simultaneously.
            let auth_header = parse_authorization(req).unwrap_or("");
            match verify_healthz_request(auth_header, &store, &mgmt_blacklist).await {
                Ok(_scope) => {
                    let body = build_healthz_body(path, &state);
                    let _ = write_json_response(&mut stream, 200, "OK", body.as_bytes()).await;
                }
                Err(HealthzAuthError::Missing) => {
                    let _ = write_response(
                        &mut stream,
                        401,
                        "Unauthorized",
                        gw_core::error::codes::GW_AUTH_MGMT_TOKEN_MISSING.as_bytes(),
                    )
                    .await;
                }
                Err(HealthzAuthError::Invalid) => {
                    let _ = write_response(
                        &mut stream,
                        401,
                        "Unauthorized",
                        gw_core::error::codes::GW_AUTH_MGMT_TOKEN_INVALID.as_bytes(),
                    )
                    .await;
                }
                Err(HealthzAuthError::Expired) => {
                    let _ = write_response(
                        &mut stream,
                        401,
                        "Unauthorized",
                        gw_core::error::codes::GW_AUTH_MGMT_TOKEN_EXPIRED.as_bytes(),
                    )
                    .await;
                }
                Err(HealthzAuthError::Disabled) => {
                    let _ = write_response(
                        &mut stream,
                        401,
                        "Unauthorized",
                        gw_core::error::codes::GW_AUTH_MGMT_TOKEN_DISABLED.as_bytes(),
                    )
                    .await;
                }
                Err(HealthzAuthError::Revoked) => {
                    let _ = write_response(
                        &mut stream,
                        401,
                        "Unauthorized",
                        gw_core::error::codes::GW_AUTH_MGMT_TOKEN_REVOKED.as_bytes(),
                    )
                    .await;
                }
                Err(HealthzAuthError::ScopeDenied) => {
                    let _ = write_response(
                        &mut stream,
                        403,
                        "Forbidden",
                        gw_core::error::codes::GW_AUTH_MGMT_TOKEN_SCOPE_DENIED.as_bytes(),
                    )
                    .await;
                }
            }
        }
        _ => {
            let _ = write_response(&mut stream, 404, "Not Found", b"").await;
        }
    }

    let _ = stream.shutdown().await;
}

async fn handle_aether_upgrade(
    stream: TcpStream,
    req: &[u8],
    state: &HealthState,
) -> anyhow::Result<()> {
    let Some(listener) = state.aether_listener.as_ref().cloned() else {
        return Ok(());
    };
    let headers = parse_headers(req)?;
    let authorized = match listener.authorize(&headers) {
        Ok(v) => v,
        Err(err) => {
            let mut stream = stream;
            let status = listener_error_status(&err);
            write_response(
                &mut stream,
                status,
                listener_error_reason(status),
                err.to_string().as_bytes(),
            )
            .await?;
            return Ok(());
        }
    };
    let permit = match listener.try_acquire_permit() {
        Ok(permit) => permit,
        Err(err) => {
            let mut stream = stream;
            write_response(
                &mut stream,
                503,
                "Service Unavailable",
                err.to_string().as_bytes(),
            )
            .await?;
            return Ok(());
        }
    };

    let node_id = authorized.node_view.id.clone();
    let max_streams = authorized.max_streams;
    let ws_stream = PrebufferedStream::new(req.to_vec(), stream);
    let ws = tokio_tungstenite::accept_hdr_async(
        ws_stream,
        move |request: &Request, response: Response| {
            validate_ws_headers(request.headers(), &headers)?;
            Ok(response)
        },
    )
    .await;
    let ws = match ws {
        Ok(ws) => ws,
        Err(WsError::Http(response)) => {
            tracing::debug!(target: "gw_gateway::health", status = %response.status(), "aether websocket rejected");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };

    let cfg = gw_tunnel_client::aether::transport::TransportConfig {
        heartbeat_interval: Duration::from_secs(30),
        missed_heartbeat_threshold: 3,
        max_streams_per_conn: max_streams,
    };
    let transport = gw_tunnel_client::aether::transport::WsTransport::attach(
        cfg,
        authorized.node_view,
        ws,
        state.aether_heartbeat_sink.clone(),
    )
    .await?;
    let transport = Arc::new(transport);
    listener
        .publish(
            node_id,
            transport.clone() as Arc<dyn gw_tunnel_client::Transport>,
        )
        .await;
    tokio::spawn(async move {
        while transport.alive() {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        drop(permit);
    });
    Ok(())
}

const AETHER_HEARTBEAT_STREAM: &str = "events:aether_heartbeat";
const AETHER_HEARTBEAT_MAXLEN: u64 = 50_000;

#[derive(Clone)]
pub struct RedisHeartbeatSink {
    pool: fred::clients::RedisPool,
}

impl RedisHeartbeatSink {
    pub fn new(pool: fred::clients::RedisPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl gw_tunnel_client::aether::redis_sink::HeartbeatSink for RedisHeartbeatSink {
    async fn push(&self, event: gw_tunnel_client::aether::redis_sink::HeartbeatEvent) {
        let client = self.pool.next();
        let fields = aether_heartbeat_fields(&event);
        let cap = XCap::try_from((
            XCapKind::MaxLen,
            XCapTrim::AlmostExact,
            AETHER_HEARTBEAT_MAXLEN,
        ))
        .expect("valid Aether heartbeat stream cap");
        if let Err(err) = client
            .xadd::<(), _, _, _, _>(AETHER_HEARTBEAT_STREAM, false, cap, "*", fields)
            .await
        {
            tracing::warn!(
                target: "gw_gateway::aether",
                error = %err,
                "events:aether_heartbeat XADD failed"
            );
        }
    }
}

fn aether_heartbeat_fields(
    event: &gw_tunnel_client::aether::redis_sink::HeartbeatEvent,
) -> Vec<(&'static str, String)> {
    vec![
        ("node_id", event.node_id.clone()),
        ("heartbeat_session_id", event.heartbeat_session_id.clone()),
        ("heartbeat_id", event.heartbeat_id.to_string()),
        ("received_at_ms", event.received_at_ms.to_string()),
        ("active_connections", event.active_connections.to_string()),
        ("total_requests", event.total_requests.to_string()),
        ("avg_latency_ms", event.avg_latency_ms.to_string()),
        ("failed_requests", event.failed_requests.to_string()),
        ("dns_failures", event.dns_failures.to_string()),
        ("stream_errors", event.stream_errors.to_string()),
        ("proxy_version", event.proxy_version.clone()),
    ]
}

fn parse_headers(req: &[u8]) -> anyhow::Result<http::HeaderMap> {
    let text = std::str::from_utf8(req)?;
    let mut headers = http::HeaderMap::new();
    for line in text.split('\n').skip(1) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = http::HeaderName::from_bytes(name.trim().as_bytes())?;
        let value = http::HeaderValue::from_str(value.trim())?;
        headers.insert(name, value);
    }
    Ok(headers)
}

fn validate_ws_headers(
    actual: &http::HeaderMap,
    authorized: &http::HeaderMap,
) -> Result<(), http::Response<Option<String>>> {
    let authorized_node = authorized.get("x-node-id");
    if actual.get("x-node-id") != authorized_node {
        return Err(http::Response::builder()
            .status(http::StatusCode::BAD_REQUEST)
            .body(Some("node id changed during websocket upgrade".to_string()))
            .unwrap());
    }
    Ok(())
}

struct PrebufferedStream {
    prebuffer: bytes::Bytes,
    stream: TcpStream,
}

impl PrebufferedStream {
    fn new(prebuffer: Vec<u8>, stream: TcpStream) -> Self {
        Self {
            prebuffer: bytes::Bytes::from(prebuffer),
            stream,
        }
    }
}

impl AsyncRead for PrebufferedStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if !self.prebuffer.is_empty() {
            let n = self.prebuffer.len().min(buf.remaining());
            buf.put_slice(&self.prebuffer[..n]);
            self.prebuffer.advance(n);
            return Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut self.stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for PrebufferedStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.stream).poll_write(cx, data)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

fn listener_error_status(err: &gw_tunnel_client::aether::listener::ListenerError) -> u16 {
    use gw_tunnel_client::aether::listener::ListenerError;
    match err {
        ListenerError::UnknownNode => 404,
        ListenerError::Saturated => 503,
        ListenerError::Unauthorized | ListenerError::BadToken => 401,
        ListenerError::WrongProtocol
        | ListenerError::MissingHeader(_)
        | ListenerError::MalformedHeader(_) => 400,
    }
}

fn listener_error_reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

/// Internal mgmt-verify outcome for `/healthz/*`. Mirrors the proxy
/// `request_filter` mgmt branch (T3-B) so the same revocation /
/// expiry semantics apply on both surfaces.
enum HealthzAuthError {
    Missing,
    Invalid,
    Expired,
    Disabled,
    Revoked,
    ScopeDenied,
}

async fn verify_healthz_request(
    auth_header: &str,
    store: &Arc<gw_store::EntityStore>,
    blacklist: &Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
) -> Result<gw_core::entity::MgmtScope, HealthzAuthError> {
    let parsed =
        gw_auth::mgmt::parse_mgmt_authorization(auth_header).ok_or(HealthzAuthError::Missing)?;
    let parts =
        gw_auth::mgmt::split_mgmt_token_payload(&parsed.token).ok_or(HealthzAuthError::Invalid)?;

    let entry = store
        .mgmt_token_or_load(parts.token_id)
        .await
        .ok_or(HealthzAuthError::Invalid)?;

    if blacklist
        .check_mgmt(
            parts.token_id,
            gw_auth::jwt_blacklist::BlacklistFailMode::FailClosed,
        )
        .await
    {
        return Err(HealthzAuthError::Revoked);
    }

    use gw_auth::mgmt::MgmtVerifyOutcome;
    match gw_auth::mgmt::verify_mgmt_token_secret(
        &entry,
        parts.secret,
        gw_core::entity::MgmtScope::AdminRead,
    ) {
        MgmtVerifyOutcome::Ok => Ok(entry.value.scope),
        MgmtVerifyOutcome::Expired => Err(HealthzAuthError::Expired),
        MgmtVerifyOutcome::Disabled => Err(HealthzAuthError::Disabled),
        MgmtVerifyOutcome::ScopeDenied => Err(HealthzAuthError::ScopeDenied),
        MgmtVerifyOutcome::Invalid | MgmtVerifyOutcome::InvalidFormat => {
            Err(HealthzAuthError::Invalid)
        }
    }
}

fn build_healthz_body(path: &str, state: &HealthState) -> String {
    if path.starts_with("/healthz/circuits") {
        state.key_health.snapshot_json()
    } else if let Some(key_id) = path.strip_prefix("/healthz/key/") {
        state.key_health.key_snapshot_json(key_id)
    } else if path == "/healthz/resilience" {
        state.resilience.snapshot_json()
    } else if path == "/healthz/tunnel" {
        build_tunnel_healthz_body(state)
    } else if path == "/healthz/strategies" {
        build_strategies_body(state)
    } else if let Some(uid) = path.strip_prefix("/healthz/credit/lease/") {
        build_credit_lease_body(uid)
    } else if path == "/healthz/credit/coef-table" {
        build_credit_coef_table_body()
    } else {
        String::from("{}")
    }
}

fn build_tunnel_healthz_body(state: &HealthState) -> String {
    if !state.tunnel_enabled {
        return String::from(r#"{"enabled":false,"nodes":[]}"#);
    }

    {
        if let Some(ref stack) = state.tunnel_stack {
            let nodes = stack.health_snapshot();
            let mut buf = String::from(r#"{"enabled":true,"nodes":["#);
            for (i, n) in nodes.iter().enumerate() {
                if i > 0 {
                    buf.push(',');
                }
                buf.push_str(&format!(
                    r#"{{"node_id":"{}","protocol":"{}","present":{},"alive":{},"last_heartbeat_age_ms":{},"active_streams":{},"transport_present":{},"last_pong_age_seconds":{}}}"#,
                    escape_json_string(&n.node_id),
                    match n.protocol {
                        gw_core::tunnel_protocol::TunnelProtocol::Cnexus => "cnexus",
                        gw_core::tunnel_protocol::TunnelProtocol::Aether => "aether",
                    },
                    n.present,
                    n.alive,
                    match n.last_heartbeat_age_ms {
                        Some(v) => v.to_string(),
                        None => "null".to_owned(),
                    },
                    n.active_streams,
                    n.transport_present,
                    match n.last_pong_age_secs {
                        Some(v) => format!("{v:.1}"),
                        None => "null".to_owned(),
                    },
                ));
            }
            buf.push_str("]}");
            return buf;
        }
    }

    String::from(r#"{"enabled":true,"nodes":[]}"#)
}

fn build_strategies_body(state: &HealthState) -> String {
    let mut buf = String::from("[");
    let mut first = true;
    for entry in state.latency_tracker.by_key_format.iter() {
        let (key_id, format) = entry.key();
        let p50 = match state.latency_tracker.p50_ttft_ms(key_id, *format) {
            Some(v) => v,
            None => continue,
        };
        if !first {
            buf.push(',');
        }
        first = false;
        buf.push_str(&format!(
            r#"{{"key_id":"{}","format":"{}","p50_ttft_ms":{}}}"#,
            key_id,
            format.as_str(),
            p50,
        ));
    }
    buf.push(']');
    buf
}

fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// `/healthz/credit/lease/<uid>` — returns the current LeaseState for a user,
/// or 404 JSON if the user has no entry in the pool.
///
/// The response body is a JSON object; the HTTP status is embedded in the
/// `write_json_response` call at the call site. Because `build_healthz_body`
/// returns a plain `String`, we encode the 404 case as a sentinel prefix
/// `"404:"` that `handle_connection` can detect. To keep the call-site diff
/// minimal we instead return a two-tuple via a dedicated helper called
/// directly from `handle_connection` — but that would require restructuring
/// the existing single-return `build_healthz_body` contract.
///
/// Simpler approach: embed status in the JSON and always return 200 from the
/// outer handler (consistent with how `/healthz/key/<id>` works — it returns
/// `{}` for unknown keys rather than 404). The operator can inspect the
/// `"found"` field.
fn build_credit_lease_body(uid: &str) -> String {
    use gw_core::ids::UserId;
    use std::sync::atomic::Ordering;

    let key = UserId::from(uid);
    match gw_ledger::LEASE_POOL.get(&key) {
        Some(state) => {
            let mode_str = match state.mode() {
                gw_ledger::Mode::Protected => "protected",
                gw_ledger::Mode::Unprotected => "unprotected",
            };
            let credits = state.credits();
            let expires_at_ms = state.expires_at_ms.load(Ordering::Acquire);
            let coef_table_version = state.coef_table_version.load(Ordering::Acquire);
            let lease_id = state.lease_id.load();
            format!(
                r#"{{"found":true,"user_id":"{uid}","mode":"{mode_str}","credits":{credits},"expires_at_ms":{expires_at_ms},"coef_table_version":{coef_table_version},"lease_id":"{lease_id}"}}"#,
                uid = escape_json_str(uid),
                mode_str = mode_str,
                credits = credits,
                expires_at_ms = expires_at_ms,
                coef_table_version = coef_table_version,
                lease_id = escape_json_str(&lease_id),
            )
        }
        None => format!(
            r#"{{"found":false,"user_id":"{}","error":"no lease for user"}}"#,
            escape_json_str(uid)
        ),
    }
}

/// `/healthz/credit/coef-table` — returns a summary of the currently active
/// coef snapshot: version, model count, and up to 64 model IDs.
fn build_credit_coef_table_body() -> String {
    let snap = gw_ledger::current_coef_snapshot();
    let version = snap.version;
    let model_count = snap.by_model.len();
    let mut models_buf = String::from("[");
    for (i, model_id) in snap.by_model.keys().take(64).enumerate() {
        if i > 0 {
            models_buf.push(',');
        }
        models_buf.push('"');
        models_buf.push_str(&escape_json_str(model_id));
        models_buf.push('"');
    }
    models_buf.push(']');
    format!(r#"{{"version":{version},"model_count":{model_count},"models":{models_buf}}}"#,)
}

/// Escape a string for safe embedding inside a JSON string literal.
/// Handles the characters that would break JSON parsing.
fn escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

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

/// Locate the value of an `Authorization:` header in a raw HTTP/1.1 request
/// buffer. Case-insensitive on the field name; preserves the value
/// verbatim (trimmed of leading whitespace and any trailing `\r`). Returns
/// `None` if the header is absent.
fn parse_authorization(req: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(req).ok()?;
    for line in text.split('\n').skip(1) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let colon = line.find(':')?;
        let (name, value) = line.split_at(colon);
        if name.eq_ignore_ascii_case("authorization") {
            // value still starts with ':' — strip it and any leading space.
            return Some(value.trim_start_matches(':').trim_start());
        }
    }
    None
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

/// JSON-typed variant of [`write_response`] used by the `/healthz/*` admin
/// surface. Identical wire format except for the `Content-Type` header.
async fn write_json_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {code} {reason}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
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

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::tunnel_protocol::TunnelProtocol;
    use gw_tunnel_client::registry::ProxyNodeRegistry;
    use gw_tunnel_client::resolver::ProxyNodeView;
    use secrecy::SecretString;
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    struct StubRegistry;

    impl ProxyNodeRegistry for StubRegistry {
        fn lookup_by_id(&self, node_id: &str) -> Option<ProxyNodeView> {
            (node_id == "n1").then(|| ProxyNodeView {
                id: node_id.to_string(),
                ws_url: "wss://example.test/aether".parse().unwrap(),
                auth_token: Some(SecretString::from("ae_t")),
                enabled: true,
                latency_ms: 0,
                allowed_provider_ids: Vec::new(),
                allowed_vendor_key_ids: Vec::new(),
                tunnel_protocol: TunnelProtocol::Aether,
            })
        }

        fn verify_auth_token(&self, node_id: &str, candidate: &str) -> bool {
            node_id == "n1" && candidate == "ae_t"
        }
    }

    #[test]
    fn parse_headers_captures_aether_upgrade_headers() {
        let req = b"GET /api/internal/aether-tunnel HTTP/1.1\r\nHost: gateway\r\nAuthorization: Bearer ae_t\r\nX-Node-Id: n1\r\nX-Node-Name: node\r\nX-Tunnel-Max-Streams: 64\r\n\r\n";

        let headers = parse_headers(req).unwrap();

        assert_eq!(headers["authorization"], "Bearer ae_t");
        assert_eq!(headers["x-node-id"], "n1");
        assert_eq!(headers["x-tunnel-max-streams"], "64");
    }

    #[test]
    fn listener_errors_map_to_http_statuses() {
        use gw_tunnel_client::aether::listener::ListenerError;

        assert_eq!(listener_error_status(&ListenerError::Unauthorized), 401);
        assert_eq!(listener_error_status(&ListenerError::BadToken), 401);
        assert_eq!(listener_error_status(&ListenerError::UnknownNode), 404);
        assert_eq!(listener_error_status(&ListenerError::Saturated), 503);
        assert_eq!(
            listener_error_status(&ListenerError::MalformedHeader("X-Node-Id")),
            400
        );
    }

    #[tokio::test]
    async fn prebuffered_stream_replays_already_read_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream.write_all(b"tail").await.unwrap();
        });
        let (stream, _) = listener.accept().await.unwrap();
        let mut replay = PrebufferedStream::new(b"head-".to_vec(), stream);
        let mut out = Vec::new();
        replay.read_to_end(&mut out).await.unwrap();
        client.await.unwrap();

        assert_eq!(out, b"head-tail");
    }

    #[tokio::test]
    async fn aether_route_upgrades_and_publishes_transport() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (attach_tx, mut attach_rx) =
            tokio::sync::mpsc::channel::<(String, Arc<dyn gw_tunnel_client::Transport>)>(1);
        let aether_listener = Arc::new(gw_tunnel_client::aether::listener::InboundListener::new(
            Arc::new(StubRegistry),
            attach_tx,
        ));
        let state = Arc::new(HealthState {
            key_health: Arc::new(gw_health::KeyHealthStore::default()),
            resilience: Arc::new(gw_proxy::resilience::ResilienceState::default()),
            latency_tracker: Arc::new(gw_routing::LatencyTracker::new()),
            tunnel_enabled: true,
            tunnel_stack: None,
            aether_listener: Some(aether_listener),
            aether_listener_path: "/api/internal/aether-tunnel".to_string(),
            aether_heartbeat_sink: Arc::new(gw_tunnel_client::aether::redis_sink::NoopSink),
        });
        let store = Arc::new(gw_store::EntityStore::new());
        let blacklist = Arc::new(dummy_blacklist());
        let phase = gw_runtime::PhaseHandle::detached_for_test();
        let backpressure = Arc::new(AtomicU8::new(0));
        let unhealthy = Arc::new(AtomicBool::new(false));

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connection(
                stream,
                state,
                store,
                blacklist,
                phase,
                backpressure,
                unhealthy,
            )
            .await;
        });

        let mut req = format!("ws://{addr}/api/internal/aether-tunnel")
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "authorization",
            http::HeaderValue::from_static("Bearer ae_t"),
        );
        req.headers_mut()
            .insert("x-node-id", http::HeaderValue::from_static("n1"));
        req.headers_mut().insert(
            "x-node-name",
            http::HeaderValue::from_static("aether-node-1"),
        );
        req.headers_mut()
            .insert("x-tunnel-max-streams", http::HeaderValue::from_static("64"));

        let (ws, response) = tokio_tungstenite::connect_async(req).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::SWITCHING_PROTOCOLS);
        let (node_id, transport) = tokio::time::timeout(Duration::from_secs(2), attach_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(node_id, "n1");
        assert_eq!(transport.tunnel_protocol(), TunnelProtocol::Aether);
        drop(ws);
        server.await.unwrap();
    }

    #[test]
    fn aether_heartbeat_fields_match_go_consumer_schema() {
        let event = gw_tunnel_client::aether::redis_sink::HeartbeatEvent {
            node_id: "n1".to_string(),
            heartbeat_session_id: "s1".to_string(),
            heartbeat_id: 7,
            received_at_ms: 123,
            active_connections: 2,
            total_requests: 3,
            avg_latency_ms: 4.5,
            failed_requests: 6,
            dns_failures: 8,
            stream_errors: 9,
            proxy_version: "v0.0.8".to_string(),
        };

        let fields = aether_heartbeat_fields(&event);

        assert_eq!(
            fields.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec![
                "node_id",
                "heartbeat_session_id",
                "heartbeat_id",
                "received_at_ms",
                "active_connections",
                "total_requests",
                "avg_latency_ms",
                "failed_requests",
                "dns_failures",
                "stream_errors",
                "proxy_version",
            ]
        );
        assert_eq!(fields[2].1, "7");
        assert_eq!(fields[6].1, "4.5");
    }

    fn dummy_blacklist() -> gw_auth::jwt_blacklist::RedisJwtBlacklist {
        let cfg = fred::types::RedisConfig::from_url("redis://127.0.0.1:6379").unwrap();
        let pool = fred::clients::RedisPool::new(
            cfg,
            Some(fred::types::PerformanceConfig::default()),
            Some(fred::types::ConnectionConfig::default()),
            Some(fred::types::ReconnectPolicy::default()),
            1,
        )
        .unwrap();
        gw_auth::jwt_blacklist::RedisJwtBlacklist::new(pool)
    }
}
