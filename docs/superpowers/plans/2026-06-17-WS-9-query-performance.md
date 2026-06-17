# WS-9 — Performance / Query Governance · TDD Sub-Plan

> **状态**:子计划（master plan v2 §4 WS-9 + 原 v1 §4 WS-9 的展开）
> **双仓 mirror**:本文件与 `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-9-query-performance.md` 必须字节一致除标题。
> **起草**:2026-06-17
> **承接 finding**:M6、SLQ1、SLQ3、SLQ4、SLQ5、SLQ6、SLQ7、SLQ8、A2、B1、G9 + REV-FINAL-G9
> **审计假设性披露(P0)**:本文档每个 task 引用的 file:line 与"现状代码"都是写作时 Read 验证过的;但本子计划的所有 v2 候选修复仍是**hypothesis**。落地前执行者必须复跑 §0 §0.3 检查清单。

---

## 0. 阅读须知（对执行该计划的 agent）

### 0.1 假设你不熟悉 Rust 或 Go

本子计划全部 task 用 master v2 §3 的 **7 段强制格式** 写,任何一段缺失即 PR 拒。术语与具体路径写到字面级,不要凭印象推。

### 0.2 v1 与 v2 不是结论,是 hypothesis

- master v1 §4 WS-9 与 master v2 §4 WS-9 REV-FINAL-G9 列出"候选修复"。
- 原始审计 (`docs/issues/2026-06-17-p0-audit-report.md`) 对 SLQ1/SLQ3/SLQ5/B1 都已**降级**(P1 → mitigation 充足);对 SLQ4/SLQ6/SLQ7/SLQ8/A2 已**降级**为 P2。
- 这并不意味着不修,而是意味着**"修复需要带架构理由,而不是按 master v1 文字照搬"**。

### 0.3 进入任何 task 前的强制检查清单

1. Read 该 task §1【是什么】列举的所有 file:line,核对字面代码与本文件§1 描述是否一致。
2. 不一致 → 在该 task §1 加 "**核对修正**"段,标注差异,以源码为准。
3. 在 §6【怎么做】内挑选候选修复(本子计划已为每个 task 列出 A/B 至少两种候选)。选定后写一段 ADR 风格的 30-80 字理由"为什么选 A 而非 B"。
4. 若 task §7【验证】中的 EXPLAIN 输出与现状不符,以 staging EXPLAIN 为准,同步修订该 task。
5. 落地前先跑一次 `make e2e-soak-smoke` 取基线,落地后再跑同一命令,产出 `before/after.json` 一同 PR 附件。

### 0.4 与其他 WS 的强依赖

- **WS-1**:M6 的"同 tx 增量"要与 WS-1 T6(G1 CheckAndApply CheckQuota 锁定 user_subscriptions)在**同一 PR 内**合入,避免重复加锁路径;否则 row-lock 顺序不一致会死锁。
- **WS-9 G9** 依赖 WS-1 outbox 已落地(REV-003j)。本子计划 §3 任务 9 排到 WS-1 outbox PR 之后。
- **WS-14**:本子计划新增的 4 个 migration(idx for SLQ3/SLQ4 + A2 持久化表 + EXPLAIN 留档表)goose 编号必须先经 WS-14 owner 集中分配。提交前不准用占位编号。
- **WS-15**:SLQ3/SLQ4 加索引若上线即影响 plan,需 `flags/query_plan_freeze.yaml` 在分阶段 enforce 之前 warn-only(可选,讨论后决定)。

### 0.5 不在范围

- 任何与"账户/钱"有关的 hot-path 优化属 WS-1。
- 任何与"用量记录"管道的批量优化属 WS-2。
- 任何 ClickHouse schema 设计层重构(MV / projection)需要单独 spec,不进 WS-9。
- 本子计划不包含 Rust 侧 query 治理(Rust 没有 sqlc/PG 慢查询路径在 WS-9 范围内)。

---

## 1. 根因聚类

所有 WS-9 任务属于以下三类之一:

| 类 | 根因一句话 | 涉及 finding |
|---|---|---|
| **C1 N+1 / per-row round-trip** | 设计上每条主行触发 1 次或多次额外查询/网络 RT | SLQ1、G9、SLQ3、SLQ8 |
| **C2 缺谓词 / 缺索引 / 缺时间窗** | 查询无 selective predicate,planner 走 seq scan;或 ClickHouse 未触达分区裁剪 | SLQ4、SLQ5、SLQ7、SLQ8 (page_size) |
| **C3 资源生命周期 / 持久化** | DEL 阻塞、连接池早关、内存状态丢失 | SLQ6、B1、A2、M6 |

每一类下的 task 共享相同 verify 蓝本(EXPLAIN / pg_stat_statements / pg_stat_activity / chaos kill),所以分组实现成本最低。

---

## 2. 全量 task 列表

| # | 任务 ID | 承接 finding | 文件主战场 | 估时 |
|---|---|---|---|---|
| 1 | **W9-T01** | SLQ1 + G9 + REV-FINAL-G9 | `modules/billing/business/reconciler.go` | 1.0d |
| 2 | **W9-T02** | SLQ3 | `modules/apikey/{business/service.go,model/queries/api_keys.sql}` + migration | 0.8d |
| 3 | **W9-T03** | SLQ4 | `modules/user/business/directory.go` listPageFromStore + migration | 0.6d |
| 4 | **W9-T04** | SLQ5 | `modules/monitoring/business/trace.go` providerTraceStatsSQL | 0.5d |
| 5 | **W9-T05** | SLQ6 | `modules/monitoring/business/cache.go` deletePattern/deleteKeys | 0.6d |
| 6 | **W9-T06** | SLQ7 | `modules/analytics/business/service.go` TTLAnalysis | 0.4d |
| 7 | **W9-T07** | SLQ8 | `modules/geminifiles/business/service.go` ListMappings + toResponse | 0.8d |
| 8 | **W9-T08** | A2 | `modules/user/business/sessions_apikeys.go` + sessions migration | 1.0d |
| 9 | **W9-T09** | B1 | `binaries/web/main.go` + `shared/platform/store/pg.go` | 0.6d |
| 10 | **W9-T10** | M6 | `modules/subscription/business/cycle.go` RollIfDue | 0.6d |

合计 **6.9 人日**,单人可在 7-8 工作日内完成;双人可并行(C1 由一人,C2+C3 由另一人)。

---

## 3. Task 详述（7 段强制格式）

---

### W9-T01:billing reconciler HSCAN + Pipeline + partial-error 容忍 + 5k cap

#### 1.【是什么 What it IS】

- 一个 Go 代码改动,文件 `modules/billing/business/reconciler.go`,函数 `countSettling` (L91-121) 与 `countOpAggTentative` (L144-162) 与 `Check` (L64-89)。
- 把 N+1 的 `SCAN + EXISTS + TTL`(每键 2 次 round-trip)改成"批 SCAN → MGET Pipeline → 单次返回"。
- 把 `Check` 的"任一子查询返 err 即整个 abort"(L73/L78/L81 直接 `return err`)改成"sub-check 之间互不影响"。
- 加 5,000 key 硬上限,达上限即 break + metric `gw_billing_reconciler_capped_total{kind}` +1。
- 不引入新文件、不引入新 dependency、不动 `Run` 调度。

#### 2.【不是什么 What it is NOT】

- ❌ 不是把 SCAN 改成 KEYS(KEYS 在 Redis 是 O(N) blocking,Cluster 部分 shard 拒)。
- ❌ 不是把数据搬出 Redis(billing 用 stream + hash + set 的语义在 WS-1/WS-2 已确定,只优化扫描方式)。
- ❌ 不是给 reconciler 加 priority/lease/leader-election(单 leader 由现 `cfg.Interval` 5min 自然串行)。
- ❌ 不引入新 cfg 字段除非 §6【怎么做】明确列出。
- ❌ 不重命名现有 metric callback。

#### 3.【为什么 Why this exists】

