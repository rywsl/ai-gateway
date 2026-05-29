# Aether Gateway Coverage Gaps

Recorded 2026-05-29 from a multi-agent feature-coverage diff of the Rust gateway against the Aether reference
implementation (Python `src/` data plane), **excluding website / frontend / control-plane CRUD**. Each Aether
data-plane capability was classified COVERED / PARTIAL / MISSING / out-of-scope; only PARTIAL and MISSING items
that are genuine data-plane gaps are recorded here. Findings were verified against the Rust source; severity
reflects estimated user/operator impact.

The two HIGH items (§1, §2) are also security/functional defects and are cross-referenced from
[`2026-05-29-functional-audit-findings.md`](2026-05-29-functional-audit-findings.md). Capabilities intentionally
delegated to Go (cost computation, billing math, control-plane CRUD, TPM/daily/monthly quota) are **not** gaps and
are omitted.

Sort order is by severity (HIGH → LOW).

---

## Remediation status (2026-05-30)

Fixed on branch `fix/audit-remediation-20260530` (staged commits per wave).

| # | Gap | Status | Wave |
|---|-----|--------|------|
| 1 | Client JWT revocation blacklist not consulted | **Fixed** | Wave 1 (jti extracted; `check_jwt` FailClosed; `GW_AUTH_JWT_REVOKED`) |
| 2 | Disabled key / banned user not enforced | **Fixed** | Wave 1 (= functional-audit §1) |
| 3 | Upstream auth only injected Authorization (no x-goog-api-key) | **Fixed** | Wave 5 (auth_template header-name parsing) |
| 4 | Direct-path per-key proxy ignored | **Fixed (fail-closed)** | Wave 5 (pingora 0.8 Proxy is UDS-only → `GW_UPSTREAM_PROXY_UNSUPPORTED`; use Impersonate) |
| 5 | Gemini tool-call id dropped | **Fixed** | Wave 4 (functionResponse.name from id→name map) |
| 6 | No `/v1/embeddings` endpoint | **Deferred** | Blocked on a Go capability contract (which vendor keys are embeddings-capable); current safe-400 preserved |
| 7 | prompt-cache key injection | **Deferred** | OFF-by-default optimization; CacheAffinity + body_rules cover locality; avoided risky body mutation |
| 8 | Error passthrough all-or-nothing | **Fixed** | Wave 6 (substring allow/deny rule engine; also wired `enabled` from config) |
| 9 | No vendor-specific rate-limit header parsing | **Fixed** | Wave 5 (`rpm_limit_header` + `detect_rpm_kind`) |
| 10 | No endpoint-family priority ordering | **Fixed (folded)** | Wave 3 (deterministic first-accessible via `resolve_upstream_format`) |

Eight of ten fixed; §6 (embeddings) and §7 (prompt-cache) are deferred with rationale above — both are
LOW-severity net-new features that are safe today and whose hasty implementation would risk regressions
(the explicit "don't fix one, break another" constraint). Each remains actionable as its own follow-up.

---

**Status**: open · **Severity**: HIGH (security / fail-open) · **Verified by hand**

**Aether reference**: `src/.../jwt_blacklist.py` (features 25/26) checks client access/refresh tokens against a
Redis blacklist (hashed token, TTL = exp).

**Rust gap**: `crates/gw-proxy/src/filters/request.rs:236-273` — the client JWT branch only runs
`validate_sub` / legacy `verify_jwt` and returns a `UserId`; it never probes any blacklist. `RedisJwtBlacklist::check_jwt`
(the `jwt:blacklist:` namespace) **exists** but is wired only to the `/admin/*` mgmt branch (`request.rs:147`) and
healthz admin verify. `JwtValidator::validate_sub` (`crates/gw-auth/src/jwt.rs:241`) extracts only `{sub}` and never
reads `jti`, so even if wired it could not look up a revocation. The error code `GW_AUTH_JWT_REVOKED` does not exist
in the catalog, while `GW_AUTH_MGMT_TOKEN_REVOKED` does — underscoring that only the mgmt path got the check.

**Normal trigger**: a client presents a signature-valid JWT that Go added to `jwt:blacklist:<jti>` (on logout /
admin revocation) but that is still inside its `exp` window.

**Why it matters**: Revoked / logged-out client JWTs keep authenticating until `exp`. This is a data-plane auth gap,
not a Go delegation — admit-time revocation enforcement is intrinsically the gateway's job. The contract
(`docs/go-contracts/parts/01 §1.6`, `main.md:30`) explicitly designates Rust as the reader: after JWT validation,
do an `EXISTS` on the blacklist; hit → 401; Redis unreachable → **fail-closed** (default). The code is fail-open,
violating the explicit fail-closed default.

