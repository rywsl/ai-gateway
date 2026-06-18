# WS-3 TDD 子计划 · 内存有界不变式（Bounded Memory Invariant）

> **父计划**：`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §4 WS-3 + §5；`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0（重要 1+2）+ §4 WS-3 REV-FINAL-V1..V7。
> **路径**：`AILinkMax-Go/docs/superpowers/plans/2026-06-17-WS-3-bounded-memory.md`（双仓 mirror 到 `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-3-bounded-memory.md`）。
> **起草**：2026-06-17。
> **状态**：候选 · 待对抗复审（参见 §99）。

---

## 阅读 / 执行守则（强制）

1. **本文档的"修复方案"全部是候选**。v2 §0 重要 2 已抽样反证过原审计声明（如 H9 的 `api_keys.deleted_at`）。**所有 file:line 必须执行者用 Read 工具亲自打开核对**；不一致以源码为准并在本文件 §98 留痕。
2. 每 task 8 段格式（v2 §3 7 段 + 新增"源码验证"段）：
   - 【是什么】 What it IS
   - 【不是什么】 What it is NOT
   - 【为什么】 Why bug exists（根因 + 引用 finding ID）
   - 【做什么】 DO（动作顺序）
   - 【不做什么】 DON'T（诱人陷阱）
   - 【怎么做 literal】 HOW（literal code + 文件:行）
   - 【源码验证】 Sonnet 输出是假设；本段强制要求执行前 Read 验证
   - 【如何验证】 VERIFY（命令 + 期望输出）
3. 每条修复至少列 1 个 ADR 备选 + 推荐 + 理由。
4. 受众：**不假定熟悉 Rust 或 Go**。所有命令显式给路径、显式给 grep 字面值。
5. 默认不创建文档；本文件本身是 v2 §6 A 阶段要求的产物，例外。

---

## 0. WS-3 范围（一句话）

为所有"buffer / map / channel / cache / accumulator / metric label"加 cap、加 sweep、加 backpressure、加显式 fail-policy，并消除任何在恶意上游 / 慢客户端 / 长尾 ID 下可线性 / 累积爆 RSS 的路径。**这是"内存被打爆"类问题的根治**，不是单点止血。

---

## 1. 覆盖矩阵（finding ID → task ID）

> 取自 v1 §4 WS-3 与 §5（5.1/5.2/5.3/5.4），加 v2 §4 WS-3 REV-FINAL-V1..V7。任何遗漏即子计划失败。

| Source finding | v1 §5 行 | v2 修订 | 本子计划 task |
|---|---|---|---|
| Go H6（archive codec 解压无 size cap） | §5.1 H6 | — | T-W3-01 |
| Go G14（OAuth bind-token 内存 map） | §5.2 G14 | REV-FINAL-V18 共用 fail-closed | T-W3-02 |
| Go S5（rate limiter cleanup O(N) 持锁） | §5.2 S5 | REV-FINAL-H7.3（WS-7） | T-W3-03 |
| Rust Proxy-1（upstream body accumulator 无 cap） | §5.3 Proxy-1 | REV-FINAL-V1（policy=RejectRequest） | T-W3-04, T-W3-05 |
| Rust Proxy-2（sse_leftover 无 cap） | §5.3 Proxy-2 | REV-FINAL-V1（policy=Truncate） | T-W3-04, T-W3-06 |
| Rust Proxy-3（stream_probe.buffer 无 cap） | §5.3 Proxy-3 | REV-FINAL-V1（policy=RejectRequest） | T-W3-04, T-W3-07 |
| Rust Proxy-8（cross-protocol body buffer 无 cap） | §5.3 Proxy-8 | REV-FINAL-V1 | T-W3-04, T-W3-05 |
| Rust Codec-4（codec state max_accumulated_bytes / pending_function_calls 无 cap） | §5.3 Codec-4 | REV-FINAL-V3 | T-W3-08 |
| Rust Routing-1（InFlightStore by_key DashMap 无 sweep） | §5.3 Routing-1 | REV-FINAL-V2（谓词：count==0 才 evict） | T-W3-09, T-W3-10 |
| Rust Routing-2（LatencyTracker 无 sweep） | §5.3 Routing-2 | REV-FINAL-V2 | T-W3-09, T-W3-11 |
| Rust Routing-3（AffinityManager 无 sweep） | §5.3 Routing-3 | REV-FINAL-V2 | T-W3-09, T-W3-12 |
| Rust Routing-4（ModelFrequencyTracker / ReservationGlobalView 无 sweep） | §5.3 Routing-4 | REV-FINAL-V2 | T-W3-09, T-W3-13 |
| Rust Routing-7（DashMap obs 无 size/evicted metric） | §5.3 Routing-7 | REV-FINAL-V7 共用 | T-W3-14 |
| Rust R14（Redos：v2 已确认 RE2 linear-time，**删静态估算**） | §5.4 R14 | **REV-FINAL-V5** | T-W3-15 |
| Rust R15（Prom request_id 高基数 label） | §5.4 R15 | **REV-FINAL-V7** | T-W3-16 |
| Rust R19（counter_store EXPIRE 吞错 + 无 sweeper） | §5.4 R19 | **REV-FINAL-V6**（一次性迁移脚本而非常驻 loop） | T-W3-17, T-W3-18 |
| REV-FINAL-V1（BoundedBuf trait + policy-per-buffer） | — | v2 §4 WS-3 | T-W3-04（基础设施） |
| REV-FINAL-V2（Sweepable 谓词具体定义） | — | v2 §4 WS-3 | T-W3-09（基础设施） |
| REV-FINAL-V3（codec cap 触发 SSE [ERROR] + settler refund） | — | v2 §4 WS-3 | T-W3-08 |
| REV-FINAL-V4（IpRateLimit / AuthFailureIpLimiter sweep + Redis email verify） | — | v2 §4 WS-3 | T-W3-19, T-W3-20 |
| REV-FINAL-V5（Redos 删静态估算 + 阈值 10→1） | — | v2 §4 WS-3 | T-W3-15 |
| REV-FINAL-V6（counter_store 一次性迁移脚本） | — | v2 §4 WS-3 | T-W3-18 |
| REV-FINAL-V7（高基数 label 杀名单 + CI lint） | — | v2 §4 WS-3 | T-W3-16, T-W3-21 |

**总 task 数：21。** 上述 ID 全数有 task 对应。任何后续发现走 v1 §8 持续发现机制纳入本文件 §98。

---

## 2. 强依赖

- **T-W3-04（BoundedBuf trait）** 阻塞 T-W3-05/06/07/08。先落 trait，再落每个 buffer 的接入。
- **T-W3-09（Sweepable trait + 注册中心）** 阻塞 T-W3-10/11/12/13。
- **T-W3-16（高基数 label 删除）** 与 WS-10 obs migration（迁移 Grafana 面板）协同；属于 WS-3 P0 子集。
- **T-W3-21（CI metric-label allowlist lint）** 由 WS-12 owner 提供脚本框架；WS-3 在框架上加规则。
- **T-W3-17（counter_store EXPIRE retry）** 与 v1 R19 直接相关；**T-W3-18 是 v2 REV-FINAL-V6 对 v1 "常驻 sweeper" 的修正**（一次性 PR-deploy-后-48h 迁移脚本）。两者同 PR。
- **T-W3-15（Redos）** 不依赖其他 WS-3 task，但 **REV-FINAL-V5 确认 RE2 linear-time，所以删整段静态估算**；此处仅做 (i) tracker 阈值 10→1，(ii) is_disabled 检查移到 match 之前。

---

## 3. 任务清单（21 项 · 每项 8 段格式）

---

### T-W3-01 · Go archive codec lz4/zstd 加 size cap（H6）

【是什么 What it IS】
- 一个 Go 改动，文件 `AILinkMax-Go/modules/archive/business/codec.go`，函数 `DecodeEntry`（已 read：第 80-112 行 `switch enc` 分支）。
- 在 `lz4+json`、`zstd+json` 两个解码分支前置插入"压缩后 + 解压后双侧 size cap"；超 cap 返 `ErrPayloadTooLarge`，不分配内存。
- 引入新 export `MaxDecodedBytes int64`（默认 16 MiB，可配）。

【不是什么 What it is NOT】
- ❌ 不修改 `raw+json` 分支（已是 1:1 原文，无放大风险）。
- ❌ 不改 stream entry 读取层（drainer.go）；本 task 仅 codec。
- ❌ 不引入新 encoding；不重命名常量。
- ❌ 不对 Rust 侧 `crates/gw-ledger/src/encoding.rs` 做改动（属 Rust 侧 emit 路径，超 WS-3 范围）。

【为什么 Why bug exists】
- 根因（H6）：`lz4+json` 用 `binary.LittleEndian.Uint32(src[:4])` 直接拿"声明的解压长度"做 `make([]byte, dstLen)`。攻击者构造一个 4 字节头 `0xFFFFFFFF`（4 GiB）+ 几 KB 的非法 LZ4 块即可让一个 entry 触发 OOM。
- zstd 分支用 `io.ReadAll(d)`，无 cap。`zstd` 极限解压比 ~10000:1，几 KB 输入可膨胀几十 MB。
- 配合 H6 原描述（v1 §2.1 H6 一句话："archive codec 解压无 size cap"），加 cap 是 mitigation。

【做什么 DO】
1. 在 codec.go 顶部新增 `const DefaultMaxDecodedBytes int64 = 16 << 20  // 16 MiB`。
2. 把 `DecodeEntry` 的签名从全局函数改为 `(d *Decoder) DecodeEntry(...)`；或保持函数式但加 `WithMaxDecodedBytes(int64)` opt。**ADR 推荐方案**：函数式 + opts，避免动 caller（drainer.go）签名。
3. `lz4+json` 分支：在 `dstLen := binary.LittleEndian.Uint32(src[:4])` 之后立即：`if int64(dstLen) > maxDecoded { return nil, ErrPayloadTooLarge }`。
4. `zstd+json` 分支：用 `io.LimitReader(d, maxDecoded+1)` + 读后判断 `n > maxDecoded`。
5. 新增 `var ErrPayloadTooLarge = errors.New("archive: decoded payload exceeds cap")`。
6. 加 metric counter `archive_decode_rejected_total{encoding,reason="size_cap"}`（走 `shared/platform/metrics`）。
7. caller drainer 收到 `ErrPayloadTooLarge` → 不 panic、不 retry、计数 + skip + 写 audit log。

【不做什么 DON'T】
- ❌ 不要把 cap 写死在 `MaxBytesReader`（HTTP 层，与 stream entry 解码层无关）。
- ❌ 不要 default 32 MiB 或更大——Rust 侧 spill_root 文件 1 entry 上限就在这量级，cap 必须比之小且操作员可配。
- ❌ 不要在 zstd 解压完才检查 size（`io.ReadAll` 已经把内存占满）；必须 `io.LimitReader`。
- ❌ 不要对 `raw+json` 也加 cap——`raw+json` 已经是 plain bytes，cap 改在 entry-read 层而不是 codec 层。

【怎么做 HOW (具体)】

```go
// AILinkMax-Go/modules/archive/business/codec.go
package business

import (
    // ... existing imports ...
    "github.com/cnxx-tech/ailinkmax-go/shared/platform/metrics"
)

const DefaultMaxDecodedBytes int64 = 16 << 20 // 16 MiB

var ErrPayloadTooLarge = errors.New("archive: decoded payload exceeds cap")

type decodeOpts struct {
    maxDecoded int64
}

type DecodeOption func(*decodeOpts)

func WithMaxDecodedBytes(n int64) DecodeOption {
    return func(o *decodeOpts) { o.maxDecoded = n }
}

func decodePayload(enc string, payload []byte, opts ...DecodeOption) ([]byte, error) {
    o := decodeOpts{maxDecoded: DefaultMaxDecodedBytes}
    for _, fn := range opts { fn(&o) }
    switch enc {
    case "raw+json":
        return payload, nil
    case "lz4+json":
        src := payload
        if len(src) < 4 {
            return nil, fmt.Errorf("archive: lz4 decode: payload too short ...")
        }
        dstLen := binary.LittleEndian.Uint32(src[:4])
        if int64(dstLen) > o.maxDecoded {
            metrics.Counter("archive_decode_rejected_total",
                "encoding", "lz4+json", "reason", "size_cap").Inc()
            return nil, ErrPayloadTooLarge
        }
        dst := make([]byte, dstLen)
        n, err := lz4.UncompressBlock(src[4:], dst)
        if err != nil {
            return nil, fmt.Errorf("archive: lz4 block decode: %w", err)
        }
        return dst[:n], nil
    case "zstd+json":
        d, err := zstd.NewReader(bytes.NewReader(payload))
        if err != nil {
            return nil, fmt.Errorf("archive: zstd reader: %w", err)
        }
        defer d.Close()
        lr := io.LimitReader(d, o.maxDecoded+1)
        out, err := io.ReadAll(lr)
        if err != nil {
            return nil, fmt.Errorf("archive: zstd decode: %w", err)
        }
        if int64(len(out)) > o.maxDecoded {
            metrics.Counter("archive_decode_rejected_total",
                "encoding", "zstd+json", "reason", "size_cap").Inc()
            return nil, ErrPayloadTooLarge
        }
        return out, nil
    default:
        return nil, fmt.Errorf("%w: %q", ErrUnknownEncoding, enc)
    }
}
```

**ADR：备选 A=参数注入；备选 B=全局 var；推荐 A。** B 全局 var 测试更易污染、配置漂移、且不支持多 binary 不同 cap；A 让 caller 显式声明意图。

