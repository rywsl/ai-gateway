# tunnel-e2e (Phase 1)

Local-only Docker e2e suite for the tunnel subsystem. It is not wired into CI;
run it on demand from the repo root.

## Phase 1 Scope

`scenarios/test_cnexus_protocol.py` runs three scenarios against a real
`gw-tunnel-server` container:

| Test | Assertion |
|---|---|
| `test_tunnel_no_node_returns_hard_error` | empty pool + `require_tunnel=true` returns `GW_TUNNEL_NO_NODE` |
| `test_tunnel_target_denied` | global target whitelist mismatch returns `GW_TUNNEL_TARGET_DENIED` |
| `test_tunnel_direct_vs_tunnel_byte_identity` | direct and cnexus-tunnel streaming responses match byte-for-byte |

Phase 2 adds Aether protocol services and scenarios. Phase 3 adds mixed-pool
stress coverage.

## Commands

```bash
make tunnel-e2e
make tunnel-e2e-up
make tunnel-e2e-down
make tunnel-e2e-logs
make tunnel-e2e-deps
```

First build is a cold Rust Docker build. Later runs should reuse Docker layers.

## Failure Triage

| Symptom | First check |
|---|---|
| pytest cannot connect to `ali-gateway` | `make tunnel-e2e-logs`, then inspect `ali-gateway` and `schema-applier` |
| migration failure | `docker compose -f tests/tunnel-e2e/docker-compose.yml --project-directory tests/tunnel-e2e logs schema-applier` |
| expected tunnel but got direct | confirm `GW_TUNNEL__ENABLED=true` and the seeded vendor key has `tunnel_preference.require_tunnel=true` |
| target-denied scenario fails | check `tests/tunnel-e2e/config/local.toml`; Phase 1 whitelists only `fake-upstream:9000` |
| byte mismatch | inspect fake-upstream logs to compare direct and tunneled requests |

## Aether Submodule

`aether-src/` is registered in `.gitmodules` with `update = none` so fresh
clones do not require private GitLab credentials during Phase 1. Phase 2 is the
first phase that consumes that source. The Phase 1 gitlink records
`a01f699502d1536a63daa1a8110afe9dbe1b362f` (`v0.0.7-15-ga01f6995`).

## Spec

`docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
