# AILinkMax 上线前缺陷根治计划（Pre-Launch Defect Eradication Plan）

> **状态**：草案 · 双仓 mirror（本文件与 `ailinkmax-gateway/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` 必须字节一致除标题）
> **起草**：2026-06-17
> **依据**：
> - `AILinkMax-Go/docs/issues/2026-06-17-p0-audit-report.md`（原始 Go 审计，60+ 项）
> - `ailinkmax-gateway/docs/issues/2026-06-17-p0-bug-audit-findings.md`（原始 Rust 审计，37 confirmed + 3 FP）
> - 5 个 sonnet 并行复审（详见附录 A），追加 21 + 25 = **46 项新真实缺陷** + **45 项原修复方案的对抗式打回**
> **总规模**：**150+ 项**缺陷需根治（含已证伪 6 项需明确归档）

---

## 0. 元信息与执行说明

- 本文档是**主计划（master plan）**，不是 TDD 步骤计划。每个工作流（WS-N）到执行阶段，将基于 `superpowers:writing-plans` 技能展开为 2-5 分钟 TDD 步骤的子文档。
- 每个工作流单独可执行、可验收，但跨工作流有强依赖时本文档显式标注 `BLOCKED-BY`。
- 任何 WS 的 DoD 不达成，**不允许进入合并队列**（参见 §7 验收门）。
- 本文档随发现演进；任何新增问题在 §8 持续发现机制下纳入对应 WS，**不另起编号体系**。

---

## 1. 五条强制原则（项目期内不可商榷）

| # | 原则 | 落实方式 |
|---|---|---|
| **P1** | **根因修复**：从架构层消除问题类，禁止单点止血 | 每个 WS 必须改的是模式/不变式/契约，不是局部 if-else |
| **P2** | **零回归**：任何修复方案落地前必须经独立 sonnet 对抗审查 | 每个 WS 的 DoD 包含"对抗审查通过"清单；本文档已对 WS-1/WS-2 完成首轮（附录 B），其他 WS 在执行阶段补 |
| **P3** | **完全修复**：禁止"先发布后补"或"留尾巴" | 每个 WS DoD 包含"chaos 注入测试"，必须通过；不允许 known-broken 上线 |
| **P4** | **零疑点**：任何设计层不确定**立即派 sonnet/haiku 独立调查**，未消除疑点不入执行 | 本文档中所有 `INVESTIGATE-N` 标记必须在对应 WS 启动前清零 |
| **P5** | **意外发现强制纳入**：执行/审查中发现的、不在本计划内的真实缺陷，同等优先级纳入对应 WS | §8 提供模板；不允许"超出范围，下次再说" |

> **说明**：上述五条不是一般倡议，而是合并门禁。任何 PR / commit 触发 §7 验收门的 lint 不通过即拒。

---

## 2. 来源、范围、规模

### 2.1 来源分布

| 来源 | 数量 | 备注 |
|---|---|---|
| 原始 Go 审计 P0-CRIT | 4 | M1-M4 |
| 原始 Go 审计 P0-HIGH | 13 | H1-H13 |
| 原始 Go 审计 P1-MEDIUM | 8 | Panic site 1/2/5、D4、S4、SLQ1/3/5、B1 |
| 原始 Go 审计 P2-LOW | 5 | A2、SLQ4/6/7/8 |
| 原始 Go 审计 M6 特别 | 1 | 降级到 P2 |
| 原始 Go 已证伪 | 5 | M5、D5、SLQ2、SLQ9、A1（B2 半证伪也归此） |
| 原始 Rust 审计 P0 | 21 | Billing-1..8、Proxy-1/2/3/6/7/8、Tunnel-1..5、Task-Runtime-1/2、Codec-2/3/4/5、Routing-1..6 |
| 原始 Rust 审计 P1 | 12 | Billing-9、Proxy-5/9/10、Routing-7、Tunnel-6 等 |
| 原始 Rust 已证伪 | 3 | FP-1、FP-2、FP-3 |
| **追加 Go 新发现** | **21** | G1-G14 + S1-S7（详见 §2.2） |
| **追加 Rust 新发现** | **25** | R1-R25（详见 §2.3） |
| **设计回归** | **45** | WS-1/2 原案对抗审查打回（详见附录 B） |
| **跨进程契约** | **15** | C1-C15 contract integrity（详见 §2.4） |

### 2.2 Go 新增真实缺陷（5 个 sonnet 之 1，agentId `ab6bb6e04ee3458b7`）

> 完整描述（file:line + 字面代码 + 影响 + mitigation grep）见对应 WS 子节，下表只列 ID 与归属。

| ID | 一句话 | 归属 WS |
|---|---|---|
| G1 | 订阅 quota TOCTOU：Check + Apply 不在同 tx 也无 row lock，并发可超支 | WS-1 |
| G2 | Refund.Create 不查 pending refunds，admin 双批可超退 | WS-1 |
| G3 | wallet outbox ORDER BY id 不是 commit 序，relay 写陈旧余额到 Redis 无 TTL | WS-1 |
| G4 | payment webhook 重复 callback 直接返回，错失修 M2 的重试窗口 | WS-1 |
| G5 | subscription.Cancel UPDATE 无 status 守卫，可"复活"已过期/升级订阅 | WS-1 |
| G6 | CycleService.RollIfDue 用 pre-lock 副本判分支，与并发 Cancel 竞态多送一周期 | WS-1 |
| G7 | subscription.Upgrade 非原子：旧标 expired 后才开 Purchase tx，Purchase 失败留孤儿 | WS-1 |
| G8 | InflightReaper XADD 后 DEL，DEL 失败下一轮再发 → usage:records 重复爆 MAXLEN | WS-2 |
| G9 | billing.Reconciler N+1 round-trip 且首错全 abort，正发生故障时观测黑屏 | WS-9 |
| **G10** | **mgmttoken.Blacklist.Revoke 用 `_ =` 吞 Redis 错，已撤销 token 仍授权 5 分钟（P0）** | WS-5 |
| G11 | auth email-verify 无 per-target rate limit，6 位码可暴力破 | WS-5 |
| **G12** | **apikey 本地缓存 60s 在 Update/Toggle/Delete/Rotate 后从不调用 `cache.Delete`，已撤销/旋转密钥继续放行（P0）** | WS-5 |
| G13 | Logout 路由 `_ = svc.RevokeRefresh`，refresh JTI 未拉黑名单 | WS-5 |
| G14 | OAuth bind-token 进程内 map 无 janitor、多实例 LB 下不一致 | WS-3 / WS-5 |
| S1 | pgxpool/CH lazy 不 Ping，错配置时 `/livez`/`/readyz` 撒谎 | WS-10 |
| S2 | binaries/web Shutdown 后 defer Close 与 invalidator 异步 XAck 竞态 | WS-2 |
| S3 | all-in-one errgroup 让 worker 错误连坐杀 HTTP listener | WS-4 |
| S4 | binaries/web cache invalidator 是裸 goroutine，不走 supervisor | WS-4 |
| S5 | rate limiter cleanup 在 DoS 下走 O(N) 删 0 项，每请求重复 | WS-3 / WS-7 |
| S6 | cache.GetOrLoadSF singleflight 跨 callsite 同 key 不同 T 会 `v.(T)` panic | WS-7 |
| S7 | httplog 把 r.URL.Path 与 UserAgent 写入日志无脱敏 | WS-10 |

### 2.3 Rust 新增真实缺陷（sonnet 2，agentId `ab9a0b56c409b36bd`）

| ID | 一句话 | 归属 WS |
|---|---|---|
| **R1** | **legacy `verify_jwt`（默认配置走的路径）只校验 exp，无 iss/aud/nbf（P0 跨租户冒充）** | WS-5 |
| **R2** | **HMAC ±300s 仅校验时间戳，无 nonce/jti 缓存（P0 5 分钟内可重放）** | WS-5 |
| **R3** | **JWT kid 不与配置交叉校验，未知 kid 静默回退 static_pem_rs256，一把密钥被偷=全局伪造（P0）** | WS-5 |
| **R4** | **IpRateLimit + AuthFailureLimiter 用未验证的 X-Forwarded-For 做 key，暴力破限速被绕过（P0）** | WS-5 |
| **R5** | **TaskFreeze.release CAS-released 在 async releaser 前，Redis 抖一下永久泄漏 frozen 余额（P0）** | WS-1 |
| **R6** | **inflight_reconciler 在 XADD 成功后 `?` 抛 DEL 错误，下次启动重复发 → 同一 (rid, attempt) 双计费（P0）** | WS-1 |
| **R7** | **保护模式下 `actual_usage_credits` 返回 Ok(0) + 200 OK，被 `parse_response` 失败触发即免费请求（P0）** | WS-1 / WS-6 |
| **R8** | **`reset_response_attempt_state` 不清 upstream_body_accum/operation_id/quota_snapshot/stream_state，response-side 重试拼 Frankenstein body + 错位 quota（P0）** | WS-6 |
| **R9** | **release_task_freeze 失败被 `let _ =` 吞，task_id 又是新 UUID 未持久化，无 sweeper 可回收（P0）** | WS-1 |
| R10 | FormatHealth retry_after_secs 无 clamp，u32::MAX 让 vendor 永久下线 | WS-7 |
| R11 | FormatHealth 录条件用排他 else-if，半开探针成功也可能再 trip | WS-7 |
| R12 | WeightedRr Phase 1 对不可用候选也 fetch_add 权重，恢复时雪崩 | WS-7 |
| R13 | JWT 根据客户端 header.alg 选 key family，双密钥并存期可降级 HS256 | WS-5 |
| R14 | RedosTracker 检查 budget 在 match 之后，前 10 次慢正则裸跑 | WS-3 / WS-5 |
| R15 | inflight_reaper Prom 计数器带 `request_id` 高基数标签，scrape 几小时撑爆 Prom | WS-3 / WS-10 |
| R16 | DetailedArchive worker `tokio::spawn` 无 supervisor | WS-4 |
| R17 | QuotaStateProbeRunner `tokio::spawn` 无 supervisor | WS-4 |
| R18 | instance_id heartbeat task 无 supervisor，panic 后 TTL 到期被误判 dead | WS-4 |
| R19 | counter_store.incrby_with_expire 吞 EXPIRE 错误，`daily_tokens:*` 永久无 TTL | WS-3 |
| R20 | LedgerCounterHandler 把 malformed `lease_credits` 默认 0，覆盖在用 credits → 用户被锁 | WS-5 |
| R21 | InflightLedger HSET + EXPIRE 非原子，HSET 成功 EXPIRE 失败 → 永生 key | WS-2 |
| R22 | stream_consumer XACK 失败留 PEL，无 XAUTOCLAIM，PEL 永增长（新 site，独立 Task-Runtime-2） | WS-2 |
| R23 | QuotaStateWriter PG commit 后 Redis DEL + XADD 都是 best-effort，集群缓存陈旧到自然 TTL | WS-2 |
| R24 | 跨协议非流式 parse_response 失败时把上游 protocol bytes 直接 verbatim 发给客户端 | WS-6 |
| R25 | verify_mgmt_token_secret 用 4 种不同 GW_AUTH_MGMT_TOKEN_* 错误码 → 攻击者拿到状态预言机 | WS-5 |

