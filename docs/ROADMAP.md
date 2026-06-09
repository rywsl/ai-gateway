# Gateway ROADMAP — unbuilt & divergent

Items here are **not in the as-built standard** (`docs/spec/`). Each carries actionable design points so work can start without git archaeology. Populated from the 2026-06-08 spec-vs-code audit.

## Legend
- **Priority**: `runtime-risk` (bug, fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.

## 01 protocol-codec

### Kiro / S5 EventStream codec — `capability-gap`
- **Intent:** decode Kiro upstreams (AWS EventStream binary frames + embedded `<thinking>` tags) — the heaviest of the six vendor codecs.
- **Design points:** new `crates/gw-codec/src/kiro/` module: EventStream framer (length-prefixed + `crc32fast` CRCs — add the dep), wire request/response types, thinking-tag parser, request/response roundtrip. Register a Kiro variant in `gw-codec/src/any.rs::AnyCodec`. No code exists today (only stray comments in `gw-adapter`).
- **Source:** deleted `codec-s5-kiro-eventstream` + `adapter-a-t6-kiro-vendor` (git history).

### `UsageSource::CliExtracted` provenance — `doc-divergence`
- **Intent:** let billing distinguish CLI-extracted usage from parsed/estimated.
- **Design points:** the ledger replaced the old `usage_source` field with `usage_provenance` (`crates/gw-ledger/src/usage.rs`); decide whether a CLI-extracted provenance value is needed and add it there rather than reviving `UsageSource`.
- **Source:** deleted `cli-variants-design` ADR-CLI-10.

## 02 routing

### R-D AffinityManager (api-key → vendor-key affinity) — `capability-gap`
- **Intent:** keep a user pinned to the same upstream key across requests to preserve prompt-cache continuity and reduce re-billing.
- **Design points:** new `crates/gw-routing/src/affinity.rs`: `AffinityManager` (`record_hit`/`get`/`invalidate_vendor_key`/`sweep_expired`), `AffinityRecord`, an `AffinityDimension`, and a sweep loop. Distinct from the existing request-level `strategy/cache_affinity.rs` (message-hash).
- **Source:** deleted `routing-r-d-affinity-manager`.

### R-F stream probe (first-chunk failover window) — `capability-gap`
- **Intent:** allow failover when a stream fails *after* the upstream's first byte but *before* any SSE event reaches the client.
- **Design points:** `StreamPhase` (Connecting/Probing/Streaming) + `probe_timeout` watchdog + `ProbeFailure` (upstream-error-body / malformed-frame / timeout) + `gw_stream_probe_*` metrics. Today `gw-proxy/src/gateway.rs` conflates first-upstream-byte with first-client-byte via `body_started`.
- **Source:** deleted `routing-r-f-stream-probe`.

## 03 key-health

No open ROADMAP items.

## 04 rate-limiting

No open ROADMAP items.

## 05 auth-permissions

No open ROADMAP items.

## 06 billing-ledger

No open ROADMAP items.

## 07 vendor-integration

### Full A-T0..A-T6 vendor adapter layer + wire-or-delete `gw-adapter` — `capability-gap`
- **Intent:** per-vendor outbound rewrite (OAuth bearer, fingerprint headers, URL rewrite, body transforms) behind one trait, reused by proxy + task runtime.
- **Design points:** `crates/gw-adapter` response-side `UpstreamErrorAnalyzer` is wired into `gw-proxy`, but outbound `AdapterRegistry::for_vendor` is still not called by any binary and only `NoopAdapter` exists. Today outbound behavior is inline in `gw-proxy::inject_vendor_headers` + `gw-core::impersonate`. Decide whether to wire outbound adapters into `gw-proxy`/`gw-task-runtime` and build the 6 vendor adapters, or bless the inline+impersonate design for outbound traffic. `AdapterCtx` also lacks the `oauth_cache` field its hooks assume (`ctx.rs` comment).
- **Source:** deleted `adapter-a-t0..t6`, `adapter-layer-design`.

### K-C in-Rust OAuth refresh stack — `capability-gap`
- **Intent:** refresh vendor OAuth tokens in Rust instead of delegating to Go.
- **Design points:** today there is no `OAuthRefresher`/`OauthCache`/`get_access_token`; `gw-proxy/src/filters/upstream.rs` injects a pre-decrypted, possibly-expired `access_token` and Go refreshes async via the `events:entity` reverse-XADD (`error/listeners/oauth.rs`). If brought in-Rust: `OauthCache` with CAS-lock lazy refresh + per-vendor `OAuthRefresher` (e.g. `console.anthropic.com/v1/oauth/token`) + write-back of `oauth_*_enc` to `entity:vendor_key`. **Note:** the current Go-delegated design is deliberate and documented in `spec/07`; only build this if moving the boundary.
- **Source:** deleted `key-health-k-c-oauth-token`, `adapter-a-t1-claude-code`.

### kiro adapter / vertex_ai SA-JWT / codex body transform — `capability-gap`
- **Intent:** the heavier vendor adapters.
- **Design points:** kiro AWS SigV4 + EventStream (pairs with the codec gap in §01); vertex_ai RS256 service-account JWT signing + token exchange (`jsonwebtoken` is currently used only for *inbound* client JWTs); codex `patch_request` body image transform. None exist.
- **Source:** deleted `adapter-a-t3-vertex-ai`, `adapter-a-t5-codex`, `adapter-a-t6-kiro-vendor`.

## 08 async-tasks

### Codec render of `ir_wire_body` for task worker — `capability-gap`
- **Intent:** make async task worker requests/responses use the same codec-normalized wire bodies as sync proxy traffic.
- **Design points:** W4 implemented Redis task freeze, settle accounting, submit→poll, and watchdog leader election. The worker still forwards `input_payload.body` raw and stores raw upstream JSON; it does not normalize/re-render request or response through `gw-codec` IR.
- **Source:** residual from deleted `task-worker-design` ADR-T6/T3.

## 09 tunnel

### Default artifact remains non-tunnel — `capability-gap`
- **Intent:** decide whether tunnel should stay as a separate operator-selected artifact or compile into the default release image.
- **Design points:** `default = []` in `crates/gw-proxy/Cargo.toml` and `crates/gw-gateway/Cargo.toml`; `.gitlab-ci.yml` now builds two artifacts: default `latest` without `--features tunnel`, and `latest-tunnel` with `--features tunnel`. The split artifact is operationally usable, but the default image still compiles the `#[cfg(not(feature = "tunnel"))]` Direct arm. Decide whether this is the final shipping model or whether default `latest` should include the tunnel feature.
- **Source:** deleted `tunnel-t-design` §1/§9.

### Full tunnel e2e round-trip — `capability-gap`
- **Intent:** exercise the live WebSocket tunnel path, not only entity loading and `TunnelStack::resolve`.
- **Design points:** `crates/gw-gateway/tests/e2e_tunnel.rs` remains `#![cfg(feature = "tunnel")]` and `#[ignore]`, and its header says the full Pingora → tunnel-node → fake-upstream round-trip is deferred. Extend the ignored test (or add a dedicated one) to cover `peer_for_node` with a live WS node, `ControlAuth`, `Open` frame forwarding, DNS/target denial, and response relay.
- **Source:** authoring pass over `crates/gw-tunnel-client` + `gw-proxy` (2026-06-08).
