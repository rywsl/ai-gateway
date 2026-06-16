# Cnexus SOCKS5 Node Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make SOCKS5/SOCKS5h proxy-node links pasted into the Go admin UI directly usable by the Rust gateway runtime.

**Architecture:** Extend the proxy-node contract with `proxy_url` and classify each node as Cnexus WebSocket or SOCKS5 backend. Keep Pingora pointed at the existing local tunnel bridge, and let the bridge open either a Cnexus stream or a SOCKS5 TCP stream before relaying HTTP bytes.

**Tech Stack:** Rust 1.88 workspace, Pingora, tokio-socks, Go proxynode module, Vue 3 admin frontend.

---

### Task 1: Rust Contract And Resolver

**Files:**
- Modify: `crates/gw-core/src/entity.rs`
- Modify: `crates/gw-tunnel-client/src/resolver.rs`
- Test: `crates/gw-core/tests/proxy_node_tunnel_protocol.rs`
- Test: `crates/gw-tunnel-client/tests/resolver.rs`

- [ ] Add failing Rust tests proving `ProxyNode.proxy_url` deserializes and SOCKS5 nodes are resolver-eligible.
- [ ] Add `proxy_url: Option<url::Url>` to `ProxyNode`.
- [ ] Add `proxy_url` and backend helpers to `ProxyNodeView`.
- [ ] Filter unsupported proxy URLs out of resolution.
- [ ] Run targeted tests.

### Task 2: Rust SOCKS5 Bridge Runtime

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/gw-proxy/Cargo.toml`
- Modify: `crates/gw-proxy/src/tunnel_bridge.rs`
- Modify: `crates/gw-proxy/src/tunnel_stack.rs`
- Test: `crates/gw-proxy/src/tunnel_bridge.rs`
- Test: `crates/gw-proxy/src/tunnel_stack.rs`

- [ ] Add failing tests showing a SOCKS5 node returns a bridge peer without live WebSocket transport.
- [ ] Add failing bridge test that relays through a SOCKS5 backend.
- [ ] Move `tokio-socks` to workspace dependency and add it to `gw-proxy`.
- [ ] Generalize bridge backend from WebSocket-only to WebSocket-or-SOCKS5.
- [ ] Keep DNS and target whitelist checks for both backends.
- [ ] Run targeted gw-proxy tests.

### Task 3: Rust Gateway Mapping And Refresh

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Test: add or extend `crates/gw-gateway/src/lib.rs` unit tests if feasible, otherwise add source-level focused tests around signature helper.

- [ ] Add failing test proving refresh signature changes when `proxy_url` changes.
- [ ] Map `ProxyNode.proxy_url` into `ProxyNodeView`.
- [ ] Include all routing-relevant fields in node refresh signature.
- [ ] Run targeted gw-gateway check/test.

### Task 4: Go Proxynode Contract And Menu

**Files:**
- Modify: `AILinkMax-Go/modules/proxynode/business/entity.go`
- Modify: `AILinkMax-Go/modules/proxynode/business/service.go`
- Modify: `AILinkMax-Go/modules/proxynode/api/handlers_test.go`
- Modify: `AILinkMax-Go/modules/proxynode/business/service_test.go`
- Modify: `AILinkMax-Go/modules/system/business/modules.go`

- [ ] Add failing tests for SOCKS5 manual node with empty `ws_url` and WebSocket node with empty `proxy_url`.
- [ ] Add DTO/request `ws_url` support.
- [ ] Remove fake WebSocket endpoint normalization.
- [ ] Make SOCKS5/SOCKS5h nodes runtime-enabled when active.
- [ ] Move proxy-node module menu group to System.
- [ ] Run targeted Go proxynode/system tests.

### Task 5: Frontend Admin UI/API

**Files:**
- Modify: `AILinkMax-Go/frontend/src/api/proxy-nodes.ts`
- Modify: `AILinkMax-Go/frontend/src/views/admin/ProxyNodes.vue`
- Optional test files depending on existing frontend test layout.

- [ ] Add `protocol` and `ws_url` to API types and create/update payloads.
- [ ] Add table/backend display fields.
- [ ] Update create/edit form so either `ws_url` or SOCKS5/SOCKS5h `proxy_url` can satisfy submit validation.
- [ ] Submit `protocol`, `ws_url`, and `proxy_url`.
- [ ] Run frontend typecheck/build or targeted test available in repo.

### Task 6: Verification And Review

**Files:**
- Modify docs if implementation details differ from the spec.

- [ ] Run Rust format/check and targeted tests.
- [ ] Run Go format/tests.
- [ ] Run frontend validation.
- [ ] Inspect git diffs for unrelated user changes.
