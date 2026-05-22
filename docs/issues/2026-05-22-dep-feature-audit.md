# Dependency-feature audit: silent feature-gate failures

**Filed:** 2026-05-22
**Severity:** Low (none blocking; #1 is a doc-vs-reality discrepancy)
**Status:** Open — cleanup grade

## Trigger

`pingora = "0.8"` was declared without any TLS-backend feature, which
silently activated pingora's `noop_tls` stub. Every
`HttpPeer { tls: true, .. }` upstream was dropped on the floor with no
loud failure mode. The fix (`features = ["rustls"]` on `pingora` +
`pingora-proxy`) then exposed the `aws-lc-rs` + `ring` `CryptoProvider`
conflict in `rustls 0.23`, patched by
`rustls::crypto::ring::default_provider().install_default()` at startup.

This audit greps for analogous patterns — features or deps enabled with
the right intent whose gated code path is unreachable.

## Findings

### 1. `tunnel` cargo feature is structurally dead at the data path

- `crates/gw-gateway/Cargo.toml:14-15` defines `tunnel = ["dep:gw-tunnel-client"]`.
- The only consumer of `gw-tunnel-client` is `crates/gw-gateway/src/lib.rs:43-44`
  doing `pub use gw_tunnel_client;`.
- Zero references to `WsTransport`, `resolve_tunnel`, `TunnelResolution`,
  or `gw_tunnel_client::` in `gw-proxy/`, `gw-routing/`, or
  `gw-gateway/src/main.rs`.
- CLAUDE.md advertises WebSocket tunnel transport behind
  `--features tunnel`. Building with the flag changes nothing at runtime
  — `gw-tunnel-client` is never invoked. Operators believe tunnels are
  active when they aren't. Wider than the wave-7 T6-A follow-up noted
  in CLAUDE.md.

### 2. `reqwest` declared workspace-wide but never `use`d in compiled code

- Workspace `Cargo.toml:72`; only `[dev-dependencies]` consumer is
  `crates/gw-gateway/Cargo.toml`.
- Source mentions are a doc-comment in `crates/gw-adapter/src/ctx.rs:6`
  and comments in `tests/e2e_full_flow.rs:6,140`. No `use reqwest`
  anywhere. Build cost only.

### 3. `sqlx` features `macros` + `migrate` enabled but unused

- Workspace `Cargo.toml:71`.
- `grep -rn 'sqlx::query!\|query_as!\|sqlx::migrate'` returns zero hits.
- CLAUDE.md §Toolchain notes "v1 uses runtime-checked `sqlx::query` /
  `sqlx::raw_sql` exclusively" and migrations run via `psql -f`.
- Fix: `features = ["postgres", "runtime-tokio-rustls", "json"]`.

### 4. `fred` features `partial-tracing` + `subscriber-client` enabled but unused

- Workspace `Cargo.toml:70`.
- `grep -rn 'SubscriberClient\|partial_tracing'` returns no Rust matches
  (`tracing::Subscriber` is unrelated). No pubsub call sites.
- Fix: `features = ["metrics"]`.

### 5. `hickory-resolver` pulls a second, older `rustls 0.21` / `tokio-rustls 0.24` into the dep graph

- Workspace `Cargo.toml:82` declares `hickory-resolver`; only
  `crates/gw-tunnel-client/src/safe_dns.rs` uses it.
- `Cargo.lock` confirms `rustls 0.21.12` coexists with `rustls 0.23.40`.
- Combined with #1: the entire DoH path is built but never reached at
  runtime in the default binary.
- Fix: bump hickory to a release on rustls 0.23, or move
  `hickory-resolver` behind `gw-tunnel-client/dns-over-https` as an
  optional dep.

### 6. `pingora` umbrella pulls `pingora-cache` (zstd, blake2, indexmap 1.x) but no code uses it

- `Cargo.lock` shows `pingora 0.8.0` → `pingora-cache`.
- `grep -rn 'pingora_cache\|pingora::cache'` returns zero hits.
- Inverse of the `noop_tls` pattern: enabled umbrella whose gated code
  path is unreachable.
- Fix:
  `pingora = { default-features = false, features = ["rustls"] }`, or
  depend on `pingora-core` + `pingora-proxy` directly.

## Closing note

Pattern to watch for: **"feature/dep enabled with the right intent but
the code path it gates is unreachable."** Silent failures look identical
to success in CI. None of #1–#6 are blocking; they are structural
cleanup with measurable build-time and binary-size wins, plus #1 is a
documentation-vs-reality discrepancy operators would notice.
