# Functional Audit Findings

Recorded 2026-05-29 from a multi-agent correctness/panic audit of the Rust gateway (no code changed). Each
finding was adversarially verified by reading the cited source; the five highest-stakes items (§1, §3, §6,
§13, plus the JWT-revocation gap in [`2026-05-29-aether-coverage-gaps.md`](2026-05-29-aether-coverage-gaps.md))
were additionally spot-checked by hand. Severity reflects estimated user/operator impact, not engineering effort.

These are **newly discovered** defects — none was in `docs/issues/` or `docs/architecture.md` §Deliberate behaviours
/ §Known gaps at audit time. Coverage gaps vs the Aether reference implementation live in the sibling file
[`2026-05-29-aether-coverage-gaps.md`](2026-05-29-aether-coverage-gaps.md).

Sort order is by severity (HIGH → LOW), not by impact area.

---

## Remediation status (2026-05-30)

Fixed on branch `fix/audit-remediation-20260530` (staged commits per wave). Each section below
corresponds to a row here.

| # | Finding | Status | Commit / wave |
|---|---------|--------|---------------|
| 1 | Banned users / disabled keys never rejected | **Fixed** | Wave 1 (`fix(auth): admit-time security gates`) |
| 2 | Response-side failover under-count + inflight clobber | **Fixed** | Wave 2 (`fix(billing): settle prior attempt …`) |
| 3 | Anthropic stream usage clobber | **Fixed** | Wave 2 |
| 4 | Half-open circuit never recovers | **Fixed** | Wave 3 (`fix(routing,health): …`) |
| 5 | Cross-protocol health-bucket mismatch | **Fixed** | Wave 3 |
| 6 | HMAC empty-secret fail-open | **Fixed** | Wave 1 |
| 7 | OAuth `access_expires_at` decode mismatch | **Fixed** | Wave 5 (`fix(store,egress,proxy): …`) |
| 8 | Adaptive-RPM enforcement dead code | **Fixed** | Wave 3 (relocated to `pick_peer::try_admit`) |
| 9 | Cross-protocol content-encoding strip | **Fixed** | Wave 4 (`fix(codec,proxy): …`) |
| 10 | OpenAI Responses non-stream thinking dropped | **Fixed** | Wave 4 |
| 11 | multi_dim preset panic | **Fixed** | Wave 3 (fail-operational fallback) |
| 12 | PG acquire-timeout false not-found | **Fixed** | Wave 5 (500ms + PoolTimedOut metric) |
| 13 | Egress classifier proxy-timeout mislabel | **Fixed** | Wave 5 (reorder) |

All thirteen are remediated. Broader normal-path panic hardening beyond the multi_dim panic (§11) was
done as the **Wave 6b sweep** (commit `c510910`): a 24-agent read-only triage of **138** production
panic-capable sites; adversarial verification rejected 13/15 candidates as provably safe and confirmed
**2** reachable normal-path panics — both now fixed:
- `gw-auth/.../adaptive.rs::try_admit` — a TOCTOU race introduced by Wave 3's adaptive-RPM enforcement
  (insert-then-separate-get + concurrent `evict` → worker panic); fixed with an atomic DashMap `entry()`.
- `gw-ledger/.../lease.rs::now_ms` — pre-epoch clock `expect()`; fixed with `unwrap_or_default()` (fail-closed).

The low confirmed count reflects that the codebase was already largely panic-safe (locks / `OnceCell` /
const regex / length-checked indices).

---

## 1. Banned users / disabled API keys are never rejected on the business request path

**Status**: open · **Severity**: HIGH (security / fail-open) · **Verified by hand**

**What**: The business auth branch in `crates/gw-proxy/src/filters/request.rs:274-348` checks the api-key hash
(`verify_apikey`), expiry (`check_api_key_expiry`), and signature requirement, then resolves `user_id` — but it
**never reads** `ApiKeyEntry.status` (`ApiKeyStatus::Disabled`) or `UserEntry.status` (`UserStatus::Banned`).
A repo-wide grep shows both enums are defined in `entity.rs` and read only in tests/benches; production request
handling reads them zero times. The loader SQL (`crates/gw-store/src/loader.rs:172-175`) filters only on
`deleted_at IS NULL`, so a row whose status changed but was not soft-deleted loads and authenticates normally.
`CircuitReason::BannedUser` / `USER_CIRCUIT::Open` have no write site — dead logic.

