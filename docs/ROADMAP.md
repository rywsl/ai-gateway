# Gateway ROADMAP — unbuilt & divergent

Items here are **not in the as-built standard** (`docs/spec/`). Each carries actionable design points so work can start without git archaeology. Populated from the 2026-06-08 spec-vs-code audit.

## Legend
- **Priority**: `runtime-risk` (bug, fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.
- **Decision-gated residual**: deliberate as-built boundary in `docs/spec/`; implement only after an explicit architecture or release decision, not as a bug fix.

## 01 protocol-codec

### Kiro / S5 EventStream codec — `capability-gap`
- **Intent:** decode Kiro upstreams (AWS EventStream binary frames + embedded `<thinking>` tags) — the heaviest of the six vendor codecs.
- **Landed:** internal `crates/gw-codec/src/kiro/` EventStream frame codec (length-prefixed + `crc32fast` CRCs), embedded `<thinking>` parser/renderer, and focused roundtrip/error tests.
- **Remaining design points:** Kiro is not registered in `gw-codec/src/any.rs::AnyCodec` and has no `gw-core::ProtocolKind` / `ApiShape` cross-process shape yet. Do not treat it as full Kiro vendor integration until that protocol-contract decision is made.
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

### Full A-T0..A-T6 outbound adapter registry — `decision-gated residual`
- **Intent:** optionally move per-vendor outbound rewrite (OAuth bearer, fingerprint headers, URL rewrite, body transforms) behind one trait, reused by proxy + task runtime.
- **Design points:** `docs/spec/07-vendor-integration.md` blesses inline `gw-proxy::inject_vendor_headers` + `gw-core::impersonate` as outbound v1, and `gw-adapter` response-side `UpstreamErrorAnalyzer` is already wired into `gw-proxy`. Outbound `AdapterRegistry::for_vendor` remains experimental: do not implement the six outbound adapters or wire request dispatch unless the user chooses to move that boundary out of the proxy filter/profile modules. `AdapterCtx` also lacks the `oauth_cache` field its hooks assume (`ctx.rs` comment), so adopting this path is a broader adapter design change.
- **Source:** deleted `adapter-a-t0..t6`, `adapter-layer-design`.

### K-C in-Rust OAuth refresh/write-back — `decision-gated residual`
- **Intent:** optionally refresh vendor OAuth tokens in Rust instead of delegating to Go.
- **Design points:** `docs/spec/07-vendor-integration.md` documents Go-delegated refresh as the current ownership boundary. Rust injects a pre-decrypted, possibly-expired `access_token`, emits expiry metrics, and Go refreshes async via the `events:entity` reverse-XADD (`error/listeners/oauth.rs`). Build `OAuthRefresher`/`OauthCache`/`get_access_token` plus encrypted write-back only if the user decides Rust should become the OAuth writer.
- **Source:** deleted `key-health-k-c-oauth-token`, `adapter-a-t1-claude-code`.

### Heavy outbound adapter transforms — `decision-gated residual`
- **Intent:** optionally add Kiro AWS SigV4 + EventStream outbound integration, Vertex AI RS256 service-account JWT signing + token exchange, and Codex request-body transforms.
- **Design points:** these transforms depend on choosing an outbound transform boundary. Under current outbound v1, header/auth dispatch remains inline+impersonate and no adapter `patch_request` path is active. Treat these as follow-on work only after the outbound adapter registry, or an equivalent transform boundary, is explicitly selected.
- **Source:** deleted `adapter-a-t3-vertex-ai`, `adapter-a-t5-codex`, `adapter-a-t6-kiro-vendor`.

## 08 async-tasks

### Codec render of `ir_wire_body` for task worker — `capability-gap`
- **Intent:** make async task worker requests/responses use the same codec-normalized wire bodies as sync proxy traffic.
- **Design points:** W4 implemented Redis task freeze, settle accounting, submit→poll, and watchdog leader election. The worker still forwards `input_payload.body` raw and stores raw upstream JSON; it does not normalize/re-render request or response through `gw-codec` IR.
- **Source:** residual from deleted `task-worker-design` ADR-T6/T3.

## 09 tunnel

### HTTP<->WS frame bridge runtime — `decision-gated residual`
- **Intent:** complete the runtime bridge that maps Pingora upstream HTTP streams into the tunnel frame protocol.
- **Design points:** `docs/spec/09-tunnel.md` documents current coverage as storage/resolve + peer decision wiring. `WsTransport::send_open` and `send_data` exist, but no production caller turns an upstream request into `Open` / `Data` frames or relays response frames back to the client. Implementing this requires a runtime architecture decision and a remote-node contract, not just a docs cleanup.
- **Source:** authoring pass over `crates/gw-tunnel-client` + `gw-proxy` (2026-06-08).

### Full tunnel e2e round-trip — `decision-gated residual`
- **Intent:** exercise the live WebSocket tunnel path after the bridge design is chosen.
- **Design points:** `crates/gw-gateway/tests/e2e_tunnel.rs` remains `#![cfg(feature = "tunnel")]` and `#[ignore]`, and its header says the full Pingora → tunnel-node → fake-upstream round-trip is deferred. Current e2e coverage does not exercise `peer_for_node` with a live WS node, `ControlAuth`, `Open` frame forwarding, DNS/target denial, or response relay.
- **Source:** authoring pass over `crates/gw-tunnel-client` + `gw-proxy` (2026-06-08).

### Default artifact policy — `decision-gated residual`
- **Intent:** decide whether tunnel should remain a separate operator-selected artifact or compile into the default release image.
- **Design points:** `docs/spec/09-tunnel.md` documents the split model as current: default `latest` is non-tunnel, while `latest-tunnel` is built with `--features tunnel` for operators that opt in. Keep this split until the bridge and full e2e loop are closed; changing default `latest` is a release decision.
- **Source:** deleted `tunnel-t-design` §1/§9.