【源码验证 file:line Read】
- Read `AILinkMax-Go/modules/archive/business/codec.go:80-112` 核对 `switch enc` 实际分支序、`UncompressBlock` 用法、`zstd.NewReader` + `io.ReadAll` 用法。本文档已 Read 一次验证（2026-06-17，确认 `dstLen := binary.LittleEndian.Uint32(src[:4])`、`dst := make([]byte, dstLen)` 字面存在）。
- Read `AILinkMax-Go/modules/archive/business/drainer.go` 找出 `decodePayload` / `DecodeEntry` 实际调用点，确认 caller 是否能接受新 `ErrPayloadTooLarge`（若 caller 把 err 一并往上抛 → 引入 audit log）。
- Read `AILinkMax-Go/shared/platform/metrics/` 包名与导入路径（实际包名以仓库为准；若不存在则用 `expvar` 或本仓库现有 metrics shim）。

【如何验证 VERIFY】
1. 单元：`cd AILinkMax-Go && go test ./modules/archive/business/ -run TestDecodePayload_LZ4SizeCap -race -count=10`
   - 期望：构造 4 字节头 `\xff\xff\xff\xff` + 任意 LZ4 块 → `errors.Is(err, ErrPayloadTooLarge)` 为 true；过程内存峰值 < 1 MiB（`testing.AllocsPerRun` < 1024）。
2. 单元：构造 zstd "12 KB 输入 → 声明 256 MB 输出" payload → `errors.Is(err, ErrPayloadTooLarge)`；`go test -benchmem -run=^$ -bench BenchmarkZstdCap` 显示 `B/op` 在 cap 上界以内（≤ 16 MiB + overhead）。
3. 集成：drainer e2e（`drainer_e2e_test.go` 内加 fixture 写一条恶意 entry）→ drainer Run 一周期后 `archive_decode_rejected_total{reason="size_cap"}` 计数 = 1，entry 被 skip 不 panic。
4. 输出文件：`logs/W3-T01-verify.txt` 包含 `PASS` + 内存峰值断言。

---

### T-W3-02 · Go OAuth bind-token 迁移到 Redis HSET + 5 min TTL（G14）

【是什么 What it IS】
- 一个 Go 改动，文件 `AILinkMax-Go/modules/oauth/business/service.go`（已 read：第 320-379 行，`CreateBindToken` / `ConsumeBindToken` / `newMemBindTokenStore`）。
- 把 in-process `bindTokenStore`（基于 `map[string]bindTokenEntry`）换成 Redis 实现 `redisBindTokenStore`；保留 interface，新增 contracts 桩。
- TTL 由代码内 `s.now().Add(10*time.Minute)` 改为 v2 §4 WS-3 描述的 **5 min**（同时缩小窗口、与原案 10min 不一致以 v2 为准）。

【不是什么 What it is NOT】
- ❌ 不改 OAuth 业务流程（CreateBindToken / ConsumeBindToken 调用方不变）。
- ❌ 不新增 PG 表（bind-token 是短寿数据，Redis 即可）。
- ❌ 不删除 mem store（保留为测试 fixture / 单进程开发模式，由 wiring 选择）。
- ❌ 不修改 OAuth provider 表、用户绑定关系（在 store.UpsertLink 路径）。

【为什么 Why bug exists】
- 根因（G14）：bind-token 存活在单进程 `sync.Mutex` + `map`。在多副本 LB 后 Pod A 发的 state，回调可能命中 Pod B → consume 失败 → CSRF 防护退化为"运气好命中同 Pod"。
- 同时 in-memory 无 janitor，TTL 仅在 `consume` 路径懒过期；攻击者大量调 CreateBindToken 即可单调增内存。
- 解决：用 Redis HSET（或更直观的 string + EX）跨 Pod 一致；EX 自动过期。

【做什么 DO】
1. 在 `shared/contracts/oauth.go`（若不存在则新建）声明 interface：
   ```go
   type BindTokenStore interface {
       Put(ctx context.Context, token, userID, providerType string, ttl time.Duration) error
       Consume(ctx context.Context, token string) (userID, providerType string, ok bool, err error)
   }
   ```
2. 现 `bindTokenStore` 改名为 `memBindTokenStore`，实现 `BindTokenStore`；保留供测试 / 单进程模式 wiring。
3. 新增 `redisBindTokenStore`：
   - key 形如 `oauth:bindtok:{token}` 存 hash `{user_id, provider_type, created_at_ms}`。
   - Put：`HSET key user_id ... provider_type ... created_at_ms ...` + `PEXPIRE key 300000`（5 min）。原子用 Lua：`redis.call('HSET', KEYS[1], ...); redis.call('PEXPIRE', KEYS[1], 300000); return 1`。
   - Consume：Lua `local v = redis.call('HGETALL', KEYS[1]); if #v == 0 then return nil end; redis.call('DEL', KEYS[1]); return v`。
4. `service.go` 接受 `BindTokenStore` 接口（不再 hardcode mem）；`module.go` wiring：`if redisClient != nil { return newRedisBindTokenStore(rdb) } else { return newMemBindTokenStore() }`。
5. `CreateBindToken` 调 `s.bindTok.Put(ctx, tok, userID, providerType, 5*time.Minute)`。Redis 失败 → 返回 503（与 REV-FINAL-V18 一致："禁止 in-mem fallback"）。

【不做什么 DON'T】
- ❌ 不要 HSET + EXPIRE 不在 Lua / pipeline 内（与 R19 / R21 同根问题，HSET 成功 EXPIRE 失败 → 永生 key）。
- ❌ 不要把 TTL 加到 10 min 或更大；CSRF 防护窗口越小越好。
- ❌ 不要 mem fallback "redis 故障就退化到内存"——退化即漏 G14；操作上是 503 + 告警。
- ❌ 不要把 token 字面值写日志（CSRF token = secret）；只 hash 后写。

【怎么做 HOW (具体)】

```go
// AILinkMax-Go/modules/oauth/business/bindtoken_redis.go
package business

import (
    "context"
    "fmt"
    "time"
    "github.com/redis/go-redis/v9"
)

type redisBindTokenStore struct{ rdb redis.UniversalClient }

const bindTokenPutLua = `
redis.call('HSET', KEYS[1],
  'user_id', ARGV[1],
  'provider_type', ARGV[2],
  'created_at_ms', ARGV[3])
redis.call('PEXPIRE', KEYS[1], ARGV[4])
return 1
`

const bindTokenConsumeLua = `
local v = redis.call('HGETALL', KEYS[1])
if #v == 0 then return nil end
redis.call('DEL', KEYS[1])
return v
`

func (s *redisBindTokenStore) Put(ctx context.Context, token, userID, providerType string, ttl time.Duration) error {
    key := "oauth:bindtok:" + token
    nowMs := time.Now().UnixMilli()
    ttlMs := ttl.Milliseconds()
    res := s.rdb.Eval(ctx, bindTokenPutLua, []string{key}, userID, providerType, nowMs, ttlMs)
    return res.Err()
}

func (s *redisBindTokenStore) Consume(ctx context.Context, token string) (string, string, bool, error) {
    key := "oauth:bindtok:" + token
    res, err := s.rdb.Eval(ctx, bindTokenConsumeLua, []string{key}).Result()
    if err == redis.Nil || res == nil { return "", "", false, nil }
    if err != nil { return "", "", false, err }
    fields, ok := res.([]interface{})
    if !ok { return "", "", false, fmt.Errorf("unexpected EVAL return shape") }
    m := map[string]string{}
    for i := 0; i+1 < len(fields); i += 2 {
        k, _ := fields[i].(string); v, _ := fields[i+1].(string)
        m[k] = v
    }
    return m["user_id"], m["provider_type"], true, nil
}
```

`service.go` 修改：
```go
// service.CreateBindToken — line 332 起，替换 s.bindTok.put(...)
if err := s.bindTok.Put(ctx, tok, userID, providerType, 5*time.Minute); err != nil {
    return "", fmt.Errorf("oauth bindtok put: %w", err) // caller 包 503
}
return tok, nil
```

**ADR：备选 A=Redis HSET+Lua（推荐）；备选 B=Redis 单 SET key={token} value=JSON(...) + EX；备选 C=PG 表 + cron。**
- A 与 v2 §4 一致；HSET 可分别取字段，便于调试。
- B 更简单但 JSON 解析多一步；TTL 仍要走 SET EX。
- C 引入新表 + janitor，过度工程化；CSRF token 用 Redis 是 industry standard。

【源码验证 file:line Read】
- 已 Read `AILinkMax-Go/modules/oauth/business/service.go:320-379`，确认 `s.bindTok.put(...)` 字面用 `s.now().Add(10*time.Minute)`、`newMemBindTokenStore` 返 `bindTokenStore{...}`。
- Read `AILinkMax-Go/modules/oauth/business/module.go`（若存在）确认 wiring 注入点。
- Read `AILinkMax-Go/modules/oauth/api/routes.go` 找出 `CreateBindToken` / `ConsumeBindToken` HTTP 入口，确认错误码到 503 的 mapping。
- 检查 `shared/contracts/oauth.go` 是否存在；不存在则新建。
- 验 Redis client 引入路径：`grep -rn "redis.UniversalClient" AILinkMax-Go/modules/oauth/` 找现有用法或在 module.go 注入。

【如何验证 VERIFY】
1. 单元：`go test ./modules/oauth/business/ -run TestRedisBindTokenStore -race`，用 miniredis fixture 验 Put → Consume 返 (userID, providerType, true) → 再 Consume 返 (.., false)；PEXPIRE 后过 5min Consume 返 (.., false)。
2. 集成：`go test ./modules/oauth/api/ -run TestBindToken_CrossPodConsistency -race`，模拟 2 Service 实例（共享 Redis）→ A.Create 返 token，B.Consume 成功；CSRF 跨 Pod PASS。
3. e2e：`make test-frontend`（contracts 中 `/api/oauth/bind` → consumer）必须仍绿。
4. 注入：Redis down 时 Create → 503；Consume → 503；**禁止 200 + fallback to mem**（grep `module.go` 确认无 fallback 分支）。
5. 输出：`logs/W3-T02-verify.txt`。

---

### T-W3-03 · Go API rate limiter cap + LRU + 独立 cleanup goroutine（S5）

【是什么 What it IS】
- 一个 Go 改动，文件 `AILinkMax-Go/shared/runtime/host.go`（已 read：第 95-149 行，`apiRateLimiter` 结构、`allow` 方法）。
- 把现有"每请求持锁全扫 map 删 0 项"的 cleanup（line 130-136）拆为 (i) 独立 goroutine 周期扫，(ii) `allow` 内只做插 / 加；(iii) 引入硬 cap + LRU 淘汰（含 cap warn metric）。
- 与 WS-7 REV-FINAL-H7.3 同一根因；本 task 是 WS-3 内存维度负责，WS-7 是并发 / 死锁维度负责（同 PR 合并）。

【不是什么 What it is NOT】
- ❌ 不引入 chi/httprate 第三方依赖。
- ❌ 不改 `DefaultAPIRateLimitRequests = 120` 数值。
- ❌ 不改 clientIP 逻辑（X-Forwarded-For 处置归 WS-5 REV-FINAL-V22 + WS-7 REV-FINAL-H7.2）。
- ❌ 不改限速窗口语义（仍 fixed-window，与现有测试一致）。

【为什么 Why bug exists】
- 根因（S5）：`allow` 内 `if len(l.hits) > l.limit*16` 进入"全扫删过期"。在 DoS 下每次 `allow` 都触发，遍历 `len(hits)` 在持 `l.mu` 下做 O(N)；同时若全部 IP 都在窗口内（攻击者保持 1s 内），删 0 项但扫一遍——每请求都 O(N) 重复。锁持有时间随 N 线性，正常请求被阻塞 → DoS 放大。
- 同时 map 无 cap：百万 unique IP 攻击 → map 增长到 OOM。
- 解决：(i) cleanup 独立 goroutine + 双相（read-lock 扫候选 → write-lock 批删）；(ii) `allow` 内仅 cap 检查 + LRU eviction，不扫。

【做什么 DO】
1. `apiRateLimiter` 加字段：`lru *list.List`（双向链表），`elem map[string]*list.Element`，`cap int`（默认 `limit*64` ≈ 7680）。
2. `allow` 改：
   - 命中 → 更新计数 + `l.lru.MoveToFront(e)`。
   - miss → 若 `l.lru.Len() >= l.cap` → `l.lru.Remove(l.lru.Back())` + del map + `apirl_evicted_total{reason="lru_cap"}++`；插入 front。
   - 不再扫全 map。
3. 新增 goroutine `cleanup(ctx)` 在 `BuildRouter` 启动：每 `window/2` tick：
   - 阶段 1（read-lock）：遍历 lru 从 back，收集 `now-state.start >= window` 的 key 到本地 `toDel`，cap 单次 1024 个，break。
   - 阶段 2（write-lock）：批 `delete(map, k)` + `l.lru.Remove(e)`。
4. 走 `runtime.Supervised`（同 WS-4 REV-FINAL-V8 接口；若尚未 land，先用裸 goroutine + panic recover，加 TODO 注释跟 WS-4 PR 联动）。
5. 暴露 metric `apirl_size`（gauge）+ `apirl_evicted_total{reason}`（counter）。

【不做什么 DON'T】
- ❌ 不在 `allow` 内 `for range l.hits` 遍历——根因就是这条。
- ❌ 不用全局裸 goroutine 启 cleanup（panic 进程死）；必须 Supervised。
- ❌ 不用 `delete + iterate` 同时（Go map 迭代删元素行为安全但 cleanup 阶段 1 仅 read，阶段 2 单独 write，避免持锁时间膨胀）。
- ❌ 不要把 cap 设到 1M（足够 botnet 打爆）；`limit*64` 是经验值，可配。

【怎么做 HOW (具体)】

