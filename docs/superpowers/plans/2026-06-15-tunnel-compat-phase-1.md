# Tunnel Compat Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 1 of the Cnexus ↔ Aether tunnel compatibility spec: remove the `--features tunnel` build flag (tunnel becomes default-compile, since Phase 2 makes it core infrastructure); add a `TunnelProtocol` discriminator to `ProxyNode`; reorganize `gw-tunnel-client` so existing Cnexus transport code lives under a `cnexus/` submodule; introduce the `Transport` trait + `StreamTarget` enum abstraction in `core.rs` so Phase 2 can plug Aether in without breaking existing code; ship Go-side admin-API protocol field + Aether bootstrap registration endpoint; and stand up the `tests/tunnel-e2e/` Docker harness with the 3 Cnexus-protocol scenarios that were previously skipped — without changing any wire behaviour.

**Branch / MR model:**
- **`ailinkmax-gateway` repo** — Phase 1 + Phase 2 + Phase 3 all accumulate on a **single shared feature branch** (suggested name `feature/tunnel-compat`); only one final MR to `main` after all three phases land. The "PR 1a / PR 1c" labels below are **logical commit groups within that single branch**, not independent MRs to `main`. PR 1a's clean-then-feature ordering is preserved as commit sequence for review-readability of the final MR's diff history.
- **`AILinkMax-Go` repo** — Phase 1 Go work (PR 1b), Phase 2 Go work (G3 heartbeat consumer), and Phase 3 Go work (G4 frontend) are **real PRs/MRs in that repo** with cross-repo coordination per spec §3.6.8. PR 1b lands first per the spec.

**Architecture:** Three commit groups on the `ailinkmax-gateway` shared branch plus one cross-repo Go PR.
- **PR 1a — Rust foundation** (Tasks 0a-0f, 1-8, this repo, on shared branch): Stage 0 (Tasks 0a-0f) removes `--features tunnel` and consolidates the build into a single default-compile artifact (supersedes `docs/design/2026-06-10-tunnel-artifact-release-decision.md`). Stage 1 (Tasks 1-8) introduces the discriminator enum, the migration, the source-layout move into `cnexus/`, and the protocol-neutral abstraction in `gw-tunnel-client/src/{core,target}.rs`. Default `tunnel_protocol = Cnexus`, deserialization is forward-compatible, runtime behaviour of an `enabled = false` deploy is unchanged.
- **PR 1b — Go management plane** (Tasks G1.1-G2.3 + G-gate, `AILinkMax-Go` repo, **real MR**): adds `Protocol string` field to the proxynode entity and admin APIs (G1, spec §3.6.2) and `POST /api/admin/proxy-nodes/register` plus `aether_register.go` upsert logic (G2, spec §3.6.1). Required so Phase 2's `aether-proxy` containers can bootstrap. Lands as its own MR in the Go repo **before** the Rust shared branch's final MR.
- **PR 1c — tunnel-e2e harness** (Tasks 9-19, this repo, on shared branch): introduces `tests/tunnel-e2e/` with its own `docker-compose.yml` (no Aether services yet — that lands in Phase 2), exercises the 3 currently-skipped tunnel scenarios via a `gw-tunnel-server` container, adds `Makefile` targets, and registers the Aether submodule with `update = none` for Phase 2 to consume.

**Tech Stack:** Rust 1.88 (pinned), tokio, serde, sqlx, futures, Go 1.21+ (Gin, GORM per existing `AILinkMax-Go` conventions), pytest 8.x, Postgres 16, Redis 7, Docker Compose v2.

**Reference spec:** `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` (§1, §2, §3.1, §3.2, §3.3, §3.4, §3.6.1, §3.6.2, §6.1, §6.3, §6.4, §6.5 A-scenarios, §7 Phase 1).

**Out of scope for this plan (deferred to Phase 2/3 plans):** Aether wire protocol (`aether/` submodule under `gw-tunnel-client`), `InboundListener`, `/api/internal/aether-tunnel` route, `StreamTarget::Http` variant (Phase 1 ships `Tcp`-only), `AnyTransport` enum (deferred; Phase 1 uses `Arc<dyn Transport>` directly), Aether Docker images, Aether scenarios, mixed-pool scenarios, dep-boundary script, Go G3 heartbeat consumer (Phase 2), Go G4 frontend UI (Phase 3).

**Verification policy:** Per project convention all `cargo` compile/test commands run via `cargo-remote-ctl` (remote build runner). `docker compose` / `make tunnel-e2e` / `make e2e` / `psql` / shell utilities run locally. Migrations are **not** auto-applied (see `CLAUDE.md`): the tunnel-e2e harness applies them via a shell runner pre-baked into Phase 1.

---

## File Structure

### New files (PR 1a, Stage 0 — `--features tunnel` removal, this repo)

| Path | Responsibility |
|---|---|
| `docs/design/2026-06-15-tunnel-default-compile-decision.md` | New ADR superseding `docs/design/2026-06-10-tunnel-artifact-release-decision.md`. Records: why the split-artifact policy made sense on 2026-06-10 (tunnel was not production-validated); why this Phase 1 reverses it (tunnel-compat work productionizes it via the e2e harness and the cross-protocol abstraction); the new policy (single default-compile image; runtime gate `cfg.tunnel.enabled` is the only off-switch). |

### New files (PR 1a, Stage 1 — functional, this repo)

| Path | Responsibility |
|---|---|
| `crates/gw-core/src/tunnel_protocol.rs` | New `TunnelProtocol` enum (`Cnexus` \| `Aether`) with `Default = Cnexus`. Distinct from existing `gw-core::protocol::ProtocolKind` which discriminates vendor API families (OpenAi/Anthropic/Gemini/Kiro). |
| `crates/gw-tunnel-client/src/cnexus/mod.rs` | New submodule that re-exports the contents of `frame.rs`, `pool.rs`, `transport.rs`. |
| `crates/gw-tunnel-client/src/core.rs` | Protocol-neutral abstraction: `Transport` trait (`open_stream`, `alive`, `last_heartbeat_age`, `tunnel_protocol`, `close`), `TransportError` enum, `StreamHandle` boxed trait object. Phase 2 adds `aether::WsTransport` as a second impl behind this trait. |
| `crates/gw-tunnel-client/src/target.rs` | `StreamTarget` enum. Phase 1 ships **only** the `Tcp { host, port, tls }` variant; Phase 2 adds the `Http { ... }` variant for Aether. |
| `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql` | Backfill payload `protocol` key on existing rows; add partial index on `payload->>'protocol'`. |

### New files (PR 1b, `AILinkMax-Go` repo)

> PR 1b lives in `AILinkMax-Go`, not `ailinkmax-gateway`. All file paths below are relative to the **Go** repo root (`/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/`).

| Path | Responsibility |
|---|---|
| `modules/proxynode/business/aether_register.go` | Upsert-by-token logic for Aether bootstrap registration (spec §3.6.1). Lookup by decrypted `ae_*` token among `protocol='aether'` rows, update mutable fields, return `node_id`. Auto-provision flag gated by env var `AETHER_AUTO_PROVISION` (default off). |
| `modules/proxynode/api/aether_register_handler.go` | HTTP handler for `POST /api/admin/proxy-nodes/register`. Mounted on a **separate route** from the existing admin handlers; uses dedicated middleware that only accepts `ae_*`-prefixed tokens (spec §3.6.1.1). |
| `modules/proxynode/api/aether_token_middleware.go` | Bearer middleware that validates `ae_*` prefix and decrypts against `proxy_nodes.auth_token_enc` column. Distinct from existing admin-token middleware. |
| `modules/proxynode/business/aether_register_test.go` | Table-driven tests for upsert/auto-provision/reject branches. |
| `modules/proxynode/api/aether_register_handler_test.go` | Handler-layer tests (401 on bad prefix, 404 on unknown token + auto-provision off, 200 on match). |

### Files moved (PR 1a)

| From | To | Why |
|---|---|---|
| `crates/gw-tunnel-client/src/frame.rs` | `crates/gw-tunnel-client/src/cnexus/frame.rs` | Cnexus-protocol-specific frame codec; isolates from forthcoming `aether/` peer. |
| `crates/gw-tunnel-client/src/pool.rs` | `crates/gw-tunnel-client/src/cnexus/pool.rs` | `ReconnectingTransport` supervisor is dial-direction specific to Cnexus. |
| `crates/gw-tunnel-client/src/transport.rs` | `crates/gw-tunnel-client/src/cnexus/transport.rs` | Cnexus WS transport. |
| `crates/gw-tunnel-client/tests/frame.rs` | `crates/gw-tunnel-client/tests/cnexus_frame.rs` | Mirror the src move. |
| `crates/gw-tunnel-client/tests/pool.rs` | `crates/gw-tunnel-client/tests/cnexus_pool.rs` | Mirror the src move. |
| `crates/gw-tunnel-client/tests/transport.rs` | `crates/gw-tunnel-client/tests/cnexus_transport.rs` | Mirror the src move. |

### Files kept at top level (PR 1a)

| Path | Why it does not move |
|---|---|
| `crates/gw-tunnel-client/src/resolver.rs` | Protocol-agnostic node selection. Operates on `ProxyNodeView` regardless of wire protocol. |
| `crates/gw-tunnel-client/src/safe_dns.rs` | Generic DNS resolver. |
| `crates/gw-tunnel-client/src/target_filter.rs` | Generic target allow-list. |
| Their corresponding tests | Same reason. |

### Files modified (PR 1a, Stage 0 — `--features tunnel` removal, this repo)

| Path | Change |
|---|---|
| `crates/gw-proxy/Cargo.toml` | Remove `[features]` table entirely. Remove `optional = true` from `gw-tunnel-client`, `tokio-rustls`, `webpki-roots` dep lines (they become unconditional deps). |
| `crates/gw-gateway/Cargo.toml` | Remove `[features]` table entirely. Remove `optional = true` from `gw-tunnel-client` dep line. |
| `crates/gw-proxy/src/lib.rs` | Remove `#[cfg(feature = "tunnel")]` attrs on lines 27, 29 (modules become unconditional). |
| `crates/gw-proxy/src/gateway.rs` | Remove `#[cfg(feature = "tunnel")]` attrs on lines 372, 472, 530, 952. Comment at line 370 referencing `feature = "tunnel"` updated to describe runtime gate only. |
| `crates/gw-proxy/src/filters/upstream.rs` | Remove `#[cfg(feature = "tunnel")]` attrs on lines 175, 481, 503, 533, 1173. **Delete** `#[cfg(not(feature = "tunnel"))]` blocks at lines 75, 614, 1187 (fallback Direct path that is now always-available via `cfg.tunnel.enabled = false`). **Delete** the `#[cfg(all(test, not(feature = "tunnel")))]` test module block at line 2513 (its tests are subsumed by the always-on tunnel build with runtime-off config). |
| `crates/gw-gateway/src/lib.rs` | Remove `#[cfg(feature = "tunnel")]` attrs on lines 45, 104, 106, 362, 708, 1211, 1214, 1217, 1451, 1509, 1589, 1591. Replace the `#[cfg_attr(not(feature = "tunnel"), allow(unused_mut))]` at line 1506 with a plain `let mut` (the mut is always used now). |
| `crates/gw-gateway/src/health.rs` | Remove `#[cfg(feature = "tunnel")]` attrs on lines 59, 318, 369. |
| `crates/gw-gateway/tests/e2e_tunnel.rs` | Remove file-level `#![cfg(feature = "tunnel")]` at line 6. Remove the matching `--features tunnel` instruction from the file header comment at line 4. |
| `crates/gw-gateway/tests/health_endpoint.rs` | Remove `#[cfg(feature = "tunnel")]` at line 39. |
| `.gitlab-ci.yml` | Delete the entire `rust-build-tunnel` job (lines ~40-50). Delete the entire `push-tunnel-image` job (lines ~70-90). The default `push-image` job already publishes the single image and stays unchanged. |
| `CLAUDE.md` | Tunnel bullet (line 35) updated: drop `--features tunnel` reference, keep runtime gate semantics (`cfg.tunnel.enabled`). |
| `AGENTS.md` | Lockstep-mirror the `CLAUDE.md` edit (per `CLAUDE.md` §Sync rule). |
| `docs/architecture.md` | Line 46 `gw-tunnel-client` row: drop `(under feature = "tunnel")` clause. Line 76 diagram comment `(feature="tunnel")` → `(default)`. |
| `docs/workflow.md` | Line 163: rewrite the "If `feature = "tunnel"` is built AND ..." sentence to drop the feature-flag conjunction; keep the runtime check `LlmGateway::tunnel = Some(_)`. |
| `docs/ROADMAP.md` | Line 85: strike the F3 "split artifact" decision; cross-reference the new ADR `docs/design/2026-06-15-tunnel-default-compile-decision.md`. |
| `docs/go-contracts/parts/08-control-plane.md` | The `features.tunnel` field in the published JSON shape: keep present, fix value to `true` (Option A per 2026-06-15 decision). Add a one-line note that the field is retained for forward-compat with existing Go consumers and now always emits `true`. |

### Files modified (PR 1a, Stage 1 — functional, this repo)

| Path | Change |
|---|---|
| `crates/gw-core/src/lib.rs` | `pub mod tunnel_protocol;` |
| `crates/gw-core/src/entity.rs` | Add `pub tunnel_protocol: TunnelProtocol` field to `ProxyNode` with `#[serde(default)]`. |
| `crates/gw-tunnel-client/src/lib.rs` | Replace `pub mod {frame, pool, transport};` with `pub mod cnexus; pub mod core; pub mod target;` plus forward exports `pub use core::{Transport, TransportError, StreamHandle}; pub use target::StreamTarget;`. Keep `resolver`/`safe_dns`/`target_filter` mod lines unchanged. **No back-compat re-exports** — call sites are migrated to `cnexus::` paths in the same task (Task 6). |
| `crates/gw-tunnel-client/src/resolver.rs` | Add `pub tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol` to `ProxyNodeView`. |
| `crates/gw-tunnel-client/src/cnexus/{frame,pool,transport}.rs` | Update internal `crate::frame::` / `crate::pool::` / `crate::transport::` paths to `crate::cnexus::frame::` etc. |
| `crates/gw-tunnel-client/src/cnexus/transport.rs` | Add `impl Transport for WsTransport` adapter (convert `TunnelError` ↔ `TransportError`, wrap `TunnelStream` into `StreamHandle`, only accept `StreamTarget::Tcp`). |
| `crates/gw-tunnel-client/src/cnexus/pool.rs` | Add `impl Transport for ReconnectingTransport` delegating to inner cnexus transport. |
| `crates/gw-proxy/src/tunnel_bridge.rs` | Migrate `use gw_tunnel_client::transport::{TunnelStreamReader, TunnelStreamWriter, WsTransport};` to `use gw_tunnel_client::cnexus::transport::{...};`. |
| `crates/gw-gateway/src/lib.rs` | Two changes: (a) migrate `use gw_tunnel_client::transport::TransportConfig;` (line 1223) and `use gw_tunnel_client::pool::PoolConfig;` (line 1219) to the explicit `cnexus::` paths; (b) `tunnel_refresher` reads `payload.tunnel_protocol` from `ProxyNode` and propagates into `ProxyNodeView`. |
| `crates/gw-gateway/tests/e2e_tunnel.rs` | Migrate `use gw_tunnel_client::{transport, pool}::*;` imports (lines 10, 14) to the explicit `cnexus::` paths. |
| `crates/gw-tunnel-server/src/lib.rs` | Migrate `use gw_tunnel_client::frame::{...};` at line 5 to `gw_tunnel_client::cnexus::frame::{...};`. |
| `crates/gw-tunnel-server/tests/protocol.rs` | Migrate `use gw_tunnel_client::transport::{...};` at line 3 to `gw_tunnel_client::cnexus::transport::{...};`. |
| `docs/spec/09-tunnel.md` | New subsection "Per-node protocol discriminator" + brief mention of `core.rs` `Transport` trait. |
| `docs/architecture.md` | Append entries under `gw-tunnel-client` for the `cnexus/` submodule and the protocol-neutral `core.rs` trait. |

### Files modified (PR 1b, `AILinkMax-Go` repo)

| Path | Change |
|---|---|
| `modules/proxynode/business/entity.go` | Add `Protocol string` field to the proxynode struct (json tag `protocol`, validate `oneof=cnexus aether`, GORM default `cnexus`). |
| `modules/proxynode/business/service.go` | Validate `Protocol` enum on create/update; default `"cnexus"`; reject unknown values with 400. Transition rule per spec §3.6.3 (Cnexus→Aether and Aether→Cnexus both allowed; both trigger `events:entity` push). |
| `modules/proxynode/api/routes.go` | `GET /api/admin/proxy-nodes` response includes `protocol`; `POST /api/admin/proxy-nodes/manual` accepts `protocol`; `PATCH /api/admin/proxy-nodes/{id}` accepts `protocol`. Register new route `POST /api/admin/proxy-nodes/register` under the Aether-token middleware. |
| `modules/proxynode/api/handlers_test.go` | Extend table-driven tests to cover the new `protocol` field on each touched route. |
| `modules/proxynode/business/service_test.go` | Extend cases for enum validation + transition. |

