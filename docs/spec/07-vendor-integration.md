# 07 — Vendor Integration (as-built)

- Family: A (adapter/egress) · Backing crates: `gw-adapter`, `gw-egress-impersonate`, `gw-core::impersonate` · Source-of-truth: `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/gateway.rs`, `crates/gw-adapter/src/{analyzer.rs,registry.rs}`, `crates/gw-core/src/impersonate/`, `crates/gw-egress-impersonate/src/`, `crates/gw-store/src/loader.rs`, `crates/gw-proxy/src/error/listeners/oauth.rs` · Status: outbound vendor integration runs **inline in the upstream filter** — header injection, OAuth-token injection, and egress impersonation are all live; `gw-adapter` is wired only for response-side error analyzers.

## Purpose

Take a routed-and-picked `(VendorKey, Vendor)` pair and turn the in-memory request into bytes the upstream vendor will accept: strip client credentials, inject the vendor's auth secret under the correct header, set `host`/`traceparent`/`content-length`, and — for vendors that opt into TLS-fingerprint impersonation — route the connection through an in-process BoringSSL loopback that presents a browser-class JA3/JA4 and a per-identity static header set. OAuth-credentialed keys inject a pre-decrypted bearer token; token refresh is delegated to the Go side. Cost is never computed here (workspace lint `no_cost_micro`).

## As-built surface

### Upstream header injection (inline, not via an adapter)

All vendor header work happens inside `rewrite_request` → `inject_vendor_headers` in `crates/gw-proxy/src/filters/upstream.rs`. There is no dispatch to a vendor adapter object.

- `inject_vendor_headers` (`crates/gw-proxy/src/filters/upstream.rs:1024`) first removes the inbound client credentials `authorization`, `x-api-key`, `x-api-key-id`, then injects the vendor secret.
- The auth secret is chosen at `upstream.rs:1040`: if `picked.oauth.is_some()` the (already-decrypted) `oauth.access_token` is used; otherwise `picked.api_key` is exposed via `secrecy::ExposeSecret`.
- The injection honors the **header name encoded in `Vendor.auth_template`** (`upstream.rs:1067`–`1077`). The template is split on its first `:`; if the left side is a valid header-name token (`is_header_token`, `upstream.rs:1018`) the value is injected under that name (e.g. `x-goog-api-key: {api_key}` for native Gemini, `x-api-key: {api_key}` for Anthropic). A template with no colon (e.g. `Bearer {api_key}`) injects under `authorization`. A colon with an invalid token falls back leniently to `authorization` and ticks `gw_upstream_auth_header_invalid_total`. `{api_key}` is substituted in the value template after the split, so a secret containing a colon is safe.
- `host` is set from `vendor.value.base_urls.first()` (`upstream.rs:1078`).
- A W3C `traceparent` (`00-<trace_id>-<fresh-8-byte-span>-01`) is set per upstream hop (`upstream.rs:1084`); `content-length` is rewritten from the rendered body length (`upstream.rs:1089`).
- The upstream peer host/port also come from `Vendor.base_urls.first()` in `pick_peer` (`upstream.rs:362`–`371`); a vendor with no base URL is a hard `gw_vendor_no_base_url` 500.

### OAuth: inject pre-decrypted token, refresh delegated to Go

- OAuth credentials are decrypted **at load time** in `crates/gw-store/src/loader.rs`: `load_vendor_key` (`loader.rs:283`–`329`) AES-256-GCM-decrypts `access_token_enc` and `refresh_token_enc` via `crate::encrypt::decrypt` into an in-memory `OAuthCredentials` (`loader.rs:290`–`299`). The wire row shape is `OAuthCredentialsRow` (`loader.rs:131`), carrying `access_expires_at`, `token_endpoint`, `scope`, and `quota_project_id`.
- On dispatch, `inject_vendor_headers` (`upstream.rs:1040`–`1058`) compares `oauth.access_expires_at` against `now`: at/after expiry it increments `gw_oauth_token_expired_total`; within 60 s of expiry it increments `gw_oauth_token_near_expiry_total`. **The token is sent regardless of expiry** — `oauth.access_token.expose_secret()` is injected even when the metric fired. This is deliberate: Rust does not refresh.
- Refresh is **delegated to Go**. The error path's `OAuthListener` (`crates/gw-proxy/src/error/listeners/oauth.rs`) reacts to a classifier `OAuthSignalKind::RefreshFailed` by reverse-`XADD`ing to `events:entity` (`oauth.rs:58`–`83`) with fields `ns=vendor_key id=<key_id> op=status_change field=oauth_failed reason=upstream_401 ts_ms=<now>`, then ticks `gw_oauth_refresh_failed_xadd_total`. The XADD is fire-and-forget via `tokio::spawn` because the listener trait is synchronous. `gw_oauth_refresh_total{vendor,outcome}` is emitted for every signal (`oauth.rs:32`).

