# Part 08 — Control Plane(Rust↔Go 透明控制平面)

> 本 part 描述 **Rust 网关与 Go 管理面之间的控制平面契约**:Go 完全控制 Rust(配置下发 + 命令),Rust 对 Go 完全透明(能力/状态上行)。索引见 [main.md](../main.md)。
>
> **状态:Rust Wave 1 部分实现** —— Rust 已落地 `gw-core::control_plane` DTO/key helpers、`gw-gateway` capability/runtime 覆盖式快照 writer、`commands:gateway` delivery consumer 和安全命令 handler 子集。SP-A 全量配置下行、`:effective` 生效版本闭环、Go 侧 command batch/delivery 管理仍按本 part 和设计 doc [SP-A 配置下行](../../design/2026-06-08-control-plane-sp-a-config-downlink.md) / [SP-B 能力上行](../../design/2026-06-08-control-plane-sp-b-capability-uplink.md) / [SP-C 命令通道](../../design/2026-06-08-control-plane-sp-c-command-channel.md) 推进。本 part 遵循 [main.md §3](../main.md) 通用约定(`ts_ms` 毫秒 / `snake_case` / entity 信封 §3.2 / 写顺序 §3.4)。

## Purpose

定义 Go 管理面和 Rust 网关之间的配置下行、能力上行、运行状态和命令 ACK 契约，使控制面字段与 Rust runtime 能力保持一致。

## As-built surface

- Runtime instance identity and heartbeat anchors live in `crates/gw-ledger/src/instance_id.rs`.
- Control-plane DTOs and Redis key helpers live in `crates/gw-core/src/control_plane.rs`.
- Rust Wave 1 starts `crates/gw-gateway/src/control_plane::{state_writer,command_consumer}.rs` when `[control_plane].enabled = true`.
- Capability snapshots truthfully advertise `kiro:eventstream` as production ready and `kiro:cli` as not production ready with `reason=requires_real_cli_sample`.
- Executable Rust command hooks are limited to `reload-config`, `cooldown-key`, `clear-cooldown`, and supported `clear-cache` namespaces. `set-log-level`, `refresh-oauth`, drain/restart, and KEK rotation remain visible disabled capabilities and return failed ACKs if sent.

## Not built

- SP-A full `config_*` PgSQL downlink and `gateway:instance:<id>:effective` applied-version feedback are not complete in Rust Wave 1.
- Go-side command batch/delivery persistence, fan-out, reconciliation, and frontend disablement are owned by Go waves.
- Kiro CLI remains capability-advertised only; detector and usage extraction require a real CLI traffic sample.

## 8.1 概览(三线接触点)

| 线 | 接触点 | Writer | Reader | 说明 |
|---|---|---|---|---|
| **SP-A** | PgSQL `config_<table>` | **Go** | Rust(冷加载) | 配置权威源,每表一行 `id='global'` |
| SP-A | `entity:config:<table>` | Go(write-through 可选) | Rust(冷加载写回) | L1 cache,信封 §3.2 |
| SP-A | `events:entity` ns=`config` | Go | Rust StreamConsumer | 配置热重载信号 |
| **SP-B** | `gateway:instance:<id>:capability` | **Rust**(boot) | Go | 静态能力清单 |
| SP-B | `gateway:instance:<id>:effective` | Rust(reload) | Go | 配置生效值回报(闭环 SP-A) |
| SP-B | `gateway:instance:<id>:runtime` | Rust(周期 2s) | Go | 运行状态慢快照(覆盖式,不积压) |
| SP-B | `gateway:instance:<id>:heartbeat` | Rust(**已存在**) | Go reaper | 实例存活,`gw_ledger::instance_id` |
| **SP-C** | `commands:gateway` | **Go** | Rust(每实例独立 group) | 命令索引/通知流(广播 + 定向,主体在 command delivery entity) |
| SP-C | `entity:gateway_command_delivery:<delivery_id>` | **Go** | Rust | Go-owned per-instance delivery 主体,Rust 消费后删除 |
| SP-C | `gateway:instance:<id>:cmd_ack` | Rust | Go | 命令 ACK |

> `<id>` = `gw_ledger::instance_id::resolve` 的结果(L0 `GW_INSTANCE_ID` env / L2 `gateway:instance:<hostname>`)。Go 经现有心跳 reaper 发现存活实例集合,再读其 `:capability`/`:effective`/`:runtime`/`:cmd_ack`。

