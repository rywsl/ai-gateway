# Cnexus SOCKS5 Node Runtime Design

## Goal

Make old Aether-era SOCKS5 links directly usable in the new Cnexus proxy-node management surface. An operator must be able to paste `socks5://...` or `socks5h://...` into the new proxy-node UI/API and have Rust gateway traffic selected for that node actually egress through that SOCKS5 service.

## Scope

This is a complete feature across Rust, Go, and frontend:

- Rust gateway consumes `proxy_nodes.payload.proxy_url` at runtime.
- Go API/business persists and validates `protocol`, `ws_url`, and `proxy_url` without manufacturing dead WebSocket endpoints.
- Frontend shows the system proxy-node page and exposes the fields needed to create both Cnexus WebSocket tunnel nodes and SOCKS5 compatibility nodes.

This does not implement the old Aether tunnel wire protocol. Compatibility is at the SOCKS5 URL layer.

## Runtime Model

Proxy nodes have two Cnexus-compatible backends:

- `cnexus_ws`: a first-party `gw-tunnel-server` node reached through `ws_url`.
- `socks5`: a direct SOCKS5/SOCKS5h node reached through `proxy_url`.

Both backends participate in the same vendor-key tunnel selection flow. `TunnelPreference.preferred_node_ids`, provider allowlists, vendor-key allowlists, `require_tunnel`, and low-latency selection continue to apply.

When a selected node has a SOCKS5 `proxy_url`, Rust returns a local loopback peer and injects the same `X-Gateway-Tunnel-*` metadata already used by the Cnexus bridge. The bridge resolves the selected node backend and dials the upstream through SOCKS5. For HTTPS upstreams, the bridge performs TLS over the SOCKS5 TCP stream, matching the existing WebSocket tunnel bridge behavior.

## Rust Changes

`gw-core::entity::ProxyNode` gains `proxy_url: Option<url::Url>`. Missing values continue to deserialize, so existing rows are compatible.

`gw-tunnel-client::resolver::ProxyNodeView` gains `proxy_url` and a derived backend classification:

- `backend = CnexusWs` when `ws_url` is present and valid.
- `backend = Socks5` when `proxy_url` scheme is `socks5` or `socks5h`.
- Other schemes are not eligible for runtime selection.

`TunnelStack` keeps the current resolver API but filters non-runnable nodes out before resolution. It validates DNS and target whitelist for both backend types. For Cnexus WebSocket nodes it keeps the existing `ReconnectingTransport` flow. For SOCKS5 nodes it does not require a live WebSocket transport; it returns the same bridge peer after validating the node has a supported SOCKS5 URL.

`tunnel_bridge` is generalized so `transport_for_node` returns a backend handle. For `CnexusWs`, it opens a `WsTransport` stream as today. For `Socks5`, it opens a TCP stream through `tokio-socks` using the node `proxy_url`, then relays the HTTP request bytes exactly like the existing tunnel bridge. Internal tunnel headers are stripped in both cases.

The refresh signature in `gw-gateway` includes `ws_url`, `proxy_url`, `enabled`, `latency_ms`, allowlists, `tunnel_protocol`, and encrypted auth fields so edits in Go become effective without gateway restart.

## Go Changes

The Go `proxynode` model exposes `protocol`, `ws_url`, and `proxy_url` in DTOs and manual create/update requests.

Normalization rules:

- `protocol` defaults to `cnexus`.
- `tunnel_protocol` mirrors `protocol`.
- A `socks5://` or `socks5h://` `proxy_url` is a runnable node even when `ws_url` is empty.
- `http://` and `https://` proxy URLs remain storable/testable for existing UI compatibility but are not marked runtime-enabled for tunnel selection.
- No new row may get `ws://127.0.0.1:9/tunnel` as a fake runtime endpoint.

Go validation rejects unknown protocols and clearly separates WebSocket URL validation from SOCKS5 URL validation.

## Frontend Changes

The system menu must show the proxy-node page.

The proxy-node table displays protocol, backend type, WebSocket endpoint, and proxy URL. The create/edit dialog supports:

- Protocol selection.
- WebSocket endpoint for Cnexus tunnel-server nodes.
- SOCKS5/SOCKS5h proxy URL for old Aether SOCKS5 compatibility nodes.

The form must allow a SOCKS5 node with no `ws_url`. It must not require `proxy_url` for a pure WebSocket tunnel node. The submit payload includes `protocol`, `ws_url`, and `proxy_url`.

## Error Handling

Unsupported node URLs are excluded from resolver selection instead of silently falling back to Direct when `require_tunnel=true`. If all preferred nodes are unsupported, the existing `GW_TUNNEL_NO_NODE` behavior applies.

If a selected SOCKS5 proxy cannot connect or authenticate, the bridge returns `502` and the gateway reports a tunnel-open failure path. This is equivalent to a WebSocket tunnel node being unavailable from the caller's perspective.

## Tests

Rust tests cover:

- `ProxyNode` deserializes `proxy_url`.
- Resolver/TunnelStack treats SOCKS5 nodes as eligible.
- `peer_for_node` returns a bridge peer for SOCKS5 nodes without requiring a WebSocket transport.
- Tunnel bridge can relay through a SOCKS5 backend and strips internal headers.
- Gateway refresh signature changes when `proxy_url`, `ws_url`, allowlists, protocol, or auth changes.

Go tests cover:

- Manual SOCKS5 nodes can be created with no `ws_url`.
- Manual WebSocket nodes can be created with no `proxy_url`.
- `NormalizeContract` does not write fake WebSocket endpoints.
- Menu metadata puts proxy nodes under System.

Frontend tests/build cover:

- API types include `protocol`, `ws_url`, and `proxy_url`.
- Create/edit payloads include the selected backend fields.
- Proxy-node route is visible under the system menu.
