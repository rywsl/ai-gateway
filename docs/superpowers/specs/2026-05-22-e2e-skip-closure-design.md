# 2026-05-22 — E2E Skip Closure Design

> **Status**: spec. Plan: [`docs/superpowers/plans/2026-05-22-e2e-skip-closure.md`](../plans/2026-05-22-e2e-skip-closure.md).
>
> **Scope**: close the three e2e tests currently marked `pytest.skip(...)` with documented gateway-side or test-infrastructure gaps. After this lands, `make e2e` reports **30 passed, 0 skipped**.
>
> **Why this exists**: skips that document a known gap are honest, but they degrade over time. Each skip is a piece of gateway behaviour or test infrastructure that nobody is exercising end-to-end. Three skips today; without closure pressure, this grows.

## 0. The three skips

| Test | Skip reason (verbatim, abbreviated) | Root cause |
|------|--------------------------------------|------------|
| `test_happy_streaming.py::test_gemini_stream_happy` | "Gemini codec parse_request never sets `ir.stream=true` (no body field, no path-based override), so request_filter rejects every Gemini call with GW_REQUEST_NONSTREAM_NOT_SUPPORTED" | Gemini's "streaming?" signal lives in the URL path (`:streamGenerateContent`), but `ProtocolCodec::parse_request` only sees the body. |
| `test_routing_retry.py::test_sticky_strategy_binds_user_to_same_key_then_expires` | "`DEFAULT_STICKY_TTL_SECS=1800s` is too long for CI; gateway plumbs `conversation_id=None` so the moka bind path is structurally unreachable" | Two coupled issues: (a) `PickCtx.conversation_id` hard-coded to `None`; (b) sticky TTL is a compile-time constant with no config knob. |
| `test_routing_retry.py::test_tls_handshake_error_returns_503_no_retry` | "fake_upstream has no TLS listener; pingora's HTTPS-against-plain-HTTP error surfaces via `fail_to_proxy` not `fail_to_connect`, so `gw_retry_attempt_total{outcome=\"tls_blocked\"}` never increments" | Test-infrastructure gap: the fake upstream listens on TCP only. The gateway's TLS classification (`gw-proxy/src/gateway.rs::classify_connect_error`) is already correct and unit-tested. |

These are the only `pytest.skip(...)` calls in the suite (verified by `grep -rn "pytest.skip" tests/e2e/client/scenarios/`).

## 1. Design decisions

### ADR-S1 — Gemini stream detection via path-aware codec hook

**Decision**: Extend the `ProtocolCodec` trait with a static method `fn path_implies_stream(path: &str) -> bool` (default returns `false`). Gemini overrides to return `true` when `path.contains(":streamGenerateContent")`. `request_filter` ORs this into `ir.stream` after parsing.

**Rejected alternatives**:

- **A1.1 — Plumb path into `parse_request` signature.** Cleanest semantically, but breaks 20+ call sites including every roundtrip test and the cross-protocol re-parse in `filters/upstream.rs`. Cost > benefit for a one-codec quirk.
- **A1.2 — Inspect path in `filters/request.rs` directly with a `match` on `client_codec.kind()`.** Leaks Gemini-specific knowledge into the proxy. Future protocols with the same wart would have to extend the `match`, not their own codec module.
- **A1.3 — Add a `stream` field to `GeminiRequest` defaulting to false, set it before serialization.** Would require synthesising path knowledge inside `parse_request`, which has no access to path. Same problem as A1.1, less honest.

**Why this wins**: protocol knowledge stays inside the codec module. The trait grows by one default-providing method; existing impls (OpenAI, Anthropic, mock codec in tests) get the `false` default for free.

**Wire contract**: the path that the gateway sees is the one google-genai SDK constructs: `/v1beta/models/<model>:streamGenerateContent?alt=sse`. The `?alt=sse` query string is irrelevant — the `:streamGenerateContent` token in the path itself is sufficient.

### ADR-S1b — Gemini model name also derived from URL path

**Decision**: Add a second default-providing trait method `fn extract_path_model(path: &str) -> Option<&str>` (default returns `None`). Gemini overrides to extract the substring between `/models/` and the next `:` (the verb separator). `request_filter` falls back to this when `ir.model` came back empty from `parse_request`.

**Why this is needed**: discovered while validating ADR-S1 end-to-end. The Gemini wire spec puts the model in the URL path (`/v1beta/models/<MODEL>:streamGenerateContent`) and not in the request body. `GeminiRequest::model` is `#[serde(skip)]` and defaults to empty `Cow`, so `ir.model` is always empty after `parse_request`. Without this fix, every Gemini request fails `GW_ROUTING_NO_AVAILABLE_CHANNEL` after ADR-S1 unblocks the stream gate — the symptom we observed during W1-A's first integration run.

**Symmetry with ADR-S1**: same shape (default-providing trait method, codec overrides, dispatched through `AnyCodec`, proxy uses the result post-parse). Both signals (`stream`, `model`) live in the path for Gemini; both fixes land in W1-A together rather than in a separate wave.

