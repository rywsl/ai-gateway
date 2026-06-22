# Part 03 — Shared Schema（PgSQL / 加密 / 版本协议）

> 本 part 描述 **双方都接触** 的持久化 schema：PgSQL 表清单、每个 entity 的 payload JSONB 形态、`version BIGINT` 协议、AES-256-GCM 加密 payload 形式、KEK 轮换状态机。索引见 [main.md](../main.md)。

---

## Purpose

锁定 Go 管理面和 Rust 网关共享的持久化 schema、JSONB payload、加密封包和版本协议，避免两端各自解释同一字段。

## As-built surface

- Rust entity payload 类型由 `crates/gw-core/src/entity.rs` 反序列化，PgSQL loader 在 `crates/gw-store/src/loader.rs` 折叠 top-level 列后加载。
- Vendor OAuth runtime token 由 `crates/gw-adapter/src/vertex.rs` 消费，Kiro SigV4 credential plaintext 由 `crates/gw-adapter/src/kiro.rs` 消费。
- Protocol and shape wire strings come from `crates/gw-core/src/protocol.rs`; Kiro EventStream/CLI share `ProtocolKind::Kiro` and diverge by `ApiShape`.

## Not built

- Kiro CLI detector and usage extraction are shape-registered only; production detection requires a real CLI traffic sample.
- Go-side generated types are contract consumers and are not emitted from this markdown automatically.

## 3.1 PgSQL 表清单

Migrations 位于 `crates/gw-store/migrations/`，**不**自动应用——开发期由 `psql -f` 跑，生产由 Go 端 / 运维负责执行。Rust 启动期假定所有表已就位。

| Migration | 引入的表 / 列 |
|---|---|
| `0001_initial.sql` | `users`、`user_groups`、`api_keys`、`vendors`、`vendor_keys`、`model_groups`、`pricings`、`jwks`、`ip_blocks` |
| `0002_gemini_file_mapping.sql` | `gemini_file_mapping`（非 entity，文件去重） |
| `0003_model_mapping.sql` | `model_mapping`（非 entity，alias 路由） |
| `0004_capabilities.sql` | `models`（新表）+ `vendor_keys.forced_capabilities` JSONB 列 + `vendor_keys.exclusive_capability_match` BOOLEAN 列 |
| `0005_proxy_nodes.sql` | `proxy_nodes`（tunnel-t T1，已 merged） |
| `0006_auth_configs.sql` | `auth_configs`（T1-E 落地） |
| `0007_management_tokens.sql` | `management_tokens`（B2，T1-D 落地） |
| `0008_signature_secret.sql` | `api_keys.signature_secret BYTEA`（HMAC 独立密钥列；当前实现按原始 BYTEA 明文读取，**不是** KEK 加密 at rest；S-proxy-core H-1 的加密迁移需 Go+Rust 同 PR 定义 envelope） |
| `0009_egress_impersonation.sql` | `vendors.egress_mode` JSONB + `vendor_keys.identity_snapshot` JSONB（egress-impersonate 身份持久化） |
| `0010_vendor_key_quota_state.sql` | `vendor_keys.quota_*` 结构化列（Rust 运行时写入 quota snapshot，Go 只读/管理展示） |
| Go `00080_error_message_rules.sql` | `error_message_rules`（Go 管理面写入，Rust 只读全局 snapshot） |

### 3.1.1 公共控制列（所有 entity 表）

```sql
id          TEXT        PRIMARY KEY,
payload     JSONB       NOT NULL,
version     BIGINT      NOT NULL DEFAULT 0,
updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
deleted_at  TIMESTAMPTZ NULL
```

- `id`：业务键。命名前缀见 [main.md](../main.md) §3.1。
- `payload`：JSONB，权威 entity 数据；schema 见 §3.2。
- `version`：BIGINT，单调递增；写入语义见 §3.3。
- `updated_at`：写入时 `now()`，用于运维排查。**不**作版本依据（NTP 抖动）。
- `deleted_at`：soft-delete。Rust 冷加载 SQL 一律附 `WHERE deleted_at IS NULL`（见 `loader.rs:143-151`）。Go 端**禁止**物理 DELETE。

### 3.1.2 非 entity 辅助表

- **`gemini_file_mapping`**（0002）：Gemini 文件去重映射。Writer：**Go**（`/api/admin/gemini-files/*` 端点接收上传 → 推送 Gemini File API → 写入）。Reader：Rust（多模态请求时按 `(sha256_hex, user_id)` PK 校验 `file_uri` 归属）。无 `events:entity` ns（非 entity 表）；Rust 直接查 PgSQL，命中即用。
- **`model_mapping`**（0003）：用户 alias → 上游真实 model 名映射。Writer：Go（管理面 UI）。Reader：Rust `gw-codec` model alias 解析。详见 §3.2.11。
- **`error_message_rules`**（Go migration 00080）：可配置错误消息改写规则。Writer：**Go**
  `errormessage` module。Reader：Rust `ErrorMessageRuleStore`，启动冷加载并在
  `events:entity ns=error_message_rules id=global` 后整表 reload + compile。
  这是全局 snapshot 表，不是普通 `EntityStore` Moka entity。

### 3.1.3 `vendor_keys` 生产形态：可写 VIEW over `provider_keys`（CNxus-Rust #11）

Rust migrations 把 `vendor_keys` 建成**真实 TABLE**（`0001` + `0009` `identity_snapshot` + `0010` `quota_*`），开发/测试期 `psql -f` 即可。**但 Go 生产** schema 把 `vendor_keys` 暴露为 `provider_keys` 之上的**可写 VIEW**（Go 迁移 `00060_vendor_keys_write_compat.sql`）。Rust 不感知底层是 TABLE 还是 VIEW——它只发出下列 UPDATE。因此 Go 生产的 VIEW（经 `INSTEAD OF` 触发器或 updatable-view 规则）**必须**保持以下列 / JSONB 路径可写，否则对应 Rust 运行时写路径静默失败：

