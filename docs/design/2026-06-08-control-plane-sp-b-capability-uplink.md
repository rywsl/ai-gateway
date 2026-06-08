# Control Plane SP-B — Capability/State Uplink (Rust→Go) — Design

> **Brainstorm 产出 (2026-06-08).** Rust↔Go 透明控制平面第二条线:SP-B = 能力/状态上行(Rust 对 Go 透明)。依赖 **SP-A**(配置生效值回报、推送频率配置)。姊妹:[SP-A](2026-06-08-control-plane-sp-a-config-downlink.md)、SP-C(命令通道,待 design)。未建成,放 `docs/design/`。

## 现状盘点(SP-B 大半已存在,关键修正)

勘探发现实例注册/心跳/运行状态**大部分已有**,SP-B 是**扩展**而非新建:

| 暴露内容 | 现状 | SP-B 动作 |
|---|---|---|
| 实例 ID + 注册 | ✅ `gw_ledger::instance_id::resolve` L0(`GW_INSTANCE_ID`)/L1(OnceLock)/L2(`gateway:instance:<hostname>` 无TTL,稳定跨重启) | 复用 |
| 心跳 | ✅ `gateway:instance:<id>:heartbeat`(TTL 600s / 60s renew)+ **Go reaper 已消费** | 复用,可加 `version` 字段 |
| 运行状态 | ⚠️ HTTP `/healthz/{circuits,key,resilience,tunnel,strategies}` 已完整,**但非 redis** | 镜像到 redis(覆盖式慢快照) |
| impersonate 身份 | ✅ `IdentityPersistQueue` + `PgSnapshotWriter` 已持久化 PG | Go 直接读 PG;runtime 快照含活跃统计 |
| 静态能力清单 | ❌ 无 redis 暴露 | 新增(boot 写一次) |
| 配置生效值回报 | ❌ 无 | 新增(reload 事件写,闭环 SP-A) |
| key 轮换时间 | ❌(F2 K-C 产生) | 随 K-C 写入 runtime 快照 |

## 命名空间(修正骨架:复用 `gateway:instance:<id>`,不另起 `gw:*`)

骨架最初提议 `gw:capability/gw:effective/gw:runtime` 是**双轨**;既然 `gateway:instance:*` 体系已存在且 Go 已消费,SP-B 全部挂在**同一 instance_id 下**:

```
gateway:instance:<hostname>            (现有: L2 注册, 无 TTL)
gateway:instance:<id>:heartbeat        (现有: TTL 600 / 60s renew, Go reaper)
gateway:instance:<id>:capability       (新: 静态能力 JSON, boot 写一次, 随版本变)
gateway:instance:<id>:effective        (新: 配置生效值 table→applied_version, reload 写)
gateway:instance:<id>:runtime          (新: 运行状态慢快照, 覆盖式周期写, 不积压)
```

> `<id>` = resolve 出的 instance_id(env 覆盖优先,否则 hostname→UUID)。Go 用现有心跳 reaper 发现存活实例,再读其 `:capability`/`:effective`/`:runtime`。

## SP-B 锁定决策(2026-06-08 brainstorm)

| # | 决策点 | 结论 |
|---|--------|------|
| D1 | **命名空间** | 复用 `gateway:instance:<id>:*`,不另起 `gw:*`;Go 经现有心跳发现实例 |
| D2 | **运行状态通道** | **覆盖式慢快照**:`SET gateway:instance:<id>:runtime <json>`(非 stream → 天然不积压),Go 读最新 |
| D3 | **推送频率** | **Go 控频**——周期是配置项 `control_plane.runtime_push_interval_ms`(经 **SP-A** 下发热生效),默认 2000ms,**下限 1000ms**(校验拒绝 <1s) |
| D4 | **快照范围** | 只推**慢变**:key-health/circuit、tunnel 节点、strategies、能力指针、配置生效 version、impersonate 活跃统计、key 轮换时间。**高频 in-flight/实时 QPS 留 Prometheus/metrics,不推 redis**(避免写放大) |
| D5 | **静态能力 / 生效值 / 身份** | 能力 boot 写一次(版本变才重写);生效值 reload 事件写(闭环 SP-A);impersonate 身份留 PG(Go 直读),runtime 仅放活跃统计 |