### 2.4 跨进程契约 issue（sonnet 5，agentId `aab9678de124f1f9c`）

| ID | 一句话 | 归属 WS |
|---|---|---|
| C1 | UsageSnapshot 8 个新字段（audio/prediction/cache_tier/web_search）已在 IR 存在但未传至 Go，rustpact 也不会 dedup 进 provenance 向量 | WS-8 |
| **C2** | **Go 的 `provenanceRank` 表完全没有 `cli_extracted`，已 live 在 main（CLI billing-guard 失效）** | WS-8 / WS-1 |
| C3 | Terminated 语义未统一：Rust 没强制 credits_consumed=Some(0)，Go 没有据此 force cost=0 | WS-8 / WS-1 |
| **C4** | **Go 的 dedupKey 不带 attempt_index，跨重试同 rid 即被去重 → 真实结算被早期失败 attempt 顶掉（生产中已发生）** | WS-8 / WS-1 |
| C5 | request_id 必须显式声明为 Rust-allocated 不可信客户端 | WS-8 |
| C6 | 拟加的 `dup_safe` 字段与 provenance 仲裁重复，应不加 | WS-8 |
| C7 | wallet:idem → PG 迁移期需 dual-write 否则 SETNX vs INSERT 双开 → 双扣 | WS-1 |
| C10 | `docs/rust-contract/parts/*` 比 Rust main 落后 7 commits（含 AttemptedKey、DLQ on-disk 格式、control-plane） | WS-8 |
| C11 | rustpact 无 protocol version，Rust 加字段、Go 静默忽略 = silent data loss | WS-8 |
| C12-C15 | 多项 NONE 或 DOC-ONLY | 不动 |

### 2.5 已证伪条目（一次性归档，确认不修）

| ID | 反证 |
|---|---|
| Go M5 | float64 精度数学证伪：max 6×10^12 < 2^53 ≈ 9×10^15 |
| Go D5 | 全局 32MiB MaxBytesReader 抵消，dead code |
| Go SLQ2 | env 默认不开，worker 完全不启 |
| Go SLQ9 | 10 分钟一次后台任务，串行 by design |
| Go A1 | `VideoTaskService.Submit` 0 调用方，dead code |
| Go B2 | bind 失败进程死 → LB 标 unhealthy，仅 worker/invalidator 静默部分另有归并 |
| Rust FP-1 | gw-gateway 高层在 worker drain 后显式 `dump_to_shutdown_file`，跨 crate 兜底 |
| Rust FP-2 | spill_root 引导期无条件设置，InMemory 是 unit-test dead code |
| Rust FP-3 | CacheAffinity 是 stateless 单结构体，无持久化 |

**处置**：每条在源文件相应行添加注释，引用本文件 §2.5；CI lint 阻止后续 PR 把它们重新报为缺陷。

---

## 3. 根因聚类十大工作流

整张计划按下面 10 个根因聚类组织。每个 cluster 治一类问题，而非治多条具体 bug。

| WS | 根因 / 名称 | 治理范围 | 估时 | 跨仓 |
|---|---|---|---|---|
| WS-1 | Commit-then-side-effect（钱、幂等、补偿） | M1-M4, R5-R7, R9, G1-G7, G10 一致性子集 | 7-10 人日 | Go + Rust |
| WS-2 | ACK-before-side-effect + Worker/Drainer 生命周期 | H3, H4, H5, H13, G8, R21, R22, R23, S2 | 5-7 人日 | Go + Rust |
| WS-3 | 内存有界不变式（buffer/map/cache） | H6, Proxy-1/2/3/8, Codec-4, Routing-1..4, Routing-7, R15, R19, S5, G14 | 5-7 人日 | Go + Rust |
| WS-4 | Panic 兜底与 Supervisor 不变式 | H1, H2, Panic-site 1/2/5, S3, S4, R16, R17, R18 | 3-4 人日 | Go + Rust |
| WS-5 | 鉴权 / 守卫 fail-closed | H7, H8, H9, H10, D4, S4(P1), Routing-5, Routing-6, R1, R2, R3, R4, R13, R14, R20, R25, G10, G11, G12, G13 | 5-7 人日 | Go + Rust |
| WS-6 | HTTP 响应完整性 | Proxy-5, Proxy-6, Proxy-7, Proxy-9, Proxy-10, R7, R8, R24 | 4-6 人日 | Rust 主 + 少量 Go |
| WS-7 | 并发与数值安全 | H11, H12, Billing-7, Billing-9, S6, R10, R11, R12 | 3-4 人日 | Go + Rust |
| WS-8 | 用量 schema 与跨进程契约 | Codec-2, Codec-3, Codec-5, C1-C11 | 4-5 人日 | 跨仓强协调 |
| WS-9 | 性能 / 查询治理 | M6, SLQ1, SLQ3-8, A2, B1, G9 | 4-6 人日 | Go 主 |
| WS-10 | 运维 hardening（配置/观测/runbook） | SH1-3、S1, S7, R15(obs)、所有新 metric 暴露 | 3-4 人日 | 跨仓 |

总人日范围：43-60 人日。建议 7 人团队并行 2 周完成。详细排序见 §6。

---

## 4. 工作流子计划

> 每个 WS 包含：① 根因陈述 ② 覆盖的 finding ID 全集 ③ 对抗审查打回点 ④ **修正后的架构方案** ⑤ 任务清单（高度抽象，执行时再展为 TDD） ⑥ DoD（含故障注入测试） ⑦ 依赖。

---

### WS-1 Commit-then-side-effect（钱 / 幂等 / 补偿）

#### 根因
跨进程 / 跨 DB / 跨 tx 的多步操作里，"状态成功"的可见性早于"副作用完成"。任一步失败留下状态-副作用不一致的"半完成"，无法重试也无法回滚。

#### 覆盖（21 项）
- Go 原：M1、M2、M3、M4
- Go 新：G1、G2、G3、G4、G5、G6、G7、G10（fail-closed 子集）
- Rust 原：Billing-1、Billing-2、Billing-3、Billing-4、Billing-5、Billing-6、Billing-8
- Rust 新：R5、R6、R7、R9
- 契约：C2、C3、C4、C7

#### 对抗审查打回（sonnet 3，agentId `abc9b119111da75e9`）
原案被打回的关键点（全部已采纳到本节修正方案）：
1. **M1**：`idempotency_keys` 不能简单"放进 InTx"。`READ COMMITTED` 下两个并发 caller 都看不到对方 row，PK 冲突返回 23505 错误形态与现有 `(false, nil)` short-circuit 不兼容。改为：`INSERT ... ON CONFLICT (op, key) DO NOTHING RETURNING xmax`，`xmax=0` 表示自己写入，否则查 `response_jsonb` 列复现历史结果。
2. **M2**：`defer Release()` 不能保证与 InTx rollback 原子（pgx 无 client AfterRollback hook）。**取消 defer-Release 路径**，改为 idempotency_keys row 写入与 wallet 余额变更在同一 InTx；不依赖外部 release。
3. **M3 + M1 必须同 PR**：outbox 处理器的正确性建立在 M1 的 idem table 之上，两者不可拆分发布。
4. **M2 webhook 5xx**：重复 callback 必须查 outbox.status 决定返回（`committed`→200，`pending` 且老→5xx，`pending` 且新→200 让我方 worker 跟），否则与平台重试形成 ABA livelock。
5. **M5（refund 跨模块同 tx）**：违反 `scripts/check-mod-deps.sh`，不可行。**outbox 是唯一路径**，全部 M2/M3/M4 走同一种 outbox。
6. **M4 idemKey**：服务端"requestNonce"语义模糊，admin 双击仍可生成两个 nonce。**强制 client 端 `Idempotency-Key` header，空即 400**。
7. **R1 LeaseGuard**：源码现在**没有 `settled` 字段**，原案"Drop check settled" 不可实现。改为：新增 `settled: AtomicBool`，`settle_actual` 和 `release` 都设置之；Drop 调用 `release()`；`release` 内 `if !settled { refund full reservation }`。
8. **R3 pipeline RTT**：MULTI/EXEC 在 Sentinel 故障切换下不保证原子。**改为单条 Lua 脚本** `EVAL "XADD ... ; HSET inflight state settled ; DEL inflight"`，单 shard 原子。
9. **R3 + 重排**：Lua 之外，reconciler 在发 CrashFallback 前必须查 Go 侧 `billing:settled:{rid}:{attempt}` SET 兜底（Rust 部分 XADD 已成功的边界）。
10. **R5 deploy ordering**：`attempt_index` 是写契约破坏。**强制 Go 先部署**（读端兼容 `attempt|0`），Rust 后部署。CI 检查跨仓部署顺序。
11. **R6 replay file**：rename → push 之间崩溃会导致下次 replay 复发 1499/1000 条。改为**每 envelope SETNX** Redis 去重键（24h TTL）。
12. **R7 catch_unwind 内存**：clone batch 翻倍内存。改为**先 spill 到磁盘**再 flush，成功删；panic 时磁盘文件成 recovery 源。
13. **R8 DEL 重排**：无 bounded queue。**取消重排，全依赖 reconciler**。
14. **G2 新 bug**：outbox 设计若不在 Refund 通道也校验，会引入"先付钱后退款失败"。Refund handler 必须查 `payment_outbox.status='committed'` 前置守门。

