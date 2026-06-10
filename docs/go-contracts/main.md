# Go 端协同契约 — main

> Rust 网关与 Go 管理面 / 计费面之间所有跨进程契约的**单一来源**。任何修改 schema、stream、Redis namespace、PgSQL 表的 PR，**必须**在同一 commit 内更新本文档及对应 part。

- **文档状态**：v1（吸收并取代原 `docs/go-contracts/main.md`，该文件本 PR 内删除；α/β/γ/δ 仍是 Rust 侧实现 spec，与本契约配合存在）
- **作者**：Rust 网关团队（Claude + 用户协作）
- **覆盖面**：Rust 与 Go 之间所有 Redis stream / Redis key / PgSQL 表 / 加密 payload 接触点。**不**覆盖：Rust 内部实现、Go 内部公式（如 BillingPricing）、Aether 历史路径迁移。

---

## 1. 上下文

Rust 网关只做"用量记录 + 路由 + 限流 + 估算冻结"，**不算 cost、不写余额权威值、不维护号池元数据**。Go 管理面是这些数据的权威 writer。两边通过：

```
   Go (权威 writer / settle 端)                   Rust (高频读 / 路由 / 冻结)
   ──────────────────────────────                ──────────────────────────────
   PgSQL                  ────────────────►       sqlx 直连读
   Redis entity:{ns}:{id}  ◄──────write-back──── Rust 冷加载写回（带 TTL）
   events:entity (XADD)   ────────────────►       StreamConsumer XREADGROUP
   events:counter (XADD)  ────────────────►       StreamConsumer XREADGROUP
   counter:user:*:base_micro / deducted_micro    （Go INCRBY；Rust 读快照）
                                                  counter:user:*:frozen_micro  ◄── Rust HINCRBY
                          ◄──────XADD──────────  usage:records (UsageRecord)
                          ◄──────XADD──────────  usage:records:dlq
                          ◄──────XADD──────────  usage:detailed（必需,180d 归档）
                          ◄──────XADD──────────  events:entity（status_change / 自动封禁）
   entity:estimate_pricing ────────────────►     EntityStore 懒加载
   entity:management_token ────────────────►     EntityStore.mgmt_token（mgmt verify chain）
   jwt:blacklist:<jti>     ────────────────►     JwtBlacklist 命中查询
   mgmt_token:blacklist:<token_id> ────────►     RedisJwtBlacklist::check_mgmt 命中查询
```

## 2. 契约文件组织

