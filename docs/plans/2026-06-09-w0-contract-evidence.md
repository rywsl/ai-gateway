# W0 Contract And Evidence Index

- Date: 2026-06-09
- Scope: evidence and contract inputs for `docs/plans/2026-06-09-remaining-roadmap-execution.md`
- Status: docs-only; no Rust source changes

## Evidence Read Set

- Plans and authority: `docs/plans/2026-06-08-roadmap-implementation-program.md`, `docs/plans/2026-06-09-remaining-roadmap-execution.md`, `docs/design/2026-06-09-roadmap-authority-and-scope.md`, `docs/ROADMAP.md`.
- As-built specs: `docs/spec/01-protocol-codec.md`, `docs/spec/06-billing-ledger.md`, `docs/spec/07-vendor-integration.md`, `docs/spec/08-async-tasks.md`, `docs/spec/09-tunnel.md`.
- Go contracts: `docs/go-contracts/parts/02-go-as-consumer.md`, `docs/go-contracts/parts/03-shared-schema.md`, `docs/go-contracts/parts/07-oauth-ldap.md`, `docs/go-contracts/parts/08-control-plane.md`.
- Source evidence: `crates/gw-core/src/protocol.rs`, `crates/gw-core/tests/wire_enum_lock.rs`, `crates/gw-core/src/usage.rs`, `crates/gw-ledger/src/usage.rs`, `crates/gw-codec/src/any.rs`, `crates/gw-codec/src/cross_render.rs`, `crates/gw-codec/src/kiro/mod.rs`, `crates/gw-codec/src/wire/any.rs`, `crates/gw-proxy/src/filters/response.rs`, `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/error/listeners/oauth.rs`, `crates/gw-store/src/loader.rs`, `crates/gw-store/src/entity_store.rs`, `crates/gw-store/src/encrypt.rs`, `crates/gw-store/src/{identity_persist.rs,quota_state.rs}`, `crates/gw-adapter/src/{ctx.rs,trait_def.rs,registry.rs,noop.rs}`, `crates/gw-tunnel-client/src/{frame.rs,transport.rs,pool.rs}`, `crates/gw-proxy/src/tunnel_stack.rs`, `tests/e2e/client/soak/dedupe.py`, `crates/e2e-fake-upstream/src/tunnel_node.rs`.

## 1. Kiro Protocol Contract Implementation Inputs

### Current Evidence

- `crates/gw-codec/src/kiro/mod.rs` already implements an internal `Kiro` `ProtocolCodec`: AWS EventStream frame encode/decode with CRC checks, request/response parse/render, stream parse/render, and embedded `<thinking>` support.
- `crates/gw-codec/src/kiro/mod.rs` explicitly says Kiro is not added to `gw-core::protocol` until the cross-process protocol contract grows a Kiro shape.
- `crates/gw-codec/src/any.rs::AnyCodec` has only `OpenAi`, `OpenAiResponses`, `Anthropic`, and `Gemini`.
- `crates/gw-core/src/protocol.rs::ProtocolKind` has only `OpenAi`, `Anthropic`, and `Gemini`; serde wire strings are locked by `crates/gw-core/tests/wire_enum_lock.rs`.
- `crates/gw-core/src/protocol.rs::ApiShape` has four standard shapes plus four CLI shapes; there is no Kiro shape.
- `crates/gw-codec/src/cross_render.rs::WireFormat` has four wire formats; no Kiro capabilities are modeled.
- `crates/gw-codec/src/wire/any.rs::WireAnyRequest` has OpenAI/Anthropic/Gemini only. Adapter `patch_request` consumers cannot dispatch a Kiro request yet.
- `docs/go-contracts/parts/03-shared-schema.md` documents `vendor_keys.payload.accessible_endpoints` as a `ProtocolKind` list and `model_mapping.protocol` as `ProtocolKind`-backed.

### Recommended Contract Choice

- Treat Kiro as a **standard EventStream upstream protocol**, not as a CLI variant.
- Register it as **EventStream-only** for W7. It is not CLI-like today because the existing implementation parses/render IR through `ProtocolCodec`, not body-passthrough `CliCodec`; there is no current Kiro CLI detector, `CliHints`, or same-protocol CLI-only contract.
- Add `ProtocolKind::Kiro`.
  - Serde wire for Go JSONB `accessible_endpoints`: `"kiro"` under `serde(rename_all = "snake_case")`.
  - `ProtocolKind::as_str()` wire for `model_mapping.protocol` and event labels: `"kiro"`.