- 现状(已 Read 验证):
  - L93 `Scan(... "settling:*", 100)` 每键 cursor RT。
  - L100 `Exists(... settledDoneKey(rid))`、L107 `TTL(... key)` 每键再 2 个 RT。
  - L73/L78/L81 第一个 sub-check 错误即 `return err`,导致后续 metrics 不再 emit → 真故障期 SRE 看到 monitor 一片空白,误判恢复(G9 + REV-FINAL-G9 根因)。
- 引用:原审计 SLQ1(P1 降级 mitigation:稳态 50 key/check)、G9(P1 → WS-9)、REV-FINAL-G9(partial-error 容忍 + Pipeline)。
- 后果:Rust 一边把 settling pending 暴涨到上界(故障期 1000 QPS × 90s = 90k key),Go reconciler 这一边 5min 周期 × 3 sub-check × 90k × 2RT = 90 万 RT,跑不完;同时第一个 sub-check 因 timeout/CLOSED 错全 abort,SRE 看不到 stuck 计数。
- 根本治理:HSCAN(对 hash)/MGET(对 string)的 Pipeline + per sub-check 独立 err。

#### 4.【做什么 DO】

1. 在文件顶部增加 const `reconcilerMaxKeys = 5000`、`reconcilerScanCount = 500`。
2. 新增私有方法 `scanKeysCapped(ctx, pattern) ([]string, bool, error)`:循环 `Scan` cursor,达 5,000 即 `capped=true` 退出。
3. 重写 `countSettling`:
   - 先 `scanKeysCapped(ctx, "settling:*")` 拿全 keys。
   - 用一次 `client.Pipeline()`:对每 key 入 `Exists(settledDoneKey(rid))` + `TTL(key)`,`Exec` 一次。
   - 遍历 cmds 累加 active/stuck,**每条 Pipeline 内 cmd 错误**仅计入 `gw_billing_reconciler_partial_error_total{kind="settling"}`,**不 return**;整张 Pipeline `Exec` 网络错误才 return。
4. 重写 `countOpAggTentative`:
   - 同样 `scanKeysCapped(ctx, "opbilled:tent:*")`。
   - Pipeline 内每 key 一次 `HKeys`。Pipeline 错误同上,partial。
5. 重写 `Check`(L64-89):每个 sub-check 用独立 err 容器;sub-check 之间不传染。最末 `return errors.Join(errs...)` 让 `Run` 的 `Warn` 日志拿到所有原因,但 ticker 不退出。
6. 新增两个 metric:`Partial(kind string)`、`Capped(kind string)`。在 `ReconcilerMetrics` struct 追加两个可选回调,nil-safe。
7. 单元测试:在 `reconciler_test.go` 加 5 个 case(详见 §7)。
8. 在 PR 描述链回 SLQ1、G9、REV-FINAL-G9。

#### 5.【不做什么 DON'T】

- ❌ 不要把 Pipeline cmd 错误 mapping 成 `return err`(那等于退化回 G9)。
- ❌ 不要把 5,000 写死成 magic number 在循环里;必须用 const。
- ❌ 不要在 Pipeline 里把 Exists 和 TTL 合成 Lua;Cluster 跨 key CROSSSLOT 风险(INVESTIGATE-008)。
- ❌ 不要把 `Scan` count 改为 10,000;Redis server 单次拷贝越大 latency spike 越明显,500 是经验值。
- ❌ 不要在 reconciler 内做任何 settling 状态修复(本子计划只看,不改;修复属 WS-1 T16/T22)。
- ❌ 不要把"5,000 cap"放进 cfg 让 operator 调到 100,000 — 高水位时 reconciler 自己变 DoS 源。

#### 6.【怎么做 HOW (具体)】

候选 A(推荐):**Pipeline 批 + per-key cmd 容忍**。
候选 B:**HSCAN over a single hash**(需要 settler 把 `settling:*` 改成单 hash `settling`)— 跨 WS-1/WS-2 协同太大,落地周期长,本子计划不采用。
候选 C:**Lua MGET-style 脚本**(`local r={};for ... local v=redis.call('EXISTS',k);... end`)— Cluster CROSSSLOT 风险,且把 server 端 CPU 拉高,放弃。

**ADR(写入 PR 描述)**:选 A;A 的所有 cmd 仍是单 key 操作,Cluster 安全;Pipeline 把 N×RT 折成 1 RT;partial 容忍直接对 G9 收口。B/C 各有结构性副作用,等 WS-1 T16 完成后再评估。

字面代码骨架:

```go
const (
    reconcilerScanCount = 500
    reconcilerMaxKeys   = 5000
)

func (r *Reconciler) scanKeysCapped(ctx context.Context, pattern string) ([]string, bool, error) {
    var (
        keys   = make([]string, 0, 1024)
        cursor uint64
    )
    for {
        chunk, next, err := r.rdb.Scan(ctx, cursor, pattern, reconcilerScanCount).Result()
        if err != nil {
            return nil, false, fmt.Errorf("scan %s: %w", pattern, err)
        }
        keys = append(keys, chunk...)
        if len(keys) >= reconcilerMaxKeys {
            return keys[:reconcilerMaxKeys], true, nil
        }
        cursor = next
        if cursor == 0 {
            return keys, false, nil
        }
    }
}

func (r *Reconciler) countSettling(ctx context.Context) (int64, int64, error) {
    keys, capped, err := r.scanKeysCapped(ctx, "settling:*")
    if err != nil {
        return 0, 0, err
    }
    if capped {
        r.emitCapped("settling")
    }
    pipe := r.rdb.Pipeline()
    existsCmds := make([]*redis.IntCmd, 0, len(keys))
    ttlCmds := make([]*redis.DurationCmd, 0, len(keys))
    rids := make([]string, 0, len(keys))
    for _, k := range keys {
        rid := strings.TrimPrefix(k, "settling:")
        if rid == "" {
            continue
        }
        existsCmds = append(existsCmds, pipe.Exists(ctx, settledDoneKey(rid)))
        ttlCmds = append(ttlCmds, pipe.TTL(ctx, k))
        rids = append(rids, rid)
    }
    if _, err := pipe.Exec(ctx); err != nil && !errors.Is(err, redis.Nil) {
        // Pipeline-level transport error: bubble up.
        return 0, 0, fmt.Errorf("settling pipeline exec: %w", err)
    }
    var active, stuck int64
    for i := range rids {
        existsVal, eErr := existsCmds[i].Result()
        if eErr != nil {
            r.emitPartial("settling")
            continue
        }
        if existsVal > 0 {
            continue
        }
        ttl, tErr := ttlCmds[i].Result()
        if tErr != nil {
            r.emitPartial("settling")
            continue
        }
        if ttl < 0 || ttl <= r.cfg.StuckTTLThreshold {
            stuck++
            continue
        }
        active++
    }
    return active, stuck, nil
}

func (r *Reconciler) countOpAggTentative(ctx context.Context) (int64, error) {
    keys, capped, err := r.scanKeysCapped(ctx, "opbilled:tent:*")
    if err != nil {
        return 0, err
    }
    if capped {
        r.emitCapped("opagg")
    }
    pipe := r.rdb.Pipeline()
    cmds := make([]*redis.StringSliceCmd, 0, len(keys))
    for _, k := range keys {
        cmds = append(cmds, pipe.HKeys(ctx, k))
    }
    if _, err := pipe.Exec(ctx); err != nil && !errors.Is(err, redis.Nil) {
        return 0, fmt.Errorf("opagg pipeline exec: %w", err)
    }
    var count int64
    for _, c := range cmds {
        keys, kErr := c.Result()
        if kErr != nil {
            r.emitPartial("opagg")
            continue
        }
        for _, k := range keys {
            if strings.HasPrefix(k, "rid:") {
                count++
            }
        }
    }
    return count, nil
}

func (r *Reconciler) Check(ctx context.Context) error {
    if r.rdb == nil {
        r.emitDedupSettling("active", 0)
        r.emitDedupSettling("stuck", 0)
        r.emitOpAggTentative(0)
        r.emitFleetSizeMismatch(0)
        return nil
    }
    var errs []error
    active, stuck, err := r.countSettling(ctx)
    if err != nil {
        errs = append(errs, fmt.Errorf("settling: %w", err))
    } else {
        r.emitDedupSettling("active", active)
        r.emitDedupSettling("stuck", stuck)
    }
    tentative, err := r.countOpAggTentative(ctx)
    if err != nil {
        errs = append(errs, fmt.Errorf("opagg: %w", err))
    } else {
        r.emitOpAggTentative(tentative)
    }
    mismatch, err := r.fleetMismatch(ctx)
    if err != nil {
        errs = append(errs, fmt.Errorf("fleet: %w", err))
    } else {
        r.emitFleetSizeMismatch(mismatch)
    }
    return errors.Join(errs...)
}
```