#### 修正后的架构方案

**Go 侧**：

S1.1 新 PG 表 `idempotency_keys`：
```sql
CREATE TABLE idempotency_keys (
  op            text         NOT NULL,
  key           text         NOT NULL,
  status        text         NOT NULL CHECK (status IN ('committed')),
  request_hash  text         NOT NULL,            -- sha256(canonicalize(payload))
  response_jsonb jsonb       NOT NULL,            -- 历史响应，供重复请求复现
  created_at    timestamptz  NOT NULL DEFAULT now(),
  PRIMARY KEY (op, key)
);
CREATE INDEX idempotency_keys_created_at_idx ON idempotency_keys (created_at);
```

S1.2 `Idem` 接口替换：
```go
// Reserve+Commit 由 wallet InTx 内通过单条 INSERT ... ON CONFLICT 决定。
// 不再暴露 Release，因为不再有 reserve-then-rollback 这一中间态。
type Idem interface {
    // Commit 在 wallet tx 内调用。返回 (didWrite bool, prior *Response, err error)。
    // didWrite=true: 我们写入了行，应继续业务逻辑；prior==nil。
    // didWrite=false: 已存在，prior 含历史响应；调用方直接返回 prior 给客户端。
    Commit(ctx context.Context, tx pgx.Tx, op, key string, requestHash string, response any) (didWrite bool, prior *json.RawMessage, err error)
}
```

S1.3 wallet.Deduct/Credit/Refund/adminDebit 全部接受 `(tx, op, key)`，在 InTx 内最后一步调 `idem.Commit`。失败 → tx rollback → idem 行不存在 → 重试可重新进入；成功 → 行存在 → 重试复用 response_jsonb。

S1.4 新表 `payment_outbox`：
```sql
CREATE TABLE payment_outbox (
  id           bigserial PRIMARY KEY,
  payment_id   bigint       NOT NULL UNIQUE,    -- 一笔 payment 一行
  payload      jsonb        NOT NULL,           -- 给 wallet.Credit 用
  status       text         NOT NULL CHECK (status IN ('pending','committed','failed')),
  attempt      int          NOT NULL DEFAULT 0,
  next_attempt_at timestamptz NOT NULL DEFAULT now(),
  created_at   timestamptz  NOT NULL DEFAULT now(),
  updated_at   timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX payment_outbox_due_idx ON payment_outbox (next_attempt_at) WHERE status='pending';
```

S1.5 `payment.webhook.Receive` 重写：
- 同 tx 写 payment_row.status='succeeded' + payment_outbox(status='pending')。
- handler 返回 HTTP 5xx 当 outbox 行写入失败；否则 200。
- 重复 callback 命中 `payment_id` UNIQUE → 查 outbox.status：
  - `committed` → 200
  - `pending` 且 `created_at > 5min ago` → 5xx（请平台再试）
  - 否则 → 200

S1.6 `payment_outbox_processor` worker：
- 每 30s 扫 `WHERE status='pending' AND next_attempt_at<=now() LIMIT 100`。
- 对每行：
  1. SELECT idempotency_keys WHERE op='credit' AND key=outbox.payment_id 检查是否已 credit。
  2. 已 credit → 直接 UPDATE outbox SET status='committed'。
  3. 未 credit → 在新 tx 内调 `wallet.Credit(tx, op='credit', key=payment_id, ...)` → 成功 commit + outbox SET committed；失败 attempt++、next_attempt_at=now+backoff、status 仍 pending。

S1.7 Refund.Complete 同样走 outbox：
- 同 tx 写 refund.status='succeeded' + refund_outbox(status='pending')。
- `refund_outbox_processor` worker 拉 wallet.Refund。
- **Refund.Create 前置守门**：`SELECT 1 FROM payment_outbox WHERE payment_id=$1 AND status='committed'`，未 committed 返回 `ErrPaymentStillProcessing`。

S1.8 G1 subscription quota：
- `ApplySubscriptionUsage` 与 `CheckQuota` 合一为 `CheckAndApply` 走单 tx，外加 `SELECT ... FOR UPDATE` 锁定 `user_subscriptions` 行。
- migration 加 `CHECK (cycle_used_micros <= cycle_quota_micros + overage_cap)` 防守。

S1.9 G2 refund double-approve：
- `refund.Create` 前 `SELECT SUM(amount_micros) FROM refund_requests WHERE wallet_id=$1 AND status IN ('pending_approval','approved','processing')` 与 `bal.RechargeMicros` 比对。
- `SELECT ... FOR UPDATE` 锁定 wallet 行。

S1.10 G3 wallet outbox 顺序：
- 加 `commit_timestamp` 列（`pg_xact_commit_timestamp` 或 `clock_timestamp() AT TIME ZONE 'UTC'`）。
- relay `ORDER BY commit_timestamp ASC, id ASC` + `SET ... EX 86400` 给 Redis 设过期。

S1.11 G4 webhook 重复 callback：由 S1.5 webhook 重写自然覆盖。

S1.12 G5 subscription.Cancel：
- 加 `WHERE id=$1 AND status='active'`；检查 RowsAffected==1 否则返回 `ErrSubscriptionNotActive`。

S1.13 G6 RollIfDue：
- 在 lock 内**重新读**当前订阅状态（不用 pre-lock 副本），决定分支。

S1.14 G7 subscription.Upgrade：
- 在单 tx 内 `UPDATE old SET status='expired'` + Purchase 同 tx，回滚二者一体。

S1.15 G10 mgmttoken Blacklist.Revoke：将 `_ = b.r.Set(...).Err()` 改为 `return b.r.Set(...).Err()`。**这其实是 fail-closed 修复，但根因属本 WS**。

**Rust 侧**：

S1.16 `LeaseGuard` 加 `settled: AtomicBool`：
```rust
pub struct LeaseGuard {
    uid: u64,
    reserved: i64,
    coef_version: u32,
    released: AtomicBool,
    settled: AtomicBool,       // NEW
    state: Arc<LeaseState>,
}

impl LeaseGuard {
    pub fn settle_actual(&self, actual: i64) {
        if self.settled.swap(true, AcqRel) { return; }      // idempotent
        let refund = self.reserved.saturating_sub(actual);
        if refund > 0 { self.state.credits.fetch_add(refund, AcqRel); }
    }
    pub fn release(&self) {
        if self.released.swap(true, AcqRel) { return; }
        // 如果尚未 settle，等价于 settle_actual(0)
        if !self.settled.load(Acquire) {
            self.settle_actual(0);
        }
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) { self.release(); }
}
```

S1.17 logging.rs Err 分支 + CLI 路径：删除 `warn!` 直返；显式 `guard.release()` 即可触发 `settle_actual(0)` + Terminated 通过 UsageRecord 显式 emit。

S1.18 R5 TaskFreeze 同样的 settled+released 模型，Drop 走 release。Redis releaser 失败时同步重试 3 次 + 落 DLQ 文件（不能纯 `tokio::spawn` fire-and-forget）。

S1.19 R6 inflight_reconciler：在 `?` 抛 DEL 错前，必须保证未发的 emit 不会因为部分进度而下次重发。改为 Lua 一次性 `(XADD ; DEL)` 原子（与 Billing-3 同一脚本）。

S1.20 R7 protected-mode parse fail：
- `parse_response` Err 时 lease 走 `release()`（refund full），并 emit `UsageRecord { vendor_exposure_state: Terminated, credits_consumed: Some(0), parse_failed: true }`。
- HTTP 路径**不返回 200**：返回 502 + `GW_UPSTREAM_PROTOCOL_DECODE_ERROR`（与 R24 共用）。

S1.21 R9 release_task_freeze fallback：当 `contract.submit` 失败必须 release：
- 在调 submit 前先把 `(user_id, task_id, frozen_micro)` 写入 PG 表 `task_freeze_pending`。
- submit 失败 → 同 tx 删除 + Redis release。
- 失败的 release 由 sweeper 定期扫 PG 表重试。

S1.22 Pipeline RTT 改 Lua（替换 Billing-2 原案）：每条 LedgerItem 独立 Lua `(XADD ; HSET state settled ; DEL inflight)`，成功路径单条原子；批量 pipeline 用于性能，但每条 server-side 已原子。

S1.23 Reconciler state-aware：状态机如 Rust-5 原案，叠加 Go 侧 `billing:settled:{rid}:{attempt}` SET 兜底。

S1.24 replay file 每 envelope 去重：
- 启动时遍历 `*.replaying` 与未处理 `*.jsonl`。
- 对每 envelope 算 sha256，`SETNX redis:replay_dedup:{hash} 1 EX 604800`，命中即跳过，否则 push 到 backlog。

S1.25 catch_unwind 改磁盘：
- flush() 入口 `let snapshot_path = write_batch_to_spill(&taken)?;`。
- 成功路径删除 snapshot。
- panic 路径捕获后 `move_to_recovery(snapshot_path)`，下次启动作为正常 backlog 文件处理。

#### 跨进程契约同 PR 改动（详见 WS-8 协同）
- C2：Go `provenanceRank` 加 `cli_extracted=4`，原表向下移。
- C3：Rust `Terminated` 路径 `credits_consumed = Some(0)` 显式，Go 据此 `cost_micros=0`。
- C4：Go `dedupKey` 与 Lua 脚本加 attempt_index 维度；`settled:{rid}:{attempt|0}` 兼容旧记录。**Go 先于 Rust 部署**。
- C7：迁移期 dual-write（Redis SETNX + PG INSERT 同 tx）至少 25h（>24h TTL），然后停 SETNX。

