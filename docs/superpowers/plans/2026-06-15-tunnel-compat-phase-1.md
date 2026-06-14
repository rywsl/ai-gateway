# Tunnel Compat Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 1 of the Cnexus ↔ Aether tunnel compatibility spec: add a `TunnelProtocol` discriminator to `ProxyNode`, reorganize `gw-tunnel-client` so existing transport code lives under a `cnexus/` submodule, and stand up the `tests/tunnel-e2e/` Docker harness with the 3 Cnexus-protocol scenarios that were previously skipped — without changing any wire behaviour.

**Architecture:** Two landable PRs.
- **PR 1a (in this plan, Tasks 1-13)** introduces the discriminator, the migration, and the source-layout move. Default value is `cnexus`, deserialization is forward-compatible, runtime behaviour is unchanged.
- **PR 1b (Tasks 14-26)** introduces `tests/tunnel-e2e/` with its own `docker-compose.yml` (no Aether services yet — that lands in Phase 2), exercises the 3 currently-skipped tunnel scenarios via a `gw-tunnel-server` container, adds `Makefile` targets, and registers the Aether submodule with `update = none` for Phase 2 to consume.

**Tech Stack:** Rust 1.88 (pinned), tokio, serde, sqlx, pytest 8.x, Postgres 16, Redis 7, Docker Compose v2.

**Reference spec:** `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` (§1, §2, §3.1, §3.2, §6.1, §6.3, §6.4, §6.5 A-scenarios, §7 Phase 1).

**Out of scope for this plan (deferred to Phase 2/3 plans):** Aether wire protocol (`aether/` submodule), `InboundListener`, `/api/internal/proxy-tunnel` route, `Transport` trait + `AnyTransport` enum, `StreamTarget` two-variant enum, Aether Docker images, Aether scenarios, mixed-pool scenarios, dep-boundary script, Go-side cross-repo PRs.

**Verification policy:** Per project convention all `cargo` compile/test commands run via `cargo-remote-ctl` (remote build runner). `docker compose` / `make tunnel-e2e` / `make e2e` / `psql` / shell utilities run locally. Migrations are **not** auto-applied (see `CLAUDE.md`): the tunnel-e2e harness applies them via a shell runner pre-baked into Phase 1.

---

## File Structure

### New files (PR 1a)

| Path | Responsibility |
|---|---|
| `crates/gw-core/src/tunnel_protocol.rs` | New `TunnelProtocol` enum (`Cnexus` \| `Aether`) with `Default = Cnexus`. Distinct from existing `gw-core::protocol::ProtocolKind` which discriminates vendor API families (OpenAi/Anthropic/Gemini/Kiro). |
| `crates/gw-tunnel-client/src/cnexus/mod.rs` | New submodule that re-exports the contents of `frame.rs`, `pool.rs`, `transport.rs`. |
| `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql` | Backfill payload `protocol` key on existing rows; add partial index on `payload->>'protocol'`. |

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

### Files modified (PR 1a)

| Path | Change |
|---|---|
| `crates/gw-core/src/lib.rs` | `pub mod tunnel_protocol;` |
| `crates/gw-core/src/entity.rs` | Add `pub tunnel_protocol: TunnelProtocol` field to `ProxyNode` with `#[serde(default)]`. |
| `crates/gw-tunnel-client/src/lib.rs` | Replace `pub mod {frame, pool, transport};` with `pub mod cnexus;` plus back-compat re-exports `pub use cnexus::{frame, pool, transport};`. Keep `resolver`/`safe_dns`/`target_filter` mod lines unchanged. |
| `crates/gw-tunnel-client/src/resolver.rs` | Add `pub tunnel_protocol: gw_core::tunnel_protocol::TunnelProtocol` to `ProxyNodeView`. |
| `crates/gw-tunnel-client/src/cnexus/{frame,pool,transport}.rs` | Update internal `crate::frame::` / `crate::pool::` / `crate::transport::` paths to `crate::cnexus::frame::` etc. |
| `crates/gw-gateway/src/lib.rs` | `tunnel_refresher` reads `payload.tunnel_protocol` from `ProxyNode` and propagates into `ProxyNodeView`. |
| `docs/spec/09-tunnel.md` | New subsection "Per-node protocol discriminator" documenting the field, the default, and the future Aether use. |
| `docs/architecture.md` | Append a one-line entry under the `gw-tunnel-client` crate row noting the `cnexus/` submodule. |

