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

- **Protocol divergence is sealed inside `gw-tunnel-client`.** Upstream code (`gw-proxy`, `gw-gateway`) selects a node and asks the transport layer to open a stream. The transport layer dispatches to the correct protocol implementation based on `protocol_kind` carried in the node view.
- **Connection direction is also sealed inside `gw-tunnel-client`.** Outside callers do not know who initiated the WS; they only see a `Transport` trait object.
- **`tunnel_bridge` learns about protocol kind only once**, when it looks up the transport handle by `node_id`. It then branches between two code paths:
  - `protocol_kind=Cnexus`: forwards the loopback HTTP/1 byte stream as-is into a `StreamTarget::Tcp` (current behavior).
  - `protocol_kind=Aether`: parses the loopback HTTP/1 stream with `httparse`, constructs a `StreamTarget::Http`, lets the transport translate it to Aether frames, and serializes the response back to HTTP/1 bytes for Pingora.
- **The `cnexus/` module is bit-compatible with the existing `gw-tunnel-server` wire protocol** (14-byte big-endian header with magic `[0xAE, 0x21]` + version + msg_type + flags + stream_id + length). No protocol change for existing nodes.
- **The `aether/` module is bit-compatible with the Aether wire protocol** as observed in the Aether reference source: 10-byte little-endian header, `RequestMeta`/`ResponseMeta` JSON, gzip flag 0x02, Bearer-token WS upgrade auth at `/api/internal/proxy-tunnel`.
- **`aether::InboundListener` is the only new long-running service** the hub spawns. It is started by `gw-gateway` when at least one Aether-protocol node exists (or when `tunnel.aether_listener_enabled` config is true).

### 2.4 Change Footprint Summary

| Crate / path | Change kind | Phase |
|---|---|---|
| `crates/gw-tunnel-client/src/cnexus/` | new submodule housing existing outbound-dialing transport logic | 1 |
| `crates/gw-tunnel-client/src/aether/` | new submodule (frame / transport / heartbeat / meta / gzip / http_adapter / listener) | 2 |
| `crates/gw-tunnel-client/src/lib.rs` | export `trait Transport`, `AnyTransport`, `factory::build_or_attach` | 2 |
| `crates/gw-tunnel-client/src/target.rs` | `enum StreamTarget { Tcp, Http }` | 2 |
| `crates/gw-proxy/src/tunnel_stack.rs` | `ProxyNodeView.protocol`; DashMap holds `Arc<dyn Transport>`; `peer_for_node` returns kind; new `attach_inbound_transport(node_id, transport)` API | 2 |
| `crates/gw-proxy/src/tunnel_bridge.rs` | per-protocol branching at accept | 2 |
| `crates/gw-gateway/src/lib.rs` | `tunnel_refresher` reads `payload.protocol`, dispatches outbound (Cnexus) vs InboundListener (Aether); spawns listener if any aether node present | 2 |
| `crates/gw-gateway/src/routes.rs` | new route `/api/internal/proxy-tunnel` → delegates to `aether::InboundListener::accept` | 2 |
| `crates/gw-store/migrations/0011_proxy_nodes_protocol.sql` | backfill + partial index | 1 |
| `crates/gw-store/src/entity/proxy_node.rs` | add `protocol: ProtocolKind` field | 1 |
| `config/default.toml` | new `[tunnel.aether_listener]` section (enabled, bind path) | 2 |
| `tests/tunnel-e2e/` | full new directory | 1 + 2 + 3 |
| `Makefile` | `tunnel-e2e` / `tunnel-e2e-up` / `tunnel-e2e-down` / `tunnel-e2e-logs` / `check-tunnel-deps` targets | 1 + 3 |
| `scripts/check-tunnel-deps.sh` | dep-boundary enforcement (shell + jq) | 3 |
| `.gitmodules` | `tests/tunnel-e2e/aether-src` → `https://gitlab.ailinkmax.com/ailinkmax/Aether` | 1 |
| `docs/spec/09-tunnel.md` | new sections: `protocol_kind`, Aether compatibility, dep boundary | 3 |
| `tests/tunnel-e2e/README.md` | new — usage, scenario catalog, failure-mode guide | 3 |

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
pub enum ProtocolKind {
    Cnexus,
    Aether,
}