#### 任务清单

| T# | 内容 | 文件 | 估时 |
|---|---|---|---|
| T1 | `idempotency_keys` migration + repo + idem 接口重写 | `migrations/`, `modules/wallet/business/idem.go`, `modules/wallet/business/idem_pg.go` | 1d |
| T2 | wallet.Deduct/Credit/Refund/adminDebit 接受 (tx, op, key) + tx 内 commit | `modules/wallet/business/service.go` | 1d |
| T3 | `payment_outbox` migration + processor + webhook 重写 | `modules/payment/business/{webhook.go,orders.go,outbox.go}`, `binaries/worker-billing/main.go` | 2d |
| T4 | `refund_outbox` + processor + Create 前置守门 | `modules/payment/business/refunds.go` | 1d |
| T5 | AdminAdjust 强制 Idempotency-Key header | `modules/wallet/api/handlers_admin.go` | 0.3d |
| T6 | G1 subscription quota CheckAndApply | `modules/subscription/business/quota.go`, `modules/billing/business/quota.go` | 0.5d |
| T7 | G2 refund Create 双查 + FOR UPDATE | `modules/payment/business/refunds.go` | 0.3d |
| T8 | G3 wallet outbox commit_timestamp + TTL | `modules/wallet/business/{counter_relay.go,outbox.go}`, migration | 0.5d |
| T9 | G5 subscription.Cancel WHERE status='active' + RowsAffected | `modules/subscription/business/subscription.go` | 0.2d |
| T10 | G6 RollIfDue 在 lock 内重读 | `modules/subscription/business/cycle.go` | 0.3d |
| T11 | G7 subscription.Upgrade 单 tx | `modules/subscription/business/subscription.go` | 0.5d |
| T12 | G10 mgmttoken Blacklist.Revoke 返回错误 | `modules/mgmttoken/model/blacklist.go` | 0.1d |
| T13 | Rust `LeaseGuard.settled` 字段 + release 内联 settle_actual(0) | `crates/gw-ledger/src/lease.rs` | 0.5d |
| T14 | logging.rs Err + CLI 路径改 release+emit Terminated | `crates/gw-proxy/src/filters/logging.rs` | 0.5d |
| T15 | R5 TaskFreeze.release 同步重试 + DLQ | `crates/gw-ledger/src/freeze.rs` | 0.5d |
| T16 | R6 + R7 + Lua atomic (XADD;HSET;DEL) + reconciler state-aware + Go billing:settled 兜底 | `crates/gw-ledger/src/{flusher.rs,inflight_reconciler.rs}`, `crates/gw-task/src/scripts.rs`, Go `modules/billing/business/dedup.go` | 2d |
| T17 | R7 + R24 HTTP path 502 GW_UPSTREAM_PROTOCOL_DECODE_ERROR | `crates/gw-proxy/src/filters/response.rs`, `crates/gw-proxy/src/gateway.rs` | 0.5d |
| T18 | R9 task_freeze_pending PG 表 + sweeper | `crates/gw-store/migrations/`, `crates/gw-proxy/src/filters/task.rs`, `crates/gw-ledger/src/freeze_sweeper.rs` | 1d |
| T19 | replay_file 每 envelope SETNX | `crates/gw-ledger/src/backlog.rs` | 0.5d |
| T20 | catch_unwind → spill-to-disk 改造 | `crates/gw-ledger/src/flusher.rs` | 1d |
| T21 | C2 Go provenanceRank 加 cli_extracted | `modules/billing/business/dedup.go` | 0.1d |
| T22 | C4 Go dedupKey 加 attempt_index + Lua 改 + 兼容旧 key | `modules/billing/business/{dedup.go,settler.go,dlq.go}` | 1d |
| T23 | C7 wallet:idem dual-write 迁移脚本 + 监控 | scripts | 0.3d |
| T24 | 故障注入测试套 | `tests/contract`, `tests/e2e`, `crates/gw-ledger/tests/` | 2d |

合计：~18-22 人日（可 3-4 人并行）。

#### DoD
- 故障注入测试通过：
  - DB 闪断中点：1000 并发 Deduct 钱包 → 余额变化等于"成功响应数 × cost"，0 漏扣 0 重扣。
  - Redis 闪断中点：1000 并发 outbox processor → 全部最终 committed，0 双扣。
  - Pingora kill 中点：1000 inflight → reconciler 跑完后 `usage:records` 去重计数 == 真实 settle 数 ± 0。
  - 部署窗 mixed-version：Go 新 / Rust 旧 → 旧记录写新 dedup key（attempt|0），无丢失；Go 旧 / Rust 新 → 拒绝部署（CI 检查）。
  - Refund 在 outbox pending 时 → 返回 `ErrPaymentStillProcessing`，不进入 refund 状态机。
  - 软删用户 24h 内 outbox processor 处理 → 不写入新余额、记录到 audit。
  - 跨协议 parse 失败 → 客户端收 502 + GW_UPSTREAM_PROTOCOL_DECODE_ERROR，不收 200 空 body，不扣 credits。
- 性质测试：
  - 10k Reserve-then-fail 模拟 → 0 leaked credit。
  - LeaseGuard panic 路径 fuzz → 0 漏 refund。
- 对抗审查复跑（sonnet `abc9b119111da75e9` 复审）：所有 17 项 critique 已 closed。
- 跨进程契约：跑 `scripts/sync-rust-contract.sh` 同步基线到 Rust main HEAD；`make e2e` 通过。

#### 依赖
- 阻塞 WS-2、WS-6、WS-8（共用 LeaseGuard / outbox / dedup key 改动）。
- 不阻塞 WS-3 / WS-4 / WS-5 / WS-7 / WS-9 / WS-10。

---

### WS-2 ACK-before-side-effect + Worker / Drainer 生命周期

#### 根因
Worker / drainer / sink 把"已处理"的 ack 信号写出**早于**实际副作用完成。失败时，副作用丢失但 ack 已发，无法重做。

#### 覆盖（11 项）
- Go 原：H3、H4、H5、H13
- Go 新：G8、S2
- Rust 原：Task-Runtime-1、Task-Runtime-2、Tunnel-1..6
- Rust 新：R21、R22、R23

#### 对抗审查打回（sonnet 4，agentId `a107a6254359f127c`）
关键打回项（已采纳）：
1. **W1+W2 `Safely` + errgroup 连坐**：`Safely` 返回非 nil → errgroup 取消 ctx → 所有兄弟 worker 跟死。改为 `runtime.Supervised(name, restart_policy, f)`，内部循环+退避，仅在 ctx 取消时向上返回 ctx.Err()。**禁止 `Safely` 直接当 errgroup func 用**。
2. **W3 token 化 Push**：CH down 时 token 不解析、drainer parked、shutdown 卡住。改为**完全沿用 billing 的 `last_flushed_offset` 模式**：sink 暴露 offset，drainer XACK 不超 offset，无 per-row token。
3. **W4 XAUTOCLAIM 重复插 CH**：consumer A 写 CH 后死，reclaim 后重写。改为 CH 用 `ReplacingMergeTree (request_id, attempt_index)` 去重；或 sink 写入时同 batch 写 sentinel row。
4. **W5 shutdown 不能 close channel**：会引发 send-on-closed panic。改为 deadline 退出而不关 channel；生产侧必须先停。
5. **W6 OnInsertError 不修 `buf[:0]`**：根因是 buf 无条件清。改为出错时 keep buf 直至 cap，cap 触发 drop-oldest 并计数。
6. **R3 JoinSet abort 漏 nested spawn**：`relay_stream` 内部还 spawn 了 read_task，外层 JoinSet 不覆盖。把 read_task 也注册到内层 JoinSet 或用 CancellationToken 双向传播。
7. **R4 in-flight Redis op 被 abort 截断**：未读响应漏读，fred 后续在别的连接读出来引发 protocol noise。把"提交类" Redis op 包 `tokio::spawn` + `shielded` 或 timeout，cancel 不杀掉 in-flight。
8. **R6 SETTLE_LUA + XACK CROSSSLOT**：若引入 Redis Cluster 会 CROSSSLOT。**XACK 留在 Lua 外**，settle_lua 幂等以支持重试。
9. **R10 routes broadcast deadlock**：lock + .await 死锁。改为 drain 到 Vec → drop lock → 再 await。
10. **R13 reload_nodes 非原子**：DashMap 操作非原子。改为 `arc_swap::ArcSwap<HashMap>` 真正原子换。

#### 修正后的架构方案

**Go 侧** — Sink/Drainer 模式统一为 billing pattern：

S2.1 `shared/runtime/supervisor.go` 重写：
```go
// Supervised wraps f with panic-recover + restart-with-backoff.
// On ctx.Done it returns ctx.Err() to the errgroup; on panic it logs + sleeps + retries.
func Supervised(name string, policy RestartPolicy, f func(context.Context) error) func(context.Context) error
```
弃用裸 `RunWorkers` 直接 `g.Go(w.Run)`。

S2.2 `OneShot(name, f)`：显式声明无重启，CI lint 强制只能在 shared/runtime 内用。

S2.3 `shared/platform/store/sink.go` 改造：
- 新字段 `lastFlushedID atomic.Uint64`（单调递增 batch ID）。
- 每 batch flush 成功后 `lastFlushedID.Store(batchID)`。
- 新方法 `LastFlushedID() uint64` 给 drainer 查询。
- buf 不再无条件清：失败时 cap 检查，超 cap drop-oldest + `dropped_total++` 计数。

S2.4 `archive/business/drainer.go`：
- Push 时记录 `redis_id → batch_id` 映射（small ring buffer，size = max in-flight）。
- XACK 时按 `LastFlushedID()` 反查 redis_id，仅 XACK 已落盘部分。

S2.5 archive backport billing 的 XAUTOCLAIM reconciler：
- 每 30s 取 IDLE > 5 分钟的 PEL → XCLAIM → 重新 Push → 与新 batch 一起走 sink。
- CH 端用 `ReplacingMergeTree ORDER BY (request_id, attempt_index)` + 周期 `OPTIMIZE FINAL` 去重。

