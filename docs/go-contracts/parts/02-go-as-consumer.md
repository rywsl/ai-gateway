# Part 02 — Go-as-Consumer（Rust 写、Go 读）

> 本 part 描述所有 **Rust 主动写入、Go 被动消费**的契约面。读完即可在 Go 端实施 consumer / settler / drainer。索引见 [main.md](../main.md)。
>
> 本文件吸收了原 `docs/superpowers/specs/2026-05-19-billing-c-epsilon-go-contract.md` 的契约描述部分（§2 usage:records、§4 base/deducted、§5 events:counter、§6 settled_billing）。原文件随本 PR 删除——历史 commit 内可追溯。

---

## 2.1 `usage:records` Stream（核心契约）

每条 Rust 处理过的请求（成功 / 失败 / 取消）都会落一条 `UsageRecord` 到这条 stream。Go 端在此基础上算 cost、写 deducted、推 events:counter。

### 2.1.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `usage:records` |
| Writer | Rust `gw-ledger::flusher`（`crates/gw-ledger/src/flusher.rs`），mpsc batched XADD |
| Reader | Go billing settler |
| 推荐 consumer group | `go-billing-settler` |
| 序列化 | JSON 单字段 `payload`（XADD `* payload <json>`） |
| 流量预算 | 1000 req/s ≈ 600 KB/s（每条 ~600B JSON） |
| MAXLEN 策略 | Rust **不**自动裁剪。Go 端自定 XTRIM（建议保留 ack 后 7 天） |

### 2.1.2 Payload schema — `UsageRecord`

