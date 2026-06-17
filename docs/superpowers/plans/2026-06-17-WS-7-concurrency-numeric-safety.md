# WS-7 并发与数值安全 — TDD 子计划

> **状态**：草案 · 双仓 mirror（本文件与 `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-7-concurrency-numeric-safety.md` 必须字节一致除标题）
> **起草**：2026-06-17
> **上级计划**：`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-7 + `2026-06-17-prelaunch-eradication-master-plan-v2.md` §4 WS-7 REV-FINAL-H7.1..H7.8
> **覆盖**：H11、H12、S6、Billing-7、Billing-9、R10、R11、R12 + REV-FINAL-H7.1..H7.8
> **审计来源原文**：H11/H12 in `docs/issues/2026-06-17-p0-audit-report.md`（Go），Billing-7/9 + R10/R11/R12 in `docs/issues/2026-06-17-p0-bug-audit-findings.md`（Rust），S6 in master plan §2.2

---

## 0. 阅读须知（给执行者）

**强假定 — 必读**：

1. 你不必熟悉 Rust 或 Go。每个 task 用 8 段格式：是什么 / 不是什么 / 为什么 / 做什么 / 不做什么 / 怎么做 / 如何验证 / 备选 + ADR。
2. **本计划里的 sonnet 推断都是假设，不是结论**。开始前用 Read 打开 cited file:line 核对字面代码。已经核过的关键纠偏写在每个 task §3，**不要再被原文档措辞带偏**。
3. v2 §0 给出 7 段格式；本文件额外加第 8 段 "备选方案 + ADR" 以满足 v2 §0 "v2 修复方案均为候选" 要求。
4. **8 个 task** 顺序无强依赖（除 T2 + T3 共享 trusted_proxies 配置项），可并行；建议 1-2 人 3-4 天完成。
5. **测试硬约束**：Go 侧 `go test -race ./...` 必须 0 数据竞争；Rust 侧 `cargo test --workspace` 必须通过；新加测试不允许只 happy-path，必须覆盖 §6 验证表格内的边界。

**已核实的源码事实**（如与原审计描述冲突，以此为准）：

| 编号 | 文件 + 行 | 已核实状态 |
|---|---|---|
| H11 | `shared/platform/cache/cache.go:97-110` | ✓ loader 用外层 `ctx`；`v.(T)` 强转 panic；singleflight key 无 type tag |
| H12 + S6（同一组） | `shared/runtime/host.go:36-43, 111-149` | ✓ 用 chi `middleware.RealIP`；`allow()` 内行内 scan（lines 130-136）；无 trusted_proxies 配置 |
| Billing-7 | `crates/gw-ledger/src/freeze.rs:66-98` | ✓ `RequestFreeze.freeze_amount: u64`（line 68）；`new(amount: u64)` 全程 `as i64` 裸转（lines 89, 123, 124, 136, 137）；**原审计描述 `RequestFreeze::new` 用 try_from 的措辞与源码不符**——源码当前根本没有 try_from |
| Billing-9 | `crates/gw-billing/src/formulas.rs:71-84` | ✓ `scaled_micro` f64 算术（lines 81-83），末尾 `clamp(0.0, i64::MAX as f64) as i64`；中间过程未用 i128 |
| R10 | `crates/gw-health/src/store.rs:237-258` + `format_health.rs:157-187` | ✓ `retry_after_secs: Option<u32>` 直接落到 `CooldownDirective.ttl_secs: u32`；`apply_cooldown` 内 `now_ms() + (d.ttl_secs as u64) * 1000`；无 clamp。`u32::MAX` ≈ 4.29×10^9 秒 ≈ 136 年 |
| R11 | `format_health.rs:95-152` | ✓ 三分支 `if total>=20 && rate>0.5 { ... } else if ok && HalfOpen { close } else if !ok && HalfOpen { re-arm Open }`；**问题真在 else-if 互斥**：HalfOpen 时若成功且当时 window 仍 >50% rate（因为 OPEN_HOLD_MS 期间 window 没被清），第一分支 TRIP 命中，第二分支 close 永远走不到；半开探针永远没法把 circuit 拉回 closed |
| R12 | `crates/gw-routing/src/strategy/weighted_rr.rs:64-95` | ✓ Phase 1 `for k in c { ... fetch_add(weight[i]) }` 对**所有**候选累加，无 availability filter；Phase 2 `is_available` 才过滤；恢复后被沉积的 current 值"爆冲" |
| S6 / 类型安全 | `cache.go:97-110` | ✓ 同 H11；singleflight key 无 type 维度；`v.(T)` panic |
| REV-FINAL-H7.7 | `weighted_rr.rs` + `gw-health/store.rs` | 选项 A 需在 `KeyHealthStore` 内对 unavailable 转变事件挂 hook，回调 `WeightedRr` 把 `current[key]=0`。当前 `KeyHealthStore` 无 observer 注册机制——本 task 需要新增 |

---

## 1. Task 总览

| # | 编号 | 单句 | 文件 | 估时 | 阻塞 |
|---|---|---|---|---|---|
| T1 | H11 + REV-FINAL-H7.1 | `cache.GetOrLoadSF` loader 用 `WithoutCancel + WithTimeout(2s)` | `shared/platform/cache/cache.go` | 0.3d | — |
| T2 | H12 + REV-FINAL-H7.2 | 自实现 RealIP middleware + `trusted_proxies` CIDR + prod-fail-start | `shared/runtime/{host,realip}.go` + `config/runtime.go` | 0.5d | — |
| T3 | H12 + REV-FINAL-H7.3 | rate limiter cleanup goroutine（read-lock collect + write-lock delete） | `shared/runtime/{host,ratelimit}.go` | 0.5d | T2 共享 client-ip helper |
| T4 | S6 + REV-FINAL-H7.4 | singleflight key 加 type tag + `v.(T)` 改返回 err | `shared/platform/cache/cache.go` | 0.3d | T1 同文件 → 合 PR |
| T5 | Billing-7 + REV-FINAL-H7.8 | `RequestFreeze.freeze_amount: u64 → i64` + 构造期 `i64::try_from(amount)?` 全路径含 release | `crates/gw-ledger/src/freeze.rs` + 上游 caller | 0.5d | — |
| T6 | Billing-9 + REV-FINAL-H7.5 | `vendor_factor` 量化为 u32 micro-units (×1e6)，全 i128 算术，末除并 clamp i64 | `crates/gw-billing/src/formulas.rs` | 0.5d | — |
| T7 | R10 + REV-FINAL-H7.6 | retry_after_secs clamp [1, 3600]；vendor 反复 >1h 升级到 Open(vendor_full) | `crates/gw-health/src/store.rs` | 0.3d | — |
| T8 | R11 | FormatHealth.record 分支拆开：trip 与 half-open 转换互不竞争 | `crates/gw-health/src/format_health.rs` | 0.3d | — |
| T9 | R12 + REV-FINAL-H7.7 | KeyHealthStore 加 transition observer；WeightedRr 注册回调把 unavailable key 的 current 清零 | `crates/gw-routing/src/strategy/weighted_rr.rs` + `crates/gw-health/src/store.rs` | 0.7d | — |

**合计**：~3.9 人日。建议 Track E（Go infra）拿 T1-T4，Track C 或 D（Rust）拿 T5-T9。

---

## 2. TASK T1：cache.GetOrLoadSF loader 用 `WithoutCancel + WithTimeout(2s)`

### 2.1 是什么 What it IS
- 一个 Go 代码改动，文件 **绝对路径** `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/platform/cache/cache.go`，函数 `GetOrLoadSF[T any](...)`（约第 76-111 行）。
- 在调用 caller 提供的 `load` 闭包之前，把 ctx 包成 `context.WithoutCancel(ctx)` 再 `context.WithTimeout(..., 2*time.Second)`，传给 `load`。
- 同时记录两个新 metric：`gw_cache_singleflight_orphan_total{key_prefix}`（leader 取消后 follower 仍出错过几次）与 `gw_cache_singleflight_timeout_total{key_prefix}`。

### 2.2 不是什么 What it is NOT
- ❌ 不修改 `GetOrLoad`（非 SF 版本）。它没 singleflight 套桶，每 caller 自己的 ctx 没有"杀其他 follower"问题。
- ❌ 不改 singleflight key 计算（key 改造是 T4 的事）。
- ❌ 不把 `c.rdb.Set` 也包到新 ctx 内——写回 Redis 本来就 `_ = ... .Err()` 吞了，无破坏点。
- ❌ 不引入新 import 之外的第三方包（`context.WithoutCancel` 是 Go 1.21+ std）。

### 2.3 为什么 Why this bug exists
- **根因**：singleflight `Do` 用 leader caller 的 ctx 执行 `load`。leader 客户端 HTTP 断开 → ctx 取消 → DB 查询返回 `context.Canceled` → **同时**等在桶里的 follower 都拿到 `context.Canceled` 错误，集体失败（即使 follower 自己 ctx 还活着）。
- **引用**：原审计 H11；master plan §4 WS-7 S7.1；v2 REV-FINAL-H7.1。
- **后果**：在 1k 并发同 key 场景下，leader 偶发掉线 → 999 个无辜请求拿 ErrContext。运行半年没事，第一次大客户长轮询断连 → 雪崩。
- **根本治理**：把 leader ctx 与 load 解耦。`WithoutCancel`（Go 1.21+）保留 trace/value 不再传播 cancel；`WithTimeout(2s)` 防 leaked goroutine 永驻。

### 2.4 做什么 DO
1. 在 `GetOrLoadSF` 内，singleflight `c.group.Do(key, fn)` 的 `fn` 闭包顶部：构造 `loaderCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 2*time.Second)` 并 `defer cancel()`。
2. 把闭包内 `load(ctx)` 改为 `load(loaderCtx)`。
3. 把闭包内 `c.rdb.Set(ctx, key, b, ttl)` 改为 `c.rdb.Set(loaderCtx, key, b, ttl)`——同一新 ctx，timeout 一致。
4. loader 返回 err 且 `errors.Is(err, context.DeadlineExceeded)` → `gw_cache_singleflight_timeout_total.WithLabelValues(prefix).Inc()`。
5. follower 路径（`v, err, _ := c.group.Do(...)` 后 err != nil）若 `errors.Is(err, context.Canceled)` → `gw_cache_singleflight_orphan_total.WithLabelValues(prefix).Inc()`。
6. 在 PR 描述链回 H11 + REV-FINAL-H7.1。

### 2.5 不做什么 DON'T
- ❌ 不要把 timeout 设成可配置（YAGNI；2s 是经验值，遇 SLO 不达再调）。
- ❌ 不要直接用 `context.Background()` 代替 `WithoutCancel`——会丢 trace span 与 deadline-attribution。
- ❌ 不要把 `defer cancel()` 漏掉——少一行就 leak。
- ❌ 不要在 `load` 错误路径里**吞** err（必须返回原 err，metric 只是观测）。
- ❌ 不要用 `select { case <-ctx.Done(): }` 自实现取消——Go std 已经搞定。

### 2.6 怎么做 HOW (具体)

```go
// shared/platform/cache/cache.go (replace lines ~97-110)
var (
    gwCacheSFTimeoutTotal = promauto.NewCounterVec(prometheus.CounterOpts{
        Name: "gw_cache_singleflight_timeout_total",
        Help: "singleflight loader exceeded 2s deadline.",
    }, []string{"key_prefix"})
    gwCacheSFOrphanTotal = promauto.NewCounterVec(prometheus.CounterOpts{
        Name: "gw_cache_singleflight_orphan_total",
        Help: "follower observed leader ctx cancellation.",
    }, []string{"key_prefix"})
)