`ReconcilerMetrics` 字段扩展(struct,不破坏 zero-value):

```go
type ReconcilerMetrics struct {
    DedupSettling     func(state string, count int64)
    OpAggTentative    func(count int64)
    FleetSizeMismatch func(value int64)
    Partial           func(kind string)   // NEW, nil-safe
    Capped            func(kind string)   // NEW, nil-safe
}
```

#### 7.【如何验证 VERIFY】

1. 单元(Go):`go test ./modules/billing/business/ -run TestReconciler -race -count=5`
   - 5 个 case:
     - (a) 10 keys, all settled → active=0, stuck=0
     - (b) 10 keys, 5 settled, 5 ttl<threshold → stuck=5
     - (c) 10 keys, 1 Exists 注入 err → 仅 1 次 Partial,count 正确
     - (d) 5,001 keys → capped=true 触发,Capped 计数 1
     - (e) settling sub-check 整段 PipeExec err → Check 返非 nil,但其他 sub-check 仍 emit metric
   - 期望:`PASS` 全部 5 次 race 检测;无数据竞争。
2. 集成:`tests/integration/billing_reconciler_test.go`(新增)
   - 启 mini-redis;塞入 1,000 settling key + 5,000 opagg key;一次 Check 运行 <500ms wall time;`gw_billing_reconciler_partial_error_total` == 0。
3. EXPLAIN/INFO:无 PG;Redis `INFO commandstats` `cmdstat_pipeline:calls=...,usec_per_call=...` 验证 RT 数量从 ~10,000 减到 ~30(3 pattern × 1 pipe round)。
4. chaos:WS-13 scenario `redis-flap-mid-check.yaml`(本子计划新增):一个 cmd 注入 error → reconciler 不退出,partial+1,其他 metric 仍发布。
5. 输出文件:`logs/W9-T01-verify.txt` 含 PASS 字样 + Redis INFO 摘要 + chaos 报告路径。

---

### W9-T02:apikey 覆盖索引 + 查询重写 + EXPLAIN 留档

#### 1.【是什么】

- 一个 PG migration + sqlc 查询新增 + Go 调用方重写,三处合一个 PR:
  - migration `migrations/000XX_apikey_standalone_owner_idx.sql`(编号由 WS-14 分配)。
  - SQL 新增:`modules/apikey/model/queries/api_keys.sql` 增加 `-- name: ListStandaloneByOwners :many`。
  - Go:`modules/apikey/business/service.go` `BatchResolveByOwnerUserIDs`(L520-569)弃用分页 ListStandalone 全扫,改成单条 `WHERE is_standalone=TRUE AND user_id = ANY($1)`。

#### 2.【不是什么】

- ❌ 不是给 `api_keys` 加软删(此表本来 hard delete,见 v2 REV-FINAL-V14 抽样反证)。
- ❌ 不是改 ListUserApiKeys / ListStandaloneApiKeys(它们仍服务于"全量分页"用例,不属本 task)。
- ❌ 不动 apikey 缓存(那属 WS-5 S5.8 / REV-FINAL-V17)。

#### 3.【为什么】

- 现状(Read 验证 L524-568):为了反查"哪些 user 是 standalone owner",service 用 pageSize=500 翻整张 standalone 列表直到 `len(out) == len(want)`。当 want=1 但 standalone 总量大时仍可能翻多页。
- 原审计 SLQ3 mitigation:命中 want 即 break — 但 worst case 当 want 全在表末尾。
- 后果:用户上限 10万 + standalone 7万规模时,该 batch resolve 单次 30-300ms,被 cache 短路但 cache miss 时仍是问题。
- 根本治理:索引 `(is_standalone, user_id)` 让"按 user 反查"走 index range scan;查询带 `user_id = ANY($1)` 一次返回。

#### 4.【做什么】

1. 写 migration:
   ```sql
   -- +goose Up
   CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_api_keys_standalone_user
     ON api_keys (user_id)
     WHERE is_standalone = TRUE;
   -- +goose Down
   DROP INDEX CONCURRENTLY IF EXISTS idx_api_keys_standalone_user;
   ```
2. 在 `api_keys.sql` 末尾新增 query(返回全字段以复用 sqlcgen.APIKey 类型):
   ```sql
   -- name: ListStandaloneByOwners :many
   SELECT id, user_id, key_hash, key_display, name, is_standalone, is_active, is_locked,
       allowed_providers, allowed_api_formats, allowed_models, force_capabilities, rate_limit,
       concurrent_limit, total_requests, total_cost_usd, last_used_at, expires_at,
       auto_delete_on_expiry, created_at, updated_at
   FROM api_keys
   WHERE is_standalone = TRUE
     AND user_id = ANY(sqlc.arg('user_ids')::text[]);
   ```
3. 跑 `sqlc generate`(本仓既有命令)。
4. 在 `Store` 接口加 `ListStandaloneByOwners(ctx, ids []string) ([]model.APIKey, error)`,impl 包装 sqlcgen。
5. 重写 `BatchResolveByOwnerUserIDs`:trim/去重 ids → `s.store.ListStandaloneByOwners` → 一遍组装 map。
6. EXPLAIN 留档:把 staging 的 `EXPLAIN (ANALYZE, BUFFERS) SELECT ... user_id = ANY(...)` 输出贴到 PR 描述。
7. 更新 `docs/business/apikey/model.md` 标注新索引用途。

#### 5.【不做什么】

- ❌ 不要在 production migration 用 `CREATE INDEX`(无 CONCURRENTLY)— 会锁表。
- ❌ 不要把现有 `BatchResolveByOwnerUserIDs` 函数签名改掉(调用方 WS-7 / analytics 多处)。
- ❌ 不要保留旧"分页翻 store"路径作为 fallback;否则两套 index hit 路径常年漂移。
- ❌ 不要把索引加进 `(is_standalone, user_id, created_at DESC)` — created_at 不在谓词路径上;无意义且更大。

#### 6.【怎么做】

候选 A(推荐):**partial index `WHERE is_standalone=TRUE`**。索引最小、planner 必走。
候选 B:**普通 composite index `(is_standalone, user_id)`**。表中 standalone 占比 <30% 时浪费 70% 空间。
候选 C:**BRIN on user_id** — 仅当表按 user_id 物理有序时有效,api_keys 是 random insert,放弃。

**ADR**:选 A;预估 standalone 占比 ≤ 30%,partial 比 B 节省一半 size + planner cost。

字面 Go 重写:

```go
func (s *Service) BatchResolveByOwnerUserIDs(ctx context.Context, userIDs []string) (map[string]contracts.ApiKeyDetail, error) {
    want := make(map[string]struct{}, len(userIDs))
    ids := make([]string, 0, len(userIDs))
    for _, uid := range userIDs {
        uid = strings.TrimSpace(uid)
        if uid == "" {
            continue
        }
        if _, ok := want[uid]; ok {
            continue
        }
        want[uid] = struct{}{}
        ids = append(ids, uid)
    }
    out := make(map[string]contracts.ApiKeyDetail, len(ids))
    if len(ids) == 0 {
        return out, nil
    }
    keys, err := s.store.ListStandaloneByOwners(ctx, ids)
    if err != nil {
        return nil, fmt.Errorf("apikey: list standalone by owners: %w", err)
    }
    for _, k := range keys {
        uid := strings.TrimSpace(k.UserID)
        if _, ok := want[uid]; !ok {
            continue
        }
        if _, done := out[uid]; done {
            continue
        }
        name := ""
        if k.Name != nil {
            name = *k.Name
        }
        status := "active"
        if !k.IsActive {
            status = "disabled"
        }
        out[uid] = contracts.ApiKeyDetail{
            ID: k.ID, UserID: k.UserID, Name: name, Status: status,
            ProviderID: singleProvider(k.AllowedProviders),
        }
    }
    return out, nil
}
```