| Rust writer | 代码 | 必须可写的列 / JSONB 路径 |
|---|---|---|
| OAuth runtime refresh | `crates/gw-store/src/oauth_refresh.rs::update_sql` | `payload`（`jsonb_set '{oauth}'`）、`version`、`updated_at` |
| Egress identity 快照 | `crates/gw-store/src/identity_persist.rs::update_sql` | `identity_snapshot`、`updated_at`（条件 `identity_snapshot IS NULL`） |
| Quota 快照 | `crates/gw-store/src/quota_state.rs::update_sql` | `quota_availability`、`quota_remaining`、`quota_limit`、`quota_reset_at`、`quota_observed_at`、`payload`（`jsonb_set '{quota_state}'`）、`version`、`updated_at` |

公共谓词：所有写均带 `WHERE id = $1 ... AND deleted_at IS NULL`，OAuth 额外带 `AND version = $2`（乐观 CAS）。Go 的 VIEW / 触发器必须把这些列映射回 `provider_keys` 对应列并保持 `version` 单调（§3.3.2）。该列集由契约测试 `crates/gw-store/tests/vendor_keys_write_contract.rs` 锁定；Rust 侧任何列面变更都会破坏该测试并强制同步本节。

> **不变量**：Rust 只写本表既有列；新增任何列都属于跨进程改动，必须 Go+Rust 同 PR 并更新本 part。

### 3.1.4 key-health / cooldown 不持久化（CNxus-Rust #12）

Key-health 熔断 / cooldown 状态是 **per-replica、纯内存**的（`crates/gw-health/src/format_health.rs` 原子量；`crates/gw-health/src/lib.rs` "No persistence in v1"；`docs/spec/03-key-health.md`）。`vendor_keys`（及任何表）**没有**、也**不应**有 `cooldown_*` / `consecutive_failures` / `circuit` / `health_state` 等列。唯一例外是 §3.2.5「0010 quota_state 列」——quota-remaining 是结构化持久列，但它是 quota 可用性，不是熔断/cooldown。Go 不得假设可从 PG 读取 Rust 的熔断/cooldown 状态，也不得新增此类列期望 Rust 写入。该约束由 `vendor_keys_write_contract.rs::no_vendor_keys_writer_persists_cooldown_or_health_state` 锁定。

## 3.2 Entity payload JSONB schemas

每个表的 `payload` JSONB 字段对应 Rust 端类型，权威定义在 `crates/gw-core/src/entity.rs`。Go 写入时**必须**保持字段名 / 类型与 Rust 端一致；新增字段必须在两边同 PR 落。

> **关键约束**：Rust 端用 `serde` 反序列化，未知字段被 `serde(default)` 兜底或忽略；缺失必填字段直接 deserialize 失败 → Rust 视为该 entity 不可用。Go 端禁止删除任何已存在字段，除非 Rust 同 PR 移除使用。

### 3.2.0 `error_message_rules` — global error-message rewrite snapshot

This table is Go-owned and Rust read-only:

```sql
CREATE TABLE error_message_rules (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  enabled BOOLEAN NOT NULL DEFAULT TRUE,
  priority INTEGER NOT NULL DEFAULT 1000,
  match_json JSONB NOT NULL DEFAULT '{}'::jsonb,
  message TEXT NOT NULL,
  notes TEXT,
  version BIGINT NOT NULL DEFAULT 1,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL,
  deleted_at BIGINT
);
```

`match_json` has this wire shape. Empty arrays mean wildcard for that dimension:

```json
{
  "provider_ids": ["provider_openai"],
  "models": ["gpt-4o"],
  "endpoint_ids": [],
  "api_formats": ["openai:chat"],
  "request_paths": ["/v1/chat/completions"],
  "client_protocols": ["openai:chat"],
  "gateway_codes": ["GW_UPSTREAM_INVALID_REQUEST"],
  "http_statuses": [400],
  "upstream_message_contains": ["image"]
}
```

Go publishes Redis HASH `entity:error_message_rules:global`:

```json
{
  "v": "42",
  "enc": "json",
  "d": "[{\"id\":\"emr_...\",\"enabled\":true,\"priority\":10,\"match\":{...},\"message\":\"...\",\"version\":42}]"
}
```

`d` is a JSON array of enabled, non-deleted rules sorted by
`priority ASC, id ASC`. Rust repeats validation during compile: max 5000 enabled
rules, max 50 values per exact-match dimension, max 10 message substrings, max
256 bytes per substring, max 1000 bytes per replacement message, bounded
expanded tree branches, no regex/script/expression markers. Runtime matching is
a fixed-dimension tree followed by bounded case-insensitive substring checks over
the first 8 KiB of the upstream message. A match rewrites only the client-visible
`message`; gateway code, error type, protocol envelope, and HTTP status remain
unchanged.

`endpoint_ids` is an active schema field, but Rust first version only matches it
when `selected_endpoint_id` is available in request context. Current
`VendorKey` projection does not carry endpoint row identity, so the gateway
sets it to `None` until that projection is extended.

### 3.2.1 `users.payload` — `UserEntry`

