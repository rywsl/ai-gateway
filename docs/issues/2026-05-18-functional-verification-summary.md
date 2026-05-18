# v1 functional verification — overall report (2026-05-18)

Branch: `feat/llm-gateway-impl` @ `143bbd3`
Verification was performed on a fresh `docker compose up -d` (clean
`-v` volumes), against the gateway image built from the same SHA.

## Phases executed

| Phase | Topic | Status |
| :---: | :--- | :---: |
| A | Docker stack starts; three services healthy; bootstrap logs clean | PASS |
| B | All eight short-circuit error paths return the correct code + envelope | PASS (with two test-plan corrections) |
| C | Metrics emit; cardinality bounded | PASS on cardinality, FAIL on expressiveness for short-circuit paths |
| D | Seeded warm path (PG + Redis L1) fails to authenticate any real key | FAIL (root cause: cold loader is not wired) |
| E | Background tasks: ledger flusher and health server alive; stream consumer not started; cold loader not wired | partial PASS |
| F | `cargo bench -p gw-codec --bench passthrough` runs cleanly with criterion P50/P99 output | PASS |

## What works (verified)

- `/livez`, `/readyz`, `/metrics` endpoints on the right ports.
- `gateway_started_total` = 1 immediately after boot
  (recorder is installed in `main` before the first request, so the
  registry is never empty — fixes the issue documented in
  `2026-05-18-metrics-endpoint-empty.md`).
- `gateway_requests_total{status_class=...}` increments per request.
- All three vendor codecs detect their protocol and render their
  vendor-flavoured error envelope on auth/codec/path-detect failure.
- Short-circuit pipeline correctly maps internal `ErrorIR` to:
  - OpenAI: `{"error":{"code","message","type"}}`
  - Anthropic: `{"type":"error","error":{"type","message"}}`
  - Gemini: `{"error":{"code":<int>,"message","status":<gw_code>}}`
- Ledger flusher background task starts and is parked on its
  receiver.
- Health server stays alive across the bootstrap → run_forever
  handoff.
- `gw-codec` criterion benches compile and emit valid summaries (no
  mutation: ~3.6 ns/op; with-mutation 6kb: ~11.3 µs/op).

## Findings — recorded as separate issue docs

### Critical (v1 cannot serve any real request)

1. **`docs/issues/2026-05-18-warm-path-no-cold-loader.md`** —
   `EntityStore` is a bare Moka cache with no PG/Redis cold-loader
   wired in. `gw-gateway/src/lib.rs:182` does `let _ = pg;`,
   discarding the PgPool that the loader chain needs. Auth always
   returns `GW_AUTH_INVALID_APIKEY` regardless of seeded data,
   verified end-to-end. The `loader::load_entity` function exists
   and works (covered by `crates/gw-store/tests/entity_loader.rs`),
   but no runtime caller exists.

### Minor

2. **`docs/issues/2026-05-18-metrics-status-class-collapses-shortcircuit.md`** —
   all short-circuit responses (auth/codec/quota rejections) land
   in `status_class="err"` regardless of whether the wire status is
   400, 401, 413, etc. `pingora::Error::HTTPStatus` carries the
   right value but is not extracted at the logging emit site.
   Cardinality stays bounded; just expressiveness suffers.

3. **`docs/issues/2026-05-18-background-tasks-verification.md`** —
   `StreamConsumer::start` is never called from bootstrap, and its
   `consume_loop` is a placeholder anyway. If you add the call, the
   loop will not propagate any events. The Ledger flusher works.

4. **`docs/issues/2026-05-18-error-code-verification.md`** — full
   per-request log of Phase B. Two notes recorded:
   - The verifier's expectations for B4 (`stream=false`) and B5
     (malformed JSON) were wrong: auth runs before stream-flag
     validation and body-parse, so those codes are only reachable
     with valid credentials.
   - Anthropic public protocol uses just `x-api-key` (no
     `x-api-key-id` companion). The gateway's `request_filter`
     requires both, so an Anthropic SDK client hitting `/v1/messages`
     today will get "missing auth" rather than "invalid key". Not a
     bug per se for a self-issuing BYOK gateway, but worth a doc
     callout.

## v1 limitations (working as designed, verified)

- Non-stream completions are documented as out-of-scope. The
  `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` code path exists at
  `crates/gw-proxy/src/filters/request.rs:225` and is unreachable
  in this verification only because auth fires first — when the
  warm path comes online, this filter step will fire correctly.
- Error envelope shapes match each vendor's documented JSON
  conventions.
- No high-cardinality labels in the Prometheus registry. Two
  series exposed total (`gateway_started_total`,
  `gateway_requests_total`); only one bounded label (8 possible
  `status_class` values).

## Recommended next steps (informational only)

1. Wire `LoaderCtx { pg, redis, kek }` into `Wired` and hand it to
   the proxy filters. Replace `store.{user,apikey,...}.get(&id)`
   with `cache.get_with_by_ref(&id, async || load_entity(...))`.
2. Either implement `StreamConsumer::consume_loop`'s `XREADGROUP`
   poll (per ADR-007) or document the v1 cache as TTL-only
   invalidation.
3. Either extract `ErrorType::HTTPStatus` in the logging filter to
   bucket short-circuit responses correctly, or stash the
   short-circuit HTTP status onto `ReqCtx` at every emit site.
4. After (1) is in place, redo Phase D: seed `vendor`, `vendor_key`,
   `pricing`, `model_group`, observe the request progress past auth
   into routing and (in absence of a real upstream) fail with
   `GW_BACKEND_UNAVAILABLE` or `GW_UPSTREAM_TIMEOUT`.

## Files touched in this verification

- `docs/issues/2026-05-18-error-code-verification.md` (new)
- `docs/issues/2026-05-18-metrics-status-class-collapses-shortcircuit.md` (new)
- `docs/issues/2026-05-18-warm-path-no-cold-loader.md` (new)
- `docs/issues/2026-05-18-background-tasks-verification.md` (new)
- `docs/issues/2026-05-18-functional-verification-summary.md` (this file)

No code or `Cargo.lock` changes.
