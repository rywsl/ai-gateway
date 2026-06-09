# W8 Tunnel HTTP<->WS Bridge, E2E, And Artifact Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the tunnel runtime by bridging Pingora upstream HTTP traffic through WebSocket frames, proving a live round trip, and keeping the default artifact non-tunnel until a post-e2e release decision.

**Architecture:** Do not treat the remote node WebSocket URL as a Pingora upstream peer. Keep Pingora's existing request/response filters intact by routing tunnel-selected traffic to an in-process HTTP/1 loopback bridge, then have that bridge open a `gw-tunnel-client` stream to the selected node and relay `Open` / `Data` / `Close` / `Error` frames. The split artifact model remains unchanged during W8: default `latest` stays non-tunnel and `latest-tunnel` remains the operator-selected build.

**Tech Stack:** Rust 1.88 workspace, Pingora 0.8 `ProxyHttp`, `gw-tunnel-client`, `gw-proxy`, `gw-gateway`, `gw-egress-impersonate` loopback pattern, `hyper` HTTP/1 server/client utilities, testcontainers Redis/Postgres e2e, `e2e-fake-upstream` tunnel node, GitLab split artifact jobs.

---

## Scope Boundaries

- This plan is docs-only. Source implementation happens in the W8 execution wave.
- Do not change Rust source until the real aether-hub protocol decision below is answered.
- Do not promote `latest` to a tunnel-enabled image in W8 source execution.
- Do not delete `latest-tunnel` or the separate tunnel build/push jobs.
- Do not let `require_tunnel = true` fall back to Direct in either default or tunnel builds.
- Do not combine tunnel with `egress_mode = Impersonate`; keep `GW_TUNNEL_IMPERSONATE_CONFLICT`.
- Do not bypass `SafeDnsResolver` or `target_filter::assert_target_allowed`.
- Do not send any single `Data` frame larger than `MAX_PAYLOAD_LEN`.
- Do not archive or forward tunnel-internal `X-Gateway-Tunnel-*` headers to the real upstream.

## Current Evidence

- `docs/spec/09-tunnel.md` marks three W8 residuals: HTTP<->WS frame bridge runtime, full round-trip e2e, and default artifact policy.
- `docs/plans/2026-06-09-w0-contract-evidence.md` says the local mock accepts `Open { target }`, forwards `Data` frames to a TCP connection, and sends upstream bytes back as `Data`, but the real aether-hub protocol is not proven from this checkout.
- `crates/gw-tunnel-client/src/frame.rs` defines the frame layout and `MsgType::{Open, Data, Close, Ping, Pong, Error, ControlAuth, ControlNodeInfo}`.
- `crates/gw-tunnel-client/src/transport.rs` can `send_open(target)` and `send_data(stream_id, payload)`, but it does not expose inbound non-Pong frames to callers.
- `crates/gw-tunnel-client/src/pool.rs` supervises one `WsTransport` per node and exposes `current()`.
- `crates/gw-proxy/src/tunnel_stack.rs::peer_for_node` validates DNS/target and returns an `HttpPeer` pointed at the node WS endpoint. That is not a complete bridge because Pingora would speak HTTP to a WebSocket endpoint.
- `crates/gw-proxy/src/filters/upstream.rs::pick_peer` already selects Direct vs tunnel and maps `NoLive`, `DnsBlocked`, and `TargetDenied` to stable `GW_TUNNEL_*` codes.
- `crates/e2e-fake-upstream/src/tunnel_node.rs` is the local mock: raw `host:port` `Open` payload, TCP connect, `Data` frames in both directions, `Close` removes stream state, `Ping` returns `Pong`.
- `.gitlab-ci.yml` already builds default non-tunnel `latest` and tunnel-enabled `latest-tunnel`; `Dockerfile` already accepts `ARG CARGO_FEATURES`.

## User Decision Needed

The real aether-hub protocol must be confirmed before W8 source execution starts. The current local mock contract is usable only if the user confirms it matches the real remote node.

### Decision Packet: Real Aether-Hub Tunnel Protocol

Send this packet to the user or aether-hub owner and wait for an explicit answer:

