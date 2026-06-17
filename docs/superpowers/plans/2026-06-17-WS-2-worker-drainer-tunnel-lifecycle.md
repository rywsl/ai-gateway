# WS-2 TDD Sub-Plan — ACK-before-side-effect + Worker / Drainer / Tunnel 生命周期

> **Mirror (byte-identical except title)**: `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-2-worker-drainer-tunnel-lifecycle.md`
> **起草**：2026-06-17
> **上游依据**：
> - v1 master: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` (§4 WS-2 / §5.1-5.4 / §6.1-§6.2)
> - v2 revisions: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` (§0 重要 1+2, §4 WS-2 REV-FINAL-W1..W13)
> - 原始 Go 审计：`AILinkMax-Go/docs/issues/2026-06-17-p0-audit-report.md`（H3、H4、H5、H13、G8、S2）
> - 原始 Rust 审计：`ailinkmax-gateway/docs/issues/2026-06-17-p0-bug-audit-findings.md`（Task-Runtime-1/2、Tunnel-1..6；R21/R22/R23 由 Rust-Hunter sonnet 追加）

---

## 0. 阅读须知（强制）

**最关键的一条**：本文件里所有声明 — 包括 v1/v2 / 原审计 / sonnet hunter 给出的 file:line — 都是 **HYPOTHESES, NOT GROUND TRUTH**。

执行者在进入任何 TDD step 前必须：

1. 用 Read 打开 cited 的 file:line，逐字符核对字面代码是否与本文档声明一致。
2. **不一致以源码为准**，把"声明 vs 实际"差异写到任务的 SOURCE-CHECK 块里。
3. 每个 REV-FINAL-W* 候选方案至少考虑 1 个备选方案，落地前写 ADR 风格的一句话理由（"why A not B"）。
4. 与源码冲突的候选必须撤回，改用源码确证的方案。
5. INVESTIGATE-N 落在你的工作范围 → 当作开放项标记到本 plan 顶部 "Open Investigates" 列表，**不要假定结论**。

**七段强制格式**（缺一不可，PR 不合）：【是什么】【不是什么】【为什么】【做什么】【不做什么】【怎么做 (literal code)】【源码验证 SOURCE-CHECK】【如何验证 VERIFY】（共 8 段；§0 七段+ SOURCE-CHECK，是 v2 §0 七段强制格式 + WS-2 附加的 SOURCE-CHECK，作 P5/P2 一致性证据留档）。

**禁止含糊词**："see code"、"as needed"、"appropriate"、"passes tests"、"add error handling"、"implement appropriately"、"write tests" —— PR 不合。

---

## 1. 已抽样验证（执行者可信、不需再 grep）

| 主张 | 源 file:line | 验证结论 |
|---|---|---|
| SETTLE_LUA 非幂等：无 status 顶端检查，`INCRBY released_micro` `INCRBY deducted_micro` 重试即双扣 | `crates/gw-task/src/scripts.rs:59-88` | **CONFIRMED**（line 71-75 直接 HSET status='settled' 无 cur 检查；line 82-83 双 INCRBY） |
| `binaries/web/main.go` 3 个裸 `go func`，shutdown 无 wg.Wait | `binaries/web/main.go:67, 74, 79`（无 wg） | **CONFIRMED**（line 88 `_ = srv.Shutdown(...)` 后直接 log 退出） |
| Rust 已有 Supervisor：`gw-runtime/src/supervisor.rs:58` `Supervisor::new` + `spawn`；`gw-store/src/managed_loop.rs:35` `Supervisor::spawn(Arc<ManagedLoop>)` | 上面两文件 | **CONFIRMED — 两个 Supervisor 并存**：W11 必须 audit 现有 API（panic 捕获 / RestartPolicy / CancellationToken / one_shot），不可凭空"新建 crate" |
| H4 archive drainer Push 后立即 XAck，sink 异步 flush | `modules/archive/business/drainer.go:65-69, 105-110` + `shared/platform/store/sink.go:41-48` | **CONFIRMED**（drainer.go:65 sink.Push 立即返回；line 107 XAck；sink.go:43 chan send 即 ok） |
| H3 sink OnInsertError nil + `buf = buf[:0]` 无条件清 | `shared/platform/store/sink.go:55-63` | **CONFIRMED**（line 59 `if err != nil && s.opts.OnInsertError != nil`；line 62 `buf = buf[:0]` 永远执行） |
| H13 sink shutdown drain 用 `default` 分支退出，与非阻塞 Push 之间有微秒漏窗 | `shared/platform/store/sink.go:67-77` | **CONFIRMED** |
| G8 InflightReaper XADD 后 DEL，DEL 失败下一轮再发 | `modules/billing/business/inflight_reaper.go:126-134` | **CONFIRMED**（XAdd line 126；Del line 132） |
| R21 InflightLedger HSET + EXPIRE 非原子 | `crates/gw-ledger/src/inflight.rs:80-81` | **CONFIRMED**（line 80 `pool.hset`、line 81 `pool.expire`：两次 round-trip） |
| R22 stream_consumer XACK 失败留 PEL，无 XAUTOCLAIM 兜底 | `crates/gw-store/src/stream_consumer.rs:287-303` | **CONFIRMED**（line 290 `xack` Err 仅 warn；全 crate `grep xautoclaim` 0 命中） |
| R23 QuotaStateWriter PG commit 后 Redis DEL + XADD 都是 `let _ : Result` | `crates/gw-store/src/quota_state.rs:21-49` | **CONFIRMED**（line 34、line 35-47 两个 best-effort） |
| Task-Runtime-1 ExecLoop 无 `registry` 字段 | `crates/gw-task-runtime/src/exec_loop.rs:97-103` | **CONFIRMED**（struct 字段为 `{contract, http, cfg, limits}`，无 registry） |
| Tunnel-1 TunnelStreamWriter 无 Drop impl；显式 `close()` 在 line 192 | `crates/gw-tunnel-client/src/cnexus/transport.rs:160-203` | **CONFIRMED**（Stream/Reader 各自 Drop 仅清 routes；Writer 无 Drop；line 192 `close()`） |
| Tunnel-2 server `relay_stream` 用 `tokio::spawn` 不存 JoinHandle | `crates/gw-tunnel-server/src/lib.rs:289-299` | **CONFIRMED**（line 291 `tokio::spawn(relay_stream(...))` 返回值丢弃） |
| Tunnel-6 server `read_task` 在 `send_frame` Err 时静默 return，parent 永远 park | `crates/gw-tunnel-server/src/lib.rs:370-403` | **CONFIRMED**（line 386-393 Err 路径 break 不通知 parent rx） |

---

## 2. 任务覆盖矩阵（每行：task ID → finding ID → REV-FINAL 引用）

| Task | finding | REV-FINAL | 仓 | Track |
|---|---|---|---|---|
| W2-T01 | H1 supervisor 框架（Go side）| REV-FINAL-W1 + REV-FINAL-V8 | Go | E |
| W2-T02 | Worker recipe table + OneShot lint | REV-FINAL-W1 / -V9 | Go | E |
| W2-T03 | H3 sink OnInsertError + buf cap policy | REV-FINAL-W2 (前置) | Go | E |
| W2-T04 | H4 archive drainer last-flushed-offset model | REV-FINAL-W2 | Go | E |
| W2-T05 | archive XAUTOCLAIM reconciler + ReplacingMergeTree | REV-FINAL-W3 | Go | E |
| W2-T06 | H5 monitoring audit_sink 同 H3/H4 改造 | REV-FINAL-W2/W3 | Go | E |
| W2-T07 | H13 sink shutdown ordering + drainer wait | REV-FINAL-W4 | Go | E |
| W2-T08 | G8 InflightReaper Lua 合并 XADD+DEL | (无新 REV) | Go | E |
| W2-T09 | S2 binaries/web shutdown + Supervised 替换 | REV-FINAL-W4 + REV-FINAL-V10 | Go | E |
| W2-T10 | binaries/all-in-one shutdown + Supervised | REV-FINAL-W4 | Go | E |
| W2-T11 | Rust supervisor API audit（REV-FINAL-W11 前置） | REV-FINAL-W11 | Rust | D |
| W2-T12 | Task-Runtime-1 ExecLoop 注册 + cancel | (no new REV) | Rust | D |
| W2-T13 | Task-Runtime-2 SETTLE_LUA 幂等 + 外层 XACK + XAUTOCLAIM | REV-FINAL-W6 | Rust | D |
| W2-T14 | EXTEND_LEASE_LUA NX+GT 两步 | REV-FINAL-W5 | Rust | D |
| W2-T15 | exec_loop cancel drain timeout per-codec | REV-FINAL-W7 | Rust | D |
| W2-T16 | Tunnel client: writer Drop scopeguard + close().await | REV-FINAL-W8 | Rust | D |
| W2-T17 | Tunnel client: ReconnectingTransport CancellationToken + arc_swap reload | REV-FINAL-W9 + Tunnel-3/5 | Rust | D |
| W2-T18 | Tunnel client: reader exit broadcast Close (Tunnel-4) | (no new REV) | Rust | D |
| W2-T19 | Tunnel server: JoinSet 外层 + scopeguard | REV-FINAL-W10 + Tunnel-2 | Rust | D |
| W2-T20 | Tunnel server: read_task → parent select! (Tunnel-6) | REV-FINAL-W10 | Rust | D |
| W2-T21 | R21 InflightLedger HSET+PEXPIRE 单 Lua | (no new REV) | Rust | B/D |
| W2-T22 | R22 stream_consumer XAUTOCLAIM reconciler + boot XGROUP destroy | REV-FINAL-W13 | Rust | D |
| W2-T23 | R23 QuotaStateWriter outbox + state machine | REV-FINAL-W12 | Rust | D |
| W2-T24 | Chaos suite for WS-2 (CH down / SIGKILL / Sentinel partition) | (WS-13 联合) | both | H |

**覆盖完整性自查**（execute 前必跑）：
- v1 §5.1 / §5.2 / §5.3 / §5.4 内归属 WS-2 的每个 finding 在上表 ≥1 task 命中。
- v2 §4 WS-2 REV-FINAL-W1..W13 在上表 ≥1 task 命中。
- 若 grep 出本 plan 未覆盖的 WS-2 finding ID，按 v1 §8 持续发现机制立即创建任务。

---

## 3. Open Investigates（合并前必须清零；本 WS 范围内的）

| ID | 待证（v2 §99） |
|---|---|
| INVESTIGATE-008 | Rust 部署是单 shard Redis 还是 Cluster？若 Cluster，SETTLE_LUA 跨 key 即 CROSSSLOT。**影响 W2-T13 设计**（决定是否需 hash-tag 把 KEYS[1..6] 放同 slot，或拆 Lua）。 |
| INVESTIGATE-009 | Pingora 0.8 是否暴露 `RST_STREAM` 接口（**REV-FINAL-W7 / W2-T15 依赖**）。 |
| INVESTIGATE-007 | block_in_place 替换后 Tokio worker 余量审计（与 WS-6 共享，但 task drain semaphore 同 runtime）。 |

每个 INVESTIGATE 闭合记录写到 `docs/issues/2026-06-17-investigates/<ID>.md`，引用本 plan task ID。

---

## 4. TDD 任务（共 24 个，每个 8 段格式）

---

### TASK W2-T01：Go `runtime.Supervised` / `runtime.OneShot` 框架

**【是什么 What it IS】**
- 一组 Go 代码改动，文件 `shared/runtime/supervisor.go`（已存在，需重写公开 API），约 200-280 行最终长度。
- 新增 `Supervised(name, RestartPolicy, func(ctx) error) func(ctx) error` 与 `OneShot(name, func(ctx) error) func(ctx) error` 两个工厂，返回 errgroup-兼容 `func(ctx) error`。
- 新增 `SupervisorRegistry` 全局注册表 + `AllHealthy() bool` 暴露给 `/readyz`（REV-FINAL-V11）。

**【不是什么 What it is NOT】**
- ❌ 不删除现有 `Safely`/`RunWorkers`（保留作过渡）；仅新增。
- ❌ 不改 `shared/runtime/host.go` 内 HTTP 中间件（H12 / WS-7 范围）。
- ❌ 不在本 task 里替换调用点（那是 T02 / T09 / T10 的事）。
- ❌ 不引入新进程级 panic handler；只 wrap goroutine 内 panic。

**【为什么 Why this exists / why bug occurs】**
- 根因（H1/H2/S2/S3/S4 + Panic-site 1/2/5 共通模式）：长驻 goroutine 用裸 `go func`，panic = 杀进程；用 errgroup `g.Go(Safely(...))` = 单 worker 错误连坐杀所有兄弟。
- 引用：v1 §4 WS-2 S2.1-S2.2 + adversarial-WS2 (`a107a6254359f127c`) W1。
- v2 修订：REV-FINAL-W1 要求**具体类型签名** + worker recipe + `unhealthy *atomic.Bool` 发布到 `/readyz`。
- v1 S2.1 写"`Supervised` wraps f with panic-recover + restart-with-backoff" — 太抽象；本 task 落地完整结构。

**【做什么 DO】**
1. 在 `shared/runtime/supervisor.go` 内定义 `RestartPolicy` 结构（见 HOW）。
2. 定义 `Supervised(name string, policy RestartPolicy, body func(context.Context) error) func(context.Context) error`。
3. 定义 `OneShot(name string, body func(context.Context) error) func(context.Context) error`（显式无重启，failure 即 errgroup 错）。
4. 定义 `SupervisorRegistry`（singleton package-level，包内 mutex 保护 map）。
5. 新增 Prom 指标：`alm_supervisor_restart_total{worker,reason}`、`alm_supervisor_unhealthy{worker}`。
6. 编写单元测试覆盖：(a) panic 计入 restart + 退避；(b) MaxConsecutivePanics 触发 `PanicAction` 分支；(c) OneShot 不重启；(d) ctx 取消立刻返回 `ctx.Err()`；(e) `SupervisorRegistry.AllHealthy()` 任一 unhealthy → false。

**【不做什么 DON'T】**
- ❌ 不在 supervisor 里调 `os.Exit` 或 `panic`（防止杀进程）。
- ❌ 不允许 `PanicAction` 默认值是 `FatalToErrgroup`；**默认必须是 `Restart`**（否则与"不连坐"原则冲突）。
- ❌ 不在 `body` 退出 nil err 时重启（nil err = clean exit）；与 panic/Err 区分。
- ❌ 不用 `recover()` 然后 swallow；recover 必须 emit metric + log + 进入退避路径。
- ❌ 不在 `Supervised` 外暴露 RestartPolicy 字段为 `interface{}`（必须强类型）。

**【怎么做 HOW (具体)】**

```go
// shared/runtime/supervisor.go — 关键签名（伪截选；执行者需补全实现）

type PanicAction int

const (
    PanicRestart PanicAction = iota // 默认
    PanicFatalToErrgroup            // 仅 OneShot 等明确允许"杀 errgroup"的 worker 选用
)

type RestartPolicy struct {
    MaxConsecutivePanics int           // <=0 视为 5
    BackoffInitial       time.Duration // <=0 视为 200ms
    BackoffMax           time.Duration // <=0 视为 30s
    JitterFrac           float64       // 0..1，<0 或 >1 视为 0.2
    PanicAction          PanicAction
}

// 全局 registry：包内 var；测试可用 reset helper
type SupervisorRegistry struct {
    mu       sync.RWMutex
    statuses map[string]*atomic.Bool // worker name -> unhealthy
}

var defaultRegistry = &SupervisorRegistry{statuses: make(map[string]*atomic.Bool)}

func Registry() *SupervisorRegistry { return defaultRegistry }

func (r *SupervisorRegistry) AllHealthy() bool {
    r.mu.RLock()
    defer r.mu.RUnlock()
    for _, b := range r.statuses {
        if b.Load() {
            return false
        }
    }
    return true
}

func Supervised(name string, policy RestartPolicy, body func(context.Context) error) func(context.Context) error {
    if policy.MaxConsecutivePanics <= 0 { policy.MaxConsecutivePanics = 5 }
    if policy.BackoffInitial <= 0 { policy.BackoffInitial = 200 * time.Millisecond }
    if policy.BackoffMax <= 0 { policy.BackoffMax = 30 * time.Second }
    if policy.JitterFrac < 0 || policy.JitterFrac > 1 { policy.JitterFrac = 0.2 }

    unhealthy := defaultRegistry.register(name)

    return func(ctx context.Context) error {
        backoff := policy.BackoffInitial
        consecutive := 0
        for {
            if ctx.Err() != nil { return ctx.Err() }

            err := runWithRecover(ctx, name, body) // recover -> errPanic; clean nil -> nil
            switch {
            case errors.Is(err, context.Canceled), errors.Is(err, context.DeadlineExceeded):
                return err
            case err == nil:
                // 干净退出：仅在 ctx 取消下视为退出；否则重启（worker 应该长驻）
                if ctx.Err() != nil { return ctx.Err() }
                consecutive = 0
                backoff = policy.BackoffInitial
                continue
            case isPanic(err):
                consecutive++
                metricRestart(name, "panic")
                if consecutive >= policy.MaxConsecutivePanics {
                    unhealthy.Store(true)
                    metricUnhealthy(name, 1)
                    if policy.PanicAction == PanicFatalToErrgroup {
                        return fmt.Errorf("supervisor %s: panic budget exhausted: %w", name, err)
                    }
                    // 默认 Restart：清零计数继续（但保持 unhealthy=true 让 /readyz 503）
                    consecutive = 0
                }
            default:
                consecutive++
                metricRestart(name, "error")
                if consecutive >= policy.MaxConsecutivePanics {
                    unhealthy.Store(true)
                    metricUnhealthy(name, 1)
                    consecutive = 0
                }
            }
            // jittered backoff，ctx-aware
            d := jitter(backoff, policy.JitterFrac)
            select {
            case <-ctx.Done(): return ctx.Err()
            case <-time.After(d):
            }
            backoff = nextBackoff(backoff, policy)
        }
    }
}

func OneShot(name string, body func(context.Context) error) func(context.Context) error {
    return func(ctx context.Context) error {
        err := runWithRecover(ctx, name, body)
        if err != nil && isPanic(err) {
            metricRestart(name, "panic_oneshot")
            return fmt.Errorf("oneshot %s panicked: %w", name, err)
        }
        return err
    }
}
```