权威 Rust 类型见 `crates/gw-ledger/src/usage.rs` 与 [C-α §ADR-Cα-1](../../superpowers/specs/2026-05-19-billing-c-alpha-usage-record.md#adr-cα-1-改名--字段重塑)。JSON 形态：

```json
{
  "ts_ms": 1716345678901,
  "request_id": "req_01HXYZ...",
  "trace_id": "00-...",

  "user_id": "u_abc",
  "child_id": "u_def",
  "api_key_id": "ak_xxx",

  "client_protocol": "openai",
  "upstream_protocol": "anthropic",
  "model": "claude-3-5-sonnet",
  "vendor_id": "v_anthropic",
  "vendor_key_id": "vk_xxx",

  "status": "ok",                  // ok | error | cancelled
  "error_code": null,              // 仅 status=error 时填，如 "GW_UPSTREAM_5XX"
  "http_status": 200,

  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567,
    "cache_read_input_tokens": 100,
    "cache_creation_input_tokens": null,
    "thinking_tokens": null,
    "multimodal_input_tokens": null
  },
  "usage_source": "upstream",      // upstream | estimated | fallback_conservative | partial

  "retry_count": 0,
  "latency_ms": 1820,
  "ttft_ms": 380,
  "is_stream": true,

  "freeze_amount_micro": 12340,
  "estimate_pricing_version": 7,

  "markup_chain_snapshot": {
    "user_group_markup": 1.0,
    "api_key_markup": 1.2,
    "vendor_markup": 1.0,
    "model_markup": 1.5,
    "product": 1.8
  }
}
```

**禁字段**：`cost_micro` —— 由 [`no_cost_micro` invariant](../../architecture.md#7-invariants-scriptscheck-invariantssh) 保证 Rust 永远不写。Go 自己算。

### 2.1.3 Go consumer 义务

每收一条 UsageRecord 必须：

1. **消费**：`XREADGROUP GROUP go-billing-settler <consumer_id> COUNT <N> BLOCK <ms> STREAMS usage:records >`。
2. **算 cost**：基于 Go 端 `BillingPricing` + `markup_chain_snapshot` + `usage` 多维。本契约不规定公式。
3. **写 deducted**：
   ```
   INCRBY counter:user:<user_id>:deducted_micro <cost_micro>
   ```
   若 `child_id` 存在，按业务规则决定扣谁的 counter（本契约不规定）。
4. **通知 Rust**：
   ```
   XADD events:counter * ns user_balance uid <user_id> op incrby
        field deducted_micro delta <cost_micro> value_after <new_value> ts_ms <now>
   ```
   （详见 [parts/01 §1.3](01-go-as-producer.md#13-eventscounter-stream)）。
5. **XACK**：成功后 ack。
6. **可选**：另写一条到 `settled_billing` 内部 stream 用于报表 / Pg 持久化（详见 §2.1.6）。

### 2.1.4 幂等性义务

`request_id` 是天然主键。同一 `request_id` 重复消费时（XREADGROUP 重投 / consumer 重启）**不得重复扣 deducted**。建议实现：

- PgSQL `usage_records(request_id PK)` + `ON CONFLICT DO NOTHING`；或
- Redis `SET NX settled:<request_id> "1" EX 86400`，命中即跳过。

### 2.1.5 错误回路

Go 算 cost 失败（公式异常 / DB 挂 / network 抖）时：

- **不 XACK**；
- 等待重试（pending 重投）；
- 多次失败 → 写 Go 自己的 DLQ stream，与 Rust DLQ（§2.2）**互不相关**；
- 告警。

### 2.1.6 `settled_billing` Stream（Go 内部 / 可选）

| 字段 | 值 |
|---|---|
| Stream name | `settled_billing` |
| Writer | Go（settle 完一条 UsageRecord 后） |
| Reader | Go 内部（报表 / Pg 持久化 / dashboard） |
| Rust 角色 | **完全不读不写** |

目的：留一条"最终结算结果"审计流，schema 由 Go 自定，允许包含 cost_breakdown / pricing_version_used / markup_chain_applied 等。Rust 不感知，schema 变更不需要通知 Rust。

非强制；Rust 期望 Go 至少有"幂等去重"机制（§2.1.4），未必走 settled_billing 实现。

### 2.1.7 流量管理

`usage:records` 是 Rust 高频写、Go 消费的核心通路。Go 端必须保证消费速率 ≥ Rust 写入速率：

- 实测：Rust 1000 req/s → 1000 record/s；
- 单 consumer 每条 < 1 ms（含 PgSQL 写）→ 1000/s 单 consumer 可达；
- 高 QPS 需多 consumer 并行（consumer group 内多 consumer name）。

Rust **不**自动裁剪 stream，避免数据丢失。Go 消费 + XACK + XTRIM 由 Go 自决，建议保留 ack 过的部分 7 天用于排查。

## 2.2 `usage:records:dlq` Stream（G11 主路径兜底）

### 2.2.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `usage:records:dlq` |
| Writer | Rust `gw-ledger::flusher`，仅当主路径 `XADD usage:records` 失败时（`flusher.rs:73-105`） |
| Reader | Go drainer（可选） |
| 推荐 consumer group | `go-billing-dlq` |
| MAXLEN | Rust 端写时 `MAXLEN ~ 100_000`（粗略限制；溢出 → metric `gateway_usage_records_lost_total`） |

### 2.2.2 Payload schema — `dlq_meta` 包裹

DLQ 条目在 UsageRecord 之外再加一层元数据：

```json
{
  "dlq_meta": {
    "first_failure_ts_ms": 1716345678901,
    "primary_failure_reason": "<error string>",
    "retry_count_before_dlq": 3
  },
  "usage_record": { /* 完整 UsageRecord，schema 同 §2.1.2 */ }
}
```

> Rust 当前实现是 G11 v1（单 Redis stream fallback）。v2 三层兜底（内存 + 磁盘 + 周期 drain + 启动恢复）见 [`docs/superpowers/specs/2026-05-19-billing-c-delta-stream-dlq.md`](../../superpowers/specs/2026-05-19-billing-c-delta-stream-dlq.md)，本契约保留兼容（Go drainer 只需处理 `dlq_meta` + `usage_record` 包裹）。

### 2.2.3 Go drainer 义务（推荐）

- 周期 XREADGROUP `usage:records:dlq`；
- 解开 `dlq_meta` + `usage_record`；
- 同 §2.1 流程算 cost + 写 deducted + XADD events:counter + XACK；
- 注意去重（与主流共用 `request_id` 索引）；
- Drain 成功率监控（`dlq_drain_total{result}`）。

Drainer 是 Go 端**可选**实现。不实现的代价：主路径写失败时记录永久丢失（已被 `gateway_usage_records_lost_total` 计数）。

### 2.2.4 不变量

- DLQ 条目**不**包含 `cost_micro`（由 `no_cost_in_dlq` invariant 强制）。
- DLQ 主路径失败的 Rust 行为：`tracing::warn!` + metric +（v1）drop。

## 2.3 `usage:detailed` Stream（可选）

### 2.3.1 Stream 定义

| 字段 | 值 |
|---|---|
| Stream name | `usage:detailed` |
| Writer | Rust `gw-ledger::log_sink::RedisStreamSink`（`crates/gw-ledger/src/log_sink.rs`） |
| Reader | Go 可选 consumer / 归档 worker |
| MAXLEN | Rust 写时 `MAXLEN ~ <配置值>`（参 `[debug]` 表，详见 [`docs/architecture.md`](../architecture.md) §4.1） |
| 触发条件 | `[debug]` sampling rate 命中，或 `ApiKeyEntry.debug_enabled / UserEntry.debug_enabled` 命中 |

### 2.3.2 Payload schema

每条携带完整请求 / 响应快照（三级 redaction 后）。字段集随 `[debug] body_capture_*` toggle 变化，不在本契约 stable surface 内。Go 端如要消费，应当读 `payload` 字段 JSON，按字段名兜底。

Go 端实现归档建议：直接 XREADGROUP 串到对象存储（gzip 压缩，分 1h 文件），保留 7 天。本契约**不**约束 Go 端是否消费。

### 2.3.3 三级 redaction 规则

由 Rust 写入前应用，详见 [`docs/architecture.md`](../architecture.md) §3 `Redactor` trait。Go 端**禁止**对接收到的 payload 再做反向脱敏（已脱敏字段就是脱敏后的样子）。

## 2.4 `events:entity` 反向写入（Rust XADD）

绝大多数 `events:entity` 写入由 Go 完成（[parts/01 §1.2](01-go-as-producer.md#12-eventsentity-stream)）。Rust 仅在两类受控场景下反向 XADD：

### 2.4.1 `vendor_key.status_change`（rate_limited 自动设置）

| 触发 | Rust 探活 / 健康统计判定某 vendor_key 速率受限（spec §1302-1303） |
|---|---|
| Payload | `ns=vendor_key id=<key_id> op=status_change field=status from=active to=rate_limited reason=rate_limited ts_ms=<now>` |
| Go 接收行为 | 更新 PgSQL `vendor_keys.payload.status` 为 `rate_limited` + 自增 version；可选写审计表 |
| Rust 后续 | Rust 自己**不**改 PgSQL；仅本地 Moka `vendor_key` 失效（下次冷加载若 Go 已写新 status 即生效，否则保留 active） |

### 2.4.2 `ipblock.upsert`（异常流量自动封禁）

| 触发 | 单 IP QPS > 100 持续 1 min（spec §1830-1831, §1885）或其他异常检测命中 |
|---|---|
| Payload | `ns=ipblock id=<cidr> op=upsert field=expires_at value=<now+3600s> reason=abnormal_traffic ts_ms=<now>` |
| Go 接收行为 | 写 `ip_blocks.payload`，包含 `cidr / scope=global / reason=abnormal_traffic / expires_at`；自增 version |
| Rust 后续 | Rust 通过 events:entity 自然消费此条 → 失效 `ipblock` cache → 下次请求拒服 |

### 2.4.3 不变量

- Rust 反向写**仅限**上述两类 op；其他实体（user / apikey / vendor / model_group / pricing / jwks / model / auth_config）的写权 Rust 一律没有。
- Rust 写时 `v` 字段**不**自行分配（Rust 不知道 Go 的下一个 version），传 `0` 或省略。Go 收到后应当：
  1. PgSQL `UPDATE ... SET version = version + 1 RETURNING version`；
  2. 把 `version` 写回 events:entity 的 `v` 字段（再 XADD 一次"权威 upsert"）；
  3. 这避免了 Rust 反向写 `v=0` 让其他 Rust 副本的 versioned upsert 误判。

> v1 实现：Rust 端可以省略 `v`，Go 端按收到的 `op=status_change` / `reason=abnormal_traffic` 字段识别"Rust 反向写"。Go 端实施完成后，可将此约定显式化为 stream 字段 `source=rust`。

### 2.4.4 失败模式

| 故障 | 结果 |
|---|---|
| Rust XADD 失败 | 仅 metric warn；rate_limited / abnormal_traffic 检测下次再触发 |
| Go 未实现接收 | Rust 反向写丢失；vendor_key 状态不会同步到 PgSQL，但 Rust 本地 health store 仍能挡（[`gw-health::KeyHealthStore`](../../architecture.md#21-crate-responsibilities)） |

## 2.5 `counter:*:frozen_micro`（Rust 写、Go 读）

| Key | `counter:user:<uid>:frozen_micro` |
|---|---|
| Writer | Rust（freeze 时 +est, release 时 -est），异步 HINCRBY |
| Reader | Rust（available 计算 + 启动期对齐） + Go（balance 审计 / 透支检测） |
| TTL | 无（永久 key，Rust 不设 EXPIRE） |

### 2.5.1 Rust 写入路径

```
请求到达：
  1. compute estimate (Part 01 §1.5)
  2. local: state.frozen_micro.fetch_add(estimate)        // 本地 AtomicI64，实时
  3. async: HINCRBY counter:user:<uid>:frozen_micro +est  // 异步刷 Redis（mpsc 队列）

请求结束：
  1. local: state.frozen_micro.fetch_sub(estimate)
  2. async: HINCRBY counter:user:<uid>:frozen_micro -est
```

详见 [C-γ §ADR-Cγ-3](../../superpowers/specs/2026-05-19-billing-c-gamma-balance-split.md#adr-cγ-3-frozen-同步到-redis)。

### 2.5.2 Go 读取语义

Go 在做 balance check（如 settle 前验证 user 不透支）时可以：

```
GET counter:user:<uid>:base_micro       → base
GET counter:user:<uid>:deducted_micro   → deducted  (Go 自己写过的)
GET counter:user:<uid>:frozen_micro     → frozen    (Rust 异步写)
available = base - deducted - frozen
```

`frozen` 是**全局视图**：所有 Rust 副本对同一 user 的 freeze 都 HINCRBY 到同一 key，累加自然多副本汇总。

### 2.5.3 一致性窗口

- Rust 本地 fetch_add 先于 HINCRBY 异步推送，存在 ~几十 ms 延迟。Go 在此窗口内看到的 frozen 偏低 → balance 可能误判为可用。**v1 接受此不一致**；Go settle 时如果检测到透支，可走"拒 settle + 告警"路径，由人工或后台对账修正。
- Rust 进程崩溃 → 本地未 flush 的 frozen delta 丢失。Redis 上已 flush 的 frozen 仍存（"幽灵冻结"问题）。详见 C-γ §未决问题 #7，v1 留 follow-up。

### 2.5.4 写权独占

- Go **禁止**直接写 `frozen_micro`（会破坏 Rust 本地 AtomicI64 与 Redis 一致性）。
- Rust **禁止**直接写 `base_micro` / `deducted_micro`。

## 2.6 `counter:*:daily_tokens:*`（Rust 写、Go 报表可读）

| Key | `counter:user:<uid>:daily_tokens:<YYYY-MM-DD>` |
|---|---|
| Writer | Rust `logging` filter，`counter_store.rs::incrby_with_expire` |
| Reader | Rust（request_filter 前置 quota 检查） + Go（报表 / 分析，**可选**） |
| TTL | 48 小时（Rust 写入时 EXPIRE） |

### 2.6.1 Rust 写入路径

`logging` filter 在请求结束时：

```
INCRBY counter:user:<uid>:daily_tokens:<UTC_today> <input + output tokens>
EXPIRE counter:user:<uid>:daily_tokens:<UTC_today> 172800   // 48h
```

详见 G3 + [`docs/architecture.md`](../architecture.md) §9 Known gaps。Rust **不**写 `events:counter` 通知 Go（这是 observational 计数，不是权威结算）。

### 2.6.2 Go 报表可读

Go 端如需"今日 token 消耗排行"等报表，直接 GET 即可。**禁止**写入。

> 注意：UTC midnight 边界存在 < 0.001% 的请求错日（[`docs/architecture.md`](../architecture.md) §8 Deliberate behaviours）；Go 端报表可接受此误差。

## 2.7 监控义务

Go 端必须暴露：

| 指标 | 类型 | 阈值 |
|---|---|---|
| `usage_records_consume_lag_secs`（XPENDING 最老 entry） | gauge | > 60 |
| `usage_records_processed_total` | counter | — |
| `usage_records_settle_failure_total` | counter | rate > 1% |
| `dlq_records_pending`（XLEN usage:records:dlq） | gauge | > 1000 |
| `dlq_drain_total{result}` | counter | drain success rate < 95% |
| `events_entity_rust_inbound_total{op}` | counter | （反向 XADD 监控） |
| `events_entity_rust_inbound_unprocessed_total` | counter | rate > 0 → 告警（Go 未处理 Rust 反向写） |
| `balance_overdraft_detected_total` | counter | 出现即告警（Go settle 时检测到 balance < 0） |

## 2.8 不变量

| 不变量 | 强制方 |
|---|---|
| Rust 永远不写 `cost_micro` | Rust `no_cost_micro` invariant（`scripts/check-invariants.sh`） |
| `usage:records` 是唯一权威 cost 输入流 | 双方约定 + `no_billing_records_stream` invariant（禁旧名 `billing:records`） |
| DLQ 条目不引入 `cost_micro` | `no_cost_in_dlq` invariant |
| `markup_chain` 是数组，不是单一 scalar `markup_product` | `no_markup_product_scalar` invariant |
| `FreezeGuard::release` 唯一释放方式 | `no_freezeguard_settle` invariant |

## 2.9 `mgmt_token:blacklist:<token_id>` Redis Key

| Key 格式 | `mgmt_token:blacklist:<token_id>` |
|---|---|
| Type | STRING |
| Writer | Go（用户主动撤销 / 管理员强制 revoke） |
| Reader | Rust（`/healthz/*` 与 management-path `request_filter` 在 `verify_mgmt_token_secret` 之前查 `EXISTS`） |
| TTL | Go 设为 token 的剩余有效期 |

写入语义与 [`jwt:blacklist:<jti>`](01-go-as-producer.md#16-jwtblacklistjti-redis-key) 一致。Rust 端 `RedisJwtBlacklist::check_mgmt`（`crates/gw-auth/src/jwt_blacklist.rs`）做一次 `EXISTS`：

- 命中 → 401 `GW_AUTH_MGMT_TOKEN_REVOKED`；
- Redis 不可达 → 默认 `BlacklistFailMode::FailClosed`（仍 401），与 JWT 黑名单共用 `AuthConfig.blacklist_fail_mode` 开关。

Go 写入示例：

```
SET mgmt_token:blacklist:<token_id> "1" EX <remaining_seconds>
```

`token_id` 即 `ManagementTokenEntry.token_id`（前缀 `mt_`，详见 [parts/03 §3.2.13](03-shared-schema.md#3213-management_tokenspayload--managementtokenentrymigration-0007)）。

## 2.10 未决问题

1. **`settled_billing` 是否要 Rust 强制要求 Go 保留**：本契约不强制（Rust 不读）。Go 自决保留与否。
2. **`usage:records:dlq` 在 v2 三层兜底（C-δ）后的 payload 是否扩展**：v2 加 `dlq_layer` 字段（mem / disk / redis），Go drainer 必须兼容。
3. **`counter:*:frozen_micro` 副本级子键**（C-γ §未决问题 #7）：解决"幽灵冻结"问题需要 `frozen_micro:<replica_id>` 子键，本契约 v1 不涵盖，留 follow-up。
4. **Rust 反向写 `events:entity` 的 source 标记**：v1 用 `reason` 字段隐式识别；v2 加显式 `source=rust` 字段，Go 端读时可显式分流"Rust 反向写" vs "Go 自身写"。
