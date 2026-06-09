# W8 Owned Tunnel Server/Client, Bridge, E2E, And Artifact Policy Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Own the tunnel runtime end to end: define this project's tunnel protocol, implement the gateway-side client transport and a first-party tunnel server, bridge Pingora HTTP traffic through WebSocket streams, prove a live round trip against the owned server, and keep the default artifact non-tunnel until an explicit post-e2e release decision.

**Architecture:** The current aether/mock tunnel behavior is reference material only. W8 does not target full aether-hub compatibility and must not block on reverse-engineering the real aether protocol. The project will define its own tunnel protocol, implement both ends in this repository, and use that owned server/client contract for production behavior and e2e tests. Pingora must not speak HTTP directly to a remote WebSocket endpoint; tunnel-selected traffic routes to an in-process HTTP/1 loopback bridge, and that bridge opens an owned tunnel stream to a selected node.

**Artifact policy:** Default `latest` remains non-tunnel during W8. `latest-tunnel` remains the operator-selected tunnel-enabled artifact. Promoting tunnel into default `latest` requires a separate post-e2e release decision from the user.

**Tech Stack:** Rust 1.88 workspace, Pingora 0.8 `ProxyHttp`, `gw-tunnel-client`, new owned `gw-tunnel-server` crate/binary, `gw-proxy`, `gw-gateway`, `gw-egress-impersonate` loopback pattern, `hyper` HTTP/1 utilities, testcontainers Redis/Postgres e2e, GitLab split artifact jobs.

---

## Scope Boundaries

- This plan update is docs-only. Source implementation happens in the W8 execution wave.
- Do not chase full aether-hub compatibility. Treat aether and `e2e-fake-upstream` mock behavior as prior art for frame shapes, not as a compatibility target.
- Do define and test the owned tunnel protocol before proxy bridge wiring.
- Do implement a first-party tunnel server/client pair in this repo.
- Do not promote `latest` to a tunnel-enabled image during W8 implementation.
- Do not delete `latest-tunnel` or the separate tunnel build/push jobs.
- Do not let `require_tunnel = true` fall back to Direct in either default or tunnel builds.
- Do not combine tunnel with `egress_mode = Impersonate`; keep `GW_TUNNEL_IMPERSONATE_CONFLICT`.
- Do not bypass `SafeDnsResolver` or `target_filter::assert_target_allowed`.
- Do not send any single `Data` frame larger than `MAX_PAYLOAD_LEN`.
- Do not archive or forward tunnel-internal `X-Gateway-Tunnel-*` headers to the real upstream.

## Current Evidence

- `docs/spec/09-tunnel.md` marks three W8 residuals: HTTP<->WS frame bridge runtime, full round-trip e2e, and default artifact policy.
- `docs/plans/2026-06-09-w0-contract-evidence.md` records that the current local mock accepts `Open { target }`, forwards `Data` frames to a TCP connection, and sends upstream bytes back as `Data`.
- `crates/gw-tunnel-client/src/frame.rs` defines the current frame layout and `MsgType::{Open, Data, Close, Ping, Pong, Error, ControlAuth, ControlNodeInfo}`.
- `crates/gw-tunnel-client/src/transport.rs` can `send_open(target)` and `send_data(stream_id, payload)`, but it does not expose inbound non-Pong frames to callers.
- `crates/gw-tunnel-client/src/pool.rs` supervises one `WsTransport` per node and exposes `current()`.
- `crates/gw-proxy/src/tunnel_stack.rs::peer_for_node` validates DNS/target and returns an `HttpPeer` pointed at the node WS endpoint. That is incomplete because Pingora would speak HTTP to a WebSocket endpoint.
- `crates/gw-proxy/src/filters/upstream.rs::pick_peer` already selects Direct vs tunnel and maps `NoLive`, `DnsBlocked`, and `TargetDenied` to stable `GW_TUNNEL_*` codes.
- `crates/e2e-fake-upstream/src/tunnel_node.rs` is a mock fixture and may remain useful for comparison, but W8 e2e must run against the owned `gw-tunnel-server`.
- `.gitlab-ci.yml` already builds default non-tunnel `latest` and tunnel-enabled `latest-tunnel`; `Dockerfile` already accepts `ARG CARGO_FEATURES`.

## Owned Tunnel Protocol Contract

W8 source execution must add a versioned protocol specification before changing transport behavior.

**Create:** `docs/spec/10-owned-tunnel-protocol.md`