**ADR 候选选择记录（强制写到 commit message）**：
- **候选 A（推荐 / 已选）**：`Supervised` 返回 `func(ctx) error`，由 caller 用 `g.Go(...)` 加入 errgroup。理由：与 errgroup 自然衔接，单 worker `unhealthy` 不杀 errgroup（默认 PanicAction=Restart）。
- **候选 B（拒绝）**：`Supervised` 直接 spawn goroutine。理由：失去 errgroup 协调权，shutdown 等待逻辑分散，更难审计。

**【源码验证 SOURCE-CHECK】**
- Read `shared/runtime/supervisor.go` 全文（当前文件，size 与字段）。
- Read `shared/runtime/module.go` 确认 `Worker` 接口签名。
- Read `binaries/web/main.go:53-83` 与 `binaries/all-in-one/main.go`（找出所有现有 `g.Go` / 裸 `go func` 调用点，列为 T09/T10 的"必须替换"清单）。
- 若 `supervisor.go` 已有 `Safely(...)`，**保留它但加 Deprecated 注释**：`// Deprecated: use Supervised; Safely returns nil err for panic which causes errgroup to swallow.`

**【如何验证 VERIFY】**

1. 单元：`go test ./shared/runtime/ -run TestSupervised -race -count=20 -timeout 60s`
   - 期望：`PASS`，且 `alm_supervisor_restart_total{worker="t",reason="panic"}` ≥ 4。
2. 单元：`go test ./shared/runtime/ -run TestOneShot -race`
   - 期望：panic 路径返回 non-nil err，errgroup 进入 cancel。
3. 单元：`go test ./shared/runtime/ -run TestRegistryAllHealthy -race`
   - 期望：单个 worker unhealthy=true 后 `Registry().AllHealthy()` 返回 false；恢复后 true。
4. lint：`go vet ./shared/runtime/...` 与 `golangci-lint run ./shared/runtime/...` 无 warning。
5. 输出文件：`logs/W2-T01-verify.txt` 含 `PASS` 字样并附 race 报告全文。

**【依赖】** 无前置；阻塞 T02/T09/T10/T11；可与 T03 并行。

---

### TASK W2-T02：Worker recipe 表 + `make check` lint：禁止裸 `go func` 在 `binaries/`、`modules/`

**【是什么 What it IS】**
- 一份新文件 `shared/runtime/recipes.go`：列出**当前全部** worker recipe（name → restart policy → owning binary），约 60-100 行。
- 一份新脚本 `scripts/check-goroutines.sh`（PowerShell 平行 `scripts/check-goroutines.ps1`）：用 `go/analysis` AST walker 扫 `binaries/`、`modules/` 下所有 `go func` / `g.Go(...)`，若未走 `runtime.Supervised`/`runtime.OneShot`/`runtime.Safely` 包裹即 fail。
- 把 lint 挂入 `make check`。

**【不是什么 What it is NOT】**
- ❌ 不扫 `shared/runtime/` 自身（豁免：framework 实现自身需要直接 `go func`）。
- ❌ 不扫 `*_test.go` 文件。
- ❌ 不阻断 `binaries/` 内 `main()` 第一层 `go func`（如 signal handler），需通过白名单具名豁免。

**【为什么 Why this exists / why bug occurs】**
- 根因（v1 §4 WS-2 + WS-4 共享）：lint 缺失 → 新代码继续退化。S2/S3/S4 都是没人挡住裸 spawn 的结果。
- v2 REV-FINAL-V9：用 `go/analysis` AST，**不用** regex（regex 会被 `//go:build` 与 multiline 误判）。

**【做什么 DO】**
1. `shared/runtime/recipes.go`：定义 `WorkerRecipe { Name, Binary, Policy RestartPolicy }`；导出 `KnownRecipes []WorkerRecipe`。
2. 列入：`web.cacheInvalidator`、`web.moduleWorkers`、`all-in-one.cacheInvalidator`、`all-in-one.billingSettler`、`all-in-one.archiveDrainer`、`all-in-one.monitoringAuditDrainer`、`worker-billing.*`、`worker-archive.*` — 通过 T09/T10 时确认完整清单。
3. `scripts/check-goroutines.sh`：调 `go run ./scripts/check-goroutines/main.go`；后者用 `golang.org/x/tools/go/analysis/singlechecker`。
4. Analyzer 逻辑：找 `*ast.GoStmt` → 若包不为 `shared/runtime` 且函数非 `runtime.Supervised`/`runtime.OneShot`/`runtime.Safely` 调用包装 → diagnostic。
5. 编辑 `Makefile`：在 `check:` target 内追加 `bash scripts/check-goroutines.sh`。
6. PowerShell 镜像 `scripts/check-goroutines.ps1` 与 `make.ps1` `check` target 同步。
7. 白名单：在 `scripts/check-goroutines/allowlist.txt` 内列具名豁免（带文件:行注释为什么）。

**【不做什么 DON'T】**
- ❌ 不引入 `nolint:gocheck` 注释豁免机制（白名单文件足够，注释会被滥用）。
- ❌ 不在 lint 失败时打印整堆 stack（只打印 file:line + 期望写法）。
- ❌ 不向 `recipes.go` 加 `// AUTO-GENERATED`（人工维护是契约）。

**【怎么做 HOW (具体)】**

```go
// shared/runtime/recipes.go
package runtime

import "time"

type WorkerRecipe struct {
    Name   string
    Binary string         // "web", "all-in-one", "worker-billing", ...
    Policy RestartPolicy
}

var KnownRecipes = []WorkerRecipe{
    {Name: "cache_invalidator", Binary: "web", Policy: RestartPolicy{
        MaxConsecutivePanics: 5, BackoffInitial: 500 * time.Millisecond,
        BackoffMax: 30 * time.Second, JitterFrac: 0.2, PanicAction: PanicRestart,
    }},
    {Name: "module_workers", Binary: "web", Policy: RestartPolicy{
        MaxConsecutivePanics: 5, BackoffInitial: 500 * time.Millisecond,
        BackoffMax: 30 * time.Second, JitterFrac: 0.2, PanicAction: PanicRestart,
    }},
    // ... 填充至 T09/T10 替换时校准的最终清单
}
```

```sh
# scripts/check-goroutines.sh
#!/usr/bin/env bash
set -euo pipefail
go run ./scripts/check-goroutines/main.go ./binaries/... ./modules/...
```

```go
// scripts/check-goroutines/main.go (简化骨架)
package main

import (
    "go/ast"
    "strings"
    "golang.org/x/tools/go/analysis"
    "golang.org/x/tools/go/analysis/singlechecker"
)

var Analyzer = &analysis.Analyzer{
    Name: "checkgoroutines",
    Doc:  "禁止裸 go func / g.Go 未经 runtime.Supervised/OneShot/Safely 包裹",
    Run:  run,
}

func run(pass *analysis.Pass) (interface{}, error) {
    pkg := pass.Pkg.Path()
    if strings.HasSuffix(pkg, "/shared/runtime") { return nil, nil }
    for _, file := range pass.Files {
        ast.Inspect(file, func(n ast.Node) bool {
            switch v := n.(type) {
            case *ast.GoStmt:
                if !isWrapped(v.Call) {
                    pass.Reportf(v.Pos(),
                        "bare `go func` not allowed; wrap with runtime.Supervised or runtime.OneShot")
                }
            case *ast.CallExpr:
                if isErrgroupGo(v) && len(v.Args) >= 1 && !isWrapped(callOf(v.Args[0])) {
                    pass.Reportf(v.Pos(),
                        "errgroup.Group.Go arg must be runtime.Supervised(...) or runtime.OneShot(...)")
                }
            }
            return true
        })
    }
    return nil, nil
}

// helpers: isWrapped 检查 selector 是否 runtime.Supervised/OneShot/Safely
func main() { singlechecker.Main(Analyzer) }
```

**【源码验证 SOURCE-CHECK】**
- Read `Makefile` 当前 `check:` target。
- Read `scripts/` 现有 `*.sh` / `*.ps1` 命名风格。
- Read `binaries/web/main.go`、`binaries/all-in-one/main.go`、`binaries/worker-billing/main.go`（确认 binary 名集合）。
- 与 v2 REV-FINAL-V9 比对："用 `go/analysis` AST，不用 regex" — 本 task 已遵循。

**【如何验证 VERIFY】**

1. `make check`（macOS / Linux）期望：PASS，无 lint warning。
2. 注入测试：临时在 `modules/wallet/business/_test_lint.go` 加入 `go func() { ... }()`，跑 `bash scripts/check-goroutines.sh` 期望 exit 非零且打印 "bare `go func` not allowed"。
3. 注入测试：`g.Go(func(ctx context.Context) error { ... })`（未包 Supervised）期望同 fail。
4. 白名单回归：临时把上述文件加入 allowlist，期望 PASS。
5. Windows：`.\make.ps1 check` 同样 PASS。
6. 输出文件：`logs/W2-T02-verify.txt`。

**【依赖】** 阻塞于 T01；并行于 T03。

---

### TASK W2-T03：H3 + REV-FINAL-V6 改造 — `Sink` `OnInsertError` 默认 panic-to-restart + buf cap policy

**【是什么 What it IS】**
- 修改 `shared/platform/store/sink.go` Sink 行为：
  1. `SinkOptions` 新增 `OverflowPolicy enum{DropOldest, RejectPush, FailFast}` 与 `MaxBuffered int`（cap 上限）。
  2. flush 失败时**不无条件清 buf**：只在显式策略允许时丢弃；默认 `OverflowPolicy=DropOldest` 时保留 buf 至 `MaxBuffered`，超 cap 才 drop-oldest 并 emit `alm_sink_dropped_total{name}`。
  3. archive 模块构造时**必须**传 `OnInsertError`（panic-on-nil bootstrap 守卫）。

**【不是什么 What it is NOT】**
- ❌ 不改 Sink Push API（仍是 `func(row) error`）。
- ❌ 不引入 retry-on-flush（retry 由调用方自己处理；sink 仅决定 buf 留/清）。
- ❌ 不实现 CH 重试循环（H3 的 mitigation：archive 用 XAUTOCLAIM reconciler，T05 / T22 落地）。

**【为什么 Why this exists / why bug occurs】**
- 根因：sink.go:55-63 失败时 `buf = buf[:0]` 无条件清，导致整批静默丢；archive 模块构造时 `SinkOptions{}` 不传 `OnInsertError`（即 nil）→ 失败完全无报。
- 引用：v1 §4 WS-2 S2.3（buf 不再无条件清；失败时 cap 检查），adversarial-WS2 W6（buf[:0] 必修）。

**【做什么 DO】**
1. 给 `SinkOptions` 加 `OverflowPolicy`、`MaxBuffered`、`Name`（用于 metric 标签）。
2. `flush` 失败时按策略处理：
   - `FailFast`（默认 panic 风险大，仅测试用）：返回 err 让 Run 退出（触发 Supervised 重启）。
   - `DropOldest`：保留 buf；下次 flush 仍包含；超 `MaxBuffered` → 头部丢弃 + counter。
   - `RejectPush`：保留 buf；下次 Push 返回 ErrSinkFull。
3. `OnInsertError` 仍然回调（变成钩子，metric+log；不影响 buf 处理）。
4. 在 `archive/business/sink_builder.go`（或 `module.go`）必传 `OnInsertError = log.Warn`，policy = `DropOldest`，MaxBuffered = 50000。
5. 新增 metric：`alm_sink_dropped_total{name,reason}`、`alm_sink_buffered_rows{name}` gauge、`alm_sink_flush_error_total{name}`。
6. 单元测试：(a) DropOldest 触发；(b) RejectPush 触发；(c) FailFast 让 Run 返回 err。

**【不做什么 DON'T】**
- ❌ 不引入 disk spill（v1 R7/R20 已澄清归属 WS-1，本 WS 不重复）。
- ❌ 不在 sink 内 sleep-retry（让 Supervised 接管重启逻辑）。
- ❌ 不 silently 把 nil OnInsertError 替换为 default no-op（应在构造期 panic 提示）。
- ❌ 不在 Push 路径加锁（仍保持 chan 非阻塞）。

**【怎么做 HOW (具体)】**

```go
// shared/platform/store/sink.go
type OverflowPolicy int

const (
    OverflowDropOldest OverflowPolicy = iota
    OverflowRejectPush
    OverflowFailFast
)

type SinkOptions struct {
    Name           string
    MaxBatch       int
    FlushInterval  time.Duration
    Buffer         int
    MaxBuffered    int            // hard cap; <= 0 → 10 * Buffer
    OverflowPolicy OverflowPolicy
    OnInsertError  func(error, int) // 必填；nil 构造期 panic
}

func NewSink(ins Inserter, opts SinkOptions) *Sink {
    if opts.OnInsertError == nil {
        panic("store.NewSink: OnInsertError required (audit hook)")
    }
    if opts.Name == "" { panic("store.NewSink: Name required for metrics") }
    if opts.MaxBuffered <= 0 { opts.MaxBuffered = opts.Buffer * 10 }
    // ... existing defaults
    return &Sink{...}
}

// flush — 失败时按 policy 处理；成功时返回 nil；当前批被持有于 retained
func (s *Sink) flushOnce(ctx context.Context, retained *[]([]any)) error {
    if len(*retained) == 0 { return nil }
    err := s.ins.Insert(ctx, *retained)
    s.opts.OnInsertError(err, len(*retained))                  // 钩子：永远 emit metric+log
    if err == nil {
        *retained = (*retained)[:0]
        return nil
    }
    metric_flush_error_inc(s.opts.Name)
    switch s.opts.OverflowPolicy {
    case OverflowFailFast:
        return err
    case OverflowDropOldest, OverflowRejectPush:
        // 不清 buf；调用方继续累积。下个 Push 在 Run 内由 cap 决定
        return nil
    }
    return nil
}
```

**ADR**：
- **候选 A（已选）**：strict bootstrap guard（nil OnInsertError → panic）。理由：避免 archive 模块永远静默丢；boot-time error 比 prod 静默更容易发现。
- **候选 B（拒绝）**：默认 OnInsertError = noop log。理由：无法区分"开发者忘传"与"显式静默"。

**【源码验证 SOURCE-CHECK】**
- Read `shared/platform/store/sink.go:1-89`（已确认 line 59 OnInsertError nil 守卫、line 62 buf[:0]）。
- Read `modules/archive/module.go:60-80` 找到 `store.NewSink` 调用点（v1 §5.1 H3 引用 `module.go:67`）。
- Read `modules/monitoring/business/audit_sink.go:50-63` 同样的 `NewSink` 调用。
- 抽样核对结果：若 `module.go:67` 不传 `OnInsertError` 即直接落 strict-guard fail；保证 T06 落地前 archive 不会启动。

