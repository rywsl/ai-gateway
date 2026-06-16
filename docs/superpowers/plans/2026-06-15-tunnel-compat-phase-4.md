# Tunnel Compat Phase 4 Implementation Plan — Cnexus SOCKS5 Egress for Aether-parity

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **POST-AUDIT ERRATA (2026-06-15, applied after parallel-agent audit). 21 fixes applied; the most impactful:**
>
> 1. **Frontend stack is Vue 3 + Vite at `frontend/`**, NOT React + Vite at `web/`. All `web/src/` paths rewritten to `frontend/src/`. React/TSX code snippets remain as architectural placeholders; the implementing engineer translates to Vue 3 Composition API (`<script setup>`) following the existing pattern at `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend/src/views/admin/ProxyNodes.vue`.
> 2. **The `proxy_url` UI is ALREADY protocol-agnostic in the live codebase** — `frontend/src/views/admin/ProxyNodes.vue:473` renders it unconditionally for all protocols today. Phase 3 G4 was originally believed to introduce an aether-only conditional that Phase 4 would widen — that assumption was wrong; Phase 3 G4 tasks do not touch `proxy_url`. Phase 4 PR 4b reframed: it is a **coordination + regression guard**, NOT a "widen the conditional" task. See Task 4b.2 below.
> 3. **Go `proxy_url` scheme validation does NOT exist** — `service.go::CreateManual` / `UpdateManual` / `Register` perform zero scheme validation on `ProxyURL`; the only place `socks5`/`socks5h` strings appear in the Go code is `ops.go:38-46::defaultPort` for the reachability probe. The original draft's `TestService_RejectsUnknownProxyURLScheme` test was wrong by construction. Task 4b.1 has been redesigned to **add a small `validateProxyURL` helper** (whitelisting `socks5`, `socks5h`, `http`, `https`) and gate `Create`/`Update` on it, then assert both accept and reject cases.
> 4. **Go test struct is `ManualCreateRequest{Name, ProxyURL, Protocol, ...}`, NOT `CreateInput{NodeName, WsURL, Protocol, ProxyURL}`.** All Go test code in Task 4b.1 has been rewritten to use the real DTO shape (verified against `modules/proxynode/business/entity.go:193-199`).
> 5. **Compose service `cnexus-tunnel-server-socks5` builds from `Dockerfile.tunnel-e2e` (NOT root `Dockerfile`).** The original draft used `dockerfile: Dockerfile` + `args: BIN: gw-tunnel-server` which is the spec's idealized form; the live `Dockerfile.tunnel-e2e` already builds all e2e binaries and has no `BIN` arg. Task 4a.4 corrected.
> 6. **`--allow-target example.com:9000` (NOT `fake-upstream:9000`).** The gateway sends the network alias `example.com:9000` over the wire-protocol target string; `validate_target` is a string match, not IP resolution. The fake-upstream container is aliased to `example.com` in the existing compose (Phase 1).
> 7. **`gw-tunnel-server` `parse_egress_url` uses async `tokio::net::lookup_host` (NOT blocking `std::net::ToSocketAddrs::to_socket_addrs`)**, AND stores `EgressMode::Socks5 { proxy_host: String, proxy_port: u16, ... }` (resolved at dial-time, not parse-time) so a proxy-container restart with a new IP does not produce a stale cached address. Task 4a.1/4a.2/4a.3 rewritten accordingly.
> 8. **Reachability probe runs in `tokio::spawn` (NOT serially before `spawn(cfg)`)** so a slow/dead proxy does not delay listener bind. Task 4a.3 Step 4 corrected.
> 9. **URL userinfo is percent-decoded** before passing to SOCKS5 (`url::Url::username()`/`.password()` return percent-encoded ASCII; passwords like `p@ss` round-trip as `p%40ss` if not decoded). Task 4a.3 Step 2 implementation now uses `percent_encoding::percent_decode_str(...).decode_utf8_lossy()`.
> 10. **HEALTHCHECK in `Dockerfile.socks5-egress` does not use `nc`** (the upstream `serjs/go-socks5-proxy` is distroless and has no shell or coreutils). Replaced with a Docker Compose-level `healthcheck:` block using `test: ["CMD-SHELL", ...]` that requires a shell, so the Dockerfile uses a slim Alpine base; the SOCKS5 binary is copied in from the distroless upstream's bin.
> 11. **`seed/04_socks5.sql` removed.** The Python `seed_nodes_socks5_only` fixture (Task 4a.5) is the sole seeding path; the SQL file was orphaned because the existing `schema-applier` does not auto-load it. Documentation comment about the seeding path added.
> 12. **`seed_aether_node_via_socks5` fixture is `yield`-style with try/finally cleanup** so a test failure does not leak the named container. Pre-test `docker rm -f` defensively clears any leftover from prior runs.
> 13. **Parity test (`test_protocol_parity_through_same_socks5`) polls `/healthz/tunnel` for the new node's `present=true` AND the old node's absence** before issuing the second request, eliminating the routing-cache staleness race.
> 14. **`Makefile` target `tunnel-e2e-socks5` teardown uses `$(TUNNEL_E2E_COMPOSE_SOCKS5) down -v --remove-orphans`** (NOT the default `tunnel-e2e-down` which lacks `--profile socks5` and would leak profile-gated containers).
> 15. **New Task 4a.10 (ROADMAP update)** — Phase 4 now updates `docs/ROADMAP.md` to strike SOCKS5 egress as resolved and add HTTP-CONNECT as a future candidate. The original draft missed this.
> 16. **Cold-start time estimate corrected** in Task 4a.7: cold first run takes ~10-15 min (Aether + tunnel-server image builds); warm cache ~3 min.
> 17. **Self-Review section 5 narrowed** — Phase 4 introduces no Phase-4-internal add-then-remove; the cross-phase audit acknowledges Phase 3 G4 + Phase 4 4b.2 coordination is "ensure no gate is added" rather than "remove a gate that was added".
> 18. **Placeholder hint includes all four schemes per spec §3.8 contract** — `http://proxy:port 或 socks5://proxy:port 或 socks5h://proxy:port` — not narrowed to SOCKS5-only (would contradict spec §3.8 "both protocols accept the same set").
> 19. **`relay_stream` plumb-through shown concretely** — the existing `tokio::spawn(relay_stream(...))` call site at lib.rs:210 area is extended with `cfg.egress.clone()` and `relay_stream`'s signature gains `egress: EgressMode`. Task 4a.2 Step 4 has the concrete diff.
> 20. **Memory file `tunnel-compat-cnexus-aether.md` line 22 corrected** — said "only one MR to main at end of Phase 3"; now reads "end of Phase 4".
> 21. **Spec §7 Phase 4 verification scenario count corrected** — was a flat "17 passed via make tunnel-e2e" conflating profile-gated counts; now broken into `tunnel-e2e` (12), `tunnel-e2e-baseline` (1), `tunnel-e2e-mixed` (11), `tunnel-e2e-socks5` (2) = 26 aggregate.

**Goal:** Phase 4 of the Cnexus ↔ Aether tunnel compatibility spec: give `gw-tunnel-server` SOCKS5/SOCKS5h egress dial support behind a `--proxy-url` CLI flag / `PROXY_URL` env, so an Aether-era deployment using `proxy_nodes.proxy_url = socks5://...` can be replaced 1:1 by a Cnexus tunnel-server with the same `proxy_url` value at deployment time and continue to function identically. Phase 4 closes the migration-continuity gap surfaced after Phase 3 was already drafted — SOCKS5 egress is the only Aether capability the Cnexus tunnel-server cannot match today, and replacing Aether-era nodes without it would require operators to reinstrument upstream egress (proxychains, sidecar, etc.) and break the Aether→Cnexus seamless-migration story.

**Why this is a phase, not a ROADMAP item:** the work is mid-sized (14 tasks across PR 4a / 4b / F1 / F2 after post-audit additions — see errata above) but it's load-bearing for the migration. Folding it into Phase 4 *before* the integrated MR opens means the migration story ships complete in one merge rather than as a follow-up that operators have to track. Per the user-locked 2026-06-15 review decision, Phase 4 also defers Phase 3's Task F3 (final MR) to Phase 4's final task — the integrated MR now carries Phase 1+2+3+4 commits.

**Branch / MR model:**
- **`ailinkmax-gateway` repo** — Phase 4 commits append to the **same single shared feature branch** as Phase 1+2+3. Phase 3 Task F3 (the originally-planned final integrated MR) is **deferred**: instead, Phase 4 Task F1 below opens the integrated MR that carries Phase 1+2+3+4 commits.
- **`AILinkMax-Go` repo** — PR 4b (frontend `proxy_url` exposure for both protocols) is a **real MR** in that repo. Phase 3 G4 (PR 3b) had introduced `proxy_url` UI for aether only; Phase 4 PR 4b widens it to also surface on cnexus nodes. Validation already supports both schemes on the Go side; no enum gate needs removal (verified at Task 4b.1).

**Architecture:** One commit group on the `ailinkmax-gateway` shared branch + one cross-repo Go PR + one final integrated MR to `main`.
- **PR 4a — Cnexus tunnel-server SOCKS5 egress + e2e + spec/issues update** (Tasks 4a.1-4a.9, this repo, on shared branch): `tokio-socks` dep, `EgressMode` enum, `--proxy-url` CLI / `PROXY_URL` env, single-point dial dispatcher at `relay_stream`'s `TcpStream::connect(&target)` site (line 261), unit tests, `socks5-egress` docker-compose addition + 2 e2e scenarios, `docs/spec/09-tunnel.md` Egress proxy section, ops gotcha doc.
- **PR 4b — Go frontend widening** (Tasks 4b.1-4b.3, `AILinkMax-Go` repo, **real MR**): verify no aether-only gate; surface `proxy_url` form-field for cnexus nodes; regression test on validation.
- **Phase 4 Task F1 — Open the integrated MR to `main`** (this repo): single MR carrying Phase 1+2+3+4 commits. Replaces Phase 3 Task F3.

**Tech Stack:** Rust 1.88 (pinned), `tokio-socks 0.5`, Go 1.21+, pytest 8.x, Docker Compose v2, `serjs/go-socks5-proxy` (or equivalent) SOCKS5 server image.

**Reference spec:** `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` (§3.8 Node-level upstream egress proxy, §7 Phase 4, §10 Glossary `proxy_url` / `EgressMode`).

**Out of scope for this plan (deferred to ROADMAP / future):**
- HTTP-CONNECT proxy schemes (`http://`, `https://`). Phase 4 only ships SOCKS5/SOCKS5h.
- Per-stream proxy selection. Single deployment-level SOCKS5 only.
- SOCKS5 connection pooling. Each relay-stream dials a fresh SOCKS5 handshake.
- Hub→tunnel-server push of `proxy_url` (would require Aether-style `remote_config` reverse channel).