## 组件设计:四个 redis key

### 1. `:capability` — 静态能力清单(boot 写一次)
```json
{
  "instance_id": "…", "gateway_version": "1.5.0+<git_sha>", "started_at_ms": 0,
  "protocols": ["openai","anthropic","gemini"],
  "cli_variants": ["claude:cli","openai:cli","openai:compact","gemini:cli"],
  "routing_strategies": ["FixedOrder","RoundRobin","LoadBalance","WeightedRr",
                         "CostFirst","LatencyFirst","CacheAffinity","Sticky"],
  "features": {"tunnel": false, "egress_impersonate": true, "credit_lease": true,
               "detailed_log": true, "task_worker": false},
  "impersonate_identities": ["claude_code_chrome131","openai_codex","gemini_cli"]
}
```
- 写时机:boot 完成后一次 `SET gateway:instance:<id>:capability <json>`(无 TTL,或长 TTL + 心跳续)。
- 来源:编译期常量(协议/策略/身份枚举)+ 启动时 feature/配置开关(`Wired`)。版本/feature 变(=新部署)才重写。

### 2. `:effective` — 配置生效值回报(闭环 SP-A,reload 事件写)
```json
{ "routing": 7, "ratelimit": 3, "auth": 1, "billing": 5, "protection": 2, "…": 0 }
```
- `table → applied_version`。Go 下发配置(SP-A)后,读此 key 确认**这个实例**是否已应用到目标 version。
- 写时机:SP-A reload 成功后(SP-A 数据流 step 10)`HSET gateway:instance:<id>:effective <table> <version>`。reload 被拒(validate 失败)则**不更新**该表 version → Go 看到 version 滞后即知未生效。

### 3. `:runtime` — 运行状态慢快照(覆盖式周期写,**不积压**)
```json
{
  "ts_ms": 0,
  "key_health": [{"key_id":"…","format":"openai","circuit":"closed",
                  "in_cooldown":false,"consecutive_failures":0}],
  "tunnel": [{"node_id":"…","transport_present":true,"last_pong_age_s":2}],
  "oauth_rotation": [{"key_id":"…","last_refresh_ms":0,"access_expires_at_ms":0}],
  "impersonate_active": {"openai_codex": 3, "claude_code_chrome131": 1},
  "inflight_total": 42
}
```
- **覆盖式** `SET gateway:instance:<id>:runtime <json>`(非 stream)→ Go 永远读到最新一帧,旧帧被覆盖,**结构上不可能积压**。
- **复用现有 snapshot 逻辑**:`KeyHealthStore::snapshot_entries()`(已存在,返回 `Vec<KeyHealthEntrySnapshot>`)、`/healthz/tunnel` 的节点序列化、`/healthz/resilience`。SP-B 的 pusher = 周期调这些 snapshot 函数 → 序列化 → `SET`。**工程量小**(不重造 snapshot)。
- `oauth_rotation` 由 F2 K-C 的刷新事件填充(key 轮换时间);K-C 未落地前该数组为空。
- 高频 `inflight_total` 可放(单标量便宜);**逐 key/逐请求的实时明细不放**(留 Prometheus)。

### 4. `:heartbeat` — 复用现有,可选加 `version`
现状 `SET …:heartbeat <now_ms> EX 600`(60s renew)保持不变。实例元数据(version/started_at)已在 `:capability`,心跳无需扩展;**若** Go 想从心跳直接读版本,可改写为 `HSET …:heartbeat ts <now_ms> ver <gateway_version>` + `EXPIRE`。默认不改,留 D5 决定。

## 推送机制 + 写时机

新增 `StatePusher`(`gw-store/src/control_plane.rs` 或 `gw-gateway`),持 redis pool + snapshot 句柄:

```
StatePusher loop:
  interval = control_plane.runtime_push_interval_ms (SP-A 配置, 默认2000, 下限1000)
  每 tick:
    snap = { key_health: KeyHealthStore::snapshot_entries(),   ← 复用现有
             tunnel: tunnel_snapshot(), oauth_rotation: …, inflight_total: … }
    SET gateway:instance:<id>:runtime <json> EX <2×interval 或 心跳TTL>   ← 覆盖, 带TTL
  interval 变更(SP-A 热下发)→ 重建 tick(B 类 reconfigure)
```