#### 7.【验证】

1. 单元:`go test ./modules/apikey/business/ -run TestBatchResolveByOwnerUserIDs -race -count=3` — `PASS`。
2. EXPLAIN(staging,先种 50,000 standalone + 50,000 non-standalone):
   ```
   EXPLAIN (ANALYZE, BUFFERS)
   SELECT ... FROM api_keys
   WHERE is_standalone=TRUE AND user_id = ANY(ARRAY['u1','u2',...,'u50']);
   ```
   - 期望首行包含 `Index Scan using idx_api_keys_standalone_user`,Buffers shared hit < 200,Total runtime < 5ms。
   - **若计划仍是 `Seq Scan`**,可能因 ANALYZE 未跑或 ANY 选择率高;`ANALYZE api_keys` 后重跑;若仍 seq 则评估候选 B。
3. 集成:`tests/integration/apikey_batch_resolve_test.go` 1000 standalone × 50 want,P95 < 5ms。
4. down migration 在 staging snapshot 上能还原。
5. 输出:`logs/W9-T02-verify.txt` 含 EXPLAIN 文本 + PASS。

---

### W9-T03:users 搜索改 pg_trgm GIN 索引

#### 1.【是什么】

- migration `migrations/000XX_users_trgm.sql`:`CREATE EXTENSION IF NOT EXISTS pg_trgm` + `CREATE INDEX CONCURRENTLY ... USING gin (lower(username) gin_trgm_ops, lower(email) gin_trgm_ops)`。
- Go 改动:`modules/user/business/directory.go` `listPageFromStore`(L314-397)目前用 `lower(u.username) LIKE '%'||lower($6)||'%'` 三路 OR(L325-330)。该查询走的是 seq scan;trgm GIN 让 leading-wildcard ILIKE 索引化。
- 不动 in-mem `memStore.listPage`(L1741+),也不动 cache 30s 短路。

#### 2.【不是什么】

- ❌ 不是给 users 加全文检索 (`tsvector`) — 用户名/邮箱非自然语言。
- ❌ 不是改 search 谓词的语义(三路 OR 保留)。
- ❌ 不动 user_groups 表(name 的 trgm 与否独立 task,不在范围)。
- ❌ 不动 `is_internal IS NOT TRUE` 与 `deleted_at IS NULL` 谓词(那已是正确的)。

#### 3.【为什么】

- 现状(Read 验证 L327-329):leading-wildcard `'%' || $6 || '%'` 无法走 B-tree 索引。
- 原审计 SLQ4 mitigation:admin-only + 30s Redis cache;但 admin 高并发 dashboard / 新搜索词无 cache hit 时仍跑全表 seq scan。
- 用户上限 10万 + 软删保留 → 表常驻 ~10万行,seq scan 单次 100-300ms。
- pg_trgm GIN 让 `ILIKE '%term%'` 在 length≥3 时走 index;短 term 走 seq scan(planner 自动选)。

#### 4.【做什么】

1. migration(分两次以避免 lock):
   - 第一次:`CREATE EXTENSION IF NOT EXISTS pg_trgm`(由 superuser 跑)。
   - 第二次:`CREATE INDEX CONCURRENTLY idx_users_username_trgm ON users USING gin (lower(username) gin_trgm_ops)`。
   - 第三次:同上 email。
2. 不动 Go 查询字面文本(planner 自动选索引)。
3. EXPLAIN 留档(staging)。
4. 更新 `docs/business/user/model.md` 标注 trgm 索引。

#### 5.【不做什么】

- ❌ 不要把 `WHERE` 改写成 `username % $6`(trgm similarity operator)— 改变语义,SDK 期望子串。
- ❌ 不要把 search 长度限制下沉到 SQL(`length($6) >= 3`)— 业务可能要 2 字符匹配;让 planner 决定走 seq 还是 GIN。
- ❌ 不要在 GIN 索引里加 created_at(GIN 多列开销大,ORDER BY 不会被 GIN 服务)。

#### 6.【怎么做】

候选 A(推荐):**lower(col) gin_trgm_ops 两个独立索引**。简单、planner 已支持。
候选 B:**单一 multi-column GIN `(lower(username), lower(email))`** — 只有"两列都 ILIKE"才能合用,本查询是 OR,无意义。
候选 C:**materialized search column** — 复杂,放弃。

**ADR**:选 A。

migration 字面:

```sql
-- +goose Up
-- +goose StatementBegin
CREATE EXTENSION IF NOT EXISTS pg_trgm;
-- +goose StatementEnd
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_users_username_trgm
    ON users USING gin (lower(username) gin_trgm_ops)
    WHERE deleted_at IS NULL AND NOT is_internal;
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_users_email_trgm
    ON users USING gin (lower(email) gin_trgm_ops)
    WHERE deleted_at IS NULL AND NOT is_internal;

-- +goose Down
DROP INDEX CONCURRENTLY IF EXISTS idx_users_email_trgm;
DROP INDEX CONCURRENTLY IF EXISTS idx_users_username_trgm;
```

#### 7.【验证】

1. EXPLAIN(staging,先种 100,000 users):
   ```
   EXPLAIN (ANALYZE, BUFFERS)
   SELECT id FROM users
   WHERE deleted_at IS NULL AND NOT is_internal
     AND (lower(username) LIKE '%john%' OR lower(email) LIKE '%john%');
   ```
   - 期望:`Bitmap Heap Scan on users` + `BitmapOr` + `Bitmap Index Scan on idx_users_username_trgm`;runtime < 30ms;Buffers shared read < 5%。
2. 2-char term 应回退 seq scan(预期);文档注明。
3. 业务回归:`go test ./modules/user/business/ -run TestDirectoryListSearch -race`。
4. WS-11 前端 `/api/admin/users?search=...` API 行为不变。
5. 输出:`logs/W9-T03-verify.txt`。

---

### W9-T04:CH trace stats 显式时间窗 + LIMIT 100

#### 1.【是什么】

- 一个 Go 改动:`modules/monitoring/business/trace.go` `providerTraceStatsSQL`(L176-178)。
- 现 SQL:`SELECT ... WHERE vendor_id = ? OR positionCaseInsensitive(extra, ?) > 0 ORDER BY ts_ms DESC LIMIT ?`。
- 改为:加 `AND ts_ms >= ?`(`now() - 7d` 由 Go 计算传入)+ 改默认 limit 100、上限 100。

#### 2.【不是什么】

- ❌ 不动表 schema、不动 MV、不动 codec(R8 等 attempt-reset 与本 task 无关)。
- ❌ 不动 ProviderTraceStats 接口签名(L22),仅内部增加时间窗 const。

#### 3.【为什么】

- 现状(Read 验证 L105-161):虽然 CH MergeTree 按月分区,但 `vendor_id = ?` 选择率高时仍只读最新 1-2 分区(原审计 mitigation);但 `OR positionCaseInsensitive(extra, ?) > 0` 这段是**全表 substring 扫**,extra 是 JSON-string 字段,无索引,导致即便 vendor_id 命中也得对每行做 positionCaseInsensitive。
- 原审计 SLQ5:mitigation 是 LIMIT 1000(默认 100),3s timeout。但 timeout 是 fail-safe,不是性能,日常 SRE 看 dashboard 受影响。
- 后果:7 天外的历史扫描白白消耗 CH IO。

#### 4.【做什么】

1. 在文件顶部 const `traceStatsWindow = 7 * 24 * time.Hour`、`traceStatsMaxLimit = 100`。
2. `ProviderTraceStats` 入口:
   - `if limit <= 0 || limit > traceStatsMaxLimit { limit = traceStatsMaxLimit }`(替换现有 100/1000 边界)。
   - `since := time.Now().Add(-traceStatsWindow).UnixMilli()`。