### Egress impersonation

Per-vendor selection lives in `gw_core::impersonate::VendorEgressMode` (`crates/gw-core/src/impersonate/mod.rs:59`): `Rustls` (default, `mod.rs:65`) preserves the standard Pingora-rustls path; `Impersonate { profile }` routes through the in-process BoringSSL loopback. `pick_peer` reads it at `upstream.rs:382`–`385` and, for the impersonate branch (`upstream.rs:432`–`464`), builds the `HttpPeer` pointing at the egress loopback bind (`EgressBind::Uds` or `EgressBind::Tcp`, `crates/gw-egress-impersonate/src/handle.rs:19`) and ticks `gw_egress_impersonate_dispatch_total`.

Profiles (`gw_core::impersonate::ImpersonateProfile`, `mod.rs:22`):
- `ClaudeCodeChrome131` — `crates/gw-core/src/impersonate/claude_code.rs`. `derive` (`claude_code.rs:24`) deterministically seeds OS/arch/node/package/chrome/electron from pools via `rng::seeded_rng(key_id)`; `headers` (`claude_code.rs:50`) emits the Stainless header set (`X-Stainless-*`), `anthropic-version` (`2023-06-01`), `anthropic-beta` (joined `BETA_TOKENS`), `User-Agent`, and `x-stainless-helper-method: stream` when streaming.
- `OpenAiCodex` — `crates/gw-core/src/impersonate/openai_codex.rs`. `derive` (`openai_codex.rs:27`) seeds codex version/os/arch and a UUIDv5 `installation_id`; `headers` (`openai_codex.rs:46`) emits `OpenAI-Beta: responses=v1`, the `codex_cli_rs/...` `User-Agent`, `x-codex-installation-id`, and per-request `session-id`/`thread-id`/`x-codex-window-id`/`x-codex-turn-state`.
- `GeminiCli` — `crates/gw-core/src/impersonate/gemini_cli.rs`. `derive` (`gemini_cli.rs:16`) seeds cli-version/os/arch + UUIDv5 install id; `headers` (`gemini_cli.rs:33`) emits the `GeminiCLI-/...` `User-Agent`, `x-gemini-api-privileged-user-id`, and `X-Goog-User-Project` when `oauth.quota_project_id` is present.

Per-key identity stability (`crates/gw-core/src/impersonate/ensure.rs`): `ensure_identity(key_id, existing_snapshot, profile)` reuses a persisted `IdentitySnapshot` when its `profile_kind` matches, otherwise re-derives and returns `should_persist = true` (`ensure.rs:14`–`37`). On a fresh derive, `inject_egress_headers` (`upstream.rs:1098`–`1143`) schedules a **lazy persist** via `ctx.identity_persist` so subsequent requests from the same key present a stable fingerprint. The snapshot is read back through `VENDOR_KEY_SQL`'s `jsonb_set` fold of the dedicated `identity_snapshot` column (`loader.rs:224`) into `VendorKeyRow.identity_snapshot` (`loader.rs:200`).

Egress headers (`inject_egress_headers`, `upstream.rs:1098`) set `X-Egress-Target` (host:port), `X-Egress-Profile` (e.g. `claude_code_chrome131`), and `X-Egress-Proxy` (per-key proxy URL when set), then append the profile-specific forged identity headers via `identity_headers_for` (`upstream.rs:1146`).

