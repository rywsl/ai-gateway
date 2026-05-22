# 2026-05-22 — E2E Skip Closure Plan

> **Source spec**: [`docs/superpowers/specs/2026-05-22-e2e-skip-closure-design.md`](../specs/2026-05-22-e2e-skip-closure-design.md). Read it first — this plan executes that spec's ADRs.
>
> **Branch**: `feature/e2e-skip-closure` (create from `origin/main` before Wave 1).
>
> **Outcome**: `make e2e` reports `30 passed, 0 skipped`.

## Wave overview

```
Wave 1  Gemini stream (ADR-S1, S2)        [SERIAL, one subagent]
Wave 2  Sticky binding + TTL (ADR-S3, S4) [SERIAL, one subagent — code touches PickCtx + ctx + filters + config]
Wave 3  Fake-upstream TLS (ADR-S5)        [SERIAL, one subagent — TLS listener + test rewrite]
Wave 4  Final sweep                       [SERIAL — verify make e2e green, docs/architecture.md prune]
```

Why all serial: each wave is bounded in scope (~2 crates) and the test verifies independently. Parallelism here would cost more in merge conflicts (especially Wave 2 ↔ Wave 3 on `tests/e2e/docker-compose.yml`) than it saves in latency. Waves 1 and 2 are also a useful learning order: ADR-S1 establishes the "small additive trait method" precedent that ADR-S3's `ctx.conversation_id` field follows.

## Task index

| ID | Wave | Title | ADR | Touches |
|----|------|-------|-----|---------|
| W1-A | 1 | Gemini stream path-aware hook | S1, S2 | `gw-codec`, `gw-proxy::filters::request`, `e2e-fake-upstream::routes` (regex), `tests/e2e/client/scenarios/test_happy_streaming.py` |
| W2-A | 2 | Sticky binding via X-Conversation-Id + configurable TTL | S3, S4 | `gw-proxy::ctx`, `gw-proxy::filters::early`, `gw-proxy::filters::upstream`, `gw-routing::strategy::set`, `gw-gateway::config`, `gw-gateway::lib`, `config/default.toml`, `tests/e2e/docker-compose.yml`, `tests/e2e/client/scenarios/test_routing_retry.py` |
| W3-A | 3 | Fake-upstream TLS listener | S5 | `crates/e2e-fake-upstream/Cargo.toml`, `crates/e2e-fake-upstream/src/{lib,server,tls}.rs`, `tests/e2e/docker-compose.yml`, `tests/e2e/client/scenarios/test_routing_retry.py` |
| W4-A | 4 | Architecture doc sync + final e2e sweep | (shared) | `docs/architecture.md`, `CLAUDE.md` cross-check |

---

## Conventions (apply to all tasks)

1. **Path-restricted commits**: each task lists the exact `git add <path>` it must use. Never `git add .`. If a subagent finds a file outside its allowlist needing changes, escalate (`NEEDS_CONTEXT`) rather than expanding scope.
2. **One subagent = one commit per task.** The commit subject is given below verbatim. Body includes a `Tests` section listing the un-skipped e2e tests.
3. **Verify before commit**:
   - `cargo build -p <crate>` clean for every touched crate.
   - `cargo test --workspace` passing (catches unit-level regressions in codec roundtrip / sticky_ttl / etc.).
   - `cargo clippy --workspace --all-targets -- -D warnings` clean.
   - `bash scripts/check-invariants.sh` clean.
   - `make e2e` runs to completion; the un-skipped test(s) for this wave PASS, all previously-passing tests still PASS.
4. **Wave boundary**: do not start Wave N+1 until Wave N is committed and `make e2e` is green with that wave's tests un-skipped.
5. **Cargo.lock drift**: only W3-A adds dependencies (`tokio-rustls`, `rcgen`); commit `Cargo.lock` alongside.
6. **No new docs comments** beyond what the spec requires. Use the existing `// ADR-S1:` style references where a non-obvious decision is being made (e.g. in `filters/request.rs` where we OR `path_implies_stream` into `ir.stream`).

