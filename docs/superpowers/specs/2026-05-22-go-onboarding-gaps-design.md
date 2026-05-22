# 2026-05-22 — Go 接入缺口项 Spec(6 项非阻塞补齐)

> **Status**: spec(待审阅,实现不在本 spec 范围)
> **日期**: 2026-05-22
> **作者**: 协作设计(Claude + 用户)
> **触发**: Go 网站端 FRD(`golang-website-frd.md`)落地前的网关侧 gap 盘点。
> **范围**: Go 侧落地 FRD 时 Rust 网关需要补齐的 6 项契约 / 能力缺口。**每项均非阻塞**(B1/B2 阻塞另见 blockers spec),但若不落,Go 侧某些 FRD 章节无法 1:1 对接。
> **不在范围**:
>   - 阻塞项(非流式 / mgmt token)—— 见 [`2026-05-22-go-onboarding-blockers-design.md`](2026-05-22-go-onboarding-blockers-design.md)
>   - Go 侧业务实现细节(参 FRD)
>   - Aether 前端 DTO 兼容层(Go 自治)
>
> **Companion**:
>   - 阻塞 spec(同日):[`2026-05-22-go-onboarding-blockers-design.md`](2026-05-22-go-onboarding-blockers-design.md)
>   - 契约变更记录:[`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md)
>   - Tunnel 子系统设计(G1 上游依赖):[`2026-05-22-tunnel-t-design.md`](2026-05-22-tunnel-t-design.md)

> 一句话定义:把 Go 落 FRD 时会"碰到但不会立即报错"的 6 个契约盲区显式化,避免 Go 上线后才发现 Rust 端读不到字段 / 多 pod 缓存不一致 / OAuth 没地方放凭据等问题。

---

## 0. 缺口清单

| # | 缺口 | FRD 章节 | 优先级 | Go 契约变更 |
|---|------|----------|--------|--------------|
| G1 | Proxy Nodes Hub 实体契约(`0005_proxy_nodes` 已 merged,本 spec 补契约面)| §6.10 | 高(启用 tunnel 时必落)| ✓ 新 ns + 新 part04 |
| G2 | `auth_configs` migration 落地(0006) | §6.1 / §6.8 | 高(启用严格 JWT 时必落)| ✓ 新表(part03 已定义 schema)|
| G3 | OAuth 刷新凭据 schema 扩展 | §6.4 | 高(启用 OAuth provider 时必落)| ✓ `vendor_keys.payload.oauth` 扩 |
| G4 | Gemini Files 端点责任分工 | §6.11 | 中 | ✓ writer 责任翻转 |
| G5 | 视频 / long-running operation 透传 | §6.11 | 中 | ✓ `UsageRecord.operation_id` 可选字段 |
| G6 | `events:entity` 失效完整清 Redis L1 | 多 pod 一致性 | 中 | ✓ 写顺序备注 |

按 FRD 模块开关:G1 仅在启用 tunnel(`[tunnel] enabled = true`)时必落;G2 仅在使用严格 JWT 验证(iss/aud/nbf)时必落;G3 仅在系统中存在 OAuth 型 vendor key 时必落。其它(G4/G5/G6)与开关无关。

---

## 1. G1 — Proxy Nodes Hub 实体契约

### 1.1 Status

Rust 已有 tunnel client(`gw-tunnel-client`),T 系列 spec 在 [`2026-05-22-tunnel-t-design.md`](2026-05-22-tunnel-t-design.md) 闭环。但 FRD §6.10 描述的"Go 维护节点元数据 / 心跳 / 状态、Rust 读节点视图"的跨进程契约 —— Go 写哪些字段、Rust 读哪些字段、节点心跳怎么进入实体表 —— 均未在 `docs/go-contracts/` 显式定义。Tunnel spec 内 ADR-T-3 提到要加 `ProxyNode` 实体进 `gw-store`,本 spec 把契约面补全。

### ADR-G1-1 — 引入 `ProxyNodeEntry`

**Decision**: 新增 entity:

```rust
// crates/gw-core/src/entity.rs
pub struct ProxyNodeEntry {
    pub node_id: String,            // 前缀 pn_
    pub display_name: String,
    pub region: Option<String>,
    pub ws_endpoint: String,        // gateway 拨号目标 wss://...
    pub status: ProxyNodeStatus,    // active | draining | offline
    pub latency_ms: Option<u32>,
    pub last_heartbeat_ts_ms: i64,
    pub target_whitelist: Vec<String>,    // 该节点可代理的上游 host:port
    pub provider_affinity: Vec<VendorId>, // 偏好出口的 vendor_id
    pub key_id_affinity: Vec<KeyId>,      // 偏好出口的 vendor_key_id
    #[serde(default)]
    pub extra: serde_json::Value,   // forward-compat
}
```

入 `EntityStore.proxy_node` + `loader::load_proxy_node` + `StreamConsumer` dispatch(ns = `proxy_node`),Moka TTL 30 min(与 vendor_key 同档,因为节点变更不频繁)。

### ADR-G1-2 — Heartbeat 仅 Go 写

**Decision**: 远端节点的 `/api/internal/hub/heartbeat` 端点由 **Go 实现**;Rust 端**完全不接收**节点心跳。

收到心跳后,Go:
1. UPDATE `proxy_nodes` SET `payload = jsonb_set(payload, '{last_heartbeat_ts_ms}', new_ts)`,version + 1;
2. 若连续 3 次心跳从 offline → active,同步 status 字段;
3. XADD `events:entity * ns proxy_node id <node_id> v <new_v> op upsert`。

**Why**: 节点状态机(`active → draining → offline`)涉及多次心跳判定、配置变更通知、运维介入 —— 属于控制面职责,放 Go。Rust 端只读取 status 最终态,不维护状态机。

**Rejected alternative**:Rust 端通过 admin endpoint 接收节点心跳(类似 ADR-T 的本机 admin API)。被否决理由:Rust 网关无身份验证管理面,且节点上报字段会持续演化(latency / capacity / 健康指标),让 Go 单独维护 schema 演化更轻。

### ADR-G1-3 — `TunnelStack.nodes` 更新方式

**Decision**: `TunnelStack`(参 tunnel-t spec ADR-T-2)启动期冷加载 `proxy_nodes` 全表 → `ArcSwap<Vec<ProxyNodeView>>`;`StreamConsumer` 收到 `events:entity ns=proxy_node` 时,触发 `TunnelStack::refresh()`(从 `EntityStore` 重读所有 status != offline 的节点列表),原子 swap。

**不变量**:同一 `ws_endpoint` 对应的 `ReconnectingTransport` 不在 swap 时重建 —— `DashMap<node_id, Arc<ReconnectingTransport>>` 按 `node_id` 持有 transport;只有 node 软删 / status → offline 时 drop 该 transport。

**Why 全量 refresh 而非 diff**:节点数量在 v1 < 100 量级,全量 refresh 简单;diff 复杂度的收益不抵实现成本。

### ADR-G1-4 — Status 转换不在 Rust 触发

**Decision**: Rust 端**不**做心跳超时判定;以 Go 写的 `status` 为准。Rust 内部如果发现某 node WS 连接频繁断 → 通过 metric `gateway_tunnel_node_failure_total{node_id}` 暴露,运维 / Go 侧决定何时下线。

**Rejected alternative**:Rust 检测到连续 N 次 WS 断连后反向 XADD `events:entity ns=proxy_node op=status_change` 把 node 标 offline。被否决理由:(1) Rust 多 pod 部署时,某 pod 网络抖动不代表节点真挂;(2) 节点状态机权威必须单源(避免 Go 标 active / Rust 标 offline 互相覆盖)。

### 1.2 Acceptance

- Go 写 `proxy_nodes` + XADD → 1s 内 Rust(任一 pod)开始拨号到新 `ws_endpoint`;
- Go 软删除节点(`deleted_at` + `op=delete`)→ Rust 该 node 的 transport drop;同节点的后续 key 请求重新走 `resolve_tunnel`(无其他节点 → Direct 或 `GW_TUNNEL_NO_NODE`);
- Go 把 status 改 draining → Rust 不再选该 node 作为新 stream 出口,已有 stream 不中断;
- 新测:e2e 加 `test_proxy_node_lifecycle.py`(`feature/tunnel-e2e` 范畴)。

### 1.3 Go 侧契约影响

- 新 ns = `proxy_node`(events:entity)
- PgSQL 表 `proxy_nodes`(migration 0005,已 merged;本 spec 不再新建 migration,仅补契约文档)
- 新 part: [`docs/go-contracts/parts/04-proxy-nodes.md`](../../go-contracts/parts/04-proxy-nodes.md)
- main.md §2 文件组织表新增行

详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §2。

---

## 2. G2 — `auth_configs` migration 落地

### 2.1 Status

`docs/go-contracts/parts/03-shared-schema.md` §3.2.12 已完整定义 `AuthConfig` payload schema。`crates/gw-store/src/loader.rs:158-165` 已写 SELECT SQL。但 `0005_auth_configs.sql` migration 文件不存在,导致生产部署时 Rust 启动期 `auth_config_or_load("global")` 总是失败 → JWT 走 legacy fallback(只校验 `exp`,不校验 `iss`/`aud`/`nbf`)。

[`docs/workflow.md`](../../workflow.md) §2.2 step 2 已记录此行为为有意的过渡兼容,本 spec 把过渡期收尾。

### ADR-G2-1 — 0005 migration 落地

**Decision**: 增加 `crates/gw-store/migrations/0006_auth_configs.sql`(`0005_proxy_nodes` 已存在):

```sql
CREATE TABLE auth_configs (
  id          TEXT PRIMARY KEY,
  payload     JSONB NOT NULL,
  version     BIGINT NOT NULL DEFAULT 1,
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at  TIMESTAMPTZ NULL
);
```

**不在 migration 内 seed `global` 行**:由 Go 端管理 UI / 初始化脚本写入。理由:`expected_iss` / `expected_aud` 等字段必须由部署环境填充,migration 不该硬编码。

### ADR-G2-2 — 启动期 Rust 行为

**Decision**: Rust 启动期 `auth_config_or_load("global")`:
- 表存在 + `global` 行存在 → 严格模式,`LlmGateway.jwt_validator = Some(_)`;
- 表存在 + `global` 行缺失 → 启动期 `tracing::warn!("auth_configs.global missing, falling back to legacy JWT exp-only validation")`,继续启动,`LlmGateway.jwt_validator = None`;
- 表不存在(0005 未跑)→ 同上,继续启动。

**Why 不 fail-fast**:Go 端 0005 落地与 `global` seed 是两步;若 fail-fast,Go 没 seed 时 Rust 拒启动会阻塞迭代。Warn + 降级是更安全的过渡。

### 2.2 Acceptance

- 0005 migration 可独立跑 `psql -f` 不报错;
- `global` 行存在时 JWT 请求按 `expected_iss` / `expected_aud` 验证,失败精细 401(参 [G6](../../architecture.md#9-known-gaps-closed-by-featureknown-gaps-closure));
- `global` 行缺失时 Rust 启动 warn + 走 legacy `exp` 校验(无回归);
- Sync rule 触发更新 `docs/architecture.md` §5.1 migration 表与 `docs/go-contracts/parts/03-shared-schema.md` §3.1 / §3.9。

### 2.3 Go 侧契约影响

`docs/go-contracts/parts/03-shared-schema.md` §3.9 第 1 条("auth_configs migration 待加")随本 spec 实现移除;§3.1 migration 表加 0005 行。

详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §3。

---

## 3. G3 — OAuth 刷新凭据 schema 扩展

### 3.1 Status

当前 `VendorKey.payload.api_key_enc` 是单一加密值(AES-256-GCM 四元组)。FRD §6.4 要求 OAuth 型 Provider Key 支持 "start / complete / refresh / device flow / batch import";Go 端需要存:`access_token` / `refresh_token` / 过期时间 / `token_endpoint` URL —— Rust 端 `vendor_keys` schema 没有对应字段,导致 Go 没地方放凭据。

### ADR-G3-1 — VendorKey payload 增加 `oauth` 子结构

**Decision**: `VendorKey` payload 加可选字段 `oauth: Option<OAuthCredentials>`:

```rust
pub struct OAuthCredentials {
    pub access_token_enc: EncryptedKey,    // AES-256-GCM 四元组
    pub refresh_token_enc: EncryptedKey,
    pub access_expires_at: OffsetDateTime,
    pub token_endpoint: String,            // Go refresh 用
    pub scope: Vec<String>,
}
```

`None`(非 OAuth 型 key)→ 走 `api_key_enc` 原路径;`Some(_)` → 走 ADR-G3-2 注入。

**Rejected alternative**:把 OAuth 凭据放在与 vendor_key 1:1 的单独表(如 `vendor_key_oauth`)。被否决理由:(1) Rust 端要读两次(vendor_key + oauth),增加冷加载复杂度;(2) version 单调性跨表难协调,refresh 时凭据与 vendor_key 不同步 = 安全风险;(3) JSONB 子对象 forward-compat 简单。

### ADR-G3-2 — Rust 注入 `access_token` 而非 `api_key`

**Decision**: `crates/gw-proxy/src/filters/upstream.rs::upstream_request_filter` 注入 auth header 时:

```rust
let auth_token = match &picked.oauth {
    Some(oauth) => decrypt(&oauth.access_token_enc, &kek)?,
    None => decrypt(&picked.api_key_enc, &kek)?,
};
let auth_header = vendor.auth_template.replace("{api_key}", auth_token.expose_secret());
```

**Rust 不主动 refresh**:`token_endpoint` 字段 Rust 端**不读**,仅供 Go task-worker 用。refresh 是 Go 责任。

### ADR-G3-3 — Access token 临过期处理

**Decision**: `upstream_request_filter` 在使用 `oauth.access_token` 前检查 `now >= access_expires_at - 60s`:

- **未临期**:正常使用;
- **临期但未过**:仍使用现有 token(Go 端可能正在 refresh);emit metric `gw_oauth_token_near_expiry_total{vendor_key_id}`,不阻塞请求;
- **已过期**:仍使用现有 token,emit metric `gw_oauth_token_expired_total{vendor_key_id}`;上游必然返回 401 → 走 ADR-G3-4。

**Why 不主动 refresh**:复杂度太高(并发 refresh 控制 / refresh 失败回退 / 多 pod 协调),且 Rust 持有 KMS 密钥后还需要加密回写 PgSQL —— 跨越 Go-as-Writer 不变量。统一 Go task-worker 周期扫描 + 即将过期前 refresh。

### ADR-G3-4 — Token endpoint 401 → 反向通知 Go

**Decision**: 现有 `OAuthListener`([architecture.md §2.8 error fanout](../../architecture.md#21-crate-responsibilities))收到上游 401 + `AccountBlocked(OAuthRefreshFailed)` 时,XADD:

```
events:entity * ns vendor_key id <key_id> op status_change
              field oauth_failed reason upstream_401 ts_ms <now>
```

Go task-worker 监听后:
1. 检查 refresh_token 有效性 → 尝试 refresh;
2. 成功:加密新 access_token + 整体替换 `payload.oauth` 子对象 + version + 1 + XADD `events:entity` op=upsert;
3. 失败:status = disabled + XADD。

Rust 端不主动 refresh,等 Go 写新 vendor_key payload 后自然冷加载到新 token。

### 3.2 Acceptance

- `VendorKey.payload.oauth = Some(_)` 时,Rust 上游请求注入 oauth.access_token;
- `oauth = None` 时,沿用 `api_key_enc` 路径,行为无回归;
- Access token 临期 / 已过期触发对应 metric;
- 上游 401 触发反向 `events:entity` 通知,Go 收到后 refresh 并 XADD,Rust 1s 内拿到新 token;
- 新测:`crates/gw-proxy/tests/oauth_vendor_key.rs` + 反向事件测试。

### 3.3 Go 侧契约影响

- `VendorKey.payload.oauth` 子结构(parts/03 §3.2.5 扩展)
- 新反向 `events:entity op=status_change field=oauth_failed`(parts/02 §2.4 扩)
- Go task-worker 实现 refresh + access_token rotation 后 XADD `events:entity ns=vendor_key v=<new>`

详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §4。

---

## 4. G4 — Gemini Files 端点责任分工

### 4.1 Status

FRD §6.11 列了完整 Gemini Files API:`/upload/v1beta/files`(multipart 上传)、`/v1beta/files/*`(列出 / 详情 / 删除)。Rust 当前只有 `gemini_file_mapping` 表用于去重映射(sha256 → file_uri),不处理 multipart upload。当前 writer 是 Rust(`gw-codec::gemini` 在多模态请求处理时写)。FRD 描述与 Rust 现状之间有空白:谁负责 Aether 前端的 file upload?

### ADR-G4-1 — Gemini Files 端点完全 Go 实现

**Decision**: Rust 网关**不实现** `/upload/v1beta/files`、`/v1beta/files/*` 端点。Go 在自己的 HTTP server 实现(`/api/admin/gemini-files/*` 或独立 Gemini 兼容 endpoint),理由:

- multipart upload 是非 LLM 数据面流量,与 Rust gateway 热路径(token-by-token)目标不匹配;
- 文件落对象存储(S3 / OSS)是 Go-native 能力,Rust 引入会拖入新依赖;
- Aether 前端按 FRD §6.11 调用 `/api/admin/gemini-files/*`,Go 路由层不需要绕 Rust。

### ADR-G4-2 — `gemini_file_mapping` writer 翻转

**Decision**: `gemini_file_mapping` 表的 writer 从 Rust(当前 codec 路径)改为 **Go**(文件上传成功 + 推送到 Gemini 后写)。Rust 仅读用于多模态请求时校验 `file_uri` 归属。

- writer 切换 = breaking change([feedback_breaking_changes_ok](../../.claude/projects/-Users-whereslow-Desktop-work-ailinkmax-gateway/memory/feedback_breaking_changes_ok.md) 允许);
- 现有 Rust 写路径(若 `gw-codec/src/gemini` 内有写入 `gemini_file_mapping` 的代码)删除;
- Rust 不监听 `events:entity ns=gemini_file_mapping`(此表非 entity ns,无事件通知,Rust 按 `(sha256_hex, user_id)` PK 直接查 PgSQL,命中即用)。

**Why writer 翻转**:Aether 前端上传文件 → Go 接收 multipart → Go 转推给 Gemini File API → Go 拿到 `file_uri`。Rust 在这条路径上没有任何一步;若 Rust 也写,需要 Go 把 sha256 / file_uri 反向告诉 Rust,等价于多一次 RPC,意义不大。

### 4.2 Acceptance

- 前端调用 `/api/admin/gemini-files/*` 命中 Go endpoint(本 spec 不约束实现);
- Go 上传文件后写 `gemini_file_mapping`(sha256_hex, user_id, file_uri, mime, size, expire_unix_s);
- Rust 多模态请求(用户引用 `file_uri`)时按 sha256 查到映射后正常透传 Gemini 上游;
- 现有 Rust 写路径删除后,所有非 e2e 测试仍通过(因为该写路径无 e2e 测试覆盖,断网即可看到 SELECT 不到的失败兜底)。

### 4.3 Go 侧契约影响

- `gemini_file_mapping` writer = Go(parts/03 §3.1.2 现写 "Writer: Rust" 需翻转)
- 详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §5

---

## 5. G5 — 视频 / long-running operation 透传

### 5.1 Status

FRD §6.11 列了 `:predictLongRunning`、`/v1beta/operations/{name}`、`/v1/videos/*`。Rust 网关单次 LLM 请求模型不适配长任务轮询。FRD 明确"实际模型调用可由 Rust 网关或 task worker 执行;Go 主服务必须保留任务状态、查询和管理 API"。即:Rust 处理初始 POST,Go 处理后续轮询。但 Rust 当前不抽取 `operation_id`,Go 无法关联 usage record 与后续轮询任务。

### ADR-G5-1 — Rust 仅透传初始 operation 请求

**Decision**: `:predictLongRunning` POST 等价于一次普通非流式请求(B1 落地后可走),Rust 处理 codec / 路由 / usage 与现有流程一致;**响应中如果带 operation name(Gemini 上游返回顶层 `name: "operations/..."` 字段),Rust 抽取并写入 `UsageRecord.operation_id`**。

后续轮询(GET `/v1beta/operations/{name}`)由 Go task-worker 直接打到上游(绕过 Rust gateway)。Rust 不参与轮询路径,理由:轮询请求负载小(就是查 status),且 Go task-worker 自带凭据 / 上游路由能力(它要做 video_tasks 状态机)。

**Rejected alternative**:让 Rust 代理 operations 查询。被否决理由:(1) operations 路径不是 LLM 数据面,与网关定位不符;(2) 5 个 vendor key 的 operation 跨 key 查询语义模糊(operation 与发起它的 key 绑定),Rust 路由层没有这种细分能力。

### ADR-G5-2 — UsageRecord schema 加 `operation_id`

**Decision**: `gw-ledger::UsageRecord` 加 `operation_id: Option<String>`(默认 `None`);`response_body_filter` 在解析 `GenerateContentResponse` / `VideoGenerateResponse` / `Operation` body 时,若发现顶层 `name: "operations/..."` 字段,填入 `ctx.operation_id`,最终落 `UsageRecord.operation_id`。

抽取实现位于 codec 层(`gw-codec` 各 codec 的 `parse_response` 顺带抽 operation_id)。

### 5.2 Acceptance

- `:predictLongRunning` POST 成功后 `UsageRecord.operation_id != None`,值与上游返回 `name` 一致;
- 普通 chat 请求 `operation_id == None`,Go settler 兼容(field 是 optional);
- Go task-worker 用 `operation_id` 关联 `video_tasks` 表与 usage records(`operation_id` 作为关联键);
- 新测:e2e fake_upstream 加 long-running scenario,验证 operation_id 落到 `usage:records`。

### 5.3 Go 侧契约影响

`UsageRecord` 新字段 `operation_id`(parts/02 §2.1.2 schema 扩);Go settler 端需保证未知 / 缺失 `operation_id` 不破坏反序列化(`serde(default)` / Go 端 omitempty)。

详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §6。

---

## 6. G6 — `events:entity` 失效完整清 Redis L1

### 6.1 Status

[`docs/architecture.md`](../../architecture.md) §10.5 列出的已知 production 问题:`events:entity` 触发 Rust `StreamConsumer.invalidate` 时,只清当前进程 Moka L1,**不清 Redis L1**(`cache:{ns}:{id}` HASH)。多 pod 部署时,其他 pod 的 Redis L1 残留陈旧值最长 TTL(30 min for vendor_key / pricing / model_group)。e2e harness 在 `put_entity` / `delete_entity` 中 `DEL` Redis L1 来补,生产二进制无此补口。

### ADR-G6-1 — Rust 端 invalidate 时 `DEL` Redis L1

**Decision**: `crates/gw-store/src/stream_consumer.rs::apply_entity_event` 在 invalidate Moka **之前**,先执行:

```rust
redis.del(format!("cache:{}:{}", ns, id)).await;
```

失败仅 `tracing::warn!`(不阻塞 Moka invalidate)。

**幂等性**:任意 pod 收到 `events:entity` → DEL Redis L1;多 pod 同时收同一事件即多次 DEL,无害(Redis DEL 对不存在的 key 是 no-op,对存在的 key 仅删一次)。

**Why 在 invalidate 之前**:让 Moka miss 时确保 Redis L1 也 miss → 强制走 PgSQL 冷加载 → 写回新版本到 Redis L1。如果先清 Moka 后清 Redis,中间窗口 Moka miss 后从 Redis 拿到陈旧值会回写到自己 Moka,产生短暂陈旧。

### ADR-G6-2 — 启动期不清 Redis L1

**Decision**: Rust 启动期对 `events:entity` stream 用 `$` 锚点(只消费启动后事件),仍保留此行为。冷加载链:Moka miss → Redis L1 命中(可能陈旧 ≤ TTL)→ PgSQL fallback。本 ADR **不解决"启动期 Redis L1 陈旧"**,只解决"运行期"。

如要彻底解决启动期,需要 Go 主动 DEL 或 Rust 全量预热 —— 不在本 spec 范围,留 follow-up。

**Why 不全量预热**:启动期全表预热在 vendor_key 数量 > 1000 时启动时间不可控;且启动期 Redis L1 陈旧的最大影响 = 一次冷启动后请求看到最长 30 min 旧 vendor_key(可被 Go 主动 invalidate),与多 pod 运行期持续不一致相比,影响面小得多。

### 6.2 Acceptance

- Go 写 entity → XADD `events:entity` → 任一 Rust pod 内 Redis L1 中该 key 在 1s 内消失(`DEL cache:{ns}:{id}` 已执行);
- 多 pod 部署场景:pod A 写、pod B 收 events → pod B 也 DEL Redis L1(同一 Redis key 被多次 DEL 无害);
- 重启场景:Redis L1 中陈旧 vendor_key 在 30 min TTL 内仍存在(已知行为,本 ADR 不解决);
- 新测:`crates/gw-store/tests/stream_consumer_redis_l1.rs`(testcontainers) —— 验证 invalidate 后 Redis HGET 该 key 返回空。

### 6.3 Go 侧契约影响

写顺序无变更,但 Rust 行为补充(invalidate 时清 Redis L1)需要在 parts/01 §1.2.4 与 parts/02 备注;Go 多 pod 部署文档应当移除"建议 Go 端也主动 DEL Redis L1"的歧义。

详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §7。

---

## 7. 实施顺序提示(非 plan)

互相独立的优先级排序:

- **必落优先级**:G2(`auth_configs` migration) > G6(Redis L1 一致性) > G5(operation_id)—— 这三项不依赖外部 feature 开关。
- **依赖 feature 开关**:G1(tunnel)/ G3(OAuth)/ G4(Gemini files) —— 视 Go 落地节奏。
- **依赖关系**:
  - G1 ADR-G1-3 依赖 [`2026-05-22-tunnel-t-design.md`](2026-05-22-tunnel-t-design.md) `TunnelStack` 已实例化(参 tunnel ADR-T-2);
  - G3 ADR-G3-3 / G3-4 依赖 G3-1 schema 落地;
  - 其余 ADR 内独立。

---

## 8. 风险

| 风险 | 缓解 |
|---|---|
| ProxyNodeEntry schema 演进频繁 | `extra: serde_json::Value` 字段做 forward-compat;Rust 只读 stable 字段 |
| OAuth refresh 与 Rust 使用并发竞态 | 临过期 60s 安全窗口 + Go refresh 完成 XADD 让 Rust 立即拉新 token;窗口期内仍用旧 token 不阻塞 |
| Gemini Files writer 切换期 | 一次性切,沿用 [breaking changes OK](../../.claude/projects/-Users-whereslow-Desktop-work-ailinkmax-gateway/memory/feedback_breaking_changes_ok.md) 立场 |
| Redis L1 DEL 与 Moka invalidate 间隙 | ADR-G6-1 先 DEL Redis 再清 Moka,避免回写陈旧;DEL 失败仅 warn,Moka invalidate 仍执行 |
| `operation_id` 抽取在不同 vendor 字段名不同 | 由各 codec `parse_response` 内承接(Gemini = 顶层 `name`,OpenAI 视具体 video endpoint 而定);Rust 上层不感知 |

---

## 9. 关联

- 主架构:[`docs/architecture.md`](../../architecture.md) §10.5 已知 production 问题
- 请求流:[`docs/workflow.md`](../../workflow.md) §4 cache invalidation
- Go 契约:[`docs/go-contracts/main.md`](../../go-contracts/main.md) 及 parts/01-03
- Tunnel 子系统:[`2026-05-22-tunnel-t-design.md`](2026-05-22-tunnel-t-design.md)(G1 依赖)
- 契约变更记录:[`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md)
- 同日阻塞 spec:[`2026-05-22-go-onboarding-blockers-design.md`](2026-05-22-go-onboarding-blockers-design.md)