### New files (PR 1c, this repo)

| Path | Responsibility |
|---|---|
| `tests/tunnel-e2e/README.md` | Usage, scenario catalog, common-failure index, `git submodule update --init` instructions. |
| `tests/tunnel-e2e/docker-compose.yml` | Postgres 16 + Redis 7 + fake-upstream + cnexus-tunnel-server + ali-gateway + pytest-runner. **No** `aether-proxy` / `aether-hub` (lands in Phase 2). |
| `tests/tunnel-e2e/pytest.ini` | Pytest config: `testpaths = scenarios`, junit-xml, asyncio off. |
| `tests/tunnel-e2e/requirements.txt` | `pytest`, `pytest-httpx`, `psycopg[binary]`, `redis`, `httpx`, `pyyaml`. |
| `tests/tunnel-e2e/conftest.py` | `pg_conn`, `redis_client`, `gateway_client`, `seed_nodes` fixtures; admin token from env. |
| `tests/tunnel-e2e/seed/01_schema.sh` | Wrapper that applies migrations 0001-0011 to `postgres`. |
| `tests/tunnel-e2e/seed/02_provider_keys.sql` | Minimal vendor / vendor_key seed enabling a `fake` provider against `fake-upstream`. |
| `tests/tunnel-e2e/scenarios/__init__.py` | Empty. |
| `tests/tunnel-e2e/scenarios/test_cnexus_protocol.py` | Three scenarios from spec §6.5.A. |
| `tests/tunnel-e2e/support/__init__.py` | Empty. |
| `tests/tunnel-e2e/support/http_helpers.py` | `make_proxy_request(client, *, require_tunnel)` plus a direct-vs-tunnel response collector for the byte-identity test. |
| `tests/tunnel-e2e/.gitignore` | `logs/`, `.pytest_cache/`. |
| `.gitmodules` | `tests/tunnel-e2e/aether-src` → `https://gitlab.ailinkmax.com/ailinkmax/Aether`, `update = none` (Phase 1 does not consume it; Phase 2 does). |

### Files modified (PR 1c, this repo)

| Path | Change |
|---|---|
| `Makefile` | New PHONY targets `tunnel-e2e`, `tunnel-e2e-up`, `tunnel-e2e-down`, `tunnel-e2e-logs`, `tunnel-e2e-deps`. |

---

## PR 1a · Stage 0 — Remove `--features tunnel`, then `TunnelProtocol` discriminator, migration, `cnexus/` reorg, `Transport` trait

> **Stage 0 (Tasks 0a-0f)** removes the `--features tunnel` flag and consolidates the build to a single default-compile artifact. Zero functional change: every `cfg(feature = "tunnel")` block becomes unconditional, every `cfg(not(feature = "tunnel"))` fallback block is deleted (covered now by runtime gate `cfg.tunnel.enabled = false`). Supersedes `docs/design/2026-06-10-tunnel-artifact-release-decision.md`. **Must complete in order Tasks 0a → 0b → 0c → 0d → 0e → 0f** — the Cargo `[features]` removal in 0c must come **after** the cfg-gate removals (0a, 0b), otherwise the `cfg(not(feature = "tunnel"))` blocks would flip on during the intermediate state and conflict with the always-on tunnel-arm code.
>
> **Stage 1 (Tasks 1-8)** then introduces the functional Phase 1 changes.

### Task 0a: Remove `cfg(feature = "tunnel")` gates from `gw-proxy`

**Files:**
- Modify: `crates/gw-proxy/src/lib.rs` (2 gates)
- Modify: `crates/gw-proxy/src/gateway.rs` (4 gates + 1 doc comment)
- Modify: `crates/gw-proxy/src/filters/upstream.rs` (5 `cfg(feature="tunnel")` + 3 `cfg(not(feature="tunnel"))` + 1 `cfg(all(test, not(feature="tunnel")))`)

- [ ] **Step 1: Inspect each cfg site to confirm the deletion semantics**

Run: `grep -n 'cfg(feature = "tunnel")\|cfg(not(feature = "tunnel"))\|cfg(all(test, not(feature = "tunnel")))' crates/gw-proxy/src/{lib.rs,gateway.rs,filters/upstream.rs}`
Expected output:

```
crates/gw-proxy/src/lib.rs:27:#[cfg(feature = "tunnel")]
crates/gw-proxy/src/lib.rs:29:#[cfg(feature = "tunnel")]
crates/gw-proxy/src/gateway.rs:372:    #[cfg(feature = "tunnel")]
crates/gw-proxy/src/gateway.rs:472:        #[cfg(feature = "tunnel")] tunnel: Option<Arc<crate::tunnel_stack::TunnelStack>>,
crates/gw-proxy/src/gateway.rs:530:            #[cfg(feature = "tunnel")]
crates/gw-proxy/src/gateway.rs:952:            #[cfg(feature = "tunnel")]
crates/gw-proxy/src/filters/upstream.rs:75:#[cfg(not(feature = "tunnel"))]
crates/gw-proxy/src/filters/upstream.rs:175:    #[cfg(feature = "tunnel")] tunnel: Option<&Arc<crate::tunnel_stack::TunnelStack>>,
crates/gw-proxy/src/filters/upstream.rs:481:    #[cfg(feature = "tunnel")]
crates/gw-proxy/src/filters/upstream.rs:503:        #[cfg(feature = "tunnel")]
crates/gw-proxy/src/filters/upstream.rs:533:        #[cfg(feature = "tunnel")]
crates/gw-proxy/src/filters/upstream.rs:614:        #[cfg(not(feature = "tunnel"))]
crates/gw-proxy/src/filters/upstream.rs:1173:#[cfg(feature = "tunnel")]
crates/gw-proxy/src/filters/upstream.rs:1187:#[cfg(not(feature = "tunnel"))]
crates/gw-proxy/src/filters/upstream.rs:2513:#[cfg(all(test, not(feature = "tunnel")))]
```

For each `cfg(not(feature = "tunnel"))` block, Read the surrounding ~30 lines to confirm it is the **fallback Direct path** (typically a `Direct::HttpPeer::new(...)` construction that bypasses `TunnelStack`). If any block contains logic that is **not** subsumed by the runtime gate `cfg.tunnel.enabled = false`, stop and report — that case needs design discussion before deletion.

- [ ] **Step 2: Delete the cfg gates and the fallback blocks**

`gw-proxy/src/lib.rs` lines 27, 29: remove `#[cfg(feature = "tunnel")]` attrs; the `pub mod tunnel_stack;` and `pub mod tunnel_bridge;` lines become unconditional.

`gw-proxy/src/gateway.rs`:
- Line 370 comment: change `/// feature = "tunnel" by gw-gateway::bootstrap only when` → `/// (runtime) by gw-gateway::bootstrap only when`.
- Lines 372, 472, 530, 952: remove the `#[cfg(feature = "tunnel")]` attrs. Field, parameter, and field-init become unconditional.

`gw-proxy/src/filters/upstream.rs`:
- Lines 175, 481, 503, 533, 1173: remove the `#[cfg(feature = "tunnel")]` attrs (keep the code).
- Lines 75, 614, 1187: **delete the entire `#[cfg(not(feature = "tunnel"))]` block** including the attr and the body up to the next non-cfg sibling. Refer to the symmetric tunnel-arm block as the authoritative behavior; the now-deleted block was its `else` arm.
- Line 2513: delete the entire `#[cfg(all(test, not(feature = "tunnel")))]` test module block (closing `}` included). The tunnel-build test module already covers these cases with `cfg.tunnel.enabled = false` runtime config.

- [ ] **Step 3: Compile `gw-proxy` standalone**