| key | 写时机 | 写法 | TTL |
|---|---|---|---|
| `:capability` | boot 一次 + 版本/feature 变 | `SET` | 心跳级(随心跳续)或无 |
| `:effective` | SP-A reload 成功(事件) | `HSET <table> <ver>` | 心跳级 |
| `:runtime` | 周期(Go 控频 2s) | `SET`(覆盖,不积压) | `2×interval` |
| `:heartbeat` | 60s(现有) | `SET …  EX 600` | 600s |

> **TTL 是死实例自清理的关键**:实例宕机后,所有 `gateway:instance:<id>:*` 在 TTL 内过期,Go 不会读到 stale 状态。`:runtime` 用 `2×interval`(短),`:capability`/`:effective` 随心跳续期。

## go-contract(`parts/08-control-plane.md` 的 SP-B 部分)

| 方向 | 载体 | 写方 | 读方 |
|---|---|---|---|
| Rust→Go | `gateway:instance:<id>:capability` (JSON) | Rust(boot) | Go 控制台/调度 |
| Rust→Go | `gateway:instance:<id>:effective` (hash table→ver) | Rust(reload) | Go 验证下发生效 |
| Rust→Go | `gateway:instance:<id>:runtime` (JSON 覆盖) | Rust(2s) | Go 监控 |
| Go→Rust | `control_plane.runtime_push_interval_ms`(经 SP-A `entity:config`) | Go | Rust(控频) |

实例发现:Go 复用**现有心跳 reaper** 已知的存活 instance_id 集合,再读各 `:capability`/`:effective`/`:runtime`。

## 实现影响(crate / 文件)

- **`gw-store`**(或 `gw-ledger`,instance_id 在此):新 `control_plane.rs` — `StatePusher` + capability/effective 写入 API。
- **`gw-health` / `gw-gateway`**:把 `health.rs` 里 tunnel/resilience 的 JSON 生成抽成可复用函数(今天嵌在 HTTP handler),供 `:runtime` 与 `/healthz/*` 共用,避免两份逻辑漂移。
- **SP-A 联动**:`:effective` 写入点挂在 SP-A 的 reload 成功 hook;`control_plane.runtime_push_interval_ms` 是 SP-A 的一个 config table。
- **能力来源**:协议/策略/身份用编译期枚举;feature/开关读 `Wired`。

## 风险与开放问题

1. **(开放)snapshot 序列化成本** — `key_health` 有 N×M 行,每 2s 全量序列化+SET。N 大时成本上升。缓解:`:runtime` 只推**异常态**(circuit≠closed / in_cooldown)+ 健康计数;或 Go 控频调长。默认全量,N 大再优化。
2. **(已答)死实例 stale 状态** — 全部 `gateway:instance:<id>:*` 带 TTL(见上表),宕机自清理。
3. **(开放)同 hostname 多实例** — instance_id 默认基于 hostname;k8s 每 Pod 不同 hostname 则 OK,裸机多进程需 `GW_INSTANCE_ID` 显式区分。SP-B plan 展开前确认部署形态。
4. **(说明)key_health 是 per-instance 视图** — 每实例有自己的本地 circuit 状态;Go 聚合时须知这是分实例的,非全局。

## 与 SP-A / SP-C 接口

- **依赖 SP-A**:推送频率配置下发 + `:effective` 写入点在 reload hook。
- **给 SP-C**:命令 ACK 可复用同一 `gateway:instance:<id>` 前缀(`:cmd_ack`);实例发现机制共享。

## Self-review(brainstorming spec gate)

- **Placeholder**:无 TBD;不确定项列「开放问题」并标缓解/默认。
- **内部一致**:4 key 结构 ↔ 写时机表 ↔ 复用点(`snapshot_entries`/health.rs)一一对应。
- **Scope**:聚焦 SP-B 上行;SP-A/SP-C 仅作接口引用。
- **歧义**:snapshot 成本、同 hostname 多实例显式标开放,不留隐性假设。