**Cross-protocol implication**: the cross-protocol re-parse at `filters/upstream.rs:389` is unaffected — by that point the IR already carries the resolved `model` (we will set `ir.model` before re-parse, see implementation note in plan W1-A).

### ADR-S2 — Cross-protocol Gemini stream inherits the same hook

**Decision**: The same `path_implies_stream` hook must be evaluated when a Gemini client request is later re-parsed for cross-protocol re-rendering at `filters/upstream.rs:389`. We do NOT re-parse the path there (the IR already carries `.stream`), so the fix in ADR-S1 is sufficient: `ctx.is_stream` is set in `request_filter`, the IR shape into `parse_request` doesn't expose `stream`, but the re-parse uses the cached IR-equivalent body and the proxy already knows whether it's streaming via `ctx.is_stream`. No additional cross-protocol changes needed.

**Why this matters**: skipping the cross-protocol leg would silently break a Gemini-client → OpenAI-upstream flow even after ADR-S1 lands. Verifying upfront avoids that surprise.

### ADR-S3 — Sticky binding via `X-Conversation-Id` header

**Decision**: Read an optional `X-Conversation-Id` request header in `early_request_filter`, store on `ReqCtx.conversation_id: Option<String>`, and thread into `PickCtx.conversation_id` at the strategy dispatch site (`filters/upstream.rs:220`).

**Rejected alternatives**:

- **A3.1 — Derive from request body (e.g. `messages[].id` hashing).** No vendor protocol carries a stable conversation identifier in the request body; SDKs do not surface one. Hashing the message list would change every turn → no stickiness.
- **A3.2 — Use `(user_id, api_key_id)` as the binding key.** Conflates "principal" with "conversation"; one user driving multiple parallel conversations would route them all to one key, defeating the strategy's purpose.
- **A3.3 — Derive from `X-Forwarded-For` / source IP.** Same problem as A3.2 plus NAT/proxy interactions.

**Why this wins**: the only callers that need sticky behaviour are clients that already know their conversation id (chat UIs, agent loops). Letting them opt in via a header is the cleanest signal. Header is single-source-of-truth and trivially testable.

**Header naming**: `X-Conversation-Id`. Case-insensitive read (HTTP is case-insensitive). When absent, sticky behaves exactly as today (falls back to "first available"), so this is a strictly additive change.

