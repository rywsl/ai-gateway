# T — Tunnel 接入与生效路径打通(v2026-05-22)

- **日期**：2026-05-22
- **状态**：Draft(待审阅,契约 + 边界 + 必要数据结构,实现拆 T1..T6)
- **作者**：协作设计(Claude + 用户)
- **触发**：[`docs/issues/2026-05-22-dep-feature-audit.md`](../../issues/2026-05-22-dep-feature-audit.md) §1 / §5。
  - `gw-gateway/Cargo.toml:18` 声明 `tunnel = ["dep:gw-tunnel-client"]`,但 `gw-tunnel-client` 在 `gw-proxy`、`gw-routing`、`gw-gateway::run` 中无任何 `use` / 调用——`pub use gw_tunnel_client` 是唯一引用。
  - `CLAUDE.md:30` 对外宣称 "Tunnel: optional WebSocket transport to remote nodes (`--features tunnel`)",而数据路径上 tunnel 永远不生效。
  - `hickory-resolver 0.24` 依赖 `rustls 0.21`,与 `pingora 0.8` 链入的 `rustls 0.23` 在 `Cargo.lock` 中并存,目前仅因 `gw-tunnel-client` 不入二进制而无副作用;一旦 tunnel 真正接入,二进制内会同时存在两个 `rustls`。
- **关联**：
  - [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md)(SelectorAny::pick 返回 SmallVec)
  - [2026-05-19-routing-r-e-failover.md](./2026-05-19-routing-r-e-failover.md)(execution-layer failover state)
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)(per-(key, format) 健康)
- **范围**：在 `gw-proxy::filters::upstream::pick_peer` 与 `LlmGateway::upstream_peer` 之间植入 tunnel 决策点,把现有 `gw-tunnel-client`(`frame` + `transport` + `pool` + `resolver` + `safe_dns` + `target_filter`)接到数据路径上,并补齐配置 / 持久化 / 遥测三个支撑面。
- **不在范围**：
  - 隧道节点的服务端(aether-hub);本 spec 只覆盖网关侧的客户端接入。
  - 反向隧道 / 节点间网状互联;仅 gateway → node 单向 WS。
  - 替换 hickory 为其它 DoH 实现(本 spec 仅描述兼容性策略,选型见 ADR-T-6)。

> 一句话定义:把 `--features tunnel` 从"链接进来但 dead code"变成"在 `upstream_peer` 真正生效",并把对外文档的"已就绪"改成可被运维校验的事实。

---

## 1. 现状盘点

### 1.1 已有(`crates/gw-tunnel-client/src/`)

| 模块 | 文件 | 公开类型 / 函数 | 接入状态 |
|------|------|-----------------|----------|
| 帧编解码 | `frame.rs` | `Frame { msg_type, flags, stream_id, payload }`、`MsgType` 8 种、`encode_frame` / `decode_frame` | 完整,有 magic `0xAE21` |
| WS 传输 | `transport.rs` | `WsTransport::connect / send_open / send_data / is_alive / last_pong_age` | 完整,带心跳 + missed-pong 阈值 |
| 重连池 | `pool.rs` | `ReconnectingTransport::start / current`、`PoolConfig`、`jittered_backoff` | 完整,`tokio::spawn` 监督循环 |
| 节点选择 | `resolver.rs` | `resolve_tunnel(ctx, &[ProxyNodeView]) -> TunnelResolution { Use(id) \| Direct }` | 完整,按 latency_ms 排序 |
| DoH | `safe_dns.rs` | `SafeDnsResolver::cloudflare_default / resolve`、`is_ip_blocked`、`set_override` | 完整,LRU 缓存 + 黑名单后过滤 |
| 目标白名单 | `target_filter.rs` | `set_whitelist / assert_target_allowed` | 完整,全局 ArcSwap |

### 1.2 缺口