**【如何验证 VERIFY】**

1. 单元：`go test ./shared/platform/store/ -run TestSinkOverflow -race -count=10`。
2. 单元：`TestSinkBootstrapGuard` — `NewSink(opts{})` 必须 panic with msg "OnInsertError required"。
3. 集成：在 archive harness 内强制 `Insert` 永远返回 err 30s，期望 `alm_sink_flush_error_total{name="archive"}` ≥ 60，`alm_sink_dropped_total{name="archive",reason="cap"}` ≥ 1（超 MaxBuffered），`alm_sink_buffered_rows{name="archive"}` ≤ MaxBuffered。
4. 输出：`logs/W2-T03-verify.txt`。

**【依赖】** 阻塞于 T01（OnInsertError 钩子写入指标使用 supervisor 注册的全局 prom registry）；阻塞 T04/T05/T06。

---

### TASK W2-T04：H4 + REV-FINAL-W2 — archive drainer last-flushed-offset 模型

**【是什么 What it IS】**
- 改造 `modules/archive/business/drainer.go` 与 `shared/platform/store/sink.go`：
  - Sink 新增 `LastFlushedID() uint64` 与 success-batch 通知。具体：每个 flush 成功后通过 `successCh chan FlushSuccess { BatchID uint64, RedisIDs []string }` 通知 drainer。
  - Drainer 不再在 `processBatch` 内即时 XAck；改为 Push 时记录 `(redisID, batchID)` 映射，XAck 只对已 success 的 batchID 内 RedisIDs 调用。

**【不是什么 What it is NOT】**
- ❌ 不引入 per-row token（v2 REV-FINAL-W2 明确禁用：token RTT 太多）。
- ❌ 不让 Push 阻塞（仍非阻塞）。
- ❌ 不删除 PEL 兜底（T05 会加 XAUTOCLAIM reconciler，本 task 仅做"先写 CH 再 XAck"主路径）。

**【为什么 Why this exists / why bug occurs】**
- 根因：`drainer.go:65-69, 105-110` Push 立即返回成功 → XAck → sink 还没真写 CH → CH down 或 OnInsertError 触发 → 永久丢。
- 引用：v1 §4 WS-2 S2.3-S2.4；adversarial-WS2 W2（token 模式被打回）；v2 REV-FINAL-W2。

**【做什么 DO】**
1. 在 `Sink` 内为每次 flush 分配单调递增 `batchID`（uint64 atomic）。
2. Sink 在 `Run` flush 成功路径推送 `FlushSuccess { BatchID, RedisIDs []string, RowCount int }` 到 buffered `successCh chan FlushSuccess`（cap 大小如 1024）。
3. **Push 接口扩展**：新增 `PushWithID(redisID string, row []any) error`；Sink 在 flush 时按 batch 收集对应 redisIDs。
4. Drainer 改造：
   - `processBatch` 改名 `enqueueBatch`：仅 Push，不 XAck，不 append ackIDs。
   - 起一个 `ackLoop(ctx)` goroutine（用 OneShot 包），消费 `successCh` 并对收到的 RedisIDs 调 XAck，**XAck 失败仅 log + metric，留 PEL 待 T05 XAUTOCLAIM 接管**。
5. shutdown 顺序（T07 收尾）：producer 停 → sink 最后 flush → ackLoop 等 successCh drain → 关 rdb。
6. metric：`alm_archive_ack_lag_rows`、`alm_archive_ack_failed_total`。

**【不做什么 DON'T】**
- ❌ 不让 Push 在 batch 没写 CH 前阻塞调用方；用 chan 容量做背压。
- ❌ 不在 ackLoop 重试 XAck > 3 次（让 PEL 累积，由 reconciler 拉走）。
- ❌ 不在 Drainer 内合并 XAck 跨大 batch（每个 batchID 一次 XAck，O(N_redis_ids)）。
- ❌ 不删除 Push 现有 `ErrSinkFull` 路径（cap 满仍要拒绝）。

**【怎么做 HOW (具体)】**

```go
// shared/platform/store/sink.go
type FlushSuccess struct {
    BatchID  uint64
    RedisIDs []string
    Rows     int
}

type Sink struct {
    in        chan flushRow
    successCh chan FlushSuccess
    ins       Inserter
    opts      SinkOptions
    nextBatch atomic.Uint64
}

type flushRow struct {
    redisID string
    row     []any
}

func (s *Sink) PushWithID(redisID string, row []any) error {
    select {
    case s.in <- flushRow{redisID: redisID, row: row}:
        return nil
    default:
        return ErrSinkFull
    }
}

func (s *Sink) SuccessChan() <-chan FlushSuccess { return s.successCh }

// flush — 成功时 emit FlushSuccess（drop on overflow with metric）
```

```go
// modules/archive/business/drainer.go (rewrite)
func (d *Drainer) Run(ctx context.Context) error {
    if err := d.consumer.EnsureGroup(ctx); err != nil {
        return fmt.Errorf("archive: ensure group: %w", err)
    }
    var wg sync.WaitGroup
    wg.Add(1)
    go func() {
        defer wg.Done()
        d.ackLoop(ctx)
    }()
    if err := d.drain(ctx, "0", 0); err != nil && ctx.Err() == nil {
        d.log.Warn("archive: pending recovery interrupted", "err", err)
    }
    err := d.drain(ctx, ">", 2*time.Second)
    wg.Wait()
    return err
}

func (d *Drainer) ackLoop(ctx context.Context) {
    for {
        select {
        case <-ctx.Done():
            return
        case fs, ok := <-d.sink.SuccessChan():
            if !ok { return }
            if ackErr := d.consumer.Ack(ctx, fs.RedisIDs...); ackErr != nil {
                d.log.Warn("archive: xack failed; pel retain", "batch", fs.BatchID, "err", ackErr)
                metricAckFailed("archive")
            }
        }
    }
}
```

**ADR**：
- **候选 A（已选）**：success-chan 模型。理由：sink 仍然非阻塞，drainer 通过 chan 收到批级 ack 信号；与 billing settler 的 `last_flushed_offset` 模式一致（v1 §4 WS-2 S2.3 要求 billing pattern 沿用）。
- **候选 B（拒绝）**：sink 持久化 batchID 到 disk（每行 fsync）。理由：fsync 成本不可接受；ack 路径仅是兜底，PEL XAUTOCLAIM 是兜底。

**【源码验证 SOURCE-CHECK】**
- Read `modules/archive/business/drainer.go:1-178`（已确认 65-69 Push 同步、107 即时 XAck）。
- Read `shared/platform/store/sink.go:41-88`（已确认 Push 非阻塞、Run drain 用 default 退出）。
- 验证 `rustpact.ReadGroupFrom`、`StreamUsageDetailed`、`GroupArchiveDrainer` 已存在（line 127-129）。
- 若 v1/v2 任何细节与现有 file:line 冲突 → 记录到 commit message。

**【如何验证 VERIFY】**

1. 单元：`go test ./modules/archive/business/ -run TestDrainerLastFlushedOffset -race -count=10`。
2. 集成 chaos：`docker compose -f tests/chaos/ch-down.yml up`，CH down 30s 期间产生 10k events → CH 恢复 → drain 完成 → 用 `XLEN usage:detailed`、`XPENDING usage:detailed archive_drainer` 验证 0 数据丢失，0 PEL > 60s。
3. 性能：吞吐回归测试 ≥ baseline -10%（success-chan 不应明显劣化）。
4. 输出：`logs/W2-T04-verify.txt`。

**【依赖】** 阻塞于 T01/T03；阻塞 T05/T06/T07；并行于 T08-T10。

---

### TASK W2-T05：REV-FINAL-W3 + archive XAUTOCLAIM reconciler + ReplacingMergeTree

**【是什么 What it IS】**
- 新文件 `modules/archive/business/pel_reconciler.go`：周期任务每 30s 跑 `XPENDING usage:detailed archive_drainer IDLE 300000 - + 100` → XCLAIM 给本 consumer → 重新 Push 到 sink。
- ClickHouse 表 schema 改 `ReplacingMergeTree` 按 `(request_id, attempt_index, surface)` 去重，加 `OPTIMIZE FINAL` 周期任务（每日 1 次）。
- ClickHouse driver 必须设 `wait_for_async_insert=1`。

**【不是什么 What it is NOT】**
- ❌ 不在主 hot path 调用（reconciler 独立 worker goroutine）。
- ❌ 不删除 PEL 内 entries 不通过 XACK（仅通过 reclaim + re-Push + 主路径 ack）。
- ❌ 不引入 dedup 在 Go 端（让 CH ReplacingMergeTree 处理重复 row）。

**【为什么 Why this exists / why bug occurs】**
- 根因：v1 W4 "XAUTOCLAIM 重复插 CH"：reclaim 后重写 CH → 同一行写两次。
- v2 REV-FINAL-W3：CH 用 `ReplacingMergeTree ORDER BY (request_id, attempt_index, surface)` 去重；`wait_for_async_insert=1` 让 sink 错误及时报告（否则 async 写 OK 但 server-side 后失败）。

**【做什么 DO】**
1. 写 `pel_reconciler.go`：用 `runtime.Supervised("archive_pel_reconciler", ...)` 包；30s 周期；XPENDING → XCLAIM IDLE 300000 → 解码 → `sink.PushWithID(...)`。
2. CH migration（`migrations-clickhouse/00XX_archive_replacing_mergetree.sql`）：把 `usage_detailed` 表 engine 改 `ReplacingMergeTree` ORDER BY `(request_id, attempt_index, surface)`，TTL 保留 30 天。
3. CH driver 选项：在 `shared/platform/store/clickhouse.go` 注入 `wait_for_async_insert=1`、`async_insert=1`。
4. 周期 `OPTIMIZE FINAL`：cron job 在 `binaries/worker-archive` 或独立 oneshot 容器，每日 03:00 UTC。
5. binaries/all-in-one 与 binaries/worker-archive 启动时把 `archive_pel_reconciler` 加入 errgroup。

**【不做什么 DON'T】**
- ❌ 不在 `pel_reconciler` 内做 CH 写（仅 re-Push 到 sink，让主 sink 处理）。
- ❌ 不让 `OPTIMIZE FINAL` 同步阻塞 reconciler（cron 独立 worker）。
- ❌ 不在生产期间手动跑 `OPTIMIZE FINAL`（仅 cron）。
- ❌ 不开 CH `deduplicate_blocks_in_dependent_materialized_views` 兜底——MV 同步不在本表上下文。

**【怎么做 HOW (具体)】**

```go
// modules/archive/business/pel_reconciler.go
type pelReconciler struct {
    rdb      redis.UniversalClient
    sink     *store.Sink
    log      *slog.Logger
    consumer string // local consumer name
}

func (r *pelReconciler) Run(ctx context.Context) error {
    t := time.NewTicker(30 * time.Second)
    defer t.Stop()
    for {
        select {
        case <-ctx.Done(): return ctx.Err()
        case <-t.C:
        }
        if err := r.scanOnce(ctx); err != nil {
            r.log.Warn("archive pel scan failed", "err", err)
            metricPelScanError("archive")
        }
    }
}

func (r *pelReconciler) scanOnce(ctx context.Context) error {
    pending, err := r.rdb.XPendingExt(ctx, &redis.XPendingExtArgs{
        Stream: rustpact.StreamUsageDetailed,
        Group:  rustpact.GroupArchiveDrainer,
        Idle:   5 * time.Minute,
        Start:  "-",
        End:    "+",
        Count:  100,
    }).Result()
    if err != nil { return err }
    if len(pending) == 0 { return nil }
    var ids []string
    for _, p := range pending { ids = append(ids, p.ID) }
    msgs, err := r.rdb.XClaim(ctx, &redis.XClaimArgs{
        Stream:   rustpact.StreamUsageDetailed,
        Group:    rustpact.GroupArchiveDrainer,
        Consumer: r.consumer,
        MinIdle:  5 * time.Minute,
        Messages: ids,
    }).Result()
    if err != nil { return err }
    for _, m := range msgs {
        e, derr := DecodeEntry(toStreamFields(m.Values))
        if derr != nil {
            // 烂数据：直接 XAck 防止永驻 PEL
            _ = r.rdb.XAck(ctx, rustpact.StreamUsageDetailed, rustpact.GroupArchiveDrainer, m.ID).Err()
            continue
        }
        if perr := r.sink.PushWithID(m.ID, EntryToRow(e)); perr != nil {
            r.log.Warn("archive pel push failed; will retry next scan", "id", m.ID, "err", perr)
        }
    }
    return nil
}
```

```sql
-- migrations-clickhouse/00XX_archive_replacing_mergetree.sql
ALTER TABLE usage_detailed MODIFY ENGINE = ReplacingMergeTree;
-- 或重建（视表非空与生产策略，up/down 在 WS-14 内验证）
-- 关键：ORDER BY (request_id, attempt_index, surface)
-- TTL ts + INTERVAL 30 DAY DELETE
```

**ADR**：
- **候选 A（已选）**：ReplacingMergeTree 在 CH 端去重。理由：reclaim 不可避免会重发；CH 引擎天然支持。
- **候选 B（拒绝）**：在 Go 端用 Redis SET dedup 24h。理由：重复 6 倍 RTT，且 24h 后窗口过期可能漏夯。

**【源码验证 SOURCE-CHECK】**
- Read `shared/platform/store/clickhouse.go`（确认 driver 已有 settings 注入位）。
- Read `migrations-clickhouse/` 历史，确认 goose schema_migrations 表 + 编号。
- Read `modules/billing/business/settler.go` 查看其 XAUTOCLAIM/XPending 用法，复刻模式。
- Read `crates/gw-store/migrations/` 看 Rust 是否对同表有 DDL（contract 同步）。

**【如何验证 VERIFY】**

1. chaos：模拟 drainer SIGKILL → 5min 后 reconciler 拉走 PEL → CH 行数 == 期望（用 `SELECT count() FROM usage_detailed FINAL WHERE request_id IN (...)`）。
2. 集成：手动写两次同 `(request_id, attempt_index, surface)` → `OPTIMIZE FINAL` 后只剩 1 行。
3. CH driver setting 验证：`SELECT name, value FROM system.settings WHERE name = 'wait_for_async_insert'` → `'1'`。
4. 输出：`logs/W2-T05-verify.txt`。

**【依赖】** 阻塞于 T01/T03/T04；并行于 T06。

---

### TASK W2-T06：H5 monitoring `audit_sink` 同 H3/H4 改造

**【是什么 What it IS】**
- 修改 `modules/monitoring/business/audit_sink.go`：复用 T03/T04/T05 的 Sink + Drainer 改造。
- 替换 `Emit` 内 `_ = s.sink.Push(row)` 为 `PushWithID + 反馈错误`；如 `ErrSinkFull` → emit `alm_audit_sink_full_total` + 不静默丢。
- `auditInserter.Insert` 改为整批 INSERT 而非 row-by-row（CH SQL）；CH 表同改 ReplacingMergeTree。

**【不是什么 What it is NOT】**
- ❌ 不改 audit event payload schema（仅 sink 路径）。
- ❌ 不替换 audit DLQ 策略（DLQ 由 WS-5 V16 详细定义）。

**【为什么 Why this exists / why bug occurs】**
- 根因：v1 H5 + audit_sink.go:50-63 row-by-row 串行写第一行错就 return；service.go:73-79 用 `_ = sink.Push` 丢错；buffer 满后审计 silent 丢；安全审计完整性丢失。
- 引用：v1 §4 WS-2 S2.6；adversarial-WS2 W6（buf cap policy）。

**【做什么 DO】**
1. 同 T03：`audit_sink` 构造时必传 `OnInsertError`（panic guard 自动覆盖）。
2. 同 T04：drainer ackLoop 模型（若 monitoring 有独立 redis stream → 加同样 reconciler；若直接 sink-only → 仅 OverflowPolicy + metric）。
3. `Emit` 改为返回错误（caller decide）：
   ```go
   if err := s.sink.Push(row); err != nil {
       metricAuditDrop("monitoring", err.Error())
       return err // caller 决定 retry / DLQ
   }
   ```
4. `auditInserter.Insert` 改批量 INSERT（CH `INSERT INTO ... VALUES (...), (...), ...` 整批）。
5. service.go 调用方：高优先级事件（login_failed/unauthorized_access）必须返回 err 给 caller；低优先级（rate_limited）可吞但 emit metric。
6. CH 表 schema 改 ReplacingMergeTree（同 T05 模式）。

