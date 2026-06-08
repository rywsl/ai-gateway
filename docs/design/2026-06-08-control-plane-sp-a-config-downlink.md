# Control Plane SP-A — Config Downlink (Go→Rust) — Design

> **Brainstorm 产出 (2026-06-08).** 这是 **Rust↔Go 透明控制平面** 三条线之一。SP-A = 配置下行(Go 控制 Rust)。姊妹设计 SP-B(能力/状态上行)、SP-C(命令通道)各自 brainstorm。实现计划由 `superpowers:writing-plans` 从本 doc 派生到 `docs/plans/`。
>
> 本 doc 描述**尚未建成**的设计,因此放在 `docs/design/`,不在 as-built 的 `docs/spec/`(后者由 `check-spec.sh` 强制"只描述已存在代码")。SP-A 落地后,把已建成的部分迁入 `docs/spec/` 对应域(配置加载属架构层,迁 `docs/architecture.md` §Configuration)。

## Program 上下文:Rust↔Go 透明控制平面

目标:**Go 完全控制 + Rust 对 Go 透明**,全部经 redis,复用现有 `entity + events + ArcSwap` 机制(不造新轮子)。

| 线 | 方向 | 内容 | redis 命名空间 |
|---|---|---|---|
| **SP-A** 配置下行 | Go→Rust | 配置=第14种 entity,热下发+热重载 | `entity:config:<table>` + `events:entity ns=config` |
| **SP-B** 能力上行 | Rust→Go | 心跳/注册、静态能力、配置生效值回报、运行时状态 | `gw:instance:<id>` `gw:capability:<id>` `gw:effective:<id>` `gw:runtime:<id>` |
| **SP-C** 命令通道 | Go→Rust | 运行时指令(cooldown/refresh/reload/drain)+ ACK | `commands:gw:<id>` + `gw:cmd_ack:<id>` |

依赖顺序:**SP-A(基础)→ SP-B(回报配置生效值,依赖 A)→ SP-C(reload-config 依赖 A、ACK 依赖 B)**。一份新双向 go-contract:`docs/go-contracts/parts/08-control-plane.md`。

## SP-A 锁定决策(2026-06-08 brainstorm)

| # | 决策点 | 结论 |
|---|--------|------|
| D1 | **Bootstrap 边界** | 连接+端口留本地 env(`redis.url`/`kms.kek_hex`/`pgsql.url`/`*_port`);其余业务配置走 redis |
| D2 | **权威源 (source of truth)** | 配置=**第 14 种 entity**:PgSQL 为源 + redis L1 缓存 + `events:entity ns=config` 失效/重载信号。复用现有 13-entity 机制,与 F2「go 配置」一致 |
| D3 | **热重载范围** | **X 全做(零重启)**:每请求读字段 ArcSwap 热替;后台 loop 配置 drain+rebuild+swap 热重建 |
| D4 | **粒度** | 按表分 `entity:config:<table>`(`ns=config, id=<table>`),每表独立 `version`,**改谁重载谁** |
| D5 | **Fallback** | `default.toml` = 内置默认值 + schema 真相源 → redis 覆盖 → 热重载。env 只剩 D1 的 bootstrap |
| D6 | **物理下限** (socket/pool/KEK) | 走 **Pingora graceful upgrade**(SIGHUP 零停机进程替换),不是 in-place 热重载 |

## 架构总览

