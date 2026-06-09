# Part 07 — OAuth / LDAP（Go 写、Rust 读）

> 本 part 描述 OAuth 与 LDAP 认证配置的 **Go 主动写入、Rust 被动读取** 契约面。覆盖 PgSQL `oauth_providers` / `auth_configs` 权威写入、Redis 缓存键、token exchange / refresh 载荷，以及 LDAP group→permission 映射。索引见 [main.md](../main.md)。

---

## OAuth

### 7.1 PgSQL `oauth_providers` 表（权威在 Go）

Go 是 `oauth_providers` 表的唯一 writer；Rust 不写该表，仅通过 Go 预热到 Redis 的 auth cache / entity cache 被动读取。推荐列如下：

| 列 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `id` | TEXT | ✓ | provider 主键，建议前缀 `op_` |
| `provider_type` | TEXT enum | ✓ | `google` \| `github` \| `microsoft` \| `oidc_generic` |
| `client_id` | TEXT | ✓ | OAuth client id |
| `client_secret_enc` | JSONB | ✓ | 加密后的 client secret；**禁止**明文 |
| `auth_url` | TEXT | ✓ | 授权地址 |
| `token_url` | TEXT | ✓ | token 交换 / refresh 地址 |
| `userinfo_url` | TEXT | 可选 | 用户信息端点 |
| `scopes` | JSONB array | ✓ | 请求 scopes，snake_case 字面量 |
| `extra` | JSONB | 可选 | vendor-specific 扩展字段 |
| `enabled` | BOOLEAN | ✓ | 是否启用 |
| `version` | BIGINT | ✓ | 严格单调递增 |
| `updated_at` | TIMESTAMPTZ | ✓ | Go 写入时间 |