```text
User Decision Needed: real aether-hub tunnel protocol for W8

1. Open payload
   - Is `MsgType::Open` payload raw `host:port`, a URL, or structured JSON?
   - If structured, provide the exact JSON fields and required encoding.

2. TLS ownership
   - Does the gateway own TLS to the vendor over the tunnel stream, with the remote node forwarding opaque TCP bytes?
   - Or does the remote node terminate TLS to the vendor and receive plaintext HTTP bytes from the gateway?

3. Data/Close/Error semantics
   - Does `MsgType::Data` carry opaque bytes in both directions for the same `stream_id`?
   - Does upstream EOF map to `MsgType::Close(stream_id)`?
   - Does downstream EOF require the gateway to send `MsgType::Close(stream_id)`?
   - What exact payload should `MsgType::Error(stream_id)` carry, and which errors require a gateway retry vs a client-visible 502?

4. ControlAuth ack/error
   - Is `ControlAuth` payload the raw token bytes from `ProxyNodeView.auth_token`?
   - Does the node send an ack frame, an Error frame, or no ack?
   - If auth fails, does the node close the WebSocket immediately or send a typed error first?

5. ControlNodeInfo
   - Does the node send `ControlNodeInfo` after connect?
   - Must the gateway wait for it before opening streams?
   - What exact payload schema should the gateway parse or ignore?

6. Multiplex constraints
   - Is `max_streams_per_conn` the only concurrent stream cap?
   - Are stream ids client-assigned monotonically increasing u32 values?
   - Are interleaved `Data` frames for different stream ids supported?
   - Are there per-stream flow-control windows beyond the 4 MiB frame cap?

Recommended only if you confirm it matches real aether-hub:
Use the current mock contract for W8:
- Open payload is raw `host:port`.
- Gateway owns TLS when the target is HTTPS; the node forwards opaque TCP bytes.
- Data carries opaque bytes in both directions.
- Close means EOF for that stream.
- Error carries a UTF-8 diagnostic and maps to a tunnel upstream error.
- ControlAuth is raw token bytes with no ack required.
- ControlNodeInfo is ignored if received.
- `max_streams_per_conn` is the concurrent multiplex limit.
```

If the real protocol differs from the recommended mock contract, update `crates/e2e-fake-upstream/src/tunnel_node.rs` and `gw-tunnel-client` tests together before implementing proxy wiring. If the protocol is not answered, stop W8 execution after writing this decision packet.

## File Structure

- Modify: `crates/gw-tunnel-client/src/transport.rs`
  - Add an inbound frame router, bidirectional `TunnelStream`, `open_stream`, chunked data send, close send, error propagation, and stream-cap enforcement.
- Modify: `crates/gw-tunnel-client/src/pool.rs`
  - Keep `current()` as the live transport accessor; add tests only if stream-cap behavior needs pool-level coverage.
- Modify: `crates/gw-tunnel-client/tests/transport.rs`
  - Cover `Open`, inbound `Data`, inbound `Close`, inbound `Error`, chunk splitting, and max stream cap.
- Create: `crates/gw-proxy/src/tunnel_bridge.rs`
  - In-process HTTP/1 loopback bridge that strips `X-Gateway-Tunnel-*`, opens a tunnel stream, relays request/response bytes, and maps tunnel failures to stable gateway-visible responses.
- Modify: `crates/gw-proxy/src/lib.rs`
  - Export `tunnel_bridge` under `#[cfg(feature = "tunnel")]`.
