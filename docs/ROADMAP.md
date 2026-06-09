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

### `UsageSource::CliExtracted` provenance — closed in spec
- **Landed:** CLI body and stream extraction now stamp `usage_provenance` dimensions with `cli_extracted`, and the shared rank order is `upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown`.
- **Spec:** `docs/spec/01-protocol-codec.md`, `docs/spec/06-billing-ledger.md`, `docs/go-contracts/parts/02-go-as-consumer.md`.
- **Boundary:** the retired `UsageSource` field remains deleted; the as-built contract stays on per-dimension `usage_provenance`.

### OpenAI/Kiro CLI body-level detection and ApiShape split — `capability-gap`
- **Intent:** distinguish standard OpenAI Responses async tasks from OpenAI CLI/Kiro semantic request bodies without relying on test-only `client_protocol` injection.
- **Design points:** W4 stores full IR for standard `/v1/responses background:true` and preserves `/v1/responses/compact` plus CLI-marked direct-submit compatibility. Real early detection still only has path/header/body-head inputs, so body-level OpenAI/Kiro markers and any dedicated Kiro/OpenAI CLI `ApiShape` registration belong in the W7/Kiro CLI lane.
- **Source:** W4 final quality review (2026-06-09).

## 02 routing

### R-D proxy wiring for api-key affinity — closed in spec
- **Landed:** `gw-routing::affinity::AffinityManager` is now held by gateway bootstrap and wired into live proxy upstream selection. `gw-proxy` uses authenticated `ReqCtx.api_key_id` as `AffinityDimension::ApiKey`, consults affinity after ranked candidates and before normal first-candidate selection, records the selected vendor key after outbound request rewrite succeeds, and invalidates bindings via vendor-key invalidation hooks plus key-health/OAuth error listeners.
- **Boundary:** `strategy/cache_affinity.rs` remains unchanged and continues to implement message-hash request affinity, separate from API-key request-external affinity.
- **Spec:** `docs/spec/02-routing.md`.

### R-F stream probe (first-chunk failover window) — closed in spec
- **Landed:** `gw-proxy::stream_probe` now separates first upstream body bytes from first client-visible body commit. Streaming responses enter `Connecting` / `Probing` / `Streaming` phases, buffer probe bytes in `upstream_response_body_filter`, fail over on upstream error body, malformed first frame, or probe timeout before downstream commit, and preserve no-retry behavior after a valid event/body has been committed. `gw_stream_probe_*` metrics are registered without key-level labels.
- **Spec:** `docs/spec/02-routing.md`.

## 03 key-health

No open ROADMAP items.

## 04 rate-limiting

No open ROADMAP items.

## 05 auth-permissions

No open ROADMAP items.

## 06 billing-ledger

No open ROADMAP items.

## 07 vendor-integration

### Full A-T0 outbound adapter seam — closed in spec
- **Landed:** `gw-proxy` outbound request rewrite dispatches through `gw-adapter::AdapterRegistry::for_vendor`, with `NoopAdapter` preserving the legacy direct-vendor auth/header rewrite and the Direct-mode snapshot renamed to `direct_mode_outbound_adapter_headers`.
- **Boundary:** task worker/task submit do not call adapters; egress impersonation still runs through `gw-core::impersonate` / `gw-egress-impersonate` after adapter rewrite; `AdapterCtx` carries an inert optional OAuth runtime slot for K-C.
- **Spec:** `docs/spec/07-vendor-integration.md`.

### K-C in-Rust OAuth refresh/write-back — `capability-gap` / decision confirmed
- **Intent:** move runtime OAuth token refresh/write-back to Rust while leaving OAuth configuration and first-time authorization in Go.
- **Design points:** F2 is confirmed by `docs/plans/2026-06-08-roadmap-implementation-program.md` and `docs/design/2026-06-09-roadmap-authority-and-scope.md`: Go remains the config writer, Rust becomes the sole runtime refresh writer, and there must be no double-writer path. Build `OAuthRefresher`/`OauthCache`/`get_access_token`, encrypted token write-back including rotated refresh tokens, version bump + entity broadcast, and cross-Pod single-flight or version-CAS. Retire or repurpose the previous Go runtime refresh reverse-XADD path with a Go-contract change.
- **Source:** deleted `key-health-k-c-oauth-token`, `adapter-a-t1-claude-code`.

### Heavy outbound adapter transforms — `capability-gap` / decision confirmed
- **Intent:** add Kiro AWS SigV4 + EventStream outbound integration, Vertex AI RS256 service-account JWT signing + token exchange, and Codex request-body transforms.
- **Design points:** these transforms use the confirmed outbound adapter boundary from F1. Kiro also requires a protocol-contract plan because the internal `gw-codec::kiro` frame codec is not yet registered as an `AnyCodec` variant and has no `gw-core::ProtocolKind` / `ApiShape` shape.
- **Source:** deleted `adapter-a-t3-vertex-ai`, `adapter-a-t5-codex`, `adapter-a-t6-kiro-vendor`.

## 08 async-tasks

No open roadmap items after W4 full IR task request/response persistence.

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