- Add `ApiShape::KiroEventStream`.
  - Serde wire: `"kiro_event_stream"`.
  - `FORMAT_ID`: recommend `"kiro:eventstream"`.
  - `is_cli()`: `false`.
  - `protocol_kind()`: `ProtocolKind::Kiro`.
- Add `AnyCodec::Kiro(gw_codec::kiro::Kiro)`.
  - `AnyCodec::detect` should try Kiro after the existing four HTTP JSON surfaces unless implementation evidence shows Kiro detection cannot collide. Current Kiro detection requires `content-type` containing `application/vnd.amazon.eventstream` plus path/body evidence, so late registration is low-risk.
  - `kind()` maps to `ProtocolKind::Kiro`.
  - `api_shape()` maps to `ApiShape::KiroEventStream`.
- Add `WireFormat::KiroEventStream`.
  - Label: recommend `"kiro_eventstream"` to match the recommended `FORMAT_ID` spelling and avoid changing existing `WireFormat::label()` style.
  - Capabilities should set at least `supports_thinking = true`; every other flag must be decided from the Kiro wire implementation, not inherited blindly from Anthropic/OpenAI.
  - `WireFormat::from_any_codec` must map `AnyCodec::Kiro`.
- Add `WireAnyRequest::Kiro(...)` only if W7 Kiro adapter needs semantic request mutation through `patch_request`. If Kiro SigV4 only signs bytes and headers after codec render, W7 can avoid adding a Kiro wire request type in the first pass; however, `WireAnyRequest` is already the adapter request-mutation seam and should be kept in sync with `AnyCodec` if Kiro body transforms are part of W7.

### Go-Facing Contract Strings

- `vendor_keys.payload.accessible_endpoints`: `["kiro"]`.
- `model_mapping.protocol`: `"kiro"` if using `ProtocolKind::as_str()` / TEXT contract.
- `ApiShape` / `FORMAT_ID`: `"kiro:eventstream"` for Aether capability/cross-process format fields.
- Go contract docs must call out the two different protocol strings already present in Rust:
  - `ProtocolKind` serde JSONB for entity payloads.
  - `ProtocolKind::as_str()` for model mapping / event labels.
  - For Kiro both are `"kiro"`, unlike `OpenAi`, where JSONB is `"open_ai"` but `as_str()` is `"openai"`.

### Files To Modify In W7

- `crates/gw-core/src/protocol.rs`
- `crates/gw-core/tests/wire_enum_lock.rs`
- `crates/gw-codec/src/any.rs`
- `crates/gw-codec/src/cross_render.rs`
- `crates/gw-codec/src/wire/any.rs` if Kiro request patching is needed
- `crates/gw-codec/src/kiro/mod.rs`
- `crates/gw-codec/tests/kiro_eventstream.rs`
- `crates/gw-codec/tests/{cross_protocol_matrix.rs,lossy_paths.rs,wire_any.rs}` as applicable
- `crates/gw-proxy/src/filters/response.rs` (`codec_for`, extractor/cross-render surfaces)
- `crates/gw-proxy/src/filters/upstream.rs` if request render / routing needs Kiro-specific codec selection
- `crates/gw-gateway/src/metrics_catalog.rs` if new Kiro metrics are emitted
- `docs/spec/01-protocol-codec.md`
- `docs/spec/07-vendor-integration.md`
- `docs/go-contracts/parts/03-shared-schema.md`
- `docs/go-contracts/parts/08-control-plane.md` capability examples (`protocols`, `FORMAT_ID`/shape lists)
- `docs/ROADMAP.md`

## 2. CLI Usage Provenance Implementation Inputs

### Current Evidence