impl Default for ProtocolKind {
    fn default() -> Self { Self::Cnexus }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProxyNodePayload {
    // ... existing fields
    #[serde(default)]
    pub protocol: ProtocolKind,
}
```

`crates/gw-proxy/src/tunnel_stack.rs` adds `protocol: ProtocolKind` to `ProxyNodeView`. `tunnel_refresher` propagates the field from payload into the view.

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
    fn last_pong_age(&self) -> Option<std::time::Duration>;
    fn protocol_kind(&self) -> ProtocolKind;
    fn close(&self) -> futures::future::BoxFuture<'_, ()>;
}

pub enum AnyTransport {
    Cnexus(cnexus::WsTransport),
    Aether(aether::WsTransport),
}

impl Transport for AnyTransport { /* delegating dispatch */ }
```

Because connection direction differs per protocol, the construction surface splits into two paths:

**Outbound (Cnexus)** — used by `tunnel_refresher` when seeding nodes:

```rust
// gw-tunnel-client/src/factory.rs

pub async fn dial_cnexus(
    cfg: TransportConfig,
    node: &ProxyNodeView,
) -> Result<AnyTransport, TransportError> {
    // outbound WS upgrade to node.ws_url; wraps in ReconnectingTransport supervisor.
    Ok(AnyTransport::Cnexus(cnexus::WsTransport::connect(cfg, node).await?))
}
```

**Inbound (Aether)** — used by `aether::InboundListener` when an Aether proxy dials in:

```rust
pub async fn attach_aether(
    cfg: TransportConfig,
    node_view: ProxyNodeView,
    upgraded_ws: tokio_tungstenite::WebSocketStream<...>,
) -> Result<AnyTransport, TransportError> {
    Ok(AnyTransport::Aether(aether::WsTransport::attach(cfg, node_view, upgraded_ws).await?))
}
```

`tunnel_refresher` does not call `attach_aether` directly. For Aether nodes it only:

1. Ensures the row exists in the in-memory `proxy_nodes` snapshot (so `peer_for_node` can later look up the node by id and the listener can authenticate inbound connections against it).
2. Verifies the `InboundListener` is running (spawns it idempotently the first time an Aether node appears).

The DashMap in `tunnel_stack` becomes `DashMap<NodeId, Arc<dyn Transport>>` (no `ReconnectingTransport` wrapper for the abstract type; the supervisor wraps the inner Cnexus transport only). For Aether nodes the slot is populated by `InboundListener::on_upgrade` callback after a successful WS handshake, and removed on disconnect; reconnection is entirely driven by `aether-proxy` redialing.

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

Mounting the route is opt-in: `tunnel.aether_listener.enabled` (default false). `gw-gateway` auto-enables when `tunnel_refresher` sees at least one Aether node, OR when the operator sets the flag explicitly in config.

### 3.6 End-to-End Flow

```
upstream_peer filter selects node_id by routing
        │
        ▼
tunnel_stack::peer_for_node(node_id) → (Arc<dyn Transport>, ProtocolKind)
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

```rust
// 10-byte LE header: stream_id (4) | msg_type (1) | flags (1) | payload_len (4)
pub const HEADER_SIZE: usize = 10;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgType {
    RequestHeaders  = 0x01,
    RequestBody     = 0x02,
    ResponseHeaders = 0x03,
    ResponseBody    = 0x04,
    StreamEnd       = 0x05,
    StreamError     = 0x06,
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
        // put_u32_le(stream_id), put_u8(msg_type), put_u8(flags),
        // put_u32_le(payload.len()), put_slice(payload)
    }
    pub fn decode(src: &mut std::io::Cursor<&[u8]>) -> Result<Option<Self>, FrameError> {
        // peek 10 bytes; bail with None if short; otherwise consume and yield Frame
    }
}
```

The byte layout mirrors `aether-proxy/src/tunnel/protocol.rs` exactly. Before implementing, Phase 2 starts with a fixture-capture pass: run a throwaway client against a real `aether-proxy` container, dump hex of one `RequestHeaders` + one `ResponseHeaders` + one `HeartbeatData` frame, save to `crates/gw-tunnel-client/tests/fixtures/aether/`. Encoder roundtrip tests reference these fixtures so any drift breaks the build.

### 4.3 `aether/meta.rs`

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestMeta {
    pub method: String,
    pub url: String,
    pub headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}
```