## 8.2 SP-A 配置下行(Go → Rust)

### 8.2.1 PgSQL `config_<table>`(权威源)

每个配置表(`config_routing` / `config_ratelimit` / `config_auth` / `config_billing` / `config_protection` / …)一行:

```
config_<table>
  id          TEXT  PRIMARY KEY  -- 固定 'global'(预留多 profile)
  payload     JSONB              -- 对应 Rust TableCfg 子结构
  version     BIGINT             -- 单调递增
  updated_at  TIMESTAMPTZ
```

`payload` shape = Rust `Config` 的对应子结构(`RoutingCfg`/`RateLimitCfg`/…),字段 `snake_case`,Rust 端 `#[serde(default)]` 容忍缺字段回落 `default.toml`。

### 8.2.2 `events:entity` ns=`config`(热重载信号)

```
XADD events:entity * ns config id <table> v <new_version> op upsert
```

Rust `StreamConsumer` 收到 → DEL `entity:config:<table>` L1 → `ConfigReloadRegistry` 按表分发到 A(ArcSwap)/B(rebuild)/C(graceful upgrade)路径(详见 SP-A design §3/§4)。

### 8.2.3 写顺序(遵循 main §3.4)

Go 写配置:① `UPDATE config_<table> SET payload=…, version=version+1`;②(可选)`HSET entity:config:<table>`;③ `XADD events:entity ns=config`。三步失败由 `version` 单调 + Rust 冷加载兜底。

### 8.2.4 env 边界(SP-A D5 / D1)

- **业务字段**:`redis > default.toml`,**env 不再覆盖业务字段**(Rust `config.rs::load()` 对业务表移除 `Env::prefixed` merge)。
- **bootstrap 字段**(env 唯一来源,鸡生蛋):`GW_REDIS__URL`、`GW_KMS__KEK_HEX`、`GW_PGSQL__URL`、`GW_SERVER__*_PORT`。
- **C 类字段**(socket/pool/KEK)变更触发 Rust **Pingora graceful upgrade**(零停机进程替换),非 in-place 热重载。

## 8.3 SP-B 能力/状态上行(Rust → Go)

全部挂在 `gateway:instance:<id>` 前缀下(复用现有实例注册体系,不另起 namespace)。

### 8.3.1 `:capability`(boot 写一次)

```
SET gateway:instance:<id>:capability <json>   [无 TTL 或随心跳续]
```
```json
{
  "instance_id": "…", "gateway_version": "1.5.0+<git_sha>", "started_at_ms": 0,
  "protocols": ["openai","anthropic","gemini","kiro"],
  "api_shapes": ["openai:chat","openai:responses","anthropic:messages","gemini:generate","kiro:eventstream","kiro:cli"],
  "cli_variants": ["claude:cli","openai:cli","openai:compact","gemini:cli","kiro:cli"],
  "routing_strategies": ["FixedOrder","RoundRobin","LoadBalance","WeightedRr",
                         "CostFirst","LatencyFirst","CacheAffinity","Sticky"],
  "features": {"tunnel": true, "egress_impersonate": true, "credit_lease": true,
               "detailed_log": true, "task_worker": false},
  "impersonate_identities": ["claude_code_chrome131","openai_codex","gemini_cli"]
}
```
Since 2026-06-15, `features.tunnel` is always emitted as `true`. The field is
retained in the contract for forward compatibility with Go consumers that may
inspect it; new consumers should treat it as a constant.

版本/feature 变(=新部署)才重写。

### 8.3.2 `:effective`(reload 事件写,闭环 SP-A)

```
HSET gateway:instance:<id>:effective <table> <applied_version>
```
`table → applied_version`。Go 下发 SP-A 配置后,读此 hash 确认**该实例**是否应用到目标 version;reload 被拒(validate 失败)则不更新该表 → version 滞后即未生效信号。

### 8.3.3 `:runtime`(周期覆盖写,**不积压**)