The management-token path **does** enforce status (`crates/gw-auth/src/mgmt.rs:118`:
`if !matches!(entry.value.status, KeyStatus::Active)`), which confirms the business-path omission is an oversight,
not an intentional delegation.

**Normal trigger**: Ops (or Go's auto-ban) sets a user to `banned` / a key to `disabled` via `op=status_change`
(`deleted_at` stays NULL — Go's contract distinguishes `op=delete` → soft-delete → 401 from `op=status_change` →
row retained: `docs/go-contracts/main.md:27`, `parts/01:120`). Rust invalidates its cache, cold-loads the new row
(status=banned/disabled), then authenticates and admits the request.

**Why it matters**: Disable/ban actions from the control plane have **no in-band effect** on the data plane until
the row is physically deleted. Leaked or abused keys, and abusive users, keep being served.

**Fix sketch**: in the business auth branch, after `apikey_or_load`, reject `ApiKeyStatus::Disabled` (→
`GW_AUTH_INVALID_APIKEY` or a dedicated `GW_AUTH_KEY_DISABLED`) and reject the resolved user's
`UserStatus::Banned` (→ a dedicated code; wire the dormant `CircuitReason::BannedUser`). This is also tracked from
the coverage angle as §3.2 in the sibling doc.

**Tests**: none. Add a request-path test seeding a `disabled` key / `banned` user (with `deleted_at` NULL) and
asserting rejection.

---

## 2. Response-side failover never bumps `attempt_index`, emits no prior-attempt UsageRecord, and overwrites the inflight billing key

**Status**: open · **Severity**: HIGH (billing under-count + crash-recovery corruption)

**What**: In `crates/gw-proxy/src/gateway.rs:751-782` (egress arm) and `799-849` (5xx/429 arm), the
`response_filter` retry arms reset routing state and bump `ctx.failover_attempt`, but **never** touch
`ctx.attempt_index`, never call `on_retry_sync` / `build_attempt_record`, never `mark_settled` the failed attempt,
and never reset `ctx.usage` / `ctx.vendor_exposure`. On retry, `upstream_request_filter` calls
`inflight.register(...)` with the **unchanged** `attempt_index` (`gateway.rs:575`), so the new vendor key's inflight
hash HSETs over the **same** Redis key `billing:inflight:{id}:{request_id}:{attempt_index}`
(`crates/gw-ledger/src/inflight.rs:59-83`), clobbering the prior attempt's record. The connect-failure path
(`fail_to_connect` → `on_retry_sync`, `billing_lifecycle.rs:217/230`) is the correct in-repo reference.

**Normal trigger**: every response-side failover. A client sends a well-formed request to a model group with ≥2
candidate keys; the upstream returns a retryable 503/429 **before** the body starts; `classify_for_failover` →
`Retry(new_key)`; the retry arm fires. At that point `ctx.vendor_exposure == Sent`, so the first attempt genuinely
warranted a UsageRecord that is never produced.

**Why it matters**: Reproduces exactly the L6 under-billing that spec `2026-05-24-billing-robustness` §5.6
(AttemptManifest) set out to fix. The inflight crash-recovery record is silently overwritten so the W4 reconciler
cannot recover it, and Go's `(request_id, attempt_index)` dedup collapses both attempts into one (both carry
`attempt_index=0`).

**Fix sketch**: make the response-side retry arms perform the same AttemptManifest sequence as `on_retry_sync`:
force-Terminate, emit the prior attempt's UsageRecord + `mark_settled`, then `attempt_index += 1` before re-register.

**Tests**: `tests/billing_lifecycle.rs` covers only `on_retry_sync` (connect path); add a response-side-failover
test asserting `attempt_index` increments and a prior-attempt UsageRecord is emitted.

---

## 3. Anthropic streaming usage: terminal `message_delta` clobbers `input_tokens` and `cached_tokens` to zero

**Status**: open · **Severity**: HIGH (systematic billing under-count on a core path)