v, err, _ := c.group.Do(key, func() (any, error) {
    loaderCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 2*time.Second)
    defer cancel()

    val, lErr := load(loaderCtx)
    if lErr != nil {
        if errors.Is(lErr, context.DeadlineExceeded) {
            gwCacheSFTimeoutTotal.WithLabelValues(prefix).Inc()
        }
        return zero, lErr
    }
    if b, mErr := json.Marshal(val); mErr == nil {
        _ = c.rdb.Set(loaderCtx, key, b, ttl).Err()
    }
    return val, nil
})
if err != nil {
    if errors.Is(err, context.Canceled) {
        gwCacheSFOrphanTotal.WithLabelValues(prefix).Inc()
    }
    return zero, err
}
return v.(T), nil  // 注意：v.(T) 强转留待 T4 修复
```

### 2.7 如何验证 VERIFY
1. 单元 race：`go test -race ./shared/platform/cache/... -run TestGetOrLoadSF_LeaderCancel -count=20`
   - 测试设计：1000 goroutine 同 key，leader（goroutine #0）`time.AfterFunc(10ms, cancel)` 取消自身 ctx；loader 内 `time.Sleep(30ms)` 模拟 DB 调用；断言其他 999 个 follower 全 `err == nil` 且拿到同一 value。
   - 期望：`PASS` 全 20 次。
2. 单元 timeout：`TestGetOrLoadSF_LoaderTimeout`：loader sleep 3s；首 caller 应在 ~2s 拿到 `context.DeadlineExceeded`；metric `gw_cache_singleflight_timeout_total{key_prefix=...}` ≥ 1。
3. 集成：`go test -race ./shared/platform/cache/... -run TestGetOrLoadSF_RedisDown` 已有；不应被打破。
4. e2e（如已存在）：`make e2e` 通过。

### 2.8 备选方案 + ADR
- **B**：每 caller 独立 `WithTimeout`，singleflight 取消仅由"所有 follower 都没了"时触发。需自写 ref-count 的 singleflight；复杂。
- **C**：换 hashicorp/`groupcache` 之类带 `DoChan` 的库，让 follower 选 `select { case <-chan, case <-myCtx }` 自决。引入新依赖且接口不兼容。
- **D**：保持现状，加 `panic recover`。完全不解决根因。
- **选 A（WithoutCancel + WithTimeout）的理由**：std 库；3 行修改；无新依赖；timeout 兜底 leak；与 GO 1.21+ 习语对齐。

---

## 3. TASK T2：自实现 RealIP middleware + `trusted_proxies` CIDR + 生产环境无配置即拒启

### 3.1 是什么 What it IS
- 一个 Go 代码改动，新文件 `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/realip.go`，注册到 `shared/runtime/host.go` 的 `BuildRouter` 内，**替换** `middleware.RealIP`（host.go:38）。
- 新增配置字段（在哪定义见 §3.6）：`runtime.TrustedProxies []string`（CIDR 列表）与 `runtime.TrustXff bool`。
- 启动期校验：若环境变量 `GW_ENV=production` 且 `TrustXff=true` 且 `TrustedProxies=[]` → **panic 拒启**（boot fail-closed）。

### 3.2 不是什么 What it is NOT
- ❌ 不删除 chi 框架——chi 仍是 router；只是不再用其 `middleware.RealIP`。
- ❌ 不改 `clientIP(r)` helper 本身（host.go:151-160）——它读 `r.RemoteAddr`，我们让 RealIP middleware 在它之前把 `r.RemoteAddr` 改写到信任过的 XFF 值，使下游兼容。
- ❌ 不在本 task 内改 rate limiter——那是 T3 的事，本 task 只暴露 helper。
- ❌ 不在 Rust 仓库改任何代码——v1 提到 "H12 / R4 共用配置"指的是**语义**共用（Rust 单独的 `trusted_proxies` 配置项归 WS-5 REV-FINAL-V22）。本 task 只管 Go。

### 3.3 为什么 Why this bug exists
- **根因**：chi `middleware.RealIP` 无论 peer 是不是反代，都信任 `X-Forwarded-For` / `X-Real-IP`。任何客户端伪造 `X-Forwarded-For: 1.2.3.4` 都能让上游观测到的"客户端 IP" 完全失真。直接后果：rate limiter / auth-failure limiter / IP 黑白名单全部以伪造 IP 计算。
- **引用**：原审计 H12；master plan §4 WS-7 S7.2、WS-5 S5.15（同一配置）；v2 REV-FINAL-H7.2、REV-FINAL-V22。
- **后果**：DoS 攻击者刷 `X-Forwarded-For: $RANDOM` → 每请求换一个 key → 100% 越过限速；爆破 `X-Forwarded-For: $TARGET_USER_IP` → 把无辜用户打到 429。
- **根本治理**：仅当 `peer_addr ∈ trusted_proxies CIDR` 才解析 XFF；否则用 peer_addr。

### 3.4 做什么 DO
1. 新文件 `shared/runtime/realip.go`，实现 `RealIPFromTrusted(trusted []*net.IPNet, trustXff bool) func(http.Handler) http.Handler` middleware 构造器。
2. middleware 行为：若 `trustXff=false` → 直接 `next.ServeHTTP`，不动 `r.RemoteAddr`。若 `trustXff=true`：解析 `r.RemoteAddr` 得 peer IP；判断 peer IP 是否 ∈ 任一 `trusted` CIDR；若是 → 解析 XFF 头**最右侧**（最接近 peer）开始向左依次跳过 trusted，遇到第一个非 trusted IP，把它写入 `r.RemoteAddr`；若 peer 不可信 → 不动 `r.RemoteAddr`。
3. 新增 helper `realClientIP(r)` 返回 just-the-host 部分；本 task 内不替换 `clientIP`，仅 export 一个等价的 `RealClientIP(r) string` 给 T3 用。
4. `shared/runtime/host.go` 里 `BuildRouter` 入口接收 `RealIPConfig{TrustXff bool, Trusted []*net.IPNet}`（通过 `Deps` 扩展或 `BuildRouter` 多参——见 §3.6）。
5. `cmd/.../main.go` 启动期（具体路径见 §3.6 step 4）：从 env / config 解析 `GW_TRUSTED_PROXIES`（逗号分隔 CIDR 列表）与 `GW_TRUST_XFF`（bool）；在 `BuildRouter` 之前校验：`os.Getenv("GW_ENV") == "production" && trustXff && len(trusted) == 0` → `log.Fatalf("trust_xff=true requires trusted_proxies in production")`。

### 3.5 不做什么 DON'T
- ❌ 不要解析 `X-Real-IP`——XFF 已足够，多一个头多一个 oracle。
- ❌ 不要"信任最左侧"——XFF 从左到右是 client → proxy1 → proxy2，最左是**客户端自己写的**，最右才是被反代追加的。从右往左走才安全。
- ❌ 不要把 CIDR 解析放进 middleware 热路径——预解析一次，存 `[]*net.IPNet`。
- ❌ 不要让生产环境 "warn-only"——这是安全配置，启动失败比静默放行好。
- ❌ 不要尝试支持 X-Forwarded-Host / X-Forwarded-Proto——超出本 task 范围。
- ❌ 不要修改 `chi.NewRouter()` 之外的 middleware 顺序，只是把 `middleware.RealIP` 这一行替换。

### 3.6 怎么做 HOW (具体)

`shared/runtime/realip.go`（新文件）：

```go
package runtime

import (
    "net"
    "net/http"
    "strings"
)

// RealIPFromTrusted returns a middleware that, when trustXff is true,
// rewrites r.RemoteAddr from X-Forwarded-For only if the immediate peer
// is in one of the trusted CIDRs. Right-to-left walk skips trusted hops
// and picks the first untrusted address as the canonical client IP.
//
// trustXff=false makes the middleware a no-op (recommended default).
func RealIPFromTrusted(trusted []*net.IPNet, trustXff bool) func(http.Handler) http.Handler {
    return func(next http.Handler) http.Handler {
        return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
            if !trustXff || len(trusted) == 0 {
                next.ServeHTTP(w, r)
                return
            }
            peer := hostOnly(r.RemoteAddr)
            peerIP := net.ParseIP(peer)
            if peerIP == nil || !cidrContains(trusted, peerIP) {
                next.ServeHTTP(w, r)
                return
            }
            xff := r.Header.Get("X-Forwarded-For")
            if xff == "" {
                next.ServeHTTP(w, r)
                return
            }
            parts := strings.Split(xff, ",")
            // Walk right-to-left, skip trusted hops, return first untrusted.
            for i := len(parts) - 1; i >= 0; i-- {
                cand := strings.TrimSpace(parts[i])
                ip := net.ParseIP(cand)
                if ip == nil {
                    continue
                }
                if cidrContains(trusted, ip) {
                    continue
                }
                // Preserve port shape so downstream parsers don't choke.
                r.RemoteAddr = net.JoinHostPort(cand, "0")
                break
            }
            next.ServeHTTP(w, r)
        })
    }
}

func cidrContains(nets []*net.IPNet, ip net.IP) bool {
    for _, n := range nets {
        if n.Contains(ip) {
            return true
        }
    }
    return false
}

func hostOnly(addr string) string {
    if h, _, err := net.SplitHostPort(addr); err == nil {
        return h
    }
    return addr
}

// ParseTrustedCIDRs parses a comma-separated CIDR list.
// Returns the parsed nets; the bootstrap caller is responsible for
// fail-closed if production-with-trust-xff yields an empty result.
func ParseTrustedCIDRs(s string) ([]*net.IPNet, error) {
    var out []*net.IPNet
    for _, raw := range strings.Split(s, ",") {
        raw = strings.TrimSpace(raw)
        if raw == "" {
            continue
        }
        _, n, err := net.ParseCIDR(raw)
        if err != nil {
            return nil, err
        }
        out = append(out, n)
    }
    return out, nil
}

// RealClientIP returns the host portion of r.RemoteAddr, after any RealIP
// middleware rewriting. Replaces the unexported clientIP helper for
// downstream code (rate limiter, audit log).
func RealClientIP(r *http.Request) string {
    if h, _, err := net.SplitHostPort(r.RemoteAddr); err == nil {
        return h
    }
    if r.RemoteAddr == "" {
        return "unknown"
    }
    return r.RemoteAddr
}
```

`shared/runtime/host.go` 的 `BuildRouter` 修改（具体位置：约第 30-43 行）：

```go
type RealIPConfig struct {
    TrustXff       bool
    TrustedProxies []*net.IPNet
}

// BuildRouter signature additions: take a RealIPConfig param.
// Either widen Deps or take it as an explicit arg. For minimal blast
// radius, widen Deps:
type Deps struct {
    Log        *slog.Logger
    // ... existing fields ...
    RealIP     RealIPConfig
}