- The retired `UsageSource` field is gone. `crates/gw-ledger/src/usage.rs::UsageRecord` carries `usage_provenance: UsageProvenance`.
- `crates/gw-core/src/usage.rs::ProvenanceTag` currently has `UpstreamFinal`, `UpstreamPartial`, `Estimated`, `CrashFallback`, and `Unknown`.
- `crates/gw-ledger/src/usage.rs::UsageProvenance` stores one `ProvenanceTag` per usage dimension.
- `crates/gw-proxy/src/filters/response.rs::copy_provenance` ranks current tags as `UpstreamFinal > UpstreamPartial > Estimated > CrashFallback > Unknown`.
- CLI extraction metrics already exist: `gw_cli_usage_extract_total{shape,source}` is emitted in `crates/gw-proxy/src/filters/response.rs` and registered in `crates/gw-gateway/src/metrics_catalog.rs`. Do not add a duplicate metric.
- CLI extracted usage currently updates `ctx.usage` in `apply_cli_full_usage` / `apply_cli_delta`, but does not stamp a distinct `usage_provenance` tag.
- Go contract `docs/go-contracts/parts/02-go-as-consumer.md` and soak helper `tests/e2e/client/soak/dedupe.py` both embed the provenance priority list.

### Recommended Tag

- Add `ProvenanceTag::CliExtracted`, serialized as `"cli_extracted"`.
- Priority order:

```text
upstream_final > cli_extracted > upstream_partial > estimated > crash_fallback > unknown
```

Rationale: CLI extraction parses usage emitted by the upstream CLI wire body/stream, so it is more authoritative than tokenizer estimates and ordinary partial IR accumulation, but it should not outrank an explicit final usage payload from a standard codec.

### Consumer Priority And Dedupe Changes

- `docs/go-contracts/parts/02-go-as-consumer.md` §2.1.2 example and §2.1.8 priority list must include `cli_extracted`.
- Go settler priority must use the same rank for all duplicate `(request_id, attempt_index)` decisions:
  - `upstream_final = 5`
  - `cli_extracted = 4`
  - `upstream_partial = 3`
  - `estimated = 2`
  - `crash_fallback = 1`
  - `unknown = 0`
- The soak dedupe oracle must change `tests/e2e/client/soak/dedupe.py::_PROVENANCE_RANK` to include `"cli_extracted": 4`, then bump the existing values accordingly.
- Update `tests/e2e/client/soak/test_kill_loop.py` if it asserts exact numeric ranks.

### Files To Modify In W3

- `crates/gw-core/src/usage.rs`
- `crates/gw-ledger/tests/usage_schema.rs`
- `crates/gw-proxy/src/filters/response.rs`
- `crates/gw-proxy/src/ctx.rs` only if helper placement requires ctx-level defaulting
- `crates/gw-gateway/src/metrics_catalog.rs` only if metric help text needs to mention provenance; the metric itself is already present
- `tests/e2e/client/soak/dedupe.py`
- `tests/e2e/client/soak/test_kill_loop.py` as needed
- `docs/spec/01-protocol-codec.md`
- `docs/spec/06-billing-ledger.md`
- `docs/go-contracts/parts/02-go-as-consumer.md`
- `docs/ROADMAP.md`

## 3. OAuth Ownership Delta

### Current Contract Conflict

F2 is authoritative: Go writes OAuth configuration and first-time authorization state; Rust is the sole runtime refresh writer after K-C.

Current docs still describe the old model:

- `docs/go-contracts/parts/02-go-as-consumer.md` §2.4.3 says `vendor_key.oauth_failed` tells the Go task-worker to refresh runtime tokens.
- `docs/go-contracts/parts/03-shared-schema.md` §3.2.5.1 says Go encrypts and replaces `payload.oauth` after refresh, `token_endpoint` is only for Go refresh, and Rust does not actively refresh.
- `docs/go-contracts/parts/07-oauth-ldap.md` §7.3 says OAuth code exchange, access-token cache, and refresh-token renewal are all done by Go, and Rust does not connect to OAuth providers.
- `docs/spec/07-vendor-integration.md` describes Rust as decrypt/inject/signal-only and Go as runtime refresh owner.

### Required Go Contract Edits