**What**: In `crates/gw-proxy/src/filters/response.rs:409-413`, the `StreamEventIR::Usage` arm **unconditionally**
overwrites all three `ctx.usage` fields. Anthropic SSE delivers usage in two frames: `message_start` carries
`input_tokens` + `cache_read_input_tokens`, and the terminal `message_delta` carries **only** `output_tokens`.
`AnthropicUsage.input_tokens` is `#[serde(default)]` and `cache_read_input_tokens` is `Option`
(`crates/gw-codec/.../anthropic/wire.rs:294-299`), so the `message_delta` frame deserializes them to `0`/`None`.
Each frame is its own `handle_event` call, so the later `message_delta` sets `input_tokens=0` and
`cached_tokens=None`. The post-pass guards in `apply_extracted_to_ctx` (`response.rs:528-540`, `>0` / `is_some()`)
cannot restore them because the extractor also yields `0`/`None` for that frame. `logging.rs:283-285` reads
`ctx.usage` straight into the UsageRecord with no max/merge.

**Normal trigger**: any `"stream": true` Anthropic `/v1/messages` request to an Anthropic upstream (same-protocol
streaming — the dominant Anthropic path). Non-stream Anthropic and OpenAI/Gemini streaming are unaffected (OpenAI
sends usage once at the end; Gemini sends cumulative `promptTokenCount` on every chunk).

**Why it matters**: Every Anthropic streaming response is recorded with `input_tokens=0` / `cached_tokens=None`.
Go bills from `usage:records`, so input tokens (the dominant cost for long-context / cached requests) and cache-read
accounting are systematically lost.

> Note: the server-tool / web-search `message_delta` variant *does* include cumulative `input_tokens` and is
> self-correcting; the standard text / tool-call streaming path is the broken majority.

**Fix sketch**: in the `Usage` arm, merge rather than overwrite — `input_tokens` and `cached_tokens` should take
`max(existing, incoming)` (or only overwrite when incoming `> 0` / `is_some()`); `output_tokens` is correct to
overwrite (cumulative).

**Tests**: add a streaming-usage test feeding `message_start{input_tokens:N,...}` then `message_delta{output_tokens:M}`
and asserting the emitted UsageRecord retains `input_tokens=N`.

---

## 4. Half-open circuit never recovers from a failed recovery probe — key permanently sidelined

**Status**: open · **Severity**: HIGH (availability; clearable only by restart)

**What**: In `crates/gw-health/src/format_health.rs:95-127`, `record(ok)` transitions `HalfOpen → Closed` only on
success. On a **failed** probe (`ok=false`): the recovery branch is skipped, and the trip branch
(`total >= 20 && rate > 0.5`) cannot fire because after the 60 s `OPEN_HOLD_MS` the 30 s sliding window holds ~1
sample. The circuit is left stuck in `HalfOpen`, and `is_available()` (`66-89`) returns `false` for `HalfOpen`
unconditionally. The only production circuit transitions are `Closed→Open`, `Open→HalfOpen`, `HalfOpen→Closed` —
there is **no `HalfOpen→Open`**. `clear_cooldown` explicitly does not touch the circuit, and the quota probe only
probes keys with an active cooldown deadline, so neither recovers a HalfOpen-stuck key. This diverges from the K-A
spec (`docs/spec/03-key-health.md`: "半开失败一次就回 open 60s").

**Normal trigger**: a bad upstream minute (≥20 samples, >50% failures) trips the circuit Open for 60 s; after 60 s
the first request wins the half-open CAS and is routed to the still-degraded upstream; that one request fails; the
circuit is stuck `HalfOpen` for the process lifetime. A 30 s cooldown from the 5xx classifier masks it for 30 s,
after which `is_available()` still returns `false`.

**Why it matters**: A single failed probe permanently removes a `(key, format)` from selection even after the
upstream fully recovers. Capacity erodes key-by-key under transient incidents; with one key per model group it
becomes a hard `GW_ROUTING_NO_AVAILABLE_CHANNEL` outage clearable only by restart.

**Fix sketch**: on a failed probe while `HalfOpen`, re-Open the circuit and re-arm `open_until_ms` (add the missing
`HalfOpen→Open` transition), per the spec.

**Tests**: `tests/key_health.rs` covers admission only; add a failed-probe test asserting re-Open and eventual
recovery.

---

## 5. Circuit breaker and cooldown are bypassed for protocol-conversion (cross-protocol) vendor keys

**Status**: open · **Severity**: HIGH (availability protection inert for a traffic class)

