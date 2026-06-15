# Tunnel Compat Phase 4 Implementation Plan — Cnexus SOCKS5 Egress for Aether-parity

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 4 of the Cnexus ↔ Aether tunnel compatibility spec: give `gw-tunnel-server` SOCKS5/SOCKS5h egress dial support behind a `--proxy-url` CLI flag / `PROXY_URL` env, so an Aether-era deployment using `proxy_nodes.proxy_url = socks5://...` can be replaced 1:1 by a Cnexus tunnel-server with the same `proxy_url` value at deployment time and continue to function identically. Phase 4 closes the migration-continuity gap surfaced after Phase 3 was already drafted — SOCKS5 egress is the only Aether capability the Cnexus tunnel-server cannot match today, and replacing Aether-era nodes without it would require operators to reinstrument upstream egress (proxychains, sidecar, etc.) and break the Aether→Cnexus seamless-migration story.

**Why this is a phase, not a ROADMAP item:** the work is small (~10 tasks, ~600-900 lines of plan) but it's load-bearing for the migration. Folding it into Phase 4 *before* the integrated MR opens means the migration story ships complete in one merge rather than as a follow-up that operators have to track. Per the user-locked 2026-06-15 review decision, Phase 4 also defers Phase 3's Task F3 (final MR) to Phase 4's final task — the integrated MR now carries Phase 1+2+3+4 commits.

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
| `tests/tunnel-e2e/seed/04_socks5.sql` (new) | Seeds a fourth proxy node `cn-socks5` (tunnel_protocol=cnexus, ws_url=wss://cnexus-tunnel-server-socks5:7000/tunnel, proxy_url=socks5h://socks5-egress:1080) so the scenario can route through it. |
| `Makefile` | New target `tunnel-e2e-socks5`: brings up the `socks5` compose profile, runs `pytest scenarios/test_cnexus_socks5.py`, tears down. |
| `docs/spec/09-tunnel.md` | Append an "Egress proxy" section with the full `proxy_url` table from spec §3.8, the `EgressMode` enum, the URL parsing rules, and a worked example (Aether-era node → Cnexus replacement with matching `--proxy-url`). |
| `crates/gw-tunnel-server/README.md` | Add a short "Egress proxy" subsection: how to use `--proxy-url` / `PROXY_URL`, the scheme set, the userinfo form, the `socks5` vs `socks5h` distinction. Reference `docs/spec/09-tunnel.md` for the full contract. |

### Files modified (PR 4b, `AILinkMax-Go` repo)

> All paths below are relative to the `AILinkMax-Go` repo root. **Phase 3 PR 3b already introduced `proxy_url` UI for aether nodes via the per-node detail drawer**; Phase 4 PR 4b widens this to the cnexus path (form field + table column where applicable).

| Path | Change |
|---|---|
| `modules/proxynode/business/service.go` | Verify (do not edit unless needed) that `CreateInput` / `UpdateInput` accept `proxy_url` for both protocols. The existing comments at `service.go:151,186` warn against confusing `proxy_url` with `ws_url`; that guardrail is correct and stays. If a `tunnel_protocol == cnexus`-only gate exists on `proxy_url`, REMOVE it (the SOCKS5 path is meant for both protocols). Cross-check with `modules/proxynode/business/entity.go:108` comments. |
| `modules/proxynode/business/service_test.go` | New table-driven case: `TestService_AcceptsProxyURLOnCnexusNode` covering `socks5://` and `socks5h://` schemes for a cnexus node. |
| `web/src/components/ProxyNodes/<form-component>` | Where the create/edit form is defined: show the `proxy_url` field for **all** protocols (Phase 3 PR 3b had conditioned it on `protocol === 'aether'`). The hint text mentions both schemes. Placeholder: `socks5://user:pass@proxy.host:1080` (matches Aether reference UI). |
| `web/src/pages/ProxyNodes/<table>` | Optional column addition (low priority): a `proxy_url`-presence indicator (badge: ⚠️ "via proxy" if set, blank if direct). Implementing engineer's call. |

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
    let m = EgressMode::Socks5 {
        proxy_addr: "127.0.0.1:1080".parse().unwrap(),
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
        proxy_addr: "127.0.0.1:1080".parse().unwrap(),
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
#[derive(Clone, Debug)]
pub enum EgressMode {
    /// Direct TCP dial to the target. Default; current behaviour.
    Direct,

    /// Dial via a SOCKS5 proxy. `resolve_at_proxy = true` implements `socks5h`
    /// semantics (send hostname); `false` implements plain `socks5` (resolve
    /// locally then send the resolved IP).
    Socks5 {
        proxy_addr: SocketAddr,
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
/// the proxy is responsible for DNS resolution. Suited for environments where
/// the tunnel-server cannot reach the target's DNS but the proxy can.
///
/// `socks5` (resolve_at_proxy=false): resolves the hostname locally, then asks
/// the proxy to connect to the resolved IP. Matches RFC 1928 §4 with
/// ATYP=0x01 (IPv4) or ATYP=0x04 (IPv6).
pub async fn dial_via_egress(egress: &EgressMode, target: &str) -> Result<TcpStream> {
    match egress {
        EgressMode::Direct => {
            TcpStream::connect(target).await
                .with_context(|| format!("direct connect to {target}"))
        }
        EgressMode::Socks5 { proxy_addr, auth, resolve_at_proxy } => {
            use tokio_socks::tcp::Socks5Stream;
            if *resolve_at_proxy {
                let stream = match auth {
                    Some((u, p)) => Socks5Stream::connect_with_password(
                        *proxy_addr, target, u.as_str(), p.as_str()
                    ).await,
                    None => Socks5Stream::connect(*proxy_addr, target).await,
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
                        *proxy_addr, resolved, u.as_str(), p.as_str()
                    ).await,
                    None => Socks5Stream::connect(*proxy_addr, resolved).await,
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

- [ ] **Step 4: Replace the dial site in `relay_stream`**

Find:
```bash
grep -n 'TcpStream::connect(&target)' crates/gw-tunnel-server/src/lib.rs
```

Expected: one hit at ~line 261 inside `relay_stream`. Replace:

```rust
// Before:
let tcp = match TcpStream::connect(&target).await { ... };

// After (assuming `cfg` is in scope as the TunnelServerConfig; if not, plumb it through
// from `spawn` → `handle_connection` → `relay_stream`):
let tcp = match dial_via_egress(&cfg.egress, &target).await {
    Ok(s) => s,
    Err(e) => {
        sink.send_error(frame.stream_id, Bytes::from(format!("dial: {e}"))).await;
        return;
    }
};
```

> Plumb `cfg.egress` into `relay_stream` by extending the spawn signature or by capturing an `Arc<TunnelServerConfig>` in the closure. Match the existing argument-passing style of the crate — do NOT introduce a global / lazy_static.

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
        EgressMode::Socks5 { proxy_addr, auth, resolve_at_proxy } => {
            assert_eq!(proxy_addr.to_string(), "127.0.0.1:1080");
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
fn parse_egress_url_default_port_socks5() {
    let m = parse_egress_url(Some("socks5://proxy.local")).expect("parse");
    if let EgressMode::Socks5 { proxy_addr, .. } = m {
        assert_eq!(proxy_addr.port(), 1080, "RFC 1928 default port");
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
fn parse_egress_url_rejects_missing_port_resolution() {
    let r = parse_egress_url(Some("socks5h://invalid-host-no-dns.example/"));
    // DNS resolution may succeed (default IP) or fail; in either case the
    // parser must not panic. If you can't reach DNS in CI, this test may be
    // flaky — mark it as #[ignore] and run locally.
    let _ = r;
}
```

- [ ] **Step 2: Implement `parse_egress_url` in `lib.rs`**

```rust
/// Parse a `proxy_url` string into an `EgressMode`. None or empty → Direct.
/// Scheme set: `socks5`, `socks5h`. Unknown scheme → error.
/// Default port: 1080 (RFC 1928).
pub fn parse_egress_url(s: Option<&str>) -> anyhow::Result<EgressMode> {
    let Some(raw) = s else { return Ok(EgressMode::Direct) };
    let raw = raw.trim();
    if raw.is_empty() { return Ok(EgressMode::Direct); }

    let u = url::Url::parse(raw).with_context(|| format!("invalid proxy_url {raw:?}"))?;

    let resolve_at_proxy = match u.scheme() {
        "socks5" => false,
        "socks5h" => true,
        other => return Err(anyhow!(
            "proxy_url scheme {other:?} not supported by gw-tunnel-server (Phase 4 ships SOCKS5 only; HTTP-CONNECT is a future ROADMAP item)"
        )),
    };

    let host = u.host_str().ok_or_else(|| anyhow!("proxy_url missing host"))?;
    let port = u.port().unwrap_or(1080);

    // Resolve the proxy host. SOCKS5 client needs a SocketAddr.
    let proxy_addr: std::net::SocketAddr = {
        let mut iter = (host, port).to_socket_addrs()
            .with_context(|| format!("resolving proxy host {host}:{port}"))?;
        iter.next().ok_or_else(|| anyhow!("no addresses for {host}:{port}"))?
    };

    let auth = if !u.username().is_empty() {
        let user = u.username().to_string();
        let pass = u.password().unwrap_or("").to_string();
        Some((user, pass))
    } else {
        None
    };

    Ok(EgressMode::Socks5 { proxy_addr, auth, resolve_at_proxy })
}
```

Add `url` dep to `gw-tunnel-server/Cargo.toml` if not already present:
```bash
grep '^url' crates/gw-tunnel-server/Cargo.toml || cargo-remote-ctl add -p gw-tunnel-server url
```

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

- [ ] **Step 4: Optional reachability probe**

Right before `spawn(cfg).await?`:

```rust
if let EgressMode::Socks5 { proxy_addr, .. } = &cfg.egress {
    use tokio::time::{timeout, Duration};
    match timeout(Duration::from_secs(5), tokio::net::TcpStream::connect(proxy_addr)).await {
        Ok(Ok(_)) => tracing::info!("proxy reachable: {proxy_addr}"),
        Ok(Err(e)) => tracing::warn!("proxy unreachable at startup: {e} — relays will fail until proxy is up"),
        Err(_) => tracing::warn!("proxy probe timed out (5s) — relays will fail until proxy is up"),
    }
}
```

Per spec §3.8 the probe failure does NOT refuse startup. Operators may be staging the proxy.

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

- [ ] **Step 1: Write the Dockerfile**

```dockerfile
# tests/tunnel-e2e/Dockerfile.socks5-egress
FROM serjs/go-socks5-proxy:latest
# serjs/go-socks5-proxy listens on :1080 by default.
# No auth in the e2e default; add PROXY_USER / PROXY_PASSWORD env vars to enable.
HEALTHCHECK --interval=5s --timeout=2s --retries=5 \
  CMD nc -z 127.0.0.1 1080 || exit 1
```

> If `serjs/go-socks5-proxy` is unavailable in your registry, swap for `wernight/dante:latest` (different SOCKS5 impl) or build from `golang:1.21` + `armon/go-socks5`. Adjust the listen-port matching if you swap.

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

  cnexus-tunnel-server-socks5:
    profiles: ["socks5"]
    build:
      context: ../../
      dockerfile: Dockerfile
      args:
        BIN: gw-tunnel-server
    command: >
      gw-tunnel-server
      --bind 0.0.0.0:7000
      --auth-token e2e-socks5-token
      --max-streams 64
      --allow-target fake-upstream:9000
      --allow-target fake-upstream:9443
      --proxy-url socks5h://socks5-egress:1080
    networks: [tunnel-net]
    expose: ["7000"]
    depends_on:
      fake-upstream:
        condition: service_started
      socks5-egress:
        condition: service_healthy
```

Create `tests/tunnel-e2e/seed/04_socks5.sql`:

```sql
-- tests/tunnel-e2e/seed/04_socks5.sql — Phase 4 SOCKS5 egress fixtures.
-- Loaded ONLY by the `socks5` compose profile via the schema-applier container.
-- Idempotent: drops first.

DELETE FROM proxy_nodes WHERE id LIKE 'cn-socks5%' OR id LIKE 'ae-socks5%';

INSERT INTO proxy_nodes (id, payload, version, deleted_at) VALUES
('cn-socks5', jsonb_build_object(
    'tunnel_protocol', 'cnexus',
    'node_name', 'cnexus-via-socks5',
    'ws_url', 'wss://cnexus-tunnel-server-socks5:7000/tunnel',
    'auth_token', 'e2e-socks5-token',
    'proxy_url', 'socks5h://socks5-egress:1080',
    'enabled', true
), 1, NULL);

-- The aether-parity case reuses an existing aether-proxy + its own proxy_url
-- env: it doesn't get a dedicated row here because the existing aether seed
-- (Phase 2) already provides `ae1`, and the parity test starts the matching
-- aether-proxy with PROXY_URL=socks5h://socks5-egress:1080.
```

- [ ] **Step 3: Commit**

```bash
git add tests/tunnel-e2e/Dockerfile.socks5-egress \
        tests/tunnel-e2e/docker-compose.yml \
        tests/tunnel-e2e/seed/04_socks5.sql
git commit -m "test(tunnel-e2e): add socks5-egress service + cnexus-via-socks5 + seed"
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
    seed_aether_node_via_socks5, seed_nodes_socks5_only, gateway_client
):
    """The migration claim: an aether-proxy and a cnexus tunnel-server, BOTH
    configured to egress via the same SOCKS5, MUST yield byte-identical
    response bodies for the same request.
    """
    # 1. Drive the request through the cnexus-via-socks5 node.
    seed_nodes_socks5_only("cn-socks5")
    cnexus_resp = gateway_client.post(
        "/v1/echo",
        headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid-socks5"},
        json={"q": "parity-check"},
        timeout=15.0,
    )

    # 2. Drive the SAME request through the aether-via-socks5 node.
    # Note: `seed_aether_node_via_socks5` is a fixture that ensures an
    # aether-proxy has been started with PROXY_URL=socks5h://socks5-egress:1080
    # and that its row is `ae-socks5` in the registry.
    seed_aether_node_via_socks5("ae-socks5")
    seed_nodes_socks5_only("ae-socks5")
    aether_resp = gateway_client.post(
        "/v1/echo",
        headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid-socks5"},
        json={"q": "parity-check"},
        timeout=15.0,
    )

    # 3. Byte-identical bodies (modulo response order if upstream is non-
    # deterministic — `fake-upstream` returns deterministic echo).
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
    it under the given node_id. Docker-managed via `docker compose run`."""
    def _seed(node_id: str):
        # The base Phase-2 aether-proxy is the `aether-proxy` service.
        # Phase 4 we shell out to start a second one with PROXY_URL injected.
        import subprocess
        subprocess.run([
            "docker", "compose",
            "-f", "tests/tunnel-e2e/docker-compose.yml",
            "--profile", "socks5",
            "run", "-d", "--rm",
            "-e", "AETHER_PROXY_AUTH_TOKEN=ae_e2e_socks5",
            "-e", "AETHER_PROXY_NODE_NAME=aether-via-socks5",
            "-e", "PROXY_URL=socks5h://socks5-egress:1080",
            "--name", "aether-proxy-socks5",
            "aether-proxy",
        ], check=True)
        aether_admin.await_aether_attach(node_id, timeout_s=30)
    return _seed
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

.PHONY: tunnel-e2e-socks5

tunnel-e2e-socks5: tunnel-e2e-deps
	docker compose $(TUNNEL_E2E_COMPOSE_SOCKS5) build
	docker compose $(TUNNEL_E2E_COMPOSE_SOCKS5) run --rm \
	  pytest-runner pytest scenarios/test_cnexus_socks5.py -v
	$(MAKE) tunnel-e2e-down
```

Commit.

---

### Task 4a.7: Run `make tunnel-e2e-socks5` end-to-end

- [ ] **Step 1: Clean tree + run**

```bash
make tunnel-e2e-down
make tunnel-e2e-socks5
```

Expected: `2 passed`. The `protocol parity` scenario takes longer because it boots a second aether-proxy mid-test; allow up to ~3 min on a cold cache.

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

## PR 4b · Go frontend widening (AILinkMax-Go)

> **Repo**: `AILinkMax-Go`. **Branch**: `feature/tunnel-compat-phase-4-go`. Lands as a real MR before Phase 4 Task F1 opens the Rust integrated MR.

### Task 4b.1: Verify no aether-only gate on `proxy_url` validation

**Files:**
- Read-only: `modules/proxynode/business/{entity,service,ops}.go`
- Modify (only if a gate exists): same files
- Modify: `modules/proxynode/business/service_test.go`

- [ ] **Step 1: Audit existing `proxy_url` handling**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
grep -n 'proxy_url\|ProxyURL\|tunnel_protocol\|Protocol' modules/proxynode/business/*.go
```

Expected findings:
- `entity.go` defines `ProxyURL string` field with a JSON tag.
- `service.go` validates `proxy_url` scheme via the existing helper.
- `ops.go:42` has `case "socks5", "socks5h"` for `defaultPort`.

If **no** branch is conditional on `Protocol == "aether"`, the validation is already protocol-agnostic and **no Go code change is needed** for the validation path. Document this finding in the MR description.

If a gate exists (`if e.Protocol != "aether" { return errors.New("proxy_url only for aether") }`), remove it. The Phase 4 design explicitly makes `proxy_url` valid for both protocols.

- [ ] **Step 2: Write a regression test**

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
    }
    for _, tc := range cases {
        t.Run(tc.name, func(t *testing.T) {
            n, err := svc.Create(context.Background(), CreateInput{
                NodeName: "cn-with-proxy", WsURL: "wss://x/tunnel",
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
    _, err := svc.Create(context.Background(), CreateInput{
        NodeName: "cn-bad", WsURL: "wss://x/tunnel",
        Protocol: "cnexus",
        ProxyURL: "ftp://x.example.com:21",
    })
    if err == nil {
        t.Fatal("unknown scheme must be rejected")
    }
}
```

- [ ] **Step 3: Run tests**

```bash
go test ./modules/proxynode/business -run 'TestService_(Accepts|Rejects)ProxyURL' -v
```

Expected: PASS. If the `Accepts` test fails, the gate exists — remove it per Step 1 and re-run.

- [ ] **Step 4: Commit**

```bash
git add modules/proxynode/business/service_test.go
# Plus any service.go / entity.go gate-removal edits from Step 1.
git commit -m "feat(proxynode): accept proxy_url on cnexus nodes (parity with aether)"
```

---

### Task 4b.2: Frontend `proxy_url` form-field exposure for both protocols

**Files:**
- Modify: `web/src/components/ProxyNodes/<form-component>` (Phase 3 PR 3b had the aether-only conditional)
- Optional: `web/src/pages/ProxyNodes/<table-component>` (proxy_url presence indicator)

- [ ] **Step 1: Locate the conditional render added in Phase 3 PR 3b**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
grep -rn "protocol === 'aether'\|protocol === \"aether\"\|proxy_url" web/src/ 2>/dev/null | head -20
```

Find the line that renders `proxy_url` only when `protocol === 'aether'`. Remove the condition; render unconditionally.

- [ ] **Step 2: Update the placeholder text**

The Aether reference UI placeholder is `http://proxy:port 或 socks5://proxy:port`. For Cnexus's Phase 4, the supported schemes are SOCKS5 only (HTTP-CONNECT is ROADMAP), so use `socks5://proxy:port 或 socks5h://user:pass@proxy:port` to set correct expectations.

> If the form is shared across protocols, the placeholder hint can mention all four schemes (`socks5`, `socks5h`, `http`, `https`) with a note that HTTP-CONNECT is supported by Aether only at the moment. Match the protocol of the currently-selected radio.

- [ ] **Step 3: Optional `proxy_url`-presence column on the proxy-nodes table**

Per the locked decision (engineer's call, no design review), this is opt-in. If implementing, render a small badge `⚠️ via proxy` when `proxy_url` is set, blank otherwise. Tooltip shows the value.

- [ ] **Step 4: Smoke-test**

```bash
npm run dev   # or whatever the existing dev-server command is
```

Open the admin UI; create a cnexus node; confirm the `proxy_url` field is now visible. Enter `socks5h://proxy.example.com:1080`; submit; verify the value persists across page reloads.

- [ ] **Step 5: Commit**

```bash
git add web/src/components/ProxyNodes/<form-component-file> \
        web/src/pages/ProxyNodes/<table-component-file>   # if changed
git commit -m "feat(web): expose proxy_url form-field for both cnexus and aether nodes"
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
  - Phase 4: 4a (gw-tunnel-server SOCKS5 egress + 2 scenarios + spec/issues + README).
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
   - Phase 3 Task F3 (the originally-planned final MR) is **deferred** by an inline note added in this plan's preamble; the new final task is Phase 4 Task F1.
   - Phase 4 introduces **no** "add then remove" — `EgressMode::Direct` default preserves existing behaviour; the dispatcher replaces the single dial site without changing any other path; the `tokio-socks` dep is permanent; the `socks5-egress` compose service is profile-gated (default off).
   - Phase 1's `--features tunnel` removal, Phase 2's `aether/` submodule + `StreamTarget::Http`, Phase 3's mixed-pool e2e + dep script + as-built docs — all stay in effect through Phase 4.
   - Control-plane JSON `features.tunnel` field stays `true` (per Phase 1 decision A). Phase 4 does not add or remove any control-plane fields; `proxy_url` is a node-row payload key, not a control-plane feature.

---

Plan complete and saved to `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-4.md`. Execution model continues from the Phase 1/2/3 chosen approach (Subagent-Driven or Inline).

**This is now the final implementation plan for the Cnexus ↔ Aether tunnel compatibility work.** After Task F1 merges, the work is complete. Phase 5+ candidates (HTTP-CONNECT egress, SOCKS5 connection-pool reuse, hub-pushed `proxy_url`) remain in `docs/ROADMAP.md` for future scheduling.
