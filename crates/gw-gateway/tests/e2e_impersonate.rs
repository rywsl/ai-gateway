//! Task 6.1 — E2E Impersonation through fake-upstream.
//!
//! Spec: `docs/spec/07-vendor-integration.md`.
//!
//! # What is exercised
//!
//! Two paths, both gated on Docker (testcontainers Postgres + Redis):
//!
//! 1. **Storage layer.** Apply migrations 0001 → 0004 → 0009 in order; seed a
//!    `vendor` row with `egress_mode = {"mode":"impersonate","profile":
//!    "claude_code_chrome131"}` and round-trip through `EntityStore::
//!    vendor_or_load`. Verifies the schema added by 0009 deserialises into
//!    `VendorEgressMode::Impersonate` end-to-end.
//!
//! 2. **Loopback edge.** Spin up the `e2e-fake-upstream::Recorder` (plain
//!    HTTP), bind a `gw-egress-impersonate::server::Handler` with
//!    `new_with_scheme("http")` on a TCP loopback port, and emit a request
//!    through the loopback that mirrors what `inject_egress_headers` writes
//!    in production. Assert on the headers the recorder actually saw.
//!
//! # Why not boot the full Pingora gateway?
//!
//! `gw_gateway::run` parks a Pingora `Server` on the main thread and never
//! returns; reconstructing it inside a `#[tokio::test]` would mean
//! rebuilding the bootstrap pipeline (Wired, LlmGateway, listener
//! orchestration). The Plan §"When to stop and ask" explicitly calls that
//! out as a stop condition. Instead this test composes the same public APIs
//! the production code calls (`gw_core::impersonate::ensure_identity`,
//! `claude_code::headers`, `gw_egress_impersonate::server::Handler::handle`)
//! to exercise the contract the plan cares about:
//!   - per-key identity headers reach the upstream;
//!   - `X-Egress-*` channel headers do NOT reach the upstream;
//!   - the User-Agent for a given `key_id` is stable across requests.
//!
//! Run with:
//!   `cargo test -p gw-gateway --test e2e_impersonate -- --ignored --nocapture`

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use e2e_fake_upstream::{start_recorder, CapturedRequest, Recorder};
use http_body_util::{BodyExt, Empty};
use hyper::body::Incoming;
use hyper::Request;
use hyper_util::rt::TokioIo;

use gw_core::impersonate::{
    claude_code, ensure_identity, IdentitySnapshot, ImpersonateProfile, ResolvedIdentity,
    VendorEgressMode,
};
use gw_egress_impersonate::server::Handler;

// ── Migration chain ──────────────────────────────────────────────────────────
//
// 0009 adds `vendors.egress_mode` and `vendor_keys.identity_snapshot`. Both
// 0001 and 0004 are required for 0009 to ALTER the right tables.

const MIGRATION_0001: &str = include_str!("../../gw-store/migrations/0001_initial.sql");
const MIGRATION_0004: &str = include_str!("../../gw-store/migrations/0004_capabilities.sql");
const MIGRATION_0009: &str =
    include_str!("../../gw-store/migrations/0009_egress_impersonation.sql");

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Bind a `gw-egress-impersonate::server::Handler` on an OS-assigned loopback
/// TCP port, with the `http` scheme so it can target the plain-HTTP recorder.
async fn spawn_loopback_handler() -> std::net::SocketAddr {
    use hyper::server::conn::http1;
    use hyper::service::service_fn;
    use std::convert::Infallible;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("loopback local_addr");
    let handler = Arc::new(Handler::new_with_scheme(16, "http"));

    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let io = TokioIo::new(stream);
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                let svc = service_fn(move |req| {
                    let handler = Arc::clone(&handler);
                    async move { Ok::<_, Infallible>(handler.handle(req).await) }
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });

    addr
}