The spec must lock these behaviors:

- Wire format: keep the existing frame header and `MsgType` enum unless implementation discovers a concrete reason to change it.
- Versioning: `ControlNodeInfo` payload includes protocol name `ailinkmax-tunnel`, semver-compatible protocol version, node id, and server capabilities.
- Authentication: client sends `ControlAuth` as the first client frame when `ProxyNodeView.auth_token` is present. The owned server accepts the configured token and otherwise sends `Error` then closes the WebSocket.
- Open target: `Open` payload is UTF-8 `host:port`.
- TLS ownership: gateway owns TLS to HTTPS vendors; the tunnel server forwards opaque TCP bytes.
- Data: `Data` carries opaque bytes in both directions for the same `stream_id`.
- Close: `Close` means EOF for that stream; both sides may send it and must release stream state after forwarding buffered bytes.
- Error: `Error` payload is UTF-8 diagnostic text and maps to a tunnel upstream error at the bridge.
- Multiplexing: stream ids are client-assigned monotonically increasing `u32` values; interleaved `Data` frames for different stream ids are supported; `max_streams_per_conn` is enforced client-side and server-side.
- Frame cap: `MAX_PAYLOAD_LEN` is the maximum payload size per frame; larger writes are chunked before send.
- Target safety: server applies the same private-network and deny-list posture as the gateway resolver path when it performs TCP connect.

Run after spec and frame tests:

```bash
cargo test -p gw-tunnel-client frame
rg -n "ailinkmax-tunnel|ControlAuth|Open|MAX_PAYLOAD_LEN|max_streams_per_conn" docs/spec/10-owned-tunnel-protocol.md
```

Expected: PASS, and the spec names the owned protocol instead of aether compatibility.

## File Structure

- Create: `docs/spec/10-owned-tunnel-protocol.md`
  - Owned tunnel protocol contract for server and client.
- Modify: `Cargo.toml`
  - Add workspace member `crates/gw-tunnel-server`.
- Create: `crates/gw-tunnel-server/Cargo.toml`
  - New owned tunnel server crate.
- Create: `crates/gw-tunnel-server/src/lib.rs`
  - Server accept loop, auth handling, frame router, TCP stream relay, target validation, graceful shutdown helpers.
- Create: `crates/gw-tunnel-server/src/main.rs`
  - Binary entrypoint `gw-tunnel-server`.
- Create: `crates/gw-tunnel-server/tests/protocol.rs`
  - Server/client protocol contract tests.
- Modify: `crates/gw-tunnel-client/src/transport.rs`
  - Add inbound frame router, bidirectional `TunnelStream`, `open_stream`, chunked data send, close send, error propagation, and stream-cap enforcement.
- Modify: `crates/gw-tunnel-client/src/pool.rs`
  - Keep `current()` as the live transport accessor; add tests only if stream-cap behavior needs pool-level coverage.
- Modify: `crates/gw-tunnel-client/tests/transport.rs`
  - Cover `Open`, inbound `Data`, inbound `Close`, inbound `Error`, chunk splitting, auth-first behavior, and max stream cap.
- Create: `crates/gw-proxy/src/tunnel_bridge.rs`
  - In-process HTTP/1 loopback bridge that strips `X-Gateway-Tunnel-*`, opens a tunnel stream, relays request/response bytes, and maps tunnel failures to stable gateway-visible responses.
- Modify: `crates/gw-proxy/src/lib.rs`
  - Export `tunnel_bridge` under `#[cfg(feature = "tunnel")]`.