| 文件 | 角色 | 主要内容 |
|---|---|---|
| [parts/01-go-as-producer.md](parts/01-go-as-producer.md) | **Go → Rust** | Go 主动写、Rust 消费。涵盖：PgSQL 实体写、`events:entity`、`events:counter`、`counter:*` 余额、`entity:estimate_pricing`、`jwt:blacklist:*`、KEK 轮换 producer 责任 |
| [parts/02-go-as-consumer.md](parts/02-go-as-consumer.md) | **Rust → Go** | Rust 主动写、Go 消费。涵盖：`usage:records`（吸收原 C-ε §2）、`usage:records:dlq`、`usage:detailed`、Rust 反向 `events:entity`（`vendor_key.rate_limited` / `ipblock` 自动封禁）、`counter:*:frozen_micro` / `daily_tokens` 读路径 |
| [parts/03-shared-schema.md](parts/03-shared-schema.md) | **共享 schema** | 双方都接触的 PgSQL 表清单、entity payload JSONB shape、`version BIGINT` 协议、AES-256-GCM 加密 payload、KEK 轮换、Kiro/Vertex vendor-key payload touchpoints |
| [parts/04-proxy-nodes.md](parts/04-proxy-nodes.md) | **Proxy Nodes** | Rust tunnel client ↔ Go Hub 契约;节点 entity / 心跳分工 / status 状态机 |
| [parts/05-wallet-payment.md](parts/05-wallet-payment.md) | **Wallet / Payment** | 钱包余额计数器、`wallets` / `payments` 表、支付状态枚举、webhook 归一化 payload、`usage:records` 与冻结/解冻衔接 |
| [parts/06-subscription-video.md](parts/06-subscription-video.md) | **Subscription / Video** | 订阅套餐、用户订阅、视频任务状态机、`operation_id` 关联、订阅 entitlement 到 Rust permission merge 的投影规则 |
| [parts/07-oauth-ldap.md](parts/07-oauth-ldap.md) | **OAuth / LDAP** | OAuth provider 配置、token exchange / refresh cache、LDAP bind 配置、group→permission mapping、`auth_configs` seed 机制 |
| [parts/08-control-plane.md](parts/08-control-plane.md) | **Control Plane** | Rust↔Go 透明控制平面:配置下行(`config_*` + `events:entity ns=config`)、能力/状态上行(`gateway:instance:<id>:{capability,effective,runtime}`)、命令通道(`commands:gateway` 索引/通知 + `entity:gateway_command:<command_id>` 主体 + `cmd_ack`)；能力 payload includes protocol/API shape strings such as `kiro:eventstream` / `kiro:cli`。**目标契约(design 阶段)** |
| [contract-change/](contract-change/) | **契约变更日志** | 每次跨进程契约改动的索引记录(触发 spec + parts 落点 + Go 模块) |
| [contract-change/2026-05-29-vendor-validation.md](contract-change/2026-05-29-vendor-validation.md) | **Vendor validation** | EI-8 Go 写入期拒绝 tunnel+impersonate 冲突；EI-7 Go 管理动作清空 `vendor_keys.identity_snapshot` 并递增 `version` 以触发 Rust cache miss + 重新派生 |

Current contract-change entries include [`contract-change/2026-05-29-vendor-validation.md`](contract-change/2026-05-29-vendor-validation.md), which defines Go write-time validation for egress impersonation vendor keys and the `identity_snapshot = NULL, version = version + 1` regenerate-identity action.

## 3. 通用约定（所有 part 共享）

### 3.1 字段命名

- 时间戳一律 `ts_ms`，单位 **毫秒**，UTC（i64，wall-clock）。
- 金额单位一律 **micro**（1 元 = 1_000_000 micro），i64。**禁止**出现 `cost_micro` 字段（由 `no_cost_micro` invariant 强制；详见 [`docs/architecture.md`](../architecture.md) §7）。
- ID 一律 `snake_case` TEXT，前缀绑定语义：`u_*`（user）、`ak_*`（apikey）、`v_*`（vendor）、`vk_*`（vendor_key）、`mg_*`（model_group）。前缀由 Go 端生成时统一遵守。
- 枚举字段 JSON 序列化用 `serde(rename_all = "snake_case")`，Go 端必须匹配相同字面量。

### 3.2 Entity payload 信封（ADR-013 协议 1）

每个 entity 在 Redis HASH 中的字段固定为：

```
entity:<ns>:<id>     HASH
  v       u64       单调递增版本号
  enc     "json"    编码方式（预留 msgpack）
  d       <bytes>   payload 主体（JSON 字符串 / 二进制）
  EXPIRE  按 namespace 决定（Rust 写回带 TTL；Go 写无 TTL）
```

PgSQL 同步表则携带 `version BIGINT`（详见 Part 03）。

### 3.3 Stream 命名与 consumer group

| Stream | Writer | Reader | Reader consumer group 命名 |
|---|---|---|---|
| `events:entity` | Go（主） / Rust（反向少量） | Rust StreamConsumer | `<instance_id>`（每副本独立 group） |
| `events:counter` | Go | Rust StreamConsumer | `<instance_id>`（每副本独立 group） |
| `usage:records` | Rust Ledger flusher | Go settler | `go-billing-settler`（推荐） |
| `usage:records:dlq` | Rust flusher（主路径失败） | Go drainer（可选） | `go-billing-dlq`（推荐） |
| `usage:detailed` | Rust DetailedArchive flusher pool | Go drainer（**必需**） | `go-archive-drainer`（推荐） |
| `commands:gateway` | **Go** | Rust StreamConsumer | `group:<instance_id>`（每副本独立 group,广播,SP-C;stream 只携带 `command_id`/`target`/`delivery`/`issued_at_ms`,主体在 `entity:gateway_command:<command_id>`） |