**What**: Key health is bucketed per `(KeyId, ApiFormat)`. Selection reads the **client** format bucket
(`PickCtx.api_format = client_codec_kind`, `crates/gw-proxy/src/filters/upstream.rs:301`; health gate
`pipeline/mod.rs:199`), but outcomes are recorded under the **upstream** format
(`logging.rs:187-200`, `api_fmt = ctx.upstream_codec.kind()`; the cooldown listener at `logging.rs:447` likewise).
For a protocol-conversion key (`accessible_endpoints=[Anthropic]`, `protocol_conversion=true`) serving an OpenAI
client, `upstream_kind = accessible_endpoints.first() = Anthropic` (`upstream.rs:760-766`), so selection consults
`(key, OpenAi)` while every success/failure/cooldown/circuit-trip is written to `(key, Anthropic)`. The
`logging.rs` comment "record currently ignores api_format in v1" is factually wrong — `store.rs` selects the
`FormatHealth` bucket by `fmt` for both read and write.

**Normal trigger**: any model group routed through protocol-conversion keys where the client format is absent from
`accessible_endpoints` — a normal, supported config.

**Why it matters**: The cross-request circuit breaker and upstream-driven cooldown (429/5xx Retry-After) accumulate
in a bucket the picker never reads, so degraded cross-protocol keys keep being chosen. (In-request `FailoverExecutor`
still retries within a single request, so no single request is pinned to a dead key — hence HIGH, not critical — but
the persistent protection is a no-op for this class.)

**Fix sketch**: resolve `upstream_kind` before the pick and use that single dimension for both `is_available` and
`record` (or normalize to one per-key health dimension).

**Tests**: add a cross-protocol-key test asserting a cooldown/trip recorded after a 429 is visible to the next
selection.

---

## 6. HMAC signature verification treats an empty secret as a valid key — signatures forgeable when required-but-unset

**Status**: open · **Severity**: HIGH (security / fail-open) · **Verified by hand**

**What**: When signing is required, `crates/gw-proxy/src/filters/request.rs:317` sets
`ctx.signature_secret = entry.value.signature_secret.clone().unwrap_or_default()` — `None` becomes an empty `Vec`.
The empty secret flows into `crates/gw-auth/src/signature.rs::verify`, where `Hmac::<Sha256>::new_from_slice(&[])`
returns `Ok` (HMAC has no minimum key length), so a MAC is computed over `<method>\n<path>\n<timestamp>\n<sha256(body)>`
with the empty key and compared via `ct_eq`. All four signing-string inputs are client-known/controllable, so a
client can forge a `Valid` result using `key=""`. (Confirmed by a temporary probe test — passed, then reverted.)

**Normal trigger**: (a) global `[signature].required = true` while any key has `signature_secret = NULL`; or
(b) a single row with `signature_required = true` and `signature_secret = NULL`. No guard rejects the
"required-but-no-secret" combination.

**Why it matters**: The design is fail-closed (DB-20: "populate `signature_secret` before enabling HMAC"), but the
code is fail-open. Ops believe signing is enforced while every secret-less key is forgeable by any client; replay /
tamper protection is fully defeated.

**Fix sketch**: treat an empty/absent secret while signing is required as a hard configuration error → reject the
request (`GW_AUTH_*`, fail-closed) rather than verifying against the empty key. Optionally validate at load time.

**Tests**: add a test that required + NULL secret rejects (not `Valid`) for an attacker-computed empty-key MAC.

---

## 7. OAuth vendor-key `access_expires_at` decode mismatch: Rust expects a Unix integer, the Go contract publishes an RFC3339 string

**Status**: open · **Severity**: HIGH (whole-row cold-load failure for OAuth keys)

**What**: `crates/gw-store/src/loader.rs:100-112` declares `OAuthCredentialsRow.access_expires_at` with
`#[serde(with = "time::serde::timestamp")]`, which deserializes from a JSON **number**
(`OffsetDateTime::from_unix_timestamp(<i64>)`). But the Go→Rust contract
(`docs/go-contracts/parts/03-shared-schema.md:197`, `contract-change/2026-05-22-go-onboarding.md:325`) specifies an
RFC3339 **string** (`"2026-05-22T12:00:00Z"`). The in-tree unit test hand-feeds `1893456000_i64`, validating the
opposite of the documented shape.