Header container type asymmetry (request = `HashMap`, response = `Vec`) reflects Aether's wire-level decision: response headers may repeat (`Set-Cookie`), request headers in Aether's model do not. Preserving this asymmetry is a wire-compatibility requirement.

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
- `GoAway` → mark transport as draining; existing streams complete; new `open_stream` calls fail until reconnect.

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

```rust
#[derive(Deserialize)]
struct HeartbeatData {
    node_id: String,
    heartbeat_session_id: String,
    heartbeat_id: u64,
    sent_at_ms: u64,
    active_connections: u32,
    proxy_metadata: ProxyMetadata,
    metrics: HeartbeatMetrics, // exact field list mirrored from Aether source
}

#[derive(Serialize, Default)]
struct HeartbeatAck {
    heartbeat_id: u64,
    received_at_ms: u64,
    // Reverse-push fields intentionally omitted from our serialized ACK.
    // Field-presence remains tolerant on the wire because the proxy decodes with
    // `#[serde(default)]` semantics; this is the explicit "do not reverse-push"
    // contract from §1.2.
}
```

- We never populate `remote_config` or `upgrade_to` in outgoing ACKs.
- Inbound `HeartbeatData` payloads are decoded for telemetry only (emitted via `tracing::debug!`); the transport itself uses arrival time, not payload metrics, for liveness.
- Heartbeat cadence is driven by the proxy. Liveness is declared dead after `cfg.missed_heartbeat_threshold` consecutive `cfg.heartbeat_interval` windows pass with no `HeartbeatData` (defaults: threshold 3, interval 30s — i.e. ~90s of silence).
- Field list (`metrics`, `proxy_metadata`) is captured from Aether source at Phase 2 start and pinned to a JSON fixture under `crates/gw-tunnel-client/tests/fixtures/aether/heartbeat.json`. Any field rename in Aether breaks the fixture roundtrip test before it reaches a container.

### 4.7 `aether/gzip.rs`

```rust
pub fn maybe_compress(payload: bytes::Bytes) -> (bytes::Bytes, bool) {
    if payload.len() < 512 { return (payload, false); }
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

Compression is opportunistic: ≥ 512 bytes payload + actual size reduction. Applied to outbound `RequestBody`; honored on inbound `ResponseBody` when `GZIP_COMPRESSED` flag is set.

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
│   └── docker_helpers.py     (kill/start helpers for failover scenarios)
└── aether-src/                (git submodule -> Aether repo)
```

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

  # In the compatibility scenarios (B), this proxy dials INTO ali-gateway.
  # In the baseline scenario (C), an additional aether-proxy-baseline instance
  # below dials into aether-hub to keep the two test surfaces independent.
  aether-proxy:
    build:
      context: .
      dockerfile: Dockerfile.aether-proxy
    environment:
      AETHER_PROXY_AETHER_URL: ws://ali-gateway:8000
      AETHER_PROXY_MANAGEMENT_TOKEN: e2e-aether-mgmt
      AETHER_PROXY_NODE_NAME: aether-node-01
      AETHER_PROXY_NODE_ID: aether-node-1
      AETHER_PROXY_ALLOWED_PORTS: "9000,9443"
    networks: [tunnel-net]
    depends_on:
      ali-gateway:
        condition: service_started

  # Baseline (C) — proves the Aether images themselves work; dials its own hub.
  aether-proxy-baseline:
    build:
      context: .
      dockerfile: Dockerfile.aether-proxy
    environment:
      AETHER_PROXY_AETHER_URL: ws://aether-hub:8001
      AETHER_PROXY_MANAGEMENT_TOKEN: e2e-aether-mgmt
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
    # node row pre-registers the proxy so InboundListener accepts its inbound auth.
    # ws_url is informational only for Aether (connection direction is reversed —
    # the proxy dials INTO the hub at /api/internal/proxy-tunnel).
    seed_nodes({
        "id": "aether-node-1",
        "ws_url": "ws://ali-gateway:8000/api/internal/proxy-tunnel",
        "auth_token_enc": encrypt_for_test("e2e-aether-mgmt"),
        "protocol": "aether",
        "enabled": True,
        "allowed_provider_ids": ["fake"],
        "allowed_vendor_key_ids": ["fake-key"],
    })
    # wait_for_aether_attach polls /healthz/tunnel until aether-proxy has dialed in.
    wait_for_aether_attach("aether-node-1", timeout_s=10)
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

Change set:

- Reorganize `gw-tunnel-client` so existing transport logic lives under `cnexus/`. Public re-exports preserve backwards-compatible call sites.
- Add migration `0011_proxy_nodes_protocol.sql`.
- Add `ProtocolKind` enum and `protocol` field to `ProxyNodePayload`; deserialization defaults to `Cnexus`.
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

- **PR 1a**: `cnexus/` submodule reorg + `ProtocolKind` + migration.
- **PR 1b**: `tests/tunnel-e2e/` skeleton + 3 cases + Makefile targets + `.gitmodules`.

### Phase 2 · Aether adapter + compatibility cases (core deliverable)

Goal: `make tunnel-e2e` passes 12 cases (3 cnexus + 8 aether compat + 1 aether baseline).

Change set:

- Full `aether/` subtree under `gw-tunnel-client/src/`: `frame.rs`, `meta.rs`, `transport.rs` (with `attach` not `connect`), `heartbeat.rs`, `gzip.rs`, `http_adapter.rs`, `listener.rs`.
- `gw-tunnel-client` introduces a `ProxyNodeRegistry` trait that `gw-proxy::tunnel_stack` implements; this keeps the listener decoupled from `gw-store`.
- New dependencies in `gw-tunnel-client/Cargo.toml`: `httparse = "1"`, `flate2 = "1"`, `http = "1"`, `axum` (only for `WebSocketUpgrade` extractor on the listener side).
- Introduce `trait Transport`, `AnyTransport`, `factory::{dial_cnexus, attach_aether}`, `StreamTarget` two-variant enum.
- Update `gw-proxy::tunnel_stack`: `DashMap<NodeId, Arc<dyn Transport>>`; `peer_for_node` returns `(Arc<dyn Transport>, ProtocolKind)`; new `attach_inbound_transport(node_id, Arc<dyn Transport>)` API exposed for the listener.
- Update `gw-proxy::tunnel_bridge` to branch by `ProtocolKind` at accept; the Aether branch uses `aether::http_adapter`.
- Update `gw-gateway`: `tunnel_refresher` dispatches Cnexus nodes via `dial_cnexus`; for Aether nodes it ensures the listener is running and the node is registered in the stack snapshot. Add `/api/internal/proxy-tunnel` route in the axum router that hands the upgrade to `aether::InboundListener`.
- New config section `[tunnel.aether_listener]` in `config/default.toml` with fields `enabled` (default false) and `path` (default `/api/internal/proxy-tunnel`).
- Add `tests/tunnel-e2e/Dockerfile.aether-proxy` and `Dockerfile.aether-hub`.
- Extend `docker-compose.yml` with `aether-proxy` (dials INTO `ali-gateway`), `aether-proxy-baseline` (dials INTO `aether-hub`), and `aether-hub` services.
- Add `test_aether_compat.py` (8 cases), `test_aether_baseline.py` (1 case), `support/aether_admin.py`, `support/wait_for_aether_attach` helper.
- Capture Aether wire fixtures: connect to real `aether-proxy` once (via a one-shot listener), save hex dumps under `crates/gw-tunnel-client/tests/fixtures/aether/`.

Risk-bound PR breakdown:

- **PR 2a**: Structural refactor — `trait Transport`, `AnyTransport`, `StreamTarget`, `gw-proxy` view changes — **with only `cnexus::` implementation**. Behavior identical to Phase 1. All existing tests green. This isolates the structural blast radius.
- **PR 2b**: `aether/` implementation + unit tests + Aether fixture roundtrips. No bridge changes yet (Aether path unreachable from the bridge in this PR).
- **PR 2c**: Bridge branching + Dockerfiles + compose extension + Aether scenarios.

Verification:

- New unit tests under `crates/gw-tunnel-client/src/aether/*.rs` and `crates/gw-tunnel-client/tests/aether_integration.rs` pass via `cargo-remote-ctl test -p gw-tunnel-client`.
- `make tunnel-e2e` reports 12 passed.
- `make e2e` remains green.

### Phase 3 · Mixed-pool stress + dep-boundary + docs

Goal: `make tunnel-e2e` passes 15 cases; `make check-tunnel-deps` reports 0 violations; documentation is current.

Change set:

- `test_mixed_pool.py` (3 cases): load distribution, failover, no-corruption.
- `support/docker_helpers.py`: docker.sock-mediated `kill_service` / `start_service` helpers for failover.
- `scripts/check-tunnel-deps.sh` + `Makefile check-tunnel-deps` target.
- `crates/gw-tunnel-client/README.md` and `crates/gw-tunnel-server/README.md`: dep-boundary warnings.
- `docs/spec/09-tunnel.md`: new sections (`protocol_kind`, `Aether compatibility`, `Crate dependency boundary`).
- `tests/tunnel-e2e/README.md`: usage, scenarios, common-failure-mode guide.

PR breakdown:

- **PR 3a**: `test_mixed_pool.py` + helpers.
- **PR 3b**: `check-tunnel-deps.sh` + docs.

Verification:

- `make tunnel-e2e` reports 15 passed.
- `make check-tunnel-deps` reports 0 violations.
- Performance-baseline warnings (see §8.3) surface as `pytest.warning`, not failures.

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
| Aether heartbeat schema includes undocumented required fields | medium | Phase 2 B7 fails | Capture heartbeat fixture early; serde misses report missing field name, fail-fast |
| `aether-proxy` rejects the Cnexus hub's HeartbeatAck (e.g. expects `remote_config` key present) | medium | proxy disconnects in B7 | If observed during fixture capture, emit an empty `remote_config: {}` rather than omit. Tracked as an Open Item to resolve at Phase 2 start |
| `/api/internal/proxy-tunnel` shared on existing axum port (8000) introduces auth surface | low | misconfigured admin token leak | Bearer enforcement is per-node-token (`auth_token_enc` lookup), independent from the admin token; verify in PR 2c review |
| `tunnel_refresher` for Aether nodes that have never dialed in reports them as "registered but absent" | low | user confusion when reading `/healthz/tunnel` | health endpoint distinguishes `present=false` (proxy never dialed) from `alive=false` (dialed but stale heartbeat); documented in README |

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
- **`protocol_kind`** — discriminator on a `ProxyNode` selecting which wire protocol to use. Values: `cnexus` (default), `aether`.
- **`StreamTarget`** — request to the transport layer to open a stream. Two variants: `Tcp` (Cnexus) and `Http` (Aether).
- **`AnyTransport`** — enum that holds either `cnexus::WsTransport` or `aether::WsTransport`; implements `trait Transport` via delegation.
- **`cargo-remote-ctl`** — project-specific wrapper for remote-executed `cargo` operations (build / test / clippy). Local-only `cargo metadata` operations do not require it.
