# 2026-05-21 — Known-Gaps Closure Design

> **Scope**: close the 13 gaps documented in [`docs/architecture.md`](../../architecture.md) §9 with a minimum-viable v1.5 implementation. Several gaps already have rich v2 specs (codec-s3, routing-r-e, billing-c-δ, routing-r-g) that depend on larger refactors (S1 v2 schema rename, K-B vendor-error hook landing, etc.). This spec sets a tighter, shippable boundary and explicitly defers the v2 surface area to those existing specs.
>
> **Companion specs** (cross-reference, not superseded):
> - `2026-05-19-codec-s3-sse-multi-event.md` — v2 N² stream codegen (this spec lands the prerequisite render-via-client-codec path)
> - `2026-05-19-codec-s4-response-and-cross-protocol.md` — v2 N² response codegen + UsageSnapshot POD
> - `2026-05-19-routing-r-e-failover.md` — v2 FailoverExecutor (this spec lands a Pingora `fail_to_connect` hook only)
> - `2026-05-19-routing-r-g-sticky-fix.md` — v2 Sticky moka migration (this spec adopts ADR-RG-1 verbatim as the v1.5 fix)
> - `2026-05-19-billing-c-delta-stream-dlq.md` — v2 three-layer DLQ (this spec lands single-fallback Redis DLQ stream only)
>
> **Boundaries**:
> - No cost computation in Rust (`no_cost_micro` invariant).
> - No schema rename (`gw-core::ir` remains; v2 Phase 2 lands `gw-core::schema` atomically).
> - No `parse_response` implementation — non-stream rejection stays. Cross-protocol response work in this spec only covers the **stream chunk** path.

## 0. Coverage map

| # | Gap | Existing spec | This spec |
|---|-----|---------------|-----------|
| 1 | `ApiKeyEntry.expires_at` enforcement | — | **ADR-G1-1** (new) |
| 2 | `ApiKeyEntry.ip_allowlist` per-key | — | **ADR-G2-1** (new) |
| 3 | `UserEntry.daily_quota_tokens` | — | **ADR-G3-1, G3-2** (new) |
| 4 | `Vendor.timeout_overrides` | — | **ADR-G4-1** (new) |
| 5 | `Pricing.model_markup` capture | C-β (markup chain) | **ADR-G5-1** (new — wiring) |
| 6 | JWT `iss`/`aud`/`nbf` on client bearer | — | **ADR-G6-1** (new) |
| 7 | Mandatory HMAC at body EOS | — | **ADR-G7-1, G7-2** (new) |
| 8 | Cross-protocol response stream re-render | codec-s3 (v2) | **ADR-G8-1** (v1.5 path; v2 = S3) |
| 9 | Multi-event `parse_stream_chunk` | codec-s3 ADR-S3-v2-1 | **ADR-G9-1** (v1.5 path) |
| 10 | Retry executor on proxy hot path | routing-r-e (v2) | **ADR-G10-1** (v1.5 connect-only) |
| 11 | DLQ for billing flush | billing-c-δ (v2) | **ADR-G11-1** (v1.5 single fallback) |
| 12 | Sticky TTL sweep + shared-state fix | routing-r-g (v2) | **ADR-G12-1** (adopt R-G ADR-RG-1) |
| 13 | `CapabilityResolver` injection | T6-A note in plans/ | **ADR-G13-1, G13-2** (new) |

Section §1–§5 cluster the 13 gaps by subsystem. Acceptance criteria live with each ADR; the consolidated test-matrix is in §6.

---

## 1. Auth + permission hardening

Cluster goal: every field on `ApiKeyEntry` / `UserEntry` that v1 already loads from Postgres must actually be enforced, and the JWT path used by **clients** (not management tokens) must validate the same `iss`/`aud`/`nbf` envelope as the management-token path.

### ADR-G1-1 — Enforce `ApiKeyEntry.expires_at`

**Status**: SCAFFOLDED_BUT_UNUSED. Field exists at `crates/gw-core/src/entity.rs:110`; loaded at `crates/gw-store/src/loader.rs:205`; cloned into `VendorKey` at `crates/gw-routing/src/candidates.rs:71`; never compared against the current time.

**Decision**: check at the auth-extraction step in `request_filter`, **not** during candidate filtering. The check is per-API-key, not per-vendor-key, and the v1 candidate filter is already responsible for routing concerns only. An expired key must yield `401 GW_AUTH_KEY_EXPIRED` before any IR parse or permission merge.

**Implementation site**: `crates/gw-proxy/src/filters/request.rs` immediately after `store.apikey_or_load(&akid)` resolves the `ApiKeyEntry` (currently around line 145–166). Behaviour:

```text
if let Some(exp) = entry.value.expires_at {
    if OffsetDateTime::now_utc() >= exp {
        emit ErrorIR { category: ClientAuth, status: 401,
                       code: "GW_AUTH_KEY_EXPIRED", retryable: false };
        short_circuit(401);
    }
}
```

`expires_at` is `Option<OffsetDateTime>`; `None` means never expires.

**New error code**: `GW_AUTH_KEY_EXPIRED` in `crates/gw-core/src/error/codes.rs`.

**Acceptance**:
- A request with an expired key returns `401 GW_AUTH_KEY_EXPIRED` regardless of model / vendor.
- A request with `expires_at = None` is unaffected.
- A request issued exactly at `expires_at` rejects (closed interval).
- New unit test under `crates/gw-proxy/tests/` covering all three.