- Modify: `crates/gw-proxy/src/ctx.rs`
  - Add tunnel bridge target metadata used by `upstream::rewrite_request`.
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`
  - Replace `peer_for_node`'s remote-WS `HttpPeer` behavior with validated tunnel bridge peer construction.
  - Keep DNS/target validation and live transport checks.
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
  - Store selected node/target in `ReqCtx`, return the bridge `HttpPeer`, inject tunnel internal headers after adapter rewrite, and redact them from archives.
- Modify: `crates/gw-proxy/src/gateway.rs`
  - Redact `X-Gateway-Tunnel-*` from detailed archives and keep failover/body filters unchanged.
- Modify: `crates/gw-gateway/src/lib.rs`
  - Start the bridge handle when tunnel is enabled and pass it into `TunnelStack`.
- Modify: `crates/gw-gateway/tests/e2e_tunnel.rs`
  - Add a live ignored round-trip test using `e2e-fake-upstream` tunnel node and fake upstream.
- Modify: `crates/e2e-fake-upstream/src/tunnel_node.rs`
  - Update mock behavior only if the real aether-hub decision differs from the current raw TCP mock.
- Modify docs after green source verification:
  - `docs/spec/09-tunnel.md`
  - `docs/workflow.md`
  - `docs/architecture.md`
  - `docs/ROADMAP.md`
- Do not modify `.gitlab-ci.yml` or `Dockerfile` unless the tunnel e2e proves the existing `latest-tunnel` job cannot build a usable tunnel image.

## Task W8.1: Confirm Protocol And Lock The Bridge Contract

**Files:**
- Modify only if confirmed protocol differs from current mock: `crates/e2e-fake-upstream/src/tunnel_node.rs`
- Modify only if confirmed protocol differs from current mock: `crates/gw-tunnel-client/src/frame.rs`
- Test: `crates/gw-tunnel-client/tests/frame.rs`
- Test: `crates/gw-tunnel-client/tests/transport.rs`

- [ ] **Step 1: Send the decision packet**

Send the exact `User Decision Needed: real aether-hub tunnel protocol for W8` packet from this plan. Record the answer in the W8 execution branch commit message or in a small `docs/plans/2026-06-09-w8-decision-record.md` file if the answer is lengthy.

Expected: explicit user confirmation of the mock contract or exact alternate protocol details.

- [ ] **Step 2: Stop if protocol is unanswered**

If no answer is available, stop source execution and return this status:

```text
W8 blocked before source changes.
Reason: real aether-hub Open/TLS/Data/Close/Error/ControlAuth/ControlNodeInfo/multiplex contract is not confirmed.
Default artifact remains non-tunnel.
No Rust source changed.
```

Expected: no source files are modified when the decision is missing.

- [ ] **Step 3: If the mock contract is confirmed, keep frame layout unchanged**

Run:

```bash
cargo test -p gw-tunnel-client frame
```

Expected: PASS. No `crates/gw-tunnel-client/src/frame.rs` change is needed.

- [ ] **Step 4: If the protocol differs, update mock and frame tests first**

Adjust `crates/e2e-fake-upstream/src/tunnel_node.rs` and `crates/gw-tunnel-client/tests/frame.rs` before proxy wiring. The frame test must prove the selected protocol with concrete bytes. For a structured JSON Open payload, add a test with this shape:

```rust
#[test]
fn open_payload_matches_confirmed_node_contract() {
    let payload = serde_json::json!({
        "target": "api.openai.com:443",
        "mode": "tcp"
    })
    .to_string();
    let frame = Frame {
        msg_type: MsgType::Open,
        flags: 0,
        stream_id: 7,
        payload: bytes::Bytes::from(payload.clone()),
    };
    let mut buf = bytes::BytesMut::new();
    encode_frame(&frame, &mut buf);
    let (decoded, consumed) = decode_frame(&buf).unwrap().unwrap();
    assert_eq!(consumed, buf.len());
    assert_eq!(decoded.msg_type, MsgType::Open);
    assert_eq!(decoded.stream_id, 7);
    assert_eq!(decoded.payload.as_ref(), payload.as_bytes());
}
```

Run:

```bash
cargo test -p gw-tunnel-client open_payload_matches_confirmed_node_contract
cargo test -p e2e-fake-upstream
```

Expected: PASS with the confirmed payload semantics reflected in both client tests and mock node behavior.

## Task W8.2: Add Bidirectional Tunnel Stream API

**Files:**
- Modify: `crates/gw-tunnel-client/src/transport.rs`
- Modify: `crates/gw-tunnel-client/src/pool.rs` only if stream-cap state is owned outside `WsTransport`
- Test: `crates/gw-tunnel-client/tests/transport.rs`

- [ ] **Step 1: Add failing stream receive test**

Append this test to `crates/gw-tunnel-client/tests/transport.rs`:

```rust
#[tokio::test]
async fn open_stream_receives_data_and_close_frames() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (s, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
        while let Some(Ok(Message::Binary(bin))) = ws.next().await {
            let (f, _) = decode_frame(&bin).unwrap().unwrap();
            if f.msg_type == MsgType::Open {
                let mut data = bytes::BytesMut::new();
                encode_frame(
                    &Frame {
                        msg_type: MsgType::Data,
                        flags: 0,
                        stream_id: f.stream_id,
                        payload: Bytes::from_static(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"),
                    },
                    &mut data,
                );
                ws.send(Message::Binary(data.to_vec())).await.unwrap();
                let mut close = bytes::BytesMut::new();
                encode_frame(
                    &Frame {
                        msg_type: MsgType::Close,
                        flags: 0,
                        stream_id: f.stream_id,
                        payload: Bytes::new(),
                    },
                    &mut close,
                );
                ws.send(Message::Binary(close.to_vec())).await.unwrap();
                break;
            }
        }
    });

    let cfg = TransportConfig {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        heartbeat_interval: Duration::from_secs(60),
        missed_pong_threshold: 5,
        max_streams_per_conn: 4,
        ..TransportConfig::default()
    };
    let transport = WsTransport::connect(cfg).await.unwrap();
    let mut stream = transport.open_stream("api.example.com:443").await.unwrap();

    let first = stream.recv().await.unwrap().unwrap();
    assert_eq!(first.msg_type, MsgType::Data);
    assert_eq!(first.payload.as_ref(), b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let second = stream.recv().await.unwrap().unwrap();
    assert_eq!(second.msg_type, MsgType::Close);
    server.await.unwrap();
}
```

Run:

```bash
cargo test -p gw-tunnel-client open_stream_receives_data_and_close_frames
```

Expected: FAIL with `no method named 'open_stream'`.

- [ ] **Step 2: Add the public stream type and inbound router**

In `crates/gw-tunnel-client/src/transport.rs`, add these public types:

```rust
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("transport is not alive")]
    NotAlive,
    #[error("stream cap reached")]
    StreamCapReached,
    #[error("stream closed")]
    Closed,
    #[error("remote error: {0}")]
    RemoteError(String),
    #[error("send failed: {0}")]
    SendFailed(String),
}