---

# Wave 1 — Gemini stream path-aware hook (ADR-S1, ADR-S2)

## W1-A — Gemini codec `path_implies_stream` + request_filter wiring + un-skip

**Goal**: make `test_happy_streaming.py::test_gemini_stream_happy` pass against the live stack.

**Files touched** (allowlist):

- `crates/gw-codec/src/trait_def.rs`
- `crates/gw-codec/src/gemini/codec.rs`
- `crates/gw-codec/src/openai/codec.rs` (compile-only: confirms default trait impl applies)
- `crates/gw-codec/src/anthropic/codec.rs` (compile-only: same)
- `crates/gw-codec/src/any.rs`
- `crates/gw-proxy/src/filters/request.rs`
- `tests/e2e/client/scenarios/test_happy_streaming.py`

**Steps**:

1. **Trait method**, `crates/gw-codec/src/trait_def.rs`:

   ```rust
   pub trait ProtocolCodec: Send + Sync + 'static {
       // ... existing methods unchanged ...

       /// Some protocols carry the "this is a streaming request" signal in
       /// the URL path (e.g. Gemini `:streamGenerateContent`) rather than
       /// in the request body. Codecs that do this override this method;
       /// the proxy ORs the result into `ir.stream` after `parse_request`.
       ///
       /// Default returns false. Implementations MUST be branch-free.
       fn path_implies_stream(path: &str) -> bool
       where
           Self: Sized,
       {
           let _ = path;
           false
       }
   }
   ```

2. **Gemini override**, `crates/gw-codec/src/gemini/codec.rs`:

   ```rust
   impl ProtocolCodec for Gemini {
       // ... existing impls ...

       #[inline]
       fn path_implies_stream(path: &str) -> bool {
           path.contains(":streamGenerateContent")
       }
   }
   ```

3. **Dispatch**, `crates/gw-codec/src/any.rs`. Add a method that mirrors `detect_request`'s shape:

   ```rust
   impl AnyCodec {
       // ... existing methods ...

       pub fn path_implies_stream(&self, path: &str) -> bool {
           match self {
               AnyCodec::OpenAi(_) => OpenAi::path_implies_stream(path),
               AnyCodec::Anthropic(_) => Anthropic::path_implies_stream(path),
               AnyCodec::Gemini(_) => Gemini::path_implies_stream(path),
           }
       }
   }
   ```

4. **request_filter wiring**, `crates/gw-proxy/src/filters/request.rs`. After the existing `parse_request` block (around line 492–504), look up the path from the session and OR into `is_stream`:

   ```rust
   let (model_name, parsed_stream, est_input_tokens) = match codec.parse_request(&body_bytes) {
       Ok(ir) => (
           ModelName::from(ir.model.as_ref()),
           ir.stream,
           codec.estimate_input_tokens(&ir),
       ),
       Err(err) => {
           emit_and_bail(session, ctx, &err, &request_id).await?;
           unreachable!()
       }
   };
   // ADR-S1: Gemini's streaming signal lives in the URL path, not the body.
   let path_str = session.req_header().uri.path();
   let is_stream = parsed_stream || codec.path_implies_stream(path_str);
   ```

   Rest of the file unchanged.

5. **Un-skip the test**, `tests/e2e/client/scenarios/test_happy_streaming.py`:

   - Delete the `pytest.skip(...)` call at line 275.
   - Delete the multi-paragraph docstring explaining the gap (keep a one-line "Gemini streaming happy path.").
   - Remove the `# Retained for re-enable once...` `# noqa: F401` comment on `make_gemini_client` (line 35).
   - Verify the test runs `_gemini_sse_events()` against the existing fake_upstream scenario shape — the default `path_regex: "^/v1/chat/completions$"` does NOT match Gemini, so the scenario installed by the test (which uses `path_regex: "^/v1beta/models/.*:streamGenerateContent"` or similar) is the one to use.
   - If the existing inline scenario uses `path_regex: "^/v1/chat/completions$"`, change it to `"^/v1beta/models/[^/]+:streamGenerateContent"`. Check the file before editing — only change what's there.