**Fix sketch**: extract `jti` in `validate_sub` (or alongside it), call `check_jwt(jti, FailClosed)` on the client
JWT branch mirroring the mgmt path at `request.rs:147`, add a `GW_AUTH_JWT_REVOKED` code, and return 401 on a hit.

**Tests**: add a request-path test seeding `jwt:blacklist:<jti>` and asserting a still-unexpired JWT with that jti
is rejected; plus a Redis-down test asserting fail-closed.

---

## 2. Disabled API keys / banned users are authenticated and admitted (status fields never enforced)

**Status**: open · **Severity**: HIGH (security / fail-open) · **Verified by hand**

**Aether reference**: `src/services/auth/service.py::authenticate_api_key_threadsafe` (feature 24) performs
active/locked/expiry/ownership checks.

**Rust gap**: this is the coverage-angle view of functional defect §1 in
[`2026-05-29-functional-audit-findings.md`](2026-05-29-functional-audit-findings.md). `verify_apikey`
(`crates/gw-auth/src/apikey.rs:10-22`) only computes a SHA-256 and constant-time-compares the hash; it never reads
`ApiKeyEntry.status`. `ApiKeyStatus{Active,Disabled}` and `UserStatus{Active,Banned}` are never matched on the
request path; `merge_permissions` loads the user but does not check status; `CircuitReason::BannedUser` /
`USER_CIRCUIT::Open` have no write site (dead logic). The status field exists in the Rust IR — it is **not** a schema
delegation to Go — but the admit-time gate is entirely absent.

**Normal trigger**: control plane sets `status='disabled'` / `'banned'` with `deleted_at` still NULL and emits
`events:entity (op=upsert)`; after invalidation the cold load returns the row (with status) via
`WHERE id=$1 AND deleted_at IS NULL`, and the data path never reads status.

**Why it matters**: disable/ban cannot be enforced in-band; leaked keys and abusive users keep being served until the
row is physically deleted. Fix is shared with functional-audit §1.

---

## 3. Upstream auth only injects an `Authorization` header — no Gemini `x-goog-api-key` / query-key support