**Normal trigger**: ordinary cold-load of an OAuth-type `vendor_keys` row. If Go writes the documented string,
`serde_json::from_value::<VendorKeyRow>` fails for the whole row ("invalid type: string, expected i64") and
`with_cold_load_timeout` collapses it to `None`.

**Why it matters**: Any OAuth vendor key written per contract (Claude Code OAuth / Gemini OAuth) fails to load
entirely — even its static fields are lost — so the key is invisible to candidate selection and requests routed to
an OAuth-only vendor return `GW_ROUTING_NO_AVAILABLE_CHANNEL`.

**Fix sketch**: deserialize `access_expires_at` from RFC3339 (`time::serde::rfc3339`) to match the contract, or
accept both forms; update the misleading test to use the documented string. Confirm the live Go writer's actual
shape before choosing.

**Tests**: change the loader test fixture to the contract's RFC3339 string and assert a successful decode.

---

## 8. Adaptive-RPM enforcement (`should_enforce`) is dead code — gated on `ctx.picked_vendor_key`, which is always `None` in `request_filter`

**Status**: open · **Severity**: MEDIUM (feature silently disabled; misleading metrics)

**What**: `crates/gw-proxy/src/filters/request.rs:1118-1123` clamps the RPM limit only inside
`if let Some(key) = ctx.picked_vendor_key.as_ref() { ... should_enforce(...) }`. But `picked_vendor_key` is set only
in `pick_peer` (`upstream.rs:517`, the `upstream_peer` phase, **after** `request_filter`), and `request_filter`
runs once and does not re-run on failover. So at line 1119 `picked_vendor_key` is structurally `None` and the
clamp body never executes. `request.rs:1120` is the sole production caller of `should_enforce`. The learning half
(`observe_429`, in `response_filter`) is wired, so the manager learns a lower ceiling that is never applied.

**Normal trigger**: every request's step 8.

**Why it matters**: The proactive learned back-pressure feature is silently off; the gateway keeps admitting at the
configured RPM after the manager has learned a lower ceiling, continuing to drive 429s into the overloaded key. The
`_learned_value` metric rises while `_enforced_total` never increments — misleading. MEDIUM because the base merged
RPM and response-side cooldown still provide partial protection. (Secondary nuance: the RPM gate keys on the merged
user/api-key bucket while `should_enforce` keys on vendor `key_id`, so the two key-spaces are cross-cutting even if
the branch were reachable.)

**Fix sketch**: move the adaptive clamp to where the vendor key is known (`upstream_peer`/`pick_peer`), or key the
adaptive manager by something available in `request_filter`.

**Tests**: add a test asserting the learned limit is actually applied at the rate-limit gate after `observe_429`.

---

## 9. Cross-protocol non-stream re-render strips `content-encoding` then forwards the still-encoded body verbatim on parse failure

**Status**: open · **Severity**: MEDIUM (corrupted client response under a conjunction of conditions)

**What**: `crates/gw-proxy/src/gateway.rs:696-708` unconditionally removes `content-length` and `content-encoding`
for cross-protocol non-stream responses. The response path has **no decode step** (content-encoding is decoded only
request-side). `parse_response` is fed the raw (still gzip/br) bytes and fails, setting `return_body_to_accum=true`,
and the caller forwards the original compressed bytes verbatim (`filters/response.rs:111-117`) — but the
`content-encoding` header was already stripped. `inject_vendor_headers` does not touch `accept-encoding`, so the
upstream may legitimately compress.

**Normal trigger**: a client sends `Accept-Encoding: gzip` (default for OpenAI/Anthropic SDKs, httpx, curl) on a
**non-stream** request through a protocol-conversion key, and the upstream compresses the response.

**Why it matters**: The client receives gzip/brotli bytes with no `content-encoding` header and cannot decode them —
a corrupted response with no error signal. Even the successful cross-render path silently relies on the upstream
not compressing (compressed bytes fail `parse_response` first). Same-protocol paths are unaffected. MEDIUM because
it needs cross-protocol + upstream compression together.

**Fix sketch**: decode `content-encoding` on the response path before `parse_response` (and before verbatim
fallback), or strip the header only after confirming the body was decoded / will be re-rendered.

**Tests**: add a cross-protocol + gzip-upstream non-stream test asserting the client receives decodable bytes.

---

## 10. OpenAI Responses non-stream render ignores `ResponseIR.thinking` — silent drop with no lossy metric