**配置生命周期(boot + 热重载统一）:**

```
                 ┌─────────────────────────────────────────────┐
   boot 启动      │ default.toml (默认值 + schema 真相源)         │  ← 内置兜底
                 │      ↓ 覆盖                                   │
                 │ redis L1: HGETALL entity:config:<table>      │  ← Go 写的权威值
                 │      ↓ miss 兜底                              │
                 │ PgSQL: SELECT payload,version FROM config_*  │  ← source of truth
                 └─────────────────────────────────────────────┘
                                   ↓ 反序列化 + validate
                        ConfigCell { ArcSwap<TableCfg> } ×N 表
                                   ↓ boot 时 wire 进各子系统
   运行热重载     events:entity {ns=config, id=<table>}
                                   ↓ stream_consumer 分发
                        ┌──────────┼───────────┬──────────────┐
                  A 类:ArcSwap   B 类:drain+   C 类:graceful
                  原子热替        rebuild+swap   upgrade(SIGHUP)
```

**三类字段三条热路径**(分类见下一段「组件设计」):
- **A 每请求读** → 子系统持 `Arc<ArcSwap<TableCfg>>`,reload 时 `.store(new)`,下个请求自然读到新值。
- **B 后台 loop** → reload 时构造新 loop / 新资源,drain 旧的,原子 swap 句柄;旧在途任务排空后退出。
- **C socket/pool/KEK** → 不能 in-place;配置变更标记触发 Pingora graceful upgrade,新进程用新配置 boot、转移 listening fd、drain 旧连接。

**env 的残余职责(仅 bootstrap,鸡生蛋下限）:** 只剩 `GW_REDIS__URL`、`GW_KMS__KEK_HEX`、`GW_PGSQL__URL`、`GW_SERVER__*_PORT`。业务字段的 `GW_*` 覆盖**取消**(env 退出业务字段,2026-06-08 定)——避免 env 残留静默盖掉 redis 热下发。

## 组件设计

### 1. `ConfigStore` — 配置 entity 加载面

新增 `crates/gw-store/src/config_store.rs`(或并入 `EntityStore`),提供 per-table 的三级加载,镜像现有 `load_entity()`:

```
config_or_load(table) -> Arc<TableCfg>:
   L1 Redis  HGETALL entity:config:<table>     命中→反序列化→validate→返回
   L2 PgSQL  SELECT payload,version FROM config_<table> WHERE id='global'
   L3 写回    回填 Redis hash + TTL
   L0 兜底    三级全 miss → default.toml 内置默认值(boot 永不失败)
```

- 每个 `<table>` 对应一个 PG 表 `config_<table>`(payload JSONB + version),由 Go 写。迁移:新增 `00NN_config_tables.sql`。
- `TableCfg` = 现有 `Config` 各子结构(`RoutingCfg`/`RateLimitCfg`/...)直接复用,加 `#[serde(default)]` 让 redis 缺字段时回落默认。
- **validate 失败 = 拒绝该次 reload**(保留旧值),并经 SP-B 上报 `config_reload_rejected{table,reason}`。fail-safe:坏配置不接管,旧配置继续服务。

### 2. 30 表三类分类(SP-A 工程量地图)

| 类 | 表 / 字段 | reload 机制 | 备注 |
|---|---|---|---|
| **A 每请求读**<br/>ArcSwap 原子热替 | `ratelimit.global_rpm` · `security.trust_xff` · `signature.required` · `auth.hard_cap_*` · `ip_filter.allowlist_mode` · `adaptive_rpm.*` · `error.classification/passthrough` · `body_rules.*` · `protection.*` · `billing` 阈值 | 子系统持 `Arc<ArcSwap<TableCfg>>`,`.store(new)` | 最简单。今天这些是 boot 时 copy 进 `Wired` 的标量/小结构 |
| **A′ 每请求读但重建较重** | `routing.presets/sticky_ttl` | 重建 `StrategySet` 再 ArcSwap | `presets` 要重新编译成 `HashMap<String,DimWeights>`;路由本身每请求,语义属 A |
| **B 后台 loop/service**<br/>drain+rebuild+swap | `detailed_log.*`(archive loop) · `ip_ratelimit.*`(sweep) · `task.*`(runtime) · `health.quota_probe.*`(scheduler) · `tunnel.*` 非-enabled 字段(pool 模板) · `egress.*` 非-bind 字段 · `admin.upstream_url` | 建新 loop/资源 → drain 旧 → 原子 swap 句柄 | 见 §3 `Reconfigurable` |
| **B→C 边界**(mpsc/池容量) | `ledger.channel_capacity/batch_size/flush_interval_ms` | channel 容量不可 resize → 重建 flusher + drain 旧 channel | `batch_size`/`flush_interval` 可热改;`channel_capacity` 实际接近 C |
| **B→A 边界** | `loader.cold_load_timeout_ms` | `EntityStore.cold_load_timeout` 改 `AtomicU64` 直接热改 | 名义在 loop,实则一个时长字段 |
| **C 物理 boot-only**<br/>graceful upgrade | `server.*_port` · `redis.url/pool_size` · `pgsql.url/max_connections` · `kms.kek_hex` · `egress.bind_*` · `tunnel.enabled`(翻转=重建整 stack) | SIGHUP 进程替换(§3.C) | socket bind / 池 size / KEK / 子系统存在性 |

> **`tunnel.enabled` 注意**:`false→true` 要构建整个 `TunnelStack`(spawn watcher + pool),等价 boot,归 C(graceful upgrade)。已 enabled 后改 pool 模板(heartbeat/backoff/max_streams)归 B。

### 3. 三类 reload 机制

#### A — ArcSwap 原子热替
把今天 boot 时 copy 进 `Wired` 的字段,改成各子系统持有 `Arc<ArcSwap<TableCfg>>`:

```rust
// boot
let cell = Arc::new(ArcSwap::from_pointee(store.config_or_load("ratelimit")));
// 每请求读(零锁)
let rpm = cell.load().global_rpm;
// reload
cell.store(Arc::new(new_cfg));   // 下个请求自然读到新值
```
今天读 `Wired.global_rpm` 的点改读 `cell.load()`。无 drain、无重建,最简单。

#### B — `Reconfigurable`(drain + rebuild + swap)
后台 loop/service 实现统一抽象:

```rust
trait Reconfigurable: Send + Sync {
    fn reconfigure(&self, new: TableCfg) -> Result<(), ReconfigError>;
}
```
实现协议:① 构造新资源(新 sweep loop / 新 archive writer);② 原子 swap 内部句柄(`ArcSwapOption<Handle>`);③ 向旧资源发 shutdown,**drain 在途任务**后退出(如 ledger flusher 必须 flush 完旧 batch、ip_ratelimit sweep 完成当前扫描)。失败→保留旧,上报。**幂等**:同 version 重复事件不重建。

#### C — Pingora graceful upgrade(零停机进程替换)
socket/pool/KEK 不能 in-place。当前 `run_forever()`(`lib.rs:345`)改为支持 Pingora 的 graceful upgrade:
- boot 时启用 upgrade socket(Pingora `Server` 的 upgrade 能力 + `--upgrade` 路径)。
- **触发**:`stream_consumer` 检测到 C 类表/字段在 redis 变更(或 SP-C 的 `reload-config --restart` 指令)→ 发起 `SIGQUIT`-based graceful upgrade。
- 新进程**重新 boot**:读 `default.toml` + redis(C 字段从这里拿新值)+ env(`kek_hex` 等 bootstrap)→ Pingora 转移 listening fd → 旧进程 drain 连接后退出。对外零停机。
- **KEK 轮换特例**:`kek_hex` 在 env,改它 = 更新 env(由编排层/Go 注入)+ 触发 upgrade;新进程用新 env boot。注意现有 `KekCache` 已支持 DualRead 热轮换(`encrypt.rs`),数据列的 KEK 轮换走那条,**不需** upgrade;upgrade 只针对"换 bootstrap KEK 本身"这种罕见场景。

### 4. 变更分发:`events:entity ns=config`

`stream_consumer.rs::apply_entity_event` 增加 `ns=config` 分支(与现有 13 ns 并列):

```
on events:entity {ns=config, id=<table>, v=<version>}:
    DEL entity:config:<table>            (复用现有 L1 失效)
    ConfigReloadRegistry.dispatch(table) → 按表查 A/B/C handler:
        A → reload TableCfg, cell.store()
        B → store.config_or_load(table) → handler.reconfigure(new)
        C → 标记 pending-upgrade, 发起 graceful upgrade
    成功 → 经 SP-B 写 gw:effective:<id> 回报新 version(闭环)
    失败 → 保留旧 + 上报 gw:runtime config_reload_rejected
```

`ConfigReloadRegistry: HashMap<&'static str, ReloadKind>`,boot 时注册每表归属(A/A′/B/C),是 SP-A 的中枢路由表。

## 数据流(端到端示例:Go 调 routing 权重)

```
1. 运维在 Go 控制台改 routing 权重
2. Go: UPDATE config_routing SET payload=…, version=version+1 WHERE id='global'
3. Go: XADD events:entity * ns config id routing v <new_version>
4. Rust stream_consumer.tick() XREADGROUP 收到
5.   DEL entity:config:routing            (L1 失效)
6.   dispatch("routing") → A′: store.config_or_load("routing")
7.     L1 miss → L2 PG SELECT → 反序列化 RoutingCfg → validate
8.     重建 StrategySet(presets) → routing_cell.store(new)
9. 下个请求 pick() 读 routing_cell.load() → 新权重生效(零重启)
10. SP-B: HSET gw:effective:<id> routing.version <new_version>   (闭环回报)
```

## 错误处理 / fail-safe

- **validate 失败** → 拒绝该次 reload,保留旧 `TableCfg`,上报 `config_reload_rejected{table,reason}`。坏配置永不接管。
- **B `reconfigure` 失败** → 保留旧 loop/资源,上报,旧继续服务。
- **C graceful upgrade 失败** → Pingora 回退,旧进程继续 `run`(upgrade 是"新进程就绪才转 fd")。
- **boot 时 redis+PG 全不可达** → L0 `default.toml` 兜底,系统仍能起(降级:无 Go 下发,跑内置默认)。
- **原子性**:D4「改谁重载谁」→ 每表独立 reload;**跨表一致性不保证**(见开放问题 config epoch)。

## go-contract(`parts/08-control-plane.md` 的 SP-A 部分)

| 方向 | 载体 | 写方 | 说明 |
|---|---|---|---|
| Go→Rust | PG `config_<table>`(payload JSONB + version) | Go | 配置权威存储,每表一行 `id='global'` |
| Go→Rust | `XADD events:entity ns=config id=<table> v=<version>` | Go | 改配置后必发,触发热重载 |
| 附录 | `default.toml` schema | — | 契约附录:Go 配置中心据此生成 UI + 校验 |
| Rust→Go | `gw:effective:<id>`(SP-B) | Rust | 生效 version 回报,闭环验证下发是否生效 |

## 实现影响(crate / 文件)

- **`gw-store`**:新 `config_store.rs`(三级加载);新 migration `00NN_config_tables.sql`;`stream_consumer.rs` 加 `ns=config` 分支 + `ConfigReloadRegistry`。
- **`gw-gateway`**:`lib.rs` bootstrap 大改造——A 类字段从 copy-into-`Wired` 改为 `Arc<ArcSwap<TableCfg>>` cells;各 B 类 loop owner 实现 `Reconfigurable`;`run_forever()` → Pingora graceful-upgrade wiring。
- **`config.rs`**:各 `TableCfg` 加 `#[serde(default)]`;`load()` 业务表**移除 `Env::prefixed` merge**(env 退出业务字段,§风险-2),仅 bootstrap 字段(`redis`/`kms`/`pgsql`/`server`)保留 env 来源。
- **各子系统 crate**:请求路径读 `cell.load()` 取代读固化值。

## 风险与开放问题

1. **(高危)B 类 drain 不丢数据** — `ledger` flusher 重建必须 flush 完旧 batch、`InflightReaper` 重建不能丢在途账。这是 SP-A 最危险的部分,W0 CI + soak harness 必须覆盖。
2. **(已定 2026-06-08)env vs redis 优先级** — 业务字段 `redis > default.toml`,**env 退出业务字段**(只管 bootstrap:`redis.url`/`kek_hex`/`pgsql.url`/`*_port`)。消除"env 残留静默盖掉 redis 热下发"的歧义。实现:`config.rs::load()` 对业务表**不再 merge `Env::prefixed`**,仅 bootstrap 字段保留 env 来源。
3. **(开放)graceful upgrade × k8s** — Pingora fd 转移要求同进程树;与 k8s 滚动更新语义如何共存(两套零停机机制),需在 SP-A plan 展开前定部署形态。
4. **(开放)跨表配置原子性** — 多表联动变更(如同时改 routing+ratelimit)目前各自独立生效,中间态可能不一致。若需要,引入 `config epoch`(Go 发一个 epoch 栅栏,Rust 集齐再整体 swap)。默认不做。

## 与 ROADMAP / W0 的关系

独立新 program,**不在 W0–W8 内**;但这是大改动,**必须先有 W0 的 CI 门禁**(test/clippy/invariants/soak)兜底再动。建议执行序:`W0 先行 → SP-A → SP-B → SP-C`。

## Self-review(brainstorming spec gate)

- **Placeholder**:无 TBD;不确定项显式列为「开放问题」并标推荐。
- **内部一致**:A/B/C 分类 ↔ §3 三机制 ↔ §4 分发表一一对应。
- **Scope**:聚焦 SP-A 单线;SP-B/SP-C 仅作依赖引用,不混入。
- **歧义**:env/redis 优先级、跨表原子性两处显式标为待定,不留隐性假设。