- Modify: `crates/gw-proxy/src/ctx.rs`
  - Add tunnel bridge target metadata used by `upstream::rewrite_request`.
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`
  - Replace remote-WS `HttpPeer` behavior with validated tunnel bridge peer construction.
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
  - Store selected node/target in `ReqCtx`, return the bridge `HttpPeer`, inject tunnel internal headers after adapter rewrite, and redact them from archives.
- Modify: `crates/gw-proxy/src/gateway.rs`
  - Redact `X-Gateway-Tunnel-*` from detailed archives and keep failover/body filters unchanged.
- Modify: `crates/gw-gateway/src/lib.rs`
  - Start the bridge handle when tunnel is enabled and pass it into `TunnelStack`.
- Modify: `crates/gw-gateway/tests/e2e_tunnel.rs`
  - Add live ignored round-trip tests using `gw-tunnel-server` and fake upstream.
- Modify docs after green source verification:
  - `docs/spec/09-tunnel.md`
  - `docs/workflow.md`
  - `docs/architecture.md`
  - `docs/ROADMAP.md`
- Do not modify `.gitlab-ci.yml` or `Dockerfile` unless the owned tunnel e2e proves the existing `latest-tunnel` job cannot build or run a usable tunnel image.

## Task W8.1: Define The Owned Tunnel Protocol

**Files:**
- Create: `docs/spec/10-owned-tunnel-protocol.md`
- Modify if needed: `crates/gw-tunnel-client/src/frame.rs`
- Test: `crates/gw-tunnel-client/tests/frame.rs`

- [ ] **Step 1: Write the protocol spec**

Create `docs/spec/10-owned-tunnel-protocol.md` with the contract in this plan. State clearly that aether/mock behavior is reference material, not a compatibility target.

Run:

```bash
rg -n "owned|ailinkmax-tunnel|not aether|Open|ControlAuth|ControlNodeInfo|MAX_PAYLOAD_LEN" docs/spec/10-owned-tunnel-protocol.md
```

Expected: the spec locks the owned protocol and explicitly rejects aether compatibility as the W8 goal.

- [ ] **Step 2: Keep or adjust frame layout deliberately**

Prefer keeping `crates/gw-tunnel-client/src/frame.rs` unchanged. If the owned protocol needs a frame change, update `crates/gw-tunnel-client/tests/frame.rs` first with exact byte-level expectations.

Run:

```bash
cargo test -p gw-tunnel-client frame
```

Expected: PASS.

- [ ] **Step 3: Add protocol conformance tests**

Add tests proving:

- `Open` payload is raw UTF-8 `host:port`;
- `ControlAuth` is sent before `Open` when auth is configured;
- `ControlNodeInfo` with protocol `ailinkmax-tunnel` can be parsed or ignored without breaking stream setup;
- oversized outbound payloads are split at `MAX_PAYLOAD_LEN`.

Run:

```bash
cargo test -p gw-tunnel-client protocol
cargo test -p gw-tunnel-client transport
```

Expected: PASS.

## Task W8.2: Implement Owned Tunnel Server Crate And Binary

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/gw-tunnel-server/Cargo.toml`
- Create: `crates/gw-tunnel-server/src/lib.rs`
- Create: `crates/gw-tunnel-server/src/main.rs`
- Create: `crates/gw-tunnel-server/tests/protocol.rs`

- [ ] **Step 1: Add the crate skeleton**

Add workspace member `crates/gw-tunnel-server`. The crate must expose a library API for tests and a binary named `gw-tunnel-server`.

Minimum public API:

```rust
pub struct TunnelServerConfig {
    pub bind: std::net::SocketAddr,
    pub auth_token: Option<String>,
    pub max_streams_per_conn: usize,
}

pub struct TunnelServerHandle {
    pub local_addr: std::net::SocketAddr,
}

pub async fn spawn(config: TunnelServerConfig) -> anyhow::Result<TunnelServerHandle>;
```

Run:

```bash
cargo check -p gw-tunnel-server
cargo run -p gw-tunnel-server -- --help
```

Expected: the crate checks and the binary exposes bind/auth/max-stream options.

- [ ] **Step 2: Implement WebSocket frame handling**

The server must:

- accept WebSocket connections;
- require valid `ControlAuth` first when `auth_token` is configured;
- send `ControlNodeInfo` after successful auth or immediately for unauthenticated mode;
- accept `Open` with `host:port`;
- validate target before TCP connect;
- relay TCP bytes as `Data`;
- write inbound `Data` frames to the TCP stream;
- release state on `Close`, TCP EOF, WebSocket close, or `Error`;
- enforce server-side max stream count.

Run:

```bash
cargo test -p gw-tunnel-server
```

Expected: PASS.

- [ ] **Step 3: Add client/server interoperability tests**

In `crates/gw-tunnel-server/tests/protocol.rs`, start a local plaintext fake upstream, start `gw-tunnel-server`, connect with `gw-tunnel-client`, open a stream to the fake upstream, send an HTTP/1 request as opaque bytes, and assert the response bytes are returned as `Data`.

Also cover:

- auth success;
- auth failure returns `Error` or closes before `Open`;
- stream cap rejection;
- target denied does not reach the fake upstream.

Run:

```bash
cargo test -p gw-tunnel-server --test protocol -- --nocapture
```