6. **Run**:

   ```bash
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   bash scripts/check-invariants.sh
   make e2e
   ```

   Assert `test_gemini_stream_happy PASSED`. All other tests remain at their previous status.

7. **Commit** (`git add` only the allowlisted paths):

   ```
   gap(codec): close Gemini stream skip via path_implies_stream hook (W1-A)

   ProtocolCodec gains a default-providing `path_implies_stream(path)`
   method; Gemini overrides to detect `:streamGenerateContent`. The
   request_filter ORs the result into `ir.stream` after parsing the
   body, so Gemini streaming flows through without the codec needing
   the path in `parse_request`.

   Closes the only Gemini e2e skip; OpenAI and Anthropic inherit the
   default `false` and are byte-identical.

   Tests:
   - scenarios/test_happy_streaming.py::test_gemini_stream_happy
   ```

**Acceptance**: `make e2e` reports `28 passed, 2 skipped` (Wave 1 closes one of three).

---

# Wave 2 — Sticky binding + configurable TTL (ADR-S3, ADR-S4)

## W2-A — `X-Conversation-Id` plumbing + `routing.sticky_ttl_secs` config + un-skip

**Goal**: make `test_routing_retry.py::test_sticky_strategy_binds_user_to_same_key_then_expires` pass against the live stack with a 2-second TTL.

**Files touched** (allowlist):

- `crates/gw-proxy/src/ctx.rs`
- `crates/gw-proxy/src/filters/early.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-routing/src/strategy/set.rs`
- `crates/gw-routing/tests/strategy_set.rs` (compile-only)
- `crates/gw-routing/tests/sticky_ttl.rs` (compile-only — production default stays 1800)
- `crates/gw-gateway/src/config.rs`
- `crates/gw-gateway/src/lib.rs`
- `crates/gw-gateway/tests/e2e_full_flow.rs` (compile-only)
- `config/default.toml`
- `tests/e2e/docker-compose.yml`
- `tests/e2e/client/scenarios/test_routing_retry.py`

**Steps**:

1. **ReqCtx field**, `crates/gw-proxy/src/ctx.rs`. Add to the struct:

   ```rust
   pub conversation_id: Option<String>,
   ```

   Initialize to `None` in `new_ctx()`.

2. **early_request_filter read**, `crates/gw-proxy/src/filters/early.rs`. After protocol detection (around step 4), before the function returns:

   ```rust
   // ADR-S3: sticky-strategy binding key. Header is case-insensitive
   // per HTTP semantics. Absent → strategy falls back to "first available".
   ctx.conversation_id = session
       .req_header()
       .headers
       .get("x-conversation-id")
       .and_then(|v| v.to_str().ok())
       .map(|s| s.to_owned());
   ```

3. **upstream.rs plumb into PickCtx**, `crates/gw-proxy/src/filters/upstream.rs` line 220:

   ```rust
   let pick_ctx = PickCtx {
       user_id: &perm.effective_user_id,
       model: &model,
       conversation_id: ctx.conversation_id.as_deref(),
       message_hash: None,
       api_format: client_codec_kind,
   };
   ```

4. **StrategySet TTL parameter**, `crates/gw-routing/src/strategy/set.rs`:

   ```rust
   impl StrategySet {
       pub fn new(sticky_ttl: Duration) -> Self {
           Self {
               // ... other fields with their existing defaults ...
               sticky: StrategyAny::Sticky(Sticky::new_with_ttl(sticky_ttl, DEFAULT_STICKY_CAP)),
           }
       }

       pub fn new_default() -> Self {
           Self::new(Duration::from_secs(DEFAULT_STICKY_TTL_SECS))
       }

       // `placeholder` stays as `Self::new_default`; do not delete.
   }
   ```

   Update existing call sites (`gw-gateway::lib`, tests under `crates/gw-routing/tests/strategy_set.rs`) — most stay on `new_default()`. The wiring site in `lib.rs` switches to `new(configured_ttl)`.