### New files (PR 1b)

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

### Files modified (PR 1b)

| Path | Change |
|---|---|
| `Makefile` | New PHONY targets `tunnel-e2e`, `tunnel-e2e-up`, `tunnel-e2e-down`, `tunnel-e2e-logs`, `tunnel-e2e-deps`. |

---

## PR 1a · `TunnelProtocol` discriminator, migration, `cnexus/` reorg

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

Run: `cargo-remote-ctl build --workspace --features tunnel`
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

// Back-compat re-exports so call sites built before the cnexus/ reorg still
// resolve. Remove these once all downstream call sites use the cnexus::
// path explicitly (tracked in the Phase 2 plan).
pub use cnexus::{frame, pool, transport};

pub use resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};
```

- [ ] **Step 5: Move the integration tests so test names stay readable**

Run:
```bash
git mv crates/gw-tunnel-client/tests/frame.rs     crates/gw-tunnel-client/tests/cnexus_frame.rs
git mv crates/gw-tunnel-client/tests/pool.rs      crates/gw-tunnel-client/tests/cnexus_pool.rs
git mv crates/gw-tunnel-client/tests/transport.rs crates/gw-tunnel-client/tests/cnexus_transport.rs
```

The integration tests use `use gw_tunnel_client::frame::...` etc. which still resolve via the back-compat re-exports — no edits to the test bodies needed.

- [ ] **Step 6: Compile the crate**

Run: `cargo-remote-ctl build -p gw-tunnel-client`
Expected: clean build.

- [ ] **Step 7: Run every tunnel-client test**

Run: `cargo-remote-ctl test -p gw-tunnel-client`
Expected: PASS, identical test names to before the move except the three renames above.

- [ ] **Step 8: Workspace-level compile + tests with `--features tunnel`**

Run: `cargo-remote-ctl test --workspace --features tunnel`
Expected: PASS. This is the gate that proves no downstream crate's compile broke. Capture the final 5 lines of output for the PR description.

- [ ] **Step 9: Commit**

```bash
git add crates/gw-tunnel-client
git commit -m "refactor(gw-tunnel-client): move frame/pool/transport into cnexus/ submodule"
```

---

### Task 7: Sync the architecture and tunnel spec docs

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/spec/09-tunnel.md`

> Per `CLAUDE.md` §Sync rule, crate-layout changes and tunnel subsystem changes require these docs to be updated in the same PR.

- [ ] **Step 1: Patch `docs/architecture.md`**

Search for the existing description of `gw-tunnel-client`. Append one sentence:

```
Phase 1 of the tunnel-compat work (2026-06-15) moves the Cnexus-protocol
codec, pool, and transport into the `cnexus/` submodule; `resolver`,
`safe_dns`, and `target_filter` remain at the crate root because they
are protocol-agnostic.
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
```

- [ ] **Step 3: Commit**

```bash
git add docs/architecture.md docs/spec/09-tunnel.md
git commit -m "docs(tunnel): record cnexus/ reorg and tunnel_protocol field"
```

---

### Task 8: PR-1a verification gate

- [ ] **Step 1: Run the dep-only build to surface any forgotten consumer**

Run: `cargo-remote-ctl check --workspace --all-features`
Expected: clean. `--all-features` is broader than `--features tunnel` and catches code that may be feature-gated elsewhere.

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

PR title: `feat(tunnel): Phase 1a — TunnelProtocol discriminator, migration 0011, cnexus/ reorg`

PR body should include:
- One-line summary of each commit.
- The captured 5-line `cargo-remote-ctl test --workspace --features tunnel` tail from Task 6 Step 8.
- The captured `make e2e` summary from Step 5 above.
- Link to the spec: `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`.

---