func BuildRouter(d Deps, modules []Module) chi.Router {
    r := chi.NewRouter()
    plog.SetDefault(d.Log)
    rateLimiter := newAPIRateLimiter(DefaultAPIRateLimitRequests, defaultAPIRateLimitWindow)
    r.Use(
        middleware.RequestID,
        RealIPFromTrusted(d.RealIP.TrustedProxies, d.RealIP.TrustXff),  // REPLACED middleware.RealIP
        securityHeaders,
        limitRequestBody(DefaultMaxRequestBodyBytes),
        rateLimiter.Middleware,
        Observe(d.Log),
    )
    // ... rest unchanged ...
}
```

启动入口（每个 `binaries/<name>/main.go` 都需要此片段；具体文件由 §6 VERIFY step 4 列出）：

```go
trustXff := strings.EqualFold(os.Getenv("GW_TRUST_XFF"), "true")
trustedCIDRs, err := runtime.ParseTrustedCIDRs(os.Getenv("GW_TRUSTED_PROXIES"))
if err != nil {
    log.Fatalf("invalid GW_TRUSTED_PROXIES: %v", err)
}
if os.Getenv("GW_ENV") == "production" && trustXff && len(trustedCIDRs) == 0 {
    log.Fatalf("GW_TRUST_XFF=true requires non-empty GW_TRUSTED_PROXIES in production")
}
deps.RealIP = runtime.RealIPConfig{TrustXff: trustXff, TrustedProxies: trustedCIDRs}
```

### 3.7 如何验证 VERIFY
1. 单元：`go test -race ./shared/runtime/... -run TestRealIPFromTrusted -count=5`
   - 表驱动测试：(a) trustXff=false → 不动 RemoteAddr；(b) trustXff=true, peer 不在 CIDR → 不动；(c) peer 在 CIDR, XFF="1.2.3.4" → RemoteAddr="1.2.3.4:0"；(d) peer 在 CIDR, XFF="evil, 10.0.0.5, 10.0.0.6"，trusted=10.0.0.0/8 → 右往左跳过两 trusted 取 "evil"；(e) peer 在 CIDR, XFF 全 trusted → 不动；(f) XFF 含非法 IP → 跳过继续。
2. 单元：`TestParseTrustedCIDRs` 表驱动：空 → 空；"10.0.0.0/8,192.168.0.0/16" → 2 项；"bad" → err。
3. 启动期：`TestBootstrap_ProductionRejectsEmptyTrustedProxies`（用 testmain 或独立子进程测试）：env `GW_ENV=production GW_TRUST_XFF=true GW_TRUSTED_PROXIES=` → process exit code != 0。
4. 跨仓人工：列出**每个**调用 `runtime.BuildRouter` 的 binary：`grep -rln "runtime.BuildRouter" /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/binaries/ /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/cmd/`，对每文件加 §3.6 的 bootstrap 片段。报告：必须 0 binary 遗漏。
5. DoS chaos（与 T3 联合，可推迟到 T3 完成后）：5000 个 unique `X-Forwarded-For` 头打 `/api/...`；在 trustXff=false 配置下应被限速（peer 单 IP 计数）；在 trustXff=true + 错配 trusted=0.0.0.0/0 下被打穿——演示生产 fail-closed 配置的必要性。

### 3.8 备选方案 + ADR
- **B**：保留 chi `middleware.RealIP`，在它后面加一个 "revert if peer untrusted" middleware。两层 middleware 反复改 RemoteAddr，状态难追。
- **C**：用 `github.com/sebest/xff` 三方库。引入维护变量；该库不强制 trusted 必填，仍可能被错配。
- **D**：把 trust 判断挪进 rate limiter（不动 RemoteAddr，rate limiter 自己读 XFF）。问题：rate limiter 不是唯一 IP 消费者；audit log / abuse-detection / 黑白名单都要。
- **选 A（自实现 middleware）的理由**：~60 行自实现代码可读、可测；强制 `trusted_proxies` 非空门禁；与 v2 REV-FINAL-V22 的 Rust 侧语义对齐；可被 T3 / 后续 audit-log 共用 `RealClientIP` helper。

---

## 4. TASK T3：rate limiter 拆出独立 cleanup goroutine（read-lock collect + write-lock delete）

### 4.1 是什么 What it IS
- 一个 Go 代码改动，文件 `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/host.go`，结构 `apiRateLimiter`（约 95-149 行）。
- 把 `allow()` 内的 inline cleanup（lines 130-136）**整段删除**；同时为 `apiRateLimiter` 引入 `sync.RWMutex`（替换原 `sync.Mutex`）与一个常驻 cleanup goroutine（由 `BuildRouter` 内启动，绑定 ctx）。
- cleanup goroutine：每 `window/2` 秒一轮；read-lock 扫描收集过期 key 到本地 slice；释放 read-lock；获取 write-lock 删除 key（删前二次校验仍过期，避免 race）；释放。

### 4.2 不是什么 What it is NOT
- ❌ 不改限速算法语义（仍是 fixed-window）。
- ❌ 不引入 `golang.org/x/time/rate` 或 `uber/ratelimit`——这两个都是 token-bucket，与现行实现不等价。
- ❌ 不改 `Middleware` 的入口签名——仍 `func(http.Handler) http.Handler`。
- ❌ 不在本 task 加 hard-cap（v1 §3 已要求，但属 WS-3 BoundedBuf 范畴）。
- ❌ 不复用 `RealIPFromTrusted` 的 chi middleware 顺序——但**必须**把 `clientIP(r)` 改为 `RealClientIP(r)`（T2 export）。

### 4.3 为什么 Why this bug exists
- **根因**：`allow()` 持锁同时做 cleanup。`len(l.hits) > l.limit*16` 触发后**每个请求**都重扫整张 map，O(N) 持锁。DoS 攻击下 hits 涨到 ~10k 后每请求 ~10k 删除尝试，~99% 删除是 no-op（已 fresh 的不应删）。锁持有时间从 ~ns 涨到 ~ms，吞吐崩塌。
- **引用**：原审计 H12 第二段；master plan §4 WS-7 S7.3；v2 REV-FINAL-H7.3；新发现 S5（rate limiter cap）。
- **后果**：DoS 攻击者打 5000 unique IP → hits 涨到 80k → 每请求 80k 持锁扫描 → 限速本身把限速服务搞挂。
- **根本治理**：扫描挪到后台 goroutine，allow() 路径只做 O(1) map lookup + RWMutex.RLock 释放 quick。

### 4.4 做什么 DO
1. 把 `mu sync.Mutex` 改为 `mu sync.RWMutex`。
2. `allow()` 内：**整段删除** lines 130-136 的 inline scan。
3. `allow()` 内读路径 (`state := l.hits[ip]`) 用 `l.mu.RLock()` 试读；如 `state.start.IsZero()` 或过期则升级到 `l.mu.Lock()` 写入新 `rateWindow{start: now, count: 1}` 后释放（先 RUnlock → Lock → 二次检查）；如未过期但需 `count++`，需 Lock 写。原 fast-path（已存在且未过期且未到 limit）仍是 write 路径（counter++）；不可用 RLock。
   **简化方案**：保留 `sync.Mutex` 不变，仅做单线程模型；cleanup 路径自己用 `mu.Lock`，allow 路径也用 `mu.Lock`——锁竞争不在 cleanup vs allow 之间，而在 allow vs allow 之间，原模型即如此。引入 RWMutex 反而复杂。**ADR 见 §4.8**。
4. 新增方法 `(l *apiRateLimiter) StartCleanup(ctx context.Context)`：起 goroutine，`time.Ticker(l.window / 2)`，每轮：(a) `mu.Lock()` 拷 key+state 到 `expired []string`；(b) `mu.Unlock()`；(c) `mu.Lock()` 对每 key 二次校验仍过期（防 race 期间它被刷新）→ `delete(l.hits, k)`；(d) `mu.Unlock()`；(e) 监听 ctx.Done() 退出。
5. `BuildRouter` 内：在创建 rateLimiter 后 `rateLimiter.StartCleanup(d.Ctx)`（要求 `Deps.Ctx context.Context`；若现无此字段，本 task 一并加上）。
6. `Middleware` 内的 `clientIP(r)` 改为 `RealClientIP(r)`（T2 export）。
7. 新 metric `gw_ratelimit_hits_map_size`（Gauge）：cleanup goroutine 每轮结束更新，便于观测 map 规模。

### 4.5 不做什么 DON'T
- ❌ 不要在 `allow()` 路径调用 cleanup（推回后台 goroutine）。
- ❌ 不要用 `time.AfterFunc` 一次性——必须 ticker 常驻。
- ❌ 不要忘了 ctx.Done() 退出；否则 goroutine 泄漏（CLAUDE.md 违规）。
- ❌ 不要把 cleanup 锁住一次性扫全 map——大 map 仍 spike 长锁。改 chunk 化（每轮最多扫 1000 key）。
- ❌ 不要在 cleanup 内 panic 导致 goroutine 静默死掉——用 `runtime.Supervised`（WS-4 交付）；当下无 Supervised 时可先用 `defer recover` log。
- ❌ 不要顺便加 hard-cap LRU——非本 task 范围。

### 4.6 怎么做 HOW (具体)

```go
// shared/runtime/host.go (replace apiRateLimiter type and allow/cleanup)

type apiRateLimiter struct {
    mu     sync.Mutex
    limit  int
    window time.Duration
    hits   map[string]rateWindow
}

func (l *apiRateLimiter) allow(ip string, now time.Time) bool {
    l.mu.Lock()
    defer l.mu.Unlock()

    state := l.hits[ip]
    if state.start.IsZero() || now.Sub(state.start) >= l.window {
        l.hits[ip] = rateWindow{start: now, count: 1}
        return true
    }
    if state.count >= l.limit {
        return false
    }
    state.count++
    l.hits[ip] = state
    return true
}

// StartCleanup runs until ctx is done. Sweeps expired keys without holding
// the lock for the full scan. Safe to call once at boot.
func (l *apiRateLimiter) StartCleanup(ctx context.Context) {
    go func() {
        defer func() {
            if r := recover(); r != nil {
                slog.Error("ratelimiter cleanup panic", "panic", r)
            }
        }()
        t := time.NewTicker(l.window / 2)
        defer t.Stop()
        const maxBatch = 1000
        for {
            select {
            case <-ctx.Done():
                return
            case now := <-t.C:
                // Phase 1: collect expired candidates under short lock.
                l.mu.Lock()
                expired := make([]string, 0, maxBatch)
                for k, st := range l.hits {
                    if now.Sub(st.start) >= l.window {
                        expired = append(expired, k)
                        if len(expired) >= maxBatch {
                            break
                        }
                    }
                }
                size := len(l.hits)
                l.mu.Unlock()

                gwRateLimitHitsSize.Set(float64(size))

                if len(expired) == 0 {
                    continue
                }
                // Phase 2: delete under second short lock, with re-check.
                l.mu.Lock()
                for _, k := range expired {
                    if st, ok := l.hits[k]; ok && now.Sub(st.start) >= l.window {
                        delete(l.hits, k)
                    }
                }
                l.mu.Unlock()
            }
        }
    }()
}

