# Part 01 — Go-as-Producer（Go 写、Rust 读）

> 本 part 描述所有 **Go 主动写入、Rust 被动消费**的契约面。读完即可在 Go 端实施 writer。索引见 [main.md](../main.md)。

---

## 1.1 PgSQL 写权威

Go 是所有 entity 表的唯一 writer（Rust 不写 PgSQL，仅 sqlx `SELECT`）。表结构、payload JSONB schema、`version BIGINT` 协议详见 [parts/03-shared-schema.md](03-shared-schema.md)。

写入义务（事务内串联三步）：

1. **`UPDATE <table> SET payload = $1, version = version + 1, updated_at = now() WHERE id = $2 RETURNING version`**（或对应 `INSERT ... ON CONFLICT (id) DO UPDATE`）。`version` 必须**严格单调递增**，由数据库 trigger 或 Go 应用层保证。
2. **写回 Redis `entity:{ns}:{id}` HASH**（可选 write-through，加速 Rust 命中；不做也行——Rust miss 时会自己写回带 TTL 的副本）。Go 写时**禁止**附 `EXPIRE`（永久 key 表示"Go 接管"）。
3. **`XADD events:entity * ns <ns> id <id> v <new_version> op <upsert|delete>`**。失败时**不**回滚 PgSQL，由后续 ALTER + 重试或下次更新覆盖。

Rust 端通过 `version` 单调性兜底：即使 XADD 漏发，下一次 Rust 冷加载读到的就是新 version。

## 1.2 `events:entity` Stream