```
SET gateway:instance:<id>:runtime <json>   EX <2×push_interval>
```
```json
{
  "ts_ms": 0,
  "key_health": [{"key_id":"vk_…","format":"openai","circuit":"closed",
                  "in_cooldown":false,"consecutive_failures":0}],
  "tunnel": [{"node_id":"pn_…","transport_present":true,"last_pong_age_s":2}],
  "oauth_rotation": [{"key_id":"vk_…","last_refresh_ms":0,"access_expires_at_ms":0}],
  "impersonate_active": {"openai_codex": 3},
  "inflight_total": 42
}
```
**覆盖式 `SET`(非 stream)→ Go 永读最新一帧,结构上不积压。** 慢变状态(key-health/tunnel/oauth 轮换/活跃统计);逐请求实时明细不入此 key(留 Prometheus)。`oauth_rotation` 由 F2 K-C 填充(未落地前为空数组)。

### 8.3.4 频率(Go 控频)+ TTL(死实例自清理)

- 推送间隔 = 配置项 `config_control_plane.runtime_push_interval_ms`(经 **SP-A** 下发,默认 `2000`,**下限 `1000`**,Rust validate 拒绝 < 1000)。
- Rust Wave 1 配置字段为 `[control_plane] runtime_push_interval_ms / snapshot_ttl_ms / command_group / command_consumer_name / command_block_ms`。`snapshot_ttl_ms >= 2×runtime_push_interval_ms`,`command_block_ms` 范围为 `1000..=30000`。
- `:capability`/`:runtime` 使用覆盖式 `SET ... PX snapshot_ttl_ms`,不产生积压。`:effective` 随 SP-A reload hook 后续落地。实例宕机 → `gateway:instance:<id>:*` 在 TTL 内过期,Go 不读 stale。

### 8.3.5 实例发现 + 语义

Go 用现有心跳 reaper(`gateway:instance:<id>:heartbeat`,TTL 600s)已知存活集合,逐实例读 `:capability`/`:effective`/`:runtime`。**注意**:`key_health` 是 **per-instance** 本地视图(各实例独立 circuit),Go 聚合时须知非全局。

## 8.4 SP-C 命令通道(Go → Rust + ACK)

### 8.4.1 `commands:gateway` 索引/通知结构

```
XADD commands:gateway * command_id <id> delivery_id <delivery_id> target_instance_id <instance_id> delivery_mode <durable|ephemeral> issued_at_ms <ts>
```
- `commands:gateway` 只做命令索引/通知,**不承载** `action`/`args`/完整命令主体。
- **寻址**:每实例 boot 时 `XGROUP CREATE commands:gateway group:<instance_id> $`(每实例独立 group → 广播语义)。Go 侧 `target_scope=all` 不是单个可消费命令,而是 command batch selector;Go 按下发瞬间 ready/capability 快照 fan-out 出多个 delivery,每条 stream entry 只指向一个 `target_instance_id`。
- Rust 收到 stream entry 后,如果 `target_instance_id != 自己 instance_id`,只 `XACK`,不得写 ACK、不得删除 entity。只有目标实例用 `delivery_id` 读取 `entity:gateway_command_delivery:<delivery_id>`;缺失时按 Go 审计事实可能是 cancel/expire,目标 Rust 只写 `skipped` ACK 并 `XACK`。

### 8.4.2 命令 batch 与 delivery entity

Go 先创建 `command_batch`(管理员意图),再按目标实例创建 `command_delivery`。只有 delivery 是 Rust 可消费 entity。

`command_batch` 字段:

| 字段 | 必填 | 说明 |
|---|---|---|
| `command_id` | yes | 管理员动作批次 id |
| `action` | yes | 命令动作 |
| `target_scope` | yes | `all` 或显式实例集合 |
| `delivery_mode` | yes | `durable` / `ephemeral` |
| `args` | yes | 命令参数 |
| `issued_at_ms` | yes | Go 下发时间 |
| `issued_by` | yes | 管理员/系统来源 |

Go 写每个 delivery 主体,再写 `commands:gateway` 索引/通知:

```
HSET entity:gateway_command_delivery:<delivery_id> v <version> enc json d <delivery_json>
```

Redis HASH 字段遵循 main §3.2 entity 信封:

| 字段 | 必填 | 说明 |
|---|---|---|
| `v` | yes | 单调版本号 |
| `enc` | yes | `json` |
| `d` | yes | command JSON |

`d` 是 JSON:

```json
{
  "command_id": "cmd_...",
  "delivery_id": "cmd_...:gw-a",
  "action": "reload-config",
  "target_scope": "all",
  "target_instance_id": "gw-a",
  "args": {"table": "config_control_plane"},
  "delivery_mode": "durable",
  "issued_at_ms": 0,
  "expires_at_ms": null,
  "issued_by": "admin_..."
}
```