// Metric (declare at package init or top of file with other metrics):
var gwRateLimitHitsSize = promauto.NewGauge(prometheus.GaugeOpts{
    Name: "gw_ratelimit_hits_map_size",
    Help: "size of in-memory rate limiter hit map.",
})
```

`BuildRouter` 调整：在 `rateLimiter := newAPIRateLimiter(...)` 之后加 `rateLimiter.StartCleanup(d.Ctx)`；并把 `Middleware` 内 `clientIP(r)` 改为 `RealClientIP(r)`。

### 4.7 如何验证 VERIFY
1. 单元 race：`go test -race ./shared/runtime/... -run TestAPIRateLimiter -count=20`
   - 既有测试不应回归；新加 `TestAPIRateLimiter_CleanupRunsInBackground`：注入 `window=50ms`；fill 100 unique IP；wait 200ms；assert `len(hits) == 0`。
2. 单元：`TestAPIRateLimiter_AllowHotPathNoScan`：fill 50k unique IP；measure `allow("hot-key")` p99 latency；assert < 5µs（无 inline scan 应达）。
3. goroutine leak：`TestAPIRateLimiter_CleanupRespectsContext`：起 cleanup，ctx.Cancel()，wait 200ms，`goleak.VerifyNone(t)`。
4. 集成：`make e2e` 通过。
5. chaos（与 T2 联合验证）：5000 unique 真实 IP（peer IP 而非 spoofed XFF）打 `/api/foo` 60s；map size 在 cleanup tick 后回落；服务 p99 不抖。

### 4.8 备选方案 + ADR
- **B**：RWMutex 拆读写。允许多 reader 同时观测 map，cleanup 单写者。然而 `allow` 99% 路径需要写（counter++），RWMutex 反而 worst-case 更慢且复杂。
- **C**：换 `sync.Map`。`sync.Map` 在 read-mostly 场景才赢；本场景每请求都写，反而慢；且 `sync.Map.Range` 不持快照，cleanup 与 in-flight 写竞态难推理。
- **D**：分片 map（`[]struct{ mu Mutex; hits map }`），减少锁热点。提升吞吐但不解决 cleanup 时长问题；可作后续优化（WS-9 性能治理）。
- **选 A（保留 Mutex + 后台 cleanup + 拆两段锁）的理由**：最小改动，根因（inline scan）消除；锁持续时间从 O(N) 降到 O(1)（collect 是 O(N) 但**不再每请求触发**，且 maxBatch=1000 限制单轮持锁时间）；RWMutex/sync.Map/分片可作 WS-9 后续课题。

---

## 5. TASK T4：cache singleflight key 加 type tag + `v.(T)` 改返回 err

### 5.1 是什么 What it IS
- 一个 Go 代码改动，文件 `/Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/platform/cache/cache.go`，函数 `GetOrLoadSF`（约 76-111 行）。
- singleflight key 从 `key` 改为 `key + "|" + reflect.TypeOf(zero).String()`。
- 末尾 `return v.(T), nil` 改为 `v, ok := v.(T); if !ok { return zero, errors.New("cache: type mismatch") }`。
- 同时 export 一个 sentinel error `ErrCacheTypeMismatch` 给上游精确捕获。

### 5.2 不是什么 What it is NOT
- ❌ 不改 `key` 本身的命名约定（v1 提到 `cache:{module}:{...}` 是 advisory，本 task 不强制）。
- ❌ 不改 `GetOrLoad`（非 SF 版本）——它无 singleflight 桶，调用方各自的 `var v T` 反序列化，类型不混。
- ❌ 不为 `Set` / `Invalidate` 加 type tag——它们没 singleflight 集结。
- ❌ 不引入 `reflect` 的开销讨论——`reflect.TypeOf(zero).String()` 调一次 per call，单字符串拼接 µs 级。

### 5.3 为什么 Why this bug exists
- **根因**：singleflight 按 string key 聚合。两个调用方 `GetOrLoadSF[A](key="foo", ...)` 与 `GetOrLoadSF[B](key="foo", ...)` 会聚到同一桶。先到的 caller 的 `load` 返回 `A` 实例 → 后到 caller 用 `v.(B)` 强转 → panic。
- 即使所有 caller 都用同一 T，反序列化分支若失败（Redis 残留旧 schema bytes）会走 `load` 路径，但 singleflight 已经聚合，第一个回收 cache 写回后第二个仍 `v.(T)` panic 风险存在（少见但非零）。
- **引用**：原审计 S6；master plan §4 WS-7 S7.4；v2 REV-FINAL-H7.4。
- **后果**：单次进程 panic kill goroutine 群、释放 panic recovery 包装压力到 chi `Observe` middleware，恶劣下打挂某 endpoint。

### 5.4 做什么 DO
1. 计算 `var zero T` 之后立刻 `sfKey := key + "|" + reflect.TypeOf(zero).String()`。
2. `c.group.Do(sfKey, ...)` 用 `sfKey`。
3. 替换 `return v.(T), nil`：
   ```go
   typed, ok := v.(T)
   if !ok {
       return zero, ErrCacheTypeMismatch
   }
   return typed, nil
   ```
4. Export `var ErrCacheTypeMismatch = errors.New("cache: singleflight type mismatch")` 在文件顶部。
5. 新 metric `gw_cache_type_mismatch_total{key_prefix}`：上面 `!ok` 分支自增。

### 5.5 不做什么 DON'T
- ❌ 不要 panic——这是本 task 的核心目标，必须返回 err。
- ❌ 不要在 type tag 内编码具体 generics 信息（`reflect.TypeOf(zero).String()` 对 `*MyStruct` 返回 `"*pkg.MyStruct"` 已足够）。
- ❌ 不要在 cache key 里写人类不可读的 hash——保留 `|<T>` 后缀利于 debug。
- ❌ 不要把 T 编码进 Redis key 本身——Redis key 仍是 `key`，只在 in-process singleflight 桶名内加 type tag。
- ❌ 不要把这个改动与 T1 拆 PR——同文件同函数，合并风险更低。

### 5.6 怎么做 HOW (具体)

```go
// shared/platform/cache/cache.go
import "reflect"

var ErrCacheTypeMismatch = errors.New("cache: singleflight type mismatch")

var gwCacheTypeMismatchTotal = promauto.NewCounterVec(prometheus.CounterOpts{
    Name: "gw_cache_type_mismatch_total",
    Help: "singleflight value did not assert to caller T.",
}, []string{"key_prefix"})

func GetOrLoadSF[T any](ctx context.Context, c *Cache, key string, ttl time.Duration, load func(ctx context.Context) (T, error)) (T, error) {
    var zero T
    if c == nil || c.rdb == nil {
        return load(ctx)
    }
    prefix := keyPrefix(key)
    sfKey := key + "|" + reflect.TypeOf(zero).String()  // NEW

    raw, err := c.rdb.Get(ctx, key).Bytes()
    switch {
    case err == nil:
        var v T
        if jErr := json.Unmarshal(raw, &v); jErr == nil {
            gwCacheHitTotal.WithLabelValues(prefix).Inc()
            return v, nil
        }
    case errors.Is(err, redis.Nil):
    default:
        return load(ctx)
    }
    gwCacheMissTotal.WithLabelValues(prefix).Inc()

    v, err, _ := c.group.Do(sfKey, func() (any, error) {  // CHANGED: sfKey
        // T1 will replace ctx with loaderCtx; for review-time atomicity,
        // perform the T1 + T4 edits together in one PR.
        val, lErr := load(ctx)
        if lErr != nil {
            return zero, lErr
        }
        if b, mErr := json.Marshal(val); mErr == nil {
            _ = c.rdb.Set(ctx, key, b, ttl).Err()
        }
        return val, nil
    })
    if err != nil {
        return zero, err
    }
    typed, ok := v.(T)            // CHANGED
    if !ok {                       // CHANGED
        gwCacheTypeMismatchTotal.WithLabelValues(prefix).Inc()
        return zero, ErrCacheTypeMismatch
    }
    return typed, nil
}
```

### 5.7 如何验证 VERIFY
1. 单元：`go test -race ./shared/platform/cache/... -run TestGetOrLoadSF_TypeIsolation -count=20`
   - 测试设计：两 goroutine 同 `key="k1"` 但泛型 A 与 B；assert 各自拿到正确类型，不 panic。
2. 单元：`TestGetOrLoadSF_TypeMismatchReturnsErr`：用反射强行喂错类型到 singleflight（注入 mock cache）；assert err == ErrCacheTypeMismatch；metric ≥ 1。
3. 单元：`TestGetOrLoadSF_PanicFreeBacktoBack`：1000 goroutine 同 key 但混入 30 个不同 T；assert 0 panic（如果回归 panic，test runner 会 fail）。
4. 回归：原有 `cache_test.go` 全过。

### 5.8 备选方案 + ADR
- **B**：用 `crypto/sha256(key||T.String())` 当 sf key。哈希后 key 不可读，debug 难度上升；无功能收益。
- **C**：把 generic T 移到接口层用 type assertion 链。重设计 API；超出 task 范围。
- **D**：保留 panic，加 chi recover middleware 兜底。`recover` 救得了 goroutine 但救不了 caller 拿到正确结果；伪修复。
- **选 A（type-tag + ok-check）的理由**：3 行修改；不变更 API；保留 cache key 可读；可观测的 metric；errors-as-values 符合 CLAUDE.md "不吞错"。

---

## 6. TASK T5：Billing-7 — `RequestFreeze.freeze_amount` 由 `u64` 改 `i64` + 构造期 `try_from`，全路径含 release/drop

### 6.1 是什么 What it IS
- 一个 Rust 代码改动，文件 `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-ledger/src/freeze.rs`：
  - `RequestFreeze.freeze_amount` 字段类型 `u64` → `i64`（line 68）。
  - `RequestFreeze::new` 签名从 `pub fn new(uid: UserId, amount: u64) -> Self` 改为 `pub fn new(uid: UserId, amount: u64) -> Result<Self, FreezeAmountOverflow>`，内部用 `i64::try_from(amount).map_err(...)`。
  - 同样改 `TaskFreeze::new` 与 `TaskFreeze.freeze_amount` 字段；同样改 `FreezeGuard::freeze_amount(&self) -> u64` trait 方法（保持 trait 返回 `u64`——caller 可读，但内部存 i64）。
  - 删除所有 `amount as i64` / `self.freeze_amount as i64`（freeze.rs:89, 123, 124, 136, 137 + TaskFreeze 对应位置）。
- 所有上游调用 `RequestFreeze::new` / `TaskFreeze::new` 的 site 改为 `?` 传播或 `match` 处理。

### 6.2 不是什么 What it is NOT
- ❌ 不改 trait `FreezeGuard.freeze_amount(&self) -> u64` 的返回类型——外部观测仍是 u64（amount 是非负量）。
- ❌ 不改 `DeductionState.frozen_micro: AtomicI64` 类型——它本就是 i64。
- ❌ 不改 `crate::flusher::enqueue_frozen_delta(uid, delta: i64)` 签名——`delta` 已是 i64。
- ❌ 不改 R5/R9 处理（task freeze 的 DLQ + sweeper 是 WS-1 范畴）。
- ❌ 不修复 `TaskFreeze.release` 的 `tokio::spawn` fire-and-forget bug（WS-1 T15 范畴）。

### 6.3 为什么 Why this bug exists
- **根因**：`amount: u64` 输入可 ∈ [0, u64::MAX]；`as i64` 在 amount > i64::MAX 时**静默**变成负数（two's complement wrap）。负 amount 进入 `fetch_add(amount as i64)` → `frozen_micro` 被错误减少 → 用户拿到"免费 quota"。
- **引用**：原审计 Billing-7（Rust audit P0）；master plan §4 WS-7 S7.5；v2 REV-FINAL-H7.8。
- **后果**：cost 计算上游若曾在某协议返回 u64 > i64::MAX（特殊 token 计费），结算永久错位。
- **根本治理**：在边界（构造期）一次性显式拒绝越界值；内部全程 i64。

### 6.4 做什么 DO
1. `freeze.rs:68`：`pub(crate) freeze_amount: u64` → `pub(crate) freeze_amount: i64`。
2. `freeze.rs:148`：`TaskFreeze.freeze_amount: u64` → `i64`。
3. 新增 error 类型：
   ```rust
   #[derive(Debug, thiserror::Error)]
   #[error("freeze amount {0} exceeds i64::MAX")]
   pub struct FreezeAmountOverflow(pub u64);
   ```
4. `RequestFreeze::new` 签名改 `pub fn new(uid: UserId, amount: u64) -> Result<Self, FreezeAmountOverflow>`：
   ```rust
   let amount_i64 = i64::try_from(amount).map_err(|_| FreezeAmountOverflow(amount))?;
   // 然后用 amount_i64 替换原 amount as i64。
   ```
5. 同样改 `TaskFreeze::new`。
6. `FreezeGuard::freeze_amount(&self) -> u64` 的实现：`self.freeze_amount as u64`（i64 → u64 安全因为构造期已校验非负 ≤ i64::MAX）。
7. 删除所有 `as i64` 出现在本文件内（5 处）。
8. **upstream caller 改造**：`rg "RequestFreeze::new\|TaskFreeze::new" /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/` 收集所有调用点；每处改成 `?` 传播或 explicit handle。**调用点必须返回 HTTP 400 `GW_AMOUNT_OVERFLOW`**（caller 端，本 task 不要进 caller 协议层细节，只要求 caller 编译通过且不静默 unwrap）。

### 6.5 不做什么 DON'T
- ❌ 不要在 `new` 里 `clamp(0..=i64::MAX as u64)`——必须显式拒绝越界，不静默截断。
- ❌ 不要把 `try_from` 的失败 silenced 成 `panic!("amount too big")`——`new` 必须返回 Result，由 caller 决定 HTTP 响应。
- ❌ 不要顺便把 `DeductionState.frozen_micro` 改 AtomicU64——会破坏负数 delta（refund）路径。
- ❌ 不要新增 wrapper struct `FreezeAmount(i64)`——简单 i64 即可。
- ❌ 不要修 `Drop`（line 128-141）的 metric `gateway_freeze_leak_recovered_total`——它的语义跟本 task 无关。

### 6.6 怎么做 HOW (具体)

```rust
// crates/gw-ledger/src/freeze.rs (key sections)