| 缺口 | 影响 |
|------|------|
| `gw-proxy::filters::upstream::pick_peer` 没有任何 tunnel 分支 | 选 key 之后直接 `HttpPeer::new(addr, true, host)`,永远走 Direct |
| `LlmGateway` 字段表中没有 tunnel 相关状态(`Arc<ReconnectingTransport>` 池 / proxy node 视图 / target_filter 初始化) | 即使把 `gw-tunnel-client` use 进来,运行时也没有可消费的 transport 实例 |
| `EntityStore` 没有 `ProxyNode` 实体 | `resolve_tunnel` 需要 `&[ProxyNodeView]`,目前没有持久化来源 |
| `VendorKey` 没有 tunnel 偏好字段 | 无法按 key / provider 维度开关 tunnel |
| `config::Config` 没有 `[tunnel]` 表 | DoH provider / 默认白名单 / 池大小无配置项 |
| 遥测无 tunnel 维度 | `gw_routing_pick_total` 不能区分 Direct vs Tunnel,运维不可观测 |
| `hickory-resolver 0.24` 与 `rustls 0.23` 双链 | tunnel 接入后,二进制同时链接两份 rustls,可能引发 `aws-lc-rs` / `ring` provider 冲突 |

---

## 2. 设计决策

### ADR-T-1 接入点 = `pick_peer` 末段,在生成 `HttpPeer` 之前

`gw-proxy/src/filters/upstream.rs::pick_peer` 选出 `VendorKey` → 算 `(host, port)` → `HttpPeer::new(addr, true, host)`。在最后一步前插入 tunnel 决策:

```rust
// 既有
let (host, port) = resolve_host_port(&vendor_key, ctx)?;

// 新增
let resolution = self.tunnel.resolve(provider_id, &vendor_key.id, &ctx.tunnel_pref);
let mut peer = match resolution {
    TunnelResolution::Direct => HttpPeer::new((host.as_str(), port), true, host.clone()),
    TunnelResolution::Use(node_id) => self.tunnel.peer_for_node(&node_id, &host, port)?,
};
// 既有(set_alpn / sni / verify_cert / ...)
```

`peer_for_node` 返回 `HttpPeer`,其底层 socket 经由 `ReconnectingTransport::send_open` 在远端节点出口。

**关键不变量**:
- tunnel 决策**只在 `upstream_peer` 内部发生**,不污染 `request_filter` / `early_request_filter`;`ctx.tunnel_resolution: Option<String>` 仅用于遥测与 failover 状态。
- failover(R-E)blacklist 用 `KeyId`,与 tunnel 选择正交;同一个 key 在 retry 时可能 Direct → Tunnel,但反之不允许(避免环路)。

### ADR-T-2 `TunnelStack` 作为 `LlmGateway` 的可选字段

```rust
// crates/gw-proxy/src/tunnel_stack.rs(新增)
pub struct TunnelStack {
    /// node_id -> 重连传输
    transports: dashmap::DashMap<String, Arc<ReconnectingTransport>>,
    /// 当前可用的代理节点视图(EntityStore 提供 cold-load + 失效订阅)
    nodes: arc_swap::ArcSwap<Vec<ProxyNodeView>>,
    /// DoH 解析器(SafeDnsResolver)
    dns: Arc<SafeDnsResolver>,
}

impl TunnelStack {
    pub fn resolve(&self, provider_id: &str, key_id: &str, pref: &TunnelPreference) -> TunnelResolution;
    pub fn peer_for_node(&self, node_id: &str, host: &str, port: u16) -> pingora::Result<HttpPeer>;
}
```

`LlmGateway::new` 新增可选参数 `tunnel: Option<Arc<TunnelStack>>`;`None` 时 `pick_peer` 等价于现有行为(`TunnelResolution::Direct`),保持向后行为兼容。**`Option` 不是灰度开关**,而是反映"是否启用 tunnel 子系统"——参考 memory 的 breaking-changes-OK 立场,生产里要么开要么关,不留中间态。

### ADR-T-3 `ProxyNode` 实体进入 `gw-store`

新增持久化实体:

```sql
-- crates/gw-store/migrations/0004_proxy_nodes.sql
CREATE TABLE proxy_nodes (
    id TEXT PRIMARY KEY,
    ws_url TEXT NOT NULL,                  -- ws[s]://host:port/path
    enabled BOOLEAN NOT NULL DEFAULT true,
    latency_ms INTEGER NOT NULL DEFAULT 0, -- 健康探测覆盖
    allowed_provider_ids JSONB NOT NULL,   -- []
    allowed_vendor_key_ids JSONB NOT NULL, -- [];空表示不限 key
    auth_token_enc BYTEA,                  -- KMS 加密后的节点鉴权 token,可空
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_proxy_nodes_enabled ON proxy_nodes(enabled) WHERE enabled = true;
```

