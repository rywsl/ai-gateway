# Gateway Spec Standard (as-built)

Authoritative, code-mirroring design standard. Each spec describes **only what exists in `crates/`**; anything unbuilt or divergent lives in [`../ROADMAP.md`](../ROADMAP.md). Cross-system overview is in [`../architecture.md`](../architecture.md); request lifecycle in [`../workflow.md`](../workflow.md).

| Spec | Backing crate(s) | Status |
|------|------------------|--------|
| [01-protocol-codec](01-protocol-codec.md) | gw-codec, gw-codec-macros | built; kiro/S5 + CLI metrics in ROADMAP |
| [02-routing](02-routing.md) | gw-routing, gw-capability | 8 strategies built; multi-score/affinity/stream-probe in ROADMAP |
| [03-key-health](03-key-health.md) | gw-health, gw-adapter, gw-store | K-A/K-B/K-D built; K-C in ROADMAP |
| [04-rate-limiting](04-rate-limiting.md) | gw-auth | complete |
| [05-auth-permissions](05-auth-permissions.md) | gw-auth, gw-proxy | complete |
| [06-billing-ledger](06-billing-ledger.md) | gw-ledger, gw-billing | complete |
| [07-vendor-integration](07-vendor-integration.md) | gw-adapter, gw-egress-impersonate, gw-core::impersonate | inline outbound + Go-delegated OAuth + egress built; adapter error analyzers built; outbound adapter layer in ROADMAP |
| [08-async-tasks](08-async-tasks.md) | gw-task, gw-task-runtime, gw-task-worker | default-off; Redis freeze/settle + submit-poll built; codec render in ROADMAP |
| [09-tunnel](09-tunnel.md) | gw-tunnel-client, gw-tunnel-server, gw-proxy | owned tunnel bridge built; split artifacts (`latest` / `latest-tunnel`) recorded |

> History: this standard replaced 51 dated specs + 24 dated plans on 2026-06-08 (see git history and `CONSOLIDATION-DESIGN.md`).
