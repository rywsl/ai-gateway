# Gateway ROADMAP — unbuilt & divergent

Items here are **not in the as-built standard** (`docs/spec/`). Each carries actionable design points so work can start without git archaeology. Populated from the 2026-06-08 spec-vs-code audit.

## Legend
- **Priority**: `runtime-risk` (bug, fix first) > `capability-gap` (unbuilt feature) > `doc-divergence`.
- **Decision-gated residual**: deliberate as-built boundary in `docs/spec/`; implement only after an explicit architecture or release decision, not as a bug fix.
- **Decision confirmed**: the architecture decision has been made and the remaining work is implementation planning/execution. See `docs/design/2026-06-09-roadmap-authority-and-scope.md`.

## 01 protocol-codec

### Kiro / S5 EventStream codec — closed in spec
- **Intent:** decode Kiro upstreams (AWS EventStream binary frames + embedded `<thinking>` tags) — the heaviest of the six vendor codecs.
- **Landed:** internal `crates/gw-codec/src/kiro/` EventStream frame codec (length-prefixed + `crc32fast` CRCs), embedded `<thinking>` parser/renderer, focused roundtrip/error tests, `gw-core::ProtocolKind::Kiro`, `ApiShape::KiroEventStream`, `ApiShape::KiroCli`, and `gw-codec::AnyCodec::Kiro` registration for standard EventStream detection via `content-type: application/vnd.amazon.eventstream` plus `/kiro` or `/s5` path.
- **Boundary:** this closes codec registration and cross-process shape registration. Kiro EventStream outbound auth is now covered by the W7 Kiro adapter; Kiro CLI marker detection, Kiro CLI usage extraction, and Kiro CLI passthrough behavior remain out of scope pending a real CLI traffic sample.
- **Source:** deleted `codec-s5-kiro-eventstream` + `adapter-a-t6-kiro-vendor` (git history).

### `UsageSource::CliExtracted` provenance — closed in spec
- **Landed:** CLI body and stream extraction now stamp `usage_provenance` dimensions with `cli_extracted`, and the shared rank order is `upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown`.
- **Spec:** `docs/spec/01-protocol-codec.md`, `docs/spec/06-billing-ledger.md`, `docs/go-contracts/parts/02-go-as-consumer.md`.
- **Boundary:** the retired `UsageSource` field remains deleted; the as-built contract stays on per-dimension `usage_provenance`.

### OpenAI/Kiro CLI body-level detection and ApiShape split — `capability-gap`
- **Intent:** distinguish standard OpenAI Responses async tasks from OpenAI CLI/Kiro semantic request bodies without relying on test-only `client_protocol` injection.
- **Design points:** W4 stores full IR for standard `/v1/responses background:true` and preserves `/v1/responses/compact` plus CLI-marked direct-submit compatibility. Real early detection still only has path/header/body-head inputs, so body-level OpenAI/Kiro markers belong in the W7/Kiro CLI lane. `ApiShape::KiroCli` is registered as a shape, but Kiro CLI marker detection, usage extraction, and passthrough behavior are still residual.
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
- **Boundary:** task worker/task submit do not call adapters; egress impersonation still runs through `gw-core::impersonate` / `gw-egress-impersonate` after adapter rewrite; `AdapterCtx` carries the optional OAuth runtime used by K-C for vendor-key refresh before bearer injection.
- **Spec:** `docs/spec/07-vendor-integration.md`.

### K-C in-Rust OAuth refresh/write-back — closed in spec
- **Landed:** Rust is the sole runtime refresh writer for `vendor_keys.payload.oauth`. `gw-adapter::oauth_runtime::RustOAuthRuntime` refreshes within the configured skew window, coordinates Redis `SET NX PX` single-flight, calls the provider `token_endpoint`, reloads winner tokens on lock/CAS loss, and `gw-store::oauth_refresh::PgOAuthRefreshWriter` encrypts refreshed tokens, persists rotated refresh tokens when present, CAS-bumps version, and publishes `events:entity ns=vendor_key op=upsert source=rust_oauth_refresh`.
- **Contract:** Go remains the OAuth provider configuration and first-time authorization writer, but post-K-C Go must not refresh runtime vendor-key OAuth tokens. The old `oauth_failed` reverse-XADD command path is repurposed as status-only `oauth_refresh_status`.
- **Spec:** `docs/spec/07-vendor-integration.md`, `docs/go-contracts/contract-change/2026-06-09-rust-oauth-refresh.md`.