```json
{
  "user_id": "u_abc",
  "parent_id": null,             // Option<UserId>，子账号场景
  "tier": "pro",                 // 自由字符串
  "status": "active",            // active | banned
  "allowed_models": [],
  "allowed_channels": [],        // Vec<KeyId>，可空表示全允许
  "allowed_groups": [],
  "rpm_limit": 0,                // 0 = 不限
  "tpm_limit": 0,
  "max_concurrent_streams": 100,
  "daily_quota_tokens": 0,       // 0 = 不限；非 0 走 G3 daily counter
  "user_group_ids": [],
  "markup": 1.0,                 // 用户级 markup（5-tier 之一）
  "debug_enabled": false         // 命中后写 usage:detailed
}
```

### 3.2.2 `user_groups.payload` — `UserGroup`

```json
{
  "id": "ug_premium",
  "display_name": "Premium",
  "priority": 0,
  "allowed_models": [],
  "allowed_channels": [],
  "model_group_binding": null,   // Option<Group>
  "rpm_limit": 0,
  "tpm_limit": 0,
  "max_concurrent_streams": 0,
  "markup": 1.0
}
```

### 3.2.3 `api_keys.payload` — `ApiKeyEntry`

```json
{
  "key_id": "ak_xxx",
  "user_id": "u_abc",
  "status": "active",            // active | disabled
  "hash": "<bytes base64>",      // bcrypt / argon2 / sha256 哈希；Rust serde 字节
  "allowed_models": [],
  "allowed_channels": [],
  "rpm_limit": 0,
  "tpm_limit": 0,
  "markup": 1.0,                 // api_key 级 markup
  "ip_allowlist": null,          // Option<Vec<String>>，CIDR 字符串
  "expires_at": null,            // RFC3339 / time::OffsetDateTime
  "signature_required": false,   // HMAC body signing 开关
  "signature_algo": "hmac_sha256",
  "signature_secret": null,      // Option<Vec<u8>>，独立 HMAC 密钥；null 时签名不可用。当前由 loader 从 api_keys.signature_secret BYTEA 注入原始字节数组，非 KEK 加密 envelope
  "force_capabilities": [],      // SmallVec<[String;4]>，caller-side 必需 capability
  "match_mode_exclusive": false
}
```

> `signature_secret` 当前真实形态：`crates/gw-store/src/loader.rs` 用 `jsonb_agg(get_byte(signature_secret, i))` 把 BYTEA 折叠为 JSON 整数数组，`ApiKeyEntry.signature_secret: Option<Vec<u8>>` 直接收到这些原始字节。任何把该列改为 KEK envelope 的迁移都必须先定义 BYTEA/envelope wire shape，并让 Rust loader dual-read 后，Go 才能开始写新格式。