#[derive(Debug, thiserror::Error)]
#[error("freeze amount {0} exceeds i64::MAX")]
pub struct FreezeAmountOverflow(pub u64);

pub struct RequestFreeze {
    pub(crate) uid: UserId,
    pub(crate) freeze_amount: i64,  // was u64
    pub(crate) released: AtomicBool,
    pub(crate) state: Arc<DeductionState>,
}

impl RequestFreeze {
    pub fn new(uid: UserId, amount: u64) -> Result<Self, FreezeAmountOverflow> {
        let amount_i64 = i64::try_from(amount).map_err(|_| FreezeAmountOverflow(amount))?;
        let state = DEDUCT
            .entry(uid.clone())
            .or_insert_with(|| {
                Arc::new(DeductionState {
                    base_micro: AtomicI64::new(0),
                    deducted_micro: AtomicI64::new(0),
                    frozen_micro: AtomicI64::new(0),
                })
            })
            .clone();
        state.frozen_micro.fetch_add(amount_i64, Ordering::AcqRel);
        crate::flusher::enqueue_frozen_delta(&uid, amount_i64);
        Ok(Self {
            uid,
            freeze_amount: amount_i64,
            released: AtomicBool::new(false),
            state,
        })
    }
}

impl FreezeGuard for RequestFreeze {
    fn freeze_amount(&self) -> u64 {
        // construction-time validation guarantees 0 <= freeze_amount <= i64::MAX
        self.freeze_amount as u64
    }

    fn release(&self) {
        if self.released.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }
        self.state.frozen_micro.fetch_sub(self.freeze_amount, Ordering::AcqRel);
        crate::flusher::enqueue_frozen_delta(&self.uid, -self.freeze_amount);
    }
}

impl Drop for RequestFreeze {
    fn drop(&mut self) {
        if !self.released.swap(true, Ordering::AcqRel) {
            self.state.frozen_micro.fetch_sub(self.freeze_amount, Ordering::AcqRel);
            crate::flusher::enqueue_frozen_delta(&self.uid, -self.freeze_amount);
            metrics::counter!("gateway_freeze_leak_recovered_total").increment(1);
        }
    }
}