**【不做什么 DON'T】**
- ❌ 不让 audit Emit 阻塞 critical path（用 chan 容量做背压，cap 满 fail-closed）。
- ❌ 不在 audit Emit 路径调 PG（保持 CH-only）。
- ❌ 不用 `_ =` 吞错（与原 bug 同模式）。

**【怎么做 HOW (具体)】**

```go
// modules/monitoring/business/service.go
func (s *Service) Emit(ctx context.Context, ev AuditEvent) error {
    row := ev.ToRow()
    if err := s.sink.PushWithID(ev.RequestID, row); err != nil {
        metricAuditDrop("emit", err.Error())
        if ev.Severity >= SeverityHigh {
            return fmt.Errorf("audit emit failed (severity=%v): %w", ev.Severity, err)
        }
        return nil // 低优先级吞错，仅 metric
    }
    return nil
}
```

```go
// modules/monitoring/business/audit_sink.go
func (i *auditInserter) Insert(ctx context.Context, rows [][]any) error {
    if len(rows) == 0 { return nil }
    // 整批 INSERT：CH driver 支持多 row 一次发送
    return i.ch.Exec(ctx, "INSERT INTO audit_events (...) VALUES", rows...)
}
```

**【源码验证 SOURCE-CHECK】**
- Read `modules/monitoring/business/service.go:60-100`（确认 `_ = s.sink.Push(row)` 与 H5 描述一致）。
- Read `modules/monitoring/business/audit_sink.go:1-90`（确认 row-by-row 行为）。
- Read `migrations-clickhouse/` 找 `audit_events` 现有 DDL。
- 若 service.go 实际未用 `_ = Push`（v2 §0 重要 2 + H9 反证类） → 修正本 task 并记录。

**【如何验证 VERIFY】**

1. 单元：`go test ./modules/monitoring/business/ -run TestAuditEmit -race`。
2. chaos：CH down 30s + 10k audit events → 恢复后所有 events 在 CH 表（用 ReplacingMergeTree FINAL 验证）。
3. 安全测试：刷 5k `request_rate_limited` 后再发 1 个 `login_failed` → 后者必到达 CH（用 fail-closed return err 保证不丢）。
4. 输出：`logs/W2-T06-verify.txt`。

**【依赖】** 阻塞于 T03/T04/T05；并行于 T07。

---

### TASK W2-T07：H13 + REV-FINAL-W4 — sink shutdown ordering + drainer wait

**【是什么 What it IS】**
- 修改 `shared/platform/store/sink.go` `Run` 的 drain 退出路径：不再用 `default` 退出，改为 producer-driven close 信号 + deadline timeout。
- 修改 `modules/archive/business/drainer.go` 与 `modules/monitoring/business/service.go` shutdown 路径：①关 producer ctx；②等 producer goroutine 全退（wg.Wait）；③让 sink 最后一次 flush；④关 rdb/pgPool。
- v2 REV-FINAL-W4 明确："**deadline 退出而不关 channel**；生产侧必须先停"。

**【不是什么 What it is NOT】**
- ❌ 不 close sink.in channel（会引发 send-on-closed panic）。
- ❌ 不在 sink 内部超时后 panic（仅 return ctx.Err()）。
- ❌ 不替换 errgroup 退出顺序（本 task 仅改 shutdown，errgroup 拓扑由 T09/T10 重写）。

**【为什么 Why this exists / why bug occurs】**
- 根因：sink.go:67-77 drain 用 `case <-ctx.Done(): for { ... default: flush; return }`。但 Push 是非阻塞 chan send，两者间有微秒漏窗 → 漏数据 + caller 收到 nil。
- v2 REV-FINAL-W4：用 `producerDone <-chan struct{}` 信号 + 5s deadline；deadline 内 drain 完 `s.in` 全部 buffered rows，超 deadline 强 flush 已收到部分。

**【做什么 DO】**
1. Sink 新增 `Close(ctx context.Context) error`：caller 调用此方法标记 producer 已停；Run 切换到 drain phase 直到 chan empty 或 deadline。
2. Run 的 drain phase 改为：
   ```go
   case <-ctx.Done() or <-s.closeSig:
       deadline := time.After(5 * time.Second)
       for {
           select {
           case row := <-s.in:
               buf = append(buf, row)
               if len(buf) >= s.opts.MaxBatch {
                   flush(...)
               }
           case <-deadline:
               flush(...) // 强 flush 已收到
               return ctx.Err()
           default:
               // chan 空且无 deadline 触发：尝试再 read 一次，若仍空 → flush + return
               if !drainOneRound() { flush(...); return ctx.Err() }
           }
       }
   ```
3. Drainer 改 Run 退出：先 cancel producer ctx → wg.Wait → `sink.Close(ctx2)`。
4. binaries 改 shutdown 顺序（与 T09/T10 同 PR 同步）。

**【不做什么 DON'T】**
- ❌ 不 close in-chan。
- ❌ 不让 sink.Close 接收 nil ctx（必须超时上限）。
- ❌ 不删除 `default` 分支后改为永久阻塞（必须有 deadline）。

**【怎么做 HOW (具体)】**

```go
// shared/platform/store/sink.go
type Sink struct {
    // ... existing
    closeSig chan struct{}
    closed   atomic.Bool
}

func (s *Sink) Close(ctx context.Context) error {
    if !s.closed.CompareAndSwap(false, true) { return nil }
    close(s.closeSig) // 通知 Run 进入 drain phase
    // 等待 Run 完成 - 调用方应通过 errgroup wait
    return nil
}

func (s *Sink) Run(ctx context.Context) error {
    t := time.NewTicker(s.opts.FlushInterval)
    defer t.Stop()
    buf := make([][]any, 0, s.opts.MaxBatch)

    for {
        select {
        case <-ctx.Done():
            return s.drainAndExit(ctx, &buf)
        case <-s.closeSig:
            return s.drainAndExit(ctx, &buf)
        case row := <-s.in:
            buf = append(buf, row.row)
            if len(buf) >= s.opts.MaxBatch {
                s.flushOnce(ctx, &buf)
            }
        case <-t.C:
            s.flushOnce(ctx, &buf)
        }
    }
}

func (s *Sink) drainAndExit(ctx context.Context, buf *[][]any) error {
    deadline := time.After(5 * time.Second)
    for {
        select {
        case row := <-s.in:
            *buf = append(*buf, row.row)
        case <-deadline:
            flushCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
            s.flushOnce(flushCtx, buf)
            cancel()
            return nil
        default:
            // chan 空：最终 flush 后退出
            flushCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
            s.flushOnce(flushCtx, buf)
            cancel()
            return nil
        }
    }
}
```

**ADR**：
- **候选 A（已选）**：closeSig + deadline，不 close chan。理由：避免 send-on-closed panic，producer 仍可在 close 后短暂尝试 Push（chan send select default 路径 ErrSinkFull）。
- **候选 B（拒绝）**：close chan + recover send。理由：recover panic 是反模式。

**【源码验证 SOURCE-CHECK】**
- Read `shared/platform/store/sink.go:65-88` 当前 drain loop。
- Read `binaries/web/main.go:85-90` shutdown 顺序。
- Read `binaries/worker-billing/main.go` 与 `binaries/all-in-one/main.go` shutdown 路径。

**【如何验证 VERIFY】**

1. 单元：`TestSinkCloseDrainsAllRows` — 1000 Push → Close → 期望 CH 端 1000 行（mock inserter 计数）。
2. 单元：`TestSinkCloseDeadline` — mock inserter sleep 10s → Close → 5s deadline 触发 → metric `alm_sink_close_deadline_exceeded_total` ≥ 1。
3. chaos：archive drainer SIGTERM → 期望 PEL 与 sink buffered rows 全 drain，无未 XAck 的成功批。
4. 输出：`logs/W2-T07-verify.txt`。

**【依赖】** 阻塞于 T03/T04；阻塞 T09/T10。

---

### TASK W2-T08：G8 InflightReaper XADD+DEL 合并为单条 Lua

**【是什么 What it IS】**
- 修改 `modules/billing/business/inflight_reaper.go:126-134`：把 `XAdd` 和 `Del` 改为单条 EVAL Lua 脚本，server-side 原子。

**【不是什么 What it is NOT】**
- ❌ 不改 InflightReaper 的扫盘策略（HSCAN 频率、ZRANGEBYSCORE 等）。
- ❌ 不改 record schema。
- ❌ 不和 Rust 侧 R6 inflight_reconciler 合并（那是 WS-1 T16）。

**【为什么 Why this exists / why bug occurs】**
- 根因：line 126 `XAdd` 成功后 line 132 `Del` 失败 → 下一轮再 XAdd → `usage:records` 重复爆 MAXLEN（v1 G8）。
- adversarial-WS2 W6 已确认 Lua atomic 唯一可行方案；与 WS-1 R6 同模式。

**【做什么 DO】**
1. 在 `modules/billing/business/scripts.go` 新增常量 `ReapInflightLua`：`XADD KEYS[1] * payload ARGV[1]; DEL KEYS[2]; return 'ok'`。
2. `inflight_reaper.go` flushFallbackOnce 内：
   ```go
   _, err := r.rdb.Eval(ctx, ReapInflightLua,
       []string{rustpact.StreamUsageRecords, key},
       string(body),
   ).Result()
   if err != nil { return false, err }
   return true, nil
   ```
3. 删除 124-134 原有两步代码。
4. 单元测试：mock redis EvalSha 与 Eval 兼容；模拟 server 故障两步原子。

**【不做什么 DON'T】**
- ❌ 不在 Lua 内加 MAXLEN（v2 REV-FINAL-M7 已禁；trim 由独立 job）。
- ❌ 不在 Lua 内加额外 condition check（保持简单：XADD + DEL）。

**【怎么做 HOW (具体)】**

```go
// modules/billing/business/scripts.go
const ReapInflightLua = `
redis.call('XADD', KEYS[1], '*', 'payload', ARGV[1])
redis.call('DEL', KEYS[2])
return 'ok'
`
```

**【源码验证 SOURCE-CHECK】**
- Read `modules/billing/business/inflight_reaper.go:100-140` 全文（已确认 line 126/132 模式）。
- Read `modules/billing/business/inflight_reaper_test.go` 确认现有测试 mock。
- Read `shared/rustpact/` 找 `StreamUsageRecords` 常量。

**【如何验证 VERIFY】**

1. 单元：`go test ./modules/billing/business/ -run TestInflightReaper -race -count=10`。
2. 集成 chaos：在 Eval 之间杀 Redis → 重启后扫盘 → 0 重复 XADD（统计 `XLEN usage:records` 与预期 record 数比对）。
3. 输出：`logs/W2-T08-verify.txt`。

**【依赖】** 阻塞于 T01（用 OneShot 包 reaper）；独立于 T03-T07。

---

### TASK W2-T09：S2 + REV-FINAL-W4/V10 — `binaries/web/main.go` shutdown 改造

**【是什么 What it IS】**
- 重写 `binaries/web/main.go` 的 main 函数：
  - 三个裸 `go func`（lines 67, 74, 79）全部包 `runtime.Supervised(...)` 注册到 `*errgroup.Group`。
  - shutdown 顺序：(1) `srv.Shutdown(ctx)` → (2) cancel ctx → (3) `g.Wait()` → (4) `sink.Close(...)` → (5) defer Close (rdb/pgPool)。
  - `defer pgPool.Close()` / `defer rdb.Close()` **移出 defer**：必须在 g.Wait 之后。

**【不是什么 What it is NOT】**
- ❌ 不改 cache/biz module API。
- ❌ 不改 HTTP server timeout / TLS。

**【为什么 Why this exists / why bug occurs】**
- 根因：S2 — 三个裸 `go func`：(a) `srv.ListenAndServe`、(b) `cachebiz.NewInvalidator`、(c) `runtime.RunWorkers` — shutdown 时 `srv.Shutdown` 后立刻 defer Close pgPool/rdb；invalidator 和 module workers 仍在跑可能正在 XAck/DB write，连接 nil → panic / 静默丢。
- v2 REV-FINAL-W4 + REV-FINAL-V10 强制重写。

**【做什么 DO】**
1. main 函数顶部 `var g errgroup.Group; ctx, cancel := context.WithCancel(ctx)`。
2. 用 `g.Go(runtime.Supervised("http_server", ..., httpServerFn))` 起 HTTP。
3. 用 `g.Go(runtime.Supervised("cache_invalidator", ..., invalidatorFn))` 起 invalidator。
4. 用 `g.Go(runtime.Supervised("module_workers", ..., runtime.RunWorkers))` 起 workers。
5. signal handler 触发后：
   - `_ = srv.Shutdown(ctx2)`（HTTP-only graceful）
   - `cancel()`（停 Supervised loop）
   - `if err := g.Wait(); err != nil && !errors.Is(err, context.Canceled) { logger.Error(...) }`
   - `pgPool.Close()`
   - `rdb.Close()`
6. defer 仅 `signal.Stop`、`cancel`。

**【不做什么 DON'T】**
- ❌ 不在 supervisor 内调 srv.Shutdown（让 main 控）。
- ❌ 不并行 srv.Shutdown 与 cancel（顺序确定）。
- ❌ 不删除 `errors.Is(..., http.ErrServerClosed)` 判断（仍合法）。

**【怎么做 HOW (具体)】**

```go
// binaries/web/main.go (rewrite)
func main() {
    cfg, err := config.Load(os.Getenv("ALM_CONFIG_FILE"))
    if err != nil { panic(err) }
    logger := log.New(cfg.String("log.level"))

    rootCtx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
    defer stop()
    ctx, cancel := context.WithCancel(rootCtx)
    defer cancel()

    pgPool, err := store.NewPG(ctx, store.PgConfig{DSN: cfg.String("db.url"), Multiplier: 4})
    if err != nil { panic(err) }
    rdb, err := store.NewRedis(cfg.String("redis.url"))
    if err != nil { panic(err) }
    sharedCache := pcache.New(rdb)

    deps := runtime.Deps{Cfg: cfg, Log: logger, DB: pgPool, Redis: rdb, Reg: contracts.NewRegistry()}
    modules := app.WebModules()
    for _, m := range modules { m.Provide(deps, deps.Reg) }
    var workers []runtime.Worker
    for _, m := range modules { workers = append(workers, m.Workers()...) }

    r := runtime.BuildRouter(deps, modules)
    runtime.ServeStatic(r, cfg.String("frontend.dist"))
    addr := cfg.String("http.addr")
    if addr == "" { addr = ":8080" }
    srv := runtime.NewHTTPServer(addr, r)

    g, gctx := errgroup.WithContext(ctx)

    g.Go(runtime.Supervised("http_server",
        runtime.RestartPolicy{PanicAction: runtime.PanicFatalToErrgroup},
        func(c context.Context) error {
            logger.Info("web listening", "addr", addr)
            if err := srv.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
                return err
            }
            return nil
        }))

    g.Go(runtime.Supervised("cache_invalidator",
        runtime.RestartPolicy{}, // defaults
        func(c context.Context) error {
            return cachebiz.NewInvalidator(rdb, sharedCache, logger).Run(c)
        }))

    g.Go(runtime.Supervised("module_workers",
        runtime.RestartPolicy{},
        func(c context.Context) error {
            return runtime.RunWorkers(c, deps, workers)
        }))

    <-gctx.Done()
    shutdownCtx, c2 := context.WithTimeout(context.Background(), 10*time.Second)
    _ = srv.Shutdown(shutdownCtx)
    c2()
    cancel() // 让 Supervised loop ctx.Done
    if werr := g.Wait(); werr != nil && !errors.Is(werr, context.Canceled) {
        logger.Error("workers exited with error", "err", werr)
    }
    pgPool.Close()
    rdb.Close()
    logger.Info("web stopped")
}
```

**【源码验证 SOURCE-CHECK】**
- Read `binaries/web/main.go` 全文（已确认 line 67/74/79 + line 36/42 defer Close）。
- Read `shared/runtime/host.go` HTTPServer 工厂 + module 接口。
- 注意：`http_server` 用 `PanicFatalToErrgroup`（HTTP 死了应整体退出）；其他 worker 默认 Restart。

**【如何验证 VERIFY】**

1. 集成：`go run ./binaries/web/ &; kill -TERM $!` → 在 strace / 日志检查 shutdown 顺序：HTTP closed → cancel → wg done → pool close。
2. chaos：HTTP request 进行中（slowloris）发 SIGTERM → 10s 内 graceful shutdown，pgPool 在 in-flight query 之后才关。
3. lint：`bash scripts/check-goroutines.sh ./binaries/web/...` 期望 PASS（已无裸 `go func`）。
4. 输出：`logs/W2-T09-verify.txt`。