> 字段命名差异：`ApiKeyEntry.force_capabilities` (singular `force_`) vs `VendorKey.forced_capabilities` (plural `forced_`)。这是 [G13 已知 gap](../../architecture.md#21-crate-responsibilities)，Go 端需 schema 适配。

### 3.2.4 `vendors.payload` — `Vendor`

```json
{
  "id": "v_anthropic",
  "display_name": "Anthropic",
  "base_urls": ["https://api.anthropic.com"],
  "auth_template": "x-api-key: {api_key}",  // 模板，{api_key} 占位
  "timeout_overrides": {
    "connect_ms": 5000,
    "read_ms": 30000,
    "total_ms": 60000,
    "stream_total_ms": 600000
  },
  "default_proxy": null,         // Option<Url>
  "contact": null,
  "egress_mode": { "mode": "rustls" }
}
```

`egress_mode` 由 migration 0009 引入，默认 `{"mode":"rustls"}`。启用 impersonate 时 Go 写入：

```json
{
  "mode": "impersonate",
  "profile": "claude_code_chrome131"
}
```

支持的 `profile` 字面量与 `gw_core::impersonate::ImpersonateProfile` 的 serde tag 对齐。Go 控制面写入 `mode = impersonate` 前必须执行 vendor-validation 契约：不得与 tunnel-eligible keys 同时启用（见 [`contract-change/2026-05-29-vendor-validation.md`](../contract-change/2026-05-29-vendor-validation.md)）。

### 3.2.5 `vendor_keys.payload` — `VendorKey`（含加密 api_key）

```json
{
  "key_id": "vk_xxx",
  "vendor_id": "v_anthropic",
  "api_key_enc": { /* AES-GCM 四元组，见 §3.4 */ },
  "accessible_models": ["claude-3-5-sonnet"],
  "model_mapping": {},           // HashMap<ModelName, String>，per-key model override
  "accessible_endpoints": ["anthropic"],   // ProtocolKind 列表
  "protocol_conversion": false,  // 是否允许 cross-protocol 路由到此 key
  "priority": 0,
  "weight": 1,
  "rpm_limit": 0,
  "tpm_limit": 0,
  "max_usage_count": null,       // Option<u64>
  "proxy": null,                 // Option<Url>，per-key 代理
  "ip_allowlist": null,
  "expires_at": null,
  "status": "active",            // active | disabled | expired | depleted | rate_limited | rotated | deleted
  "tags": [],
  "forced_capabilities": [],     // Vec<String>，channel-side advertised capability
  "exclusive_capability_match": false,
  "tunnel_preference": null,      // Option<TunnelPreference>，见下文
  "oauth": null,                  // 可选;null = 非 OAuth 型 key,走 api_key_enc 注入
  "identity_snapshot": null       // Rust loader 从 top-level 列折叠进 payload；Go 不直接写 payload 内字段
}
```

`accessible_endpoints` 使用 Rust `ProtocolKind::as_str()` / JSONB wire string。当前标准协议字符串为 `openai`、`anthropic`、`gemini`、`kiro`。Kiro EventStream and Kiro CLI share `ProtocolKind::Kiro` / `kiro`; EventStream uses `ApiShape::KiroEventStream` with `ApiShape::format_id() == "kiro:eventstream"`, while CLI uses `ApiShape::KiroCli` with `ApiShape::format_id() == "kiro:cli"`. Runtime code must choose by `ApiShape`, not by protocol string alone.

> **0004 列冗余说明**：migration 0004 在 `vendor_keys` 表上**同时**加了 top-level 列 `forced_capabilities JSONB` 与 `exclusive_capability_match BOOLEAN`，但 Rust 实际通过 `payload.forced_capabilities` / `payload.exclusive_capability_match` 反序列化（见 `loader.rs:101-141`）。top-level 列**仅供 Go 端建索引或查询**，不是 Rust 数据来源。Go 写入时**必须**同时写两处，否则 Rust 看不到。

> **0009 `identity_snapshot` 列说明**：`vendor_keys.identity_snapshot` 是 top-level JSONB 列，不是 Go 写入 `payload` 的字段。Rust `load_vendor_key` 用 `jsonb_set(payload, '{identity_snapshot}', COALESCE(identity_snapshot, 'null'::jsonb), true)` 将该列折叠进反序列化 payload。Rust 首次 impersonated dispatch 通过 `PgSnapshotWriter::write_if_null` 懒写该列；Go 的 regenerate-identity 管理动作必须 `SET identity_snapshot = NULL, version = version + 1` 并发出 `events:entity ns=vendor_key`，详见 [`contract-change/2026-05-29-vendor-validation.md`](../contract-change/2026-05-29-vendor-validation.md)。

> **0010 `quota_state` 列说明**：`vendor_keys.quota_availability`、`quota_remaining`、`quota_limit`、`quota_reset_at`、`quota_observed_at`、`quota_source` 是 Rust 运行时 quota snapshot writer 的结构化列。Rust `load_vendor_key` 将这些列折叠为 `payload.quota_state`，路由在候选过滤阶段跳过 `availability = exhausted` 且未到 reset 的 key。Go 可读取这些列做报表/管理展示，但不得作为运行时 writer；Rust 写入后会自增 `version` 并发出 `events:entity ns=vendor_key op=quota_state`。

#### Kiro SigV4 `api_key_enc` plaintext shape

For Kiro vendor keys, the decrypted `api_key_enc` plaintext is not a raw bearer token. It is JSON consumed by Rust `KiroAdapter`:

```json
{
  "access_key_id": "AKIA...",
  "secret_access_key": "aws-secret",
  "session_token": null,
  "region": "us-east-1",
  "service": "bedrock"
}
```

`session_token` is optional and `service` defaults to `execute-api` when omitted. Go still stores this value through the existing encrypted `api_key_enc` envelope; Rust decrypts it into `VendorKey.api_key` and redacts it through `SecretString`.

#### `tunnel_preference` 子结构(可选)

```json
{
  "preferred_node_ids": ["node-a"],
  "require_tunnel": false,
  "prefer_low_latency": false
}
```

`require_tunnel = true` 表示没有匹配 tunnel node 时 Rust 必须 fail-closed（`GW_TUNNEL_NO_NODE`），不能静默回落 Direct。Go 控制面必须同时执行 EI-8 vendor-validation：任何 tunnel-eligible key（`require_tunnel = true` 或 Go 侧 node mapping）不得与所属 vendor 的 `egress_mode = impersonate` 同时存在。

### 3.2.5.1 OAuth 子结构(可选)

当 vendor_key 是 OAuth 型(如 Claude Code OAuth / Gemini OAuth client),`payload.oauth` 填充:

```json
{
  "access_token_enc": { /* AES-256-GCM 四元组,见 §3.4 */ },
  "refresh_token_enc": { /* AES-256-GCM 四元组 */ },
  "access_expires_at": "2026-05-22T12:00:00Z",
  "token_endpoint": "https://oauth.anthropic.com/token",
  "scope": ["chat.completions"],
  "quota_project_id": null
}
```

**Writer responsibility after K-C**:

- Go writes OAuth configuration and initial authorization seed data.
- Rust writes runtime refresh results for `vendor_keys.payload.oauth`.
- Rust reads `token_endpoint` for vendor-key runtime refresh.
- Vertex/Google keys use this same OAuth runtime contract: W7 `VertexAdapter` consumes the W6 runtime access token and injects `Authorization: Bearer`; it reads `quota_project_id` to set `x-goog-user-project` when present. The adapter does not call PgSQL, Redis, or Google token endpoints directly.
- Rust encrypts refreshed tokens with the active KEK and persists with version CAS.
- Rust emits `events:entity ns=vendor_key op=upsert` after a successful write.
- Go must not perform runtime refresh write-back for this sub-object after K-C.

**Rust 行为**:

- `upstream_request_filter` 注入 auth header 时若 `oauth.is_some()`,通过 Rust `OAuthRuntime` 在 refresh skew 内先刷新,再用 runtime access token 代替 `api_key`;
- 临过期判定:`now >= access_expires_at - 60s` 后不得继续无条件发送旧 token;Rust runtime 负责 refresh 或 fail-closed;
- 上游 401 → `OAuthListener` 可触发 status-only `events:entity`,Go 不得将该事件作为 runtime refresh command。

### 3.2.5.2 Capability 字段契约（`forced_capabilities` / `exclusive_capability_match`）

Migration 0004 同时在 `vendor_keys` 上引入了 **JSONB payload 字段**与**top-level 列**。Rust 端只读 JSONB；Go 端建索引、查询走 top-level 列。本节规定两侧的 schema 形态、索引激活、双写不变量。

#### JSONB 形态

`vendor_keys.payload` 中的两个字段：

```json
{
  "forced_capabilities": ["streaming", "tool_use"],   // Vec<String>，default []
  "exclusive_capability_match": false                  // bool，default false
}
```

| 字段 | 类型 | 默认值 | Rust 权威类型 | 说明 |
|------|------|--------|---------------|------|
| `forced_capabilities` | JSON array of strings | `[]` | `Vec<String>` | channel-side advertised capability；`CapabilityResolver` 在路由阶段读取 |
| `exclusive_capability_match` | JSON boolean | `false` | `bool` | `true` 时 resolver 仅在所有 required capability 全部命中时匹配；`false` 走宽松包含检查 |

#### Top-level 列映射

| Top-level 列 | PgSQL 类型 | 映射 |
|---|---|---|
| `forced_capabilities` | `JSONB` | 镜像 `payload.forced_capabilities` |
| `exclusive_capability_match` | `BOOLEAN DEFAULT false` | 镜像 `payload.exclusive_capability_match` |

#### Go 端索引激活

```sql
-- partial index：仅覆盖 exclusive_capability_match = true 的行（少数派）
CREATE INDEX idx_vendor_keys_excl_cap
    ON vendor_keys(exclusive_capability_match)
    WHERE exclusive_capability_match = true;
```

Go 端查询 `WHERE exclusive_capability_match = true` 命中此 partial index，避免全表扫描。`forced_capabilities` 列无索引需求（Rust 不查 top-level，Go 端按需 JSONB `@>` 查询即可）。

#### 双写不变量

Go 写入 `vendor_keys` 时**必须**同时更新：

1. `payload` JSONB 内的 `forced_capabilities` 与 `exclusive_capability_match`；
2. top-level `forced_capabilities` 列与 `exclusive_capability_match` 列。

两处**必须**语义一致。违反双写 → Rust 看到的 JSONB 与 Go 索引查询结果不一致，产生幽灵路由（Go 认为 key 是 exclusive 但 Rust 按 JSONB `false` 宽松匹配）或漏选（反向）。

推荐写法（单条 UPDATE 原子保证）：

```sql
UPDATE vendor_keys
   SET payload = jsonb_set(
         jsonb_set(payload, '{forced_capabilities}', $caps::jsonb),
         '{exclusive_capability_match}', $excl::jsonb
       ),
       forced_capabilities = $caps::jsonb,
       exclusive_capability_match = $excl_bool,
       version = version + 1,
       updated_at = now()
 WHERE id = $id;
```

#### 命名差异

| 侧 | Entity | 字段名 | 类型 |
|---|---|---|---|
| Rust（channel-side） | `VendorKey` | `forced_capabilities` | `Vec<String>` |
| Rust（caller-side） | `ApiKeyEntry` | `force_capabilities` | `SmallVec<[String;4]>` |
| Go（caller-side） | `ApiKeyEntry` | `force_capabilities` | `[]string` |

`VendorKey.forced_capabilities`（复数 `forced_`）与 `ApiKeyEntry.force_capabilities`（单数 `force_`）的命名/类型不对称是 **刻意的**，记录在 [`docs/architecture.md`](../../architecture.md) §8 Deliberate behaviours。两者由不同 wave 引入（T4-B vs T0-C），不做追齐 rename。Go 端需保持各自表的字段名不变。

#### 同步规则

任何 Rust 侧对 `forced_capabilities` / `exclusive_capability_match` JSONB layout 的变更（字段名、类型、语义）**必须**同 PR 更新 `docs/go-contracts/`，参见 [`CLAUDE.md`](../../../CLAUDE.md) sync rule table "Cross-process touchpoint with Go" 行。

### 3.2.6 `model_groups.payload` — `ModelGroup`

```json
{
  "group": "default",
  "strategy": "round_robin",     // fixed_order | round_robin | load_balance | weighted_rr | cost_first | latency_first | cache_affinity | sticky
  "cross_vendor_fallback": false,
  "max_tokens_cap": null,        // Option<u32>
  "default_params": {
    "temperature": null,
    "top_p": null
  },
  "forced_params": {
    "temperature": null,
    "top_p": null
  },
  "vendor_key_ids": ["vk_xxx", "vk_yyy"]
}
```

### 3.2.7 `pricings.payload` — `Pricing`（过渡期）

```json
{
  "model": "gpt-4-turbo",
  "input_price_micro_per_token": 30,
  "output_price_micro_per_token": 60,
  "max_output": 4096,
  "model_markup": 1.0,
  "connect_timeout_ms": null,
  "read_timeout_ms": null,
  "total_timeout_ms": null,
  "stream_total_timeout_ms": null
}
```

> 过渡期保留。目标态 Rust 仅读 [`entity:estimate_pricing`](01-go-as-producer.md#15-entityestimatepricingmodel-push-协议)（不入 PgSQL，Go 独立维护）。Go 端可继续维护 `pricings` 表作为内部 BillingPricing 来源，但 Rust 不再读。

### 3.2.8 `jwks.payload` — `JwksEntry`

```json
{
  "kid": "key-2026-05",
  "pem": "-----BEGIN PUBLIC KEY-----\n..."
}
```

ID = kid 字符串。Rust 在 RS256 验签时按 token header `kid` lookup。

### 3.2.9 `ip_blocks.payload` — `IpBlockEntry`

```json
{
  "cidr": "1.2.3.0/24",
  "scope": "global",             // global | { "user": "u_abc" } | { "api_key": "ak_xxx" }
  "reason": "manual",            // manual | abnormal_traffic | compliance
  "expires_at": null             // Option<OffsetDateTime>；abnormal_traffic 自动写时填 now+1h
}
```

ID = CIDR 字符串（与 `cidr` 字段同值，方便 Rust invalidate）。

### 3.2.10 `models.payload` — `Model`（0004）

```json
{
  "name": "claude-3-5-sonnet",
  "capabilities": ["streaming", "tool_use", "vision", "thinking"]
}
```

Capability 字符串由 `gw-capability::Capability` enum 解析，详见 [`docs/ROADMAP.md`](../../ROADMAP.md)。未知 capability 字符串 Rust 端忽略 + warn。

### 3.2.11 `model_mapping`（0003，非 entity）

| 列 | 类型 | 含义 |
|---|---|---|
| `user_alias` | TEXT | 用户面看到的 model 名 |
| `protocol` | TEXT | `openai \| anthropic \| gemini`（snake_case） |
| `default_upstream_model` | TEXT | 上游真实 model 名 |
| `vendor_key_overrides` | JSONB | `Map<vendor_key_id, upstream_model>`，per-key 覆盖 |
| `availability` | TEXT | `enabled \| disabled \| shadow_only` |
| `pricing_strategy_ref` | TEXT NULL | 指向 Go 内部 BillingPricing 选择器 |
| `updated_at` | TIMESTAMPTZ | — |

PK = `(user_alias, protocol)`。Rust 端通过 `events:entity` ns=`model_mapping` id=`"<user_alias>|<protocol>"` 接收失效。

### 3.2.12 `auth_configs.payload` — `AuthConfig`（待 migration）

```json
{
  "expected_iss": ["https://issuer.example.com"],
  "expected_aud": ["gateway-prod"],
  "leeway_secs": 60,
  "require_nbf": false,
  "blacklist_fail_mode": "fail_closed",    // fail_closed | fail_open
  "static_secret_hs256": "<base64 bytes>", // Option<Vec<u8>>，HS256 共享密钥
  "static_pem_rs256": "<PEM bytes>"        // Option<Vec<u8>>，RS256 静态 fallback
}
```

ID = `"global"`（well-known，T1-E 假定单一全局 AuthConfig）。

### 3.2.13 `management_tokens.payload` — `ManagementTokenEntry`（migration 0007）

```json
{
  "token_id": "mt_xxx",
  "user_id": "u_abc",
  "scope": "admin_full",         // admin_full | admin_read | user_self
  "hash": "<bytes base64>",      // SHA-256(secret)，与 ApiKeyEntry.hash 同范式
  "expires_at": null,            // Option<OffsetDateTime>；null = 永不过期
  "status": "active",            // active | disabled
  "display_name": "personal CI token"
}
```

ID = `token_id`（前缀 `mt_`）。Rust 行为：

- 客户端在 `Authorization: Mgmt mt_<id>.<secret>` 中携带；
- Rust（`crates/gw-auth/src/mgmt.rs`）用 `mt_<id>` 作为 entity key 经 `EntityStore::mgmt_token_or_load` 拉取；将 `<secret>` SHA-256 后与 `payload.hash` constant-time 比对；
- `scope` 用 `MgmtScope::covers(required)` 判定（`admin_full ⊇ admin_read ⊇ user_self`）；
- 撤销走 [`mgmt_token:blacklist:<token_id>`](02-go-as-consumer.md#29-mgmt_tokenblacklisttoken_id-redis-key)（FailClosed `EXISTS` 查询）；
- 失败码：`GW_AUTH_MGMT_TOKEN_{MISSING, INVALID, EXPIRED, DISABLED, REVOKED, SCOPE_DENIED}`（参 [docs/architecture.md](../../architecture.md) §6 health endpoints 与 [docs/workflow.md](../../workflow.md) §2.2 step 1）。

权威 Rust 类型定义见 `crates/gw-core/src/entity.rs::{MgmtScope, ManagementTokenEntry}`。

## 3.3 `version BIGINT` 协议

### 3.3.1 写入语义

每次 entity payload 变更：

```sql
UPDATE <table>
   SET payload = $1,
       version = version + 1,
       updated_at = now()
 WHERE id = $2
   AND deleted_at IS NULL
RETURNING version;
```

或 INSERT path（首次写）：

```sql
INSERT INTO <table> (id, payload, version, updated_at)
VALUES ($1, $2, 1, now())
ON CONFLICT (id) DO UPDATE
   SET payload = EXCLUDED.payload,
       version = <table>.version + 1,
       updated_at = now(),
       deleted_at = NULL
RETURNING version;
```

### 3.3.2 单调性

同一 `id` 的 `version` **必须**严格单调递增。**禁止**任何路径写回旧值：

- 不允许 admin tool 直接 SET version；
- 不允许 PgSQL 还原后不 bump version；
- 即使 payload 完全相同也要 +1（让 Rust 缓存失效有可观察事件）。

### 3.3.3 Rust 端用法

Rust 冷加载读到 `(payload, version)` 后封装为 `Versioned<T> { version, value: Arc<T> }`。当前实现（v1）**不**做 versioned upsert——StreamConsumer invalidate 后下次 miss 直接拿 PgSQL 新值。`version` 在 v1 仅用于：

1. metrics（监控陈旧度）；
2. v2 演进位（versioned upsert 进缓存以避免 invalidate-then-reload 的窗口期）；
3. UsageRecord 内 `estimate_pricing_version` 字段（审计 freeze 用了哪版）。

### 3.3.4 软删除（`deleted_at`）

`UPDATE <table> SET deleted_at = now(), version = version + 1 WHERE id = $1`。然后 XADD events:entity `op=delete v=<new_version>`。

Rust 行为：invalidate cache；下次冷加载 SELECT 因 `WHERE deleted_at IS NULL` 拿不到，cold-load wrapper 返回 `None`。等同于"该 entity 不存在"。

Go 端运维如要硬删除（超长归档），应当先确认无 Rust 在飞请求引用，且单条审计落地。

## 3.4 加密 payload — AES-256-GCM

`vendor_keys.payload.api_key_enc` 是四元组对象。**禁止**任何路径出现明文 `api_key`（PgSQL / Redis / 日志）。

### 3.4.1 JSON 形态

```json
{
  "ciphertext": [13, 118, 93, ...],   // 加密后字节，JSON 整数数组（Vec<u8>，非 base64）
  "nonce": [1, 2, 3, ...],            // 96-bit 随机 IV，每条独立，整数数组
  "auth_tag": [100, 0, 68, ...],      // 128-bit GCM tag，整数数组
  "kek_kid": "primary"               // KEK 标识，用于轮换匹配；默认/规范 kid 见下
}
```

字段顺序不限；Rust 端用 `serde` 按字段名反序列化（权威类型见 `crates/gw-store/src/encrypt.rs::EncryptedKey`）。
**`ciphertext` / `nonce` / `auth_tag` 是 `Vec<u8>`，serde 仅接受 JSON 整数数组 `[b0,b1,…]`，不接受 base64 字符串**（写入方必须按整数数组落库，否则整行 `EncryptedKey` 反序列化失败、该 vendor_key 不可用）。

**`kek_kid` 规范值：** 正常态下，网关把 `cfg.kms.key_id` 对应的 active KEK 装入 `KekCache`（`config/default.toml` 默认 `key_id = "primary"`，见 `crates/gw-gateway/src/kms.rs`）。轮换窗口内，网关可同时加载 `cfg.kms.previous_key_id` / `cfg.kms.previous_kek_hex` 作为 previous KEK，并让 active `kms.key_id` 用于新写入。因此 Go 写入 `api_key_enc.kek_kid` 必须等于当前 active `kms.key_id`；历史行可在轮换窗口内继续使用 `previous_key_id` 直到重加密完成。默认部署下 active kid 为 `"primary"`。轮换期下文以 `primary` / `<new-kid>` 表述（早先示例中的 `kek_v1`/`kek_v2` 仅为轮换占位说明，非默认字面量）。

### 3.4.2 加密算法

| 项 | 值 |
|---|---|
| 算法 | AES-256-GCM |
| 密钥长度 | 32 字节（KEK） |
| Nonce | 12 字节，CSPRNG，每条独立 |
| AAD | 暂无（v1 不绑定 vendor_id；v2 可考虑） |
| Auth tag | 16 字节 |

### 3.4.3 加密时机（Go 端）

- 用户 UI 输入 vendor key 明文 → Go 后端立即 AES-256-GCM 加密 → 写 `vendor_keys.payload.api_key_enc`；
- 明文**绝不**入数据库 / 日志 / 缓存；
- 加密所用 KEK 必须是 KMS 当前 active KEK（轮换期可能是新 KEK）。

### 3.4.4 解密时机（Rust 端）

- 仅在 `gw-store::loader::load_vendor_key` 内冷加载时解密（详见 `loader.rs:200-225`）；
- 解密后 `api_key` 包在 `SecretString` 内，Moka 缓存持有；
- Redis 写回 `entity:vendor_key:<id>` 仍是密文形态；
- Drop 时清零内存（`secrecy` crate 保证）。

### 3.4.5 失败模式

| 故障 | 结果 |
|---|---|
| `kek_kid` 不在 Rust 当前 KekCache 内 | 解密失败 → 该 vendor_key 不可用 → `gateway_vendor_key_decrypt_failures_total` +1 |
| `auth_tag` 验证失败 | 解密失败（同上）；密钥被篡改或 KEK 错误 |
| KMS 不可达（首次启动） | Rust fail-fast 拒启动 |

## 3.5 KEK 轮换状态机

详见 [`docs/architecture.md`](../../architecture.md) §4.3 + `crates/gw-store/src/encrypt.rs::KekCache`。

### 3.5.1 状态图

```
Healthy(K_old)
    │ enter_dual_read(K_new)
    ▼
DualRead { reencrypting: false }    # K_old + K_new 并存，K_new 是 active（新加密用）
    │ mark_reencrypting()
    ▼
DualRead { reencrypting: true }     # re-encryption job 运行中
    │ drop_old_kek(K_old)
    ▼
Healthy(K_new)
```

### 3.5.2 不变量

1. 同时最多 2 个 KEK 驻留内存；
2. 不能 drop 唯一 KEK（必须先 enter_dual_read 引入新的）；
3. 不能 drop active KEK（drop_old_kek 只能 drop 非 active）；
4. 不能从 DualRead 重复 enter_dual_read（顺序违规）。

### 3.5.3 Go / 运维端责任

1. 在 KMS 生成 K_new + 分配 `kek_kid`；
2. 将 K_new 作为 active `GW_KMS__KEK_HEX` / `GW_KMS__KEY_ID` 注入 Rust 集群所有 pod，并同时保留 K_old 到 `GW_KMS__PREVIOUS_KEK_HEX` / `GW_KMS__PREVIOUS_KEY_ID`；
3. 滚动重启 Rust（让所有 pod bootstrap 进入 DualRead）；
4. 跑 re-encryption batch：
   ```
   for row in vendor_keys WHERE payload->>'api_key_enc'->>'kek_kid' = 'kek_v1':
     plaintext  = decrypt(api_key_enc, K_old)
     new_enc    = encrypt(plaintext, K_new)
     UPDATE vendor_keys
        SET payload = jsonb_set(payload, '{api_key_enc}', new_enc),
            version = version + 1,
            updated_at = now()
      WHERE id = row.id;
     XADD events:entity * ns vendor_key id <id> v <new_version> op upsert;
   ```
5. 等待所有 `vendor_keys.kek_kid = 'kek_v1'` 重加密完 → 通知运维 drop K_old；
6. finalize K_old 二选一：
   - **命令式（推荐，无需重启）**：发 `rotate-kek` `step=drop-old` `old_kid=<K_old kid>`，Rust 在进程内 `KekCache::drop_old_kek` 回到 `Healthy(K_new)`（经状态机守卫 fail-closed）；可选先发 `step=mark-reencrypting` 标记、`step=status` 观测。命令通道**不传输** KEK 材料（详见 [parts/08 §8.4.3](08-control-plane.md#843-命令集--args--内部映射)）。
   - **重启式**：Rust 滚动重启移除 `GW_KMS__PREVIOUS_KEK_HEX` / `GW_KMS__PREVIOUS_KEY_ID`。
7. Rust 集群进入 Healthy(K_new)。

> **KEK 持久化契约（CNxus-Rust #20）**：KEK 材料只经环境变量（`GW_KMS__KEK_HEX` / `GW_KMS__PREVIOUS_KEK_HEX`）在 **boot** 注入，装入进程内 `KekCache`（`Clone` 共享一个 `Arc`）。**没有** durable `kek_keys` 表、也不把 KEK 材料写入 PgSQL / Redis——这是刻意的安全设计，且避免与 Go 迁移所有权冲突。新 KEK 的引入（`enter_dual_read`）只能在 boot 发生；`rotate-kek` 命令只能 finalize / 观测既有轮换，不能引入新 KEK。

### 3.5.4 失败模式

| 故障 | 结果 |
|---|---|
| re-encryption job 中途崩溃 | 部分 row `kek_kid = 'kek_v1'`、部分 `kek_kid = 'kek_v2'`；Rust DualRead 兼容两者；重启 job 续跑 |
| Go 在 Rust 集群进入 DualRead 前就跑 re-encryption | 部分 pod 解密失败 `kek_v2`（K_new 未加载）→ `vendor_key_decrypt_failures_total` 突增；告警，回滚 |
| Drop K_old 时仍有 row 用 K_old 加密 | Rust 收到这些 row 解密失败；告警；运维需把 K_old 重新挂回 |

## 3.6 ID 命名空间约定

| Entity | ID 前缀 | 来源 |
|---|---|---|
| user | `u_` | Go 生成 |
| user_group | `ug_` | Go 生成 |
| api_key | `ak_` | Go 生成 |
| vendor | `v_` | Go 生成 |
| vendor_key | `vk_` | Go 生成 |
| model_group | （无前缀，名字本身） | Go 配置 |
| pricing | （model name） | — |
| jwks | （kid 字符串） | — |
| ipblock | （CIDR 字符串） | Rust 或 Go |
| model | （model name） | — |
| model_mapping | `"<alias>\|<protocol>"`（在 stream 事件中） | — |
| auth_config | `"global"`（v1 单值） | — |
| management_token | `mt_` | Go 生成 |
| proxy_node | `pn_` | Go 生成 |

### 3.6.1 不变量

- 不同 entity 的 ID 命名空间**禁止**冲突；
- 同 entity 的 ID 一旦分配**禁止**复用（即便软删后），避免 stream 事件 id 复用导致 Rust 误 invalidate；
- ID 生成器（Go 端）建议用 ULID / Snowflake，**禁止**用自增整数（增加可枚举风险）。

## 3.7 监控义务（schema 层）

| 指标 | 来源 | 阈值 |
|---|---|---|
| `pgsql_entity_version_lag_seconds{ns}` | Go 端：`now() - max(updated_at)` 与 events:entity 最近 XADD ts 的差 | p99 > 5s |
| `pgsql_soft_deleted_total{table}` | Go 端：count where deleted_at IS NOT NULL | 趋势监控 |
| `vendor_key_kek_kid_distribution` | Go 端：按 `payload->'api_key_enc'->>'kek_kid'` group by | 轮换期监控 |
| `pgsql_schema_drift_detected_total` | Go 端 CI：实际 column set vs migration 声明 | > 0 → 告警 |

Rust 端对应指标 `gateway_entity_*` / `gateway_vendor_key_decrypt_failures_total` 详见 [`docs/architecture.md`](../../architecture.md) §9。

## 3.8 演进规则

| 改动 | 兼容性 |
|---|---|
| 新增字段（带 `serde(default)`） | 双方任意先上 |
| 新增 NOT NULL 字段 | 必须 DEFAULT 否则迁移失败；Rust 同 PR 加 `serde(default)` |
| 删除字段 | Rust 先移除使用 → 双方同 PR 改 schema → 下次部署 |
| 重命名字段 | 等同删旧加新；不保留兼容 alias（用户决议） |
| 列类型变化 | 必须双方同 PR；提供数据迁移脚本 |
| 新 entity 表 | 加 migration + part 03 §3.2 子段 + part 01 §1.2.3 ns 表 + Rust loader/EntityStore/StreamConsumer dispatch |

## 3.9 未决问题

1. `vendor_keys` 的 `forced_capabilities` / `exclusive_capability_match` top-level 列是否要 Rust 端用上（建索引查询）：**已规定，见 §3.2.5.2**。Rust 不查 top-level 列（仅 JSONB）；Go 端通过 partial index `idx_vendor_keys_excl_cap` 查询 `exclusive_capability_match = true`。双写不变量已落。
2. PgSQL `deleted_at` 后多久可硬清理：本契约不规定；建议 ≥ 30 天 + 审计追溯。
3. `gemini_file_mapping` 是否要进 Go 报表：当前仅 Rust 内部使用，Go 不接触；如需对外暴露需新增契约 part。
