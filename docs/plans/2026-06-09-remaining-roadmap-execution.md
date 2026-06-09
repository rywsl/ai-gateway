# Remaining ROADMAP Execution Wave Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` after each wave is expanded into a bite-sized plan. This document is a program index, not an implementation plan. Do not implement directly from this file.

**Goal:** Schedule all remaining Rust proxy `docs/ROADMAP.md` items after `50ea049 docs(roadmap): confirm adapter and oauth authority`.

**Architecture:** Keep the confirmed F1/F2/F3 authority order: proxy-only outbound adapter seam, Rust-only runtime OAuth refresh writer, and separate tunnel artifact until bridge plus e2e are built. Split execution by runtime boundary so independent workers can collect evidence and implement in isolated branches, while new architecture-boundary conflicts return to the user instead of being silently decided by a worker.

**Tech Stack:** Rust 1.88 workspace, Pingora 0.8, `gw-codec`, `gw-adapter`, `gw-proxy`, `gw-routing`, `gw-ledger`, `gw-task-runtime`, `gw-tunnel-client`, Redis/PgSQL entity contracts, GitLab CI gates from the ROADMAP program.

---

## Authority And Scope

This plan is governed by:

- `docs/plans/2026-06-08-roadmap-implementation-program.md`, specifically F1 and F2.
- `docs/design/2026-06-09-roadmap-authority-and-scope.md`.
- Current `docs/ROADMAP.md`, which is the remaining work inventory.

All items below are in scope:

- Kiro protocol/adapter integration.
- `UsageSource::CliExtracted` replacement via current `usage_provenance`.
- R-D API-key affinity wiring.
- R-F stream probe before the first committed SSE/client event.
- A-T0..A-T6 outbound adapter registry.
- K-C Rust OAuth refresh/write-back.
- Heavy transforms: Kiro SigV4/EventStream, Vertex AI service-account JWT/token exchange, Codex body transform.
- `docs/spec/08-async-tasks.md` residual: codec render of `ir_wire_body`.
- `docs/spec/09-tunnel.md` residuals: HTTP<->WS frame bridge, live e2e, default artifact policy.

Subagents may collect evidence and implement assigned slices. If a subagent finds a conflict with the F1/F2/F3 boundaries, a cross-process contract mismatch, or a release/artifact policy choice not already confirmed, it must stop and return a decision packet to the user.

## Common Execution Protocol

- Use one implementation branch or isolated worktree per wave. If two waves touch the same high-churn files, collect evidence in parallel but serialize source edits.
- Before each wave, expand the wave brief into `docs/plans/YYYY-MM-DD-<wave>.md` with `superpowers:writing-plans`.
- Each wave should produce one to three reviewable commits. Keep contract/docs commits near the code that changes the contract.
- When a ROADMAP item lands, move it from `docs/ROADMAP.md` into the matching `docs/spec/0N-*.md` as as-built behavior and re-run spec checks.
- Do not let task-worker code call outbound adapters. Per F1, task submission renders the wire body before enqueue; the worker sends bytes verbatim.
- Do not let Go refresh runtime OAuth tokens after K-C. Per F2, Go writes config and initial authorization state; Rust is the sole runtime refresh writer.
- Do not change the default tunnel artifact without a user release decision after bridge and e2e are green. Per F3, the default image remains non-tunnel until then.