3. `providerTraceStatsSQL` 改为带 4 个参数:
   ```sql
   SELECT ts_ms, request_id, trace_id, api_key_id, model, status, api_format, vendor_id, attempt_index, latency_ms, http_status, error_code, extra
   FROM request_records
   WHERE ts_ms >= ?
     AND (vendor_id = ? OR positionCaseInsensitive(extra, ?) > 0)
   ORDER BY ts_ms DESC
   LIMIT ?
   ```
4. `Query` 调用相应改成 `Query(ctx, sql, since, providerID, providerID, limit)`。
5. 更新对应测试 fixture 与 trace.go 测试(L70 之类的 limit=100 已 OK)。

#### 5.【不做什么】

- ❌ 不要把 `positionCaseInsensitive(extra, ?)` 改成 `extra LIKE`(更慢,无法用 token bloom)。
- ❌ 不要把窗口塞进 CH 表 TTL(那是数据生命周期,跨业务,WS-9 不动)。
- ❌ 不要把窗口暴露成 user-controllable param;admin 调大可能拖死 CH。
- ❌ 不要从 limit max 1000 缩到 100 而忘了调用方 — 检索调用方 `grep -n "ProviderTraceStats" modules/`,改默认 if 链。

#### 6.【怎么做】

候选 A(推荐):**Go 端注入 since 常量,SQL 添 `ts_ms >= ?`**。
候选 B:**window 变 CH 表的 `WHERE _partition_id` 显式** — 跨 CH 版本兼容差,放弃。
候选 C:**materialized projection on (vendor_id, ts_ms)** — schema 改动需 WS-8 / 独立 task,本子计划不做。

**ADR**:选 A;7 天窗 cover 99% 实际 SRE 用例,且 7d 是惯例。

#### 7.【验证】

1. 单元:`go test ./modules/monitoring/business/ -run TestProviderTraceStats -race`。
2. CH EXPLAIN(staging):
   ```
   EXPLAIN indexes=1
   SELECT count() FROM request_records
   WHERE ts_ms >= toUnixTimestamp64Milli(now() - INTERVAL 7 DAY)
     AND (vendor_id='openai' OR positionCaseInsensitive(extra,'openai')>0);
   ```
   - 期望:`Granules: <2% of total`、 `Skip indexes` 显示 minmax_ts_ms 被用。
3. 业务 e2e:`/api/admin/monitoring/providers/openai/trace-stats` P95 < 200ms over 30d 数据。
4. 输出:`logs/W9-T04-verify.txt`。

---

### W9-T05:UNLINK 异步分块替换 DEL

#### 1.【是什么】

- 一个 Go 改动:`modules/monitoring/business/cache.go` `deleteKeys`(L484-496)与上游 `deletePattern`(L476-482)。
- 现:`s.redis.Del(ctx, cleaned...)` 一次性 N-key DEL(blocking)。
- 改为:固定 chunk size 256 + `UNLINK`(Redis 4+ 异步回收),累计 deleted 计数返回。

#### 2.【不是什么】

- ❌ 不动 `scanKeys` cursor 逻辑(已对)。
- ❌ 不动 `cacheCategories` 表。
- ❌ 不引入新 cfg。

#### 3.【为什么】

- 现状(Read 验证 L495):`s.redis.Del(ctx, cleaned...)` 把整 batch 阻塞在 Redis 单线程上;monitoring 操作可能涉及 50,000+ key,主线程卡 1-3s,全部其他 client 排队。
- 原审计 SLQ6 mitigation:admin-only 手工。但 admin 一次"清 model 缓存"操作就会触发(L362 `ClearModelMappingCache` → `deletePattern(model:*)` + `deletePattern(global_model:*)`)。
- 根本治理:`UNLINK` 让 server 把回收 offload 到后台线程;chunk 让 packet 不爆。

#### 4.【做什么】

1. 在文件顶部 const `unlinkChunkSize = 256`。
2. 重写 `deleteKeys`:循环 chunked → `pipe.Unlink(ctx, chunk...)`,Pipeline `Exec`,累加 result。
3. `deletePattern` 不需要改(它只调 `deleteKeys`)。
4. 单元 mock(monitoring 已有 redis mock fixture)验证 chunk 边界。

#### 5.【不做什么】

- ❌ 不要把 chunk size 调高到 10,000 — TCP packet 单次 RESP > 1MB 触 client buffer。
- ❌ 不要在 Redis 4 以下 fallback `DEL`(go-redis `Unlink` 会自动回退,无需我们写)。
- ❌ 不要给 chunk loop 加并发(N chunk 串行已足够;并发 = 同时 N RT,意义不大)。

#### 6.【怎么做】

候选 A(推荐):**`Pipeline + UNLINK chunked`**。
候选 B:**单条 `UNLINK key1 key2 ... keyN`(无 chunk)** — 大 N 仍 packet 大;放弃。
候选 C:**用 SCAN 边扫边 UNLINK** — `loadAffinities` / `ClearProviderCache` 还要拿 keys 做 in-mem 过滤,改架构超范围。

**ADR**:选 A。

字面代码:

```go
const unlinkChunkSize = 256

func (s *Service) deleteKeys(ctx context.Context, keys []string) (int64, error) {
    cleaned := make([]string, 0, len(keys))
    for _, key := range keys {
        key = strings.TrimSpace(key)
        if key != "" {
            cleaned = append(cleaned, key)
        }
    }
    if len(cleaned) == 0 {
        return 0, nil
    }
    var total int64
    for start := 0; start < len(cleaned); start += unlinkChunkSize {
        end := start + unlinkChunkSize
        if end > len(cleaned) {
            end = len(cleaned)
        }
        n, err := s.redis.Unlink(ctx, cleaned[start:end]...).Result()
        if err != nil {
            return total, fmt.Errorf("monitoring: unlink chunk: %w", err)
        }
        total += n
    }
    return total, nil
}
```

#### 7.【验证】

1. 单元:`go test ./modules/monitoring/business/ -run TestDeleteKeysChunked -race` 注入 1,000 keys + chunk=256 → 4 调用次数 = 4。
2. mini-redis 集成:1万 keys 一次 deletePattern,P95 < 80ms(对照 DEL 路径 P95 ~500ms)。
3. 输出:`logs/W9-T05-verify.txt`。

---

### W9-T06:analytics global scope admin-only + 30d cap

#### 1.【是什么】

- 一个 Go 改动:`modules/analytics/business/service.go` `TTLAnalysis`(L322-343)。
- `resolveScope` 现已经把 `Scope.Global=true` 限制给 admin(L54-原审计 mitigation 已证实)。本 task 显式加 **claims.Role != "admin" && scope.Global → 403** 双保险 + `req.Hours` 上限 720(30 天)。

#### 2.【不是什么】

- ❌ 不改 `HitAnalysis`、`IntervalTimeline` 等(它们也走 resolveScope,但 hours 默认更短,且 ttl-analysis 是最重的查询)。
- ❌ 不动 `resolveScope` 本身(避免影响所有 caller)。
- ❌ 不引入新 contracts。

#### 3.【为什么】

- 原审计 SLQ7 mitigation:普通用户被 `resolveScope` 限制只能查自己。但 admin global + hours=8760(1y) 仍允许,跑 CH MV 全量扫,占内存 1-2GB。
- 本任务边界:对**最重的接口**(TTLAnalysis)显式 30d cap;其他接口在 v3 / 后续 task 收口。

#### 4.【做什么】

1. const `ttlAnalysisMaxHours = 720`。
2. 在 `TTLAnalysis` 入口 `if req.Hours <= 0 || req.Hours > ttlAnalysisMaxHours { req.Hours = ttlAnalysisMaxHours }`。
3. 在 `resolveScope` 已限制 global 给 admin 的基础上,加 explicit `if es.Global && claims.Role != "admin" { return ..., ErrForbidden }`(若 resolveScope 已 return forbidden 则跳过)。
4. 更新 `docs/business/analytics/api.md`。

