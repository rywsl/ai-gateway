# 2026-05-22 — Go 接入(Blockers + Gaps)契约变更

> **Status**: 契约变更记录(active)
> **日期**: 2026-05-22
> **触发 spec**:
>   - [`docs/superpowers/specs/2026-05-22-go-onboarding-blockers-design.md`](../../superpowers/specs/2026-05-22-go-onboarding-blockers-design.md) — B1 非流式 / B2 Mgmt Token
>   - [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md) — G1..G6
>
> **本文件作用**:把两份 spec 引入的 Go 控制面侧契约新增 / 修改集中索引化,作为"日志条目"存档。每条改动标明**触发 ADR + parts 落点 + Rust 落点**。
>
> **何时同步 parts/**:本 changelog 在 spec 审阅通过后即可合并;`parts/01`、`parts/02`、`parts/03` 与新加 `parts/04` 的实际正文修改**与 spec 实现 PR 同 PR 落地**(对齐 [main.md §7 Sync rule](../main.md#7-sync-rule))。
>
> **后续每次跨进程契约变更**:在本目录(`docs/go-contracts/contract-change/`)加 `<YYYY-MM-DD>-<topic>.md`,引用对应 spec + 列出 parts 改动点 + Rust 落点。

---

## 0. 变更索引

| # | 变更类型 | 来源 ADR | parts 落点 | Rust 落点 |
|---|----------|----------|------------|-----------|
| 1 | 新 entity 命名空间:`management_token` | blockers ADR-B2-1 / B2-4 | parts/01 §1.2.3 + parts/03 §3.2(新加 §3.2.13)+ parts/02 新加 §2.10 | `EntityStore.mgmt_token` + migration **0007** + `RedisTokenBlacklist` |
| 2 | 新 entity 命名空间:`proxy_node` + 新 part04 | gaps ADR-G1-1 / G1-2 / G1-3 | 新建 [parts/04-proxy-nodes.md](../parts/04-proxy-nodes.md) + main.md §2 表 + parts/01 §1.2.3 + parts/03 §3.6 ID 表 | `EntityStore.proxy_node` + migration **0005**(已 merged)+ `TunnelStack.nodes` |
| 3 | `auth_configs` migration 落地 | gaps ADR-G2-1 / G2-2 | parts/03 §3.9 第 1 条删除 + parts/03 §3.1 migration 表加 0006 | migration `0006_auth_configs.sql` |
| 4 | VendorKey payload 加 `oauth` 子结构 + 新反向事件 op | gaps ADR-G3-1 / G3-2 / G3-3 / G3-4 | parts/03 §3.2.5 扩展 + parts/02 §2.4 新增 `oauth_failed` op | `gw-core::VendorKey.oauth`、`OAuthListener` 反向 XADD |
| 5 | `gemini_file_mapping` writer 翻转 = Go | gaps ADR-G4-2 | parts/03 §3.1.2 writer 行翻转 | 删 Rust 写路径(`gw-codec::gemini` 若有)|
| 6 | `UsageRecord` 加 `operation_id` 可选字段 | gaps ADR-G5-2 | parts/02 §2.1.2 JSON schema 扩 | `gw-ledger::UsageRecord.operation_id` |
| 7 | Rust invalidate 时清 Redis L1 | gaps ADR-G6-1 / G6-2 | parts/01 §1.2.4 末尾追加 + parts/02 新加 §2.11 | `stream_consumer.rs::apply_entity_event` |

下文按行号给出每条改动在对应 `parts` 文件中的**最终文本**,供 PR 实施时直接对照。**禁止**改动其他段(除非本 changelog 显式列出)。

---

## 1. 新 entity:`management_token`

### 1.1 `parts/01-go-as-producer.md` §1.2.3 namespace 列表新增行

在 `auth_config` 行下方追加:

```
| `management_token` | `management_tokens`(0007 migration) | `EntityStore.mgmt_token` | `ManagementTokenEntry` | 5 min TTL;ID 前缀 `mt_` |
```

### 1.2 `parts/03-shared-schema.md` 新增 §3.2.13

在 §3.2.12 后追加:

````
### 3.2.13 `management_tokens.payload` — `ManagementTokenEntry`(0007 待 migration)

```json
{
  "token_id": "mt_xxx",
  "user_id": "u_abc",
  "scope": "admin_full",       // admin_full | admin_read | user_self
  "hash": "<bytes base64>",    // SHA-256(secret),与 ApiKeyEntry.hash 同范式
  "expires_at": null,          // 可空,null = 永不过期
  "status": "active",          // active | disabled
  "display_name": "personal CI token"
}
```

ID = `token_id`(前缀 `mt_`)。Rust 行为:

- 客户端在 `Authorization: Bearer mt_<id>.<secret>` 中携带;
- Rust 用 `mt_<id>` 部分作为 entity 查询 key;`<secret>` SHA-256 后与 `hash` constant-time 比对;
- 撤销走 `mgmt_token:blacklist:<token_id>` Redis key(参 [parts/02 §2.10](../parts/02-go-as-consumer.md#210-mgmt_tokenblacklist-redis-key))。
````

### 1.3 `parts/03-shared-schema.md` §3.6 ID 前缀表新增行

在 `auth_config` 行下方追加:

```
| management_token | `mt_` | Go 生成 |
```

### 1.4 `parts/02-go-as-consumer.md` 新增 §2.10

在 §2.9 (未决问题) **之前** 插入(章节后续顺延为 §2.12 未决问题):

````
## 2.10 `mgmt_token:blacklist:<token_id>` Redis Key

| Key 格式 | `mgmt_token:blacklist:<token_id>` |
|---|---|
| Type | STRING |
| Writer | Go(用户主动撤销 / 管理员强制 revoke)|
| Reader | Rust(`/healthz/*` + management-path 验证后查 `EXISTS`)|
| TTL | Go 设为 token 的剩余有效期 |

写入语义与 [`jwt:blacklist`](../parts/01-go-as-producer.md#16-jwtblacklistjti-redis-key) 一致。Rust 验证通过后做一次 `EXISTS`:

- 命中 → 401 `GW_AUTH_MGMT_TOKEN_REVOKED`;
- Redis 不可达 → 按 `AuthConfig.blacklist_fail_mode` 走 `FailClosed`(默认,仍 401)或 `FailOpen`(放行);
- `FailMode` 与 JWT 黑名单共用单一开关(Rust 端实现:`RedisJwtBlacklist` 改名 `RedisTokenBlacklist`,支持两个 namespace)。

Go 写入示例:

```
SET mgmt_token:blacklist:<token_id> "1" EX <remaining_seconds>
```
````

### 1.5 migration 0007 模板(供 Go 端参考)

```sql
CREATE TABLE management_tokens (
  id          TEXT PRIMARY KEY,
  payload     JSONB NOT NULL,
  version     BIGINT NOT NULL DEFAULT 0,
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at  TIMESTAMPTZ NULL
);
CREATE INDEX management_tokens_user_id_idx ON management_tokens ((payload->>'user_id'));
```

落点:`crates/gw-store/migrations/0007_management_tokens.sql`。

---

## 2. 新 entity:`proxy_node` + 新 part04

### 2.1 `parts/01-go-as-producer.md` §1.2.3 namespace 列表新增行

在 `management_token` 行下方追加(本变更与 §1.1 同表):

```
| `proxy_node` | `proxy_nodes`(0005 migration,已 merged)| `EntityStore.proxy_node` | `ProxyNodeEntry` | 30 min TTL;支撑 tunnel client 节点视图;详见 [parts/04](../parts/04-proxy-nodes.md) |
```

### 2.2 新增 `parts/04-proxy-nodes.md`

新建文件,完整内容:

````
# Part 04 — Proxy Nodes(Hub / Tunnel 节点契约)

> 本 part 描述 **Rust 网关 tunnel client 与 Go Hub 之间** 的代理节点契约。索引见 [main.md](../main.md)。本 part 引入 = [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md) §1 + [`docs/superpowers/specs/2026-05-22-tunnel-t-design.md`](../../superpowers/specs/2026-05-22-tunnel-t-design.md) ADR-T-3。

## 4.1 概览

| 接触点 | Writer | Reader | 说明 |
|---|---|---|---|
| PgSQL `proxy_nodes` | Go(Hub heartbeat ingestion + 管理 UI)| Rust(EntityStore 冷加载)| 节点元数据权威 |
| Redis `cache:proxy_node:<id>` | Go(write-through 可选)| Rust(冷加载写回)| L1 cache |
| `events:entity` ns=`proxy_node` | Go | Rust StreamConsumer | 节点变更通知 |
| `/api/internal/hub/heartbeat` | (远端节点 → Go)| Go | Rust **不**接收 |
| `/api/internal/hub/node-status` | Go 暴露 | 前端 / 运维 | Rust **不**接收 |
| WS endpoint(`ws_endpoint` 字段)| 远端节点暴露 | Rust tunnel client 拨号 | 仅 Rust → 远端单向 |

## 4.2 `proxy_nodes.payload` — `ProxyNodeEntry`

```json
{
  "node_id": "pn_xxx",
  "display_name": "us-west-1-a",
  "region": "us-west-1",
  "ws_endpoint": "wss://node-xxx.example.com:443/tunnel",
  "status": "active",            // active | draining | offline
  "latency_ms": 42,
  "last_heartbeat_ts_ms": 1716345678901,
  "target_whitelist": ["api.openai.com:443", "api.anthropic.com:443"],
  "provider_affinity": ["v_openai"],
  "key_id_affinity": ["vk_xxx", "vk_yyy"],
  "extra": {}                    // serde_json::Value,forward-compat
}
```

ID = `node_id`(前缀 `pn_`)。

### 4.2.1 `status` 语义

| 状态 | 含义 | Rust 行为 |
|---|---|---|
| `active` | 心跳正常,可拨号 | 加入 `resolve_tunnel` 候选 |
| `draining` | 不接新流量,已有 stream 继续 | `resolve_tunnel` 排除;`TunnelStack` 保留 transport 直到 stream 结束 |
| `offline` | 心跳超时 / 主动下线 | `TunnelStack` drop 对应 `ReconnectingTransport` |

## 4.3 Heartbeat 责任分工

### 4.3.1 远端节点 → Go

`/api/internal/hub/heartbeat` 由 **Go** 服务;远端节点周期性上报。Rust 端**完全不接收**节点心跳。Heartbeat payload 字段由 Go 端 + 节点协议自决,本契约不规定。

### 4.3.2 Go → 写 PgSQL + XADD

收到心跳后,Go 端:

1. `UPDATE proxy_nodes SET payload = jsonb_set(payload, '{last_heartbeat_ts_ms}', new_ts), version = version + 1 WHERE id = $1`;
2. 若 status 从 offline → active(连续 3 次心跳),同步更新 `payload.status`;
3. `XADD events:entity * ns proxy_node id <node_id> v <new_v> op upsert`。

### 4.3.3 心跳超时状态机(Go 责任)

| 超时窗口 | 判定 |
|---|---|
| < 30s 未收 | 不动(网络抖动容忍)|
| 90s 未收 | status → draining + XADD |
| 300s 未收 | status → offline + XADD |

Rust 自身**不**做超时判定;以 Go 写的 status 为准(参 [gaps spec ADR-G1-4](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md#adr-g1-4-status-转换不在-rust-触发))。

## 4.4 Rust 读路径

详见 [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md) ADR-G1-1 / G1-3 与 [`2026-05-22-tunnel-t-design.md`](../../superpowers/specs/2026-05-22-tunnel-t-design.md) ADR-T-2 / T-3。要点:

- `TunnelStack` 启动期冷加载 `proxy_nodes` 全表;
- `StreamConsumer` 收到 ns=`proxy_node` 事件 → 触发 `TunnelStack::refresh()`(全量重读 status != offline 的节点)→ ArcSwap;
- `DashMap<node_id, Arc<ReconnectingTransport>>` 按 node_id 持有 transport;node 软删除 / status = offline → drop transport。

## 4.5 监控

Go 端:

| 指标 | 阈值 |
|---|---|
| `hub_heartbeat_lag_secs{node_id}` | > 60 |
| `proxy_node_status_distribution{status}` | `offline` 比例 > 20% → 告警 |
| `proxy_node_upsert_total` | counter |

Rust 端 `gateway_tunnel_*` 详见 [`docs/superpowers/specs/2026-05-22-tunnel-t-design.md`](../../superpowers/specs/2026-05-22-tunnel-t-design.md) §遥测段。

## 4.6 migration 0005 模板(已 merged,本节供历史对照)

```sql
CREATE TABLE proxy_nodes (
  id          TEXT PRIMARY KEY,
  payload     JSONB NOT NULL,
  version     BIGINT NOT NULL DEFAULT 0,
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at  TIMESTAMPTZ NULL
);
CREATE INDEX proxy_nodes_status_idx ON proxy_nodes ((payload->>'status'));
```

落点:`crates/gw-store/migrations/0005_proxy_nodes.sql`(随 tunnel-t T1 一同 merged,本 spec 仅补契约文档)。

## 4.7 未决问题

1. 节点出口 IP 池 / 健康抖动指标的 schema 演化:`extra` JSONB 字段做兜底,稳定字段沉淀进 `ProxyNodeEntry` 主体。
2. 节点级双向心跳(Rust 主动 ping 节点 + 节点主动 ping Hub)是否要 reconcile:v1 由两条独立链路各自维护,运维侧观察 metric 差异。
````

### 2.3 `parts/03-shared-schema.md` §3.6 ID 前缀表新增行

在 `management_token` 行下方追加:

```
| proxy_node | `pn_` | Go 生成 |
```

### 2.4 `main.md` §2 文件组织表新增行

在 `parts/03-shared-schema.md` 行下方追加:

```
| [parts/04-proxy-nodes.md](parts/04-proxy-nodes.md) | **Proxy Nodes** | Rust tunnel client ↔ Go Hub 契约;节点 entity / 心跳分工 / status 状态机 |
```

---

## 3. `auth_configs` migration 落地

### 3.1 `parts/03-shared-schema.md` §3.1 migration 表新增行

在 "**待加** `0006_auth_configs.sql`" 行的位置(若尚未列出则在 0005 行下方追加),把 "**待加**" 标记**移除**,正式列入:

```
| `0006_auth_configs.sql` | `auth_configs`(T1-E 落地)|
```

### 3.2 `parts/03-shared-schema.md` §3.9 第 1 条整条删除

删除:

```
1. `auth_configs` 表的 migration(T1-E 待加);当前 `loader.rs:158-165` 已写 SQL,但表不存在 → 冷加载 always 失败 → T1-E 走宽松默认值。
```

剩余未决问题项编号顺延(原 #2 → #1,以此类推)。

### 3.3 Rust 落点

migration 文件:`crates/gw-store/migrations/0006_auth_configs.sql`:

```sql
CREATE TABLE auth_configs (
  id          TEXT PRIMARY KEY,
  payload     JSONB NOT NULL,
  version     BIGINT NOT NULL DEFAULT 1,
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at  TIMESTAMPTZ NULL
);
```

**不**在 migration 内 seed `global` 行,由 Go 端管理 UI / 初始化脚本写入(参 gaps spec ADR-G2-1 末段)。

---

## 4. VendorKey payload 加 `oauth` 子结构 + 新反向事件 op

### 4.1 `parts/03-shared-schema.md` §3.2.5 schema 末尾追加字段

在 §3.2.5 JSON 形态的 `"exclusive_capability_match": false` 之后追加:

```json
{
  ...
  "exclusive_capability_match": false,
  "oauth": null                   // 可选;null = 非 OAuth 型 key,走 api_key_enc 注入
}
```

### 4.2 `parts/03-shared-schema.md` §3.2.5 后新增子段 §3.2.5.1

````
### 3.2.5.1 OAuth 子结构(可选)

当 vendor_key 是 OAuth 型(如 Claude Code OAuth / Gemini OAuth client),`payload.oauth` 填充:

```json
{
  "access_token_enc": { /* AES-256-GCM 四元组,见 §3.4 */ },
  "refresh_token_enc": { /* AES-256-GCM 四元组 */ },
  "access_expires_at": "2026-05-22T12:00:00Z",
  "token_endpoint": "https://oauth.anthropic.com/token",
  "scope": ["chat.completions"]
}
```

**Writer 责任(Go)**:

- 加密字段由 Go OAuth task-worker 完成;明文绝不入库 / 日志 / 缓存;
- refresh 完成后**整体替换** `payload.oauth` 子对象 + `version + 1` + XADD `events:entity` op=upsert;
- `token_endpoint` 仅供 Go refresh 使用,Rust 端不读。

**Rust 行为**:

- `upstream_request_filter` 注入 auth header 时若 `oauth.is_some()`,用 `access_token`(解密后)代替 `api_key`(参 gaps spec ADR-G3-2);
- 临过期判定:`now >= access_expires_at - 60s` 仍使用现有 token,不主动 refresh,仅 emit metric `gw_oauth_token_near_expiry_total{vendor_key_id}`;
- 上游 401 → `OAuthListener` 触发反向 `events:entity`(参 [parts/02 §2.4.3](../parts/02-go-as-consumer.md#243-vendor_keyoauth_failedoauth-失败通知))。
````

### 4.3 `parts/02-go-as-consumer.md` §2.4 新增 §2.4.3

把现有 §2.4.3(不变量)顺延为 §2.4.4,并在原 §2.4.2 之后插入新 §2.4.3:

````
### 2.4.3 `vendor_key.oauth_failed`(OAuth 失败通知)

| 触发 | 上游返回 401 且 `vendor_key.oauth.is_some()`,经 `OAuthListener` 判定为 `AccountBlocked(OAuthRefreshFailed)` |
|---|---|
| Payload | `ns=vendor_key id=<key_id> op=status_change field=oauth_failed reason=upstream_401 ts_ms=<now>` |
| Go 接收行为 | task-worker 检查 `refresh_token` 有效性 → 尝试 refresh:<br/>① 成功:加密新 access_token + 整体替换 `payload.oauth` 子对象 + `version + 1` + XADD `events:entity ns=vendor_key op=upsert`;<br/>② 失败:`status = disabled` + XADD `events:entity ns=vendor_key op=upsert` |
| Rust 后续 | **不**主动 refresh;等 Go 写新 vendor_key payload 后自然冷加载到新 token |

不变量:与 §2.4.1(`rate_limited`)、§2.4.2(`ipblock`)并列,均属"Rust 反向写,Go 接收"的白名单 op。
````

### 4.4 `parts/02-go-as-consumer.md` §2.4.4 不变量段调整

原 §2.4.3 内容(不变量段)顺延为 §2.4.4;并在不变量列表中追加一行:

```
- §2.4.3 oauth_failed:Rust 反向写,Go task-worker 端必须实现 refresh 处理;未实现的代价 = 该 vendor_key 失效后无法自愈,等同 §2.4.4 表中"Go 未实现接收 → vendor_key 状态不会同步到 PgSQL"。
```

### 4.5 Rust 落点

- `crates/gw-core/src/entity.rs::VendorKey` 增字段 `oauth: Option<OAuthCredentials>`(`serde(default)`);
- 新结构 `OAuthCredentials { access_token_enc, refresh_token_enc, access_expires_at, token_endpoint, scope }`;
- `crates/gw-proxy/src/filters/upstream.rs::upstream_request_filter` 注入分支;
- `OAuthListener`(`crates/gw-adapter/src/analyzer.rs` 或 proxy 内 error fanout)增加反向 XADD 路径;
- 新 metric `gw_oauth_token_near_expiry_total` / `gw_oauth_token_expired_total`(`crates/gw-gateway/src/metrics_catalog.rs`)。

---

## 5. `gemini_file_mapping` writer 翻转

### 5.1 `parts/03-shared-schema.md` §3.1.2 修改 writer 行

原文:

```
- **`gemini_file_mapping`**(0002):Gemini 文件去重映射。Writer:Rust `gw-codec::gemini`(多模态请求时写)。Reader:Rust 自身。Go **不**接触此表。
```

改为:

```
- **`gemini_file_mapping`**(0002):Gemini 文件去重映射。Writer:**Go**(`/api/admin/gemini-files/*` 端点接收上传 → 推送 Gemini File API → 写入)。Reader:Rust(多模态请求时按 `(sha256_hex, user_id)` PK 校验 `file_uri` 归属)。无 `events:entity` ns(非 entity 表);Rust 直接查 PgSQL,命中即用。
```

### 5.2 Rust 落点

- 若 `crates/gw-codec/src/gemini/` 内存在写入 `gemini_file_mapping` 的代码 → 删除;
- 多模态请求处理路径保留 `SELECT file_uri FROM gemini_file_mapping WHERE sha256_hex = $1 AND user_id = $2`;
- 删除 = breaking change(参 [feedback_breaking_changes_ok](../../.claude/projects/-Users-whereslow-Desktop-work-ailinkmax-gateway/memory/feedback_breaking_changes_ok.md))。

---

## 6. `UsageRecord` 加 `operation_id` 可选字段

### 6.1 `parts/02-go-as-consumer.md` §2.1.2 JSON schema 在 `is_stream` 字段下方插入

```json
{
  ...
  "is_stream": true,
  "operation_id": null,            // 可选;Gemini :predictLongRunning / OpenAI /v1/videos 返回的 operation name
  "freeze_amount_micro": 12340,
  ...
}
```

### 6.2 `parts/02-go-as-consumer.md` §2.1.2 schema 表后追加段

````
**`operation_id`** 含义:

- 来源:Rust `response_body_filter` 在解析上游响应 body 时,若发现顶层 `name: "operations/..."` 字段(Gemini long-running operation / OpenAI video 等),抽取并填入;
- 普通 chat / completions 请求该字段为 `null`;
- Go task-worker 用 `operation_id` 关联 `video_tasks` / `async_tasks` 表与 usage records;
- 后续轮询 `/v1beta/operations/{operation_id}` 等由 Go task-worker 直接打上游,**Rust gateway 不再介入**;
- 同一 `operation_id` 可能产生**多条** `UsageRecord`(初始 POST + 轮询完成的 finalize 写入),Go settler 自行 dedup / aggregate。

`serde(default)` / Go 端 omitempty 兼容:不存在 / 缺失视为 `null`。
````

### 6.3 Rust 落点

- `crates/gw-ledger/src/usage.rs::UsageRecord.operation_id: Option<String>`(默认 `None`);
- `gw-codec` 各 codec `parse_response` 增 operation_id 抽取(Gemini 优先,其它 vendor 按需扩);
- `crates/gw-proxy/src/filters/response.rs::response_body_filter` 非流式分支 / 流式 End 事件均填 `ctx.operation_id`;
- `crates/gw-proxy/src/filters/logging.rs` 构建 `UsageRecord` 时填入。

---

## 7. Rust invalidate 时清 Redis L1

### 7.1 `parts/01-go-as-producer.md` §1.2.4 末尾追加段

在 "**漏发 XADD**:Rust 等待 Moka TTL 到期..." 段之后追加:

````
**Redis L1 invalidate(Rust 端补口)**:

Rust StreamConsumer 收到 `events:entity` 后,**在 invalidate Moka 之前**额外执行:

```
DEL cache:{ns}:{id}
```

这是为了多 pod 部署时 Redis L1 不残留陈旧值(详见 [`docs/architecture.md`](../../architecture.md) §10.5 已知问题)。Go 端**写顺序无变更**;Rust DEL 失败仅 `tracing::warn!`,不阻塞 Moka invalidate。

幂等性:任意 pod 收到 `events:entity` → DEL Redis L1 → Moka invalidate;多 pod 同时收同一事件即多次 DEL,无害。

启动期 Redis L1 陈旧不在本补口范围:Rust 启动锚点是 `$`,启动前的事件不消费;Redis L1 中陈旧 entity 在 TTL 内仍可能命中(最长 30 min for vendor_key / pricing / model_group)。如要彻底解决,需要 Go 主动 DEL 或 Rust 全量预热(留 follow-up,不在本契约范围)。
````

### 7.2 `parts/02-go-as-consumer.md` 新增 §2.11

在 §2.10(本变更 §1.4 新增的 `mgmt_token:blacklist`)之后插入(后续 §2.11 未决问题顺延为 §2.12):

````
## 2.11 Redis L1 失效一致性责任

Rust 端在消费 `events:entity` 时会 `DEL cache:{ns}:{id}`(详见 [parts/01 §1.2.4](../parts/01-go-as-producer.md#124-写顺序与重试) 补口段)。Go 端**无需**主动 DEL `cache:{ns}:{id}` 来配合;Go 写 entity 的三步流程(PgSQL UPDATE → 可选 Redis HSET write-through → XADD `events:entity`)不变。

**不变量**:

- 任意 Rust pod 收到 `events:entity` → DEL Redis L1 是幂等的;
- 多 pod 同时收同一事件多次 DEL 无害;
- Rust DEL 失败 → 仅 warn;下次该 entity 被读时,Moka miss → Redis L1 命中陈旧值(若 Go 未 write-through 则不存在)→ 写回。Go 写顺序保证 PgSQL 已经更新,所以即便 Redis L1 命中陈旧,后续 Moka TTL 到期后冷加载仍能拿到 PgSQL 新值。

**例外**:启动期 Redis L1 陈旧(Rust 启动用 `$` 锚点,启动前事件不消费)不在本节范围,见 [parts/01 §1.2.4 补口段](../parts/01-go-as-producer.md#124-写顺序与重试) 末段。
````

### 7.3 Rust 落点

`crates/gw-store/src/stream_consumer.rs::apply_entity_event`:在调用 `EntityStore::invalidate_*` 之前 `redis.del(format!("cache:{}:{}", ns, id)).await`,错误仅 warn。

---

## 8. PR 顺序与回滚

### 8.1 推荐 PR 拆分

PR 之间互相独立,可任意顺序;每个 PR **同时**改 Rust 代码 + parts 文件 + 必要 migration。本 changelog 与 spec 文件**先于**所有 PR 合入(作为"已设计"的标记),实施 PR 引用本 changelog 对应 §段。

| PR | 覆盖 § | 主要文件 |
|---|---|---|
| 阻塞 PR-A: B1 非流式 | (无 contract change)| `gw-codec/{openai,anthropic,gemini}` + `gw-proxy/filters/{request,response}` |
| 阻塞 PR-B: B2 mgmt token | §1(本 changelog)| `gw-core::entity` + migration 0006 + `gw-auth::mgmt_token` + `gw-proxy::filters::request` + `gw-gateway::health` + parts/01,02,03 |
| 缺口 PR-1: proxy_node | §2 | `gw-core::entity` + migration 0007 + `EntityStore.proxy_node` + parts/01 + parts/03 + parts/04(新)+ main.md |
| 缺口 PR-2: auth_configs migration | §3 | migration 0005 + parts/03 |
| 缺口 PR-3: vendor_key oauth | §4 | `gw-core::entity` 扩 + `gw-proxy::filters::upstream` + OAuthListener + parts/02,03 |
| 缺口 PR-4: gemini_file_mapping writer | §5 | 删 Rust 写路径(若存在)+ parts/03 |
| 缺口 PR-5: UsageRecord operation_id | §6 | `gw-ledger::usage` + codec 抽取 + parts/02 |
| 缺口 PR-6: Redis L1 invalidate | §7 | `gw-store::stream_consumer` + parts/01,02 |

### 8.2 回滚

每条契约改动均可独立 revert:

- §1 / §2 / §3:删表 / 删 migration / 撤回 parts 修改;Rust 端对应 `*_or_load` 调用回到"实体不存在 → fallback"分支;
- §4(oauth):`oauth = None` 兜底分支保证非 OAuth key 不受影响;若需 revert,把 `payload.oauth` 字段移除,Rust 端 `serde(default)` 兼容;
- §5(gemini files):writer 翻转可逆 —— 把 writer 重新交给 Rust(沿用历史代码);
- §6(operation_id):字段 optional,删除字段不影响现有 settler;
- §7(Redis L1 DEL):移除 Rust DEL 调用即可,数据状态不变。

---

## 9. 后续 changelog 入口

本 contract-change 目录(`docs/go-contracts/contract-change/`)的作用是:

- 每个**跨 spec 的契约改动包**写一份 `<YYYY-MM-DD>-<topic>.md`,引用对应 spec(可多个)+ 列出 parts 改动点 + Rust 落点;
- parts/ 内容的"最终文本"在 changelog 内给出,parts 文件的实际正文修改在实施 PR 内完成;
- 新加 part(如本 changelog 的 parts/04)的完整内容也在 changelog 内**预演**,实施 PR 创建文件时按 changelog 内嵌内容生成。

下一次进入本目录的契约变更示例:计费 c-δ v2 三层 DLQ 落地、`gw-core::ir` → `gw-core::schema` rename、JWT 黑名单分布式同步,等等。
