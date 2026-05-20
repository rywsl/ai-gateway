# C-ε — Go 侧计费 settlement 契约（v2026-05-19）

> **Integration status**: integrated by [2026-05-20-v2-integration-design.md](./2026-05-20-v2-integration-design.md). See §2 表 A / 表 B / 表 D and §3 不变量 for cross-spec resolutions and authoritative type / stream / metric locations.


- **日期**：2026-05-19
- **状态**：Draft（待审阅；**文档型 spec**，Rust 端无实现，描述 Go 侧需要做的事）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 6 总览）
  - [2026-05-19-billing-c-alpha-usage-record.md](./2026-05-19-billing-c-alpha-usage-record.md)（Go 消费的 stream）
  - [2026-05-19-billing-c-beta-estimate-pricing.md](./2026-05-19-billing-c-beta-estimate-pricing.md)（Go push 给 Rust 的 entity）
  - [2026-05-19-billing-c-gamma-balance-split.md](./2026-05-19-billing-c-gamma-balance-split.md)（Go 写 base/deducted 与 events:counter）
- **范围**：把 Rust ↔ Go 之间的契约面**单独文档化**，作为后续 Go 端实施 plan 的输入参考。Rust 端不实现本 spec，但本 spec 是 Rust 侧需求约束 Go 行为的权威源。
- **不在范围**：
  - Go 内部 BillingPricing 公式、阶梯、表达式（Go 团队自定）；
  - 用户余额充值流程（Go 管理面已有，不在网关协议层）。

> 用户决议：Rust 只记录用量，公式归 Go。Go 端的实现 spec 不归本仓库，但 Rust 端依赖的契约必须固化。

---

## 1. Rust 期望 Go 做的事

总览：

```
Rust 生成 → Go 消费              Go 维护 → Rust 消费
─────────────────────         ────────────────────────────────
stream:usage:records          entity:estimate_pricing:<model>
stream:counter:frozen:dlq     entity:billing_pricing:<model> (内部用)
                              counter:user:<uid>:base_micro
                              counter:user:<uid>:deducted_micro
                              events:counter (Go XADD)
                              stream:settled_billing (Go 内部审计)
```

## 2. 契约 1：消费 usage:records

### 2.1 stream 详情

| 字段 | 值 |
|---|---|
| stream name | `usage:records` |
| 写者 | Rust（每个请求 1 条） |
| 读者 | Go（consumer group 推荐 `go-billing-settler`） |
| payload | JSON 形态 `UsageRecord`（参 C-α §ADR-Cα-1） |
| stream MAXLEN | 无限（不建议自动裁剪，参 ADR-Cε-3） |

### 2.2 消费义务

Go consumer group **必须**：

1. **消费**：周期 `XREADGROUP GROUP go-billing-settler <consumer> COUNT N BLOCK <ms>`；
2. **算 cost**：对每条 UsageRecord 应用 `BillingPricing` 公式（多维 + markup chain + 阶梯）；
3. **写 deducted**：`HINCRBY counter:user:<uid>:deducted_micro +cost_micro`；
4. **写 events:counter**：XADD `events:counter` 通知 Rust 缓存失效；
5. **XACK**：成功后 ack；
6. **XPENDING 监控**：unacked 累积 = settler 跟不上；告警；
7. **写 settled_billing**（可选）：把"最终结算结果"另写一条到内部 stream，供 Go 自己报表 / Pg 持久化。

### 2.3 幂等性义务

UsageRecord 有 `request_id` 字段。Go consumer **必须**保证：

- 同一 request_id 重复消费时（XREADGROUP 重投）→ 不重复扣 deducted；
- 实现方式（建议）：Go 端用 PgSQL `usage_records` 表的 `request_id` 唯一约束 + ON CONFLICT DO NOTHING；或 Redis SET NX `settled:<request_id>` TTL 24h。

### 2.4 错误回路

Go 算 cost 失败（公式异常、数据库挂）时：

- **不能** XACK；
- 等待重试（pending 重投）；
- 多次失败 → DLQ（Go 自己定的 stream，与 Rust DLQ 无关）；
- 告警。

### 2.5 stream 长度管理

`usage:records` 是 Rust 高频写、Go 消费的核心通路。Go 端必须保证消费速率 ≥ Rust 写入速率：

- 实测：Rust 写入 1000 req/s → 1000 record/s；
- Go consumer 每条 < 1ms（含 PgSQL 写）→ 单 consumer 可达 1000/s；
- 高 QPS 场景需要多 consumer 并行（consumer group 内多 consumer name）。