**XREADGROUP 锚点**：Rust 启动期对 `events:*` 用 `$`（参 `crates/gw-store/src/stream_consumer.rs:41`），仅消费启动后事件；启动前状态靠冷加载链获取。Go consumer 自决（settle 通常从 `0` 开始消费全量）。

### 3.4 写顺序原则

- **Go 写实体**：先 `UPDATE pgsql SET version=version+1`，再 `HSET entity:{ns}:{id}`（可选 write-through），最后 `XADD events:entity`。三步全失败导致 Rust 缓存陈旧，由 `version` 单调性 + Rust 启动期冷加载兜底。
- **Go 写 counter**：先 `INCRBY counter:user:<uid>:<field>`，再 `XADD events:counter` 携带 `value_after`。Rust 用 `value_after` 覆盖本地缓存，**不**做 delta 累加。
- **Rust 反向写 entity**：仅限白名单（`vendor_key.status_change`、`ipblock.upsert`），见 Part 02。`v` 字段 Rust 不能伪造单调，**必须**通过 Go 中转或使用 Go 预留的"反向写入 op"枚举。

### 3.5 失败语义

| 场景 | Rust 行为 | Go 行为 |
|---|---|---|
| Redis 全挂 | 热路径 fail-open（Moka 命中仍服务）；冷加载 → 503；mpsc 满 → 丢 `usage:records` 并 metrics 报警 | settler 跟不上 → `XPENDING` 累积；告警 |
| PgSQL 不可达 | Rust 冷加载失败 → 503 `GW_BACKEND_UNAVAILABLE`；命中缓存请求不受影响 | settler 落盘失败 → 不 XACK，等待重试 |
| KMS 不可达 | 启动期 fail-fast；运行期 KEK 已缓存仍工作；新解密失败 → 该 vendor_key 不可用 | 不影响 |
| Rust 进程崩溃 | 本地 `frozen_micro` 丢；重启后从 Redis sync 回 | settle 不受影响（usage:records 已落 stream） |
| Go 晚于 Rust 上线 | `usage:records` / `usage:records:dlq` 堆积；deducted 不增长 → balance 看似不扣；用户暂不计费（可作 dev/test 形态） | — |
| Stream 严重 lag（> 30s）| metrics 报警 | settler 必须扩容 |

### 3.6 版本兼容（破坏性变更可直接做）

> 项目未上线。**不**保留旧字段 / 旧 stream 名 / 兼容路径 / 灰度期。Schema 改动直接全量切换；Rust 与 Go 同 PR 协调；DLQ 旧条目允许丢弃。

## 4. 上线 ordering

Rust 与 Go 同时上线（推荐）或 Go 晚上线（容忍）两种姿态。两种姿态下的依赖图：

```
PgSQL schema (migrations 0001-0004 + 待加 auth_configs)
    │   先 psql -f 跑，不自动应用
    ▼
Redis 部署 + KEK_HEX 入 env
    │
    ├──► Rust 上线（可单独运行，usage:records 堆 stream）
    │
    └──► Go 上线（开始消费 usage:records、写 events:entity / events:counter、写 estimate_pricing）
```

Rust 单跑形态下：
- 不能依赖 Go 写 `entity:estimate_pricing` → freeze 退化为读旧 `Pricing` entity（C-β §ADR-Cβ-4 step 1 兼容路径，**仅过渡期允许**；目标态 Rust 必须读 `EstimatePricing`）；
- `balance_micro` 不递减（用户看似无限额度）；可在 dev/test 使用。

## 5. 监控与告警（双方约定）

Go 端最低需暴露：