S2.6 monitoring audit sink 同样改造（H5）。

S2.7 H13 sink shutdown：
- drainer/binary shutdown 顺序：① 关 producer ctx ② 等 producer goroutine 全退（wg.Wait） ③ 让 sink 最后一次 flush ④ 关 pgPool/rdb。
- 不 close sink.in channel。

S2.8 G8 InflightReaper：XADD + DEL 改单条 Lua。

S2.9 S2 binaries/web shutdown 顺序：所有 worker WaitGroup join 后才 defer Close。

S2.10 binaries 用新 `Supervised`，invalidator 等裸 goroutine 全部走 supervisor。

**Rust 侧**：

S2.11 ExecLoop registry 注入（修复 Task-Runtime-1）：
```rust
pub struct ExecLoopConfig {
    pub registry: Arc<RunningTaskRegistry>,   // NEW
    ...
}
```
`execute_claimed` 入口注册（含 cancel_token），出口 RAII guard 移除。

S2.12 heartbeat 与 lease 一致性（修 W5）：
- heartbeat 写 Redis 用 `EXTEND_LEASE_LUA` 含 `ZADD ... GT`（只增不减）。
- exec_claimed 不再写 lease，只读 lease_until_ms 用于 cancel_token 超时设定。

S2.13 SETTLE_LUA 保持原 KEYS 不变，新增 XACK **在 Lua 外**：
```rust
settle_lua(...).await?;
let _: () = client.xack("tasks:queue", "tasks-workers", stream_id).await?;  // 单独原子，失败计数 + Lua 已幂等
```
+ 新增 `tasks_pending_reconciler`：每 30s `XPENDING tasks:queue tasks-workers IDLE 300000` → 已 settled 的 XACK 兜底。

S2.14 Task-Runtime-1 cancel：`call_upstream` poll 内 `tokio::select!(cancel_token.cancelled(), upstream_body.next())`；cancel 触发后**继续读上游 body 至 EOS or 5s timeout**（避免 R7 反向漏：上游已计费但本方 release 全额）。

S2.15 Tunnel client（修复 Tunnel-1..5 + W7 + W9）：
- `TunnelStream` / `TunnelStreamWriter` / `TunnelStreamIo` 弃 Drop 自动 try_send Close。改为 `close().await` 显式方法，由 bridge `select!` 在响应完成时调用。
- `TunnelStreamReader` 保持 routes 清理（已有）。
- Permit 改由 writer 持有，writer.close 之后 permit drop（修 R1 permit-leak）。
- bridge `relay_plain*` 用 `tokio::select!` 协调 request/response 任务；abort 在 close ack 或 5s timeout 之后。
- `ReconnectingTransport.close()` 设 CancellationToken；supervisor 所有 `.await` 包括 `WsTransport::connect` 全部 `select!` 进 token。
- `TunnelStack.transports` 用 `arc_swap::ArcSwap<HashMap<NodeId, Arc<ReconnectingTransport>>>`；reload_nodes 构建新 HashMap → `swap` → 然后异步 cancel + 等 supervisor 退。

S2.16 Tunnel server（修复 Tunnel-1..6 + W11/W12）：
- `handle_ws_conn` 维护 `JoinSet`；`relay_stream` 内部 spawn 的 `read_task` 也注册到内层 JoinSet。
- WS close → 外层 abort_all → 等内层逐个 abort_all。
- 关键 await（CH 写、Redis 提交）包 `tokio::spawn` shielded，不被 abort 撕裂。
- read_task 退出广播 Close：drain routes 到 Vec → drop lock → 再 await sends。

S2.17 R21 InflightLedger HSET+EXPIRE 改单 Lua `HSET + PEXPIRE` 一次性。

S2.18 R22 stream_consumer 加 XAUTOCLAIM reconciler，IDLE 5min 重处理。

S2.19 R23 QuotaStateWriter PG commit → 同 tx 写 `quota_state_outbox`；专门的 outbox processor 负责 Redis DEL + XADD `events:entity`。

S2.20 R16/R17/R18 全部走 `Supervised` 等价物（Rust：自定义 `supervised_spawn(name, f)` 包 panic 捕获 + 退避重启 + CancellationToken）。

#### 任务清单
（略，按上面 S2.x 拆 18 项，每项 0.2-1d）

#### DoD
- Chaos 测试：
  - CH down 30s → 0 数据丢失，恢复后所有 ACK 化为 CH 写入，无 PEL 残留 > 30s。
  - Task worker SIGKILL 中点 → lease 超时后另一 worker reclaim、registry 不空双执行（CrashFallback 上 dup_safe 或 Go dedup 兜底）。
  - Tunnel 5k stream churn / 5min → 0 permit 泄漏、0 FD 泄漏（`/proc/<pid>/fd` 与 `gw_tunnel_active_streams` 都回零）。
  - WS server 1k connection abort → 所有内层 read_task 退出，relay_stream 0 残留。
- Lint：禁止裸 `go func`、禁止 `tokio::spawn(...)` 出现在 `crates/gw-{billing,ledger,proxy,task-runtime,tunnel-*}` 内未经 `supervised_spawn`。

#### 依赖
- 部分依赖 WS-1 的 (rid, attempt_index) dedup 完成。
- 阻塞 WS-3、WS-4 的部分检查。

---

### WS-3 内存有界不变式

#### 根因
buffer / map / cache / channel / accumulator 缺 cap，缺 sweep，缺 backpressure。攻击者或异常上游可以单请求或缓慢累积把节点打爆。

#### 覆盖（17 项）
- Go 原：H6
- Go 新：G14（OAuth in-memory map）、S5（rate limiter）
- Rust 原：Proxy-1、Proxy-2、Proxy-3、Proxy-8、Codec-4、Routing-1、Routing-2、Routing-3、Routing-4、Routing-7
- Rust 新：R14（Redos）、R15（Prom 高基数）、R19（counter_store EXPIRE 吞）

#### 关键设计
- Rust 引入 `BoundedBuf` trait + 配置化 cap，buffer 超 cap 触发显式策略（`Truncate` / `RejectRequest` / `DropOldest`）+ Prometheus 计数。
- Routing 各 DashMap 加入 `routing_sweep`；每个 map 暴露 `gw_routing_<name>_size` 与 `gw_routing_<name>_evicted_total`；硬 cap 10k（可配），超 cap LRU evict。
- Codec state 每 builder `max_accumulated_bytes` + `pending_function_calls` cap 128，超 cap emit `GW_CODEC_BUFFER_OVERFLOW` 终止流。
- Go OAuth bind-token 移到 Redis HSET + 5min TTL；删除 in-memory map。
- Go rate limiter 加硬 cap + LRU + 单独 cleanup goroutine（不持锁全扫）。
- R14 Redos：`check_model_against_permissions` 入口先估算 regex 长度 + 复杂度，超阈值直接拒；TTI 一类的"先跑 N 次再禁"路径取消。
- R15 Prom：删除所有 `request_id` 之类的高基数 label；改用 exemplar 或 histogram。
- R19 counter_store：EXPIRE 失败时同步 retry 3 次 + 失败计数；新增 `sweeper` 周期扫无 TTL 的 `daily_tokens:*`。

#### 任务清单
（19 项，每项 0.3-1d）

#### DoD
- Adversarial 上游 chaos（slow-loris、no-delim、1Gbps、unique-name 流）→ RSS 平稳。
- 10k key rotation × 1h → DashMap 大小平稳。
- Redis `daily_tokens:*` 7 天扫描 → 0 个无 TTL。
- Prom `/metrics` scrape size < 5MB at peak。

---

### WS-4 Panic 兜底与 Supervisor 不变式

#### 根因
spawn 的 goroutine / tokio task 未被监督，panic 杀进程或杀单点 worker。

#### 覆盖（8 项）
- Go：H1、H2、Panic-site 1/2/5（已降级）、S3、S4
- Rust：R16、R17、R18

#### 关键设计
- 已在 WS-2 中定义 Go 的 `Supervised`/`OneShot`、Rust 的 `supervised_spawn`。
- 本 WS 仅做"扫盘所有 spawn 点 → 改造"。
- CI lint：
  - Go：`go func`/`g.Go(...)` 在 `binaries/` 与 `modules/` 内除非 `runtime.Supervised`/`OneShot` 包裹否则失败。
  - Rust：`tokio::spawn` 同上。
- S3 all-in-one errgroup：拆为两个 errgroup（HTTP 一组，worker 一组），用 separate ctx；worker 错误不杀 HTTP，HTTP 关闭后才停 worker。

#### DoD
- 单元测试：每个 supervisor 注入 panic → 进程存活 + metric 增 + worker 重启或显式不重启。
- chaos：随机 handler panic 注入 1h → 进程存活率 100%。

---

### WS-5 鉴权 / 守卫 fail-closed

#### 根因
auth / license / soft-delete / 安全相关错误路径默认放行（fail-open），错误吞掉、配置缺失、状态泄漏。

#### 覆盖（22 项）
- Go 原：H7、H8、H9、H10、D4、S4(P1)
- Go 新：G10、G11、G12、G13、G14（fail-closed 子集）
- Rust 原：Routing-5、Routing-6
- Rust 新：R1、R2、R3、R4、R13、R14（fail-closed 子集）、R20、R25

#### 关键设计

**Go**：

S5.1 License middleware：chi middleware 强制 `license.Status().Licensed`；白名单 `/livez`/`/readyz`/`/api/admin/license/*`；非白名单 + 未授权 → 402 + `X-Reason: license_required`。

S5.2 H8 `docker-compose.prod.yml` 删除 `ALM_E2E_LICENSE_BOOTSTRAP`；`e2e_bootstrap.go` 代码硬阻 `ALM_ENV=production`。CI lint。

S5.3 H9 sqlc 软删审计脚本：扫描所有 `WHERE`-clause 引用带 `deleted_at` 的表 → 强制 `AND deleted_at IS NULL`。`apikey` 修。