`EntityStore::proxy_node_or_load(id)` 走 L1(moka) / L2(redis `entity:proxy_node:*`) / L3(PG) 三层,与现有 vendor_key 加载路径同形;失效订阅复用 `events:entity`(`namespace=proxy_node`)。`auth_token_enc` 由 `LoaderCtx.kek` 解密,与 `vendor_key.api_key_enc` 同套机制。

### ADR-T-4 `VendorKey` 增字段 `tunnel_preference: Option<TunnelPreference>`

```rust
// crates/gw-core/src/entity/vendor_key.rs
pub struct TunnelPreference {
    /// None / Empty -> 由 ProxyNode 列表自决;Some(ids) -> 限定优先级
    pub preferred_node_ids: SmallVec<[String; 4]>,
    pub require_tunnel: bool,           // true 时无可用节点直接报错
    pub prefer_low_latency: bool,       // 与 ProxyNodeView::latency_ms 排序联动
}
```

注意:**`require_tunnel = true` 且无可用节点 → emit `GW_TUNNEL_NO_NODE` 错误码**,不静默回退 Direct。这是 [`docs/issues/2026-05-22-dep-feature-audit.md`](../../issues/2026-05-22-dep-feature-audit.md) 标题里"silent feature-gate failures"的核心教训。

### ADR-T-5 `[tunnel]` 配置表

```toml
# config/default.toml
[tunnel]
enabled = false                          # 全局开关;`false` 时跳过 TunnelStack 构造
doh = "cloudflare"                       # 当前仅 cloudflare;未来可加 google / quad9
dns_cache_ttl_secs = 600
target_whitelist = []                    # ["api.openai.com:443", ...]
heartbeat_interval_secs = 30
missed_pong_threshold = 3
backoff_min_secs = 1
backoff_max_secs = 60
max_streams_per_conn = 256
```

环境变量遵循 figment `GW_TUNNEL__*` 约定;`enabled` 是 boot-time only(不支持热切换;切换要 SIGHUP)。

### ADR-T-6 `hickory-resolver` 升级到与 `rustls 0.23` 同源的版本

`hickory-resolver 0.24` 链入 `rustls 0.21` / `tokio-rustls 0.24`;`pingora 0.8` 链入 `rustls 0.23` / `tokio-rustls 0.26`。同二进制双 rustls 会导致 `CryptoProvider` 路径分裂(目前 `gw-gateway/src/lib.rs:87` 已为 0.23 安装了 `ring::default_provider`,但 0.21 拥有独立的 provider 注册表),实测会出现"DoH 请求走的 rustls 0.21 → 用 default provider → 与 0.23 的 ring provider 各自为政"。

**选型**:升级到 `hickory-resolver 0.25.x`(已迁移到 rustls 0.23 + tokio-rustls 0.26)。API 变化点(需要在 T4 实施时核对):
- `TokioAsyncResolver` → `TokioResolver`(类型重命名,但 `tokio` feature 默认开启)。
- `ResolverConfig::cloudflare_https()` 返回签名不变。
- `lookup_ip` 接口签名稳定。

**回退方案**:若 hickory 0.25 与现有 `safe_dns.rs` API 不兼容且修改面 > 50 行,改用 `reqwest::Client` 直接发 DoH JSON over HTTPS;`reqwest` 已经在 ADR-T-7 引入,无新增依赖代价。

### ADR-T-7 `reqwest` 不重新引入,DoH 仍用 hickory

刚刚在 [`docs/issues/2026-05-22-dep-feature-audit.md`](../../issues/2026-05-22-dep-feature-audit.md) §2 把 `reqwest` 从 workspace 摘掉了。本 spec **明确不再把它请回来**——hickory 0.25 升级后双 rustls 自然消失,reqwest 仅是回退方案。如果回退被触发,需要把 reqwest 重新加回 workspace dep,但仅作为 `gw-tunnel-client` 的可选依赖,而非全工作区 dep。

---

## 3. 数据流与时序

