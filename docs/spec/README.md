# Gateway Spec Standard (as-built)

Authoritative, code-mirroring design standard. Each spec describes **only what exists in `crates/`**; anything unbuilt or divergent lives in [`../ROADMAP.md`](../ROADMAP.md). Cross-system overview is in [`../architecture.md`](../architecture.md); request lifecycle in [`../workflow.md`](../workflow.md).

| Spec | Backing crate(s) | Status |
|------|------------------|--------|
| [01-protocol-codec](01-protocol-codec.md) | gw-codec, gw-codec-macros | built; kiro/S5 + CLI metrics in ROADMAP |
| [02-routing](02-routing.md) | gw-routing, gw-capability | 8 strategies built; multi-score/affinity/stream-probe in ROADMAP |
| [03-key-health](03-key-health.md) | gw-health | K-A built; K-B/K-C/K-D in ROADMAP |
| [04-rate-limiting](04-rate-limiting.md) | gw-auth | per-user + adaptive + IP built; per-key RPM gate in ROADMAP |
| [05-auth-permissions](05-auth-permissions.md) | gw-auth, gw-proxy | complete |
| [06-billing-ledger](06-billing-ledger.md) | gw-ledger, gw-billing | built; C-beta/gamma/delta + lease fixes in ROADMAP |
| [07-vendor-integration](07-vendor-integration.md) | gw-adapter, gw-egress-impersonate, gw-core::impersonate | inline + Go-delegated OAuth + egress built; adapter layer in ROADMAP |
| [08-async-tasks](08-async-tasks.md) | gw-task, gw-task-runtime, gw-task-worker | structure built (default-off); settle billing + poll-loop in ROADMAP |
| [09-tunnel](09-tunnel.md) | gw-tunnel-client | feature-gated; off in shipped binary, see ROADMAP |

> History: this standard replaced 51 dated specs + 24 dated plans on 2026-06-08 (see git history and `CONSOLIDATION-DESIGN.md`).