```go
// AILinkMax-Go/shared/runtime/host.go (extend apiRateLimiter)
import "container/list"

type apiRateLimiter struct {
    mu     sync.Mutex
    limit  int
    window time.Duration
    cap    int
    hits   map[string]*list.Element
    lru    *list.List
}

type rateEntry struct {
    ip    string
    state rateWindow
}

func newAPIRateLimiter(limit int, window time.Duration) *apiRateLimiter {
    return &apiRateLimiter{
        limit:  limit,
        window: window,
        cap:    limit * 64,
        hits:   make(map[string]*list.Element),
        lru:    list.New(),
    }
}

func (l *apiRateLimiter) allow(ip string, now time.Time) bool {
    l.mu.Lock()
    defer l.mu.Unlock()

    if e, ok := l.hits[ip]; ok {
        re := e.Value.(*rateEntry)
        if re.state.start.IsZero() || now.Sub(re.state.start) >= l.window {
            re.state = rateWindow{start: now, count: 1}
            l.lru.MoveToFront(e)
            return true
        }
        if re.state.count >= l.limit {
            return false
        }
        re.state.count++
        l.lru.MoveToFront(e)
        return true
    }

    // miss + cap check
    if l.lru.Len() >= l.cap {
        back := l.lru.Back()
        if back != nil {
            re := back.Value.(*rateEntry)
            delete(l.hits, re.ip)
            l.lru.Remove(back)
            metrics.Counter("apirl_evicted_total", "reason", "lru_cap").Inc()
        }
    }
    e := l.lru.PushFront(&rateEntry{ip: ip, state: rateWindow{start: now, count: 1}})
    l.hits[ip] = e
    return true
}

// runs in supervised goroutine
func (l *apiRateLimiter) cleanupLoop(ctx context.Context) {
    t := time.NewTicker(l.window / 2)
    defer t.Stop()
    for {
        select {
        case <-ctx.Done():
            return
        case <-t.C:
            l.cleanupOnce(time.Now())
        }
    }
}

func (l *apiRateLimiter) cleanupOnce(now time.Time) {
    const maxPerTick = 1024
    var toDel []string
    l.mu.Lock()
    e := l.lru.Back()
    for e != nil && len(toDel) < maxPerTick {
        re := e.Value.(*rateEntry)
        prev := e.Prev()
        if now.Sub(re.state.start) >= l.window {
            toDel = append(toDel, re.ip)
            delete(l.hits, re.ip)
            l.lru.Remove(e)
        } else {
            break // LRU back = oldest used; once not expired, none further back is
        }
        e = prev
    }
    l.mu.Unlock()
    // metric without lock
    if len(toDel) > 0 {
        metrics.Counter("apirl_evicted_total", "reason", "ttl").Add(float64(len(toDel)))
    }
}
```

**ADR：备选 A=container/list LRU（推荐）；备选 B=排序定时扫；备选 C=`golang.org/x/time/rate.Limiter` per-ip + size-bound sync.Map。**
- A 与现有 map 自然集成、O(1) 操作。
- B 需要堆排，复杂度上升。
- C 引入新依赖（已被 P5 限制：必须查证最新版且不必引入），且 `sync.Map` 没有原生 cap，仍要手写。

【源码验证 file:line Read】
- 已 Read `AILinkMax-Go/shared/runtime/host.go:95-149`，确认 `allow` 字面用 `len(l.hits) > l.limit*16`、`for key, state := range l.hits`。
- Read `AILinkMax-Go/shared/runtime/host_test.go`（已知 line 103-107 有 `TestBuildRouter_RateLimitsAPIByClientIP`）确认现有契约——新代码不能 break：发 121 个请求触发 429 的语义保持。
- 确认 `container/list` 标准库（无第三方依赖）。
- 确认 `runtime.Supervised` 是否在 WS-4 已 land；如未 land，留 TODO 注释；本 PR 暂用 `panic recover + goroutine`，并与 WS-4 PR 同里程碑（v2 §6.1 D1）。

【如何验证 VERIFY】
1. 单元：`go test ./shared/runtime/ -run TestRateLimiter -race -count=50`
   - 验：100k unique IP × 1 req → 内存上限 = cap * sizeof(rateEntry) + overhead（~ 2 MiB），`apirl_evicted_total` > 0。
2. 微基准：`go test -bench BenchmarkRateLimiterAllowAtSaturation -benchmem -count=5` → `ns/op` 在 cap 上界附近平稳不增长（vs 旧实现在 N=cap*16 时 ns/op 线性增）。
3. 集成：`tests/e2e/dos_test.go`（新）模拟 50k unique IP 50s → `wallets/sessions` API P99 不退化（同 baseline ±20%）。
4. 现有契约：`TestBuildRouter_RateLimitsAPIByClientIP` 仍 PASS。
5. 输出：`logs/W3-T03-verify.txt`。

---

### T-W3-04 · Rust BoundedBuf trait + policy enum + metrics 基础设施（REV-FINAL-V1 基础）

【是什么 What it IS】
- 一个 Rust 改动，新增 module `ailinkmax-gateway/crates/gw-core/src/bounded_buf.rs`（或 `gw-proxy/src/bounded_buf.rs`——以 caller 跨 crate 情况决定）。
- 一个 trait + 一个 policy enum：
  ```rust
  pub enum OverflowPolicy { Truncate, RejectRequest, DropOldest }
  pub trait BoundedBuf {
      fn cap(&self) -> usize;
      fn policy(&self) -> OverflowPolicy;
      fn try_push(&mut self, chunk: &[u8]) -> Result<(), BoundedBufError>;
      fn len(&self) -> usize;
  }
  pub enum BoundedBufError {
      RequestRejected { cap: usize, attempted: usize, buf_name: &'static str },
      Truncated { cap: usize, dropped: usize, buf_name: &'static str },
  }
  ```
- 配套 Prometheus metrics：`gw_bounded_buf_overflow_total{buf,policy}`、`gw_bounded_buf_size{buf}`。
- 配置注入校验：每 buffer 必须显式声明 policy（boot-time fail panic 若漏）。

【不是什么 What it is NOT】
- ❌ 不实现具体 buffer 接入（那是 T-W3-05/06/07/08）。
- ❌ 不动 codec 内部状态机；仅提供基础设施。
- ❌ 不发布到 gw-runtime（避免 gw-core/gw-proxy/gw-codec 间循环依赖；放最少跨 crate 共享点 `gw-core` 内）。
- ❌ 不替代 `BytesMut`；仅 wrap。

【为什么 Why bug exists】
- 根因（Proxy-1/2/3/8、Codec-4）：buffer 在多处零散；每处单独写 cap 检查会 (i) 漏，(ii) policy 不一致（有的 truncate 有的 reject 有的 silent drop）。
- v2 REV-FINAL-V1 关键：**policy 必须 per-buffer**。`upstream_body_accum` truncate → codec 收到截断 JSON 会假装解析成功 → 错费用；`sse_leftover` truncate 可接受（只丢一些事件）；`stream_probe.buffer` truncate 让 probe 误判 → 必须 RejectRequest。
- 解决：trait + enum 让每 buffer 静态声明 policy，CI 可校验。

【做什么 DO】
1. 新建 `crates/gw-core/src/bounded_buf.rs`。
2. 实现 trait 与 enum，body 见 §怎么做。
3. 配置 layer：`config/default.toml` 加 `[bounded_buf]` table，per-buffer key（`upstream_body_accum_cap`、`upstream_body_accum_policy = "RejectRequest"` 等）；`gw-gateway/src/config.rs` 加 struct + figment 解析。
4. boot-time 校验 `BoundedBufConfig::validate(&cfg) -> Result<(), BoundedBufConfigError>`，在 `main.rs` 中调，错则 panic。
5. 注册 Prometheus 描述（`crates/gw-core/src/metrics.rs` 或现有等价位置）。

【不做什么 DON'T】
- ❌ 不要 trait method 用 `&mut [u8]` 返回内部 buffer 引用（生命周期纠缠）；用 `try_push` push 模型。
- ❌ 不要默认 policy 是 `Truncate`——默认必须是 `RejectRequest`（最保守）；显式声明 truncate 才能用。
- ❌ 不要把 `cap` 设为常量；必须 figment 可注入。
- ❌ 不要把 metric label 取为 `request_id`（直接违 R15 / REV-FINAL-V7）；label 取静态 buf 名 + policy enum。

【怎么做 HOW (具体)】

```rust
// crates/gw-core/src/bounded_buf.rs
use bytes::BytesMut;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverflowPolicy { Truncate, RejectRequest, DropOldest }

#[derive(Debug, thiserror::Error)]
pub enum BoundedBufError {
    #[error("buffer `{buf_name}` would exceed cap {cap} (attempted {attempted})")]
    RequestRejected { cap: usize, attempted: usize, buf_name: &'static str },
    #[error("buffer `{buf_name}` truncated: dropped {dropped} bytes at cap {cap}")]
    Truncated { cap: usize, dropped: usize, buf_name: &'static str },
}

pub trait BoundedBuf {
    fn cap(&self) -> usize;
    fn policy(&self) -> OverflowPolicy;
    fn name(&self) -> &'static str;
    fn len(&self) -> usize;
    fn try_push(&mut self, chunk: &[u8]) -> Result<(), BoundedBufError>;
}

pub struct BytesMutBounded {
    inner: BytesMut,
    cap: usize,
    policy: OverflowPolicy,
    name: &'static str,
}

impl BytesMutBounded {
    pub fn new(name: &'static str, cap: usize, policy: OverflowPolicy) -> Self {
        Self { inner: BytesMut::new(), cap, policy, name }
    }
    pub fn freeze(self) -> bytes::Bytes { self.inner.freeze() }
    pub fn as_ref(&self) -> &[u8] { &self.inner }
}

impl BoundedBuf for BytesMutBounded {
    fn cap(&self) -> usize { self.cap }
    fn policy(&self) -> OverflowPolicy { self.policy }
    fn name(&self) -> &'static str { self.name }
    fn len(&self) -> usize { self.inner.len() }

    fn try_push(&mut self, chunk: &[u8]) -> Result<(), BoundedBufError> {
        let new_len = self.inner.len().saturating_add(chunk.len());
        if new_len <= self.cap {
            self.inner.extend_from_slice(chunk);
            metrics::gauge!("gw_bounded_buf_size", "buf" => self.name).set(self.inner.len() as f64);
            return Ok(());
        }
        match self.policy {
            OverflowPolicy::RejectRequest => {
                metrics::counter!("gw_bounded_buf_overflow_total",
                    "buf" => self.name, "policy" => "reject").increment(1);
                Err(BoundedBufError::RequestRejected {
                    cap: self.cap, attempted: new_len, buf_name: self.name,
                })
            }
            OverflowPolicy::Truncate => {
                let room = self.cap.saturating_sub(self.inner.len());
                self.inner.extend_from_slice(&chunk[..room]);
                let dropped = chunk.len() - room;
                metrics::counter!("gw_bounded_buf_overflow_total",
                    "buf" => self.name, "policy" => "truncate").increment(1);
                Err(BoundedBufError::Truncated {
                    cap: self.cap, dropped, buf_name: self.name,
                })
            }
            OverflowPolicy::DropOldest => {
                let need = new_len - self.cap;
                let _ = self.inner.split_to(need);
                self.inner.extend_from_slice(chunk);
                metrics::counter!("gw_bounded_buf_overflow_total",
                    "buf" => self.name, "policy" => "drop_oldest").increment(1);
                Ok(())
            }
        }
    }
}
```

config（`gw-gateway/src/config.rs`）：
```rust
#[derive(Deserialize, Debug)]
pub struct BoundedBufCfg {
    pub upstream_body_accum_cap_bytes: usize,
    pub upstream_body_accum_policy: String, // "RejectRequest"
    pub sse_leftover_cap_bytes: usize,
    pub sse_leftover_policy: String,        // "Truncate"
    pub stream_probe_buffer_cap_bytes: usize,
    pub stream_probe_buffer_policy: String, // "RejectRequest"
    pub codec_max_accumulated_bytes: usize, // see T-W3-08
    pub codec_pending_function_calls_cap: usize, // see T-W3-08
}

impl BoundedBufCfg {
    pub fn validate(&self) -> anyhow::Result<()> {
        for (k, v) in [
            ("upstream_body_accum_policy", &self.upstream_body_accum_policy),
            ("sse_leftover_policy", &self.sse_leftover_policy),
            ("stream_probe_buffer_policy", &self.stream_probe_buffer_policy),
        ] {
            match v.as_str() {
                "RejectRequest" | "Truncate" | "DropOldest" => {}
                other => anyhow::bail!("bounded_buf: invalid policy `{}` for {}", other, k),
            }
        }
        if self.upstream_body_accum_policy != "RejectRequest" {
            anyhow::bail!("bounded_buf: upstream_body_accum MUST be RejectRequest (codec correctness)");
        }
        if self.stream_probe_buffer_policy != "RejectRequest" {
            anyhow::bail!("bounded_buf: stream_probe_buffer MUST be RejectRequest");
        }
        Ok(())
    }
}
```

**ADR：备选 A=trait 抽象（推荐）；备选 B=单一 newtype；备选 C=宏。**
- A：未来可换底层（不仅 BytesMut，也可 SmallVec 等）；测试可 mock。
- B：少抽象但 caller 必须用具体类型；不利于跨 buffer 共用 metrics 注入。
- C：增加编译期复杂度，不利新 buffer 接入。

【源码验证 file:line Read】
- Read `ailinkmax-gateway/crates/gw-core/src/lib.rs` 确认 module 添加位置与 pub use 风格。
- Read `ailinkmax-gateway/crates/gw-gateway/src/config.rs:1-50` 确认 figment + serde 结构风格（已知 line 760 起 `IpRateLimitCfg`）。
- Read `ailinkmax-gateway/config/default.toml` 确认现有 table 风格。
- 确认 `metrics` crate import 路径（grep `use metrics::counter` 找现有用法）。

