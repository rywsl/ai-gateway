# 10 — Owned Tunnel Protocol

- Family: tunnel · Backing crates: `gw-tunnel-client`, `gw-tunnel-server` · Protocol name: `ailinkmax-tunnel` · Status: first-party contract for W8.

## Purpose

This protocol is owned by this repository. Prior aether and mock tunnel behavior is reference material only; W8 does not target aether compatibility. The gateway client and `gw-tunnel-server` are the compatibility pair.

The tunnel carries opaque TCP bytes for an already-selected upstream target. The gateway owns HTTP rendering and TLS to HTTPS vendors; the tunnel server only connects TCP to `host:port` and relays bytes.

## As-built surface

- Frame codec — `crates/gw-tunnel-client/src/frame.rs`: fixed 14-byte header, `MsgType`, `MAX_PAYLOAD_LEN`, `encode_frame`, and `decode_frame`.
- Client transport — `crates/gw-tunnel-client/src/transport.rs`: WebSocket handshake, optional auth-first frame, heartbeat/pong tracking, bidirectional stream routing, `TunnelStream`, chunked `Data`, `Close`, and `Error` propagation.
- Server — `crates/gw-tunnel-server/src/lib.rs`: WebSocket acceptor, optional `ControlAuth`, `ControlNodeInfo`, target validation, TCP relay, stream cap, and test `spawn` API.
- Proxy bridge — `crates/gw-proxy/src/tunnel_bridge.rs`: loopback HTTP/1 bridge, internal header stripping, tunnel stream open, HTTP byte relay, and tunnel error mapping.
- Proxy selection — `crates/gw-proxy/src/tunnel_stack.rs` and `crates/gw-proxy/src/filters/upstream.rs`: selected node/target metadata, bridge `HttpPeer`, DNS/target/live checks, and `GW_TUNNEL_*` failure mapping.

## Frame Format

Frames use the existing fixed header from `gw-tunnel-client/src/frame.rs`:

```text
magic(2) | version(1) | msg_type(1) | flags(2) | stream_id(4) | length(4) | payload(length)
```

- `magic`: `0xAE 0x21`
- `version`: `1`
- `flags`: reserved, currently `0`
- `stream_id`: client-assigned `u32`; stream `0` is reserved for control frames.
- `length`: payload byte length.
- `MAX_PAYLOAD_LEN`: 4 MiB. Any larger write is chunked into multiple `Data` frames before send; any frame declaring a larger payload is rejected.

## Message Types

| Type | Value | Payload |
| --- | ---: | --- |
| `Open` | `0x01` | UTF-8 `host:port` target. |
| `Data` | `0x02` | Opaque bytes for `stream_id`. |
| `Close` | `0x03` | Empty payload; EOF for `stream_id`. |
| `Ping` | `0x04` | Heartbeat payload echoed by `Pong`. |
| `Pong` | `0x05` | Heartbeat response. |
| `Error` | `0x06` | UTF-8 diagnostic text. |
| `ControlAuth` | `0x10` | UTF-8 bearer token on stream `0`. |
| `ControlNodeInfo` | `0x11` | JSON node metadata on stream `0`. |

## Handshake

If the configured proxy node has an auth token, the client sends `ControlAuth` as the first WebSocket binary frame. The server accepts only an exact token match. On mismatch or missing auth, the server sends `Error` on stream `0` and closes the WebSocket.

After successful auth, or immediately when no auth is configured, the server sends `ControlNodeInfo` on stream `0`:

```json
{
  "protocol": "ailinkmax-tunnel",
  "version": "1.0.0",
  "node_id": "node-id",
  "max_streams_per_conn": 256,
  "capabilities": ["tcp-relay", "chunked-data", "target-filter"]
}
```

Clients must ignore unknown fields and may ignore the frame entirely for forward compatibility.

## Streams

The client opens streams with monotonically increasing `u32` ids starting at `1`. Interleaved `Data` frames for different stream ids are valid. Both client and server enforce `max_streams_per_conn`; excess `Open` frames receive `Error` and do not create a TCP connection.

`Close` is half-close/EOF for the stream. Either side may send it after forwarding buffered bytes. Stream state is released after `Close`, TCP EOF, `Error`, or WebSocket close.

## Target Safety

The gateway validates DNS and target whitelist before routing to the loopback bridge. The owned server also applies target validation before TCP connect. Server-side validation rejects malformed targets, deny-listed host/port pairs, and blocked/private IP literals. A rejected target returns `Error` on that stream and never dials TCP.

## Bridge Semantics

Pingora never points an upstream HTTP peer at the node WebSocket endpoint. Tunnel-selected traffic goes to an in-process loopback HTTP/1 bridge. The bridge strips `X-Gateway-Tunnel-*` internal headers, opens an `ailinkmax-tunnel` stream to the selected node, sends HTTP/1 bytes as `Data`, and forwards inbound `Data` as the upstream response. Tunnel `Error`, no live node, denied target, or premature `Close` map to gateway-visible upstream errors.

## Not built

- Compatibility with external aether nodes is not built and is not a W8 target.
- Default `latest` artifact promotion is not built into this protocol decision; `latest-tunnel` remains the operator-selected tunnel artifact until an explicit release decision.