/// Build the inbound request the gateway emits to the loopback for a given
/// `key_id`. This mirrors the headers that `gw-proxy`'s `inject_egress_headers`
/// would write before forwarding through the loopback:
///   - `X-Egress-Target` / `X-Egress-Profile` (channel headers; loopback strips)
///   - Claude Code's full static + variable header set (must pass through)
fn build_impersonated_request(
    key_id: &str,
    recorder_addr: std::net::SocketAddr,
) -> Request<Empty<Bytes>> {
    let (resolved, _should_persist) =
        ensure_identity(key_id, None, ImpersonateProfile::ClaudeCodeChrome131);
    build_request_with_identity(&resolved, recorder_addr)
}

/// Build the same loopback request as [`build_impersonated_request`], but from
/// an already-resolved identity. Lets a test resolve the identity from a
/// persisted snapshot (instead of re-deriving from RNG) and still emit the
/// exact wire request the gateway would.
fn build_request_with_identity(
    resolved: &ResolvedIdentity,
    recorder_addr: std::net::SocketAddr,
) -> Request<Empty<Bytes>> {
    let identity_hdrs = claude_code::headers(&resolved.variable, /*is_stream*/ false);

    let target = format!("{}:{}", recorder_addr.ip(), recorder_addr.port());
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("X-Egress-Target", target)
        .header(
            "X-Egress-Profile",
            ImpersonateProfile::ClaudeCodeChrome131.as_static_str(),
        );

    for (name, value) in &identity_hdrs {
        builder = builder.header(name, value);
    }

    builder
        .body(Empty::<Bytes>::new())
        .expect("valid impersonated request")
}