S5.4 H10 defaultGroupID 区分 ErrNoRows 与其他 err；调用方对 err != nil 走 fail-closed（拒绝、audit）。

S5.5 D4 cache invalidator：失败不 XACK；max-attempts DLQ 落地。

S5.6 G10 mgmttoken Blacklist.Revoke return err（已在 WS-1 列）。

S5.7 G11 email-verify per-target + per-IP 速率限制：Redis SETNX 每邮箱 5 次/30min。

S5.8 G12 apikey 本地缓存 60s **全部 mutation 路径加 `s.cache.Delete`**（Update / Toggle / Delete / Rotate）。emit + Delete 同步。

S5.9 G13 Logout 路由：refresh revoke err 必须 propagate，Redis 故障返回 503，前端引导重试。

S5.10 G14 OAuth bind-token 改 Redis HSET + 5min TTL；移除 in-memory map（同时解决 WS-3）。

**Rust**：

S5.11 R1 legacy verify_jwt：默认配置 `jwt_validator = None` 路径**完全删除**；启动期若发现 `cfg.global.jwt_validator.is_none()` 直接 `panic!("must configure jwt_validator explicitly")`。

S5.12 R3 JWT kid 强制：JWKS 解析失败 → 直接拒；**static_pem_rs256 fallback 必须配合 `expected_iss`、`expected_aud`、`require_nbf` 全部配置才允许 enable**；配置校验在启动期。

S5.13 R13 JWT 允许 alg 列表：`JwtValidatorConfig.allowed_algs: Vec<Algorithm>`，启动期非空校验；token alg 不在列表 → 拒。

S5.14 R2 HMAC nonce：所有签名请求带 `nonce`（uuid v4）。Redis SETNX `sig_nonce:{nonce} 1 EX 600`；命中即拒（GW_AUTH_REPLAY_DETECTED）。

S5.15 R4 trusted_proxies：新增配置 `[runtime] trusted_proxies = ["10.0.0.0/8", "172.16.0.0/12"]`；`client_ip_from_session` 仅当 `peer_addr ∈ trusted_proxies` 时读 `X-Forwarded-For`，否则用 `peer_addr`。Go 侧 H12 用同一份配置。

S5.16 R14 RedosTracker：检查 budget 在调 `check_model_against_permissions` **之前**用静态长度 / 复杂度估算；超阈值直接拒。

S5.17 R20 LedgerCounterHandler malformed event：`lease_credits` 字段缺失或非 int → `return Handled::Skip`（不 ack 也不 zero credits）；同时 emit `gw_counter_malformed_total`。

S5.18 R25 mgmt 错误码归并：四种 outcomes 在 HTTP 层统一为 `GW_AUTH_FAILED`，详细原因仅写 server-side log。

S5.19 Routing-5 Sticky：candidates 缺 bound key 时**当前请求 fallback 但不写 moka**；连续 N=3 不可用才更新 moka。

S5.20 Routing-6 ReservationGate cold-start：`view == None` 当 gate 关；`view == Some` 但 model 行缺 → fail-closed（reject 直到 events:reservation 收到 bootstrap-complete 标记或 model 行实际写入）。

#### DoD
- e2e：bypass license header → 402。
- e2e：软删用户 → apikey 解析返回 not_found。
- e2e：HMAC 同请求 5min 内重放 → 拒。
- e2e：spoof X-Forwarded-For → 限速以 peer_addr 计数，不被绕过。
- 单元：JWT alg 不在 allowed_algs → 拒；未知 kid 无 fallback → 拒。
- chaos：Redis 闪断中点 → mgmttoken Revoke 返回 503，调用方拿到错误而非 200。

---

### WS-6 HTTP 响应完整性

#### 根因
状态码已提交后无法撤销。代码路径在 status 已发出后才发现错误（probe fail / archive fail / parse fail），客户端收到 200 + 空 body / 200 + 错协议字节 / 200 + 错 quota。

#### 覆盖（9 项）
- Rust 原：Proxy-5、Proxy-6、Proxy-7、Proxy-9、Proxy-10
- Rust 新：R7、R8、R24

#### 关键设计

S6.1 Proxy-7 + R24 + R7：response_filter 引入 preflight buffer，header 提交延后到 probe 通过 / parse 成功。失败 → 返回 Err 让 Pingora 走标准 error 响应。

S6.2 Proxy-6：grace-expiry 路径在 `return Err` 前必调 `emit_response_surfaces(ctx, true)`。

S6.3 **R8 关键**：`reset_response_attempt_state` 清单**完全重写**，必须清：
- `upstream_body_accum`（避免 Frankenstein body）
- `operation_id`（避免错位 Gemini LRO）
- `quota_snapshot`（避免错位 quota bucket）
- `stream_state`
- 其他凡是 attempt 特定的字段
- 加单元测试穷举 retry 边界。

S6.4 Proxy-9 block_in_place → mpsc 通道改造：archive 事件由 handler `mpsc::UnboundedSender::send`（同步）→ 专用 archive task 异步消费。彻底删除 block_in_place + block_on。

S6.5 Proxy-5 + Proxy-10 spawn timeout：所有 `tokio::spawn(async { ... emit / counter / quota ... })` 包 `tokio::time::timeout(5s, ...)` + 全局 `Semaphore::new(N)` 限并发。失败回退到 spill backlog。

#### DoD
- 单元 + e2e：probe fail 路径客户端收 5xx，绝不 200 空 body。
- 单元：response-side retry × 10 → 每次响应仅含本次 attempt 的字节，operation_id / quota_snapshot 对应本次 attempt。
- chaos：archive 阻塞 → 业务 worker 不被 starved（block_in_place 替换有效）。

---

### WS-7 并发与数值安全

#### 根因
context 取消级联、numeric wrap、deprecated middleware、原子 ordering。

#### 覆盖（10 项）
- Go 原：H11、H12
- Go 新：S6
- Rust 原：Billing-7、Billing-9
- Rust 新：R10、R11、R12

#### 关键设计

S7.1 H11 cache.GetOrLoadSF：loader 闭包用 `context.WithoutCancel(ctx)`。

S7.2 H12 / R4 trusted_proxies + 自实现 RealIP middleware（与 WS-5 共用配置）。

S7.3 H12 rate limiter cleanup：标记过期 key → release lock → batch delete；单独 goroutine 周期跑。

S7.4 S6 cache.GetOrLoadSF singleflight 键：namespace 前缀强制 `cache:{module}:{...}`；类型 mismatch 改 `errors.New(...)` 不 panic。

S7.5 Billing-7：`RequestFreeze::new` 用 `i64::try_from(amount).map_err(|_| ...)` 显式错误而不静默 wrap。

S7.6 Billing-9：`scaled_micro` 中间用 `i128`，末尾 clamp。

S7.7 R10 FormatHealth retry_after_secs：clamp 到 [1s, 3600s]。

S7.8 R11 FormatHealth 分支：拆分"失败更新"与"半开探针更新"为独立条件，互不竞争。

S7.9 R12 WeightedRr Phase 1：在 fetch_add 前先 filter availability。

#### DoD
- Race test：HTTP 断开 lead caller → waiter 全 OK。
- DoS test：spoof X-Forwarded-For 5000 unique → 限速以 peer_addr 计数。
- Property：u64 ∈ [i64::MAX, u64::MAX] → 不 wrap。
- 单元：retry_after_secs = u32::MAX → cooldown 1h 而非 136 年。

---

### WS-8 用量 schema 与跨进程契约

#### 根因
跨进程结构无版本无验证；新字段静默被丢；dedup key 设计落后于 attempt_index 概念。

#### 覆盖（15 项）
- Rust 原：Codec-2、Codec-3、Codec-5
- Contract：C1、C2、C3、C4、C5、C6（don't add）、C7（联动 WS-1）、C10、C11

#### 关键设计

S8.1 启动期先跑 `scripts/sync-rust-contract.sh`，把 `docs/rust-contract/parts/*` 头部 commit 基线刷新到 Rust HEAD。所有 PR 在此基线上对比。

S8.2 `UsageSnapshot` 扩展（Rust）：
```rust
pub struct UsageSnapshot {
    ...原 6 字段...
    pub cache_creation_5m_tokens: u32,
    pub cache_creation_1h_tokens: u32,
    pub audio_input_tokens: u32,
    pub audio_output_tokens: u32,
    pub accepted_prediction_tokens: u32,
    pub rejected_prediction_tokens: u32,
    pub web_search_count: u32,
    pub web_fetch_count: u32,
    pub _pv: u32,  // protocol version, NEW，初始 = 2
}
```

S8.3 `rustpact.Usage`（Go）镜像加同样字段；`json.NewDecoder(...).DisallowUnknownFields()`；遇到 `_pv > supported` → DLQ。

S8.4 provenance 向量扩展（Go `dedup.go:27-34`）：增加新 8 个维度，每个一行；同时按 C2 加 `cli_extracted=4`。

S8.5 Codec-3 Gemini ToolCallDelta：每个 tool-call 维护 `args_buffer: String`；`arguments_chunk` 仅 append；`ToolCallEnd` 解析一次完整 JSON 后 emit。下游若需流式增量，emit `partial_json` style chunks。

S8.6 C4 dedup key：已在 WS-1 落地。

S8.7 C5 spec 文档化：`request_id` is Rust-allocated。`request.rs` 启动期日志确认 `x-request-id` 入参被忽略或仅用于 trace。

S8.8 C6 拒加 `dup_safe`：仲裁完全交 provenance ladder。

S8.9 Go 侧 cost 表扩展（`modules/billing/model/queries/model_pricing.sql`）：新增 8 个维度的价格列；缺失默认 0。

#### DoD
- gpt-4o-audio-preview 测：audio_input_tokens 被记账。
- claude-3.5-sonnet 测：cache_creation_5m vs _1h 分别记账。
- Anthropic→Gemini tool-call e2e：args 完整。
- 迁移测试：Go 先升级，Rust 仍旧；旧记录写新 dedup 键，无丢失。