Expected: PASS with a real local WebSocket-to-TCP round trip.

## Task W8.3: Add Bidirectional Client Transport API

**Files:**
- Modify: `crates/gw-tunnel-client/src/transport.rs`
- Modify: `crates/gw-tunnel-client/src/pool.rs` only if stream-cap state is owned outside `WsTransport`
- Test: `crates/gw-tunnel-client/tests/transport.rs`

- [ ] **Step 1: Add `TunnelStream` and inbound routing**

Add a public `TunnelStream` type with:

- `stream_id()`;
- `send_data(Bytes)`;
- `close()`;
- `recv()` returning inbound `Data`/`Close` or typed remote error.

Change `WsTransport` to route inbound `Data`, `Close`, and `Error` by `stream_id`. Keep `Pong` on the heartbeat path. Remove the stream route after `Close` or `Error`.

- [ ] **Step 2: Add `open_stream`**

`open_stream(target: &str)` must:

- fail with `NotAlive` when transport is not live;
- acquire the `max_streams_per_conn` permit;
- allocate a client-owned stream id;
- register inbound routing before sending `Open`;
- send `Open` payload as UTF-8 `host:port`;
- release the permit when `TunnelStream` is dropped.

Keep `send_open` and `send_data` as compatibility wrappers only if existing callers still need them; they must share the same frame sender semantics.

- [ ] **Step 3: Test transport behavior**

Cover:

- `open_stream_receives_data_and_close_frames`;
- `open_stream_maps_error_frame_to_remote_error`;
- `stream_send_data_splits_payload_at_max_frame_len`;
- `open_stream_respects_max_streams_per_conn`;
- `control_auth_is_first_frame_when_token_configured`.

Run:

```bash
cargo test -p gw-tunnel-client transport
```

Expected: PASS.

## Task W8.4: Build The In-Process Tunnel HTTP Bridge

**Files:**
- Create: `crates/gw-proxy/src/tunnel_bridge.rs`
- Modify: `crates/gw-proxy/src/lib.rs`
- Modify: `crates/gw-proxy/Cargo.toml` if additional `hyper` features are required
- Test: `crates/gw-proxy/src/tunnel_bridge.rs`

- [ ] **Step 1: Add bridge constants and redaction helper**

Create stable internal header constants:

```rust
pub const HDR_TUNNEL_NODE_ID: &str = "X-Gateway-Tunnel-Node-Id";
pub const HDR_TUNNEL_TARGET: &str = "X-Gateway-Tunnel-Target";
pub const HDR_TUNNEL_TARGET_TLS: &str = "X-Gateway-Tunnel-Target-Tls";
```

Add `strip_internal_headers` and tests proving internal headers are removed while vendor headers remain.

Run:

```bash
cargo test -p gw-proxy tunnel_bridge --features tunnel
```

Expected: PASS.

- [ ] **Step 2: Add bridge handle and loopback listener**

Follow the existing `gw-egress-impersonate` listener shape. The bridge must expose a local `HttpPeer` target and accept only loopback traffic from the gateway process.

Minimum API:

```rust
pub struct TunnelBridgeHandle {
    pub local_addr: std::net::SocketAddr,
}

impl TunnelBridgeHandle {
    pub async fn start(
        bind: std::net::SocketAddr,
        stack: std::sync::Weak<crate::tunnel_stack::TunnelStack>,
    ) -> anyhow::Result<Self>;
}
```

Run:

```bash
cargo test -p gw-proxy tunnel_bridge --features tunnel
```

Expected: PASS.

- [ ] **Step 3: Relay HTTP/1 through tunnel streams**

The bridge must:

- read `X-Gateway-Tunnel-Node-Id` and `X-Gateway-Tunnel-Target`;
- look up the selected live `WsTransport`;
- call `open_stream(target)`;
- serialize the outbound HTTP/1 request as opaque bytes after stripping internal headers;
- stream request body chunks through `Data`;
- parse inbound response bytes from `Data`;
- map inbound `Close` to upstream EOF;
- map `Error`, no node, denied target, and transport failures to stable gateway-visible responses.

Run:

```bash
cargo test -p gw-proxy tunnel_bridge --features tunnel
```

Expected: PASS.