必填字段:`command_id` / `delivery_id` / `action` / `target_scope` / `target_instance_id` / `args` / `delivery_mode` / `issued_at_ms` / `issued_by`。`expires_at_ms` 只在 `delivery_mode=ephemeral` 时必填。

Delivery 语义:

| delivery_mode | Go 写 entity | Rust 消费 | Go 判定 |
|---|---|---|---|
| `durable` | 不设置 Redis TTL | 执行或跳过后删除 entity | 目标 ACK `ok` 且 delivery entity 消失才算成功 |
| `ephemeral` | 必须设置 `expires_at_ms` 和 Redis TTL | 若发现过期,写 ACK `status=skipped` / `message=expired`,删除 entity | TTL 到期不算成功;Go 标记 expired/skipped |

Go 不自动删除 durable delivery entity;durable 超时未消费由 Go 标记 `pending_timeout` 并告警。管理员取消时,Go 直接删除尚未被 Rust 消费的 delivery entity,并把 delivery 审计状态标记为 `cancelled`;已经不存在的 entity 表示 Rust 已消费或 TTL 已过,不能回滚。Rust 只有成功读取目标 delivery 并执行/跳过后才删除 `entity:gateway_command_delivery:<delivery_id>`。

### 8.4.3 命令集 + args + 内部映射

| action | args | 默认 target | 首批状态 | 内部 API(Rust) |
|---|---|---|---|---|
| `reload-config` | `table`(或 `"*"`) | all | executable | SP-A `ConfigReloadRegistry` |
| `cooldown-key` | `key_id, format, ttl_secs` | all | executable | `KeyHealthStore::apply_cooldown` |
| `clear-cooldown` | `key_id, format` | all | executable | `KeyHealthStore::clear_cooldown` |
| `clear-cache` | `ns, id` | all | executable | `EntityStore` invalidate |
| `set-log-level` | `level` | all/instance | disabled + reason | tracing reload handle 未接入 |
| `refresh-oauth` | `key_id` | all(leader 单飞) | disabled + reason | F2 K-C Rust `OAuthRuntime` 尚未暴露 force-refresh hook |
| `drain` / `undrain` | — | instance | disabled + reason | graceful-upgrade `GracefulDrain` 尚未接入命令面 |
| `restart-instance` | — | instance | disabled + reason | Pingora graceful upgrade 尚未接入命令面 |
| `rotate-kek` | `new_kid` | all | disabled + reason | `KekCache` 轮换尚未接入命令面 |

禁用命令可以在 capability 中可见,但必须带 `enabled=false` 和 `reason`;Go/前端不得把 disabled 命令作为可执行动作开放。

### 8.4.4 ACK(`gateway:instance:<id>:cmd_ack`)

```
HSET gateway:instance:<id>:cmd_ack <delivery_id> <result_json>   + EXPIRE
result = { "command_id": "cmd_...", "delivery_id": "cmd_...:gw-a", "status": "ok|failed|skipped|not_applicable", "message": "…", "finished_at_ms": 0 }
```
Rust 处理顺序:

1. 从 `commands:gateway` 取得 `command_id` / `delivery_id` / `target_instance_id` / `delivery_mode` / `issued_at_ms`。
2. 如果 `target_instance_id != 自己 instance_id`,只 `XACK` 当前 stream entry,不写 ACK、不读/删 entity。
3. 读取 `entity:gateway_command_delivery:<delivery_id>` 并解码 `d`。
4. 校验 stream 元数据与 delivery JSON 一致。
5. 执行、跳过或判定 disabled/expired。
6. 写 `gateway:instance:<id>:cmd_ack` hash field `<delivery_id>`。
7. 删除 `entity:gateway_command_delivery:<delivery_id>`。
8. `XACK commands:gateway group:<instance_id> <stream_id>`。

Rust Wave 1 的 malformed entity 处理遵循同一顺序:能解析 stream delivery metadata 时写 `failed` ACK,删除该 delivery entity,再 `XACK`;若 stream entry 自身缺少 `delivery_id` 等索引字段,无法可靠写 delivery ACK,仅记录 warn 并 `XACK`。