5. **Config knob**, `crates/gw-gateway/src/config.rs`:

   ```rust
   #[derive(Debug, Deserialize, Default)]
   pub struct RoutingCfg {
       #[serde(default = "default_sticky_ttl_secs")]
       pub sticky_ttl_secs: u64,
   }

   fn default_sticky_ttl_secs() -> u64 { 1800 }
   ```

   Add to `Config` with `#[serde(default)] pub routing: RoutingCfg`.

6. **Wiring**, `crates/gw-gateway/src/lib.rs` line ~320:

   ```rust
   strategies: Arc::new(gw_routing::strategy::set::StrategySet::new(
       Duration::from_secs(cfg.routing.sticky_ttl_secs),
   )),
   ```

7. **Config file**, `config/default.toml`:

   ```toml
   [routing]
   sticky_ttl_secs = 1800
   ```

8. **docker-compose TTL override**, `tests/e2e/docker-compose.yml`:

   ```yaml
   services:
     gateway:
       environment:
         # ... existing vars ...
         GW_ROUTING__STICKY_TTL_SECS: "2"
   ```

9. **Rewrite the e2e test**, `tests/e2e/client/scenarios/test_routing_retry.py::test_sticky_strategy_binds_user_to_same_key_then_expires`. Replace the skip-and-no-rebind half with:

   - Both requests send header `X-Conversation-Id: e2e-conv-1`.
   - Assert both records report the SAME `vendor_key_id` (binding hit).
   - `time.sleep(3.5)` (TTL=2s + slack).
   - Third request with header `X-Conversation-Id: e2e-conv-1` MAY land on a different key — the hash-mod-n rebind over `[vk-stk-a, vk-stk-b]` is deterministic per id, so we cannot assert "different key". Instead assert: third record's `vendor_key_id` is one of the two and metrics `gw_sticky_bind_total` ticked by at least 1 between t=0 and t=3.5s (proves rebind path ran).
   - Delete the trailing `pytest.skip(...)` entirely.

   The `_pair_auth_request_hook` already strips Authorization but does not touch `X-Conversation-Id`, so the SDK calls plus a `default_headers={"x-conversation-id": "..."}` on the openai client is the cleanest way to attach the header. If the openai client doesn't accept `default_headers`, fall back to the `event_hooks` request hook in conftest and add the header there for this test only.

10. **Run**:

    ```bash
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings
    bash scripts/check-invariants.sh
    make e2e
    ```

    Assert `test_sticky_strategy_binds_user_to_same_key_then_expires PASSED`.

11. **Commit**:

    ```
    gap(routing): X-Conversation-Id sticky binding + configurable TTL (W2-A)

    PickCtx.conversation_id now reads from request header
    `X-Conversation-Id`; StrategySet TTL is wired through
    `routing.sticky_ttl_secs` (default 1800s). e2e overrides to 2s so
    the TTL-expiry leg is exercisable in CI.

    Without the header, behaviour is unchanged: Sticky falls back to
    first-available, identical to v1.

    Tests:
    - scenarios/test_routing_retry.py::test_sticky_strategy_binds_user_to_same_key_then_expires
    ```

**Acceptance**: `make e2e` reports `29 passed, 1 skipped` (Wave 2 closes another).

---

# Wave 3 — Fake-upstream TLS listener (ADR-S5)

## W3-A — TLS listener on port 9443 + un-skip TLS handshake test

**Goal**: make `test_routing_retry.py::test_tls_handshake_error_returns_503_no_retry` pass — the gateway's existing TLS classification path is exercised end-to-end.

**Files touched** (allowlist):

- `crates/e2e-fake-upstream/Cargo.toml`
- `crates/e2e-fake-upstream/src/lib.rs`
- `crates/e2e-fake-upstream/src/server.rs`
- `crates/e2e-fake-upstream/src/tls.rs` (new file)
- `Dockerfile.e2e-fake-upstream`
- `tests/e2e/docker-compose.yml`
- `tests/e2e/client/scenarios/test_routing_retry.py`
- `Cargo.lock`

