# Gateway ROADMAP — unbuilt & divergent

Items here are **not in the as-built standard** (`docs/spec/`). Each carries actionable design points so work can start without git archaeology. Populated from the 2026-06-08 spec-vs-code audit.

## Legend
- **Priority**: `runtime-risk` (bug, fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.
- **Decision-gated residual**: deliberate as-built boundary in `docs/spec/`; implement only after an explicit architecture or release decision, not as a bug fix.
- **Decision confirmed**: the architecture decision has been made and the remaining work is implementation planning/execution. See `docs/design/2026-06-09-roadmap-authority-and-scope.md`.

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

### R-D proxy wiring for api-key affinity — `capability-gap`
- **Intent:** make the pure `gw-routing::affinity::AffinityManager` influence live upstream selection once proxy/auth exposes an `ApiKeyId` affinity dimension and selected-key recording point.
- **Design points:** `crates/gw-routing/src/affinity.rs` now provides `AffinityManager` (`record_hit`/`get`/`invalidate_vendor_key`/`sweep_expired`), `AffinityRecord`, and `AffinityDimension` with API-key support. Remaining work is cross-module wiring only: decide where authenticated API-key identity enters `gw-proxy` routing, when a selected vendor key should call `record_hit`, and how vendor-key invalidation hooks should call `invalidate_vendor_key`. Do not change `strategy/cache_affinity.rs`; it remains message-hash request affinity.
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

### Full A-T0..A-T6 outbound adapter registry — `capability-gap` / decision confirmed
- **Intent:** move per-vendor outbound rewrite (OAuth bearer, fingerprint headers, URL rewrite, body transforms) behind the standalone `gw-adapter` seam while preserving byte-identical direct-vendor behavior during incremental rollout.
- **Design points:** F1 is confirmed by `docs/plans/2026-06-08-roadmap-implementation-program.md` and `docs/design/2026-06-09-roadmap-authority-and-scope.md`: keep `crates/gw-adapter`, make `AdapterRegistry::for_vendor` the proxy outbound seam, keep the task worker out of adapter dispatch, leave direct vendors as Noop/short-circuit, keep impersonate vendors backed by `gw-core::impersonate`, and put new heavy vendor transforms behind adapter hooks. `AdapterCtx` must gain the OAuth cache field required by the K-C work.
- **Source:** deleted `adapter-a-t0..t6`, `adapter-layer-design`.

### K-C in-Rust OAuth refresh/write-back — `capability-gap` / decision confirmed
- **Intent:** move runtime OAuth token refresh/write-back to Rust while leaving OAuth configuration and first-time authorization in Go.
- **Design points:** F2 is confirmed by `docs/plans/2026-06-08-roadmap-implementation-program.md` and `docs/design/2026-06-09-roadmap-authority-and-scope.md`: Go remains the config writer, Rust becomes the sole runtime refresh writer, and there must be no double-writer path. Build `OAuthRefresher`/`OauthCache`/`get_access_token`, encrypted token write-back including rotated refresh tokens, version bump + entity broadcast, and cross-Pod single-flight or version-CAS. Retire or repurpose the previous Go runtime refresh reverse-XADD path with a Go-contract change.
- **Source:** deleted `key-health-k-c-oauth-token`, `adapter-a-t1-claude-code`.

### Heavy outbound adapter transforms — `capability-gap` / decision confirmed
- **Intent:** add Kiro AWS SigV4 + EventStream outbound integration, Vertex AI RS256 service-account JWT signing + token exchange, and Codex request-body transforms.
- **Design points:** these transforms use the confirmed outbound adapter boundary from F1. Kiro also requires a protocol-contract plan because the internal `gw-codec::kiro` frame codec is not yet registered as an `AnyCodec` variant and has no `gw-core::ProtocolKind` / `ApiShape` shape.
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