---

### ADR-G2-1 — Enforce `ApiKeyEntry.ip_allowlist`

**Status**: SCAFFOLDED_BUT_UNUSED. Field exists at `crates/gw-core/src/entity.rs:108`; loaded at `crates/gw-store/src/loader.rs:204`; cloned into `VendorKey` at `crates/gw-routing/src/candidates.rs:70`; never consulted.

**Decision**: check immediately after the User-/ApiKey-scoped blocklist re-check in `request_filter`. The check is **deny-list complementary**, not a replacement: a request must pass both global blocklist and (if `ip_allowlist` is non-empty) the per-key allowlist.

**Storage shape**: `Vec<String>` CIDR list on `ApiKeyEntry`. Empty / `None` = no restriction (open). Non-empty = client IP must match at least one CIDR.

**Implementation site**: `crates/gw-proxy/src/filters/request.rs` after `check_blocklist(ip, Some(&user_id), api_key_id, store)` (around line 244). Behaviour:

```text
if let Some(allowlist) = &entry.value.ip_allowlist {
    if !allowlist.is_empty() && !match_any_cidr(client_ip, allowlist) {
        emit ErrorIR { category: ClientAuth, status: 403,
                       code: "GW_AUTH_KEY_IP_NOT_ALLOWED", retryable: false };
        short_circuit(403);
    }
}
```

Reuse / extend the CIDR matcher in `crates/gw-auth/src/ip_filter.rs` rather than adding a new dependency.

**New error code**: `GW_AUTH_KEY_IP_NOT_ALLOWED`.

**Acceptance**:
- Empty / `None` allowlist → unrestricted.
- Non-empty allowlist with matching CIDR → allowed.
- Non-empty allowlist with no match → `403 GW_AUTH_KEY_IP_NOT_ALLOWED`.
- Invalid CIDR in the stored allowlist → log warning, fail closed (deny).
- Tests cover IPv4 single-host, IPv4 prefix, IPv6 prefix, and a deliberately malformed CIDR.

---

### ADR-G6-1 — Validate `iss`/`aud`/`nbf` on the client-bearer JWT path (hybrid JWKS)

**Status**: PARTIAL. The T1-A `JwtValidator` (`crates/gw-auth/src/jwt.rs:62+`) supports `iss`/`aud`/`nbf`/leeway but takes a **single static secret** (HS256 bytes or RS256 PEM). The **client** bearer path (`crates/gw-proxy/src/filters/request.rs` around line 133) still calls the legacy async `verify_jwt(bearer, store)` (`crates/gw-auth/src/jwt.rs:26-38`) which only checks `exp` but performs **dynamic JWKS lookup** via the token's `kid` header (`store.jwks_or_load(&kid)`). The management-token path (`crates/gw-auth/src/mgmt.rs`) uses `JwtValidator` already.

**Decision (hybrid)**: extend `JwtValidator` with an optional JWKS resolver so the client bearer path keeps RS256 + `kid` rotation support **and** gains `iss`/`aud`/`nbf` enforcement. A straight replacement would break any deployment relying on JWKS key rotation — that is a v1 capability we must preserve.

New `JwtValidator` shape:

```rust
pub struct JwtValidatorConfig {
    pub static_secret_hs256: Option<Vec<u8>>,
    pub static_pem_rs256: Option<Vec<u8>>,
    pub expected_iss: Option<String>,
    pub expected_aud: Option<String>,
    pub leeway_secs: u64,
    pub require_nbf: bool,
}

#[async_trait::async_trait]
pub trait JwksResolver: Send + Sync {
    async fn resolve(&self, kid: &str) -> Option<Vec<u8>>;  // PEM bytes
}

pub struct JwtValidator {
    cfg: JwtValidatorConfig,
    jwks: Option<Arc<dyn JwksResolver>>,
}
```

`validate(token)` dispatch:
1. Decode header → `alg`, optional `kid`.
2. Pick verification key:
   - `alg = HS256` → require `static_secret_hs256`, reject if absent.
   - `alg = RS256` + `kid present` + `jwks` resolver wired → `jwks.resolve(kid).await`.
   - `alg = RS256` + no `kid` (or resolver returned `None`) → fall back to `static_pem_rs256`.
3. `jsonwebtoken::decode` with `Validation { iss, aud, leeway, validate_nbf, validate_exp }`.
4. Map errors:
   - `InvalidIssuer` → `JwtError::InvalidIss`
   - `InvalidAudience` → `JwtError::InvalidAud`
   - `ImmatureSignature` → `JwtError::NotYetValid`
   - `ExpiredSignature` → `JwtError::Expired`
   - `InvalidSignature` / decode failures → `JwtError::InvalidSignature`

**Backend impl**: `EntityStoreJwksResolver` wraps `Arc<EntityStore>` and calls the existing `jwks_or_load(&Kid)` path verbatim — no behaviour change for the v1 JWKS lookup.