**Locked decisions referenced in this plan (from 2026-06-15 plan-time review):**
1. **SOCKS5 only**, no HTTP-CONNECT. Per user: "保证目前cnexus的tunnel能够承接 aether时代的 socket5代理就行了" — minimal scope, just SOCKS5 parity.
2. **Deployer config, not DB push**: `gw-tunnel-server` reads `--proxy-url` / `PROXY_URL`; the DB's `proxy_url` field is informational. Matches Aether's existing operational model.
3. **`socks5` vs `socks5h` semantics honoured**: `socks5h` → DNS-at-proxy (send hostname); `socks5` → DNS-local (resolve then send IP). The URL scheme distinguishes.
4. **Allowed-target validation stays BEFORE SOCKS5 dial**: a misconfigured `proxy_url` cannot bypass the `--allow-target` / `--allow-private-targets` whitelist.
5. **Phase 3 Task F3 deferred**: integrated MR opens at Phase 4 Task F1 instead.
6. **Userinfo auth supported**: `socks5://user:pass@host:1080` parses authentication via standard URL form.
7. **No connection-pool reuse of SOCKS5 sessions**: each dial opens a fresh handshake. Performance acceptable for migration scope; pool is a future optimization.

---

## File Structure

### New files (PR 4a, this repo)

| Path | Responsibility |
|---|---|
| `tests/tunnel-e2e/Dockerfile.socks5-egress` | Tiny wrapper around `serjs/go-socks5-proxy:latest` (or `dperson/openvpn-client` equivalent) — just a `FROM` + healthcheck. Used by the two new e2e scenarios. |
| `tests/tunnel-e2e/scenarios/test_cnexus_socks5.py` | 2 scenarios: (a) `test_cnexus_through_socks5_reaches_upstream`, (b) `test_protocol_parity_through_same_socks5` (the migration-continuity case — both cnexus + aether through the same `socks5-egress` container yield byte-identical responses). |
| `docs/issues/2026-06-15-socks5-egress-deployment-coupling.md` | Operational gotcha doc: DB `proxy_url` is informational; tunnel-server reads from CLI/env. Documents the mismatch failure mode and the recommended deployment template (env-injected via the same source of truth that writes the DB row). |

### Files modified (PR 4a, this repo)

| Path | Change |
|---|---|
| `crates/gw-tunnel-server/Cargo.toml` | Add `tokio-socks = "0.5"` dep. |
| `crates/gw-tunnel-server/src/main.rs` | Add `--proxy-url <url>` CLI flag handling. Read `PROXY_URL` env var as fallback (CLI takes precedence). Parse URL, validate scheme is `socks5` or `socks5h`, extract `host:port` + optional userinfo. Place the parsed `EgressMode` into the `TunnelServerConfig` passed to `spawn`. Extend `--help` text. |
| `crates/gw-tunnel-server/src/lib.rs` | (a) New `pub enum EgressMode` with `Direct` and `Socks5 { proxy_addr, auth, resolve_at_proxy }` variants. (b) Add `pub egress: EgressMode` to `TunnelServerConfig` (`Default = Direct`). (c) Replace the single `TcpStream::connect(&target)` call at line 261 (`relay_stream`) with a `dial_via_egress(&cfg.egress, &target)` helper that dispatches per `EgressMode`. (d) Implement `dial_via_egress` per the spec §3.8 dispatch sketch using `tokio_socks::tcp::Socks5Stream`. (e) On startup (in `spawn` or its caller), log the resolved egress mode: `tracing::info!("proxy egress: direct")` or `tracing::info!("proxy egress: socks5{h}://<host>:<port> (DNS at <proxy\|local>)")`. |
| `crates/gw-tunnel-server/src/lib.rs` (cont.) | (f) Optional startup-time reachability probe: connect to the SOCKS5 endpoint with a 5s timeout. Failure logs `tracing::warn!`, does NOT refuse startup — per spec §3.8 operational contract, the operator may be staging the proxy. |
| `crates/gw-tunnel-server/tests/socks5_dispatch.rs` (new file or extend existing) | Unit tests: `direct_dial_unchanged`, `socks5_parses_userinfo`, `socks5h_resolves_at_proxy`, `socks5_resolves_locally`, `unsupported_scheme_rejected`. |
| `tests/tunnel-e2e/docker-compose.yml` | Add `socks5-egress` service (built from `Dockerfile.socks5-egress`). Add `cnexus-tunnel-server-socks5` service (built from same Dockerfile as the existing `cnexus-tunnel-server`, but with `--proxy-url socks5h://socks5-egress:1080` appended to the command). Wire the new tunnel-server with `depends_on: [socks5-egress: service_started]`. Both new services are under a `profiles: ["socks5"]` flag so `make tunnel-e2e` keeps the existing default scope and `make tunnel-e2e-socks5` opts into the SOCKS5 cases. |
| ~~`tests/tunnel-e2e/seed/04_socks5.sql`~~ | **Removed per Errata #11.** Schema-applier doesn't auto-discover extra SQL files; the Python `seed_nodes_socks5_only` fixture is the sole seeding path. |
| `Makefile` | New target `tunnel-e2e-socks5`: brings up the `socks5` compose profile, runs `pytest scenarios/test_cnexus_socks5.py`, tears down. |
| `docs/spec/09-tunnel.md` | Append an "Egress proxy" section with the full `proxy_url` table from spec §3.8, the `EgressMode` enum, the URL parsing rules, and a worked example (Aether-era node → Cnexus replacement with matching `--proxy-url`). |
| `crates/gw-tunnel-server/README.md` | Add a short "Egress proxy" subsection: how to use `--proxy-url` / `PROXY_URL`, the scheme set, the userinfo form, the `socks5` vs `socks5h` distinction. Reference `docs/spec/09-tunnel.md` for the full contract. |

### Files modified (PR 4b, `AILinkMax-Go` repo)

> All paths below are relative to the `AILinkMax-Go` repo root. **Phase 3 PR 3b already introduced `proxy_url` UI for aether nodes via the per-node detail drawer**; Phase 4 PR 4b widens this to the cnexus path (form field + table column where applicable).

| Path | Change |
|---|---|
| `modules/proxynode/business/service.go` | Verify (do not edit unless needed) that `CreateInput` / `UpdateInput` accept `proxy_url` for both protocols. The existing comments at `service.go:151,186` warn against confusing `proxy_url` with `ws_url`; that guardrail is correct and stays. If a `tunnel_protocol == cnexus`-only gate exists on `proxy_url`, REMOVE it (the SOCKS5 path is meant for both protocols). Cross-check with `modules/proxynode/business/entity.go:108` comments. |
| `modules/proxynode/business/service_test.go` | New table-driven case: `TestService_AcceptsProxyURLOnCnexusNode` covering `socks5://` and `socks5h://` schemes for a cnexus node. |
| `frontend/src/components/ProxyNodes/<form-component>` | Where the create/edit form is defined: show the `proxy_url` field for **all** protocols (Phase 3 PR 3b had conditioned it on `protocol === 'aether'`). The hint text mentions both schemes. Placeholder: `socks5://user:pass@proxy.host:1080` (matches Aether reference UI). |
| `frontend/src/pages/ProxyNodes/<table>` | Optional column addition (low priority): a `proxy_url`-presence indicator (badge: ⚠️ "via proxy" if set, blank if direct). Implementing engineer's call. |

---

## PR 4a · Cnexus tunnel-server SOCKS5 egress

### Task 4a.1: Add `tokio-socks` dep and the `EgressMode` enum + config field

**Files:**
- Modify: `crates/gw-tunnel-server/Cargo.toml`
- Modify: `crates/gw-tunnel-server/src/lib.rs`

- [ ] **Step 1: Add the dep**

Run:
```bash
cargo-remote-ctl add -p gw-tunnel-server tokio-socks
```

Verify the new line in `crates/gw-tunnel-server/Cargo.toml`:
```toml
tokio-socks = "0.5"
```

> If a newer compatible version is in the workspace catalog already, prefer that; otherwise pin to the latest 0.5.x.

- [ ] **Step 2: Write the failing test**

Create `crates/gw-tunnel-server/tests/socks5_dispatch.rs`:

```rust
use gw_tunnel_server::{EgressMode, TunnelServerConfig};

#[test]
fn config_defaults_to_direct_egress() {
    let c = TunnelServerConfig::default();
    assert!(matches!(c.egress, EgressMode::Direct), "default egress must be Direct, got {:?}", c.egress);
}

#[test]
fn egress_mode_socks5_constructs() {
    // Errata #7: proxy host stored as String + u16, NOT pre-resolved SocketAddr,
    // so a proxy container restart with a different IP does not produce a stale
    // cached address (e2e + prod robustness).
    let m = EgressMode::Socks5 {
        proxy_host: "127.0.0.1".to_string(),
        proxy_port: 1080,
        auth: None,
        resolve_at_proxy: true,
    };
    match m {
        EgressMode::Socks5 { resolve_at_proxy, .. } => assert!(resolve_at_proxy),
        _ => panic!("variant"),
    }
}

#[test]
fn egress_mode_socks5_with_auth() {
    let m = EgressMode::Socks5 {
        proxy_host: "127.0.0.1".to_string(),
        proxy_port: 1080,
        auth: Some(("user".into(), "pass".into())),
        resolve_at_proxy: false,
    };
    if let EgressMode::Socks5 { auth: Some((u, p)), .. } = m {
        assert_eq!(u, "user");
        assert_eq!(p, "pass");
    } else {
        panic!("expected auth");
    }
}
```

- [ ] **Step 3: Run failing test**

Run: `cargo-remote-ctl test -p gw-tunnel-server --test socks5_dispatch 2>&1 | tail -20`
Expected: COMPILE FAIL (`EgressMode` undefined, `TunnelServerConfig.egress` field missing).

- [ ] **Step 4: Implement the enum + config field**

In `crates/gw-tunnel-server/src/lib.rs`, near the existing `TunnelServerConfig` definition (currently around line 25-40 per the audit grep — confirm with `grep -n 'TunnelServerConfig' crates/gw-tunnel-server/src/lib.rs`):

```rust
use std::net::SocketAddr;

/// Upstream-egress mode for `gw-tunnel-server`'s outbound dials from `relay_stream`.
/// See `docs/spec/09-tunnel.md` §Egress proxy.
///
/// **Errata #7 (2026-06-15):** `proxy_host`/`proxy_port` are stored as String+u16
/// rather than a pre-resolved `SocketAddr`. DNS resolution happens at dial time
/// via `tokio::net::lookup_host` (async). This:
///  - avoids blocking the tokio runtime at parse time (the prior `std::net::ToSocketAddrs`
///    impl was synchronous);
///  - allows the proxy container to restart with a new IP without invalidating
///    the cached address;
///  - keeps `EgressMode` cheap to Clone.
#[derive(Clone, Debug)]
pub enum EgressMode {
    /// Direct TCP dial to the target. Default; current behaviour.
    Direct,

    /// Dial via a SOCKS5 proxy. `resolve_at_proxy = true` implements `socks5h`
    /// semantics (send hostname to proxy, proxy resolves); `false` implements
    /// plain `socks5` (resolve locally then send the resolved IP).
    Socks5 {
        proxy_host: String,
        proxy_port: u16,
        auth: Option<(String, String)>,
        resolve_at_proxy: bool,
    },
}

impl Default for EgressMode {
    fn default() -> Self { Self::Direct }
}

// In the existing TunnelServerConfig struct, add:
pub struct TunnelServerConfig {
    // ... existing fields (bind, auth_token, max_streams_per_conn, allowed_targets, allow_private_targets) ...
    pub egress: EgressMode,
}

// Mirror in Default impl:
impl Default for TunnelServerConfig {
    fn default() -> Self {
        Self {
            // ... existing defaults ...
            egress: EgressMode::Direct,
        }
    }
}
```