【如何验证 VERIFY】
1. 单元：`cargo test -p gw-core --test bounded_buf` 验三种 policy 行为；`Truncate` 验真截断到 cap；`RejectRequest` 验 Err；`DropOldest` 验 FIFO。
2. 单元：`BoundedBufCfg::validate` 在 `upstream_body_accum_policy = "Truncate"` 时返 Err（boot 必须 panic）。
3. 文档：grep CI 验 metrics `gw_bounded_buf_overflow_total` label set ⊆ `{buf, policy}`（无 request_id）。
4. 配套 lint（T-W3-21）：metric label allowlist 含 `bounded_buf -> {buf, policy}`。
5. 输出：`logs/W3-T04-verify.txt`。

---

### T-W3-05 · Rust upstream_body_accum 接入 BoundedBuf（Proxy-1 / Proxy-8）

【是什么 What it IS】
- 一个 Rust 改动，文件 `ailinkmax-gateway/crates/gw-proxy/src/ctx.rs`（已 read 含 `pub upstream_body_accum: Option<bytes::BytesMut>` at line 532）与 `crates/gw-proxy/src/filters/response.rs`（grep 命中多处 push）。
- 把 `Option<bytes::BytesMut>` 换成 `Option<BytesMutBounded>`；所有 `.extend_from_slice` / 写入 site 改 `.try_push(...)?`。
- 接入 cap 与 policy 来自 §T-W3-04 config：`upstream_body_accum_cap_bytes`（默认 8 MiB）、`upstream_body_accum_policy = "RejectRequest"`。
- Reject 路径返回 `502 GW_UPSTREAM_BODY_TRUNCATED`（与 WS-6 REV-FINAL-H6.1 协调；同 PR 合并）。

【不是什么 What it is NOT】
- ❌ 不改 streaming 路径（已 commit 200 后 truncate 不能撤）；本 task 只覆盖 non-stream cross-protocol parse 与 stream-probe-buffered 累积。
- ❌ 不改 codec 内部 state；只动 proxy 层 buffer。
- ❌ 不改 request_body_accum（若存在）；那是 request 侧，无 unbounded 风险（请求体已被 Pingora upstream limit 守）。

【为什么 Why bug exists】
- 根因（Proxy-1 / Proxy-8）：`upstream_body_accum` 在 cross-protocol 非流式重渲染路径累积上游全 body；上游恶意 / chunked-but-never-EOF → 单请求吃光 Pod RSS。
- 同 Proxy-2 / Proxy-3 不同：那两个是 SSE 行残片 / probe 前 N 字节，cap 与 policy 各异；本 task 仅做 body accum。
- v2 REV-FINAL-V1 强制 RejectRequest（codec 收到 truncated JSON 会假装解析成功 → 错算 cost；这是 codec correctness vs 部分 body）。

【做什么 DO】
1. 改 ctx.rs：
   ```rust
   pub upstream_body_accum: Option<BytesMutBounded>,
   ```
2. 初始化点（grep `upstream_body_accum: None` / 重置点 line 698 等）保持 None；首次写入处 `replace_or_init`：
   ```rust
   let buf = ctx.upstream_body_accum.get_or_insert_with(||
     BytesMutBounded::new("upstream_body_accum",
       cfg.bounded_buf.upstream_body_accum_cap_bytes,
       OverflowPolicy::RejectRequest));
   buf.try_push(chunk)?;
   ```
3. `?` 路径上抛 `BoundedBufError::RequestRejected` → 在 response filter 顶层 map 为 `GW_UPSTREAM_BODY_TRUNCATED`（502）；与 R7 / R24 502 协调（WS-6 共用错误码）。
4. 单元测试：`tests/upstream_body_cap.rs` 构造 chunked body 10 MiB（cap=8 MiB）→ 期望 502 + 计数 `gw_bounded_buf_overflow_total{buf="upstream_body_accum",policy="reject"}=1`。

【不做什么 DON'T】
- ❌ 不要在 streaming 路径加 cap（200 已 commit，truncate 无救）；那条路径 cap 在 codec 内（T-W3-08）。
- ❌ 不要在 cap reject 时 drain 上游 body 同步等 EOF（拖死）；直接撕掉 stream（同 WS-6 REV-FINAL-W7 RST_STREAM 策略）。
- ❌ 不要把 cap 写死 `const = 8 MiB`；用 cfg。

【怎么做 HOW (具体)】

```rust
// crates/gw-proxy/src/ctx.rs
use gw_core::bounded_buf::BytesMutBounded;

pub struct ReqCtx {
    // ...
    pub upstream_body_accum: Option<BytesMutBounded>,
    // ...
}

impl ReqCtx {
    pub fn reset_response_attempt_state(&mut self, cfg: &BoundedBufCfg) {
        self.upstream_body_accum = None; // re-init lazily on next push with current cfg
        // ... other resets per WS-6 REV-FINAL-H6.3
    }
}

// crates/gw-proxy/src/filters/response.rs (around the 18 push sites)
fn push_upstream_chunk(ctx: &mut ReqCtx, cfg: &BoundedBufCfg, chunk: &[u8])
    -> Result<(), pingora::Error> {
    let buf = ctx.upstream_body_accum.get_or_insert_with(|| {
        BytesMutBounded::new(
            "upstream_body_accum",
            cfg.upstream_body_accum_cap_bytes,
            OverflowPolicy::RejectRequest,
        )
    });
    match buf.try_push(chunk) {
        Ok(()) => Ok(()),
        Err(BoundedBufError::RequestRejected { .. }) => {
            // map to 502 GW_UPSTREAM_BODY_TRUNCATED via error_emit (WS-6 共用)
            Err(pingora::Error::because(
                pingora::ErrorType::CustomCode("GW_UPSTREAM_BODY_TRUNCATED", 502),
                "upstream body exceeded bounded_buf cap",
                anyhow::anyhow!("bounded_buf reject"),
            ))
        }
        Err(BoundedBufError::Truncated { .. }) => {
            unreachable!("policy is RejectRequest")
        }
    }
}
```

**ADR：备选 A=cap reject + 502（推荐）；备选 B=DropOldest（错的，codec 会读 head 错位）；备选 C=cap reject + 502 但 *drain* 上游避免 RST_STREAM 心理负担。**
- A 与 REV-FINAL-V1 一致；codec 不容忍 truncate。
- B 错语义。
- C 在 chunked + slow upstream 下放大攻击；A 直接撕。

【源码验证 file:line Read】
- 已 grep 验：`ctx.rs:532` 字面 `pub upstream_body_accum: Option<bytes::BytesMut>`；`response.rs` 18+ 处 push site（行 181-1372）。
- 实际 push 写入 sites 需 Read 第 1024-1099、1219、1372 行附近，确认 `BytesMut::extend_from_slice` / `put_slice` 字面位置（grep 命中行不一定全是写入）。
- Read `gw-proxy/src/error_emit.rs` 找现有 502 错误码 emit 风格（GW_UPSTREAM_PROTOCOL_DECODE_ERROR 类似——T17 / R24 同型）。

【如何验证 VERIFY】
1. 单元：`cargo test -p gw-proxy --test upstream_body_cap -- --nocapture`
   - 期望：10 MiB chunked body 跨 cap=8 MiB → 502 + `gw_bounded_buf_overflow_total` 计数 1。
2. 集成：`tests/e2e/chaos/scenarios/slow-loris.yaml`（WS-13 提供）→ slow-drip 上游 100 KB/s × 5 min → 单连接 RSS 不增长（绝对 cap 8 MiB）。
3. 现有 e2e `cargo test -p gw-proxy --test upstream_envelope` 仍绿（不能 break）。
4. 输出：`logs/W3-T05-verify.txt`。

---

### T-W3-06 · Rust sse_leftover 接入 BoundedBuf（Proxy-2）

【是什么 What it IS】
- 改 `crates/gw-proxy/src/ctx.rs` 内 `sse_leftover`（已知第 144 行 `ctx.sse_leftover.clear()` 出现）字段类型；改 push site 接入 BoundedBuf。
- policy = `Truncate`（v2 REV-FINAL-V1 明确：SSE 行残片 truncate 只丢部分 event，不影响 cost 算法的整体正确性）。
- cap 默认 1 MiB。

【不是什么 What it is NOT】
- ❌ 不删 `clear()` 调用；reset 路径仍正常清空。
- ❌ 不动 SSE 解析器；仅 buffer 容器。

【为什么 Why bug exists】
- 根因（Proxy-2）：恶意上游发送一个超长无 `\n` 行（10 GB），sse_leftover 一直 append 等 delimiter。
- Truncate 可接受：丢最末 N 字节相当于丢部分 event，下游 SSE consumer 处理"event 缺尾"是标准的。

【做什么 DO】
1. ctx.rs 改 `pub sse_leftover: BytesMutBounded`（非 Option，因 reset 走 `.clear()`，类型保留方便）。BoundedBuf 加 `clear` 方法。
2. push site（`crates/gw-proxy/src/filters/response.rs` 中 SSE 拼接路径）改 `try_push`。
3. Truncated 路径只 log + metric，不抛错。

【不做什么 DON'T】
- ❌ 不用 RejectRequest（已 commit 200，无法翻回错）。
- ❌ 不用 DropOldest（SSE 行头丢了下游解析炸更严重）。

【怎么做 HOW (具体)】

```rust
// gw-core/src/bounded_buf.rs — extend
impl BytesMutBounded {
    pub fn clear(&mut self) {
        self.inner.clear();
        metrics::gauge!("gw_bounded_buf_size", "buf" => self.name).set(0.0);
    }
}

// ctx.rs
pub sse_leftover: BytesMutBounded,

// init in ReqCtx::new(cfg):
sse_leftover: BytesMutBounded::new(
    "sse_leftover",
    cfg.sse_leftover_cap_bytes,
    OverflowPolicy::Truncate,
),

// push site
match ctx.sse_leftover.try_push(line) {
    Ok(()) | Err(BoundedBufError::Truncated { .. }) => {} // ok
    Err(BoundedBufError::RequestRejected { .. }) => unreachable!(),
}
```

**ADR：备选 A=Truncate（推荐）；备选 B=DropOldest（错）；备选 C=RejectRequest（已不可能：200 commit 后）。**

【源码验证 file:line Read】
- Read `gw-proxy/src/ctx.rs` 找 `sse_leftover` 字面声明（应在 ~ 532 附近，与 `upstream_body_accum` 邻近）确认现类型为 `BytesMut`。
- grep `ctx.sse_leftover` 所有 push site，逐一改。

【如何验证 VERIFY】
1. 单元：构造 1 行 2 MiB 无 `\n` 输入 → push 后 len == cap=1 MiB、metric +1。
2. e2e：现有 `cargo test -p gw-proxy --test stream_probe` 仍绿。
3. 输出：`logs/W3-T06-verify.txt`。

---

### T-W3-07 · Rust stream_probe.buffer 接入 BoundedBuf（Proxy-3）

【是什么 What it IS】
- 改 `crates/gw-proxy/src/stream_probe.rs`（已知 module 存在，gateway.rs:139, 1383 引用 `StreamProbeState`）内 buffer 字段；policy = `RejectRequest`。

【不是什么 What it is NOT】
- ❌ 不改 probe 超时 / failover 逻辑（已在 gateway.rs:1392-1396 验过 timeout 路径）。
- ❌ 不动 ProbeFailure enum。

【为什么 Why bug exists】
- 根因（Proxy-3）：stream-probe 在 "probing" 阶段 buffer 前 N 字节判断是否合法 SSE / chunk 包。N 不设上限 → 慢上游或恶意上游打满。
- Reject：probe 失败导致 502 是标准链路（与 stream_probe_failover_total 一致）；truncate 会让 probe 错判 valid。

【做什么 DO】
1. `StreamProbeState.buffer: BytesMutBounded`（默认 cap 256 KiB，policy RejectRequest）。
2. probe push site `try_push(chunk)?` → 错误经 ProbeFailure::BufferCap 上抛。
3. `can_retry_probe_failure` 加 `BufferCap => true`（与 timeout 同样可 failover）；retry 仍失败 → 502。

【不做什么 DON'T】
- ❌ 不要 truncate。
- ❌ 不要不 metric（出现 BufferCap 是上游异常信号，需告警）。

【怎么做 HOW (具体)】

```rust
// crates/gw-proxy/src/stream_probe.rs
pub enum ProbeFailure { Timeout, Decode, BufferCap }

pub struct StreamProbeState {
    pub buffer: BytesMutBounded,
    // ...
}

impl StreamProbeState {
    pub fn new(cfg: &BoundedBufCfg) -> Self {
        Self {
            buffer: BytesMutBounded::new(
                "stream_probe_buffer",
                cfg.stream_probe_buffer_cap_bytes,
                OverflowPolicy::RejectRequest,
            ),
            // ...
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<(), ProbeFailure> {
        match self.buffer.try_push(chunk) {
            Ok(()) => Ok(()),
            Err(BoundedBufError::RequestRejected { .. }) => Err(ProbeFailure::BufferCap),
            Err(BoundedBufError::Truncated { .. }) => unreachable!(),
        }
    }
}

// gateway.rs failover handling
let failure = match status {
    Status::Timeout => ProbeFailure::Timeout,
    Status::BufferOverflow => ProbeFailure::BufferCap,
    // ...
};
metrics::counter!("gw_stream_probe_total", "outcome" => format!("{:?}", failure))
    .increment(1);
```

【源码验证 file:line Read】
- Read `crates/gw-proxy/src/stream_probe.rs` 完整。
- Read `gateway.rs:1383-1396` 确认 timeout 路径走法。
- 确认 probe failover 计数器 label set 已不含 request_id（R15 / REV-FINAL-V7）。

【如何验证 VERIFY】
1. 单元：构造 512 KiB chunk → probe.push 返 `Err(BufferCap)`；`gw_stream_probe_total{outcome="BufferCap"}` 计数 1。
2. e2e：现有 `cargo test -p gw-proxy --test stream_probe` 仍绿。
3. 输出：`logs/W3-T07-verify.txt`。

---

### T-W3-08 · Rust codec state caps：max_accumulated_bytes + pending_function_calls 上限（Codec-4 + REV-FINAL-V3）