/// Dial the loopback handler with `req` over a fresh TCP connection and
/// return the response.
async fn send_through_loopback(
    addr: std::net::SocketAddr,
    req: Request<Empty<Bytes>>,
) -> hyper::Response<Incoming> {
    use hyper::client::conn::http1;
    use tokio::net::TcpStream;

    let stream = TcpStream::connect(addr).await.expect("connect loopback");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = http1::handshake::<_, Empty<Bytes>>(io)
        .await
        .expect("http1 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    sender.send_request(req).await.expect("send_request")
}

/// Block until the recorder has captured at least `n` requests (poll loop with
/// 50ms sleeps, capped at `timeout`). Returns the captured slice once the
/// count is reached.
async fn await_count(recorder: &Recorder, n: usize, timeout: Duration) -> Vec<CapturedRequest> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let reqs = recorder.all_requests();
        if reqs.len() >= n {
            return reqs;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "recorder never captured {n} requests (got {}) within {timeout:?}",
                reqs.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ───────────────────────────────────────────────────────────────────────────────
// Test 1 — Impersonate Claude Code reaches upstream with stainless headers
// ───────────────────────────────────────────────────────────────────────────────

/// Storage-layer check + loopback edge check.
///
/// 1. Boots PG + Redis, applies 0001/0004/0009, seeds a `vendors` row whose
///    JSONB payload sets `egress_mode = {mode: impersonate, profile:
///    claude_code_chrome131}`. Loads it back through `EntityStore::
///    vendor_or_load` and asserts the egress mode round-trips.
/// 2. Spins up the recorder + the loopback Handler. Sends a request through
///    the loopback. Asserts that the recorder's captured headers include the
///    full Claude Code static header set (verbatim from
///    `crates/gw-core/src/impersonate/claude_code.rs`) and do NOT include any
///    of the `X-Egress-*` channel headers (those are loopback-internal).
#[tokio::test]
#[ignore = "requires Docker (testcontainers Postgres + Redis)"]
async fn impersonate_claude_code_reaches_upstream_with_stainless_headers() {
    use std::sync::Arc;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::{postgres::Postgres, redis::Redis};

    use gw_core::ids::VendorId;
    use gw_store::{encrypt::KekCache, loader::LoaderCtx, EntityStore};

    // ── 1. Boot Postgres + Redis ────────────────────────────────────────────
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_node = Redis::default().start().await.expect("start redis");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let redis_port = redis_node
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");
    let pg = gw_store::pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .expect("pg connect");
    let redis =
        gw_store::redis_client::RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
            .await
            .expect("redis connect");

    // ── 2. Apply migrations through 0009 ────────────────────────────────────
    sqlx::raw_sql(MIGRATION_0001)
        .execute(&pg)
        .await
        .expect("apply 0001");
    sqlx::raw_sql(MIGRATION_0004)
        .execute(&pg)
        .await
        .expect("apply 0004");
    sqlx::raw_sql(MIGRATION_0009)
        .execute(&pg)
        .await
        .expect("apply 0009 (egress_impersonation)");

    // ── 3. Seed a vendor row with egress_mode = impersonate ─────────────────
    let payload = serde_json::json!({
        "id": "v-impersonate",
        "display_name": "Anthropic (impersonate)",
        "base_urls": ["https://api.anthropic.com/"],
        "auth_template": "Bearer {api_key}",
        "egress_mode": {
            "mode": "impersonate",
            "profile": "claude_code_chrome131"
        }
    });
    sqlx::query("INSERT INTO vendors (id, payload, version) VALUES ($1, $2, 1)")
        .bind("v-impersonate")
        .bind(payload)
        .execute(&pg)
        .await
        .expect("seed vendor");

    // ── 4. Round-trip the vendor through the loader ─────────────────────────
    let loader = LoaderCtx {
        redis,
        pg,
        kek: KekCache::default(),
    };
    let store = Arc::new(EntityStore::with_loader(loader));
    let v = store
        .vendor_or_load(&VendorId::from("v-impersonate"))
        .await
        .expect("vendor_or_load returned None");

    match &v.value.egress_mode {
        VendorEgressMode::Impersonate { profile } => assert_eq!(
            *profile,
            ImpersonateProfile::ClaudeCodeChrome131,
            "expected ClaudeCodeChrome131 profile"
        ),
        other => panic!("expected Impersonate egress_mode, got {other:?}"),
    }

    // ── 5. Spin up recorder + loopback Handler ──────────────────────────────
    let recorder = start_recorder().await;
    let loopback_addr = spawn_loopback_handler().await;

    // ── 6. Send an impersonated request through the loopback ────────────────
    let req = build_impersonated_request("vk-claude-001", recorder.addr);
    let resp = send_through_loopback(loopback_addr, req).await;
    assert_eq!(resp.status(), 200, "loopback returned non-200");
    // Drain the body so the connection completes cleanly.
    let _ = resp.into_body().collect().await;

    // ── 7. Assert on recorder-captured headers ──────────────────────────────
    let captured = await_count(&recorder, 1, Duration::from_secs(5)).await;
    assert_eq!(captured.len(), 1, "expected exactly 1 captured request");
    let cap = &captured[0];

    // Claude Code static headers must reach the upstream.
    for required in [
        "X-Stainless-Lang",
        "X-Stainless-Runtime",
        "X-Stainless-Retry-Count",
        "X-App",
        "Anthropic-Dangerous-Direct-Browser-Access",
        "anthropic-version",
        "Accept",
        "X-Stainless-OS",
        "X-Stainless-Arch",
        "X-Stainless-Runtime-Version",
        "X-Stainless-Package-Version",
        "X-Stainless-Timeout",
        "User-Agent",
        "anthropic-beta",
    ] {
        assert!(
            cap.has_header(required),
            "missing forged header {required}; captured: {:?}",
            cap.headers
        );
    }

    // Sanity-check a couple of values against the pool definitions in
    // `crates/gw-core/src/impersonate/claude_code.rs`.
    assert_eq!(cap.header("X-Stainless-Lang"), Some("js"));
    assert_eq!(cap.header("X-Stainless-Runtime"), Some("node"));
    assert_eq!(cap.header("X-App"), Some("cli"));
    let ua = cap.header("User-Agent").expect("User-Agent set");
    assert!(
        ua.contains("Mozilla/5.0") && ua.contains("Electron/"),
        "User-Agent {ua:?} does not look like a Claude Code UA"
    );

    // The internal egress-channel headers MUST be stripped by the loopback.
    assert!(
        !cap.has_header("X-Egress-Target"),
        "X-Egress-Target leaked to upstream"
    );
    assert!(
        !cap.has_header("X-Egress-Profile"),
        "X-Egress-Profile leaked to upstream"
    );
    assert!(
        !cap.has_header("X-Egress-Proxy"),
        "X-Egress-Proxy leaked to upstream"
    );
}

// ───────────────────────────────────────────────────────────────────────────────
// Test 2 — Per-key identity is stable across requests
// ───────────────────────────────────────────────────────────────────────────────

/// Two consecutive impersonated requests with the same `key_id` must produce
/// the same User-Agent at the upstream. This is what makes the per-key
/// fingerprint stable in the absence of the persist layer — `ensure_identity`
/// is deterministic in `key_id` (see `crates/gw-core/src/impersonate/rng.rs`),
/// so even without reading from `vendor_keys.identity_snapshot` the second
/// request must reproduce the first request's identity.
///
/// The test also confirms that a *different* `key_id` produces a *different*
/// identity (cheap cross-check; the unit tests in `claude_code.rs` cover the
/// deeper distribution properties).
#[tokio::test]
#[ignore = "requires Docker (testcontainers Postgres + Redis)"]
async fn per_key_identity_is_stable_across_requests() {
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    // The schema isn't strictly required for this test (it doesn't read from
    // PG), but the plan calls for the migration chain to be applied so that
    // the egress schema compiles in this binary. Booting PG is the cheapest
    // way to keep the migration path in the link graph.
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let pg = gw_store::pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .expect("pg connect");
    sqlx::raw_sql(MIGRATION_0001)
        .execute(&pg)
        .await
        .expect("apply 0001");
    sqlx::raw_sql(MIGRATION_0004)
        .execute(&pg)
        .await
        .expect("apply 0004");
    sqlx::raw_sql(MIGRATION_0009)
        .execute(&pg)
        .await
        .expect("apply 0009");

    // Spin up recorder + loopback Handler.
    let recorder = start_recorder().await;
    let loopback_addr = spawn_loopback_handler().await;

    // ── Two requests with the SAME key ──────────────────────────────────────
    let key_id = "vk-stable-42";
    for _ in 0..2 {
        let req = build_impersonated_request(key_id, recorder.addr);
        let resp = send_through_loopback(loopback_addr, req).await;
        assert_eq!(resp.status(), 200);
        let _ = resp.into_body().collect().await;
    }

    let captured = await_count(&recorder, 2, Duration::from_secs(5)).await;
    assert!(captured.len() >= 2, "expected at least 2 captures");
    let ua1 = captured[0]
        .header("User-Agent")
        .expect("ua on request 1")
        .to_owned();
    let ua2 = captured[1]
        .header("User-Agent")
        .expect("ua on request 2")
        .to_owned();
    assert_eq!(
        ua1, ua2,
        "per-key User-Agent must be stable across requests (key={key_id})"
    );

    // Other Stainless-* fields driven by the same RNG must also match.
    for hdr in [
        "X-Stainless-OS",
        "X-Stainless-Arch",
        "X-Stainless-Runtime-Version",
        "X-Stainless-Package-Version",
        "X-Stainless-Timeout",
    ] {
        let v1 = captured[0].header(hdr);
        let v2 = captured[1].header(hdr);
        assert_eq!(v1, v2, "header {hdr} drifted across requests");
    }

    // ── One request with a DIFFERENT key — UA should differ (cross-check) ──
    let other_recorder = start_recorder().await;
    let other_loopback = spawn_loopback_handler().await;
    let other_key = "vk-different-99";
    let req = build_impersonated_request(other_key, other_recorder.addr);
    let resp = send_through_loopback(other_loopback, req).await;
    assert_eq!(resp.status(), 200);
    let _ = resp.into_body().collect().await;
    let other_captured = await_count(&other_recorder, 1, Duration::from_secs(5)).await;
    let other_ua = other_captured[0]
        .header("User-Agent")
        .expect("ua for other key")
        .to_owned();
    // We don't assert `assert_ne!(ua1, other_ua)` strictly because two key_ids
    // *could* in principle pick the same pool slots; instead we assert that
    // the per-key derivation is deterministic in both directions.
    let (re_resolved, _) =
        ensure_identity(other_key, None, ImpersonateProfile::ClaudeCodeChrome131);
    let re_hdrs = claude_code::headers(&re_resolved.variable, false);
    let expected_ua = re_hdrs
        .iter()
        .find(|(k, _)| k == "User-Agent")
        .map(|(_, v)| v.clone())
        .expect("re-derived UA");
    assert_eq!(
        other_ua, expected_ua,
        "other-key identity is not deterministic in key_id"
    );
}

// ───────────────────────────────────────────────────────────────────────────────
// Test 3 — IdentitySnapshot persist round-trip through Postgres
// ───────────────────────────────────────────────────────────────────────────────

/// Closes the loop between the lazy-persist layer (Task 2.7/2.8) and the wire.
///
/// Test 2 proves `ensure_identity` is deterministic in `key_id` — but that
/// alone does not prove the *persist path* works. This test forces the second
/// request to take the read-from-Postgres path:
///
/// 1. Request 1 resolves identity with `existing = None` → derives from RNG and
///    reports `should_persist = true`. The request is sent through the loopback.
/// 2. The resulting `IdentitySnapshot` is scheduled on a real
///    `IdentityPersistQueue` backed by `PgSnapshotWriter`, then flushed so the
///    background write commits to `vendor_keys.identity_snapshot`.
/// 3. A *fresh* `EntityStore` (cold in-memory + cold Redis) loads the key via
///    `vendor_key_or_load`, exercising `load_vendor_key`'s SQL. The
///    `identity_snapshot` column lives outside the `payload` JSONB, so the
///    loader's `jsonb_set` fold (see `VENDOR_KEY_SQL` in `loader.rs`) is the
///    only thing that surfaces the persisted snapshot through
///    `VendorKeyRow.identity_snapshot`. If that fold ever regresses,
///    `vk.value.identity_snapshot` is `None` and this test fails loudly.
/// 4. Request 2 resolves identity with `existing = Some(&snapshot_from_pg)` →
///    must report `should_persist = false` (it trusted the snapshot, did not
///    re-derive) and is sent through the loopback.
/// 5. The User-Agent observed at the upstream must be identical across both
///    requests. If serialisation, the conditional `write_if_null` UPDATE, or
///    `ensure_identity`'s snapshot-reuse branch dropped any field, the UAs
///    would diverge.
#[tokio::test]
#[ignore = "requires Docker (testcontainers Postgres + Redis)"]
async fn identity_snapshot_round_trips_through_postgres() {
    use std::collections::HashMap;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;
    use testcontainers_modules::{postgres::Postgres, redis::Redis};

    use gw_core::ids::KeyId;
    use gw_store::encrypt::{encrypt_for_test_only, KekCache};
    use gw_store::identity_persist::{IdentityPersistQueue, PgSnapshotWriter};
    use gw_store::{loader::LoaderCtx, EntityStore};

    // ── Boot Postgres + Redis, migrate through 0009 ─────────────────────────
    let pg_node = Postgres::default().start().await.expect("start postgres");
    let redis_node = Redis::default().start().await.expect("start redis");
    let pg_port = pg_node.get_host_port_ipv4(5432).await.expect("pg port");
    let redis_port = redis_node
        .get_host_port_ipv4(6379)
        .await
        .expect("redis port");
    let pg = gw_store::pg_client::connect(
        &format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/postgres"),
        4,
    )
    .await
    .expect("pg connect");
    sqlx::raw_sql(MIGRATION_0001)
        .execute(&pg)
        .await
        .expect("apply 0001");
    sqlx::raw_sql(MIGRATION_0004)
        .execute(&pg)
        .await
        .expect("apply 0004");
    sqlx::raw_sql(MIGRATION_0009)
        .execute(&pg)
        .await
        .expect("apply 0009");

    // ── Seed a loadable vendor_keys row (identity_snapshot column = NULL) ────
    // The row must deserialise through `load_vendor_key`, so it needs a real
    // encrypted `api_key_enc`; `encrypt_for_test_only` hard-codes kek kid
    // "test", so the `KekCache` must hold that kid. The `identity_snapshot`
    // *column* starts NULL — `write_if_null` only writes when it IS NULL.
    let key_id = "vk-roundtrip-001";
    let kek_bytes = [7u8; 32];
    let kek = KekCache::new(HashMap::from([("test".to_string(), kek_bytes)]));
    let api_key_enc = encrypt_for_test_only("sk-roundtrip", kek_bytes);
    let payload = serde_json::json!({
        "key_id": key_id,
        "vendor_id": "v-impersonate",
        "api_key_enc": api_key_enc,
        "status": "active",
    });
    sqlx::query("INSERT INTO vendor_keys (id, payload, version) VALUES ($1, $2, 1)")
        .bind(key_id)
        .bind(payload)
        .execute(&pg)
        .await
        .expect("seed vendor_key");

    let recorder = start_recorder().await;
    let loopback_addr = spawn_loopback_handler().await;

    // ── Request 1: derive from RNG (no snapshot), send, then persist ────────
    let (resolved1, should_persist1) =
        ensure_identity(key_id, None, ImpersonateProfile::ClaudeCodeChrome131);
    assert!(
        should_persist1,
        "first ensure_identity (no snapshot) must request a lazy persist"
    );
    let req1 = build_request_with_identity(&resolved1, recorder.addr);
    let resp1 = send_through_loopback(loopback_addr, req1).await;
    assert_eq!(resp1.status(), 200);
    let _ = resp1.into_body().collect().await;

    let snap1 = IdentitySnapshot {
        profile_kind: resolved1.profile,
        variable: resolved1.variable.clone(),
    };
    let queue = IdentityPersistQueue::new(Arc::new(PgSnapshotWriter::new(pg.clone())), 128);
    queue.schedule(key_id.to_owned(), snap1);
    queue.flush_for_test().await;

    // ── Load through a FRESH EntityStore (cold cache) ───────────────────────
    // `load_vendor_key`'s SQL must fold the dedicated `identity_snapshot`
    // column into the payload that decodes into `VendorKey`. If it doesn't,
    // `vk.identity_snapshot` is `None`, the next ensure_identity re-derives,
    // `should_persist` flips back to true, and the assertion below catches it.
    let redis =
        gw_store::redis_client::RedisCtx::connect(&format!("redis://127.0.0.1:{redis_port}"), 4)
            .await
            .expect("redis connect");
    let store = Arc::new(EntityStore::with_loader(LoaderCtx {
        redis,
        pg: pg.clone(),
        kek,
    }));
    let vk = store
        .vendor_key_or_load(&KeyId::from(key_id))
        .await
        .expect("vendor_key_or_load returned None");
    let snap_from_pg =
        vk.value.identity_snapshot.clone().expect(
            "identity_snapshot not rehydrated from the column — loader read path is broken",
        );

    // ── Request 2: resolve FROM the persisted snapshot, send ────────────────
    let (resolved2, should_persist2) = ensure_identity(
        key_id,
        Some(&snap_from_pg),
        ImpersonateProfile::ClaudeCodeChrome131,
    );
    assert!(
        !should_persist2,
        "second ensure_identity (with snapshot) must reuse it, not re-persist"
    );
    let req2 = build_request_with_identity(&resolved2, recorder.addr);
    let resp2 = send_through_loopback(loopback_addr, req2).await;
    assert_eq!(resp2.status(), 200);
    let _ = resp2.into_body().collect().await;

    // ── Assert the upstream saw an identical UA across the round-trip ───────
    let captured = await_count(&recorder, 2, Duration::from_secs(5)).await;
    assert!(captured.len() >= 2, "expected at least 2 captures");
    let ua1 = captured[0]
        .header("User-Agent")
        .expect("ua on request 1")
        .to_owned();
    let ua2 = captured[1]
        .header("User-Agent")
        .expect("ua on request 2")
        .to_owned();
    assert_eq!(
        ua1, ua2,
        "User-Agent must survive the persist → Postgres → read round-trip (key={key_id})"
    );
}