**Cache key contract**: the moka cache key is the raw header value (UTF-8). Length-bounded by HTTP header limits (8 KiB in pingora's default). No further normalization — clients see-saw on different ids would just produce different bindings.

### ADR-S4 — Sticky TTL is a `Settings` knob

**Decision**: Add `routing.sticky_ttl_secs` (default `1800`) to `config/default.toml` and `Config`. `StrategySet::new_default` becomes `StrategySet::new(sticky_ttl: Duration)`; the wiring site in `gw-gateway/src/lib.rs` passes the configured value. `DEFAULT_STICKY_TTL_SECS` stays as a `pub const` so the existing `gw-routing::tests::sticky_ttl` test still pins production default to 30 min.

**Why a knob** (vs lower the constant): production default of 30 min is correct for real chat traffic. Test deployments need ~2s. A constant only serves one master; the knob serves both.

**Test wiring**: e2e docker-compose sets `GW_ROUTING__STICKY_TTL_SECS=2` in the gateway container, the sticky test waits ~3s after the first stream completes, and asserts the second request lands on a *different* key (because the binding expired and the hash-mod-n rebind picks freshly). Because moka's eviction is opportunistic, the test calls a small admin endpoint **only if** we add one; otherwise we accept moka's behaviour and assert a probabilistic distribution. Plan §3 picks the simpler path: no admin endpoint, just rely on moka's read-side eviction (a `get` after `time_to_live` returns `None`).

### ADR-S5 — Fake upstream TLS listener with self-signed cert

**Decision**: Add a second listener to `crates/e2e-fake-upstream/` on port `9443` that terminates TLS via `tokio-rustls` with a self-signed cert generated at startup (`rcgen` crate). The listener serves the same scenario engine as the existing port-9000 listener.

**The actual goal**: make pingora's `connect_to_upstream` fail at the TLS handshake — not before (refused connect would fire `fail_to_connect` with `Connect`, not `Tls`) and not after (a successful TLS handshake against a wrong-cert peer would route into `fail_to_proxy`, not `fail_to_connect`).

**How the test triggers TLS-error path**: vendor_keys point at `https://fake-upstream:9443` (note the `https://` scheme and 9443 port). pingora dials port 9443, attempts a TLS handshake with the gateway-side CA bundle (which doesn't trust our self-signed cert), the handshake fails, `fail_to_connect` fires with `ConnectErrorKind::Tls`, the metric `gw_retry_attempt_total{outcome="tls_blocked"}` increments, the request fails non-retryable, the gateway emits `GW_UPSTREAM_TLS_ERROR` and a 503.

**Rejected alternatives**:

- **A5.1 — Use a public TLS endpoint with a known-bad cert (`expired.badssl.com`).** Test runs without docker-network egress. Brittle in CI.
- **A5.2 — Generate the cert at image build time and bake into the image.** Adds build complexity for no gain — `rcgen` at startup is ~5 ms and the cert never needs to be trusted.
- **A5.3 — Use a single port that serves both plain TCP and TLS via ALPN.** Too much surface area for one test; isolating port 9443 keeps the existing plain-TCP scenarios undisturbed.

**Implementation note**: the existing `connection_refused_first_n` injection trick (vendor_keys at port 9999 to fake a refused connect) is preserved; the new TLS port is purely additive. fake_upstream's existing routes apply to BOTH listeners — the scenario engine doesn't care which transport carried the bytes.

### ADR-S6 — No backwards-compatibility shims

The codebase memory: `feedback_breaking_changes_ok.md` — "未上线，不要为旧字段/旧 stream 保留兼容路径或灰度期". Concretely:

- `StrategySet::new_default()` is renamed/refactored if needed; no `new_v2` alias.
- `DEFAULT_STICKY_TTL_SECS` constant stays but is **no longer the only source**; the runtime TTL is whatever `routing.sticky_ttl_secs` resolves to.
- `path_implies_stream` is a real trait method, not a separate `PathAwareCodec` extension trait.
- The new TLS listener is unconditional; no feature flag.

## 2. Out of scope

- Sticky binding *eviction* metrics (currently zero observability into moka internals). Tracked separately; not needed to close the skip.
- Gemini non-streaming path. Skip text says `parse_request never sets ir.stream=true`; this spec only closes the stream side. Non-stream Gemini is its own design problem (`parse_response` is `unimplemented!()` for all codecs in v1 by deliberate choice — see `docs/architecture.md` §Deliberate behaviours).
- Conversation-id propagation to *upstream* (the header is consumed by sticky and dropped, NOT forwarded). Forwarding would couple gateway routing semantics to vendor-side behaviour, which is out of scope.
- TLS cert rotation. The self-signed cert is generated once per fake_upstream process lifetime; container restart picks a new one. Tests do not depend on cert identity.

## 3. Invariants this spec MUST preserve

1. **Same-protocol Gemini passthrough is byte-identical** for non-streaming bodies (i.e. when path is `:generateContent`, not `:streamGenerateContent`). The new `path_implies_stream` hook returns `false` for those paths.
2. **`Sticky` without a `conversation_id` still falls back to "first available"** — i.e. requests with NO `X-Conversation-Id` header continue to behave exactly as v1 today.
3. **TLS error classification is identical** to the unit-tested behaviour in `gw-proxy/src/gateway.rs`; the new fake_upstream listener only exposes that behaviour to e2e, it does not alter the gateway code path.
4. **The three test functions stop calling `pytest.skip(...)` entirely.** The skip text is removed, not converted to a comment.

## 4. Files this spec touches (preview — see plan for full breakdown)

- `crates/gw-codec/src/trait_def.rs` — new trait method `path_implies_stream`.
- `crates/gw-codec/src/gemini/codec.rs` — override `path_implies_stream`.
- `crates/gw-codec/src/any.rs` — dispatch.
- `crates/gw-proxy/src/filters/request.rs` — OR into `ir.stream` after parse.
- `crates/gw-proxy/src/filters/early.rs` — read `X-Conversation-Id` → `ctx.conversation_id`.
- `crates/gw-proxy/src/ctx.rs` — `conversation_id: Option<String>` field.
- `crates/gw-proxy/src/filters/upstream.rs` — thread `ctx.conversation_id` into `PickCtx`.
- `crates/gw-routing/src/strategy/set.rs` — `new(sticky_ttl)`; `new_default` becomes a thin wrapper passing `DEFAULT_STICKY_TTL_SECS`.
- `crates/gw-gateway/src/config.rs` — `RoutingCfg { sticky_ttl_secs }`.
- `crates/gw-gateway/src/lib.rs` — pass configured TTL into `StrategySet::new`.
- `config/default.toml` — `[routing] sticky_ttl_secs = 1800`.
- `crates/e2e-fake-upstream/Cargo.toml` — add `tokio-rustls`, `rcgen`.
- `crates/e2e-fake-upstream/src/server.rs` (or new sibling `tls.rs`) — TLS listener.
- `tests/e2e/docker-compose.yml` — expose 9443; set `GW_ROUTING__STICKY_TTL_SECS=2`.
- `tests/e2e/client/scenarios/test_happy_streaming.py` — un-skip Gemini test.
- `tests/e2e/client/scenarios/test_routing_retry.py` — un-skip sticky + TLS tests; rewrite TLS test to target port 9443 with `https://` base_url.
- `docs/architecture.md` — §Known gaps cleanup per CLAUDE.md sync rule.

## 5. Acceptance gate

```
make e2e
...
==================== 30 passed, 0 skipped in <runtime> ====================
```

with no warnings beyond the existing third-party `InsecureKeyLengthWarning` (which is a pyjwt fixture artefact, not our concern).