> Read the existing struct/impl shape first: `grep -n -A 20 'pub struct TunnelServerConfig\|impl Default for TunnelServerConfig' crates/gw-tunnel-server/src/lib.rs`. Match style (whether fields are listed alphabetically, whether `Default` is derived or hand-rolled).

- [ ] **Step 5: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-server --test socks5_dispatch
git add crates/gw-tunnel-server/Cargo.toml \
        crates/gw-tunnel-server/src/lib.rs \
        crates/gw-tunnel-server/tests/socks5_dispatch.rs
git commit -m "feat(gw-tunnel-server): add EgressMode + tokio-socks dep (config-only, no behaviour change)"
```

---

### Task 4a.2: Wire the dispatcher at `relay_stream`'s single dial site

**Files:**
- Modify: `crates/gw-tunnel-server/src/lib.rs`

> The current code (verified by audit): `let tcp = match TcpStream::connect(&target).await { ... }` at line 261 inside `relay_stream`. This is the **only** TCP dial site in the relay path. Replacement is mechanical.

- [ ] **Step 1: Write the failing test (dispatcher behaviour)**

Append to `crates/gw-tunnel-server/tests/socks5_dispatch.rs`:

```rust
use std::net::SocketAddr;

/// `dial_via_egress` with `EgressMode::Direct` and a reachable target on
/// localhost yields a connected TcpStream. Asserts the regression-safe path.
#[tokio::test]
async fn direct_dial_reaches_localhost_target() {
    // Start a one-shot listener on a random local port.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (_sock, _) = listener.accept().await.unwrap();
    });
    let target = format!("127.0.0.1:{port}");
    let stream = gw_tunnel_server::testing::dial_via_egress(&EgressMode::Direct, &target).await
        .expect("direct dial must succeed");
    drop(stream);
}

/// `dial_via_egress` with `EgressMode::Socks5` and an UNREACHABLE proxy errors
/// promptly (no hang). The exact error variant is library-specific; just assert
/// `is_err()`.
#[tokio::test]
async fn socks5_dial_to_unreachable_proxy_errors() {
    let mode = EgressMode::Socks5 {
        proxy_addr: "127.0.0.1:1".parse::<SocketAddr>().unwrap(),  // port 1: refused
        auth: None,
        resolve_at_proxy: true,
    };
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw_tunnel_server::testing::dial_via_egress(&mode, "example.com:80"),
    ).await;
    let inner = res.expect("must not hang");
    assert!(inner.is_err(), "unreachable proxy must error, got Ok");
}
```

> `gw_tunnel_server::testing::dial_via_egress` is exposed via a `pub mod testing { pub use ... }` block in `lib.rs` so the test can call it. If you prefer, mark `dial_via_egress` itself as `pub(crate)` and add an integration-test-only re-export, mirroring the existing test-export pattern in the crate (grep for `pub(crate)` and `pub mod testing` to see what's already used).

- [ ] **Step 2: Run failing test**

Run: `cargo-remote-ctl test -p gw-tunnel-server --test socks5_dispatch direct_dial_reaches_localhost_target 2>&1 | tail -20`
Expected: COMPILE FAIL (`dial_via_egress` undefined).

- [ ] **Step 3: Implement `dial_via_egress`**

Add to `crates/gw-tunnel-server/src/lib.rs`:

```rust
use anyhow::{anyhow, Context, Result};
use tokio::net::TcpStream;