#### 5.【不做什么】

- ❌ 不动 `Service.HitAnalysis` 等其他 hours-driven endpoints — 范围爆炸。
- ❌ 不依赖前端 SDK 自我约束 hours(SLQ7 等价于服务端硬上限)。

#### 6.【怎么做】

候选 A:`ttlAnalysisMaxHours = 720` const(推荐)。
候选 B:cfg field — 不需要,30d 是业务硬上限。
**ADR**:选 A。

#### 7.【验证】

1. 单元:`go test ./modules/analytics/business/ -run TestTTLAnalysisHoursCap -race`。
2. e2e: 用 `req.Hours=10000` 调 → 实际只查 720h。
3. e2e: 非 admin + Global=true → 403。
4. 输出:`logs/W9-T06-verify.txt`。

---

### W9-T07:gemini ListMappings JOIN 改写 + page_size cap 100

#### 1.【是什么】

- 一个 Go + SQL 改动:`modules/geminifiles/business/service.go` `ListMappings`(L111-136)与 `toResponse`(L138-159),以及 `modules/geminifiles/model/queries/gemini_files.sql` `ListMappings`(L13-19)。
- 现:Service 层每行各调 1 次 `apikeys.ResolveDetail` + 1 次 `users.GetByID` = 2N RT。
- 改:① page_size 上限 100;② Sql JOIN api_keys/users 一次拿 key_name/username;③ Service 不再 per-row resolve。

#### 2.【不是什么】

- ❌ 不动 `Upload`、`Delete`、`Stats` 路径。
- ❌ 不动 cache 层(geminifiles 未走 cache)。

#### 3.【为什么】

- 现状(Read 验证 L148-156):`toResponse` 内每条 mapping 触 `s.apikeys.ResolveDetail(KeyID)` + `s.users.GetByID(UserID)`。pageSize=100 → 200 内部 RT。
- 原审计 SLQ8 mitigation:admin-only + page_size 默认 20。缺上限是真问题(`pageSize<=0 → 20`,但 caller 可传 10000)。
- 后果:admin 把 page_size 拉满 → 单页 20,000 内部 RT(本表 + apikey store + user store) → 10-30s。
- 根本治理:JOIN + cap。

#### 4.【做什么】

1. 改 `ListMappings` SQL(已知 `gemini_file_mapping(user_id, key_id)` 与 `users(id)`、`api_keys(id)` 关联):
   ```sql
   -- name: ListMappings :many
   SELECT m.id, m.sha256_hex, m.user_id, m.key_id, m.file_uri, m.file_name, m.display_name, m.mime_type, m.size_bytes, m.created_at, m.expires_at,
          COALESCE(u.username, '') AS username,
          COALESCE(k.name, '')      AS key_name
   FROM gemini_file_mapping m
   LEFT JOIN users    u ON u.id = m.user_id AND u.deleted_at IS NULL
   LEFT JOIN api_keys k ON k.id = m.key_id
   WHERE (@include_expired::bool OR m.expires_at > @now)
     AND (@search::text = '' OR m.file_name ILIKE '%' || @search || '%' OR m.display_name ILIKE '%' || @search || '%')
   ORDER BY m.created_at DESC
   LIMIT @lim OFFSET @off;
   ```
2. 在 `model/dto.go`(或对应 row 类型)增加返回字段 username/key_name(by sqlcgen)。
3. Service:`pageSize<=0 → 20; pageSize>100 → 100`。
4. `toResponse` 删除 `ResolveDetail` / `GetByID` 路径,直接读 row。
5. EXPLAIN 留档:确认 JOIN 走 `users_pkey` / `api_keys_pkey`。

#### 5.【不做什么】

- ❌ 不要给 mapping 表加 username/key_name 冗余列(双写漂移)。
- ❌ 不要把 JOIN 改 INNER — 若 user 已硬删 / apikey 已删,LEFT 让 mapping 仍可见(目前 toResponse 也是 fail-silent)。
- ❌ 不要因为 JOIN 跑慢就回滚到 per-row;先评估 pg_stat_statements。

#### 6.【怎么做】

候选 A(推荐):**SQL LEFT JOIN**。
候选 B:**保留 per-row but batch via `BatchResolveDetail([]ids)`** — 仍 3 次 RT,胜过现状但仍非 1 次,选 A 更彻底。
候选 C:**冗余列双写** — 漂移风险,放弃。

**ADR**:选 A;join keys 都是 PK,O(N log M) 完全可接受。

#### 7.【验证】

1. 单元:`go test ./modules/geminifiles/business/ -run TestListMappings -race`。
2. EXPLAIN(staging,10万 mapping × 1万 user × 5万 apikey):
   - 期望 `Limit → Sort → Hash Right Join`(Service 实际 ORDER BY 在 m.created_at,所以 `Index Scan Backwards on idx_gemini_created_at`)。
   - 总 cost < 100,actual time < 50ms。
3. e2e:`page_size=10000` 实际仅返回 100 行。
4. 输出:`logs/W9-T07-verify.txt`。

---

### W9-T08:sessions blacklist PG-persisted + TTL

#### 1.【是什么】

- migration `migrations/000XX_revoked_sessions.sql`(WS-14 分配编号)新建表 `revoked_sessions(user_id, session_id, revoked_at, expires_at, PRIMARY KEY(user_id, session_id))`。
- Go 改动:`modules/user/business/sessions_apikeys.go`,把 `seeded` map(L57)与 `sessions` map(L165 等)的 `ForceRevokeSession` / `RevokeOtherSessions` / `RevokeAllSessions` 改为同时写 PG `revoked_sessions`,TTL=token max age + 24h。
- 新 sweeper(同文件)每 1h `DELETE FROM revoked_sessions WHERE expires_at < now()` 收尾。

#### 2.【不是什么】

- ❌ 不重写整个 session lifecycle / token 验证(那属 WS-5)。
- ❌ 不动 in-mem store 的 read 路径(它仍服务于 dev/test 二进制 module.go L34 panic-on-prod fallback)。
- ❌ 不引入 Redis blacklist(WS-5 G13 logout 是 Redis 路径;本 task 是 PG 持久;两者不冲突,WS-5 owner 决定是否合并)。

#### 3.【为什么】

- 原审计 A2:`seeded` map 是 process-local;重启即 reset → 已 revoke 的 token version 标志丢失 → 旧 token 又能 pass 认证。
- 后果:进程重启即"复活"已撤销 session,这本身就是安全缺陷,即便 mitigation 是"重启不常见"。
- 根本治理:PG 持久。

#### 4.【做什么】

1. migration:
   ```sql
   -- +goose Up
   CREATE TABLE IF NOT EXISTS revoked_sessions (
     user_id    text NOT NULL,
     session_id text NOT NULL,
     revoked_at bigint NOT NULL,
     expires_at bigint NOT NULL,
     PRIMARY KEY (user_id, session_id)
   );
   CREATE INDEX IF NOT EXISTS idx_revoked_sessions_expires
     ON revoked_sessions (expires_at);
   -- +goose Down
   DROP TABLE IF EXISTS revoked_sessions;
   ```
2. 在 `Directory` 上加 `pool *pgxpool.Pool`(已存在,见 directory.go);新增方法 `persistRevoke(ctx, userID, sessionID, ttl)`、`isRevoked(ctx, userID, sessionID)`。
3. `ForceRevokeSession`(L157-177)落地后立刻 `INSERT ... ON CONFLICT DO NOTHING`;`RevokeAllSessions` 类似(对每条 session 批量 INSERT)。
4. token 验证侧(`shared/contracts` TokenVerifier 接口在 auth 模块实现) — 改:验证后再查一次 `revoked_sessions(user_id, session_id)`,命中则返回 ErrSessionRevoked。**该项跨 WS-5 协调,放本 task 描述里,执行者协同 WS-5 owner**。
5. sweeper:每 1h tick `DELETE WHERE expires_at < extract(epoch from now())*1000`;走 `shared/runtime` Supervised(WS-4 落地 API)。
6. dev/test in-mem 路径保留(`pool==nil` 分支),但 prod `module.go L34` 已 panic,无 fallback 风险。