## PR 1b · `tests/tunnel-e2e/` skeleton + 3 Cnexus-protocol scenarios

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
        CARGO_FEATURES: "--features tunnel"
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
      args:
        CARGO_FEATURES: "--features tunnel"
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
tests/e2e/client/scenarios/test_tunnel.py, which has been skipped behind
`pytestmark = pytest.mark.skip(reason='requires --features tunnel build')`.
Phase 1 brings them back into a `--features tunnel` build that runs against a
real `gw-tunnel-server` container.

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

> Do not adjust the gateway code or shipping config to make the tests pass — Phase 1 is supposed to be wire-identical to the prior behavior. If a real bug surfaces, file it and decide whether to fix in PR 1b or split off.

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

### Task 19: PR-1b verification gate

- [ ] **Step 1: Confirm `make tunnel-e2e` passes from a clean tree**

Run:
```bash
make tunnel-e2e-down
docker volume prune -f
make tunnel-e2e
```
Expected: `3 passed`. This is the gate the reviewer will replay.

- [ ] **Step 2: Re-run `cargo-remote-ctl test --workspace --features tunnel`**

Expected: unchanged from PR 1a baseline. (`tests/tunnel-e2e/` is Python — it should not affect the cargo workspace.)

- [ ] **Step 3: Confirm `make e2e` still green**

Expected: 25 passed.

- [ ] **Step 4: Open PR 1b**

PR title: `feat(tunnel-e2e): Phase 1b — Docker harness + 3 cnexus scenarios`

PR body should include:
- The captured `make tunnel-e2e` green summary.
- The captured `make e2e` green summary.
- Note that the Aether submodule is registered with `update = none` per Phase 2 plan; Phase 1 does not exercise it.
- Link to the spec.

---

## Self-Review

After PR 1a + PR 1b merge:

1. **Spec coverage (Phase 1 only):**
   - §3.1 migration 0011 — Task 3.
   - §3.2 `TunnelProtocol` enum + `ProxyNodePayload.protocol` field — Tasks 1 & 2.
   - §6.1 directory layout (Phase 1 subset) — Tasks 9-15 cover compose, seed, fixtures, scenarios, README; the Aether-specific files are explicitly deferred to Phase 2.
   - §6.3 docker-compose (Phase 1 subset) — Task 10.
   - §6.4 seed_nodes fixture — Task 13.
   - §6.5.A 3 scenarios — Task 15.
   - §6.6 Makefile targets — Task 16.
   - §7 Phase 1 PR-1a / PR-1b breakdown — Tasks 1-8 / 9-19.
   - Phase 1 explicitly out of scope per the plan header: §3.3 (StreamTarget), §3.4 (Transport trait), §3.5 (InboundListener), §4 (Aether internals), §5 (dep boundary), §6.5.B-D scenarios, §3.6 (Go management plane). These are in the Phase 2 / Phase 3 plans (to be written after Phase 1 lands).

2. **Placeholder scan:**
   - No "TBD"/"TODO" inside concrete code blocks.
   - Task 11 Step 3 notes "schema may differ — adjust to match production" — this is a real, documented adjustment-point against the running schema, not a placeholder. The plan tells the engineer exactly where to look (`crates/gw-core/src/entity.rs`) to confirm the field names.
   - Task 14 Step 1 notes the `require_tunnel` header is illustrative and points to the real surface (`VendorKey.tunnel_preference.require_tunnel`) — again, a documented adjustment-point with a concrete file reference.

3. **Type consistency:**
   - `TunnelProtocol` (gw-core), `ProxyNode.tunnel_protocol` (gw-core entity), `ProxyNodeView.tunnel_protocol` (gw-tunnel-client resolver), payload key `"tunnel_protocol"` — all four match.
   - Values: `"cnexus"` (default) / `"aether"` consistent in SQL, Rust serde, Python seed.
   - Service names in compose match the URLs the Python tests use: `cnexus-tunnel-server:7000`, `ali-gateway:8000`, `fake-upstream:9000`.

---

Plan complete and saved to `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-1.md`. Two execution options:

**1. Subagent-Driven (recommended)** — A fresh subagent runs each task, I review between tasks. Fast iteration; protects this session's context for coordination across all three phases.

**2. Inline Execution** — Execute tasks in this session via the executing-plans skill, with checkpoints for review.

Which approach?