**【依赖】** 阻塞于 T01/T02/T07；并行于 T10。

---

### TASK W2-T10：`binaries/all-in-one/main.go` 同样 shutdown 改造

**【是什么 What it IS】**
- 与 T09 同模式重写 `binaries/all-in-one/main.go`。

**【不是什么 What it is NOT】**
- ❌ 不动 `worker-billing` 单独 binary（assumed already supervised；执行者 Read 确认）。

**【为什么 Why this exists / why bug occurs】**
- v1 S3：all-in-one errgroup 让 worker 错误连坐杀 HTTP listener；S4 cache invalidator 是裸 goroutine。WS-2 + WS-4 共治。

**【做什么 DO】**
1. Read `binaries/all-in-one/main.go` 全文先做差异分析（与 web 不同点：含 billing settler / archive drainer / monitoring 等更多 worker）。
2. 拆为 2 个 errgroup：`gHTTP`（仅 HTTP server，PanicFatalToErrgroup）与 `gWorkers`（所有后台，PanicRestart）。
3. 共享 `signal ctx`；shutdown 顺序：HTTP 先 → wait(gHTTP) → cancel workers ctx → wait(gWorkers) → close pools。
4. 每个 worker 走 `runtime.Supervised`，名字加 `aio_` 前缀作 metric label。

**【不做什么 DON'T】**
- ❌ 不让 worker err 杀 HTTP errgroup（拆 group 是关键）。
- ❌ 不在 main 持有 cache invalidator 引用（让 Supervised 自治）。

**【怎么做 HOW (具体)】**

参考 T09 + 拆 group：
```go
gHTTP, hctx := errgroup.WithContext(ctx)
gWorkers, wctx := errgroup.WithContext(ctx)
gHTTP.Go(runtime.Supervised("aio_http_server", policyFatal, httpFn))
gWorkers.Go(runtime.Supervised("aio_cache_invalidator", policyRestart, invalidatorFn))
gWorkers.Go(runtime.Supervised("aio_billing_settler", policyRestart, settlerFn))
gWorkers.Go(runtime.Supervised("aio_archive_drainer", policyRestart, archiveDrainerFn))
gWorkers.Go(runtime.Supervised("aio_archive_pel_reconciler", policyRestart, pelReconcilerFn))
gWorkers.Go(runtime.Supervised("aio_monitoring_audit_drainer", policyRestart, monitoringFn))
gWorkers.Go(runtime.Supervised("aio_module_workers", policyRestart, runtime.RunWorkers))

<-ctx.Done()
shutdownCtx, c := context.WithTimeout(context.Background(), 10*time.Second)
_ = srv.Shutdown(shutdownCtx); c()
_ = gHTTP.Wait()
cancel()
_ = gWorkers.Wait()
pgPool.Close(); rdb.Close()
```

**【源码验证 SOURCE-CHECK】**
- Read `binaries/all-in-one/main.go` 全文。
- 记录每个 worker 名字到 `shared/runtime/recipes.go`（T02 已建结构）。

**【如何验证 VERIFY】**

1. 注入：让 cache invalidator panic 5 次 → metric 显示 restart_total ≥ 5；HTTP 仍 200。
2. 注入：让 billing settler 持续 err → 进入 unhealthy → `/readyz` 返回 503（REV-FINAL-V11）。
3. shutdown chaos：SIGTERM 时让 archive 仍有 buffered → 5s 内 drain 完毕（W2-T07 通过链）。
4. 输出：`logs/W2-T10-verify.txt`。

**【依赖】** 阻塞于 T01/T02/T05/T07/T08；并行于 T09。

---

### TASK W2-T11：REV-FINAL-W11 — Rust Supervisor API audit + 缺口补全（**强制先做**）

**【是什么 What it IS】**
- 一份 audit 报告 + 一组潜在小 PR：先核对 `crates/gw-runtime/src/supervisor.rs` 与 `crates/gw-store/src/managed_loop.rs` 两个现有 Supervisor 实现，针对 4 项需求列 diff 表：
  1. panic 捕获（`catch_unwind(AssertUnwindSafe(...))`）
  2. RestartPolicy 退避（jitter / cap / consecutive budget）
  3. CancellationToken 传播（含 child token）
  4. `one_shot_spawn` 兄弟函数

**【不是什么 What it is NOT】**
- ❌ 不新建 crate（v2 REV-FINAL-W11 明确："不要默认新建 crate"）。
- ❌ 不删除 `gw-store::managed_loop::Supervisor`（保留作 store 内 ManagedLoop 专用；本 task 仅扩展 `gw-runtime`）。

**【为什么 Why this exists / why bug occurs】**
- v2 §0 重要 2 + REV-FINAL-W11 明确：现有 API 已部分覆盖；不可凭空建新框架。

**【做什么 DO】**
1. Read 两文件全文，建表：
   | 需求 | gw-runtime::Supervisor | gw-store::managed_loop::Supervisor | gap |
   | panic 捕获 | line 98 `AssertUnwindSafe(...).catch_unwind()` ✅ | 无 ✗ | gw-store 缺 |
   | RestartPolicy | line 29-49 完整 ✅ | 仅 Recover/Sleep/Fatal 状态机 ✗ | gw-store 缺 |
   | CancellationToken | line 12 `ShutdownToken`，line 94 `child()` ✅ | 无 ✗ | gw-store 缺 |
   | one_shot_spawn | 无 ✗ | 无 ✗ | 两者都缺 |
2. 决定（写 ADR）：
   - 在 `gw-runtime` 内**新增** `one_shot_spawn(name, fut)`：spawn 一个一次性 task，panic 捕获，无重启，Err 通过 metric。
   - **不动** `gw-store::managed_loop::Supervisor`（它的 Recover 语义对 store-level loop 有意义）；但其调用方应优先用 `gw-runtime::Supervisor`，除非确实需要 ManagedLoop 状态机。
3. 写 audit 报告 `docs/issues/2026-06-17-investigates/INVESTIGATE-W11-supervisor-audit.md`。
4. 实现 `one_shot_spawn`（小 PR 跟在 audit 后）。
5. 更新 `docs/architecture.md` 区分两套 Supervisor 用法。

**【不做什么 DON'T】**
- ❌ 不重写 `gw-runtime::Supervisor`（已 OK）。
- ❌ 不引入 `tokio::task::JoinSet` 替代 Supervisor（不同 use case）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-runtime/src/supervisor.rs (新增)
pub fn one_shot_spawn<F, Fut>(name: &'static str, fut: F) -> tokio::task::JoinHandle<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), anyhow::Error>> + Send + 'static,
{
    tokio::spawn(async move {
        use std::panic::AssertUnwindSafe;
        use futures::FutureExt;
        let result = AssertUnwindSafe(fut()).catch_unwind().await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(target: "gw_runtime::oneshot", task = name, error = %err, "one-shot returned error");
                metrics::counter!("gw_oneshot_total", "task" => name, "result" => "error").increment(1);
            }
            Err(_panic) => {
                tracing::error!(target: "gw_runtime::oneshot", task = name, "one-shot panicked");
                metrics::counter!("gw_oneshot_total", "task" => name, "result" => "panic").increment(1);
            }
        }
    })
}
```

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-runtime/src/supervisor.rs` 全文（已读，confirmed 完整）。
- Read `crates/gw-store/src/managed_loop.rs` 全文（已读 1-135 行，confirmed 不同 use case）。
- 抽样 5 个调用点：`grep -rn "managed_loop::Supervisor::spawn" crates/`、`grep -rn "Supervisor::new" crates/` — 记录到 audit 报告。

**【如何验证 VERIFY】**

1. `cargo test -p gw-runtime -- supervisor` 全 PASS。
2. 新 `one_shot_spawn` 单元：panic 路径不杀进程，metric +1。
3. audit report 检入 `docs/issues/2026-06-17-investigates/INVESTIGATE-W11-supervisor-audit.md`。
4. `bash scripts/check-invariants.sh` 仍 PASS。
5. 输出：`logs/W2-T11-verify.txt`。

**【依赖】** 无前置；阻塞 T12/T13/T15/T20/T22/T23（这些都要选择正确 supervisor wrapping）。

---

### TASK W2-T12：Task-Runtime-1 — ExecLoop 注册到 RunningTaskRegistry + cancel 联动

**【是什么 What it IS】**
- 修改 `crates/gw-task-runtime/src/exec_loop.rs`：`ExecLoopConfig` 加 `registry: Arc<RunningTaskRegistry>` 字段；`execute_claimed` 入口注册（含 cancel_token），出口 RAII guard 移除。
- 修改 `crates/gw-task-worker/src/lib.rs:392-448`：把已创建的 registry 传给 `ExecLoop::new(...)`。

**【不是什么 What it is NOT】**
- ❌ 不改 HeartbeatLoop / CancelLoop（已有 registry，正确）。
- ❌ 不改 Lua 脚本（T13/T14 范围）。

**【为什么 Why this exists / why bug occurs】**
- 根因（已源码确认）：`exec_loop.rs:97-103` ExecLoop struct 无 `registry` 字段；worker lib.rs 创建 registry 后只传 HeartbeatLoop/CancelLoop，导致 admin cancel + heartbeat 全 no-op，>60s 任务 lease 过期被另一 worker 重 claim → 双计费。

**【做什么 DO】**
1. `ExecLoopConfig` 加 `pub registry: Arc<RunningTaskRegistry>` 字段。
2. `ExecLoop` struct 加 `registry: Arc<RunningTaskRegistry>`。
3. `execute_claimed` 入口 `let _guard = self.registry.scope_guard(task_id, lease_until_ms, cancel_token.clone());`（RAII guard 出 scope 自动 remove）。
4. `RunningTaskRegistry` 加 `scope_guard(...)` 方法（若未有，加在 `crates/gw-task-runtime/src/lib.rs` 或 registry 所在文件）。
5. `call_upstream` poll 内用 `tokio::select!(token.cancelled() => ..., upstream.next() => ...)` — drain 上游至 EOS or per-codec timeout（T15 落地，本 task 仅注入 cancel_token 引用）。
6. worker lib.rs:392 把 registry 传给 ExecLoop。

**【不做什么 DON'T】**
- ❌ 不在 settle 之前 remove registry 项（必须 RAII guard 在 settle 之后 drop）。
- ❌ 不让 cancel_token cancel 后立即 abort 上游 stream（会触发 R7 反向漏）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-task-runtime/src/exec_loop.rs
pub struct ExecLoopConfig {
    // ... existing fields
    pub registry: Arc<RunningTaskRegistry>,
}

pub struct ExecLoop<C> {
    // ... existing
    registry: Arc<RunningTaskRegistry>,
}

impl<C: TaskContract> ExecLoop<C> {
    async fn execute_claimed(&self, claim: ClaimedTask) -> anyhow::Result<()> {
        let cancel_token = CancellationToken::new();
        let _guard = self.registry.scope_guard(
            claim.task_id.clone(),
            claim.lease_until_ms,
            cancel_token.clone(),
        );
        // ... existing body, pass cancel_token down to call_upstream
    }
}
```

```rust
// crates/gw-task-worker/src/lib.rs:395-412 (rewrite)
let registry = Arc::new(RunningTaskRegistry::default());
let exec_cfg = ExecLoopConfig {
    // ... existing fields
    registry: Arc::clone(&registry),
    cancellation_token: shutdown.child(),
};
let exec = ExecLoop::new(contract.clone(), http_client.clone(), exec_cfg);
// 然后 heartbeat / cancel 用同一 Arc<RunningTaskRegistry>
```

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-task-runtime/src/exec_loop.rs:1-110`（已确认 struct 字段）。
- Read `crates/gw-task-worker/src/lib.rs:392-448`（确认 registry 传递路径）。
- Read `crates/gw-task-runtime/src/{heartbeat_loop.rs,cancel_loop.rs}` 找 `RunningTaskRegistry` 定义文件。

**【如何验证 VERIFY】**

1. `cargo test -p gw-task-runtime -- exec_loop_registers_task` 通过。
2. 集成：90s 任务 → heartbeat 持续延 lease（不触发 RECOVER_LEASE_LUA）。
3. 集成：admin send cancel → exec_loop poll 内 select 触发 → upstream drain to EOS → settle Terminated。
4. metric：`gw_task_double_executed_total` == 0。
5. 输出：`logs/W2-T12-verify.txt`。

**【依赖】** 阻塞于 T11；并行于 T13/T14。

---

### TASK W2-T13：REV-FINAL-W6 + Task-Runtime-2 — SETTLE_LUA 幂等 + 外层 XACK + XAUTOCLAIM 兜底

**【是什么 What it IS】**
- 修改 `crates/gw-task/src/scripts.rs` `SETTLE_LUA`：在脚本顶部加 status='settled' 检查 → 'noop'。
- `execute_claimed` settle 路径在 Lua 调用成功**之后**单独调 `client.xack("tasks:queue", "tasks-workers", stream_id)`（**不在 Lua 内**，避免 CROSSSLOT — INVESTIGATE-008 待定，但默认按 single shard 假设）。
- 新增 `tasks_pending_reconciler`：每 30s `XPENDING tasks:queue tasks-workers IDLE 300000` → 检查 HGET task status='settled' → XACK 兜底。

**【不是什么 What it is NOT】**
- ❌ 不在 SETTLE_LUA 内加 XACK（v2 REV-FINAL-W6 / 上游 W6 已禁 CROSSSLOT 风险）。
- ❌ 不删除 SETTLE_LUA 的 ZREM/INCRBY 等其他 mutation（仅在脚本顶部加幂等守卫）。
- ❌ 不引入 Lua 内 transaction（脚本本身已是原子）。

**【为什么 Why this exists / why bug occurs】**
- 根因（已源码确认）：`scripts.rs:59-88` SETTLE_LUA 无 status 检查，retry 时 `INCRBY -released_micro` 和 `INCRBY deducted_micro` 双扣；XADD usage:records 也重复。
- 根因（Task-Runtime-2）：`tasks:queue` 永不 XACK，PEL 无界增长。

**【做什么 DO】**
1. SETTLE_LUA 顶部加：
   ```lua
   local cur = redis.call('HGET', KEYS[1], 'status')
   if cur == 'settled' or cur == 'terminated' then
       return 'noop'
   end
   ```
2. `execute_claimed` settle 成功后追加 `client.xack(...)`；失败仅 log。
3. 新文件 `crates/gw-task-runtime/src/pending_reconciler.rs`：30s 周期；XPENDING IDLE 5min；对每个 entry HGET status；已 settled → XACK；未 settled 且 IDLE > 30min → log + metric（不强 XCLAIM，由 RECOVER_LEASE_LUA 处理）。
4. worker lib.rs 启动 reconciler（用 `gw-runtime::Supervisor`）。
5. 处理 INVESTIGATE-008：若部署是 Cluster → 让 KEYS[1..6] 共享 hash-tag（如 `{task:<id>}:lease`），本 task 先按 single shard 假设，留 INVESTIGATE-008 决议时回填。

**【不做什么 DON'T】**
- ❌ 不在 settle 失败时 abort task（idempotent 守卫足够）。
- ❌ 不让 reconciler 强 XCLAIM 已 settled 项（XACK 即可）。
- ❌ 不引入 PEL XCLAIM 写新 record（CH 端 ReplacingMergeTree 不适用 task stream）。

**【怎么做 HOW (具体)】**

```lua
-- crates/gw-task/src/scripts.rs SETTLE_LUA (patched header)
local task_id = ARGV[1]
local terminal_status = ARGV[2]
-- ... 既有 ARGV 解析

-- W2-T13 NEW: idempotency guard
local cur = redis.call('HGET', KEYS[1], 'status')
if cur == 'settled' or cur == 'terminated' then
    return 'noop'
end

-- ... 既有 mutation
```

