# Cnexus ↔ Aether Tunnel Compatibility Design

Status: Approved (brainstorming complete, awaiting implementation plan)
Date: 2026-06-15
Owners: gateway team
Related: `docs/spec/09-tunnel.md` (existing tunnel spec)

## 1. Background & Goals

The current ailinkmax-gateway workspace (project name: **Cnexus**) ships a tunnel subsystem with two components:

- `gw-tunnel-server` — a standalone binary deployed on remote VPSes acting as the **proxy node**. Speaks the Cnexus wire protocol (TCP-relay semantics).
- `gw-tunnel-client` — a library linked into the hub (`gw-gateway`). Drives connection pools to remote proxy nodes via `gw-proxy::tunnel_stack` + `gw-proxy::tunnel_bridge`.

A reference project, **Aether**, exists with its own proxy and hub binaries. Aether's proxy node speaks a different wire protocol (HTTP-relay semantics, 10-byte little-endian framing, JSON `RequestMeta`/`ResponseMeta`, Bearer-token WS-upgrade auth). The two protocols are incompatible at every layer (frame header size, byte order, opcode semantics, auth flow, heartbeat format).

This design enables the Cnexus hub to drive **either** a Cnexus-protocol proxy node **or** an Aether-protocol proxy node, on a per-node basis, without changes to upstream filter logic. It also introduces a dedicated tunnel end-to-end (e2e) test suite that exercises both protocol paths against real Docker containers, and codifies a crate-dependency boundary so the tunnel subsystem remains independently extractable in the future.

### 1.1 Goals