### 1.2.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `events:entity` |
| Writer | Go（主路径）；Rust 仅反向写两类 op（详见 [parts/02 §2.4](02-go-as-consumer.md#24-events:entity-反向写入)） |
| Reader | Rust StreamConsumer，`crates/gw-store/src/stream_consumer.rs:36-156` |
| Consumer group | `<rust_instance_id>`（每副本独立 group，启动期锚点 `$`） |
| 持久化策略 | 由 Go 自定；建议 `MAXLEN ~ 1_000_000` |

### 1.2.2 Payload schema

XADD 字段（key-value 列表）：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `ns` | string | ✓ | namespace，见 §1.2.3 |
| `id` | string | ✓ | 该 entity 在对应表的 PK |
| `v` | u64 | 推荐 | 写后的新 version（Rust v1 忽略，invalidate-then-reload；为未来 version-aware upsert 预留） |
| `op` | `upsert` \| `delete` | ✓ | 操作语义 |
| `enc` | `json` | 可选 | 与 entity HASH 字段同义；预留 msgpack |
| `d` | bytes | 不写 | 事件**不**携带 payload，Rust 收到后清缓存重新冷加载 |

> Rust 当前实现（`stream_consumer.rs:170-217`）**仅按 `(ns, id)` invalidate Moka 缓存**，不解析 `v` / `d`。这是有意的：invalidate-then-reload 简单且 Moka 提供 read-after-invalidate 强一致；versioned upsert 留作 v2 演进点。Go 端**仍必须**写 `v` 字段以备 v2 升级。

### 1.2.3 Namespace 列表

下表所列 namespace 必须由 Go 维护写入；Rust 已实现 invalidate handler（顺序对照 `stream_consumer.rs:170-211`）：

| ns | PgSQL 表 | Rust Moka cache | Rust 类型 | 备注 |
|---|---|---|---|---|
| `user` | `users` | `EntityStore.user` | `UserEntry` | 5 min TTL |
| `user_group` | `user_groups` | `EntityStore.user_group` | `UserGroup` | 5 min TTL |
| `apikey` | `api_keys` | `EntityStore.apikey` | `ApiKeyEntry` | 5 min TTL |
| `vendor` | `vendors` | `EntityStore.vendor` | `Vendor` | 30 min TTL；`egress_mode` 写入契约见 [parts/03 §3.2.4](03-shared-schema.md#324-vendorspayload--vendor) |
| `vendor_key` | `vendor_keys` | `EntityStore.vendor_key` | `VendorKey`（含 `api_key_enc` 解密前形态、`tunnel_preference`、`identity_snapshot` 与 `quota_state` 折叠字段，见 [parts/03 §3.2.5](03-shared-schema.md#325-vendor_keyspayload--vendorkey含加密-api_key) / [§3.4](03-shared-schema.md#34-加密-payload-aes-256-gcm)） | 30 min TTL |
| `model_group` | `model_groups` | `EntityStore.model_group` | `ModelGroup` | 30 min TTL |
| `pricing` | `pricings` | `EntityStore.pricing` | `Pricing` | 30 min TTL，但 Rust 实际只读 estimate_pricing；过渡期保留 |
| `jwks` | `jwks` | `EntityStore.jwks` | `JwksEntry` | 1 h TTL |
| `ipblock` | `ip_blocks` | `EntityStore.ipblock` | `IpBlockEntry` | 5 min TTL；ID = CIDR 字符串 |
| `model` | `models`（0004 migration） | `EntityStore.model` | `Model` | 30 min TTL；G13 capability metadata |
| `model_mapping` | `model_mapping`（0003 migration） | `EntityStore.model_mapping` | `ModelMapping` | 30 min TTL；ID 格式 `"<user_alias>|<protocol>"`，protocol ∈ `openai \| anthropic \| gemini` |
| `auth_config` | `auth_configs`（待 migration） | `EntityStore.auth_config` | `AuthConfig` | 1 h TTL；well-known ID `"global"` |
| `management_token` | `management_tokens`（0007 migration） | `EntityStore.mgmt_token` | `ManagementTokenEntry` | 5 min TTL；ID 前缀 `mt_` |
| `proxy_node` | `proxy_nodes`（0005 migration,已 merged）| `EntityStore.proxy_node` | `ProxyNodeEntry` | 30 min TTL;支撑 tunnel client 节点视图;详见 [parts/04](04-proxy-nodes.md) |
| `estimate_pricing` | `estimate_pricings`（Go 负责建表/写入） | `EntityStore.estimate_pricing` | `EstimatePricing`（见 §1.5） | 30 min TTL |
| `error_message_rules` | `error_message_rules`（Go 负责建表/写入） | `ErrorMessageRuleStore` 全局 `ArcSwap` snapshot | `CompiledErrorMessageRules` | ID 固定为 `"global"`；Rust 收到事件后整表 reload + compile，失败保留旧 snapshot |

未知 ns Rust 端 warn 后丢弃。新增 ns **必须**同步：本表 + `crates/gw-store/src/stream_consumer.rs::apply_entity_event` + `crates/gw-store/src/entity_store.rs::EntityStore`。

### 1.2.4 写顺序与重试

- **顺序**：PgSQL UPDATE 完成 → XADD。**禁止**先 XADD 后 UPDATE。
- **重复 XADD 安全**：Rust invalidate 幂等。
- **漏发 XADD**：Rust 等待 Moka TTL 到期自然 miss → 冷加载 → 拿到 PgSQL 新值。最坏陈旧 = TTL（30 min for vendor_key/pricing/model_group）。

### 1.2.4.1 Vendor-key regenerate identity

The Go admin action for egress impersonation identity regeneration is a normal
`vendor_key` entity update:

1. `UPDATE vendor_keys SET identity_snapshot = NULL, version = version + 1, updated_at = now() WHERE id = $1 RETURNING version`.
2. `XADD events:entity * ns vendor_key id <id> v <new_version> op upsert`.

The XADD is required. Rust does not compare PgSQL versions after a Redis
`entity:vendor_key:<id>` hit; the `events:entity` handler deletes Redis L1 and
invalidates Moka, forcing the next load to observe the cleared snapshot. See
[`contract-change/2026-05-29-vendor-validation.md`](../contract-change/2026-05-29-vendor-validation.md).

**Redis L1 invalidate(Rust 端补口)**:

Rust StreamConsumer 收到 `events:entity` 后,**在 invalidate Moka 之前**额外执行:

```
DEL entity:{ns}:{id}
```

这是为了多 pod 部署时 Redis L1 不残留陈旧值(详见 [`docs/architecture.md`](../../architecture.md) §10.5 已知问题)。Go 端**写顺序无变更**;Rust DEL 失败仅 `tracing::warn!`,不阻塞 Moka invalidate。

幂等性:任意 pod 收到 `events:entity` → DEL Redis L1 → Moka invalidate;多 pod 同时收同一事件即多次 DEL,无害。

启动期 Redis L1 陈旧不在本补口范围:Rust 启动锚点是 `$`,启动前的事件不消费;Redis L1 中陈旧 entity 在 TTL 内仍可能命中(最长 30 min for vendor_key / pricing / model_group)。如要彻底解决,需要 Go 主动 DEL 或 Rust 全量预热(留 follow-up,不在本契约范围)。

**egress-impersonate 管理动作补充（0009）**:

- Go 写 `vendors.egress_mode = {"mode":"impersonate", ...}` 或写 `vendor_keys.payload.tunnel_preference` 前，必须执行 EI-8 write-time exclusion：同一 vendor 不得同时存在 impersonate egress 与 tunnel-eligible key（`tunnel_preference.require_tunnel = true` 或 Go 侧 node mapping）。Rust 请求路径仍以 `GW_TUNNEL_IMPERSONATE_CONFLICT` fail-closed 兜底。
- Go 的 regenerate-identity 管理动作必须执行：

```sql
UPDATE vendor_keys
   SET identity_snapshot = NULL,
       version = version + 1,
       updated_at = now()
 WHERE id = $id;
```

随后按本节写顺序发出 `XADD events:entity * ns vendor_key id <id> v <new_version> op upsert`。Rust 收到事件后 DEL `entity:vendor_key:<id>` + invalidate Moka；下一次请求冷加载到 `identity_snapshot = NULL`，通过 `ensure_identity` 重新派生并由 Rust lazy-persist 在 `IS NULL` guard 下回写。完整契约见 [`contract-change/2026-05-29-vendor-validation.md`](../contract-change/2026-05-29-vendor-validation.md)。

### 1.2.4.2 Error-message rules global snapshot

Error-message rules are Go-owned control-plane data, but Rust consumes them as
one immutable global snapshot rather than per-row Moka entries:

1. Go writes `error_message_rules` rows in PgSQL and soft-deletes by setting
   `deleted_at`.
2. Go publishes Redis HASH `entity:error_message_rules:global` with fields:
   `v=<max enabled row version>`, `enc=json`, `d=<json array of enabled rules>`.
3. Go emits `XADD events:entity * ns error_message_rules id global v <snapshot_version> op upsert`.

Rust bootstrap cold-loads the same snapshot once. Runtime `events:entity`
delivery triggers `ErrorMessageRuleStore::reload()`: read Redis first, fall back
to PgSQL, validate all limits again, compile the fixed-dimension rule tree, then
atomically replace the `ArcSwap` snapshot. If Redis/PG read or compilation
fails, Rust logs and keeps serving with the previous snapshot; the request hot
path never calls Redis/PgSQL and never runs regex, scripts, callbacks, or
configurable loops.

The rule dimensions are fixed in order: `provider_id`, `model`, `endpoint_id`,
`api_format`, `request_path`, `client_protocol`, `gateway_code`, `http_status`,
then bounded upstream-message substring checks. `endpoint_id` participates only
when the request context has `selected_endpoint_id`; first-version routing keeps
it `None` because the current `VendorKey` projection does not expose endpoint
row identity.

### 1.2.5 删除语义

`op = delete` 时 Rust 仅 invalidate cache。Rust 下次冷加载会 SELECT 到 `deleted_at IS NOT NULL` → loader 返回错误（loader.rs 内 `WHERE deleted_at IS NULL`），cold-load wrapper 转 `None`。这相当于"该 entity 不存在"。Go 端**必须**走 soft-delete（写 `deleted_at`），不要物理删除——否则有 in-flight 请求未来不可追溯。

## 1.3 `events:counter` Stream

### 1.3.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `events:counter` |
| Writer | Go |
| Reader | Rust StreamConsumer（v1 仅 observability，handler 见 `stream_consumer.rs:136-138`）；C-γ 上线后改为权威 counter sync handler |
| Consumer group | `<rust_instance_id>`（每副本独立 group） |

### 1.3.2 Payload schema

XADD 字段：

| 字段 | 类型 | 必填 | 含义 |
|---|---|---|---|
| `ns` | `user_balance` | ✓ | 当前唯一支持 ns；预留扩展 |
| `uid` | string | ✓ | UserId |
| `op` | `incrby` | ✓ | 当前唯一 op |
| `field` | `base_micro` \| `deducted_micro` | ✓ | counter 字段名（**不**包含 `frozen_micro`——那是 Rust 写） |
| `delta` | i64 | ✓ | 本次变化量（可负） |
| `value_after` | i64 | ✓ | INCRBY 后的当前值（**权威**：Rust 用它覆盖本地缓存，不用 delta 累加） |
| `ts_ms` | i64 | ✓ | wall-clock 毫秒 |

### 1.3.3 触发场景

| 场景 | field | delta |
|---|---|---|
| 用户充值 | `base_micro` | `+amount` |
| 退款 / 管理员调账 | `base_micro` | `+/-amount` |
| Go settle 一条 UsageRecord | `deducted_micro` | `+cost_micro` |

> Rust **不**写 `events:counter`。`frozen_micro` 变化由 Rust 异步 `INCRBY counter:user:<uid>:frozen_micro`，**不**通过 events:counter 通知 Go（Go 直接 GET Redis 读 frozen）。

### 1.3.4 写顺序

Go 端必须：

1. `INCRBY counter:user:<uid>:<field> <delta>` 得到 `new_value`；
2. 立即 `XADD events:counter * ns user_balance uid <uid> op incrby field <field> delta <delta> value_after <new_value> ts_ms <now_ms>`。

第 1 步是真相源，第 2 步是通知。第 2 步漏发 → Rust 缓存陈旧，但 Rust 启动期会 HGET 对齐，且 freeze 机制本身就能容忍 deducted 短期偏低（详见 [`docs/spec/06-billing-ledger.md`](../../spec/06-billing-ledger.md)）。

### 1.3.5 value_after 必须单调（同 field 内）

同一 `(uid, field)` 上后写的 `value_after` 必须 ≥ 先写的 `value_after`。Rust 用 `ts_ms` 排序去重，但实际依赖 Redis Stream 的 entry-id 单调性（`*` 自动分配）。Go 端**不要**用旧值覆盖新值（如重试时拿了缓存的旧 value）。

## 1.4 Redis Counter Keys（`counter:*`）

| Key | Writer | Reader | 写入路径 |
|---|---|---|---|
| `counter:user:<uid>:base_micro` | Go | Rust（启动期 HGET + events:counter 增量） | Go INCRBY |
| `counter:user:<uid>:deducted_micro` | Go | Rust（启动期 HGET + events:counter 增量） | Go INCRBY，每条 UsageRecord 算完 cost 后写 |
| `counter:user:<uid>:frozen_micro` | **Rust** | Rust + Go | Rust 异步 HINCRBY（freeze +est / release -est）；Go 仅 GET 读用于审计 / balance check |
| `counter:user:<uid>:daily_tokens:<YYYY-MM-DD>` | **Rust**（observational） | Rust（quota 检查） + Go（报表可读） | Rust `INCRBY + EXPIRE 48h`（`counter_store.rs:50-60`） |
| `vendor_keys.quota_*` | **Rust**（runtime writer） | Rust（routing 前置过滤） + Go（报表/管理只读） | Rust `QuotaStateUpdateQueue` 更新列、自增 version、发 `events:entity ns=vendor_key op=quota_state` |

> 写权独占：`base_micro` / `deducted_micro` 仅 Go 写；`frozen_micro` 仅 Rust 写；`daily_tokens:*` 仅 Rust 写；`vendor_keys.quota_*` 运行时仅 Rust 写。**禁止**任一方越线写另一方的 field——会破坏 [parts/02 §2.5](02-go-as-consumer.md#25-counter-frozen-micro-rust-写) 的不变量。

Go 写顺序：见 §1.3.4。Go 不需要写 `counter:*:frozen_micro` 或 `counter:*:daily_tokens:*`，直接 GET 即可。

## 1.5 `entity:estimate_pricing:<model>` Push 协议

Rust 在 freeze 估算路径用此 entity，**保证 freeze 金额 ≥ 真实可能 cost**。Go 在每次更新内部 `BillingPricing`（Go 自定义、Rust 不感知）时**必须**同步推算并 push 对应 `EstimatePricing`。

### 1.5.1 Payload schema

```json
{
  "model": "gpt-4-turbo",
  "conservative_per_token_micro": 60,
  "default_max_tokens": 4096,
  "hard_cap_tokens": 200000,
  "safety_factor": 1.0
}
```

字段含义参 [`docs/spec/06-billing-ledger.md`](../../spec/06-billing-ledger.md)。Rust 用：

```
freeze = (input_tokens_est + max_tokens_capped) * conservative_per_token_micro * safety_factor
```

### 1.5.2 推算公式（Go 端责任）

```
conservative_per_token_micro = ceil(
  max(
    output_per_token_micro,
    cache_creation_per_token_micro,
    thinking_per_token_micro,
    multimodal_per_token_equivalent,
    ...所有 BillingPricing 维度的 per-token 最大可能值
  ) * markup_max_chain * 1.1
)
```

`markup_max_chain` = 系统内已知所有 4 级 markup 链（user_group × api_key × vendor × model）的最大可能乘积。

**强不变量**：对任意请求 N 个 token，`conservative_per_token_micro * N ≥ 真实 cost`。违反 → Rust freeze 不足 → 透支风险。Go 端 CI 推荐校验：每次改 BillingPricing 同时 push EstimatePricing。

### 1.5.3 上线 ordering

- v1：Rust 同时支持旧 `entity:pricing` 与新 `entity:estimate_pricing`；请求冻结估算优先用 `entity:estimate_pricing`，缺失 / cold-load miss 时回退 legacy flat estimate（C-β §ADR-Cβ-4 step 1）。
- v2：Go 切到完全用 BillingPricing 算 cost，删除 `entity:pricing` namespace。Rust 仅读 `entity:estimate_pricing`。

> 用户决议（[breaking changes OK](../../../../../.claude/projects/-Users-whereslow-Desktop-work-ailinkmax-gateway/memory/feedback_breaking_changes_ok.md)）：v2 切换可以直接做，不需要 step 1 兼容期。

### 1.5.4 失败模式

| 故障 | Rust 行为 |
|---|---|
| Go 改 BillingPricing 但忘记 push EstimatePricing | Rust 可能继续使用旧 conservative_per_token_micro，或在 entity 缺失时使用 legacy flat estimate → freeze 可能偏低 → 透支风险。Go 必须 CI 防呆 |
| EstimatePricing entity 缺失 / cold-load miss | Rust 回退 legacy flat estimate，`UsageRecord.estimate_pricing_version = 0`，用于区分非 C-β 估算路径 |

## 1.6 `jwt:blacklist:<jti>` Redis Key

| Key 格式 | `jwt:blacklist:<jti>` |
|---|---|
| Type | STRING（或 SET，由 Go 自决；Rust 仅 `EXISTS` 判定） |
| Writer | Go（用户登出 / 强制 revoke 时写） |
| Reader | Rust `RedisJwtBlacklist`（`crates/gw-auth/src/blacklist.rs`） |
| TTL | Go 设为 token 的剩余有效期；过期自动清理 |

Rust 行为：JWT 验签通过后查一次 `EXISTS`：

- 命中 → 401 `GW_AUTH_JWT_REVOKED`；
- Redis 不可达 → 按 `AuthConfig.blacklist_fail_mode` 走 `FailClosed`（默认，仍 401）或 `FailOpen`（放行）。

Go 写入时：

```
SET jwt:blacklist:<jti> "1" EX <remaining_seconds>
```

JTI 是 JWT `payload.jti` 字段；Go 签发时必须填。

## 1.7 KEK 轮换 Producer 责任

Vendor key 的 `api_key` 用 AES-256-GCM 加密入库，KEK 由 KMS 托管。轮换是 Go / 运维责任，Rust 仅作为消费者支持 dual-KEK 期。

### 1.7.1 KEK 轮换流程（Go / 运维侧）

```
1. KMS 中生成新 KEK，分配新 kek_kid（如 "kek_v2"）
2. 通知运维：将新 KEK_HEX 加入 Rust pod 环境变量（或下次重启时挂载）
3. 滚动重启 Rust 集群 → 所有 pod 进入 DualRead 状态（K_old + K_new）
4. 在 PgSQL 上跑 re-encryption job：
     for row in vendor_keys where kek_kid = 'kek_v1':
        plaintext = decrypt(row.api_key_enc, K_old)
        new_enc = encrypt(plaintext, K_new)
        UPDATE vendor_keys SET payload = jsonb_set(payload, '{api_key_enc}', new_enc),
                              version = version + 1, updated_at = now()
        XADD events:entity * ns vendor_key id <id> v <new_v> op upsert
5. 等待所有 vendor_keys 重加密完成 → 撤下 K_old
6. Rust pod 进入 Healthy(K_new) 状态
```

### 1.7.2 Rust 行为

详见 [parts/03 §3.5](03-shared-schema.md#35-kek-轮换状态机) 与 `crates/gw-store/src/encrypt.rs::KekCache`。Go 端**禁止**在 Rust 仍处 `Healthy(K_old)` 状态时单独切换为 `Healthy(K_new)`——必须经过 DualRead 中间态。

### 1.7.3 加密 payload 形态

Go 写 `vendor_keys.payload` 时，`api_key_enc` 字段必须是四元组 JSON 对象（见 [parts/03 §3.4](03-shared-schema.md#34-加密-payload-aes-256-gcm)）。**禁止**任何路径写明文 api_key。

## 1.8 监控义务

Go 端必须暴露：

| 指标 | 类型 | 阈值 |
|---|---|---|
| `entity_upsert_total{ns}` | counter | — |
| `entity_xadd_failure_total{ns}` | counter | rate > 0.1% |
| `counter_xadd_failure_total{field}` | counter | rate > 0.1% |
| `events_counter_publish_lag_ms` | histogram | p95 > 100 |
| `estimate_pricing_push_total` | counter | — |
| `estimate_pricing_skip_total{reason}` | counter | rate > 0 → 告警 |
| `kek_rotation_in_progress` | gauge 0/1 | 持续 > 24h → 告警 |
| `vendor_key_reencrypt_pending` | gauge | re-encryption job 剩余条数；> 0 但 24h 不变 → 告警 |

## 1.9 未决问题

1. `entity:estimate_pricing` 的 PgSQL 表名 / payload schema 由 Go 自决，本契约仅约束 Redis HASH 字段。
2. `events:counter` 当前 ns 仅 `user_balance`；如要扩展（如 `vendor_quota`）需双方协议。
3. KEK 轮换 re-encryption job 的执行位置：建议 Go 写一个独立 batch worker，可由 Rust pod 通过 admin 端口手动触发；待后续 spec 化。
4. JWT 黑名单是否需要分布式去重 / 跨地域同步：v1 单 Redis 集群即可；多 region 部署后再设计。
