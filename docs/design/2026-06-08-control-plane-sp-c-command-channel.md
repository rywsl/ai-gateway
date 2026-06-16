# Control Plane SP-C — Command Channel (Go→Rust) — Design

> **Brainstorm 产出 (2026-06-08).** Rust↔Go 透明控制平面第三条线:SP-C = 命令通道(Go 主动下**瞬时动作**指令)。依赖 **SP-A**(`reload-config` + `restart-component` 复用 `Reconfigurable`/graceful-upgrade)、**SP-B**(ACK 复用 `gateway:instance:<id>` 前缀 + 实例发现)。配置类持久变更走 SP-A;SP-C 只管不持久化的动作。未建成,放 `docs/design/`。

## 现状盘点(命令多映射到已有内部 API)

| 命令 | 现状内部 API | SP-C 动作 |
|---|---|---|
| `cooldown-key` / `clear-cooldown` | ✅ `KeyHealthStore::apply_cooldown`/`clear_cooldown`(`store.rs:133/142`) | 路由到既有 API |
| `refresh-oauth` | ❌ F2 K-C(未建) | 触发 K-C 刷新栈 |
| `reload-config` | ← SP-A reload 机制 | 调 SP-A `ConfigReloadRegistry` |
| `drain` / `undrain` | ❌ `run_forever()` 无 drain(`lib.rs:345`) | 复用 SP-A 引入的 graceful-upgrade drain |
| `restart-component` | ← SP-A `Reconfigurable` | 用当前配置强制重建子系统 |
| `rotate-kek` | ✅ `encrypt.rs` DualRead + 命令驱动 finalize | `status`/`mark-reencrypting`/`drop-old`（不引入 KEK 材料；见 go-contracts parts/08 §8.4.3） |
| `set-log-level` | ⚠️ 需 wire `tracing` reload handle | 运行时调日志级别 |
| `clear-cache` | ✅ `events:entity` invalidate 机制已有 | 复用失效逻辑 |
| (先例)`task.cancel` | ✅ `task.cancel_stream`(`config.rs:757`) | Go→Rust 命令流先例,验证模式可行 |

**载体复用**:`StreamConsumer`(现有 `events:entity`/`events:counter` 两个 consumer group,`stream_consumer.rs`)加**第三个** consumer group 消费命令流。

## 命令集(2026-06-08 确认)

cooldown-key · clear-cooldown · refresh-oauth · **reload-config**(必含,SP-A 闭环)· drain · undrain · **restart-component** · rotate-kek · set-log-level · clear-cache。

> **"部分重启"= 两级**(用户补充):
> - **软** `restart-component <name>`:用**当前**配置强制重建某 B 类子系统(flusher/sweep/archive/tunnel-stack/egress),复用 SP-A `Reconfigurable`——即使配置没变也能强制 drain+rebuild。
> - **硬** `restart-instance`:整进程零停机替换,复用 SP-A 的 Pingora graceful upgrade(C 类 socket/pool/KEK 的"重启"上限)。

## 寻址模型(广播 + 定向,两者都要)

```
commands:gateway  (单 stream)
   每个实例用独立 consumer group  group:<instance_id>  →  各自收到全量命令(广播语义)
   命令带 target 字段:
       target = "all"          → 所有实例执行(reload-config / cooldown-key / clear-cache 全局)
       target = "<instance_id>" → 仅该实例执行,其余实例读到后跳过(drain 某 Pod / restart-instance)
```

> redis stream 同一 consumer group 内消息被单 consumer 抢占;**广播**靠"每实例一个独立 group"实现(各 group 独立游标,都收全量)。`target` 字段在消费侧过滤,决定本实例是否执行。

## SP-C 锁定决策(2026-06-08 brainstorm)

| # | 决策点 | 结论 |
|---|--------|------|
| D1 | **载体** | `commands:gateway` stream;每实例独立 consumer group(广播)+ `target` 字段(定向) |
| D2 | **命令集** | 见上(10 个动作);配置类持久变更不进 SP-C,走 SP-A |
| D3 | **"部分重启"** | 软 `restart-component`(`Reconfigurable` 重建子系统)+ 硬 `restart-instance`(graceful upgrade) |
| D4 | **ACK** | `gateway:instance:<id>:cmd_ack`(`HSET <command_id> <result_json>`),Go 用 command_id 关联 |
| D5 | **幂等** | `command_id` 去重:`SETNX seen:cmd:<id>`(TTL),已见即跳过(防 stream 重投) |
| D6 | **权限** | 沿用现有 redis 可信边界(与 `events:entity` 同等信任,Go 已能改 entity);全命令带 `command_id` 审计,敏感命令(drain/rotate-kek/restart)记 audit log,**可选** HMAC 签名 |

## 组件设计

### 1. 命令结构(`XADD commands:gateway`)
```json
{
  "command_id": "uuid-or-snowflake",
  "action": "cooldown-key",
  "target": "all",                       // "all" | "<instance_id>"
  "args": { "key_id": "…", "format": "openai", "ttl_secs": 300 },
  "issued_at_ms": 0,
  "sig": "…"                             // 可选 HMAC(敏感命令)
}
```

### 2. 消费 + 分发(`StreamConsumer` 加第三 group)
```
boot:  XGROUP CREATE commands:gateway group:<instance_id> $   (每实例独立 group → 广播)
tick:  XREADGROUP group:<instance_id> COUNT n
  apply_command(fields):
    1. 解析 {command_id, action, target, args}
    2. target 过滤:  target≠"all" 且 ≠本 instance_id  → XACK 跳过(不执行)
    3. 幂等:         SETNX seen:cmd:<command_id> (TTL)  → 已存在 → XACK 跳过
    4. (可选)验签:  敏感命令校验 HMAC,失败 → ACK failed
    5. CommandRegistry.dispatch(action, args) → 内部 API
    6. HSET gateway:instance:<id>:cmd_ack <command_id> <result_json>
    7. XACK
```
`CommandRegistry: HashMap<&'static str, CommandHandler>`,boot 注册;是 SP-C 中枢(对称于 SP-A 的 `ConfigReloadRegistry`)。