命令结果聚合:Go 以 `command_delivery` 为最小确认单位,读取目标实例 `<delivery_id>` ACK,并检查 `entity:gateway_command_delivery:<delivery_id>` 是否不存在。Go 只有同时看到目标实例 ACK `status=ok` 且 delivery entity 不存在,才算该 delivery 成功。只看到 ACK 但 entity 仍存在,或 entity 已删除但 ACK 缺失,均为 `inconsistent`。一个 batch 只有所有目标 delivery 都成功时才显示成功;部分 failed/skipped/expired/cancelled 必须显示 partial/final,不能夸大为成功。

### 8.4.5 权限

沿用现有 redis 可信边界(与 `events:entity` 同等信任,Go 已能改 entity);全命令 `command_id` 审计,敏感命令(`drain`/`rotate-kek`/`restart-*`)记 audit log。redis 多租户/不可信时需新增已确认的签名字段设计;本 wave 不引入未确认的 HMAC 字段。

## 8.5 失败语义(扩展 main §3.5)

| 场景 | Rust 行为 | Go 行为 |
|---|---|---|
| config validate 失败 | 拒绝该次 reload,保留旧 `TableCfg`,`:effective` version 不前进 | 读 `:effective` 见 version 滞后 → 知未生效,告警 |
| `events:entity ns=config` 丢失 | `version` 单调 + 启动冷加载兜底(同 §3.4) | 重发或等冷加载 |
| `:runtime` 推送失败 | 下个 tick 覆盖重试(无积压) | 读到 stale(TTL 内)或 key 缺失(TTL 过) |
| 命令 handler 失败 | 以 `delivery_id` 写 ACK `status=failed` + message,删除 delivery entity,再 XACK | 按 `delivery_id` 读 ACK,聚合到 batch |
| 命令 entity 已过期 | 以 `delivery_id` 写 ACK `status=skipped` + message=`expired`,删除 entity,再 XACK | 记录 skipped/expired,不计成功 |
| 管理员取消命令 | 目标 Rust 若稍后读到 stream 但 entity 已缺失,只 ACK stream 或写 skipped ACK | Go 删除未消费 delivery entity 并标记 cancelled;不等待 Rust 反向改写 |
| ACK/entity 不一致 | 保持 ACK/entity 事实状态,不伪造成功 | 标记 `inconsistent` 并告警/人工处理 |
| `refresh-oauth` 广播 | 必须复用 K-C `SETNX lock:oauth:refresh:<key_id>` 全局单飞 | 期望单次刷新 |
| 实例宕机 | 全部 `gateway:instance:<id>:*` TTL 过期 | 心跳 reaper 摘除,不读 stale |

## 8.6 依赖与上线 ordering

- **新增 PgSQL 表**:`config_<table>` 一组 + 一个 `config_control_plane`(承载 `runtime_push_interval_ms` 等控制面自身配置)。`psql -f` 手动跑(同 main §4)。
- **依赖关系**:SP-A 是基础;SP-B `:effective` 写入点挂 SP-A reload hook;SP-C `reload-config`/`restart`/`drain` 复用 SP-A 机制;SP-C `refresh-oauth` 依赖 [F2 K-C](../../ROADMAP.md)。
- **上线 ordering**:Rust 先上(读 `default.toml` 兜底可单跑)→ Go 写 `config_*` + 发 `events:entity ns=config` 接管控制。破坏性变更直接做(main §3.6,项目未上线)。

## 8.7 Wire compatibility addendum (2026-06-11)

Rust `gateway:instance:<id>:capability` emits both legacy and detailed API-shape fields:

- `api_shapes`: plain string array for older Go readers that only need shape names.
- `api_shape_capabilities`: detailed per-shape objects with `api_shape`, `readiness`, `production_ready`, and optional `reason`.
- `kiro_cli`: top-level Kiro CLI readiness object.
- `readiness.kiro_cli`: the same Kiro CLI readiness object nested under the general readiness surface.

Kiro CLI is advertised but not production ready until a real CLI traffic sample exists. Go must not infer Kiro CLI production readiness from membership in `api_shapes`; use `kiro_cli.production_enabled` and `kiro_cli.usage_ready`.

Rust `gateway:instance:<id>:runtime` emits `readiness.runtime_snapshot=true` from the state writer and `readiness.command_consumer=true` once the command consumer is started. Runtime snapshots also include `readiness.kiro_cli` so Go can render runtime readiness without treating the gateway capability as missing.

> Sync rule:本 part 任何接触点改动须同步 [main.md](../main.md) §2/§3.3/§7;Rust 侧实现细节见三个 [SP design doc](../../design/)。