- Add a new `docs/go-contracts/contract-change/YYYY-MM-DD-rust-oauth-refresh.md` that records:
  - Go remains config writer and initial authorization writer.
  - Rust becomes the only runtime refresh writer for `vendor_keys.payload.oauth`.
  - Runtime token write-back is Rust-only: encrypted access token, encrypted rotated refresh token if present, `access_expires_at`, `scope`, `quota_project_id` preservation, `token_endpoint` read by Rust, `version + 1`, and `events:entity ns=vendor_key op=upsert`.
  - Go must stop consuming `oauth_failed` as a refresh command after K-C migration.
- Update `docs/go-contracts/parts/03-shared-schema.md`:
  - Change `vendor_keys.payload.oauth` writer responsibilities from Go-only runtime refresh to split ownership: Go config/seed, Rust runtime refresh.
  - Remove "Rust 端不读" for `token_endpoint`; Rust reads it.
  - Add version-CAS / single-flight write semantics.
- Update `docs/go-contracts/parts/02-go-as-consumer.md`:
  - Retire §2.4.3 as a Go refresh command.
  - If kept during transition, mark it legacy/pre-K-C only.
  - Post-K-C, any Rust-to-Go event must be status/alert only, not "Go should refresh this token".
- Update `docs/go-contracts/parts/07-oauth-ldap.md`:
  - Preserve Go ownership for user authentication provider config and user token cache.
  - Separate that auth-cache contract from vendor-key runtime OAuth credentials used by upstream vendor egress.
  - Do not state that Rust never sees refresh tokens for `vendor_keys.payload.oauth`; current loader already decrypts `refresh_token_enc` into in-process `OAuthCredentials`.
- Update `docs/go-contracts/parts/08-control-plane.md`:
  - Keep `refresh-oauth` command but define it as invoking Rust's K-C single-flight refresher, not Go refresh.
  - `gateway:instance:<id>:runtime.oauth_rotation` becomes Rust-observed refresh status.

### Rust Runtime Interfaces Needed

- Store writer:
  - New `gw-store` OAuth write-back module or method, likely near `quota_state.rs` / `identity_persist.rs`.
  - Encrypt plaintext token strings using active `KekCache` into the same `EncryptedKey` JSON shape as `loader.rs::OAuthCredentialsRow`.
  - Update `vendor_keys.payload.oauth` with `jsonb_set`, set `updated_at = NOW()`, and increment `version`.
  - Prefer optimistic CAS: `WHERE id = $1 AND version = $expected_version AND deleted_at IS NULL`.
  - Return the new version and enough metadata to broadcast an entity update.
- Event broadcast:
  - XADD `events:entity * ns vendor_key id <key_id> op upsert v <new_version> ts_ms <now> source rust_oauth_refresh`.
  - Invalidate local Moka / Redis L1 as the existing stream consumer will do for other pods.
- Single-flight:
  - Redis lock: `SET lock:oauth:refresh:<key_id> <instance/request> NX PX <bounded_ttl>`.
  - Losers cold-load or wait for entity invalidation, then use the newer token.
  - CAS failure is a successful lost race, not a hard refresh failure; reload and retry injection once.
- Refresher/cache:
  - `OAuthRefresher` or `OauthCache` available to `gw-adapter::AdapterCtx` after W5.
  - `rewrite_auth` remains async and can refresh synchronously before injecting bearer tokens.
  - Must handle rotated `refresh_token` by encrypt-writing the returned value; if the provider omits a new refresh token, preserve the old one.
- Observability:
  - Add success/error/lock-lost/CAS-lost metrics distinct from the old `gw_oauth_refresh_failed_xadd_total`.
  - Existing `gw_oauth_token_near_expiry_total` / `gw_oauth_token_expired_total` may remain but semantics should be updated once refresh happens before injection.

### Old `oauth_failed` Reverse-XADD Retirement

- Pre-K-C transition: keep `OAuthListener` behavior only while Go remains the actual runtime refresher.
- K-C landing: either remove the reverse-XADD branch or repurpose it to an alert/status event that never instructs Go to refresh.
- If repurposed, use a new field/reason so old Go consumers do not accidentally refresh: for example `field=oauth_refresh_failed status=rust_refresh_failed`.
- Update the source-grep tripwire in `crates/gw-proxy/src/error/listeners/oauth.rs` and metric help in `crates/gw-gateway/src/metrics_catalog.rs` when retiring the old branch.

### Files To Modify In W6

