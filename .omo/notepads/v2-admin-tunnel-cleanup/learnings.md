# Learnings — v2-admin-tunnel-cleanup

## T2: /healthz/tunnel endpoint

- `HealthState` is constructed in `bootstrap()` (lib.rs) and passed to `run_health_server`. Adding tunnel fields required moving the health server spawn to AFTER the `#[cfg(feature = "tunnel")] let tunnel` block so the variable is in scope.
- `TunnelStack` fields (`transports`, `nodes`) are private. Added `health_snapshot()` pub method returning `Vec<TunnelNodeHealth>` to expose read-only health data without leaking internals. This touches `tunnel_stack.rs` minimally (the task said T3 owns it, but read-only accessor was unavoidable for T2).
- `escape_json_string` must be `#[cfg(feature = "tunnel")]` gated — without the feature, the `build_tunnel_healthz_body` function's tunnel block is stripped and the helper becomes dead code.
- `last_pong_age_secs` is `None` (null in JSON) until T3 adds the pong tracker to `ReconnectingTransport`.
- The codebase uses manual `format!`-based JSON throughout health.rs — no serde_json dependency.
- T1 mgmt routing: Pingora `request_filter` must return `Ok(false)` after a verified `/admin/*` management token to continue into `upstream_peer`; `Ok(true)` is the error/fully-handled short-circuit path. The Go control-plane target is boot-time config `[admin].upstream_url` / `GW_ADMIN__UPSTREAM_URL`.

## T3: transport-present gauge sampler + probe_loop (2026-05-24)

- `start_probe_loop` requires `self: &Arc<Self>` receiver since it clones the Arc for the spawned task. The `stack` variable in bootstrap is already `Arc<TunnelStack>`, so the call sites naturally.
- `ReconnectingTransport::current()` returns `Option<Arc<Transport>>` — `is_some()` is the presence test. No `last_pong_at` field exists yet on `ReconnectingTransport`, so `gw_tunnel_last_pong_age_seconds` is described in the catalog but not emitted until transport internals expose pong timing.
- Metric rename `gw_tunnel_transport_alive` → `gw_tunnel_transport_present` — only one reference existed (metrics_catalog.rs). The old metric was never emitted by any code path; the probe_loop is the first emitter.
- `dns` field on `TunnelStack` was `#[allow(dead_code)]` since T3-A. Removing the allow is safe because `start_probe_loop` holds `&Arc<Self>` which keeps `dns` alive; even though the current probe loop doesn't call dns methods directly, holding it is intentional for future DNS-based health checks.
- Bootstrap wiring: `_probe_handle` is an intentionally-leaked `JoinHandle<()>` — the spawned task lives for the process lifetime on the bootstrap runtime (same pattern as `_deduct_gc_handle`).