**Implementation sites**:
- `crates/gw-auth/src/jwt.rs`: extend `JwtValidator` per above. Add `JwksResolver` trait and `EntityStoreJwksResolver` adapter struct (the adapter lives in `gw-auth` to avoid pulling `gw-store` into the validator's trait surface — `Arc<EntityStore>` is the only thing the adapter needs).
- `crates/gw-gateway/src/lib.rs::bootstrap`: after `EntityStore` is up, load `AuthConfig::global` (best-effort; absent → permissive defaults — no `iss`/`aud`/`nbf` enforcement, keeps current behaviour). Construct `EntityStoreJwksResolver { store: store.clone() }`. Build `JwtValidator` with the resolver wired. Thread into `Wired` and `LlmGateway::new`.
- `crates/gw-proxy/src/gateway.rs::LlmGateway`: add field `jwt_validator: Option<Arc<JwtValidator>>` (Option because absent AuthConfig is allowed).
- `crates/gw-proxy/src/filters/request.rs`: client bearer branch calls `self.jwt_validator.as_ref().map(|v| v.validate(bearer).await).transpose()?`. When validator is `None` (no AuthConfig.global) fall back to the legacy `verify_jwt(bearer, store)` path — explicit graceful degradation so deployments that have not deployed AuthConfig.global yet keep working with v1 semantics.
- Keep `verify_jwt` legacy function. Do not delete — it is the v1 JWKS-only fallback used when AuthConfig is absent and is also the path used by the management-token surface until that migrates in v2.

**Acceptance**:
- A deployment **with** `AuthConfig.global` and a JWKS-rotating RS256 token: validator resolves the right key via `kid`, then enforces `iss`/`aud`/`nbf`. Wrong `iss` → 401 `GW_AUTH_JWT_INVALID_ISS`.
- A deployment **without** `AuthConfig.global`: legacy `verify_jwt` runs; only `exp` is checked. No regression vs v1.
- A token with valid `exp` but mismatched `iss` (validator wired) → 401 `GW_AUTH_JWT_INVALID_ISS`.
- A token with valid `exp`/`iss` but mismatched `aud` → 401 `GW_AUTH_JWT_INVALID_AUD`.
- A token with `nbf` in the future (outside `leeway`) → 401 `GW_AUTH_JWT_NOT_YET_VALID`.
- A management token continues to go through the existing management-token branch unchanged.
- HS256 deployments (no JWKS) work via `static_secret_hs256` without a resolver — unit test covers this.

**New error codes**: `GW_AUTH_JWT_INVALID_ISS`, `GW_AUTH_JWT_INVALID_AUD`, `GW_AUTH_JWT_NOT_YET_VALID`. (`GW_AUTH_JWT_EXPIRED` already exists.)

---

### ADR-G7-1 — Wire `verify_body` into the request body filter

**Status**: SCAFFOLDED_BUT_UNUSED. `crates/gw-proxy/src/body_verify.rs` carries an explicit TODO to wire `verify_body` into `gateway.rs::request_body_filter` at EOS. `signature::verify_required` (`crates/gw-auth/src/signature.rs:64-99`) implements HMAC-SHA256 / HMAC-SHA512 with a ±300 s replay window. `ApiKeyEntry.signature_required` + `signature_algo` are loaded and stashed on ctx at `request_filter` step 1c (around `crates/gw-proxy/src/filters/request.rs:150-165`).

**Decision**: after the cached body is assembled in `request_filter` (the existing 16 MiB read loop, currently around line 250-274), invoke `verify_body` synchronously **before** IR parse. Fail-closed: if `ApiKeyEntry.signature_required = true` and either header missing or signature mismatch / timestamp drift → `401 GW_AUTH_SIGNATURE_INVALID`. The check is body-bound, so it must happen after the body is fully buffered, which is the same point IR parse happens. The hook does **not** belong in Pingora's `request_body_filter` callback (that fires per chunk and is too late for short-circuit envelopes).

**Implementation site**: `crates/gw-proxy/src/filters/request.rs` after the body-read loop, before the codec parse. Reuse `crates/gw-proxy/src/body_verify.rs::verify_body` — extract the function out of its current TODO scaffolding into an active call site.

**Acceptance**:
- `signature_required = false` → no-op even if signature header absent.
- `signature_required = true` + valid signature + timestamp within ±300 s → pass.
- Timestamp drift > 300 s → `401 GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`.
- Wrong HMAC → `401 GW_AUTH_SIGNATURE_INVALID`.
- Missing `x-signature` or `x-timestamp` when required → `401 GW_AUTH_SIGNATURE_MISSING`.

**New error codes**: `GW_AUTH_SIGNATURE_INVALID`, `GW_AUTH_SIGNATURE_MISSING`, `GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT`.

### ADR-G7-2 — Out of scope for v1.5

A workspace-global "force HMAC for all keys regardless of `signature_required`" toggle is out of scope. Granularity remains per-`ApiKeyEntry`. v2 may add a `signature_policy: Required | OptionalIfSet | Forbidden` enum on `UserEntry` / `ApiKeyEntry` if the policy granularity widens. The replay-window constant (`REPLAY_WINDOW_SECS = 300`, `crates/gw-auth/src/signature.rs:47`) stays hardcoded; moving it to config is also out of scope.

---

## 2. Quota & markup wiring

### ADR-G3-1 — `daily_quota_tokens` storage and key shape

**Status**: SCAFFOLDED_BUT_UNUSED. Field at `crates/gw-core/src/entity.rs:54`; no read site; no Redis helper.

**Decision**: track daily consumption in Redis with a per-user, per-UTC-day counter. Key shape:

```
counter:user:{effective_user_id}:daily_tokens:{YYYY-MM-DD}
```

UTC day boundary; no per-timezone customisation in v1.5. TTL = 48 h on first `INCRBY` (next-day cleanup margin).

`crates/gw-store/src/counter_store.rs` gains a thin helper:

```text
pub fn daily_tokens_key(user_id: &UserId, day_utc: &str) -> String { ... }
```

Window string is `YYYY-MM-DD` derived from `time::OffsetDateTime::now_utc().date()`. The helper does **not** read clock — caller passes the precomputed string so tests can pin the day.

### ADR-G3-2 — `daily_quota_tokens` enforcement points

**Decision**: two points along the request lifecycle:

1. **Pre-check** in `request_filter` step 7b (between balance check and `USER_CIRCUIT`). Reads `counter:user:{eid}:daily_tokens:{today}` and rejects with `403 GW_QUOTA_DAILY_TOKENS_EXCEEDED` if `current + est_input_tokens > daily_quota_tokens`. The freeze estimate is intentionally added: the check is conservative and fail-closed.
2. **Post-increment** in `logging`. Once the actual `input + output` token total is known, `INCRBY counter:user:{eid}:daily_tokens:{today} (input + output)` with `EXPIRE 48h`. Non-fatal: Redis failure increments `gw_daily_quota_increment_error_total` and is dropped. No DLQ in v1.5 — daily-quota is best-effort, not billing.

`daily_quota_tokens = 0` is treated as "no quota" (unlimited). `None` is also unlimited. Only `> 0` enforces.

**Acceptance**:
- A new day rolls the counter back to 0 (verified by pinning the day-string).
- Pre-check rejects when freeze estimate would push over the quota.
- Logging post-increment uses actual usage (not estimate), so the next request's pre-check sees the true consumption.
- Redis failure on pre-check fails open (consistent with the existing balance check).
- Redis failure on post-increment is logged + metric only.

**New error code**: `GW_QUOTA_DAILY_TOKENS_EXCEEDED`.

---

### ADR-G5-1 — Capture `Pricing.model_markup` into `MarkupChainSnapshot`

**Status**: PARTIAL. `MarkupChainSnapshot` (`crates/gw-auth/src/merge.rs:156`) has `model_markup: 1.0` hardcoded. `Pricing.model_markup` exists at `crates/gw-core/src/entity.rs:241`. `Pricing` is currently lazy-loaded at `crates/gw-proxy/src/filters/upstream.rs:166`, **after** `merge_permissions` runs in `request_filter`.

**Decision**: pre-load `Pricing` in `request_filter` (after the IR parse so we know `model_name`) and pass it into `merge_permissions`. `merge_permissions` reads `pricing.model_markup` and populates the field. The lazy-load in `upstream_peer` becomes a cache hit because `EntityStore::pricing_or_load` is memoising. Rationale: keep the chain authoritative at the point of merge so downstream readers (the ledger emitter in `logging`) see a complete chain without re-stamping.

This is **not** cost computation. Rust still emits the chain verbatim into `UsageRecord.markup_chain_snapshot`; Go multiplies. `no_cost_micro` and `no_markup_product_scalar` invariants stay green.

**Implementation site**:
- `crates/gw-proxy/src/filters/request.rs`: after the IR parse, call `store.pricing_or_load(&model_name)` and stash on a local; pass to `merge_permissions`.
- `crates/gw-auth/src/merge.rs::merge_permissions`: extend signature with `pricing: Option<Arc<Versioned<Pricing>>>`; replace the `1.0` placeholder.
- `crates/gw-proxy/src/filters/upstream.rs`: the existing `pricing_or_load` call at line 166 becomes redundant — replace with a load from ctx.

**Acceptance**:
- `Pricing.model_markup` flows into `UsageRecord.markup_chain_snapshot.markup_chain[…].model_markup`.
- The `MarkupChainOrigin` enum gains a `Model` variant (or reuses an existing one).
- A missing `Pricing` entity defaults `model_markup` to `1.0` and logs a warning (not an error).
- An integration test on `crates/gw-auth/tests/merge.rs` asserts the chain contains the model entry.

---

## 3. Routing & strategy resilience

### ADR-G12-1 — Sticky shared state + TTL (adopt `routing-r-g` ADR-RG-1)

**Status**: SCAFFOLDED but two bugs:
1. `strategy_for()` at `crates/gw-proxy/src/filters/upstream.rs:285` constructs a fresh `Sticky::default()` per request, so the assignment map is always empty.
2. Even if shared, no TTL sweep — entries leak.

**Decision**: adopt `2026-05-19-routing-r-g-sticky-fix.md` ADR-RG-1 verbatim:
- Replace `DashMap` with `moka::sync::Cache` (TTL = 30 min, capacity = 100 000).
- Make `Sticky` shareable: `Arc<Sticky>` constructed once in bootstrap, threaded through `Wired` → `LlmGateway` → `filters::upstream::strategy_for`.
- `strategy_for` becomes a thin lookup over an `Arc<StrategySet>` instead of constructing strategies fresh.

The `pick_ordered` `SmallVec` interface from R-G ADR-RG-2 is **deferred** to the v2 routing rework (R-AB). v1.5 keeps `pick → Option<&Arc<VendorKey>>` and adds Sticky's `take_first_available` shape as an internal helper.

**Implementation sites**:
- `crates/gw-routing/src/strategy/sticky.rs`: replace `DashMap<String, (KeyId, Instant)>` with `moka::sync::Cache<String, KeyId>`.
- `crates/gw-routing/src/strategy/mod.rs` (or new `set.rs`): introduce `StrategySet { fixed_order: FixedOrder, sticky: Sticky, ... }` — each strategy that holds state is constructed once.
- `crates/gw-proxy/src/gateway.rs::LlmGateway`: add field `strategies: Arc<StrategySet>`.
- `crates/gw-gateway/src/lib.rs::bootstrap`: construct `Arc<StrategySet>`; thread through `Wired`.
- `crates/gw-proxy/src/filters/upstream.rs::strategy_for`: take `&StrategySet` and return `&dyn Strategy` (or an enum reference).

**Acceptance**:
- Two requests from the same `(user_id, model)` choose the same key under `Sticky` for 30 min.
- After 30 min the assignment is re-rolled.
- A bound key removed from `model_group.vendor_keys` (e.g. via `events:entity` invalidation) is gracefully re-picked on the next request (the cache entry is stale; Sticky falls back to round-robin and writes a new binding).
- New metrics: `gw_sticky_bind_total`, `gw_sticky_hit_total`, `gw_sticky_evict_total`, `gw_sticky_bindings_active`.
- Bench `gw-routing/benches/strategy_pick.rs` confirms Sticky pick stays under the existing P99 budget.

**Out of scope for v1.5**: cross-replica sticky persistence (Redis-backed). v1 relies on L4 sticky in front of the gateway.

---

### ADR-G13-1 — `CapabilityResolver` shape and injection

**Status**: SCAFFOLDED_BUT_UNUSED. `CapabilityResolver` (`crates/gw-capability/src/resolver.rs:18-20`) carries `Arc<dyn ResolveBackend>` and works in tests with `FakeResolveBackend`. No production backend implements `ResolveBackend`. The TODO at `crates/gw-proxy/src/filters/request.rs:96-104` documents the exact injection point.

**Decision**: provide a production `EntityStoreResolveBackend` that wires `ResolveBackend` to the existing `EntityStore` model and key metadata. Three lookups:

1. `get_model_caps(model)` → query `EntityStore::model_or_load(&model)` and read `Model.capabilities: Vec<Capability>`. If model entity does not exist, return empty (treat as "no specific requirements").
2. `get_key_caps(key_id)` → query `EntityStore::vendor_key_or_load(&key_id)` and read `VendorKey.forced_capabilities: Vec<Capability>`.
3. `is_exclusive_matcher(key_id)` → read `VendorKey.exclusive_capability_match: bool`. Default false.

Three new fields on existing entities:
- `Model.capabilities: Vec<Capability>` — what this model can do (vision, audio, thinking, etc.).
- `VendorKey.forced_capabilities: Vec<Capability>` — what this key advertises as required-to-route.
- `VendorKey.exclusive_capability_match: bool` — if true, only requests whose required-caps **exactly** match this key's `forced_capabilities` may route to it.

Schema migration in `crates/gw-store/migrations/0004_capabilities.sql`.

### ADR-G13-2 — Resolver call site and candidate filtering

**Decision**: call `resolver.resolve(req).await` in `request_filter` after permission merge, before any non-failing operation, and stash `RequiredCapabilities` on `ctx.required_caps`. In `upstream_peer`, after candidate filter (`crates/gw-routing/src/candidates.rs`), apply `CapabilityFilter::evaluate(candidate, &required_caps)` to remove keys that fail to satisfy the requirement set.

The header source for `x-require-capability` is a comma-separated list parsed in `request_filter`; absent header → resolver derives requirements from `Model.capabilities` only.

**Implementation sites**:
- `crates/gw-proxy/src/gateway.rs::LlmGateway`: new field `capability_resolver: Option<Arc<CapabilityResolver>>`. `Option` because a deployment without `Model` entities can disable resolution by passing `None`.
- `crates/gw-gateway/src/lib.rs::bootstrap`: construct `EntityStoreResolveBackend`, wrap in `CapabilityResolver`, thread to `Wired`.
- `crates/gw-proxy/src/filters/request.rs:96-104`: replace the TODO with the real call.
- `crates/gw-routing/src/candidates.rs`: extend `get_candidate_keys` signature to accept `Option<&RequiredCapabilities>`; iterate with `CapabilityFilter::evaluate`.
- `crates/gw-proxy/src/filters/upstream.rs:170-194`: pass `ctx.required_caps.as_ref()` into the candidate call.

**Acceptance**:
- A request for a vision-capable model with `vision` in `Model.capabilities` routes only to keys whose `forced_capabilities` include `Vision` (or whose `forced_capabilities` is empty — non-restrictive).
- `x-require-capability: thinking` header forces `Thinking` into the requirement set even if the model doesn't declare it.
- A key with `exclusive_capability_match = true` and `forced_capabilities = [Vision]` only matches requests with **exactly** `[Vision]`.
- No keys match → `GW_ROUTING_NO_CAPABILITY_MATCH` 503 (distinct from `GW_ROUTING_NO_AVAILABLE_CHANNEL`).
- `gw_capability_resolve_total` metric is incremented per resolve, partitioned by outcome.

**New error code**: `GW_ROUTING_NO_CAPABILITY_MATCH`.

---

### ADR-G10-1 — Pingora `fail_to_connect` hook (retry on connect only)

**Status**: NOT_STARTED for v1.5. `ClassifierOutcome.retryable` (`crates/gw-core/src/error/class.rs:42-48`) exists but never consulted. `crates/gw-proxy/src/ctx.rs:87` already carries a `retry_count` field. The full FailoverExecutor design in `2026-05-19-routing-r-e-failover.md` is the v2 target; this spec lands the minimum: **connect-failure retry only**.

**Decision**: implement `ProxyHttp::fail_to_connect` on `LlmGateway`. When upstream TCP connect or TLS handshake fails:

1. Increment `ctx.retry_count`.
2. Push the failing `KeyId` onto `ctx.retry_blacklist: Vec<KeyId>`.
3. Classify the failure (`ConnectRefused`, `Timeout`, `TlsError`) and decide retryability:
   - `ConnectRefused` / `Timeout` → CrossKey (try a different vendor key, same vendor).
   - `TlsError` → No retry (config error, not transient).
4. If `retry_count < MAX_CONNECT_RETRIES` (constant, default 2) and retryability is not `No`, return `true` to instruct Pingora to retry. The next iteration enters `upstream_peer` again; the candidate filter reads `ctx.retry_blacklist` and skips blacklisted keys.
5. Otherwise return `false` and let the error bubble through `error_emit::send_error` with `GW_UPSTREAM_CONNECT_FAILED` 503.

`ctx.retry_blacklist` is consulted at `crates/gw-routing/src/candidates.rs` filter; add an early skip clause.

**Out of scope for v1.5**:
- Response-side retry (5xx / 429 after `response_filter`) — not possible in standard Pingora `ProxyHttp` without re-dispatching the entire request. This is the FailoverExecutor v2 surface (`routing-r-e`).
- Same-key retry with backoff — connect retries cross-key only; same-key retry is a strict superset that v2 covers.
- `RetryConfig` per-`ModelGroup` — v1.5 uses a global constant.

**Acceptance**:
- Two upstream keys, first `ConnectRefused` → second key serves the request, `gw_retry_attempt_total{outcome="recovered"}` = 1.
- Both keys fail → 503 `GW_UPSTREAM_CONNECT_FAILED`, `gw_retry_attempt_total{outcome="exhausted"}` = 1.
- TLS error on first key → no retry; immediate 503 `GW_UPSTREAM_TLS_ERROR`.
- `retry_count` exposed in the detailed log.

**New error codes**: `GW_UPSTREAM_CONNECT_FAILED`, `GW_UPSTREAM_TLS_ERROR`.

---

## 4. Codec residuals

### ADR-G4-1 — `Vendor.timeout_overrides` precedence

**Status**: PLACEHOLDER. Field at `crates/gw-core/src/entity.rs:149` (`Option<TimeoutSpec>` with `connect_ms` / `read_ms` / `total_ms` / `stream_total_ms` at line 157-162). Never read.

**Decision**: precedence ladder for timeouts in `crates/gw-proxy/src/filters/upstream.rs:240-259`:

```
hardcoded defaults  <  Vendor.timeout_overrides  <  Pricing timeouts
```

Pricing wins because it's the most specific (per-model). Vendor wins over defaults because operators typically tune at the vendor level.

**Implementation site**: in `pick_peer` after `store.vendor_or_load(&picked.vendor_id)` resolves the vendor, before the Pricing block. Apply `Vendor.timeout_overrides` first; the Pricing block then overwrites any field it sets.

**Acceptance**:
- Vendor has `connect_ms = 1000`, Pricing has `connect_ms = 500` → effective 500.
- Vendor has `read_ms = 30000`, Pricing has no `read_ms` → effective 30000.
- Neither vendor nor pricing sets `total_ms` → effective default.
- Tests on `crates/gw-proxy/tests/timeouts.rs` (new file) cover all four field permutations.

---

### ADR-G8-1 — Cross-protocol stream chunk re-render (v1.5 minimum)

**Status**: PLACEHOLDER. `crates/gw-proxy/src/filters/response.rs:13-14` explicitly says "Cross-protocol re-rendering of the body is not yet wired". Codecs implement `render_stream_chunk` already; only the conditional in `handle_event` is missing.

**Decision (v1.5)**: on each parsed `StreamEventIR`, if `ctx.client_codec.kind() != ctx.upstream_codec.kind()`, re-render via `ctx.client_codec.render_stream_chunk(&event, &mut out_buf)` and write `out_buf` to the client instead of forwarding the upstream chunk verbatim. Same-protocol path is unchanged (zero-copy forward).

This is **not** the codec-s3 N² wire-direct codegen. It's an IR-mediated round-trip: `upstream_chunk → upstream_codec.parse_stream_chunk → IR → client_codec.render_stream_chunk → client_chunk`. The IR round-trip is slower than direct mapping but works without the S1 v2 schema refactor and ships now. S3 supersedes this path when it lands.

**Implementation site**: `crates/gw-proxy/src/filters/response.rs::handle_event` (around line 93-116). Today it only updates ctx counters; extend it to optionally produce a body chunk for the response. The body-write coordination needs a small ctx field `pending_client_body: Option<Bytes>` so the outer loop in `response_body_filter` can emit it after the function returns.

**Constraints**:
- Stream payload mutations (added system prompt, etc.) are out of scope. The IR round-trip carries finish_reason and content; any feature unsupported on the destination protocol degrades per ADR-S3-v2-1 §degradation rules (drop, log).
- Multi-event handling shares §ADR-G9-1.

**Acceptance**:
- OpenAI client ↔ Anthropic upstream: client receives OpenAI-shaped SSE deltas.
- OpenAI client ↔ Gemini upstream: client receives OpenAI-shaped SSE deltas.
- Anthropic client ↔ Anthropic upstream: zero-copy, no IR allocation (asserted by counter `gw_codec_passthrough_total`).
- TTFT ≤ 1.5 × same-protocol baseline on the cross-protocol path (criterion bench).
- A test on `crates/gw-codec/tests/cross_protocol_stream.rs` verifies the wire shape per pair.

**New metric**: `gw_codec_cross_render_total{from, to}`.

**Deferred to v2 (codec-s3)**: direct wire→wire conversion (no IR), stateful cross-chunk accumulators, multi-event coalescing inside one chunk, thinking-signature passthrough, tool_call delta coalescing.

---

### ADR-G9-1 — Multi-event `parse_stream_chunk` (v1.5)

**Status**: SCAFFOLDED. `ProtocolCodec::parse_stream_chunk` returns `Option<StreamEventIR>`. Gemini and OpenAI sometimes emit chunks containing multiple semantically distinct events.

**Decision (v1.5)**: change the trait method to return `SmallVec<[StreamEventIR; 1]>`. The common case (one event per chunk) stays heap-free; multi-event chunks allocate exactly one heap entry. This is a breaking trait change; per user policy `feedback_breaking_changes_ok` no compatibility shim.

Codec implementations:
- OpenAI: today returns at most one — port unchanged but wrap in `SmallVec::from_iter`.
- Anthropic: today returns at most one — same.
- Gemini: detect the "combined final event" shape (text + `finishReason` in the same `candidates[0]`) and emit `[Delta, Finish]` as two events.

`crates/gw-proxy/src/filters/response.rs::handle_event` iterates the returned `SmallVec` instead of matching one.

`stream_accumulator.rs` already supports `IntoIterator<Item = &[u8]>` so its public API doesn't change; only the framing loop in `response.rs` does.

**Acceptance**:
- A Gemini chunk with text + `finishReason` decomposes into two `StreamEventIR` entries.
- A typical OpenAI / Anthropic chunk allocates 0 heap (assert with `smallvec::SmallVec::spilled() == false` in tests).
- The framing loop processes all events from one chunk before pulling the next chunk.

**Deferred to v2 (codec-s3)**: stateful cross-chunk accumulators (thinking signature continuation, tool_call delta merging) — those need the `StreamAccumSchema` design in ADR-S3-v2-1.

---

## 5. Reliability

### ADR-G11-1 — DLQ stream for billing flush failures (v1.5)

**Status**: NOT_STARTED. `crates/gw-ledger/src/flusher.rs:73-105` logs and counters on XADD failure but drops the record on the floor. The full three-layer fallback in `2026-05-19-billing-c-delta-stream-dlq.md` is the v2 target; this spec lands a single-Redis-stream fallback.

**Decision (v1.5)**: on `XADD usage:records` failure (per-record or batch-RTT), write the record to `usage:records:dlq` (separate Redis stream) with the same JSON payload plus a `dlq_meta` envelope:

```text
{
  "original": <UsageRecord JSON>,
  "dlq_meta": {
    "first_failure_at_unix_ms": <ts>,
    "attempts": <u32>,
    "last_error": "<short string>"
  }
}
```

Stream config: `XADD usage:records:dlq MAXLEN ~ 100_000` (approximate-trim, prevents Redis-side blowup).

No retry loop, no file fallback, no startup recovery in v1.5 — those are billing-c-δ scope. If `usage:records:dlq` itself fails, log + metric, drop. Operationally, Redis being completely down is a sev-1 anyway.

**Implementation site**: `crates/gw-ledger/src/flusher.rs::flush` — extend the error handling block (today around line 95-104) with a single `xadd_dlq` call on failure. Add a helper `dlq_xadd_with_trim(redis, payload)` next to the primary `xadd_usage_record`.

**Acceptance**:
- Primary XADD success → no DLQ writes.
- Primary XADD fail + DLQ success → DLQ stream receives wrapped record; `gw_usage_dlq_enqueued_total` increments.
- Primary XADD fail + DLQ fail → both `gw_usage_dlq_enqueued_total{outcome="error"}` and `gw_usage_records_lost_total` increment, no panic.
- DLQ stream length stays bounded under the MAXLEN even under sustained failure (criterion / loadgen).
- Lint compliance: `no_billing_records_stream` (forbids `billing:records`) and the new DLQ name `usage:records:dlq` do not collide — verified by adding a test pattern to `scripts/check-invariants.sh`.

**New metric**: `gw_usage_dlq_enqueued_total{outcome}`. The existing `gw_usage_records_lost_total` retains its meaning (records lost after **both** primary and DLQ fail).

**Deferred to v2 (billing-c-δ)**: in-memory retry, on-disk file fallback, periodic DLQ drainer, startup recovery, multi-replica DLQ consumer-group coordination.

---

## 6. Test matrix

Acceptance criteria above per ADR are necessary but not sufficient. The consolidated test matrix that gates the PR:

| Layer | Test target | What it asserts |
|-------|-------------|-----------------|
| Unit | `gw-auth::merge` | Markup chain includes `Model` entry sourced from Pricing. |
| Unit | `gw-auth::ip_filter::match_any_cidr` | IPv4/IPv6 match correctness, malformed-CIDR fail-closed. |
| Unit | `gw-auth::jwt::JwtValidator` | iss / aud / nbf / leeway pass + fail variants on client bearer path. |
| Unit | `gw-auth::signature::verify_body` | Wired through `request_filter` short-circuits per ADR-G7-1. |
| Unit | `gw-codec::*::parse_stream_chunk` | Returns `SmallVec` heap-free for single-event chunks. |
| Unit | `gw-codec::stream_accumulator` | Multi-event drain for Gemini combined final event. |
| Unit | `gw-routing::strategy::sticky` | moka TTL, capacity overflow, shared-state correctness. |
| Unit | `gw-routing::candidates` | Capability filter drops keys lacking required caps. |
| Unit | `gw-ledger::flusher` | Primary fail → DLQ stream receives wrapped record. |
| Integration | `gw-proxy::tests/expires_at` | Expired key → 401 `GW_AUTH_KEY_EXPIRED`. |
| Integration | `gw-proxy::tests/ip_allowlist` | Non-matching IP → 403 `GW_AUTH_KEY_IP_NOT_ALLOWED`. |
| Integration | `gw-proxy::tests/daily_quota` | Pre-check + post-increment + UTC day rollover. |
| Integration | `gw-proxy::tests/timeouts` | Vendor < Pricing precedence holds across all four fields. |
| Integration | `gw-proxy::tests/cross_protocol_stream` | OpenAI ↔ Anthropic ↔ Gemini stream chunks land in client's wire format. |
| Integration | `gw-proxy::tests/connect_retry` | Connect refused on first key → second key serves; both fail → 503. |
| Integration | `gw-proxy::tests/capability` | Vision request rejected when no key advertises `Vision`. |
| Bench | `gw-routing::strategy_pick::sticky` | P99 ≤ 100 µs per the spec budget. |
| Bench | `gw-codec::cross_protocol_stream` | Cross-protocol TTFT ≤ 1.5 × same-protocol baseline. |
| Invariants | `scripts/check-invariants.sh` | No new violations; `no_billing_records_stream` still green with `usage:records:dlq` added. |

The `#[ignore]`d full-flow harness (`crates/gw-gateway/tests/e2e_full_flow.rs`) is **not** required to pass in this spec — it stays gated on Docker. Per-filter harness `crates/gw-proxy/tests/e2e_failure_modes.rs` grows tests as listed above.

## 7. Migration & invariant impact

New invariants to add to `scripts/check-invariants.sh`:

| Lint | What it forbids |
|------|-----------------|
| `no_cost_in_dlq` | DLQ payload must contain a `UsageRecord`, not a `cost_micro`-bearing struct. |
| `no_global_hmac_force` | No workspace-wide HMAC enforcement boolean (per ADR-G7-2). |

No existing invariants flip from active to pending. `no_ir_runtime_struct` stays pending (Phase 2 trigger unchanged — the IR round-trip in ADR-G8-1 *does* construct IR per chunk, which makes Phase 2's schema rename more urgent, but doesn't change the lint's gating condition).

`docs/architecture.md` §9 must be updated when this spec lands — each gap moves from "deferred" to "implemented" or, where v1.5 is partial (G7-2 / G8-1 / G9-1 / G10-1 / G11-1), to "v1.5 lands minimum; v2 = <ref>".

## 8. Out of scope (consolidated)

- Non-stream response (`parse_response`) — still rejected with `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400. Codec-s4 v2 retracts.
- Cross-protocol non-stream response re-render — depends on the above.
- Cross-protocol response stream **direct wire codegen** — codec-s3 v2.
- FailoverExecutor (cross-vendor retry, response-side retry, configurable per ModelGroup) — routing-r-e v2.
- Three-layer DLQ (in-memory + disk + recovery) — billing-c-δ v2.
- Sticky cross-replica persistence — routing-r-g §out-of-scope.
- Schema rename `gw-core::ir` → `gw-core::schema` and removal of public constructors / `Default` / `Serialize`+`Deserialize` — Phase 2.
- Per-tenant JWT config, signature-policy enum, configurable replay window, configurable retry max — all v2 surfaces.
- Worker (gw-task / gw-task-runtime) — out of v1 scope; ADR-T16 guarantee unchanged.

## 9. Sequencing

The implementation plan (separate document) orders waves by dependency:

```
Wave 0  shared scaffolding: error codes, Wired bundle field additions,
        StrategySet struct, AuthConfig global eager-load helper

Wave 1  parallel:
        G1 (expires_at)    G2 (ip_allowlist)    G4 (timeout_overrides)
        G7 (HMAC wire)     G6 (JWT validator on client path)

Wave 2  parallel:
        G3 (daily_quota: store helper + filter checks + logging increment)
        G5 (model_markup: pricing pre-load + merge wiring)
        G12 (Sticky moka + shared state)
        G11 (DLQ stream)

Wave 3  parallel:
        G9 (parse_stream_chunk → SmallVec; codec impl updates)
        G10 (fail_to_connect hook + retry_blacklist threading)
        G13 (CapabilityResolver injection, EntityStoreResolveBackend,
              migration 0004)

Wave 4  serial after Wave 3:
        G8 (cross-protocol stream chunk re-render — needs G9 SmallVec
            for multi-event-per-chunk handling)

Wave 5  consolidation:
        docs/architecture.md §9 update
        scripts/check-invariants.sh new lints
        full test-matrix run
```

Wave breakdowns into concrete TDD tasks are in `docs/superpowers/plans/2026-05-21-known-gaps.md`.