The loopback itself is `gw-egress-impersonate`: `EgressHandle::start` (`handle.rs:52`) spawns a supervised listener over a `ClientPool` of `wreq::Client` instances keyed by `(profile, normalized-proxy)` with LRU eviction (`crates/gw-egress-impersonate/src/pool.rs:55`). Each client is built with `wreq::Client::builder().emulation(emulation_for(profile))` (`pool.rs:128`); all three profiles currently pin `wreq_util::Emulation::Chrome131` (`crates/gw-egress-impersonate/src/profiles.rs:20`), giving the JA3/JA4 + HTTP/2 SETTINGS fingerprint. The loopback handler (`crates/gw-egress-impersonate/src/server.rs`) reads the `X-Egress-*` control headers, strips them plus hop-by-hop + `Host`/`Content-Length` (`server.rs:49`–`57`), dispatches via the pooled client, and streams the response back. Per-key proxying is honored only here: `pool.rs` builds `wreq::Proxy::all(...)` from the normalized URL.

Workspace kill-switch: `[egress].disabled` (`EgressCfg.disabled`, `crates/gw-gateway/src/config.rs:548`) drops every `Impersonate`-mode candidate in `pick_peer` (`upstream.rs:201`–`221`), ticking `gw_egress_disabled_total{profile}` per skipped candidate; Rustls vendors are unaffected. `[egress]` is boot-time-only — changing it requires a restart (`config.rs:540`).

### `gw-adapter` error analyzers (wired)

`crates/gw-adapter/src/lib.rs` exports an `UpstreamAdapter` v2 trait (`trait_def.rs`: `rewrite_auth`/`rewrite_transport`/`patch_request`/`patch_response`/`transport_profile`), an `AdapterRegistry`, an `AdapterCtx` carrier, and an `UpstreamErrorAnalyzer` trait. The outbound `UpstreamAdapter` path is not used by production request rewriting; outbound v1 is the inline `gw-proxy` filter plus `gw-core::impersonate` profile code described above. The response-side error analyzer path is wired: `gw-proxy::LlmGateway` holds an `AdapterRegistry`, calls `error_analyzer_for(vendor_id)` at response EOS with headers and an 8 KiB body head, and merges the resulting `ErrorOutcome` into the classifier outcome. Built-in analyzer fallback exists for codex/openai, anthropic/claude, and gemini/google vendor ids.

Outbound adapter dispatch remains experimental and decision-gated: wiring `AdapterRegistry::for_vendor` into the request path would move auth/header/body ownership out of `inject_vendor_headers` and the impersonation modules, and would need an explicit architecture decision rather than being treated as an unclosed bug in the current runtime.

## Deliberate behaviours

