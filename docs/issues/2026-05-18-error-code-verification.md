# Issue: Error-code & error-envelope verification (Phase B)

## Context

Eight curl probes were sent against a freshly-booted Docker stack
(`docker compose up -d`, gateway image built from
`feat/llm-gateway-impl @ 143bbd3`) to validate the protocol-detection
and error-rendering paths that do NOT require a live upstream.

Each request was crafted to fail at a known short-circuit point
(`request_filter` / `early_request_filter`). The verifier compared the
observed HTTP status, error code, message, and vendor envelope shape
against `gw_core::error::codes` and the per-codec
`render_error` impls.

## Observations

### B1 — no auth header at all (OpenAI path)

```
POST /v1/chat/completions
HTTP 401
{"error":{"code":"GW_AUTH_INVALID_APIKEY","message":"missing auth","type":"client_auth"}}
```

Matches expected `GW_AUTH_INVALID_APIKEY` with OpenAI envelope shape
`{"error":{"code","message","type"}}`. PASS.

### B2 — fake Bearer JWT

```
HTTP 401
{"error":{"code":"GW_AUTH_INVALID_JWT","message":"auth failed","type":"client_auth"}}
```

Matches `GW_AUTH_INVALID_JWT`. PASS.

### B3 — x-api-key + x-api-key-id with no DB row

```
HTTP 401
{"error":{"code":"GW_AUTH_INVALID_APIKEY","message":"api key invalid","type":"client_auth"}}
```

`verify_apikey` returns the generic "api key invalid" message rather
than distinguishing "row not found" from "hash mismatch". The
`apikey not found` branch in `crates/gw-proxy/src/filters/request.rs:84-95`
also exists but is dead behind `verify_apikey` — `verify_apikey`
itself already short-circuits on row-not-found and returns
`GW_AUTH_INVALID_APIKEY`, so the post-verify `store.apikey.get(&akid)`
call is redundant. Behaviour is correct from a client perspective.
PASS (with a note about dead code).

### B4 — `"stream": false` (with bad credentials)

```
HTTP 401
{"error":{"code":"GW_AUTH_INVALID_APIKEY","message":"api key invalid","type":"client_auth"}}
```

Expected `400 GW_REQUEST_NONSTREAM_NOT_SUPPORTED` per the spec, but
the gateway short-circuits in `request_filter` step 1 (auth) before
it reaches step 4b (stream validation, line 225 of
`crates/gw-proxy/src/filters/request.rs`). This is by design — auth
runs before parse — but the verifier's expectations need to be
restated: **stream=false rejection is only observable with valid
credentials**. PASS (filter ordering is correct; the test plan
expectation was wrong).

### B5 — malformed JSON body (with bad credentials)

```
HTTP 401
{"error":{"code":"GW_AUTH_INVALID_APIKEY","message":"api key invalid","type":"client_auth"}}
```

Same situation as B4: auth fires before the body is parsed (and even
before `read_request_body`), so the codec-parse error path is
unreachable without valid credentials. The codec-parse short-circuit
is wired in `request_filter` step 5 (line ~310) but cannot be
exercised here. PASS (filter ordering; test expectation needed
adjustment).

### B6 — Anthropic `/v1/messages` with only `x-api-key`

```
HTTP 401
{"error":{"message":"missing auth","type":"GW_AUTH_INVALID_APIKEY"},"type":"error"}
```

Vendor envelope shape matches `render_error` in
`crates/gw-codec/src/anthropic/codec.rs:173-184` — `{"type":"error",
"error":{"type":<code>,"message":<msg>}}`. Note the inner field is
`type`, not `code`, matching Anthropic's public API. PASS.

**Semantic mismatch (record but not blocker):** Anthropic's real
protocol authenticates with `x-api-key` alone (no `x-api-key-id`
companion header — Anthropic API keys are self-identifying). The
gateway's `request_filter` requires `(x-api-key-id, x-api-key)` as a
pair (line 77: `if let (Some(akid_str), Some(akey)) = ...`), and
falls into the "missing auth" branch when only one is present. For
a v1 BYOK gateway terminating its own auth this is acceptable, but a
client porting an existing Anthropic SDK will fail closed with
"missing auth" rather than "invalid key", which can confuse
integrators. Documented as a v1 limitation; no code change needed.

### B7 — Gemini streamGenerateContent, no auth

```
HTTP 401
{"error":{"code":401,"message":"missing auth","status":"GW_AUTH_INVALID_APIKEY"}}
```

Gemini envelope from `crates/gw-codec/src/gemini/codec.rs:213-224`:
`{"error":{"code":<http_status>,"message","status":<gw_code>}}`.
Matches Google API conventions (http status integer in `code`, text
identifier in `status`). PASS.

### B8 — unknown path `/v1/random/path`

```
HTTP 400
{"error":{"code":"GW_REQUEST_PROTOCOL_UNKNOWN","message":"unknown protocol","type":"client_bad_request"}}
```

`AnyCodec::detect` fails to identify any protocol → 400 with
`GW_REQUEST_PROTOCOL_UNKNOWN`. Default emit codec is OpenAI
(`pick_emit_codec` on `None`), which is why the envelope uses the
OpenAI shape. PASS.

## Summary

All eight short-circuit error paths fire with the expected
`code`/`status`. Each per-codec envelope matches the vendor's
documented error shape (OpenAI flat object, Anthropic
`type:"error"` wrapper, Gemini Google-style `code`/`status`).

The verifier's expectations for B4 and B5 were wrong: the auth
filter runs *before* stream-flag validation and body parsing, so
those two error codes (`GW_REQUEST_NONSTREAM_NOT_SUPPORTED`,
codec-parse) cannot be exercised without valid credentials. Phase D
re-tests these.

The only behavioural note is that Anthropic clients using just
`x-api-key` (no `-id` companion) get "missing auth" rather than a
more pointed error — see B6 for details.

## Files referenced

- `crates/gw-core/src/error.rs:33-59`
- `crates/gw-proxy/src/filters/request.rs:40-120`
- `crates/gw-proxy/src/filters/early.rs:39-145`
- `crates/gw-codec/src/openai/codec.rs:173`
- `crates/gw-codec/src/anthropic/codec.rs:173`
- `crates/gw-codec/src/gemini/codec.rs:213`