**Steps**:

1. **Dependencies**, `crates/e2e-fake-upstream/Cargo.toml`:

   ```toml
   [dependencies]
   # ... existing ...
   tokio-rustls = "0.26"
   rustls = "0.23"
   rcgen = { version = "0.13", default-features = false, features = ["aws_lc_rs"] }
   rustls-pki-types = "1"
   ```

   Pick versions consistent with anything else in `Cargo.lock` (look up `tokio-rustls`, `rustls` first; if already present at older versions used by pingora, match those to avoid a dual rustls in the dep tree).

2. **TLS module**, `crates/e2e-fake-upstream/src/tls.rs` (new file). Generates a self-signed cert at startup, builds a `tokio_rustls::TlsAcceptor`, accepts connections on `0.0.0.0:9443`, hands the decrypted stream to the same HTTP/1.1 service the plain listener uses:

   ```rust
   //! TLS variant of the fake_upstream listener.
   //!
   //! Self-signed cert generated at startup with `rcgen`. The gateway's
   //! truststore does NOT trust this cert; that's the whole point — pingora's
   //! TLS handshake fails, `fail_to_connect` fires with `ConnectErrorKind::Tls`,
   //! and the gateway emits GW_UPSTREAM_TLS_ERROR.

   use crate::scenario::Scenario;
   use crate::server::handle_business;
   use crate::routes::HitCounters;
   use crate::inject::RefuseCounter;
   use hyper::server::conn::http1;
   use hyper::service::service_fn;
   use hyper_util::rt::TokioIo;
   use rustls::pki_types::{CertificateDer, PrivateKeyDer};
   use rustls::ServerConfig;
   use std::sync::Arc;
   use tokio::net::TcpListener;
   use tokio::sync::RwLock;
   use tokio_rustls::TlsAcceptor;

   pub async fn run_tls(
       addr: &str,
       scenario: Arc<RwLock<Scenario>>,
       counters: Arc<RwLock<HitCounters>>,
       refuse: Arc<RefuseCounter>,
   ) -> anyhow::Result<()> {
       let cert = rcgen::generate_simple_self_signed(vec!["fake-upstream".into()])?;
       let cert_der = CertificateDer::from(cert.cert.der().to_vec());
       let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
           .map_err(|e| anyhow::anyhow!("rustls key from rcgen: {e}"))?;

       let server_cfg = ServerConfig::builder()
           .with_no_client_auth()
           .with_single_cert(vec![cert_der], key_der)?;
       let acceptor = TlsAcceptor::from(Arc::new(server_cfg));
       let listener = TcpListener::bind(addr).await?;

       loop {
           let (sock, _) = listener.accept().await?;
           let acceptor = acceptor.clone();
           let scenario = scenario.clone();
           let counters = counters.clone();
           let refuse = refuse.clone();
           tokio::spawn(async move {
               // Handshake intentionally fails for clients that don't trust
               // our self-signed cert — that's the test path.
               let stream = match acceptor.accept(sock).await {
                   Ok(s) => s,
                   Err(_) => return, // expected for the e2e test
               };
               let io = TokioIo::new(stream);
               let svc = service_fn(move |req| {
                   handle_business(req, scenario.clone(), counters.clone(), refuse.clone())
               });
               let _ = http1::Builder::new().serve_connection(io, svc).await;
           });
       }
   }
   ```

   `handle_business` must be a `pub` function in `server.rs` (factor it out of the existing plain-listener loop). If it isn't already, refactor in this commit — keep the change minimal.

