# Part 03 — Shared Schema（PgSQL / 加密 / 版本协议）

> 本 part 描述 **双方都接触** 的持久化 schema：PgSQL 表清单、每个 entity 的 payload JSONB 形态、`version BIGINT` 协议、AES-256-GCM 加密 payload 形式、KEK 轮换状态机。索引见 [main.md](../main.md)。

---

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

## 3.2 Entity payload JSONB schemas

每个表的 `payload` JSONB 字段对应 Rust 端类型，权威定义在 `crates/gw-core/src/entity.rs`。Go 写入时**必须**保持字段名 / 类型与 Rust 端一致；新增字段必须在两边同 PR 落。

> **关键约束**：Rust 端用 `serde` 反序列化，未知字段被 `serde(default)` 兜底或忽略；缺失必填字段直接 deserialize 失败 → Rust 视为该 entity 不可用。Go 端禁止删除任何已存在字段，除非 Rust 同 PR 移除使用。

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
  "force_capabilities": [],      // SmallVec<[String;4]>，caller-side 必需 capability
  "match_mode_exclusive": false
}
```

> 字段命名差异：`ApiKeyEntry.force_capabilities` (singular `force_`) vs `VendorKey.forced_capabilities` (plural `forced_`)。这是 [G13 已知 gap](../architecture.md#21-crate-responsibilities)，Go 端需 schema 适配。

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
  "contact": null
}
```

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
  "oauth": null                   // 可选;null = 非 OAuth 型 key,走 api_key_enc 注入
}
```

> **0004 列冗余说明**：migration 0004 在 `vendor_keys` 表上**同时**加了 top-level 列 `forced_capabilities JSONB` 与 `exclusive_capability_match BOOLEAN`，但 Rust 实际通过 `payload.forced_capabilities` / `payload.exclusive_capability_match` 反序列化（见 `loader.rs:101-141`）。top-level 列**仅供 Go 端建索引或查询**，不是 Rust 数据来源。Go 写入时**必须**同时写两处，否则 Rust 看不到。

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

Capability 字符串由 `gw-capability::Capability` enum 解析，详见 [`docs/superpowers/specs/2026-05-21-known-gaps-design.md`](../superpowers/specs/2026-05-21-known-gaps-design.md) ADR-G13-1。未知 capability 字符串 Rust 端忽略 + warn。

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
- 失败码：`GW_AUTH_MGMT_TOKEN_{MISSING, INVALID, EXPIRED, DISABLED, REVOKED, SCOPE_DENIED}`（参 [docs/architecture.md](../architecture.md) §6 health endpoints 与 [docs/workflow.md](../workflow.md) §2.2 step 1）。

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
  "ciphertext": "<base64>",       // 加密后字节
  "nonce": "<base64>",            // 96-bit 随机 IV，每条独立
  "auth_tag": "<base64>",         // 128-bit GCM tag
  "kek_kid": "kek_v1"             // KEK 标识，用于轮换匹配
}
```

字段顺序不限；Rust 端用 `serde` 按字段名反序列化（权威类型见 `crates/gw-store/src/encrypt.rs::EncryptedKey`）。

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

详见 [`docs/architecture.md`](../architecture.md) §4.3 + `crates/gw-store/src/encrypt.rs::KekCache`。

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
2. 将 K_new 注入 Rust 集群所有 pod 的环境变量；
3. 滚动重启 Rust（让所有 pod 进入 DualRead）；
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
6. Rust 滚动重启或运行时通过 admin 端点（待加）drop K_old；
7. Rust 集群进入 Healthy(K_new)。

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

Rust 端对应指标 `gateway_entity_*` / `gateway_vendor_key_decrypt_failures_total` 详见 [`docs/architecture.md`](../architecture.md) §9。

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

1. `vendor_keys` 的 `forced_capabilities` / `exclusive_capability_match` top-level 列是否要 Rust 端用上（建索引查询）：当前未用，留 follow-up。
2. PgSQL `deleted_at` 后多久可硬清理：本契约不规定；建议 ≥ 30 天 + 审计追溯。
3. `gemini_file_mapping` 是否要进 Go 报表：当前仅 Rust 内部使用，Go 不接触；如需对外暴露需新增契约 part。