- `crates/gw-store/src/encrypt.rs`
- New or modified `crates/gw-store/src/oauth_refresh.rs`
- `crates/gw-store/src/lib.rs`
- `crates/gw-store/src/entity_store.rs` if exposing the writer through `EntityStore`
- `crates/gw-core/src/entity.rs` if the in-process OAuth type needs version/metadata
- `crates/gw-adapter/src/ctx.rs`
- `crates/gw-adapter/src/trait_def.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-proxy/src/error/listeners/oauth.rs`
- `crates/gw-gateway/src/lib.rs`
- `crates/gw-gateway/src/metrics_catalog.rs`
- `docs/go-contracts/contract-change/YYYY-MM-DD-rust-oauth-refresh.md`
- `docs/go-contracts/parts/02-go-as-consumer.md`
- `docs/go-contracts/parts/03-shared-schema.md`
- `docs/go-contracts/parts/07-oauth-ldap.md`
- `docs/go-contracts/parts/08-control-plane.md`
- `docs/spec/07-vendor-integration.md`
- `docs/ROADMAP.md`

## 4. Tunnel Bridge Contract Assumptions

### Current Frame / Transport / Pool / Proxy State

- `crates/gw-tunnel-client/src/frame.rs` defines the frame header: `magic(2) | version(1) | msg_type(1) | flags(2) | stream_id(4) | length(4) | payload`.
- Message types are `Open`, `Data`, `Close`, `Ping`, `Pong`, `Error`, `ControlAuth`, and `ControlNodeInfo`.
- `WsTransport::connect` opens a WebSocket, optionally sends `ControlAuth` on stream `0`, maintains heartbeat via `Ping`/`Pong`, and exposes `send_open(target) -> stream_id` plus `send_data(stream_id, payload)`.
- `ReconnectingTransport` supervises a `WsTransport` and exposes the currently alive handle.
- `TunnelStack::resolve` selects `Direct` or `Use(node_id)`.
- `TunnelStack::peer_for_node` validates DNS/target whitelist and returns an `HttpPeer` pointed at the node WebSocket endpoint, but does not call `send_open` or `send_data`.
- `crates/gw-proxy/src/filters/upstream.rs` can select tunnel vs direct and fail closed on `require_tunnel`, but it does not bridge Pingora HTTP bodies into frames.
- `crates/e2e-fake-upstream/src/tunnel_node.rs` is a mock aether-hub: it accepts `Open { target }`, forwards subsequent `Data` frames to a TCP connection, and sends upstream bytes back as `Data`.

### Recommended HTTP<->WS Bridge Flow

The bridge should not treat the node WebSocket URL as the upstream `HttpPeer`. It needs a stream relay that owns the `WsTransport` handle:

1. Routing selects `TunnelResolution::Use(node_id)`.
2. Gateway validates `host:port` through the same DNS and target whitelist logic currently in `peer_for_node`.
3. Gateway obtains the live `WsTransport` from the node pool.
4. Gateway sends `Open` with a target payload agreed with the remote node.
5. Gateway serializes the upstream request stream into `Data(stream_id, bytes)` frames.
6. Gateway relays response-side `Data(stream_id, bytes)` frames back to the downstream client.
7. Either side maps EOF to `Close(stream_id)`; remote `Error(stream_id)` maps to a gateway error/failover surface.
8. Flow control and `MAX_PAYLOAD_LEN` chunking must be explicit; no single Pingora body chunk may bypass the 4 MiB frame cap.

### External / User Decision Points

The current repository does **not** prove the real remote aether-hub protocol. It only has a local mock whose comments say it mirrors aether-hub. Before W8 source implementation, confirm these with the owner of the remote node:

- `Open` payload format: raw `"host:port"`, URL, or structured JSON?
- TLS ownership: does the remote node open raw TCP and the gateway sends TLS bytes, or does the remote node terminate TLS to the vendor and receive HTTP bytes?
- HTTP version support: HTTP/1.1 bytes only, HTTP/2 frames, or opaque TCP stream?
- Response semantics: are response bytes returned only as `Data`, and how are upstream EOF, remote connect errors, and HTTP status failures represented?
- `ControlAuth` behavior: token payload bytes, ack/error expectation, and whether unauthenticated nodes close immediately.
- `ControlNodeInfo` purpose and whether gateway needs to wait for it before opening streams.
- Multiplexing constraints beyond `max_streams_per_conn`.