```rust
// crates/gw-task-runtime/src/pending_reconciler.rs (new)
pub struct PendingReconciler<C: TaskContract> {
    contract: C,
    pool: RedisPool,
}

impl<C: TaskContract> PendingReconciler<C> {
    pub async fn run(&self, token: ShutdownToken) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                _ = interval.tick() => {}
            }
            if let Err(e) = self.scan_once().await {
                tracing::warn!("pending reconciler: {e:?}");
                metrics::counter!("gw_task_pending_scan_error_total").increment(1);
            }
        }
    }

    async fn scan_once(&self) -> anyhow::Result<()> {
        let client = self.pool.next();
        let pending: Vec<(String, String, u64, u64)> = client.xpending_count(
            "tasks:queue", "tasks-workers",
            Some(300_000), // IDLE 5min
            "-", "+", 100, None,
        ).await?;
        for (entry_id, _consumer, _idle, _deliv) in pending {
            // 解析 task_id 从 entry fields（XRANGE 单条）
            let task_id = read_task_id(&client, &entry_id).await?;
            let status: Option<String> = client.hget(format!("task:{task_id}"), "status").await?;
            if matches!(status.as_deref(), Some("settled") | Some("terminated")) {
                let _: u64 = client.xack("tasks:queue", "tasks-workers", entry_id.clone()).await?;
                metrics::counter!("gw_task_pending_reclaimed_total", "reason" => "already_settled").increment(1);
            }
        }
        Ok(())
    }
}
```

**ADR**：
- **候选 A（已选）**：Lua 顶部 status guard + 外层 XACK + reconciler。理由：避免 CROSSSLOT，保留单 shard 原子，PEL 由 reconciler 收尾。
- **候选 B（拒绝）**：所有 KEYS 用 hash-tag 强制同 slot + Lua 内 XACK。理由：耦合 Redis 部署拓扑，未来 sharding 不灵活。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-task/src/scripts.rs:59-88` 全文（已确认）。
- Read `crates/gw-task-runtime/src/redis_contract.rs:367-435`（XREADGROUP）—确认 stream_id 字段名与 ClaimedTask 结构。
- Read `cancel_loop.rs:92` 现有唯一 XACK 调用（验证 XACK 用法不变）。
- 若 Cluster 部署确实需要 hash-tag（INVESTIGATE-008），在 commit message 标注。

**【如何验证 VERIFY】**

1. `cargo test -p gw-task -- settle_lua_idempotent` 通过：两次 EVAL 同 task 仅一次 mutation。
2. `cargo test -p gw-task-runtime -- pending_reconciler_acks_settled` 通过。
3. 集成 chaos：执行 task → settle 成功 → kill worker before XACK → 30s 内 reconciler 兜底 XACK → `XLEN tasks:queue` 与 PEL 正常。
4. 输出：`logs/W2-T13-verify.txt`。

**【依赖】** 阻塞于 T11/T12；并行于 T14/T15。

---

### TASK W2-T14：REV-FINAL-W5 — EXTEND_LEASE_LUA NX+GT 两步

**【是什么 What it IS】**
- 修改 heartbeat 用的 `EXTEND_LEASE_LUA`：先 ZSCORE 探测；缺 → ZADD NX；在 → ZADD GT。两步在同 Lua 内。

**【不是什么 What it is NOT】**
- ❌ 不改 RECOVER_LEASE_LUA。
- ❌ 不改 heartbeat 频率配置。

**【为什么 Why this exists / why bug occurs】**
- 根因（v2 REV-FINAL-W5）：`ZADD ... GT` 在成员缺失时**不会创建**，导致 reclaim 后 heartbeat 永久 no-op（lease 不再延长）→ 另一 worker 又会 reclaim。

**【做什么 DO】**
1. 找到 heartbeat 代码（`crates/gw-task-runtime/src/heartbeat_loop.rs` 或 `crates/gw-task/src/scripts.rs`）。
2. 把 single-step `ZADD GT` 改为：
   ```lua
   local existed = redis.call('ZSCORE', KEYS[1], ARGV[1])
   if existed then
       redis.call('ZADD', KEYS[1], 'GT', ARGV[2], ARGV[1])
   else
       redis.call('ZADD', KEYS[1], 'NX', ARGV[2], ARGV[1])
   end
   return 'ok'
   ```
3. 单元测试覆盖：member 不存在 → NX 创建；member 存在 → GT 仅更大才更新。

**【不做什么 DON'T】**
- ❌ 不让 NX 路径无条件创建（必须 ZSCORE 探测）。
- ❌ 不让 NX 与 GT 在不同 Lua 调用里分两次（必须同脚本）。

**【怎么做 HOW (具体)】**

如上 Lua 片段。

**【源码验证 SOURCE-CHECK】**
- `grep -rn "EXTEND_LEASE_LUA\|ZADD.*GT" crates/gw-task*/src/` 找到准确位置。
- 若现有不存在 `EXTEND_LEASE_LUA`（v1 假设过强）→ 本 task 改为定义新 const + 重构 heartbeat_loop 用法。
- Read `crates/gw-task-runtime/src/heartbeat_loop.rs` 现有逻辑。

**【如何验证 VERIFY】**

1. `cargo test -p gw-task -- extend_lease_nx_creates_when_missing` 通过。
2. `cargo test -p gw-task -- extend_lease_gt_only_increases` 通过。
3. 集成：reclaim 后 heartbeat → 新 lease 写入 ZSET → RECOVER_LEASE_LUA 不再触发。
4. 输出：`logs/W2-T14-verify.txt`。

**【依赖】** 阻塞于 T11；并行于 T12/T13。

---

### TASK W2-T15：REV-FINAL-W7 — exec_loop cancel drain per-codec timeout + 全局 semaphore

**【是什么 What it IS】**
- 修改 `crates/gw-task-runtime/src/exec_loop.rs` `call_upstream`：cancel 触发后继续 drain 上游至 EOS or **per-codec** drain_timeout；超时 → `RST_STREAM`（如 Pingora 暴露 → INVESTIGATE-009 待定，否则 abort connection）。
- 引入全局 `Semaphore::new(2 * global_concurrency)` 限制 drainer 并发。

**【不是什么 What it is NOT】**
- ❌ 不在 task path 之外调 drainer（仅 exec_loop / upstream-response path）。
- ❌ 不引入新 codec 抽象（drain_timeout 注入到现有 codec config）。

**【为什么 Why this exists / why bug occurs】**
- v2 REV-FINAL-W7：v1 S2.14 的 5s 太短/太长不一致；不同 codec 流式响应 size 不同，固定值会"短的早死，长的 5s 漏读后段"。

**【做什么 DO】**
1. `ExecLoopConfig` 加 `drain_timeouts: HashMap<TaskTimeoutKey, Duration>`（per-codec key 复用 vendor/kind）。
2. `call_upstream` 中：
   ```rust
   tokio::select! {
       _ = cancel_token.cancelled() => {
           // 进入 drain phase
           let drain_to = self.cfg.drain_timeouts
               .get(&key).copied()
               .unwrap_or(Duration::from_secs(15));
           let _permit = self.drain_sem.clone().try_acquire_owned()
               .map_err(|_| anyhow!("drain semaphore exhausted"))?;
           tokio::time::timeout(drain_to, drain_upstream(&mut upstream)).await.ok();
           // 超时 → drop upstream (TCP RST) or RST_STREAM if available
       }
       result = upstream.next() => { /* normal path */ }
   }
   ```
3. INVESTIGATE-009：检 Pingora 0.8 是否暴露 `RST_STREAM`；若暴露则 prefer 之；否则 drop connection 触发 TCP RST。
4. 全局 `drain_sem: Arc<Semaphore> = Arc::new(Semaphore::new(2 * cfg.global_concurrency))`。

**【不做什么 DON'T】**
- ❌ 不让 drain phase 无限 wait（必须 timeout）。
- ❌ 不在 drain phase 写 settle（drain 是为了上游 commit 计费，不重写本 task 状态）。

**【怎么做 HOW (具体)】**

如上 select 片段。具体 per-codec 默认（参考 v2 REV-FINAL-W7）：
```rust
drain_timeouts: {
    TaskTimeoutKey::new("anthropic", TaskKind::Messages) => Duration::from_secs(30),
    TaskTimeoutKey::new("openai", TaskKind::Chat) => Duration::from_secs(15),
    TaskTimeoutKey::new("gemini", TaskKind::Generate) => Duration::from_secs(30),
}
```

**ADR**：
- **候选 A（已选）**：per-codec drain + semaphore。理由：避免全局短超时丢长流，又防止 DoS 让 drainer task 爆。
- **候选 B（拒绝）**：固定 5s。理由：v1 已被打回。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-task-runtime/src/exec_loop.rs` `call_upstream` 实现（找出 stream poll loop 位置）。
- Read Pingora 0.8 API doc 或 `cargo doc -p pingora` 找 RST_STREAM 接口（落入 INVESTIGATE-009）。

**【如何验证 VERIFY】**

1. 单元：mock upstream 持续推 chunk，cancel 触发后 anthropic codec drain 30s 内完成。
2. 集成 chaos：1000 并发 task 全部 cancel → drain_sem 不爆（metric `gw_task_drain_sem_acquire_fail_total` ≤ 阈值）。
3. INVESTIGATE-009 闭合：写决议到 `docs/issues/.../INVESTIGATE-009.md`。
4. 输出：`logs/W2-T15-verify.txt`。

**【依赖】** 阻塞于 T11/T12；INVESTIGATE-009 闭合前以"drop connection" fallback；并行于 T13/T14。

---

### TASK W2-T16：REV-FINAL-W8 + Tunnel-1 — Tunnel client writer Drop scopeguard + 显式 close().await

**【是什么 What it IS】**
- 修改 `crates/gw-tunnel-client/src/cnexus/transport.rs`：保留 `TunnelStreamWriter::close()` (line 192) 不变；新增 RAII 安全网：writer 被持有点（bridge `relay_plain*`）外包 `scopeguard::guard(writer, |w| { tokio::spawn(async move { let _ = w.close().await; }); })` 确保 Ok/Err/panic 三路径都尝试 close。
- 修改 `crates/gw-proxy/src/tunnel_bridge.rs`：`relay_plain` 内 `tokio::select!` 协调 request/response；abort 等 close ack 或 5s timeout。

**【不是什么 What it is NOT】**
- ❌ 不为 `TunnelStreamWriter` 加 Drop 自动 `try_send Close`（v1 已被打回：失败静默 + shutdown 期 panic）。
- ❌ 不动 `TunnelStreamReader::Drop`（已正确：仅 remove routes）。
- ❌ 不改 Frame protocol。

**【为什么 Why this exists / why bug occurs】**
- 根因（Tunnel-1，源码确认）：`transport.rs:183-203` TunnelStreamWriter 无 Drop；bridge `tunnel_bridge.rs:207-214` `request_task.abort()` 抢在 shutdown 之前 → server `relay_stream` 永远 park → TCP / permit / streams 三重泄漏。

**【做什么 DO】**
1. `scopeguard = "1"` 加入 `gw-proxy/Cargo.toml`（若未）。
2. `tunnel_bridge.rs` 三处 `relay_plain*` 函数：把 writer 移入 scopeguard：
   ```rust
   use scopeguard::guard;
   let writer_guard = guard(writer, |w| {
       tokio::spawn(async move { let _ = w.close().await; });
   });
   ```
3. select! 协调 request/response：response copy 完成后 explicit `let writer = ScopeGuard::into_inner(writer_guard); writer.close().await?;` 走显式路径；abort 仅在 select! all-done 之后。
4. metric：`gw_tunnel_writer_close_path_total{reason}`（"explicit" / "scopeguard" / "panic"）。

**【不做什么 DON'T】**
- ❌ 不在 Drop 内 try_send Close（v1 打回理由：shutdown 期 send 失败 panic）。
- ❌ 不删除 close().await 显式方法（仍是 happy path）。
- ❌ 不在 spawn 内调 abort（让 task 自然完成）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-proxy/src/tunnel_bridge.rs (partial rewrite of relay_plain)
async fn relay_plain(stream: TunnelStream, mut downstream: ...) -> anyhow::Result<()> {
    let (reader, writer) = stream.split();
    let writer_guard = scopeguard::guard(writer, |w| {
        tokio::spawn(async move { let _ = w.close().await; });
        metrics::counter!("gw_tunnel_writer_close_path_total", "reason" => "scopeguard").increment(1);
    });

    let req_task = tokio::spawn(async move {
        // 拷贝 downstream → writer
    });
    let resp_task = tokio::spawn(async move {
        // 拷贝 reader → downstream
    });

    let (req_res, resp_res) = tokio::join!(req_task, resp_task);
    let writer = scopeguard::ScopeGuard::into_inner(writer_guard);
    let _ = writer.close().await;
    metrics::counter!("gw_tunnel_writer_close_path_total", "reason" => "explicit").increment(1);
    req_res??; resp_res??;
    Ok(())
}
```

**ADR**：
- **候选 A（已选）**：scopeguard。理由：覆盖 Ok/Err/panic 三路径；不依赖 Drop tokio context（v1 打回点）。
- **候选 B（拒绝）**：Drop impl + try_send。理由：shutdown 期 chan closed 触发 send panic。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-tunnel-client/src/cnexus/transport.rs:160-203`（已确认 Drop 与 close 现状）。
- Read `crates/gw-proxy/src/tunnel_bridge.rs:200-330`（找 relay_plain* 三处 abort 调用）。
- Read `gw-proxy/Cargo.toml` 确认是否已有 scopeguard。

**【如何验证 VERIFY】**

1. `cargo test -p gw-proxy -- tunnel_bridge_writer_close_paths` 通过。
2. 集成 chaos：5k stream churn / 5min → `gw_tunnel_active_streams` 回零；server-side `streams.lock().await.len()` 回零。
3. metric `gw_tunnel_writer_close_path_total{reason="scopeguard"}` 在 panic-injected test 中 ≥ 1。
4. 输出：`logs/W2-T16-verify.txt`。

**【依赖】** 阻塞于 T11；并行于 T17-T20。

---

### TASK W2-T17：REV-FINAL-W9 + Tunnel-3/5 — `ReconnectingTransport` CancellationToken + `arc_swap` reload_nodes

**【是什么 What it IS】**
- 修改 `crates/gw-tunnel-client/src/cnexus/pool.rs`：`ReconnectingTransport` supervisor 接 `CancellationToken`；`close()` 触发 cancel；soft-close grace 60s 后 hard close。
- 修改 `crates/gw-proxy/src/tunnel_stack.rs`：`transports` 从 `DashMap` 改 `arc_swap::ArcSwap<HashMap<NodeId, Arc<ReconnectingTransport>>>`；`reload_nodes` 构建新 HashMap → `swap` → 对差集 transports 调 close() + 等 supervisor 退。

**【不是什么 What it is NOT】**
- ❌ 不动 frame 序列化。
- ❌ 不在 hot path 做 swap（仅 reload_nodes 路径）。
- ❌ 不重写 WS 协议。

**【为什么 Why this exists / why bug occurs】**
- 根因（Tunnel-3）：supervisor outer loop 无 break；reload_nodes 不清 transports；永生 reconnect。
- 根因（Tunnel-5）：close() 仅 alive=false，supervisor 不停继续重连。
- v2 REV-FINAL-W9：soft-close + 60s grace + hard close（防止存量 stream 中断）。

**【做什么 DO】**
1. `ReconnectingTransport` 加 `cancel_token: CancellationToken` + `soft_closed: Arc<AtomicBool>`。
2. supervisor outer loop：`loop { select! { _ = token.cancelled() => break, _ = inner_supervise() => ... } }`。
3. `open_stream`：若 `soft_closed` → 立即返回 `TunnelError::SoftClosed`。
4. `close()` API：set soft_closed=true → 等 grace 60s → cancel token → 等 supervisor 退。
5. `TunnelStack` 改 `transports: arc_swap::ArcSwap<HashMap<NodeId, Arc<ReconnectingTransport>>>`。
6. `reload_nodes` 流程：
   ```rust
   let old = self.transports.load_full();
   let mut new = HashMap::new();
   for nid in new_nodes { new.insert(nid, old.get(nid).cloned().unwrap_or_else(|| Arc::new(ReconnectingTransport::new(...)))); }
   self.transports.store(Arc::new(new));
   // 关闭差集
   for (nid, t) in old.iter() {
       if !new.contains_key(nid) { tokio::spawn(async move { let _ = t.close().await; }); }
   }
   ```
7. 所有 `.await` 包括 `WsTransport::connect` `select!` 进 token。

**【不做什么 DON'T】**
- ❌ 不在 swap 后立即 hard close 老 transports（必须 soft + grace + hard）。
- ❌ 不让 reload_nodes 阻塞调用方等 close 完成（spawn fire-and-forget）。
- ❌ 不让 DashMap 与 ArcSwap 并存（彻底切换）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-tunnel-client/src/cnexus/pool.rs
pub struct ReconnectingTransport {
    cfg: TransportConfig,
    cancel: CancellationToken,
    soft_closed: Arc<AtomicBool>,
    inner: Arc<arc_swap::ArcSwapOption<WsTransport>>,
    // ...
}