【是什么 What it IS】
- Rust codec 改动，两路：
  - `crates/gw-codec/src/gemini/state.rs:22`：`pending_function_calls: Vec<GeminiFunctionCallBuilder>` 加上限 `cap=128`（cfg.codec_pending_function_calls_cap）。
  - `crates/gw-codec/src/openai/responses/state.rs:29`：`pending_function_calls: HashMap<String, FunctionCallBuilder>` 同上限。
  - 每 builder 内 `args_buffer` / 任何 String 累积通过 BoundedBuf（cap=cfg.codec_max_accumulated_bytes 默认 4 MiB）。
- 超 cap 触发 v2 REV-FINAL-V3：emit SSE 帧 `event: error\n data: GW_CODEC_BUFFER_OVERFLOW\n\n` + 设 `ctx.usage.aborted = true` + 让 settler `refund` 本 attempt 的 output cost。

【不是什么 What it is NOT】
- ❌ 不删 pending_function_calls 整字段；只加上限。
- ❌ 不动 Anthropic codec（已用 streaming deltas，无累积）。
- ❌ 不改 cost 算法；只 emit `aborted=true` 信号让 Go 算 cost=0。
- ❌ 不写 client error frame 时改 HTTP status code（已 200，无法翻；通过 SSE event 内 error 信号通信）。

【为什么 Why bug exists】
- 根因（Codec-4）：上游一次发 100k 个 unique-name tool call → `pending_function_calls.push` 无 cap；每 builder 内 `args_chunk += "..."` 无 cap。
- Codec-4 v1 描述："codec state 每 builder `max_accumulated_bytes` + `pending_function_calls` cap 128"。
- REV-FINAL-V3 强制超 cap 时的 fail-policy（emit error frame + refund cost）—— v1 只说"emit overflow"，不够具体。

【做什么 DO】
1. 在 `gemini/state.rs` 加：
   ```rust
   pub(crate) fn try_add_function_call(&mut self, builder: GeminiFunctionCallBuilder, cap: usize) -> Result<usize, CodecOverflowError> {
       if self.pending_function_calls.len() >= cap {
           return Err(CodecOverflowError::TooManyFunctionCalls { cap });
       }
       self.pending_function_calls.push(builder);
       Ok(self.pending_function_calls.len() - 1)
   }
   ```
   replace `gemini/codec.rs:425` push 直调。
2. builder 内 `args_buffer: BytesMutBounded`（cap=cfg.codec_max_accumulated_bytes，policy=RejectRequest 但 codec 层把 RejectRequest 翻译为 `CodecOverflowError::ArgsBufferCap`）。
3. `openai/responses/state.rs:29` 同。
4. codec.rs 上抛 `CodecOverflowError` → response filter 收到 → emit SSE error frame + `ctx.usage.aborted = true` + settler 在 emit lifecycle 时取消计 output cost（与 R7 protected mode parse fail 共用路径，沿 ctx.usage.aborted 分流）。
5. 暴露 metric `gw_codec_overflow_total{kind}`（kind ∈ {`too_many_function_calls`, `args_buffer_cap`, `accumulated_bytes`}）。

【不做什么 DON'T】
- ❌ 不在 codec layer 内"丢一个 function call 继续"；codec 一旦 aborted 必须停止 emit 给下游（错算 cost 风险）。
- ❌ 不要把 cap 设到 1024（业务不会有那么多 tool call；保守 128 即可）。
- ❌ 不要忘记 Anthropic→Anthropic streaming：那条路不走 cross-render，不应误打开 abort。

【怎么做 HOW (具体)】

```rust
// crates/gw-codec/src/error.rs (new)
#[derive(Debug, thiserror::Error)]
pub enum CodecOverflowError {
    #[error("too many pending function calls (cap {cap})")]
    TooManyFunctionCalls { cap: usize },
    #[error("function-call args buffer exceeded cap {cap}")]
    ArgsBufferCap { cap: usize },
    #[error("accumulated body exceeded cap {cap}")]
    AccumulatedBytes { cap: usize },
}

// gemini/codec.rs:417 area (Read 行后确认)
match st.try_add_function_call(
    GeminiFunctionCallBuilder { /* ... */ },
    cfg.codec_pending_function_calls_cap
) {
    Ok(idx) => { /* existing path */ }
    Err(e) => {
        metrics::counter!("gw_codec_overflow_total",
            "kind" => "too_many_function_calls").increment(1);
        return Err(CodecError::Overflow(e));
    }
}

// response handler — emit SSE error + mark aborted (REV-FINAL-V3)
ctx.usage.aborted = true;
write_sse_error_frame(downstream, "GW_CODEC_BUFFER_OVERFLOW").await?;
return Ok(()); // stream cleanly closes; settler reads ctx.usage.aborted → cost 0
```

settler 联动（与 v1 §4 WS-1 R7 共用，由 T-W3-08 仅声明 ctx 字段 + emit 事件；settler 改动归 WS-1 / WS-8）。

**ADR：备选 A=emit SSE error + abort + refund（推荐 / V3）；备选 B=只 emit 不 abort（错算 cost 风险）；备选 C=502（已 commit 200，无法）。**

【源码验证 file:line Read】
- 已 Read 行号 `gemini/state.rs:22` 字面 `pub(crate) pending_function_calls: Vec<GeminiFunctionCallBuilder>`。
- 已 Read `gemini/codec.rs:417, 424-431, 435` 等 push site。
- 已 Read `openai/responses/state.rs:29` 字面 `pub(crate) pending_function_calls: HashMap<String, FunctionCallBuilder>`。
- 需 Read：`openai/responses/codec.rs:557, 652, 691` builder 内字段写入位置。
- 需 Read：`ctx.rs` 内 `ctx.usage.aborted` 是否已存在；若无则同 T-W3-08 加。

【如何验证 VERIFY】
1. 单元：构造 200 unique-name tool call → `Err(CodecOverflowError::TooManyFunctionCalls { cap: 128 })`；metric +1。
2. 单元：单 tool args_buffer 写 8 MiB（cap 4 MiB）→ Err；metric +1。
3. e2e：现有 `cargo test -p gw-codec --test cross_protocol_stream` 全绿（不 break 正常流）。
4. e2e：构造恶意 codec abort → 下游收到 `event: error\ndata: GW_CODEC_BUFFER_OVERFLOW`；ctx.usage.aborted=true；Go settler 收到 emit 算 cost=0（WS-1 已覆盖）。
5. 输出：`logs/W3-T08-verify.txt`。

---

### T-W3-09 · Rust Sweepable trait + SweepRegistry 基础设施（REV-FINAL-V2 基础）

【是什么 What it IS】
- 新增 `crates/gw-routing/src/sweepable.rs`：
  ```rust
  pub trait Sweepable: Send + Sync {
      fn name(&self) -> &'static str;
      fn size(&self) -> usize;
      fn sweep(&self, now: Instant) -> SweepReport;
  }
  pub struct SweepReport { pub removed: usize, pub kept: usize, pub policy: &'static str }
  ```
- 改造 `routing_sweep.rs`（已存在；Read 验过）从 hard-code 各 tracker 调用，换成遍历 SweepRegistry。
- 每 tracker 暴露 `gw_routing_{name}_size`（gauge）+ `gw_routing_{name}_evicted_total`（counter）。

【不是什么 What it is NOT】
- ❌ 不删现有 `routing_sweep::spawn` 入口；签名向后兼容。
- ❌ 不引入异步 sweep（同步 OK；DashMap iter 是 sharded lock）。
- ❌ 不动 InFlightStore by_key 行为（只暴露 size，evict 走专门谓词 REV-FINAL-V2）。

【为什么 Why bug exists】
- 根因（Routing-1..4）：v1 routing_sweep 已有 hard-code 调用 ModelFrequencyTracker / ReservationGlobalView / LatencyTracker / AffinityManager，但 InFlightStore 与 RoundRobin/WeightedRr 未接入；现有 sweep_stale 谓词不统一。
- v2 REV-FINAL-V2：InFlightStore 只能 evict `count==0`、KeyHealthStore 用 `last_seen`、RR/WRR 加 `last_used: Instant`、硬 cap 仅 warn + metric。

【做什么 DO】
1. 新 trait `Sweepable` + `SweepRegistry`。
2. 实现：
   - `InFlightStore::sweep`：iter by_key，evict where `counter.in_flight() == 0 && counter.last_used_lt(now - threshold)`。**严禁** evict `count > 0`（count 损坏）。
   - `LatencyTracker::sweep`：保持已有 `evict_stale`。
   - `AffinityManager::sweep`：保持已有 `sweep_expired`。
   - `ModelFrequencyTracker::sweep_stale` 包装。
   - `ReservationGlobalView::sweep_stale_instances` 包装。
   - 新增 `RoundRobinStrategy::sweep` / `WeightedRrStrategy::sweep`（要求两者底层 map 加 `last_used` 字段；如已有则直接接入）。
3. `routing_sweep::spawn` 接受 `Arc<SweepRegistry>` 而非各 tracker arg；boot 时构造 registry 注册全部。
4. 每 tracker 都暴露 `gw_routing_{name}_size`、`gw_routing_{name}_evicted_total`。

【不做什么 DON'T】
- ❌ 不在 InFlightStore 强制 LRU 硬 cap（count>0 evict 会让 in_flight 计数损坏，后续 in_flight check 错放）。硬 cap **仅 warn + metric**。
- ❌ 不用 tokio::spawn 起每 tracker 独立 sweeper（现有单一 routing_sweep loop 即可，避免支离破碎）。
- ❌ 不要让 sweep 谓词使用 wall clock（用 `Instant`），避免时钟跳变误 evict。

【怎么做 HOW (具体)】

```rust
// crates/gw-routing/src/sweepable.rs
use std::sync::Arc;
use std::time::Instant;

pub struct SweepReport { pub removed: usize, pub kept: usize, pub policy: &'static str }

pub trait Sweepable: Send + Sync {
    fn name(&self) -> &'static str;
    fn size(&self) -> usize;
    fn sweep(&self, now: Instant, threshold: std::time::Duration) -> SweepReport;
}

pub struct SweepRegistry {
    items: Vec<Arc<dyn Sweepable>>,
    soft_cap: usize, // warn only
}
impl SweepRegistry {
    pub fn new(soft_cap: usize) -> Self { Self { items: vec![], soft_cap } }
    pub fn register<T: Sweepable + 'static>(&mut self, t: Arc<T>) {
        self.items.push(t);
    }
    pub fn tick(&self, now: Instant, threshold: std::time::Duration) {
        for item in &self.items {
            let report = item.sweep(now, threshold);
            metrics::gauge!("gw_routing_size", "name" => item.name())
                .set(item.size() as f64);
            metrics::counter!("gw_routing_evicted_total", "name" => item.name())
                .increment(report.removed as u64);
            if item.size() > self.soft_cap {
                tracing::warn!(
                    target: "gw_routing::sweep",
                    tracker = item.name(),
                    size = item.size(),
                    cap = self.soft_cap,
                    "tracker exceeded soft cap"
                );
                metrics::counter!("gw_routing_soft_cap_warn_total",
                    "name" => item.name()).increment(1);
            }
        }
    }
}

// crates/gw-routing/src/in_flight.rs (extend)
impl Sweepable for InFlightStore {
    fn name(&self) -> &'static str { "in_flight_store" }
    fn size(&self) -> usize { self.by_key.len() }
    fn sweep(&self, now: Instant, threshold: std::time::Duration) -> SweepReport {
        let mut removed = 0;
        let mut kept = 0;
        self.by_key.retain(|_, counter| {
            // SAFETY-INVARIANT: never evict count>0 (counter integrity)
            if counter.in_flight() == 0 && counter.last_used().elapsed() > threshold {
                removed += 1;
                false
            } else {
                kept += 1;
                true
            }
        });
        SweepReport { removed, kept, policy: "in_flight_zero_only" }
    }
}

// crates/gw-routing/src/routing_sweep.rs (rewrite spawn)
pub fn spawn(registry: Arc<SweepRegistry>, shutdown: ShutdownToken) -> JoinHandle<()> {
    Supervisor::new("routing_sweep", RestartPolicy::default(), shutdown).spawn(move |tok| {
        let registry = registry.clone();
        async move {
            let mut tick = tokio::time::interval(SWEEP_INTERVAL);
            tick.tick().await;
            loop {
                tokio::select! {
                    _ = tok.cancelled() => { return Ok(()); }
                    _ = tick.tick() => {
                        registry.tick(Instant::now(), Duration::from_millis(STALE_THRESHOLD_MS));
                    }
                }
            }
        }
    })
}
```

**ADR：备选 A=统一 trait + registry（推荐）；备选 B=保留 hard-code 调用，只补 InFlightStore；备选 C=每 tracker 独立 loop。**
- A：统一 + 可扩展 + 测试容易（mock Sweepable）。
- B：增量风险小但漏掉新 tracker（future）。
- C：增加 supervisor 维护成本。

【源码验证 file:line Read】
- 已 Read `routing_sweep.rs` 全文（v1 验：spawn(freq, view, latency, affinity, shutdown) → ModelFrequencyTracker.sweep_stale / ReservationGlobalView.sweep_stale_instances / LatencyTracker.evict_stale / AffinityManager.sweep_expired）。
- 已 grep：`InFlightStore::by_key: DashMap<KeyId, Arc<InFlightCounter>>` (in_flight.rs:48)；`InFlightCounter` 是否有 `in_flight()` getter 与 `last_used()` —— **需 Read in_flight.rs 全文确认**；若无，T-W3-10 提供。
- Read `crates/gw-routing/src/strategy/round_robin.rs`、`strategy/weighted_rr.rs` 确认是否有 last_used / DashMap，并实现 Sweepable。