Run: `cargo-remote-ctl build -p gw-proxy`
Expected: clean. (`gw-proxy/Cargo.toml` still has the feature def at this point; the binary is unaffected because the feature isn't activated. The cfg-attr deletions just make the code unconditional. We delete the feature def in Task 0c after the gates are gone.)

- [ ] **Step 4: Run `gw-proxy` tests (no `--features tunnel`)**

Run: `cargo-remote-ctl test -p gw-proxy`
Expected: PASS. This is the gate that proves the non-tunnel fallback deletions did not regress any test. The pre-existing tunnel-arm code path is now exercised unconditionally.

> If a test fails here, it is one of: (a) a test that exercised the non-tunnel fallback's distinct semantics — investigate and adapt; (b) a real bug surfaced by the always-on tunnel-arm — file and split off. Do NOT just delete the failing test.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/lib.rs crates/gw-proxy/src/gateway.rs crates/gw-proxy/src/filters/upstream.rs
git commit -m "refactor(gw-proxy): remove cfg(feature=\"tunnel\") gates — tunnel becomes default-compile"
```

---

### Task 0b: Remove `cfg(feature = "tunnel")` gates from `gw-gateway`

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs` (12 gates incl. 1 `cfg_attr`)
- Modify: `crates/gw-gateway/src/health.rs` (3 gates)
- Modify: `crates/gw-gateway/tests/e2e_tunnel.rs` (file-level attr + header comment)
- Modify: `crates/gw-gateway/tests/health_endpoint.rs` (1 gate)

- [ ] **Step 1: Inspect each cfg site**

Run: `grep -n 'cfg(feature = "tunnel")\|cfg_attr(not(feature = "tunnel")\|cfg(feature = "tunnel"\|cfg!(feature' crates/gw-gateway/src/{lib.rs,health.rs} crates/gw-gateway/tests/{e2e_tunnel.rs,health_endpoint.rs}`
Expected output: the 17 sites listed in "Files modified (PR 1a, Stage 0)".

- [ ] **Step 2: Delete the cfg gates**

`gw-gateway/src/lib.rs`:
- Lines 45, 104, 106, 362, 708, 1211, 1214, 1217, 1451, 1509, 1589, 1591: remove `#[cfg(feature = "tunnel")]` attrs. Code (use lines, fields, blocks) becomes unconditional.
- Line 1506: replace `#[cfg_attr(not(feature = "tunnel"), allow(unused_mut))]` with no attribute (the `let mut` is always used now; rustc won't warn).

`gw-gateway/src/health.rs`:
- Lines 59, 318: remove `#[cfg(feature = "tunnel")]` attrs.
- Line 369: this `#[cfg(feature = "tunnel")]` likely gates a free function (probably `build_tunnel_healthz_body`); also remove. Pre-existing note in `.omo/notepads/v2-admin-tunnel-cleanup/learnings.md` warns that `escape_json_string` is feature-gated dead code without `--features tunnel` — verify and remove that gate too if it appears here.

`gw-gateway/tests/e2e_tunnel.rs`:
- Line 4 header comment: replace `cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored --nocapture` with `cargo test -p gw-gateway --test e2e_tunnel -- --ignored --nocapture`.
- Line 6: delete `#![cfg(feature = "tunnel")]`.

`gw-gateway/tests/health_endpoint.rs`:
- Line 39: remove `#[cfg(feature = "tunnel")]` attr.

- [ ] **Step 3: Compile `gw-gateway` standalone**

Run: `cargo-remote-ctl build -p gw-gateway`
Expected: clean.

- [ ] **Step 4: Run `gw-gateway` tests (no `--features tunnel`)**

Run: `cargo-remote-ctl test -p gw-gateway`
Expected: PASS. `e2e_tunnel.rs` is `#[ignore]`-gated per its existing convention and is not run; the gate-removal only affects whether the file is compiled-in. Confirm with: `cargo-remote-ctl test -p gw-gateway --test e2e_tunnel -- --ignored --nocapture` returns either PASS or `connection refused` (Docker not up), **not** "test not found".

- [ ] **Step 5: Commit**

```bash
git add crates/gw-gateway/src/lib.rs crates/gw-gateway/src/health.rs \
        crates/gw-gateway/tests/e2e_tunnel.rs crates/gw-gateway/tests/health_endpoint.rs
git commit -m "refactor(gw-gateway): remove cfg(feature=\"tunnel\") gates — tunnel becomes default-compile"
```

---

### Task 0c: Remove `[features]` tables from `Cargo.toml`s

**Files:**
- Modify: `crates/gw-proxy/Cargo.toml`
- Modify: `crates/gw-gateway/Cargo.toml`

- [ ] **Step 1: Edit `crates/gw-proxy/Cargo.toml`**

Delete the entire `[features]` block:

```toml
[features]
default = []
tunnel = ["dep:gw-tunnel-client", "dep:tokio-rustls", "dep:webpki-roots"]
```

Remove `optional = true` from three dep lines:

```toml
gw-tunnel-client = { path = "../gw-tunnel-client", optional = true }
tokio-rustls = { workspace = true, optional = true }
webpki-roots = { workspace = true, optional = true }
```

becomes:

```toml
gw-tunnel-client = { path = "../gw-tunnel-client" }
tokio-rustls = { workspace = true }
webpki-roots = { workspace = true }
```

- [ ] **Step 2: Edit `crates/gw-gateway/Cargo.toml`**

Delete the entire `[features]` block:

```toml
[features]
default = []
tunnel = ["dep:gw-tunnel-client", "gw-proxy/tunnel"]
```

Remove `optional = true` from the `gw-tunnel-client` dep line:

```toml
gw-tunnel-client = { path = "../gw-tunnel-client", optional = true }
```

becomes:

```toml
gw-tunnel-client = { path = "../gw-tunnel-client" }
```

- [ ] **Step 3: Workspace-level compile**

Run: `cargo-remote-ctl check --workspace --all-targets`
Expected: clean.

- [ ] **Step 4: Workspace-level tests**

Run: `cargo-remote-ctl test --workspace`
Expected: PASS. (Note: no `--features tunnel` flag anymore; the feature simply does not exist.)

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/Cargo.toml crates/gw-gateway/Cargo.toml
git commit -m "build: drop tunnel feature flag from gw-proxy and gw-gateway"
```

---

### Task 0d: Update CI to drop the split tunnel image

**Files:**
- Modify: `.gitlab-ci.yml`

- [ ] **Step 1: Open `.gitlab-ci.yml` and delete two job blocks**

Delete the entire `rust-build-tunnel` job (search for `rust-build-tunnel:` and remove from that line through the next blank line / next top-level job key — currently lines ~40-50, but use grep to confirm against the live file).

Delete the entire `push-tunnel-image` job (search for `push-tunnel-image:` and remove from that line through the closing job block — currently lines ~70-90).

The remaining jobs (`lint-test`, `rust-build`, `push-image`) are unchanged and already publish the single image to `$GATEWAY_IMAGE:latest`.

- [ ] **Step 2: Validate the YAML**

Run: `python3 -c 'import yaml,sys; yaml.safe_load(open(".gitlab-ci.yml"))'`
Expected: no output (validation pass).

- [ ] **Step 3: grep for any stale `--features tunnel` references**

Run: `grep -n 'features tunnel\|features.*tunnel' .gitlab-ci.yml`
Expected: empty.

- [ ] **Step 4: Commit**

```bash
git add .gitlab-ci.yml
git commit -m "ci: drop --features tunnel build and push-tunnel-image jobs"
```

---

### Task 0e: Update agent docs, architecture, workflow, ROADMAP, control-plane contract

**Files:**
- Modify: `CLAUDE.md`
- Modify: `AGENTS.md`
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`
- Modify: `docs/ROADMAP.md`
- Modify: `docs/go-contracts/parts/08-control-plane.md`

- [ ] **Step 1: Patch `CLAUDE.md` and `AGENTS.md` in lockstep**

In the "Tunnel" bullet (line ~35 in both files), change:

> **Tunnel**: optional WebSocket transport to remote proxy nodes (`--features tunnel`). When `cfg.tunnel.enabled = true`, `pick_peer` consults `TunnelStack::resolve` per-request; otherwise the byte-identical Direct path runs.

to:

> **Tunnel**: WebSocket transport to remote proxy nodes (`cnexus` and, after Phase 2 of the 2026-06-15 tunnel-compat work, `aether` protocols). Tunnel code is default-compile as of 2026-06-15 (`docs/design/2026-06-15-tunnel-default-compile-decision.md` supersedes the prior split-artifact policy). When `cfg.tunnel.enabled = true`, `pick_peer` consults `TunnelStack::resolve` per-request; otherwise the byte-identical Direct path runs at runtime.

Keep `VendorKey.tunnel_preference.require_tunnel`, `GW_TUNNEL_NO_NODE`, boot-time-only restart caveat, and the references to `docs/spec/09-tunnel.md` and `[tunnel]` in `config/default.toml` — these are unchanged.

> **Mirror this edit exactly** in `AGENTS.md`. The `CLAUDE.md` § Sync rule and `AGENTS.md` ⇄ `CLAUDE.md` lockstep rule make any divergence a CI invariant failure on the next `bash scripts/check-invariants.sh` run.

- [ ] **Step 2: Patch `docs/architecture.md`**

Line 46 (the `gw-tunnel-client` row): change

> Consumed by `gw-proxy::tunnel_stack::TunnelStack` and `gw-proxy::tunnel_bridge` (under `feature = "tunnel"`).

to:

> Consumed by `gw-proxy::tunnel_stack::TunnelStack` and `gw-proxy::tunnel_bridge` (default-compile since 2026-06-15).

Line 76 (the dependency-DAG diagram): change `gw-tunnel-client ◄── gw-gateway (feature="tunnel")` to `gw-tunnel-client ◄── gw-gateway`.

- [ ] **Step 3: Patch `docs/workflow.md`**

Line 163: change

> If `feature = "tunnel"` is built AND `LlmGateway::tunnel = Some(_)`, `TunnelStack::resolve(...)`...

to:

> If `LlmGateway::tunnel = Some(_)` (set when `cfg.tunnel.enabled = true` at boot), `TunnelStack::resolve(...)`...

The rest of the paragraph is unchanged.

- [ ] **Step 4: Patch `docs/ROADMAP.md`**

Line 85 entry "F3 — Tunnel artifact": strike the line and append:

> Superseded 2026-06-15 by `docs/design/2026-06-15-tunnel-default-compile-decision.md`. Tunnel becomes default-compile; the `latest-tunnel` image is retired.

- [ ] **Step 5: Patch `docs/go-contracts/parts/08-control-plane.md`**

Find the JSON shape at line ~93 containing `"features": {"tunnel": false, ...}`.

Change the literal `false` to `true` and add an inline comment paragraph immediately after the JSON block:

> Since 2026-06-15, `features.tunnel` is always emitted as `true`. The field is retained in the contract for forward-compat with Go consumers that may inspect it; new consumers should treat it as a constant.

- [ ] **Step 6: Run invariant check**

Run: `bash scripts/check-invariants.sh`
Expected: clean. The CLAUDE.md ⇄ AGENTS.md lockstep is invariant-enforced; a divergence here would trip it.

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md AGENTS.md docs/architecture.md docs/workflow.md \
        docs/ROADMAP.md docs/go-contracts/parts/08-control-plane.md
git commit -m "docs: drop --features tunnel from agent/arch/workflow/roadmap/control-plane"
```

---

### Task 0f: Write the supersession ADR

**Files:**
- Create: `docs/design/2026-06-15-tunnel-default-compile-decision.md`

- [ ] **Step 1: Create the ADR**

Create the file with this body:

```markdown
# Tunnel default-compile decision (2026-06-15)

## Status

Accepted. Supersedes [`docs/design/2026-06-10-tunnel-artifact-release-decision.md`](./2026-06-10-tunnel-artifact-release-decision.md) (4 days prior).

## Context

The 2026-06-10 decision locked the policy: tunnel code stays out of `latest`
behind the `--features tunnel` flag, with a separate `latest-tunnel` image
built via a CI second-job. The stated reason was that tunnel was
"not yet production-validated."

Phase 1 of the Cnexus ↔ Aether tunnel compatibility work (this branch)
changes the underlying premise:

1. Tunnel is being **productionized** via the new `tests/tunnel-e2e/`
   Docker harness with three cnexus-protocol scenarios.
2. Tunnel becomes **core integration infrastructure** with the addition
   of the Aether wire protocol in Phase 2 — Aether-compatible proxy
   nodes are existing production deployments.
3. The `cfg.tunnel.enabled = false` **runtime** gate remains, so the
   compile-time gate's emergency-off value is fully duplicated by config.

Keeping `--features tunnel` after the Aether work lands would mean:
- Every tunnel code change requires a `cfg(feature="tunnel")` /
  `cfg(not(feature="tunnel"))` matching pair, increasing change cost.
- CI continues to build two artifacts for what is a single runtime config
  switch.
- Reviewers must mentally evaluate every diff against both feature
  states.

## Decision

Remove the `--features tunnel` Cargo feature. Tunnel code compiles
unconditionally into the single `latest` image. The runtime gate
`[tunnel].enabled = false` (default) remains the operator's kill switch;
flipping it requires restart per spec §1.2.

The `latest-tunnel` image is retired. CI publishes one image.

## Consequences

- **Binary size**: grows by the size of `gw-tunnel-client` + the now-
  unconditional `tokio-rustls` + `webpki-roots` deps. Estimated <1 MB
  per release — acceptable.
- **Compile time**: tunnel code is always compiled; a one-time
  ~5-10 s increase for `cargo build -p gw-gateway` cold builds.
- **`features.tunnel` in the control-plane contract** (per
  `docs/go-contracts/parts/08-control-plane.md`): retained in the JSON
  shape, value fixed at `true` for forward-compat with existing Go
  consumers.
- **Runtime-off behavior is unchanged**: `cfg.tunnel.enabled = false`
  produces a byte-identical Direct path; this was the entire point of
  the runtime gate and remains the supported kill switch.

## Reversal

To restore the split-artifact policy:

1. Re-introduce `[features] tunnel = [...]` in `gw-proxy` and `gw-gateway`
   Cargo.toml with `optional = true` on the gated deps.
2. Re-introduce `cfg(feature = "tunnel")` gates on the changed sites
   (git history at the commit of this ADR is the canonical reference).
3. Re-introduce the CI `rust-build-tunnel` + `push-tunnel-image` jobs.
4. Restore `features.tunnel = false` in the control-plane contract.

The cost of reversal is roughly equal to the cost of this forward step,
so reversal is feasible. The reason to do it would be: tunnel is found
to cause a hard regression that cannot be controlled via the runtime
gate. Document the failure mode here if that happens.

## References

- Spec: `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
- Plan: `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-1.md` Stage 0 (Tasks 0a-0f)
- Superseded: `docs/design/2026-06-10-tunnel-artifact-release-decision.md`
```

- [ ] **Step 2: Final Stage-0 gate — full workspace test + invariants**

Run: `cargo-remote-ctl test --workspace -- --include-ignored 2>&1 | tail -20`
Expected: PASS (or `connection refused` on `e2e_tunnel.rs` if Docker is not up — that is acceptable for this gate). Capture the summary line for the PR-1a description.

Run: `cargo-remote-ctl clippy --workspace --all-targets -- -D warnings`
Expected: clean.

Run: `bash scripts/check-invariants.sh`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add docs/design/2026-06-15-tunnel-default-compile-decision.md
git commit -m "docs(design): supersede 2026-06-10 split-artifact ADR with default-compile"
```

---

## PR 1a · Stage 1 — `TunnelProtocol`, migration, `cnexus/` reorg, `Transport` trait

### Task 1: Add `TunnelProtocol` enum to `gw-core`

**Files:**
- Create: `crates/gw-core/src/tunnel_protocol.rs`
- Modify: `crates/gw-core/src/lib.rs`
- Test: `crates/gw-core/src/tunnel_protocol.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append at the end of `crates/gw-core/src/tunnel_protocol.rs` (file will be created in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_cnexus() {
        assert_eq!(TunnelProtocol::default(), TunnelProtocol::Cnexus);
    }

    #[test]
    fn serde_round_trip_cnexus() {
        let json = serde_json::to_string(&TunnelProtocol::Cnexus).unwrap();
        assert_eq!(json, "\"cnexus\"");
        let parsed: TunnelProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, TunnelProtocol::Cnexus);
    }

    #[test]
    fn serde_round_trip_aether() {
        let json = serde_json::to_string(&TunnelProtocol::Aether).unwrap();
        assert_eq!(json, "\"aether\"");
        let parsed: TunnelProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, TunnelProtocol::Aether);
    }

    #[test]
    fn unknown_variant_is_rejected() {
        let r: Result<TunnelProtocol, _> = serde_json::from_str("\"unknown\"");
        assert!(r.is_err(), "unknown variants must not deserialize");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo-remote-ctl test -p gw-core --lib tunnel_protocol -- --nocapture`
Expected: FAIL — `tunnel_protocol` module does not exist (compile error).

- [ ] **Step 3: Write the minimal implementation**

Create `crates/gw-core/src/tunnel_protocol.rs`:

```rust
//! Per-`ProxyNode` discriminator selecting which wire protocol the tunnel
//! transport speaks. Distinct from [`crate::protocol::ProtocolKind`], which
//! discriminates **vendor** API families (OpenAi / Anthropic / Gemini / Kiro).
//!
//! Adding a variant here requires a corresponding wire-protocol implementation
//! in `gw-tunnel-client` (one submodule per variant). See
//! `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", deny_unknown_fields)]
pub enum TunnelProtocol {
    /// First-party Cnexus tunnel wire (14-byte BE header, TCP-relay semantics).
    Cnexus,
    /// Aether-compat wire (10-byte BE header, HTTP-relay semantics).
    /// The transport for this variant is added in Phase 2 of the spec.
    Aether,
}

impl Default for TunnelProtocol {
    fn default() -> Self {
        Self::Cnexus
    }
}

impl TunnelProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cnexus => "cnexus",
            Self::Aether => "aether",
        }
    }
}
```

Add to `crates/gw-core/src/lib.rs` in alphabetical order with the other `pub mod` lines (find the existing `pub mod protocol;` line — insert `pub mod tunnel_protocol;` right after `pub mod traits;`, keeping alphabetical order; adjust if there are no neighbours):

```rust
pub mod tunnel_protocol;
```

> Note: `deny_unknown_fields` on an enum at the container level rejects unknown variants on deserialize. This is the intended fail-closed behaviour: a future variant on disk must not be silently accepted as `default()`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo-remote-ctl test -p gw-core --lib tunnel_protocol -- --nocapture`
Expected: PASS — all 4 tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/tunnel_protocol.rs crates/gw-core/src/lib.rs
git commit -m "feat(gw-core): add TunnelProtocol enum (cnexus default, aether)"
```

---

### Task 2: Add `tunnel_protocol` field to `gw-core::entity::ProxyNode`

**Files:**
- Modify: `crates/gw-core/src/entity.rs`
- Test: `crates/gw-core/src/entity.rs` (existing `#[cfg(test)]` module if present; else inline-new one)

- [ ] **Step 1: Write the failing test**

Search `crates/gw-core/src/entity.rs` for `#[cfg(test)]`. If a tests module exists, append the two tests inside it; otherwise add the whole block at the end of the file.

```rust
#[cfg(test)]
mod tunnel_protocol_field_tests {
    use super::*;
    use crate::tunnel_protocol::TunnelProtocol;

    #[test]
    fn proxy_node_defaults_tunnel_protocol_to_cnexus_when_payload_omits_it() {
        // The on-disk payload pre-dating migration 0011 has no `tunnel_protocol`
        // key. Deserialization must default to Cnexus, never error.
        let raw = serde_json::json!({
            "id": "n1",
            "ws_url": "ws://example.invalid",
        });
        let n: ProxyNode = serde_json::from_value(raw).expect("must deserialize");
        assert_eq!(n.tunnel_protocol, TunnelProtocol::Cnexus);
    }

    #[test]
    fn proxy_node_round_trips_aether_protocol() {
        let raw = serde_json::json!({
            "id": "n2",
            "ws_url": "ws://example.invalid",
            "tunnel_protocol": "aether",
        });
        let n: ProxyNode = serde_json::from_value(raw).expect("must deserialize");
        assert_eq!(n.tunnel_protocol, TunnelProtocol::Aether);
        let back = serde_json::to_value(&n).unwrap();
        assert_eq!(back["tunnel_protocol"], "aether");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo-remote-ctl test -p gw-core --lib tunnel_protocol_field_tests`
Expected: FAIL — `ProxyNode` has no `tunnel_protocol` field (compile error).

- [ ] **Step 3: Implement the minimal field**

In `crates/gw-core/src/entity.rs`, locate the `pub struct ProxyNode { ... }` definition (around line 551 per current repo state). Add the new field. Insert **after** `auth_token_enc`:

```rust
    /// Wire protocol the tunnel transport speaks to this node. Defaults to
    /// `Cnexus` for forward-compatibility with payloads written before
    /// migration 0011.
    #[serde(default)]
    pub tunnel_protocol: crate::tunnel_protocol::TunnelProtocol,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo-remote-ctl test -p gw-core --lib tunnel_protocol_field_tests`
Expected: PASS — both tests green.

- [ ] **Step 5: Verify no other callers broke**

Run: `cargo-remote-ctl test -p gw-core`
Expected: PASS — all gw-core tests green, no other compile error.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-core/src/entity.rs
git commit -m "feat(gw-core): add tunnel_protocol field to ProxyNode entity"
```

---

### Task 3: Add migration `0011_proxy_nodes_protocol.sql`

**Files:**
- Create: `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql`

> No Rust test for the SQL itself; verification is via the tunnel-e2e schema applier (Task 17) and a manual sanity command below.

- [ ] **Step 1: Write the migration file**

Create `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql`:

```sql
-- 0011_proxy_nodes_protocol.sql — add `tunnel_protocol` discriminator to
-- proxy_nodes payload (cnexus default; aether variant lands in Phase 2 of the
-- tunnel-compat design at docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md).
--
-- Schema-shape is unchanged (entity table still (id, payload, version,
-- updated_at, deleted_at)); only the JSONB payload schema is extended.

-- 1. Backfill existing rows: any row whose payload lacks the new key gets it
--    set to "cnexus" so the partial index below is dense.
UPDATE proxy_nodes
SET payload = jsonb_set(payload, '{tunnel_protocol}', '"cnexus"', true)
WHERE payload->>'tunnel_protocol' IS NULL;

-- 2. Partial index for debugging / per-protocol stats. Skip soft-deleted rows.
CREATE INDEX IF NOT EXISTS proxy_nodes_tunnel_protocol_idx
  ON proxy_nodes ((payload->>'tunnel_protocol'))
  WHERE deleted_at IS NULL;
```

- [ ] **Step 2: Sanity-check syntax (host-side)**

If a local Postgres is up (see `docs/dev-setup.md`), run a dry parse:

Run: `psql -h localhost -U postgres -d gateway -f crates/gw-store/migrations/0011_proxy_nodes_protocol.sql`
Expected: `UPDATE 0` (or whatever existing row count) + `CREATE INDEX`. If no local Postgres is available, skip — Task 17 will exercise it via Docker.

- [ ] **Step 3: Update `CLAUDE.md` migrations list**

`CLAUDE.md` enumerates the migrations under "Toolchain & build pitfalls". Append `0011_proxy_nodes_protocol.sql` to the parenthesized list. Mirror the same change in `AGENTS.md` (per the lockstep rule).

> Locate the line beginning with "Migrations live under `crates/gw-store/migrations/`" and edit the parenthesised list. The full file lookup is a simple `grep -n "0010_vendor_key_quota_state.sql" CLAUDE.md AGENTS.md`.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-store/migrations/0011_proxy_nodes_protocol.sql CLAUDE.md AGENTS.md
git commit -m "feat(gw-store): add 0011_proxy_nodes_protocol migration"
```

---

### Task 4: Add `tunnel_protocol` to `ProxyNodeView` in `gw-tunnel-client::resolver`

**Files:**
- Modify: `crates/gw-tunnel-client/src/resolver.rs`
- Modify: `crates/gw-tunnel-client/tests/resolver.rs`

- [ ] **Step 1: Write the failing test**

Open `crates/gw-tunnel-client/tests/resolver.rs`. Find any helper that constructs a `ProxyNodeView` literal. Add a new test:

```rust
#[test]
fn proxy_node_view_carries_tunnel_protocol() {
    use gw_core::tunnel_protocol::TunnelProtocol;
    use gw_tunnel_client::resolver::ProxyNodeView;

    let v = ProxyNodeView {
        id: "n".into(),
        ws_url: "ws://h:1".parse().unwrap(),
        auth_token: None,
        enabled: true,
        latency_ms: 10,
        allowed_provider_ids: vec!["p".into()],
        allowed_vendor_key_ids: vec![],
        tunnel_protocol: TunnelProtocol::Aether,
    };
    assert_eq!(v.tunnel_protocol, TunnelProtocol::Aether);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test resolver -- proxy_node_view_carries_tunnel_protocol`
Expected: FAIL — `ProxyNodeView` has no `tunnel_protocol` field.

- [ ] **Step 3: Implement the field**

In `crates/gw-tunnel-client/src/resolver.rs`, add to `pub struct ProxyNodeView` after the `allowed_vendor_key_ids` line:

```rust
    /// Wire protocol this node speaks. Phase 1 always populates `Cnexus`;
    /// Phase 2 starts honoring `Aether`.
    pub tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol,
```

- [ ] **Step 4: Fix every other constructor of `ProxyNodeView` in this crate**

The crate's own tests and any internal callers construct `ProxyNodeView` as a struct literal. Run:

Run: `grep -rn "ProxyNodeView \{" crates/gw-tunnel-client`
Expected: a handful of sites. For each, add `tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol::Cnexus,` to the literal.