impl ReconnectingTransport {
    pub fn new(cfg: TransportConfig) -> Arc<Self> { /* ... */ }

    pub fn start(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = me.cancel.cancelled() => break,
                    res = WsTransport::connect(me.cfg.clone()) => {
                        match res {
                            Ok(ws) => me.inner.store(Some(Arc::new(ws))),
                            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                        }
                    }
                }
            }
        })
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        self.soft_closed.store(true, Ordering::Release);
        tokio::time::sleep(Duration::from_secs(60)).await; // grace
        self.cancel.cancel();
        self.inner.store(None);
        Ok(())
    }
}
```

```rust
// crates/gw-proxy/src/tunnel_stack.rs
pub struct TunnelStack {
    transports: arc_swap::ArcSwap<HashMap<NodeId, Arc<ReconnectingTransport>>>,
    // ...
}

impl TunnelStack {
    pub fn reload_nodes(&self, new_nodes: Vec<NodeId>) {
        let old = self.transports.load_full();
        let mut new_map = HashMap::new();
        for nid in &new_nodes {
            new_map.insert(nid.clone(), old.get(nid).cloned()
                .unwrap_or_else(|| {
                    let t = ReconnectingTransport::new(self.config_for(nid));
                    t.start();
                    t
                }));
        }
        self.transports.store(Arc::new(new_map));
        // 差集 soft-close
        for (nid, t) in old.iter() {
            if !new_nodes.contains(nid) {
                let t = Arc::clone(t);
                tokio::spawn(async move { let _ = t.close().await; });
            }
        }
    }
}
```

**ADR**：
- **候选 A（已选）**：ArcSwap + soft/grace/hard close。理由：reload 原子；存量 stream 60s 内可完成（典型 LLM stream 60s 内结束）。
- **候选 B（拒绝）**：DashMap + 立即 hard close。理由：中断存量请求。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-tunnel-client/src/cnexus/pool.rs:1-150`（确认 supervisor 与 close 实现）。
- Read `crates/gw-proxy/src/tunnel_stack.rs:80-260`（确认 transports 类型与 reload_nodes）。
- 验证 `arc_swap` 在 workspace 内可用（`grep -rn "arc_swap" Cargo.toml crates/*/Cargo.toml`）；若无需要加。

**【如何验证 VERIFY】**

1. `cargo test -p gw-tunnel-client -- close_stops_supervisor` 通过。
2. `cargo test -p gw-proxy -- reload_nodes_swap_atomic` 通过。
3. 集成：node rotate 1000 次 → `gw_tunnel_supervisor_alive_total` 等于当前 nodes 数（无累积泄漏）。
4. 集成 grace：active stream 在 reload 时不中断（写测请求 35s，reload 在第 5s 触发，请求仍 200）。
5. 输出：`logs/W2-T17-verify.txt`。

**【依赖】** 阻塞于 T11；并行于 T16/T18-T20。

---

### TASK W2-T18：Tunnel-4 — reader exit broadcast Close（drain Vec → drop lock → 再 await）

**【是什么 What it IS】**
- 修改 `crates/gw-tunnel-client/src/cnexus/transport.rs:344-410` reader 退出路径：lock routes → drain 到 `Vec<Sender>` → drop lock → 对每个 sender `send(TunnelEvent::Close).await`。

**【不是什么 What it is NOT】**
- ❌ 不改 alive flag 行为。
- ❌ 不引入 broadcast::Sender（保持 mpsc）。

**【为什么 Why this exists / why bug occurs】**
- 根因（Tunnel-4）：reader 退出只 `alive.store(false, ...)`；routes 不广播 Close → `TunnelStreamReader::poll_read` 永远 park（line 233-262 仅在收到 Close/None 时返回）。

**【做什么 DO】**
1. reader 退出尾部：
   ```rust
   let senders: Vec<_> = {
       let mut routes = routes.lock().await;
       let v: Vec<_> = routes.values().cloned().collect();
       routes.clear();
       v
   };
   for s in senders {
       let _ = s.send(TunnelEvent::Close).await;
   }
   ```
2. 对照 v2 REV-FINAL-W8 / -V25 风格：drain to Vec → drop lock → 再 await（避免 lock + .await 死锁）。

**【不做什么 DON'T】**
- ❌ 不持锁 await（死锁）。
- ❌ 不只 broadcast Close 而不清 routes（让 stream_id 复用安全）。

**【怎么做 HOW (具体)】**

如上代码片段。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-tunnel-client/src/cnexus/transport.rs:344-410`（reader 退出路径）。
- Read `TunnelEvent` 定义找 `Close` variant。

**【如何验证 VERIFY】**

1. `cargo test -p gw-tunnel-client -- reader_exit_broadcasts_close` 通过。
2. 集成：模拟 reader 故障 → bridge 内 `tokio::io::copy` 立即返回 EOF（非永 park）。
3. metric：`gw_tunnel_reader_exit_routes_drained_total` ≥ 期望。
4. 输出：`logs/W2-T18-verify.txt`。

**【依赖】** 阻塞于 T11；并行于 T16/T17。

---

### TASK W2-T19：REV-FINAL-W10 + Tunnel-2 — Tunnel server JoinSet 外层 + scopeguard

**【是什么 What it IS】**
- 修改 `crates/gw-tunnel-server/src/lib.rs`：`handle_ws_conn` 维护 `tokio::task::JoinSet<()>`；spawn `relay_stream` 入 JoinSet；函数顶部 `scopeguard::defer!(joinset.abort_all())`；WS close → 外层 abort_all → 等内层。

**【不是什么 What it is NOT】**
- ❌ 不动 `relay_stream` 内部 spawn 逻辑（那是 T20 的 read_task）。
- ❌ 不引入 broadcast Close。

**【为什么 Why this exists / why bug occurs】**
- 根因（Tunnel-2）：`lib.rs:289-299` `tokio::spawn(relay_stream(...))` JoinHandle 丢弃；handle_ws_conn 返回后 relay_stream 仍持 streams Arc，rx 永 park。

**【做什么 DO】**
1. `handle_ws_conn` 顶部 `let mut joinset = tokio::task::JoinSet::new();`。
2. 改 `tokio::spawn(relay_stream(...))` 为 `joinset.spawn(relay_stream(...))`。
3. 函数底部 `scopeguard::defer!(joinset.abort_all());` 或显式 await `joinset.shutdown().await`。
4. 关键 await（CH 写、Redis 提交）包 `tokio::spawn` shielded，确保不被 abort 撕裂（参考 v1 §4 WS-2 S2.16）。

**【不做什么 DON'T】**
- ❌ 不在 abort 后立即 close TCP（让 relay_stream 自然退出，避免半写）。
- ❌ 不让 joinset 跨 WS connection 共享（每 conn 一个）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-tunnel-server/src/lib.rs handle_ws_conn (rewrite around 289-299)
async fn handle_ws_conn(...) -> anyhow::Result<()> {
    let mut joinset: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
    scopeguard::defer! {
        // best-effort：JoinSet drop 自身会 abort_all，但显式 + log 更清晰
    }
    // ... existing loop, replace tokio::spawn(relay_stream(...)) with joinset.spawn(relay_stream(...));

    // WS exits naturally
    joinset.abort_all();
    while let Some(_) = joinset.join_next().await {}
    Ok(())
}
```

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-tunnel-server/src/lib.rs:280-330`（已确认 line 291 spawn 丢 handle）。
- Read 验证 scopeguard 依赖（同 T16 / Tunnel-client 共享）。

**【如何验证 VERIFY】**

1. `cargo test -p gw-tunnel-server -- handle_ws_aborts_relay_streams` 通过。
2. 集成：1k WS connection 全 abort → `streams.lock().await.len()` == 0；TCP FD 回零。
3. 输出：`logs/W2-T19-verify.txt`。

**【依赖】** 阻塞于 T11；并行于 T16-T18, T20。

---

### TASK W2-T20：Tunnel-6 + REV-FINAL-W10 嵌套 — server `read_task` → parent select! 通知

**【是什么 What it IS】**
- 修改 `crates/gw-tunnel-server/src/lib.rs:370-403` `relay_stream` 内 `read_task`：使用内层 JoinSet 或共享 flag 通知 parent；parent `select!` 同时监听 `rx.recv()` 与 read_task 完成 / err。

**【不是什么 What it is NOT】**
- ❌ 不取代外层 JoinSet（T19）；本 task 是 nested JoinSet（read_task 之于 relay_stream）。

**【为什么 Why this exists / why bug occurs】**
- 根因（Tunnel-6 + adversarial-WS2 R3）：`read_task` 在 `send_frame` Err 时仅 break / return；parent `while let Some(bytes) = rx.recv().await` 永 park。adversarial 指出外层 JoinSet 不覆盖 nested spawn。

**【做什么 DO】**
1. `relay_stream` 内：
   ```rust
   let mut inner: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
   inner.spawn(async move { /* read_task body */ });
   let read_done = CancellationToken::new();
   // 让 read_task end → cancel read_done

   loop {
       tokio::select! {
           bytes = rx.recv() => match bytes {
               Some(b) => { /* write */ }
               None => break,
           },
           _ = read_done.cancelled() => break,
       }
   }
   inner.abort_all();
   while let Some(_) = inner.join_next().await {}
   ```
2. read_task 退出（Ok/Err）前 `read_done.cancel()`。
3. send_frame Err 时 read_task 不再 silent return — 必须发 `MsgType::Close` to client (best-effort) 再 cancel + return。

**【不做什么 DON'T】**
- ❌ 不让 read_task 与 parent 共享 mpsc::Sender 通信（用 CancellationToken 更轻）。
- ❌ 不在 read_task 内 unwrap（panic 杀 nested）。

**【怎么做 HOW (具体)】**

如上 select 片段。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-tunnel-server/src/lib.rs:335-410`（已读 280-403，确认 read_task 与 parent 流程）。
- 注意 line 386-393 send_error Err 路径仅 break 不通知 parent。

**【如何验证 VERIFY】**

1. `cargo test -p gw-tunnel-server -- read_task_failure_unblocks_parent` 通过。
2. 集成：模拟 sink-only failure → relay_stream 在 5s 内退出 0 残留。
3. 输出：`logs/W2-T20-verify.txt`。

**【依赖】** 阻塞于 T11/T19；并行于 T16-T18。

---

### TASK W2-T21：R21 — InflightLedger HSET + PEXPIRE 单 Lua

**【是什么 What it IS】**
- 修改 `crates/gw-ledger/src/inflight.rs:66-83` `register`：把两次 `hset` + `expire` 合并为单条 EVAL Lua。

**【不是什么 What it is NOT】**
- ❌ 不改 mark_settled / clear（它们只是 hset / del 单步，无 EXPIRE）。
- ❌ 不动 24h EXPIRE 语义。

**【为什么 Why this exists / why bug occurs】**
- 根因（源码确认 line 80-81）：`pool.hset(...)` 与 `pool.expire(...)` 是两次 round-trip；HSET 成功 EXPIRE 失败 → 永生 key → Redis 内存累积。

**【做什么 DO】**
1. 新增 const REGISTER_INFLIGHT_LUA：
   ```lua
   redis.call('HSET', KEYS[1], unpack(ARGV, 2))
   redis.call('PEXPIRE', KEYS[1], ARGV[1])
   return 'ok'
   ```
2. `register` 改为单条 EVAL：
   ```rust
   let script = REGISTER_INFLIGHT_LUA;
   let pexpire_ms = (EXPIRE_SECS * 1000).to_string();
   let mut args: Vec<String> = vec![pexpire_ms];
   for (k, v) in fields { args.push(k.to_string()); args.push(v); }
   let _: () = self.pool.eval(script, vec![key], args).await?;
   ```
3. 测试 fail injection：HSET ok → 模拟 EXPIRE 失败前后差异。

**【不做什么 DON'T】**
- ❌ 不用 MULTI/EXEC（Sentinel 故障切换下不原子）。
- ❌ 不 cache EvalSha（首次部署可能 NOSCRIPT，简化用 EVAL 直送）。

**【怎么做 HOW (具体)】**

