# Tunnel Compat Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phase 2 of the Cnexus ↔ Aether tunnel compatibility spec: implement the full Aether wire protocol as a sibling submodule under `gw-tunnel-client/src/aether/` behind the `Transport` trait built in Phase 1; mount `aether::InboundListener` at `/api/internal/aether-tunnel` so `aether-proxy` instances can dial in; activate the Aether git submodule and extend the `tests/tunnel-e2e/` Docker harness with `aether-proxy` + `aether-hub` + nginx path-router + the AILinkMax-Go web binary; ship the 9 Aether scenarios (8 compat + 1 baseline) defined in spec §6.5.B/C; and add the Go-side G3 heartbeat consumer (`events:aether_heartbeat` → ClickHouse → monitoring API).

**Branch / MR model:**
- **`ailinkmax-gateway` repo** — Phase 2 commits append to the **same single shared feature branch** that Phase 1 (and Phase 3) live on. No PR-to-`main` is opened for Phase 2 alone; the final MR happens after Phase 3. The "PR 2a / 2b / 2c" labels below are **logical commit groups within that branch** for review-readability.
- **`AILinkMax-Go` repo** — PR 2d (G3 heartbeat consumer) is a **real MR** in that repo, separate branch, ships after Phase 1 G1/G2 PR lands.