---

### WS-9 性能 / 查询治理

#### 覆盖（10 项）
- Go 原 M6（特别）、SLQ1、SLQ3-8、A2、B1
- Go 新 G9

#### 关键设计
- SLQ1：reconciler 用 HSCAN + Pipeline 批，5k cap，partial-error 容忍。
- G9：同上，且首错降级为计数+log，不 abort。
- SLQ3：apikey 加覆盖索引，EXPLAIN 验证。
- SLQ4：用户搜索加 pg_trgm 索引。
- SLQ5：CH trace stats 显式 7d 时间窗、LIMIT 100。
- SLQ6：DEL 改 UNLINK 异步分块。
- SLQ7：global scope analytics admin-only + 30d cap。
- SLQ8：page_size cap 100；join 重写消除 N+1。
- A2：sessions blacklist 改 PG 持久化 + TTL。
- B1：pgxpool 关闭前等 `Stat().AcquiredConns() == 0`，10s timeout。
- M6：`subscription_cycles.used_micros` 与 `user_subscriptions.cycle_used_micros` 同 tx 增量。

#### DoD
- 10× 当前负载下 P99 < SLA。
- subscription cycle 不变式 10k tx 0 漂移。

---

### WS-10 运维 hardening

#### 覆盖（5 项 + 横切）
- Go 新：S1、S7
- Rust 新：R15（observability 部分）

#### 关键设计
- S1：binaries 启动期 `pgPool.Ping()`、`rdb.Ping()`、`chConn.Ping()`；失败 panic 退出。`/readyz` 实际查 pool。
- S7：httplog 接 `RedactPath` / `RedactUserAgent` helper；audit 列出敏感模式 regex（`sk_*`、`Bearer *`、`Authorization: *`），匹配即 `[redacted]`。
- R15：删除 `request_id` 之类高基数 label；改用 `exemplar` 或 trace。
- 新增 Prom dashboard 覆盖：WS-1..9 每个不变式都有 gauge / counter。
- 新增 alert 规则：`gw_*_panic_recovered > 0` 5min、`idempotency_keys_reserved_age > 5min`、`gw_routing_*_size` 高水位、`gw_tunnel_active_streams` 持续涨。
- Runbook：每个 P0 不变式破坏都有 runbook 条目。

#### DoD
- 坏配置启动 → 拒启 + 清晰错误。
- 日志 sample 1k 行 → 0 个明显 secret pattern。

---

## 5. 全量覆盖矩阵

> 所有 finding ID → WS 映射。任何遗漏即计划失败。

### 5.1 Go 原始审计

| ID | WS | 任务 |
|---|---|---|
| M1 | WS-1 | T1, T2 |
| M2 | WS-1 | T3 |
| M3 | WS-1 | T4 |
| M4 | WS-1 | T5 |
| M5 | §2.5 已证伪 | doc-only |
| M6 | WS-9 | M6 |
| H1 | WS-4 | scheduler-runner |
| H2 | WS-4 | archive drainer/sink |
| H3 | WS-2 | S2.3 |
| H4 | WS-2 | S2.4 |
| H5 | WS-2 | S2.6 |
| H6 | WS-3 | codec cap |
| H7 | WS-5 | S5.1 |
| H8 | WS-5 | S5.2 |
| H9 | WS-5 | S5.3 |
| H10 | WS-5 | S5.4 |
| H11 | WS-7 | S7.1 |
| H12 | WS-5 + WS-7 | S5.15 + S7.2/S7.3 |
| H13 | WS-2 | S2.7 |
| D1=H3 | WS-2 | 同上 |
| D2=H4 | WS-2 | 同上 |
| D3=H5 | WS-2 | 同上 |
| D4 | WS-5 | S5.5 |
| D5 | §2.5 已证伪 | doc-only |
| D6=H6 | WS-3 | 同上 |
| S1=H7 | WS-5 | 同上 |
| S2=H8 | WS-5 | 同上 |
| S3=H9 | WS-5 | 同上 |
| S4 | WS-5 | monitoring DB nil fail-closed |
| S5=H10 | WS-5 | 同上 |
| SLQ1 | WS-9 | SLQ1 |
| SLQ2 | §2.5 已证伪 | doc-only |
| SLQ3 | WS-9 | SLQ3 |
| SLQ4 | WS-9 | SLQ4 |
| SLQ5 | WS-9 | SLQ5 |
| SLQ6 | WS-9 | SLQ6 |
| SLQ7 | WS-9 | SLQ7 |
| SLQ8 | WS-9 | SLQ8 |
| SLQ9 | §2.5 已证伪 | doc-only |
| A1 | §2.5 已证伪 | dead code 删除 |
| A2 | WS-9 | A2 |
| B1 | WS-9 | B1 |
| B2 | §2.5 半证伪 | doc-only |
| Panic-site 1 | WS-4 | ShardFleet |
| Panic-site 2 | WS-4 | drainFleetAcks |
| Panic-site 5 | WS-4 | cache invalidator |

### 5.2 Go 新发现（21）

| ID | WS | 任务 |
|---|---|---|
| G1 | WS-1 | T6 |
| G2 | WS-1 | T7 |
| G3 | WS-1 | T8 |
| G4 | WS-1 | T3（webhook 重写） |
| G5 | WS-1 | T9 |
| G6 | WS-1 | T10 |
| G7 | WS-1 | T11 |
| G8 | WS-2 | S2.8 |
| G9 | WS-9 | G9 |
| G10 | WS-5 / WS-1 | T12 + S5.6 |
| G11 | WS-5 | S5.7 |
| G12 | WS-5 | S5.8 |
| G13 | WS-5 | S5.9 |
| G14 | WS-3 + WS-5 | S5.10 |
| S1 | WS-10 | S1 |
| S2 | WS-2 | S2.9 |
| S3 | WS-4 | S3 |
| S4 | WS-4 | invalidator supervisor |
| S5 | WS-3 + WS-7 | rate limiter cap + cleanup |
| S6 | WS-7 | S7.4 |
| S7 | WS-10 | S7 |

### 5.3 Rust 原始审计

| ID | WS | 任务 |
|---|---|---|
| Billing-1 | WS-1 | T13 + T14 |
| Billing-2 | WS-1 | T16 |
| Billing-3 | WS-1 | T16 |
| Billing-4 | WS-1 | T19 |
| Billing-5 | WS-1 | T13 |
| Billing-6 | WS-1 | T14 |
| Billing-7 | WS-7 | S7.5 |
| Billing-8 | WS-1 | T20 |
| Billing-9 | WS-7 | S7.6 |
| Proxy-1 | WS-3 | upstream cap |
| Proxy-2 | WS-3 | sse_leftover cap |
| Proxy-3 | WS-3 | probe cap |
| Proxy-5 | WS-6 | S6.5 |
| Proxy-6 | WS-6 | S6.2 |
| Proxy-7 | WS-6 | S6.1 |
| Proxy-8 | WS-3 | 同 Proxy-1 |
| Proxy-9 | WS-6 | S6.4 |
| Proxy-10 | WS-6 | S6.5 |
| Tunnel-1 | WS-2 | S2.15 |
| Tunnel-2 | WS-2 | S2.16 |
| Tunnel-3 | WS-2 | S2.15 |
| Tunnel-4 | WS-2 | S2.15 |
| Tunnel-5 | WS-2 | S2.15 |
| Tunnel-6 | WS-2 | S2.16 |
| Task-Runtime-1 | WS-2 | S2.11 + S2.14 |
| Task-Runtime-2 | WS-2 | S2.13 |
| Routing-1 | WS-3 | sweep + cap |
| Routing-2 | WS-3 | sweep + cap |
| Routing-3 | WS-3 | sweep + cap |
| Routing-4 | WS-3 | sweep + cap |
| Routing-5 | WS-5 | S5.19 |
| Routing-6 | WS-5 | S5.20 |
| Routing-7 | WS-3 + WS-10 | observability + cap |
| Codec-2 | WS-8 | S8.2/3/4 |
| Codec-3 | WS-8 | S8.5 |
| Codec-4 | WS-3 | builder caps |
| Codec-5 | WS-8 | S8.2/3/4 |
| FP-1 | §2.5 已证伪 | doc-only |
| FP-2 | §2.5 已证伪 | doc-only |
| FP-3 | §2.5 已证伪 | doc-only |

### 5.4 Rust 新发现（25）

| ID | WS | 任务 |
|---|---|---|
| R1 | WS-5 | S5.11 |
| R2 | WS-5 | S5.14 |
| R3 | WS-5 | S5.12 |
| R4 | WS-5 + WS-7 | S5.15 + S7.2 |
| R5 | WS-1 | T15 |
| R6 | WS-1 | T16 |
| R7 | WS-1 + WS-6 | T17 + S6.1 |
| R8 | WS-6 | S6.3 |
| R9 | WS-1 | T18 |
| R10 | WS-7 | S7.7 |
| R11 | WS-7 | S7.8 |
| R12 | WS-7 | S7.9 |
| R13 | WS-5 | S5.13 |
| R14 | WS-3 + WS-5 | S5.16 |
| R15 | WS-3 + WS-10 | label 清理 + obs |
| R16 | WS-4 | supervisor |
| R17 | WS-4 | supervisor |
| R18 | WS-4 | supervisor |
| R19 | WS-3 | counter_store sweep |
| R20 | WS-5 | S5.17 |
| R21 | WS-2 | S2.17 |
| R22 | WS-2 | S2.18 |
| R23 | WS-2 | S2.19 |
| R24 | WS-6 | S6.1 |
| R25 | WS-5 | S5.18 |

### 5.5 跨进程契约（15）