| 指标 | 阈值（告警） |
|---|---|
| `usage_records_consume_lag_secs`（XPENDING 最老 entry 年龄） | > 60 |
| `usage_records_processed_total` | counter，仅可观测 |
| `settle_failure_total` rate | > 1% |
| `estimate_pricing_push_total` | counter |
| `events_counter_publish_lag_ms`（INCRBY → XADD 延迟） | > 100 |
| `dlq_records_pending`（usage:records:dlq XLEN） | > 1000 |

Rust 端对应指标（前缀 `gateway_*`）在 [`docs/architecture.md`](../architecture.md) 与各 spec 内已记录，交叉验证用。

## 6. 参考资料

- 主架构：[`docs/architecture.md`](../architecture.md)（持久化 §5、路由 §3、KEK §4.3、invariant §7）
- 请求流：[`docs/workflow.md`](../workflow.md)（Pingora filter chain，含每条 Redis 写时机）
- 设计 spec（Rust 侧实现细节）：
  - C-α UsageRecord：[`docs/spec/06-billing-ledger.md`](../spec/06-billing-ledger.md)
  - C-β EstimatePricing：[`docs/spec/06-billing-ledger.md`](../spec/06-billing-ledger.md)
  - C-γ 余额读写分工：[`docs/spec/06-billing-ledger.md`](../spec/06-billing-ledger.md)
  - C-δ Stream DLQ：[`docs/spec/06-billing-ledger.md`](../spec/06-billing-ledger.md)
  - 主设计：[`docs/architecture.md`](../architecture.md)
- 代码锚点：
  - `crates/gw-store/src/stream_consumer.rs`（events:entity / events:counter 消费）
  - `crates/gw-store/src/loader.rs`（PgSQL 冷加载 + Redis 写回）
  - `crates/gw-store/src/counter_store.rs`（counter:* HINCRBY/INCRBY）
  - `crates/gw-store/src/encrypt.rs`（KekCache / 加密 payload）
  - `crates/gw-ledger/src/flusher.rs`（usage:records XADD + DLQ）
  - `crates/gw-ledger/src/log_sink.rs`（usage:detailed XADD）
  - `crates/gw-core/src/entity.rs`（所有 entity payload Rust 端类型）

## 7. Sync rule（与 CLAUDE.md §Sync rule 联动）

| 改动 | 必须同 commit 更新 |
|---|---|
| `events:entity` 的 namespace / payload 字段 | parts/01 §1.2 + parts/03 |
| `events:counter` payload | parts/01 §1.3 |
| `usage:records` / `usage:records:dlq` 字段 | parts/02 §2.1 / §2.2 |
| Rust 反向写 entity 的 op 枚举 | parts/02 §2.4 |
| PgSQL 表新列 / 表 | parts/03 + `crates/gw-store/migrations/` + architecture.md §5.1 |
| `vendor_keys.payload` / decrypted vendor credential semantic changes（例如 Kiro SigV4 JSON、Vertex OAuth quota project） | parts/03 + relevant domain spec |
| 加密 payload 列结构 | parts/03 §3.4 + architecture.md §4.3 |
| KEK 轮换状态机 | parts/03 §3.5 + architecture.md §4.3 |
| 新增 counter:* key | parts/01 §1.4 或 parts/02 §2.5 |
| 新增 Redis 黑名单 namespace（`jwt:blacklist` / `mgmt_token:blacklist` 等） | parts/01 §1.6 或 parts/02 §2.9 |
| 控制平面接触点（`config_*` 表 / `events:entity ns=config` / `gateway:instance:<id>:{capability,effective,runtime,cmd_ack}` / `commands:gateway` / `entity:gateway_command:<command_id>`） | parts/08 + 对应 [SP design doc](../design/) |
| 任何新跨进程接触点 | 新加 part 或扩展现有 part；同步本文件 §2 |

CLAUDE.md `§Sync rule` 应在本文件上线同 PR 内追加一行 `go-contracts/` 触发器（详见 §收尾 / 实施）。