Common verification gate for code waves:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-invariants.sh
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f"; done
```

## Parallel Lane Map

| Lane | Waves | Can Start After | Parallelism Notes |
|---|---|---|---|
| Program contracts | W0 | now | Evidence and contract mapping only; no source implementation. |
| Routing | W1, W2 | W0 evidence | W1 and W2 can be planned in parallel, but both touch `gw-proxy` request/response state; implement W1 then W2 to reduce merge conflicts. |
| CLI/task normalization | W3, W4 | W0 evidence | W3 and W4 are mostly independent. W4 may reuse codec/provenance evidence from W3 but does not depend on W3 landing. |
| Adapter/OAuth/vendor | W5, W6, W7 | W0, then W5 for runtime seam | W6 and W7 depend on W5's adapter seam and `AdapterCtx` shape. Kiro protocol decisions in W0 also gate the Kiro part of W7. |
| Tunnel | W8 | W0 evidence | Can proceed in parallel with vendor waves after its bridge contract is written. Default artifact change remains a final user decision. |

Recommended implementation order:

1. W0 contract/evidence index.
2. W1 R-D affinity wiring.
3. W2 R-F stream probe.
4. W3 CLI-extracted provenance and metrics cleanup.
5. W4 async `ir_wire_body`.
6. W5 outbound adapter seam.
7. W6 K-C Rust OAuth refresh.
8. W7 heavy transforms and Kiro protocol/adapter.
9. W8 tunnel bridge/e2e/artifact decision packet.

W3, W4, and W8 can run while W1/W2 are being reviewed. W6 and W7 should not write production hooks until W5 lands.

## W0: Contract And Evidence Index

**Goal:** Produce the exact implementation inputs for the remaining waves without changing source code.

**Suggested worker:** one coordinator/evidence worker.

**Depends on:** none.

**File range:**

- Read: `docs/ROADMAP.md`.
- Read: `docs/plans/2026-06-08-roadmap-implementation-program.md`.
- Read: `docs/design/2026-06-09-roadmap-authority-and-scope.md`.
- Read: `docs/spec/01-protocol-codec.md`, `02-routing.md`, `07-vendor-integration.md`, `08-async-tasks.md`, `09-tunnel.md`.
- Read: `docs/go-contracts/parts/02-go-as-consumer.md`, `03-shared-schema.md`, `07-oauth-ldap.md`, `08-control-plane.md`.
- Create when expanded: `docs/plans/YYYY-MM-DD-w0-contract-evidence.md`.

**Required outputs:**

- Kiro protocol contract choice list: `ProtocolKind`, `ApiShape`, `AnyCodec` registration, Go `accessible_endpoints` string, and whether Kiro is CLI-like, EventStream-only, or both.
- CLI usage provenance choice: concrete `usage_provenance` enum/tag value replacing the deleted `UsageSource::CliExtracted`.
- OAuth ownership delta: exact Go-contract sections that stop describing Go runtime refresh.
- Tunnel bridge contract assumptions: remote node frame expectations and whether aether-hub changes are required.

**Boundary rule:** If Kiro shape, CLI provenance priority, OAuth encryption writer, or tunnel remote-node protocol cannot be reconciled with current contracts, return a user decision packet before source implementation.

**Verification commands:**

```bash
rg -n "Kiro|CliExtracted|usage_provenance|oauth_failed|HTTP<->WS|default.*artifact" docs crates
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
bash scripts/check-spec.sh docs/spec/09-tunnel.md
```

**Commit granularity:** one docs commit if W0 creates a persistent evidence doc: `docs(plan): map remaining roadmap contracts`.

## W1: R-D API-Key Affinity Wiring

**Goal:** Make the existing pure `gw-routing::affinity::AffinityManager` influence live upstream selection through authenticated API-key identity, selected-key recording, and invalidation hooks.

**Suggested worker:** routing worker with proxy/auth context.

**Depends on:** W0 evidence. Uses as-built R-AB ranked candidate storage from `docs/spec/02-routing.md`.

**File range:**

- Modify likely: `crates/gw-proxy/src/ctx.rs`, `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/gateway.rs`.
- Modify likely: `crates/gw-gateway/src/lib.rs` or bootstrap wiring where routing managers are constructed.
- Reference: `crates/gw-routing/src/affinity.rs`, `crates/gw-routing/tests/affinity.rs`, `crates/gw-routing/src/strategy/cache_affinity.rs`.
- Reference invalidation listeners: `crates/gw-proxy/src/error/listeners/cache_invalidate.rs`, `crates/gw-proxy/src/error/listeners/key_health.rs`, `crates/gw-proxy/src/error/listeners/oauth.rs`.
- Docs: `docs/spec/02-routing.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Do not alter message-hash affinity in `strategy/cache_affinity.rs`; R-D is API-key/request-external affinity.
- Store the authenticated `ApiKeyId` in request context before routing.
- Consult `AffinityManager::get(AffinityDimension::ApiKey(...))` before choosing a ranked candidate, but fall back to normal ranked routing when the affinity key is absent, unhealthy, blacklisted, or no longer a candidate.
- Call `record_hit` only after a vendor key is actually selected for a send.
- Call `invalidate_vendor_key` from vendor-key invalidation paths.