| ID | WS | 任务 |
|---|---|---|
| C1 | WS-8 | S8.2/3 |
| C2 | WS-8 + WS-1 | T21 |
| C3 | WS-8 + WS-1 | T17 |
| C4 | WS-8 + WS-1 | T22 |
| C5 | WS-8 | S8.7 |
| C6 | WS-8 | S8.8 (decline) |
| C7 | WS-1 | T23 |
| C8 | NONE | confirmed no-op |
| C9 | NONE | doc-only |
| C10 | WS-8 | S8.1 (run sync first) |
| C11 | WS-8 | S8.2 (_pv field) |
| C12 | NONE | confirmed no-op |
| C13 | NONE | confirmed no-op |
| C14 | WS-5 + WS-10 | doc + license events |
| C15 | NONE | confirmed already in sync |

---

## 6. 排序与并行化

### 6.1 强依赖

- **WS-8 S8.1（sync-rust-contract.sh）必须最先跑**，否则后续所有契约改动基线错。
- **WS-1 T22（C4 dedup key 加 attempt_index）必须 Go 先部署**，Rust 后部署。CI 阻止反向。
- **WS-2 supervisor 改造（包括 Go Supervised、Rust supervised_spawn）阻塞 WS-4 大部分迁移**。
- WS-6 R7 / R24 与 WS-1 T17 共用 502 错误码路径，应同 PR。
- WS-3 与 WS-2 在 Routing/Tunnel 重叠，建议同一负责人。

### 6.2 并行 7 轨

| Track | 内容 | 工作流子集 | 负责人画像 |
|---|---|---|---|
| A | Go billing/wallet/payment/subscription | WS-1 (Go T1-T12) + WS-9(M6/SLQ) | senior Go backend |
| B | Rust ledger/billing/freeze | WS-1 (Rust T13-T22) | senior Rust |
| C | Rust proxy/codec/contract | WS-3 (proxy buffer) + WS-6 + WS-8 | senior Rust |
| D | Rust tunnel + routing + task-runtime | WS-2 (Rust S2.11-2.20) + WS-3 routing + WS-5 routing | senior Rust |
| E | Go shared infra | WS-3 (Go) + WS-4 (Go) + WS-7 (Go) + WS-10 (Go) | senior Go infra |
| F | Auth & security | WS-5 全部 | senior security |
| G | Ops & e2e harness | WS-10 + chaos 测试套 | SRE |

### 6.3 里程碑

| 日 | 目标 |
|---|---|
| D0 | 启动；sync-rust-contract.sh 完成；所有 INVESTIGATE 派 sonnet |
| D3 | WS-1 Go 侧 T1-T5 land；WS-2 supervisor 基础设施 land；WS-4 完成 |
| D5 | WS-1 Rust 侧 T13-T20 land；WS-3 全部 land；WS-5 P0 子集 (R1-R4, G10, G12) land |
| D8 | WS-2 全部 land；WS-6 land；WS-7 land |
| D10 | WS-8 land；WS-5 全部 land；WS-9 land |
| D12 | WS-10 land；24h stability soak |
| D14 | 验收门评审；cutover 演练 |
| D15 | Launch |

---

## 7. 验收门（Launch Gate）

所有项必须 PASS 才允许上线：

1. **覆盖矩阵 §5 内每个 finding ID 状态均为 `closed` 或 `refuted`**。任何 `open` 即拒。
2. **每个 WS 的 DoD 全部 PASS**，包括 chaos 注入测试。
3. **`make e2e` + `make e2e-soak`**（50,000 req × 60s × 3 protocol × stream/non-stream × {off, light, heavy} chaos）全绿。
4. **24h stability soak**：RSS 平稳、FD/permit/PEL 平稳、`gw_*_panic_recovered_total` 在所有正常路径为 0。
5. **跨进程契约**：rustpact 序列化兼容矩阵（Go 旧 ↔ Rust 旧 / Go 新 ↔ Rust 旧 / Go 新 ↔ Rust 新）全 PASS；Go 旧 ↔ Rust 新 由 CI 阻止上线。
6. **CI lint** 全部启用：
   - 禁止裸 `go func` / `tokio::spawn`
   - 禁止 sqlc 表带 deleted_at 列但 query 缺 `AND deleted_at IS NULL`
   - 禁止 `ALM_E2E_LICENSE_BOOTSTRAP` 在 prod compose
   - 禁止 money 类型 `as i64` 直接转换
   - 禁止 rustpact 新增字段未配 `_pv` bump
7. **第二轮对抗审查**：执行后再派一组 sonnet 复审全部修复，确保未引入回归。门禁：0 critical critique。
8. **runbook + dashboard** 都已就绪。
9. **回滚计划**：每个 WS 的 PR 都有 revert plan；DB migration 都有 down 脚本（或显式标注不可回滚理由 + 备份策略）。

---

## 8. 持续发现机制（P5 落地）

执行期任何角色（人或 agent）发现疑点 → 立即按下流程：

```
1. 创建 issue 文件：docs/issues/2026-XX-XX-<short-slug>.md
   - 描述：file:line + 字面代码 + 影响链 + mitigation grep
2. 派 sonnet/haiku 独立调查（verify 或 refute）
3. CONFIRMED → 加入对应 WS 的任务列表（本文件 §5）；不另起编号
4. REFUTED → 加入 §2.5 已证伪条目，源文件加注释
5. 期间 PR 阻塞，直至 (3) 或 (4) 完成
```

CI 集成：每次 `make check` 跑一次 `scripts/audit-newfindings.sh`（待编写），扫已编辑文件相对历史的可疑模式，提交报告到 issue 文件。

---

## 9. 已证伪条目（一次性确认）

见 §2.5。这 9 条不再视为缺陷；任何后续 PR 将其重新报为缺陷的均自动 close。

---

## 附录 A：调查/审查过程留档

| 角色 | 子代理 ID | 范围 | 关键产出 |
|---|---|---|---|
| Hunter-Go | `ab6bb6e04ee3458b7` | Go 所有 module + binaries | 21 新缺陷（G1-G14, S1-S7）|
| Hunter-Rust | `ab9a0b56c409b36bd` | Rust 全 crate | 25 新缺陷（R1-R25）|
| Adversarial-WS1 | `abc9b119111da75e9` | WS-1 钱修复方案 | 17 critique（M1-M6, R1-R9, G1-G2）|
| Adversarial-WS2 | `a107a6254359f127c` | WS-2 worker/tunnel 方案 | 13 critique（W1-W7, R1-R13）|
| Contract-integrity | `aab9678de124f1f9c` | rustpact + parts/* | 15 contract item（C1-C15）|

所有 agent 可通过 `SendMessage(to=<agentId>)` 重新询问后续问题。

---

## 附录 B：关键设计修正对照表

| 原案条目 | 错处 | 修正 |
|---|---|---|
| Idem.Release 接口 | pgx 无 AfterRollback hook，defer 与 InTx rollback 不原子 | 删 Release 接口，idem 写入与余额改动在同 InTx |
| Reserve 在 InTx 外 / Redis SETNX | 23505 形态不兼容旧 short-circuit；Redis 难持久化 | 改 PG `INSERT ... ON CONFLICT DO NOTHING RETURNING xmax`；`response_jsonb` 列复现 |
| webhook 失败 5xx | 与平台重试 ABA livelock | 失败仅当 outbox.pending 且老才 5xx，其他 200 |
| 跨模块同 tx Refund | 违反 modules-no-import | 改 refund_outbox 同模式 |
| AdminAdjust server-side nonce | "requestNonce"语义模糊 | 强制 client Idempotency-Key header，空即 400 |
| LeaseGuard::Drop check settled | `settled` 字段不存在 | 加 `settled: AtomicBool`，release 内 settle_actual(0) |
| MULTI/EXEC Pipeline 原子 | Sentinel 失败下不原子 | 改单条 Lua `(XADD;HSET;DEL)` |
| reconciler 仅本地 state-aware | XADD 部分成功仍可能重复 | Go 侧 `billing:settled:{rid}:{attempt}` SET 兜底 |
| attempt_index 同时上线 | 旧 Go 不识别静默丢 | Go 先部署兼容 `attempt|0`，Rust 后部署 |
| replay_file 按文件 dedup | 部分 push 后崩溃多发 | 每 envelope SETNX Redis dedup |
| catch_unwind 内存 clone | 翻倍内存 | spill 到磁盘 |
| DEL 重排 queue | 无 bounded | 删 queue，全依赖 reconciler |
| Safely 直接当 g.Go func | errgroup 连坐 | 改 Supervised(name, policy, f) |
| token-based Push ACK | CH down 卡 shutdown | 沿用 billing last_flushed_offset 模式 |
| XAUTOCLAIM 重插 CH | 无 idempotency | `ReplacingMergeTree (rid, attempt)` |
| sink shutdown 关 channel | send-on-closed panic | deadline 退出不关 channel；上游先停 |
| SETTLE_LUA + XACK | CROSSSLOT 风险 | XACK 留 Lua 外，settle_lua 幂等 |
| Tunnel Drop try_send Close | 失败静默 + 运行时 shutdown 期 panic | Drop 仅 warn+metric，改 close().await 显式 |
| JoinSet 外层 abort | 漏 nested read_task | read_task 注册到内层 JoinSet |
| reload_nodes 非原子 | DashMap 操作非原子 | `arc_swap::ArcSwap<HashMap>` |
| Routing-5 Sticky 直接重绑 | transient blip 永久断 | 当前请求 fallback，N=3 不可用才更新 moka |
| 加 dup_safe flag | 与 provenance 仲裁重复 | 不加，单一仲裁 |

---

## 附录 C：执行交接

本文件交付即视为：
- §1 五原则强制生效。
- §5 全量覆盖矩阵生效。任何遗漏需走 §8 流程纳入。
- §7 验收门作为 launch 的 hard gate。

下一步建议（用户选择）：
1. **A：分轨派 7 个 sonnet 同时启 WS-1..WS-10 子计划（TDD 步骤展开）**——本文件之上每个 WS 产出一份 `docs/superpowers/plans/2026-06-17-WS-N-<name>.md`。
2. **B：先派 sonnet 验证本计划的覆盖矩阵 §5 是否真的全覆盖**，零误漏作为执行前置。
3. **C：直接进入 WS-1 执行**，其他 WS 边走边展开。

推荐顺序：B → A → C。

— 文档结束 —