**Status**: open · **Severity**: MEDIUM (native-Gemini upstream auth fails; depends on Go's `auth_template` shape)

**Aether reference**: `src/services/provider/auth.py::get_auth_handler` (feature 82) dispatches 5 upstream auth
methods: Bearer / x-api-key / GoogApiKey / QueryKey / OAuth2.

**Rust gap**: `crates/gw-proxy/src/filters/upstream.rs:1004-1008` — `inject_vendor_headers` hardcodes the result of
`auth_template.replace("{api_key}", ...)` into the `authorization` header. The Vendor entity carries a single
`auth_template: String` with no auth-method / header-name selector. `x-goog-api-key` appears zero times in the main
proxy path (only the separate `gemini_files` reqwest gateway uses `?key=`). Note: the contract intends `auth_template`
to encode the header name (e.g. `"x-api-key: {api_key}"`), but the implementation never parses a header name and
hardcodes `authorization` — so a contract-shaped `"x-goog-api-key: ..."` template would produce a malformed
`Authorization: x-goog-api-key: ...`.

**Normal trigger**: deploy a native Gemini vendor (`ProtocolKind::Gemini`, `generateContent`) and route Gemini
requests. No `auth_template` value can produce an `x-goog-api-key` header or a `?key=` query param.

**Why it matters**: native Gemini upstreams authenticate via `x-goog-api-key` / `?key=`, not Bearer; the gateway puts
the Gemini key in `Authorization` and Google rejects it — main-path Gemini chat/generate upstream auth fails, and it
cannot be fixed by config alone. MEDIUM because actual exposure depends on the `auth_template` shape Go actually
writes.

**Fix sketch**: parse the header name from `auth_template` (split on the first `:`) and inject under that header,
and support a query-key form (`?key=`) for Gemini; or add an explicit auth-method enum on the Vendor entity.

---

## 4. Direct rustls upstream path ignores per-key / per-vendor HTTP proxy

**Status**: open · **Severity**: MEDIUM (proxy silently bypassed in the default egress mode)

**Aether reference**: `src/services/proxy_node/resolver.py` (features 92-93) resolves an effective proxy
(system → ops → per-key / per-endpoint) and applies it to the upstream connection.

**Rust gap**: `crates/gw-proxy/src/filters/upstream.rs:1046-1049` — `VendorKey.proxy` is read only in
`inject_egress_headers`, and only when `ctx.egress_profile` is `Some` (Impersonate mode), to inject `X-Egress-Proxy`.
The default Direct rustls path (`HttpPeer::new`, `upstream.rs:439/484`) never applies a proxy, and `Vendor.default_proxy`
is read zero times in production. pingora 0.8's `HttpPeer::new_proxy(...)` exists but is not wired.

**Normal trigger**: a VendorKey/Vendor configured with `proxy` / `default_proxy` while `egress_mode` is the default
`Rustls`. The cold load fills the proxy field, but the Direct path reads neither `picked.proxy` nor `Vendor.default_proxy`.

**Why it matters**: in the common non-impersonate case, a vendor key configured with a proxy connects directly,
silently bypassing it, with no system/ops fallback. Per-key proxy is honored only in explicit Impersonate mode — a
data-plane divergence from Aether's fingerprint-independent proxy resolution. MEDIUM: a connection/routing divergence,
not a correctness/security/billing defect.

**Fix sketch**: in the Direct path, resolve the effective proxy (per-key → vendor default → system/ops) and build
the peer with `HttpPeer::new_proxy` when one is present.

---

## 5. Tool-call IDs are dropped when down-translating tool results to Gemini

**Status**: open · **Severity**: LOW (narrow: Gemini cross-protocol multi-turn tool use)

**Aether reference**: `src/.../registry.py::_repair_internal_tool_call_ids` (feature 17) reassigns/maps tool_call IDs
across formats to correlate calls with responses.

**Rust gap**: `crates/gw-codec/src/gemini/via.rs:119-137, 320, 383` — down-translating `MessageIR::Tool` →
Gemini `functionResponse` explicitly drops `tool_call_id` and emits `name: Cow::Borrowed("")`; inbound parse uses the
function name as the id. There is no stateful cross-IR id-repair table (`grep repair` → no hits). An in-code comment
claiming "Gemini carries no per-call id" is factually wrong — Gemini `functionResponse.name` is required and supports
an optional id.

**Normal trigger**: an OpenAI/Anthropic-shaped multi-turn tool-call conversation routed to a Gemini upstream triggers
cross-protocol re-render; the `Tool` arm of `TryFrom<&RequestIR> for GeminiRequest` emits `name: ""`, dropping the
tool_call_id.

**Why it matters**: multi-turn tool use through Gemini cannot reliably correlate a `functionResponse` back to its
originating call (the name is blanked instead of mapped from the recorded tool_call_id). Aether maintains a consistent
mapping; Rust does not. LOW — narrow scope, adjacent to the project's accepted lossy-cross-render posture.

**Fix sketch**: carry the originating tool name (and id) through the IR so the Gemini `functionResponse.name` is
populated from the recorded call rather than blanked.

---

## 6. No image/video request format conversion; no embeddings endpoint

**Status**: open · **Severity**: LOW (embeddings is a real but cleanly-rejected gap)

**Aether reference**: `convert_video_request` / `convert_video_task` (feature 12) and `_detect_endpoint_type`
classifying chat/image/video/embeddings (feature 1).

**Rust gap**: image/video are routed through the `task` subsystem as opaque payloads (no codec normalization /
cross-format conversion), and the task subsystem is off by default (`[task].enabled=false`). Embeddings has zero
presence (`grep` only hits an unrelated JSON-escape helper in `health.rs`). `POST /v1/embeddings` matches no CLI shape
or `AnyCodec::detect` and short-circuits to a 400 `GW_REQUEST_PROTOCOL_UNKNOWN` at `crates/gw-proxy/src/filters/early.rs:147-165`.

**Why it matters**: image/video cross-format conversion is arguably out-of-scope for a chat-IR gateway (image already
flows as opaque passthrough). But **embeddings is a genuine data-plane gap**: the v1 design spec
(`docs/superpowers/specs/2026-05-18-llm-gateway-design.md:1070`) lists `/v1/embeddings` as "v1 mandatory", yet there
is no detection / codec / routing and no descope record in-repo. LOW because the effect is a clean 400 on an
unsupported endpoint (no crash / corruption / security issue).

**Fix sketch**: either add `/v1/embeddings` detection + passthrough routing (it needs no chat-IR conversion), or
record an explicit descope decision so the spec and code agree.

---

## 7. Prompt-cache key injection (stable deterministic cache key) is not implemented

**Status**: open · **Severity**: LOW (optimization increment over existing cache affinity)

**Aether reference**: `src/services/provider/prompt_cache.py::maybe_patch_request_with_prompt_cache_key` (feature 102)
injects a deterministic `prompt_cache_key` into OpenAI/Codex request bodies so consecutive requests from the same
client family hit the same upstream cache.

**Rust gap**: `grep prompt_cache` / `maybe_patch` / `cache_key` → zero hits in production modules. The Rust upstream
path does model rewrite + set-if-absent `body_rules` + `forced_params` only.

**Why it matters**: cache locality is partly covered by the `CacheAffinity` routing strategy (hashes
`model || message_hash` to pin same-prefix requests to the same upstream key) and static `[body_rules]` overrides.
What is missing is the OpenAI/Codex-specific dynamic per-client-family key — an optimization increment over the
existing locality mechanism, with no correctness/security/billing impact. LOW.

**Fix sketch**: if upstream cache hit-rate matters for OpenAI/Codex, add a body-mutation step that injects a
deterministic `prompt_cache_key` derived from the client family + prompt prefix.

---

## 8. Error passthrough is a single global switch — no per-pattern passthrough rules

**Status**: open · **Severity**: LOW (configurability, not safety — masks by default)

**Aether reference**: `src/core/error_utils.py` / `error_passthrough.py::match_error_passthrough_rule` /
`_compile_patterns` (features 119-120) use configurable per-pattern rules to decide which upstream errors pass through
vs. are masked, plus `_sanitize_message` / `_extract_labelled_message`.

**Rust gap**: `crates/gw-proxy/src/gateway.rs:884-885` — the passthrough decision is a single boot-time boolean
(`if self.error_passthrough_enabled { ... }`) applied uniformly to every ≥400 upstream error. There is no pattern set,
no per-rule allow/deny, and no `[error.passthrough]` rule table (only `enabled` is exposed). `extract_message` reads
fields verbatim with no sanitization equivalent.

**Why it matters**: operators cannot selectively expose specific error patterns (e.g. quota/billing messages) while
masking others (e.g. internal stack traces / vendor identifiers) — it is all-or-nothing. Mitigated: default is
`false` (mask-by-default), and when enabled, `render_error_to_client` rebuilds the client-protocol envelope keeping
only one extracted `message` string (arbitrary upstream fields/headers/stack structure are already dropped by the
structured re-render). So it is a configurability enhancement, not a safety hole. LOW.

**Fix sketch**: add an optional `[error.passthrough]` rule table (ordered allow/deny patterns) and a sanitization
step, defaulting to today's global behavior when unset.

---

## 9. No vendor-specific rate-limit header parsing — only generic `limit` + `retry-after`

**Status**: open · **Severity**: LOW (AIMD learner fidelity; core learning works)

**Aether reference**: `src/.../detector.py` (feature 54) parses vendor-specific rate-limit headers
(`anthropic-ratelimit-*`, `x-ratelimit-remaining-*` / `-reset`, `tokens-remaining`) plus `Retry-After`.

**Rust gap**: `crates/gw-proxy/src/gateway.rs:790-796` — the adaptive 429 hook reads only `x-ratelimit-limit` and
`retry-after`; the `DefaultErrorAnalyzer` fallback table adds only generic-limit aliases. `grep anthropic-ratelimit` /
`x-ratelimit-remaining` / `tokens-remaining` → zero hits. The `RateLimitKind` enum exists but every production
construction hardcodes `Unknown`. The AIMD learner core itself is complete.

**Why it matters**: the learner ingests a single generic signal and cannot read Anthropic request/token limit headers
or OpenAI remaining/reset, so the learned ceilings and cooldown timing are coarser than Aether's. Note: TPM is
explicitly out-of-scope for v1 and daily/monthly is Go-delegated (RL-Core spec), so the token-header part is an
intentional deferral; request-rate (RPM) vendor-header parsing + 429 `RateLimitKind` classification is in-scope but
missing. LOW — fidelity gap only; the AIMD core works.

**Fix sketch**: parse the vendor-specific RPM headers into the existing `observe_429` signal and classify
`RateLimitKind` from them instead of hardcoding `Unknown`.

---

## 10. No endpoint-family priority ordering for multi-protocol keys — selection is "prefer client codec, else first"

**Status**: open · **Severity**: LOW (sub-optimal target family; traffic still succeeds)

**Aether reference**: `src/services/scheduling/candidate_builder.py::_sort_endpoints_by_family_priority` (feature 65)
orders a key's supported endpoints by API-family priority.

**Rust gap**: `crates/gw-proxy/src/filters/upstream.rs:759-762` — upstream-protocol selection is
`if accessible_endpoints.contains(client_codec.kind()) { client } else { accessible_endpoints.first() }`: prefer
same-protocol, else take the first Vec element, with no priority sort. `candidates.rs:57` admits purely on
membership. `grep family_priority` / `sort_endpoints` → zero hits. The target protocol is decided purely by DB JSONB
array insertion order (`loader.rs:269`, no sort).

**Normal trigger**: an admitted `protocol_conversion=true` key whose `accessible_endpoints` does not include the
client codec and which has multiple endpoints hits the else-first branch.

**Why it matters**: for a multi-protocol key with no client match, the target protocol is chosen by Vec insertion
order rather than an intentional family preference, possibly routing to a less-optimal upstream endpoint family than
Aether would. Same-protocol preference (the dominant case) is covered, and traffic still succeeds — just possibly to
a sub-optimal family. LOW.

**Fix sketch**: add a family-priority ordering (config- or const-driven) applied to `accessible_endpoints` before the
else-first fallback.