【如何验证 VERIFY】
1. 单元：mock 3 个 Sweepable，registry.tick → 各 size gauge 更新、evicted_total 正确。
2. soft_cap warn：注入 size > cap → metric `gw_routing_soft_cap_warn_total` 增。
3. 现有 routing tests（`tests/affinity.rs`、`tests/in_flight.rs`、`tests/strategy_set.rs`）全绿。
4. 输出：`logs/W3-T09-verify.txt`。

---

### T-W3-10 · Rust InFlightStore 实现 Sweepable（Routing-1）

【是什么 What it IS】
- 实现 T-W3-09 trait 在 `crates/gw-routing/src/in_flight.rs`；并保证 `InFlightCounter` 暴露 `in_flight() -> u64` 与 `last_used() -> Instant`（若缺则加）。

【不是什么 What it is NOT】
- ❌ 不改 in_flight 计数器语义（加 / 减）；只读访问。

【为什么 Why bug exists】
- 根因（Routing-1）：DashMap by_key 在大量 churn 下永不清；ApiKey 删除后残留。

【做什么 DO】
1. `InFlightCounter` 加 `last_used: AtomicI64`（Instant 不能跨线程方便 store，用 `coarsetime::Instant::now().as_ticks()` 或 `Instant::elapsed_since_program_start_nanos`；最简：`std::sync::atomic::AtomicU64` 存自 monotonic now 的 nanos）。
2. 每次 `incr`/`decr` 更新 `last_used`。
3. Sweepable::sweep 见 T-W3-09 实现：**严禁** evict `count>0`。

【不做什么 DON'T】
- ❌ 不要在 sweep 内重置 last_used（用读后比较）。
- ❌ 不要把 last_used 用 Mutex / RwLock（高频路径性能）。

【怎么做 HOW (具体)】

```rust
// in_flight.rs
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Instant;
use once_cell::sync::Lazy;

static EPOCH: Lazy<Instant> = Lazy::new(Instant::now);

pub struct InFlightCounter {
    count: AtomicI64,
    last_used_nanos: AtomicU64,
}
impl InFlightCounter {
    pub fn new() -> Self {
        Self {
            count: AtomicI64::new(0),
            last_used_nanos: AtomicU64::new(EPOCH.elapsed().as_nanos() as u64),
        }
    }
    pub fn incr(&self) { self.count.fetch_add(1, Ordering::AcqRel); self.touch(); }
    pub fn decr(&self) { self.count.fetch_sub(1, Ordering::AcqRel); self.touch(); }
    pub fn in_flight(&self) -> i64 { self.count.load(Ordering::Acquire) }
    pub fn touch(&self) {
        self.last_used_nanos.store(EPOCH.elapsed().as_nanos() as u64, Ordering::Release);
    }
    pub fn last_used_elapsed(&self) -> std::time::Duration {
        let now = EPOCH.elapsed().as_nanos() as u64;
        let last = self.last_used_nanos.load(Ordering::Acquire);
        std::time::Duration::from_nanos(now.saturating_sub(last))
    }
}
```

【源码验证 file:line Read】
- Read `in_flight.rs` 全文 确认 `InFlightCounter` 实际字段、incr/decr 调用方位置；如已有 last_used 字段就跳过。
- 检查 Atomic vs Cell；`Lazy<Instant>` 引入 once_cell 是否已被允许（grep `once_cell`）。

【如何验证 VERIFY】
1. 单元：sweep with count=0 且过期 → evicted；count=1 即便过期 → 保留。
2. 现有 `tests/in_flight.rs` 全绿。
3. 输出：`logs/W3-T10-verify.txt`。

---

### T-W3-11 · Rust LatencyTracker 接入 Sweepable（Routing-2）

【是什么 What it IS】
- 改 `crates/gw-routing/src/latency.rs` 把现有 `evict_stale(Duration)` 包成 `Sweepable::sweep`。

【不是什么 What it is NOT】
- ❌ 不改 latency 算法。

【为什么 Why bug exists】
- 根因（Routing-2）：原 v1 sweep 已调 evict_stale，但 v2 REV-FINAL-V2 要求 size + evicted 计数统一暴露。

【做什么 DO】
- 在 LatencyTracker impl Sweepable，name="latency_tracker"。
- 暴露 `size()`（map 长度）；sweep 直调 evict_stale，返回 removed 数（需 evict_stale 返 usize；现有签名若 ()，改）。

【不做什么 DON'T】
- ❌ 不改 latency window 算法。

【怎么做 HOW (具体)】

```rust
impl Sweepable for LatencyTracker {
    fn name(&self) -> &'static str { "latency_tracker" }
    fn size(&self) -> usize { self.records_len() }
    fn sweep(&self, _now: Instant, threshold: Duration) -> SweepReport {
        let removed = self.evict_stale(threshold);
        SweepReport { removed, kept: self.size(), policy: "stale_window" }
    }
}
```

【源码验证 file:line Read】
- Read `crates/gw-routing/src/latency.rs` 全文确认 `evict_stale` 现签名、是否有 `len`/`records_len`。

【如何验证 VERIFY】
1. 现有 `tests/strategies.rs` 全绿。
2. 输出：`logs/W3-T11-verify.txt`。

---

### T-W3-12 · Rust AffinityManager 接入 Sweepable（Routing-3）

【是什么 What it IS】
- 改 `crates/gw-routing/src/affinity.rs` Sweepable 包装 `sweep_expired`。

【不是什么 What it is NOT】
- ❌ 不改 affinity 行为。

【做什么 DO】
- `impl Sweepable for AffinityManager`：name="affinity_manager"；size = records.len()；sweep 调 sweep_expired(now)。

【不做什么 DON'T】
- ❌ 不引入新字段。

【怎么做 HOW (具体)】

```rust
impl Sweepable for AffinityManager {
    fn name(&self) -> &'static str { "affinity_manager" }
    fn size(&self) -> usize { self.records.len() }
    fn sweep(&self, now: Instant, _threshold: Duration) -> SweepReport {
        let removed = self.sweep_expired(now);
        SweepReport { removed, kept: self.size(), policy: "expired_ttl" }
    }
}
```

【源码验证 file:line Read】
- 已 grep `affinity.rs:34, 76` 字面 `DashMap::new()`、`sweep_expired(&self, now: Instant) -> usize`。

【如何验证 VERIFY】
1. 现有 `tests/affinity.rs`、`tests/sticky_ttl.rs` 全绿。
2. 输出：`logs/W3-T12-verify.txt`。

---

### T-W3-13 · Rust ModelFrequencyTracker / ReservationGlobalView 接入 Sweepable（Routing-4）

【是什么 What it IS】
- 改 `crates/gw-routing/src/reservation.rs` 加 Sweepable for `ModelFrequencyTracker`（包 sweep_stale(threshold_ms)）+ `ReservationGlobalView`（包 sweep_stale_instances(threshold_ms)）。

【不是什么 What it is NOT】
- ❌ 不改 reservation 算法。

【做什么 DO】
- name="model_frequency_tracker" / "reservation_global_view"。

【怎么做 HOW (具体)】

```rust
impl Sweepable for ModelFrequencyTracker {
    fn name(&self) -> &'static str { "model_frequency_tracker" }
    fn size(&self) -> usize { self.counters.len() }
    fn sweep(&self, _now: Instant, threshold: Duration) -> SweepReport {
        let removed = self.sweep_stale(threshold.as_millis() as u64) as usize;
        SweepReport { removed, kept: self.size(), policy: "stale_ms" }
    }
}

impl Sweepable for ReservationGlobalView {
    fn name(&self) -> &'static str { "reservation_global_view" }
    fn size(&self) -> usize { self.by_model.len() + self.by_model_instance.len() }
    fn sweep(&self, _now: Instant, threshold: Duration) -> SweepReport {
        let removed = self.sweep_stale_instances(threshold.as_millis() as u64) as usize;
        SweepReport { removed, kept: self.size(), policy: "stale_ms" }
    }
}
```

【源码验证 file:line Read】
- 已 grep `reservation.rs:46-130` 字面 `ModelFrequencyTracker`、`sweep_stale`、`ReservationGlobalView`、`by_model: DashMap`。

【如何验证 VERIFY】
1. 现有 `tests/reservation_gate.rs`、`benches/strategy_pick.rs` 全绿。
2. 输出：`logs/W3-T13-verify.txt`。

---

### T-W3-14 · Rust DashMap obs metrics 统一（Routing-7）

【是什么 What it IS】
- T-W3-09 SweepRegistry.tick 已为 registered 表暴露 `gw_routing_size{name}` / `gw_routing_evicted_total{name}`。本 task 补齐：
  - 删除任何旧的 per-tracker hand-rolled gauges（若存在）。
  - 加 dashboard JSON 模板（`docs/observability/routing-sweep.json`），WS-10 owner 部署。

【不是什么 What it is NOT】
- ❌ 不重写 Prometheus 客户端；用 metrics crate。

【做什么 DO】
1. grep `gw_routing_` 现有 gauges，统一改 label 用 `name`（不要 per-tracker 单独 metric name 比如 `gw_routing_in_flight_size`，那让 PromQL 难写）。
2. 加 Grafana panel JSON。
3. 加 alert：`size > soft_cap × 1.5` 持续 10min → warn。

【源码验证 file:line Read】
- grep `gw_routing` 现 metric name 集合（在 routing crate 与 gateway crate）。

【如何验证 VERIFY】
1. `cargo test -p gw-routing --test sweep_registry_metrics` 验 metric name 与 label 集合。
2. 部署后 `/metrics` 抓取确认面板可视。
3. 输出：`logs/W3-T14-verify.txt`。

---

### T-W3-15 · Rust RedosTracker：删静态估算 + 阈值 10→1 + is_disabled 前置（R14 + REV-FINAL-V5）

【是什么 What it IS】
- 改 `crates/gw-auth/src/model_permissions.rs`（已 read：line 200-233 `check_with_budget`、line 209-216 documentation block 证 RE2 linear-time）。
- 把 `is_disabled` 检查 **保留在调 `check_model_against_permissions` 之前**（已是现状）；将 `TIMEOUT_THRESHOLD` 从默认（grep 找当前值，按 v1 R14 描述是 10）改为 **1**。
- **不**增加静态长度 / 复杂度估算（v2 REV-FINAL-V5 明确删除该 v1 提议）。
- 同时确认 `record_timeout` 在 `check` 之后调用是 OK 的——已是现状。

【不是什么 What it is NOT】
- ❌ 不改正则 crate（仍 `regex` RE2）。
- ❌ 不删 RedosTracker 类型。
- ❌ 不引入"超长 pattern 即拒"。

【为什么 Why bug exists】
- v1 R14 描述：`RedosTracker 检查 budget 在 match 之后，前 10 次慢正则裸跑`。
- 抽样 Read 已证：line 209-216 已显式 documentation "RE2 linear-time, no catastrophic backtracking possible"。所以 v1 提的"静态估算"不适用；v2 REV-FINAL-V5 删之。
- 真实 mitigation：阈值 10 → 1（首次超时立即 disable），减少所谓"前 10 次裸跑"的损失（虽然 RE2 linear 下损失极小，但 defense in depth）。
- `is_disabled` 已在 `check_with_budget` 顶端调（line 223）；保留即可。

【做什么 DO】
1. grep `TIMEOUT_THRESHOLD` 找定义位置；改为 `1`。
2. 添加注释引用 v2 REV-FINAL-V5 与 RE2 linear-time block（line 209-216）。
3. 确认 `BUDGET_MS` 当前值合理（grep；若需要细调，按 audit 上下文）。
4. 加单元 `redos_tracker_disables_after_first_timeout`。

【不做什么 DON'T】
- ❌ 不要写"len(pattern) > N 即拒"——`regex` 已用 `RegexBuilder::dfa_size_limit`（line 211）守 DFA 构建。
- ❌ 不要换 `regex` 为 `pcre`（即引入 catastrophic backtracking）。
- ❌ 不要把 budget 设到 1s（已有 100ms，正常用 < 1ms；100ms 是宽容上界）。
- ❌ 不要 panic / 503 在 disabled——返 None（caller 视为 reject），保持原 contract。

【怎么做 HOW (具体)】

```rust
// crates/gw-auth/src/model_permissions.rs
// Around line 138:
pub const TIMEOUT_THRESHOLD: usize = 1; // v2 REV-FINAL-V5: 10 → 1 (first slow call disables)

// check_with_budget unchanged: is_disabled already called BEFORE check (line 223).
// Comment update at line 209-216:
//   `regex` crate has no native incremental-step API; the post-check timestamp
//   is therefore observational, not preemptive. This is acceptable because:
//     1. `RegexBuilder::dfa_size_limit` already bounds DFA construction;
//     2. RE2-style matching is linear-time on input (no catastrophic backtracking
//        possible regardless of pattern shape);
//     3. The budget guard is a defense-in-depth — even with a pathological
//        input, threshold-disable bounds total damage to ~ `TIMEOUT_THRESHOLD`
//        slow calls per ApiKey (now 1).
//   v2 REV-FINAL-V5: 静态估算被显式拒绝 — RE2 linear-time 性质让长度估算无意义。
```

**ADR：备选 A=阈值 10→1 + 保留 post-hoc tracker（推荐 / V5）；备选 B=保留 10 加日志告警；备选 C=改 `pcre2` 加超时 wall clock interrupt。**
- A：最小变化、与 RE2 性质一致。
- B：放弃 P5 完全修复。
- C：换库引入 backtracking 类风险（C 选项是错的）。

【源码验证 file:line Read】
- 已 Read `model_permissions.rs:200-252` 字面确认 `is_disabled` 已在 check 之前（line 223 顶端）+ RE2 doc block 在 line 209-216。
- 需 Read `model_permissions.rs:1-200` 找 `TIMEOUT_THRESHOLD` 真实定义（line 138 附近）。
- 需 Read 找 `BUDGET_MS` 真实数值。

【如何验证 VERIFY】
1. 单元：`cargo test -p gw-auth --test model_permissions -- redos`
   - `redos_tracker_disables_after_first_timeout`：record_timeout 1 次后 is_disabled=true。