- [ ] **Step 5: Run the resolver test suite**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test resolver`
Expected: PASS, including the new test.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-tunnel-client/src/resolver.rs crates/gw-tunnel-client/tests/resolver.rs
git commit -m "feat(gw-tunnel-client): add tunnel_protocol to ProxyNodeView"
```

---

### Task 5: Wire `tunnel_protocol` through `tunnel_refresher` in `gw-gateway`

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs` (only the `tunnel_refresher` / `ProxyNodeView` construction site)

- [ ] **Step 1: Locate the construction site**

Run: `grep -n "ProxyNodeView" crates/gw-gateway/src/lib.rs`
Expected: one or more `ProxyNodeView { id: n.id ... }` literals fed by `EntityStore::proxy_nodes_list_or_load`.

- [ ] **Step 2: Add the field to each literal**

For each occurrence, propagate the value from the loaded `ProxyNode` payload:

```rust
ProxyNodeView {
    // ... existing fields ...
    tunnel_protocol: n.tunnel_protocol,
}
```

> `ProxyNode::tunnel_protocol` was added in Task 2; payloads pre-dating migration 0011 fall back to `TunnelProtocol::Cnexus` automatically.

- [ ] **Step 3: Compile-check the workspace**

Run: `cargo-remote-ctl build --workspace`
Expected: clean build, no warnings about the new field.

- [ ] **Step 4: Run the gateway crate's tests**

Run: `cargo-remote-ctl test -p gw-gateway`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-gateway): propagate tunnel_protocol through tunnel_refresher"
```

---

### Task 6: Reorganize `gw-tunnel-client/src/` into `cnexus/` submodule

**Files:**
- Move: `crates/gw-tunnel-client/src/frame.rs` → `crates/gw-tunnel-client/src/cnexus/frame.rs`
- Move: `crates/gw-tunnel-client/src/pool.rs` → `crates/gw-tunnel-client/src/cnexus/pool.rs`
- Move: `crates/gw-tunnel-client/src/transport.rs` → `crates/gw-tunnel-client/src/cnexus/transport.rs`
- Create: `crates/gw-tunnel-client/src/cnexus/mod.rs`
- Modify: `crates/gw-tunnel-client/src/lib.rs`

- [ ] **Step 1: Move the files with `git mv` (preserves history)**

Run:
```bash
mkdir -p crates/gw-tunnel-client/src/cnexus
git mv crates/gw-tunnel-client/src/frame.rs     crates/gw-tunnel-client/src/cnexus/frame.rs
git mv crates/gw-tunnel-client/src/pool.rs      crates/gw-tunnel-client/src/cnexus/pool.rs
git mv crates/gw-tunnel-client/src/transport.rs crates/gw-tunnel-client/src/cnexus/transport.rs
```

- [ ] **Step 2: Create the submodule entry point**

Create `crates/gw-tunnel-client/src/cnexus/mod.rs`:

```rust
//! Cnexus wire-protocol transport: outbound WS to a remote `gw-tunnel-server`.
//! 14-byte BE header, TCP-relay semantics. See
//! `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` §2.3.
//!
//! Phase 2 of that spec adds a sibling `aether::` submodule for the Aether
//! wire-protocol; the two are mutually exclusive per `ProxyNode.tunnel_protocol`.

pub mod frame;
pub mod pool;
pub mod transport;
```

- [ ] **Step 3: Fix internal cross-module paths**

The moved files reference each other via `crate::frame::...`, `crate::pool::...`, `crate::transport::...`. They must now use `crate::cnexus::frame::...` etc.

Run: `grep -rn "crate::frame::\|crate::pool::\|crate::transport::" crates/gw-tunnel-client/src/cnexus`
Expected: a handful of hits.

For each, rewrite the path. E.g. inside `crates/gw-tunnel-client/src/cnexus/pool.rs`:
- `use crate::transport::WsTransport;` → `use crate::cnexus::transport::WsTransport;`

Same sweep for `cnexus/transport.rs` and `cnexus/frame.rs`.

- [ ] **Step 4: Rewrite `crates/gw-tunnel-client/src/lib.rs`**

Open the file. Replace the existing module declarations:

```rust
//! Tunnel client — connects gateway to remote proxy nodes.
//! Phase 1 ships only the cnexus wire-protocol submodule.

pub mod cnexus;
pub mod resolver;
pub mod safe_dns;
pub mod target_filter;

pub use resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};
```

> **No back-compat re-exports.** All external call sites are migrated in Step 5 within the same commit so Phase 1 ends in a clean state. The single-branch / single-MR model means there is no external consumer of the old import paths to support.

- [ ] **Step 5: Migrate external call sites to `cnexus::` paths**

Seven import lines across four crates reference the old top-level modules. Update each:

| File | Old import | New import |
|---|---|---|
| `crates/gw-proxy/src/tunnel_bridge.rs:4` | `use gw_tunnel_client::transport::{TunnelStreamReader, TunnelStreamWriter, WsTransport};` | `use gw_tunnel_client::cnexus::transport::{TunnelStreamReader, TunnelStreamWriter, WsTransport};` |
| `crates/gw-gateway/src/lib.rs:1219` | `use gw_tunnel_client::pool::PoolConfig;` | `use gw_tunnel_client::cnexus::pool::PoolConfig;` |
| `crates/gw-gateway/src/lib.rs:1223` | `use gw_tunnel_client::transport::TransportConfig;` | `use gw_tunnel_client::cnexus::transport::TransportConfig;` |
| `crates/gw-gateway/tests/e2e_tunnel.rs:10` | `use gw_tunnel_client::pool::PoolConfig;` | `use gw_tunnel_client::cnexus::pool::PoolConfig;` |
| `crates/gw-gateway/tests/e2e_tunnel.rs:14` | `use gw_tunnel_client::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};` | `use gw_tunnel_client::cnexus::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};` |
| `crates/gw-tunnel-server/src/lib.rs:5` | `use gw_tunnel_client::frame::{decode_frame, encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN};` | `use gw_tunnel_client::cnexus::frame::{decode_frame, encode_frame, Frame, MsgType, MAX_PAYLOAD_LEN};` |
| `crates/gw-tunnel-server/tests/protocol.rs:3` | `use gw_tunnel_client::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};` | `use gw_tunnel_client::cnexus::transport::{TransportConfig, TunnelError, TunnelEvent, WsTransport};` |

Run after each batch: `grep -rn 'gw_tunnel_client::\(frame\|pool\|transport\)::' crates/ 2>/dev/null` — expected empty after all 7 edits land.

- [ ] **Step 6: Move the integration tests so test names stay readable**

Run:
```bash
git mv crates/gw-tunnel-client/tests/frame.rs     crates/gw-tunnel-client/tests/cnexus_frame.rs
git mv crates/gw-tunnel-client/tests/pool.rs      crates/gw-tunnel-client/tests/cnexus_pool.rs
git mv crates/gw-tunnel-client/tests/transport.rs crates/gw-tunnel-client/tests/cnexus_transport.rs
```

These test files also import the moved modules. Grep them and update the imports:

Run: `grep -n 'use gw_tunnel_client::\(frame\|pool\|transport\)::' crates/gw-tunnel-client/tests/cnexus_*.rs`
Expected: a handful of hits. Replace each `gw_tunnel_client::<old>::` with `gw_tunnel_client::cnexus::<old>::`.

- [ ] **Step 7: Compile the crate**

Run: `cargo-remote-ctl build -p gw-tunnel-client`
Expected: clean build.

- [ ] **Step 8: Run every tunnel-client test**

Run: `cargo-remote-ctl test -p gw-tunnel-client`
Expected: PASS, identical test names to before the move except the three renames above.

- [ ] **Step 9: Workspace-level compile + tests**

Run: `cargo-remote-ctl test --workspace`
Expected: PASS. This is the gate that proves no downstream crate's compile broke after the call-site migration in Step 5. Capture the final 5 lines of output for the PR-1a description.

- [ ] **Step 10: Commit (one commit covering the move + migration)**

```bash
git add crates/gw-tunnel-client crates/gw-proxy/src/tunnel_bridge.rs \
        crates/gw-gateway/src/lib.rs crates/gw-gateway/tests/e2e_tunnel.rs \
        crates/gw-tunnel-server/src/lib.rs crates/gw-tunnel-server/tests/protocol.rs
git commit -m "refactor(tunnel-client): move frame/pool/transport into cnexus/ + migrate call sites"
```

---

### Task 6a: Add `core.rs` (`Transport` trait + `TransportError` + `StreamHandle`) and `target.rs` (`StreamTarget`)

**Why this task exists:** Phase 2 adds `aether::WsTransport` as a second impl. Without an abstraction now, Phase 2 either has to retrofit a trait through a wider blast radius (because cnexus types will already have proliferated into call sites), or has to duplicate the dispatch logic. Doing it in Phase 1 — with only one impl on hand — keeps the trait surface minimal and forces the cnexus impl to prove the shape before Aether arrives.

**Files:**
- Create: `crates/gw-tunnel-client/src/core.rs`
- Create: `crates/gw-tunnel-client/src/target.rs`
- Modify: `crates/gw-tunnel-client/src/lib.rs`
- Test: `crates/gw-tunnel-client/src/core.rs` (inline `#[cfg(test)]` for `TransportError` Display + StreamHandle compile-fence)

- [ ] **Step 1: Write the failing test (in `core.rs`)**

Append a `#[cfg(test)] mod tests` at the end of the file that doesn't yet exist:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_display_includes_variant_tag() {
        let err = TransportError::Closed;
        assert!(format!("{err}").contains("closed"));
        let err = TransportError::UnsupportedTarget;
        assert!(format!("{err}").contains("unsupported"));
    }

    /// Compile-fence: any `tokio::io::AsyncRead + AsyncWrite + Send + Unpin`
    /// must be usable as a `StreamHandle`. If this stops compiling, the
    /// `StreamHandle` blanket impl has regressed.
    fn _assert_stream_handle_accepts_tokio_io<T>()
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        fn assert_box<S: TunnelStream + ?Sized>(_: Box<S>) {}
        let _: fn(Box<T>) = |b| assert_box(b);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test-of-mod core 2>&1 | tail -20`
Expected: FAIL with `error[E0432]: unresolved import` for `crate::core`. (The file doesn't exist yet; lib.rs has no `pub mod core;`.)

- [ ] **Step 3: Create `crates/gw-tunnel-client/src/target.rs`**

```rust
//! Protocol-neutral target descriptor for `Transport::open_stream`.
//!
//! Phase 1 ships only the `Tcp` variant (Cnexus is a TCP relay; `tunnel_bridge`
//! has already serialized HTTP into bytes before calling `open_stream`).
//!
//! Phase 2 of the design at
//! `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` §3.3
//! adds the `Http { method, url, headers, body_rx, body_tx_response, meta_tx_response }`
//! variant for Aether. Phase 1 leaves the enum non-exhaustive so adding the
//! variant in Phase 2 is forward-compatible (existing exhaustive matches over
//! `StreamTarget` are a compile error, which is the intended migration signal).

#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum StreamTarget {
    /// Cnexus: opaque TCP stream identified by host + port.
    Tcp { host: String, port: u16, tls: bool },
}
```

- [ ] **Step 4: Create `crates/gw-tunnel-client/src/core.rs`**

```rust
//! Protocol-neutral transport abstraction.
//!
//! Phase 1 has one impl (`cnexus::WsTransport`, wrapped by `cnexus::pool::ReconnectingTransport`).
//! Phase 2 adds `aether::WsTransport` behind the same trait. Call sites should
//! depend on `Arc<dyn Transport>` from then on; Phase 1 keeps the concrete cnexus
//! path direct because dynamic dispatch is unobservable with a single impl.
//!
//! See `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` §3.4.

use std::time::Duration;

use futures::future::BoxFuture;
use gw_core::tunnel_protocol::TunnelProtocol;

use crate::target::StreamTarget;

/// Bidirectional stream produced by `Transport::open_stream`.
/// Phase 1: `cnexus::transport::TunnelStream` satisfies this via its existing
/// `AsyncRead + AsyncWrite` impls.
pub trait TunnelStream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}

impl<T> TunnelStream for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + ?Sized
{
}

pub type StreamHandle = Box<dyn TunnelStream>;

/// Errors surfaced through the protocol-neutral `Transport` surface.
/// cnexus and (in Phase 2) aether impls convert their internal error types to this.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed")]
    Closed,
    #[error("unsupported target variant for this protocol")]
    UnsupportedTarget,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
}

/// Common surface for any tunnel transport. Implementations must be cheap to
/// `Arc`-share across `tokio` tasks (the gateway's `tunnel_stack` holds them
/// behind `DashMap<NodeId, Arc<dyn Transport>>`).
pub trait Transport: Send + Sync {
    fn open_stream<'a>(
        &'a self,
        target: StreamTarget,
    ) -> BoxFuture<'a, Result<StreamHandle, TransportError>>;

    fn alive(&self) -> bool;

    /// Time since the most recent liveness signal: Pong (Cnexus) or HeartbeatData (Aether).
    /// `None` if no signal has been observed yet (just-attached transport).
    fn last_heartbeat_age(&self) -> Option<Duration>;

    fn tunnel_protocol(&self) -> TunnelProtocol;

    fn close<'a>(&'a self) -> BoxFuture<'a, Result<(), TransportError>>;
}
```

- [ ] **Step 5: Rewrite `crates/gw-tunnel-client/src/lib.rs`**

Replace the existing module declarations with:

```rust
//! Tunnel client — connects gateway to remote proxy nodes.
//!
//! Phase 1 ships only the cnexus wire-protocol submodule and the
//! protocol-neutral abstraction (`core::Transport`, `target::StreamTarget`).
//! Phase 2 of `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
//! adds an `aether/` sibling submodule that implements the same trait surface.

pub mod cnexus;
pub mod core;
pub mod resolver;
pub mod safe_dns;
pub mod target;
pub mod target_filter;

// Forward exports of the protocol-neutral surface.
pub use core::{StreamHandle, Transport, TransportError, TunnelStream};
pub use target::StreamTarget;

pub use resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};
```

> **No back-compat re-exports.** The cnexus reorg in Task 6 Step 5 already migrated all external call sites to `gw_tunnel_client::cnexus::*` paths. This `lib.rs` does not re-export the cnexus submodules at the crate root.

- [ ] **Step 6: Run the failing test (now compiles)**

Run: `cargo-remote-ctl test -p gw-tunnel-client core::tests 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Verify nothing else broke**