If the real aether-hub contract matches `crates/e2e-fake-upstream/src/tunnel_node.rs`, W8 can implement a raw TCP byte relay over `Open`/`Data`/`Close`. If it differs, W8 must update `gw-tunnel-client` and the mock together before claiming e2e coverage.

### Aether-Hub Change Assessment

- No aether-hub source is present in this repository.
- The Rust side already declares the byte frame layout as matching `aether-hub/src/protocol.rs`, but this is an assertion, not proof from this checkout.
- Treat any remote behavior beyond the local mock as an external decision point.
- Aether-hub changes may be required if it does not support bidirectional raw byte relay, close/error frames, or the selected `Open` payload format.

### Files To Modify In W8

- `crates/gw-tunnel-client/src/frame.rs`
- `crates/gw-tunnel-client/src/transport.rs`
- `crates/gw-tunnel-client/src/pool.rs`
- `crates/gw-tunnel-client/tests/*`
- `crates/gw-proxy/src/tunnel_stack.rs`
- `crates/gw-proxy/src/filters/upstream.rs`
- `crates/gw-proxy/src/gateway.rs`
- `crates/gw-gateway/tests/e2e_tunnel.rs`
- `crates/e2e-fake-upstream/src/tunnel_node.rs`
- `.gitlab-ci.yml` / `Dockerfile` only if the separate tunnel artifact job or build args need adjustment
- `docs/spec/09-tunnel.md`
- `docs/workflow.md`
- `docs/architecture.md`
- `docs/ROADMAP.md`

## 5. Wave Unblock Matrix

| Wave | Can start after W0? | Additional blocker / decision |
|---|---:|---|
| W1 R-D API-key affinity | Yes | No new contract decision from W0. Use existing `AffinityManager` and current auth/routing evidence. |
| W2 R-F stream probe | Yes | No new contract decision from W0. Prefer implementation after W1 because both touch `gw-proxy` request/response state. |
| W3 CLI provenance | Yes | Use `ProvenanceTag::CliExtracted` / `"cli_extracted"` and the priority order in this document. Go contract and soak dedupe must update with the code. |
| W4 async `ir_wire_body` | Yes | Preserve F1: task submission may pre-render `ir_wire_body`; task runtime must not depend on or call `gw-adapter`. |
| W5 outbound adapter seam | Yes | Use F1. Fix `trait_def.rs` comment, add `AdapterCtx` OAuth-cache slot, keep direct byte-identical, keep impersonate backed by existing `gw-core::impersonate` / `gw-egress-impersonate`. |
| W6 Rust OAuth refresh | Partially | Plan/store work can start from this document. Production adapter hook integration should wait for W5 `AdapterCtx` seam. Must land the Go-contract ownership delta in the same wave. |
| W7 heavy transforms and Kiro | Partially | Vertex/Codex planning can start after W5. Kiro source work depends on adopting this document's Kiro protocol strings and adding the matching Go/spec contract updates. |
| W8 tunnel bridge/e2e/artifact | Evidence/planning yes; source gated | Source implementation must first confirm the external aether-hub frame contract. Default artifact promotion remains a post-e2e user release decision; keep split artifact while building bridge. |

## New Decision Points Found

1. Kiro is recommended as EventStream-only standard protocol with `ProtocolKind::Kiro`, `ApiShape::KiroEventStream`, `FORMAT_ID = "kiro:eventstream"`, and Go `accessible_endpoints = ["kiro"]`. If product needs Kiro CLI behavior, that should be a separate future `ApiShape`, not this registration.
2. CLI provenance should use a new `"cli_extracted"` tag ranked below `upstream_final` and above `upstream_partial`. Go settler and soak dedupe must match this rank.
3. OAuth contract docs conflict with F2 and must be changed before or during W6. The old `oauth_failed` reverse-XADD cannot remain a Go-refresh command after K-C.
4. The real aether-hub remote protocol is not proven by this repo. W8 must confirm Open payload, TLS ownership, response `Data` semantics, close/error behavior, and auth handshake with the external service owner before source implementation.