#### 5.【不做什么】

- ❌ 不要在 token 每次验证都 RTT 走 PG(性能炸);用 60s in-process cache + Redis bus(改架构超本 task)。**当前 task 接受每验证 1 次 PG RT**,把 RT 优化作为 follow-up issue。
- ❌ 不要把 `seeded` map 删除(它服务于不同语义"materialise current device");仅追加 PG 持久层。
- ❌ 不要把 sweeper interval 调到 1min(无意义)。

#### 6.【怎么做】

候选 A(推荐):**PG 表 + 1h sweeper + verify 时 PG lookup**。
候选 B:**Redis `sess_revoke:<uid>:<sid>` TTL** — 跨实例好,但与 G10/G13/G12 Redis 黑名单同源,设计要 WS-5 owner 决定。本 task 选 PG 是因为 "持久" 是核心需求,Redis 重启也丢。
候选 C:**bump user.token_version 一次性所有 session revoke** — 已存在路径(L1731),但只能 revoke ALL,不能 revoke single → ForceRevokeSession 不能用。

**ADR**:选 A;TokenVerifier 改动协调 WS-5 F1 owner 一同合 PR。

#### 7.【验证】

1. 单元:`go test ./modules/user/business/ -run TestForceRevokePersisted -race`。
2. 集成:`tests/integration/session_revoke_persist_test.go` — 重启进程后 revoke 状态保留。
3. chaos:`session-restart.yaml`(WS-13 scenario)kill -9 web 进程,起回后旧 token verify → 拒。
4. down migration 在 staging snapshot 验证。
5. 输出:`logs/W9-T08-verify.txt`。

---

### W9-T09:pgxpool 关闭前等 AcquiredConns==0 + 10s timeout

#### 1.【是什么】

- 一个 Go 改动:`binaries/web/main.go`(L36 `defer pgPool.Close()` 与 L86-89 shutdown 序列)。
- 新增 helper `shared/platform/store/pg.go::DrainAndClose(pool, timeout)`。
- 等价改动 `binaries/all-in-one/main.go`、`binaries/worker-billing/main.go` 等所有 main(grep 后清单完整列出)。

#### 2.【不是什么】

- ❌ 不动 `NewPG` 创建路径。
- ❌ 不动 pgxpool 配置(超时/min/max)。
- ❌ 不动 runtime.RunWorkers — 它已有 ctx 取消(WS-2 supervisor 改造的范围)。

#### 3.【为什么】

- 现状(Read 验证):
  - L36 `defer pgPool.Close()` 在 main 退出时调用。
  - L86 `srv.Shutdown(shutdownCtx)` 等 HTTP 排空。
  - **但 workers / invalidator 是 L74-83 的裸 go func**,shutdown 后他们可能仍持有 PG conn 一段时间;`pgPool.Close()` 在 defer 执行时,workers 仍可能 in-flight Query。
- 原审计 B1:pgx Close 在 in-flight 上返 error 不 panic;但 settler 等 "must-finalize" 路径返 error 后会重试(产生异常 log + metric 噪)。
- 根本治理:在 Close 前等 `Stat().AcquiredConns() == 0` 或 10s timeout 兜底。

#### 4.【做什么】

1. 在 `shared/platform/store/pg.go` 加:
   ```go
   func DrainAndClose(pool *pgxpool.Pool, timeout time.Duration) {
       if pool == nil { return }
       deadline := time.Now().Add(timeout)
       for time.Now().Before(deadline) {
           if pool.Stat().AcquiredConns() == 0 {
               break
           }
           time.Sleep(50 * time.Millisecond)
       }
       pool.Close()
   }
   ```
2. `binaries/web/main.go` 改:
   - 删 L36 `defer pgPool.Close()`。
   - 在 L89 `_ = srv.Shutdown(shutdownCtx)` 后加:
     ```go
     // 等 worker / invalidator 释放 conn,然后关闭 pool
     store.DrainAndClose(pgPool, 10*time.Second)
     ```
   - rdb.Close 类似,但目前 redis 不持有 long conn,保留 defer。
3. 同步 `binaries/all-in-one/main.go`、`binaries/worker-billing/main.go`、`binaries/worker-monitoring/main.go`(grep `defer.*pgPool.Close` 全替换)。
4. 单元:`shared/platform/store/pg_test.go` 新增 case 验证 timeout 路径。
5. WS-2 / WS-4 owner 联动:确保 worker ctx cancel 后 PG Query 优先 return,而不是 retry。

#### 5.【不做什么】

- ❌ 不要把 timeout 调到 30s — shutdown deadline 现在 10s,延后 30s 会让 supervisor / k8s 强杀。
- ❌ 不要 poll 1ms — busy loop。
- ❌ 不要把"drain"逻辑塞进 NewPG/Close — 与 pgx 语义脱节。

#### 6.【怎么做】

候选 A(推荐):**外部 helper poll AcquiredConns**。
候选 B:**给 pool wrap `wgPool` 包内部 wg.Add/Done** — 触每条 Query,改面太大。
候选 C:**改写所有 worker Run 函数,在 cancel 后明确 Wait pool drain** — 等价但要求每 worker 同步改。

**ADR**:选 A;helper 只在 binaries main 调,改面最小。

#### 7.【验证】

1. 单元:`go test ./shared/platform/store/ -run TestDrainAndClose -race`(注入 1 个长 Query)。
2. 集成:`tests/integration/shutdown_test.go` — 启 web,1000 inflight query,SIGTERM → 进程 < 11s 退出 + 0 error log。
3. chaos:`shutdown-mid-query.yaml`(WS-13 scenario)。
4. 输出:`logs/W9-T09-verify.txt`。

---

### W9-T10:subscription_cycles.used_micros 同 tx 增量(M6)

#### 1.【是什么】

- 一个 Go 改动:`modules/subscription/business/cycle.go` `RollIfDue`(L34-114)。
- 现:L84/L99 `subscription_cycles SET used_micros = $2` 用的是 `sub.CycleUsedMicros`(L35 `subs.Get` tx 外读取的本地快照)。在 tx 与 Get 之间若有 `ApplySubscriptionUsage` 并发执行,subscription_cycles 的 used_micros 会被 stale 值覆盖。
- 改:在 tx 内 `UPDATE subscription_cycles SET used_micros = (SELECT cycle_used_micros FROM user_subscriptions WHERE id=$1) ...` 让两列在同一 tx + 同一时刻取数。

#### 2.【不是什么】

- ❌ 不动 `ApplySubscriptionUsage`(`quota_contract.go`)— 它已在自身 tx 内做 `+=`,正确。
- ❌ 不动 `subs.Get`(L35 tx 外读)。
- ❌ 不动 cycle 流转的 quota 判断逻辑(L39/L42)。

#### 3.【为什么】

- 原审计 M6 verifier 给的 PARTIAL:被覆盖的是归档列 `subscription_cycles.used_micros`;**活配额读 `user_subscriptions.cycle_used_micros`**(由 `ApplySubscriptionUsage` 用 `+=` 维护)。
- 后果:历史 cycle 审计漂移 — 不是计费 correctness,但审计/合规期看到 used_micros < 真实消费。
- 根本治理:在 RollIfDue 的 tx 内用一条 SELECT 现读 cycle_used_micros,与 cycle 关闭/开新 cycle 在同一 tx 内提交。

#### 4.【做什么】

1. 改 L76-86 expired 分支与 L88-101 roll 分支:
   - 删除 SQL 中 `used_micros=$2`(`$2`=sub.CycleUsedMicros)。
   - 改为 `used_micros = (SELECT cycle_used_micros FROM user_subscriptions WHERE id = $1)`。