// Apply parallel edits to TaskFreeze (lines 145-216).
```

调用点改造（命令模板）：
```bash
rg 'RequestFreeze::new\(' /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/
rg 'TaskFreeze::new\('    /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/
```
每处调用点 `let g = RequestFreeze::new(uid, amount);` → `let g = RequestFreeze::new(uid, amount).map_err(|e| GwError::AmountOverflow(e.0))?;`（具体 error 变体由 caller 自决）。

### 6.7 如何验证 VERIFY
1. 单元：`cargo test -p gw-ledger -- freeze::tests::overflow_rejected` 新加测试：`RequestFreeze::new(uid, u64::MAX)` → `Err(FreezeAmountOverflow(u64::MAX))`；`RequestFreeze::new(uid, i64::MAX as u64)` → `Ok(_)`；`RequestFreeze::new(uid, 0)` → `Ok(_)`，`freeze_amount() == 0`。
2. 单元：`cargo test -p gw-ledger -- freeze::tests::release_idempotent` 既有；不应回归。
3. 性质测试 (`proptest`)：任意 `amount ∈ [0, i64::MAX as u64]` → `new` Ok；`freeze_amount() == amount`；release 后 `state.frozen_micro` 回到 base。
4. 编译：`cargo build --workspace`——所有 caller 已迁移。
5. 全工作区：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。

### 6.8 备选方案 + ADR
- **B**：保留 u64，全程算术换成 `checked_add` / `checked_sub`，越界返回 None 同 panic-or-skip。需要每个 `fetch_add` 拆 CAS-loop 实现 checked，复杂；DeductionState 仍 i64 时再得做转换。
- **C**：把 `frozen_micro` 改 AtomicU128（i128 替代）。无 std AtomicI128；要 third-party crate 或 Mutex 包；性能炸。
- **D**：在 `as i64` 处加 `debug_assert!(amount <= i64::MAX as u64)`——release 模式失效，根因留存。
- **选 A（边界 try_from + 内部 i64）的理由**：单点校验；类型简单；零运行时新依赖；与 v2 REV-FINAL-H7.8 一致；编译期强制 caller 处理 err（Result 不能忽略）。

---

## 7. TASK T6：Billing-9 — `vendor_factor` 量化为 u32 micro-units，全 i128 算术，末除 1e6 + clamp i64

### 7.1 是什么 What it IS
- 一个 Rust 代码改动，文件 `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-billing/src/formulas.rs`，函数 `scaled_micro`（约 71-84 行）；同时改 `TaskBillingConfig.vendor_multiplier: f64` 与 `TaskBillingResult.vendor_multiplier: f64` 为 `u32`（micro-units，×1e6）。
- 即原 `1.25` → `1_250_000`（u32）。
- `scaled_micro` 用 `i128` 中间累乘，最后除 `1_000_000`，clamp 到 `[0, i64::MAX]`，返回 i64。

### 7.2 不是什么 What it is NOT
- ❌ 不删除 `TaskBillingConfig` / `TaskBillingResult` 类型——只换字段类型。
- ❌ 不引入 `rust_decimal` 或 `bigdecimal`——增加依赖且无收益（micro-units + i128 已精确）。
- ❌ 不在 Go 侧改 cost 表——Go 自有 cost 计算路径（modules/billing），本 task 仅 Rust formulas。
- ❌ 不改 `kind_multiplier` / `duration_multiplier`（const u32 函数）的语义。

### 7.3 为什么 Why this bug exists
- **根因**：`scaled_micro` 把 `base_micro: i64` 转 `f64` 做四个浮点乘法。`f64` 尾数 53 bit，超过 2^53 后 ulp = 2、4、8...。例：`base_micro = 1e12, kind=6, vendor=1.000001, duration=4` → 中间值 ~2.4×10^13，ulp ≈ 4，最后 `.round()` 误差至 4 micro-units；积累跨日跨月用户可"白嫖" $0.0000001..$0.001。
- 更糟：浮点不结合，相同输入跨平台（x86 vs ARM）边界可不一致。
- **引用**：原审计 Billing-9；master plan §4 WS-7 S7.6；v2 REV-FINAL-H7.5。
- **根本治理**：整数化 vendor_factor，全 i128 算术，避免任何 f64。

### 7.4 做什么 DO
1. `TaskBillingConfig.vendor_multiplier: f64` → `vendor_multiplier_micro: u32`（注释："vendor_factor × 1e6, e.g. 1.25 → 1_250_000"）。
2. `TaskBillingResult.vendor_multiplier: f64` → `vendor_multiplier_micro: u32`。
3. `scaled_micro` 重写：
   ```rust
   fn scaled_micro(
       base_micro: i64,
       kind_factor: u32,
       vendor_factor_micro: u32,
       duration_factor: u32,
   ) -> i64 {
       if base_micro <= 0 || vendor_factor_micro == 0 {
           return 0;
       }
       let base = base_micro as i128;
       let acc = base
           .saturating_mul(kind_factor as i128)
           .saturating_mul(vendor_factor_micro as i128)
           .saturating_mul(duration_factor as i128);
       let scaled = acc / 1_000_000_i128;  // back to micro-units
       scaled.clamp(0, i64::MAX as i128) as i64
   }
   ```
4. `calculate_freeze_amount` / `calculate_settle_amount` 调用方传 `config.vendor_multiplier_micro` / `result.vendor_multiplier_micro`。
5. 所有构造 `TaskBillingConfig` / `TaskBillingResult` 的 caller（多见于 task layer 与测试）：把字面 `1.25` 改为 `1_250_000`。
6. Serde 兼容：因为字段名变化 + 类型变化，**显式 #[serde(rename = "vendor_multiplier")]** 保留 wire 名不变（避免破坏存量 JSON）；新值仍按 micro-units 整数序列化。**如果 wire 仍是 f64**（既存 JSON in Redis），需要自定义 deserializer 在 deserialize 时 `(f * 1e6).round() as u32`——本 task 加 INVESTIGATE-WS7-001（见 §10）询问"wire 是否有存量 f64 数据"。

### 7.5 不做什么 DON'T
- ❌ 不要保留 f64 字段做 "兼容"——只要任何一条路径走 f64 就回归。
- ❌ 不要把 `vendor_multiplier_micro` 升 u64——u32 上限 4.29e9 / 1e6 ≈ 4290x 倍率，绰绰有余；u32 让乘法溢出更难。
- ❌ 不要忘记 `saturating_mul`——i128 也会溢出（i128::MAX ≈ 1.7e38；base*kind*vendor*dur ≤ ~1e12 × 6 × 4.29e9 × 4 ≈ 1e23，安全；但保险起见 saturate）。
- ❌ 不要把除 1e6 改 `>> 20`——1e6 不是 2 的幂，会引入新误差。
- ❌ 不要修 `kind_multiplier` 让它们也 micro-unit——它们本就是整数倍率（3, 6, 2, 1）。

### 7.6 怎么做 HOW (具体)

```rust
// crates/gw-billing/src/formulas.rs
use gw_task::TaskKind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TaskBillingConfig {
    pub base_micro: i64,
    /// vendor multiplier in micro-units (×1e6). e.g. 1.25 → 1_250_000.
    /// Encoded this way to avoid f64 accumulation drift in scaled_micro.
    pub vendor_multiplier_micro: u32,
    pub estimated_duration_secs: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct TaskBillingResult {
    pub actual_duration_secs: u64,
    pub succeeded: bool,
    pub vendor_multiplier_micro: u32,
}

#[must_use]
pub fn calculate_freeze_amount(kind: TaskKind, config: &TaskBillingConfig) -> i64 {
    scaled_micro(
        config.base_micro,
        kind_multiplier(kind),
        config.vendor_multiplier_micro,
        duration_multiplier(config.estimated_duration_secs),
    )
}

#[must_use]
pub fn calculate_settle_amount(kind: TaskKind, result: &TaskBillingResult) -> i64 {
    if !result.succeeded {
        return 0;
    }
    scaled_micro(
        settle_base_micro(kind),
        kind_multiplier(kind),
        result.vendor_multiplier_micro,
        duration_multiplier(result.actual_duration_secs),
    )
}

fn scaled_micro(
    base_micro: i64,
    kind_factor: u32,
    vendor_factor_micro: u32,
    duration_factor: u32,
) -> i64 {
    if base_micro <= 0 || vendor_factor_micro == 0 {
        return 0;
    }
    let acc = (base_micro as i128)
        .saturating_mul(kind_factor as i128)
        .saturating_mul(vendor_factor_micro as i128)
        .saturating_mul(duration_factor as i128);
    let scaled = acc / 1_000_000_i128;
    scaled.clamp(0, i64::MAX as i128) as i64
}
// kind_multiplier / settle_base_micro / duration_multiplier unchanged.
```

### 7.7 如何验证 VERIFY
1. 单元 `cargo test -p gw-billing -- formulas::`（既有不应回归）。
2. 新单元：`scaled_micro_precision`：固定输入 `(base=1_000_000_000_000, kind=6, vendor=1_000_001, dur=4)`；assert 结果 == `(1e12 × 6 × 1_000_001 × 4) / 1e6` == `24_000_024_000_000`；**对比 f64 旧实现的结果**展示 drift。
3. 性质测试：`proptest!(|(b: u32, k in 1u32..=6, v in 1u32..u32::MAX/10000, d in 1u32..=4)| { let r = scaled_micro(b as i64, k, v, d); assert!(r >= 0); })`。
4. JSON 兼容（取决于 INVESTIGATE-WS7-001 结论）：若 wire 现存 f64 JSON，加 `#[serde(deserialize_with = "deserialize_factor_as_micro")]`；新增测试 `deserialize_f64_legacy_payload` 验证 round-trip。
5. 全工作区：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。

### 7.8 备选方案 + ADR
- **B**：保留 f64，仅末尾改 `round_ties_even()` 替 `round()`。仍 f64 累乘 drift。
- **C**：用 `rust_decimal::Decimal`。引入大依赖；运行时性能 5-10x 慢于 i128。
- **D**：让 caller 传已经预乘后的 i64 amount，删 `scaled_micro` 公式。设计倒退（业务规则散落 caller）。
- **选 A（u32 micro + i128 中间）的理由**：精确无 drift；零依赖；i128 寄存器在 x86_64 / ARM64 都是原生支持；类型表达"我是整数化倍率"清晰。

---

## 8. TASK T7：R10 — `retry_after_secs` clamp + 反复 >1h 升级到 vendor-full Open

### 8.1 是什么 What it IS
- 一个 Rust 代码改动，文件 `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-health/src/store.rs`，函数 `failure_payload_directive`（约 237-258 行）。
- 在写 `ttl_secs` 字段前 clamp 到 `[1, 3600]`；同时记录原始未 clamp 值 `gw_health_cooldown_clamp_total{key, vendor, original_bucket}`。
- 在 `KeyHealthStore::record` 内（约 127-140 行）跟踪某 `(key, fmt)` 连续 N=3 次被 vendor 要求 `retry_after >= 3600` 的次数；命中后调用 `apply_cooldown` 时**升级**到 `CooldownReason::VendorFullOutage`（新增枚举值）+ TTL `min(7200, 全局上限)`，并 alert。

### 8.2 不是什么 What it is NOT
- ❌ 不改 `Option<u32>` 类型——`None` 仍 fallback 到 60s/30s 默认。
- ❌ 不改 `FormatHealth::apply_cooldown`（format_health.rs:157）——它只接收 `CooldownDirective`，本 task 只改 directive 来源处。
- ❌ 不引入新 alert subsystem——`tracing::warn!(target="alert", ...)` + metric 即可。
- ❌ 不删除 `apply_cooldown` 已有的 `cooldown_until_ms` 仅增不减语义——本 task 不动 `FormatHealth`。

### 8.3 为什么 Why this bug exists
- **根因**：`failure_payload_directive` 把 `retry_after_secs: Option<u32>` 直接落到 `ttl_secs: u32`。`format_health.rs:158` 计算 `now_ms() + (ttl_secs as u64) * 1000`：`u32::MAX × 1000` = ~4.29e12 ms ≈ 136 年。任何恶意/错配 vendor 返回 `Retry-After: 4294967295` 即可让该 vendor 永久下线。
- **次要根因**：合法 vendor 在大事故时反复回 `Retry-After: 3600`，每次刷新只重置成 1h；运维需手工 `clear_cooldown`。
- **引用**：原审计 R10；master plan §4 WS-7 S7.7；v2 REV-FINAL-H7.6。

### 8.4 做什么 DO
1. 在 `failure_payload_directive` 内，把 `retry_after_secs.unwrap_or(60)` / `.unwrap_or(30)` 计算结果再 clamp `.clamp(1, 3600)`，写入 `ttl_secs`。
2. 当原始值 > 3600 → `metrics::counter!("gw_health_cooldown_clamp_total", "bucket" => "above_1h").increment(1)`；当 < 1 → bucket `"below_1s"`。
3. `CooldownReason` 枚举（位于 `crates/gw-health/src/cooldown.rs`）新增变体 `VendorFullOutage`；wire label `"vendor_full_outage"`。
4. 在 `KeyHealthStore` 加字段 `vendor_full_streak: DashMap<(KeyId, ApiFormat), AtomicU32>`，每次 `record` 调用：
   - 若 `OutcomeKind::Failure { retry_after_secs: Some(s), .. }` 且 `s > 3600`：`streak.fetch_add(1)`。
   - 若 streak ≥ 3：`apply_cooldown(key, fmt, CooldownDirective { reason: VendorFullOutage, ttl_secs: 7200, reset_consecutive: false })`，`tracing::warn!(target="alert", key=%key, "vendor demanding >1h retry 3x, escalated to VendorFullOutage")`。
   - `OutcomeKind::Success` → `streak.store(0)`。
5. metric `gw_health_vendor_full_escalation_total{key}`。

### 8.5 不做什么 DON'T
- ❌ 不要把 clamp 边界设成宽松的 `[1, u32::MAX]`——根因要消灭。
- ❌ 不要 page / wake oncall 在 metric 自增——alert 配置另由 SRE 在 Prom rule 内设置（WS-10 范畴）。
- ❌ 不要让 `VendorFullOutage` 阻塞所有 fmt——每 fmt 独立追踪。
- ❌ 不要在 `apply_cooldown` 内部 clamp——这会让管理面（如 Quota Probe）显式设 24h 的合法用法也被截断。clamp 仅在 vendor 头来源路径。

### 8.6 怎么做 HOW (具体)

```rust
// crates/gw-health/src/cooldown.rs
pub enum CooldownReason {
    None,
    Status429RateLimit,
    Status529Overloaded,
    StatusTransient5xx,
    SlidingWindowTrip,
    // ... existing ...
    VendorFullOutage,  // NEW
}

impl CooldownReason {
    pub fn label(&self) -> &'static str {
        match self {
            // ... existing ...
            CooldownReason::VendorFullOutage => "vendor_full_outage",
        }
    }
}
```

```rust
// crates/gw-health/src/store.rs

const RETRY_AFTER_CLAMP_MIN: u32 = 1;
const RETRY_AFTER_CLAMP_MAX: u32 = 3_600;
const VENDOR_FULL_ESCALATION_THRESHOLD: u32 = 3;

fn failure_payload_directive(
    http_status: u16,
    retry_after_secs: Option<u32>,
) -> Option<CooldownDirective> {
    let mk = |reason: CooldownReason, default_ttl: u32| {
        let raw = retry_after_secs.unwrap_or(default_ttl);
        let clamped = raw.clamp(RETRY_AFTER_CLAMP_MIN, RETRY_AFTER_CLAMP_MAX);
        if raw > RETRY_AFTER_CLAMP_MAX {
            metrics::counter!("gw_health_cooldown_clamp_total", "bucket" => "above_1h").increment(1);
        } else if raw == 0 {
            metrics::counter!("gw_health_cooldown_clamp_total", "bucket" => "below_1s").increment(1);
        }
        CooldownDirective { reason, ttl_secs: clamped, reset_consecutive: false }
    };
    match http_status {
        429 => Some(mk(CooldownReason::Status429RateLimit, 60)),
        500..=599 => {
            let reason = if http_status == 529 {
                CooldownReason::Status529Overloaded
            } else {
                CooldownReason::StatusTransient5xx
            };
            Some(mk(reason, 30))
        }
        _ => None,
    }
}

// Add to KeyHealthStore:
pub struct KeyHealthStore {
    pub by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>,
    vendor_full_streak: DashMap<(KeyId, ApiFormat), AtomicU32>,  // NEW
}

impl KeyHealthStore {
    pub fn record(&self, key: &KeyId, fmt: ApiFormat, outcome: OutcomeKind) {
        let health = self.get_or_create(key, fmt);
        let ok = matches!(outcome, OutcomeKind::Success);
        health.record(ok);

        match outcome {
            OutcomeKind::Success => {
                if let Some(s) = self.vendor_full_streak.get(&(key.clone(), fmt)) {
                    s.store(0, Ordering::Release);
                }
            }
            OutcomeKind::Failure { http_status, retry_after_secs } => {
                if let Some(directive) = failure_payload_directive(http_status, retry_after_secs) {
                    health.apply_cooldown(directive);
                }
                if let Some(raw) = retry_after_secs {
                    if raw > RETRY_AFTER_CLAMP_MAX {
                        let entry = self.vendor_full_streak
                            .entry((key.clone(), fmt))
                            .or_insert_with(|| AtomicU32::new(0));
                        let n = entry.fetch_add(1, Ordering::AcqRel) + 1;
                        if n >= VENDOR_FULL_ESCALATION_THRESHOLD {
                            health.apply_cooldown(CooldownDirective {
                                reason: CooldownReason::VendorFullOutage,
                                ttl_secs: 7200,
                                reset_consecutive: false,
                            });
                            metrics::counter!(
                                "gw_health_vendor_full_escalation_total",
                                "key" => key.to_string(),
                            ).increment(1);
                            tracing::warn!(
                                target: "alert",
                                key = %key,
                                streak = n,
                                "vendor demanding >1h retry; escalated to VendorFullOutage",
                            );
                            entry.store(0, Ordering::Release);
                        }
                    } else {
                        if let Some(s) = self.vendor_full_streak.get(&(key.clone(), fmt)) {
                            s.store(0, Ordering::Release);
                        }
                    }
                }
            }
        }
    }
}
```

### 8.7 如何验证 VERIFY
1. 单元：`cargo test -p gw-health -- store::tests::retry_after_clamped_above_3600`：传 `retry_after_secs=Some(u32::MAX)` → directive.ttl_secs == 3600；metric `gw_health_cooldown_clamp_total{bucket="above_1h"}` ≥ 1。
2. 单元：`retry_after_clamped_to_one_below_1s`：传 `Some(0)` → ttl_secs == 1；metric bucket `"below_1s"`。
3. 单元：`vendor_full_escalation_after_3`：3 次 `record(Failure{retry_after_secs: Some(7200)})` → 第 3 次后 `peek` 返回 `cooldown_reason == VendorFullOutage`，`cooldown_until_ms` 至少 +7200_000ms。
4. 单元：`vendor_full_streak_resets_on_success`：失败 2 次 + 1 成功 + 1 失败 >1h → 未触发 escalation。
5. 既有：`cargo test -p gw-health` 全过。

### 8.8 备选方案 + ADR
- **B**：clamp 上限为 `[1, 86400]`（24h）。够"宽容"但仍允许单次回 24h 让 vendor 离线一天，运维感受差。
- **C**：完全忽略 `Retry-After`，全部 30s。vendor 真要 5min 让流时反复探测打死。
- **D**：用一个外部 timer 而非 `cooldown_until_ms`。重设计；不符合"最小改动"。
- **选 A（[1, 3600] + streak escalation）的理由**：常见 vendor `Retry-After` 极少超 1h；超过即可疑；3-次 streak 后转 7200s（与 escalation 区分）+ alert 让 SRE 介入；最小新增状态。

---

## 9. TASK T8：R11 — FormatHealth.record 分支拆开：trip 与 half-open 转换互不竞争

### 9.1 是什么 What it IS
- 一个 Rust 代码改动，文件 `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-health/src/format_health.rs`，方法 `record(&self, ok: bool)`（约 95-152 行）。
- 把现有 `if total >= SAMPLE_FLOOR && rate > FAILURE_RATE_TRIP { ... } else if ok && HalfOpen { close } else if !ok && HalfOpen { re-arm Open }` 拆为：
  1. 先判 half-open 转换（独立 if，非 else-if）：HalfOpen + ok → close；HalfOpen + !ok → re-arm Open。
  2. 再判 closed 状态下是否 trip（独立 if）：仅当当前 circuit == Closed 才考虑 trip。
- 同时清空 `failure_window` 在 half-open → closed 转换时（防止 trip 路径凭老数据再 trip）。

### 9.2 不是什么 What it is NOT
- ❌ 不改 `is_available`（66-89 行）的语义。
- ❌ 不改 trip 阈值（20 sample / 50% rate）。
- ❌ 不改 `apply_cooldown` 仅增不减语义。
- ❌ 不修改 `record` 的签名 `fn record(&self, ok: bool)`。
- ❌ 不改 R12（WeightedRr）——那是 T9。

### 9.3 为什么 Why this bug exists
- **核实结论（与 v1/v2 描述有出入）**：v1 R11 说"录条件用排他 else-if，半开探针成功也可能再 trip"。**实读结果**：当 `circuit == HalfOpen` 且当时 `failure_window` 仍 ≥20 sample 且 rate > 50%（因为 OPEN_HOLD_MS=60s 期间窗口没被清；窗口本身是 30s 滑动），半开探针**成功**也会进入 trip 分支（trip 条件不依赖 ok），把 circuit 再次 store Open，**第二个 else-if（close）永远走不到**。结果：half-open 探针 ok 后 circuit **没有** 转 Closed；window 还在持续累积老 failure。事实上 trip 分支用 `self.circuit.store(Open)`，无论当前是 Open / HalfOpen / Closed 都写。HalfOpen 期间 trip 这一行重置 `open_until_ms` 一次。
- **根因**：trip 路径与 half-open 转换路径用 else-if 绑定，且 trip 路径不感知当前状态，导致 half-open recovery 路径被无声吞掉。
- **引用**：原审计 R11；master plan §4 WS-7 S7.8。
- **后果**：vendor 闪故障 → circuit Open → 60s 后探针成功 → 本应 Closed，实际再 60s 又 Open，循环；用户感受是"已恢复但被持续误判故障"。
- **根本治理**：half-open 转换优先且独立；trip 仅在 Closed 下评估。

### 9.4 做什么 DO
1. `record` 重写为两段独立 if：
   - 段 1：读取 circuit；若 HalfOpen → 视 ok 转 Closed（同时清 failure_window）或 re-arm Open。HalfOpen 路径 return（不再评估 trip）。
   - 段 2：仅当 circuit == Closed 才评估 trip。
2. 段 1 内"转 Closed"路径加 `self.failure_window.lock().reset()`（避免新 Closed 周期内残留老 failure 立刻又 trip）。
3. consecutive_failures 更新逻辑保留（顶部独立 if，与 circuit 转换无关）。
4. 添加注释引用 master plan + 本 task 编号 + 解释为什么"清空 window"。

### 9.5 不做什么 DON'T
- ❌ 不要让 trip 路径无视 circuit 当前状态——必须 Closed 才考虑 trip。
- ❌ 不要在 HalfOpen → Open 的 re-arm 分支也清 window——保留旧失败记录利于下次 half-open 后快速感知问题。
- ❌ 不要让 `record` 持锁跨多分支调用 `apply_cooldown`——本 task 不动 cooldown。
- ❌ 不要把 `failure_window` 由 Mutex 改 RwLock——读路径不存在，无收益。
- ❌ 不要去 sleep / spin——状态机纯 CAS。

### 9.6 怎么做 HOW (具体)

```rust
// crates/gw-health/src/format_health.rs (replace record method, ~95-152)

pub fn record(&self, ok: bool) {
    {
        let mut w = self.failure_window.lock();
        w.record(ok);
    }
    if ok {
        self.consecutive_failures.store(0, Ordering::Release);
    } else {
        self.consecutive_failures.fetch_add(1, Ordering::AcqRel);
    }

    // Section 1 (priority): handle half-open transitions independently.
    // A successful probe MUST be able to close the circuit even if the
    // 30s sliding window still shows >50% failure rate from the pre-open
    // era. We also reset the window on close so the next Closed cycle
    // starts fresh.
    let circuit = CircuitState::from_u8(self.circuit.load(Ordering::Acquire));
    if circuit == CircuitState::HalfOpen {
        if ok {
            if self
                .circuit
                .compare_exchange(
                    CircuitState::HalfOpen.as_u8(),
                    CircuitState::Closed.as_u8(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.failure_window.lock().reset();
            }
        } else {
            if self
                .circuit
                .compare_exchange(
                    CircuitState::HalfOpen.as_u8(),
                    CircuitState::Open.as_u8(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.open_until_ms
                    .store(now_ms() + OPEN_HOLD_MS, Ordering::Release);
            }
        }
        return; // half-open handling complete; do not trip from here.
    }

    // Section 2: only Closed circuits can trip.
    if circuit == CircuitState::Closed {
        let (total, rate) = self.failure_window.lock().failure_rate();
        if total >= SAMPLE_FLOOR && rate > FAILURE_RATE_TRIP {
            // Race-tolerant: only trip if still Closed.
            let _ = self.circuit.compare_exchange(
                CircuitState::Closed.as_u8(),
                CircuitState::Open.as_u8(),
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            self.open_until_ms
                .store(now_ms() + OPEN_HOLD_MS, Ordering::Release);
        }
    }
}
```

### 9.7 如何验证 VERIFY
1. 单元（既有）：`cargo test -p gw-health -- format_health::tests` 不应回归（包括 `half_open_failed_returns_to_open`，2026-05-30 §1.4 引用的测试）。
2. 新单元：`half_open_success_closes_even_with_dirty_window`：构造场景 — 先 record 25 次 fail 触发 Open；fast-forward `open_until_ms`；`is_available()` 触发 HalfOpen；record(true) → 状态 == Closed；`failure_window.failure_rate()` 总数 == 0（已 reset）。
3. 新单元：`closed_circuit_trips_on_high_failure`：25 次 record(false) → Open。
4. 新单元：`half_open_skips_trip_path`：HalfOpen 时 record(false) → 状态 Open（来自 re-arm）；assert `open_until_ms` 接近 `now_ms() + 60_000`。
5. property：在 random ok 序列下，任何 HalfOpen 状态结束后必定转为 Closed 或 Open，决不残留在 HalfOpen 超过一个 record 调用。

### 9.8 备选方案 + ADR
- **B**：在 trip 分支加 `if circuit != HalfOpen` 守卫，保留原 else-if 结构。仍依赖 else-if 的尾巴，可读性差。
- **C**：用单独的 Mutex 包整个 `record` 体序列化。降低并发；当前 CAS 已无锁，倒退。
- **D**：把 window 改为"open 时清空"——开启周期早，窗口被清，但 HalfOpen 再 fail 时窗口又是空的，sliding window 失效一段。半灵半不灵。
- **选 A（段拆 + close 时清窗口）的理由**：分别表达"半开优先转换"与"仅 Closed 评估 trip"两个不变式；半开成功后明确进入新 Closed 周期；CAS 保留无锁路径；测试可独立覆盖每段。

---

## 10. TASK T9：R12 + REV-FINAL-H7.7 — KeyHealthStore 加 transition observer，WeightedRr 注册回调把 unavailable key 的 current 清零

### 10.1 是什么 What it IS
- 两文件改动：
  - `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-health/src/store.rs`：`KeyHealthStore` 加 `availability_observers: RwLock<Vec<Arc<dyn AvailabilityObserver>>>` 与公共 trait `AvailabilityObserver`；新 fn `register_availability_observer(&self, obs: Arc<dyn AvailabilityObserver>)`；在 `record` 内当 `is_available()` 从 true 转 false 时，对所有 observer 调 `on_became_unavailable(key, fmt)`（true→true、false→false、false→true 不调）。
  - `/Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/gw-routing/src/strategy/weighted_rr.rs`：`WeightedRr` 实现 `AvailabilityObserver`，回调把所有 `(model, key)` 对应的 `current` AtomicI32 store 为 0。装配代码（在 bootstrap / lib.rs 内）注册之。

### 10.2 不是什么 What it is NOT
- ❌ 不改 `WeightedRr::pick` 的 Phase 1/2/3 算法骨架——Phase 1 仍对所有候选 fetch_add；正确性由"unavailable 转变即清零 current"维持。
- ❌ 不为 RoundRobin / LoadBalance 加 observer——它们的状态机不积累 current weight。
- ❌ 不引入 channel/异步 callback——同步 trait 即可（observer 数量小、调用频次低）。
- ❌ 不实现"available 转变"回调——本 task 只关心 unavailable 转变（恢复时 current=0 起步天然平衡）。
- ❌ 不在本 task 内 sweep `WeightedRr.current` 跨 `model` 维度（v1 §3 WS-3 routing sweep）——routing sweep 是另一 task。

### 10.3 为什么 Why this bug exists
- **根因**：`WeightedRr::pick` Phase 1 `for k in c { ... fetch_add(weight[i]) }` 对**所有**候选累加，包括当前 unavailable 的；Phase 2 才用 `is_available` 过滤。在 vendor key 故障期，Phase 1 持续加 weight 到 unavailable key 的 current；Phase 3 减 total weight 只减 winner（available），unavailable 的 current 单调上涨。故障恢复瞬间 unavailable → available，其 current 已远大于其他正常 key 的 current → Phase 2 选中它（"雪崩"），同时它刚恢复未必稳。
- **REV-FINAL-H7.7 给的选项 A**：transition-to-unavailable 时 hook 把 `current[key]=0`。恢复时从 0 起步，与其他 key 公平竞争（SWRR 数学保持平衡）。
- **引用**：原审计 R12；master plan §4 WS-7 S7.9；v2 REV-FINAL-H7.7。

### 10.4 做什么 DO
1. `gw-health/src/store.rs` 内：
   ```rust
   pub trait AvailabilityObserver: Send + Sync {
       fn on_became_unavailable(&self, key: &KeyId, fmt: ApiFormat);
   }

   pub struct KeyHealthStore {
       pub by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>,
       observers: RwLock<Vec<Arc<dyn AvailabilityObserver>>>,
       vendor_full_streak: DashMap<(KeyId, ApiFormat), AtomicU32>, // from T7
   }
   ```
2. 新 fn `register_availability_observer(&self, obs: Arc<dyn AvailabilityObserver>)`。
3. `record` 内（与 T7 共存）：在调 `health.record(ok)` / `apply_cooldown` **之前**记录 `was_avail = health.is_available()`，**之后**记录 `now_avail = health.is_available()`；若 `was_avail && !now_avail` → 调每个 observer 的 `on_became_unavailable(key, fmt)`。
4. `gw-routing/src/strategy/weighted_rr.rs`：
   ```rust
   impl AvailabilityObserver for WeightedRr {
       fn on_became_unavailable(&self, key: &KeyId, _fmt: ApiFormat) {
           // Zero current for every (model, key) entry whose KeyId matches.
           for entry in self.current.iter() {
               if &entry.key().1 == key {
                   entry.value().store(0, Ordering::Relaxed);
               }
           }
       }
   }
   ```
   注意：DashMap iter 是 sharded scan；如 cardinality 大可分批；本 task 简单实现即可（unavailable 事件低频）。
5. 装配（具体位置：bootstrap，`crates/gw-gateway/src/main.rs` 或同等装配处）：`health_store.register_availability_observer(weighted_rr.clone() as Arc<dyn AvailabilityObserver>);`——前提 `WeightedRr` 在 Arc 内被装配（如不是，需 wrap）。

### 10.5 不做什么 DON'T
- ❌ 不要回调时持任何 health store 内部锁——回调可能反向调 health store（虽不应该），死锁风险。release `observers` read-lock 后再循环调。
- ❌ 不要把 observer 注册成 `Box<dyn>`——多个调用方共享，必须 Arc。
- ❌ 不要让 observer panic 影响 record 主路径——本 task 简单实现下 panic 会冒泡；加 `catch_unwind`（unwind safe 取决于 observer impl）暂不强求；若有顾虑可加 `tracing::error!` + `let _ = catch_unwind(...)` 包裹。
- ❌ 不要给 observer "becomes available" 回调——选项 A 明确不需要。
- ❌ 不要在 `WeightedRr::pick` 内调 health-store mutating 方法——保持单向：health → routing。

### 10.6 怎么做 HOW (具体)

`crates/gw-health/src/store.rs`（关键片段，与 T7 改动合并）：

```rust
use parking_lot::RwLock;

pub trait AvailabilityObserver: Send + Sync {
    fn on_became_unavailable(&self, key: &KeyId, fmt: ApiFormat);
}

pub struct KeyHealthStore {
    pub by_key_format: DashMap<(KeyId, ApiFormat), Arc<FormatHealth>>,
    observers: RwLock<Vec<Arc<dyn AvailabilityObserver>>>,
    vendor_full_streak: DashMap<(KeyId, ApiFormat), AtomicU32>, // from T7
}

impl Default for KeyHealthStore {
    fn default() -> Self {
        Self {
            by_key_format: DashMap::new(),
            observers: RwLock::new(Vec::new()),
            vendor_full_streak: DashMap::new(),
        }
    }
}

impl KeyHealthStore {
    pub fn register_availability_observer(&self, obs: Arc<dyn AvailabilityObserver>) {
        self.observers.write().push(obs);
    }

    pub fn record(&self, key: &KeyId, fmt: ApiFormat, outcome: OutcomeKind) {
        let health = self.get_or_create(key, fmt);
        let was_avail = health.is_available();
        let ok = matches!(outcome, OutcomeKind::Success);
        health.record(ok);
        // ... T7 escalation logic here ...
        let now_avail = health.is_available();
        if was_avail && !now_avail {
            // Snapshot observers, drop lock, then call.
            let obs: Vec<Arc<dyn AvailabilityObserver>> = self.observers.read().clone();
            for o in obs {
                o.on_became_unavailable(key, fmt);
            }
        }
    }

    pub fn apply_cooldown(&self, key: &KeyId, fmt: ApiFormat, d: CooldownDirective) {
        let health = self.get_or_create(key, fmt);
        let was_avail = health.is_available();
        health.apply_cooldown(d);
        // metric (existing) ...
        let now_avail = health.is_available();
        if was_avail && !now_avail {
            let obs: Vec<Arc<dyn AvailabilityObserver>> = self.observers.read().clone();
            for o in obs {
                o.on_became_unavailable(key, fmt);
            }
        }
    }
}
```

`crates/gw-routing/src/strategy/weighted_rr.rs`：

```rust
use gw_health::store::AvailabilityObserver;
use gw_core::ids::KeyId;
use gw_core::protocol::ApiFormat;

impl AvailabilityObserver for WeightedRr {
    fn on_became_unavailable(&self, key: &KeyId, _fmt: ApiFormat) {
        for entry in self.current.iter() {
            if &entry.key().1 == key {
                entry.value().store(0, Ordering::Relaxed);
            }
        }
    }
}
```

装配（`crates/gw-gateway/src/...` 或工作区实际 wire-up 处；用 `rg "KeyHealthStore::default\|new" /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/` 查 真实组装点）：

```rust
let health_store = Arc::new(KeyHealthStore::default());
let weighted_rr = Arc::new(WeightedRr::new());
health_store.register_availability_observer(weighted_rr.clone() as Arc<dyn AvailabilityObserver>);
```

### 10.7 如何验证 VERIFY
1. 单元 `cargo test -p gw-health -- store::tests::observer_called_on_unavailable`：构造 KeyHealthStore + 注入 mock observer；record 一次失败不触发（仍 available）；record 25 次失败触发 sliding-window trip → assert observer 调用 1 次。
2. 单元 `observer_not_called_when_already_unavailable`：先把 circuit 推到 Open；再 record 一次失败 → observer 调用 0 次（was_avail 已 false）。
3. 单元 `observer_not_called_on_recovery`：从 Open 转 HalfOpen 转 Closed → observer 不被调（仅 unavailable 方向）。
4. 单元 `weighted_rr_zeros_current_on_callback`：构造 WeightedRr，pick 几轮让 current 非零；调 `on_became_unavailable(key)` → 所有 (model, key) 维度 current == 0。
5. 集成：`cargo test -p gw-routing -- weighted_rr_recovery_no_avalanche`：
   - 候选 [A,B]，weight [10, 10]，初始全 available。
   - 1000 pick：仅 B 失败 → B unavailable → A 占满；assert B 的 current 跌至 0（不积累）。
   - B 恢复 → assert 接下来 100 pick 内 A 与 B 选中数差 < 30（fairness 测试，而非"绝对均衡"）。
6. 全工作区：`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。

### 10.8 备选方案 + ADR
- **B**（v1 原案）：`WeightedRr::pick` Phase 1 加 `if h.is_available(...)` filter。仍允许在 pick 间隙 unavailable 转变；不是"事件驱动归零"，是"每次 pick 检查"。CPU 多一点；语义上 unavailable 期间 current 仍单调，恢复瞬间被 B 解决（B 让 current 自然 0）。但回到旧问题：恢复瞬间 current=0 与其他 key（持续 fetch_add）相比偏低，可能反而**饿死**新恢复 key。SWRR 数学差异需仔细推导。
- **C**：在 `pick` Phase 3 时把 unavailable winner 的 current 撤销 fetch_sub。winner 是 available 的（Phase 2 过滤），所以无机会 fetch_sub 给 unavailable；同 B 一样不解决根因。
- **D**：每次 `WeightedRr::pick` 入口扫一遍 health，把所有当前 unavailable 的 current 设 0。无事件驱动，每 pick O(N) 全扫；浪费。
- **选 A（observer + 单事件归零）的理由**：与 REV-FINAL-H7.7 一致；事件驱动 O(1)（除 DashMap iter），低频；SWRR 数学性质保留（current 都是有效候选的，total 与 sum-current 守恒）；observer 模式可被 LoadBalance / LatencyFirst 等其他 strategy 复用。

---

## 11. 跨 task 依赖与执行顺序

| Task | 阻塞于 | 同 PR 推荐 | 跨仓 |
|---|---|---|---|
| T1 | — | T4（同文件） | Go |
| T2 | — | — | Go |
| T3 | T2（共用 `RealClientIP`） | — | Go |
| T4 | — | T1（同文件） | Go |
| T5 | — | — | Rust |
| T6 | — | — | Rust |
| T7 | — | T9（同 store.rs） | Rust |
| T8 | — | — | Rust |
| T9 | — | T7（同 store.rs） | Rust |

推荐 PR 分组：
- **PR-Go-1**：T1 + T4（cache.go 两改动合并）。
- **PR-Go-2**：T2 + T3（host.go + realip.go + ratelimit cleanup，T3 依赖 T2 的 helper）。
- **PR-Rust-1**：T5（freeze.rs，独立）。
- **PR-Rust-2**：T6（formulas.rs，独立）。
- **PR-Rust-3**：T7 + T9（store.rs 共改 + 新 observer + WeightedRr）。
- **PR-Rust-4**：T8（format_health.rs，独立）。

---

## 12. DoD（合并 v1 §4 WS-7 DoD + v2 REV-005 量化）

| 类别 | 指标 | 工具 / 命令 |
|---|---|---|
| Go race | T1+T4 通过 `go test -race ./shared/platform/cache/... -count=20` 0 fail | go test |
| Go race | T2+T3 通过 `go test -race ./shared/runtime/... -count=20` 0 fail | go test |
| Go leak | T3 cleanup goroutine 在 ctx cancel 后 200ms 内退出（`goleak.VerifyNone`） | goleak |
| Go DoS | 5000 unique XFF 头打 `/api/...`，trustXff=false → 限速以 peer_addr 计；spoofed XFF 无效 | tests/e2e/chaos/xff_spoof_test.go（WS-13 待交付，本 task 先 mark TODO） |
| Go boot | env `GW_ENV=production GW_TRUST_XFF=true GW_TRUSTED_PROXIES=` → process exit != 0 | TestBootstrap_ProductionRejectsEmptyTrustedProxies |
| Rust unit | T5+T6+T7+T8+T9 全部 `cargo test --workspace` 通过 | cargo test |
| Rust clippy | `cargo clippy --workspace --all-targets -- -D warnings` 0 warn | cargo clippy |
| Rust property | T5: 1e5 random `amount ∈ [0, i64::MAX as u64]` 全 Ok | proptest |
| Rust property | T6: 1e5 random `(base, k, v_micro, d)` 输入 → result ≥ 0 且与 reference impl（python decimal）对齐 ± 1 micro | proptest + 外部 reference |
| Rust unit | T7: 3 次 `retry_after=7200` → VendorFullOutage cooldown 触发；metric ≥ 1 | cargo test |
| Rust unit | T8: half-open success 后 circuit == Closed，failure_window reset | cargo test |
| Rust integration | T9: WeightedRr recovery test 显示 100 pick 内 A/B 差 < 30 | cargo test -p gw-routing |
| 对抗复审 | 派 sonnet 用 `tdd-adversarial-review.txt`（WS-12 交付）复审每 task | sonnet |
| 双仓 mirror | 本文件与 Rust 仓内 mirror 路径字节一致（除标题） | `diff` |

---

## 13. INVESTIGATE 开放项（P4 零疑点）

| ID | 待证 | 由谁负责 |
|---|---|---|
| INVESTIGATE-WS7-001 | `TaskBillingConfig.vendor_multiplier` 是否曾以 f64 形式 serde 到 Redis / PG / wire？若是，T6 需要 deserializer 兼容层 | T6 owner（任务开始前 30min 调查；用 `rg "TaskBillingConfig\|vendor_multiplier" --type rust` 看 caller，再看是否有持久化路径） |
| INVESTIGATE-WS7-002 | `BuildRouter` 调用方有几个 binary？每个是否有 main.go 自定义 deps？T2 需修改每个 | T2 owner（`rg "runtime.BuildRouter" /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/` 列出全部） |
| INVESTIGATE-WS7-003 | `WeightedRr` 在装配时是 `Arc<WeightedRr>` 还是嵌入到某 `StrategySet` 内？T9 注册 observer 时需 Arc 句柄 | T9 owner（`rg "WeightedRr::new\|Arc::new\(WeightedRr" /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/`） |
| INVESTIGATE-WS7-004 | `CooldownReason` 枚举有多少调用方使用？T7 新增 `VendorFullOutage` 是否破坏 match exhaustive？ | T7 owner（`rg "CooldownReason::" /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/crates/` 列 match 表达式） |
| INVESTIGATE-WS7-005 | `Deps.Ctx context.Context` 在 Go 侧 `shared/runtime.Deps` 是否已存在？T3 需要此字段启 cleanup goroutine | T3 owner（`grep -rn "type Deps" /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/shared/runtime/`） |

每条调查闭合记录在 `docs/issues/2026-06-17-investigates/INVESTIGATE-WS7-NNN.md`。

---

## 14. 回滚 / Feature flag（联动 WS-15）

| Task | 风险等级 | 回滚策略 |
|---|---|---|
| T1 | 低 | revert 单 PR；行为退化为现状（leader 取消 → follower 一起 err） |
| T2 | 高 | 风险 = 误配 trusted_proxies 导致生产被拒启。WS-15 flag `flags/trust_xff.yaml`：默认 `false`；先一个 release `false`，运维确认 trusted_proxies 配置；下个 release `true`。回滚即翻 flag |
| T3 | 低 | revert 单 PR；行为退化为 inline scan（性能差但功能正确） |
| T4 | 低 | revert 单 PR |
| T5 | 中 | revert 后 caller 编译失败（Result→直接 Self）；需配套 revert caller 改动。建议 T5 land 后 24h soak 再拓宽 |
| T6 | 中 | wire 兼容性见 INVESTIGATE-WS7-001；如发现存量 f64 数据则 PR 必须含 dual-read deserializer，否则不可 land |
| T7 | 低 | revert 单 PR；行为退化为无 clamp 无 escalation |
| T8 | 中 | revert 后回归 HalfOpen 卡死现象。建议同 PR 加 metric `gw_health_halfopen_close_total` 与 `gw_health_halfopen_retrip_total` 让 SRE 观测 land 后行为 |
| T9 | 中 | revert 后 unavailable key 的 current 又开始累积；雪崩风险回归。建议 land 后 24h soak 观察 `gw_routing_weighted_rr_current_max{key}` 不再有持续上升 trend |

---

## 15. 元信息

- 起草作者：sonnet（agentId 当前会话）
- 审阅：在 PR 阶段派独立 sonnet 用 `tdd-adversarial-review.txt`（WS-12 交付）复审每 task；重点检查：源码是否真如本文件 §0 已核实表所述；ADR §X.8 备选方案是否真考虑过；HOW §X.6 字面代码是否可编译。
- 双仓 mirror：本文件路径 `AILinkMax-Go/docs/superpowers/plans/2026-06-17-WS-7-concurrency-numeric-safety.md`；Rust 仓 mirror `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-7-concurrency-numeric-safety.md`，**byte-identical 除 H1 标题外的所有内容**。
- 版本：v1 草案 2026-06-17。

— 文档结束 —