- **Expired OAuth tokens are still sent.** `inject_vendor_headers` injects `oauth.access_token` even when `now >= access_expires_at`; it only records `gw_oauth_token_expired_total`. Refresh is Go's job; sending the stale token (which usually 401s) is what triggers the `OAuthListener` reverse-XADD that drives Go's async refresh. (`upstream.rs:1040`, `oauth.rs:39`)
- **Direct (Rustls) keys with a configured proxy fail closed.** Pingora 0.8's next-hop is UDS-only and cannot route through an arbitrary HTTP proxy, so a Direct vendor key with `proxy`/`default_proxy` set is rejected with `GW_UPSTREAM_PROXY_UNSUPPORTED` rather than silently leaking the gateway egress IP. Proxied egress must use `egress_mode = Impersonate`, whose loopback honors `X-Egress-Proxy`. (`upstream.rs:394`–`407`)
- **Tunnel and Impersonate are mutually exclusive.** When the tunnel feature would route an impersonate vendor through a tunnel node, `pick_peer` returns `GW_TUNNEL_IMPERSONATE_CONFLICT` instead of silently picking one. (`upstream.rs:436`–`448`)
- **Auth header-name fallback is lenient, not fatal.** A malformed `auth_template` (colon present but the name isn't a header token) falls back to `authorization` plus a metric rather than failing the request — pre-production operator-misconfig tolerance. (`upstream.rs:1070`–`1073`)
- **Identity persistence is lazy and fingerprint-stable.** A per-key `IdentitySnapshot` is derived once on first impersonated dispatch and persisted out-of-band; profile drift forces a re-derive. (`ensure.rs:19`–`36`)
- **Outbound v1 is inline+impersonate.** The production auth/transport path deliberately remains the inline filter code plus `gw-core::impersonate` / `gw-egress-impersonate`; this is the documented as-built boundary, not a missing production hook.
- **`gw-adapter::UpstreamAdapter` remains decision-gated.** Only `UpstreamErrorAnalyzer` is wired into `gw-proxy`. A full outbound adapter registry would be a boundary move that needs a user architecture decision.
- **Rust OAuth is not a writer.** Rust decrypts and injects the access token, emits expiry/401 signals, and delegates refresh/write-back to Go. Moving refresh and encrypted write-back into Rust is a cross-process ownership decision.

## Cross-process touchpoints

- **PgSQL `vendor_keys` payload** — `oauth` JSONB sub-object (`access_token_enc`, `refresh_token_enc`, `access_expires_at`, `token_endpoint`, `scope`, `quota_project_id`); tokens AES-256-GCM at rest, decrypted in `load_vendor_key`. (`loader.rs:131`, `loader.rs:290`)
- **PgSQL `vendor_keys.identity_snapshot` column** — written by the lazy-persist queue (Rust) and folded into the payload via `jsonb_set` on read (`loader.rs:224`); shape is `gw_core::impersonate::IdentitySnapshot`.
- **Redis `events:entity` stream** — reverse-XADD from `OAuthListener` to ask the Go task-worker to refresh a key whose token 401'd: `ns=vendor_key id=<key_id> op=status_change field=oauth_failed reason=upstream_401 ts_ms=<now>`. (`oauth.rs:60`–`70`)
- **KEK** — `OAuthCredentialsRow`/`VendorKeyRow` decryption uses the shared `KekCache` (`loader.rs:22`, `loader.rs:283`).

These are governed by `docs/go-contracts/` (KEK rotation, encrypted column shape, `events:entity` payload).

## Invariants & lints

- `no_cost_micro` / `no_billing_crate` (`scripts/check-invariants.sh`) — no cost arithmetic in this path; vendor integration emits usage, never price.
- `no_v1_markers_in_prod` (`scripts/check-invariants.sh`, error mode) — no placeholder markers in `crates/`.
- Source tripwire `oauth_injection_present_in_source` (`upstream.rs:1502`) asserts the OAuth branch, `access_token` usage, and both expiry metrics stay present.
- Source tripwire `oauth_listener_emits_reverse_xadd_on_refresh_failed` (`oauth.rs:133`) pins the `events:entity` stream name, field keys, success metric, and the spawn.
- `direct_mode_inject_vendor_headers` snapshot (`upstream.rs:1656`) holds the Direct-mode header output byte-stable so impersonation additions never regress the rustls path.
- `impersonate_predicate_matches_both_variants` (`upstream.rs:1682`) pins the kill-switch predicate against both `VendorEgressMode` variants.

## Not built → ROADMAP

The following are outside outbound v1 and require an explicit decision before implementation. Tag: **decision-gated residual**.

- **Full outbound adapter registry (A-T0..A-T6).** `gw-adapter`'s `AdapterRegistry::for_vendor` is not called by a binary and only `NoopAdapter` exists, while response analyzers are wired. This is not a current-runtime bug because outbound v1 is inline+impersonate; build the outbound registry only if the architecture boundary moves from the proxy filter/profile modules into adapters. (decision-gated residual)
- **In-Rust OAuth refresh/write-back (K-C).** Rust injects pre-decrypted tokens and delegates refresh to Go via `events:entity`. Do not add `OAuthRefresher`/cache/write-back in Rust unless ownership moves away from the Go delegated refresh model. (decision-gated residual)
- **Heavy vendor-specific outbound transforms.** Kiro adapter/AWS SigV4, Vertex AI SA-JWT token exchange, and Codex request-body transforms all depend on choosing the outbound adapter registry or another explicit transform boundary. They are not required to complete the documented inline+impersonate outbound v1. (decision-gated residual)