### 3. 命令 → 内部 API 路由表
| action | 内部 API | 默认 target | args |
|---|---|---|---|
| `cooldown-key` | `KeyHealthStore::apply_cooldown`(已有) | all | key_id, format, ttl_secs |
| `clear-cooldown` | `KeyHealthStore::clear_cooldown`(已有) | all | key_id, format |
| `refresh-oauth` | K-C `OAuthRefresher`(依赖 F2) | all(leader 单飞) | key_id |
| `reload-config` | SP-A `ConfigReloadRegistry::dispatch` | all | table（或 "*"） |
| `restart-component` | SP-A `Reconfigurable::reconfigure`(当前配置) | all/instance | component |
| `drain` / `undrain` | `GracefulDrain`(SP-A graceful-upgrade 引入) | instance | — |
| `restart-instance` | graceful upgrade 触发 | instance | — |
| `rotate-kek` | `KekCache` 双读 finalize（`command_consumer::rotate_kek`） | all | `step`（`status`\|`mark-reencrypting`\|`drop-old`）, `old_kid` |
| `set-log-level` | `tracing` reload handle(需 wire) | all/instance | level |
| `clear-cache` | `EntityStore` invalidate(已有) | all | ns, id |

### 4. ACK(`gateway:instance:<id>:cmd_ack`)
```
HSET gateway:instance:<id>:cmd_ack <command_id> {
  "status": "ok|failed|skipped|not_applicable",
  "detail": "…", "applied_at_ms": 0
}   + EXPIRE(避免无限增长)
```
Go 发命令后轮询/读此 hash 按 `command_id` 取结果。`skipped`=target 不匹配或幂等命中;`not_applicable`=该实例无此资源(如 cooldown 一个本实例没见过的 key)。

## go-contract(`parts/08-control-plane.md` 的 SP-C 部分)

| 方向 | 载体 | 写方 | 说明 |
|---|---|---|---|
| Go→Rust | `XADD commands:gateway {command_id, action, target, args, sig?}` | Go | 命令集 + 每命令 args schema 是契约 |
| Rust→Go | `HSET gateway:instance:<id>:cmd_ack <command_id> <result>` | Rust | Go 按 command_id 收集存活实例的 ACK |

广播命令的结果聚合:Go 用 SP-B 的实例发现(心跳 reaper 已知存活集合)→ 读每个 `gateway:instance:<id>:cmd_ack` 的同一 `command_id` → 聚合判定全局成功/部分失败。

## 实现影响(crate / 文件)

- **`gw-store`**:`stream_consumer.rs` 加 `commands:gateway` consumer group + `apply_command` + `CommandRegistry`;`SETNX seen:cmd:*` 幂等。
- **`gw-gateway`**:`GracefulDrain`(drain/undrain/restart-instance,与 SP-A graceful-upgrade 同源);`tracing` reload handle(set-log-level);各 handler wire 到内部 API。
- **依赖**:SP-A(reload/restart/drain 机制)、SP-B(ACK 前缀 + 实例发现)、F2 K-C(refresh-oauth)。

## 风险与开放问题

1. **(开放)广播命令部分失败聚合** — N 实例部分成功部分失败,Go 须按 `command_id` 读全部存活实例的 `cmd_ack` 才能判全局结果。需在 go-contract 明确聚合协议 + 超时(某实例无 ACK = 超时未执行)。
2. **(开放)`refresh-oauth` 广播单飞** — broadcast 给 N 实例会 N 个同时刷同一 key;必须复用 F2 K-C 的 `SETNX lock:oauth:refresh:<key_id>` 全局单飞,否则打爆 token endpoint。
3. **(开放)命令执行时序** — 各实例独立 group,广播命令在实例间执行时序不保证(A 先 reload、B 后);最终一致,中间态短暂不一致。强一致需求场景(如全局 drain 顺序)由 Go 编排(逐实例定向命令)。
4. **(安全)敏感命令签名** — 可信 redis 边界默认不签(与 `events:entity` 一致);redis 多租户/不可信时启用 `sig` HMAC。标可选。
5. **(开放)drain/restart × k8s** — 同 SP-A 风险3(graceful upgrade 与 k8s 滚动更新语义),plan 展开前定部署形态。

## 与 SP-A / SP-B / K-C 接口

- **依赖 SP-A**:`reload-config`/`restart-component`/`drain`/`restart-instance` 全复用 `Reconfigurable` + graceful-upgrade,SP-C 不重造重建逻辑。
- **依赖 SP-B**:ACK 用 `gateway:instance:<id>:cmd_ack`(同前缀);实例发现共享心跳 reaper。
- **依赖 F2 K-C**:`refresh-oauth`(K-C 未落地前该命令返回 `not_applicable`)。
- **闭环**:SP-C 动作面 + SP-A 配置面 = **Go 对 Rust 的完整控制**;SP-B 状态面 = **Rust 对 Go 的完整透明**。三线合一即"透明控制平面"。

## Self-review(brainstorming spec gate)

- **Placeholder**:无 TBD;不确定项列「开放问题」并标缓解。
- **内部一致**:命令集 ↔ 路由表 ↔ ACK status ↔ 依赖接口一一对应。
- **Scope**:聚焦 SP-C 动作通道;SP-A/SP-B/K-C 仅作接口引用,机制不重述。
- **歧义**:广播部分失败聚合、命令时序、签名显式标开放,不留隐性假设。