pub struct TunnelStream {
    stream_id: u32,
    out_tx: mpsc::Sender<Frame>,
    in_rx: mpsc::Receiver<Frame>,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
```

Add methods:

```rust
impl TunnelStream {
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    pub async fn send_data(&self, payload: Bytes) -> Result<(), StreamError> {
        for chunk in payload.chunks(crate::frame::MAX_PAYLOAD_LEN) {
            self.out_tx
                .send(Frame {
                    msg_type: MsgType::Data,
                    flags: 0,
                    stream_id: self.stream_id,
                    payload: Bytes::copy_from_slice(chunk),
                })
                .await
                .map_err(|e| StreamError::SendFailed(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn close(&self) -> Result<(), StreamError> {
        self.out_tx
            .send(Frame {
                msg_type: MsgType::Close,
                flags: 0,
                stream_id: self.stream_id,
                payload: Bytes::new(),
            })
            .await
            .map_err(|e| StreamError::SendFailed(e.to_string()))
    }

    pub async fn recv(&mut self) -> Result<Option<Frame>, StreamError> {
        match self.in_rx.recv().await {
            Some(frame) if frame.msg_type == MsgType::Error => {
                let msg = String::from_utf8_lossy(&frame.payload).to_string();
                Err(StreamError::RemoteError(msg))
            }
            other => Ok(other),
        }
    }
}
```

Change `WsTransport` to hold:

```rust
inbound: Arc<dashmap::DashMap<u32, mpsc::Sender<Frame>>>,
stream_cap: Arc<tokio::sync::Semaphore>,
```

In the WebSocket receive task, route `Data`, `Close`, and `Error` frames by `stream_id` through `inbound`. Keep `Pong` handling on the existing heartbeat path. On `Close` or `Error`, remove the `stream_id` entry after forwarding the frame.

- [ ] **Step 3: Add `open_stream`**

Add this method beside `send_open`:

```rust
pub async fn open_stream(&self, target: &str) -> Result<TunnelStream, StreamError> {
    if !self.is_alive() {
        return Err(StreamError::NotAlive);
    }
    let permit = self
        .stream_cap
        .clone()
        .try_acquire_owned()
        .map_err(|_| StreamError::StreamCapReached)?;
    let id = self.next_stream_id.fetch_add(1, Ordering::AcqRel);
    let (in_tx, in_rx) = mpsc::channel::<Frame>(64);
    self.inbound.insert(id, in_tx);
    self.out_tx
        .send(Frame {
            msg_type: MsgType::Open,
            flags: 0,
            stream_id: id,
            payload: Bytes::copy_from_slice(target.as_bytes()),
        })
        .await
        .map_err(|e| StreamError::SendFailed(e.to_string()))?;
    Ok(TunnelStream {
        stream_id: id,
        out_tx: self.out_tx.clone(),
        in_rx,
        _permit: permit,
    })
}
```

Keep `send_open` and `send_data` as compatibility wrappers, or replace all production callers during this task. Compatibility wrappers must delegate to the same frame sender so tests do not fork semantics.

- [ ] **Step 4: Add chunking and stream-cap tests**

Add `stream_send_data_splits_payload_at_max_frame_len` to `crates/gw-tunnel-client/tests/transport.rs` with this exact behavior:

- start a local WebSocket server that accepts one connection;
- call `transport.open_stream("api.example.com:443")`;
- drain the first `Open` frame from the server;
- build `let payload = Bytes::from(vec![b'x'; gw_tunnel_client::frame::MAX_PAYLOAD_LEN + 1]);`;
- call `stream.send_data(payload).await.unwrap();`;
- collect the next two `Data` frames on the server;
- assert the first payload length is `gw_tunnel_client::frame::MAX_PAYLOAD_LEN`;
- assert the second payload length is `1`.

Add `open_stream_respects_max_streams_per_conn` to the same file with this exact behavior:

- start a local WebSocket server that accepts one connection and keeps it open;
- connect with `TransportConfig { max_streams_per_conn: 1, .. }`;
- call `open_stream("api.example.com:443")` and keep the returned stream in scope;
- call `open_stream("api.example.com:443")` again;
- assert the second result matches `Err(StreamError::StreamCapReached)`;
- drop the first stream;
- call `open_stream("api.example.com:443")` a third time;
- assert the third result is `Ok(_)`.

```rust
assert!(matches!(second, Err(StreamError::StreamCapReached)));
assert!(third.is_ok());
```

Use the existing local WebSocket helper style in `transport.rs`; keep every server assertion on decoded `Frame` values, not raw byte offsets.

Run:

```bash
cargo test -p gw-tunnel-client transport
```

Expected: PASS.

## Task W8.3: Build The In-Process Tunnel HTTP Bridge

**Files:**
- Create: `crates/gw-proxy/src/tunnel_bridge.rs`
- Modify: `crates/gw-proxy/src/lib.rs`
- Modify: `crates/gw-proxy/Cargo.toml` if additional `hyper` features are required
- Test: `crates/gw-proxy/src/tunnel_bridge.rs`

- [ ] **Step 1: Add failing bridge header contract tests**

Create `crates/gw-proxy/src/tunnel_bridge.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_header_names_are_stable() {
        assert_eq!(HDR_TUNNEL_NODE_ID, "X-Gateway-Tunnel-Node-Id");
        assert_eq!(HDR_TUNNEL_TARGET, "X-Gateway-Tunnel-Target");
        assert_eq!(HDR_TUNNEL_TARGET_TLS, "X-Gateway-Tunnel-Target-Tls");
    }

    #[test]
    fn strip_internal_headers_keeps_vendor_headers() {
        let headers = vec![
            ("X-Gateway-Tunnel-Node-Id".to_string(), "node-a".to_string()),
            ("X-Gateway-Tunnel-Target".to_string(), "api.example.com:443".to_string()),
            ("Authorization".to_string(), "Bearer sk".to_string()),
        ];
        let stripped = strip_internal_headers(headers);
        assert_eq!(stripped, vec![("Authorization".to_string(), "Bearer sk".to_string())]);
    }
}
```

Run:

```bash
cargo test -p gw-proxy tunnel_bridge --features tunnel
```

Expected: FAIL with missing constants/functions.

- [ ] **Step 2: Add bridge public constants and redaction helper**

Add:

```rust
pub const HDR_TUNNEL_NODE_ID: &str = "X-Gateway-Tunnel-Node-Id";
pub const HDR_TUNNEL_TARGET: &str = "X-Gateway-Tunnel-Target";
pub const HDR_TUNNEL_TARGET_TLS: &str = "X-Gateway-Tunnel-Target-Tls";

pub fn strip_internal_headers(headers: Vec<(String, String)>) -> Vec<(String, String)> {
    headers
        .into_iter()
        .filter(|(name, _)| {
            !name.eq_ignore_ascii_case(HDR_TUNNEL_NODE_ID)
                && !name.eq_ignore_ascii_case(HDR_TUNNEL_TARGET)
                && !name.eq_ignore_ascii_case(HDR_TUNNEL_TARGET_TLS)
        })
        .collect()
}
```

In `crates/gw-proxy/src/lib.rs`, add:

```rust
#[cfg(feature = "tunnel")]
pub mod tunnel_bridge;
```

Run:

```bash
cargo test -p gw-proxy tunnel_bridge --features tunnel
```

Expected: PASS.

- [ ] **Step 3: Add bridge handle and loopback listener**

Follow the existing `gw-egress-impersonate` listener shape:

```rust
#[derive(Clone, Debug)]
pub enum TunnelBridgeBind {
    Uds(std::path::PathBuf),
    Tcp(std::net::SocketAddr),
}

#[derive(Clone)]
pub struct TunnelBridgeHandle {
    bind: TunnelBridgeBind,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
    done_rx: tokio::sync::watch::Receiver<bool>,
}
```

Expose:

```rust
impl TunnelBridgeHandle {
    pub fn start(bind: TunnelBridgeBind, stack: std::sync::Weak<crate::tunnel_stack::TunnelStack>) -> Self;
    pub fn bind_addr(&self) -> &TunnelBridgeBind;
    pub async fn shutdown(self, drain_timeout: std::time::Duration);
}
```

The listener must accept only UDS or loopback TCP. For TCP, reject non-loopback peers at accept time. Use `hyper::server::conn::http1::Builder::new().serve_connection(...)` with `hyper_util::rt::TokioIo`, matching the egress loopback pattern.

- [ ] **Step 4: Implement request relay semantics**

The bridge handler must:

1. Read `X-Gateway-Tunnel-Node-Id`, `X-Gateway-Tunnel-Target`, and `X-Gateway-Tunnel-Target-Tls`.
2. Return `502` with `X-Tunnel-Error-Class: missing_header` if any required tunnel header is missing.
3. Resolve the node's current `WsTransport` from `TunnelStack`.
4. Call `open_stream(target)` with the confirmed Open payload format.
5. Strip `X-Gateway-Tunnel-*` before forwarding.
6. Send an HTTP/1.1 request over the tunnel stream.
7. If `X-Gateway-Tunnel-Target-Tls` is `true`, wrap the tunnel stream in TLS before sending HTTP if the user confirmed gateway-owned TLS.
8. Parse the HTTP/1.1 response and return it to Pingora.
9. Send `Close(stream_id)` after request body EOF.
10. Map `StreamError::RemoteError` to `502` with `X-Tunnel-Error-Class: remote_error`.
11. Map `StreamError::NotAlive` and dropped receive channel to `502` with `X-Tunnel-Error-Class: no_live`.

For the mock-confirmed first implementation, the e2e target can be plaintext HTTP and `X-Gateway-Tunnel-Target-Tls=false`; HTTPS behavior must still be represented in unit tests if the user confirms gateway-owned TLS.

- [ ] **Step 5: Run bridge tests**

Run:

```bash
cargo test -p gw-proxy tunnel_bridge --features tunnel
```

Expected: PASS.

## Task W8.4: Wire Tunnel Selection To The Bridge Peer

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: `crates/gw-proxy/src/tunnel_stack.rs`
- Test: `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: Add context fields**

In `ReqCtx`, add:

```rust
#[cfg(feature = "tunnel")]
pub tunnel_node_id: Option<String>,
#[cfg(feature = "tunnel")]
pub tunnel_target: Option<(String, u16, bool)>,
```

Initialize both to `None` in `ReqCtx::new`.

Run:

```bash
cargo test -p gw-proxy --features tunnel tunnel_node_id
```

Expected: compile succeeds after constructor updates, or no matching tests run if no field-specific test exists yet.

- [ ] **Step 2: Replace remote-WS peer construction with bridge peer construction**

In `TunnelStack`, replace the production behavior of `peer_for_node` with a method that returns a bridge peer. Keep DNS and target filtering before returning:

```rust
pub async fn bridge_peer_for_node(
    &self,
    node_id: &str,
    host: &str,
    port: u16,
    scheme_tls: bool,
) -> Result<HttpPeer, TunnelPeerError>
```

The method must:

- run `self.dns.resolve(host).await`;
- take one `nodes.load_full()` snapshot;
- lazily start `ReconnectingTransport`;
- require `transport.current().is_some()`;
- run `assert_target_allowed` against `https://host:port` or `http://host:port` based on `scheme_tls`;
- return `HttpPeer::new_uds(path, false, "tunnel.local")` for UDS bridge binds, or `HttpPeer::new(addr.to_string(), false, "tunnel.local")` for TCP bridge binds.

Keep `TunnelPeerError::{NoLive, DnsBlocked, TargetDenied}` unchanged so `upstream.rs` error mapping remains stable.

- [ ] **Step 3: Update `pick_peer` tunnel branch**

In `crates/gw-proxy/src/filters/upstream.rs`, change the `TunnelResolution::Use(node_id)` branch so success does this:

```rust
let p = t.bridge_peer_for_node(&node_id, &host, port, scheme_tls).await?;
ctx.tunnel_resolution = Some(node_id.clone());
ctx.tunnel_node_id = Some(node_id);
ctx.tunnel_target = Some((host.clone(), port, scheme_tls));
peer = p;
```

Keep existing error mapping to `GW_TUNNEL_NO_LIVE`, `GW_TUNNEL_DNS_BLOCKED`, and `GW_TUNNEL_TARGET_DENIED`.

- [ ] **Step 4: Inject tunnel bridge headers after adapter rewrite**

Add a helper near `inject_egress_headers`:

```rust
#[cfg(feature = "tunnel")]
fn inject_tunnel_headers(ctx: &ReqCtx, upstream_request: &mut RequestHeader) {
    let (Some(node_id), Some((host, port, tls))) =
        (ctx.tunnel_node_id.as_ref(), ctx.tunnel_target.as_ref())
    else {
        return;
    };
    let _ = upstream_request.insert_header(crate::tunnel_bridge::HDR_TUNNEL_NODE_ID, node_id);
    let _ = upstream_request.insert_header(
        crate::tunnel_bridge::HDR_TUNNEL_TARGET,
        format!("{host}:{port}"),
    );
    let _ = upstream_request.insert_header(
        crate::tunnel_bridge::HDR_TUNNEL_TARGET_TLS,
        if *tls { "true" } else { "false" },
    );
}
```

Call it in both non-CLI and CLI paths immediately after `inject_egress_headers(...)`.

- [ ] **Step 5: Redact tunnel headers from detailed archives**

In `crates/gw-proxy/src/gateway.rs`, update `redact_egress_headers` into a general internal-header redactor or add a second tunnel redaction pass:

```rust
#[cfg(feature = "tunnel")]
if lower == "x-gateway-tunnel-node-id"
    || lower == "x-gateway-tunnel-target"
    || lower == "x-gateway-tunnel-target-tls"
{
    return false;
}
```

Run:

```bash
cargo test -p gw-proxy --features tunnel tunnel
```

Expected: PASS. Existing no-tunnel tests must still pass with:

```bash
cargo test -p gw-proxy tunnel
```

Expected: PASS or zero matching tests; no compile error in default features.

## Task W8.5: Add Live Tunnel Round-Trip E2E

**Files:**
- Modify: `crates/gw-gateway/tests/e2e_tunnel.rs`
- Modify only if needed: `crates/e2e-fake-upstream/src/tunnel_node.rs`
- Reference: `crates/gw-gateway/tests/e2e_impersonate.rs`
- Reference: `crates/e2e-fake-upstream/src/server.rs`

- [ ] **Step 1: Add an ignored live round-trip test**

Add an ignored Tokio test named `e2e_tunnel_round_trip_reaches_fake_upstream_through_ws_node` to `crates/gw-gateway/tests/e2e_tunnel.rs` with ignore reason `requires Docker (testcontainers Redis + Postgres) and live tunnel round-trip fixtures`.

The test body must execute these concrete steps:

1. Boot Postgres and Redis with `testcontainers_modules::{postgres::Postgres, redis::Redis}`.
2. Apply the migration chain needed for users, providers, vendors, vendor keys, model groups, model mappings, and proxy nodes. Include `MIGRATION_0005` for `proxy_nodes`.
3. Start a plaintext fake upstream on `127.0.0.1:0` that returns status `200`, content type `application/json`, and body `{"id":"tun-ok","choices":[{"message":{"role":"assistant","content":"via tunnel"}}]}`.
4. Start `e2e_fake_upstream::tunnel_node::run` on `127.0.0.1:0`.
5. Seed one `proxy_nodes` row with `id = "node-e2e"`, `ws_url` pointing at the tunnel node, `enabled = true`, `allowed_provider_ids = ["openai"]`, `allowed_vendor_key_ids = []`, and `auth_token_enc = null`.
6. Seed one OpenAI vendor whose `base_urls[0]` points at the plaintext fake upstream.
7. Seed one active vendor key with `tunnel_preference.require_tunnel = true` and `accessible_endpoints = ["open_ai"]`.
8. Set the tunnel target whitelist to the fake upstream `host:port`.
9. Boot the gateway with `--features tunnel`, `[tunnel].enabled = true`, and the seeded Redis/Postgres URLs.
10. Send one OpenAI chat request through the business port.
11. Assert response status is `200`.
12. Assert response body contains `via tunnel`.
13. Assert the fake upstream recorder captured exactly one request.
14. Assert the captured upstream request does not include `X-Gateway-Tunnel-Node-Id`, `X-Gateway-Tunnel-Target`, or `X-Gateway-Tunnel-Target-Tls`.

Run:

```bash
cargo test -p gw-gateway --features tunnel --test e2e_tunnel e2e_tunnel_round_trip_reaches_fake_upstream_through_ws_node -- --ignored --nocapture
```

Expected before source wiring is complete: FAIL with a tunnel bridge or gateway route error. Expected after W8.2-W8.4: PASS with a 200 response and one fake-upstream hit.

- [ ] **Step 2: Add negative e2e cases**

Add `e2e_tunnel_require_true_fails_when_no_live_node` with this behavior:

- seed a tunnel-required vendor key;
- seed a matching `proxy_nodes` row whose `ws_url` points to `ws://127.0.0.1:9/`;
- do not start a node on that port;
- send one OpenAI chat request;
- assert the gateway response carries stable code `GW_TUNNEL_NO_LIVE`;
- assert the fake upstream recorder count is zero.

Add `e2e_tunnel_target_denied_does_not_reach_node` with this behavior:

- start the fake upstream and tunnel node;
- seed the matching `proxy_nodes` row and tunnel-required vendor key;
- leave `tunnel.target_whitelist` empty;
- send one OpenAI chat request;
- assert the gateway response carries stable code `GW_TUNNEL_TARGET_DENIED`;
- assert the fake upstream recorder count is zero.

Both tests must use `#[tokio::test]` and the ignore reason `requires Docker (testcontainers Redis + Postgres)`.

Run:

```bash
cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored --nocapture
```

Expected: PASS after W8 source wiring.

## Task W8.6: Update Specs, Workflow, Roadmap, And Artifact Decision Packet

**Files:**
- Modify: `docs/spec/09-tunnel.md`
- Modify: `docs/workflow.md`
- Modify: `docs/architecture.md`
- Modify: `docs/ROADMAP.md`
- Do not modify unless user approves release change: `.gitlab-ci.yml`
- Do not modify unless user approves release change: `Dockerfile`

- [ ] **Step 1: Update `docs/spec/09-tunnel.md` as as-built**

Change status from "production HTTP<->WS frame stream bridge is not built" to the verified bridge behavior. Include these facts:

- tunnel path uses an in-process HTTP/1 loopback bridge;
- Pingora still owns standard request/response filter execution;
- bridge opens `WsTransport::open_stream` to the selected node;
- `Open` payload format matches the confirmed user decision;
- `Data` chunks are bounded by `MAX_PAYLOAD_LEN`;
- `Close` maps EOF;
- `Error` maps to the documented gateway error surface;
- `ControlAuth`, `ControlNodeInfo`, and multiplex behavior match the confirmed user decision;
- default artifact remains non-tunnel.

Run:

```bash
bash scripts/check-spec.sh docs/spec/09-tunnel.md
```

Expected: PASS.

- [ ] **Step 2: Update `docs/workflow.md`**

Replace the tunnel peer description that says `peer_for_node` points at the WS endpoint. State that the selected tunnel branch points Pingora at the loopback bridge and passes target metadata through redacted `X-Gateway-Tunnel-*` headers.

Run:

```bash
rg -n "WS endpoint|loopback bridge|X-Gateway-Tunnel" docs/workflow.md
```

Expected: old "socket lands at the node's WS endpoint" wording is absent or explicitly marked historical, and new loopback bridge wording is present.

- [ ] **Step 3: Update `docs/architecture.md`**

In the workspace layout and deliberate behavior sections, add the bridge as part of the tunnel feature and keep the split artifact behavior:

```text
Tunnel bridge is compiled only with `--features tunnel` and is activated only when `[tunnel].enabled = true`.
Default CI artifact remains non-tunnel; `latest-tunnel` remains the operator-selected tunnel artifact.
```

Run:

```bash
rg -n "latest-tunnel|Default CI artifact remains non-tunnel|Tunnel bridge" docs/architecture.md
```

Expected: all three concepts are present.

- [ ] **Step 4: Update `docs/ROADMAP.md`**

Remove or mark closed the W8 residuals only after source verification passes:

- HTTP<->WS frame bridge runtime;
- full tunnel e2e round-trip;
- default artifact policy.

For artifact policy, do not say default promotion happened. Say:

```text
W8 bridge and live e2e are closed. Default artifact remains non-tunnel pending an explicit user release decision; `latest-tunnel` remains the tunnel-enabled operator artifact.
```

Run:

```bash
rg -n "HTTP<->WS frame bridge runtime|Full tunnel e2e round-trip|Default artifact policy|latest-tunnel" docs/ROADMAP.md docs/spec/09-tunnel.md
```

Expected: W8 runtime/e2e are no longer listed as unbuilt residuals after implementation, and artifact policy still names the user release decision.

- [ ] **Step 5: Prepare the post-e2e release decision packet**

After the live e2e is green, return this decision packet to the user:

```text
User Decision Needed: tunnel default artifact after W8 e2e

Verified:
- cargo test -p gw-tunnel-client
- cargo test -p gw-proxy --features tunnel tunnel
- cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored
- cargo build --release --locked --features tunnel -p gw-gateway
- bash scripts/check-spec.sh docs/spec/09-tunnel.md

Current artifact policy:
- default `latest` is non-tunnel.
- `latest-tunnel` is tunnel-enabled with `--features tunnel`.

Options:
1. Keep split artifacts. `latest` stays non-tunnel and operators opt in with `latest-tunnel`.
2. Promote tunnel into default `latest` while keeping `[tunnel].enabled=false` by default at runtime.

Recommended option:
Option 1 for the first production cycle after W8 because it preserves release blast radius while the new bridge is observed under operator-selected deployments.
```

Expected: no `.gitlab-ci.yml` or `Dockerfile` change unless the user explicitly chooses Option 2.

## Task W8.7: Final Verification And Commits For Source Execution

**Files:**
- All files touched by W8 source execution.

- [ ] **Step 1: Run focused tunnel verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p gw-tunnel-client
cargo test -p gw-proxy --features tunnel tunnel
cargo test -p gw-gateway --features tunnel --test e2e_tunnel -- --ignored --nocapture
cargo build --release --locked --features tunnel -p gw-gateway
bash scripts/check-spec.sh docs/spec/09-tunnel.md
```

Expected: PASS.

- [ ] **Step 2: Run repository gates**

Run:

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
bash scripts/check-invariants.sh
for f in docs/spec/0*.md; do bash scripts/check-spec.sh "$f" || exit 1; done
git diff --check
```

Expected: PASS.

- [ ] **Step 3: Commit bridge runtime**

```bash
git add crates/gw-tunnel-client crates/gw-proxy crates/gw-gateway crates/e2e-fake-upstream
git commit -m "feat(tunnel): bridge http streams to ws frames"
```

- [ ] **Step 4: Commit live e2e**

```bash
git add crates/gw-gateway/tests/e2e_tunnel.rs crates/e2e-fake-upstream/src/tunnel_node.rs
git commit -m "test(tunnel): add live tunnel roundtrip e2e"
```

- [ ] **Step 5: Commit docs and artifact policy**

```bash
git add docs/spec/09-tunnel.md docs/workflow.md docs/architecture.md docs/ROADMAP.md
git commit -m "docs(tunnel): close bridge and artifact policy residuals"
```

## Planner Verification For This Docs-Only Plan

Run after editing this plan:

```bash
git diff --check
```

Expected: PASS.

Commit this plan with:

```bash
git add docs/plans/2026-06-09-w8-tunnel-bridge-e2e.md
git commit -m "docs(plan): detail tunnel bridge e2e"
```
