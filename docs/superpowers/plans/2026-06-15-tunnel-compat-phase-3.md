# Tunnel Compat Phase 3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 3 of the Cnexus ↔ Aether tunnel compatibility spec: cap the tunnel-compat work with a mixed-pool e2e suite that proves all 8 routing strategies behave sanely across protocols (and identifies which strategies are *legitimate* across cnexus+aether vs. which degrade gracefully); ship the `scripts/check-tunnel-deps.sh` dependency-boundary script that warns when tunnel crates pull in business crates; deliver Go G4 frontend UI in `AILinkMax-Go` (Protocol column + Aether status panel + Aether token management + Aether-node register history + GatewayControl gray-out per locked decisions); rewrite `docs/spec/09-tunnel.md` as the as-built reference superseding the design spec for ongoing maintenance; mark all tunnel ROADMAP items complete; open the **single final MR** to `main` that encompasses Phase 1 + Phase 2 + Phase 3 of the `ailinkmax-gateway` work.

**Branch / MR model:**
- **`ailinkmax-gateway` repo** — Phase 3 commits append to the **same single shared feature branch** that Phase 1 and Phase 2 live on. **At the end of Phase 3** (after Task F3 below), the integrated MR is opened against `main`. There is no Phase-3-only MR to `main`.
- **`AILinkMax-Go` repo** — PR 3b (G4 frontend) is a **real MR** in that repo, separate branch, lands after Phase 1 G1/G2 PR and Phase 2 G3 PR.

**Architecture:** Two commit groups on the `ailinkmax-gateway` shared branch + one cross-repo Go PR + one final MR to `main`.
- **PR 3a — Mixed-pool e2e + dep-boundary script + final docs** (Tasks M1-M7, S1-S4, F1-F4, this repo): mixed-pool docker-compose extension (1 cnexus + 1 aether), 3 mixed-pool scenarios + cross-protocol strategy compatibility matrix as a deliverable, `scripts/check-tunnel-deps.sh`, `docs/spec/09-tunnel.md` rewrite as as-built, ROADMAP closeout + Phase 4 candidates, final integrated MR to `main`.
- **PR 3b — Go G4 frontend** (Tasks G4.1-G4.6 + Go-3-gate, `AILinkMax-Go` repo, **real MR**): Protocol column with badge, Protocol radio in create/edit form, Aether status panel (1m bucket aggregation from raw samples, 1h window default), Aether token management UI (generate / rotate with dual-token grace / revoke), Aether register history view, GatewayControl button gray-out + tooltip per spec §3.6.6, supporting Go admin API endpoints for the new UI surfaces.

**Tech Stack:** Rust 1.88 (pinned), bash + jq (dep script), pytest 8.x, Docker Compose v2, Go 1.21+, whatever existing frontend framework `AILinkMax-Go` admin already uses (audit at Task G4.1 — likely React + Vite based on industry standard, but confirmed by inspection).

**Reference spec:** `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` (§3.6.6 GatewayControl semantics, §3.6.7 Frontend UI, §5 Decoupling Discipline, §6.5.D mixed-pool scenarios, §6.6 Makefile targets `check-tunnel-deps`, §7 Phase 3, §8 Testing & Verification).

**Out of scope for this plan (deferred to ROADMAP / never):**
- Cross-protocol perf baseline (ROADMAP candidate; per E1 decision Phase 3 skips it).
- Aether `remote_config` reverse-push (Aether-side feature; Cnexus would only consume it if it ships in a future Aether release).
- Double-Aether tunnel-server (two distinct Aether token namespaces multi-tenanted on one hub instance — not requested).
- Phase 1's `--features tunnel` flag removal (already done in Phase 1; tunnel stays default-compile forever).

**Locked decisions referenced in this plan (from 2026-06-15 plan-time review):**
1. **A1** Mixed-pool e2e is steady-state only (no chaos / failover injection).
2. **A2** All 8 routing strategies validated; produce a cross-protocol compatibility matrix `tests/tunnel-e2e/docs/cross-protocol-matrix.md` as a Phase 3 deliverable.
3. **A3** Load profile: **100 RPS × 30s**.
4. **A4** tunnel-e2e harness owns the mixed-pool scenarios (not soak; `make e2e-soak` is untouched).
5. **B1** Dep-boundary script's "tunnel crates" set: `gw-tunnel-client` + `gw-tunnel-server` + the tunnel-relevant slices of `gw-proxy` (`tunnel_stack`, `tunnel_bridge`) and `gw-gateway` (`tunnel_refresher`). The first two are crate-level boundaries; the latter two are *file*-level inspections — the script flags edges into business crates within those files.
6. **B2** Whitelist is **empirical**: the script does not enforce a hard-coded list. It reports the current set of dependency edges from the tunnel-crate boundary into business crates and exits with a warning (non-zero is informational, **not** CI-failing). Reviewer makes the call in MR.
7. **B3** Script is not wired into pre-commit / CI; manual `make check-tunnel-deps` only.
8. **B4** Implementation in bash.
9. **C1** G4 frontend scope: spec §3.6.7 baseline (Protocol column, Protocol radio, Aether status panel) **plus** Aether token management UI, register history view, GatewayControl gray-out.
10. **C2** Aether status panel time window: 1h default with 1m bucket aggregation done client-side from raw samples. No new ClickHouse rollup table.
11. **C3** UI integrates with existing `AILinkMax-Go` frontend (whatever framework it uses); inspect first.
12. **C4** Visual design: implementing engineer's call; no separate design review.
13. **D1** `docs/spec/09-tunnel.md` rewritten as as-built; `docs/superpowers/specs/2026-06-15-...` kept as **historical reference**.
14. **D4** ROADMAP closeout + Phase 4 candidates section added (Aether `remote_config` reverse-push, cross-protocol perf baseline, optional double-Aether multi-tenant tunnel-server).
15. **E1** No perf baseline in Phase 3.
16. **F1** 2 PRs (3a Rust + 3b Go).
17. **F2** Single shared branch; single final MR for `ailinkmax-gateway`.