1. **Cnexus hub can drive an unmodified Aether proxy node.** Unidirectional compatibility: data flows from hub through Aether proxy to upstream. The WS connection is initiated by the proxy (Aether's standard outbound-only model); the hub accepts the inbound at `/api/internal/proxy-tunnel`. HTTP requests dispatched to a node marked `protocol="aether"` traverse the Aether wire protocol end-to-end and reach upstream byte-identical to a direct request.
2. **First-class Docker-based tunnel e2e suite** (`tests/tunnel-e2e/`) verifies both protocol paths and mixed-node-pool behavior. This suite is **local-only** (no CI integration); developers run it on demand via `make tunnel-e2e`.
3. **Dependency boundary discipline**: `gw-tunnel-client` and `gw-tunnel-server` may not depend on any business crate (`gw-store`, `gw-auth`, etc.). A shell script enforces this. The discipline prepares the tunnel subsystem for eventual extraction into an independent project.

### 1.2 Non-Goals

- **Reverse compatibility**: making Aether hub drive Cnexus proxy nodes. Out of scope.
- **HeartbeatAck reverse-push**: implementing Aether's `remote_config` / `upgrade_to` reverse-push semantics. Decoded but not applied; logged at debug level.
- **CI integration of tunnel-e2e**: the suite is local-only by design.
- **Soak / long-run tests** for the tunnel path (deferred).
- **Real LLM API / mTLS** validation through the tunnel.
- **Aether-side or Aether-hub modifications**: this design touches only the Cnexus repo (plus Go-side schema changes noted in §3.1, tracked as a parallel cross-repo PR).
- **Frontend / UI** exposure of the new `protocol` field in `AILinkMax-Go` admin panels (separate PR in that repo).

### 1.3 Success Criteria

- `make tunnel-e2e` passes 15 scenarios locally: 3 Cnexus-protocol availability cases + 8 Aether compatibility cases + 1 Aether native baseline + 3 mixed-pool stress cases.
- `make check-tunnel-deps` reports 0 boundary violations.
- All existing tests continue to pass: `cargo-remote-ctl test --workspace --features tunnel` is green; `make e2e` is green (25 existing scenarios unchanged).

## 2. Architecture

### 2.1 Connection Direction Asymmetry

A critical architectural fact governs the rest of the design: **Cnexus and Aether tunnels initiate WebSocket connections in opposite directions.**

- **Cnexus protocol**: the **hub dials out** to a `gw-tunnel-server` instance listening on a VPS at `node.ws_url`. The proxy node listens; the hub is the WS client.
- **Aether protocol**: the **proxy node dials out** to a hub endpoint at `/api/internal/proxy-tunnel`. The hub listens; the Aether proxy is the WS client. (Aether's design states `aether-proxy` requires no inbound port and only outbound connectivity to the hub.)

This means Cnexus hub must play two distinct roles simultaneously:

- As an **outbound WS client** to drive every Cnexus-protocol node (existing behavior).
- As an **inbound WS server** at `/api/internal/proxy-tunnel` to accept dialing Aether-protocol proxy nodes.

Both transports still implement the same `trait Transport` and present identical semantics to `tunnel_bridge`. The difference is how the underlying WS connection comes into existence.

### 2.2 Component Map

```
                       ┌──────────────────────────────────┐
                       │ gw-gateway (lib + bin)            │
                       │  · tunnel_refresher loads nodes   │   ← reads DB, including new `protocol` field
                       │  · spawns aether::InboundListener │   ← new for Aether
                       │  · constructs TunnelStack         │
                       └──────────┬───────────────────────┘
                                  │
                       ┌──────────▼───────────────────────┐
                       │ gw-proxy                          │
                       │  · tunnel_stack::peer_for_node    │
                       │  · tunnel_bridge: loopback HTTP/1 │
                       │  · filters::upstream selects node │
                       └──────────┬───────────────────────┘
                                  │ calls unified Transport trait
                       ┌──────────▼───────────────────────┐
                       │ gw-tunnel-client                  │
                       │  ┌─────────────┐ ┌──────────────┐ │
                       │  │ cnexus::    │ │ aether::     │ │
                       │  │   frame     │ │   frame      │ │
                       │  │   transport │ │   transport  │ │
                       │  │   heartbeat │ │   heartbeat  │ │
                       │  │   (dials)   │ │   meta(JSON) │ │
                       │  │             │ │   gzip       │ │
                       │  │             │ │   http_adapter│
                       │  │             │ │   listener   │ │ ← accepts inbound
                       │  └─────────────┘ └──────────────┘ │
                       │  trait Transport                  │
                       │  enum AnyTransport { Cnexus,Aether}│
                       │  factory::build_or_attach         │
                       └──────────────────────────────────┘
        outbound dial │                                     │ accepts inbound
                      ▼                                     ▲
            gw-tunnel-server                          aether-proxy
            (listens on VPS)                          (dials Cnexus hub)
            built in this repo                        built from submodule
```

### 2.3 Key Invariants

- **Protocol divergence is sealed inside `gw-tunnel-client`.** Upstream code (`gw-proxy`, `gw-gateway`) selects a node and asks the transport layer to open a stream. The transport layer dispatches to the correct protocol implementation based on `tunnel_protocol` carried in the node view.
- **Connection direction is also sealed inside `gw-tunnel-client`.** Outside callers do not know who initiated the WS; they only see a `Transport` trait object.
- **`tunnel_bridge` learns about protocol kind only once**, when it looks up the transport handle by `node_id`. It then branches between two code paths:
  - `tunnel_protocol=Cnexus`: forwards the loopback HTTP/1 byte stream as-is into a `StreamTarget::Tcp` (current behavior).
  - `tunnel_protocol=Aether`: parses the loopback HTTP/1 stream with `httparse`, constructs a `StreamTarget::Http`, lets the transport translate it to Aether frames, and serializes the response back to HTTP/1 bytes for Pingora.
- **The `cnexus/` module is bit-compatible with the existing `gw-tunnel-server` wire protocol** (14-byte big-endian header with magic `[0xAE, 0x21]` + version + msg_type + flags + stream_id + length). No protocol change for existing nodes.
- **The `aether/` module is bit-compatible with the Aether wire protocol** as observed in the Aether reference source: 10-byte **big-endian** header (matches `aether-proxy/src/tunnel/protocol.rs:46` `u32::from_be_bytes` and `aether-hub/src/protocol.rs:89` `BytesMut::put_u32`), `RequestMeta`/`ResponseMeta` JSON, gzip flag 0x02, Bearer-token WS upgrade auth at the App-layer path `/api/internal/proxy-tunnel` (which the Cnexus hub mounts directly; in Aether's deployment topology this path lives on the App layer that proxies to hub's `/proxy` internal route, but Cnexus collapses both layers).
- **`aether::InboundListener` is the only new long-running service** the hub spawns. It is started by `gw-gateway` when at least one Aether-protocol node exists (or when `tunnel.aether_listener_enabled` config is true).

### 2.4 Change Footprint Summary

**Cnexus Rust repo:**

| Crate / path | Change kind | Phase |
|---|---|---|
| `crates/gw-tunnel-client/src/cnexus/` | new submodule housing existing outbound-dialing transport logic | 1 |
| `crates/gw-tunnel-client/src/aether/` | new submodule (frame / transport / heartbeat / meta / gzip / http_adapter / listener) | 2 |
| `crates/gw-tunnel-client/src/lib.rs` | export `trait Transport`, `AnyTransport`, `factory::{build_cnexus_supervised, attach_aether}` | 2 |
| `crates/gw-tunnel-client/src/target.rs` | `enum StreamTarget { Tcp, Http }` | 2 |
| `crates/gw-proxy/src/tunnel_stack.rs` | `ProxyNodeView.protocol`; DashMap holds `Arc<dyn Transport>`; `peer_for_node` returns kind; new `attach_inbound_transport(node_id, transport)` + `on_protocol_change(node_id, new_kind)` + `drop_node(node_id)` APIs | 2 |
| `crates/gw-proxy/src/tunnel_bridge.rs` | per-protocol branching at accept | 2 |
| `crates/gw-gateway/src/lib.rs` | `tunnel_refresher` reads `payload.protocol`, dispatches outbound (Cnexus supervisor) vs InboundListener (Aether); manages listener auto-enable per §3.5 rule; consumes `events:entity` for protocol transitions; pushes `events:aether_heartbeat` (§3.6.5) | 2 |
| `crates/gw-gateway/src/routes.rs` | new route `/api/internal/proxy-tunnel` → delegates to `aether::InboundListener::accept`; extends `/healthz/tunnel` with per-node `present/alive` schema (§3.5) | 2 |
| `crates/gw-gateway` SIGTERM handler | drain Aether (GoAway + close) + Cnexus (skip-reconnect) + grace timeout | 2 |
| `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql` | backfill + partial index | 1 |
| `crates/gw-store/src/entity/proxy_node.rs` | add `protocol: TunnelProtocol` field | 1 |
| `config/default.toml` | new `[tunnel.aether_listener]` section (`enabled`, `path`, `max_concurrent_connections`); new `tunnel.shutdown_grace_secs` | 2 |
| `tests/tunnel-e2e/` | full new directory | 1 + 2 + 3 |
| `Makefile` | `tunnel-e2e` / `tunnel-e2e-up` / `tunnel-e2e-down` / `tunnel-e2e-logs` / `check-tunnel-deps` targets | 1 + 3 |
| `scripts/check-tunnel-deps.sh` | dep-boundary enforcement (shell + jq) | 3 |
| `.gitmodules` | `tests/tunnel-e2e/aether-src` → `https://gitlab.ailinkmax.com/ailinkmax/Aether` | 1 |
| `docs/spec/09-tunnel.md` | new sections: `tunnel_protocol`, Aether compatibility, dep boundary | 3 |
| `tests/tunnel-e2e/README.md` | new — usage, scenario catalog, failure-mode guide | 3 |

**AILinkMax-Go repo (parallel, cross-repo PRs — coordinated per §3.6.8):**

| path | Change kind | Phase |
|---|---|---|
| `modules/proxynode/business/entity.go` | add `Protocol string` field | G1 |
| `modules/proxynode/business/service.go` | validate `protocol` enum; default `"cnexus"` | G1 |
| `modules/proxynode/api/routes.go` | `manual` / `PATCH` / `GET` accept and return `protocol` | G1 |
| `modules/proxynode/api/routes.go` | new `POST /api/admin/proxy-nodes/register` (Aether-bootstrap) | G2 |
| `modules/proxynode/business/aether_register.go` | upsert-by-token logic per §3.6.1 | G2 |
| `modules/monitoring` (or `modules/proxynode/aether_heartbeat`) | new `events:aether_heartbeat` consumer | G3 |
| `migrations-clickhouse/00NN_aether_heartbeat_samples.sql` | new ClickHouse table | G3 |
| `modules/monitoring/api/routes.go` | new `GET /api/admin/monitoring/aether/{node_id}` | G3 |
| `frontend/...` | Protocol column / radio / Aether status panel | G4 |

### 2.5 Untouched Surfaces

- `tests/e2e/` (existing 25 scenarios), `make e2e`, `make e2e-soak`: unchanged.
- Existing CI jobs: `lint-test`, `rust-build`, `rust-build-tunnel`, `push-image`, `push-tunnel-image`: unchanged. **No new CI job is introduced** for tunnel-e2e (local-only by design).
- `--features tunnel` cargo feature: retained; tunnel-e2e builds with it enabled.

## 3. Protocol Routing & Data Model

### 3.1 DB Schema

`proxy_nodes` uses the entity pattern (`payload JSONB`), so no `ALTER TABLE ADD COLUMN` is needed. The payload schema gains a `protocol` key with two valid values: `"cnexus"` (default) and `"aether"`.

Migration `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql`:

```sql
-- Up: backfill existing rows and create a partial index for debugging.
UPDATE proxy_nodes
SET payload = jsonb_set(payload, '{protocol}', '"cnexus"', true)
WHERE payload->>'protocol' IS NULL;

CREATE INDEX IF NOT EXISTS proxy_nodes_protocol_idx
  ON proxy_nodes ((payload->>'protocol'))
  WHERE deleted_at IS NULL;

-- Down:
DROP INDEX IF EXISTS proxy_nodes_protocol_idx;
UPDATE proxy_nodes
SET payload = payload - 'protocol'
WHERE payload->>'protocol' = 'cnexus';
```

**Parallel Go-side change** (out of repo, tracked separately in `AILinkMax-Go`):

- `modules/proxynode/business/entity.go`: add `Protocol string` json field.
- `modules/proxynode/business/service.go`: validate enum on `Register` / `Upsert`; default to `"cnexus"` when absent.
- `POST /api/admin/proxy-nodes/manual` and `PATCH /api/admin/proxy-nodes/{id}`: optional `protocol` input.

The Cnexus-side migration runs independently and is forward-compatible with old Go code that omits the field (defaulted in the SQL backfill and again in Rust deserialization).

### 3.2 Rust Entity & View

`crates/gw-store/src/entity/proxy_node.rs`:

```rust
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TunnelProtocol {
    Cnexus,
    Aether,
}

impl Default for TunnelProtocol {
    fn default() -> Self { Self::Cnexus }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProxyNodePayload {
    // ... existing fields
    #[serde(default)]
    pub protocol: TunnelProtocol,
}
```

`crates/gw-proxy/src/tunnel_stack.rs` adds `protocol: TunnelProtocol` to `ProxyNodeView`. `tunnel_refresher` propagates the field from payload into the view.

### 3.3 `StreamTarget` Two-Variant Enum

```rust
// gw-tunnel-client/src/target.rs

pub enum StreamTarget {
    /// Cnexus: opaque TCP stream. tunnel_bridge has already serialized HTTP into bytes.
    Tcp { host: String, port: u16, tls: bool },

    /// Aether: HTTP-semantic stream. tunnel_bridge parses HTTP/1 with httparse and fills this.
    Http {
        method: http::Method,
        url: url::Url,
        headers: Vec<(String, String)>,
        body_rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        body_tx_response: tokio::sync::mpsc::Sender<bytes::Bytes>,
        meta_tx_response: tokio::sync::oneshot::Sender<crate::aether::ResponseMeta>,
    },
}
```

- `cnexus::WsTransport::open_stream` accepts only `Tcp`; returns `TransportError::UnsupportedTarget` for `Http`.
- `aether::WsTransport::open_stream` accepts only `Http`; returns `TransportError::UnsupportedTarget` for `Tcp`.
- The branching in `tunnel_bridge` ensures the correct variant is constructed before calling `open_stream`.

### 3.4 Transport Trait & Construction Surface

```rust
// gw-tunnel-client/src/lib.rs

pub trait Transport: Send + Sync {
    fn open_stream<'a>(
        &'a self,
        target: StreamTarget,
    ) -> futures::future::BoxFuture<'a, Result<StreamHandle, TransportError>>;
    fn alive(&self) -> bool;
    /// Time since the most recent liveness signal: Pong (Cnexus) or HeartbeatData (Aether).
    /// Renamed from the original `last_pong_age` because Aether uses HeartbeatData/Ack,
    /// not Ping/Pong, for liveness.
    fn last_heartbeat_age(&self) -> Option<std::time::Duration>;
    fn tunnel_protocol(&self) -> TunnelProtocol;
    fn close(&self) -> futures::future::BoxFuture<'_, ()>;
}

pub enum AnyTransport {
    Cnexus(cnexus::WsTransport),
    Aether(aether::WsTransport),
}

impl Transport for AnyTransport { /* delegating dispatch */ }
```

Because connection direction differs per protocol, the construction surface splits into two paths:

**Outbound (Cnexus)** — used by `tunnel_refresher` when seeding nodes. The reconnect supervisor wraps the inner `cnexus::WsTransport` (not the trait object) so existing supervisor semantics are preserved:

```rust
// gw-tunnel-client/src/factory.rs

pub fn build_cnexus_supervised(
    cfg: TransportConfig,
    node: ProxyNodeView,
) -> Arc<dyn Transport> {
    // ReconnectingTransport stays type-parameterized over the concrete inner
    // transport (Phase 1 keeps the supervisor exactly as today). It implements
    // `Transport` itself, so erasing it to `Arc<dyn Transport>` is safe.
    Arc::new(ReconnectingTransport::<cnexus::WsTransport>::new(cfg, node))
}
```

**Inbound (Aether)** — used by `aether::InboundListener` when an Aether proxy dials in. No supervisor: the proxy owns reconnection, the hub just accepts:

```rust
pub async fn attach_aether(
    cfg: TransportConfig,
    node_view: ProxyNodeView,
    upgraded_ws: tokio_tungstenite::WebSocketStream<...>,
) -> Result<Arc<dyn Transport>, TransportError> {
    Ok(Arc::new(aether::WsTransport::attach(cfg, node_view, upgraded_ws).await?))
}
```

`tunnel_refresher` does not call `attach_aether` directly. For Aether nodes it only:

1. Ensures the row exists in the in-memory `proxy_nodes` snapshot (so `peer_for_node` can later look up the node by id and the listener can authenticate inbound connections against it).
2. Verifies the `InboundListener` is running (spawns it idempotently the first time an Aether node appears).

The DashMap in `tunnel_stack` becomes `DashMap<NodeId, Arc<dyn Transport>>`. For Cnexus nodes the entry is the `ReconnectingTransport` supervisor (its `Transport::alive()` reflects the inner transport's live status); for Aether nodes the entry is the bare `aether::WsTransport`, populated by `InboundListener::on_upgrade` after a successful WS handshake and removed on disconnect. Reconnection of Aether nodes is entirely driven by `aether-proxy` redialing.

### 3.5 `aether::InboundListener`

A new long-running service inside `gw-tunnel-client`. It owns:

- A reference to the `ProxyNodeRegistry` (a trait-object that exposes `lookup_by_id(node_id) -> Option<ProxyNodeView>` and `verify_auth_token(node_id, candidate) -> bool`). Implemented by `gw-proxy::tunnel_stack` so the listener never touches `gw-store` or `gw-auth` directly (boundary discipline preserved; see §5).
- A `Sender<(NodeId, Arc<dyn Transport>)>` to push attached transports back into the stack.

`gw-gateway` mounts a route at `/api/internal/proxy-tunnel` on its existing axum HTTP server that hands the upgrade off to `InboundListener::handle_upgrade(headers, ws_upgrade)`. The listener:

1. Reads `Authorization: Bearer <token>`, `X-Node-Id`, `X-Node-Name`, `X-Tunnel-Max-Streams` headers.
2. Calls `registry.lookup_by_id(node_id)`. Rejects with 404 if absent or 401 if `protocol != aether`.
3. Calls `registry.verify_auth_token(node_id, candidate)`. Rejects with 401 on mismatch.
4. Performs the WS upgrade; on success constructs `aether::WsTransport::attach(...)` and pushes it into the stack via the sender.
5. On WS close: the transport's read loop exits, the transport marks itself dead, and the stack-side cleanup task removes the DashMap entry. No retry by hub side; `aether-proxy` reconnects on its own.

Mounting the route is opt-in with a config-overrides-DB rule:

- `tunnel.aether_listener.enabled` (default `null`): if `false`, the listener is **forcibly disabled** even if Aether nodes exist in DB (operator kill-switch for emergency rollback). If `true`, always enabled. If `null` (default), auto-enable iff `tunnel_refresher` sees at least one row with `protocol = 'aether'`.
- `tunnel.aether_listener.path` (default `/api/internal/proxy-tunnel`): route path; almost never changed.
- `tunnel.aether_listener.max_concurrent_connections` (default `256`): InboundListener tracks a semaphore; if exhausted, new upgrade attempts get `503` with `Retry-After: 5`. Prevents a misbehaving fleet of proxies from exhausting hub resources.

Health surface:

- `GET /healthz/tunnel` (existing endpoint, admin-token authenticated) is extended with per-node `present` and `alive` flags. Schema:

  ```json
  {
    "nodes": [
      {
        "node_id": "aether-node-1",
        "protocol": "aether",
        "present": true,               // transport slot occupied in DashMap
        "alive": true,                 // last_heartbeat_age < threshold
        "last_heartbeat_age_ms": 12340,
        "active_streams": 7
      },
      { "node_id": "cnexus-node-1", "protocol": "cnexus", "present": true, "alive": true, ... }
    ]
  }
  ```

  `present=false` distinguishes "registered in DB but no proxy ever dialed in" from "dialed in but stale". Critical for diagnosing why an Aether node looks down.

Graceful shutdown:

- On SIGTERM, gateway initiates shutdown drain:
  1. Stop accepting new WS upgrades on the listener route (return 503 to in-progress upgrades).
  2. For each Aether transport in the DashMap, send a `GoAway` frame and close the WS. The proxy's read loop breaks, and the proxy's redial logic backs off (it sees the deliberate close).
  3. For each Cnexus transport (the reconnect supervisor), call `Transport::close()` which the supervisor honors by skipping its next reconnect attempt.
  4. Wait up to `cfg.shutdown_grace_secs` (default 10) for in-flight streams to flush `StreamEnd` from upstream, then force-close.

### 3.6 Go Management Plane Integration (`AILinkMax-Go`)

The Aether proxy's startup sequence is **HTTP-register-then-WS-dial**, not just WS-dial. Before any WebSocket upgrade, `aether-proxy` does `POST /api/admin/proxy-nodes/register` with its management token and hardware/region/version payload and waits for the response to learn its assigned `node_id` (`aether-proxy/src/registration/client.rs:96-146`). Only then does it open the WS. `node_id` is not in the proxy's config at all.

Cnexus has no native concept of "issuing node IDs to dialing proxies". This section specifies the Go-side additions that make Aether proxies bootable.

#### 3.6.1 Registration endpoint (Go side, new)

**Endpoint**: `POST /api/admin/proxy-nodes/register`
**Owner**: AILinkMax-Go web binary (`modules/proxynode`).
**Authentication**: `Authorization: Bearer ae_<token>` (the proxy's management token).

Request body (Aether's schema, copied verbatim from `aether-proxy/src/registration/client.rs:48-72`):

```json
{
  "node_name": "aether-node-01",
  "public_ip": "203.0.113.7",
  "region": "us-east-1",
  "hardware": { "cpu_cores": 4, "mem_mb": 8192, ... },
  "version": "1.4.2",
  "tunnel_capabilities": { "max_streams": 64, ... }
}
```

Response body:

```json
{
  "node_id": "aether-node-1",
  "heartbeat_interval_secs": 30,
  "config": { /* optional remote_config snapshot */ }
}
```

Go side behavior:

1. Decrypt-and-match: scan `proxy_nodes` rows with `protocol = 'aether'` and `auth_token_enc` decrypting to the supplied token. The token has an `ae_` prefix (Aether convention, see `aether-proxy/src/config.rs:48` and `aether-proxy/src/setup/tui.rs:81`); reject if no match → 401.
   - **Auth discrimination from existing admin endpoints**: the existing admin-token auth middleware accepts any token in the configured admin set. The register endpoint is a *separate* route mounted with a different middleware that only accepts `ae_*`-prefixed tokens (and looks them up in `proxy_nodes`, not the admin token store). The two auth paths never overlap because they are different endpoints (`/manual` vs `/register`) and the validators reject the wrong format up-front.
2. **Upsert semantics**: if a row matches, update `node_name`, `public_ip`, `region`, `hardware`, `version` from the request body and return that row's `id` as `node_id`. If no row matches but the token is recognized via an "auto-provision" flag (see §3.6.4), insert a new row with `protocol='aether'`. Default policy: **auto-provision disabled**; operators must pre-create the row.
3. Push `events:entity` Redis stream with `{ns: "proxy_node", op: "upsert", id}` so the Rust gateway refreshes its in-memory snapshot (existing channel, no new wire).

The endpoint **does not interact with the Rust gateway directly**. The gateway learns about new/updated aether nodes through the existing `events:entity` Redis stream consumer.

#### 3.6.2 Admin API `protocol` field

The existing endpoints gain a `protocol` field:

| Endpoint | Change |
|---|---|
| `GET /api/admin/proxy-nodes` | response includes `protocol` per row |
| `POST /api/admin/proxy-nodes/manual` | request accepts `protocol` (default `"cnexus"`) |
| `PATCH /api/admin/proxy-nodes/{id}` | request accepts `protocol`; transition rules below |
| `DELETE /api/admin/proxy-nodes/{id}` | no change |
| `PUT /api/admin/proxy-nodes/{id}/config` | applies only to Cnexus nodes (no-op for Aether; Aether config is delivered via HeartbeatAck — which Cnexus does not push, see §1.2) |
| `POST /api/admin/proxy-nodes/upgrade` | applies only to Cnexus nodes |
| `POST /api/admin/proxy-nodes/test-url` | applies only to Cnexus nodes (no WS-server to dial for Aether) |

#### 3.6.3 Protocol transition state machine

When `PATCH` changes a node's `protocol`:

- **Cnexus → Aether**: existing outbound `cnexus::WsTransport` is torn down (`tunnel_stack::drop_node(node_id)`). The node row is updated in DB; `events:entity` stream notifies the gateway, which removes the cnexus supervisor and (if Aether listener is enabled) waits for an aether-proxy to dial in using the new protocol. While in-flight, requests to that node return 502 `GW_TUNNEL_NO_NODE`. Operators are expected to switch in maintenance windows.
- **Aether → Cnexus**: existing inbound `aether::WsTransport` is dropped; the gateway begins outbound dialing per the (updated) `ws_url`.
- The transition is mediated by a single `tunnel_stack::on_protocol_change(node_id, new_kind)` callback the entity-stream consumer calls. The callback is idempotent (calling it with the current kind is a no-op).

#### 3.6.4 Optional auto-provision

For unattended ops, Go side may set `AETHER_AUTO_PROVISION=true` to make `/api/admin/proxy-nodes/register` create rows on first contact instead of 401. **Default off**; spec recommends explicit pre-creation for auditability. tunnel-e2e suite explicitly sets it on for B/D scenarios.

#### 3.6.5 Heartbeat metric ingestion (Rust → Redis → Go → ClickHouse)

Rust hub: on every incoming `HeartbeatData` (§4.6), after telemetry-decode, push a JSON record to Redis stream `events:aether_heartbeat`:

```json
{
  "node_id": "aether-node-1",
  "heartbeat_session_id": "12345-1718500000000",
  "heartbeat_id": 42,
  "received_at_ms": 1718500000123,
  "active_connections": 17,
  "total_requests": 12340,
  "avg_latency_ms": 53.7,
  "failed_requests": 4,
  "dns_failures": 0,
  "stream_errors": 1,
  "proxy_version": "1.4.2"
}
```

Field flattening intentionally matches the on-wire schema (§4.6) plus the synthesized `received_at_ms` and `proxy_version` (extracted from `proxy_metadata`).

Go side `modules/monitoring`: a new `aether-heartbeat-sink` worker (housed in `binaries/worker-monitoring` or as a goroutine in the existing monitoring worker — TBD by Go team) consumes the stream and writes rows to ClickHouse table `aether_heartbeat_samples` (mirrors the existing `gateway_runtime_samples` shape but with the Aether metric set). Existing Go monitoring admin APIs gain a new endpoint `GET /api/admin/monitoring/aether/{node_id}` returning a 1h window of these rows.

Redis stream config: max length 50k entries (XADD with MAXLEN ~), so the queue is bounded if the consumer falls behind.

#### 3.6.6 GatewayControl semantics for Aether nodes

The existing `reload-config`, `cooldown-key`, `clear-cooldown`, `clear-cache` commands target the Cnexus gateway process. They are not understood by `aether-proxy`. Spec posture:

- All four commands explicitly **no-op for Aether nodes**: the Rust hub's command dispatcher checks `tunnel_protocol` on the resolved node and short-circuits with an ack carrying `{status: "skipped", reason: "aether_node_does_not_support_command"}`.
- Future work (out of scope here): if Aether's `remote_config` reverse-push is ever implemented, a new command kind `aether-remote-config` can map onto it.
- The Go side admin UI surfaces this by graying out the four buttons for Aether nodes and showing a tooltip "Aether nodes do not accept GatewayControl commands".

#### 3.6.7 Frontend UI

The AILinkMax-Go frontend gains:

- A `Protocol` column in the proxy nodes table (badge: blue=cnexus, purple=aether).
- A `Protocol` radio in the create/edit form, default `cnexus`.
- An "Aether status" detail panel showing the latest sampled metrics from `aether_heartbeat_samples` for the past 1h.

UI changes are tracked in the AILinkMax-Go repo's PR list, not enumerated here. This spec's commitment is the API contract above.

#### 3.6.8 Cross-repo PR coordination

Order of landing across the two repos:

1. AILinkMax-Go: add `protocol` field to entity (forward-compatible with current Rust — defaults to `cnexus` on Rust side). Add `auto-provision` flag (off). Land.
2. Cnexus Rust: §3 + §4 + §5 work. Land. Until step 3, the registration endpoint does not exist — that's OK because no aether-proxy is configured yet.
3. AILinkMax-Go: add registration endpoint + heartbeat ingester. Land.
4. AILinkMax-Go: admin API + frontend UI. Land.
5. Operations: provision one aether-proxy in staging; run tunnel-e2e suite locally.

This sequence keeps each step deployable in isolation.

### 3.7 End-to-End Flow

```
upstream_peer filter selects node_id by routing
        │
        ▼
tunnel_stack::peer_for_node(node_id) → (Arc<dyn Transport>, TunnelProtocol)
        │  (returns None if Aether proxy has not dialed in yet → 502 GW_TUNNEL_NO_NODE)
        ▼
Pingora upstream_peer points to tunnel_bridge loopback :PORT
        │
        ▼
tunnel_bridge accepts the loopback connection, reads X-Gateway-Tunnel-Node-Id
        │
        ├──── Cnexus path (hub previously dialed this node) ─────────────────┐
        │     build StreamTarget::Tcp{host, port, tls}                       │
        │     transport.open_stream(target) → StreamHandle                   │
        │     bi-directional byte copy                                       │
        │                                                                    │
        └──── Aether path (proxy previously dialed this hub) ────────────────┤
              http_adapter::parse_request() → method, url, headers, body_rx │
              build StreamTarget::Http with channels                        │
              transport.open_stream(target) → StreamHandle                  │
              await ResponseMeta on oneshot                                 │
              write_response(meta, body_rx) → serialize HTTP/1.1 back       │
                                                                            ┘
```

### 3.8 Node-Level Upstream Egress Proxy (`proxy_url`)

Aether proxy nodes have historically supported an upstream-egress proxy via the `proxy_url` field on the node row. `aether-proxy` reads this at startup and routes all upstream traffic to the configured proxy (typically `socks5://`, `socks5h://`, `http://`, or `https://`). The field already exists on `AILinkMax-Go`'s `proxy_nodes` entity (`modules/proxynode/business/entity.go` + `ops.go` validate the URL scheme; `ops.go:42` enumerates `socks5` / `socks5h` as supported schemes for the reachability probe). The Aether reference frontend renders `socks5://proxy:port` as the placeholder hint (`Aether/frontend/src/views/admin/ProxyNodes.vue:460`).

**Migration constraint.** When operators replace Aether-era deployments with Cnexus `gw-tunnel-server`, they expect the same SOCKS5 egress capability to keep working. Without it, every node-row currently configured with `proxy_url = socks5://...` either loses outbound egress on the migrated tunnel or has to swap to a different deployment pattern (proxychains, sidecar, etc.) that breaks operational consistency.

**Architecture posture.**

- `proxy_url` is **node-scoped operator configuration**, not a hub↔node wire-protocol concern. The hub does **not** push `proxy_url` to the proxy via a control channel: each proxy reads its own config from env / CLI / file at startup. Cnexus's tunnel-server inherits this same model (Phase 4, §7). For Aether nodes, `proxy_url` is consumed natively inside `aether-proxy`; the hub does not need to know its value.
- The DB's `proxy_url` field is therefore **informational** from the hub's perspective (where "hub" = the `gw-gateway` business binary that does request routing; this is distinct from `gw-tunnel-server`, which is the proxy-node binary deployed elsewhere and which actually consumes the `--proxy-url` flag). The hub shows the value in the admin UI so operators can see what the proxy *should* be running with, and `ops.go::probeProxy` can dial it for a reachability sanity check, but the hub never reaches into the proxy to set or change it.
- This is **distinct** from `TunnelProtocol`: protocol selects the hub↔node wire format (cnexus opaque-TCP-over-WS, or aether HTTP-over-WS); `proxy_url` selects the proxy's *own* upstream egress. Both protocols accept any of the supported `proxy_url` schemes.
- `proxy_url` is **not** rendered as an additional `TunnelProtocol` variant. It is orthogonal node-config flavour, not a new connection mode.

**Field shape (consistent across both protocols).**

| Field | Type | Source of truth | Consumer |
|---|---|---|---|
| `proxy_nodes.payload.proxy_url` | `String?` | Go admin API | Aether: `aether-proxy` reads from env injected via deployment; Cnexus: `gw-tunnel-server` reads from CLI flag at startup |
| `proxy_url` scheme | `socks5` / `socks5h` / `http` / `https` | URL parser | Both validators accept these; unknown scheme → 400 |
| Authentication | URL userinfo: `socks5://user:pass@host:1080` | Standard URL form | Both Cnexus and Aether implementations parse `user:pass` if present |
| `socks5h` semantics | DNS resolution performed at the proxy, not locally | URL scheme name | Both implementations honour `socks5` (local-DNS) vs `socks5h` (remote-DNS) |

**Phase 4 scope (Cnexus parity).**

Phase 4 adds SOCKS5/SOCKS5h egress to `gw-tunnel-server`. The relay-stream dial site (`crates/gw-tunnel-server/src/lib.rs:261` `TcpStream::connect(&target)`) is replaced by a dispatcher:

```rust
let stream = match cfg.egress {
    EgressMode::Direct => TcpStream::connect(&target).await?,
    EgressMode::Socks5 { proxy_addr, auth, resolve_at_proxy } => {
        if resolve_at_proxy {
            // socks5h: send hostname; let the proxy resolve
            tokio_socks::tcp::Socks5Stream::connect(proxy_addr, target_str)
                .await?.into_inner()
        } else {
            // socks5: resolve locally, send resolved IP
            let resolved = tokio::net::lookup_host(&target).await?.next()
                .ok_or_else(|| anyhow!("dns resolve failed"))?;
            tokio_socks::tcp::Socks5Stream::connect(proxy_addr, resolved)
                .await?.into_inner()
        }
    }
};
```

HTTP-CONNECT (`http://` / `https://` proxy schemes) is **deferred to a future phase** to keep Phase 4 small and unblock the migration-continuity use case. The Go admin already accepts those schemes and the probe already validates them; the gap is implementation on the Cnexus tunnel-server side. Documented in §7 Phase 4 and `docs/ROADMAP.md`.

**Operational contract.**

- `gw-tunnel-server` accepts `--proxy-url <url>` and `PROXY_URL` env. Both forms parse the same scheme set. CLI takes precedence over env.
- Empty / unset = direct TCP (current behaviour, no behaviour change for nodes without a proxy_url).
- On startup the server probes the proxy's reachability (5s timeout) and logs `proxy egress: direct` or `proxy egress: socks5://<scheme>://<host>:<port> (DNS at <proxy|local>)`. A probe failure does not refuse startup — the operator may be staging the proxy; the failure is reported at first connect.
- Connection-pool-style reuse of the SOCKS5 TCP session to the proxy is **not** required for Phase 4. Each relay-stream dial opens its own SOCKS5 handshake. Tunable later via a pool depth flag if perf justifies it.
- Allowed-targets and private-target restrictions (existing flags `--allow-target` and `--allow-private-targets`) still apply: they validate the *final* upstream target. They are evaluated BEFORE the SOCKS5 dial so a misconfigured proxy_url cannot bypass the host whitelist.

**Frontend contract.**

The admin UI surfaces `proxy_url` on both cnexus and aether node forms (Phase 4 PR 4b). The hint text mentions all accepted schemes. There is no protocol-conditioned hiding; both protocols accept the same set. The existing `TestNode` admin endpoint exercises `probeProxy(proxy_url)` against the live proxy to confirm reachability before saving.

## 4. Aether Adapter Internals

### 4.1 File Layout

```
crates/gw-tunnel-client/src/
├── lib.rs              (re-exports + trait + AnyTransport + factory entry)
├── factory.rs
├── target.rs           (StreamTarget enum)
├── cnexus/
│   ├── mod.rs
│   ├── frame.rs        (14B big-endian + magic + version)
│   ├── transport.rs    (ControlAuth/ControlNodeInfo + TCP relay)
│   ├── heartbeat.rs    (Ping/Pong with b"hb" payload)
│   └── pool.rs         (ReconnectingTransport supervisor)
└── aether/
    ├── mod.rs
    ├── frame.rs        (10B little-endian, no magic)
    ├── meta.rs         (RequestMeta / ResponseMeta JSON types)
    ├── transport.rs    (Bearer WS upgrade + stream table)
    ├── heartbeat.rs    (HeartbeatData / HeartbeatAck JSON)
    ├── gzip.rs         (flag 0x02 deflate/inflate)
    ├── http_adapter.rs (httparse parse + HTTP/1.1 byte serialize)
    └── listener.rs     (InboundListener: validates upgrade, attaches transport)
```

### 4.2 `aether/frame.rs`

Byte order is **big-endian** throughout. This is the network byte order Aether uses on the wire (proxy: `aether-proxy/src/tunnel/protocol.rs:46,103`; hub: `aether-hub/src/protocol.rs:89,105`). Writing little-endian here would silently produce garbage that decodes to bogus stream IDs.

```rust
// 10-byte BIG-endian header: stream_id (4) | msg_type (1) | flags (1) | payload_len (4)
pub const HEADER_SIZE: usize = 10;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgType {
    RequestHeaders  = 0x01,
    RequestBody     = 0x02,
    ResponseHeaders = 0x03,
    ResponseBody    = 0x04,
    StreamEnd       = 0x05,
    StreamError     = 0x06,  // bidirectional: client can cancel, server can signal upstream error
    Ping            = 0x10,
    Pong            = 0x11,
    GoAway          = 0x12,
    HeartbeatData   = 0x13,
    HeartbeatAck    = 0x14,
}

pub mod flags {
    pub const NONE: u8            = 0x00;
    pub const END_STREAM: u8      = 0x01;
    pub const GZIP_COMPRESSED: u8 = 0x02;
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub stream_id: u32,
    pub msg_type: MsgType,
    pub flags: u8,
    pub payload: bytes::Bytes,
}

impl Frame {
    pub fn encode(&self, dst: &mut bytes::BytesMut) {
        use bytes::BufMut;
        dst.put_u32(self.stream_id);                          // BE
        dst.put_u8(self.msg_type as u8);
        dst.put_u8(self.flags);
        dst.put_u32(self.payload.len() as u32);               // BE
        dst.put_slice(&self.payload);
    }

    pub fn decode(src: &mut std::io::Cursor<&[u8]>) -> Result<Option<Self>, FrameError> {
        // Peek 10 bytes for header; bail with None if short.
        // Read stream_id, msg_type, flags, payload_len using u32::from_be_bytes / get_u32.
        // If src has < HEADER_SIZE + payload_len remaining, restore cursor and return None.
        // Otherwise consume the frame and yield it.
        unimplemented!("see Cargo doc comment for sketch")
    }
}
```

Stream-id 0 is reserved for control frames (Ping/Pong, HeartbeatData/HeartbeatAck, GoAway). Stream IDs 2, 4, 6, ... are hub-initiated (Cnexus hub plays the hub role). The proxy never initiates a new stream; it only responds within streams the hub opened (`aether-hub/src/proxy_conn.rs:104,128-133`).

The byte layout mirrors `aether-proxy/src/tunnel/protocol.rs` exactly. Before implementing, Phase 2 starts with a fixture-capture pass: stand up a one-shot `aether-proxy` container pointing at a localhost listener, dump hex of one `RequestHeaders` + one `RequestBody` + one `ResponseHeaders` + one `ResponseBody` + one `HeartbeatData` + one `HeartbeatAck` frame, save to `crates/gw-tunnel-client/tests/fixtures/aether/`. Encoder roundtrip tests reference these fixtures so any drift breaks the build.

Stream-id parity is captured in fixtures too, so the eventual `next_stream_id: AtomicU32::new(2)` initialization (matching `aether-hub/src/proxy_conn.rs:104`) is verified rather than assumed.

### 4.3 `aether/meta.rs`

```rust
fn default_timeout() -> u64 { 60 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestMeta {
    pub method: String,
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_timeout")]
    pub timeout: u64,             // seconds; Aether clamps to [5, 300]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

// Matches aether-proxy/src/tunnel/protocol.rs:155-183: accept integers or
// integer-valued floats (e.g. 15.0 -> 15u64).
fn deserialize_timeout<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    /* dispatch on Value::U64 / Value::F64 -> u64; reject negative/NaN/Inf */
    unimplemented!()
}
```

Three traits to preserve byte-for-byte:

- `timeout` is a **non-optional `u64`** (not `Option<u64>`), default `60`, units **seconds**. Aether proxy uses `meta.timeout.clamp(5, 300)` then `Duration::from_secs(...)` (`stream_handler.rs:161`, `local_relay.rs:154`). Serializing `null` here would cause the proxy to reject the frame.
- `RequestMeta.headers` is `HashMap<String, String>` (single-valued); the Aether request model loses multi-value request headers like `Set-Cookie`-in-requests by design.
- `ResponseMeta.headers` is `Vec<(String, String)>` preserving order and multi-values (notably response `Set-Cookie`). Both `aether-proxy/src/tunnel/stream_handler.rs:251-255` and `aether-hub/src/local_relay.rs:235-245` push entries one by one without deduping.

The `ResponseMeta` carries **no `reason_phrase`** field. Both Aether and Cnexus drop the upstream's custom reason and synthesize one from `http::StatusCode::canonical_reason()` when serializing back to HTTP/1.1.

### 4.4 `aether/transport.rs`

```rust
pub struct WsTransport {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    cfg: TransportConfig,
    node_id: String,
    streams: dashmap::DashMap<u32, StreamSlot>,
    next_stream_id: std::sync::atomic::AtomicU32, // odd-numbered (client-initiated)
    write_tx: tokio::sync::mpsc::Sender<Frame>,
    alive: std::sync::atomic::AtomicBool,
    last_pong: arc_swap::ArcSwapOption<std::time::Instant>,
    auth_token: secrecy::SecretString,
    heartbeat_session_id: arc_swap::ArcSwap<String>,
}

impl WsTransport {
    /// Wrap an already-upgraded inbound WebSocket from `aether-proxy` into a transport.
    /// The Bearer auth + X-Node-Id headers have already been validated by `InboundListener`
    /// before this call.
    pub async fn attach(
        cfg: TransportConfig,
        node_view: ProxyNodeView,
        upgraded_ws: tokio_tungstenite::WebSocketStream<...>,
    ) -> Result<Self, TransportError> {
        // 1. Split the WS into (sink, stream).
        // 2. Spawn three tasks:
        //      - read loop: decode Frame -> route by stream_id to StreamSlot
        //                                  -> on HeartbeatData, reply HeartbeatAck
        //      - write loop: drain write_tx -> WS write
        //      - liveness loop: mark `alive=false` if no HeartbeatData received for
        //                       cfg.missed_heartbeat_threshold * cfg.heartbeat_interval
        // 3. Return the wrapped Inner.
    }

    async fn open_stream_impl(&self, target: StreamTarget)
        -> Result<StreamHandle, TransportError>
    {
        let StreamTarget::Http {
            method, url, headers, body_rx, body_tx_response, meta_tx_response
        } = target else {
            return Err(TransportError::UnsupportedTarget);
        };

        let sid = self.inner.next_stream_id.fetch_add(2, Ordering::Relaxed);
        let slot = StreamSlot::new(body_tx_response, meta_tx_response);
        self.inner.streams.insert(sid, slot);

        let meta = RequestMeta {
            method: method.to_string(),
            url: url.to_string(),
            headers: headers.into_iter().collect(),
            timeout: None,
        };
        let payload = serde_json::to_vec(&meta)?.into();

        self.send_frame(Frame {
            stream_id: sid,
            msg_type: MsgType::RequestHeaders,
            flags: flags::NONE,
            payload,
        }).await?;

        // Pump request body in another task; chunks emitted as RequestBody frames,
        // each potentially gzip-compressed via aether::gzip::maybe_compress.
        spawn_body_pump(sid, body_rx, self.inner.clone());

        Ok(StreamHandle { stream_id: sid })
    }
}
```

Stream ID allocation: in Aether's convention the **hub** (which initiates streams over the persistent connection) uses one parity and the **proxy** uses the other. Cnexus hub plays the hub role here, so we use the hub-side parity that matches `aether-hub` in the reference source. The fixture-capture step described in §4.2 reads one frame each direction to lock this down before encoder code is written. Reception:

- `ResponseHeaders` → decode JSON `ResponseMeta`, send via `meta_tx_response` oneshot.
- `ResponseBody` → forward bytes to `body_tx_response` mpsc (decompressing first if `GZIP_COMPRESSED` flag set).
- `StreamEnd` → close mpsc, remove slot.
- `StreamError` → translate to `TransportError`, propagate so bridge writes a 502 back to Pingora.
- `GoAway` → **immediate close**, matching Aether semantics. Hub sends it to signal "I am going away; do not send more frames" then calls `request_close` (`aether-hub/src/proxy_conn.rs:110-113`). Proxy on receipt breaks its read loop (`aether-proxy/src/tunnel/dispatcher.rs:204-206`). No graceful drain on either side. In-flight streams that have not produced `StreamEnd` propagate as a 502 to Pingora upstream callers.

### 4.5 `aether/http_adapter.rs`

The bridge-side adapter that bridges loopback HTTP/1 byte streams to `StreamTarget::Http`:

```rust
pub struct HttpAdapter<R, W> {
    reader: R,
    writer: W,
    node_id: String,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> HttpAdapter<R, W> {
    /// Reads inbound bytes until \r\n\r\n, parses with httparse, returns the
    /// request preamble (method/url/headers) and a handle to the remaining body bytes.
    pub async fn parse_request(&mut self) -> Result<(RequestPreamble, RemainingBody), HttpError> {
        let mut buf = Vec::with_capacity(8 * 1024);
        loop {
            let n = self.reader.read_buf(&mut buf).await?;
            if n == 0 { return Err(HttpError::Eof); }
            let mut headers = [httparse::EMPTY_HEADER; 64];
            let mut req = httparse::Request::new(&mut headers);
            match req.parse(&buf)? {
                httparse::Status::Complete(consumed) => {
                    // Extract X-Gateway-Tunnel-Node-Id / Target / Target-Tls,
                    // then strip them; the remainder is the body start.
                    return Ok((to_preamble(&req, consumed)?,
                               RemainingBody::new(buf, consumed)));
                }
                httparse::Status::Partial => continue,
            }
        }
    }

    /// Reassembles ResponseMeta + streamed body into HTTP/1.1 bytes for Pingora.
    pub async fn write_response(
        &mut self,
        meta: ResponseMeta,
        mut body_rx: tokio::sync::mpsc::Receiver<bytes::Bytes>,
    ) -> Result<(), HttpError> {
        let status = http::StatusCode::from_u16(meta.status)?;
        let reason = status.canonical_reason().unwrap_or("");
        self.writer.write_all(format!("HTTP/1.1 {} {}\r\n", meta.status, reason).as_bytes()).await?;

        let has_content_length = meta.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length"));
        let has_transfer_encoding = meta.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding"));

        for (k, v) in &meta.headers {
            self.writer.write_all(format!("{}: {}\r\n", k, v).as_bytes()).await?;
        }
        if !has_content_length && !has_transfer_encoding {
            self.writer.write_all(b"Transfer-Encoding: chunked\r\n").await?;
        }
        self.writer.write_all(b"\r\n").await?;

        if !has_content_length && !has_transfer_encoding {
            while let Some(chunk) = body_rx.recv().await {
                self.writer.write_all(format!("{:X}\r\n", chunk.len()).as_bytes()).await?;
                self.writer.write_all(&chunk).await?;
                self.writer.write_all(b"\r\n").await?;
            }
            self.writer.write_all(b"0\r\n\r\n").await?;
        } else {
            while let Some(chunk) = body_rx.recv().await {
                self.writer.write_all(&chunk).await?;
            }
        }
        Ok(())
    }
}
```

Bounds and assumptions:

- HTTP/1 only on the bridge loopback (Pingora speaks HTTP/1 to the bridge by configuration; HTTP/2 is not required).
- Request body chunked-encoded → decoded to raw bytes before streaming through `body_rx`. Content-Length bodies → bounded read.
- Response body always streamed via mpsc, chunked-encoded on the wire if Aether did not provide a Content-Length.

### 4.6 `aether/heartbeat.rs`

Direction reminder: in Aether's protocol, the **proxy** sends `HeartbeatData` and the **hub** replies with `HeartbeatAck`. Cnexus hub plays the hub role, so we *receive* `HeartbeatData` and *send* `HeartbeatAck`.

`HeartbeatData` (decoded from inbound; field layout mirrors `aether-proxy/src/tunnel/heartbeat.rs:233-248`):

```rust
#[derive(Deserialize)]
struct HeartbeatData {
    node_id: String,
    heartbeat_session_id: String,
    heartbeat_id: u64,
    active_connections: u32,
    // Five metrics counters live as TOP-LEVEL fields, not nested:
    total_requests: u64,
    avg_latency_ms: f64,
    failed_requests: u64,
    dns_failures: u64,
    stream_errors: u64,
    // proxy_metadata is the only nested object and only contains version:
    proxy_metadata: ProxyMetadata,
}

#[derive(Deserialize)]
struct ProxyMetadata {
    version: String,
}
```

Notably absent: `sent_at_ms` (not on the wire), no nested `metrics: {...}` object, no `proxy_metadata.region` / `proxy_metadata.public_ip` (they live elsewhere, not in heartbeat).

`HeartbeatAck` (encoded; field layout intentionally minimal):

```rust
#[derive(Serialize, Default)]
struct HeartbeatAck {
    // Option per aether-proxy/src/tunnel/heartbeat.rs:260-268 (`#[serde(default)]`),
    // so the proxy tolerates absence; emitting it is still correct and easier to debug.
    heartbeat_id: Option<u64>,

    // Reverse-push fields intentionally omitted. The proxy's AckPayload decoder
    // also marks `remote_config` and `upgrade_to` as `#[serde(default)]`, so
    // their absence is non-fatal.
}
```

Wire-compatibility checks:

- We populate `heartbeat_id` from the received `HeartbeatData.heartbeat_id` so the proxy can correlate.
- We never populate `remote_config` or `upgrade_to`.
- Inbound `HeartbeatData` payloads are decoded for telemetry **and pushed to the Redis stream `events:aether_heartbeat`** for Go-side monitoring ingestion (see §3.7).
- Liveness is judged from `HeartbeatData` arrival time alone, not payload metrics: dead after `cfg.missed_heartbeat_threshold` consecutive `cfg.heartbeat_interval` windows pass without any `HeartbeatData`. Defaults (matching `aether-proxy/src/config.rs:65` and Aether hub's broader `tunnel_stale_timeout_secs = 300`): interval 30s, threshold 3 → ~90s of silence. The Aether hub's own 300s stale timeout is a wider net that covers any kind of frame silence; Cnexus narrows to heartbeat-only because that's the dominant idle traffic.
- The exact field list above is verified at Phase 2 start by capturing one real `HeartbeatData` JSON into `crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.json`. Any rename in Aether breaks the fixture roundtrip test before it reaches a container.
- `heartbeat_session_id` is generated **once** at proxy startup (format `"{pid}-{nanos_since_epoch}"`, `aether-proxy/src/tunnel/heartbeat.rs:80-88`); restart → new session id. Cnexus hub stores the latest seen session id per node so an unexpected change tells operators the proxy restarted.

### 4.7 `aether/gzip.rs`

```rust
pub fn maybe_compress(payload: bytes::Bytes) -> (bytes::Bytes, bool) {
    if payload.len() < 512 { return (payload, false); }
    // aether-hub/src/protocol.rs:168 uses Compression::default(); we match it.
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    if enc.write_all(&payload).is_err() { return (payload, false); }
    match enc.finish() {
        Ok(out) if out.len() < payload.len() => (out.into(), true),
        _ => (payload, false),
    }
}

pub fn decompress(payload: &[u8]) -> Result<bytes::Bytes, std::io::Error> {
    use std::io::Read;
    let mut dec = flate2::read::GzDecoder::new(payload);
    let mut out = Vec::with_capacity(payload.len() * 2);
    dec.read_to_end(&mut out)?;
    Ok(out.into())
}
```

Compression scope:

- **Outbound** (hub → proxy): both `RequestHeaders` (JSON meta serialization) and `RequestBody` (each chunk independently). `Compression::default()` matches `aether-hub/src/protocol.rs:168`.
- **Inbound** (proxy → hub): the proxy may set `GZIP_COMPRESSED` on `ResponseHeaders` and `ResponseBody` frames. Note the proxy internally uses `Compression::fast()` (`aether-proxy/src/tunnel/protocol.rs:237`) for encode; the difference is irrelevant on the decode side since gzip is self-describing.
- The 512-byte threshold is per-frame, not cumulative. Sub-512 payloads ship plain, even if a series of small chunks would benefit from compression.
- Skipped when `enc.finish()` yields a no-savings result (compressed ≥ original).

## 5. Decoupling Discipline

### 5.1 Boundary Rule

Both `gw-tunnel-client` and `gw-tunnel-server` are constrained to depend on the following workspace crates and nothing else:

| Allowed in-workspace dependency | Reason |
|---|---|
| `gw-core` | shared types, error codes |
| `gw-codec` | wire encoding primitives |
| `gw-codec-macros` | proc-macro support for `gw-codec` |

All other workspace crates are forbidden: `gw-store`, `gw-auth`, `gw-ledger`, `gw-billing`, `gw-task`, `gw-task-runtime`, `gw-task-worker`, `gw-routing`, `gw-adapter`, `gw-capability`, `gw-health`, `gw-proxy`, `gw-gateway`, `gw-unified`, `gw-runtime`, `gw-egress-impersonate`, `e2e-fake-upstream`, `e2e-fake-web`.

Scope:

- Applies to `[dependencies]` and any feature-gated path. Forbidden crates may not appear under any feature combination.
- Does **not** apply to `[dev-dependencies]`. Tests may use fakes and mocks freely.
- Applies to transitive resolution: if `gw-tunnel-client` depends on a third-party crate that pulls in a forbidden crate (vanishingly unlikely but possible via path overrides), the rule still trips.

### 5.2 Enforcement Mechanism

A shell script using `cargo metadata` + `jq`:

`scripts/check-tunnel-deps.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

WHITELIST=(gw-core gw-codec gw-codec-macros)
ROOTS=(gw-tunnel-client gw-tunnel-server)

META=$(cargo metadata --format-version 1 --locked)

# Workspace member names (strip "name VER (path)" form to just name).
WORKSPACE=$(jq -r '.workspace_members[]' <<<"$META" | awk '{print $1}' | sort -u)

violations=0
for root in "${ROOTS[@]}"; do
  root_id=$(jq -r --arg n "$root" '.packages[] | select(.name==$n) | .id' <<<"$META")
  if [ -z "$root_id" ]; then
    echo "ERROR: crate '$root' not found in workspace" >&2
    exit 1
  fi

  # Walk resolve.nodes transitively, excluding DevDependency-only edges.
  visited=$(jq -r --arg root "$root_id" '
    def walk($id; $seen):
      ($seen + [$id]) as $seen2 |
      (.resolve.nodes[] | select(.id == $id) | .deps[] |
         select(any(.dep_kinds[]?; .kind != "dev"))
         | .pkg) as $next |
      if ($next | IN($seen2[])) then empty
      else $next, walk($next; $seen2)
      end;
    walk($root; []) | unique
  ' <<<"$META" 2>/dev/null || true)

  for dep_id in $visited; do
    dep_name=$(jq -r --arg id "$dep_id" '.packages[] | select(.id==$id) | .name' <<<"$META")
    if grep -qx "$dep_name" <<<"$WORKSPACE"; then
      allowed=0
      for w in "${WHITELIST[@]}" "$root"; do
        [ "$w" = "$dep_name" ] && { allowed=1; break; }
      done
      if [ "$allowed" -eq 0 ]; then
        echo "VIOLATION: $root → (transitively) $dep_name [tunnel crates may not depend on business crates]"
        violations=$((violations + 1))
      fi
    fi
  done
done

if [ "$violations" -gt 0 ]; then
  echo "" >&2
  echo "If a new dependency is genuinely required, update the whitelist in" >&2
  echo "docs/spec/09-tunnel.md and justify in the MR description." >&2
  exit 1
fi
echo "tunnel dep-boundary OK"
```

`Makefile` target:

```makefile
.PHONY: check-tunnel-deps
check-tunnel-deps:
	bash scripts/check-tunnel-deps.sh
```

Note: this script calls `cargo metadata`, not `cargo build`. Per project workflow, `cargo metadata` runs locally (it does not invoke the compiler) and does not need `cargo-remote-ctl`.

### 5.3 Documentation

`crates/gw-tunnel-client/README.md` and `crates/gw-tunnel-server/README.md` each receive a top-of-file warning block:

```markdown
> ## ⚠️ Dependency boundary
>
> This crate is guarded by `scripts/check-tunnel-deps.sh`. Within this workspace
> it may only depend on `gw-core`, `gw-codec`, and `gw-codec-macros`. All other
> workspace crates are forbidden.
>
> This discipline keeps the tunnel subsystem independently extractable. Please
> do not bypass. To update the whitelist, propose a change to
> `docs/spec/09-tunnel.md` and justify in your MR.
```

`docs/spec/09-tunnel.md` gains a `Crate dependency boundary` section listing the rule, the whitelist, the enforcement script, and the change-review process.

### 5.4 Pre-Implementation Audit

Before Phase 1 begins, run `cargo tree -p gw-tunnel-client -p gw-tunnel-server` (via `cargo-remote-ctl` or locally — `cargo tree` is metadata-only) and verify the current state already complies. Findings:

- **If current state is clean**: ship the script as-is.
- **If a single re-export is causing a leak**: move the leaked type into `gw-core` and the dependency dissolves.
- **If a real business coupling exists**: extract the relevant logic from the tunnel crate first; do not relax the rule.

The audit result is recorded in the Phase 1 PR description.

## 6. tunnel-e2e Suite

### 6.1 Directory Layout

```
tests/tunnel-e2e/
├── README.md
├── docker-compose.yml
├── Dockerfile.aether-proxy
├── Dockerfile.aether-hub
├── edge.nginx.conf            (path-routes Aether-proxy traffic to Go vs Rust)
├── requirements.txt
├── pytest.ini
├── conftest.py
├── seed/
│   ├── 01_schema.sql          (mirror of production migration up to 0011)
│   ├── 02_proxy_nodes.sql     (default seed; per-scenario fixtures override)
│   └── 03_provider_keys.sql
├── scenarios/
│   ├── test_cnexus_protocol.py     (Phase 1)
│   ├── test_aether_compat.py       (Phase 2)
│   ├── test_aether_baseline.py     (Phase 2)
│   └── test_mixed_pool.py          (Phase 3)
├── support/
│   ├── aether_admin.py
│   ├── http_helpers.py
│   ├── wait_for_aether_attach.py
│   └── docker_helpers.py     (kill/start helpers for failover scenarios)
└── aether-src/                (git submodule -> Aether repo)
```

`edge.nginx.conf` (referenced by the `edge` service in §6.3 compose):

```nginx
server {
  listen 80;
  # Aether proxy hits POST /api/admin/proxy-nodes/register first → Go.
  location /api/admin/proxy-nodes/ {
    proxy_pass http://ailinkmax-go-web:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
  }
  # Then WS upgrade at /api/internal/proxy-tunnel → Rust gateway.
  location /api/internal/proxy-tunnel {
    proxy_pass http://ali-gateway:8000;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
  }
}
```

In production this routing is the operator's existing reverse proxy (nginx / Traefik / Envoy). The compose service exists so tunnel-e2e can validate the contract.

`.gitmodules`:

```ini
[submodule "tests/tunnel-e2e/aether-src"]
    path = tests/tunnel-e2e/aether-src
    url = https://gitlab.ailinkmax.com/ailinkmax/Aether
    update = none
```

`update = none` means a plain `git submodule update --init` is a no-op; users must explicitly opt in via `make tunnel-e2e-deps` (described in §6.5). This prevents fresh clones from blocking on credentials when not running the tunnel suite.

### 6.2 Docker Images

Both Aether images build from the submodule source on demand. They use an independent Rust toolchain (`rust:1.88-slim`) rather than reusing the workspace `rust-toolchain.toml`, decoupling Aether builds from local toolchain churn.

`Dockerfile.aether-proxy` (build context: `tests/tunnel-e2e/`):

```dockerfile
FROM rust:1.88-slim AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY aether-src/ /build/aether-src/
WORKDIR /build/aether-src/aether-proxy
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/aether-src/aether-proxy/target/release/aether-proxy /usr/local/bin/aether-proxy
ENTRYPOINT ["/usr/local/bin/aether-proxy"]
```

`Dockerfile.aether-hub` follows the same pattern, building `aether-src/aether-hub/`.

**Build-context size**: the submodule includes the full Aether workspace. If build time becomes painful, a later optimization is `.dockerignore` to exclude unrelated Aether subdirs. Out of scope for this design.

### 6.3 docker-compose Topology

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

  # AILinkMax-Go web binary — serves /api/admin/proxy-nodes/* including the new
  # /register endpoint per §3.6.1. Path-routed in front of ali-gateway via nginx.
  ailinkmax-go-web:
    build:
      context: ../../../AILinkMax-Go
      dockerfile: Dockerfile
      target: web
    environment:
      DATABASE_URL: postgres://postgres:e2e@postgres:5432/cnexus
      REDIS_URL: redis://redis:6379/0
      AETHER_AUTO_PROVISION: "true"     # tunnel-e2e convenience; prod-default off
    depends_on:
      postgres: { condition: service_healthy }
      redis:    { condition: service_healthy }
    networks: [tunnel-net]
    expose: ["8080"]

  # Path-router: aether-proxy uses ONE base URL (AETHER_PROXY_AETHER_URL),
  # but registration goes to Go and WS goes to Rust. nginx splits them.
  edge:
    image: nginx:1.27-alpine
    volumes:
      - ./edge.nginx.conf:/etc/nginx/conf.d/default.conf:ro
    depends_on:
      ali-gateway:      { condition: service_started }
      ailinkmax-go-web: { condition: service_started }
    networks: [tunnel-net]
    expose: ["80"]

  # Compatibility (B/D) — proxy dials INTO edge, which routes to ali-gateway / go-web.
  aether-proxy:
    build:
      context: .
      dockerfile: Dockerfile.aether-proxy
    environment:
      AETHER_PROXY_AETHER_URL: ws://edge:80
      AETHER_PROXY_MANAGEMENT_TOKEN: ae_e2e_compat
      AETHER_PROXY_NODE_NAME: aether-node-01
      AETHER_PROXY_ALLOWED_PORTS: "9000,9443"
    networks: [tunnel-net]
    depends_on:
      edge:
        condition: service_started

  # Baseline (C) — proves the Aether images themselves work; dials its own hub.
  aether-proxy-baseline:
    build:
      context: .
      dockerfile: Dockerfile.aether-proxy
    environment:
      AETHER_PROXY_AETHER_URL: ws://aether-hub:8001
      AETHER_PROXY_MANAGEMENT_TOKEN: ae_e2e_baseline
      AETHER_PROXY_NODE_NAME: aether-baseline-01
      AETHER_PROXY_ALLOWED_PORTS: "9000,9443"
    networks: [tunnel-net]
    depends_on:
      aether-hub:
        condition: service_started

  aether-hub:
    build:
      context: .
      dockerfile: Dockerfile.aether-hub
    environment:
      AETHER_HUB_BIND: 0.0.0.0:8001
    networks: [tunnel-net]
    expose: ["8001"]

  ali-gateway:
    build:
      context: ../../
      dockerfile: Dockerfile
      args:
        CARGO_FEATURES: "--features tunnel"
    environment:
      DATABASE_URL: postgres://postgres:e2e@postgres:5432/cnexus
      REDIS_URL: redis://redis:6379/0
      GW_TUNNEL_ENABLED: "true"
      GW_TUNNEL_AETHER_LISTENER_ENABLED: "true"  # unconditionally accept aether proxies
      GW_ADMIN_TOKEN: e2e-admin
    depends_on:
      postgres: { condition: service_healthy }
      redis:    { condition: service_healthy }
      cnexus-tunnel-server: { condition: service_started }
      fake-upstream:        { condition: service_started }
    # Note: aether-proxy is NOT a depends_on here. Direction is reversed —
    # aether-proxy dials INTO ali-gateway, so the proxy waits for the gateway, not
    # the other way around.
    networks: [tunnel-net]
    expose: ["8000", "9090"]

  pytest-runner:
    image: python:3.12-slim
    working_dir: /work
    volumes:
      - ../../tests/tunnel-e2e:/work
      - /var/run/docker.sock:/var/run/docker.sock # for docker_helpers failover
    environment:
      ALI_GATEWAY_URL: http://ali-gateway:8000
      ALI_ADMIN_TOKEN: e2e-admin
      AETHER_HUB_URL:  http://aether-hub:8001
      FAKE_UPSTREAM_URL: http://fake-upstream:9000
    command: >
      bash -c "pip install -q -r requirements.txt && pytest -q --tb=short --junit-xml=logs/junit.xml scenarios/"
    depends_on:
      ali-gateway: { condition: service_started }
      aether-hub:  { condition: service_started }
    networks: [tunnel-net]
```

**Isolation guarantees**:

| Dimension | tests/e2e/ (existing) | tests/tunnel-e2e/ (new) |
|---|---|---|
| compose project name | `e2e` | `tunnel-e2e` |
| network | `e2e-net` | `tunnel-net` |
| volumes | `e2e-*` | `tunnel-e2e-*` |
| gateway image tag | `alm-e2e-gateway:local` | `cnexus-tunnel-gateway:local` |
| Makefile target | `make e2e` | `make tunnel-e2e` |

No file is written by both suites. The only shared read-only reference is `Dockerfile.e2e-fake-upstream` and the root `Dockerfile` (parameterized).

### 6.4 Seed Strategy

`conftest.py` exposes a `seed_nodes` fixture for per-scenario node table population:

```python
@pytest.fixture
def seed_nodes(pg_conn, redis_client):
    def _seed(*nodes):
        pg_conn.execute("DELETE FROM proxy_nodes")
        for n in nodes:
            pg_conn.execute(
                "INSERT INTO proxy_nodes(id, payload, version, deleted_at) "
                "VALUES (%s, %s::jsonb, 1, NULL)",
                (n["id"], json.dumps(n)),
            )
        redis_client.xadd("events:entity", {"ns": "proxy_node", "op": "reset"})
        time.sleep(2)  # let gateway refresh
    return _seed
```

Scenario examples:

```python
def test_aether_simple_get(seed_nodes, gateway_client, wait_for_aether_attach):
    # Pre-create the row so Go-side /register can authenticate by management token
    # and return this row's id. ws_url is informational for Aether (the proxy
    # dials INTO edge → /api/internal/proxy-tunnel → Rust gateway).
    seed_nodes({
        "id": "aether-node-1",
        "ws_url": "ws://edge:80/api/internal/proxy-tunnel",
        "auth_token_enc": encrypt_for_test("ae_e2e_compat"),
        "protocol": "aether",
        "enabled": True,
        "allowed_provider_ids": ["fake"],
        "allowed_vendor_key_ids": ["fake-key"],
    })
    # wait_for_aether_attach polls /healthz/tunnel until the per-node `present=true`.
    wait_for_aether_attach("aether-node-1", timeout_s=15)
    resp = gateway_client.get("/v1/proxy/fake/echo?via=tunnel")
    assert resp.status_code == 200
```

### 6.5 Scenarios

**`test_cnexus_protocol.py`** (3 cases, Phase 1):

- `test_tunnel_no_node_returns_hard_error` — empty node pool, request with `require_tunnel: true` → 502 `GW_TUNNEL_NO_NODE`.
- `test_tunnel_target_denied` — node `allowed_ports` does not include target → 502 `GW_TUNNEL_TARGET_DENIED`.
- `test_tunnel_direct_vs_tunnel_byte_identity` — same request via direct path vs tunnel; response body and headers byte-identical.

**`test_aether_compat.py`** (8 cases, Phase 2):

- `test_aether_node_handshake` — seed one `protocol=aether` node; `aether-proxy` dials in within 5s; `/healthz/tunnel` reports `alive=true` for that `node_id`.
- `test_aether_simple_get` — GET via Aether node, 200, body integrity.
- `test_aether_post_body` — POST + JSON body, upstream receives intact body.
- `test_aether_stream_response` — upstream returns chunked stream; bytes arrive in order, no truncation.
- `test_aether_error_propagation` — upstream 5xx propagates as-is.
- `test_aether_gzip_body` — body > 512B → `flags::GZIP_COMPRESSED` set on the wire; round-trips correctly.
- `test_aether_heartbeat_alive` — after 70s, `/healthz/tunnel` reports `alive=true`; `last_pong_age < 35s`.
- `test_aether_reconnect` — `docker kill aether-proxy && docker start aether-proxy`; `aether-proxy` redials and reattaches within 30s (the proxy owns reconnection logic; hub just accepts).

**`test_aether_baseline.py`** (1 case, Phase 2):

- `test_aether_native_routing` — request through aether-hub native proxy entry to fake-upstream; baseline confirms our Aether images themselves work, isolating regressions in our adapter from regressions in Aether builds.

**`test_mixed_pool.py`** (3 cases, Phase 3):

- `test_mixed_pool_load_distribution` — 1 cnexus + 1 aether node; 200 concurrent requests; both nodes receive traffic; all succeed.
- `test_mixed_pool_failover` — same pool; kill aether-proxy mid-test; remaining requests route to cnexus; 5xx rate ≤ 5%; recovery within 30s.
- `test_mixed_pool_concurrent_no_corruption` — 50 concurrent requests, each carrying a unique nonce in body; response body contains matching nonce (no stream crossover).

### 6.6 Makefile Targets

```makefile
TUNNEL_E2E_COMPOSE := -f tests/tunnel-e2e/docker-compose.yml --project-directory tests/tunnel-e2e

.PHONY: tunnel-e2e tunnel-e2e-up tunnel-e2e-down tunnel-e2e-logs tunnel-e2e-deps check-tunnel-deps

tunnel-e2e-deps:
	git submodule update --init --recursive tests/tunnel-e2e/aether-src

tunnel-e2e: tunnel-e2e-deps tunnel-e2e-down
	docker compose $(TUNNEL_E2E_COMPOSE) build
	docker compose $(TUNNEL_E2E_COMPOSE) run --rm pytest-runner
	$(MAKE) tunnel-e2e-down

tunnel-e2e-up: tunnel-e2e-deps
	docker compose $(TUNNEL_E2E_COMPOSE) up -d --build

tunnel-e2e-down:
	docker compose $(TUNNEL_E2E_COMPOSE) down -v --remove-orphans

tunnel-e2e-logs:
	docker compose $(TUNNEL_E2E_COMPOSE) logs --tail=200 -f

check-tunnel-deps:
	bash scripts/check-tunnel-deps.sh
```

### 6.7 No CI Integration

By explicit design decision, `make tunnel-e2e` is **not** wired into CI. Existing CI jobs (`lint-test`, `rust-build`, `rust-build-tunnel`, `push-image`, `push-tunnel-image`) remain unchanged. Developers run the tunnel suite locally before merging tunnel-relevant changes.

## 7. Phasing Plan

### Phase 1 · Cnexus protocol over Docker (unblock the 3 skipped tests)

Goal: `make tunnel-e2e` passes the 3 cases in `test_cnexus_protocol.py`. No protocol divergence yet.

Companion Go work (parallel cross-repo, lands first per §3.6.8):

- **G1 (lands before any Rust phase-2 work)**: AILinkMax-Go adds `Protocol string` to the proxynode entity and accepts it in admin APIs. Default `"cnexus"` so all existing rows remain valid. The Rust side does not need this yet; G1 just unblocks Phase 2 entry.

Change set (Cnexus Rust):

- Reorganize `gw-tunnel-client` so existing transport logic lives under `cnexus/`. Public re-exports preserve backwards-compatible call sites.
- Add migration `0011_proxy_nodes_protocol.sql`.
- Add `TunnelProtocol` enum and `protocol` field to `ProxyNodePayload`; deserialization defaults to `Cnexus`.
- Create `tests/tunnel-e2e/` skeleton including `docker-compose.yml` (without `aether-proxy` / `aether-hub`), `conftest.py`, `requirements.txt`, `pytest.ini`, `test_cnexus_protocol.py`, `support/http_helpers.py`.
- Add `Makefile` targets: `tunnel-e2e`, `tunnel-e2e-up`, `tunnel-e2e-down`, `tunnel-e2e-logs`, `tunnel-e2e-deps`.
- Add `.gitmodules` entry pointing to `https://gitlab.ailinkmax.com/ailinkmax/Aether` with `update = none`. Submodule is registered but unused by Phase 1 compose.

Out of Phase 1:

- `aether::` module, `trait Transport`, `AnyTransport`, `StreamTarget` two-variant enum.
- `tunnel_bridge` internal branching.
- `tunnel_stack` DashMap type change.

Verification:

- `cargo-remote-ctl test -p gw-tunnel-client -p gw-store -p gw-proxy --lib` is green.
- `cargo-remote-ctl test --workspace --features tunnel` is green.
- `make tunnel-e2e` reports 3 passed in under 60 seconds.
- `make e2e` (existing 25 scenarios) is green.

PR breakdown:

- **PR 1a**: `cnexus/` submodule reorg + `TunnelProtocol` + migration.
- **PR 1b**: `tests/tunnel-e2e/` skeleton + 3 cases + Makefile targets + `.gitmodules`.

### Phase 2 · Aether adapter + compatibility cases (core deliverable)

Goal: `make tunnel-e2e` passes 12 cases (3 cnexus + 8 aether compat + 1 aether baseline).

Companion Go work:

- **G2 (must land before Phase 2 PR-2d)**: AILinkMax-Go adds `POST /api/admin/proxy-nodes/register` + `aether_register.go` per §3.6.1. The tunnel-e2e suite cannot bring up `aether-proxy` without it. Includes the Go web binary in compose.
- **G3 (can land in parallel with PR-2d or just after)**: heartbeat Redis stream consumer + ClickHouse table + monitoring API per §3.6.5.

Change set (Cnexus Rust):

- Full `aether/` subtree under `gw-tunnel-client/src/`: `frame.rs`, `meta.rs`, `transport.rs` (with `attach` not `connect`), `heartbeat.rs`, `gzip.rs`, `http_adapter.rs`, `listener.rs`.
- `gw-tunnel-client` introduces a `ProxyNodeRegistry` trait that `gw-proxy::tunnel_stack` implements; this keeps the listener decoupled from `gw-store`.
- New dependencies in `gw-tunnel-client/Cargo.toml`: `httparse = "1"`, `flate2 = "1"`, `http = "1"`, `axum` (only for `WebSocketUpgrade` extractor on the listener side).
- Introduce `trait Transport`, `AnyTransport`, `factory::{dial_cnexus, attach_aether}`, `StreamTarget` two-variant enum.
- Update `gw-proxy::tunnel_stack`: `DashMap<NodeId, Arc<dyn Transport>>`; `peer_for_node` returns `(Arc<dyn Transport>, TunnelProtocol)`; new `attach_inbound_transport(node_id, Arc<dyn Transport>)` API exposed for the listener.
- Update `gw-proxy::tunnel_bridge` to branch by `TunnelProtocol` at accept; the Aether branch uses `aether::http_adapter`.
- Update `gw-gateway`: `tunnel_refresher` dispatches Cnexus nodes via `dial_cnexus`; for Aether nodes it ensures the listener is running and the node is registered in the stack snapshot. Add `/api/internal/proxy-tunnel` route in the axum router that hands the upgrade to `aether::InboundListener`.
- New config section `[tunnel.aether_listener]` in `config/default.toml` with fields `enabled` (default false) and `path` (default `/api/internal/proxy-tunnel`).
- Add `tests/tunnel-e2e/Dockerfile.aether-proxy` and `Dockerfile.aether-hub`.
- Extend `docker-compose.yml` with `aether-proxy` (dials INTO `ali-gateway`), `aether-proxy-baseline` (dials INTO `aether-hub`), and `aether-hub` services.
- Add `test_aether_compat.py` (8 cases), `test_aether_baseline.py` (1 case), `support/aether_admin.py`, `support/wait_for_aether_attach` helper.
- Capture Aether wire fixtures: connect to real `aether-proxy` once (via a one-shot listener), save hex dumps under `crates/gw-tunnel-client/tests/fixtures/aether/`.

Risk-bound PR breakdown (per audit recommendation, PR-2c is split to keep blast radius small):

- **PR 2a**: Structural refactor — `trait Transport`, `AnyTransport`, `StreamTarget`, `gw-proxy` view changes — **with only `cnexus::` implementation**. Behavior identical to Phase 1. All existing tests green. This isolates the structural blast radius.
- **PR 2b**: `aether/` implementation + unit tests + Aether fixture roundtrips. No bridge changes yet (Aether path unreachable from the bridge in this PR).
- **PR 2c**: Bridge branching + `InboundListener` + new gateway route `/api/internal/proxy-tunnel` + `/healthz/tunnel` per-node schema + SIGTERM drain + config keys. Pure backend wiring; the new route exists but no e2e suite exercises it yet (covered by Rust integration tests against a mock Aether client).
- **PR 2d**: tunnel-e2e Dockerfiles + compose extension (adds `aether-proxy`, `aether-proxy-baseline`, `aether-hub`, AILinkMax-Go web binary, nginx path-router — see §6.3) + Aether scenarios + `support/aether_admin.py` + `support/wait_for_aether_attach`. Lands only after G2.

Verification:

- New unit tests under `crates/gw-tunnel-client/src/aether/*.rs` and `crates/gw-tunnel-client/tests/aether_integration.rs` pass via `cargo-remote-ctl test -p gw-tunnel-client`.
- `make tunnel-e2e` reports 12 passed.
- `make e2e` remains green.

### Phase 3 · Mixed-pool stress + dep-boundary + docs

Goal: `make tunnel-e2e` passes 15 cases; `make check-tunnel-deps` reports 0 violations; documentation is current.

Companion Go work:

- **G3 (lands during or just after Rust PR-2d)**: heartbeat consumer + ClickHouse schema + monitoring API per §3.6.5.
- **G4 (after Phase 3)**: admin UI frontend changes per §3.6.7. Not strictly required for `make tunnel-e2e` (tests hit JSON APIs), but required for human operators.

Change set (Cnexus Rust):

- `test_mixed_pool.py` (3 cases): load distribution, failover, no-corruption.
- `support/docker_helpers.py`: docker.sock-mediated `kill_service` / `start_service` helpers for failover.
- `scripts/check-tunnel-deps.sh` + `Makefile check-tunnel-deps` target.
- `crates/gw-tunnel-client/README.md` and `crates/gw-tunnel-server/README.md`: dep-boundary warnings.
- `docs/spec/09-tunnel.md`: new sections (`tunnel_protocol`, `Aether compatibility`, `Crate dependency boundary`).
- `tests/tunnel-e2e/README.md`: usage, scenarios, common-failure-mode guide.

PR breakdown:

- **PR 3a**: `test_mixed_pool.py` + helpers.
- **PR 3b**: `check-tunnel-deps.sh` + docs.

Verification:

- `make tunnel-e2e` reports 15 passed.
- `make check-tunnel-deps` reports 0 violations.
- Performance-baseline warnings (see §8.3) surface as `pytest.warning`, not failures.

### Phase 4 · Cnexus SOCKS5 egress for Aether-parity (`proxy_url` migration continuity)

Goal: `gw-tunnel-server` accepts a `--proxy-url socks5[h]://...` flag and dials all upstream targets through the configured SOCKS5 proxy. Aether-era deployments using `proxy_nodes.proxy_url = socks5://...` can be replaced by a Cnexus tunnel-server with the same `proxy_url` value at deployment time — and continue to function identically — completing the migration story started in Phase 1 (protocol-neutral abstraction) and Phase 2 (Aether wire compat).

Constraint: per §3.8 `proxy_url` is **deployer config on the tunnel-server itself**, not pushed from the hub. The DB value is informational. Operators are responsible for matching the deployment flag to the DB value; mismatches cause traffic to bypass the configured proxy.

Companion Go work:

- **G5 (Phase 4 PR-4b, lands before the final integrated MR)**: minor UI updates in `AILinkMax-Go` admin — surface `proxy_url` form field for both `cnexus` and `aether` nodes (Phase 3 G4 had introduced it for aether only; Phase 4 widens). Validation already supports both protocols on the Go side (no enum gate to remove — verify in Phase 4 Task 4b.1).

Change set (Cnexus Rust):

- `crates/gw-tunnel-server/Cargo.toml`: add `tokio-socks` dep.
- `crates/gw-tunnel-server/src/main.rs`: add `--proxy-url` CLI flag + `PROXY_URL` env var.
- `crates/gw-tunnel-server/src/lib.rs`: `EgressMode` enum + dispatch at the single `TcpStream::connect(&target)` site (currently line 261 inside `relay_stream`); `cfg.egress` config plumb-through.
- Unit tests: cover `Direct` (regression: existing flow), `Socks5 { resolve_at_proxy: true }` (`socks5h`), `Socks5 { resolve_at_proxy: false }` (`socks5`), URL userinfo parsing (`socks5://user:pass@host:1080`).
- `tests/tunnel-e2e/Dockerfile.socks5-egress` + `docker-compose.yml` addition: `socks5-egress` container (e.g. `serjs/go-socks5-proxy:latest`). Both new compose services (`socks5-egress` + `cnexus-tunnel-server-socks5`) are gated behind `profiles: ["socks5"]`. The `cnexus-tunnel-server-socks5` service builds from `Dockerfile.tunnel-e2e` (NOT the root `Dockerfile`), reusing the existing `gw-tunnel-server` build pattern.
- `tests/tunnel-e2e/scenarios/test_cnexus_socks5.py`: 2 scenarios (cnexus through socks5 reaches fake-upstream; aether-parity baseline confirming both protocols can use the same socks5-egress when the deployment config matches the DB value).
- `Makefile`: new target `tunnel-e2e-socks5` activating the `socks5` profile + its own teardown wired to the same profile (default `tunnel-e2e-down` would leak profile-gated containers).
- `crates/gw-tunnel-server/README.md`: new "Egress proxy" subsection — CLI/env usage, scheme set, `socks5` vs `socks5h` distinction, `--allow-target` invariant.
- `docs/spec/09-tunnel.md`: as-built §Egress proxy section (full `proxy_url` table + scheme set + worked example).
- `docs/issues/2026-06-15-socks5-egress-deployment-coupling.md` (new): operational gotcha — DB `proxy_url` is informational; the tunnel-server reads from CLI/env. Document the failure mode of a mismatch.
- `docs/ROADMAP.md`: post-Phase-4 update — strike SOCKS5-egress as resolved; rename Phase 3's "Phase 4 candidates" section to "Future tunnel candidates"; add HTTP-CONNECT egress (`http://`, `https://` proxy schemes) as a new future candidate.

PR breakdown:

- **PR 4a** (Rust shared branch): code + unit tests + e2e + spec/issues update. Continues on the same shared feature branch as Phase 1+2+3.
- **PR 4b** (`AILinkMax-Go` repo, real MR): `proxy_url` form-field exposure for both protocols + validation regression test.

Verification (per-profile, since Phase 3's mixed-pool and Phase 4's socks5 scenarios are gated behind compose `--profile` flags):

- `cargo test -p gw-tunnel-server` PASS, including new `socks5_dispatch_*` cases.
- `make tunnel-e2e` (default profile, no flags) reports **12 passed** = 3 cnexus from Phase 1 + 8 aether compat from Phase 2 + 1 aether baseline from Phase 2.
- `make tunnel-e2e-baseline` reports **1 passed** (Phase 2 baseline; same file invoked separately).
- `make tunnel-e2e-mixed` reports **11 passed** = 3 mixed-pool scenarios + 8 parameterized strategy-matrix cases (Phase 3).
- `make tunnel-e2e-socks5` reports **2 passed** (Phase 4: cnexus-through-socks5 + protocol-parity).
- Aggregate across all profile-gated targets: **26 passed**.
- `make check-tunnel-deps` still clean (tokio-socks is a third-party dep, not a workspace crate, so dep-boundary script is unaffected).
- The final integrated MR (originally Phase 3 Task F3) opens at the end of Phase 4 instead — it now carries Phase 1+2+3+4 commits.

Out of scope for Phase 4 (deferred to ROADMAP):

- HTTP-CONNECT proxy schemes (`http://` / `https://`). The Go admin and probe accept these; the Cnexus tunnel-server does not implement them in Phase 4. File as a Phase 5 / future-work candidate when a deployment actually needs HTTP-CONNECT.
- Per-stream proxy selection (different streams through different SOCKS5 endpoints from the same tunnel-server). Single deployment-level SOCKS5 only.
- SOCKS5 connection pooling. Each relay-stream opens its own SOCKS5 handshake.
- Hub→tunnel-server push of `proxy_url` (would require Aether-style `remote_config` reverse channel). Deployer config only.

## 8. Testing & Verification

### 8.1 Three-Layer Coverage Matrix

| Component | unit (`cargo test`) | integration (`cargo test --features tunnel`) | tunnel-e2e |
|---|:--:|:--:|:--:|
| `cnexus::frame` | yes | — | indirect (A) |
| `cnexus::transport` | — | yes (local server + client) | yes (A) |
| `cnexus::heartbeat` | yes | yes | yes (baseline) |
| `aether::frame` | yes (with fixture) | — | indirect (B) |
| `aether::meta` | yes (with fixture) | — | — |
| `aether::http_adapter` | yes | yes (against fake bridge) | indirect |
| `aether::heartbeat` | yes | yes (cadence + ACK ignore) | yes (B7) |
| `aether::gzip` | yes | — | yes (B6) |
| `aether::transport` | — | yes (mock Aether server) | yes (B1–B5, B8) |
| `factory::build_transport` | yes | — | indirect |
| `Transport` + `AnyTransport` | yes | — | indirect |
| `StreamTarget` | yes (Unsupported paths) | — | indirect |
| `tunnel_bridge` branch | — | yes (HTTP→Aether, TCP→Cnexus) | yes (all) |
| `tunnel_stack` dispatch | yes (type) | yes (`peer_for_node` returns kind) | indirect |
| `proxy_nodes` migration | — | yes (pg integration) | indirect |
| `check-tunnel-deps.sh` | yes (jq fixture inputs) | — | manual |
| mixed-pool routing | — | — | yes (D) |

**Fixture pinning** (`crates/gw-tunnel-client/tests/fixtures/aether/`): wire-level hex captures of one `RequestHeaders`, one `RequestBody`, one `ResponseHeaders`, one `ResponseBody`, one `HeartbeatData`, and one `HeartbeatAck`. Captured at Phase 2 start by running a one-shot client against a real `aether-proxy` container. Frame and JSON roundtrip tests reference these as source of truth.

Coverage scope is qualitative: the critical paths (frame encoding/decoding, JSON serde, HTTP/1 adapter happy + chunked + Content-Length paths, gzip compress/decompress, heartbeat cadence, transport reconnect) are exercised. No quantitative coverage percentage is enforced.

### 8.2 Per-Phase Acceptance Criteria

**Phase 1**:

- `cargo-remote-ctl test -p gw-tunnel-client -p gw-store -p gw-proxy --lib` green.
- `cargo-remote-ctl test --workspace --features tunnel` green.
- `make tunnel-e2e` → 3 passed.
- `make e2e` (25 existing scenarios) → all green.

**Phase 2**:

- All new `aether::*` unit and integration tests green via `cargo-remote-ctl`.
- `make tunnel-e2e` → 12 passed.
- `make e2e` → all green.

**Phase 3**:

- `make tunnel-e2e` → 15 passed.
- `make check-tunnel-deps` → 0 violations.
- `make e2e` → all green.

### 8.3 Performance Baseline (warnings only)

Mixed-pool scenarios run perf assertions that **warn rather than fail**:

- Single-request p50 ≤ direct-path p50 × 2.
- Single-request p99 ≤ direct-path p99 × 5.
- 200-concurrent fake-upstream throughput ≥ 50 req/s.

Failing thresholds surface as `pytest.warning`, never as red builds. A strict variant (`@pytest.mark.perf`, opt-in via `make tunnel-e2e-perf`) is left for a later iteration if perf becomes a recurring concern.

### 8.4 Failure-Mode Index

Captured in `tests/tunnel-e2e/README.md` for quick triage:

| Symptom | First log to consult |
|---|---|
| `pytest-runner` reports `connection refused` | `docker compose logs ali-gateway` |
| Aether node never goes live | `docker compose logs aether-proxy` (look for `authenticate` / `frame decode error`) |
| Byte mismatch in B3 | `docker compose logs fake-upstream`; diff against direct baseline |
| Failover scenario times out | `docker compose logs ali-gateway | grep tunnel_stack` (reconnect loop) |
| `aether_gzip_body` fails | run `aether::gzip::decompress` over the captured wire bytes manually |

### 8.5 Regression Isolation

The tunnel work touches public types in `gw-proxy`, which is the most exposed cross-cutting surface in the workspace. Each PR must demonstrate that pre-existing tests still pass:

- `cargo-remote-ctl test --workspace --features tunnel` output appended to the MR description.
- `make e2e` (25 scenarios) confirmed green by the author locally; final-line output excerpt pasted in the MR description.

No automation enforces this; it is a checklist item in the MR template (the repo follows existing GitLab MR conventions; this design does not introduce a new template). MR reviewers verify the excerpt before approving.

## 9. Risks & Open Items

### 9.1 Implementation Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Aether wire byte layout differs from comments in source | medium | Phase 2 stuck | Capture real-traffic fixtures before writing encoder; pin via roundtrip tests |
| Aether `Cargo.lock` / toolchain conflicts with workspace | medium | image build failure | Aether images use isolated `rust:1.88-slim` toolchain |
| `Arc<dyn Transport>` refactor breaks existing `ReconnectingTransport<T>` signatures | medium | wide blast radius in PR 2a | PR 2a contains structural refactor only; all existing tests must stay green before PR 2b lands |
| HTTP/1 → Aether `RequestMeta` memory amplification on large bodies | low | perf regression | `http_adapter` streams body via mpsc, no full-body buffering; perf scenarios catch regression |
| `gw-codec` has hidden business coupling | low | dep-boundary false alarm | Phase 1 audit (§5.4) runs `cargo tree`; narrow whitelist to `gw-core` only if audit reveals coupling |
| Aether submodule URL requires credentials new contributors lack | medium | fresh clones blocked on submodule init | `update = none` in `.gitmodules`; `make tunnel-e2e-deps` opt-in; documented in README |
| `pytest-runner` failover scenarios need docker.sock | medium | privilege concern | Document explicitly in README; failover suite is local-only by design (no CI = no production risk) |
| Aether heartbeat schema differs from initial estimate | medium | Phase 2 B7 fails | Captured wire fixture in PR-2b; serde mismatch fail-fast. Verified field list in §4.6 already matches audit findings |
| `aether-proxy` rejects ACK with absent `remote_config` | low | proxy disconnects in B7 | `AckPayload` decoder uses `#[serde(default)]` for both `remote_config` and `upgrade_to` (confirmed via audit); empty ACK is safe |
| `/api/internal/proxy-tunnel` shared on existing axum port (8000) introduces auth surface | low | misconfigured admin token leak | Bearer enforcement is per-node-token (`auth_token_enc` lookup), independent from the admin token; verify in PR 2c review |
| `tunnel_refresher` for Aether nodes that have never dialed in reports them as "registered but absent" | low | user confusion when reading `/healthz/tunnel` | health endpoint distinguishes `present=false` (proxy never dialed) from `alive=false` (dialed but stale heartbeat); documented in README |
| Go register endpoint not deployed before Aether proxy provisioned in prod | high | proxy stuck in registration retry loop | §3.6.8 sequence: G1 lands first, G2 lands before any Aether proxy is started in prod; runbook documents the order |
| Auto-provision flag toggled on in prod by mistake | medium | bad token creates spurious nodes | default off in `AETHER_AUTO_PROVISION`; tunnel-e2e is the only path that flips it on; alert on any new `proxy_nodes` row created via register endpoint |
| Edge reverse-proxy timeout drops long-lived WS | medium | proxy reconnect storm | `proxy_read_timeout 3600s` baked into `edge.nginx.conf`; production deployments document the same requirement for their nginx/envoy |
| Heartbeat Redis stream backlog grows when Go consumer is down | low | memory pressure on Redis | XADD uses `MAXLEN ~ 50000`; documented in §3.6.5 |
| macOS Docker Desktop docker.sock latency disrupts failover scenarios | low | flaky D2 locally | README notes "Docker Desktop ≥ 4.x; on macOS allow extra timeout slack via `TUNNEL_E2E_FAILOVER_SLACK_MS=2000`" |
| Frame-level trace logs grow huge under mixed-pool stress | medium | local disk fills | per-frame logs default to `trace` (off); README warns and provides `RUST_LOG=gw_tunnel_client=debug` recipe |

### 9.2 Open Items (deferred to implementation plan, not blocking)

- `gw-codec` whitelist decision finalized only after Phase 1 audit (§5.4). Preliminary OK; revisit during PR 1a review.
- MR template references the existing GitLab convention; this design does not introduce one.
- Exact heartbeat field list mirrored from Aether source at Phase 2 start. The fixture-capture pass replaces speculative documentation.

### 9.3 Out of Scope (explicit deferrals)

- HeartbeatAck `remote_config` / `upgrade_to` reverse-push implementation.
- Tunnel soak suite (analogous to existing `make e2e-soak`).
- mTLS / real LLM API end-to-end validation.
- Go admin UI exposure of the `protocol` field (separate `AILinkMax-Go` repo PR).
- Eventual extraction of `gw-tunnel-client` / `gw-tunnel-server` into a standalone git repo. This design's dep-boundary rule is the prerequisite.
- CI integration of `make tunnel-e2e` (intentionally local-only per project decision).

## 10. Glossary

- **Cnexus** — project name for this workspace (formerly ailinkmax-gateway).
- **Aether** — reference project at `https://gitlab.ailinkmax.com/ailinkmax/Aether`. Provides the alternative wire protocol this design targets for compatibility.
- **proxy node** — remote VPS process that relays traffic to upstream services on behalf of the hub. Two variants in this design:
  - `gw-tunnel-server` (Cnexus protocol): **listens** on a port; hub dials out to it.
  - `aether-proxy` (Aether protocol): **does not listen**; dials out to the hub at `/api/internal/proxy-tunnel`.
- **hub** — `gw-gateway` running locally / centrally. Plays both an outbound WS-client role (to Cnexus nodes) and an inbound WS-server role (for Aether nodes) over the same `axum` HTTP port.
- **`InboundListener`** — the hub-side service that accepts inbound Aether WS upgrades, validates Bearer + `X-Node-Id`, and registers the resulting transport into `tunnel_stack`.
- **edge router** — in production an existing reverse proxy (nginx/Traefik/Envoy); in tunnel-e2e a dedicated nginx container. Path-splits the Aether proxy's single `AETHER_PROXY_AETHER_URL` between the Go register endpoint and the Rust WS endpoint.
- **`events:aether_heartbeat`** — Redis stream the Rust hub pushes per HeartbeatData; the Go monitoring sink consumes and persists to ClickHouse. Bounded at 50k entries via `MAXLEN ~`.
- **`ae_` prefix** — Aether management token convention. Used by Go register endpoint to discriminate Aether-protocol auth from admin-token auth on the same DB.
- **G1 / G2 / G3 / G4** — milestones in the AILinkMax-Go repo (§3.6.8 cross-repo coordination). G1: entity `Protocol` field; G2: register endpoint; G3: heartbeat ingest + monitoring API; G4: frontend UI.
- **`tunnel_protocol`** — discriminator on a `ProxyNode` selecting which wire protocol to use. Values: `cnexus` (default), `aether`.
- **`StreamTarget`** — request to the transport layer to open a stream. Two variants: `Tcp` (Cnexus) and `Http` (Aether).
- **`AnyTransport`** — enum that holds either `cnexus::WsTransport` or `aether::WsTransport`; implements `trait Transport` via delegation.
- **`cargo-remote-ctl`** — project-specific wrapper for remote-executed `cargo` operations (build / test / clippy). Local-only `cargo metadata` operations do not require it.
- **`proxy_url`** — node-scoped upstream-egress proxy URL stored on the `ProxyNode` row. Schemes accepted by the Go admin validation surface (across both protocols): `socks5`, `socks5h`, `http`, `https`. Schemes actually implemented on the consuming proxy: **Aether-proxy** — all four (`socks5`, `socks5h`, `http`, `https`); **Cnexus `gw-tunnel-server`** (Phase 4) — `socks5` and `socks5h` only; HTTP-CONNECT (`http://`, `https://`) deferred to a future phase. Consumed via `--proxy-url` CLI / `PROXY_URL` env on the proxy-node deployment; the hub does not push this value. See §3.8.
- **`EgressMode`** — internal `gw-tunnel-server` enum (Phase 4). Variants: `Direct`, `Socks5 { proxy_addr, auth, resolve_at_proxy }`. Selects the dial path inside `relay_stream`.