3. **lib.rs `main`/entry**: in `crates/e2e-fake-upstream/src/lib.rs` (or wherever the binary's main is — confirm by reading the existing file), spawn the TLS listener alongside the plain listener:

   ```rust
   tokio::spawn(crate::tls::run_tls("0.0.0.0:9443", scenario.clone(), counters.clone(), refuse.clone()));
   ```

   Sharing `scenario` + `counters` + `refuse` means routes installed via `/admin/scenario` apply to BOTH transports.

4. **Dockerfile expose**, `Dockerfile.e2e-fake-upstream`: add `EXPOSE 9443`.

5. **docker-compose**, `tests/e2e/docker-compose.yml`, `fake-upstream` service:

   ```yaml
   ports:
     - "9000:9000"
     - "9001:9001"
     - "9443:9443"
   ```

6. **Rewrite the TLS test**, `tests/e2e/client/scenarios/test_routing_retry.py::test_tls_handshake_error_returns_503_no_retry`:

   - Delete the `pytest.skip(...)`.
   - Point the vendor_key's `base_urls` at `https://fake-upstream:9443` (HTTPS scheme is what triggers pingora's TLS dialer).
   - Drive a single chat completion request and assert: status 503 and `_error_code(resp) == "GW_UPSTREAM_TLS_ERROR"`.
   - Snapshot `gw_retry_attempt_total{outcome="tls_blocked"}` before + after; assert delta ≥ 1.
   - Confirm `gw_retry_attempt_total{outcome="recovered"}` did NOT tick (TLS errors are non-retryable).

7. **Run**:

   ```bash
   cargo build --release -p e2e-fake-upstream
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   bash scripts/check-invariants.sh
   make e2e
   ```

   Assert `test_tls_handshake_error_returns_503_no_retry PASSED`.

8. **Commit** (include Cargo.lock):

   ```
   gap(e2e): fake-upstream TLS listener closes tls_blocked skip (W3-A)

   Adds a tokio-rustls TLS listener on port 9443 with an `rcgen`
   self-signed cert. The gateway's truststore does not trust the cert,
   so pingora's TLS handshake against `https://fake-upstream:9443`
   fails inside `connect_to_upstream`, fail_to_connect fires with
   ConnectErrorKind::Tls, and gw_retry_attempt_total{outcome=
   "tls_blocked"} increments. Routes installed via /admin/scenario
   apply to both plain (9000) and TLS (9443) listeners.

   Tests:
   - scenarios/test_routing_retry.py::test_tls_handshake_error_returns_503_no_retry
   ```

**Acceptance**: `make e2e` reports `30 passed, 0 skipped`.

---

# Wave 4 — Doc sync (W4-A)

## W4-A — Architecture / CLAUDE.md sync

**Goal**: per CLAUDE.md `Sync rule`, when a deferred gap lands, remove it from `docs/architecture.md` §Known gaps.

**Files touched**:

- `docs/architecture.md`

**Steps**:

1. Open `docs/architecture.md`, find §Known gaps.
2. Remove (or mark "landed in 2026-05-22-e2e-skip-closure") any entries about:
   - Gemini codec stream detection.
   - Sticky strategy `conversation_id` plumbing / TTL configurability.
   - fake_upstream TLS testability.
3. If §Deliberate behaviours had pointers to these gaps as deliberate ("v1 leaves Gemini stream rejected"), remove those too.
4. Cross-check: search `CLAUDE.md` for the strings "Gemini stream", "sticky", "TLS" — if any prose claims the v1 limitation, update it. Probably none, but check.
5. **Run**:

   ```bash
   bash scripts/check-invariants.sh
   make e2e   # final verification, no code changes since W3-A
   ```

6. **Commit**:

   ```
   docs(arch): remove landed e2e-skip-closure gaps from §Known gaps (W4-A)
   ```

**Acceptance**: `make e2e` still `30 passed, 0 skipped`; `docs/architecture.md` no longer claims these as deferred.

---

## End-to-end verification

After all four waves are committed, run from a fresh working directory:

```bash
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-invariants.sh
make e2e
```

Final output of `make e2e` should be:

```
==================== 30 passed in <runtime>s ====================
```

If any test fails or any skip remains, NEEDS_CONTEXT back to the coordinator — do not paper over with another skip.