2. 单元：`redos_tracker_evicts_via_lru_at_capacity` 仍绿。
3. 输出：`logs/W3-T15-verify.txt`。

【对抗复审 hook】此 task 直接与 v2 §0 重要 2 抽样反证机制相关。提示复审 sonnet：核对"是否还有别处依赖 RE2 linear-time 假设"。

---

### T-W3-16 · Rust Prom 高基数 label 杀名单 + 替换（R15 + REV-FINAL-V7）

【是什么 What it IS】
- 一系列 Rust 改动，对全 workspace metric site 走一遍杀名单（v2 REV-FINAL-V7 字面）：
  - `gw_billing_emit_lag_t0_total{request_id}` → 删 `request_id` 保 `attempt_index`。
  - `gw_redos_timeout_total{api_key_id}` → 改 `{disabled_now: bool}`。
  - 同范式扫所有 `metrics::counter!("…", "request_id" => …)` / `"api_key_id"` / `"user_id"` 等 ID 类 label。
- 与 WS-10 obs 同迁移：删 label 同时更新 Grafana panel + alert（避免静默失声）。

【不是什么 What it is NOT】
- ❌ 不删 metric 本身（只删 label）。
- ❌ 不改业务行为。
- ❌ 不影响 logging（log 仍可带 request_id；只是 metric label 不能）。

【为什么 Why bug exists】
- 根因（R15）：Prom counter label 形成时间序列基数 = 唯一 label 组合数；request_id 高基数（每请求 unique）让 `/metrics` 输出 N 行/秒，scrape 内存爆。
- 解决：label 改为有界 enum（attempt_index ∈ {0,1,2}; disabled_now ∈ {true,false}）。

【做什么 DO】
1. grep `metrics::counter\|metrics::gauge\|metrics::histogram` 全 Rust workspace，列出每个 metric 的 label set。
2. 删 high-cardinality label，按 v2 REV-FINAL-V7 与发现的新案例改 enum。
3. 与 logging.rs / archive emit 解耦：metric 不带 ID，log 带。
4. 加 unit 测试枚举 label allowlist。
5. 与 T-W3-21（CI lint）协同：build 期 fail 出现 disallowed label。

【不做什么 DON'T】
- ❌ 不要用 hashed request_id 当 label（仍高基数）。
- ❌ 不要静默删除——必须同时迁移 dashboard（WS-10）。
- ❌ 不要"以后再说"——R15 是 launch-blocking。

【怎么做 HOW (具体)】

```rust
// 例：crates/gw-ledger/src/inflight_reconciler.rs (R15 / REV-FINAL-V7 spot)
// before (hypothetical):
//   metrics::counter!("gw_billing_emit_lag_t0_total",
//       "request_id" => &rec.request_id, "attempt_index" => attempt.to_string())
//       .increment(1);
// after:
metrics::counter!("gw_billing_emit_lag_t0_total",
    "attempt_index" => attempt.to_string()).increment(1);
tracing::warn!(target: "gw_ledger::emit_lag",
    request_id = %rec.request_id, attempt_index = attempt,
    "t0 emit lag");

// 例：RedosTracker (R15 spot)
metrics::counter!("gw_redos_timeout_total",
    "disabled_now" => disabled.to_string()).increment(1);
tracing::warn!(target: "gw_auth::redos",
    api_key_id = %api_key_id, "redos timeout");
```

**ADR：备选 A=删 label 移到 log（推荐 / V7）；备选 B=label 哈希到 N=256 桶（仍高于安全阈）；备选 C=改 histogram（基数仍是问题）。**

【源码验证 file:line Read】
- grep `request_id\s*=>\|api_key_id\s*=>\|user_id\s*=>` 在 `metrics::` 调用上下文找 hit list。
- 已 grep 部分见 §源码 `inflight_reconciler.rs:90, 124` 与 R15 描述一致。
- 与 WS-10 owner 协调 Grafana JSON。

【如何验证 VERIFY】
1. Lint（T-W3-21 提供）：build fail 当任何 metric site 用 disallowed label key。
2. e2e：跑 e2e-soak 后 `curl :9090/metrics | wc -c` < 5 MB（v1 §4 WS-3 DoD）。
3. 现有 `/metrics` 抓取 dashboard 仍有数据（验证 obs 未失声）。
4. 输出：`logs/W3-T16-verify.txt`。

---

### T-W3-17 · Rust counter_store.incrby_with_expire EXPIRE 失败显式 retry + 上报（R19 part 1）

【是什么 What it IS】
- 改 `ailinkmax-gateway/crates/gw-store/src/counter_store.rs:50-60`（已 Read 字面 `let _: Result<(), _> = client.expire(key, ttl_secs).await;`）。
- 把 `let _ = ...` 改为同步 3 次 retry；3 次仍败 → error counter +1 + warn log，但 **不**让 caller 失败（v1 注释 line 42-49 明确"daily_tokens 是 observational，not billing"）。
- 引入 `gw_counter_store_expire_fail_total` 计数器。

【不是什么 What it is NOT】
- ❌ 不改 INCRBY 路径（已正确）。
- ❌ 不引入 Lua 原子（INCRBY + EXPIRE 都已是单 round-trip，加 Lua 增延迟无收益）。
- ❌ 不动 Go 侧 daily_tokens 消费路径。

【为什么 Why bug exists】
- 根因（R19）：EXPIRE 失败被 `let _ =` 吞掉 → `daily_tokens:*` 永生 → 单调累积 Redis 内存。
- v1 R19 一句话："counter_store.incrby_with_expire 吞 EXPIRE 错误，`daily_tokens:*` 永久无 TTL"。
- v2 REV-FINAL-V6：除了重试 + 上报，**v1 提议的"常驻 sweeper" 改为一次性迁移脚本**（非常驻 loop）。T-W3-18 单独覆盖迁移脚本。

【做什么 DO】
1. 加 const `EXPIRE_RETRIES: usize = 3`，per-attempt backoff `Duration::from_millis(50)` 指数。
2. retry 3 次 exhaust → `metrics::counter!("gw_counter_store_expire_fail_total").increment(1)` + `warn!` 日志含 key。
3. 仍返 `Ok(v)`（v 是 INCRBY 结果，业务不被卡）。
4. 单元覆盖：mock Redis EXPIRE 失败 → 3 次调用 → counter +1。

【不做什么 DON'T】
- ❌ 不要 hard fail（业务无关）。
- ❌ 不要把 retries 设到 10（放大 Redis 抖动期 latency）。
- ❌ 不要静默——必须有 counter（运维需可观测）。

【怎么做 HOW (具体)】

```rust
// crates/gw-store/src/counter_store.rs
pub async fn incrby_with_expire(
    &self, key: &str, delta: i64, ttl_secs: i64,
) -> anyhow::Result<i64> {
    let client = self.redis.pool.next();
    let v: i64 = client.incr_by(key, delta).await?;
    for attempt in 0..3 {
        match client.expire(key, ttl_secs).await {
            Ok(_) => return Ok(v),
            Err(e) if attempt == 2 => {
                metrics::counter!("gw_counter_store_expire_fail_total").increment(1);
                tracing::warn!(
                    target: "gw_store::counter_store",
                    key = key,
                    error = %e,
                    "EXPIRE failed after 3 retries; key may have no TTL"
                );
                return Ok(v); // observational; do not fail caller
            }
            Err(e) => {
                tracing::debug!(
                    target: "gw_store::counter_store",
                    key = key, attempt, error = %e, "EXPIRE retry"
                );
                tokio::time::sleep(Duration::from_millis(50 * (1 << attempt))).await;
            }
        }
    }
    unreachable!()
}
```

**ADR：备选 A=3 次 retry + 计数（推荐）；备选 B=单条 Lua INCRBY+EXPIRE（增 latency，无收益）；备选 C=纯计数不 retry（漏 transient）。**

【源码验证 file:line Read】
- 已 Read `counter_store.rs:42-68` 字面确认 `let _: Result<(), _> = client.expire(key, ttl_secs).await;`、`pub fn daily_tokens_key(uid, day_utc) -> String`。

【如何验证 VERIFY】
1. 单元：mock fred client EXPIRE always Err → INCRBY 后调用 → counter +1，retries=3。
2. 单元：transient（前 2 次 Err，第 3 次 Ok）→ counter 不增。
3. 输出：`logs/W3-T17-verify.txt`。

---

### T-W3-18 · 一次性迁移脚本扫无 TTL `daily_tokens:*`（R19 part 2 + REV-FINAL-V6）

【是什么 What it IS】
- 一个**一次性运行**的脚本（非常驻 sweeper），目录 `ailinkmax-gateway/scripts/one-shot/`，文件 `migrate-daily-tokens-ttl.{sh, py}`（推荐 Python 以利精确 SCAN）。
- 跑 1 次：SCAN match `daily_tokens:*` → 每命中 key `TTL`；若返 -1（无 TTL）则 `EXPIRE key <DEFAULT_TTL_SECS>`（默认 48h，与 daily_tokens_key 注释一致）。
- 跑完即结束；运维在 T-W3-17 PR 上线后 48h 跑 1 次。

【不是什么 What it is NOT】
- ❌ **不是**常驻 sweeper（v2 REV-FINAL-V6 显式改）。
- ❌ 不是 Go drainer 改动。
- ❌ 不是 K8s CronJob（一次性，无需调度）。

【为什么 Why bug exists】
- 同 R19：历史无 TTL 的 daily_tokens key 在生产里已积累。T-W3-17 修了源头（新写入会带 TTL），但**存量需要清**。

【做什么 DO】
1. 新建脚本，用 `redis.scan_iter('daily_tokens:*', count=500)`、`pipeline` 批 TTL + EXPIRE。
2. 日志 stdout per 1000 keys。
3. 在 runbook (`docs/runbook/migrations.md`) 添加条目，说明：何时跑、参数、回滚策略（脚本只 EXPIRE，无破坏性；rollback 即 PERSIST，但此处不提供 rollback 因为 PERSIST 让 key 永生违反 R19）。

【不做什么 DON'T】
- ❌ 不要在 Pingora binary 内启 sweeper loop。
- ❌ 不要 DEL 任何 key（仅 EXPIRE）。
- ❌ 不要打 prod 直接跑——先 staging snapshot 验证。

【怎么做 HOW (具体)】

```python
# ailinkmax-gateway/scripts/one-shot/migrate-daily-tokens-ttl.py
import argparse, redis, sys

DEFAULT_TTL = 48 * 3600

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--url", required=True, help="redis://host:port/db")
    p.add_argument("--ttl", type=int, default=DEFAULT_TTL)
    p.add_argument("--dry-run", action="store_true")
    p.add_argument("--batch", type=int, default=500)
    args = p.parse_args()

    r = redis.from_url(args.url)
    fixed = 0
    skipped = 0
    cursor = 0
    while True:
        cursor, keys = r.scan(cursor=cursor, match="daily_tokens:*", count=args.batch)
        if keys:
            pipe = r.pipeline()
            for k in keys: pipe.ttl(k)
            ttls = pipe.execute()
            pipe = r.pipeline()
            for k, t in zip(keys, ttls):
                if t == -1:
                    if not args.dry_run:
                        pipe.expire(k, args.ttl)
                    fixed += 1
                else:
                    skipped += 1
            pipe.execute()
        if (fixed + skipped) % 10000 < args.batch:
            print(f"progress fixed={fixed} skipped={skipped}", flush=True)
        if cursor == 0:
            break
    print(f"done fixed={fixed} skipped={skipped} dry_run={args.dry_run}", flush=True)

if __name__ == "__main__":
    sys.exit(main())
```

**ADR：备选 A=一次性脚本（推荐 / V6）；备选 B=K8s CronJob（无意义重复扫）；备选 C=常驻 Rust 内 sweeper（v1 案 — 已被 V6 显式否决）。**

【源码验证 file:line Read】
- 已 Read `counter_store.rs:62-68` 字面 `pub fn daily_tokens_key(uid: &UserId, day_utc: &str) -> String { format!("counter:user:{}:daily_tokens:{}", ...) }` —— **注意 key prefix 是 `counter:user:`，不是 `daily_tokens:`**；脚本 match 必须改为 `counter:user:*:daily_tokens:*`！
- 此处即 v2 §0 重要 2 抽样反证案例之一：v1/v2 描述简称 `daily_tokens:*`，实际 key shape 完全不同。**此差异已记入 §98 偏差。**

修正脚本 match：`r.scan(cursor=cursor, match="counter:user:*:daily_tokens:*", count=args.batch)`。

【如何验证 VERIFY】
1. 单元：local Redis fixture 写 100 key 50 无 TTL → 脚本跑 → `TTL` ≥ 0 for 50 keys, 50 untouched。
2. dry-run：`--dry-run` 不改 Redis，只报数。
3. 输出：`logs/W3-T18-verify.txt`。

---

### T-W3-19 · Rust IpRateLimitGuard.sweep 补足（REV-FINAL-V4）

【是什么 What it IS】
- 改 `crates/gw-auth/src/ip_ratelimit.rs`（grep 命中 `IpRateLimitGuard` 在 gateway.rs:822、proxy/gateway.rs:325）。
- 加 sweep（与 Routing Sweepable 同接口，挂入 SweepRegistry T-W3-09）；evict per-IP 状态在窗口外。
- 与 INVESTIGATE-003 协同：sweep 的 O(N log N) 是否能被 DoS。

【不是什么 What it is NOT】
- ❌ 不动限速算法（fixed window）。
- ❌ 不动 trusted_proxies（WS-5 / WS-7 owner）。

【为什么 Why bug exists】
- 根因（REV-FINAL-V4 + INVESTIGATE-003）：Rust 侧 IpRateLimit 与 AuthFailureIpLimiter 像 Go S5 一样，存表无 sweep。