Run: `cargo-remote-ctl test -p gw-tunnel-client`
Expected: PASS for all existing tests. The new trait is *defined* but not yet *implemented* by `WsTransport`/`ReconnectingTransport` (that's Task 6b), so no existing test depends on the impl.

- [ ] **Step 8: Commit**

```bash
git add crates/gw-tunnel-client/src/core.rs \
        crates/gw-tunnel-client/src/target.rs \
        crates/gw-tunnel-client/src/lib.rs
git commit -m "feat(gw-tunnel-client): add Transport trait + StreamTarget in core.rs/target.rs"
```

---

### Task 6b: Implement `Transport` for `cnexus::WsTransport` and `ReconnectingTransport`

**Files:**
- Modify: `crates/gw-tunnel-client/src/cnexus/transport.rs`
- Modify: `crates/gw-tunnel-client/src/cnexus/pool.rs`
- Test: `crates/gw-tunnel-client/tests/cnexus_transport.rs` (extend existing)

- [ ] **Step 1: Write the failing test (extend `cnexus_transport.rs`)**

Append at the end of `crates/gw-tunnel-client/tests/cnexus_transport.rs`:

```rust
/// Prove `cnexus::WsTransport` satisfies the protocol-neutral `Transport` trait.
/// This is a compile-time check: if the trait surface drifts away from the cnexus
/// impl, this test stops building.
#[test]
fn cnexus_ws_transport_is_a_transport() {
    fn assert_transport<T: gw_tunnel_client::Transport>() {}
    assert_transport::<gw_tunnel_client::cnexus::transport::WsTransport>();
}

#[test]
fn cnexus_ws_transport_reports_protocol() {
    use gw_core::tunnel_protocol::TunnelProtocol;
    use gw_tunnel_client::cnexus::transport::WsTransport;
    use gw_tunnel_client::Transport;
    // Construction helper used elsewhere in this file:
    let t = WsTransport::disconnected_for_test();
    assert_eq!(t.tunnel_protocol(), TunnelProtocol::Cnexus);
}

#[test]
fn reconnecting_transport_is_a_transport() {
    fn assert_transport<T: gw_tunnel_client::Transport>() {}
    assert_transport::<gw_tunnel_client::cnexus::pool::ReconnectingTransport>();
}
```

> If `disconnected_for_test()` doesn't already exist on `WsTransport`, add it as a `#[cfg(test)]` constructor returning a transport whose `alive()` is `false`. Its sole purpose is to exercise the trait methods that don't need a live socket.

- [ ] **Step 2: Run the failing test**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test cnexus_transport cnexus_ws_transport_is_a_transport 2>&1 | tail -20`
Expected: FAIL with `the trait bound 'gw_tunnel_client::cnexus::transport::WsTransport: gw_tunnel_client::Transport' is not satisfied`.

- [ ] **Step 3: Implement `Transport` for `cnexus::WsTransport`**

Add to `crates/gw-tunnel-client/src/cnexus/transport.rs`:

```rust
use std::time::Duration;
use futures::FutureExt;
use futures::future::BoxFuture;
use gw_core::tunnel_protocol::TunnelProtocol;
use crate::core::{StreamHandle, Transport, TransportError};
use crate::target::StreamTarget;

impl From<TunnelError> for TransportError {
    fn from(e: TunnelError) -> Self {
        match e {
            TunnelError::Closed => TransportError::Closed,
            TunnelError::Io(io) => TransportError::Io(io),
            other => TransportError::Protocol(other.to_string()),
        }
    }
}

impl Transport for WsTransport {
    fn open_stream<'a>(
        &'a self,
        target: StreamTarget,
    ) -> BoxFuture<'a, Result<StreamHandle, TransportError>> {
        async move {
            let target_str = match target {
                StreamTarget::Tcp { host, port, tls: false } => format!("{host}:{port}"),
                StreamTarget::Tcp { host, port, tls: true } => format!("tls://{host}:{port}"),
                #[allow(unreachable_patterns)]
                _ => return Err(TransportError::UnsupportedTarget),
            };
            let stream = WsTransport::open_stream(self, &target_str).await?;
            Ok(Box::new(stream) as StreamHandle)
        }
        .boxed()
    }

    fn alive(&self) -> bool {
        self.is_alive()
    }

    fn last_heartbeat_age(&self) -> Option<Duration> {
        // Phase 1 cnexus only: the existing `last_pong_age` returns `Duration::MAX`
        // for "never seen". Convert that sentinel to `None`.
        let age = self.last_pong_age();
        if age == Duration::MAX { None } else { Some(age) }
    }

    fn tunnel_protocol(&self) -> TunnelProtocol {
        TunnelProtocol::Cnexus
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, Result<(), TransportError>> {
        async move {
            WsTransport::close(self).await.map_err(TransportError::from)
        }
        .boxed()
    }
}
```

> If `WsTransport::is_alive()` doesn't exist yet, add it in the same diff: `pub fn is_alive(&self) -> bool { /* mirror existing alive flag */ }`. Match the existing pattern in `transport.rs`; do NOT introduce new shared state.

- [ ] **Step 4: Implement `Transport` for `ReconnectingTransport`**

Add to `crates/gw-tunnel-client/src/cnexus/pool.rs`:

```rust
use std::time::Duration;
use futures::FutureExt;
use futures::future::BoxFuture;
use gw_core::tunnel_protocol::TunnelProtocol;
use crate::core::{StreamHandle, Transport, TransportError};
use crate::target::StreamTarget;

impl Transport for ReconnectingTransport {
    fn open_stream<'a>(
        &'a self,
        target: StreamTarget,
    ) -> BoxFuture<'a, Result<StreamHandle, TransportError>> {
        async move {
            let inner = self.current_inner()
                .ok_or(TransportError::Closed)?;
            inner.open_stream(target).await
        }
        .boxed()
    }

    fn alive(&self) -> bool {
        self.current_inner().map(|t| t.alive()).unwrap_or(false)
    }

    fn last_heartbeat_age(&self) -> Option<Duration> {
        self.current_inner().and_then(|t| t.last_heartbeat_age())
    }

    fn tunnel_protocol(&self) -> TunnelProtocol {
        TunnelProtocol::Cnexus
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, Result<(), TransportError>> {
        async move {
            self.shutdown().await;
            Ok(())
        }
        .boxed()
    }
}
```

> `current_inner()` is expected to exist on `ReconnectingTransport` (it gives the supervisor's currently-attached inner transport, if any). If it doesn't exist with exactly this signature, add a thin `pub fn current_inner(&self) -> Option<Arc<WsTransport>>` wrapper over whatever the supervisor already exposes — do not change the supervisor's underlying state model.

- [ ] **Step 5: Run the trait tests**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test cnexus_transport 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Full workspace test**

Run: `cargo-remote-ctl test --workspace`
Expected: PASS. Capture the final 5 lines of output for the PR description.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-tunnel-client/src/cnexus/transport.rs \
        crates/gw-tunnel-client/src/cnexus/pool.rs \
        crates/gw-tunnel-client/tests/cnexus_transport.rs
git commit -m "feat(gw-tunnel-client): impl Transport for cnexus::{WsTransport, ReconnectingTransport}"
```

---

### Task 7: Sync the architecture and tunnel spec docs

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/spec/09-tunnel.md`

> Per `CLAUDE.md` §Sync rule, crate-layout changes and tunnel subsystem changes require these docs to be updated in the same PR.

- [ ] **Step 1: Patch `docs/architecture.md`**

Search for the existing description of `gw-tunnel-client`. Append two sentences:

```
Phase 1 of the tunnel-compat work (2026-06-15) moves the Cnexus-protocol
codec, pool, and transport into the `cnexus/` submodule and introduces
the protocol-neutral `Transport` trait + `StreamTarget` enum in
`core.rs`/`target.rs` so Phase 2 can plug an `aether/` sibling under the
same surface. `resolver`, `safe_dns`, and `target_filter` remain at the
crate root because they are protocol-agnostic.
```

- [ ] **Step 2: Patch `docs/spec/09-tunnel.md`**

Append at the end of the document a new section:

```markdown
## Per-node protocol discriminator (Phase 1, 2026-06-15)

`ProxyNode.tunnel_protocol: TunnelProtocol` (default `Cnexus`) selects the
wire protocol the tunnel transport speaks to a given node. Migration
`0011_proxy_nodes_protocol.sql` backfills the payload key for rows
written before this addition. Phase 1 implements only the `Cnexus`
variant; Phase 2 of the design at
`docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
adds the Aether variant and the `InboundListener` route that goes with it.

## `Transport` trait (Phase 1, 2026-06-15)

`gw-tunnel-client::core::Transport` is the protocol-neutral surface the
gateway depends on. Methods: `open_stream(StreamTarget) → StreamHandle`,
`alive()`, `last_heartbeat_age()`, `tunnel_protocol()`, `close()`.
`StreamTarget` is `#[non_exhaustive]`; Phase 1 ships only the `Tcp`
variant. The cnexus impls (`WsTransport`, `ReconnectingTransport`)
return `TransportError::UnsupportedTarget` for any non-`Tcp` target.
```

- [ ] **Step 3: Commit**

```bash
git add docs/architecture.md docs/spec/09-tunnel.md
git commit -m "docs(tunnel): record cnexus/ reorg, Transport trait, tunnel_protocol field"
```

---

### Task 8: PR-1a verification gate

- [ ] **Step 1: Run the dep-only build to surface any forgotten consumer**

Run: `cargo-remote-ctl check --workspace --all-targets`
Expected: clean. After Stage 0 there is no `tunnel` Cargo feature; this check catches any forgotten consumer of the moved cnexus types.

- [ ] **Step 2: Run clippy at warning-as-error**

Run: `cargo-remote-ctl clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 3: Run fmt --check**

Run: `cargo-remote-ctl fmt --all -- --check`
Expected: clean.

- [ ] **Step 4: Run architectural invariants**

Run: `bash scripts/check-invariants.sh`
Expected: clean. The script enforces layering invariants documented in `docs/architecture.md`; adding a new `gw-core` enum + extending an entity should not trip any rule.

- [ ] **Step 5: Run existing `make e2e` to confirm no regression**

Run: `make e2e`
Expected: 25 scenarios PASS. Capture the final summary line for the PR description.

> If you cannot run Docker locally, document that explicitly in the PR description and request the reviewer run it. Do **not** mark this step skipped silently.

- [ ] **Step 6: Open PR 1a**

PR title: `feat(tunnel): Phase 1a — TunnelProtocol discriminator, migration 0011, cnexus/ reorg, Transport trait`

PR body should include:
- One-line summary of each commit (Tasks 1–7, i.e. enum, ProxyNode field, migration, ProxyNodeView field, refresher wire, cnexus reorg, `core.rs` trait, cnexus impl `Transport`, docs sync).
- The captured 5-line `cargo-remote-ctl test --workspace` tail from Task 6b Step 6.
- The captured `make e2e` summary from Step 5 above.
- Link to the spec: `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`.
- A note that PR 1b (Go management plane in `AILinkMax-Go`) must land before any Aether-shaped row is created in production.

---

## PR 1b · Go Management Plane (`AILinkMax-Go` repo)

> **Repo:** `AILinkMax-Go` (located at `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/`). All paths below are relative to that repo's root.
>
> **Landing order:** Per spec §3.6.8, PR 1b lands **first** in the AILinkMax-Go repo, before any production deploy of PR 1a. Reason: PR 1a default `tunnel_protocol = Cnexus` means PR 1a is forward-compatible with existing rows, and PR 1b only writes the new `protocol` column (default `cnexus`) — so the Rust read-side never sees an unknown value. There is **no Aether-shaped row creation** in Phase 1; that happens in Phase 2.
>
> **Branch:** Create `feature/tunnel-compat-phase-1-go` in `AILinkMax-Go`. All Phase 1 Go commits land on this branch; submit as a single MR upon PR-1b gate passing.

### Task G1.1: Add `Protocol` field to `proxynode` entity

**Files:**
- Modify: `modules/proxynode/business/entity.go`
- Test: `modules/proxynode/business/service_test.go` (extend)

- [ ] **Step 1: Write the failing test**

In `modules/proxynode/business/service_test.go` add:

```go
func TestProxyNode_DefaultProtocolIsCnexus(t *testing.T) {
    var n ProxyNode
    if n.Protocol != "" && n.Protocol != "cnexus" {
        t.Fatalf("expected zero-value Protocol to be empty or 'cnexus', got %q", n.Protocol)
    }
}

func TestProxyNode_ProtocolJSONRoundTrip(t *testing.T) {
    n := ProxyNode{Protocol: "aether"}
    b, err := json.Marshal(n)
    if err != nil { t.Fatal(err) }
    if !strings.Contains(string(b), `"protocol":"aether"`) {
        t.Fatalf("expected json to include protocol=aether, got %s", b)
    }
    var n2 ProxyNode
    if err := json.Unmarshal(b, &n2); err != nil { t.Fatal(err) }
    if n2.Protocol != "aether" { t.Fatalf("round-trip lost Protocol: %+v", n2) }
}
```

- [ ] **Step 2: Run failing test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/business -run TestProxyNode_DefaultProtocolIsCnexus -v`
Expected: COMPILE FAIL — `n.Protocol undefined`.

- [ ] **Step 3: Add the field to `entity.go`**

```go
// ProxyNode represents one proxy node row.
type ProxyNode struct {
    // ... existing fields ...

    // Protocol selects the wire protocol the tunnel transport speaks to
    // this node. "cnexus" (default) means outbound dial-out to the node;
    // "aether" means the node dials in via /api/internal/aether-tunnel.
    // See ailinkmax-gateway/docs/spec/09-tunnel.md.
    Protocol string `json:"protocol" gorm:"column:protocol;type:varchar(16);not null;default:cnexus" validate:"omitempty,oneof=cnexus aether"`
}
```

> If the entity is persisted via `payload JSONB` (as in the Rust side's `proxy_nodes` table) rather than as a top-level column, change the GORM tag to embed inside the JSONB payload column. Inspect the existing `entity.go` for the persistence model **before** editing — match what's already there. The wire JSON (`json:"protocol"`) tag is the same either way.

- [ ] **Step 4: Run test, verify pass**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/business -run 'TestProxyNode_(DefaultProtocol|ProtocolJSON)' -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/business/entity.go modules/proxynode/business/service_test.go
git commit -m "feat(proxynode): add Protocol field (cnexus/aether, default cnexus)"
```

---

### Task G1.2: Validate `Protocol` enum on create/update; default `cnexus`

**Files:**
- Modify: `modules/proxynode/business/service.go`
- Test: `modules/proxynode/business/service_test.go` (extend)

- [ ] **Step 1: Write the failing test**

```go
func TestService_RejectsUnknownProtocol(t *testing.T) {
    svc := newServiceForTest(t)
    _, err := svc.Create(context.Background(), CreateInput{
        NodeName: "n1",
        WsURL:    "wss://n1.example.com/tunnel",
        Protocol: "blockchain", // invalid
    })
    if err == nil { t.Fatal("expected error for unknown protocol") }
    if !errors.Is(err, ErrInvalidProtocol) {
        t.Fatalf("expected ErrInvalidProtocol, got %v", err)
    }
}

func TestService_DefaultsProtocolToCnexusOnCreate(t *testing.T) {
    svc := newServiceForTest(t)
    n, err := svc.Create(context.Background(), CreateInput{NodeName: "n2", WsURL: "wss://n2/tunnel"})
    if err != nil { t.Fatal(err) }
    if n.Protocol != "cnexus" {
        t.Fatalf("expected default cnexus, got %q", n.Protocol)
    }
}

func TestService_AllowsCnexusToAetherTransition(t *testing.T) {
    svc := newServiceForTest(t)
    n, _ := svc.Create(context.Background(), CreateInput{NodeName: "n3", WsURL: "wss://n3/tunnel"})
    updated, err := svc.Update(context.Background(), n.ID, UpdateInput{Protocol: ptrString("aether")})
    if err != nil { t.Fatalf("transition should succeed: %v", err) }
    if updated.Protocol != "aether" {
        t.Fatalf("expected aether after update, got %q", updated.Protocol)
    }
}
```

> `newServiceForTest`, `ptrString`, `CreateInput`, `UpdateInput`, `ErrInvalidProtocol` follow the conventions of the existing test file. Mirror the existing helper signatures exactly — do not reinvent them.

- [ ] **Step 2: Run failing test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/business -run TestService_RejectsUnknownProtocol -v`
Expected: FAIL.

- [ ] **Step 3: Implement validation + default**

In `modules/proxynode/business/service.go` add:

```go
var ErrInvalidProtocol = errors.New("proxynode: protocol must be 'cnexus' or 'aether'")

func validateProtocol(p string) error {
    switch p {
    case "", "cnexus", "aether":
        return nil
    default:
        return fmt.Errorf("%w: got %q", ErrInvalidProtocol, p)
    }
}
```

Then in `Service.Create`:

```go
func (s *Service) Create(ctx context.Context, in CreateInput) (*ProxyNode, error) {
    if err := validateProtocol(in.Protocol); err != nil {
        return nil, err
    }
    if in.Protocol == "" {
        in.Protocol = "cnexus"
    }
    // ... rest unchanged ...
}
```

In `Service.Update`:

```go
func (s *Service) Update(ctx context.Context, id string, in UpdateInput) (*ProxyNode, error) {
    if in.Protocol != nil {
        if err := validateProtocol(*in.Protocol); err != nil {
            return nil, err
        }
    }
    // ... apply update ...
    // Push events:entity so the Rust side refreshes its in-memory snapshot.
    s.eventsBus.PushEntityChange(ctx, "proxy_node", "upsert", id)
    return updated, nil
}
```

> The events-bus push on Update is mandatory because protocol transitions per spec §3.6.3 require the Rust side to drop / re-attach the transport. The existing Update path likely already pushes; if so, do not duplicate. Inspect the existing handler chain first.

- [ ] **Step 4: Run tests**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/business -run 'TestService_(Rejects|Defaults|Allows)' -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/business/service.go modules/proxynode/business/service_test.go
git commit -m "feat(proxynode): validate Protocol enum, default cnexus, push events:entity on update"
```

---

### Task G1.3: Surface `Protocol` in admin API routes

**Files:**
- Modify: `modules/proxynode/api/routes.go`
- Test: `modules/proxynode/api/handlers_test.go` (extend)

- [ ] **Step 1: Write the failing test**

```go
func TestRoutes_CreateAcceptsProtocol(t *testing.T) {
    r := newTestRouter(t)
    body := `{"node_name":"n1","ws_url":"wss://n1/tunnel","protocol":"aether"}`
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("POST", "/api/admin/proxy-nodes/manual", strings.NewReader(body))
    req.Header.Set("Authorization", "Bearer "+testAdminToken)
    r.ServeHTTP(rec, req)
    if rec.Code != 200 { t.Fatalf("expected 200, got %d (%s)", rec.Code, rec.Body.String()) }
    var resp map[string]any
    if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil { t.Fatal(err) }
    if resp["protocol"] != "aether" {
        t.Fatalf("expected protocol=aether in response, got %v", resp["protocol"])
    }
}

func TestRoutes_GetIncludesProtocol(t *testing.T) {
    r := newTestRouter(t)
    seedNode(t, r, "n2", "cnexus")
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("GET", "/api/admin/proxy-nodes", nil)
    req.Header.Set("Authorization", "Bearer "+testAdminToken)
    r.ServeHTTP(rec, req)
    if rec.Code != 200 { t.Fatalf("got %d: %s", rec.Code, rec.Body.String()) }
    if !strings.Contains(rec.Body.String(), `"protocol":"cnexus"`) {
        t.Fatalf("expected protocol field in list response, got %s", rec.Body.String())
    }
}
```