**Verification commands:**

```bash
cargo test -p gw-routing affinity
cargo test -p gw-proxy affinity
cargo test -p gw-proxy --test capability_filter
cargo test -p gw-proxy --test listeners_wire
bash scripts/check-spec.sh docs/spec/02-routing.md
```

**Commit granularity:**

- `feat(routing): wire api-key affinity into proxy selection`
- `docs(routing): close R-D affinity roadmap entry`

## W2: R-F Stream Probe Failover Window

**Goal:** Allow failover after upstream first byte but before any valid SSE/event bytes are committed to the client.

**Suggested worker:** stream/proxy worker.

**Depends on:** W0 evidence. Prefer W1 merged first because both waves touch request context and failover state.

**File range:**

- Modify likely: `crates/gw-proxy/src/ctx.rs`, `crates/gw-proxy/src/gateway.rs`, `crates/gw-proxy/src/filters/response.rs`, `crates/gw-proxy/src/error_emit.rs`.
- Modify likely: `crates/gw-routing/src/failover.rs` if a stream-specific classification result or error code is missing.
- Modify likely: `crates/gw-gateway/src/metrics_catalog.rs`.
- Tests: `crates/gw-proxy/tests/upstream_envelope.rs`, `crates/gw-gateway/tests/e2e_failure_modes.rs`, focused new stream-probe tests.
- Docs: `docs/spec/02-routing.md`, `docs/workflow.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Introduce explicit phases equivalent to Connecting, Probing, and Streaming.
- Distinguish first upstream byte from first client-committed event. Current `body_started` is too coarse for this ROADMAP item.
- Probe failures include upstream error body, malformed frame, and timeout.
- Emit `gw_stream_probe_*` metrics without key-level labels.
- Once a client-visible event has been committed, preserve existing no-retry semantics.

**Verification commands:**

```bash
cargo test -p gw-routing failover
cargo test -p gw-proxy stream_probe
cargo test -p gw-gateway --test e2e_failure_modes
bash scripts/check-spec.sh docs/spec/02-routing.md
```

**Commit granularity:**

- `feat(routing): add stream probe failover phase`
- `docs(routing): close R-F stream probe entry`

## W3: CLI-Extracted Usage Provenance

**Goal:** Replace the deleted `UsageSource::CliExtracted` concept with a current `usage_provenance` value and ensure CLI extraction observability is complete.

**Suggested worker:** ledger/codec worker.

**Depends on:** W0 CLI provenance choice.

**File range:**

- Modify likely: `crates/gw-core/src/usage.rs`, `crates/gw-ledger/src/usage.rs`, `crates/gw-ledger/tests/usage_schema.rs`.
- Modify likely: `crates/gw-proxy/src/filters/response.rs`, `crates/gw-proxy/src/ctx.rs` if CLI extraction currently promotes only parsed/estimated tags.
- Modify likely: `crates/gw-gateway/src/metrics_catalog.rs` if metric catalog is incomplete.
- Reference: `crates/gw-codec/src/cli/*`, `crates/gw-codec/tests/cli_variants.rs`.
- Docs: `docs/spec/01-protocol-codec.md`, `docs/spec/06-billing-ledger.md`, `docs/go-contracts/parts/02-go-as-consumer.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Do not revive `UsageSource`.
- Add the provenance value where the current per-dimension provenance tags live.
- Ensure Go settler priority ordering in `parts/02-go-as-consumer.md` includes the new provenance if ordering changes.
- If `gw_cli_usage_extract_total` is already implemented, verify it is registered and documented; do not duplicate it.

**Verification commands:**

```bash
cargo test -p gw-core usage
cargo test -p gw-ledger usage_schema
cargo test -p gw-proxy cli
cargo test -p gw-gateway metrics_catalog
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/06-billing-ledger.md
```

**Commit granularity:**

- `feat(ledger): record cli-extracted usage provenance`
- `docs(codec,ledger): close cli provenance roadmap entry`

## W4: Async Task `ir_wire_body`

**Goal:** Make async task requests/responses use codec-normalized wire bodies while keeping task workers out of adapter dispatch.

**Suggested worker:** task-runtime/codec worker.

**Depends on:** W0 evidence. Does not require W5, but must preserve F1's task-worker boundary.

**File range:**

- Modify likely: `crates/gw-task/src/types.rs`, `crates/gw-task/src/contract.rs`, `crates/gw-task/src/redis_contract.rs`.
- Modify likely: `crates/gw-proxy/src/filters/task.rs`, `crates/gw-proxy/tests/task_endpoints.rs`.
- Modify likely: `crates/gw-task-runtime/src/exec_loop.rs`, `crates/gw-task-runtime/tests/exec_loop.rs`.
- Reference codec APIs: `crates/gw-codec/src/any.rs`, `crates/gw-codec/src/wire/any.rs`, `crates/gw-proxy/src/filters/response.rs`.
- Docs: `docs/spec/08-async-tasks.md`, `docs/go-contracts/parts/02-go-as-consumer.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Gateway task submission should parse and render the normalized request into `input_payload.ir_wire_body`.
- Worker should send `ir_wire_body` verbatim when present and keep backward compatibility for old task payloads until migration is complete.
- Response storage should capture codec-normalized response metadata/body when the codec can parse it, with raw fallback documented.
- Do not add a dependency from task runtime to `gw-adapter`.

**Verification commands:**

```bash
cargo test -p gw-task
cargo test -p gw-task-runtime exec_loop
cargo test -p gw-proxy --test task_endpoints
cargo test -p gw-codec cross_protocol
bash scripts/check-spec.sh docs/spec/08-async-tasks.md
```

**Commit granularity:**

- `feat(tasks): enqueue codec-rendered wire body`
- `feat(tasks): normalize worker response storage`
- `docs(tasks): close ir_wire_body roadmap entry`

## W5: A-T0..A-T6 Outbound Adapter Registry

**Goal:** Make `AdapterRegistry::for_vendor` the proxy outbound seam while preserving byte-identical direct vendor behavior and existing impersonate behavior.

**Suggested worker:** adapter/proxy worker.

**Depends on:** W0 evidence and F1. W6/W7 depend on the resulting seam.

**File range:**

- Modify likely: `crates/gw-adapter/src/ctx.rs`, `crates/gw-adapter/src/trait_def.rs`, `crates/gw-adapter/src/registry.rs`, `crates/gw-adapter/src/noop.rs`, `crates/gw-adapter/tests/*`.
- Modify likely: `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/filters/request.rs`, `crates/gw-proxy/src/ctx.rs`.
- Modify likely: `crates/gw-gateway/src/lib.rs`, `crates/gw-gateway/src/config.rs` only if registry construction/config moves.
- Reference: `crates/gw-core/src/impersonate/*`, `crates/gw-codec/src/wire/any.rs`, `scripts/check-invariants.sh`.
- Docs: `docs/spec/07-vendor-integration.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Fix `trait_def.rs` wording so it no longer says task runtime calls the adapter.
- Add the OAuth cache handle/trait slot to `AdapterCtx` without forcing K-C implementation into W5.
- Direct vendors use Noop/short-circuit and must remain byte-identical.
- Impersonate vendors declare transport profile through the adapter seam but continue to reuse `gw-core::impersonate` and `gw-egress-impersonate`.
- New heavy transforms should have extension points but stay disabled until W7.
- Respect the `no_patch_body` invariant: body transforms use `patch_request` over `WireAnyRequest`.

**Verification commands:**

```bash
cargo test -p gw-adapter
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-proxy --test filter_chain_order
cargo test -p gw-gateway --test e2e_impersonate
bash scripts/check-invariants.sh
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
```

**Commit granularity:**

- `feat(adapter): make registry proxy outbound seam`
- `test(adapter): lock direct and impersonate adapter behavior`
- `docs(adapter): close A-T0 adapter registry boundary`

## W6: K-C Rust OAuth Refresh And Write-Back

**Goal:** Move runtime OAuth refresh/write-back to Rust with no Go runtime refresh writer.

**Suggested worker:** OAuth/store worker plus Go-contract reviewer.

**Depends on:** W0 OAuth contract evidence and W5 `AdapterCtx` OAuth cache slot.

**File range:**

- Modify likely: `crates/gw-adapter/src/ctx.rs` and new OAuth cache/refresher module if adapter owns the hook interface.
- Modify likely: `crates/gw-store/src/entity_store.rs`, `crates/gw-store/src/loader.rs`, `crates/gw-core/src/entity.rs`.
- Modify likely: `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/error/listeners/oauth.rs`.
- Modify likely: `crates/gw-gateway/src/kms.rs`, `crates/gw-gateway/src/lib.rs`, config for refresh locks/timeouts if needed.
- Redis/contract docs: `docs/go-contracts/parts/02-go-as-consumer.md`, `03-shared-schema.md`, `07-oauth-ldap.md`, `08-control-plane.md`, new `docs/go-contracts/contract-change/YYYY-MM-DD-rust-oauth-refresh.md`.
- Docs: `docs/spec/07-vendor-integration.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Rust reads `token_endpoint`.
- On expiry, Rust refreshes synchronously for the selected vendor key, encrypts updated `payload.oauth`, persists rotated `refresh_token` when returned, increments version, and broadcasts `events:entity op=upsert`.
- Implement cross-Pod single-flight with Redis lock or version-CAS. This is coordination among Rust writers, not a second writer.
- Retire or repurpose `oauth_failed` reverse-XADD; after K-C it must not ask Go to refresh runtime tokens.
- Preserve Go as config writer and first-time authorization owner.

**Verification commands:**

```bash
cargo test -p gw-core oauth
cargo test -p gw-store oauth
cargo test -p gw-proxy oauth
cargo test -p gw-gateway kms
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
rg -n "oauth_failed|Go.*refresh|Rust.*refresh|token_endpoint" docs/go-contracts docs/spec crates
```

**Commit granularity:**

- `feat(oauth): add rust refresh cache and singleflight`
- `feat(oauth): persist refreshed tokens and broadcast vendor key`
- `docs(oauth): move runtime refresh ownership to rust`

## W7: Heavy Vendor Transforms And Kiro Protocol

**Goal:** Land Kiro, Vertex AI, and Codex heavy outbound transforms behind the W5 adapter seam, including Kiro protocol registration.

**Suggested workers:** three vendor workers plus one adapter integrator:

- Kiro worker: protocol/codec/SigV4/EventStream.
- Vertex worker: service-account JWT and token exchange.
- Codex worker: request-body transform.
- Integrator: registry wiring, capability/docs/tests.

**Depends on:** W0 Kiro protocol contract and W5 adapter seam. Kiro also depends on any protocol contract docs from W0.

**File range:**

- Kiro: `crates/gw-codec/src/kiro/mod.rs`, `crates/gw-codec/src/any.rs`, `crates/gw-core/src/protocol.rs`, `crates/gw-codec/tests/kiro_eventstream.rs`, new adapter module under `crates/gw-adapter/src/`.
- Vertex: new adapter module under `crates/gw-adapter/src/`, `crates/gw-adapter/Cargo.toml`, possible capability mapping in `crates/gw-capability`.
- Codex: `crates/gw-core/src/impersonate/openai_codex.rs`, `crates/gw-codec/src/wire/any.rs`, new adapter module under `crates/gw-adapter/src/`.
- Shared: `crates/gw-adapter/src/registry.rs`, `crates/gw-adapter/src/trait_def.rs`, `crates/gw-proxy/src/filters/upstream.rs`.
- Docs/contracts: `docs/spec/01-protocol-codec.md`, `docs/spec/07-vendor-integration.md`, `docs/go-contracts/parts/03-shared-schema.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- Kiro is not complete until `AnyCodec`, `ProtocolKind`, `ApiShape`, accessible endpoint contracts, and adapter registry wiring agree.
- Vertex RS256 signing/token exchange should avoid leaking private-key material into logs or metrics.
- Codex body transform must use `patch_request`, not a forbidden `patch_body` API.
- Keep direct and impersonate baseline snapshots from W5 green after each heavy transform.

**Verification commands:**

```bash
cargo test -p gw-codec kiro
cargo test -p gw-adapter kiro
cargo test -p gw-adapter vertex
cargo test -p gw-adapter codex
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-core wire_enum_lock
bash scripts/check-invariants.sh
bash scripts/check-spec.sh docs/spec/01-protocol-codec.md
bash scripts/check-spec.sh docs/spec/07-vendor-integration.md
```

**Commit granularity:**

- `feat(codec): register kiro protocol shape`
- `feat(adapter): add kiro outbound transform`
- `feat(adapter): add vertex service-account transform`
- `feat(adapter): add codex request transform`
- `docs(vendor): close heavy adapter transform roadmap entries`

## W8: Tunnel HTTP<->WS Bridge, E2E, And Artifact Policy

**Goal:** Complete the production bridge from Pingora HTTP streams to tunnel WebSocket frames, add a live round-trip e2e, and produce the default artifact release decision point.

**Suggested worker:** tunnel/proxy worker plus release-doc reviewer.

**Depends on:** W0 tunnel bridge evidence. Can run in parallel with vendor waves after contract evidence is clear.

**File range:**

- Modify likely: `crates/gw-tunnel-client/src/frame.rs`, `crates/gw-tunnel-client/src/transport.rs`, `crates/gw-tunnel-client/src/pool.rs`, `crates/gw-tunnel-client/tests/transport.rs`.
- Modify likely: `crates/gw-proxy/src/tunnel_stack.rs`, `crates/gw-proxy/src/filters/upstream.rs`, `crates/gw-proxy/src/gateway.rs`.
- Modify likely: `crates/gw-gateway/tests/e2e_tunnel.rs`, possibly tunnel test fixtures.
- CI/artifact docs: `.gitlab-ci.yml`, `Dockerfile` only if the separate tunnel image job/tag needs adjustment.
- Docs: `docs/spec/09-tunnel.md`, `docs/workflow.md`, `docs/architecture.md`, `docs/ROADMAP.md`.

**Implementation notes:**

- `WsTransport::send_open` and `send_data` already exist; this wave adds production callers and response relay.
- The remote node protocol must be explicit before implementation. If aether-hub frame behavior is missing or incompatible, return a decision packet instead of inventing a unilateral protocol.
- The e2e must exercise live WS node behavior: `ControlAuth`, `Open`, `Data`, response relay, DNS/target denial, and `peer_for_node`.
- Keep the default artifact non-tunnel while building and testing the bridge.
- After bridge and e2e pass, create a release decision packet: keep separate `latest-tunnel` or promote tunnel into default `latest`. Do not change default without user confirmation.

**Verification commands:**

```bash
cargo test -p gw-tunnel-client
cargo test -p gw-proxy --features tunnel tunnel
cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored
cargo build --release --locked -p gw-gateway
cargo build --release --locked --features tunnel -p gw-gateway
bash scripts/check-spec.sh docs/spec/09-tunnel.md
```

**Commit granularity:**

- `feat(tunnel): bridge http streams to ws frames`
- `test(tunnel): add live tunnel roundtrip e2e`
- `docs(tunnel): close bridge and artifact policy residuals`

## Merge And Review Strategy

Use this merge order unless a wave reports a blocker:

1. W1, then W2, because both touch proxy routing state.
2. W3 and W4 can merge when green; resolve docs/spec overlap manually.
3. W5 before W6/W7.
4. W6 and W7 can be reviewed independently after W5, but merge W6 before any adapter that consumes runtime OAuth refresh.
5. W8 can merge after its e2e is green, but default artifact promotion waits for the user release decision.

Each wave review must include:

- Spec-compliance review against the ROADMAP item and confirmed authority docs.
- Code-quality review for unnecessary abstraction, especially around the adapter seam.
- Contract review when `docs/go-contracts` changes.
- Verification output pasted into the wave handoff.

## Final Definition Of Done

The remaining ROADMAP program is complete only when:

- `docs/ROADMAP.md` has no remaining entries for sections 01, 02, 07, 08, or 09 listed in this plan.
- `docs/spec/01-protocol-codec.md`, `02-routing.md`, `07-vendor-integration.md`, `08-async-tasks.md`, and `09-tunnel.md` describe the new as-built behavior.
- Go contracts describe Rust-owned runtime OAuth refresh and CLI provenance semantics.
- Full common verification gate passes.
- Any tunnel default artifact change has an explicit user decision recorded after W8 bridge/e2e verification.