### Heavy outbound adapter transforms — closed in spec
- **Landed:** production `apply_outbound_adapter` now executes opt-in `patch_request` before archive/send and recomputes final `content-length` / `body_sha256_hex`; `WireAnyRequest` includes Kiro EventStream frames; built-in Kiro, Vertex, and Codex adapters are registered by vendor id.
- **Boundary:** task runtime does not call adapters. Kiro CLI detector/usage remains a separate protocol-codec capability gap because no real CLI traffic sample exists.
- **Spec:** `docs/spec/07-vendor-integration.md`.

## 08 async-tasks

No open roadmap items after W4 full IR task request/response persistence.

## 09 tunnel

### Owned tunnel bridge and e2e — `closed in W8`
- **Intent:** complete the owned tunnel runtime bridge that maps selected Pingora upstream HTTP traffic into WebSocket tunnel streams.
- **Design points:** W8 defines `ailinkmax-tunnel` in `docs/spec/10-owned-tunnel-protocol.md`, implements first-party `gw-tunnel-server`, extends `gw-tunnel-client` with bidirectional `TunnelStream`, routes `TunnelStack::peer_for_node` to the loopback bridge instead of the node WebSocket endpoint, and adds ignored owned-server/client e2e coverage for positive round trip, no-live, target-denied, and auth-failure cases.
- **Source:** W8 owned tunnel implementation (2026-06-10).

### Default artifact policy — `closed / as-built decision recorded`
- **Intent:** decide whether tunnel remains a separate operator-selected artifact or compiles into the default release image after W8 delivered the bridge/e2e/split CI artifacts.
- **Decision:** keep the split artifact policy. Default `latest` remains the non-tunnel image built with Cargo defaults (`default = []`), and `latest-tunnel` remains the opt-in image built with `--features tunnel`. Do not promote tunnel into default `latest`.
- **Design points:** `docs/spec/09-tunnel.md` documents this as the as-built operator model: runtime tunnel use still requires both the tunnel-enabled artifact and `[tunnel].enabled = true`, while non-tunnel operators continue pulling `latest`.
- **Source:** master program F3 resolved input; W8 implementation evidence; `docs/design/2026-06-10-tunnel-artifact-release-decision.md`.

## 10 Resilience

### SIGTERM handler, Supervisor, DLQ async, archive cap, regex cache — `closed in resilience-fixes`
- **Landed:** `gw-runtime` crate with `Supervisor`, `ShutdownCoordinator`, `SpillFile`, `RegexCache`, `BackpressureGate`; SIGTERM/SIGINT handler in `gw-gateway`; `EmergencyDlqWriter` async over sharded `SpillFile`; `BillingBacklog` + `BacklogRecovery`; `ArchiveState` cap-aware append; `ModelPermission` via shared `RegexCache`; `BackpressureGate` in `early_request_filter`.
- **Source:** `docs/superpowers/plans/2026-06-11-resilience-fixes.md`, commits `cd0d45d..9655ec3`.

### SpillFile Drop + archive reset on failover — `closed in memory-leak-fixes`
- **Landed:** `SpillFile::Drop` for timely shard cleanup (async unlink via `tokio::spawn`, sync fallback); `reset_response_attempt_state` resets archive accumulators on response-side failover. Both prevent disk and memory leaks under churn.
- **Source:** `docs/superpowers/specs/2026-06-12-memory-leak-fixes-design.md` §2 Crate 1.

### Out-of-scope (deliberate) — still deferred
- `LatencyTracker::Mutex<VecDeque>` lock contention — flagged as a hotspot, but not in resilience-fixes ROI window. `capability-gap`.
- `EntityStore` per-(user, api-key) merged-permission cache — CPU win, orthogonal to resilience. `capability-gap`.
- `GLOBAL_STREAM_CAP` / `PER_KEY_INFLIGHT_CAP` hardcoded — revisit once load profile is real. `capability-gap`.
- Egress impersonate UDS `max_sessions=64` and `ClientPool::Mutex<HashMap>` — same reason. `capability-gap`.
- New cross-protocol streaming event mappings (Anthropic/Gemini → OpenAI Chat losing `ThinkingDelta` / `ToolCallStart`) — separate codec design effort. `capability-gap`.