2. 在 tx 顶部加 `SELECT ... FOR UPDATE` 锁定 user_subscriptions 行(防止 tx 内 `ApplySubscriptionUsage` 同时跑产生 deadlock — `ApplySubscriptionUsage` 的 tx 内 UPDATE 会等本 tx 释放,顺序一致即可避免 deadlock)。
3. 协同 WS-1 T6(G1 CheckAndApply 也加 `FOR UPDATE`)在同一 PR 内或先 T6 后 T10,确保 lock 顺序一致。
4. 单元 + 集成:`cycle_test.go` 验证并发 ApplyUsage + RollIfDue 末端归档列 == 真实 used。

#### 5.【不做什么】

- ❌ 不要把 `subs.Get` 拉进 tx 里(那就成了 G6 修复的范围 — WS-1 T10 处理 RollIfDue 锁内重读)。
- ❌ 不要把 cycle_used_micros 改成 trigger 维护(隐式行为,SRE 困惑)。
- ❌ 不要给 subscription_cycles 加 CHECK constraint(归档列,允许任意正值)。

#### 6.【怎么做】

候选 A(推荐):**tx 内一句子查询 `(SELECT cycle_used_micros FROM user_subscriptions WHERE id=$1)` + tx 顶部 FOR UPDATE**。
候选 B:**先在 tx 顶部 `SELECT cycle_used_micros INTO :var FROM user_subscriptions WHERE id=$1 FOR UPDATE`,再用 `:var` 注入 update** — 同样 2 SQL,可读性差。
候选 C:**把 RollIfDue 整段改成 CTE `WITH s AS (SELECT ... FOR UPDATE) UPDATE subscription_cycles ...`** — 高级但 pgx 直写 + sqlc 支持参差;候选 A 更稳。

**ADR**:选 A。

字面 SQL(以 expired 分支为例):

```go
if _, err := tx.Exec(ctx,
    `WITH cur AS (
        SELECT cycle_used_micros
          FROM user_subscriptions
         WHERE id = $1
         FOR UPDATE
     )
     UPDATE subscription_cycles c
        SET used_micros = (SELECT cycle_used_micros FROM cur),
            closed_at   = $2
      WHERE c.subscription_id = $1
        AND c.closed_at = 0`,
    subID, now); err != nil {
    return false, err
}
```

#### 7.【验证】

1. 单元:`go test ./modules/subscription/business/ -run TestRollIfDueAfterApply -race -count=10` — 注入 50 并发 ApplyUsage + 1 RollIfDue,end-state `subscription_cycles.used_micros == sum(applied)`。
2. 集成:`tests/integration/subscription_cycle_audit_test.go`,新建 1 个用户、10 cycle,500 ApplyUsage/RollIfDue 交错;末端归档列等于真实。
3. 死锁监控:`pg_stat_database` `deadlocks` 0 增长(15min soak)。
4. 输出:`logs/W9-T10-verify.txt`。

---

## 4. DoD(整体 WS-9 接受门)

合并到 main 前每项必须 PASS:

1. **覆盖**:M6 / SLQ1 / SLQ3 / SLQ4 / SLQ5 / SLQ6 / SLQ7 / SLQ8 / A2 / B1 / G9 / REV-FINAL-G9 全部 status=closed,任一 open 即拒。
2. **性能阈值**:10× 当前负载 chaos scenario 下 `/api/admin/users`(SLQ4)、`/api/admin/monitoring/providers/.../trace-stats`(SLQ5)、`/api/admin/geminifiles/list`(SLQ8)的 P99 ≤ 当前基线的 1.5×。基线由 `make e2e-soak-smoke` 在合并前 30s 采样确定。
3. **EXPLAIN 留档**:T02/T03/T07 三个 SQL 改写,EXPLAIN(ANALYZE,BUFFERS)输出贴 PR 描述;`Index Scan` 或 `Bitmap Index Scan` 在 plan 中出现。
4. **reconciler 故障容忍**:`tests/e2e/chaos/redis-flap-mid-check.yaml` PASS — Pipeline cmd 一半失败,partial 计数 +X,metric 全发布,Run loop 不退出。
5. **subscription cycle 不变式**:`tests/integration/subscription_cycle_audit_test.go` 10,000 tx,subscription_cycles.used_micros == 真实 sum,允许漂移 ≤ 0 micros。
6. **shutdown drain**:`shutdown-mid-query.yaml` 通过,exit code 0,无"conn closed during query"错误日志条目。
7. **session 持久**:`session-restart.yaml` 通过,kill -9 重启后旧 token 一律拒。
8. **down migration**:T02/T03/T07/T08 的 down 脚本在 staging snapshot 上 byte-equivalent 还原。
9. **对抗审查**:T01/T10 落地后派 sonnet 复审,重点(a) Pipeline cmd 错误不漏算;(b) RollIfDue FOR UPDATE 与 G1 CheckAndApply lock order 一致无 deadlock;(c) revoked_sessions 表 TTL sweeper 与 token expiry 边界。
10. **runbook 入口**:每个新 metric (`gw_billing_reconciler_partial_error_total`、`gw_billing_reconciler_capped_total`)与新告警(reconciler partial > 0、shutdown drain timeout)在 `docs/runbooks/` 有条目。

---

## 5. 依赖与排序

```
D0  (前置)  WS-14 migration 编号分配 5 个;WS-13 chaos harness baseline
D1  T01 + T05 + T06 并行(单人 1d)
D2  T02 + T03 + T04 并行(单人 1d,EXPLAIN 留档)
D3  T07 单人 0.8d
D4  T08 单人 1d(协调 WS-5 F1)
D5  T09 单人 0.6d(协调 WS-2/4 supervisor 接口)
D6  T10 单人 0.6d(协调 WS-1 T6 同 PR 或先后)
D7  对抗审查 + chaos soak
D8  合并
```

- **强依赖**:T10 必须在 WS-1 T6 之后或同 PR(锁顺序);T08 token verify 改动需 WS-5 F1 协同;T09 worker ctx 行为依赖 WS-2 supervisor。
- 其余 task 之间无依赖,可并行。

---

## 6. 风险与缓解

| 风险 | 缓解 |
|---|---|
| T02 partial index ANALYZE 未跑 → seq scan | migration 后 staging 强制 `ANALYZE api_keys`;监控 `pg_stat_user_indexes.idx_scan` 增长 |
| T03 pg_trgm 扩展未装 / 需 superuser | migration 拆三阶段;`CREATE EXTENSION` 在 ops runbook |
| T07 JOIN 与 mapping 大表的 plan flip | 留档 EXPLAIN;CI 添加 plan-regression check 作为 follow-up |
| T08 session verify per-request PG RT 拖慢 | follow-up issue 加 in-process LRU + Redis bus;本 task 接受成本 |
| T10 与 WS-1 T6 lock order 漂移 → deadlock | 同 PR 落地;`pg_stat_database.deadlocks` 监控 + alert |
| T01 Pipeline 在 mini-redis 行为 vs 真 Redis 行为差异 | 集成测试用 docker-compose Redis 7,而非纯 mini-redis |

---

## 7. 复审记录(本子计划落地前)

| 谁 | 时间 | 关注 |
|---|---|---|
| Author | 2026-06-17 | 初稿 |
| (待派 sonnet) | TBD | 复审 T01 Pipeline 错误传播是否漏算 / T10 锁顺序 |
| (待派 sonnet) | TBD | 复审本子计划 §4 DoD 阈值是否可观测 |

---

## 8. INVESTIGATE 项(本子计划新增)

| ID | 待证 |
|---|---|
| INVESTIGATE-W9-001 | T08 token verify 加 PG RT 后 P99 影响是否显著(staging 数据)。若 >10ms 必须并行做 in-process LRU follow-up。 |
| INVESTIGATE-W9-002 | T01 Pipeline 在 Redis Cluster 模式下,跨 slot key 是否被自动拆分(go-redis 行为);若 ASK redirect 数量大,改 server-side per-slot iter。 |
| INVESTIGATE-W9-003 | T07 SQL JOIN 在 sqlcgen 类型层是否需手写 row scan(LEFT JOIN 引入 nullable username/key_name)。 |

每条闭合记录入 `docs/issues/2026-06-17-investigates/INVESTIGATE-W9-<NNN>.md`。

— 子计划结束 —