- [ ] **Step 2: Run failing test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/api -run TestRoutes_CreateAcceptsProtocol -v`
Expected: FAIL (handler ignores `protocol` field).

- [ ] **Step 3: Plumb `Protocol` into the create/update DTOs**

In `modules/proxynode/api/routes.go` (or whichever file defines the request DTO), add:

```go
type createProxyNodeRequest struct {
    NodeName string `json:"node_name" binding:"required"`
    WsURL    string `json:"ws_url" binding:"required"`
    Protocol string `json:"protocol"` // optional; defaults to "cnexus" in service layer
    // ... existing fields ...
}

type updateProxyNodeRequest struct {
    NodeName *string `json:"node_name,omitempty"`
    WsURL    *string `json:"ws_url,omitempty"`
    Protocol *string `json:"protocol,omitempty"`
    // ... existing fields ...
}
```

Map them through the existing handler to `business.CreateInput` / `business.UpdateInput`. The GET handler that lists nodes already serializes the entity — and Task G1.1 added the `json:"protocol"` tag — so list output picks up `protocol` automatically with no handler change required (verify with the test above).

- [ ] **Step 4: Run tests**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/api -run 'TestRoutes_(Create|Get)' -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/api/routes.go modules/proxynode/api/handlers_test.go
git commit -m "feat(proxynode-api): accept and return protocol on manual/patch/get"
```

---

### Task G2.1: Add `ae_*` Aether-token middleware

**Files:**
- Create: `modules/proxynode/api/aether_token_middleware.go`
- Test: `modules/proxynode/api/aether_token_middleware_test.go`

- [ ] **Step 1: Write the failing test**

Create `modules/proxynode/api/aether_token_middleware_test.go`:

```go
package api

import (
    "net/http"
    "net/http/httptest"
    "strings"
    "testing"
)

func TestAetherTokenMiddleware_Rejects_NonAePrefix(t *testing.T) {
    mw := AetherTokenMiddleware(stubAetherTokenStore{})
    h := mw(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) }))
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("POST", "/", nil)
    req.Header.Set("Authorization", "Bearer admin-foo-bar")
    h.ServeHTTP(rec, req)
    if rec.Code != http.StatusUnauthorized {
        t.Fatalf("expected 401 for non-ae_ token, got %d", rec.Code)
    }
}

func TestAetherTokenMiddleware_Rejects_MissingBearer(t *testing.T) {
    mw := AetherTokenMiddleware(stubAetherTokenStore{})
    h := mw(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { w.WriteHeader(200) }))
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("POST", "/", nil)
    h.ServeHTTP(rec, req)
    if rec.Code != http.StatusUnauthorized {
        t.Fatalf("expected 401 for missing Authorization, got %d", rec.Code)
    }
}

func TestAetherTokenMiddleware_Accepts_AePrefix_KnownToken(t *testing.T) {
    store := stubAetherTokenStore{validToken: "ae_known_token", nodeID: "aether-node-1"}
    mw := AetherTokenMiddleware(store)
    h := mw(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
        nid := r.Context().Value(CtxKeyAetherNodeID).(string)
        if nid != "aether-node-1" { t.Fatalf("wrong nodeID: %s", nid) }
        w.WriteHeader(200)
    }))
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("POST", "/", strings.NewReader("{}"))
    req.Header.Set("Authorization", "Bearer ae_known_token")
    h.ServeHTTP(rec, req)
    if rec.Code != 200 { t.Fatalf("expected 200, got %d", rec.Code) }
}

type stubAetherTokenStore struct {
    validToken string
    nodeID     string
}

func (s stubAetherTokenStore) ResolveAetherToken(_ string, candidate string) (string, bool) {
    if candidate == s.validToken { return s.nodeID, true }
    return "", false
}
```

- [ ] **Step 2: Run failing test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/api -run TestAetherTokenMiddleware -v`
Expected: COMPILE FAIL (`AetherTokenMiddleware` undefined).

- [ ] **Step 3: Implement the middleware**

Create `modules/proxynode/api/aether_token_middleware.go`:

```go
package api

import (
    "context"
    "net/http"
    "strings"
)

type ctxKey string

const CtxKeyAetherNodeID ctxKey = "proxynode.aether.node_id"

// AetherTokenStore knows how to resolve a raw `ae_*` token (presented by an
// aether-proxy at /register-time) into the `proxy_nodes.id` value to return
// in the response body. Phase 1 implementations may scan all
// protocol='aether' rows and decrypt `auth_token_enc`; the lookup happens
// once per registration, so cost is not a concern.
type AetherTokenStore interface {
    ResolveAetherToken(ctx string, candidate string) (nodeID string, ok bool)
}

// AetherTokenMiddleware accepts Bearer tokens prefixed with `ae_`, resolves
// them against the store, and sets `CtxKeyAetherNodeID` on the request
// context. Any token without the `ae_` prefix is rejected up-front so this
// middleware can never collide with the existing admin-token middleware.
func AetherTokenMiddleware(store AetherTokenStore) func(http.Handler) http.Handler {
    return func(next http.Handler) http.Handler {
        return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
            authz := r.Header.Get("Authorization")
            if !strings.HasPrefix(authz, "Bearer ae_") {
                http.Error(w, "unauthorized", http.StatusUnauthorized)
                return
            }
            token := strings.TrimPrefix(authz, "Bearer ")
            nodeID, ok := store.ResolveAetherToken(r.Context().Value("requestID").(string), token)
            if !ok {
                http.Error(w, "unauthorized", http.StatusUnauthorized)
                return
            }
            ctx := context.WithValue(r.Context(), CtxKeyAetherNodeID, nodeID)
            next.ServeHTTP(w, r.WithContext(ctx))
        })
    }
}
```

> If `requestID` retrieval differs in the existing codebase (e.g. gin context key), follow the existing convention — do not introduce a new request-id mechanism.

- [ ] **Step 4: Run tests**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/api -run TestAetherTokenMiddleware -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/api/aether_token_middleware.go \
        modules/proxynode/api/aether_token_middleware_test.go
git commit -m "feat(proxynode-api): add Aether bearer-token middleware (ae_* prefix)"
```

---

### Task G2.2: Implement `aether_register.go` upsert business logic

**Files:**
- Create: `modules/proxynode/business/aether_register.go`
- Create: `modules/proxynode/business/aether_register_test.go`

- [ ] **Step 1: Write the failing test**

Create `modules/proxynode/business/aether_register_test.go`:

```go
package business

import (
    "context"
    "os"
    "testing"
)

func TestAetherRegister_Upserts_KnownToken(t *testing.T) {
    svc := newServiceForTest(t)
    n, _ := svc.Create(context.Background(), CreateInput{
        NodeName: "aether-1", WsURL: "", Protocol: "aether", AuthToken: "ae_known",
    })
    in := RegisterInput{
        NodeName: "aether-1-updated",
        PublicIP: "203.0.113.7",
        Region:   "us-east-1",
        Hardware: map[string]any{"cpu_cores": 4},
        Version:  "1.4.2",
        Token:    "ae_known",
    }
    out, err := svc.AetherRegister(context.Background(), in)
    if err != nil { t.Fatal(err) }
    if out.NodeID != n.ID {
        t.Fatalf("expected upsert to existing id %q, got %q", n.ID, out.NodeID)
    }
    if out.HeartbeatIntervalSecs == 0 {
        t.Fatalf("expected heartbeat interval to be set, got 0")
    }
    refreshed, _ := svc.Get(context.Background(), n.ID)
    if refreshed.NodeName != "aether-1-updated" {
        t.Fatalf("expected NodeName updated, got %q", refreshed.NodeName)
    }
}

func TestAetherRegister_RejectsUnknownToken_WhenAutoProvisionOff(t *testing.T) {
    t.Setenv("AETHER_AUTO_PROVISION", "false")
    svc := newServiceForTest(t)
    _, err := svc.AetherRegister(context.Background(), RegisterInput{Token: "ae_unknown"})
    if err == nil { t.Fatal("expected error for unknown token + auto-provision off") }
    if !errors.Is(err, ErrAetherTokenUnknown) {
        t.Fatalf("expected ErrAetherTokenUnknown, got %v", err)
    }
}

func TestAetherRegister_AutoProvisions_WhenEnabled(t *testing.T) {
    t.Setenv("AETHER_AUTO_PROVISION", "true")
    svc := newServiceForTest(t)
    in := RegisterInput{
        NodeName: "auto-1", PublicIP: "203.0.113.8", Region: "eu-west-1",
        Hardware: map[string]any{"cpu_cores": 2}, Version: "1.4.2",
        Token: "ae_brand_new",
    }
    out, err := svc.AetherRegister(context.Background(), in)
    if err != nil { t.Fatal(err) }
    if out.NodeID == "" { t.Fatal("expected new node id") }
    created, _ := svc.Get(context.Background(), out.NodeID)
    if created.Protocol != "aether" {
        t.Fatalf("auto-provisioned node must have Protocol=aether, got %q", created.Protocol)
    }
}
```

- [ ] **Step 2: Run failing test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/business -run TestAetherRegister -v`
Expected: COMPILE FAIL.

- [ ] **Step 3: Implement `aether_register.go`**

Create `modules/proxynode/business/aether_register.go`:

```go
package business

import (
    "context"
    "errors"
    "fmt"
    "os"
)

var ErrAetherTokenUnknown = errors.New("aether register: token not recognized and auto-provision disabled")

type RegisterInput struct {
    NodeName            string                 `json:"node_name"`
    PublicIP            string                 `json:"public_ip"`
    Region              string                 `json:"region"`
    Hardware            map[string]any         `json:"hardware"`
    Version             string                 `json:"version"`
    TunnelCapabilities  map[string]any         `json:"tunnel_capabilities"`
    Token               string                 `json:"-"` // injected by middleware/handler
}

type RegisterOutput struct {
    NodeID                string         `json:"node_id"`
    HeartbeatIntervalSecs int            `json:"heartbeat_interval_secs"`
    Config                map[string]any `json:"config,omitempty"`
}

func (s *Service) AetherRegister(ctx context.Context, in RegisterInput) (*RegisterOutput, error) {
    if in.Token == "" {
        return nil, fmt.Errorf("aether register: missing token")
    }

    // 1. Scan protocol='aether' rows, decrypt auth_token_enc, find matching row.
    matched, err := s.repo.FindAetherByToken(ctx, in.Token)
    if err != nil { return nil, err }

    if matched == nil {
        if os.Getenv("AETHER_AUTO_PROVISION") != "true" {
            return nil, ErrAetherTokenUnknown
        }
        // 2. Auto-provision: insert a new row with protocol='aether'.
        created, err := s.Create(ctx, CreateInput{
            NodeName:  in.NodeName,
            PublicIP:  in.PublicIP,
            Region:    in.Region,
            Hardware:  in.Hardware,
            Version:   in.Version,
            Protocol:  "aether",
            AuthToken: in.Token,
        })
        if err != nil { return nil, err }
        s.eventsBus.PushEntityChange(ctx, "proxy_node", "upsert", created.ID)
        return &RegisterOutput{
            NodeID:                created.ID,
            HeartbeatIntervalSecs: 30,
        }, nil
    }

    // 3. Upsert mutable fields and return the row's id.
    matched.NodeName = in.NodeName
    matched.PublicIP = in.PublicIP
    matched.Region = in.Region
    matched.Hardware = in.Hardware
    matched.Version = in.Version
    if err := s.repo.Save(ctx, matched); err != nil { return nil, err }
    s.eventsBus.PushEntityChange(ctx, "proxy_node", "upsert", matched.ID)

    return &RegisterOutput{
        NodeID:                matched.ID,
        HeartbeatIntervalSecs: 30,
    }, nil
}
```

> `s.repo.FindAetherByToken`, `s.repo.Save`, and `CreateInput.AuthToken` are expected to exist on the existing repo/service shape. If they don't, add thin wrappers that call into the existing repository — do not redesign the repository contract. `auth_token_enc` decryption uses whatever KMS helper the existing entity already uses.

- [ ] **Step 4: Run tests**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/business -run TestAetherRegister -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/business/aether_register.go \
        modules/proxynode/business/aether_register_test.go
git commit -m "feat(proxynode): add Aether registration upsert + auto-provision logic"
```

---

### Task G2.3: Wire the HTTP handler and route

**Files:**
- Create: `modules/proxynode/api/aether_register_handler.go`
- Create: `modules/proxynode/api/aether_register_handler_test.go`
- Modify: `modules/proxynode/api/routes.go`

- [ ] **Step 1: Write the failing test**

Create `modules/proxynode/api/aether_register_handler_test.go`:

```go
package api

import (
    "bytes"
    "encoding/json"
    "net/http"
    "net/http/httptest"
    "strings"
    "testing"
)

func TestAetherRegister_HandlerSucceeds_WithKnownToken(t *testing.T) {
    r := newTestRouterWithAetherToken(t, "ae_known", "aether-node-1")
    body := `{"node_name":"a1","public_ip":"203.0.113.7","region":"us-east-1","hardware":{"cpu_cores":4},"version":"1.4.2","tunnel_capabilities":{"max_streams":64}}`
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("POST", "/api/admin/proxy-nodes/register", strings.NewReader(body))
    req.Header.Set("Authorization", "Bearer ae_known")
    req.Header.Set("Content-Type", "application/json")
    r.ServeHTTP(rec, req)
    if rec.Code != 200 { t.Fatalf("expected 200, got %d: %s", rec.Code, rec.Body.String()) }
    var resp map[string]any
    if err := json.Unmarshal(rec.Body.Bytes(), &resp); err != nil { t.Fatal(err) }
    if resp["node_id"] != "aether-node-1" {
        t.Fatalf("expected node_id=aether-node-1, got %v", resp["node_id"])
    }
}

func TestAetherRegister_HandlerRejects_BadPrefix(t *testing.T) {
    r := newTestRouterWithAetherToken(t, "ae_known", "aether-node-1")
    rec := httptest.NewRecorder()
    req := httptest.NewRequest("POST", "/api/admin/proxy-nodes/register", bytes.NewReader(nil))
    req.Header.Set("Authorization", "Bearer admin_token_x") // wrong prefix
    r.ServeHTTP(rec, req)
    if rec.Code != http.StatusUnauthorized {
        t.Fatalf("expected 401, got %d", rec.Code)
    }
}
```

- [ ] **Step 2: Run failing test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/api -run TestAetherRegister_Handler -v`
Expected: FAIL (route not mounted).

- [ ] **Step 3: Implement the handler**

Create `modules/proxynode/api/aether_register_handler.go`:

```go
package api

import (
    "encoding/json"
    "errors"
    "net/http"

    "ailinkmax-go/modules/proxynode/business"
)

func AetherRegisterHandler(svc *business.Service) http.HandlerFunc {
    return func(w http.ResponseWriter, r *http.Request) {
        var in business.RegisterInput
        if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
            http.Error(w, "bad request", http.StatusBadRequest)
            return
        }
        // The Aether-token middleware verified the prefix; pass the raw token through.
        authz := r.Header.Get("Authorization")
        in.Token = authz[len("Bearer "):]

        out, err := svc.AetherRegister(r.Context(), in)
        if err != nil {
            if errors.Is(err, business.ErrAetherTokenUnknown) {
                http.Error(w, "unauthorized", http.StatusUnauthorized)
                return
            }
            http.Error(w, "internal", http.StatusInternalServerError)
            return
        }
        w.Header().Set("Content-Type", "application/json")
        _ = json.NewEncoder(w).Encode(out)
    }
}
```

- [ ] **Step 4: Mount the route**

In `modules/proxynode/api/routes.go`, add a new route block under the aether-token middleware:

```go
aether := api.Group("/api/admin/proxy-nodes")
aether.Use(AetherTokenMiddleware(svc.AetherTokenStore()))
aether.POST("/register", gin.WrapF(AetherRegisterHandler(svc)))
```

> Replace `api.Group(...)` / `gin.WrapF` with whatever router idiom the existing routes.go uses (chi, mux, gin, etc.). The key invariants are: (a) the `/register` route is **separate** from the existing admin-token routes; (b) only the aether-token middleware is in the chain — admin-token middleware must NOT also apply (collision risk).

- [ ] **Step 5: Run tests**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/api -run TestAetherRegister_Handler -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/api/aether_register_handler.go \
        modules/proxynode/api/aether_register_handler_test.go \
        modules/proxynode/api/routes.go
git commit -m "feat(proxynode-api): mount POST /api/admin/proxy-nodes/register under ae_* middleware"
```

---

### Task G-gate: PR 1b verification

