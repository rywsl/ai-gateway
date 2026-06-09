# Part 08 — Control Plane(Rust↔Go 透明控制平面)

> 本 part 描述 **Rust 网关与 Go 管理面之间的控制平面契约**:Go 完全控制 Rust(配置下发 + 命令),Rust 对 Go 完全透明(能力/状态上行)。索引见 [main.md](../main.md)。
>
> **状态:目标契约(design 阶段,尚未实现)** —— 对应设计 doc [SP-A 配置下行](../../design/2026-06-08-control-plane-sp-a-config-downlink.md) / [SP-B 能力上行](../../design/2026-06-08-control-plane-sp-b-capability-uplink.md) / [SP-C 命令通道](../../design/2026-06-08-control-plane-sp-c-command-channel.md)。Go 团队可据此**并行实现**;Rust 侧随各 SP 的实现 plan 落地。本 part 遵循 [main.md §3](../main.md) 通用约定(`ts_ms` 毫秒 / `snake_case` / entity 信封 §3.2 / 写顺序 §3.4)。

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
| **SP-C** | `commands:gateway` | **Go** | Rust(每实例独立 group) | 命令流(广播 + 定向) |
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
  "protocols": ["openai","anthropic","gemini"],
  "cli_variants": ["claude:cli","openai:cli","openai:compact","gemini:cli"],
  "routing_strategies": ["FixedOrder","RoundRobin","LoadBalance","WeightedRr",
                         "CostFirst","LatencyFirst","CacheAffinity","Sticky"],
  "features": {"tunnel": false, "egress_impersonate": true, "credit_lease": true,
               "detailed_log": true, "task_worker": false},
  "impersonate_identities": ["claude_code_chrome131","openai_codex","gemini_cli"]
}
```
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
- `:runtime` TTL = `2×interval`;`:capability`/`:effective` 随心跳续期。实例宕机 → 全部 `gateway:instance:<id>:*` 在 TTL 内过期,Go 不读 stale。

### 8.3.5 实例发现 + 语义

Go 用现有心跳 reaper(`gateway:instance:<id>:heartbeat`,TTL 600s)已知存活集合,逐实例读 `:capability`/`:effective`/`:runtime`。**注意**:`key_health` 是 **per-instance** 本地视图(各实例独立 circuit),Go 聚合时须知非全局。

## 8.4 SP-C 命令通道(Go → Rust + ACK)

### 8.4.1 `commands:gateway` 命令结构

```
XADD commands:gateway * command_id <id> action <action> target <t> args <json> issued_at_ms <ts> [sig <hmac>]
```
- **寻址**:每实例 boot 时 `XGROUP CREATE commands:gateway group:<instance_id> $`(每实例独立 group → 广播语义);`target` = `"all"` | `"<instance_id>"`,消费侧过滤决定是否执行。
- **幂等**:`command_id` 去重 `SETNX seen:cmd:<command_id>`(TTL),已见即 `XACK` 跳过。

### 8.4.2 命令集 + args + 内部映射

| action | args | 默认 target | 内部 API(Rust) |
|---|---|---|---|
| `cooldown-key` | `key_id, format, ttl_secs` | all | `KeyHealthStore::apply_cooldown` |
| `clear-cooldown` | `key_id, format` | all | `KeyHealthStore::clear_cooldown` |
| `refresh-oauth` | `key_id` | all(leader 单飞) | F2 K-C Rust `OAuthRuntime` single-flight refresher; Go issues the command, Rust performs refresh and write-back |
| `reload-config` | `table`(或 `"*"`) | all | SP-A `ConfigReloadRegistry` |
| `restart-component` | `component` | all/instance | SP-A `Reconfigurable`(强制重建) |
| `drain` / `undrain` | — | instance | graceful-upgrade `GracefulDrain` |
| `restart-instance` | — | instance | Pingora graceful upgrade |
| `rotate-kek` | `new_kid` | all | `KekCache` 轮换(已有 DualRead) |
| `set-log-level` | `level` | all/instance | `tracing` reload handle |
| `clear-cache` | `ns, id` | all | `EntityStore` invalidate |

### 8.4.3 ACK(`gateway:instance:<id>:cmd_ack`)

```
HSET gateway:instance:<id>:cmd_ack <command_id> <result_json>   + EXPIRE
result = { "status": "ok|failed|skipped|not_applicable", "detail": "…", "applied_at_ms": 0 }
```
广播命令结果聚合:Go 用 §8.3.5 实例发现 → 读所有存活实例同一 `command_id` 的 ACK → 判全局成功/部分失败。某实例无 ACK(超时)= 未执行。

### 8.4.4 权限

沿用现有 redis 可信边界(与 `events:entity` 同等信任,Go 已能改 entity);全命令 `command_id` 审计,敏感命令(`drain`/`rotate-kek`/`restart-*`)记 audit log。redis 多租户/不可信时启用 `sig` HMAC(可选,默认关)。

## 8.5 失败语义(扩展 main §3.5)

| 场景 | Rust 行为 | Go 行为 |
|---|---|---|
| config validate 失败 | 拒绝该次 reload,保留旧 `TableCfg`,`:effective` version 不前进 | 读 `:effective` 见 version 滞后 → 知未生效,告警 |
| `events:entity ns=config` 丢失 | `version` 单调 + 启动冷加载兜底(同 §3.4) | 重发或等冷加载 |
| `:runtime` 推送失败 | 下个 tick 覆盖重试(无积压) | 读到 stale(TTL 内)或 key 缺失(TTL 过) |
| 命令 handler 失败 | ACK `status=failed` + detail | 按 `command_id` 读 ACK 处理 |
| `refresh-oauth` 广播 | 必须复用 K-C `SETNX lock:oauth:refresh:<key_id>` 全局单飞 | 期望单次刷新 |
| 实例宕机 | 全部 `gateway:instance:<id>:*` TTL 过期 | 心跳 reaper 摘除,不读 stale |

## 8.6 依赖与上线 ordering

- **新增 PgSQL 表**:`config_<table>` 一组 + 一个 `config_control_plane`(承载 `runtime_push_interval_ms` 等控制面自身配置)。`psql -f` 手动跑(同 main §4)。
- **依赖关系**:SP-A 是基础;SP-B `:effective` 写入点挂 SP-A reload hook;SP-C `reload-config`/`restart`/`drain` 复用 SP-A 机制;SP-C `refresh-oauth` 依赖 [F2 K-C](../../ROADMAP.md)。
- **上线 ordering**:Rust 先上(读 `default.toml` 兜底可单跑)→ Go 写 `config_*` + 发 `events:entity ns=config` 接管控制。破坏性变更直接做(main §3.6,项目未上线)。

> Sync rule:本 part 任何接触点改动须同步 [main.md](../main.md) §2/§3.3/§7;Rust 侧实现细节见三个 [SP design doc](../../design/)。