**Status**: open · **Severity**: MEDIUM (silent content loss + falsely-clean lossy signal)

**What**: `crates/gw-codec/src/openai/responses/via.rs:246-264` (`TryFrom<&ResponseIR> for OpenAiResponsesResponse`)
emits a Reasoning item only from `ir.reasoning` (`via.rs:251`) and never reads `ir.thinking`. The Anthropic parse
path populates `ir.thinking` and leaves `ir.reasoning = None`. The cross-render matrix marks OpenAiResponses
`supports_thinking: true` (`cross_render.rs:80`), so `adapt_lossy` keeps `ir.thinking` and emits **no**
`LossyField::drop`. Gemini render and the Responses **streaming** path both handle thinking; only Responses
non-stream drops it.

**Normal trigger**: an OpenAI-Responses client issues a non-stream `POST /v1/responses` routed to an Anthropic
vendor key whose response contains extended-thinking blocks.

**Why it matters**: Assistant thinking/reasoning text is silently dropped, and `gw_codec_cross_render_lossy_total`
does not increment (the matrix declared thinking supported) — operators see a falsely-clean lossy signal while
content is lost, defeating the lossy mechanism's purpose. MEDIUM (advertised capability, but scoped to one cross-
render target on the non-stream path).

**Fix sketch**: render `ir.thinking` in the Responses non-stream renderer (mirror `gemini/via.rs:503-511` /
the Responses streaming path), or — if intentionally unsupported — set `supports_thinking: false` for that target
so the lossy metric fires.

**Tests**: add an Anthropic→OpenAiResponses non-stream cross-render test asserting thinking is rendered (or the
lossy counter increments).

---

## 11. Multi-dim routing preset panic: an unregistered preset name panics the request task at selection time

**Status**: open · **Severity**: HIGH (normal-path `panic!` on a request task) · **Verified by hand**

**What**: `crates/gw-routing/src/strategy/set.rs:129-132`:
`StrategyKind::MultiDim(preset) => self.multi_dim.get(preset).unwrap_or_else(|| panic!("unknown multi_dim routing preset: {preset}"))`.
`self.multi_dim` is populated at boot only from this pod's own `[routing.presets]` config. The incoming preset name
comes from `group.value.strategy` — written by the Go control plane into the entity JSONB and deserialized to
`StrategyKind::MultiDim(String)`, carrying an arbitrary preset name. Config validation (`config.rs:989-1008`)
validates `routing.presets` itself but **never** cross-validates entity-referenced preset names; the loader does
not either. The two preset-name sources (DB model-group strategy vs gateway config presets) are never reconciled at
load time. Other call sites: `gemini_files.rs:198,493`.

**Normal trigger**: ops sets a model group's routing strategy (via the Go control plane) to a multi_dim preset name
that does not exist in this pod's config — a typo, or a newly-added preset that has propagated to the DB but not yet
to every pod's config (ordinary config/control-plane skew). A well-formed client request resolves to that group,
reaches `strategies.pick_for(&group.value.strategy)` (`upstream.rs:287`), and panics inside `upstream_peer`
(`gateway.rs:514`, no `catch_unwind`).

**Why it matters**: The `panic!` aborts that single request task (the workspace has no `panic = "abort"`, so the
process survives and other requests/groups are unaffected), but the request is aborted abnormally instead of
returning a clean `GW_*` 5xx envelope. No malicious input — only an ops/control-plane misconfiguration gates it.

**Fix sketch**: replace the `panic!` with a fail-closed routing `GW_*` 5xx (e.g. `GW_ROUTING_NO_AVAILABLE_CHANNEL`
or a dedicated `GW_ROUTING_UNKNOWN_PRESET`), and add load/boot-time validation that every DB-referenced preset name
exists in the config preset set. Note the existing `#[should_panic]` test pins the current behaviour; the contract's
"fails fast" wording does not mandate a per-request `panic!` mechanism.

**Tests**: replace the `#[should_panic]` expectation with an assertion that an unknown preset yields a clean
routing error; add a load-time preset-name validation test.

---

## 12. PG pool 200 ms `acquire_timeout` turns transient connection pressure into a false "entity not found"

**Status**: open · **Severity**: LOW (load/environment-dependent, non-deterministic)