/// Dial `target` ("host:port") through the configured egress mode.
/// For `EgressMode::Direct`, identical to `TcpStream::connect(target)`.
/// For `EgressMode::Socks5`, opens a SOCKS5 handshake against the proxy
/// then asks the proxy to connect to the final target.
///
/// `socks5h` (resolve_at_proxy=true): the hostname is sent to the proxy as-is;
/// the proxy is responsible for DNS resolution.
/// `socks5` (resolve_at_proxy=false): resolves the hostname locally, then asks
/// the proxy to connect to the resolved IP.
///
/// **Errata #7 (2026-06-15):** the SOCKS5 proxy address is resolved **here**
/// at dial time (not cached from parse time). Each call performs one DNS
/// lookup for the proxy host. This avoids stale-IP issues when the proxy
/// container restarts.
pub async fn dial_via_egress(egress: &EgressMode, target: &str) -> Result<TcpStream> {
    match egress {
        EgressMode::Direct => {
            TcpStream::connect(target).await
                .with_context(|| format!("direct connect to {target}"))
        }
        EgressMode::Socks5 { proxy_host, proxy_port, auth, resolve_at_proxy } => {
            use tokio_socks::tcp::Socks5Stream;

            // Resolve the proxy address at dial time (Errata #7).
            let proxy_addr = tokio::net::lookup_host((proxy_host.as_str(), *proxy_port))
                .await
                .with_context(|| format!("resolving proxy host {proxy_host}:{proxy_port}"))?
                .next()
                .ok_or_else(|| anyhow!("DNS yielded no addresses for proxy {proxy_host}:{proxy_port}"))?;

            if *resolve_at_proxy {
                let stream = match auth {
                    Some((u, p)) => Socks5Stream::connect_with_password(
                        proxy_addr, target, u.as_str(), p.as_str()
                    ).await,
                    None => Socks5Stream::connect(proxy_addr, target).await,
                }.with_context(|| format!("socks5h dial to {target} via {proxy_addr}"))?;
                Ok(stream.into_inner())
            } else {
                // socks5: resolve locally first.
                let resolved = tokio::net::lookup_host(target).await
                    .with_context(|| format!("local DNS resolve of {target}"))?
                    .next()
                    .ok_or_else(|| anyhow!("DNS yielded no addresses for {target}"))?;
                let stream = match auth {
                    Some((u, p)) => Socks5Stream::connect_with_password(
                        proxy_addr, resolved, u.as_str(), p.as_str()
                    ).await,
                    None => Socks5Stream::connect(proxy_addr, resolved).await,
                }.with_context(|| format!("socks5 dial to {resolved} via {proxy_addr}"))?;
                Ok(stream.into_inner())
            }
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
pub mod testing {
    pub use super::dial_via_egress;
}
```

- [ ] **Step 4: Replace the dial site in `relay_stream` AND plumb `cfg.egress` through (Errata #19)**

First locate the dial site:
```bash
grep -n 'TcpStream::connect(&target)' crates/gw-tunnel-server/src/lib.rs
```
Expected: one hit at ~line 261 inside `relay_stream`.

Concrete diff (use the existing `cfg.egress` since `handle_ws_conn` already has `cfg: Arc<TunnelServerConfig>` per the audit):

```rust
// crates/gw-tunnel-server/src/lib.rs

// (1) Extend relay_stream's signature with `egress: EgressMode`.
// Existing:
//   async fn relay_stream(
//       stream_id: u32,
//       target: String,
//       mut rx: mpsc::Receiver<Bytes>,
//       sink: ProtocolSink,
//       streams: Arc<Mutex<HashMap<u32, mpsc::Sender<Bytes>>>>,
//       _permit: OwnedSemaphorePermit,
//   ) { ... }
async fn relay_stream(
    stream_id: u32,
    target: String,
    egress: EgressMode,          // <-- new parameter (Errata #19)
    mut rx: mpsc::Receiver<Bytes>,
    sink: ProtocolSink,
    streams: Arc<Mutex<HashMap<u32, mpsc::Sender<Bytes>>>>,
    _permit: OwnedSemaphorePermit,
) {
    // (2) Replace the dial site (was: TcpStream::connect(&target).await).
    let tcp = match dial_via_egress(&egress, &target).await {
        Ok(s) => s,
        Err(e) => {
            sink.send_error(stream_id, Bytes::from(format!("dial: {e}"))).await;
            return;
        }
    };
    // ... rest of relay_stream unchanged ...
}

// (3) Update the tokio::spawn(relay_stream(...)) call site at ~line 210.
// Existing call:
//   tokio::spawn(relay_stream(
//       frame.stream_id, target, rx, sink.clone(), streams.clone(), permit,
//   ));
// Becomes:
tokio::spawn(relay_stream(
    frame.stream_id,
    target,
    cfg.egress.clone(),           // <-- Clone is cheap (Errata #7: only Strings, no SocketAddr)
    rx,
    sink.clone(),
    streams.clone(),
    permit,
));
```

`EgressMode: Clone` (derived at Task 4a.1), so `cfg.egress.clone()` is cheap. There is no other call site to `relay_stream` — verify with `grep -n 'relay_stream(' crates/gw-tunnel-server/src/lib.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo-remote-ctl test -p gw-tunnel-server`
Expected: PASS — all existing tunnel-server tests + the new `direct_dial_reaches_localhost_target` + `socks5_dial_to_unreachable_proxy_errors`. The existing `--allow-target` / target-validation tests should be unaffected because validation runs **before** `dial_via_egress`.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-tunnel-server/src/lib.rs \
        crates/gw-tunnel-server/tests/socks5_dispatch.rs
git commit -m "feat(gw-tunnel-server): dial through EgressMode::{Direct,Socks5} at relay_stream"
```

---

### Task 4a.3: CLI flag `--proxy-url` + `PROXY_URL` env in `main.rs`

**Files:**
- Modify: `crates/gw-tunnel-server/src/main.rs`

- [ ] **Step 1: Write the failing test (in `socks5_dispatch.rs`, exercising the URL parser)**

Add a parser helper in `lib.rs` (so it's unit-testable) and the test that drives it:

```rust
// crates/gw-tunnel-server/tests/socks5_dispatch.rs
use gw_tunnel_server::parse_egress_url;
use gw_tunnel_server::EgressMode;

#[test]
fn parse_egress_url_none_yields_direct() {
    let m = parse_egress_url(None).expect("parse");
    assert!(matches!(m, EgressMode::Direct));
}

#[test]
fn parse_egress_url_socks5_basic() {
    let m = parse_egress_url(Some("socks5://127.0.0.1:1080")).expect("parse");
    match m {
        EgressMode::Socks5 { proxy_host, proxy_port, auth, resolve_at_proxy } => {
            assert_eq!(proxy_host, "127.0.0.1");
            assert_eq!(proxy_port, 1080);
            assert!(auth.is_none());
            assert!(!resolve_at_proxy, "scheme 'socks5' = local DNS");
        }
        _ => panic!(),
    }
}

#[test]
fn parse_egress_url_socks5h_sets_resolve_at_proxy() {
    let m = parse_egress_url(Some("socks5h://192.0.2.10:9050")).expect("parse");
    if let EgressMode::Socks5 { resolve_at_proxy, .. } = m {
        assert!(resolve_at_proxy, "scheme 'socks5h' = DNS at proxy");
    } else {
        panic!();
    }
}

#[test]
fn parse_egress_url_with_userinfo() {
    let m = parse_egress_url(Some("socks5h://user:pass@10.0.0.5:1080")).expect("parse");
    if let EgressMode::Socks5 { auth: Some((u, p)), .. } = m {
        assert_eq!(u, "user");
        assert_eq!(p, "pass");
    } else {
        panic!("expected Some auth");
    }
}

#[test]
fn parse_egress_url_percent_decodes_userinfo() {
    // Errata #3: passwords with @ or : in them are percent-encoded in URLs.
    // `p%40ss` (= "p@ss") and `s%3Acrt` (= "s:crt") must round-trip.
    let m = parse_egress_url(Some("socks5h://user:p%40ss@10.0.0.5:1080")).expect("parse");
    if let EgressMode::Socks5 { auth: Some((_, p)), .. } = m {
        assert_eq!(p, "p@ss", "password '@' character must be percent-decoded");
    } else {
        panic!("expected Some auth");
    }
}

#[test]
fn parse_egress_url_default_port_socks5() {
    // Note: no DNS lookup at parse time (Errata #2), so hostnames are stored verbatim.
    let m = parse_egress_url(Some("socks5://proxy.local")).expect("parse");
    if let EgressMode::Socks5 { proxy_host, proxy_port, .. } = m {
        assert_eq!(proxy_host, "proxy.local");
        assert_eq!(proxy_port, 1080, "RFC 1928 default port");
    } else {
        panic!();
    }
}

#[test]
fn parse_egress_url_rejects_unsupported_scheme() {
    for bad in ["http://x:8080", "https://x:8443", "tcp://x:80", "garbage"] {
        let r = parse_egress_url(Some(bad));
        assert!(r.is_err(), "expected error for {bad}");
    }
}

#[test]
fn parse_egress_url_pure_function_no_dns_at_parse() {
    // Errata #2: hostnames are stored verbatim and resolved at dial time.
    // This test guarantees parse_egress_url is a pure function with no DNS
    // side effects, by passing a host that cannot resolve and observing
    // that parsing still succeeds.
    let r = parse_egress_url(Some("socks5h://invalid-host-no-dns.example:9050"));
    let m = r.expect("parse must not perform DNS, so must succeed even with invalid host");
    if let EgressMode::Socks5 { proxy_host, proxy_port, .. } = m {
        assert_eq!(proxy_host, "invalid-host-no-dns.example");
        assert_eq!(proxy_port, 9050);
    } else {
        panic!();
    }
}
```

- [ ] **Step 2: Implement `parse_egress_url` in `lib.rs` (Errata #2 + #3 + #9)**

```rust
use anyhow::{anyhow, Context, Result};
use percent_encoding::percent_decode_str;

/// Parse a `proxy_url` string into an `EgressMode`. None or empty → Direct.
/// Scheme set: `socks5`, `socks5h`. Unknown scheme → error.
/// Default port: 1080 (RFC 1928).
///
/// **Errata #2 (2026-06-15):** does **not** resolve DNS at parse time.
/// The host string is stored verbatim and resolved at dial time by
/// `dial_via_egress`. This keeps `parse_egress_url` a pure / synchronous
/// function safe to call from `main()` without blocking the tokio runtime.
///
/// **Errata #3 (2026-06-15):** percent-decodes URL userinfo (`u%40ser` → `u@ser`)
/// because `url::Url::username()`/`.password()` return percent-encoded ASCII
/// per RFC 3986; passing raw to SOCKS5 server would fail authentication for
/// any password containing `@`, `:`, or other reserved characters.
pub fn parse_egress_url(s: Option<&str>) -> Result<EgressMode> {
    let Some(raw) = s else { return Ok(EgressMode::Direct) };
    let raw = raw.trim();
    if raw.is_empty() { return Ok(EgressMode::Direct); }

    let u = url::Url::parse(raw).with_context(|| format!("invalid proxy_url {raw:?}"))?;

    let resolve_at_proxy = match u.scheme() {
        "socks5" => false,
        "socks5h" => true,
        other => return Err(anyhow!(
            "proxy_url scheme {other:?} not supported by gw-tunnel-server \
             (Phase 4 ships SOCKS5 only; HTTP-CONNECT (http/https) is a future ROADMAP item)"
        )),
    };

    let proxy_host = u.host_str().ok_or_else(|| anyhow!("proxy_url missing host"))?.to_string();
    let proxy_port = u.port().unwrap_or(1080);

    // Errata #3: percent-decode userinfo.
    let auth = if !u.username().is_empty() {
        let user_raw = u.username();
        let user = percent_decode_str(user_raw).decode_utf8_lossy().into_owned();
        let pass = match u.password() {
            Some(p) => percent_decode_str(p).decode_utf8_lossy().into_owned(),
            None => String::new(),  // username without password → empty password (SOCKS5 user/pass auth)
        };
        Some((user, pass))
    } else {
        None
    };

    Ok(EgressMode::Socks5 { proxy_host, proxy_port, auth, resolve_at_proxy })
}
```

Add `url` and `percent-encoding` deps if not already present:
```bash
grep '^url' crates/gw-tunnel-server/Cargo.toml || cargo-remote-ctl add -p gw-tunnel-server url
grep '^percent-encoding' crates/gw-tunnel-server/Cargo.toml || cargo-remote-ctl add -p gw-tunnel-server percent-encoding
```

> **Errata #3 note**: `percent_decode_str(...).decode_utf8_lossy()` returns `Cow<str>` which we then `into_owned()` for storage. Lossy decoding (vs strict) is the right choice here because operator credentials are typed by humans into config; a malformed UTF-8 sequence should not refuse startup, but the SOCKS5 server may still reject the auth.

- [ ] **Step 3: Wire the CLI flag + env var in `main.rs`**

In the existing CLI loop, add a new branch for `--proxy-url`:

```rust
// Inside the `while let Some(arg) = args.next()` loop:
"--proxy-url" => {
    proxy_url = Some(args.next()
        .ok_or_else(|| anyhow::anyhow!("--proxy-url requires a url"))?);
}
```

Declare `let mut proxy_url: Option<String> = None;` at the top of `main` alongside the other locals. After the loop, fall back to env if the flag wasn't set:

```rust
let proxy_url_final: Option<String> = proxy_url
    .or_else(|| std::env::var("PROXY_URL").ok());
let egress = gw_tunnel_server::parse_egress_url(proxy_url_final.as_deref())?;
```

Pass `egress` into the `TunnelServerConfig`:

```rust
let cfg = TunnelServerConfig {
    bind,
    auth_token,
    max_streams_per_conn,
    allowed_targets,
    allow_private_targets,
    egress,
};
```

Update the `--help` text:

```rust
println!(
    "gw-tunnel-server --bind 127.0.0.1:9001 [--auth-token TOKEN] [--max-streams N] [--allow-target host:port] [--allow-private-targets] [--proxy-url socks5[h]://[user:pass@]host:port]\n\
     env: PROXY_URL (fallback for --proxy-url)"
);
```

Add a log line right after parsing the egress mode:

```rust
match &cfg.egress {
    EgressMode::Direct => tracing::info!("proxy egress: direct"),
    EgressMode::Socks5 { proxy_addr, auth, resolve_at_proxy } => {
        let scheme = if *resolve_at_proxy { "socks5h" } else { "socks5" };
        let auth_marker = if auth.is_some() { " (auth)" } else { "" };
        let dns_at = if *resolve_at_proxy { "proxy" } else { "local" };
        tracing::info!("proxy egress: {scheme}://{proxy_addr}{auth_marker} (DNS at {dns_at})");
    }
}
```

- [ ] **Step 4: Reachability probe — non-blocking (Errata #12)**

The probe must NOT delay listener bind. Run it inside `tokio::spawn` so it executes concurrently with (or after) `spawn(cfg).await?`:

```rust
// AFTER cfg has been constructed and BEFORE spawn(cfg).await?:
if let EgressMode::Socks5 { proxy_host, proxy_port, .. } = &cfg.egress {
    let host = proxy_host.clone();
    let port = *proxy_port;
    tokio::spawn(async move {
        use tokio::time::{timeout, Duration};
        let resolved_attempt = tokio::net::lookup_host((host.as_str(), port)).await
            .ok().and_then(|mut a| a.next());
        let probe_result = match resolved_attempt {
            Some(addr) => timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(addr)).await,
            None => {
                tracing::warn!("proxy probe: DNS resolution failed for {host}:{port}");
                return;
            }
        };
        match probe_result {
            Ok(Ok(_)) => tracing::info!("proxy reachable: {host}:{port}"),
            Ok(Err(e)) => tracing::warn!("proxy unreachable at startup: {e} — relays will fail until proxy is up"),
            Err(_) => tracing::warn!("proxy probe timed out (5s) — relays will fail until proxy is up"),
        }
    });
}

// Listener bind proceeds immediately; the probe runs concurrently.
spawn(cfg).await?;
```

> Per spec §3.8 the probe failure does NOT refuse startup. Operators may be staging the proxy. Errata #12 corrected an earlier design where the probe ran serially before `spawn` and could delay listener bind by up to 5s.

- [ ] **Step 5: Run tests + smoke-check CLI**

```bash
cargo-remote-ctl test -p gw-tunnel-server
cargo-remote-ctl run -p gw-tunnel-server -- --help
```

Expected: help text mentions `--proxy-url`.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-tunnel-server/src/main.rs \
        crates/gw-tunnel-server/src/lib.rs \
        crates/gw-tunnel-server/Cargo.toml \
        crates/gw-tunnel-server/tests/socks5_dispatch.rs
git commit -m "feat(gw-tunnel-server): --proxy-url CLI + PROXY_URL env (socks5/socks5h schemes)"
```

---

### Task 4a.4: e2e — add `Dockerfile.socks5-egress` + compose `socks5` profile

**Files:**
- Create: `tests/tunnel-e2e/Dockerfile.socks5-egress`
- Modify: `tests/tunnel-e2e/docker-compose.yml`
- Create: `tests/tunnel-e2e/seed/04_socks5.sql`

- [ ] **Step 1: Write the Dockerfile (Errata #10 — distroless upstream has no shell)**

The upstream `serjs/go-socks5-proxy` final stage is **distroless**, with no `nc`, no `sh`, and no other utilities. A `HEALTHCHECK CMD nc -z ...` written inside that image fails permanently and blocks any `depends_on: service_healthy` chain. Fix by wrapping in an Alpine base that contains `nc`:

```dockerfile
# tests/tunnel-e2e/Dockerfile.socks5-egress
FROM serjs/go-socks5-proxy:latest AS sock
FROM alpine:3.20
RUN apk add --no-cache busybox-extras  # provides nc
COPY --from=sock /socks5 /usr/local/bin/socks5
ENV REQUIRE_AUTH=false
ENTRYPOINT ["/usr/local/bin/socks5"]
HEALTHCHECK --interval=5s --timeout=2s --retries=5 \
  CMD nc -z 127.0.0.1 1080 || exit 1
```

> `REQUIRE_AUTH=false` is set explicitly so a future image-default flip to `true` does not silently reject the e2e's unauthenticated `Socks5Stream::connect` calls.
>
> If the path to the SOCKS5 binary inside `serjs/go-socks5-proxy` is different from `/socks5` (verify with `docker inspect`), adjust the `COPY --from=sock <src> /usr/local/bin/socks5` line. As a fallback if `serjs/go-socks5-proxy` is unavailable, use `wernight/dante:latest` (Dante SOCKS5 server) or build from `golang:1.21-alpine` + `github.com/armon/go-socks5`. Adjust port matching if the listen port is not 1080.

- [ ] **Step 2: Add services + seed file**

In `tests/tunnel-e2e/docker-compose.yml`, append (under `services:`):

```yaml
  socks5-egress:
    profiles: ["socks5"]
    build:
      context: .
      dockerfile: Dockerfile.socks5-egress
    networks: [tunnel-net]
    expose: ["1080"]

  # Errata #5 + #6: build context + dockerfile match the existing
  # `cnexus-tunnel-server` service exactly (Dockerfile.tunnel-e2e, no BIN arg).
  # The root `Dockerfile` only builds gw-gateway and rejects unknown BIN args.
  # Allow-targets use `example.com` (the network alias set on fake-upstream
  # in the existing compose), NOT `fake-upstream`, because validate_target is
  # a string match against the over-the-wire target hostname.
  cnexus-tunnel-server-socks5:
    profiles: ["socks5"]
    build:
      context: ../../
      dockerfile: tests/tunnel-e2e/Dockerfile.tunnel-e2e
    command: >
      gw-tunnel-server
      --bind 0.0.0.0:7000
      --auth-token e2e-socks5-token
      --max-streams 64
      --allow-target example.com:9000
      --allow-target example.com:9443
      --proxy-url socks5h://socks5-egress:1080
    networks: [tunnel-net]
    expose: ["7000"]
    depends_on:
      fake-upstream:
        condition: service_started
      socks5-egress:
        condition: service_healthy
```

**Errata #11: no SQL seed file.** The original draft proposed `tests/tunnel-e2e/seed/04_socks5.sql`, but the existing `schema-applier` container's `entrypoint: ["/seed/01_schema.sh"]` does not auto-discover additional `*.sql` files in the seed directory — the file would orphan. The Python `seed_nodes_socks5_only` fixture (Task 4a.5) is the sole seeding path; it goes via `psycopg` + Redis XADD invalidation, matching the existing Phase 1 pattern. **Do NOT create `seed/04_socks5.sql`.**

- [ ] **Step 3: Commit**

```bash
git add tests/tunnel-e2e/Dockerfile.socks5-egress \
        tests/tunnel-e2e/docker-compose.yml
git commit -m "test(tunnel-e2e): add socks5-egress service + cnexus-via-socks5 (socks5 profile)"
```

---

### Task 4a.5: Write `test_cnexus_socks5.py` — 2 scenarios

**Files:**
- Create: `tests/tunnel-e2e/scenarios/test_cnexus_socks5.py`

- [ ] **Step 1: Write both scenarios**

```python
# tests/tunnel-e2e/scenarios/test_cnexus_socks5.py
"""Phase 4: SOCKS5 egress on gw-tunnel-server.

Two scenarios:
  1. Cnexus tunnel-server configured with --proxy-url socks5h://socks5-egress:1080
     can reach fake-upstream through that SOCKS5 proxy.
  2. Aether-parity: both protocols, given the same upstream proxy
     configuration, yield byte-identical responses (= the migration claim).

These tests assume the `socks5` compose profile is active. Run via
`make tunnel-e2e-socks5`.
"""

import os

import httpx
import pytest


pytestmark = pytest.mark.usefixtures("socks5_profile_active")


@pytest.fixture(scope="module")
def socks5_profile_active():
    """Confirms the cnexus-via-socks5 node is in the gateway's tunnel snapshot.
    Skips with a clear message if the operator forgot --profile socks5.
    """
    resp = httpx.get(
        f"{os.environ['ALI_GATEWAY_URL']}/healthz/tunnel",
        headers={"Authorization": f"Bearer {os.environ['ADMIN_TOKEN']}"},
        timeout=5.0,
    )
    body = resp.json()
    has = any(n.get("node_id") == "cn-socks5" for n in body.get("nodes", []))
    if not has:
        pytest.skip("compose profile 'socks5' not active; run `make tunnel-e2e-socks5`")
    return True


def test_cnexus_through_socks5_reaches_upstream(seed_nodes_socks5_only, gateway_client):
    """Route a request to the cn-socks5 node; verify fake-upstream is hit
    through socks5-egress."""
    seed_nodes_socks5_only("cn-socks5")
    resp = gateway_client.post(
        "/v1/echo",
        headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid-socks5"},
        json={"q": "hello-via-socks5"},
        timeout=15.0,
    )
    assert resp.status_code == 200, resp.text
    body = resp.json()
    assert body.get("q") == "hello-via-socks5"

    # Indirect proof: fake-upstream's hit counter should have incremented.
    # If your fake-upstream exposes a counter endpoint, check it; otherwise
    # asserting 200 + body integrity is enough to prove the tunnel path worked.

    # Optional: confirm via the gateway's per-request log that the tunnel-server
    # logged a SOCKS5 dial. The exact log shape depends on `gw-tunnel-server`'s
    # logging conventions; this assertion is documented as a follow-up if a
    # structured log is needed.


def test_protocol_parity_through_same_socks5(
    seed_aether_node_via_socks5, seed_nodes_socks5_only,
    wait_for_tunnel_snapshot, gateway_client
):
    """The migration claim: an aether-proxy and a cnexus tunnel-server, BOTH
    configured to egress via the same SOCKS5, MUST yield byte-identical
    response bodies for the same request.

    Errata #13: between the cnexus and aether request we explicitly poll
    /healthz/tunnel to confirm the gateway has torn down the cnexus
    transport and attached the aether one. Eliminates routing-cache
    staleness flakes.
    """
    # 1. Drive the request through the cnexus-via-socks5 node.
    seed_nodes_socks5_only("cn-socks5")
    wait_for_tunnel_snapshot("cn-socks5", want_present=True)
    cnexus_resp = gateway_client.post(
        "/v1/echo",
        headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid-socks5-cn"},
        json={"q": "parity-check"},
        timeout=15.0,
    )

    # 2. Switch the pool to the aether-via-socks5 node. Wait for the gateway's
    # tunnel snapshot to reflect this BEFORE issuing the second request — both
    # cn-socks5 must be gone AND ae-socks5 must be present+alive.
    seed_aether_node_via_socks5("ae-socks5")
    seed_nodes_socks5_only("ae-socks5")  # wipes cn-socks5, inserts ae-socks5
    wait_for_tunnel_snapshot("cn-socks5", want_present=False)
    wait_for_tunnel_snapshot("ae-socks5", want_present=True)
    aether_resp = gateway_client.post(
        "/v1/echo",
        headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid-socks5-ae"},  # distinct kid → no sticky carryover
        json={"q": "parity-check"},
        timeout=15.0,
    )

    # 3. Byte-identical bodies (fake-upstream returns deterministic echo).
    assert cnexus_resp.status_code == aether_resp.status_code == 200
    assert cnexus_resp.content == aether_resp.content, (
        f"protocol parity broken via shared SOCKS5\n"
        f"  cnexus body: {cnexus_resp.content!r}\n"
        f"  aether body: {aether_resp.content!r}"
    )
```

> `seed_nodes_socks5_only`, `seed_aether_node_via_socks5`, and `gateway_client` are fixtures expected on `conftest.py`. The first two are new; add them in Step 2.

- [ ] **Step 2: Extend `conftest.py` with the new fixtures**

In `tests/tunnel-e2e/conftest.py`:

```python
@pytest.fixture
def seed_nodes_socks5_only(seed_nodes):
    """Wipe the node pool and seed exactly one row matching the given id.
    Used by the SOCKS5 scenarios so routing is deterministic."""
    def _seed(node_id: str):
        # `seed_nodes` is the existing Phase 1 fixture that handles XADD invalidation.
        # We piggy-back by passing the same shape.
        if node_id == "cn-socks5":
            seed_nodes({
                "id": "cn-socks5", "tunnel_protocol": "cnexus",
                "ws_url": "wss://cnexus-tunnel-server-socks5:7000/tunnel",
                "auth_token": "e2e-socks5-token",
                "proxy_url": "socks5h://socks5-egress:1080",
                "node_name": "cnexus-via-socks5", "enabled": True,
            })
        elif node_id == "ae-socks5":
            seed_nodes({
                "id": "ae-socks5", "tunnel_protocol": "aether",
                "aether_config": {"auth_token": "ae_e2e_socks5"},
                "proxy_url": "socks5h://socks5-egress:1080",
                "node_name": "aether-via-socks5", "enabled": True,
            })
        else:
            raise AssertionError(f"unsupported seed id {node_id}")
    return _seed


@pytest.fixture
def seed_aether_node_via_socks5(aether_admin):
    """Start an additional aether-proxy container with PROXY_URL set, register
    it under the given node_id.

    Errata #12: yield-style fixture with try/finally cleanup so a test failure
    does not leak the named container. Pre-test `docker rm -f` defensively
    clears any leftover from prior runs (name collision otherwise blocks
    `docker compose run`).
    """
    started_nodes: list[str] = []

    def _seed(node_id: str):
        import subprocess
        # Defensive cleanup of stale container with the same name.
        subprocess.run(
            ["docker", "rm", "-f", "aether-proxy-socks5"],
            check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        subprocess.run([
            "docker", "compose",
            "-f", "tests/tunnel-e2e/docker-compose.yml",
            "--profile", "socks5",
            "run", "-d",                  # detached
            # NOTE: no --rm — explicit teardown below catches it.
            "-e", "AETHER_PROXY_AUTH_TOKEN=ae_e2e_socks5",
            "-e", "AETHER_PROXY_NODE_NAME=aether-via-socks5",
            "-e", "PROXY_URL=socks5h://socks5-egress:1080",
            "--name", "aether-proxy-socks5",
            "aether-proxy",
        ], check=True)
        started_nodes.append(node_id)
        aether_admin.await_aether_attach(node_id, timeout_s=30)

    yield _seed

    # Teardown: stop + remove the container regardless of test outcome.
    if started_nodes:
        import subprocess
        subprocess.run(
            ["docker", "rm", "-f", "aether-proxy-socks5"],
            check=False, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )


@pytest.fixture
def wait_for_tunnel_snapshot():
    """Errata #13: poll /healthz/tunnel until the named node is present-and-alive
    (or absent). Used by the parity test between successive seed swaps to
    eliminate routing-cache staleness."""
    import os, time, httpx
    def _wait(node_id: str, *, want_present: bool, timeout_s: float = 15.0):
        deadline = time.time() + timeout_s
        last_state = None
        while time.time() < deadline:
            r = httpx.get(
                f"{os.environ['ALI_GATEWAY_URL']}/healthz/tunnel",
                headers={"Authorization": f"Bearer {os.environ['ADMIN_TOKEN']}"},
                timeout=2.0,
            )
            nodes = r.json().get("nodes", [])
            entry = next((n for n in nodes if n.get("node_id") == node_id), None)
            if want_present:
                if entry and entry.get("present") and entry.get("alive"):
                    return entry
            else:
                if entry is None:
                    return None
            last_state = entry
            time.sleep(0.5)
        raise AssertionError(
            f"timeout waiting for node {node_id} (want_present={want_present}); last state: {last_state}"
        )
    return _wait
```

- [ ] **Step 3: Commit**

```bash
git add tests/tunnel-e2e/scenarios/test_cnexus_socks5.py \
        tests/tunnel-e2e/conftest.py
git commit -m "test(tunnel-e2e): 2 SOCKS5 egress scenarios (cnexus path + protocol parity)"
```

---

### Task 4a.6: `Makefile` target `tunnel-e2e-socks5`

**Files:**
- Modify: `Makefile`

```makefile
# Append to existing tunnel-e2e block:

TUNNEL_E2E_COMPOSE_SOCKS5 := -f tests/tunnel-e2e/docker-compose.yml \
    --project-directory tests/tunnel-e2e --profile socks5

.PHONY: tunnel-e2e-socks5 tunnel-e2e-socks5-down

# Errata #14: teardown uses TUNNEL_E2E_COMPOSE_SOCKS5 (with --profile socks5).
# The plain `tunnel-e2e-down` lacks the profile flag and would leak
# socks5-egress + cnexus-tunnel-server-socks5 containers.
tunnel-e2e-socks5: tunnel-e2e-deps
	docker compose $(TUNNEL_E2E_COMPOSE_SOCKS5) build
	docker compose $(TUNNEL_E2E_COMPOSE_SOCKS5) run --rm \
	  pytest-runner pytest scenarios/test_cnexus_socks5.py -v
	$(MAKE) tunnel-e2e-socks5-down

tunnel-e2e-socks5-down:
	docker compose $(TUNNEL_E2E_COMPOSE_SOCKS5) down -v --remove-orphans
```

Commit.

---

### Task 4a.7: Run `make tunnel-e2e-socks5` end-to-end

- [ ] **Step 1: Clean tree + run**

```bash
make tunnel-e2e-down
make tunnel-e2e-socks5
```

Expected: `2 passed`. **Errata #16 (cold-start time):** on a fully-cold Docker cache, expect ~10-15 min total — Aether-proxy image build is 5-10 min (Phase 2 finding), the tunnel-e2e Rust build adds 2-4 min, and the new socks5-egress image is trivially fast (<10s). On warm cache (Phase 1-3 already built), the protocol-parity scenario adds ~3 min for the second aether-proxy bring-up.

- [ ] **Step 2: Inspect logs if anything fails**

```bash
docker compose -f tests/tunnel-e2e/docker-compose.yml --profile socks5 logs cnexus-tunnel-server-socks5
docker compose -f tests/tunnel-e2e/docker-compose.yml --profile socks5 logs socks5-egress
```

Expected to see in `cnexus-tunnel-server-socks5` logs: `proxy egress: socks5h://socks5-egress:1080 (DNS at proxy)` and `proxy reachable: socks5-egress:1080`. If the second line is absent, the probe failed silently — check connectivity inside the docker network.

- [ ] **Step 3: Capture summary for MR description (Task F1)**

Save the final 3 lines of the `pytest` output. Discard the docker build noise.

---

### Task 4a.8: Spec + issues + tunnel-server README

**Files:**
- Modify: `docs/spec/09-tunnel.md`
- Create: `docs/issues/2026-06-15-socks5-egress-deployment-coupling.md`
- Create or modify: `crates/gw-tunnel-server/README.md`

- [ ] **Step 1: `docs/spec/09-tunnel.md`**

This file was rewritten in Phase 3 Task F1 as the as-built reference. Phase 4 appends a new section under §10 (or wherever fits the F1 outline):

```markdown
## Egress proxy (Phase 4, 2026-06-15)

Node-scoped upstream-egress proxy via `proxy_nodes.payload.proxy_url`.
Supported schemes: `socks5`, `socks5h`. (HTTP-CONNECT `http://` /
`https://` schemes are accepted by the Go admin but not yet implemented
in `gw-tunnel-server`; see ROADMAP.)

| Concern | Cnexus (gw-tunnel-server) | Aether (aether-proxy) |
|---|---|---|
| Reads `proxy_url` from | `--proxy-url` CLI or `PROXY_URL` env | Aether-native config (env / CLI / file) |
| Hub awareness | None (informational in DB) | None (informational in DB) |
| DNS-at-proxy (`socks5h`) | Supported | Supported |
| DNS-local (`socks5`) | Supported | Supported |
| Userinfo auth | Supported (URL form) | Supported (URL form) |
| Allowed-target enforcement | BEFORE SOCKS5 dial (no bypass) | Aether's own allowlist |

Migration claim: an Aether-era node with `proxy_url = socks5://X` can be
replaced by a `gw-tunnel-server` deployed with `--proxy-url socks5://X`.
Body bytes from upstream are byte-identical to within the existing
cnexus-vs-aether parity bounds documented in §2.

Tunnel-e2e verification: `make tunnel-e2e-socks5` runs
`test_cnexus_socks5.py`. The protocol-parity case asserts byte-identity
across both protocols when both egress through the same SOCKS5.
```

- [ ] **Step 2: `docs/issues/2026-06-15-socks5-egress-deployment-coupling.md`**

```markdown
# SOCKS5 egress: `proxy_url` deployment coupling

**Issue type**: operational gotcha (not a bug)
**Affects**: `gw-tunnel-server` (Phase 4+), `aether-proxy` (pre-existing)

## The coupling

`proxy_nodes.payload.proxy_url` is **informational** from the gateway's
perspective. The actual `--proxy-url` value used by `gw-tunnel-server`
comes from the **deployment** of that server (CLI flag or `PROXY_URL`
env). Aether has the same coupling: the DB value is for admin UI
display + `probeProxy` reachability check; `aether-proxy` reads its own
config separately.

## Failure mode

If the DB `proxy_url` says `socks5://prod-proxy:1080` but the deployed
tunnel-server was launched with `--proxy-url socks5://staging-proxy:1080`
(or omitted entirely), upstream traffic does NOT go through the proxy the
operator believes it does. There is no runtime alarm — the admin probe
still hits the DB-recorded value, which is reachable, so health checks
look green.

## Recommended deployment template

Make the DB row and the deployment env share one source of truth (e.g.
Kustomize / Helm / Terraform). When you change `proxy_url` in the admin
UI, fire a redeploy that picks up the same value via `PROXY_URL` env.

## Detection

Operators should periodically grep deployment manifests for `PROXY_URL`
and diff against the DB `proxy_url` column. A small audit script
candidate is in ROADMAP as a future helper.
```

- [ ] **Step 3: `crates/gw-tunnel-server/README.md`**

Add or append:

```markdown
## Egress proxy

`gw-tunnel-server` can dial upstream targets through a SOCKS5 proxy by
setting:

- CLI: `--proxy-url socks5[h]://[user:pass@]host:port`
- Env: `PROXY_URL=socks5[h]://...` (used if `--proxy-url` is absent)

Scheme semantics:
- `socks5` — DNS resolved locally, IP sent to the proxy.
- `socks5h` — hostname sent to the proxy, proxy resolves DNS.

The `--allow-target` whitelist is enforced BEFORE the SOCKS5 dial, so a
misconfigured proxy cannot route to a non-whitelisted host.

Each relay-stream opens a fresh SOCKS5 handshake; no pooling. For Aether
parity (migration continuity) details see
`docs/spec/09-tunnel.md` §Egress proxy.
```

- [ ] **Step 4: Commit**

```bash
git add docs/spec/09-tunnel.md \
        docs/issues/2026-06-15-socks5-egress-deployment-coupling.md \
        crates/gw-tunnel-server/README.md
git commit -m "docs: SOCKS5 egress section + deployment-coupling gotcha + tunnel-server README"
```

---

### Task 4a.9: PR-4a verification gate

- [ ] **Step 1: Workspace check + test + clippy + invariants** — all clean

Run:
```bash
cargo-remote-ctl check --workspace --all-targets
cargo-remote-ctl test --workspace
cargo-remote-ctl clippy --workspace --all-targets -- -D warnings
bash scripts/check-invariants.sh
```

- [ ] **Step 2: `make tunnel-e2e`** (Phase 1+2+3 default scope) — clean

- [ ] **Step 3: `make tunnel-e2e-mixed`** (Phase 3 mixed-pool) — clean

- [ ] **Step 4: `make tunnel-e2e-socks5`** (Phase 4 new) — `2 passed`

- [ ] **Step 5: `bash scripts/check-tunnel-deps.sh`** — unchanged from Phase 3 baseline

- [ ] **Step 6: Capture all summaries for Task F1's MR description**

---

### Task 4a.10: ROADMAP closeout for SOCKS5 (Errata #15)

> Added 2026-06-15 post-audit. Phase 3 Task F2 created a "Phase 4 candidates" section in `docs/ROADMAP.md` listing `remote_config`, perf baseline, and double-Aether — but did NOT include SOCKS5 egress (Phase 4 was not yet conceived at that time). Phase 4 now needs to: (a) acknowledge SOCKS5 egress as resolved (just landed in PR 4a), (b) rename the section since "Phase 4 candidates" is now stale, and (c) add HTTP-CONNECT as a new candidate.

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Patch `docs/ROADMAP.md`**

Find the section header that Phase 3 Task F2 inserted:

```bash
grep -n "Phase 4 candidates\|Tunnel — Phase 4" docs/ROADMAP.md
```

Rename `Tunnel — Phase 4 candidates (priority: nice-to-have)` to `Tunnel — Future candidates (priority: nice-to-have)`.

Within that section, the original Phase 3 F2 wording must be updated:

- The line "Aether `remote_config` reverse-push" — keep, no change.
- The line "Cross-protocol performance baseline" — keep, no change.
- The line "Double-Aether multi-tenant tunnel-server" — keep, no change.
- **Add** a line at the end:

```markdown
- **HTTP-CONNECT egress (`http://`, `https://` proxy schemes)**: The Go admin
  validation accepts these schemes; Aether-proxy supports them natively.
  Cnexus `gw-tunnel-server` (Phase 4) implements `socks5` / `socks5h` only.
  A future phase could add HTTP-CONNECT to `dial_via_egress` via the
  `tokio-http-connect` crate or equivalent. Estimated effort: small —
  the dispatcher is already in place.
```

Add a new "Resolved" subsection above the candidates (or wherever the file's resolved/closed entries live):

```markdown
## Tunnel — Resolved by Phase 4 (2026-06-15)

- **Cnexus SOCKS5 egress (`proxy_url` Aether-parity)**: `gw-tunnel-server` now accepts
  `--proxy-url socks5[h]://...` / `PROXY_URL` env. See `docs/spec/09-tunnel.md` §Egress proxy.
```

- [ ] **Step 2: Verify the rename was clean**

```bash
grep -c "Phase 4 candidates" docs/ROADMAP.md
```

Expected: `0` (zero remaining mentions of the old section name).

- [ ] **Step 3: Commit**

```bash
git add docs/ROADMAP.md
git commit -m "docs(roadmap): SOCKS5 egress resolved in Phase 4; add HTTP-CONNECT as future candidate"
```

---

## PR 4b · Go frontend widening (AILinkMax-Go)

> **Repo**: `AILinkMax-Go`. **Branch**: `feature/tunnel-compat-phase-4-go`. Lands as a real MR before Phase 4 Task F1 opens the Rust integrated MR.

### Task 4b.1: Add `proxy_url` scheme validation + cnexus-acceptance regression test

> **Errata #3 + #4** (post-audit redesign): the original draft had two bugs. (a) The proposed `TestService_RejectsUnknownProxyURLScheme` test expected `ftp://` to be rejected, but the live Go code has **no scheme validation at all** — `service.go::CreateManual`/`UpdateManual` accept any string as `ProxyURL` (verified at `service.go:136-165`). (b) The test code used `CreateInput{NodeName, WsURL, Protocol, ProxyURL}` which is not a real DTO — the actual DTO is `business.ManualCreateRequest{Name, ProxyURL, Protocol, Username, Password, Region}` at `entity.go:193-199`. Both are corrected below. The redesign **adds** a small `validateProxyURL` helper to close the gap (spec §3.8 says unknown schemes → 400; the Go side did not enforce this).

**Files:**
- Modify: `modules/proxynode/business/service.go` (add `validateProxyURL` helper + call sites in `CreateManual` / `UpdateManual` / `Register`)
- Modify: `modules/proxynode/business/service_test.go` (new test cases using the real DTO shape)

- [ ] **Step 1: Audit existing `proxy_url` handling (confirms the pre-condition for the fix)**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
grep -n 'proxy_url\|ProxyURL\|tunnel_protocol\|Protocol' modules/proxynode/business/*.go
```

Expected findings (verified at audit time):
- `entity.go:193-199`: `type ManualCreateRequest struct { Name, ProxyURL, Protocol, Username, Password, Region string }`.
- `entity.go:163`: DTO `ProxyNode` has `ProxyURL string \`json:"proxy_url,omitempty"\``.
- `service.go:136-165`: `CreateManual` validates `Protocol` via `validateProtocol()` but performs **zero** scheme validation on `ProxyURL`.
- `service.go:150-153,186-188`: comments warn against confusing `proxy_url` with `ws_url`. Keep this guard; it is orthogonal to scheme validation.
- `ops.go:38-46`: `defaultPort` matches `socks5`/`socks5h`/`https`/(default 80) for the reachability probe.
- No `if Protocol != "aether"` gate exists on `proxy_url` anywhere → the field is already accepted for both protocols. The fix is purely additive (scheme validation).

- [ ] **Step 2: Add `validateProxyURL` helper**

In `modules/proxynode/business/service.go`:

```go
import "net/url"

// validateProxyURL enforces the spec §3.8 contract: proxy_url must be empty
// or use one of the four supported schemes. Errata #3 (2026-06-15): closes
// the gap where the Go service previously accepted any string.
//
// Implementation note: the WS-vs-proxy guardrail at service.go:150-153 is
// orthogonal — it prevents misuse of proxy_url AS a ws_url alias. This
// validator runs first; if proxy_url passes here it then flows through
// wsSchemeOnly for the ws_url-derivation guard.
func validateProxyURL(s string) error {
    if s == "" {
        return nil  // empty is valid: direct egress
    }
    u, err := url.Parse(s)
    if err != nil {
        return fmt.Errorf("proxynode: proxy_url is not a valid URL: %w", err)
    }
    switch u.Scheme {
    case "socks5", "socks5h", "http", "https":
        return nil
    default:
        return fmt.Errorf("proxynode: proxy_url scheme %q is not supported (allowed: socks5, socks5h, http, https)", u.Scheme)
    }
}
```

Wire it into `CreateManual` and `UpdateManual` (and `Register` if it accepts a `proxy_url`):

```go
func (s *Service) CreateManual(ctx context.Context, req ManualCreateRequest) (*ProxyNode, error) {
    // ... existing protocol validation ...
    if err := validateProxyURL(req.ProxyURL); err != nil {
        return nil, err
    }
    // ... rest unchanged ...
}
```

> If `UpdateManual` has its own DTO shape distinct from `ManualCreateRequest`, mirror the call there. Same for `Register`.

- [ ] **Step 3: Write the regression tests using the real DTO shape**

In `modules/proxynode/business/service_test.go`:

```go
func TestService_AcceptsProxyURLOnCnexusNode(t *testing.T) {
    svc := newServiceForTest(t)
    cases := []struct {
        name string
        url  string
    }{
        {"socks5", "socks5://proxy.example.com:1080"},
        {"socks5h", "socks5h://proxy.example.com:1080"},
        {"socks5_with_auth", "socks5h://user:pass@proxy.example.com:1080"},
        {"http_proxy", "http://corp-proxy.example.com:3128"},
        {"https_proxy", "https://corp-proxy.example.com:443"},
        {"empty_means_direct", ""},
    }
    for _, tc := range cases {
        t.Run(tc.name, func(t *testing.T) {
            n, err := svc.CreateManual(context.Background(), ManualCreateRequest{
                Name:     "cn-with-proxy-" + tc.name,
                Protocol: "cnexus",
                ProxyURL: tc.url,
            })
            if err != nil {
                t.Fatalf("cnexus node with proxy_url=%q must succeed, got %v", tc.url, err)
            }
            if n.ProxyURL != tc.url {
                t.Fatalf("ProxyURL not persisted: want %q got %q", tc.url, n.ProxyURL)
            }
        })
    }
}

func TestService_RejectsUnknownProxyURLScheme(t *testing.T) {
    svc := newServiceForTest(t)
    bad := []string{
        "ftp://x.example.com:21",
        "tcp://x.example.com:80",
        "ssh://x.example.com:22",
    }
    for _, u := range bad {
        t.Run(u, func(t *testing.T) {
            _, err := svc.CreateManual(context.Background(), ManualCreateRequest{
                Name:     "cn-bad",
                Protocol: "cnexus",
                ProxyURL: u,
            })
            if err == nil {
                t.Fatalf("unknown scheme %q must be rejected", u)
            }
        })
    }
}

func TestService_RejectsMalformedProxyURL(t *testing.T) {
    svc := newServiceForTest(t)
    _, err := svc.CreateManual(context.Background(), ManualCreateRequest{
        Name:     "cn-malformed",
        Protocol: "cnexus",
        ProxyURL: "::::not a url",
    })
    if err == nil {
        t.Fatal("malformed URL must be rejected by validateProxyURL")
    }
}
```

> The `newServiceForTest` helper exists per existing test conventions; mirror its setup. The `ManualCreateRequest` does NOT have `NodeName` or `WsURL` fields — those were a mis-recollection in the original draft.

- [ ] **Step 4: Run tests**

```bash
go test ./modules/proxynode/business -run 'TestService_(Accepts|Rejects)ProxyURL\|RejectsMalformedProxyURL' -v
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add modules/proxynode/business/service.go \
        modules/proxynode/business/service_test.go
git commit -m "feat(proxynode): validateProxyURL helper + cnexus proxy_url acceptance tests"
```

---

### Task 4b.2: `proxy_url` form-field stays unconditional for both protocols (coordination + smoke-test)

> **Errata #2 + #18 (post-audit redesign):** the original draft assumed Phase 3 PR 3b had introduced an aether-only `proxy_url` form-field conditional that Phase 4 would now widen. Audit revealed this assumption was wrong: the live `frontend/src/views/admin/ProxyNodes.vue:473` already shows `proxy_url` unconditionally (Vue template, no `v-if`-on-protocol). Phase 3 G4 tasks (G4.2-G4.6 in the Phase 3 plan) do not touch `proxy_url` — they add ProtocolBadge, ProtocolRadio, AetherStatusPanel, AetherTokenManager, AetherRegisterHistory, and GatewayControl gray-out, none of which conditionalize `proxy_url`. **Phase 4 PR 4b is therefore a coordination + regression guard, not a "remove an aether-only conditional" task.**
>
> The task's job:
> 1. Verify the current state at the time of execution (the unconditional render at `frontend/src/views/admin/ProxyNodes.vue:473` must still be unconditional).
> 2. Verify that Phase 3 G4 (if it has already landed on the shared branch) did NOT accidentally introduce a protocol gate while adding the new components.
> 3. Update the placeholder hint to mention all four schemes per spec §3.8 contract.
> 4. Smoke-test that the cnexus + proxy_url path works in the UI.

**Files:**
- Read-only verify: `frontend/src/views/admin/ProxyNodes.vue` (or wherever ProxyNodes is rendered after Phase 3 G4 lands)
- Modify: same file (placeholder hint widening only; no `v-if`-on-protocol changes)

- [ ] **Step 1: Audit the current `proxy_url` render shape**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
grep -n "proxy_url\|v-if.*protocol\|v-show.*protocol" frontend/src/views/admin/ProxyNodes.vue | head -20
```

Expected: `proxy_url` input renders unconditionally, no `v-if`/`v-show` keyed on `protocol`. **If you find a protocol gate, that is a Phase 3 G4 regression — file it and remove the gate as part of this commit, citing spec §3.8 "both protocols accept the same set."**

- [ ] **Step 2: Update placeholder hint to include all four schemes (Errata #18)**

The Aether reference UI placeholder is `http://proxy:port 或 socks5://proxy:port` (verified at `/Users/whereslow/Desktop/work/Aether/frontend/src/views/admin/ProxyNodes.vue:460`). The current AILinkMax-Go placeholder is the same. Phase 4 widens to **all four schemes** per spec §3.8 contract:

```vue
<el-input
  v-model="addForm.proxy_url"
  placeholder="http://proxy:port 或 socks5://proxy:port 或 socks5h://user:pass@proxy:port"
  clearable
/>
```

> Schema set in placeholder follows the spec §3.8 contract: `socks5`, `socks5h`, `http`, `https`. The implementing engineer matches Element Plus / Naive UI / whatever component library the existing form uses. Do NOT narrow to SOCKS5-only — that would contradict spec §3.8 "no protocol-conditioned hiding".

- [ ] **Step 3: (Optional, engineer's call) `proxy_url`-presence column on the proxy-nodes table**

If the existing nodes table doesn't already show `proxy_url`, an opt-in column can be added. **Caveat (Errata: post-audit nit):** all manual-created nodes have `proxy_url` set (it's required on `ManualCreateRequest` per `entity.go:195-196`), so a "⚠️ via proxy" badge would appear on nearly all rows. Only worth adding if you can distinguish "egress proxy" from "ws transport" (the validator-allowed schemes split: `socks5*` / `http*` → egress; `ws*` would be a legacy alias prevented by the existing guardrail). Skip if uncertain.

- [ ] **Step 4: Smoke-test**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/frontend
npm run dev    # vite dev-server (package.json confirms: "dev": "vite")
```

Open the admin UI; create a cnexus node with `proxy_url = socks5h://proxy.example.com:1080`; submit; reload the page; verify the value persists and the form shows it on subsequent edit.

- [ ] **Step 5: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add frontend/src/views/admin/ProxyNodes.vue
git commit -m "feat(web): widen proxy_url placeholder hint to all four schemes (Phase 4 §3.8 contract)"
```

---

### Task 4b.3: PR-4b verification gate

- [ ] **Step 1: `go test ./modules/proxynode/... -race -v`** — clean
- [ ] **Step 2: `go build ./...`** — clean
- [ ] **Step 3: `go vet ./modules/proxynode/...` + `gofmt -l modules/proxynode/`** — empty output
- [ ] **Step 4: Frontend build** — `npm run build` clean (no TS errors etc.)
- [ ] **Step 5: Open the MR**

MR title: `feat(proxynode,web): Phase 4b — surface proxy_url on cnexus nodes (Aether-parity)`

MR description:
- Background: Phase 4 of `ailinkmax-gateway` adds SOCKS5 egress on `gw-tunnel-server`; this MR widens the admin UI to match.
- Audit finding (Step 1): note whether a `protocol == aether` gate existed and was removed, or whether no gate ever existed (validation was already protocol-agnostic).
- Test summary from Steps 1-4.
- Cross-link: spec §3.8 + Phase 4 plan §PR 4b.

---

## Task F1: Open the integrated MR to `main`

> **Replaces Phase 3 Task F3.** This is now the only MR opened against `main` in the `ailinkmax-gateway` repo for the entire tunnel-compat work — it carries Phase 1 + Phase 2 + Phase 3 + Phase 4 commits.

- [ ] **Step 1: Push the shared branch**

```bash
git push origin feature/tunnel-compat
```

- [ ] **Step 2: Open the MR**

MR title: `feat(tunnel): Cnexus ↔ Aether compatibility (Phases 1+2+3+4, full)`

MR description must include:
- One-paragraph summary linking to the design spec.
- Commit-list grouped by phase:
  - Phase 1: Stage 0 (`--features tunnel` removal), Stage 1 (TunnelProtocol enum + cnexus reorg + Transport trait + migration), Stage tunnel-e2e (3 cnexus scenarios).
  - Phase 2: 2a (Aether wire), 2b (InboundListener + integration), 2c (e2e Aether activation + 9 scenarios).
  - Phase 3: 3a (mixed-pool e2e + dep script + final docs).
  - Phase 4: 4a (gw-tunnel-server SOCKS5 egress + 2 scenarios + spec/issues + README + ROADMAP closeout via 4a.10).
- Captured test summaries:
  - `cargo-remote-ctl test --workspace` final 5 lines.
  - `cargo-remote-ctl clippy --workspace --all-targets -- -D warnings` clean.
  - `bash scripts/check-invariants.sh` clean.
  - `make tunnel-e2e` summary (12 passed).
  - `make tunnel-e2e-baseline` summary (1 passed).
  - `make tunnel-e2e-mixed` summary (11 passed).
  - `make tunnel-e2e-socks5` summary (2 passed).
  - `make e2e` summary (25 scenarios, unchanged from main).
  - Total **26 scenarios across the tunnel-e2e suite** (3 + 8 + 1 + 11 + 2 + 1 = 26).
- Captured `bash scripts/check-tunnel-deps.sh` output (informational; flagged edges enumerated).
- Captured `tests/tunnel-e2e/docs/cross-protocol-matrix.md` as a collapsible section.
- Cross-repo links:
  - Phase 1 G1/G2 Go MR (merged).
  - Phase 2 G3 Go MR (merged).
  - Phase 3 G4 Go MR (merged).
  - Phase 4 4b Go MR (merged before this Rust MR is reviewable).
- ADR pointer: `docs/design/2026-06-15-tunnel-default-compile-decision.md`.
- New ops doc pointer: `docs/issues/2026-06-15-socks5-egress-deployment-coupling.md`.

- [ ] **Step 3: After MR review feedback**

Address each comment by adding new commits on the branch. Do NOT rebase / force-push during review unless the reviewer explicitly requests it.

- [ ] **Step 4: Merge**

After approval, merge via the project's standard merge mechanism. Do not squash unless the reviewer asks — the commit history is the audit trail of the work breakdown across the four phases.

---

## Task F2: Post-merge cleanup

- [ ] **Step 1: Delete the local feature branch**

```bash
git checkout main
git pull origin main
git branch -D feature/tunnel-compat
```

- [ ] **Step 2: Update memory entry**

Mark `tunnel-compat-cnexus-aether.md` memory entry as resolved (or annotate with merge SHA).

- [ ] **Step 3: Smoke-check on `main`**

```bash
cargo-remote-ctl test --workspace
make e2e
```

Both expected clean.

---

## Self-Review

After PR 4a (SOCKS5 egress) + PR 4b (Go frontend widening) are complete, then Task F1 opens the **integrated** MR and Task F2 cleans up:

1. **Spec coverage (Phase 4 only):**
   - §3.8 Node-level upstream egress proxy — Tasks 4a.1 (`EgressMode` enum), 4a.2 (`dial_via_egress` dispatcher), 4a.3 (`--proxy-url` CLI + `PROXY_URL` env + parser + reachability probe).
   - §3.8 frontend contract — Task 4b.2 (Go frontend widens `proxy_url` to both protocols).
   - §7 Phase 4 PR breakdown — Tasks 4a.1-4a.9 (PR 4a) + Tasks 4b.1-4b.3 (PR 4b) + F1 + F2.
   - §7 Phase 4 verification gates — Tasks 4a.9 + 4b.3.
   - §10 glossary `proxy_url` / `EgressMode` — Task 4a.8 (docs/spec/09-tunnel.md update).
   - Out of scope per spec §7 Phase 4: HTTP-CONNECT, per-stream proxy selection, SOCKS5 pool reuse, hub-push `proxy_url`. None of these are implemented; all are filed as ROADMAP candidates in Phase 3 Task F2's ROADMAP closeout (extend if needed at Task F1 description-time).

2. **Placeholder scan:**
   - No "TBD"/"TODO" in concrete code blocks.
   - Task 4a.1's "match existing struct/impl shape (grep first)" — documented adjustment-point, not a placeholder.
   - Task 4a.2's "plumb `cfg.egress` into `relay_stream` matching the crate's argument-passing style" — explicit anti-reinvention guard.
   - Task 4a.4's "if `serjs/go-socks5-proxy` is unavailable, swap for `wernight/dante`" — documented fallback with the substitution rule.
   - Task 4a.5's "the protocol-parity test starts a second aether-proxy mid-test" — documented; allows up to 3 min on cold cache.
   - Task 4b.1's "if no gate exists, no code change is needed" — explicit "skip if absent" instruction.

3. **Type consistency:**
   - `EgressMode` field names (`proxy_addr`, `auth`, `resolve_at_proxy`) consistent across enum def (Task 4a.1), `dial_via_egress` match arms (Task 4a.2), `parse_egress_url` returns (Task 4a.3), and tests.
   - URL scheme strings `socks5` / `socks5h` consistent across parser (4a.3), tests (4a.3 + Go 4b.1), spec §3.8, ops gotcha doc, README.
   - `proxy_url` JSON key consistent: Go side `payload.proxy_url` (entity), Rust side reads from CLI/env (no JSON), e2e seed SQL uses `'proxy_url'` JSONB key, frontend form binds to `proxy_url`.
   - `--proxy-url` CLI flag name consistent across `main.rs` (4a.3), Dockerfile / compose (4a.4), tests (4a.7 logs), README (4a.8), `--help` text (4a.3).
   - `PROXY_URL` env var name consistent: CLI fallback (4a.3), Dockerfile usage (4a.4 `cnexus-tunnel-server-socks5` command), ops gotcha doc (4a.8).

4. **Cross-repo coordination invariants:**
   - PR 4b in `AILinkMax-Go` lands before the Rust integrated MR (Task F1). The Rust shared-branch `--proxy-url` flag works without the UI widening (operators can hand-set the DB value), so there is no pre-merge ordering hazard.
   - The DB `proxy_url` value is **informational** at the gateway. The gateway never reads it for routing decisions; only the tunnel-server's deployment config matters. This decoupling means PR 4a is functionally complete on its own; PR 4b adds operator ergonomics.
   - No Aether-proxy modifications. The migration claim is that an existing `aether-proxy` config can be lifted as-is into a `gw-tunnel-server` deployment via matching `--proxy-url`.

5. **Single-branch / single-MR consistency check:**
   - Phase 4 commits append to the shared `feature/tunnel-compat` branch alongside Phases 1+2+3.
   - Phase 3 Task F3 (the originally-planned final MR) is **deferred** via the in-body errata at Phase 3 F3 (wrapped in a `<details>` block + "STOP — DO NOT EXECUTE" header so a linear reader cannot accidentally run it). The new final task is Phase 4 Task F1.
   - **Within Phase 4's own scope, no code is added then removed.** `EgressMode::Direct` default preserves existing behaviour; the dispatcher replaces the single dial site without changing any other path; the `tokio-socks` dep is permanent; the `socks5-egress` compose service is profile-gated (default off).
   - Cross-phase coordination (Errata #2 acknowledgment, post-audit): the original draft framed Task 4b.2 as "remove the aether-only conditional Phase 3 PR 3b added". Audit revealed Phase 3 G4 never added such a conditional. Task 4b.2 was reframed as a **regression guard + placeholder-hint widening**, NOT an add-then-remove. There is therefore no cross-phase add-then-remove pattern; the work is purely cumulative.
   - Phase 1's `--features tunnel` removal, Phase 2's `aether/` submodule + `StreamTarget::Http`, Phase 3's mixed-pool e2e + dep script + as-built docs — all stay in effect through Phase 4.
   - Control-plane JSON `features.tunnel` field stays `true` (per Phase 1 decision A). Phase 4 does not add or remove any control-plane fields; `proxy_url` is a node-row payload key, not a control-plane feature.
   - ROADMAP closeout: Phase 4 Task 4a.10 strikes the original SOCKS5 candidate (added by Phase 3 Task F2) as resolved and adds HTTP-CONNECT as a new future candidate — this is a section update, not an add-then-remove.

---

Plan complete and saved to `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-4.md`. Execution model continues from the Phase 1/2/3 chosen approach (Subagent-Driven or Inline).

**This is now the final implementation plan for the Cnexus ↔ Aether tunnel compatibility work.** After Task F1 merges, the work is complete. Phase 5+ candidates (HTTP-CONNECT egress, SOCKS5 connection-pool reuse, hub-pushed `proxy_url`) remain in `docs/ROADMAP.md` for future scheduling.