如上代码。

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-ledger/src/inflight.rs:1-110`（已读，确认 line 80-81 两步）。
- 验证 fred `eval` API 签名（`grep -rn "pool.eval\|client.eval" crates/`）。

**【如何验证 VERIFY】**

1. `cargo test -p gw-ledger -- inflight_register_atomic` 通过。
2. 集成：用 Toxiproxy 在 register 之间断 Redis → 重连后 key 要么不存在要么有 TTL（无永生）。
3. metric：抽样 1k key `TTL` 返回 0 或 -1 的 ≤ 0。
4. 输出：`logs/W2-T21-verify.txt`。

**【依赖】** 阻塞于 T11；独立于其他。

---

### TASK W2-T22：R22 + REV-FINAL-W13 — stream_consumer XAUTOCLAIM reconciler + boot XGROUP 清理

**【是什么 What it IS】**
- 新文件 `crates/gw-store/src/stream_consumer_reconciler.rs`：boot 时 `XINFO GROUPS events:entity` → 老 instance_id 模式且 >24h 无活跃 → `XGROUP DESTROY`；定期 `XAUTOCLAIM` IDLE 5min 兜底。
- leader 跑 `XTRIM events:entity MAXLEN ~ N`（防爆涨）。

**【不是什么 What it is NOT】**
- ❌ 不改 stream_consumer.apply 逻辑。
- ❌ 不改 entity invalidation 语义。

**【为什么 Why this exists / why bug occurs】**
- 根因（源码 line 290-301）：xack 失败仅 warn；PEL 累积；老 pod restart 留 consumer name + PEL 永久累积；无 XAUTOCLAIM。
- v2 REV-FINAL-W13：boot 时 XGROUP DESTROY 老组 + XTRIM MAXLEN。

**【做什么 DO】**
1. `stream_consumer_reconciler.rs`：
   - boot fn：`xinfo_groups(stream)` → 解析 last-delivered-id age；>24h 无活跃 consumer → `xgroup_destroy(stream, group)`。注意当前 instance_id 不可删（self-reference protection）。
   - tick fn (30s)：`xautoclaim` IDLE 300000 → re-process（或直接 XACK 如果 entry 是 entity event，因为只要 cold-load 链生效，重处理无害）。
2. leader-election 简易：`SETNX events:trim:leader $instance_id EX 30`，hold 者每 5min `XTRIM events:entity MAXLEN ~ 100000`。
3. 启动 reconciler：`StreamConsumer::start` 内同时 spawn reconciler（用 gw-runtime::Supervisor）。

**【不做什么 DON'T】**
- ❌ 不 XTRIM MINID（除非有持久订阅者保证）。
- ❌ 不 XGROUP DESTROY 当前 instance_id 自己（自杀）。
- ❌ 不在 reconciler 内做 entity 状态机推断（只调用 stream_consumer.apply）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-store/src/stream_consumer_reconciler.rs
pub struct StreamConsumerReconciler {
    pool: RedisPool,
    self_instance_id: String,
}

impl StreamConsumerReconciler {
    pub async fn boot_cleanup(&self) -> anyhow::Result<()> {
        for stream in &["events:entity", "events:counter"] {
            let groups: Vec<HashMap<String, RedisValue>> = self.pool.next()
                .xinfo_groups(*stream).await?;
            for g in groups {
                let name = g.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if name == self.self_instance_id { continue; }
                let last_delivered_id = g.get("last-delivered-id")
                    .and_then(|v| v.as_str()).unwrap_or("0-0");
                if id_age_ms(last_delivered_id) > 86_400_000 {
                    let _: Result<(), _> = self.pool.next().xgroup_destroy(*stream, &name).await;
                    metrics::counter!("gw_stream_old_group_destroyed_total", "stream" => *stream).increment(1);
                }
            }
        }
        Ok(())
    }

    pub async fn run(&self, token: ShutdownToken) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                _ = interval.tick() => {}
            }
            for stream in &["events:entity", "events:counter"] {
                let _: anyhow::Result<()> = self.autoclaim_once(stream).await;
            }
        }
    }

    async fn autoclaim_once(&self, stream: &str) -> anyhow::Result<()> {
        // XAUTOCLAIM <stream> <group=self_instance_id> <consumer=self> <min-idle-ms=300000> 0
        // 重处理 → XACK or 重新走 apply
        // 实现细节参考 fred xautoclaim API
        Ok(())
    }
}
```

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-store/src/stream_consumer.rs:225-306`（已读，确认 xack/xreadgroup 模式）。
- Read fred 的 `XAutoclaimInterface`/`StreamsInterface` API 签名。
- `grep -rn "xinfo_groups\|xautoclaim" crates/` 确认是否已用。

**【如何验证 VERIFY】**

1. `cargo test -p gw-store -- stream_consumer_reconciler_destroys_old_groups` 通过。
2. 集成 chaos：模拟 10 pod 启停 → 24h 后老 group 全部清除（XINFO GROUPS 仅剩 live ones）。
3. 集成：xack 失败注入 → 5min 后 XAUTOCLAIM 兜底 → PEL 回零。
4. 输出：`logs/W2-T22-verify.txt`。

**【依赖】** 阻塞于 T11；独立于其他。

---

### TASK W2-T23：R23 + REV-FINAL-W12 — QuotaStateWriter outbox + 状态机

**【是什么 What it IS】**
- 修改 `crates/gw-store/src/quota_state.rs`：PG commit 后**不直接** Redis DEL + XADD；改为同 PG tx 写 `quota_state_outbox` 行；专门的 outbox processor 处理 Redis DEL + XADD `events:entity`；状态机 `pending → emitted_xadd → emitted_del → committed`。

**【不是什么 What it is NOT】**
- ❌ 不动 PG vendor_keys UPDATE 语义。
- ❌ 不删除 QuotaStateUpdateQueue 内 schedule 去重。

**【为什么 Why this exists / why bug occurs】**
- 根因（源码确认 line 32-47）：两个 `let _: Result` 吞错；集群缓存陈旧到自然 TTL。
- v2 REV-FINAL-W12：分阶段状态机 + 重试。

**【做什么 DO】**
1. 新 migration `crates/gw-store/migrations/00XX_quota_state_outbox.sql`：
   ```sql
   CREATE TABLE quota_state_outbox (
     id            bigserial PRIMARY KEY,
     vendor_key_id text NOT NULL,
     payload       jsonb NOT NULL,
     status        text NOT NULL CHECK (status IN ('pending','emitted_xadd','emitted_del','committed','failed')),
     attempt       int NOT NULL DEFAULT 0,
     next_attempt_at timestamptz NOT NULL DEFAULT now(),
     created_at    timestamptz NOT NULL DEFAULT now(),
     updated_at    timestamptz NOT NULL DEFAULT now()
   );
   CREATE INDEX quota_state_outbox_due_idx ON quota_state_outbox (next_attempt_at) WHERE status IN ('pending','emitted_xadd','emitted_del');
   ```
   编号在 WS-14 内分配（草案 `0013_*`）。
2. `QuotaStateWriter::write` 改为 tx 内 UPDATE vendor_keys + INSERT quota_state_outbox（不再触 Redis）。
3. 新 worker `QuotaStateOutboxProcessor`：每 5s 扫 due rows，按状态分阶段：
   - pending → XADD（成功 → emitted_xadd）
   - emitted_xadd → DEL（成功 → emitted_del → 立即 committed）
   - 失败 → attempt++ + backoff
4. 用 `FOR UPDATE SKIP LOCKED` 防多副本竞争（参考 v2 REV-FINAL-M1）。
5. 启动 processor 用 gw-runtime::Supervisor。

**【不做什么 DON'T】**
- ❌ 不让 outbox row 持久化 Redis 失败超过 5 次（>5 → status=failed + 告警）。
- ❌ 不在主 hot path 等 outbox commit（仅 schedule）。
- ❌ 不删除 dashset 去重（schedule 层仍 dedup）。

**【怎么做 HOW (具体)】**

```rust
// crates/gw-store/src/quota_state.rs (rewrite of write)
impl QuotaStateWriter {
    pub async fn write(&self, key_id: &str, state: &QuotaState) -> anyhow::Result<()> {
        let mut tx = self.pg.begin().await?;
        sqlx::query(update_sql())
            .bind(key_id)
            // ... existing binds
            .execute(&mut *tx).await?;
        let payload = serde_json::to_value(state)?;
        sqlx::query("INSERT INTO quota_state_outbox (vendor_key_id, payload, status) VALUES ($1, $2, 'pending')")
            .bind(key_id).bind(payload).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

// crates/gw-store/src/quota_state_outbox_processor.rs (new)
pub struct QuotaStateOutboxProcessor { pg: PgPool, redis: RedisCtx }

impl QuotaStateOutboxProcessor {
    pub async fn run(&self, token: ShutdownToken) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = token.cancelled() => return Ok(()),
                _ = interval.tick() => {}
            }
            let _ = self.process_batch().await;
        }
    }

    async fn process_batch(&self) -> anyhow::Result<()> {
        let mut tx = self.pg.begin().await?;
        let rows: Vec<(i64, String, serde_json::Value, String, i32)> = sqlx::query_as(
            "SELECT id, vendor_key_id, payload, status, attempt
             FROM quota_state_outbox
             WHERE status IN ('pending','emitted_xadd','emitted_del')
               AND next_attempt_at <= now()
             ORDER BY id ASC
             LIMIT 100
             FOR UPDATE SKIP LOCKED"
        ).fetch_all(&mut *tx).await?;
        for (id, kid, _payload, status, attempt) in rows {
            match status.as_str() {
                "pending" => { /* XADD events:entity, then UPDATE status=emitted_xadd */ }
                "emitted_xadd" => { /* DEL entity:vendor_key:<kid>, then UPDATE status=committed */ }
                _ => {}
            }
        }
        tx.commit().await?;
        Ok(())
    }
}
```

**【源码验证 SOURCE-CHECK】**
- Read `crates/gw-store/src/quota_state.rs:1-110`（已读 line 21-50）。
- Read `crates/gw-store/migrations/`（找编号风格 — 已知 `0001..0011`，本 task migration 编号在 WS-14 内分配，草案 `0013_quota_state_outbox.sql`）。
- 与 v2 §4 WS-14 编号表对齐。

**【如何验证 VERIFY】**

1. `cargo test -p gw-store -- quota_state_outbox_state_machine -- --ignored`（需 PG/Redis）。
2. 集成 chaos：Redis down 30s 期间 100 个 write → 恢复后所有 outbox rows 最终 committed；vendor_keys 表的 quota_state 与 Redis cache 一致。
3. metric：`gw_quota_state_outbox_stuck_total{status}` ≤ 阈值。
4. 输出：`logs/W2-T23-verify.txt`。

**【依赖】** 阻塞于 T11；阻塞于 WS-14 编号分配；并行于 T21/T22。

---

### TASK W2-T24：Chaos suite for WS-2（CH down / SIGKILL / Sentinel partition / Tunnel churn）

**【是什么 What it IS】**
- 新 chaos scenario YAML 在 `tests/e2e/chaos/`（WS-13 交付的 harness）：
  - `ws2-ch-down-30s.yaml`：CH down 30s，期望 archive sink buffered 平稳，恢复后 0 数据丢失。
  - `ws2-task-worker-sigkill.yaml`：SIGKILL 一个 task worker mid-execution，期望 reclaim 不双计费。
  - `ws2-tunnel-churn.yaml`：5k stream churn / 5min，0 permit/FD/stream 泄漏。
  - `ws2-ws-server-abort.yaml`：1k WS connection abort，相关 relay_stream 全退出。
  - `ws2-quota-state-redis-down.yaml`：Redis down 30s 期间 QuotaStateWriter 100 write → 恢复后所有 outbox committed。

**【不是什么 What it is NOT】**
- ❌ 不实现 chaos harness 本身（WS-13）。
- ❌ 不涵盖 WS-1 / WS-3 等其他 WS 场景。

**【为什么 Why this exists / why bug occurs】**
- v1 §4 WS-2 DoD 已要求这些 chaos；v2 REV-005 进一步要求具体阈值。

**【做什么 DO】**
1. 与 WS-13 owner 协调 scenario YAML schema。
2. 写 5 个 YAML + Markdown 描述每个场景的 acceptance criteria（具体阈值见 v2 REV-005）。
3. 接入 CI gate：`make chaos-smoke` 含本 5 个。
4. 接入 24h soak：`make chaos-full` 长跑。
5. 报告输出至 `.chaos_report/<scenario>.json` PASS/FAIL 二态。

**【不做什么 DON'T】**
- ❌ 不在 chaos scenario 内做断言以外的副作用。
- ❌ 不让 PASS 仅看 metric 上下界（必须 strict）。

**【怎么做 HOW (具体)】**

```yaml
# tests/e2e/chaos/ws2-ch-down-30s.yaml
name: ws2-ch-down-30s
description: WS-2 H3/H4/H5/H13 chaos — ClickHouse down 30s, archive must not lose data
setup:
  - service: clickhouse
  - service: redis
  - service: archive-drainer
inject:
  - at: 10s
    action: stop_service
    target: clickhouse
  - at: 40s
    action: start_service
    target: clickhouse
load:
  generator: archive_event_stream
  rate: 1000/s
  duration: 90s
acceptance:
  - metric: alm_archive_lost_events_total
    constraint: "== 0"
  - metric: alm_sink_buffered_rows{name=\"archive\"}
    at: 90s
    constraint: "<= 100"
  - sql: "SELECT count() FROM usage_detailed FINAL WHERE ts >= ${START_TS}"
    constraint: "== ${INJECTED_COUNT}"
  - redis: "XPENDING usage:detailed archive_drainer"
    at: 120s
    constraint: "PEL == 0"
```

类似为其他 4 个场景写 YAML。

**【源码验证 SOURCE-CHECK】**
- Read v1/v2 §4 WS-2 DoD 与 v2 REV-005 阈值定义。
- Read `tests/e2e/` 现有 e2e harness 结构（如已存在）。

**【如何验证 VERIFY】**

1. `make chaos-smoke` 在 dev 环境 PASS。
2. 每个 scenario 的 `.chaos_report/<name>.json` `result: "PASS"`。
3. CI gate：合并前 `chaos-smoke` 必须 PASS。
4. 输出：`logs/W2-T24-verify.txt`。

**【依赖】** 阻塞于 WS-13 chaos harness；阻塞于 T01-T23（被测代码必须就绪）。

---

## 5. 跨 WS 共享 SOURCE-CHECK 清单（执行任何 task 前先核对）

| 文件 | 核对要点 | 风险（若与本 plan 声明不符） |
|---|---|---|
| `crates/gw-task/src/scripts.rs` SETTLE_LUA L59-88 | 仍是 v1 描述的"无 cur 检查 + 双 INCRBY" | 已 CONFIRMED 2026-06-17；执行 T13 前再 read 一次 |
| `crates/gw-ledger/src/inflight.rs` register L66-83 | 仍是两次 hset/expire | CONFIRMED；T21 前再 read |
| `crates/gw-store/src/quota_state.rs` write L21-49 | 仍是两个 let _: Result | CONFIRMED；T23 前再 read |
| `crates/gw-store/src/stream_consumer.rs` L287-303 | xack 失败仅 warn；无 XAUTOCLAIM | CONFIRMED；T22 前再 read |
| `crates/gw-task-runtime/src/exec_loop.rs` L97-103 | ExecLoop 仍无 registry 字段 | CONFIRMED；T12 前再 read |
| `crates/gw-tunnel-client/src/cnexus/transport.rs` L160-203 | Writer 仍无 Drop；显式 close 仍在 L192 | CONFIRMED；T16 前再 read |
| `crates/gw-tunnel-server/src/lib.rs` L289-299, L370-403 | spawn JoinHandle 仍丢；read_task 仍 silent return | CONFIRMED；T19/T20 前再 read |
| `binaries/web/main.go` L67/74/79 | 仍 3 个裸 go func 无 wg.Wait | CONFIRMED；T09 前再 read |
| `shared/platform/store/sink.go` L55-88 | OnInsertError nil 守卫 + buf[:0] + default 退出 | CONFIRMED；T03/T07 前再 read |
| `modules/archive/business/drainer.go` L65-110 | Push 后即时 XAck | CONFIRMED；T04 前再 read |
| `modules/billing/business/inflight_reaper.go` L126-134 | XAdd 后 Del | CONFIRMED；T08 前再 read |
| `crates/gw-runtime/src/supervisor.rs` L58 + managed_loop.rs L35 | 两个 Supervisor 并存 | CONFIRMED；T11 前先 read 全文 |

---

## 6. 与其他 WS 的协调点

| 边界 | 协调 |
|---|---|
| WS-1 attempt_index dedup key | T13 SETTLE_LUA 字段 `attempt_index` 必须与 Go `dedup.go`（WS-1 T22）一致 |
| WS-3 BoundedBuf trait | T03 sink overflow policy 可复用同 trait（避免双套） |
| WS-4 supervisor lint | T02 lint 与 WS-4 lint **同脚本**（不要双套） |
| WS-13 chaos harness | T24 依赖；本 plan 仅写 scenario YAML，harness 在 WS-13 |
| WS-14 migration 编号 | T23 migration 编号必须在 WS-14 集中分配（草案 0013_quota_state_outbox.sql） |

---

## 7. DoD（合并 v1 §4 WS-2 + v2 REV-005 具体阈值）

- Chaos：T24 全部 PASS。
- 强阈值：
  - `gw_tunnel_active_streams` 在 5k churn 5min 后回零（±5）；FD 在 `/proc/$pid/fd` 也回零。
  - `XPENDING usage:detailed archive_drainer IDLE 300000` 在 chaos 恢复后 60s 内回零。
  - `gw_supervisor_unhealthy{task=*}` 24h soak 内全 0；任一非零 → /readyz 503（REV-FINAL-V11）。
  - `tasks:queue` PEL size 24h soak 后 < 100（reconciler 兜底）。
  - `gw_task_double_executed_total` 全周期 == 0。
- Lint：T02 + WS-4 lint 全 PASS；裸 `go func` / `tokio::spawn` 在 binaries/modules/crates 内 0 计。
- 第二轮对抗复审（meta §1.2 协议）：派 sonnet 复审本 plan 落地后的实际 diff；0 critical critique。
- 所有 INVESTIGATE（§3）闭合或归档。

---

## 8. 任务依赖图（gantt 风格）

```
D1: T01 (Supervised framework Go) ─┐
                                   ├─→ T02 lint ─→ T09/T10 binaries
                                   ├─→ T03 sink Onerr ─→ T04 last-flush ─→ T05 PEL reconciler ─→ T06 monitoring
                                   │                                                        └─→ T07 shutdown ─┘
                                   └─→ T08 reaper Lua (independent)
D1: T11 Rust supervisor audit ─┬─→ T12 ExecLoop registry
                               ├─→ T13 SETTLE_LUA + XACK reconciler
                               ├─→ T14 EXTEND_LEASE_LUA
                               ├─→ T15 cancel drain (after T12)
                               ├─→ T16 tunnel client writer
                               ├─→ T17 reconnecting transport + arc_swap (after T16)
                               ├─→ T18 reader exit broadcast
                               ├─→ T19 tunnel server JoinSet ─→ T20 read_task select
                               ├─→ T21 InflightLedger Lua
                               ├─→ T22 stream_consumer reconciler
                               └─→ T23 QuotaStateWriter outbox (WS-14 migration 编号阻塞)

D5: T24 chaos scenarios（WS-13 harness 就绪后）
```

里程碑：
- D1：T01/T02/T03/T08/T11 落地。
- D3：T04-T07/T09-T10、T12-T14、T16/T19、T21 落地。
- D5：T15/T17/T18/T20/T22/T23 落地。
- D6：adversarial 复审 #1。
- D7-D8：T24 chaos suite 接入。
- D9：24h soak + verify。

---

## 9. 任务计数 + ID → finding 映射快查

24 个 task 覆盖：
- v1 §5 WS-2 全部 11 个 finding：H3 (T03+T05), H4 (T04+T05), H5 (T06), H13 (T07), G8 (T08), S2 (T09), Task-Runtime-1 (T12), Task-Runtime-2 (T13), Tunnel-1 (T16), Tunnel-2 (T19), Tunnel-3 (T17), Tunnel-4 (T18), Tunnel-5 (T17), Tunnel-6 (T20), R21 (T21), R22 (T22), R23 (T23)。
- v2 §4 WS-2 全部 13 个 REV-FINAL-W*：W1 (T01+T02), W2 (T04), W3 (T05), W4 (T07+T09+T10), W5 (T14), W6 (T13), W7 (T15), W8 (T16), W9 (T17), W10 (T19+T20), W11 (T11), W12 (T23), W13 (T22)。
- 跨 WS 协调：V8/V9/V10/V11/V12 与 WS-4 共享（T01/T02/T09/T10）；REV-005 阈值在 DoD（§7）落地；REV-007e 工时调整 8-10 人日已纳入。

---

— WS-2 sub-plan 结束 —