**What**: `crates/gw-store/src/pg_client.rs:6-15` sets `acquire_timeout(200ms)`, below the default 2000 ms
`cold_load_timeout`. Each cold load must first acquire a connection; with no idle connection inside 200 ms sqlx
returns `PoolTimedOut`, and `with_cold_load_timeout` maps any loader `Err` to `None` (warn only), which callers read
as "entity does not exist". This path also does **not** increment `gateway_cold_load_timeout_total`.

**Normal trigger**: a cold-load storm (empty cache after deploy, bulk `events:entity` invalidation, PG latency
jitter) where >16 distinct keys contend for connections. The `None` from `apikey_or_load` / `vendor_key_or_load` /
`user_or_load` collapses into a hard reject (`GW_AUTH_INVALID_APIKEY` 401 non-retryable / `GW_ROUTING_NO_AVAILABLE_CHANNEL`).

**Why it matters**: Entities that exist in PG are treated as absent purely because the pool is momentarily busy —
fail-wrong rather than retry/degrade. LOW because it is load-dependent and mitigated by Moka same-key coalescing,
`min_connections=2`, and small working sets.

**Fix sketch**: distinguish `PoolTimedOut` from a genuine "row absent" (retry / surface as a 503-class transient
rather than 401/no-channel), and count it under `gateway_cold_load_timeout_total`. Consider raising
`acquire_timeout` toward the cold-load budget.

**Tests**: add a loader test asserting `PoolTimedOut` does not surface as "not found".

---

## 13. Egress error classifier checks `is_timeout()` before `is_proxy_connect()` — proxy CONNECT timeouts mislabeled

**Status**: open · **Severity**: LOW (diagnostic mislabel only)

**What**: `crates/gw-egress-impersonate/src/error.rs:53-73` classifies in order
`is_tls() → is_timeout() → is_proxy_connect() → is_connect()`. When a proxy times out during the CONNECT handshake,
pinned `wreq 6.0.0-rc.28` sets both the timeout flag (inner io `TimedOut`) and the proxy-connect flag, so the
earlier `is_timeout()` wins and the error is mapped to `GW_UPSTREAM_CONNECT_TIMEOUT` (504) instead of
`GW_UPSTREAM_PROXY_ERROR` (502). A refused proxy (`ECONNREFUSED`) has `is_timeout()==false` and is classified
correctly; only a black-holed / SYN-timeout proxy triggers the dual-flag case.

**Normal trigger**: an Impersonate vendor with a per-key proxy (`X-Egress-Proxy`) whose proxy black-holes the
CONNECT at the TCP layer.

**Why it matters**: Clients and the archive see `GW_UPSTREAM_CONNECT_TIMEOUT`, pointing operators at the wrong
component (the LLM upstream) rather than the proxy. Both classes are retryable and handled identically, so no request
is lost — diagnostic only, hence LOW.

**Fix sketch**: check `is_proxy_connect()` before `is_timeout()` (proxy-specificity should win over the generic
timeout flag).

**Tests**: add a classifier unit test for the dual-flag (proxy + timeout) error → `GW_UPSTREAM_PROXY_ERROR`.

---

## Verification (2026-05-30)

Final gates on `fix/audit-remediation-20260530`:

- `cargo build --release -p gw-gateway` — ✅
- `cargo clippy --workspace --all-targets -- -D warnings` — ✅ clean
- `cargo fmt --all -- --check` — ✅
- `cargo test --workspace` — **709 passed, 11 ignored**. The single non-pass is
  `gw-ledger ... credit_lease_integration::coefs_for_six_dim_actual_usage`, the **pre-existing parallelism
  flake** documented in [`2026-05-29-credit-lease-followups.md`](2026-05-29-credit-lease-followups.md) §3
  (shares global `COEF_CACHE`/`COEF_STAGING`). It passes `--test-threads=1` (`8 passed; 0 failed`), and this
  branch touches no coef/credit-lease test or source — not a regression.
- `bash scripts/check-invariants.sh` — ✅ all active invariants hold.
- `make e2e-soak-smoke` — **could not run in this environment**: the Docker image build for the `fake-web`
  stage fails downloading the Rust 1.88 toolchain (`tls handshake eof` to static.rust-lang.org) — a builder
  network restriction, not a code issue. The billing/routing accounting paths the soak exercises are covered
  by the in-process unit + integration suites above. Re-run the soak where the Docker builder has network.