Rust **不**自动裁剪 stream，避免数据丢失。Go 消费 + XACK + XTRIM 由 Go 自己定（建议保留 7 天 ack 过的部分用于排查）。

## 3. 契约 2：维护 entity:estimate_pricing

### 3.1 entity 详情

| 字段 | 值 |
|---|---|
| key 形态 | `entity:estimate_pricing:<model_name>` |
| 写者 | Go（每次 BillingPricing 更新时同步推算 EstimatePricing） |
| 读者 | Rust（懒加载到 EntityStore） |
| 字段 | 参 C-β §ADR-Cβ-1：`model / conservative_per_token_micro / default_max_tokens / hard_cap_tokens / safety_factor / v` |

### 3.2 推算义务

每当 Go 端修改某模型的 BillingPricing 时，**必须**同步推算并 push 对应的 EstimatePricing。推算公式（建议）：

```
conservative_per_token_micro = ceil(
    max(
        output_per_token_micro,
        cache_creation_per_token_micro,
        thinking_per_token_micro,
        multimodal_per_token_equivalent,
        ...所有维度的 per-token 最大可能值
    ) * markup_max_chain * 1.1   // 10% 余量
)
```

`markup_max_chain` = 系统内已知所有 markup 链路的最大可能乘积（user_group × api_key × vendor × model 四级各自上限）。

**必须**保证：`conservative_per_token_micro × N` ≥ 任何 N tokens 的真实 cost。否则 freeze 不足，Rust 放行的请求实际花费 > 用户余额 → Go 端 settle 时透支。

### 3.3 事件通知

EstimatePricing 更新时 Go 必须 XADD `events:entity` namespace=`estimate_pricing` op=`upsert/delete` 触发 Rust 缓存失效。

### 3.4 失败模式

- Go 改了 BillingPricing 但忘记推算 EstimatePricing → Rust 用老 freeze 估算 → 可能透支；
- 防护：CI 检查（Go 改 BillingPricing 必须改 EstimatePricing）；或 Go 写 BillingPricing 时自动触发 EstimatePricing 重算。

## 4. 契约 3：写 base / deducted counter

### 4.1 counter 详情

| key | 写者 | 读者 |
|---|---|---|
| `counter:user:<uid>:base_micro` | Go（充值/调整时） | Rust（启动期 + events:counter） |
| `counter:user:<uid>:deducted_micro` | Go（每次 settle UsageRecord 后） | Rust（启动期 + events:counter） |
| `counter:user:<uid>:frozen_micro` | Rust（freeze/release） | Rust + Go |

### 4.2 deducted 写入义务

每次成功消费 UsageRecord 后，Go **必须**：

1. `HINCRBY counter:user:<uid>:deducted_micro +cost_micro`；
2. XADD `events:counter` 内容（参 C-γ §ADR-Cγ-4 payload）：
   ```json
   {
     "ns": "user_balance",
     "uid": "u_xxx",
     "op": "incrby",
     "field": "deducted_micro",
     "delta": <cost_micro>,
     "value_after": <new_value>,
     "ts_ms": <ms>
   }
   ```
3. **不**写 `frozen_micro`（Rust 自管）；
4. 监控写延迟（HINCRBY → events:counter 之间应 < 50 ms，否则 Rust 看不到新值时间长）。

### 4.3 base 写入

充值流程（Go 管理面）：

1. 验证支付到位；
2. `HINCRBY counter:user:<uid>:base_micro +delta`；
3. XADD `events:counter` field=`base_micro`；
4. 持久化到 Pg。

调整 / 退款同理。

## 5. 契约 4：events:counter stream

### 5.1 stream 详情

| 字段 | 值 |
|---|---|
| stream name | `events:counter` |
| 写者 | Go（base / deducted 变更时） |
| 读者 | Rust（StreamConsumer 订阅） |
| payload | 参 §4.2 |

### 5.2 写者义务

- 每次 HINCRBY base / deducted 后**必须**XADD events:counter；
- `value_after` **必须**是 HINCRBY 后的当前值；
- 顺序：HINCRBY 先，XADD 后（保证 Rust 收到时 Redis 已是新值）；
- 失败重试：HINCRBY 与 XADD 之间网络分裂 → 视为发生时已记录在 Redis 上，下次启动时 Rust 启动期 HGET 拿到最新 base/deducted；events:counter 漏发不致命（只是缓存失效延迟）。

### 5.3 消费者义务（Rust 实现）

Rust 通过 `gw-store::stream_consumer` 订阅。详见 C-γ §ADR-Cγ-4。

## 6. 契约 5：stream:settled_billing（可选）

### 6.1 stream 详情

