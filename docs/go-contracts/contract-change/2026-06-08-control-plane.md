# 2026-06-08 — Control Plane(Rust↔Go 透明控制平面)

> 跨进程契约变更索引。状态:**design 阶段目标契约**(Rust 实现随 SP plan 落地,Go 可并行)。

## 摘要

新增三线控制平面契约,实现「Go 完全控制 Rust + Rust 对 Go 完全透明」:

- **SP-A 配置下行(Go→Rust)**:配置=第 14 种 entity。Go 写 `config_<table>`(PgSQL)+ 发 `events:entity ns=config` → Rust 热重载(ArcSwap / rebuild / graceful upgrade 三路径)。env 退出业务字段,只剩 bootstrap。
- **SP-B 能力上行(Rust→Go)**:Rust 写 `gateway:instance:<id>:{capability,effective,runtime}`(复用现有实例心跳体系)。`:runtime` 覆盖式慢快照、Go 控频(默认 2s/下限 1s)、不积压。
- **SP-C 命令通道(Go→Rust)**:Go 写 `commands:gateway`(每实例独立 group=广播 + `target` 定向),Rust 执行后写 `gateway:instance:<id>:cmd_ack`。10 个瞬时动作命令 + 两级「部分重启」。

## 契约落点

| 落点 | 内容 |
|---|---|
| [parts/08-control-plane.md](../parts/08-control-plane.md) | 契约本体(三线全部接触点) |
| [main.md](../main.md) §2 / §3.3 / §7 | part 索引 / `commands:gateway` stream / sync rule |

## Rust 侧设计 spec

- [SP-A 配置下行](../../design/2026-06-08-control-plane-sp-a-config-downlink.md)
- [SP-B 能力上行](../../design/2026-06-08-control-plane-sp-b-capability-uplink.md)
- [SP-C 命令通道](../../design/2026-06-08-control-plane-sp-c-command-channel.md)

## Go 模块责任

| Go 模块 | 动作 |
|---|---|
| 配置中心 | 写 `config_<table>` + `version+1` + 发 `events:entity ns=config`;读 `:effective` 验证生效 |
| 实例监控 | 经心跳 reaper 发现实例 → 读 `:capability`/`:runtime` |
| 命令下发 | 写 `commands:gateway` + `command_id` → 聚合各实例 `:cmd_ack` |

## 新增 PgSQL 表

`config_<table>` 一组(routing/ratelimit/auth/billing/protection/…)+ `config_control_plane`(承载 `runtime_push_interval_ms` 等控制面自身配置)。`psql -f` 手动跑。

## 依赖

- SP-A 是 SP-B/SP-C 的基础(reload/restart/drain 机制、`:effective` 写入点、推送频率配置)。
- SP-C `refresh-oauth` 依赖 [F2 K-C](../../ROADMAP.md)(in-Rust OAuth 刷新栈)。
- 大改动,建议先有 W0 CI 门禁(见 [ROADMAP 主 plan](../../plans/2026-06-08-roadmap-implementation-program.md))。