写入顺序遵循 [main.md §3.4](../main.md#34-写顺序原则)：

1. `INSERT ... ON CONFLICT ... DO UPDATE` 写 `oauth_providers`，并令 `version = version + 1`。
2. 写 Redis 缓存键（见 [Redis Keys](#redis-keys)）；Go 直写时**不**附 TTL，表示 Go 接管。
3. 如 Rust 侧消费路径接了 `entity:auth_config:global` 或 `events:entity ns=auth_config`，则最后发 invalidate / seed 通知。

`provider_type` JSON / Redis 序列化字面量必须与 Rust `serde(rename_all = "snake_case")` 兼容，**禁止** `camelCase` / `PascalCase`。

### 7.2 OAuth provider 配置 JSON 示例

```json
{
  "id": "op_google_workspace",
  "provider_type": "google",
  "client_id": "google-client-id",
  "client_secret_enc": {
    "kid": "kek_v1",
    "nonce_b64": "8b4Q2xk7u9+5G1dM",
    "ciphertext_b64": "9qY4v6v4K5M2e0wH4f8mPQ==",
    "tag_b64": "2Wv0L8d+4vE5Qx7n1m1Ssw=="
  },
  "auth_url": "https://accounts.google.com/o/oauth2/v2/auth",
  "token_url": "https://oauth2.googleapis.com/token",
  "userinfo_url": "https://openidconnect.googleapis.com/v1/userinfo",
  "scopes": ["openid", "profile", "email"],
  "extra": {
    "pkce_required": true,
    "prompt": "consent"
  },
  "enabled": true,
  "version": 17,
  "ts_ms": 1770000000123
}
```

### 7.3 Token exchange / refresh contract

OAuth code exchange、access token cache、refresh token 续签都由 Go 执行；Rust gateway **不**直连 OAuth provider，只读 Go 写入的 Redis 结果。目标是把外部 IdP 抖动、client secret、refresh_token 生命周期都收口在 Go。

This section governs user authentication provider configuration and `auth:oauth:token:*` caches. It does not govern vendor-key runtime OAuth credentials in `vendor_keys.payload.oauth`. After K-C, vendor-key runtime refresh is Rust-owned per `contract-change/2026-06-09-rust-oauth-refresh.md`.

#### 7.3.1 Token exchange flow

1. Go 用 `authorization_code` 或 `refresh_token` 模式向外部 provider 换 token。
2. Go 将 exchange 结果写入 Redis `auth:oauth:token:<provider_id>:<subject_id>`。
3. Rust 在认证阶段仅读该 key：命中则接受 / 补充用户画像；miss 则回退到 `auth_configs` 全局配置与上层 auth 策略。
4. Go 更新全局配置或 provider 开关时，必须同步刷新 `auth_configs` seed（见 §7.5）。

#### 7.3.2 Refresh token 义务

- `refresh_token` 仅 Go 持有与使用；Rust 不应看到明文 refresh_token。
- Go 必须在 `expires_at_ms - refresh_skew_ms` 前完成续签；推荐 `refresh_skew_ms >= 300000`（5 分钟）。
- 若 provider 返回新 `refresh_token`，Go 必须原子覆盖旧缓存；若未返回，则保留旧值并仅更新 access token / expiry。
- Rust 读取时只依赖：`access_token`、`expires_at_ms`、`subject_id`、`provider_id`、`version`、`ts_ms`、可选画像字段。
- 当 token 已过期且 Go 尚未刷新成功，Rust 必须把该缓存视为不可用；**禁止**继续接受过期 access token。

### 7.4 Token cache JSON 示例

```json
{
  "provider_id": "op_google_workspace",
  "subject_id": "user_123456",
  "access_token": "ya29.a0AfH6SM...",
  "token_type": "bearer",
  "scopes": ["openid", "profile", "email"],
  "expires_at_ms": 1770003600123,
  "refresh_after_ms": 1770003300123,
  "profile": {
    "email": "alice@example.com",
    "email_verified": true,
    "name": "Alice"
  },
  "version": 42,
  "ts_ms": 1770000000123
}
```

### 7.5 `auth_configs` 全局 seed 机制

OAuth / LDAP 的运行时总开关、默认 provider、cache 版本锚点由 Go 统一写到 `auth_configs`（推荐 well-known id = `global`，与 [parts/01 §1.2.3](01-go-as-producer.md#123-namespace-列表) 中 `auth_config` namespace 保持一致）。

建议 `auth_configs.payload` 最低包含：

- `oauth.enabled`
- `oauth.default_provider_id`
- `oauth.allow_refresh`
- `ldap.enabled`
- `ldap.default_directory_id`
- `cache_seed`
- `version`
- `ts_ms`

`cache_seed` 是全局失效锚点：任一 OAuth provider、LDAP bind 配置、group→permission mapping 变化后，Go 都必须 bump `cache_seed` 并发布新的 `auth_config/global`。Rust 读到 seed/version 变化后应视为认证缓存需要整体失效，避免权限滞留造成安全事故。

## LDAP

### 7.6 LDAP bind 配置

LDAP 配置推荐由 Go 存在 `auth_configs` 的 `ldap.directories[]` 或独立 `ldap_directories` 表；无论存储形式如何，Rust 只读 Go 下发的标准化 payload。每个目录配置至少要有：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `directory_id` | TEXT | ✓ | 目录主键，建议前缀 `ld_` |
| `host` | TEXT | ✓ | LDAP 主机名 |
| `port` | u16 | ✓ | LDAP 端口 |
| `scheme` | `ldap` \| `ldaps` | ✓ | 连接协议 |
| `bind_dn` | TEXT | ✓ | 服务账号 DN |
| `bind_password_enc` | JSON object | ✓ | 加密后的 bind password |
| `base_dn` | TEXT | ✓ | 用户 / 组搜索根 |
| `user_filter` | TEXT | ✓ | 用户搜索 filter 模板 |
| `group_filter` | TEXT | 可选 | 组搜索 filter 模板 |
| `timeout_ms` | i64 | ✓ | LDAP 请求超时 |
| `enabled` | bool | ✓ | 是否启用 |

`bind_password_enc` 与 OAuth `client_secret_enc` 一样，必须使用与 [parts/03-shared-schema.md](03-shared-schema.md) 对齐的加密 payload 形态。**禁止**将 bind password 明文写入 PgSQL / Redis / stream。

### 7.7 LDAP group→permission mapping

Go 负责把 LDAP group 名 / DN 映射为网关权限模板，Rust 只消费映射结果。推荐最小 schema：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `directory_id` | TEXT | ✓ | 关联 LDAP 目录 |
| `group_key` | TEXT | ✓ | 组标识；可为 CN 或完整 DN |
| `permissions` | JSON object | ✓ | 与 Rust 权限 merge 兼容的 payload |
| `priority` | i32 | 可选 | 多组命中时的覆盖顺序 |
| `enabled` | bool | ✓ | 是否启用 |
| `version` | BIGINT | ✓ | 严格单调递增 |
| `ts_ms` | i64 | ✓ | 更新时间 |

映射输出必须使用 snake_case 字段与 micro / `ts_ms` 约定。权限值若涉及配额 / 余额，仅允许传输 `*_micro` 字段，**禁止**出现 `cost_micro`。

### 7.8 LDAP auth config JSON 示例

```json
{
  "directory_id": "ld_corp",
  "host": "ldap.internal.example.com",
  "port": 636,
  "scheme": "ldaps",
  "bind_dn": "cn=gateway,ou=svc,dc=example,dc=com",
  "bind_password_enc": {
    "kid": "kek_v1",
    "nonce_b64": "e0Sr5LrPS4g2G3Yz",
    "ciphertext_b64": "g9zBfM7FzM0Q2m7J4h8c4Q==",
    "tag_b64": "4pK2+4k6X4L8mJ2d2a2Y8A=="
  },
  "base_dn": "dc=example,dc=com",
  "user_filter": "(&(objectClass=person)(mail={email}))",
  "group_filter": "(&(objectClass=groupOfNames)(member={user_dn}))",
  "group_permission_mappings": [
    {
      "group_key": "cn=llm-admin,ou=groups,dc=example,dc=com",
      "permissions": {
        "allow_models": ["gpt-4.1", "claude-3-7-sonnet"],
        "monthly_quota_micro": 500000000
      },
      "priority": 100,
      "enabled": true
    }
  ],
  "timeout_ms": 3000,
  "enabled": true,
  "version": 9,
  "ts_ms": 1770000000456
}
```

## Redis Keys

| Key | Writer | Reader | 含义 |
|---|---|---|---|
| `entity:auth_config:global` | Go | Rust | 全局 OAuth / LDAP seed、默认开关、目录选择 |
| `auth:oauth:provider:<provider_id>` | Go | Rust | 标准化后的 provider 配置快照；Go 直写可无 TTL |
| `auth:oauth:token:<provider_id>:<subject_id>` | Go | Rust | token exchange / refresh 结果；TTL = `expires_at_ms - now` |
| `auth:ldap:directory:<directory_id>` | Go | Rust | LDAP bind / search 配置快照 |
| `auth:ldap:groups:<directory_id>:<subject_id>` | Go | Rust | 用户 LDAP group 解析结果快照；建议短 TTL（如 300s） |
| `auth:ldap:permmap:<directory_id>:<group_key>` | Go | Rust | 单组权限映射快照 |
| `auth:cache_seed:global` | Go | Rust | 可选冗余 seed key；值变化等价于 `auth_config/global.version` bump |

约束：

1. Go 直写 Redis 时，结构必须遵守 [main.md §3.2](../main.md#32-entity-payload-信封adr-013-协议-1) 的 `v` / `enc` / `d` 信封约定；非 entity token key 可直接存 JSON STRING，但字段名仍需 snake_case。
2. Rust 对 `auth:oauth:token:*`、`auth:ldap:groups:*` 只读不写；这些 key 的 TTL 与失效策略由 Go 控制。
3. 任意配置更新后，Go 必须先写新 key，再 bump `auth_config/global` 或 `auth:cache_seed:global`；**禁止**先发 seed 再写内容，避免 Rust 读到空窗。

## Failure Modes

| 故障 | Rust 行为 | Go 行为 |
|---|---|---|
| `oauth_providers` 已更新但 Redis 未刷新 | Rust 继续读旧缓存，直到 `auth_config/global` seed bump 或缓存 miss | 必须补写 Redis 并 bump seed；监控 stale key |
| `auth_config/global` 已 bump 但 provider / LDAP key 尚未写完 | Rust 可能立即失效本地缓存并读到 miss；认证应 fail-closed 或回退默认禁用 | **禁止**该写序；必须先写内容再 bump seed |
| access token 过期且 refresh 失败 | Rust 将 token 视为不可用，不接受过期凭证 | Go 记录 refresh failure、退避重试、必要时清理 token key |
| LDAP group mapping 更新漏发 | Rust 可能继续按旧权限放行，存在安全风险 | Go 必须把 group mapping 更新纳入 `cache_seed` bump 触发条件 |
| client secret / bind password 明文入库 | Rust 不负责兜底；视为严重契约违规 | Go 必须只写 `*_enc` 加密对象，并做上线前扫描 |
| Redis 故障导致 token/group cache miss | Rust 认证路径可能回退到 `entity:auth_config:global` 或直接拒绝 | Go 重建缓存；若支持重放，应以 PgSQL 权威表重新 seed |

## 7.9 监控义务

Go 端最低建议暴露：

| 指标 | 类型 | 阈值 |
|---|---|---|
| `oauth_token_refresh_total{provider_id,result}` | counter | `result="error"` rate > 1% |
| `oauth_token_refresh_lag_ms` | histogram | p95 > 60000 |
| `auth_cache_seed_bump_total{reason}` | counter | — |
| `ldap_group_mapping_update_total{directory_id}` | counter | — |
| `ldap_bind_failure_total{directory_id}` | counter | rate > 1% |

## 7.10 未决问题

1. `oauth_providers` / `ldap_directories` 是否最终落独立表还是折叠进 `auth_configs`，由 Go 自决；Rust 侧只依赖 Redis 标准化 shape。
2. `auth:oauth:token:*` 是否需要拆分 access token 与 profile cache，v1 可合并；后续按命中率再决定。
3. LDAP group 解析是否由 Go 定时预热还是登录时按需写入，v1 两者都可；但无论哪种都必须纳入 `cache_seed` 失效链。