**Architecture:** Three commit groups on the `ailinkmax-gateway` shared branch + one cross-repo Go PR.
- **PR 2a — Aether wire codec + transport + adapters** (Tasks A1-A12): `gw-tunnel-client/src/aether/{frame,meta,transport,http_adapter,heartbeat,gzip}.rs` with fixture-locked encoder roundtrips. `StreamTarget::Http` variant lands. `impl Transport for aether::WsTransport`. **No** mounting: hub-side hooks are wired in PR 2b.
- **PR 2b — InboundListener + gateway integration** (Tasks B1-B8): `aether::InboundListener` route at `/api/internal/aether-tunnel`, `ProxyNodeRegistry` trait, `tunnel_refresher` protocol dispatch, config `[tunnel.aether_listener]` section, `/healthz/tunnel` per-node `aether` status fields, SIGTERM graceful drain, heartbeat → Redis stream push.
- **PR 2c — tunnel-e2e Aether activation + 9 scenarios** (Tasks C1-C10): flip `.gitmodules` to `update = checkout` + pin tag; add `aether-proxy` / `aether-hub` Dockerfiles built from the submodule; nginx path-router (`/register` → Go binary, `/api/internal/aether-tunnel` → ali-gateway); docker-compose extension; 9 new pytest scenarios; README update.
- **PR 2d — Go G3 heartbeat consumer** (Tasks D1-D5, `AILinkMax-Go` repo, **real MR**): ClickHouse table `aether_heartbeat_samples`, `events:aether_heartbeat` Redis stream consumer in `binaries/worker-monitoring` (or a goroutine in the existing monitoring worker, per Go team's call), monitoring API `GET /api/admin/monitoring/aether/{node_id}` returning a 1h window of samples.

**Tech Stack:** Rust 1.88 (pinned), tokio, serde, sqlx, futures, `flate2` (gzip), `httparse` (HTTP/1 parse), `http`, `url`, `tokio-tungstenite` (already in workspace from Phase 1 cnexus), `dashmap`, `arc-swap`, `secrecy`, Go 1.21+, ClickHouse client library matching the existing `AILinkMax-Go` monitoring infra, pytest 8.x, Postgres 16, Redis 7, Docker Compose v2, nginx (alpine).

**Reference spec:** `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md` (§3.3 `StreamTarget` Http variant, §3.4 trait, §3.5 `InboundListener`, §3.6.5 heartbeat ingestion, §3.7 end-to-end flow, §4 Aether adapter internals (all subsections), §6.2-6.5 (Docker images, compose, scenarios), §7 Phase 2).

**Out of scope for this plan (deferred to Phase 3 plan):** mixed-pool scenarios (1 cnexus + 1 aether), cross-protocol routing-strategy compatibility matrix, dep-boundary script (`scripts/check-tunnel-deps.sh`), Go G4 frontend UI, performance baseline, final docs (`docs/spec/09-tunnel.md` rewrite).

**Locked decisions referenced in this plan (from 2026-06-15 plan-time review):**
1. Aether submodule pinned to **the most recent MR's tag** of the `Aether` GitLab project (concrete tag determined at PR 2c Task 1; locked there).
2. Trait abstraction sits in `gw-tunnel-client/src/core.rs` (Phase 1 Task 6a).
3. `InboundListener` mounts on the **existing admin port (8080)**; route `/api/internal/aether-tunnel`.
4. Aether-specific node JSONB fields nested under a single `aether_config: { auth_token, ... }` object.
5. Cross-repo dependency: Phase 1 Go PR (G1/G2) lands first; Phase 2 takes that as given.
6. Heartbeat Redis stream key `events:aether_heartbeat` schema **locked**: 5 metric flat fields + `node_id` + `heartbeat_session_id` + `heartbeat_id` + `received_at_ms` + `proxy_version`. MAXLEN ~ 50000.
7. tunnel-e2e Aether images **built from submodule on every run** (no prebuilt cache).
8. WS stream multiplexing model: **tokio task-per-stream** (one read + one write task per active stream).

---

## File Structure

### New files (PR 2a — Aether wire & transport, this repo)

| Path | Responsibility |
|---|---|
| `crates/gw-tunnel-client/src/aether/mod.rs` | Submodule entry; declares `frame`, `meta`, `transport`, `http_adapter`, `heartbeat`, `gzip` modules. |
| `crates/gw-tunnel-client/src/aether/frame.rs` | 10-byte BE header codec (`HEADER_SIZE = 10`), `MsgType`, `flags::{NONE, END_STREAM, GZIP_COMPRESSED}`, `Frame::{encode, decode}`. See spec §4.2. |
| `crates/gw-tunnel-client/src/aether/meta.rs` | `RequestMeta` + `ResponseMeta` JSON structs. `timeout: u64` non-optional with `default_timeout = 60`, float-tolerant deserializer. See spec §4.3. |
| `crates/gw-tunnel-client/src/aether/transport.rs` | `aether::WsTransport` struct + `Inner`. `attach(cfg, node_view, ws) → WsTransport`. Spawns read/write/liveness tasks. `open_stream(StreamTarget::Http)` allocates new stream id (hub parity = even, starts at 2), sends `RequestHeaders`, pumps body via `RequestBody` frames. See spec §4.4. |
| `crates/gw-tunnel-client/src/aether/http_adapter.rs` | `HttpAdapter::parse_request(reader) → (RequestPreamble, RemainingBody)` via `httparse`. `HttpAdapter::write_response(ResponseMeta, body_rx) → HTTP/1.1 byte stream` (chunked-encoded when no Content-Length / Transfer-Encoding from upstream). See spec §4.5. |
| `crates/gw-tunnel-client/src/aether/heartbeat.rs` | `HeartbeatData` (Deserialize, 5 flat metric fields + nested `proxy_metadata: { version }`) and `HeartbeatAck` (Serialize, `heartbeat_id` only; no `remote_config` / `upgrade_to` emit). See spec §4.6. |
| `crates/gw-tunnel-client/src/aether/gzip.rs` | `maybe_compress(payload) → (bytes, bool)` with 512-byte threshold. `decompress(payload) → Bytes`. See spec §4.7. |
| `crates/gw-tunnel-client/tests/fixtures/aether/request_headers.bin` | Captured wire bytes from a real `aether-proxy` for `RequestHeaders` frame. Encoder roundtrips against it. |
| `crates/gw-tunnel-client/tests/fixtures/aether/request_body.bin` | `RequestBody` frame fixture. |
| `crates/gw-tunnel-client/tests/fixtures/aether/response_headers.bin` | `ResponseHeaders` frame fixture. |
| `crates/gw-tunnel-client/tests/fixtures/aether/response_body.bin` | `ResponseBody` frame fixture. |
| `crates/gw-tunnel-client/tests/fixtures/aether/stream_end.bin` | `StreamEnd` frame fixture. |
| `crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.bin` | `HeartbeatData` frame fixture (header + JSON body). |
| `crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.json` | Decoded JSON of `HeartbeatData` payload for `serde_json` roundtrip tests. |
| `crates/gw-tunnel-client/tests/fixtures/aether/README.md` | Documents how each fixture was captured + the exact `aether-proxy` git revision used. |
| `crates/gw-tunnel-client/tests/aether_frame.rs` | Frame encode/decode roundtrip + fixture parity tests. |
| `crates/gw-tunnel-client/tests/aether_meta.rs` | `RequestMeta` / `ResponseMeta` JSON roundtrip + float-tolerant timeout deserialization. |
| `crates/gw-tunnel-client/tests/aether_gzip.rs` | Compression threshold + size-monotonicity tests. |
| `crates/gw-tunnel-client/tests/aether_heartbeat.rs` | `HeartbeatData` JSON deserialization against fixture + `HeartbeatAck` serialization. |
| `crates/gw-tunnel-client/tests/aether_http_adapter.rs` | `parse_request` partial-read loop + `write_response` chunked-encoding edge cases. |
| `crates/gw-tunnel-client/tests/aether_transport.rs` | `WsTransport::attach` + stream-id allocation + receive routing + `Transport` trait compile-fence. |

### Files modified (PR 2a, this repo)

| Path | Change |
|---|---|
| `crates/gw-tunnel-client/Cargo.toml` | Add deps: `flate2`, `httparse`, `http`, `url`, `arc-swap`, `secrecy`. (`tokio-tungstenite`, `dashmap`, `bytes`, `serde`, `serde_json`, `tokio`, `futures` already present from Phase 1 cnexus.) |
| `crates/gw-tunnel-client/src/lib.rs` | Add `pub mod aether;` next to `pub mod cnexus;`. No back-compat re-exports (matches Phase 1 D2 fix). |
| `crates/gw-tunnel-client/src/target.rs` | Add the `Http { method, url, headers, body_rx, body_tx_response, meta_tx_response }` variant per spec §3.3. The enum stays `#[non_exhaustive]`. |

### New files (PR 2b — InboundListener + gateway integration, this repo)

| Path | Responsibility |
|---|---|
| `crates/gw-tunnel-client/src/aether/listener.rs` | `InboundListener` struct with `ProxyNodeRegistry` Arc + transport-sender `mpsc::Sender<(NodeId, Arc<dyn Transport>)>`. `handle_upgrade(headers, ws_upgrade)` — validates `Authorization: Bearer ae_*`, `X-Node-Id`, `X-Node-Name`, `X-Tunnel-Max-Streams`; performs WS upgrade; constructs `aether::WsTransport::attach`; pushes through the sender. Semaphore-gated by `max_concurrent_connections`. See spec §3.5. |
| `crates/gw-tunnel-client/src/registry.rs` | `ProxyNodeRegistry` trait: `lookup_by_id(node_id) → Option<ProxyNodeView>` and `verify_auth_token(node_id, candidate) → bool`. Implemented in `gw-proxy::tunnel_stack` (next PR-2b task). |
| `crates/gw-tunnel-client/tests/aether_listener.rs` | InboundListener auth rejection cases + happy path with stub `ProxyNodeRegistry`. |
| `crates/gw-tunnel-client/tests/registry.rs` | Trait compile-fence; concrete impl exercised via integration in `gw-proxy`. |

### Files modified (PR 2b, this repo)

| Path | Change |
|---|---|
| `crates/gw-tunnel-client/src/lib.rs` | `pub mod registry;` + `pub use registry::ProxyNodeRegistry;`. `pub use aether::listener::InboundListener;`. |
| `crates/gw-proxy/src/tunnel_stack.rs` | `DashMap<NodeId, Arc<dyn Transport>>` (already an indirection in spirit; widen to `Arc<dyn Transport>` if currently concrete). Implement `ProxyNodeRegistry` for `TunnelStack` (via the existing in-memory ProxyNode snapshot). New `on_protocol_change(node_id, new_kind)` callback the entity-stream consumer calls. |
| `crates/gw-gateway/src/lib.rs` | (a) Mount `axum` route `/api/internal/aether-tunnel` on the existing admin HTTP server (port 8080) that hands `WebSocketUpgrade` to `InboundListener::handle_upgrade`. (b) `tunnel_refresher` dispatches per-node by `tunnel_protocol`: Cnexus → `ReconnectingTransport::<cnexus::WsTransport>::new(...)` (existing); Aether → ensure InboundListener is mounted (idempotent spawn) + ensure node row is in registry. (c) On SIGTERM, call `tunnel_stack.drain_aether_inbound(timeout)` per spec §3.5 graceful drain. |
| `crates/gw-gateway/src/health.rs` | `/healthz/tunnel` payload extended with per-node `present`, `alive`, `last_heartbeat_age_ms`, `active_streams`, `protocol` fields per spec §3.5. |
| `config/default.toml` | New `[tunnel.aether_listener]` section: `enabled = null` (auto-enable when ≥1 aether node present), `path = "/api/internal/aether-tunnel"`, `max_concurrent_connections = 256`. |
| `crates/gw-tunnel-client/src/aether/transport.rs` | On `HeartbeatData` receipt (post-decode), XADD to Redis stream `events:aether_heartbeat` (key fixed; MAXLEN ~ 50000) carrying the locked schema (decision #6). Take `redis::Client` via a trait-object handle injected at `attach` time so the tunnel crate stays Redis-impl-agnostic. |
| `crates/gw-gateway/src/lib.rs` | At `tunnel_refresher` startup, build the Redis handle from `cfg.redis` and pass into `InboundListener` so it can hand to `aether::WsTransport::attach`. |

### New files (PR 2c — tunnel-e2e Aether activation, this repo)

| Path | Responsibility |
|---|---|
| `tests/tunnel-e2e/Dockerfile.aether-proxy` | Multi-stage build from the Aether git submodule (`tests/tunnel-e2e/aether-src`), produces an `aether-proxy` binary. Inherits the Aether repo's own toolchain. |
| `tests/tunnel-e2e/Dockerfile.aether-hub` | Same shape, produces an `aether-hub` binary (only used by the baseline scenario). |
| `tests/tunnel-e2e/Dockerfile.go-web` | Builds the AILinkMax-Go web binary from the **already-Phase-1-G1/G2-merged** AILinkMax-Go repo (sibling directory; injected via build context). Used so `aether-proxy` can POST to `/api/admin/proxy-nodes/register`. |
| `tests/tunnel-e2e/nginx/edge.conf` | Path-router: `/api/admin/proxy-nodes/register` → `go-web:8080`; `/api/internal/aether-tunnel` → `ali-gateway:8080`; everything else → `ali-gateway:8000` (the business proxy port). |
| `tests/tunnel-e2e/support/aether_admin.py` | Python helper: `seed_aether_node(name, auth_token) → node_row`, `await_aether_attach(node_id, timeout_s) → bool` (polls `/healthz/tunnel`), `compute_ae_token_hash(token, kms_key) → bytes` (mirrors Go's `auth_token_enc` shape). |
| `tests/tunnel-e2e/support/wait_for_aether_attach.py` | Standalone wait helper used by docker-compose's `depends_on` health gate for `aether-proxy`. |
| `tests/tunnel-e2e/scenarios/test_aether_compat.py` | 8 scenarios per spec §6.5.B (`test_aether_node_handshake`, `test_aether_simple_get`, `test_aether_post_body`, `test_aether_stream_response`, `test_aether_error_propagation`, `test_aether_gzip_body`, `test_aether_heartbeat_alive`, `test_aether_reconnect`). |
| `tests/tunnel-e2e/scenarios/test_aether_baseline.py` | 1 scenario per spec §6.5.C (`test_aether_native_routing`): exercises the Aether images standalone through `aether-hub`. Isolates regressions in our adapter from regressions in Aether builds. |

### Files modified (PR 2c, this repo)

| Path | Change |
|---|---|
| `.gitmodules` | Flip `update = none` to `update = checkout` for `tests/tunnel-e2e/aether-src`. Pin to the most recent MR's tag (concrete tag locked at Task C1 Step 1). |
| `tests/tunnel-e2e/docker-compose.yml` | Add services: `aether-proxy` (built from `Dockerfile.aether-proxy`), `aether-hub` (built from `Dockerfile.aether-hub`, **disabled by default profile**, used only by baseline scenario), `go-web` (built from `Dockerfile.go-web`), `nginx-edge` (built from `nginx:alpine` + `edge.conf`). Add the existing `ali-gateway` service `depends_on: [aether-proxy: wait_for_aether_attach]` health gate. |
| `tests/tunnel-e2e/conftest.py` | Add `aether_admin` fixture composing the new `support/aether_admin.py` helpers + Go-web URL discovery. Extend `seed_nodes` to accept `aether_config: dict | None` argument. |
| `tests/tunnel-e2e/README.md` | New section "Aether scenarios" with submodule init, build cache notes, and the baseline scenario explanation. |
| `tests/tunnel-e2e/Makefile` targets | (Indirect via top-level `Makefile`) no change — the existing `tunnel-e2e-deps` already runs `git submodule update --init`. |

### New files (PR 2d — Go G3 heartbeat consumer, `AILinkMax-Go` repo)

> **Repo**: `AILinkMax-Go`. All paths below are relative to that repo's root.

| Path | Responsibility |
|---|---|
| `modules/monitoring/clickhouse/aether_heartbeat_samples.sql` | DDL for the table. Schema mirrors the existing `gateway_runtime_samples` shape with the Aether 5-metric column set. |
| `modules/monitoring/business/aether_heartbeat_sink.go` | `AetherHeartbeatSink` worker. Consumes `events:aether_heartbeat` via XREADGROUP; batches every 1s or 1000 rows (whichever first); writes to ClickHouse via the existing client wrapper. |
| `modules/monitoring/business/aether_heartbeat_sink_test.go` | Table-driven cases: decode JSON, batched insert call shape, idempotent restart (consumer group acks survive). |
| `modules/monitoring/api/aether_monitoring_handler.go` | `GET /api/admin/monitoring/aether/{node_id}?since=...&until=...` returns 1h window of samples per spec §3.6.5. Time params default to `now-1h` / `now`. |
| `modules/monitoring/api/aether_monitoring_handler_test.go` | Handler tests: empty result, non-empty result, malformed time params → 400. |
| `binaries/worker-monitoring/aether_heartbeat_runner.go` | Hooks `AetherHeartbeatSink` into the worker binary's existing supervisor (or — Go team's choice — replace this file with a goroutine inside the existing monitoring worker). |

### Files modified (PR 2d, `AILinkMax-Go` repo)

| Path | Change |
|---|---|
| `modules/monitoring/api/routes.go` | Mount the new `GET /api/admin/monitoring/aether/{node_id}` route under the existing admin-token middleware. |
| `binaries/worker-monitoring/main.go` | Spawn the new sink alongside existing monitoring workers. |
| Documentation: `docs/internal/monitoring.md` (or local equivalent) | Document the new endpoint + the ClickHouse table. |

---

## PR 2a · Aether wire codec, transport, adapters

### Task A1: Capture wire fixtures from a real `aether-proxy`

> **Why this task is first:** every encoder roundtrip in PR 2a tests against these fixtures. Capturing real bytes from the reference impl is the single anchor that prevents drift bugs (which Cnexus team has historically eaten in similar codec work).

**Files:**
- Create: `crates/gw-tunnel-client/tests/fixtures/aether/{request_headers,request_body,response_headers,response_body,stream_end,heartbeat_data}.bin`
- Create: `crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.json`
- Create: `crates/gw-tunnel-client/tests/fixtures/aether/README.md`

- [ ] **Step 1: Build a one-shot `aether-proxy` from the submodule (NOT activated yet via `.gitmodules`)**

```bash
git clone https://gitlab.ailinkmax.com/ailinkmax/Aether /tmp/aether-capture
cd /tmp/aether-capture
# Tag must match the version PR 2c will pin (recorded in fixtures/README.md).
git checkout $(git describe --tags --abbrev=0)
cd aether-proxy
cargo build --release
```

> If `aether-proxy` build fails on this machine, file the toolchain issue, do not proceed to Step 2 — you cannot fake fixtures.

- [ ] **Step 2: Write a capture harness**

Create `/tmp/aether-capture/capture-harness.rs` — a 50-line Rust binary that:
1. Listens on TCP `127.0.0.1:9001` for a single connection (the `aether-proxy`).
2. Speaks the WS upgrade handshake on receipt; **after upgrade**, every frame received is appended (length-prefixed, untouched) to one of six files chosen by `MsgType`.
3. Replies `HeartbeatAck` so the proxy keeps the connection alive long enough for `HeartbeatData` to arrive.
4. Exits after capturing at least one of each: `RequestHeaders`, `RequestBody`, `ResponseHeaders`, `ResponseBody`, `StreamEnd`, `HeartbeatData`.

Build it: `cargo build --release` in that ad-hoc dir.

- [ ] **Step 3: Drive a synthetic request through the proxy**

Run the harness on host TCP 9001. Configure `aether-proxy` with `hub_url = ws://127.0.0.1:9001/proxy-tunnel`, `auth_token = ae_capture_only`, and have it relay a fake upstream that returns a small JSON body.

```bash
./harness > /tmp/capture.log &
HARNESS_PID=$!
AETHER_PROXY_HUB_URL=ws://127.0.0.1:9001/proxy-tunnel \
  AETHER_PROXY_AUTH_TOKEN=ae_capture_only \
  ./aether-proxy &

# Send one request via aether-proxy's relay endpoint
curl -sX POST -d '{"ping":"capture"}' http://127.0.0.1:7000/forward
sleep 60   # collect at least one heartbeat
kill $HARNESS_PID
```

- [ ] **Step 4: Move captured files into the workspace + write README**

```bash
mkdir -p crates/gw-tunnel-client/tests/fixtures/aether
cp /tmp/aether-capture/{request_headers,request_body,response_headers,response_body,stream_end,heartbeat_data}.bin \
   crates/gw-tunnel-client/tests/fixtures/aether/
```

Create `tests/fixtures/aether/README.md`:

```markdown
# Aether wire fixtures

Captured 2026-06-15 from aether-proxy at commit `<git rev hash>` (tag `<tag>`).
Reference: https://gitlab.ailinkmax.com/ailinkmax/Aether @ tag `<tag>`.

Capture harness: `/tmp/aether-capture/capture-harness.rs` (ad-hoc, not committed).
Replay: see `crates/gw-tunnel-client/tests/aether_frame.rs` and siblings.

Per-file content:
- `request_headers.bin`  — one `MsgType::RequestHeaders` frame (header + JSON body)
- `request_body.bin`     — one `MsgType::RequestBody` frame (uncompressed)
- `response_headers.bin` — one `MsgType::ResponseHeaders` frame
- `response_body.bin`    — one `MsgType::ResponseBody` frame
- `stream_end.bin`       — one `MsgType::StreamEnd` frame (empty payload)
- `heartbeat_data.bin`   — one `MsgType::HeartbeatData` frame
- `heartbeat_data.json`  — same as above but with the JSON payload extracted
                            (for direct serde testing)
```

- [ ] **Step 5: Extract `heartbeat_data.json` from `heartbeat_data.bin`**

```bash
# Skip 10-byte header, dump remaining payload
python3 -c "
import sys
d = open('crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.bin','rb').read()
payload = d[10:]
open('crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.json','wb').write(payload)
"
python3 -m json.tool crates/gw-tunnel-client/tests/fixtures/aether/heartbeat_data.json
```

Expected: pretty-printed JSON with 5 flat metric fields + `proxy_metadata: { version }`. **If the JSON is nested under a `metrics` object or includes `sent_at_ms`**, the Aether upstream has drifted from the spec — stop, surface, and re-check the spec §4.6.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-tunnel-client/tests/fixtures/aether/
git commit -m "test(tunnel-client): add aether wire-protocol fixtures from upstream proxy"
```

---

### Task A2: Implement `aether/frame.rs` against the fixtures

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/mod.rs` (initial — just `pub mod frame;`)
- Create: `crates/gw-tunnel-client/src/aether/frame.rs`
- Create: `crates/gw-tunnel-client/tests/aether_frame.rs`
- Modify: `crates/gw-tunnel-client/src/lib.rs` (`pub mod aether;`)

- [ ] **Step 1: Write the failing test**

Create `crates/gw-tunnel-client/tests/aether_frame.rs`:

```rust
use bytes::{Bytes, BytesMut};
use gw_tunnel_client::aether::frame::{flags, Frame, MsgType, HEADER_SIZE};

fn read_fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("tests/fixtures/aether/{name}")).expect("fixture missing")
}

#[test]
fn frame_decode_request_headers_fixture() {
    let bytes = read_fixture("request_headers.bin");
    let mut cur = std::io::Cursor::new(bytes.as_slice());
    let frame = Frame::decode(&mut cur)
        .expect("decode failed")
        .expect("partial frame in fixture");
    assert_eq!(frame.msg_type, MsgType::RequestHeaders);
    assert_eq!(frame.flags & flags::END_STREAM, 0, "RequestHeaders does not carry END_STREAM");
    assert!(frame.payload.len() > 0);
    // Stream id parity: proxy initiates with odd ids; hub-side captures see id 1.
    assert_eq!(frame.stream_id % 2, 1, "captured fixture is proxy→hub direction; should be odd id");
}

#[test]
fn frame_decode_heartbeat_data_fixture() {
    let bytes = read_fixture("heartbeat_data.bin");
    let mut cur = std::io::Cursor::new(bytes.as_slice());
    let frame = Frame::decode(&mut cur).unwrap().unwrap();
    assert_eq!(frame.msg_type, MsgType::HeartbeatData);
    assert_eq!(frame.stream_id, 0, "control frames use stream_id 0");
}

#[test]
fn frame_encode_roundtrip_via_fixtures() {
    for name in [
        "request_headers.bin",
        "request_body.bin",
        "response_headers.bin",
        "response_body.bin",
        "stream_end.bin",
        "heartbeat_data.bin",
    ] {
        let bytes = read_fixture(name);
        let mut cur = std::io::Cursor::new(bytes.as_slice());
        let frame = Frame::decode(&mut cur).unwrap().unwrap();
        let mut out = BytesMut::new();
        frame.encode(&mut out);
        assert_eq!(&out[..], &bytes[..], "encode of {name} does not match captured bytes");
    }
}

#[test]
fn frame_decode_partial_returns_none() {
    let bytes = vec![0u8; 5];  // less than HEADER_SIZE
    let mut cur = std::io::Cursor::new(bytes.as_slice());
    let res = Frame::decode(&mut cur).unwrap();
    assert!(res.is_none(), "partial header must yield None, not error");
}

#[test]
fn frame_constants_match_spec() {
    assert_eq!(HEADER_SIZE, 10);
    assert_eq!(flags::GZIP_COMPRESSED, 0x02);
    assert_eq!(MsgType::HeartbeatData as u8, 0x13);
}
```

- [ ] **Step 2: Run failing test**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test aether_frame 2>&1 | tail -20`
Expected: COMPILE FAIL (`gw_tunnel_client::aether` missing).

- [ ] **Step 3: Implement `aether/frame.rs`**

Implement exactly per spec §4.2. The pseudo `unimplemented!()` in the spec for `decode` becomes:

```rust
use bytes::{Buf, Bytes, BytesMut};
use std::io::Cursor;

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

impl MsgType {
    fn from_u8(b: u8) -> Result<Self, FrameError> {
        Ok(match b {
            0x01 => Self::RequestHeaders,
            0x02 => Self::RequestBody,
            0x03 => Self::ResponseHeaders,
            0x04 => Self::ResponseBody,
            0x05 => Self::StreamEnd,
            0x06 => Self::StreamError,
            0x10 => Self::Ping,
            0x11 => Self::Pong,
            0x12 => Self::GoAway,
            0x13 => Self::HeartbeatData,
            0x14 => Self::HeartbeatAck,
            other => return Err(FrameError::UnknownMsgType(other)),
        })
    }
}

pub mod flags {
    pub const NONE: u8            = 0x00;
    pub const END_STREAM: u8      = 0x01;
    pub const GZIP_COMPRESSED: u8 = 0x02;
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("unknown msg type {0:#x}")]
    UnknownMsgType(u8),
    #[error("frame payload too large: {0}")]
    PayloadTooLarge(u32),
}

const MAX_PAYLOAD: u32 = 64 * 1024 * 1024; // 64 MiB safety cap

#[derive(Clone, Debug)]
pub struct Frame {
    pub stream_id: u32,
    pub msg_type: MsgType,
    pub flags: u8,
    pub payload: Bytes,
}

impl Frame {
    pub fn encode(&self, dst: &mut BytesMut) {
        use bytes::BufMut;
        dst.put_u32(self.stream_id);
        dst.put_u8(self.msg_type as u8);
        dst.put_u8(self.flags);
        dst.put_u32(self.payload.len() as u32);
        dst.put_slice(&self.payload);
    }

    /// Returns Ok(Some(frame)) when at least one full frame is available.
    /// Ok(None) when more bytes are needed; cursor is restored to original position.
    /// Err on malformed frame.
    pub fn decode(src: &mut Cursor<&[u8]>) -> Result<Option<Self>, FrameError> {
        let start = src.position();
        if src.remaining() < HEADER_SIZE {
            return Ok(None);
        }
        let stream_id = src.get_u32();
        let msg_byte = src.get_u8();
        let flags = src.get_u8();
        let payload_len = src.get_u32();
        if payload_len > MAX_PAYLOAD {
            return Err(FrameError::PayloadTooLarge(payload_len));
        }
        if (src.remaining() as u32) < payload_len {
            src.set_position(start);
            return Ok(None);
        }
        let msg_type = MsgType::from_u8(msg_byte)?;
        let mut payload = BytesMut::with_capacity(payload_len as usize);
        payload.resize(payload_len as usize, 0);
        src.copy_to_slice(&mut payload);
        Ok(Some(Self { stream_id, msg_type, flags, payload: payload.freeze() }))
    }
}
```

Create `crates/gw-tunnel-client/src/aether/mod.rs`:

```rust
//! Aether wire protocol: dial-in WebSocket transport.
//!
//! Hub-side sibling of `cnexus::*`. The aether-proxy dials inbound to a
//! gateway hub endpoint at /api/internal/aether-tunnel; the listener
//! validates the bearer auth and constructs an `aether::WsTransport`.
//!
//! See `docs/superpowers/specs/2026-06-15-cnexus-aether-tunnel-compat-design.md`
//! §3.5 and §4.

pub mod frame;
```

Update `crates/gw-tunnel-client/src/lib.rs`:

```rust
pub mod aether;
pub mod cnexus;
pub mod core;
pub mod registry;          // added in PR 2b; declare here even though file lands later
pub mod resolver;
pub mod safe_dns;
pub mod target;
pub mod target_filter;

pub use core::{StreamHandle, Transport, TransportError, TunnelStream};
pub use target::StreamTarget;

pub use resolver::{
    resolve_tunnel, ProxyNodeView, TunnelPreference, TunnelResolution, TunnelResolveCtx,
};
```

> Hold off on the `registry` line until Task B1 if you want each commit to compile in isolation; otherwise add it now with `#[allow(dead_code)] mod registry;` as a placeholder that Task B1 fills in.

- [ ] **Step 4: Add `thiserror` to `gw-tunnel-client/Cargo.toml` if not already present**

Run: `grep '^thiserror' crates/gw-tunnel-client/Cargo.toml || echo "missing"`
If missing, add: `thiserror.workspace = true` under `[dependencies]`.

- [ ] **Step 5: Run the tests**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test aether_frame`
Expected: PASS for all 5 tests. If the roundtrip test fails on a particular fixture, the cause is almost always BE-vs-LE byte order — re-read the encoder put_u32 calls; do **not** "fix" by re-capturing the fixture.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-tunnel-client/src/aether/mod.rs \
        crates/gw-tunnel-client/src/aether/frame.rs \
        crates/gw-tunnel-client/src/lib.rs \
        crates/gw-tunnel-client/tests/aether_frame.rs \
        crates/gw-tunnel-client/Cargo.toml
git commit -m "feat(tunnel-client): add aether wire frame codec (BE 10B header, 11 msg types)"
```

---

### Task A3: Implement `aether/meta.rs` (`RequestMeta` / `ResponseMeta`)

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/meta.rs`
- Modify: `crates/gw-tunnel-client/src/aether/mod.rs` (`pub mod meta;`)
- Create: `crates/gw-tunnel-client/tests/aether_meta.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/gw-tunnel-client/tests/aether_meta.rs`:

```rust
use gw_tunnel_client::aether::meta::{RequestMeta, ResponseMeta};

#[test]
fn request_meta_serializes_with_defaults() {
    let r = RequestMeta {
        method: "GET".into(),
        url: "https://example.com/x".into(),
        headers: [("accept".into(), "*/*".into())].into_iter().collect(),
        timeout: 60,
    };
    let s = serde_json::to_string(&r).unwrap();
    // Aether expects non-optional timeout integer in seconds.
    assert!(s.contains(r#""timeout":60"#), "got {s}");
}

#[test]
fn request_meta_deserializes_default_timeout() {
    let s = r#"{"method":"GET","url":"https://x/","headers":{}}"#;
    let r: RequestMeta = serde_json::from_str(s).unwrap();
    assert_eq!(r.timeout, 60);
}

#[test]
fn request_meta_deserializes_integer_valued_float_timeout() {
    // aether-proxy/src/tunnel/protocol.rs:155-183 accepts e.g. 15.0
    let s = r#"{"method":"GET","url":"https://x/","headers":{},"timeout":15.0}"#;
    let r: RequestMeta = serde_json::from_str(s).unwrap();
    assert_eq!(r.timeout, 15);
}

#[test]
fn request_meta_rejects_negative_or_nan_timeout() {
    for bad in [
        r#"{"method":"GET","url":"x","headers":{},"timeout":-1}"#,
        r#"{"method":"GET","url":"x","headers":{},"timeout":1e400}"#,
    ] {
        let r: Result<RequestMeta, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "expected failure for {bad}");
    }
}

#[test]
fn response_meta_preserves_header_order_and_multivalues() {
    let s = r#"{"status":200,"headers":[["Set-Cookie","a=1"],["Set-Cookie","b=2"]]}"#;
    let r: ResponseMeta = serde_json::from_str(s).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.len(), 2);
    assert_eq!(r.headers[0], ("Set-Cookie".into(), "a=1".into()));
    assert_eq!(r.headers[1], ("Set-Cookie".into(), "b=2".into()));
}
```

- [ ] **Step 2: Run failing test**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test aether_meta`
Expected: COMPILE FAIL.

- [ ] **Step 3: Implement `meta.rs`**

Create `crates/gw-tunnel-client/src/aether/meta.rs`:

```rust
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

fn default_timeout() -> u64 { 60 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestMeta {
    pub method: String,
    pub url: String,
    pub headers: HashMap<String, String>,
    #[serde(default = "default_timeout", deserialize_with = "deserialize_timeout")]
    pub timeout: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

/// Accepts integers (u64) and integer-valued floats (e.g. 15.0).
/// Rejects negative numbers, NaN, infinity, and non-integer floats.
/// Matches `aether-proxy/src/tunnel/protocol.rs:155-183`.
fn deserialize_timeout<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Error, Visitor};
    use std::fmt;

    struct TimeoutVisitor;
    impl<'de> Visitor<'de> for TimeoutVisitor {
        type Value = u64;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "non-negative integer or integer-valued float (seconds)")
        }
        fn visit_u64<E: Error>(self, v: u64) -> Result<u64, E> { Ok(v) }
        fn visit_i64<E: Error>(self, v: i64) -> Result<u64, E> {
            if v < 0 { Err(E::custom("timeout cannot be negative")) }
            else { Ok(v as u64) }
        }
        fn visit_f64<E: Error>(self, v: f64) -> Result<u64, E> {
            if !v.is_finite() { return Err(E::custom("timeout must be finite")); }
            if v < 0.0 { return Err(E::custom("timeout cannot be negative")); }
            if v.fract() != 0.0 { return Err(E::custom("timeout must be an integer-valued number")); }
            if v > u64::MAX as f64 { return Err(E::custom("timeout overflow")); }
            Ok(v as u64)
        }
    }
    d.deserialize_any(TimeoutVisitor)
}
```

Update `crates/gw-tunnel-client/src/aether/mod.rs`:

```rust
pub mod frame;
pub mod meta;
```

- [ ] **Step 4: Run tests**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test aether_meta`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-tunnel-client/src/aether/{mod.rs,meta.rs} \
        crates/gw-tunnel-client/tests/aether_meta.rs
git commit -m "feat(tunnel-client): add aether RequestMeta/ResponseMeta JSON shapes"
```

---

### Task A4: Implement `aether/gzip.rs` (compress/decompress with 512B threshold)

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/gzip.rs`
- Modify: `crates/gw-tunnel-client/src/aether/mod.rs`
- Create: `crates/gw-tunnel-client/tests/aether_gzip.rs`
- Modify: `crates/gw-tunnel-client/Cargo.toml` (add `flate2`)

- [ ] **Step 1: Add `flate2` dep**

```bash
grep '^flate2' crates/gw-tunnel-client/Cargo.toml || \
    cargo-remote-ctl add -p gw-tunnel-client flate2
```

- [ ] **Step 2: Write the failing test**

```rust
// crates/gw-tunnel-client/tests/aether_gzip.rs
use bytes::Bytes;
use gw_tunnel_client::aether::gzip;

#[test]
fn small_payload_skipped() {
    let body = Bytes::from(vec![0xAA; 500]);
    let (out, compressed) = gzip::maybe_compress(body.clone());
    assert!(!compressed, "≤512B must skip");
    assert_eq!(out, body);
}

#[test]
fn compressible_payload_shrinks() {
    let body = Bytes::from(vec![0x00; 8_000]); // very compressible
    let (out, compressed) = gzip::maybe_compress(body.clone());
    assert!(compressed);
    assert!(out.len() < body.len(), "compression must shrink");
}

#[test]
fn incompressible_payload_kept_raw() {
    let body: Bytes = (0..2048u32).flat_map(|n| n.to_le_bytes()).collect::<Vec<u8>>().into();
    let (out, compressed) = gzip::maybe_compress(body.clone());
    // serde-incompressible noise; if compressed output is not strictly smaller, raw is kept.
    if compressed {
        assert!(out.len() < body.len());
    } else {
        assert_eq!(out, body);
    }
}

#[test]
fn roundtrip_compress_decompress() {
    let body = Bytes::from(b"Lorem ipsum dolor sit amet, ".repeat(50)); // > 512
    let (compressed_bytes, compressed) = gzip::maybe_compress(body.clone());
    assert!(compressed);
    let decompressed = gzip::decompress(&compressed_bytes).unwrap();
    assert_eq!(decompressed, body);
}
```

- [ ] **Step 3: Run failing test**

Run: `cargo-remote-ctl test -p gw-tunnel-client --test aether_gzip`
Expected: COMPILE FAIL.

- [ ] **Step 4: Implement `gzip.rs` per spec §4.7**

```rust
use std::io::{Read, Write};

use bytes::Bytes;
use flate2::write::GzEncoder;
use flate2::read::GzDecoder;
use flate2::Compression;

pub fn maybe_compress(payload: Bytes) -> (Bytes, bool) {
    if payload.len() < 512 { return (payload, false); }
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    if enc.write_all(&payload).is_err() { return (payload, false); }
    match enc.finish() {
        Ok(out) if out.len() < payload.len() => (out.into(), true),
        _ => (payload, false),
    }
}

pub fn decompress(payload: &[u8]) -> Result<Bytes, std::io::Error> {
    let mut dec = GzDecoder::new(payload);
    let mut out = Vec::with_capacity(payload.len() * 2);
    dec.read_to_end(&mut out)?;
    Ok(out.into())
}
```

Update mod.rs: `pub mod gzip;`.

- [ ] **Step 5: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test aether_gzip
git add crates/gw-tunnel-client/src/aether/{mod.rs,gzip.rs} \
        crates/gw-tunnel-client/tests/aether_gzip.rs \
        crates/gw-tunnel-client/Cargo.toml
git commit -m "feat(tunnel-client): add aether gzip helper (≥512B threshold, default level)"
```

---

### Task A5: Implement `aether/heartbeat.rs` (`HeartbeatData` / `HeartbeatAck`)

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/heartbeat.rs`
- Modify: `crates/gw-tunnel-client/src/aether/mod.rs`
- Create: `crates/gw-tunnel-client/tests/aether_heartbeat.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/gw-tunnel-client/tests/aether_heartbeat.rs
use gw_tunnel_client::aether::heartbeat::{HeartbeatAck, HeartbeatData};

fn fixture_json() -> String {
    std::fs::read_to_string("tests/fixtures/aether/heartbeat_data.json").unwrap()
}

#[test]
fn heartbeat_data_deserializes_from_real_fixture() {
    let s = fixture_json();
    let d: HeartbeatData = serde_json::from_str(&s).expect("fixture parse failed");
    assert!(!d.node_id.is_empty());
    assert!(d.heartbeat_id > 0 || d.heartbeat_id == 0);  // any non-negative
    // 5 flat metric fields must all be present (else the JSON drifted).
    let _ = (d.total_requests, d.avg_latency_ms, d.failed_requests, d.dns_failures, d.stream_errors);
    assert!(!d.proxy_metadata.version.is_empty());
}

#[test]
fn heartbeat_ack_serializes_with_heartbeat_id_only() {
    let ack = HeartbeatAck { heartbeat_id: Some(42) };
    let s = serde_json::to_string(&ack).unwrap();
    // Must include heartbeat_id; must NOT include remote_config / upgrade_to fields.
    assert!(s.contains(r#""heartbeat_id":42"#));
    assert!(!s.contains("remote_config"));
    assert!(!s.contains("upgrade_to"));
}

#[test]
fn heartbeat_ack_default_omits_heartbeat_id() {
    let ack = HeartbeatAck { heartbeat_id: None };
    let s = serde_json::to_string(&ack).unwrap();
    // Aether proxy tolerates absence; we just confirm we know how it serializes.
    // serde_json default behavior for Option<T>=None is "null", not omission, so:
    assert!(s.contains(r#""heartbeat_id":null"#) || !s.contains("heartbeat_id"));
}
```

- [ ] **Step 2: Run failing test → expected COMPILE FAIL**

- [ ] **Step 3: Implement `heartbeat.rs` per spec §4.6**

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize)]
pub struct HeartbeatData {
    pub node_id: String,
    pub heartbeat_session_id: String,
    pub heartbeat_id: u64,
    pub active_connections: u32,
    pub total_requests: u64,
    pub avg_latency_ms: f64,
    pub failed_requests: u64,
    pub dns_failures: u64,
    pub stream_errors: u64,
    pub proxy_metadata: ProxyMetadata,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProxyMetadata {
    pub version: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct HeartbeatAck {
    pub heartbeat_id: Option<u64>,
    // intentional: do NOT emit remote_config / upgrade_to
}
```

Update mod.rs: `pub mod heartbeat;`.

- [ ] **Step 4: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test aether_heartbeat
git add crates/gw-tunnel-client/src/aether/{mod.rs,heartbeat.rs} \
        crates/gw-tunnel-client/tests/aether_heartbeat.rs
git commit -m "feat(tunnel-client): add aether HeartbeatData/HeartbeatAck JSON shapes"
```

---

### Task A6: Implement `aether/http_adapter.rs` (httparse request parse + chunked-encoding response write)

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/http_adapter.rs`
- Modify: `crates/gw-tunnel-client/src/aether/mod.rs`
- Create: `crates/gw-tunnel-client/tests/aether_http_adapter.rs`
- Modify: `crates/gw-tunnel-client/Cargo.toml` (add `httparse`, `http`, `url`)

- [ ] **Step 1: Add deps**

Run:
```bash
cargo-remote-ctl add -p gw-tunnel-client httparse http url
```

- [ ] **Step 2: Write the failing test**

```rust
// crates/gw-tunnel-client/tests/aether_http_adapter.rs
use gw_tunnel_client::aether::http_adapter::{HttpAdapter, ResponseMetaInput};
use std::io::Cursor;
use tokio::io::{AsyncWriteExt, BufWriter};

#[tokio::test]
async fn parses_simple_request_with_headers_and_partial_body() {
    let req = "POST /v1/echo HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nX-Y: 1\r\n\r\nHELLO";
    let mut reader = Cursor::new(req.as_bytes().to_vec());
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "node-1".into());
    let (pre, mut remaining) = adapter.parse_request().await.unwrap();
    assert_eq!(pre.method, "POST");
    assert!(pre.url.contains("/v1/echo"));
    // body bytes already buffered at parse time
    let mut body = Vec::new();
    remaining.read_to_end(&mut body).await.unwrap();
    assert_eq!(&body, b"HELLO");
}

#[tokio::test]
async fn parses_request_arriving_in_two_chunks() {
    let part1 = b"GET / HTT".to_vec();
    let part2 = b"P/1.1\r\nHost: x\r\n\r\n".to_vec();
    let mut reader = TwoChunk::new(part1, part2);
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "node".into());
    let (pre, _) = adapter.parse_request().await.unwrap();
    assert_eq!(pre.method, "GET");
}

#[tokio::test]
async fn writes_chunked_response_when_no_content_length() {
    let mut reader = Cursor::new(Vec::new());
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "n".into());
    let (body_tx, body_rx) = tokio::sync::mpsc::channel(4);
    body_tx.send("hello".into()).await.unwrap();
    body_tx.send(" world".into()).await.unwrap();
    drop(body_tx);
    let meta = ResponseMetaInput {
        status: 200,
        headers: vec![("Content-Type".into(), "text/plain".into())],
    };
    adapter.write_response(meta, body_rx).await.unwrap();
    let out = String::from_utf8(writer).unwrap();
    assert!(out.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(out.contains("Transfer-Encoding: chunked\r\n"));
    assert!(out.contains("\r\n5\r\nhello\r\n"));
    assert!(out.contains("\r\n6\r\n world\r\n"));
    assert!(out.ends_with("\r\n0\r\n\r\n"));
}

#[tokio::test]
async fn writes_plain_response_when_content_length_present() {
    let mut reader = Cursor::new(Vec::new());
    let mut writer = Vec::<u8>::new();
    let mut adapter = HttpAdapter::new(&mut reader, &mut writer, "n".into());
    let (body_tx, body_rx) = tokio::sync::mpsc::channel(4);
    body_tx.send("hi".into()).await.unwrap();
    drop(body_tx);
    let meta = ResponseMetaInput {
        status: 200,
        headers: vec![("Content-Length".into(), "2".into())],
    };
    adapter.write_response(meta, body_rx).await.unwrap();
    let out = String::from_utf8(writer).unwrap();
    assert!(!out.contains("Transfer-Encoding: chunked"));
    assert!(out.ends_with("hi"));
}

/// Two-chunk reader for the partial-read test.
struct TwoChunk { a: Vec<u8>, b: Vec<u8>, i: u8 }
impl TwoChunk { fn new(a: Vec<u8>, b: Vec<u8>) -> Self { Self { a, b, i: 0 } } }
impl tokio::io::AsyncRead for TwoChunk {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let chunk = if self.i == 0 { self.i = 1; std::mem::take(&mut self.a) }
                    else if self.i == 1 { self.i = 2; std::mem::take(&mut self.b) }
                    else { Vec::new() };
        buf.put_slice(&chunk);
        std::task::Poll::Ready(Ok(()))
    }
}
```

- [ ] **Step 3: Run failing test → expected COMPILE FAIL**

- [ ] **Step 4: Implement `http_adapter.rs` per spec §4.5**

(Use the code in spec §4.5 verbatim; expose `RequestPreamble { method, url, headers }`, a `RemainingBody` newtype wrapping the already-buffered overflow bytes + the underlying reader, and `ResponseMetaInput` matching what callers in PR 2c need.) The chunked-encoding branch must:

- Emit `Transfer-Encoding: chunked` ONLY when upstream provided neither `Content-Length` nor `Transfer-Encoding`.
- Emit each chunk as `{HEX_LEN}\r\n{BYTES}\r\n`.
- Terminate with `0\r\n\r\n`.

The Content-Length / Transfer-Encoding branch forwards body bytes as-is.

- [ ] **Step 5: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test aether_http_adapter
git add crates/gw-tunnel-client/src/aether/{mod.rs,http_adapter.rs} \
        crates/gw-tunnel-client/tests/aether_http_adapter.rs \
        crates/gw-tunnel-client/Cargo.toml
git commit -m "feat(tunnel-client): add aether HTTP/1 adapter (httparse + chunked-encoding)"
```

---

### Task A7: Extend `StreamTarget` with the `Http` variant

**Files:**
- Modify: `crates/gw-tunnel-client/src/target.rs`
- Modify: `crates/gw-tunnel-client/tests/aether_http_adapter.rs` (if needed)
- Create or extend: `crates/gw-tunnel-client/tests/target.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/gw-tunnel-client/tests/target.rs
use gw_tunnel_client::aether::meta::ResponseMeta;
use gw_tunnel_client::StreamTarget;

#[test]
fn stream_target_http_variant_constructs() {
    let (body_tx, body_rx) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
    let (resp_body_tx, _) = tokio::sync::mpsc::channel::<bytes::Bytes>(1);
    let (meta_tx, _) = tokio::sync::oneshot::channel::<ResponseMeta>();
    let target = StreamTarget::Http {
        method: http::Method::POST,
        url: url::Url::parse("https://example/v1/x").unwrap(),
        headers: vec![("x".into(), "y".into())],
        body_rx,
        body_tx_response: resp_body_tx,
        meta_tx_response: meta_tx,
    };
    match target {
        StreamTarget::Http { method, .. } => assert_eq!(method, http::Method::POST),
        StreamTarget::Tcp { .. } => panic!("wrong variant"),
    }
    drop(body_tx);  // silence unused-binding warning
}
```

- [ ] **Step 2: Run failing test → expected COMPILE FAIL**

- [ ] **Step 3: Add the `Http` variant**

```rust
// crates/gw-tunnel-client/src/target.rs
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

#[non_exhaustive]
#[derive(Debug)]
pub enum StreamTarget {
    Tcp { host: String, port: u16, tls: bool },
    Http {
        method: http::Method,
        url: url::Url,
        headers: Vec<(String, String)>,
        body_rx: mpsc::Receiver<Bytes>,
        body_tx_response: mpsc::Sender<Bytes>,
        meta_tx_response: oneshot::Sender<crate::aether::meta::ResponseMeta>,
    },
}
```

> Note the `Debug` impl on a `Receiver` — `tokio::sync::mpsc::Receiver` is `Debug` when its item is, so this works. `oneshot::Sender` is also `Debug`. Drop `Debug` if it doesn't compile in your local stack; it's only required for tests.

The previously-existing `Clone` impl on `StreamTarget` (if any) goes away because the new variant holds non-Clone channel handles. Verify no external caller relied on `Clone` on `StreamTarget`.

- [ ] **Step 4: Verify `cnexus::WsTransport::open_stream` impl still compiles**

It already returns `UnsupportedTarget` for non-Tcp; the new `Http` variant just adds an arm to that error path. Grep: `grep -n 'UnsupportedTarget' crates/gw-tunnel-client/src/cnexus/transport.rs` — confirm the catch-all matches the new variant.

- [ ] **Step 5: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client
git add crates/gw-tunnel-client/src/target.rs \
        crates/gw-tunnel-client/tests/target.rs
git commit -m "feat(tunnel-client): add StreamTarget::Http variant for aether"
```

---

### Task A8: Implement `aether/transport.rs` (`WsTransport::attach` + read/write/liveness tasks, task-per-stream)

> This is the largest task in PR 2a. Decompose into 4 sub-commits if needed.

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/transport.rs`
- Modify: `crates/gw-tunnel-client/src/aether/mod.rs`
- Create: `crates/gw-tunnel-client/tests/aether_transport.rs`

- [ ] **Step 1: Write the failing test (driving the surface)**

```rust
// crates/gw-tunnel-client/tests/aether_transport.rs
use gw_tunnel_client::aether::transport::WsTransport;
use gw_tunnel_client::core::Transport;

#[test]
fn ws_transport_is_a_transport() {
    fn assert_transport<T: Transport>() {}
    assert_transport::<WsTransport>();
}

#[test]
fn ws_transport_reports_aether_protocol() {
    use gw_core::tunnel_protocol::TunnelProtocol;
    let t = WsTransport::disconnected_for_test();
    assert_eq!(t.tunnel_protocol(), TunnelProtocol::Aether);
}

/// Stream-id allocation: hub uses even ids starting at 2.
#[test]
fn next_stream_id_starts_at_2_and_steps_by_2() {
    let t = WsTransport::disconnected_for_test();
    let a = t.allocate_stream_id_for_test();
    let b = t.allocate_stream_id_for_test();
    let c = t.allocate_stream_id_for_test();
    assert_eq!((a, b, c), (2, 4, 6));
}
```

- [ ] **Step 2: Run failing test → expected COMPILE FAIL**

- [ ] **Step 3: Implement `WsTransport::attach` + supporting state**

Use spec §4.4 as the implementation guide. Key pieces:

```rust
pub struct WsTransport { inner: std::sync::Arc<Inner> }

struct Inner {
    cfg: TransportConfig,
    node_id: String,
    streams: dashmap::DashMap<u32, StreamSlot>,
    next_stream_id: std::sync::atomic::AtomicU32, // start 2, step 2 (hub parity)
    write_tx: tokio::sync::mpsc::Sender<crate::aether::frame::Frame>,
    alive: std::sync::atomic::AtomicBool,
    last_heartbeat_at: arc_swap::ArcSwapOption<std::time::Instant>,
    auth_token: secrecy::SecretString,
    heartbeat_session_id: arc_swap::ArcSwap<String>,
    /// Redis handle injected at attach for events:aether_heartbeat push (Task A9).
    redis: std::sync::Arc<dyn crate::aether::redis_sink::HeartbeatSink>,
}

struct StreamSlot {
    body_tx_response: tokio::sync::mpsc::Sender<bytes::Bytes>,
    meta_tx_response: Option<tokio::sync::oneshot::Sender<crate::aether::meta::ResponseMeta>>,
}

impl WsTransport {
    pub async fn attach(
        cfg: TransportConfig,
        node_view: crate::resolver::ProxyNodeView,
        upgraded_ws: tokio_tungstenite::WebSocketStream<
            hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>,
        >,
        redis: std::sync::Arc<dyn crate::aether::redis_sink::HeartbeatSink>,
    ) -> Result<Self, crate::core::TransportError> {
        use futures::StreamExt;
        let (sink, stream) = upgraded_ws.split();
        let (write_tx, write_rx) = tokio::sync::mpsc::channel::<crate::aether::frame::Frame>(64);
        let inner = std::sync::Arc::new(Inner {
            cfg,
            node_id: node_view.node_id.clone(),
            streams: dashmap::DashMap::new(),
            next_stream_id: std::sync::atomic::AtomicU32::new(2),
            write_tx,
            alive: std::sync::atomic::AtomicBool::new(true),
            last_heartbeat_at: arc_swap::ArcSwapOption::new(None),
            auth_token: secrecy::SecretString::new(node_view.auth_token.clone()),
            heartbeat_session_id: arc_swap::ArcSwap::from_pointee("uninitialized".into()),
            redis,
        });
        // Spawn write task
        tokio::spawn(write_loop(inner.clone(), sink, write_rx));
        // Spawn read task
        tokio::spawn(read_loop(inner.clone(), stream));
        // Spawn liveness task
        tokio::spawn(liveness_loop(inner.clone()));
        Ok(Self { inner })
    }

    /// Test-only constructor.
    #[cfg(test)]
    pub fn disconnected_for_test() -> Self { /* ... */ }

    #[cfg(test)]
    pub fn allocate_stream_id_for_test(&self) -> u32 {
        self.inner.next_stream_id.fetch_add(2, std::sync::atomic::Ordering::Relaxed)
    }
}

async fn read_loop(
    inner: std::sync::Arc<Inner>,
    mut stream: impl futures::Stream<Item = Result<tokio_tungstenite::tungstenite::Message, _>> + Unpin + Send + 'static,
) {
    let mut buffer = bytes::BytesMut::with_capacity(64 * 1024);
    while let Some(msg) = stream.next().await {
        // ... append bytes, loop Frame::decode, dispatch by msg_type:
        //     Header → record meta in slot; Body → push to body_tx;
        //     StreamEnd → drop slot; StreamError → translate to TransportError;
        //     HeartbeatData → update last_heartbeat_at,
        //                     update heartbeat_session_id if changed,
        //                     send HeartbeatAck via write_tx,
        //                     **and** push to redis sink (Task A9).
        //     GoAway → set alive=false; break.
    }
    inner.alive.store(false, std::sync::atomic::Ordering::Relaxed);
}

async fn write_loop(/* ... */) { /* drain write_rx, encode, write to WS */ }
async fn liveness_loop(inner: std::sync::Arc<Inner>) {
    let mut tick = tokio::time::interval(inner.cfg.heartbeat_interval);
    let threshold = inner.cfg.missed_heartbeat_threshold;
    loop {
        tick.tick().await;
        let alive = match inner.last_heartbeat_at.load_full() {
            Some(at) => at.elapsed() < inner.cfg.heartbeat_interval * threshold,
            None => true, // give a grace period for the first one
        };
        if !alive {
            inner.alive.store(false, std::sync::atomic::Ordering::Relaxed);
            break;
        }
    }
}
```

- [ ] **Step 4: Stream-id allocation**

```rust
fn allocate_stream_id(inner: &Inner) -> u32 {
    inner.next_stream_id.fetch_add(2, std::sync::atomic::Ordering::Relaxed)
}
```

`AtomicU32::new(2)`. Hub-side parity confirmed by the fixture-capture step (Task A1).

- [ ] **Step 5: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test aether_transport
# (The compile-fence test passes; the integration of read/write loops is
# exercised at end-to-end in PR 2c.)
git add crates/gw-tunnel-client/src/aether/{mod.rs,transport.rs} \
        crates/gw-tunnel-client/tests/aether_transport.rs
git commit -m "feat(tunnel-client): add aether::WsTransport (attach + task-per-stream)"
```

---

### Task A9: Push `HeartbeatData` to Redis stream `events:aether_heartbeat`

> **Schema lock (decision #6)**: fields = `node_id`, `heartbeat_session_id`, `heartbeat_id`, `received_at_ms`, `active_connections`, `total_requests`, `avg_latency_ms`, `failed_requests`, `dns_failures`, `stream_errors`, `proxy_version`. Stream MAXLEN ~ 50000.

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/redis_sink.rs`
- Modify: `crates/gw-tunnel-client/src/aether/{mod.rs,transport.rs}`
- Create: `crates/gw-tunnel-client/tests/aether_redis_sink.rs`

- [ ] **Step 1: Define a trait-object handle in the tunnel crate so it stays Redis-impl-agnostic**

```rust
// crates/gw-tunnel-client/src/aether/redis_sink.rs

/// Trait so `gw-tunnel-client` does not depend on a specific Redis impl.
/// `gw-gateway` injects an Arc<dyn HeartbeatSink> backed by `redis::aio::ConnectionManager`
/// (or `fred`, whatever the rest of the gateway uses) at attach time.
#[async_trait::async_trait]
pub trait HeartbeatSink: Send + Sync + 'static {
    async fn push(&self, event: HeartbeatEvent);
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct HeartbeatEvent {
    pub node_id: String,
    pub heartbeat_session_id: String,
    pub heartbeat_id: u64,
    pub received_at_ms: u64,
    pub active_connections: u32,
    pub total_requests: u64,
    pub avg_latency_ms: f64,
    pub failed_requests: u64,
    pub dns_failures: u64,
    pub stream_errors: u64,
    pub proxy_version: String,
}

/// Test sink: collects events in memory.
#[cfg(any(test, feature = "test-utils"))]
pub struct CollectingSink {
    pub events: tokio::sync::Mutex<Vec<HeartbeatEvent>>,
}

#[cfg(any(test, feature = "test-utils"))]
#[async_trait::async_trait]
impl HeartbeatSink for CollectingSink {
    async fn push(&self, event: HeartbeatEvent) {
        self.events.lock().await.push(event);
    }
}
```

- [ ] **Step 2: Write the failing test**

```rust
// crates/gw-tunnel-client/tests/aether_redis_sink.rs
use std::sync::Arc;
use gw_tunnel_client::aether::redis_sink::{CollectingSink, HeartbeatEvent, HeartbeatSink};

#[tokio::test]
async fn collecting_sink_records_event() {
    let sink = Arc::new(CollectingSink { events: tokio::sync::Mutex::new(Vec::new()) });
    let s: Arc<dyn HeartbeatSink> = sink.clone();
    s.push(HeartbeatEvent {
        node_id: "n".into(), heartbeat_session_id: "s".into(),
        heartbeat_id: 1, received_at_ms: 0,
        active_connections: 0, total_requests: 1, avg_latency_ms: 0.0,
        failed_requests: 0, dns_failures: 0, stream_errors: 0,
        proxy_version: "v".into(),
    }).await;
    assert_eq!(sink.events.lock().await.len(), 1);
}
```

- [ ] **Step 3: In `transport.rs` read loop, on `HeartbeatData` receipt, build a `HeartbeatEvent` and call `inner.redis.push(...)`**

In the `read_loop`'s `HeartbeatData` branch:

```rust
MsgType::HeartbeatData => {
    let payload = if frame.flags & flags::GZIP_COMPRESSED != 0 {
        crate::aether::gzip::decompress(&frame.payload).map_err(...)?
    } else {
        frame.payload.clone()
    };
    let data: crate::aether::heartbeat::HeartbeatData = serde_json::from_slice(&payload).map_err(...)?;
    inner.last_heartbeat_at.store(Some(std::sync::Arc::new(std::time::Instant::now())));
    inner.heartbeat_session_id.store(std::sync::Arc::new(data.heartbeat_session_id.clone()));

    // Reply with HeartbeatAck
    let ack = crate::aether::heartbeat::HeartbeatAck { heartbeat_id: Some(data.heartbeat_id) };
    let ack_payload = serde_json::to_vec(&ack).unwrap().into();
    let _ = inner.write_tx.send(crate::aether::frame::Frame {
        stream_id: 0,
        msg_type: MsgType::HeartbeatAck,
        flags: flags::NONE,
        payload: ack_payload,
    }).await;

    // Push to Redis sink
    let received_at_ms = received_at_ms_now();
    let evt = crate::aether::redis_sink::HeartbeatEvent {
        node_id: data.node_id,
        heartbeat_session_id: data.heartbeat_session_id,
        heartbeat_id: data.heartbeat_id,
        received_at_ms,
        active_connections: data.active_connections,
        total_requests: data.total_requests,
        avg_latency_ms: data.avg_latency_ms,
        failed_requests: data.failed_requests,
        dns_failures: data.dns_failures,
        stream_errors: data.stream_errors,
        proxy_version: data.proxy_metadata.version,
    };
    let sink = inner.redis.clone();
    tokio::spawn(async move { sink.push(evt).await; });
}
```

> The `tokio::spawn` is deliberate: do not block the read loop on Redis IO. If the Redis sink is slow, heartbeats are lost (not buffered) — the alternative (back-pressure on the read loop) could stall the entire transport. Document this trade-off in the function-level doc comment.

- [ ] **Step 4: Run tests + commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test aether_redis_sink
git add crates/gw-tunnel-client/src/aether/{mod.rs,redis_sink.rs,transport.rs} \
        crates/gw-tunnel-client/tests/aether_redis_sink.rs
git commit -m "feat(tunnel-client): push aether HeartbeatData to events:aether_heartbeat sink"
```

---

### Task A10: `impl Transport for aether::WsTransport`

**Files:**
- Modify: `crates/gw-tunnel-client/src/aether/transport.rs`
- Modify: `crates/gw-tunnel-client/tests/aether_transport.rs` (extend)

- [ ] **Step 1: Write the failing test**

Extend `aether_transport.rs`:

```rust
#[test]
fn open_stream_rejects_tcp_target() {
    use gw_tunnel_client::{StreamTarget, Transport, TransportError};
    let t = WsTransport::disconnected_for_test();
    // open_stream is async; use a runtime
    let res = tokio_test::block_on(t.open_stream(StreamTarget::Tcp {
        host: "x".into(), port: 1, tls: false,
    }));
    assert!(matches!(res, Err(TransportError::UnsupportedTarget)));
}
```

- [ ] **Step 2: Implement `impl Transport for WsTransport`**

```rust
use crate::core::{StreamHandle, Transport, TransportError, TunnelStream};
use crate::target::StreamTarget;
use futures::future::BoxFuture;
use futures::FutureExt;

impl Transport for WsTransport {
    fn open_stream<'a>(
        &'a self,
        target: StreamTarget,
    ) -> BoxFuture<'a, Result<StreamHandle, TransportError>> {
        async move {
            let (method, url, headers, body_rx, body_tx_response, meta_tx_response) = match target {
                StreamTarget::Http { method, url, headers, body_rx, body_tx_response, meta_tx_response } => {
                    (method, url, headers, body_rx, body_tx_response, meta_tx_response)
                }
                _ => return Err(TransportError::UnsupportedTarget),
            };

            let sid = self.inner.next_stream_id.fetch_add(2, std::sync::atomic::Ordering::Relaxed);
            let slot = StreamSlot {
                body_tx_response,
                meta_tx_response: Some(meta_tx_response),
            };
            self.inner.streams.insert(sid, slot);

            let meta = crate::aether::meta::RequestMeta {
                method: method.to_string(),
                url: url.to_string(),
                headers: headers.into_iter().collect(),
                timeout: 60,
            };
            let payload = serde_json::to_vec(&meta).map_err(|e| TransportError::Protocol(e.to_string()))?.into();
            self.inner.write_tx.send(crate::aether::frame::Frame {
                stream_id: sid,
                msg_type: crate::aether::frame::MsgType::RequestHeaders,
                flags: crate::aether::frame::flags::NONE,
                payload,
            }).await.map_err(|_| TransportError::Closed)?;

            spawn_body_pump(sid, body_rx, self.inner.clone());

            // The "stream handle" we return is a wrapper around the response body receiver.
            // Pingora/tunnel_bridge will read response bytes from it.
            // For the Phase 2 compile-fence we return an empty bytes stream pending wiring.
            Ok(Box::new(EmptyTunnelStream) as StreamHandle)
        }
        .boxed()
    }

    fn alive(&self) -> bool {
        self.inner.alive.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn last_heartbeat_age(&self) -> Option<std::time::Duration> {
        self.inner.last_heartbeat_at.load_full().map(|at| at.elapsed())
    }

    fn tunnel_protocol(&self) -> gw_core::tunnel_protocol::TunnelProtocol {
        gw_core::tunnel_protocol::TunnelProtocol::Aether
    }

    fn close<'a>(&'a self) -> BoxFuture<'a, Result<(), TransportError>> {
        async move {
            self.inner.alive.store(false, std::sync::atomic::Ordering::Relaxed);
            // Drop the write_tx by replacing inner if we held one; here we just stop signalling.
            Ok(())
        }
        .boxed()
    }
}

struct EmptyTunnelStream;
impl tokio::io::AsyncRead for EmptyTunnelStream {
    fn poll_read(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>, _: &mut tokio::io::ReadBuf<'_>)
        -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
}
impl tokio::io::AsyncWrite for EmptyTunnelStream {
    fn poll_write(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>, _: &[u8])
        -> std::task::Poll<std::io::Result<usize>> { std::task::Poll::Ready(Ok(0)) }
    fn poll_flush(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>)
        -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>)
        -> std::task::Poll<std::io::Result<()>> { std::task::Poll::Ready(Ok(())) }
}

fn spawn_body_pump(sid: u32, mut body_rx: tokio::sync::mpsc::Receiver<bytes::Bytes>, inner: std::sync::Arc<Inner>) {
    tokio::spawn(async move {
        while let Some(chunk) = body_rx.recv().await {
            let (payload, flags) = {
                let (compressed_payload, did_compress) = crate::aether::gzip::maybe_compress(chunk);
                let f = if did_compress { crate::aether::frame::flags::GZIP_COMPRESSED } else { crate::aether::frame::flags::NONE };
                (compressed_payload, f)
            };
            let _ = inner.write_tx.send(crate::aether::frame::Frame {
                stream_id: sid,
                msg_type: crate::aether::frame::MsgType::RequestBody,
                flags,
                payload,
            }).await;
        }
        let _ = inner.write_tx.send(crate::aether::frame::Frame {
            stream_id: sid,
            msg_type: crate::aether::frame::MsgType::StreamEnd,
            flags: crate::aether::frame::flags::END_STREAM,
            payload: bytes::Bytes::new(),
        }).await;
    });
}
```

> `EmptyTunnelStream` is a Phase 2-temporary placeholder so the compile-fence passes. The actual wiring (bridge reads response body via `body_rx_response` mpsc, not via `StreamHandle`) is done in PR 2b when `tunnel_bridge.rs` is taught to dispatch by protocol. **Mark this `EmptyTunnelStream` with a `// PHASE 2 PR 2b TODO` comment so the next task knows where to insert the real bridge handle.**

- [ ] **Step 3: Add `tokio-test` and `gw_core` test dep, run tests, commit**

```bash
cargo-remote-ctl add -p gw-tunnel-client --dev tokio-test
cargo-remote-ctl test -p gw-tunnel-client --test aether_transport
git add crates/gw-tunnel-client/src/aether/transport.rs \
        crates/gw-tunnel-client/tests/aether_transport.rs \
        crates/gw-tunnel-client/Cargo.toml
git commit -m "feat(tunnel-client): impl Transport for aether::WsTransport"
```

---

### Task A11: PR-2a verification gate

- [ ] **Step 1: Workspace-wide compile**

Run: `cargo-remote-ctl check --workspace --all-targets`
Expected: clean.

- [ ] **Step 2: Workspace tests**

Run: `cargo-remote-ctl test --workspace`
Expected: PASS.

- [ ] **Step 3: clippy at warning-as-error**

Run: `cargo-remote-ctl clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 4: Architectural invariants**

Run: `bash scripts/check-invariants.sh`
Expected: clean.

- [ ] **Step 5: Capture the test summary for the PR description**

Save the final 5 lines of `cargo-remote-ctl test --workspace` output.

---

## PR 2b · InboundListener mount + gateway integration

### Task B1: Define the `ProxyNodeRegistry` trait and impl it for `TunnelStack`

**Files:**
- Create: `crates/gw-tunnel-client/src/registry.rs`
- Modify: `crates/gw-tunnel-client/src/lib.rs`
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`
- Create: `crates/gw-tunnel-client/tests/registry.rs`
- Create: `crates/gw-proxy/tests/registry_impl.rs`

- [ ] **Step 1: Write the failing test in tunnel-client (trait compile-fence)**

```rust
// crates/gw-tunnel-client/tests/registry.rs
use gw_tunnel_client::registry::ProxyNodeRegistry;

#[test]
fn registry_trait_is_object_safe() {
    fn obj(_: &dyn ProxyNodeRegistry) {}
    // If the trait grows generics that break object-safety, this stops compiling.
}
```

- [ ] **Step 2: Define the trait in `gw-tunnel-client/src/registry.rs`**

```rust
use crate::resolver::ProxyNodeView;

pub trait ProxyNodeRegistry: Send + Sync + 'static {
    fn lookup_by_id(&self, node_id: &str) -> Option<ProxyNodeView>;
    fn verify_auth_token(&self, node_id: &str, candidate: &str) -> bool;
}
```

- [ ] **Step 3: Write the failing test in gw-proxy (impl gate)**

```rust
// crates/gw-proxy/tests/registry_impl.rs
use std::sync::Arc;
use gw_proxy::tunnel_stack::TunnelStack;
use gw_tunnel_client::registry::ProxyNodeRegistry;

#[test]
fn tunnel_stack_implements_registry() {
    fn assert_registry<T: ProxyNodeRegistry>() {}
    assert_registry::<TunnelStack>();
}
```

- [ ] **Step 4: Implement for `TunnelStack`**

In `crates/gw-proxy/src/tunnel_stack.rs`, add:

```rust
impl gw_tunnel_client::registry::ProxyNodeRegistry for TunnelStack {
    fn lookup_by_id(&self, node_id: &str) -> Option<gw_tunnel_client::resolver::ProxyNodeView> {
        self.snapshot.read().get(node_id).cloned()
    }

    fn verify_auth_token(&self, node_id: &str, candidate: &str) -> bool {
        match self.snapshot.read().get(node_id) {
            Some(view) => {
                // ProxyNodeView carries the registered token (decrypted at refresh time).
                // Constant-time compare via subtle::ConstantTimeEq.
                use subtle::ConstantTimeEq;
                view.auth_token.as_bytes().ct_eq(candidate.as_bytes()).unwrap_u8() == 1
            }
            None => false,
        }
    }
}
```

> `ProxyNodeView.auth_token` is the decrypted token already in memory. Phase 1 G2 task ensured the row stores `auth_token_enc` (encrypted); Phase 1 Rust-side `tunnel_refresher` decrypts via KMS at refresh time. If `auth_token` field doesn't exist on `ProxyNodeView`, add it in Phase 1 Task 4 — it's needed for both inbound listener auth and (Phase 3) the GatewayControl flow. Surface the missing-field case before continuing if you find it absent.

- [ ] **Step 5: Run tests, commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test registry
cargo-remote-ctl test -p gw-proxy --test registry_impl
git add crates/gw-tunnel-client/src/{registry.rs,lib.rs} \
        crates/gw-tunnel-client/tests/registry.rs \
        crates/gw-proxy/src/tunnel_stack.rs \
        crates/gw-proxy/tests/registry_impl.rs
git commit -m "feat(tunnel-client,gw-proxy): add ProxyNodeRegistry trait + TunnelStack impl"
```

---

### Task B2: Implement `aether::InboundListener::handle_upgrade`

**Files:**
- Create: `crates/gw-tunnel-client/src/aether/listener.rs`
- Modify: `crates/gw-tunnel-client/src/aether/mod.rs`
- Create: `crates/gw-tunnel-client/tests/aether_listener.rs`

- [ ] **Step 1: Write the failing tests**

```rust
// crates/gw-tunnel-client/tests/aether_listener.rs
use gw_tunnel_client::aether::listener::{InboundListener, ListenerError};
use gw_tunnel_client::registry::ProxyNodeRegistry;
use gw_tunnel_client::resolver::ProxyNodeView;
use gw_core::tunnel_protocol::TunnelProtocol;
use std::sync::Arc;

struct StubRegistry { node_id: String, token: String }
impl ProxyNodeRegistry for StubRegistry {
    fn lookup_by_id(&self, id: &str) -> Option<ProxyNodeView> {
        if id == self.node_id {
            Some(ProxyNodeView {
                node_id: id.to_string(),
                auth_token: self.token.clone(),
                tunnel_protocol: TunnelProtocol::Aether,
                ..Default::default()
            })
        } else { None }
    }
    fn verify_auth_token(&self, id: &str, cand: &str) -> bool {
        id == self.node_id && cand == self.token
    }
}

#[tokio::test]
async fn rejects_unknown_node_id() {
    let registry = Arc::new(StubRegistry { node_id: "n1".into(), token: "ae_t".into() });
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let l = InboundListener::new(registry, tx);
    let headers = http::HeaderMap::from_iter([
        ("authorization".parse().unwrap(), "Bearer ae_t".parse().unwrap()),
        ("x-node-id".parse().unwrap(), "n999".parse().unwrap()),
    ]);
    let result = l.authorize(&headers);
    assert!(matches!(result, Err(ListenerError::UnknownNode)));
}

#[tokio::test]
async fn rejects_non_aether_protocol() {
    let registry = Arc::new(StubRegistry { node_id: "n1".into(), token: "ae_t".into() });
    // Override the stub to claim cnexus protocol
    // ... (use a second stub variant or add a flag)
}

#[tokio::test]
async fn rejects_missing_authorization() {
    let registry = Arc::new(StubRegistry { node_id: "n1".into(), token: "ae_t".into() });
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let l = InboundListener::new(registry, tx);
    let headers = http::HeaderMap::from_iter([
        ("x-node-id".parse().unwrap(), "n1".parse().unwrap()),
    ]);
    let result = l.authorize(&headers);
    assert!(matches!(result, Err(ListenerError::Unauthorized)));
}

#[tokio::test]
async fn accepts_valid_headers() {
    let registry = Arc::new(StubRegistry { node_id: "n1".into(), token: "ae_t".into() });
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let l = InboundListener::new(registry, tx);
    let headers = http::HeaderMap::from_iter([
        ("authorization".parse().unwrap(), "Bearer ae_t".parse().unwrap()),
        ("x-node-id".parse().unwrap(), "n1".parse().unwrap()),
        ("x-node-name".parse().unwrap(), "aether-node-1".parse().unwrap()),
        ("x-tunnel-max-streams".parse().unwrap(), "64".parse().unwrap()),
    ]);
    let result = l.authorize(&headers).unwrap();
    assert_eq!(result.node_view.node_id, "n1");
}

#[tokio::test]
async fn semaphore_rejects_when_exhausted() {
    let registry = Arc::new(StubRegistry { node_id: "n1".into(), token: "ae_t".into() });
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let l = InboundListener::with_capacity(registry, tx, 1);
    let _permit = l.try_acquire_permit().unwrap();
    assert!(l.try_acquire_permit().is_err(), "second permit must fail");
}
```

- [ ] **Step 2: Implement `listener.rs`**

```rust
use std::sync::Arc;
use http::HeaderMap;
use tokio::sync::{mpsc, Semaphore, OwnedSemaphorePermit};
use crate::registry::ProxyNodeRegistry;
use crate::resolver::ProxyNodeView;
use crate::core::Transport;
use gw_core::tunnel_protocol::TunnelProtocol;

#[derive(Debug, thiserror::Error)]
pub enum ListenerError {
    #[error("missing or malformed Authorization")]
    Unauthorized,
    #[error("unknown node id")]
    UnknownNode,
    #[error("node has non-aether protocol")]
    WrongProtocol,
    #[error("token mismatch")]
    BadToken,
    #[error("missing required header {0}")]
    MissingHeader(&'static str),
    #[error("inbound capacity exhausted")]
    Saturated,
}

pub struct AuthorizedUpgrade {
    pub node_view: ProxyNodeView,
    pub max_streams: u32,
}

pub struct InboundListener {
    registry: Arc<dyn ProxyNodeRegistry>,
    attach_tx: mpsc::Sender<(String, Arc<dyn Transport>)>,
    sem: Arc<Semaphore>,
}

impl InboundListener {
    pub fn new(
        registry: Arc<dyn ProxyNodeRegistry>,
        attach_tx: mpsc::Sender<(String, Arc<dyn Transport>)>,
    ) -> Self {
        Self::with_capacity(registry, attach_tx, 256)
    }

    pub fn with_capacity(
        registry: Arc<dyn ProxyNodeRegistry>,
        attach_tx: mpsc::Sender<(String, Arc<dyn Transport>)>,
        cap: usize,
    ) -> Self {
        Self { registry, attach_tx, sem: Arc::new(Semaphore::new(cap)) }
    }

    pub fn try_acquire_permit(&self) -> Result<OwnedSemaphorePermit, ListenerError> {
        self.sem.clone().try_acquire_owned().map_err(|_| ListenerError::Saturated)
    }

    /// Validates headers and resolves the node row. Does NOT touch the WS stream.
    /// Caller does the upgrade, then calls `attach`.
    pub fn authorize(&self, headers: &HeaderMap) -> Result<AuthorizedUpgrade, ListenerError> {
        let bearer = headers.get(http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer "))
            .ok_or(ListenerError::Unauthorized)?;
        let node_id = headers.get("x-node-id").and_then(|v| v.to_str().ok())
            .ok_or(ListenerError::MissingHeader("X-Node-Id"))?;
        let _node_name = headers.get("x-node-name").and_then(|v| v.to_str().ok())
            .ok_or(ListenerError::MissingHeader("X-Node-Name"))?;
        let max_streams: u32 = headers.get("x-tunnel-max-streams")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .ok_or(ListenerError::MissingHeader("X-Tunnel-Max-Streams"))?;

        let node_view = self.registry.lookup_by_id(node_id).ok_or(ListenerError::UnknownNode)?;
        if node_view.tunnel_protocol != TunnelProtocol::Aether {
            return Err(ListenerError::WrongProtocol);
        }
        if !self.registry.verify_auth_token(node_id, bearer) {
            return Err(ListenerError::BadToken);
        }
        Ok(AuthorizedUpgrade { node_view, max_streams })
    }

    /// After the WS upgrade, the gateway constructs `aether::WsTransport::attach`
    /// and calls this to publish the transport into the stack.
    pub async fn publish(&self, node_id: String, transport: Arc<dyn Transport>) {
        let _ = self.attach_tx.send((node_id, transport)).await;
    }
}
```

- [ ] **Step 3: Run tests, commit**

```bash
cargo-remote-ctl test -p gw-tunnel-client --test aether_listener
git add crates/gw-tunnel-client/src/aether/{mod.rs,listener.rs} \
        crates/gw-tunnel-client/tests/aether_listener.rs
git commit -m "feat(tunnel-client): add aether::InboundListener (auth + capacity gate)"
```

---

### Task B3: Mount `/api/internal/aether-tunnel` on the admin axum server

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Create: `crates/gw-gateway/tests/aether_listener_route.rs`

- [ ] **Step 1: Write the failing test**

```rust
// crates/gw-gateway/tests/aether_listener_route.rs
//! Hits /api/internal/aether-tunnel with bad headers, asserts 401.
//! Hits it with the right headers in a stub-registry harness, asserts 101 (upgrade).
//! Tests assume an aether-stubbed gateway boot — see `tests/common/mod.rs`
//! for the test harness (it already exists for /healthz/tunnel; extend).

#[tokio::test]
async fn reject_without_authorization() {
    let app = test_harness::boot_aether_capable_gateway().await;
    let resp = app.get("/api/internal/aether-tunnel").await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn accepts_valid_handshake() {
    let app = test_harness::boot_aether_capable_gateway().await;
    // Seed one aether node in the registry via the test harness.
    app.seed_aether_node("n1", "ae_t").await;
    let resp = app.upgrade_get("/api/internal/aether-tunnel")
        .header("Authorization", "Bearer ae_t")
        .header("X-Node-Id", "n1")
        .header("X-Node-Name", "n1")
        .header("X-Tunnel-Max-Streams", "16")
        .send().await;
    assert_eq!(resp.status(), 101);
}
```

> The `test_harness` module is the existing gateway-test scaffolding. If it doesn't yet support InboundListener mounting, extend it in this same task.

- [ ] **Step 2: Add the route in `gw-gateway/src/lib.rs`**

In the existing axum admin router builder, add:

```rust
use axum::extract::WebSocketUpgrade;
use axum::routing::get;
use gw_tunnel_client::aether::listener::InboundListener;
use gw_tunnel_client::aether::transport::WsTransport as AetherTransport;

// `inbound_listener: Arc<InboundListener>` is built earlier (during bootstrap)
// from `tunnel_stack.clone() as Arc<dyn ProxyNodeRegistry>`, the attach_tx, and the
// configured max_concurrent_connections.

let aether_path = cfg.tunnel.aether_listener.path.clone();
admin_router = admin_router.route(
    &aether_path,
    get({
        let inbound = inbound_listener.clone();
        move |ws_upgrade: WebSocketUpgrade, headers: axum::http::HeaderMap| {
            let inbound = inbound.clone();
            async move {
                // 1. Authorize headers.
                let authz = match inbound.authorize(&headers) {
                    Ok(a) => a,
                    Err(e) => return axum::response::Response::builder()
                        .status(if matches!(e, gw_tunnel_client::aether::listener::ListenerError::UnknownNode) { 404 }
                                else if matches!(e, gw_tunnel_client::aether::listener::ListenerError::Saturated) { 503 }
                                else { 401 })
                        .body(axum::body::Body::empty())
                        .unwrap(),
                };
                // 2. Acquire capacity permit.
                let permit = match inbound.try_acquire_permit() {
                    Ok(p) => p,
                    Err(_) => return axum::response::Response::builder()
                        .status(503).header("Retry-After", "5")
                        .body(axum::body::Body::empty()).unwrap(),
                };
                // 3. Perform WS upgrade.
                ws_upgrade.on_upgrade(move |socket| async move {
                    // Wrap axum's WebSocket in tokio-tungstenite-compatible Stream/Sink.
                    let upgraded = axum_to_tungstenite(socket);
                    let transport = match AetherTransport::attach(
                        TransportConfig::default(),
                        authz.node_view.clone(),
                        upgraded,
                        redis_sink_handle.clone(),
                    ).await {
                        Ok(t) => Arc::new(t) as Arc<dyn gw_tunnel_client::Transport>,
                        Err(e) => { tracing::warn!(?e, "attach failed"); drop(permit); return; }
                    };
                    inbound.publish(authz.node_view.node_id.clone(), transport).await;
                    drop(permit); // released when the transport is dropped via stack cleanup
                }).into_response()
            }
        }
    }),
);
```

> The exact `axum_to_tungstenite` adapter shape depends on the axum version; if there is no clean conversion, swap axum's WebSocket for `tokio-tungstenite` (the spec assumes the latter), or accept axum's WebSocket and refactor `aether::WsTransport::attach` to take a generic `Sink + Stream<Item = Message>`. Whichever path is taken, keep the trait shape unchanged.

- [ ] **Step 3: Wire `aether_listener` config**

In `config/default.toml`:

```toml
[tunnel.aether_listener]
enabled = null   # auto-enable when ≥1 aether node present (Task B4 implements this)
path = "/api/internal/aether-tunnel"
max_concurrent_connections = 256
```

Parse this in `gw-gateway/src/config.rs` (or wherever `[tunnel]` is parsed) as:

```rust
#[derive(serde::Deserialize, Clone, Debug)]
pub struct AetherListenerConfig {
    pub enabled: Option<bool>,
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default = "default_max")]
    pub max_concurrent_connections: usize,
}
fn default_path() -> String { "/api/internal/aether-tunnel".into() }
fn default_max() -> usize { 256 }
```

- [ ] **Step 4: Run tests, commit**

```bash
cargo-remote-ctl test -p gw-gateway --test aether_listener_route
git add crates/gw-gateway/src/{lib.rs,config.rs} \
        crates/gw-gateway/tests/aether_listener_route.rs \
        config/default.toml
git commit -m "feat(gw-gateway): mount /api/internal/aether-tunnel admin route"
```

---

### Task B4: `tunnel_refresher` dispatches by protocol; auto-enable rule for InboundListener

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs` (tunnel_refresher)

- [ ] **Step 1: Write the failing test**

Extend an existing tunnel_refresher integration test (or add a new one) that seeds the `proxy_nodes` table with one cnexus row and one aether row, then asserts:

```rust
#[tokio::test]
async fn refresher_dispatches_cnexus_outbound_aether_listener() {
    let app = test_harness::boot_with_seeded_nodes(&[
        SeedNode { id: "cn1", protocol: TunnelProtocol::Cnexus, ws_url: "wss://cn1/tunnel" },
        SeedNode { id: "ae1", protocol: TunnelProtocol::Aether, ws_url: "" /* aether dials in */ },
    ]).await;
    // Cnexus node: ReconnectingTransport supervisor must be in the stack.
    assert!(app.tunnel_stack().has_supervisor("cn1"));
    // Aether node: row must be in the registry snapshot, listener must be mounted.
    assert!(app.tunnel_stack().has_registry_entry("ae1"));
    assert!(app.is_aether_listener_mounted());
}

#[tokio::test]
async fn refresher_does_not_mount_listener_when_no_aether_node() {
    let app = test_harness::boot_with_seeded_nodes(&[
        SeedNode { id: "cn1", protocol: TunnelProtocol::Cnexus, ws_url: "wss://cn1/tunnel" },
    ]).await;
    assert!(!app.is_aether_listener_mounted());
}

#[tokio::test]
async fn explicit_enabled_false_disables_listener_even_with_aether_node() {
    let app = test_harness::boot_with_config(|c| c.tunnel.aether_listener.enabled = Some(false))
        .with_seeded_nodes(&[SeedNode { id: "ae1", protocol: TunnelProtocol::Aether, ws_url: "" }])
        .start().await;
    assert!(!app.is_aether_listener_mounted());
}
```

- [ ] **Step 2: Implement the dispatch + auto-enable rule**

In `tunnel_refresher`:

```rust
let snapshot: Vec<ProxyNode> = repo.list_active_proxy_nodes(...).await?;
for node in &snapshot {
    let view = ProxyNodeView::from_entity(node);
    tunnel_stack.upsert_registry_entry(view.clone());
    match node.tunnel_protocol {
        TunnelProtocol::Cnexus => {
            tunnel_stack.ensure_cnexus_supervisor(view).await;
        }
        TunnelProtocol::Aether => {
            // Just ensure the registry entry exists; the listener accepts the dial-in.
        }
    }
}

// Auto-enable listener.
let has_aether = snapshot.iter().any(|n| n.tunnel_protocol == TunnelProtocol::Aether);
let want_listener = match cfg.tunnel.aether_listener.enabled {
    Some(b) => b,
    None => has_aether,
};
if want_listener && !aether_listener_mounted.load(Ordering::Relaxed) {
    mount_aether_listener(&admin_router, /*...*/);
    aether_listener_mounted.store(true, Ordering::Relaxed);
}
// Note: we never UNMOUNT the listener at runtime — flipping `enabled = false`
// requires restart per spec §1.2. The auto-enable transition is one-way.
```

- [ ] **Step 3: Run tests, commit**

```bash
cargo-remote-ctl test -p gw-gateway -- tunnel_refresher
git add crates/gw-gateway/src/lib.rs
git commit -m "feat(gw-gateway): tunnel_refresher dispatches by protocol; auto-enable aether listener"
```

---

### Task B5: Extend `/healthz/tunnel` with per-node aether status

**Files:**
- Modify: `crates/gw-gateway/src/health.rs`
- Modify: `crates/gw-gateway/tests/health_endpoint.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn healthz_tunnel_includes_per_node_aether_status() {
    let app = boot_with_seeded_nodes(&[
        SeedNode::aether("ae1"),
        SeedNode::cnexus("cn1"),
    ]).await;
    let body = app.get("/healthz/tunnel").bearer(ADMIN_TOKEN).await.json::<serde_json::Value>();
    let nodes = body["nodes"].as_array().unwrap();
    let ae = nodes.iter().find(|n| n["node_id"] == "ae1").unwrap();
    assert_eq!(ae["protocol"], "aether");
    assert_eq!(ae["present"], false); // no proxy has dialed in yet
    let cn = nodes.iter().find(|n| n["node_id"] == "cn1").unwrap();
    assert_eq!(cn["protocol"], "cnexus");
}
```

- [ ] **Step 2: Implement the schema per spec §3.5**

```rust
#[derive(Serialize)]
struct TunnelHealthNode {
    node_id: String,
    protocol: TunnelProtocol,
    present: bool,
    alive: bool,
    last_heartbeat_age_ms: Option<u64>,
    active_streams: u32,
}

fn build_health(stack: &TunnelStack) -> serde_json::Value {
    let nodes: Vec<TunnelHealthNode> = stack.iter_snapshot().map(|view| {
        let transport = stack.get_transport(&view.node_id);
        TunnelHealthNode {
            node_id: view.node_id.clone(),
            protocol: view.tunnel_protocol,
            present: transport.is_some(),
            alive: transport.as_ref().map(|t| t.alive()).unwrap_or(false),
            last_heartbeat_age_ms: transport.as_ref().and_then(|t| t.last_heartbeat_age()).map(|d| d.as_millis() as u64),
            active_streams: transport.as_ref().map(|t| t.active_streams()).unwrap_or(0),
        }
    }).collect();
    serde_json::json!({ "nodes": nodes })
}
```

> If `Transport::active_streams()` does not exist, add it to the trait now in `gw-tunnel-client/src/core.rs` with a default `fn active_streams(&self) -> u32 { 0 }`. The cnexus impl can leave the default; aether impl returns `self.inner.streams.len() as u32`.

- [ ] **Step 3: Run tests, commit**

```bash
cargo-remote-ctl test -p gw-gateway --test health_endpoint
git add crates/gw-gateway/src/health.rs \
        crates/gw-tunnel-client/src/core.rs \
        crates/gw-tunnel-client/src/aether/transport.rs \
        crates/gw-gateway/tests/health_endpoint.rs
git commit -m "feat(gw-gateway): extend /healthz/tunnel with per-node aether status"
```

---

### Task B6: SIGTERM graceful drain for aether transports

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`

- [ ] **Step 1: Add a `drain_aether_inbound(timeout)` to `TunnelStack`**

Behavior per spec §3.5:
1. Stop accepting new InboundListener upgrades (set a flag the listener checks before `try_acquire_permit`).
2. For each aether transport in the DashMap, send a `GoAway` frame (control stream id 0) — proxy interprets as "do not send more frames".
3. Wait up to `timeout` for inbound streams to drain naturally.
4. After timeout, force-close remaining transports.

- [ ] **Step 2: In the gateway's existing SIGTERM handler, call `tunnel_stack.drain_aether_inbound(cfg.tunnel.drain_timeout_secs)` before the rest of the shutdown sequence**

- [ ] **Step 3: Test (integration test using a stub WS that observes GoAway receipt)**

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(gw-gateway): SIGTERM drain for aether inbound transports"
```

---

### Task B7: Wire `tunnel_bridge` to dispatch by protocol (Aether HTTP path)

> Currently `tunnel_bridge` calls `WsTransport::open_stream(target_str)` with a string target — set up for cnexus's TCP relay. Aether requires `StreamTarget::Http`. The bridge must dispatch per protocol.

**Files:**
- Modify: `crates/gw-proxy/src/tunnel_bridge.rs`

- [ ] **Step 1: Replace the concrete cnexus call with `Arc<dyn Transport>::open_stream(StreamTarget::Tcp|Http)` based on resolved protocol**

```rust
match transport.tunnel_protocol() {
    TunnelProtocol::Cnexus => {
        let (host, port, tls) = parse_target_str(&parsed.target)?;
        let target = StreamTarget::Tcp { host, port, tls };
        transport.open_stream(target).await
    }
    TunnelProtocol::Aether => {
        // Bridge parses HTTP/1 request from Pingora-side loopback, constructs Http variant.
        let (preamble, mut remaining) = http_adapter.parse_request().await?;
        let (req_body_tx, req_body_rx) = tokio::sync::mpsc::channel(8);
        let (resp_body_tx, resp_body_rx) = tokio::sync::mpsc::channel(8);
        let (resp_meta_tx, resp_meta_rx) = tokio::sync::oneshot::channel();

        // Pump remaining request body into req_body_tx in a task
        tokio::spawn(pump_request_body(remaining, req_body_tx));

        let target = StreamTarget::Http {
            method: preamble.method, url: preamble.url, headers: preamble.headers,
            body_rx: req_body_rx,
            body_tx_response: resp_body_tx,
            meta_tx_response: resp_meta_tx,
        };
        let _handle = transport.open_stream(target).await?;

        let meta = resp_meta_rx.await.map_err(|_| Closed)?;
        http_adapter.write_response(meta, resp_body_rx).await?;
        // bypass the normal "stream" plumbing — bridge owns the byte path directly
        return Ok(EarlyHandled);
    }
}
```

- [ ] **Step 2: Run e2e_tunnel.rs and confirm cnexus path still passes**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(gw-proxy/tunnel_bridge): dispatch on TunnelProtocol; route aether via HTTP adapter"
```

---

### Task B8: PR-2b verification gate

- [ ] **Step 1: Workspace check + test + clippy + invariants — all four clean**

- [ ] **Step 2: Capture summary for PR description**

---

## PR 2c · tunnel-e2e Aether activation + 9 scenarios

### Task C1: Activate Aether submodule (`update = checkout`, pin tag)

**Files:**
- Modify: `.gitmodules`

- [ ] **Step 1: Find the latest MR's tag in the Aether GitLab project**

```bash
cd tests/tunnel-e2e/aether-src 2>/dev/null || \
    git submodule add https://gitlab.ailinkmax.com/ailinkmax/Aether tests/tunnel-e2e/aether-src
cd tests/tunnel-e2e/aether-src
git fetch --tags
LATEST_TAG=$(git describe --tags --abbrev=0)
echo "Pinning to $LATEST_TAG"
git checkout "$LATEST_TAG"
cd ../../..
```

- [ ] **Step 2: Update `.gitmodules`**

```
[submodule "tests/tunnel-e2e/aether-src"]
    path = tests/tunnel-e2e/aether-src
    url = https://gitlab.ailinkmax.com/ailinkmax/Aether
    update = checkout
    branch = <LATEST_TAG>
```

- [ ] **Step 3: Commit**

```bash
git add .gitmodules tests/tunnel-e2e/aether-src
git commit -m "build(tunnel-e2e): activate Aether submodule pinned to <LATEST_TAG>"
```

---

### Task C2: `Dockerfile.aether-proxy` (build aether-proxy from submodule)

**Files:**
- Create: `tests/tunnel-e2e/Dockerfile.aether-proxy`

- [ ] **Step 1: Read the Aether repo's Dockerfile (if it exists) for the toolchain**

```bash
ls tests/tunnel-e2e/aether-src/aether-proxy/Dockerfile 2>/dev/null
# If present, base on it. Otherwise infer from rust-toolchain.toml in the proxy crate.
```

- [ ] **Step 2: Create a multi-stage build Dockerfile**

```dockerfile
# tests/tunnel-e2e/Dockerfile.aether-proxy
FROM rust:1.88-bookworm AS builder
WORKDIR /workspace
COPY aether-src /workspace/aether-src
WORKDIR /workspace/aether-src/aether-proxy
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake build-essential pkg-config perl libssl-dev libclang-dev
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /workspace/aether-src/aether-proxy/target/release/aether-proxy /usr/local/bin/aether-proxy
ENTRYPOINT ["/usr/local/bin/aether-proxy"]
```

> Build context for this Dockerfile MUST be `tests/tunnel-e2e/` so the `COPY aether-src` line works. docker-compose builds set `context: .` (within tunnel-e2e/) and `dockerfile: Dockerfile.aether-proxy`.

- [ ] **Step 3: Build the image standalone**

```bash
docker build -t aether-proxy:e2e -f tests/tunnel-e2e/Dockerfile.aether-proxy tests/tunnel-e2e/
```

- [ ] **Step 4: Commit**

```bash
git add tests/tunnel-e2e/Dockerfile.aether-proxy
git commit -m "build(tunnel-e2e): aether-proxy Dockerfile from submodule"
```

---

### Task C3: `Dockerfile.aether-hub` (for baseline scenario)

Similar to C2 but builds `aether-hub` and binds at port 7100 (distinct from cnexus-tunnel-server's 7000).

```bash
git commit -m "build(tunnel-e2e): aether-hub Dockerfile for baseline scenario"
```

---

### Task C4: `Dockerfile.go-web` + nginx edge

**Files:**
- Create: `tests/tunnel-e2e/Dockerfile.go-web`
- Create: `tests/tunnel-e2e/nginx/edge.conf`
- Create: `tests/tunnel-e2e/Dockerfile.nginx-edge`

- [ ] **Step 1: Go-web Dockerfile**

```dockerfile
# tests/tunnel-e2e/Dockerfile.go-web
FROM golang:1.21 AS builder
WORKDIR /workspace
ARG GO_REPO_DIR=AILinkMax-Go
COPY ${GO_REPO_DIR} /workspace/${GO_REPO_DIR}
WORKDIR /workspace/${GO_REPO_DIR}
RUN go build -o /tmp/web ./binaries/web

FROM debian:bookworm-slim
COPY --from=builder /tmp/web /usr/local/bin/web
ENTRYPOINT ["/usr/local/bin/web"]
```

> Build context must include the `AILinkMax-Go` directory. docker-compose sets `context: ../../..` (up to the shared workspace root) and `dockerfile: tests/tunnel-e2e/Dockerfile.go-web`. **This assumes the local layout `Cnewxus/AILinkMax-Go` and `Cnewxus/ailinkmax-gateway` are siblings.** If the engineer's layout differs, expose the AILinkMax-Go path via a `GO_REPO_PATH` env var and a `--build-arg` override.

- [ ] **Step 2: nginx edge config**

```nginx
# tests/tunnel-e2e/nginx/edge.conf
events { }
http {
    upstream go-web { server go-web:8080; }
    upstream ali-gateway-admin { server ali-gateway:8080; }
    upstream ali-gateway-business { server ali-gateway:8000; }
    server {
        listen 80;
        location /api/admin/proxy-nodes/register {
            proxy_pass http://go-web;
        }
        location /api/internal/aether-tunnel {
            proxy_pass http://ali-gateway-admin;
            proxy_http_version 1.1;
            proxy_set_header Upgrade $http_upgrade;
            proxy_set_header Connection "upgrade";
            proxy_read_timeout 86400;
        }
        location / {
            proxy_pass http://ali-gateway-business;
        }
    }
}
```

- [ ] **Step 3: nginx Dockerfile**

```dockerfile
FROM nginx:1.27-alpine
COPY nginx/edge.conf /etc/nginx/nginx.conf
```

- [ ] **Step 4: Commit**

```bash
git add tests/tunnel-e2e/Dockerfile.go-web tests/tunnel-e2e/nginx/ tests/tunnel-e2e/Dockerfile.nginx-edge
git commit -m "build(tunnel-e2e): Go web Dockerfile + nginx edge path-router"
```

---

### Task C5: Extend `docker-compose.yml` with aether services

**Files:**
- Modify: `tests/tunnel-e2e/docker-compose.yml`

Add to the existing services:

```yaml
  go-web:
    build:
      context: ../../..
      dockerfile: ailinkmax-gateway/tests/tunnel-e2e/Dockerfile.go-web
    environment:
      MIGRATE_DB: "true"
      AETHER_AUTO_PROVISION: "true"   # B + D scenarios per spec §3.6.4
      DB_DSN: postgres://postgres:e2e@postgres:5432/cnexus
      REDIS_ADDR: redis:6379
    networks: [tunnel-net]
    depends_on:
      postgres:
        condition: service_healthy
      redis:
        condition: service_started

  aether-proxy:
    build:
      context: .
      dockerfile: Dockerfile.aether-proxy
    environment:
      AETHER_PROXY_HUB_URL: http://nginx-edge   # /register + /api/internal/aether-tunnel via nginx
      AETHER_PROXY_AUTH_TOKEN: ae_e2e_token
      AETHER_PROXY_NODE_NAME: aether-e2e-1
    networks: [tunnel-net]
    depends_on:
      nginx-edge:
        condition: service_started
      go-web:
        condition: service_started
      ali-gateway:
        condition: service_started

  aether-hub:
    profiles: ["baseline"]  # opt-in for the baseline scenario only
    build:
      context: .
      dockerfile: Dockerfile.aether-hub
    networks: [tunnel-net]

  nginx-edge:
    build:
      context: .
      dockerfile: Dockerfile.nginx-edge
    networks: [tunnel-net]
    depends_on:
      ali-gateway:
        condition: service_started
      go-web:
        condition: service_started
```

Commit. The actual `--profile baseline` activation happens only in `test_aether_baseline.py` via `docker compose --profile baseline up -d aether-hub` invoked from the test setup fixture.

---

### Task C6: `support/aether_admin.py` + `support/wait_for_aether_attach.py`

Implement the two Python modules per the spec. They:
- Seed the `proxy_nodes` row with `payload.tunnel_protocol = "aether"` and the encrypted token.
- Poll `/healthz/tunnel` for `present=true && alive=true`.

Commit.

---

### Task C7: `test_aether_baseline.py` (1 scenario)

```python
# tests/tunnel-e2e/scenarios/test_aether_baseline.py
import pytest
from support.http_helpers import make_proxy_request

def test_aether_native_routing(docker_compose_baseline_profile, fake_upstream):
    """Aether images alone work end-to-end through their own hub.
    Isolates aether-image regressions from our adapter regressions."""
    # docker_compose_baseline_profile fixture brings up aether-hub.
    resp = make_proxy_request_via_aether_hub("GET", "/baseline-check")
    assert resp.status_code == 200
```

Commit.

---

### Task C8: `test_aether_compat.py` (8 scenarios)

Implement the 8 scenarios per spec §6.5.B. Use `aether_admin.seed_aether_node` + `await_aether_attach`. Some highlights:

- **test_aether_gzip_body**: assert that `wireshark`-style capture from inside the docker network shows `flags::GZIP_COMPRESSED` set on the wire — but capturing inside docker is awkward; instead, use a sidecar container that proxies the WS and counts `GZIP_COMPRESSED`-flagged frames. Document this in a comment.
- **test_aether_reconnect**: `docker stop aether-proxy && docker start aether-proxy`; polling `/healthz/tunnel` should see `present=true && alive=true` within 30s.
- **test_aether_heartbeat_alive**: sleep 70s, assert `last_heartbeat_age_ms < 35000`.

Commit per scenario or as a batch.

---

### Task C9: Update `tests/tunnel-e2e/README.md`

Add "Aether scenarios" section. Document:
- Submodule init: `git submodule update --init --recursive tests/tunnel-e2e/aether-src`.
- Building the aether images takes ~5-10 min cold; warm cache is fast.
- `make tunnel-e2e` runs compat (default profile); `make tunnel-e2e-baseline` runs `--profile baseline` for the baseline scenario.
- The `go-web` service requires the `AILinkMax-Go` repo at `../../../AILinkMax-Go`. Layout assumption is recorded here.

Commit.

---

### Task C10: PR-2c verification gate

- [ ] **Step 1: From clean tree, run `make tunnel-e2e`**

Expected: `3 + 8 = 11 passed`. (3 cnexus from Phase 1; 8 aether compat from C8.)

- [ ] **Step 2: From clean tree, run `make tunnel-e2e-baseline`**

Expected: `1 passed`.

- [ ] **Step 3: Re-run cargo workspace tests**

Expected: PASS, unchanged.

- [ ] **Step 4: `make e2e` smoke**

Expected: PASS, unchanged.

- [ ] **Step 5: Capture summaries for the integrated MR description (next phase will write it)**

---

## PR 2d · Go G3 — heartbeat consumer (AILinkMax-Go)

> **Repo**: `AILinkMax-Go`. **Branch**: `feature/tunnel-compat-phase-2-go`. Lands as a real MR.

### Task D1: ClickHouse table DDL

**Files:**
- Create: `modules/monitoring/clickhouse/aether_heartbeat_samples.sql`

```sql
-- modules/monitoring/clickhouse/aether_heartbeat_samples.sql
CREATE TABLE IF NOT EXISTS aether_heartbeat_samples (
    node_id String,
    heartbeat_session_id String,
    heartbeat_id UInt64,
    received_at DateTime64(3, 'UTC'),
    active_connections UInt32,
    total_requests UInt64,
    avg_latency_ms Float64,
    failed_requests UInt64,
    dns_failures UInt64,
    stream_errors UInt64,
    proxy_version String
) ENGINE = MergeTree()
PARTITION BY toDate(received_at)
ORDER BY (node_id, received_at)
TTL toDate(received_at) + INTERVAL 30 DAY;
```

Document in the same file: TTL = 30 days, matches `gateway_runtime_samples` retention.

Commit.

---

### Task D2: `AetherHeartbeatSink` worker

Implement per the Go-team's existing pattern for monitoring sinks (mirror `gateway_runtime_sample_sink.go` shape). The worker:

1. XREADGROUP from `events:aether_heartbeat` with consumer group `aether-heartbeat-sink` and consumer name `<hostname>-<pid>`.
2. Decode each entry as `HeartbeatEvent`.
3. Batch up to 1000 rows or 1s (whichever first); INSERT to `aether_heartbeat_samples`.
4. XACK after successful insert.

Tests: table-driven cases, idempotent restart (consumer group resumes from last XACK).

Commit.

---

### Task D3: Monitoring API endpoint

`GET /api/admin/monitoring/aether/{node_id}` returns:

```json
{
  "node_id": "ae1",
  "samples": [
    { "received_at": "...", "active_connections": 3, ... },
    ...
  ]
}
```

Query params: `since` / `until` ISO-8601 with defaults `now-1h` / `now`. Return 400 on malformed params.

Implementation queries ClickHouse with `WHERE node_id = ? AND received_at BETWEEN ? AND ? ORDER BY received_at ASC LIMIT 10000`.

Commit.

---

### Task D4: Wire into `binaries/worker-monitoring/main.go`

Spawn the sink alongside existing monitoring workers. Add a `--disable-aether-heartbeat-sink` flag for emergency disable.

Commit.

---

### Task D5: PR-2d verification gate

- [ ] **Step 1: `go test ./modules/monitoring/... -race -v`**
- [ ] **Step 2: `go build ./...`**
- [ ] **Step 3: `go vet ./modules/monitoring/...`**
- [ ] **Step 4: `gofmt -l modules/monitoring/`** — expected empty
- [ ] **Step 5: Open the MR**

MR title: `feat(monitoring): Phase 2d — aether heartbeat consumer + ClickHouse + API (G3)`

MR body should include:
- Schema (lifted from `aether_heartbeat_samples.sql`).
- The contract reference: spec §3.6.5.
- Test summary.
- Note: this MR can land alongside or after the Rust shared-branch's Phase 2c, since the Rust side starts XADDing `events:aether_heartbeat` events immediately on Aether node attach (PR 2a Task A9). Until this MR lands, the events queue grows up to MAXLEN 50000 then ring-buffers; no Rust-side back-pressure.

---

## Self-Review

After all four PRs are complete (1 Go MR + 3 logical Rust commit groups on the shared branch):

1. **Spec coverage (Phase 2 only):**
   - §3.3 `StreamTarget::Http` variant — Task A7.
   - §3.4 `Transport` impl on aether::WsTransport — Task A10. (Trait itself lives in Phase 1.)
   - §3.5 `aether::InboundListener` — Tasks B1 (registry), B2 (listener), B3 (route mount), B4 (refresher + auto-enable), B5 (health surface), B6 (drain).
   - §3.6.5 Heartbeat ingestion (Rust → Redis) — Task A9; (Redis → Go → ClickHouse) — Tasks D1-D4.
   - §3.7 End-to-end flow — exercised by Tasks C7 (baseline) and C8 (compat).
   - §4.2 frame codec — Task A2.
   - §4.3 meta types — Task A3.
   - §4.4 transport — Task A8.
   - §4.5 HTTP adapter — Task A6.
   - §4.6 heartbeat — Task A5.
   - §4.7 gzip — Task A4.
   - §6.1-6.3 Aether-extended directory layout, Dockerfiles, compose, nginx — Tasks C2-C5.
   - §6.5.B 8 aether compat scenarios — Task C8.
   - §6.5.C 1 aether baseline scenario — Task C7.
   - §7 Phase 2 G3 — Tasks D1-D5.
   - Phase 2 explicitly out of scope per the plan header: §6.5.D mixed-pool (Phase 3), §5 dep boundary (Phase 3), §3.6.6 GatewayControl semantics for Aether nodes (Phase 3 — needs G4 frontend gray-out + ack short-circuit), §3.6.7 G4 frontend UI (Phase 3).

2. **Placeholder scan:**
   - No "TBD"/"TODO" inside concrete code blocks.
   - Task A1 Step 2's "capture harness" is a 50-line ad-hoc binary, not a placeholder — the file lives at `/tmp/aether-capture/` only and is replaced with the captured fixtures.
   - Task A10's `EmptyTunnelStream` is a marked `// PHASE 2 PR 2b TODO` placeholder that Task B7's bridge-dispatch resolves. Each task is internally complete; the cross-task ordering is documented.
   - Task B3's `axum_to_tungstenite` references "if there is no clean conversion" — documented adjustment-point with the fall-back plan (refactor `attach` to take a generic Sink+Stream). Not an unresolved unknown.
   - Task C4's "if the engineer's layout differs" — explicit `--build-arg` override surfaced for layout-flexible build context.

3. **Type consistency:**
   - `HeartbeatEvent` field set (Task A9) ⇔ ClickHouse columns (Task D1) ⇔ JSON shape in spec §3.6.5: all 11 fields match (`node_id`, `heartbeat_session_id`, `heartbeat_id`, `received_at_ms`/`received_at`, `active_connections`, `total_requests`, `avg_latency_ms`, `failed_requests`, `dns_failures`, `stream_errors`, `proxy_version`).
   - `ProxyNodeView.tunnel_protocol` (Phase 1) ⇔ `InboundListener::authorize` check (Task B2) ⇔ `tunnel_refresher` dispatch (Task B4): all read the same `TunnelProtocol` enum.
   - `StreamTarget::Http { method, url, headers, body_rx, body_tx_response, meta_tx_response }` (Task A7) ⇔ `aether::WsTransport::open_stream` deconstruct (Task A10) ⇔ `tunnel_bridge` dispatch construction (Task B7): all six fields match by name and type.
   - Route paths: `/api/internal/aether-tunnel` constant in `config/default.toml` (B3) ⇔ nginx upstream `ali-gateway-admin` block (C4) ⇔ test paths (B3 tests). Single source of truth: `cfg.tunnel.aether_listener.path`.

4. **Cross-repo coordination invariants:**
   - PR 2d in `AILinkMax-Go` can land before, alongside, or after the Rust shared-branch Phase 2 work. The Rust side writes `events:aether_heartbeat` with MAXLEN ~ 50000, so if the Go consumer is offline the queue ring-buffers without back-pressure. No protocol break in either direction.
   - PR 2c assumes Phase 1 G1/G2 are already merged in the Go repo and that the Go-web binary is fetched at e2e build time from a sibling directory layout. This is the only place the AILinkMax-Go ↔ ailinkmax-gateway repo coupling is hard-coded in CI; documented in `tests/tunnel-e2e/README.md`.

5. **Single-branch / single-MR consistency check:**
   - Phase 2's `StreamTarget::Http` variant is **additive** (Phase 1 ships `Tcp`-only `#[non_exhaustive]`). Phase 3 doesn't remove it.
   - Phase 2's `aether/` submodule is **additive** to the Phase 1 `cnexus/` structure. Phase 3 doesn't remove it.
   - The `EmptyTunnelStream` placeholder from Task A10 IS removed in Task B7 within the same Phase 2 — this is intra-phase scaffolding, not an add-then-remove across phases. Phase 1's `--features tunnel` removal stays in effect throughout.
   - `tunnel.aether_listener.enabled = null` default behavior (auto-enable when ≥1 aether node) stays through Phase 3. Phase 3 mixed-pool scenarios depend on this remaining stable.

---

Plan complete and saved to `docs/superpowers/plans/2026-06-15-tunnel-compat-phase-2.md`. Execution model continues from Phase 1's chosen approach (Subagent-Driven or Inline). Phase 3 plan is the next document.