## Task W8.5: Wire Proxy Selection To The Bridge

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: `crates/gw-proxy/src/tunnel_stack.rs`
- Test: `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: Replace remote-WS peer behavior**

Change tunnel peer resolution so `peer_for_node` validates target and live transport, then returns the bridge loopback peer rather than the node WebSocket URL.

Run:

```bash
cargo test -p gw-proxy --features tunnel peer_for_node
```

Expected: PASS, and tunnel peers point at loopback bridge addresses.

- [ ] **Step 2: Carry bridge metadata through request context**

Add fields to `ReqCtx` for selected tunnel node id, target `host:port`, and TLS ownership flag. Populate them when `TunnelResolution::Use(node_id)` succeeds.

Run:

```bash
cargo test -p gw-proxy --features tunnel upstream
```

Expected: PASS.

- [ ] **Step 3: Inject and redact internal headers**

After outbound adapter rewrite, inject:

- `X-Gateway-Tunnel-Node-Id`;
- `X-Gateway-Tunnel-Target`;
- `X-Gateway-Tunnel-Target-Tls`.

Ensure archive/detailed logging redacts these headers and the bridge strips them before sending bytes to the real upstream.

Run:

```bash
cargo test -p gw-proxy --features tunnel tunnel_bridge upstream detailed_log
```

Expected: PASS.

- [ ] **Step 4: Start bridge from gateway**

When tunnel feature/config is enabled, start the bridge handle in `crates/gw-gateway/src/lib.rs` and pass it into `TunnelStack`. Keep tunnel-disabled startup unchanged.

Run:

```bash
cargo test -p gw-gateway --features tunnel feature_gate
cargo test -p gw-gateway feature_gate
```

Expected: PASS.

## Task W8.6: Prove Owned Server E2E

**Files:**
- Modify: `crates/gw-gateway/tests/e2e_tunnel.rs`
- Reference: `crates/gw-gateway/tests/e2e_impersonate.rs`
- Reference: `crates/e2e-fake-upstream/src/server.rs`

- [ ] **Step 1: Add positive ignored e2e**

Add ignored Tokio test `e2e_tunnel_round_trip_reaches_fake_upstream_through_owned_server` with ignore reason `requires Docker (testcontainers Redis + Postgres) and owned tunnel server fixtures`.

The test must:

- start Redis/Postgres fixtures;
- start plaintext fake upstream on `127.0.0.1:0`;
- start `gw-tunnel-server` on `127.0.0.1:0`;
- seed one enabled `proxy_nodes` row with `ws_url` pointing at the owned tunnel server;
- configure one provider/model requiring tunnel;
- issue a client request through the gateway;
- assert the fake upstream receives the request;
- assert the client receives a 2xx fake upstream response;
- assert no direct egress path was used.

Run:

```bash
cargo test -p gw-gateway --features tunnel --test e2e_tunnel e2e_tunnel_round_trip_reaches_fake_upstream_through_owned_server -- --ignored --nocapture
```

Expected: PASS.

- [ ] **Step 2: Add negative ignored e2e cases**

Add:

- `e2e_tunnel_require_true_fails_when_no_live_node`;
- `e2e_tunnel_target_denied_does_not_reach_owned_server`;
- `e2e_tunnel_auth_failure_is_client_visible_gateway_error`.

Run:

```bash
cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored --nocapture
```

Expected: PASS.

## Task W8.7: Update Specs, Workflow, Roadmap, And Artifact Decision Packet

**Files:**
- Modify: `docs/spec/09-tunnel.md`
- Modify: `docs/workflow.md`
- Modify: `docs/architecture.md`
- Modify: `docs/ROADMAP.md`
- Keep: `.gitlab-ci.yml`
- Keep: `Dockerfile`

- [ ] **Step 1: Close runtime/e2e residuals in tunnel spec**

After source verification is green, update `docs/spec/09-tunnel.md` so W8 bridge and owned-server e2e are closed. Keep default artifact policy as a release decision, not a completed default promotion.

Run:

```bash
rg -n "HTTP<->WS frame bridge runtime|owned tunnel server|Default artifact remains non-tunnel|latest-tunnel" docs/spec/09-tunnel.md
```

Expected: W8 runtime/e2e are described as implemented after the execution wave, and artifact policy still requires explicit release decision.

- [ ] **Step 2: Update workflow and architecture docs**

Document that:

- tunnel mode uses the owned `gw-tunnel-server` / `gw-tunnel-client` protocol;
- aether/mock behavior is reference-only;
- default CI artifact remains non-tunnel;
- `latest-tunnel` remains operator-selected.

Run:

```bash
rg -n "gw-tunnel-server|owned tunnel|latest-tunnel|Default CI artifact remains non-tunnel" docs/workflow.md docs/architecture.md
```

Expected: docs name the owned tunnel server and keep split artifact behavior.

- [ ] **Step 3: Update roadmap**

Remove W8 runtime/e2e as open residuals only after green implementation. For artifact policy, use this wording:

```text
W8 bridge and live e2e are closed against the owned tunnel server. Default artifact remains non-tunnel pending an explicit user release decision; `latest-tunnel` remains the tunnel-enabled operator artifact.
```

Run:

```bash
rg -n "owned tunnel server|Default artifact remains non-tunnel|latest-tunnel|release decision" docs/ROADMAP.md
```

Expected: roadmap reflects W8 implementation status without claiming default promotion.

- [ ] **Step 4: Prepare the post-e2e release decision packet**

After live e2e is green, return this packet to the user:

```text
User Decision Needed: tunnel default artifact after W8 owned-server e2e