- [ ] **Step 1: Full Go module test**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go test ./modules/proxynode/... -race -v`
Expected: PASS. `-race` is required because the registration path writes through the repo while readers may be iterating.

- [ ] **Step 2: Go module-wide build**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go build ./...`
Expected: clean.

- [ ] **Step 3: `go vet` and `gofmt -d` check**

Run: `cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go && go vet ./modules/proxynode/... && gofmt -l modules/proxynode/`
Expected: empty output (no diagnostics, no formatting drift).

- [ ] **Step 4: Open the AILinkMax-Go MR**

MR title: `feat(proxynode): Phase 1b — protocol field + Aether registration endpoint (G1+G2)`

MR description should include:
- Forward-compatibility note: default `protocol = "cnexus"` for all existing rows, so the Rust gateway (still on pre-Phase-1a code) deserializes them as before.
- Test summary from Step 1.
- Cross-link to the Cnexus Rust spec: `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` §3.6.

---

## PR 1c · `tests/tunnel-e2e/` skeleton + 3 Cnexus-protocol scenarios

### Task 9: Add Aether submodule placeholder with `update = none`

**Files:**
- Create / modify: `.gitmodules`

- [ ] **Step 1: Register the submodule with no-op default update**

Append to `.gitmodules` at the repo root (create the file if it does not exist):

```ini
[submodule "tests/tunnel-e2e/aether-src"]
    path = tests/tunnel-e2e/aether-src
    url = https://gitlab.ailinkmax.com/ailinkmax/Aether
    update = none
```

> `update = none` ensures `git submodule update --init` is a no-op until Phase 2's `make tunnel-e2e-deps` opts in. Fresh clones do not block on credentials.

- [ ] **Step 2: Tell git about the submodule pointer**

Run:
```bash
mkdir -p tests/tunnel-e2e/aether-src
git submodule add --name aether-src --branch HEAD https://gitlab.ailinkmax.com/ailinkmax/Aether tests/tunnel-e2e/aether-src || true
git status --short
```

> If you do not have credentials to clone Aether right now, instead create an empty placeholder directory with a `.gitkeep` and a comment in `.gitmodules` explaining the lazy-init contract — Phase 2 will run the real `submodule add` from a credentialed environment.

Expected: `.gitmodules` is staged; `tests/tunnel-e2e/aether-src/` shows as a submodule pointer (or as an empty placeholder dir per the fallback).

- [ ] **Step 3: Commit**

```bash
git add .gitmodules tests/tunnel-e2e/aether-src
git commit -m "build(tunnel-e2e): register Aether submodule (update=none placeholder)"
```

---

### Task 10: Create the Docker Compose for Phase 1 (no Aether services)

**Files:**
- Create: `tests/tunnel-e2e/docker-compose.yml`

- [ ] **Step 1: Write the compose file**

Create `tests/tunnel-e2e/docker-compose.yml`:

```yaml
name: tunnel-e2e

networks:
  tunnel-net:
    driver: bridge

services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: e2e
      POSTGRES_DB: cnexus
    networks: [tunnel-net]
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 1s
      retries: 30

  redis:
    image: redis:7-alpine
    networks: [tunnel-net]
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 1s
      retries: 30

  fake-upstream:
    build:
      context: ../../
      dockerfile: Dockerfile.e2e-fake-upstream
    networks: [tunnel-net]
    expose: ["9000", "9443"]

  cnexus-tunnel-server:
    build:
      context: ../../
      dockerfile: Dockerfile
      args:
        BIN: gw-tunnel-server
    command: >
      gw-tunnel-server
      --bind 0.0.0.0:7000
      --auth-token e2e-cnexus-token
      --max-streams 64
      --allow-target fake-upstream:9000
      --allow-target fake-upstream:9443
    networks: [tunnel-net]
    expose: ["7000"]
    depends_on:
      fake-upstream:
        condition: service_started

  schema-applier:
    image: postgres:16
    networks: [tunnel-net]
    depends_on:
      postgres:
        condition: service_healthy
    volumes:
      - ../../crates/gw-store/migrations:/migrations:ro
      - ./seed:/seed:ro
    environment:
      PGPASSWORD: e2e
    entrypoint: ["/seed/01_schema.sh"]
    restart: "no"

  ali-gateway:
    build:
      context: ../../
      dockerfile: Dockerfile
    environment:
      GW_PGSQL__URL: postgres://postgres:e2e@postgres:5432/cnexus
      GW_REDIS__URL: redis://redis:6379/0
      GW_TUNNEL__ENABLED: "true"
      GW_ADMIN__TOKEN: e2e-admin
      RUST_LOG: info,gw_tunnel_client=debug,gw_proxy=debug
    networks: [tunnel-net]
    expose: ["8000", "9090"]
    depends_on:
      schema-applier:
        condition: service_completed_successfully
      redis:
        condition: service_healthy
      cnexus-tunnel-server:
        condition: service_started
      fake-upstream:
        condition: service_started

  pytest-runner:
    image: python:3.12-slim
    working_dir: /work
    volumes:
      - ./:/work
    environment:
      ALI_GATEWAY_URL: http://ali-gateway:8000
      ALI_ADMIN_TOKEN: e2e-admin
      FAKE_UPSTREAM_URL: http://fake-upstream:9000
      PG_DSN: postgres://postgres:e2e@postgres:5432/cnexus
      REDIS_URL: redis://redis:6379/0
    command: >
      bash -c "pip install -q -r requirements.txt &&
               pytest -q --tb=short --junit-xml=logs/junit.xml scenarios/"
    networks: [tunnel-net]
    depends_on:
      ali-gateway:
        condition: service_started
```

> The `schema-applier` one-shot container runs once at startup, applies migrations, then exits zero. `ali-gateway` waits for it via `service_completed_successfully`.

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/docker-compose.yml
git commit -m "build(tunnel-e2e): docker-compose with cnexus-only services"
```

---

### Task 11: Add the schema applier and provider seed

**Files:**
- Create: `tests/tunnel-e2e/seed/01_schema.sh`
- Create: `tests/tunnel-e2e/seed/02_provider_keys.sql`

- [ ] **Step 1: Write the schema-applier shell script**

Create `tests/tunnel-e2e/seed/01_schema.sh`:

```bash
#!/usr/bin/env bash
# Phase-1 tunnel-e2e schema applier. Applies every gw-store migration in
# lexical order, then the per-scenario provider seed. Designed to be run as a
# one-shot container with PG client binaries available (postgres:16 image).

set -euo pipefail

PGHOST="${PGHOST:-postgres}"
PGUSER="${PGUSER:-postgres}"
PGDATABASE="${PGDATABASE:-cnexus}"

echo "[schema-applier] waiting for postgres to accept connections..."
for i in $(seq 1 30); do
  if pg_isready -h "$PGHOST" -U "$PGUSER" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