```text
client request
     │
request_filter ── 选模型/permission/quota,选 vendor_key 候选集
     │  (ctx.candidates: SmallVec<Arc<VendorKey>>)
upstream_peer
     │
     ├── filters::upstream::pick_peer
     │       │
     │       ├── strategy.pick(candidates, blacklist) -> Arc<VendorKey>
     │       │
     │       ├── 【新增】TunnelStack::resolve(provider, key, pref)
     │       │       │
     │       │       ├── 读 nodes ArcSwap;按 (provider 允许 ∧ key 允许 ∧ enabled) 过滤
     │       │       ├── prefer_low_latency=true → 按 latency_ms 排序
     │       │       └── 返回 Direct | Use(node_id)
     │       │
     │       ├── Direct  → HttpPeer::new(addr, tls=true, sni=host)
     │       └── Use(id) → TunnelStack::peer_for_node(id, host, port)
     │                       │
     │                       ├── transports.get_or_init(id) -> ReconnectingTransport
     │                       ├── 等待 transport.current().is_some()(or NO_TUNNEL_LIVE)
     │                       ├── target_filter::assert_target_allowed(host, port)
     │                       └── 返回经隧道转发的 HttpPeer
     │                          (peer.address = 节点 WS 地址,SNI/路由信息走 frame payload)
     │
upstream_request_filter → 写出请求头到隧道帧 / 直连 socket
response_body_filter   → 读隧道帧 / 直连流
logging                → ctx.tunnel_resolution 写入 detailed log
```

`peer.address` 指向**节点的 WS 地址**,Pingora 在 socket 层只看到一条普通 TLS 连接;真正的 upstream 路由在 frame payload 里通过 `MsgType::Open { target: host:port }` 表达。这意味着 Pingora 的连接复用、健康检查、超时全部针对"网关→节点"这一段。

---

## 4. 错误模型

新增错误码(`crates/gw-core/src/error/codes.rs`):

| 错误码 | 触发 | HTTP | 客户端可见 |
|--------|------|------|-----------|
| `GW_TUNNEL_NO_NODE` | `require_tunnel=true` 且过滤后节点列表空 | 502 | "service unavailable" |
| `GW_TUNNEL_NO_LIVE` | 选中节点的 `ReconnectingTransport::current()` 为 None(尚未连上或正在重连) | 502 | "service unavailable" |
| `GW_TUNNEL_TARGET_DENIED` | `assert_target_allowed` 失败 | 502 | "service unavailable"(白名单不暴露) |
| `GW_TUNNEL_DNS_BLOCKED` | `SafeDnsResolver::resolve` 返回 `BlockedIpError` | 502 | "service unavailable" |

**所有 tunnel 错误都参与 R-E failover**:`pick_peer` 上的 tunnel 失败要走 `set_retry(true)`,把 key 推入 `ctx.retry_blacklist`,以避免相同 key 反复打到同一个挂掉的节点。

---

## 5. 持久化与失效

- L1: `EntityStore.proxy_nodes_cache: moka::Cache<(), Arc<Vec<ProxyNodeView>>>`,TTL 600s。
- L2: Redis `entity:proxy_node:list`(整 JSON),write-through。
- L3: PG `proxy_nodes` 表,管理 API 改表后写 `events:entity` 一条 `{ namespace: "proxy_node", op: "invalidate" }`。
- `stream_consumer` 收到 `proxy_node` 命名空间事件 → `TunnelStack::reload_nodes()` 重抓全表 → 更新 ArcSwap。

KEK 旋转(`docs/architecture.md` §KEK rotation)覆盖 `proxy_nodes.auth_token_enc`;复用现有 `LoaderCtx.kek.decrypt_v(v, ciphertext)` 路径。

---

## 6. 遥测

新增计数器(在 `crates/gw-gateway/src/metrics_catalog.rs` 注册):

| 指标 | 标签 | 含义 |
|------|------|------|
| `gw_tunnel_resolve_total` | `result={direct\|tunnel\|denied\|no_node\|no_live}` | tunnel 决策结果分布 |
| `gw_tunnel_open_total` | `node_id`、`result={ok\|err}` | `send_open` 调用结果 |
| `gw_tunnel_transport_alive` | `node_id`(gauge) | `ReconnectingTransport::current().is_some()` |
| `gw_tunnel_reconnect_total` | `node_id`、`reason` | 监督循环触发的重连次数 |
| `gw_tunnel_doh_resolve_total` | `result={ok\|blocked\|nx}` | `SafeDnsResolver::resolve` 结果 |