Verified:
- owned tunnel protocol spec;
- gw-tunnel-server and gw-tunnel-client interoperability;
- proxy bridge HTTP<->WS round trip;
- cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored.

Current artifact policy:
- default `latest` is non-tunnel.
- `latest-tunnel` is tunnel-enabled with `--features tunnel`.

Please choose:
1. Keep split artifacts. `latest` stays non-tunnel and operators opt in with `latest-tunnel`.
2. Promote tunnel into default `latest` while keeping `[tunnel].enabled=false` by default at runtime.
```

Expected: no artifact default changes happen without the user's explicit answer.

## Task W8.8: Verification And Commit Plan

- [ ] **Step 1: Focused tests**

Run:

```bash
cargo test -p gw-tunnel-client frame
cargo test -p gw-tunnel-client transport
cargo test -p gw-tunnel-server
cargo test -p gw-proxy --features tunnel tunnel_bridge
cargo test -p gw-proxy --features tunnel upstream
cargo test -p gw-gateway --features tunnel feature_gate
cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored --nocapture
```

Expected: PASS.

- [ ] **Step 2: Docs and artifact checks**

Run:

```bash
rg -n "aether|mock|owned tunnel|gw-tunnel-server|latest-tunnel|release decision" docs/spec/10-owned-tunnel-protocol.md docs/spec/09-tunnel.md docs/workflow.md docs/architecture.md docs/ROADMAP.md
git diff --check
```

Expected: references make clear that aether/mock is only reference material, owned tunnel is the target, and default artifact remains gated by release decision.

- [ ] **Step 3: Commit protocol and server**

```bash
git add docs/spec/10-owned-tunnel-protocol.md Cargo.toml crates/gw-tunnel-server crates/gw-tunnel-client
git commit -m "feat(tunnel): add owned server client protocol"
```

- [ ] **Step 4: Commit proxy bridge**

```bash
git add crates/gw-proxy crates/gw-gateway/src/lib.rs
git commit -m "feat(tunnel): bridge proxy traffic through owned server"
```

- [ ] **Step 5: Commit live e2e**

```bash
git add crates/gw-gateway/tests/e2e_tunnel.rs
git commit -m "test(tunnel): prove owned server roundtrip e2e"
```

- [ ] **Step 6: Commit docs and artifact policy**

```bash
git add docs/spec/09-tunnel.md docs/workflow.md docs/architecture.md docs/ROADMAP.md
git commit -m "docs(tunnel): close owned bridge and artifact policy residuals"
```

- [ ] **Step 7: Update this plan during execution**

As W8 implementation progresses, update checkboxes in this plan with verified command output and commit ids. Keep the plan aligned with the owned server/client direction and do not reintroduce aether compatibility as a blocker.

```bash
git add docs/plans/2026-06-09-w8-tunnel-bridge-e2e.md
git commit -m "docs(plan): update owned tunnel execution status"
```

## Handoff Summary

W8 is no longer blocked on confirming the real aether-hub protocol. The implementation target is an owned `ailinkmax-tunnel` protocol with first-party `gw-tunnel-server` and `gw-tunnel-client` behavior. Aether and current mocks are reference-only. The bridge must route Pingora-selected tunnel traffic through a loopback HTTP bridge into owned WebSocket streams. E2E must prove a real request reaches a fake upstream through the owned tunnel server. Default `latest` remains non-tunnel until the user explicitly chooses a release promotion after green e2e.