echo "[schema-applier] applying migrations..."
for f in $(ls /migrations/*.sql | sort); do
  echo "  - $f"
  psql -h "$PGHOST" -U "$PGUSER" -d "$PGDATABASE" -v ON_ERROR_STOP=1 -f "$f"
done

echo "[schema-applier] applying scenario seed..."
psql -h "$PGHOST" -U "$PGUSER" -d "$PGDATABASE" -v ON_ERROR_STOP=1 -f /seed/02_provider_keys.sql

echo "[schema-applier] done."
```

- [ ] **Step 2: Make the script executable in git**

Run: `chmod +x tests/tunnel-e2e/seed/01_schema.sh`

- [ ] **Step 3: Write the minimal provider seed**

Create `tests/tunnel-e2e/seed/02_provider_keys.sql`:

```sql
-- Minimal vendor + vendor_key + api_key seed enabling a `fake` provider
-- pointing at the `fake-upstream` container. Per-scenario fixtures may
-- override or extend; this seed is just the floor needed for the gateway to
-- accept any traffic.

INSERT INTO vendors (id, payload, version, updated_at, deleted_at) VALUES
('fake', jsonb_build_object(
    'id', 'fake',
    'display_name', 'Fake upstream',
    'base_url', 'http://fake-upstream:9000',
    'enabled', true
), 1, now(), NULL)
ON CONFLICT (id) DO NOTHING;

INSERT INTO vendor_keys (id, payload, version, updated_at, deleted_at) VALUES
('fake-key', jsonb_build_object(
    'id', 'fake-key',
    'vendor_id', 'fake',
    'enabled', true,
    'allowed_provider_ids', jsonb_build_array('fake'),
    'tunnel_preference', jsonb_build_object('require_tunnel', false)
), 1, now(), NULL)
ON CONFLICT (id) DO NOTHING;
```

> The exact `payload` schema for `vendors` / `vendor_keys` may differ from the columns above — adjust to match the current production schema after consulting `crates/gw-core/src/entity.rs`. The intent is "the smallest seed that lets the gateway route a request to `fake-upstream`".

- [ ] **Step 4: Commit**

```bash
git add tests/tunnel-e2e/seed/
git commit -m "build(tunnel-e2e): schema applier + minimal provider seed"
```

---

### Task 12: Add Python deps and pytest config

**Files:**
- Create: `tests/tunnel-e2e/requirements.txt`
- Create: `tests/tunnel-e2e/pytest.ini`
- Create: `tests/tunnel-e2e/.gitignore`

- [ ] **Step 1: Write `requirements.txt`**

```
pytest==8.2.0
httpx==0.27.0
psycopg[binary]==3.1.18
redis==5.0.1
pyyaml==6.0.1
```

- [ ] **Step 2: Write `pytest.ini`**

```ini
[pytest]
testpaths = scenarios
python_files = test_*.py
python_classes = Test*
python_functions = test_*
addopts = -ra
filterwarnings =
    error::pytest.PytestUnraisableExceptionWarning
```

- [ ] **Step 3: Write `.gitignore`**

```
logs/
.pytest_cache/
__pycache__/
*.pyc
```

- [ ] **Step 4: Commit**

```bash
git add tests/tunnel-e2e/requirements.txt tests/tunnel-e2e/pytest.ini tests/tunnel-e2e/.gitignore
git commit -m "build(tunnel-e2e): python deps + pytest config"
```

---

### Task 13: Add `conftest.py` with fixtures

**Files:**
- Create: `tests/tunnel-e2e/conftest.py`

- [ ] **Step 1: Write the fixtures module**

Create `tests/tunnel-e2e/conftest.py`:

```python
"""Phase-1 tunnel-e2e fixtures.

Provides:
  - pg_conn:        a psycopg connection autocommit=True
  - redis_client:   redis-py client
  - gateway_client: httpx.Client pre-configured for ALI_GATEWAY_URL
  - seed_nodes:     wipes proxy_nodes, inserts the given nodes, and pokes the
                    events:entity stream so the gateway refreshes its cache.

No fixture mutates schema. Migrations are applied by the schema-applier
container before pytest-runner starts.
"""

from __future__ import annotations

import json
import os
import time
from typing import Callable, Iterable

import httpx
import psycopg
import pytest
import redis


def _env(name: str) -> str:
    v = os.environ.get(name)
    if not v:
        raise RuntimeError(f"required env var {name!r} not set")
    return v


@pytest.fixture(scope="session")
def pg_conn() -> Iterable[psycopg.Connection]:
    conn = psycopg.connect(_env("PG_DSN"), autocommit=True)
    try:
        yield conn
    finally:
        conn.close()


@pytest.fixture(scope="session")
def redis_client() -> Iterable[redis.Redis]:
    cli = redis.Redis.from_url(_env("REDIS_URL"))
    try:
        yield cli
    finally:
        cli.close()


@pytest.fixture
def gateway_client() -> Iterable[httpx.Client]:
    base = _env("ALI_GATEWAY_URL")
    with httpx.Client(base_url=base, timeout=10.0) as c:
        yield c


@pytest.fixture
def admin_client() -> Iterable[httpx.Client]:
    base = _env("ALI_GATEWAY_URL")
    token = _env("ALI_ADMIN_TOKEN")
    with httpx.Client(base_url=base, timeout=10.0,
                      headers={"authorization": f"Bearer {token}"}) as c:
        yield c


@pytest.fixture
def seed_nodes(pg_conn: psycopg.Connection,
               redis_client: redis.Redis) -> Callable[..., None]:
    def _seed(*nodes: dict) -> None:
        with pg_conn.cursor() as cur:
            cur.execute("DELETE FROM proxy_nodes")
            for n in nodes:
                cur.execute(
                    "INSERT INTO proxy_nodes (id, payload, version, deleted_at) "
                    "VALUES (%s, %s::jsonb, 1, NULL)",
                    (n["id"], json.dumps(n)),
                )
        redis_client.xadd("events:entity", {"ns": "proxy_node", "op": "reset"})
        # The refresher polls; give it a beat to pull the new snapshot.
        time.sleep(2)
    return _seed
```

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/conftest.py
git commit -m "build(tunnel-e2e): conftest fixtures (pg, redis, gateway client, seed_nodes)"
```

---

### Task 14: Add `support/http_helpers.py`

**Files:**
- Create: `tests/tunnel-e2e/support/__init__.py`
- Create: `tests/tunnel-e2e/support/http_helpers.py`

- [ ] **Step 1: Write the helpers**

Create `tests/tunnel-e2e/support/__init__.py` (empty file).

Create `tests/tunnel-e2e/support/http_helpers.py`:

```python
"""HTTP helpers shared by Phase-1 cnexus-protocol scenarios."""

from __future__ import annotations

from dataclasses import dataclass
import httpx


_API_KEY_HEADERS = {
    # Seed-provided key from 02_provider_keys.sql + per-scenario fixtures.
    "x-api-key": "tn_sk_client_phase1",
    "x-api-key-id": "tn_ak_phase1",
}


def echo_post(client: httpx.Client, *, path: str, body: bytes,
              require_tunnel: bool = False) -> httpx.Response:
    """POST an arbitrary body to the fake provider's echo endpoint via the
    gateway. Returns the raw httpx.Response so callers can compare bytes."""
    headers = dict(_API_KEY_HEADERS)
    if require_tunnel:
        headers["x-tunnel-required"] = "1"
    return client.post(path, content=body, headers=headers)


@dataclass(frozen=True)
class Pair:
    direct: httpx.Response
    tunnel: httpx.Response

    def bodies_match(self) -> bool:
        return self.direct.content == self.tunnel.content

    def status_match(self) -> bool:
        return self.direct.status_code == self.tunnel.status_code


def direct_vs_tunnel(client: httpx.Client, *, path: str, body: bytes) -> Pair:
    return Pair(
        direct=echo_post(client, path=path, body=body, require_tunnel=False),
        tunnel=echo_post(client, path=path, body=body, require_tunnel=True),
    )
```

> The `x-tunnel-required` header above is illustrative. The actual surface for forcing tunnel comes from `VendorKey.tunnel_preference.require_tunnel = true`. The seed in Task 11 sets `require_tunnel: false` on the shared key; scenarios that need to force tunneling will instead patch the vendor key to `require_tunnel: true` via the admin API. Update `echo_post` to **not** send `x-tunnel-required` and remove the `require_tunnel` parameter once you've confirmed against `crates/gw-gateway/src/lib.rs` how the gateway routes between tunnel and direct.

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/support/
git commit -m "build(tunnel-e2e): http_helpers for cnexus scenarios"
```

---

### Task 15: Write `test_cnexus_protocol.py` — 3 scenarios

**Files:**
- Create: `tests/tunnel-e2e/scenarios/__init__.py`
- Create: `tests/tunnel-e2e/scenarios/test_cnexus_protocol.py`

- [ ] **Step 1: Write the failing test file**

Create `tests/tunnel-e2e/scenarios/__init__.py` (empty file).

Create `tests/tunnel-e2e/scenarios/test_cnexus_protocol.py`:

```python
"""Phase-1 cnexus-protocol scenarios. These three tests are adapted from
tests/e2e/client/scenarios/test_tunnel.py, which has been skipped (previously
behind a `--features tunnel` build requirement; that flag is removed in
Stage 0 of this plan). They now run against a real `gw-tunnel-server`
container in the tunnel-e2e harness.

Spec reference:
docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md §6.5.A
"""

from __future__ import annotations

import httpx
import pytest

from support.http_helpers import direct_vs_tunnel, echo_post


def _cnexus_node(node_id: str, *, allowed_targets: list[str]) -> dict:
    return {
        "id": node_id,
        "ws_url": "ws://cnexus-tunnel-server:7000",
        "enabled": True,
        "latency_ms": 0,
        "allowed_provider_ids": ["fake"],
        "allowed_vendor_key_ids": [],
        "auth_token_enc": None,         # Phase 1 cnexus seed runs with no token
        "tunnel_protocol": "cnexus",
        "allowed_targets": allowed_targets,
    }


def test_tunnel_no_node_returns_hard_error(seed_nodes,
                                           gateway_client: httpx.Client):
    """If the node pool is empty AND the request requires tunnel, the gateway
    must answer 502 with `GW_TUNNEL_NO_NODE` rather than silently falling
    through to Direct."""
    seed_nodes()        # empty pool
    resp = echo_post(gateway_client, path="/v1/fake/echo", body=b"ping",
                     require_tunnel=True)
    assert resp.status_code == 502
    assert "GW_TUNNEL_NO_NODE" in resp.text


def test_tunnel_target_denied(seed_nodes, gateway_client: httpx.Client):
    """A node whose allow-list excludes the request target answers
    502 `GW_TUNNEL_TARGET_DENIED`."""
    seed_nodes(_cnexus_node("denied-node",
                            allowed_targets=["something-else:9000"]))
    resp = echo_post(gateway_client, path="/v1/fake/echo", body=b"ping",
                     require_tunnel=True)
    assert resp.status_code == 502
    assert "GW_TUNNEL_TARGET_DENIED" in resp.text


def test_tunnel_direct_vs_tunnel_byte_identity(seed_nodes,
                                               gateway_client: httpx.Client):
    """Same request via direct vs tunnel must produce byte-identical
    response bodies and status codes. This catches stream-handling or
    framing regressions."""
    seed_nodes(_cnexus_node("allowed-node",
                            allowed_targets=["fake-upstream:9000",
                                             "fake-upstream:9443"]))
    payload = b'{"echo":"hello-tunnel"}'
    pair = direct_vs_tunnel(gateway_client, path="/v1/fake/echo", body=payload)
    assert pair.status_match(), \
        f"status mismatch: direct={pair.direct.status_code} tunnel={pair.tunnel.status_code}"
    assert pair.bodies_match(), \
        f"body mismatch:\n  direct={pair.direct.content[:200]!r}\n  tunnel={pair.tunnel.content[:200]!r}"
```

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/scenarios/
git commit -m "test(tunnel-e2e): 3 cnexus-protocol scenarios"
```

---

### Task 16: Add `Makefile` targets

**Files:**
- Modify: `Makefile`

- [ ] **Step 1: Locate the existing `.PHONY` line**

The current header is:

```
.PHONY: e2e e2e-up e2e-down e2e-logs e2e-soak e2e-soak-smoke e2e-soak-kill
```

Replace with:

```
.PHONY: e2e e2e-up e2e-down e2e-logs e2e-soak e2e-soak-smoke e2e-soak-kill \
        tunnel-e2e tunnel-e2e-up tunnel-e2e-down tunnel-e2e-logs tunnel-e2e-deps
```

- [ ] **Step 2: Append the new targets at the end of `Makefile`**

```makefile
# ------------------------------------------------------------------
# Tunnel compatibility e2e suite (separate compose from `make e2e`).
# Phase 1 only exercises the cnexus wire protocol; Phase 2 adds
# Aether-protocol scenarios and the Aether submodule.
# ------------------------------------------------------------------

TUNNEL_E2E_COMPOSE := -f tests/tunnel-e2e/docker-compose.yml \
                     --project-directory tests/tunnel-e2e

tunnel-e2e-deps:
	@echo "Phase 1: the Aether submodule is registered with update=none."
	@echo "Phase 2 will run \`git submodule update --init tests/tunnel-e2e/aether-src\`."

tunnel-e2e: tunnel-e2e-down
	docker compose $(TUNNEL_E2E_COMPOSE) build
	docker compose $(TUNNEL_E2E_COMPOSE) run --rm pytest-runner
	$(MAKE) tunnel-e2e-down

tunnel-e2e-up:
	docker compose $(TUNNEL_E2E_COMPOSE) up -d --build

tunnel-e2e-down:
	docker compose $(TUNNEL_E2E_COMPOSE) down -v --remove-orphans

tunnel-e2e-logs:
	docker compose $(TUNNEL_E2E_COMPOSE) logs --tail=200 -f
```

- [ ] **Step 3: Commit**

```bash
git add Makefile
git commit -m "build(tunnel-e2e): Makefile targets (up/down/logs/deps/all)"
```

---

### Task 17: Locally run `make tunnel-e2e` end-to-end

- [ ] **Step 1: Sanity-build the Rust image once**

Run: `docker compose -f tests/tunnel-e2e/docker-compose.yml --project-directory tests/tunnel-e2e build`
Expected: clean build for `cnexus-tunnel-server` and `ali-gateway`. First run may take several minutes (cold Rust compile inside the image).

- [ ] **Step 2: Run the suite**

Run: `make tunnel-e2e`
Expected: pytest summary `3 passed`.

- [ ] **Step 3: If any case fails, follow the failure-mode index**

Quick triage:
- `connection refused` from `pytest-runner`: `docker compose -f tests/tunnel-e2e/docker-compose.yml logs ali-gateway` — likely a schema-applier mis-fire or migration ordering.
- `GW_TUNNEL_NO_NODE` when not expected: the `seed_nodes` fixture's `time.sleep(2)` may need to grow; the gateway's refresh cadence is config-driven (see `[tunnel].refresh_interval_ms` in `config/default.toml`). Bump the sleep before tightening other knobs.
- Byte mismatch: `docker compose ... logs fake-upstream` to confirm what the upstream actually received vs. what came back. A mismatch usually means chunked vs. content-length disagreement on the loopback path; capture both responses and diff with `xxd`.

> Do not adjust the gateway code or shipping config to make the tests pass — Phase 1 is supposed to be wire-identical to the prior behavior. If a real bug surfaces, file it and decide whether to fix in PR 1c or split off.

- [ ] **Step 4: Capture the green summary for the PR description**

Save the final 5 lines of `make tunnel-e2e` output.

- [ ] **Step 5: Verify the existing `make e2e` still passes**

Run: `make e2e`
Expected: 25 passed. Capture the summary line.

> This is the regression gate for everything PR 1a + 1b touch outside the new directory.

---

### Task 18: Write the `tests/tunnel-e2e/README.md`

**Files:**
- Create: `tests/tunnel-e2e/README.md`

- [ ] **Step 1: Write the README**

Create `tests/tunnel-e2e/README.md`:

```markdown
# tunnel-e2e (Phase 1)

Local-only Docker e2e suite for the tunnel subsystem. Not wired into CI by
design; run on demand with `make tunnel-e2e` from the repo root.

## Phase 1 scope

Three scenarios in `scenarios/test_cnexus_protocol.py` against a real
`gw-tunnel-server` container:

| Test | Asserts |
|---|---|
| `test_tunnel_no_node_returns_hard_error` | empty pool + `require_tunnel=true` → 502 `GW_TUNNEL_NO_NODE` |
| `test_tunnel_target_denied` | node allow-list mismatch → 502 `GW_TUNNEL_TARGET_DENIED` |
| `test_tunnel_direct_vs_tunnel_byte_identity` | direct vs tunnel response bodies are byte-identical |

Phase 2 of the design adds Aether scenarios; Phase 3 adds mixed-pool stress.

## How to run

```bash
make tunnel-e2e          # full lifecycle: up → pytest → down
make tunnel-e2e-up       # bring services up; leave them running for ad-hoc poking
make tunnel-e2e-down     # tear down + remove volumes
make tunnel-e2e-logs     # tail compose logs
```

First-run image build takes ~5 min cold; subsequent runs are seconds.

## Failure triage

| Symptom | First log to check |
|---|---|
| pytest "connection refused to ali-gateway" | `docker compose -f tests/tunnel-e2e/docker-compose.yml logs ali-gateway` |
| Test expects tunnel but sees direct | confirm `[tunnel].enabled=true` env (compose sets `GW_TUNNEL__ENABLED=true`) |
| Migration error during schema-applier | `docker compose ... logs schema-applier` — migrations apply in lexical order |
| Byte mismatch in `direct_vs_tunnel` | `docker compose ... logs fake-upstream` to see what upstream actually got |

## Submodule

The Aether submodule at `aether-src/` is registered with `update = none` so
fresh clones do not block on credentials. Phase 2 of the spec is the first
phase that consumes it; Phase 1 leaves it dormant.

## Spec

`docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
```

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/README.md
git commit -m "docs(tunnel-e2e): README with scenarios, run targets, triage"
```

---

### Task 19: PR-1c verification gate

- [ ] **Step 1: Confirm `make tunnel-e2e` passes from a clean tree**

Run:
```bash
make tunnel-e2e-down
docker volume prune -f
make tunnel-e2e
```
Expected: `3 passed`. This is the gate the reviewer will replay.

- [ ] **Step 2: Re-run `cargo-remote-ctl test --workspace`**

Expected: unchanged from PR 1a baseline. (`tests/tunnel-e2e/` is Python — it should not affect the cargo workspace.)

- [ ] **Step 3: Confirm `make e2e` still green**

Expected: 25 passed.

- [ ] **Step 4: Open PR 1c**

PR title: `feat(tunnel-e2e): Phase 1c — Docker harness + 3 cnexus scenarios`

PR body should include:
- The captured `make tunnel-e2e` green summary.
- The captured `make e2e` green summary.
- Note that the Aether submodule is registered with `update = none` per Phase 2 plan; Phase 1 does not exercise it.
- Link to the spec.

---

## Self-Review

After PR 1a + PR 1b + PR 1c are complete on their respective branches (PR 1b in `AILinkMax-Go` as a real MR, PR 1a + PR 1c as commit groups on the `ailinkmax-gateway` shared feature branch that Phase 2 and Phase 3 will extend before the single final MR to `main`):

1. **Spec coverage (Phase 1 only):**
   - **Pre-spec prep — `--features tunnel` removal** (decided 2026-06-15 per `docs/design/2026-06-15-tunnel-default-compile-decision.md`) — Tasks 0a-0f. Supersedes the 2026-06-10 split-artifact decision; tunnel becomes default-compile so Phase 2's Aether work doesn't need to introduce new feature-gated complexity.
   - §3.1 migration 0011 — Task 3.
   - §3.2 `TunnelProtocol` enum + `ProxyNodePayload.protocol` field — Tasks 1 & 2.
   - §3.3 `StreamTarget` enum (Tcp-only in Phase 1) — Task 6a.
   - §3.4 `Transport` trait + cnexus impls — Tasks 6a (trait definition) and 6b (impls). `AnyTransport` enum deferred to Phase 2 (Phase 1 uses `Arc<dyn Transport>` directly).
   - §3.6.1 Registration endpoint — Tasks G2.1 (middleware), G2.2 (business logic), G2.3 (handler/route).
   - §3.6.2 Admin API `protocol` field — Tasks G1.1 (entity), G1.2 (service validation + transition events), G1.3 (routes).
   - §3.6.3 Protocol transition state machine — Task G1.2 Step 3 (push `events:entity` on Update). Rust-side transport teardown/reattach driven by the existing entity-stream consumer; no new Rust code in Phase 1 since both impls of `Transport` exist only after Phase 2.
   - §3.6.4 Auto-provision flag — Task G2.2 Step 3 (`AETHER_AUTO_PROVISION` env, default off).
   - §6.1 directory layout (Phase 1 subset) — Tasks 9-15 cover compose, seed, fixtures, scenarios, README; the Aether-specific files are explicitly deferred to Phase 2.
   - §6.3 docker-compose (Phase 1 subset) — Task 10.
   - §6.4 seed_nodes fixture — Task 13.
   - §6.5.A 3 scenarios — Task 15.
   - §6.6 Makefile targets — Task 16.
   - §7 Phase 1 PR breakdown — PR 1a Tasks 1-8 (including 6a/6b), PR 1b Tasks G1.1-G2.3 + G-gate, PR 1c Tasks 9-19.
   - Phase 1 explicitly out of scope per the plan header: §3.3 `StreamTarget::Http` variant, §3.5 `InboundListener`, §4 Aether internals, §5 dep boundary, §6.5.B-D scenarios, §3.6.5 G3 heartbeat consumer (Phase 2), §3.6.6 GatewayControl semantics for Aether (Phase 2 — the dispatcher needs the `aether` protocol resolvable, which requires Phase 2's adapter), §3.6.7 G4 frontend UI (Phase 3).

2. **Placeholder scan:**
   - No "TBD"/"TODO" inside concrete code blocks.
   - Tasks 0a/0b contain "Read the surrounding ~30 lines to confirm it is the fallback Direct path" — concrete inspection step, not a placeholder; the plan tells the engineer exactly what to look for (`Direct::HttpPeer::new`) and what to do if the block contains unexpected logic (stop and report).
   - Tasks 6a/6b contain "If `WsTransport::is_alive()` doesn't exist yet" / "If `current_inner()` doesn't exist" — these are documented adjustment-points with explicit guidance on how to add the helper if absent, not unresolved unknowns.
   - Task G1.1 Step 3 notes "If the entity is persisted via `payload JSONB` ... change the GORM tag" — documented adjustment-point with the concrete file to inspect (`entity.go`) before editing.
   - Task G1.2 Step 3 notes "The existing Update path likely already pushes; if so, do not duplicate" — documented anti-duplication guard with a concrete check.
   - Task G2.1 Step 3 references `requestID` retrieval — documented as "follow the existing convention," explicit anti-reinvention guard.
   - Task G2.2 Step 3 notes `FindAetherByToken` / `Save` may need thin wrappers — explicit instruction not to redesign the repo contract.
   - Task G2.3 Step 4 says "Replace `api.Group(...)` / `gin.WrapF` with whatever router idiom..." — documented adjustment-point with the binding invariant (route separated from admin-token middleware) preserved across idiom choices.
   - Task 11 Step 3 notes "schema may differ — adjust to match production" — documented adjustment-point against the running schema. The plan tells the engineer exactly where to look (`crates/gw-core/src/entity.rs`) to confirm the field names.
   - Task 14 Step 1 notes the `require_tunnel` header is illustrative and points to the real surface (`VendorKey.tunnel_preference.require_tunnel`) — documented adjustment-point with a concrete file reference.

3. **Type consistency:**
   - `TunnelProtocol` (gw-core), `ProxyNode.tunnel_protocol` (gw-core entity), `ProxyNodeView.tunnel_protocol` (gw-tunnel-client resolver), payload key `"tunnel_protocol"` — all four match.
   - Values: `"cnexus"` (default) / `"aether"` consistent in SQL, Rust serde, Python seed, Go entity tag (`oneof=cnexus aether`).
   - Trait surface: Task 6a's `Transport`/`TransportError`/`StreamHandle`/`StreamTarget` names match Task 6b's impl signatures and Task 7's doc patch.
   - Go-side type names: `RegisterInput`/`RegisterOutput` (Task G2.2) match handler usage (Task G2.3); `ErrAetherTokenUnknown` is the same in both. `CtxKeyAetherNodeID` (Task G2.1) is the only context-key name; handler does not reach for it (it pulls token from the Authorization header instead). `AetherTokenStore.ResolveAetherToken` signature in Task G2.1 (`ctx string`, `candidate string`) matches the stub used in tests.
   - Service names in compose match the URLs the Python tests use: `cnexus-tunnel-server:7000`, `ali-gateway:8000`, `fake-upstream:9000`. Phase 1 compose does **not** include the AILinkMax-Go binary; the registration endpoint exists in Go (PR 1b) but Phase 1 e2e doesn't exercise it (no `aether-proxy` yet). Phase 2 compose extension adds it.

4. **Cross-repo coordination invariants:**
   - PR 1b lands first in `AILinkMax-Go` as a real MR. PR 1a default is `Cnexus`, so the Rust pre-Phase-1a binary deserializing a Go-written row with `protocol="cnexus"` is byte-identical to today's behavior. No forward-incompat window.
   - No Aether-shaped row is created in Phase 1 either by the e2e (PR 1c does not seed Aether nodes) or by ops (Phase 2 spec says Aether proxies are not deployed until after Phase 2 lands). So `InboundListener` being absent in Phase 1 cannot cause user-visible failure.

5. **Single-branch / single-MR consistency check:**
   - Stage 0 (`--features tunnel` removal) lands as commits 0a-0f on the shared branch. Phase 2 and Phase 3 build on top of those commits. **No later phase reverses Stage 0**: tunnel stays default-compile through Phase 3. The control-plane JSON `features.tunnel` field remains permanently set to `true` (Option A per 2026-06-15 decision).
   - Stage 1 (functional Phase 1) lands on top of Stage 0 — Tasks 6's external call-site migration is the **final state** for cnexus call paths (no back-compat re-exports; Phase 2 simply adds `aether::` paths alongside).
   - The Aether submodule `.gitmodules` line (Task 9) ships with `update = none` in Phase 1 and is flipped to `update = checkout` in Phase 2 Task 1 (or equivalent). This is a setup-then-activate sequence, not an add-then-remove; the Phase 1 state is intentionally incomplete and Phase 2's flip is part of the converging design.

---

Plan complete and saved to `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-1.md`. Two execution options:

**1. Subagent-Driven (recommended)** — A fresh subagent runs each task, I review between tasks. Fast iteration; protects this session's context for coordination across all three phases. Cross-repo subagents (PR 1b in `AILinkMax-Go`) are launched with explicit `cwd` set to that repo.

**2. Inline Execution** — Execute tasks in this session via the executing-plans skill, with checkpoints for review.

Which approach?