`ctx.tunnel_resolution: Option<String>`(node_id 或 `"direct"`) 写入 detailed log(三层冗余级别中至少 `summary`)。

---

## 7. 实施拆分

| 任务 | 范围 | 依赖 |
|------|------|------|
| **T1** | 添加 `ProxyNode` entity 到 `gw-core`,迁移 `0004_proxy_nodes.sql`,`EntityStore::proxy_nodes_or_load` 路径 | 无 |
| **T2** | `VendorKey.tunnel_preference` 字段(`Option`,DB JSONB);`KeyVersion` decode 兼容旧行(`None`) | T1 |
| **T3** | `TunnelStack` 在 `gw-proxy` 落地;`LlmGateway` 增 `tunnel: Option<Arc<TunnelStack>>` | T1, T2 |
| **T4** | 升级 `hickory-resolver` → 0.25.x;若不兼容走回退;workspace `Cargo.toml` 调整 | 独立(可与 T3 并行) |
| **T5** | `gw-gateway::bootstrap` 在 `cfg.tunnel.enabled = true` 时构造 `TunnelStack`,装载白名单 + DoH;喂给 `LlmGateway::new` | T3, T4 |
| **T6** | 新错误码 + 遥测注册 + detailed-log 字段;`scripts/check-invariants.sh` 增 `no_dead_tunnel_use_alias`(检测 `pub use gw_tunnel_client` 必须配合 `TunnelStack` 引用,二者要么都在要么都不在) | T3 |
| **T7** | e2e:`crates/e2e-fake-upstream` 增加 `--mode tunnel`,模拟 aether-hub 节点(decode `MsgType::Open` → 直连真后端);新增 `crates/gw-gateway/tests/e2e_tunnel.rs`(`#[ignore]` + testcontainers) | T5 |
| **T8** | 更新 `CLAUDE.md:30` / `docs/architecture.md` §41(把"已就绪"改成"`[tunnel]` 配置生效后启用,仅 release 内开关"),更新 `docs/workflow.md` §upstream_peer | T5 |

**最小可上线**:T1 → T2 → T3 → T5 → T8。T4 / T6 / T7 可以拆独立 PR。

---

## 8. 不变量(进入 `scripts/check-invariants.sh`)

| 名字 | 规则 |
|------|------|
| `no_dead_tunnel_use_alias` | `grep -rn 'pub use gw_tunnel_client' crates/gw-gateway/src/` 与 `grep -rn 'TunnelStack' crates/gw-proxy/src/` 必须同时为零或同时非零 |
| `tunnel_require_means_hard_fail` | `require_tunnel` 出现处必须邻接 `GW_TUNNEL_NO_NODE`(grep 距离 ≤ 5 行)以确保不会静默 fallback |
| `no_double_rustls` | T4 完成后,`Cargo.lock` 不允许 `name = "rustls"\nversion = "0.21` 出现 |

---

## 9. 不破坏的承诺(回答 user "不破坏功能")

- `cfg.tunnel.enabled = false`(默认)时:`LlmGateway::new` 拿到 `tunnel: None`,`pick_peer` 走原 Direct 路径,行为字节级等价于 spec 落地之前。
- T2 `VendorKey.tunnel_preference: Option`(`None` 视为 "follow global"),既有种子数据不需要回填。
- ADR-T-6 升级 hickory 是孤立 PR,在 tunnel 接入(T5)之前可独立验证;失败可回滚。
- T8 文档更新与代码同 PR,杜绝再次出现"文档说就绪 / 代码 dead"的偏差。

---

## 10. 未决问题(标注 `[unresolved]`)

1. `[unresolved]` ProxyNode 的健康探测策略:是网关侧主动 ping(简单但有 N×M 复杂度),还是节点侧汇报到 Redis(需要节点端实现)。倾向前者作为 v1,放在 T3 内。
2. `[unresolved]` 是否暴露 admin 端点 `/healthz/tunnel`(列出每个 node 的 alive / last_pong_age / open_count)。受 `cfg.security.health_token` 保护即可,但需要确认运维需求。
3. `[unresolved]` `MsgType::Open` 的 target 序列化用纯字符串 `host:port`(当前 `send_open` 实现)还是 protobuf。当前足够;留待与 aether-hub 服务端协议契约对齐时复核。