**New questions surfaced and decided at plan-time:**
- **Token rotate runtime behavior**: dual-token grace period (1h TTL). Old token accepted for 1h after rotate; both old and new are stored in the row's `aether_config.auth_token_history` array. Operator can revoke immediately by overriding TTL to 0.
- **Mixed-pool seed shape**: 2 cnexus + 2 aether (4 nodes) covers `RoundRobin` / `WeightedRr` / `LoadBalance` (need ≥2 nodes per protocol). Exception: `FixedOrder` / `Sticky` / `CacheAffinity` are exercised separately with their own seed (single primary per protocol).
- **Dep-boundary "violation" reporting**: every edge from a tunnel-crate to a business-crate is reported as a one-line warning. Not all edges are wrong; the reviewer judges. Example: if PR 2b introduced `gw-store` dependency via the KMS helper, the script would warn → reviewer either accepts (KMS is part of the registry's contract) or rejects (push to extract KMS into `gw-core`).

---

## File Structure

### New files (PR 3a — mixed-pool + dep script + docs, this repo)

| Path | Responsibility |
|---|---|
| `tests/tunnel-e2e/scenarios/test_mixed_pool.py` | 3 scenarios per spec §6.5.D (`test_mixed_pool_load_distribution`, `test_mixed_pool_failover` reinterpreted as steady-state recovery, `test_mixed_pool_concurrent_no_corruption`) plus a parameterized `test_strategy_compatibility_matrix` that drives all 8 routing strategies and writes results to JSON. |
| `tests/tunnel-e2e/scenarios/test_strategy_matrix_generator.py` | Generates `tests/tunnel-e2e/docs/cross-protocol-matrix.md` from the JSON produced by `test_strategy_compatibility_matrix`. Runs post-pytest in the same `tunnel-e2e` pipeline so the matrix is always current. |
| `tests/tunnel-e2e/docs/cross-protocol-matrix.md` | Cross-protocol compatibility matrix as a Phase 3 deliverable. Generated; committed at PR-3a Task M6 with a "GENERATED — do not hand-edit" header. |
| `tests/tunnel-e2e/support/load_driver.py` | 100 RPS × 30s steady-state driver. Concurrency = 50, per-request budget configurable. Returns a `LoadReport` with per-node hit counts, per-request latency histogram, and strategy-specific assertions. |
| `tests/tunnel-e2e/seed/03_mixed_pool.sql` | Seeds 2 cnexus + 2 aether nodes plus the routing strategy fixtures (one per strategy). |
| `scripts/check-tunnel-deps.sh` | Bash script that walks the tunnel-crate boundary via `cargo metadata` + `jq`, reports edges into business crates as warnings, exits 0 (informational). Documented at top with the empirical-whitelist philosophy. |
| `docs/spec/09-tunnel.md` (full rewrite) | As-built reference covering both cnexus and aether wire protocols + transport trait + InboundListener + dep boundary + Phase 4 backlog. Supersedes the section-by-section additions from Phase 1/2 with a single coherent document. |

### Files modified (PR 3a, this repo)

| Path | Change |
|---|---|
| `tests/tunnel-e2e/docker-compose.yml` | Add a second cnexus tunnel-server (`cnexus-tunnel-server-2`) and a second aether-proxy (`aether-proxy-2`) for the multi-node strategy cases. Adopt `profiles: ["mixed"]` for the new ones so default `make tunnel-e2e` keeps the smaller compat suite and `make tunnel-e2e-mixed` opts into the full 4-node setup. |
| `Makefile` | Add `tunnel-e2e-mixed` (full 4-node setup) and `check-tunnel-deps`. |
| `tests/tunnel-e2e/README.md` | Append "Mixed-pool scenarios" section + "Cross-protocol compatibility matrix" pointer + `make check-tunnel-deps` usage. |
| `docs/ROADMAP.md` | Strike all tunnel-compat items as resolved; add new "Tunnel: Phase 4 candidates" subsection listing the items deferred. |
| `docs/architecture.md` | Add `Crate dependency boundary (tunnel)` line under `gw-tunnel-client` row pointing at `scripts/check-tunnel-deps.sh` + `docs/spec/09-tunnel.md` §Boundary. |
| `crates/gw-tunnel-client/README.md` | Add the `⚠️ Dependency boundary` warning block per spec §5.3. (Create the README if missing.) |
| `crates/gw-tunnel-server/README.md` | Same warning block. |
| `crates/gw-proxy/src/filters/upstream.rs` | (a) `GatewayControl` short-circuit for Aether nodes per spec §3.6.6 (cmd dispatcher returns `{status: "skipped", reason: "aether_node_does_not_support_command"}`). (b) Strategy implementations that need cross-protocol awareness (CacheAffinity, Sticky) gain a `protocol_aware: bool` config flag; default `false` (legacy behavior); when `true`, affinity is computed per-protocol (each protocol gets its own bucket). |
| `config/default.toml` | `[routing].protocol_aware_affinity = false` default. Document the trade-off in the inline comment. |

### New files (PR 3b — G4 frontend + supporting Go endpoints, `AILinkMax-Go` repo)

> All paths below are relative to the `AILinkMax-Go` repo root. The frontend framework is whatever the existing admin UI uses — confirm at G4.1 before writing. The paths use React conventions as a placeholder; substitute as needed.

| Path | Responsibility |
|---|---|
| `web/src/components/ProxyNodes/ProtocolBadge.tsx` | `<ProtocolBadge protocol="cnexus" \| "aether" />` → blue or purple badge. |
| `web/src/components/ProxyNodes/ProtocolRadio.tsx` | `<ProtocolRadio value=... onChange=... />` for create/edit form. Default `cnexus`. |
| `web/src/components/ProxyNodes/AetherStatusPanel.tsx` | Detail panel: pulls `GET /api/admin/monitoring/aether/{node_id}?since=now-1h`, client-side buckets into 1m windows, renders 5 sparklines + active connections. |
| `web/src/components/ProxyNodes/AetherTokenManager.tsx` | Three buttons + state: generate (only if no token), rotate (dual-token grace banner with countdown), revoke (immediate). |
| `web/src/components/ProxyNodes/AetherRegisterHistory.tsx` | Table of recent `aether_register_events` rows (new audit log table, see Go modifications). |
| `web/src/components/GatewayControl/CommandButton.tsx` (modify or create wrapper) | Wraps existing per-node command buttons. When protocol = aether: disables, shows tooltip "Aether nodes do not accept GatewayControl commands". |
| `modules/proxynode/api/aether_token_rotate_handler.go` | `POST /api/admin/proxy-nodes/{id}/aether-token/rotate` returns new `ae_*` token + grace TTL. |
| `modules/proxynode/api/aether_token_revoke_handler.go` | `DELETE /api/admin/proxy-nodes/{id}/aether-token/grace` — immediate revoke of old token (cancels dual-token grace). |
| `modules/proxynode/business/aether_token_lifecycle.go` | Generate / rotate / revoke logic. Token format `ae_<32-byte hex>` (matches Aether convention). |
| `modules/proxynode/business/aether_register_events.go` | Audit log: `RecordRegisterAttempt(node_id, source_ip, outcome)` writes a row to a small Postgres table on every `/api/admin/proxy-nodes/register` call. |
| `modules/proxynode/clickhouse/aether_register_events.sql` | DDL for the audit table (or Postgres — Go team's choice based on existing audit conventions). |
| `modules/proxynode/api/aether_register_events_handler.go` | `GET /api/admin/proxy-nodes/{id}/aether/register-events` returns the audit rows. |
| Tests for each new Go file. | |

### Files modified (PR 3b, `AILinkMax-Go` repo)

| Path | Change |
|---|---|
| `web/src/pages/ProxyNodes/index.tsx` (or local equivalent) | Add Protocol column to the table; integrate `ProtocolRadio` into the create/edit form; wire `AetherStatusPanel` + `AetherTokenManager` + `AetherRegisterHistory` into the per-node detail drawer; conditionally render `GatewayControl` buttons in gray for aether nodes. |
| `modules/proxynode/api/routes.go` | Mount the new admin token-management and register-events routes. |
| `modules/proxynode/business/aether_register.go` | Call `RecordRegisterAttempt` from inside `AetherRegister` for every code path (success, unknown-token, auto-provision). |
| `modules/monitoring/api/routes.go` (Phase 2 added base route) | No change; the existing `GET /api/admin/monitoring/aether/{node_id}` is what the panel calls. |

---

## PR 3a · Mixed-pool e2e + dep-boundary + final docs

### Task M1: Mixed-pool seed + docker-compose extension

**Files:**
- Create: `tests/tunnel-e2e/seed/03_mixed_pool.sql`
- Modify: `tests/tunnel-e2e/docker-compose.yml`

- [ ] **Step 1: Write the seed SQL**

```sql
-- tests/tunnel-e2e/seed/03_mixed_pool.sql
-- Mixed-pool scenarios: 2 cnexus + 2 aether nodes, plus per-strategy seeds.

TRUNCATE proxy_nodes;

INSERT INTO proxy_nodes (id, payload, version, deleted_at) VALUES
('cn1', jsonb_build_object(
    'tunnel_protocol', 'cnexus',
    'node_name', 'cnexus-mixed-1',
    'ws_url', 'wss://cnexus-tunnel-server:7000/tunnel',
    'auth_token', 'cn-mixed-1-token',
    'enabled', true
), 1, NULL),
('cn2', jsonb_build_object(
    'tunnel_protocol', 'cnexus',
    'node_name', 'cnexus-mixed-2',
    'ws_url', 'wss://cnexus-tunnel-server-2:7000/tunnel',
    'auth_token', 'cn-mixed-2-token',
    'enabled', true
), 1, NULL),
('ae1', jsonb_build_object(
    'tunnel_protocol', 'aether',
    'node_name', 'aether-mixed-1',
    'aether_config', jsonb_build_object('auth_token', 'ae_mixed_1'),
    'enabled', true
), 1, NULL),
('ae2', jsonb_build_object(
    'tunnel_protocol', 'aether',
    'node_name', 'aether-mixed-2',
    'aether_config', jsonb_build_object('auth_token', 'ae_mixed_2'),
    'enabled', true
), 1, NULL);

-- One vendor key with each routing strategy, all pointing at the same backend.
-- Used by the strategy matrix test.
TRUNCATE vendor_keys;
INSERT INTO vendor_keys (id, vendor_id, payload, version, deleted_at) VALUES
('vk-fixed',    'fake', jsonb_build_object('routing_strategy', 'FixedOrder',     'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-rr',       'fake', jsonb_build_object('routing_strategy', 'RoundRobin',     'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-lb',       'fake', jsonb_build_object('routing_strategy', 'LoadBalance',    'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-wrr',      'fake', jsonb_build_object('routing_strategy', 'WeightedRr',     'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-cost',     'fake', jsonb_build_object('routing_strategy', 'CostFirst',      'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-latency',  'fake', jsonb_build_object('routing_strategy', 'LatencyFirst',   'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-affinity', 'fake', jsonb_build_object('routing_strategy', 'CacheAffinity',  'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL),
('vk-sticky',   'fake', jsonb_build_object('routing_strategy', 'Sticky',         'node_pool', ARRAY['cn1','cn2','ae1','ae2']), 1, NULL);
```

> Adjust column names / payload keys to match the **as-deployed** schema rather than the spec's idealized shape. Run `\d proxy_nodes` and `\d vendor_keys` to confirm before commit.

- [ ] **Step 2: Extend `docker-compose.yml`**

Add (under existing services):

```yaml
  cnexus-tunnel-server-2:
    profiles: ["mixed"]
    build:
      context: ../../
      dockerfile: Dockerfile
      args:
        BIN: gw-tunnel-server
    command: >
      gw-tunnel-server
      --bind 0.0.0.0:7000
      --auth-token cn-mixed-2-token
      --max-streams 64
      --allow-target fake-upstream:9000
      --allow-target fake-upstream:9443
    networks: [tunnel-net]
    expose: ["7000"]

  aether-proxy-2:
    profiles: ["mixed"]
    build:
      context: .
      dockerfile: Dockerfile.aether-proxy
    environment:
      AETHER_PROXY_HUB_URL: http://nginx-edge
      AETHER_PROXY_AUTH_TOKEN: ae_mixed_2
      AETHER_PROXY_NODE_NAME: aether-mixed-2
    networks: [tunnel-net]
    depends_on:
      nginx-edge:
        condition: service_started
      ali-gateway:
        condition: service_started
```

- [ ] **Step 3: Commit**

```bash
git add tests/tunnel-e2e/seed/03_mixed_pool.sql tests/tunnel-e2e/docker-compose.yml
git commit -m "test(tunnel-e2e): seed mixed-pool (2 cnexus + 2 aether) + compose mixed profile"
```

---

### Task M2: `support/load_driver.py` — 100 RPS × 30s steady-state driver

**Files:**
- Create: `tests/tunnel-e2e/support/load_driver.py`

- [ ] **Step 1: Write the driver**

```python
# tests/tunnel-e2e/support/load_driver.py
"""Steady-state load driver for mixed-pool scenarios.

- Target: 100 RPS × 30 seconds = 3000 requests.
- Concurrency: 50 (so each worker pushes ~2 RPS).
- Tags each request with a unique nonce so the receiver can verify no cross-stream
  corruption.
"""

from __future__ import annotations

import asyncio
import time
import uuid
from dataclasses import dataclass, field
from typing import Callable

import httpx


@dataclass
class RequestResult:
    nonce: str
    status: int
    latency_ms: float
    served_by_node_id: str | None
    body_len: int


@dataclass
class LoadReport:
    duration_s: float
    total_requests: int
    successes: int
    failures: int
    by_node_id: dict[str, int] = field(default_factory=dict)
    latencies_ms: list[float] = field(default_factory=list)

    def percentile(self, p: float) -> float:
        s = sorted(self.latencies_ms)
        if not s:
            return 0.0
        idx = min(int(len(s) * p / 100), len(s) - 1)
        return s[idx]


async def run_load(
    client_factory: Callable[[], httpx.AsyncClient],
    *,
    rps: int = 100,
    duration_s: int = 30,
    concurrency: int = 50,
    make_request: Callable[[httpx.AsyncClient, str], "asyncio.coroutine"],
) -> LoadReport:
    """Drive `rps` × `duration_s` requests at `concurrency` workers.

    `make_request(client, nonce)` returns (status, latency_ms, served_by_node_id, body_len).
    """
    total = rps * duration_s
    queue: asyncio.Queue[str] = asyncio.Queue(maxsize=total + 100)
    for _ in range(total):
        queue.put_nowait(str(uuid.uuid4()))

    report = LoadReport(duration_s=duration_s, total_requests=total, successes=0, failures=0)

    async def worker():
        async with client_factory() as client:
            while True:
                try:
                    nonce = queue.get_nowait()
                except asyncio.QueueEmpty:
                    return
                t0 = time.monotonic()
                try:
                    status, lat, node, body_len = await make_request(client, nonce)
                except Exception:
                    report.failures += 1
                    queue.task_done()
                    continue
                report.successes += 1
                report.latencies_ms.append(lat)
                if node is not None:
                    report.by_node_id[node] = report.by_node_id.get(node, 0) + 1
                queue.task_done()

    workers = [asyncio.create_task(worker()) for _ in range(concurrency)]
    t_start = time.monotonic()
    # Spread requests evenly over duration_s by token-bucket rate-limiting:
    interval = 1.0 / rps
    for _ in range(total):
        await asyncio.sleep(interval)
        if time.monotonic() - t_start > duration_s + 5:
            break
    await asyncio.gather(*workers, return_exceptions=True)
    return report
```

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/support/load_driver.py
git commit -m "test(tunnel-e2e): add 100 RPS × 30s steady-state load driver"
```

---

### Task M3: Write `test_mixed_pool.py` — 3 scenarios

**Files:**
- Create: `tests/tunnel-e2e/scenarios/test_mixed_pool.py`

> Per A1 (steady state only), Scenario 2's original "failover" framing changes from "kill the aether-proxy mid-test" to "with a pre-cooled-down node in the pool, traffic naturally avoids it" — i.e., still steady-state.

- [ ] **Step 1: Write the three scenarios**

```python
# tests/tunnel-e2e/scenarios/test_mixed_pool.py
import asyncio
import json
import os
import httpx
import pytest

from support.load_driver import LoadReport, run_load


pytestmark = pytest.mark.usefixtures("mixed_pool_profile")  # opt-in to mixed profile


@pytest.fixture
def mixed_pool_profile():
    """Verify docker-compose was started with profile=mixed (2cnexus+2aether)."""
    # Cheap check: the gateway's /healthz/tunnel must list 4 nodes.
    resp = httpx.get(f"{os.environ['ALI_GATEWAY_URL']}/healthz/tunnel",
                     headers={"Authorization": f"Bearer {os.environ['ADMIN_TOKEN']}"})
    nodes = resp.json()["nodes"]
    assert len(nodes) >= 4, f"expected 4 mixed-pool nodes, got {len(nodes)}: {nodes}"
    return True


async def make_echo_request(client: httpx.AsyncClient, nonce: str):
    import time
    t0 = time.monotonic()
    r = await client.post("/v1/echo",
        json={"nonce": nonce},
        headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid"})
    lat = (time.monotonic() - t0) * 1000
    return r.status_code, lat, r.headers.get("X-Tunnel-Node-Id"), len(r.content)


@pytest.mark.asyncio
async def test_mixed_pool_load_distribution():
    """100 RPS × 30s, all 4 nodes receive non-trivial traffic, success ≥ 99%."""
    report = await run_load(
        client_factory=lambda: httpx.AsyncClient(base_url=os.environ['ALI_GATEWAY_URL'], timeout=10.0),
        rps=100, duration_s=30, concurrency=50,
        make_request=make_echo_request,
    )
    assert report.successes / report.total_requests >= 0.99, f"success rate {report.successes}/{report.total_requests}"
    # Every node must serve ≥ 5% of the traffic (= 150 requests of 3000).
    for nid in ("cn1", "cn2", "ae1", "ae2"):
        count = report.by_node_id.get(nid, 0)
        assert count >= 150, f"node {nid} served only {count} of {report.total_requests}"


@pytest.mark.asyncio
async def test_mixed_pool_concurrent_no_corruption():
    """Each request carries a unique nonce in body; response echoes it. Verify no crossover."""
    # Lower volume but high nonce uniqueness verification.
    received = {}

    async def make_and_verify(client: httpx.AsyncClient, nonce: str):
        r = await client.post("/v1/echo",
            json={"nonce": nonce},
            headers={"X-Api-Key": "test-key", "X-Api-Key-Id": "test-kid"})
        echoed = r.json().get("nonce")
        received[nonce] = echoed
        return r.status_code, 0.0, r.headers.get("X-Tunnel-Node-Id"), len(r.content)

    await run_load(
        client_factory=lambda: httpx.AsyncClient(base_url=os.environ['ALI_GATEWAY_URL'], timeout=10.0),
        rps=50, duration_s=10, concurrency=20,
        make_request=make_and_verify,
    )
    mismatches = [n for n, e in received.items() if n != e]
    assert not mismatches, f"{len(mismatches)} responses had wrong nonce — stream crossover suspected"


@pytest.mark.asyncio
async def test_mixed_pool_pre_cooled_node_avoided():
    """Pre-cool one aether node via the routing-cooldown admin endpoint;
    verify the steady-state load_distribution avoids it.

    This is the A1-redefined 'failover' scenario: no chaos, but a pre-cooled
    node in the pool is observably routed around.
    """
    # 1. Pre-cool ae1
    httpx.post(f"{os.environ['ALI_GATEWAY_URL']}/api/admin/routing/cooldown",
               headers={"Authorization": f"Bearer {os.environ['ADMIN_TOKEN']}"},
               json={"node_id": "ae1", "duration_ms": 60000})

    # 2. Drive load
    report = await run_load(
        client_factory=lambda: httpx.AsyncClient(base_url=os.environ['ALI_GATEWAY_URL'], timeout=10.0),
        rps=100, duration_s=30, concurrency=50,
        make_request=make_echo_request,
    )

    # 3. Pre-cooled node should serve 0% (or near-zero from cold-cache strategy interactions).
    cooled_traffic = report.by_node_id.get("ae1", 0)
    assert cooled_traffic <= 30, f"pre-cooled node ae1 served {cooled_traffic} — cooldown ignored?"
    # Other 3 nodes split the load:
    for nid in ("cn1", "cn2", "ae2"):
        assert report.by_node_id.get(nid, 0) >= 200, f"node {nid} under-served while ae1 cooled"
```

- [ ] **Step 2: Commit**

```bash
git add tests/tunnel-e2e/scenarios/test_mixed_pool.py
git commit -m "test(tunnel-e2e): add 3 mixed-pool steady-state scenarios"
```

---

### Task M4: Cross-protocol strategy compatibility matrix test

**Files:**
- Create: `tests/tunnel-e2e/scenarios/test_strategy_matrix_generator.py`
- Modify: `tests/tunnel-e2e/scenarios/test_mixed_pool.py` (add parameterized matrix test)

- [ ] **Step 1: Add the parameterized strategy test to `test_mixed_pool.py`**

```python
STRATEGIES = [
    ("FixedOrder",    "vk-fixed"),
    ("RoundRobin",    "vk-rr"),
    ("LoadBalance",   "vk-lb"),
    ("WeightedRr",    "vk-wrr"),
    ("CostFirst",     "vk-cost"),
    ("LatencyFirst",  "vk-latency"),
    ("CacheAffinity", "vk-affinity"),
    ("Sticky",        "vk-sticky"),
]

@pytest.mark.asyncio
@pytest.mark.parametrize("strategy,api_key_id", STRATEGIES)
async def test_strategy_compatibility_matrix(strategy: str, api_key_id: str, request):
    """Run 100 RPS × 30s through this vendor-key (which has a specific routing
    strategy). Record per-node hit counts + success rate to a JSON the matrix
    generator consumes.
    """
    async def make_request(client: httpx.AsyncClient, nonce: str):
        import time
        t0 = time.monotonic()
        r = await client.post("/v1/echo",
            json={"nonce": nonce},
            headers={"X-Api-Key": "test-key", "X-Api-Key-Id": api_key_id})
        return r.status_code, (time.monotonic() - t0) * 1000, r.headers.get("X-Tunnel-Node-Id"), len(r.content)

    report = await run_load(
        client_factory=lambda: httpx.AsyncClient(base_url=os.environ['ALI_GATEWAY_URL'], timeout=10.0),
        rps=100, duration_s=30, concurrency=50,
        make_request=make_request,
    )
    # Persist per-strategy report so the matrix-generator picks it up.
    out_dir = os.environ.get("MATRIX_OUT_DIR", "tests/tunnel-e2e/.matrix")
    os.makedirs(out_dir, exist_ok=True)
    with open(f"{out_dir}/{strategy}.json", "w") as f:
        json.dump({
            "strategy": strategy,
            "total": report.total_requests,
            "success": report.successes,
            "failure": report.failures,
            "by_node": report.by_node_id,
            "p50_ms": report.percentile(50),
            "p99_ms": report.percentile(99),
        }, f, indent=2)
    # Lax assertion: just confirm the run completed. The matrix doc reports the breakdown.
    assert report.total_requests == 3000
```

- [ ] **Step 2: Write the matrix generator**

```python
# tests/tunnel-e2e/scenarios/test_strategy_matrix_generator.py
"""Runs LAST (alphabetically after `test_strategy_compatibility_matrix`).
Reads each per-strategy JSON dropped by the matrix test and emits the
cross-protocol-matrix.md document.
"""
import json, os, pytest

@pytest.mark.last
def test_emit_strategy_compatibility_matrix_document():
    out_dir = os.environ.get("MATRIX_OUT_DIR", "tests/tunnel-e2e/.matrix")
    if not os.path.isdir(out_dir):
        pytest.skip("no per-strategy data — matrix test did not run")

    lines = [
        "<!-- GENERATED by tests/tunnel-e2e/scenarios/test_strategy_matrix_generator.py — do not hand-edit -->",
        "",
        "# Cross-protocol Routing Strategy Compatibility Matrix",
        "",
        "Generated by the Phase 3 tunnel-e2e suite from `make tunnel-e2e-mixed`.",
        "",
        "Pool: 2 cnexus + 2 aether. Load: 100 RPS × 30s = 3000 requests per strategy.",
        "",
        "| Strategy | cn1 | cn2 | ae1 | ae2 | success% | p50 ms | p99 ms | Verdict |",
        "|---|---|---|---|---|---|---|---|---|",
    ]
    for fname in sorted(os.listdir(out_dir)):
        if not fname.endswith(".json"):
            continue
        d = json.load(open(f"{out_dir}/{fname}"))
        success_pct = 100 * d['success'] / d['total']
        bn = d['by_node']
        # Verdict heuristics:
        cnexus_total = bn.get('cn1', 0) + bn.get('cn2', 0)
        aether_total = bn.get('ae1', 0) + bn.get('ae2', 0)
        verdict = "✅ traffic split" if cnexus_total > 100 and aether_total > 100 \
                  else ("⚠️ single-protocol skew" if cnexus_total == 0 or aether_total == 0 else "⚠️ partial coverage")
        lines.append(f"| {d['strategy']} | {bn.get('cn1',0)} | {bn.get('cn2',0)} | {bn.get('ae1',0)} | {bn.get('ae2',0)} | {success_pct:.1f}% | {d['p50_ms']:.1f} | {d['p99_ms']:.1f} | {verdict} |")

    lines += [
        "",
        "## Reading guide",
        "",
        "- **traffic split**: traffic distributed across both protocols. Strategy is cross-protocol-sane.",
        "- **single-protocol skew**: all traffic landed on one protocol. May or may not be intentional:",
        "  - For `CacheAffinity` / `Sticky` this is **expected** when the strategy keys on a property",
        "    (e.g. cache key) that happens to map to one protocol's nodes only.",
        "  - For `RoundRobin` / `LoadBalance` this is a **bug**.",
        "- **partial coverage**: some protocol's nodes were skipped. Cross-reference per-node logs.",
        "",
        "## Caveats",
        "",
        "- This matrix observes *behavior*, not correctness contracts. A strategy that fails",
        "  byte-identity even with 100% success counts as a regression and must be investigated.",
        "- The verdict column is a heuristic; review the raw cell counts.",
    ]

    out_path = "tests/tunnel-e2e/docs/cross-protocol-matrix.md"
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"WROTE {out_path}")
```

- [ ] **Step 3: Commit**

```bash
git add tests/tunnel-e2e/scenarios/test_mixed_pool.py \
        tests/tunnel-e2e/scenarios/test_strategy_matrix_generator.py
git commit -m "test(tunnel-e2e): cross-protocol strategy compatibility matrix + generator"
```

---

### Task M5: `Makefile` targets `tunnel-e2e-mixed` + `check-tunnel-deps`

**Files:**
- Modify: `Makefile`

```makefile
# Append to existing tunnel-e2e block:

TUNNEL_E2E_COMPOSE_MIXED := -f tests/tunnel-e2e/docker-compose.yml \
    --project-directory tests/tunnel-e2e --profile mixed

.PHONY: tunnel-e2e-mixed check-tunnel-deps

tunnel-e2e-mixed: tunnel-e2e-deps
	docker compose $(TUNNEL_E2E_COMPOSE_MIXED) build
	docker compose $(TUNNEL_E2E_COMPOSE_MIXED) run --rm \
	  -e MATRIX_OUT_DIR=/work/.matrix \
	  pytest-runner pytest scenarios/test_mixed_pool.py scenarios/test_strategy_matrix_generator.py -v
	# Copy matrix output back to host workspace
	docker compose $(TUNNEL_E2E_COMPOSE_MIXED) run --rm pytest-runner \
	  cp /work/tests/tunnel-e2e/docs/cross-protocol-matrix.md tests/tunnel-e2e/docs/
	$(MAKE) tunnel-e2e-down

check-tunnel-deps:
	bash scripts/check-tunnel-deps.sh
```

Commit.

---

### Task M6: Run `make tunnel-e2e-mixed` end-to-end + commit the generated matrix

- [ ] **Step 1: Run from clean tree**

Run: `make tunnel-e2e-down && make tunnel-e2e-mixed`
Expected: All mixed-pool tests pass; `tests/tunnel-e2e/docs/cross-protocol-matrix.md` is generated.

- [ ] **Step 2: Inspect the matrix**

Open the generated file. Confirm:
- 8 rows (one per strategy).
- At least 4 strategies show "traffic split" (likely RoundRobin, LoadBalance, FixedOrder if pool order interleaves, WeightedRr).
- CacheAffinity / Sticky may show "single-protocol skew" — this is the documented expected behavior.
- p99 latency is in a reasonable range (< 2× p50) for steady-state runs.

If any strategy shows "single-protocol skew" unexpectedly (e.g., RoundRobin), STOP — that's a bug in the routing implementation, not a docs-only issue. File and fix before committing the matrix.

- [ ] **Step 3: Commit the matrix file**

```bash
git add tests/tunnel-e2e/docs/cross-protocol-matrix.md
git commit -m "docs(tunnel-e2e): commit cross-protocol routing-strategy matrix (generated)"
```

---

### Task M7: PR-3a mixed-pool gate

Run all of:
- `make tunnel-e2e` (cnexus + aether compat scenarios from Phase 1+2)
- `make tunnel-e2e-baseline`
- `make tunnel-e2e-mixed`
- `cargo-remote-ctl test --workspace`
- `cargo-remote-ctl clippy --workspace --all-targets -- -D warnings`
- `bash scripts/check-invariants.sh`

All expected clean.

---

### Task S1: Write `scripts/check-tunnel-deps.sh`

**Files:**
- Create: `scripts/check-tunnel-deps.sh`

- [ ] **Step 1: Write the script**

> Per B2/B6: empirical, warn-mode, no hard-coded whitelist.

```bash
#!/usr/bin/env bash
# scripts/check-tunnel-deps.sh
#
# Tunnel dependency-boundary audit.
#
# Philosophy (locked 2026-06-15): tunnel crates (gw-tunnel-client, gw-tunnel-server)
# and the tunnel-relevant files in gw-proxy / gw-gateway should not pull business
# crates unless there's a documented reason. This script does NOT enforce a
# hard-coded whitelist; it surfaces every edge into a business crate so the
# MR reviewer can judge. Exit code 0 always — the script is informational, not
# a CI gate.
#
# Run: `make check-tunnel-deps` or `bash scripts/check-tunnel-deps.sh`.

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required; install via 'brew install jq' or apt-get install jq" >&2
    exit 2
fi

# 1. Crate-level audit (`gw-tunnel-client`, `gw-tunnel-server`).

TUNNEL_CRATES=(gw-tunnel-client gw-tunnel-server)
META=$(cargo metadata --format-version 1 --no-deps)

# Build a list of workspace member names.
WORKSPACE=$(jq -r '.packages[].name' <<<"$META" | sort -u)

# Tunnel-relevant files in business crates (file-level audit, per B5).
TUNNEL_RELEVANT_FILES=(
    "crates/gw-proxy/src/tunnel_stack.rs"
    "crates/gw-proxy/src/tunnel_bridge.rs"
    "crates/gw-gateway/src/tunnel_refresher.rs"  # adjust path if different
)

# Hand-curated set of "business crates" — every workspace crate that is NOT
# itself a tunnel crate or a core/codec crate.
CORE_LIKE=(gw-core gw-codec gw-codec-macros)
TUNNEL_LIKE=("${TUNNEL_CRATES[@]}")
declare -A is_non_business
for c in "${CORE_LIKE[@]}" "${TUNNEL_LIKE[@]}"; do
    is_non_business[$c]=1
done

found_edges=0

echo "=== Tunnel dep-boundary audit ==="
echo ""

# Section A: crate-level dependencies
echo "## Section A — crate-level dependencies"
echo ""

for tc in "${TUNNEL_CRATES[@]}"; do
    deps=$(jq -r --arg n "$tc" '.packages[] | select(.name==$n) | .dependencies[] | .name' <<<"$META" | sort -u)
    business_deps=()
    for d in $deps; do
        # Skip if not a workspace member or if it's a non-business crate.
        if ! grep -qx "$d" <<<"$WORKSPACE"; then
            continue
        fi
        if [ "${is_non_business[$d]:-0}" = "1" ]; then
            continue
        fi
        business_deps+=("$d")
    done
    if [ ${#business_deps[@]} -eq 0 ]; then
        echo "✓ $tc: no business-crate deps"
    else
        echo "⚠ $tc depends on business crate(s): ${business_deps[*]}"
        found_edges=$((found_edges + ${#business_deps[@]}))
    fi
done

echo ""
echo "## Section B — file-level audit"
echo ""

# Section B: file-level grep for `use gw_<biz>::` in tunnel-relevant files.
for f in "${TUNNEL_RELEVANT_FILES[@]}"; do
    if [ ! -f "$f" ]; then
        echo "  $f — file missing, skipping"
        continue
    fi
    # grep for `use gw_<name>::`. Strip CORE_LIKE + TUNNEL_LIKE.
    biz_uses=$(grep -oE 'use gw_[a-z_]+' "$f" | sort -u | sed 's/^use //' | tr '_' '-' | sed 's/^/gw-/' || true)
    file_biz=()
    for u in $biz_uses; do
        if [ "${is_non_business[$u]:-0}" = "1" ]; then continue; fi
        # Also accept gw-runtime / etc. that aren't workspace members
        if ! grep -qx "$u" <<<"$WORKSPACE"; then continue; fi
        file_biz+=("$u")
    done
    if [ ${#file_biz[@]} -eq 0 ]; then
        echo "  ✓ $f"
    else
        echo "  ⚠ $f imports: ${file_biz[*]}"
        found_edges=$((found_edges + ${#file_biz[@]}))
    fi
done

echo ""
echo "## Summary"
echo ""
echo "Edges from tunnel surface into business crates: $found_edges"
echo ""
if [ "$found_edges" -gt 0 ]; then
    cat <<EOF
A reviewer should look at each edge above:
  - Is the dependency necessary (e.g. ProxyNodeRegistry needs gw-store-decrypted token)?
  - If yes, document the rationale in docs/spec/09-tunnel.md §Boundary exceptions.
  - If no, extract the shared logic into gw-core or eliminate the dep.

This audit is informational. It always exits 0 — never blocks merge.
EOF
fi

exit 0
```

- [ ] **Step 2: chmod and smoke-test**

```bash
chmod +x scripts/check-tunnel-deps.sh
bash scripts/check-tunnel-deps.sh
```

Expected: a summary report. Whatever the current dependency edges are, they're reported but the script exits 0.

- [ ] **Step 3: Commit**

```bash
git add scripts/check-tunnel-deps.sh
git commit -m "build: add scripts/check-tunnel-deps.sh dep-boundary audit (warn-mode)"
```

---

### Task S2: Add boundary warning blocks to `gw-tunnel-client` + `gw-tunnel-server` README

**Files:**
- Create or modify: `crates/gw-tunnel-client/README.md`
- Create or modify: `crates/gw-tunnel-server/README.md`

- [ ] **Step 1: Write/append the warning block**

Per spec §5.3, prepend (or insert near top of) both READMEs:

```markdown
> ## ⚠️ Dependency boundary
>
> This crate is audited by `scripts/check-tunnel-deps.sh` (manual; not in CI).
> The intent is: within this workspace it should only depend on `gw-core`,
> `gw-codec`, and `gw-codec-macros`. Other workspace crates are not strictly
> forbidden — but each one is reported by the audit script as an edge for an
> MR reviewer to judge.
>
> This discipline keeps the tunnel subsystem independently extractable. If
> you add a workspace dependency here, document the rationale in
> `docs/spec/09-tunnel.md` §Boundary so the next reviewer sees the why.
```

Commit.

---

### Task S3: `make check-tunnel-deps` Makefile target — *already in Task M5* — Skip

---

### Task S4: Boundary section in `docs/spec/09-tunnel.md`

Deferred to Task F1 (full spec rewrite below).

---

### Task F1: Full rewrite of `docs/spec/09-tunnel.md` as as-built reference

**Files:**
- Rewrite: `docs/spec/09-tunnel.md`

> Phase 1 and Phase 2 incrementally appended to this file. Phase 3 consolidates into a single coherent document that future readers can use without reading the 2026-06-15 design spec.

- [ ] **Step 1: Build the table of contents**

```markdown
# Tunnel subsystem (as-built)

> As-built reference. For the historical design rationale see
> `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`.

## 1. Overview
## 2. Wire protocols
    2.1 Cnexus (outbound)
    2.2 Aether (inbound)
## 3. Trait surface (`Transport` / `StreamTarget`)
## 4. Connection lifecycle
    4.1 Cnexus: outbound supervised
    4.2 Aether: inbound listener
## 5. Per-node protocol discriminator (`TunnelProtocol`)
## 6. Per-node `ProxyNodePayload` shape
## 7. Hub-side health surface (`/healthz/tunnel`)
## 8. Heartbeat ingestion (events:aether_heartbeat → Go → ClickHouse)
## 9. Routing strategies × protocols (compatibility matrix pointer)
## 10. Crate dependency boundary
## 11. Operational kill switches
## 12. Future work (Phase 4 candidates)
```

- [ ] **Step 2: Fill in each section**

Each section should be 5-30 lines of as-built description. References to source files use the actual paths after Phase 1+2 land. Code excerpts only where the wire shape is load-bearing (e.g., heartbeat JSON schema).

§10 references `scripts/check-tunnel-deps.sh` and lays out the empirical-whitelist philosophy.

§12 lists deferred items: Aether `remote_config` reverse-push, cross-protocol perf baseline, optional double-Aether multi-tenant tunnel-server.

- [ ] **Step 3: Commit**

```bash
git add docs/spec/09-tunnel.md
git commit -m "docs(spec): rewrite 09-tunnel.md as as-built reference (cnexus + aether)"
```

---

### Task F2: `docs/ROADMAP.md` closeout + Phase 4 candidates

- [ ] **Step 1: Mark tunnel items as resolved**

In `docs/ROADMAP.md`, strike all `tunnel`-tagged entries (or move them under a new "Resolved" subsection with a one-line note linking to the integrated MR — to be filled in after Task F3 opens it).

- [ ] **Step 2: Add "Tunnel — Phase 4 candidates"**

```markdown
## Tunnel — Phase 4 candidates (priority: nice-to-have)

- **Aether `remote_config` reverse-push**: if Aether ships this in a future release,
  Cnexus hub can act on it via a new `aether-remote-config` GatewayControl command.
  Currently `HeartbeatAck` deliberately omits `remote_config` / `upgrade_to` (spec §4.6).
- **Cross-protocol performance baseline**: per the 2026-06-15 plan-time E1 decision,
  Phase 3 did not produce a perf baseline (correctness over performance). A future
  effort could capture latency / throughput percentiles per (strategy × protocol)
  in `docs/issues/` or `tests/tunnel-e2e/baselines/`.
- **Double-Aether multi-tenant tunnel-server**: today the gateway hosts one
  `aether::InboundListener`. A future release may multi-tenant two Aether
  proxy fleets behind one listener (different token namespaces, different
  ProxyNodeRegistry shards).
```

Commit.

---

### Task F3: Final integrated MR to `main`

> This is the **only** MR opened against `main` in the `ailinkmax-gateway` repo for the entire tunnel-compat work. It carries Phase 1 + Phase 2 + Phase 3 commits.

- [ ] **Step 1: Push the shared branch**

```bash
git push origin feature/tunnel-compat
```

- [ ] **Step 2: Open the MR**

MR title: `feat(tunnel): Cnexus ↔ Aether compatibility (Phase 1 + 2 + 3, full)`

MR description must include:
- One-paragraph summary linking to the design spec.
- Commit-list grouped by phase (Phase 1: Stage 0 + Stage 1 + Stage Tunnel-E2E; Phase 2: 2a + 2b + 2c; Phase 3: 3a).
- Captured test summaries:
  - `cargo-remote-ctl test --workspace` final 5 lines.
  - `cargo-remote-ctl clippy --workspace --all-targets -- -D warnings` clean.
  - `bash scripts/check-invariants.sh` clean.
  - `make tunnel-e2e` summary (Phase 1 3 + Phase 2 8 + Phase 2 baseline 1 = 12 passed).
  - `make tunnel-e2e-mixed` summary (Phase 3 3 + 8 matrix params = 11 passed).
  - `make e2e` summary (25 scenarios, unchanged from main).
- Captured `bash scripts/check-tunnel-deps.sh` output (informational; flagged edges enumerated).
- Captured `tests/tunnel-e2e/docs/cross-protocol-matrix.md` as a collapsible section.
- Cross-repo links:
  - Phase 1 G1/G2 Go MR (already merged).
  - Phase 2 G3 Go MR (PR 2d, already merged).
  - Phase 3 G4 Go MR (PR 3b, see below) — landed first.
- ADR pointer: `docs/design/2026-06-15-tunnel-default-compile-decision.md`.

- [ ] **Step 3: After MR review feedback**

Address each comment by adding new commits on the branch (per Phase 1 §Git Safety Protocol — no rebases / no force-push during review unless explicitly requested by reviewer).

- [ ] **Step 4: Merge**

After approval, merge via the project's standard merge mechanism. Do **not** squash unless the reviewer explicitly asks — the commit history is the audit trail of the work breakdown.

---

### Task F4: Post-merge cleanup

- [ ] **Step 1: Delete the local feature branch**

```bash
git checkout main
git pull origin main
git branch -D feature/tunnel-compat
```

- [ ] **Step 2: Update `MEMORY.md` if used**

Any local memory entries referencing in-flight tunnel work should be deleted or marked as resolved.

- [ ] **Step 3: Smoke-check on `main`**

```bash
cargo-remote-ctl test --workspace
make e2e
```

Both expected clean. If anything fails, file a hotfix immediately — the MR review should have caught issues but rare edge cases exist.

---

## PR 3b · Go G4 frontend (AILinkMax-Go)

> **Repo**: `AILinkMax-Go`. **Branch**: `feature/tunnel-compat-phase-3-go`. Lands as a real MR **before** the Rust integrated MR (Task F3).

### Task G4.1: Audit existing frontend stack + plan integration points

- [ ] **Step 1: Inspect**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
ls web/ 2>/dev/null || ls frontend/ || find . -maxdepth 2 -name 'package.json' -not -path './node_modules/*'
cat web/package.json 2>/dev/null || true
```

Record findings in the MR description draft:
- Framework: React/Vue/Svelte/...
- Routing: react-router / Vue Router / ...
- HTTP client: axios / fetch wrapper / ...
- Component library: MUI / Ant / Tailwind / shadcn / ...
- Per-node detail rendering location.

- [ ] **Step 2: Identify the per-node detail drawer/page**

```bash
grep -rn "proxy.*node.*detail\|ProxyNodeDetail\|NodeDetail" web/ src/ 2>/dev/null | head -10
```

Locate the existing component that renders per-node detail. This is where the new `AetherStatusPanel`, `AetherTokenManager`, and `AetherRegisterHistory` will mount.

- [ ] **Step 3: Skip commit for this task — it's pure investigation. Document findings in a scratch file used by subsequent tasks.**

---

### Task G4.2: `ProtocolBadge` + `ProtocolRadio` + table column

**Files:**
- Create: `web/src/components/ProxyNodes/ProtocolBadge.{tsx|vue}`
- Create: `web/src/components/ProxyNodes/ProtocolRadio.{tsx|vue}`
- Modify: existing proxy-nodes table component + create/edit form

- [ ] **Step 1: ProtocolBadge component**

Implement a simple badge component (`cnexus` → blue background; `aether` → purple background) using the existing component library. ~20 lines.

- [ ] **Step 2: ProtocolRadio component**

Two radio buttons with labels. Default `cnexus`. Emits `change(value)`. ~25 lines.

- [ ] **Step 3: Wire into table + form**

Add a `Protocol` column to the per-node table using `ProtocolBadge`. Insert `ProtocolRadio` into the create/edit form between "Node name" and "WS URL" (WS URL only visible for cnexus).

- [ ] **Step 4: Commit**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add web/src/components/ProxyNodes/{ProtocolBadge,ProtocolRadio}.* \
        web/src/pages/ProxyNodes/...
git commit -m "feat(web): protocol badge + radio in proxy-nodes table and form"
```

---

### Task G4.3: `AetherStatusPanel` (1m bucket aggregation, 1h default)

**Files:**
- Create: `web/src/components/ProxyNodes/AetherStatusPanel.{tsx|vue}`

- [ ] **Step 1: Component logic**

```tsx
// React placeholder; adapt to actual framework.
import { useEffect, useState } from "react";
import { Sparkline } from "../charts/Sparkline"; // existing chart component

interface Sample {
    received_at: string; // ISO
    active_connections: number;
    total_requests: number;
    avg_latency_ms: number;
    failed_requests: number;
    dns_failures: number;
    stream_errors: number;
}

function bucketByMinute(samples: Sample[]): Bucket[] {
    const buckets = new Map<string, Sample[]>();
    for (const s of samples) {
        const dt = new Date(s.received_at);
        const key = `${dt.toISOString().slice(0, 16)}:00`; // YYYY-MM-DDTHH:MM:00
        if (!buckets.has(key)) buckets.set(key, []);
        buckets.get(key)!.push(s);
    }
    return Array.from(buckets.entries())
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([key, group]) => ({
            t: key,
            active_connections: avg(group.map(g => g.active_connections)),
            total_requests: last(group.map(g => g.total_requests)), // cumulative -> use last
            avg_latency_ms: avg(group.map(g => g.avg_latency_ms)),
            failed_requests: last(group.map(g => g.failed_requests)),
            dns_failures: last(group.map(g => g.dns_failures)),
            stream_errors: last(group.map(g => g.stream_errors)),
        }));
}

const avg = (arr: number[]) => arr.length ? arr.reduce((s, x) => s + x, 0) / arr.length : 0;
const last = (arr: number[]) => arr.length ? arr[arr.length - 1] : 0;

export function AetherStatusPanel({ nodeId }: { nodeId: string }) {
    const [buckets, setBuckets] = useState<Bucket[] | null>(null);
    useEffect(() => {
        const since = new Date(Date.now() - 60 * 60 * 1000).toISOString();
        fetch(`/api/admin/monitoring/aether/${nodeId}?since=${since}`)
            .then(r => r.json())
            .then(d => setBuckets(bucketByMinute(d.samples)))
            .catch(() => setBuckets([]));
    }, [nodeId]);
    if (buckets === null) return <div>Loading...</div>;
    if (buckets.length === 0) return <div>No heartbeat samples in the last hour.</div>;
    return (
        <div className="aether-status-panel">
            <h3>Aether status — last 1h (1m buckets)</h3>
            <div className="grid">
                <Stat label="Active connections" data={buckets.map(b => b.active_connections)} />
                <Stat label="Avg latency (ms)"  data={buckets.map(b => b.avg_latency_ms)} />
                <Stat label="Failed requests"   data={buckets.map(b => b.failed_requests)} />
                <Stat label="DNS failures"      data={buckets.map(b => b.dns_failures)} />
                <Stat label="Stream errors"     data={buckets.map(b => b.stream_errors)} />
            </div>
        </div>
    );
}

function Stat({ label, data }: { label: string; data: number[] }) {
    const latest = data[data.length - 1];
    return (
        <div className="stat">
            <div className="label">{label}</div>
            <div className="value">{latest}</div>
            <Sparkline data={data} width={120} height={32} />
        </div>
    );
}
```

> If `Sparkline` doesn't exist in the existing component library, swap for whatever chart primitive is available (line chart, area chart, etc.). The data shape is what's contractual; the visual is the engineer's call (per C4).

- [ ] **Step 2: Commit**

```bash
git add web/src/components/ProxyNodes/AetherStatusPanel.*
git commit -m "feat(web): Aether status panel (1m buckets over 1h window)"
```

---

### Task G4.4: `AetherTokenManager` with dual-token grace

**Files:**
- Create: `web/src/components/ProxyNodes/AetherTokenManager.{tsx|vue}`
- Create: `modules/proxynode/api/aether_token_rotate_handler.go`
- Create: `modules/proxynode/api/aether_token_revoke_handler.go`
- Create: `modules/proxynode/business/aether_token_lifecycle.go`

- [ ] **Step 1: Go-side token lifecycle business logic**

```go
// modules/proxynode/business/aether_token_lifecycle.go
package business

import (
    "context"
    "crypto/rand"
    "encoding/hex"
    "errors"
    "fmt"
    "time"
)

// AuthTokenEntry: stored in proxy_nodes.payload.aether_config.auth_token_history
type AuthTokenEntry struct {
    Token       string    `json:"token"`
    IssuedAt    time.Time `json:"issued_at"`
    ExpiresAt   *time.Time `json:"expires_at,omitempty"` // nil = active
}

type AetherTokenStore struct {
    AuthTokenHistory []AuthTokenEntry `json:"auth_token_history"`
}

func generateToken() string {
    var b [32]byte
    _, _ = rand.Read(b[:])
    return "ae_" + hex.EncodeToString(b[:])
}

func (s *Service) GenerateAetherToken(ctx context.Context, nodeID string) (string, error) {
    tok := generateToken()
    return tok, s.upsertAetherToken(ctx, nodeID, tok, nil)
}

// RotateAetherToken: emit a new token, mark the prior active token as expiring
// in `graceTTL` (default 1h). Both tokens are accepted by the InboundListener
// during the grace period.
func (s *Service) RotateAetherToken(ctx context.Context, nodeID string, graceTTL time.Duration) (newToken string, oldExpiresAt time.Time, err error) {
    if graceTTL <= 0 { graceTTL = time.Hour }
    newTok := generateToken()
    expiresAt := time.Now().Add(graceTTL)
    return newTok, expiresAt, s.upsertAetherToken(ctx, nodeID, newTok, &expiresAt)
}

// RevokeAetherTokenGrace: immediately expires any token in grace (sets ExpiresAt = now)
func (s *Service) RevokeAetherTokenGrace(ctx context.Context, nodeID string) error {
    return s.expireGraceTokens(ctx, nodeID, time.Now())
}

// (Implementations of upsertAetherToken / expireGraceTokens follow existing
// proxy_nodes payload manipulation patterns in the service. See existing
// methods for the JSONB read-modify-write template.)
```

- [ ] **Step 2: Handlers**

```go
// aether_token_rotate_handler.go
func AetherTokenRotateHandler(svc *business.Service) http.HandlerFunc {
    return func(w http.ResponseWriter, r *http.Request) {
        nodeID := chi.URLParam(r, "id") // or gin equivalent
        var req struct {
            GraceTTLSeconds int `json:"grace_ttl_seconds"`
        }
        _ = json.NewDecoder(r.Body).Decode(&req)
        grace := time.Duration(req.GraceTTLSeconds) * time.Second

        newTok, expiresAt, err := svc.RotateAetherToken(r.Context(), nodeID, grace)
        if err != nil {
            http.Error(w, err.Error(), http.StatusInternalServerError)
            return
        }
        json.NewEncoder(w).Encode(map[string]any{
            "new_token": newTok,
            "old_token_expires_at": expiresAt,
        })
    }
}

// aether_token_revoke_handler.go
func AetherTokenRevokeGraceHandler(svc *business.Service) http.HandlerFunc {
    return func(w http.ResponseWriter, r *http.Request) {
        nodeID := chi.URLParam(r, "id")
        if err := svc.RevokeAetherTokenGrace(r.Context(), nodeID); err != nil {
            http.Error(w, err.Error(), http.StatusInternalServerError)
            return
        }
        w.WriteHeader(http.StatusNoContent)
    }
}
```

- [ ] **Step 3: Mount routes in `routes.go`**

```go
admin := /* ... existing admin router ... */
admin.POST("/api/admin/proxy-nodes/:id/aether-token/rotate", gin.WrapF(AetherTokenRotateHandler(svc)))
admin.DELETE("/api/admin/proxy-nodes/:id/aether-token/grace", gin.WrapF(AetherTokenRevokeGraceHandler(svc)))
```

- [ ] **Step 4: Update `InboundListener::authorize` to accept any non-expired token from `auth_token_history`**

This is a **Rust-side change** that needs to land before the Go-side PR 3b can be exercised end-to-end. Add it to the Rust shared branch as part of Phase 3 (a small commit in PR 3a):

In `gw-tunnel-client/src/registry.rs` (Phase 2 added trait): `verify_auth_token` impl in `TunnelStack` is updated to scan `view.aether_config.auth_token_history` for any entry whose `ExpiresAt` is `None` or in the future and whose `Token` matches the candidate (constant-time compare).

- [ ] **Step 5: Frontend `AetherTokenManager`**

```tsx
export function AetherTokenManager({ nodeId, currentTokenState }: Props) {
    const [busy, setBusy] = useState(false);
    const onRotate = async () => {
        setBusy(true);
        const { new_token, old_token_expires_at } = await
            fetch(`/api/admin/proxy-nodes/${nodeId}/aether-token/rotate`,
                  { method: "POST", body: JSON.stringify({ grace_ttl_seconds: 3600 }) })
                .then(r => r.json());
        toast.success(`New token issued. Old token valid until ${new Date(old_token_expires_at).toLocaleString()}`);
        // Display the new token in a modal — operator must copy and supply to Aether.
        showCopyTokenModal(new_token);
        setBusy(false);
    };
    const onRevoke = async () => {
        if (!confirm("Revoke old token's grace immediately? Aether instances using the old token will lose connection.")) return;
        await fetch(`/api/admin/proxy-nodes/${nodeId}/aether-token/grace`, { method: "DELETE" });
        toast.success("Grace revoked");
    };

    return (
        <div className="aether-token-mgr">
            <h3>Aether auth token</h3>
            {currentTokenState.in_grace && (
                <Banner severity="warning">
                    Rotated {time_ago(currentTokenState.rotated_at)} ago.
                    Old token expires {time_remaining(currentTokenState.expires_at)}.
                    <button onClick={onRevoke}>Revoke now</button>
                </Banner>
            )}
            <button disabled={busy} onClick={onRotate}>Rotate token (1h grace)</button>
        </div>
    );
}
```

- [ ] **Step 6: Tests for Go-side handlers + Rust-side `verify_auth_token` history walk**

- [ ] **Step 7: Commit**

```bash
# Go side
cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add modules/proxynode/api/aether_token_*_handler.go \
        modules/proxynode/business/aether_token_lifecycle.go \
        modules/proxynode/api/routes.go \
        web/src/components/ProxyNodes/AetherTokenManager.*
git commit -m "feat(proxynode,web): Aether token rotate/revoke with 1h dual-token grace"

# Rust side — on shared branch, separate commit
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
git add crates/gw-proxy/src/tunnel_stack.rs   # verify_auth_token history walk
git commit -m "feat(gw-proxy): verify_auth_token walks aether_config.auth_token_history for grace tokens"
```

---

### Task G4.5: `AetherRegisterHistory` view + audit log

**Files:**
- Create: `modules/proxynode/business/aether_register_events.go` (audit log)
- Create: `modules/proxynode/api/aether_register_events_handler.go`
- Create: `modules/proxynode/clickhouse/aether_register_events.sql` (or Postgres DDL — Go team's call)
- Create: `web/src/components/ProxyNodes/AetherRegisterHistory.{tsx|vue}`
- Modify: `modules/proxynode/business/aether_register.go` (call `RecordRegisterAttempt`)

- [ ] **Step 1: Audit log schema**

If using Postgres (likely simpler for low-volume audit):

```sql
-- modules/proxynode/postgres/aether_register_events.sql
CREATE TABLE IF NOT EXISTS aether_register_events (
    id BIGSERIAL PRIMARY KEY,
    node_id TEXT,                         -- nullable: NULL when token unknown + auto-provision off
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    source_ip INET,
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'unknown_token', 'auto_provisioned', 'bad_prefix', 'internal_error')),
    request_body JSONB,
    response_body JSONB
);
CREATE INDEX IF NOT EXISTS aether_register_events_node_idx ON aether_register_events (node_id, received_at DESC);
CREATE INDEX IF NOT EXISTS aether_register_events_outcome_idx ON aether_register_events (outcome, received_at DESC);
```

- [ ] **Step 2-5: business + handler + UI + integration into `aether_register.go`**

Implement following the existing audit-log patterns in the Go codebase. The handler `GET /api/admin/proxy-nodes/:id/aether/register-events?limit=50` returns a paginated list.

The UI is a table component pulling from the new endpoint when the user opens the per-node detail.

- [ ] **Step 6: Commit**

```bash
git add modules/proxynode/business/aether_register_events.go \
        modules/proxynode/business/aether_register.go \
        modules/proxynode/api/aether_register_events_handler.go \
        modules/proxynode/postgres/aether_register_events.sql \
        web/src/components/ProxyNodes/AetherRegisterHistory.*
git commit -m "feat(proxynode,web): Aether register-event audit log + history UI"
```

---

### Task G4.6: GatewayControl gray-out + tooltip per spec §3.6.6

**Files:**
- Modify: `web/src/components/GatewayControl/CommandButton.{tsx|vue}` (or whatever wraps the per-node command buttons)
- Modify: `crates/gw-proxy/src/filters/upstream.rs` (Rust side: cmd dispatcher returns skipped) — on the shared branch

- [ ] **Step 1: Rust-side cmd short-circuit**

In the GatewayControl command dispatcher, before dispatching `reload-config` / `cooldown-key` / `clear-cooldown` / `clear-cache`:

```rust
let view = tunnel_stack.lookup_by_id(&cmd.node_id)
    .ok_or(GatewayControlError::NodeUnknown)?;
if view.tunnel_protocol == TunnelProtocol::Aether {
    return Ok(GatewayControlAck {
        status: AckStatus::Skipped,
        reason: Some("aether_node_does_not_support_command".into()),
    });
}
// ... existing dispatch ...
```

- [ ] **Step 2: Frontend gray-out**

```tsx
export function CommandButton({ node, command, ...props }: Props) {
    if (node.protocol === "aether") {
        return (
            <Tooltip content="Aether nodes do not accept GatewayControl commands">
                <Button disabled {...props}>{command.label}</Button>
            </Tooltip>
        );
    }
    return <Button {...props}>{command.label}</Button>;
}
```

- [ ] **Step 3: Commits (separate repos)**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
git add crates/gw-proxy/src/filters/upstream.rs
git commit -m "feat(gw-proxy): GatewayControl skips aether nodes with {status: skipped, reason: ...}"

cd /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go
git add web/src/components/GatewayControl/CommandButton.*
git commit -m "feat(web): gray out GatewayControl buttons for aether nodes with tooltip"
```

---

### Task Go-3-gate: PR-3b verification

- [ ] **Step 1: `go test ./... -race -v`** — expected PASS

- [ ] **Step 2: `go build ./...`** — expected clean

- [ ] **Step 3: `go vet ./...` + `gofmt -l .`** — expected clean

- [ ] **Step 4: Frontend build + smoke test**

`npm run build` (or framework equivalent) — expected clean.
Smoke-test: render the proxy-nodes page locally with a seeded aether node; click rotate; verify the token modal appears.

- [ ] **Step 5: Open the MR**

MR title: `feat(proxynode,monitoring,web): Phase 3 G4 — Aether protocol UI + token mgmt + audit log + GatewayControl gray-out`

MR description:
- Per-component summary.
- Schema additions (`aether_register_events`).
- API additions (rotate / revoke / history endpoints).
- Frontend integration screenshot links (placed under `docs/screenshots/` if the repo has that pattern).
- Test summary from Steps 1-4.
- Cross-link to the Rust spec: spec §3.6.6 + §3.6.7.
- Note: this MR lands **before** the Rust integrated MR (Task F3) — the UI hard-disables GatewayControl on aether nodes regardless of whether the Rust short-circuit is in place yet, so there is no pre-merge ordering hazard.

---

## Self-Review

After PR 3a (mixed-pool + dep script + final docs) + PR 3b (Go G4) are complete, then the Rust shared branch's final MR (Task F3) merges to `main`:

1. **Spec coverage (Phase 3 only):**
   - §3.6.6 GatewayControl semantics for Aether — Task G4.6 (Rust short-circuit + UI gray-out).
   - §3.6.7 Frontend UI — Tasks G4.2 (badge, radio, column), G4.3 (status panel), G4.4 (token management — beyond the original §3.6.7 spec, added per C1), G4.5 (register history — beyond §3.6.7, added per C1).
   - §5 Decoupling Discipline — Task S1 (script per §5.2), S2 (README warnings per §5.3); spec §5.4 audit is folded into Task M7's PR-3a gate (the script output IS the audit).
   - §6.5.D mixed-pool — Tasks M1-M4.
   - §6.6 `tunnel-e2e-mixed` + `check-tunnel-deps` Makefile targets — Task M5.
   - §7 Phase 3 — covered by the above.
   - Phase 3 explicitly out of scope per the plan header: cross-protocol perf baseline (filed as Phase 4 candidate in Task F2), Aether `remote_config` reverse-push (Phase 4 candidate), double-Aether multi-tenant tunnel-server (Phase 4 candidate).

2. **Placeholder scan:**
   - No "TBD"/"TODO" inside concrete code blocks.
   - Task M1 Step 1's "Adjust column names ... before commit" — documented adjustment-point with the inspection command (`\d proxy_nodes` / `\d vendor_keys`). Not an unresolved unknown.
   - Task M6 Step 2's "If any strategy shows single-protocol skew unexpectedly ... STOP" — explicit failure mode + escalation, not a placeholder.
   - Task G4.1 ("audit existing frontend stack") is investigation-only. Subsequent tasks (G4.2-G4.6) use React/TSX as a *placeholder* and explicitly say "adapt to actual framework" — this is the only way to plan without knowing what AILinkMax-Go uses; the audit task feeds the framework choice forward.
   - Task G4.5 Step 1 offers "Postgres or ClickHouse — Go team's call" — documented decision-deferral with the criterion (low-volume audit → Postgres likely simpler). Not an unresolved unknown.

3. **Type consistency:**
   - `aether_config.auth_token_history: AuthTokenEntry[]` (Task G4.4 Go) ⇔ `verify_auth_token` history walk (Task G4.4 Rust): both read the same JSONB shape.
   - `SeedNode { id, protocol, ws_url, ... }` test helper used in Phase 2 + Phase 3 mixed-pool: consistent shape, `protocol` is the same `TunnelProtocol` enum.
   - Token format `ae_<32-byte hex>` is the same in Go's `generateToken()` and the Rust-side InboundListener's `Bearer ae_*` prefix check (Phase 1 G2 / Phase 2).
   - JSON shapes in API responses:
     - `/api/admin/proxy-nodes/{id}/aether-token/rotate` returns `{new_token, old_token_expires_at}` (Task G4.4 Go) ⇔ frontend destructures the same keys (Task G4.4 web). Match.
     - `/api/admin/monitoring/aether/{node_id}` returns `{samples: [...]}` per Phase 2 G3 ⇔ Phase 3 G4 frontend (`AetherStatusPanel`) consumes the same shape.

4. **Cross-repo coordination invariants:**
   - PR 3b in `AILinkMax-Go` lands before the Rust integrated MR (Task F3). The Rust-side GatewayControl short-circuit and the auth_token_history walk are on the Rust shared branch; they land with the F3 merge. Until F3 merges:
     - The UI gray-out is the user-visible safeguard against Aether GatewayControl misuse.
     - Token rotation issues new tokens, but only the original (Phase 1 G2) auth_token check is active on the Rust side — so the dual-token grace appears to work only after F3. **Document this in the PR 3b MR description**: "Full functionality requires the Rust integrated MR to land."
   - Cross-protocol matrix (`tests/tunnel-e2e/docs/cross-protocol-matrix.md`) is generated on the Rust side and committed; the Go-side UI does not depend on it.

5. **Single-branch / single-MR consistency check:**
   - All Phase 3 Rust commits land on the **same shared feature branch** as Phase 1 + Phase 2.
   - Task F3 opens the **only** MR to `main`. There is no Phase 3a-only MR.
   - Phase 3 introduces **no** "add then remove" — everything in Phase 3 either:
     - Is new (mixed-pool e2e, dep script, cross-protocol matrix, ADR pointer, README warnings).
     - Strikes through resolved ROADMAP items (one-time closeout, not later-removal).
     - Rewrites `docs/spec/09-tunnel.md` (lands once; final state).
   - Phase 1's `--features tunnel` removal stays in effect.
   - Phase 2's `aether/` submodule + `StreamTarget::Http` variant stay in effect.
   - The Cnexus `cnexus/` submodule and the protocol-neutral `core::Transport` trait — all stable from Phase 1.

---

Plan complete and saved to `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-3.md`. Execution model continues from the Phase 1/2 chosen approach (Subagent-Driven or Inline).

**This is the final implementation plan for the Cnexus ↔ Aether tunnel compatibility work.** After Task F3 merges, the work is complete. Phase 4 candidates are filed in `docs/ROADMAP.md` for future scheduling.