| 字段 | 值 |
|---|---|
| stream name | `settled_billing` |
| 写者 | Go（每次 settle 完一条 UsageRecord） |
| 读者 | Go 内部（报表 / Pg 持久化 / dashboard） |
| Rust 角色 | **完全不读不写** |

### 6.2 目的

把"最终结算结果"留一条审计流，独立于 `usage:records`（输入流）。typical payload：

```json
{
  "request_id": "...",
  "user_id": "...",
  "ts_ms": ...,
  "usage_breakdown": <UsageSnapshot>,
  "pricing_version_used": <v>,
  "cost_breakdown": {
    "input_cost_micro": ...,
    "output_cost_micro": ...,
    "cache_read_cost_micro": ...,
    ...
  },
  "markup_chain_applied": {...},
  "cost_total_micro": ...,
  "settled_at_ms": ...
}
```

允许 Go 改 schema，**Rust 不感知**。

### 6.3 是否必须

不必须，但强烈推荐——report 用 `usage:records` 的话每次都得 Go 重算公式；用 `settled_billing` 直接读结果。

## 7. 验收与监控建议

Go 端应当暴露：

| 指标 | 来源 | 告警阈值 |
|---|---|---|
| `usage_records_consume_lag_secs` | `XPENDING` 最老 entry 年龄 | > 60 |
| `usage_records_processed_total` | counter | - |
| `settle_failure_total` | counter | rate > 1% |
| `estimate_pricing_push_total` | counter | - |
| `events_counter_publish_lag_ms` | HINCRBY 到 XADD 延迟 | > 100 |
| `deducted_writeback_total` | counter | - |

Rust 端的对应监控（C-α / C-β / C-γ / C-δ 各自的指标）能够交叉验证 Go 端动作发生与否。

## 8. 不在 Rust 仓库范围的事

本 spec 文档化 Rust 期望，但**不实现**：

- BillingPricing 字段表与算法（Aether `services/billing/*.py` 的 Rust/Go 等价物）；
- 历史 rebilling（Aether `usage/historical_rebilling.py`）；
- cache analysis（Aether `usage/cache_analysis.py`）；
- 套餐 / 阶梯 / 表达式引擎；
- 用户充值流程。

这些都在 Go 仓库（或一份 Go 端 plan）；本 spec 仅约束**接口面**。

## 9. 未决问题

1. **Go 端实现仓库位置**：Aether 现状是 Python；用户提到"web 侧功能放在 golang 端"。本 spec 假设 Go，但实际仓库 / 团队 / 实施时间表不在本设计范围。Rust 端应当容忍 Go 端晚于自己上线（contingency：Go 未就绪时 Rust 可以单独跑，UsageRecord 堆在 stream 等 Go 起来追溯消费；deducted 不更新 → balance 看起来不扣；用户暂时不计费——可作 dev/test 形态）。
2. **events:counter 双 source 冲突**：理论上只有 Go 写 base/deducted，但如果有"管理员手动调整"也走 Go 是同一 writer。但如果 Aether 时代有遗留路径写 deducted，需要在迁移期统一收口到 events:counter。
3. **conservative_per_token_micro 推算口径**：本 spec 给的是建议公式（max × markup_max × 1.1）。Go 团队可能有更精确的口径（按模型分类）。倾向 Go 自决，本 spec 仅要求"保证 ≥ 真实 cost"。
4. **settled_billing 是否归 Rust 期望**：本 spec 把它列为"可选"。是否要 Rust 强制要求 Go 保留这条流？倾向**不强制**——Rust 不读，Go 自己决定保不保。
5. **estimate_pricing 失配时的修正路径**：若 Go 算出来发现 Rust 之前 freeze 不足（用户透支了），是否要回头改 Rust 的 freeze？倾向不——透支由 Go 端兜底处理（限制下一次请求、追账等），不要求 Rust 改变行为。需要在 alert 中包含 freeze_amount vs actual_cost 的差值监控。
6. **events:counter 与 events:entity 是否独立 stream**：参 C-γ §未决问题 #3。倾向独立，但需要 ADR-013 新增"协议 5：Counter Event"。
7. **多副本 Rust 网关下 events:counter 消费的协调**：每个副本独立读 events:counter 同步本地缓存——这是 fan-out。consumer group 选 N 个独立 consumer name（每副本一个），保证每个副本都看到所有事件。

---

> C-ε 是契约文档，不是 Rust 代码。它是 Rust 团队与 Go 团队之间的**单一来源**。任何 schema 变更（C-α / C-β / C-γ 改字段）都必须同步更新本 spec 并通知 Go 团队。