【做什么 DO】
1. Read `crates/gw-auth/src/ip_ratelimit.rs` 全文确认现 map 类型与字段。
2. 加 last_used / 类似 LRU 元数据。
3. impl Sweepable；T-W3-09 SweepRegistry 注册。
4. 同样路径处理 `AuthFailureIpLimiter`（`crates/gw-auth/src/auth_ip_ratelimit.rs`）。

【不做什么 DON'T】
- ❌ 不在 `check()` 内扫（同 Go S5 错）。
- ❌ 不调整窗口大小。

【怎么做 HOW (具体)】

```rust
// 同 Go T-W3-03 的精神：sweep 走 SweepRegistry tick (T-W3-09)。
impl Sweepable for IpRateLimitGuard { /* ... */ }
impl Sweepable for AuthFailureIpLimiter { /* ... */ }
```

【源码验证 file:line Read】
- Read `crates/gw-auth/src/ip_ratelimit.rs` 全文。
- Read `crates/gw-auth/src/auth_ip_ratelimit.rs` 全文。

【如何验证 VERIFY】
1. 单元：1k unique IP × 1 hit → sweep 一周期 → size 回 0（窗口外）。
2. 现有 `crates/gw-auth/tests/ip_filter.rs` 全绿。
3. 输出：`logs/W3-T19-verify.txt`。

---

### T-W3-20 · Go email-verify 限速移入 Redis（REV-FINAL-V4 子）

【是什么 What it IS】
- 改 `AILinkMax-Go/modules/auth/`（具体路径：grep `email_verify` / `verify_email`），把 in-mem 计数换成 Redis `INCR + EXPIRE`（与 T-W3-02 oauth bindtoken Redis 同模式）。
- 与 v1 G11 / WS-5 S5.7 协同；本 task 仅 WS-3 内存维度负责（cap + sweep）；WS-5 owner 负责 fail-closed 与 brute-force 防护语义。

【不是什么 What it is NOT】
- ❌ 不改邮件发送路径。
- ❌ 不修改前端 UI。

【为什么 Why bug exists】
- 根因（REV-FINAL-V4 sub-bullet "邮件验证限速移入 Redis"）：现有 mem 实现无 sweep；同 G11 + S5。

【做什么 DO】
1. grep `RedisEmailVerificationStore`（如已存在则直接用；v2 §4 WS-3 "邮件验证限速移入 Redis（已存在 RedisEmailVerificationStore），不在 in-mem"——表明已有 store，但限速可能仍在 in-mem）。
2. 限速 key 形如 `email_verify_limit:{normalized_email}`、`INCR` 后 `EXPIRE 1800`（30min 窗口 + 5 次 cap）。
3. 与 WS-5 S5.7 owner 协同同 PR。

【不做什么 DON'T】
- ❌ 不要 in-mem 兜底。
- ❌ 不要把 normalized_email 写日志（PII）。

【怎么做 HOW (具体)】
- 详细 code 由 grep 现有 store 定型后展开（执行者在此 task 内补 literal code 并 commit 时更新本节）。

【源码验证 file:line Read】
- `find AILinkMax-Go -name "*email*verify*"` 找 store 与 throttle site。
- Read 确认 in-mem map 在何处。

【如何验证 VERIFY】
1. e2e：同邮箱 1min 内 6 次 → 第 6 次 429。
2. 跨 Pod：两实例共享 Redis → 跨 Pod 同邮箱计数共享。
3. 输出：`logs/W3-T20-verify.txt`。

---

### T-W3-21 · CI lint：metric label allowlist + bounded_buf policy enforcement（REV-FINAL-V7 + V1 配套）

【是什么 What it IS】
- 一个 CI 改动，新增 `ailinkmax-gateway/scripts/check-metric-labels.sh`（已在 v2 WS-12 列入；本 task 是 WS-3 部分的具体规则贡献）。
- 规则：
  - metric site label key ∉ `{request_id, api_key_id, user_id, trace_id, ip}` （allowlist 反向：黑名单 ID 类）。
  - `gw_bounded_buf_overflow_total` label 必须严格 `{buf, policy}`。
  - `gw_routing_*` label 必须 `{name}` 或固定 `{strategy}`（不能含 ID 类）。

【不是什么 What it is NOT】
- ❌ 不写 Go 侧 lint（同名 lint Go 维度归 WS-12）。
- ❌ 不替代 `cargo clippy`。

【做什么 DO】
1. 写 shell 脚本用 `rg`（已是 dev dep）扫 `metrics::counter!\|gauge!\|histogram!` macro，正则提取 label list。
2. 与 allowlist 比对；非空差集 → exit 1 with diff print。
3. Hook 进 `scripts/check-invariants.sh`（已知存在）。

【不做什么 DON'T】
- ❌ 不要用 AST（syn parse macro 复杂；regex 在工程上够用）。
- ❌ 不要 silent skip 不可解析的 site；要 fail。

【怎么做 HOW (具体)】

```bash
#!/usr/bin/env bash
# ailinkmax-gateway/scripts/check-metric-labels.sh
set -euo pipefail
DENY=("request_id" "api_key_id" "user_id" "trace_id")
exit_code=0
while IFS= read -r line; do
  for d in "${DENY[@]}"; do
    if echo "$line" | grep -qE "\"${d}\"\s*=>"; then
      echo "FAIL: $line"
      exit_code=1
    fi
  done
done < <(rg --no-heading 'metrics::(counter|gauge|histogram)!' -A2 crates/)
exit $exit_code
```

【源码验证 file:line Read】
- Read `ailinkmax-gateway/scripts/check-invariants.sh` 确认 hook 风格。

【如何验证 VERIFY】
1. CI run：故意在某 PR 加 `metrics::counter!("x", "request_id" => …)` → CI fail。
2. 现有 main HEAD → CI pass（说明现 codebase 不含黑名单 label，配合 T-W3-16 完成清理）。
3. 输出：`logs/W3-T21-verify.txt`。

---

## 4. DoD（Definition of Done · 含 REV-005 阈值）

合并门：

1. 全部 21 task 单元 + 集成 + e2e 均 PASS（每 task 输出 `logs/W3-T*-verify.txt`）。
2. **Adversarial 上游 chaos**（WS-13 提供 harness）：
   - Slow-loris 1 Gbps `lz4+json` payload → archive RSS 上限 16 MiB + decode_rejected_total > 0。
   - Slow-loris cross-protocol body → upstream_body_accum reject + 502。
   - 10k unique-name tool call 单请求 → codec abort + Go 算 cost=0。
3. **10k key rotation × 1h**：routing DashMap size P95 ≤ soft_cap × 1.2；evicted_total > 0。
4. **Redis `counter:user:*:daily_tokens:*` 7 天扫描** 无 TTL 数 == 0（T-W3-18 跑过后）。
5. **Prometheus `/metrics` scrape size**：e2e-soak peak < 5 MB（v1 §4 WS-3 DoD 量化阈值）。
6. **24h soak**：RSS P95 波动 ≤ ±10%；`gw_bounded_buf_overflow_total > 0` 可见（说明 cap 在线）；`gw_routing_evicted_total > 0`。
7. **Grafana 面板** 迁移完成（T-W3-16 + WS-10 协同），删 label 不致 obs 失声。
8. **CI lint** 全通过：metric label allowlist、bounded_buf policy required、ID 类 label 黑名单。
9. **对抗复审**（v2 §6.1 D6/D10）通过：复审 sonnet 重点核对：(a) BoundedBuf policy 每 buffer 显式声明；(b) Sweepable predicate 每 tracker 与 REV-FINAL-V2 一致；(c) RedosTracker 阈值 1 与 RE2 linear-time 一致；(d) counter_store key prefix 与 §98 偏差一致。
10. INVESTIGATE-003（Rust IP rate limit sort DoS）闭合。

---

## 5. 排序与并行（与 v2 §6.2 协同）

| Track | Task |
|---|---|
| **C**（Senior Rust #2 — proxy/codec） | T-W3-04（trait 基础） → T-W3-05 / 06 / 07 / 08 → T-W3-16 |
| **D**（Senior Rust #3 — routing/tunnel） | T-W3-09（Sweepable 基础） → T-W3-10 / 11 / 12 / 13 / 14 → T-W3-19 |
| **F1**（Senior Sec #1 — Go fail-closed） | T-W3-01（archive cap）→ T-W3-02（OAuth Redis）→ T-W3-20（email verify）|
| **E**（Senior Go #3 — shared infra） | T-W3-03（API rate limiter LRU）|
| **F2**（Senior Sec #2 — Rust fail-closed） | T-W3-15（RedosTracker）|
| **B**（Senior Rust #1 — ledger） | T-W3-17 / T-W3-18（counter_store）|
| **G**（SRE） | T-W3-21（CI lint） + WS-10 obs migration |

强依赖：T-W3-04 / T-W3-09 在所有其他 task 之前；T-W3-16 / T-W3-21 在所有 metric 改动落地后做最终扫尾。

---

## 6. 与 v2 各处的协调点

| 点 | 协调 |
|---|---|
| WS-1 R7 / R24 502 `GW_UPSTREAM_PROTOCOL_DECODE_ERROR` | T-W3-05 复用同错误码风格 `GW_UPSTREAM_BODY_TRUNCATED` |
| WS-4 Supervised / supervised_spawn | T-W3-09 routing_sweep 已用；T-W3-03 Go cleanup 待 WS-4 land |
| WS-6 REV-FINAL-H6.1 preflight buffer 拆三种 probe | T-W3-05/06/07 三个 buffer cap 与 H6.1 分类一致 |
| WS-6 REV-FINAL-H6.4 mpsc bounded + try_send | T-W3-08 codec overflow 与 H6.4 archive overflow 同根 (内存有界)，不重叠 |
| WS-7 REV-FINAL-H7.3 rate limiter cleanup | T-W3-03 同 PR |
| WS-8 REV-FINAL-H8.4 ToolCallEnd 信号 | T-W3-08 codec abort 时 ToolCallEnd 不再 emit；与 H8.4 兼容 |
| WS-10 obs migration | T-W3-14 / T-W3-16 必须与 WS-10 owner 同 PR 节奏 |
| WS-12 CI lint 框架 | T-W3-21 是 framework 内一条规则 |
| WS-13 chaos harness | T-W3 DoD §2 全程依赖 |

---

## 98. 偏差记录（v2 §0 重要 2 要求；本子计划起草时发现的源码 ≠ 文档处）

| ID | 文档声明 | 源码事实 | 处置 |
|---|---|---|---|
| D-W3-01 | v1/v2 描述 R19 key 为 `daily_tokens:*` | 实际 `counter:user:{eid}:daily_tokens:{day}`（counter_store.rs:66-68） | T-W3-18 脚本 match 改为 `counter:user:*:daily_tokens:*`；本表声明已记入 |
| D-W3-02 | v1 描述 routing_sweep 未实现 | `routing_sweep.rs` 已存在并跑 ModelFrequencyTracker / ReservationGlobalView / LatencyTracker / AffinityManager 4 项 | T-W3-09 是**升级**到 Sweepable trait，而非全新搭建；不删现 spawn 入口 |
| D-W3-03 | v2 REV-FINAL-V5 假设原 `BUDGET_MS` 与 `TIMEOUT_THRESHOLD` 字面 | 已 Read 至 line 252 验 RE2 doc + check_with_budget；具体阈值常量在 line 138 附近，**执行者必须 Read 1-200 找字面值后再改** | T-W3-15 执行段 |
| D-W3-04 | v1 R15 提"inflight_reaper Prom 带 request_id" | 已 Read inflight_reaper.go：现 log 用 `slog` 带 `request_id`，**未发现裸 metric 带 request_id label**；R15 的真实 hit 可能在 Rust 的 ledger/inflight_reconciler 而非 Go reaper | T-W3-16 执行段必须先 grep `metrics::*("request_id"` Rust 与 Go 双侧覆盖；以源码为准 |
| D-W3-05 | v1/v2 描述 G14 oauth bind-token TTL 10 min | 源码 service.go:344 字面 `s.now().Add(10*time.Minute)`；v2 §4 WS-3 说"5min" | T-W3-02 采用 v2 5min；执行者确认无业务流程要求 10min（如有 → 派 sonnet 调查并写 ADR） |

---

## 99. INVESTIGATE 列表（强制清零）

| ID | 待证 | 谁负责 |
|---|---|---|
| INVESTIGATE-W3-01 | INVESTIGATE-003（v2 §99）：IpRateLimitGuard / AuthFailureIpLimiter sort 在生产配置下是否真 DoS 可触达 | T-W3-19 owner（Track D） |
| INVESTIGATE-W3-02 | T-W3-04 BoundedBuf 引入 `gw-core` 是否破坏 gw-codec / gw-proxy 现有 crate 边界（是否引循环） | T-W3-04 owner 实施前先 grep `gw-core` 依赖图 |
| INVESTIGATE-W3-03 | T-W3-08 ctx.usage.aborted 字段是否已存在；若无须与 WS-1 / WS-6 协同定义 | T-W3-08 owner |
| INVESTIGATE-W3-04 | T-W3-15 grep 验 BUDGET_MS / TIMEOUT_THRESHOLD 当前数值；若与 v1 描述（10）不符以源码为准 | T-W3-15 owner |
| INVESTIGATE-W3-05 | T-W3-16 grep 全 workspace 找所有 ID 类 label 实际 hit list；与 R15 / REV-FINAL-V7 描述差额录入 | T-W3-16 owner |
| INVESTIGATE-W3-06 | T-W3-20 现有 RedisEmailVerificationStore 是否已 cover 限速；若是则本 task 退化为"接 Sweepable + cap 化" | T-W3-20 owner |

每条闭合记录在 `docs/issues/2026-06-17-investigates/INVESTIGATE-W3-NN.md`。

---

## 100. 修改历史

| 日期 | 修改 | 作者 |
|---|---|---|
| 2026-06-17 | 初稿（21 task / 全 8 段格式 / 6 INVESTIGATE / 5 偏差） | sonnet（per 用户委托） |

— 子计划结束 —
