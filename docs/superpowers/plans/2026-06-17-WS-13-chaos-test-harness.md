# WS-13 — Chaos Test Harness（TDD 子计划）

> **配套主计划**：
> - v2：`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0 重要 1+2, §5 WS-13, §7 验收门, REV-005
> - v1：`AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §7 #2/#3/#4（24h soak、chaos 注入测试、跨 protocol soak）
> **双仓 mirror**：`ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-13-chaos-test-harness.md`（字节一致除标题）
> **本子计划角色**：交付 chaos harness 与稳定 scenario 名集合，被 **所有其他 WS 的 DoD 引用**。WS-13 不修业务代码；它**测**业务代码。
> **执行者画像**：不假定熟悉 Rust 或 Go；每个 task 自包含。SQL/Lua/HTTP curl 直写。
> **格式**：每个 task 必含 8 段（v2 §0 标准 7 段 + 第 8 段「依赖与并行」）。
> **CRITICAL（v2 §0 重要 2）**：sonnet 输出是 **假设**。每个 task 在执行前先 `Read` 引用的源文件验证字面代码与本计划一致；不一致以源码为准并在 sub-plan log 记录修正。任何"候选工具"（Toxiproxy、pgmocker 等）的可行性必须先核查（见 §0.2 工具可行性表 + INVESTIGATE-CHAOS-* 列表）。

---

## §0. 阅读须知

### 0.1 本子计划交付物（必须全部存在才能闭合 WS-13）

| 路径 | 角色 |
|---|---|
| `tests/e2e/chaos/` （新目录，**双仓**：Rust 仓 + Go 仓各一份，scenario YAML 镜像）| 框架根 |
| `tests/e2e/chaos/scenarios/*.yaml` | 25 个 scenario 描述（详见 §3） |
| `tests/e2e/chaos/harness/` | Python 框架代码（沿用 Rust 仓 `tests/e2e/client/soak/` 风格） |
| `tests/e2e/chaos/injectors/` | 故障注入模块（pg、redis、ch、process、network 一类一个） |
| `tests/e2e/chaos/invariants/` | 不变式断言模块（钱、PEL、RSS、FD、log secrets 等） |
| `.chaos_report/<scenario_name>.json` | 每场景一份 PASS/FAIL JSON（schema 见 §4） |
| `Makefile` 新 target：`chaos-smoke`、`chaos-full`、`chaos-scenario`、`chaos-clean` | CI 与本地入口 |
| `make.ps1` 同步 target（CLAUDE.md "scripts/*.sh 同步 *.ps1"）| Windows 入口 |
| `docs/business/chaos/README.md` + `docs/business/chaos/scenarios.md` | 文档 |

### 0.2 工具可行性核查表（执行前**必须**逐项确认或派 sonnet）

本计划**默认不假设**任何外部工具可用；候选工具均要先验证。**简单优先**：能用 SQL/signal 触发就不引第三方代理。

| 候选工具 | 计划用途 | 简单替代 | 决策点（执行者负责调研） |
|---|---|---|---|
| **Toxiproxy** | TCP-level chaos（PG/Redis/CH 延迟、断连、bandwidth） | (a) `iptables -j DROP` (Linux 容器内 cap_net_admin)；(b) `pgbouncer` + `KILL` 连接；(c) 直接 `pg_terminate_backend()` | 首选 **(c) + Toxiproxy**：(c) 覆盖 PG 闪断；Toxiproxy 覆盖延迟/带宽/部分包丢。**INVESTIGATE-CHAOS-001** 先验证 Toxiproxy 容器在 compose 里 1 行启动 |
| **pgmocker** | 在指定 op 处主动断 PG | **不存在该工具**。改用 `pg_terminate_backend(pid)` + 自写 hook（监听 PG 日志或基于 `pg_stat_activity` 抓 query→PID 再 kill） | 删除 pgmocker 假设，统一走 §3.B PG-MID-OP scenario 的 SQL 注入路径 |
| **Redis Sentinel partition** | 模拟 sentinel 视角分区 + 自动 failover | (a) 启 3+1+1 Sentinel cluster 容器组；(b) `iptables` 阻断 master ↔ replicas 流量；(c) `DEBUG SLEEP 60` 阻塞 master | 选 (a) + (b)：sentinel cluster 与 iptables drop。**INVESTIGATE-CHAOS-002**：sentinel 在 docker-compose 跨 host network 是否真能选举（已知 docker DNS 与 Sentinel announce-ip 有坑） |
| **Redis Cluster CROSSSLOT** | 测 multi-key Lua 在 cluster mode 触发 CROSSSLOT error | 启 3-shard `redis-cli --cluster create` 6 节点 cluster；用 hash-tag `{}` 控制 slot 分布；**故意拆 settle Lua 的 KEYS[1..N] 到不同 slot** | 简单。**INVESTIGATE-CHAOS-003**：当前生产是否真用 Cluster？v2 §99 已列 INVESTIGATE-008；不论如何，本测必跑（防回归） |
| **ClickHouse down** | CH 进程 30s 不可达 | `docker compose stop clickhouse`（30s 后 `start`） | 不用任何 lib；docker CLI 直接 |
| **Pingora SIGKILL** | 中点杀 Rust 网关进程 | `docker compose kill -s SIGKILL gateway` + 自动重启 | 简单 |
| **slowloris** | 慢请求 / 不发完整 header / SSE 长连接低速 | 自写 Python `asyncio` client，每行隔 N 秒发一个字节；不引第三方 | 简单 |
| **X-Forwarded-For spoof** | 伪造 XFF 测限速/审计 | 自写 Python；header 直传 | 简单 |
| **跨版本 mixed-deploy** | 同时跑 N=2 Rust + N=2 Go，相互观察 | docker-compose override：`gateway-old` + `gateway-new` 各 1 实例 + `web-old` + `web-new` 各 1 实例；前端 LB（nginx）按 weight 50/50 | 简单 |
| **1000 并发 Deduct** | 并发钱测 | `wrk2` 或自写 `aiohttp` 1000 connection；前置 1 wallet 充值 N micros | `wrk2` 更稳，但要构造自定义 body → **决策**：自写 `aiohttp`（与现有 Python harness 同栈）|
| **多副本 outbox processor race** | 同时跑 N=3 outbox processor 见 §3.J | docker-compose scale `web=3`；让所有副本指向同 PG | 简单 |

> **§0.2 决策原则**：每个 scenario 优先用 docker 原生信号 / SQL / iptables / 自写小 Python；只在没法避免时引入 Toxiproxy。pgmocker 不存在，已替换。

### 0.3 名词

- **scenario**：一次 chaos 注入 + 一组不变式断言。一个 YAML 文件，名字（slug）就是 scenario_name。
- **invariant**：scenario 跑完必须为真的可机检属性（如「钱守恒」「PEL 收敛」「无重复 settle」）。
- **smoke 子集**：scenarios 中 marker=`smoke` 的子集，单次 ≤ 15 min，CI 每 PR 必跑。
- **full 子集**：所有 scenarios，单次 ≤ 24h，launch gate 前必跑。
- **stable name**：scenario_name 一旦在本计划落定，其它 WS DoD 直接引用；改名等同破契约。

### 0.4 stable scenario 名清单（其它 WS DoD 引用的"键"）

| scenario_name | 一句话 | 引用方 |
|---|---|---|
| `pingora-sigkill-midflight` | Rust 网关 SIGKILL 中点 | WS-1, WS-2, WS-6 |
| `pingora-sigkill-during-settle` | 网关在 settle_lua 之间被杀 | WS-1, WS-2 |
| `pg-terminate-mid-tx-deduct` | Deduct tx 第 500 个 op 触发 PG 闪断 | WS-1 (REV-005 范例) |
| `pg-terminate-mid-tx-refund` | Refund.Create 闪断 | WS-1 |
| `pg-terminate-during-outbox-relay` | outbox processor commit 后闪断 | WS-1, WS-2 |
| `pg-delay-1s-on-idem-insert` | idempotency_keys insert 延迟 | WS-1 |
| `redis-sentinel-failover-during-lease` | sentinel 切主时 lease 操作 | WS-1, WS-2 |
| `redis-sentinel-partition-30s` | 30s 分区无 quorum | WS-1, WS-2, WS-5 |
| `redis-cluster-crossslot-settle` | settle Lua 跨 slot | WS-1, WS-2 |
| `redis-down-during-mgmt-revoke` | mgmttoken Revoke 时 Redis 断 | WS-5 (G10) |
| `clickhouse-down-30s` | CH 30s 不可达 | WS-2, WS-9 |
| `clickhouse-down-during-archive-flush` | flush 中 CH 断 | WS-2 |
| `mixed-deploy-go-new-rust-old` | C4 部署顺序 | WS-1, WS-8, WS-14, WS-15 |
| `mixed-deploy-go-old-rust-new` | 反向（CI 必须阻止）→ harness 验证拦截 | WS-12, WS-15 |
| `slowloris-anthropic-stream` | 1 字节/秒 SSE | WS-3, WS-6 |
| `slowloris-openai-chat` | 慢 header | WS-3, WS-6 |
| `xff-spoof-rate-limit-bypass` | XFF 伪造打限速 | WS-5 (R4), WS-7 |
| `xff-spoof-audit-log-poison` | XFF 伪造审计污染 | WS-5, WS-10 |
| `concurrent-deduct-1000` | 1000 并发同 wallet | WS-1 (REV-005 范例) |
| `concurrent-deduct-1000-pg-blip` | 1000 并发 + 中点 PG 断 | WS-1, WS-2 |
| `outbox-processor-race-3replicas` | 3 副本同抢 outbox 行 | WS-1 (REV-FINAL-M1 SKIP LOCKED) |
| `inflight-reconciler-crash-replay` | reconciler 崩溃 + replay file dedup | WS-1 (R6, REV-FINAL-M8) |
| `lease-redis-blip-permanent-leak` | TaskFreeze.release 期 Redis 抖 | WS-1 (R5) |
| `jwt-alg-confusion` | 客户端 alg=HS256 攻 RS256 部署 | WS-5 (R13, REV-CROSS-AUTH) |
| `jwt-kid-static-pem-fallback` | 未知 kid 静默回退 | WS-5 (R3) |
| `hmac-replay-window` | 5min 内重放 | WS-5 (R2) |
| `apikey-cache-stale-after-revoke` | revoke 后 60s 内仍放行 | WS-5 (G12, REV-FINAL-V17) |
| `license-warn-to-enforce-flip` | warn 14 天 → enforce 切换 | WS-5, WS-15 (REV-004e) |
| `feature-flag-rollback-jwt-strict` | jwt_strict 回滚 | WS-15 |
| `clock-drift-2s-rust-go` | Rust 与 Go 时钟差 2s | WS-1, WS-10 |
| `redos-attack-100ms-budget` | 恶意正则 100ms 预算耗尽 | WS-5 (R14), WS-3 |
| `crossprotocol-stream-truncate-cap` | upstream 超 max_accumulated_bytes | WS-3 (REV-FINAL-V3), WS-6 |
| `oom-prevention-rss-soak-24h` | 24h RSS 波动 ≤ ±10% | WS-3, WS-7, REV-005 |
| `panic-recovered-zero-soak-24h` | 24h 所有 panic_recovered_total == 0 | WS-4 |
| `idemkey-collision-cross-admin` | 跨 admin 用同 IdemKey | WS-1 (REV-FINAL-M11) |

35 个 stable name。其它 WS DoD **必须**在 `dod:` 字段直接列出引用名，写错即 lint 报错（§7 #15 新加）。

---

## §1. 根因与本 WS 的存在理由

### 1.1 根因

v1 / v2 几乎每个 WS DoD 都包含「chaos 注入测试通过」。但 **chaos 注入基础设施在仓里不存在**——
- Rust 仓 `tests/e2e/client/soak/` 仅有"upstream chaos"（FAKE_UPSTREAM_CHAOS=on/off），不涉及 PG/Redis/CH/网关本身的故障。
- Go 仓 `tests/e2e/` 只有 `smoke_test.go`，没有任何 chaos 设施。
- 没有 `make chaos-*` target。
- 没有 scenario 描述格式。
- 没有 PASS/FAIL JSON 报告 schema。
- 没有跨版本 mixed deploy 测试位。

**WS-13 = 解决"DoD 引用了不存在的设施"这一**先决性**缺口**。它**先**被建好，**后**其它 WS 才能闭合 DoD。

### 1.2 v1 / v2 引用聚合

| 引用 | 出处 |
|---|---|
| "chaos 测试通过" | v1 §1 P3、v1 §7 #2/#3 |
| "24h stability soak" | v1 §7 #4 |
| "`make e2e-soak` ×3 protocol × stream/non-stream × {off, light, heavy} chaos" | v1 §7 #3 |
| "REV-005 1000 并发 Deduct + 第 500 op 处 pg_terminate_backend" | v2 §2 REV-005 |
| "chaos: archive 阻塞 → 业务 worker 不被 starved" | v1 §4 WS-6 DoD |
| "chaos：Redis 闪断中点 → mgmttoken Revoke 返回 503" | v1 §4 WS-5 DoD |
| "DoS test: spoof X-Forwarded-For 5000 unique" | v1 §4 WS-7 DoD |
| "Task worker SIGKILL 中点 → lease 超时后 reclaim、registry 不空双执行" | v1 §4 WS-2 DoD |
| "跨版本 mixed deploy（同时跑两个 Rust 版本 + 两个 Go 版本）" | v2 §5 WS-13 |
| "Redis CROSSSLOT 模拟" | v2 §5 WS-13, v2 §99 INVESTIGATE-008 |

---

## §2. 不做什么（DON'T）

- ❌ **不**修任何业务 bug。WS-13 是测试设施，业务 bug 归对应 WS。
- ❌ **不**使用任何不存在或未验证的第三方工具（明确：pgmocker 不存在，已被替换；详见 §0.2）。
- ❌ **不**用云供应商专属 chaos lib（GCP Fault Injection、AWS FIS）——本地 docker compose 必须能跑全集；CI 才能用。
- ❌ **不**为单次 scenario 写一次性 ad-hoc 脚本；所有 scenario 必须遵守同一 YAML schema，否则跨 WS 引用会乱。
- ❌ **不**让 chaos scenarios 修改本地代码、生成 patch、commit；harness 只**注入运行时故障**，不改源。
- ❌ **不**让 invariant 检查依赖 Grafana / Prom UI；必须能在 CI 无人值守纯 stdout/JSON 跑。
- ❌ **不**写「sleep 60 then check」式断言；用 wait-for-condition 带 deadline，超时 FAIL。
- ❌ **不**让 scenario 之间共享状态（数据库/Redis/磁盘）；每 scenario 独立 compose stack + 独立 volume + 启动前 `docker compose down -v`。

---

## §3. 任务清单（30 个 task）

> 编号 W13-Tnn。每个 task 8 段格式：是什么 / 不是什么 / 为什么 / 做什么 / 不做什么 / 怎么做 / 如何验证 / 依赖与并行。

> **任务分群**：A=框架根; B=注入器; C=不变式; D-J=具体 scenario 类别。

### TASK W13-T01：建立 `tests/e2e/chaos/` 目录骨架与 YAML schema

**【是什么】**
- 新目录 `tests/e2e/chaos/`（Go 仓与 Rust 仓**各一份**，scenario YAML 镜像；Python harness 主版本居 Rust 仓 `ailinkmax-gateway/tests/e2e/chaos/harness/`，Go 仓做 `git submodule` 或 symlink 引用。**当前决策（候选 A）**：双份完整副本 + CI 校验字节一致；**候选 B**：仅 Rust 仓一份 + Go 仓 Makefile target 触发跨仓调用。INVESTIGATE-CHAOS-004 决定）。
- 文件 `tests/e2e/chaos/scenario.schema.json`：定义 scenario YAML 的 JSON Schema。
- 文件 `tests/e2e/chaos/scenarios/_template.yaml`：每条 scenario 的范本。

**【不是什么】**
- ❌ 不在本 task 写任何具体 scenario；只建框架。
- ❌ 不写 injector / invariant 代码；那是 T02-T07。
- ❌ 不动 Makefile；那是 T28-T29。

**【为什么】**
- v2 §5 WS-13 要求"统一 scenario 描述 YAML"。
- 现仓无 chaos 框架；其它 30+ scenario task 都依赖本 task 的 schema。

**【做什么】**
1. 创建目录树：
   ```
   tests/e2e/chaos/
     scenario.schema.json
     scenarios/
       _template.yaml
     harness/
       __init__.py
       runner.py        # 主入口：load yaml → run injector → assert invariants → write JSON
       report.py        # 写 .chaos_report/*.json
       compose.py       # docker compose lifecycle 抽象
       loader.py        # YAML → ScenarioSpec dataclass + JSON Schema 校验
     injectors/
       __init__.py
     invariants/
       __init__.py
     fixtures/
       compose.base.yml          # 基础 compose（PG/Redis/CH/Rust 网关/Go web/fake-upstream）
       compose.overrides/        # 各类 override（mixed-deploy、cluster、sentinel）
   ```
2. JSON Schema 字段：
   - `name: str`（slug，作 stable name）
   - `description: str`
   - `markers: [smoke|full|p0_security|p0_money|long_soak|cluster_only]`
   - `compose_overrides: [str]`（fixtures/compose.overrides/<x>.yml 名）
   - `injector: { kind: str, params: object }`（kind 必须是 §3 注册的）
   - `workload: { kind: str, params: object }`（kind: aiohttp_concurrent | sse_slowloris | pytest_test_id | ...）
   - `invariants: [ { kind: str, params: object } ]`（顺序检）
   - `timeout_seconds: int`（默认 600）
   - `references: { ws: str, rev_ids: [str], dod_callers: [str] }`（这些 scenario 被哪些 WS 引用，CI 拿来做反向 lint）
3. `_template.yaml` 必含上述所有字段，每字段加 inline 注释。

**【不做什么】**
- ❌ 不写 `compose.base.yml` 内容（依赖 Rust 仓现 `tests/e2e/docker-compose.yml` 抄并扩展，T02 做）；本 task 只创空文件 + commit。
- ❌ 不引外部 schema validator 库；用 Python `jsonschema`（pip 已用）即可。

**【怎么做】**
- `mkdir -p tests/e2e/chaos/{scenarios,harness,injectors,invariants,fixtures/compose.overrides}`
- 写 `scenario.schema.json`（JSON Schema draft 2020-12）；字段如上。
- `runner.py` 顶层：
  ```python
  # tests/e2e/chaos/harness/runner.py
  from __future__ import annotations
  import argparse, json, sys, time
  from pathlib import Path
  from .loader import load_scenario
  from .compose import ComposeSession
  from .report import write_report
  from ..injectors import resolve_injector
  from ..invariants import resolve_invariant

  def run(scenario_path: Path, report_dir: Path) -> int:
      spec = load_scenario(scenario_path)
      report = {"name": spec.name, "status": "RUNNING", "started_at": time.time(),
                "markers": spec.markers, "invariants": [], "errors": []}
      try:
          with ComposeSession(spec.compose_overrides, timeout=spec.timeout_seconds) as cs:
              cs.wait_ready()
              injector = resolve_injector(spec.injector.kind, spec.injector.params, cs)
              with injector:
                  workload = resolve_workload(spec.workload.kind, spec.workload.params, cs)
                  workload.run()
              for inv in spec.invariants:
                  ok, detail = resolve_invariant(inv.kind, inv.params, cs).check()
                  report["invariants"].append({"kind": inv.kind, "ok": ok, "detail": detail})
                  if not ok: report["status"] = "FAIL"
              if report["status"] != "FAIL":
                  report["status"] = "PASS"
      except Exception as e:
          report["status"] = "FAIL"; report["errors"].append(repr(e))
      finally:
          report["ended_at"] = time.time()
          write_report(report_dir / f"{spec.name}.json", report)
      return 0 if report["status"] == "PASS" else 1

  if __name__ == "__main__":
      p = argparse.ArgumentParser()
      p.add_argument("scenario")
      p.add_argument("--report-dir", default=".chaos_report")
      a = p.parse_args()
      sys.exit(run(Path(a.scenario), Path(a.report_dir)))
  ```

**【如何验证】**
1. `python -m tests.e2e.chaos.harness.runner tests/e2e/chaos/scenarios/_template.yaml --report-dir /tmp/cr` → 报告 `_template.json` 写出，`status` 为 `FAIL`（template 没真注入器实现，FAIL 即预期）；schema validation 报「injector kind unknown」非「schema invalid」。
2. `pytest tests/e2e/chaos/harness/test_loader.py::test_schema_template_valid` PASS（断 `_template.yaml` 通过 schema）。
3. 文件存在断言：`ls tests/e2e/chaos/scenario.schema.json tests/e2e/chaos/scenarios/_template.yaml tests/e2e/chaos/harness/runner.py` 全部 exit 0。

**【依赖与并行】**
- 阻塞所有其它 W13-T*；本 task 必须 D0 完成。
- 并行：无。

---

### TASK W13-T02：建立 `compose.base.yml` 基础栈（PG / Redis / CH / fake-upstream / Rust 网关 / Go web）

**【是什么】**
- 文件 `tests/e2e/chaos/fixtures/compose.base.yml`：从 Rust 仓 `tests/e2e/docker-compose.yml` 派生，**追加** Go web 服务（`binaries/web`） + ClickHouse + 必要的 healthcheck。
- 文件 `tests/e2e/chaos/fixtures/compose.overrides/sentinel.yml`：3 个 Redis Sentinel + 1 master + 2 replicas。
- 文件 `tests/e2e/chaos/fixtures/compose.overrides/cluster.yml`：6 节点 Redis Cluster（3 master + 3 replica）。
- 文件 `tests/e2e/chaos/fixtures/compose.overrides/mixed-deploy.yml`：`gateway-old` + `gateway-new` + `web-old` + `web-new` + nginx LB。
- 文件 `tests/e2e/chaos/fixtures/compose.overrides/toxiproxy.yml`：toxiproxy sidecar，可选启用（验证 INVESTIGATE-CHAOS-001 后才合并）。

**【不是什么】**
- ❌ 不在本 task 写 mixed-deploy 的"old/new"镜像构建（D11 才做）；只占位 service 名 + comment。
- ❌ 不引 GCP/AWS chaos lib。

**【为什么】**
- 所有 scenario 共享同一基础栈才能保证可重复；override 文件是变种入口。
- 现 Rust 仓 compose 没 Go web → 不能测跨进程契约 chaos。

**【做什么】**
1. 读 `ailinkmax-gateway/tests/e2e/docker-compose.yml`，复制到 `tests/e2e/chaos/fixtures/compose.base.yml`。
2. 追加 service `clickhouse:`（image `clickhouse/clickhouse-server:24.3`，healthcheck `clickhouse-client -q "SELECT 1"`，volume `ch-data`）。
3. 追加 service `web:`（Go binary，build from `AILinkMax-Go/binaries/web`，env 指 PG/Redis/CH，依赖 PG/Redis healthy）。
4. 给 `postgres`、`redis`、`clickhouse`、`gateway`、`web` 每个加 healthcheck 与 `start_period`。
5. 写 `compose.overrides/sentinel.yml`：
   - service `redis-master`、`redis-replica-1/2`（image `redis:7-alpine`，命令 `redis-server --replicaof redis-master 6379`）。
   - service `sentinel-1/2/3`（image `redis:7-alpine`，`redis-sentinel /etc/sentinel.conf`）；conf 模板 mount。
   - **关键**：`sentinel announce-ip <docker bridge ip>`；INVESTIGATE-CHAOS-002 先查。
6. 写 `compose.overrides/cluster.yml`：6 个 service，启动后 entrypoint 调 `redis-cli --cluster create ... --cluster-replicas 1 --cluster-yes`。
7. 写 `compose.overrides/mixed-deploy.yml`：占位 `gateway-old` `gateway-new` `web-old` `web-new` 各一 service；构建上下文 `tests/e2e/chaos/fixtures/mixed-deploy/old/Dockerfile`（T19 落地）；加 nginx LB 服务。

**【不做什么】**
- ❌ Sentinel 不要用最简 `redis-sentinel-init` one-liner 镜像（github.com/bitnami/sentinel 有但 announce-ip 坑过）；用官方 image + 手写 sentinel.conf mount。
- ❌ 不把 healthcheck `interval` 设太小（5s 起步），否则 docker daemon 风扇响。

**【怎么做】**
- 用 `docker compose -f compose.base.yml -f compose.overrides/sentinel.yml config` 验证 merge 后无冲突。
- ClickHouse 数据 schema：跑 `AILinkMax-Go/migrations-clickhouse/`（goose clickhouse），entrypoint 加 init container。

**【如何验证】**
1. `cd tests/e2e/chaos && docker compose -f fixtures/compose.base.yml up -d --build` → `docker compose ps` 全部 healthy（120s 内）。
2. `docker compose exec postgres psql -U postgres -c "SELECT 1"` → 1。
3. `docker compose exec redis redis-cli PING` → PONG。
4. `docker compose exec clickhouse clickhouse-client -q "SELECT 1"` → 1。
5. `curl -fsS http://localhost:8080/livez` 返 200（Rust 网关）。
6. `curl -fsS http://localhost:8081/livez` 返 200（Go web）。
7. `docker compose -f fixtures/compose.base.yml -f fixtures/compose.overrides/sentinel.yml up -d` 启 sentinel；`docker compose exec sentinel-1 redis-cli -p 26379 SENTINEL master mymaster` 返 master IP。
8. `docker compose -f fixtures/compose.base.yml -f fixtures/compose.overrides/cluster.yml up -d` 启 cluster；`redis-cli -p 7000 CLUSTER INFO` `cluster_state:ok`。
9. **down 测试**：`docker compose down -v` 后 `docker volume ls` 无残留命名 volume（保隔离）。

**【依赖与并行】**
- 阻塞于 T01；阻塞所有 scenario task（T08-T27）。
- 并行：T03-T07。

---

### TASK W13-T03：注入器 — 进程信号（SIGKILL / SIGTERM）

**【是什么】**
- `injectors/process_signal.py`：注入器 kind `process_signal`，参数 `{ service: str, signal: SIGKILL|SIGTERM|SIGSTOP, delay_ms: int, restart_after_ms: int|null }`。

**【不是什么】**
- ❌ 不杀 host 进程；只杀 compose service container。
- ❌ 不用 `docker kill --signal`（在某些 docker 版本对 entrypoint 不传播 SIGSTOP）；直接 `docker compose exec <svc> kill -<sig> 1` 给 PID 1。

**【为什么】**
- `pingora-sigkill-midflight` 等 scenario 需要在网关运行中被杀；要可定时、可重启。

**【做什么】**
1. `class ProcessSignalInjector`：context manager；`__enter__` 启异步 task 等 `delay_ms` 后 `docker compose exec <svc> kill -<signal> 1`；若 `restart_after_ms` 非空，再启 task 等该时长后 `docker compose start <svc>`。
2. `__exit__` 保证 restart task 等待完成，否则后续 invariant 检查会撞到 not-ready。

**【不做什么】**
- ❌ 不用 `os.kill`；只通过 docker CLI（保证容器内行为）。

**【怎么做】**
```python
class ProcessSignalInjector:
    def __init__(self, params, cs): self.svc, self.sig = params["service"], params["signal"]
    self.delay = params["delay_ms"]/1000.0; self.restart_after = params.get("restart_after_ms")
    def __enter__(self):
        self._t = asyncio.create_task(self._inject())
        return self
    async def _inject(self):
        await asyncio.sleep(self.delay)
        await self.cs.exec(["kill", f"-{self.sig}", "1"], service=self.svc)
        if self.restart_after is not None:
            await asyncio.sleep(self.restart_after/1000.0)
            await self.cs.start(self.svc)
            await self.cs.wait_service_healthy(self.svc, timeout=60)
    def __exit__(self, *exc): asyncio.get_event_loop().run_until_complete(self._t)
```

**【如何验证】**
1. 单元：`pytest tests/e2e/chaos/injectors/test_process_signal.py::test_kill_then_restart` — mock cs，断 exec 与 start 被按时调用。
2. 集成：写一个临时 scenario `_test_process_signal.yaml`，inject SIGKILL on `gateway`，restart_after 5s，invariant `service_healthy{service: gateway}`，预期 PASS。

**【依赖与并行】**
- 阻塞于 T02；并行于 T04-T07。

---

### TASK W13-T04：注入器 — PG 故障（pg_terminate_backend / 延迟 / 断连）

**【是什么】**
- `injectors/pg_fault.py`：kind `pg_fault`，子模式 `terminate_mid_op | latency | drop_connection`。

**【参数】**
- `terminate_mid_op`: `{ match_query_regex: str, after_n_matches: int, kill_signal: int? }` — 通过轮询 `pg_stat_activity.query` 找到匹配 PID 第 N 次，调 `SELECT pg_terminate_backend(pid)`。
- `latency`: `{ via: "toxiproxy"|"iptables_tc", delay_ms: int, duration_s: int }` — 优先 toxiproxy（INVESTIGATE-CHAOS-001 通过后）。
- `drop_connection`: `{ via: ..., duration_s: int }` — 全部 PG 流量 DROP。

**【为什么】**
- v2 REV-005 范例直接点名"pgmocker 在第 500 op 处 pg_terminate_backend"；pgmocker 不存在 → 本 task 自实现。

**【做什么】**
1. `class PgFaultInjector` 实现三模式；每模式起监听 task。
2. `terminate_mid_op` 走 PG 自连，轮询周期 100ms（用 prepared `SELECT pid FROM pg_stat_activity WHERE query ~ $1 AND state='active'`）。
3. `latency` 默认实现走 toxiproxy `populate { upstream, type: latency, attributes: { latency: <ms> } }`；若 toxiproxy 不可用，降级到 `tc qdisc add dev eth0 root netem delay`（需 cap_net_admin sidecar）。
4. `drop_connection` 走 toxiproxy `enabled=false` 或 `iptables -A OUTPUT -p tcp --dport 5432 -j DROP`。

**【不做什么】**
- ❌ 不修业务代码插断点。
- ❌ 不在测试里改 PG `wal_level` / `max_connections` 等持久化配置——所有故障都是运行时可逆的。

**【怎么做】**
- `terminate_mid_op` 代码大意：
  ```python
  async def _terminate_loop(self):
      conn = await asyncpg.connect(self.dsn)
      hits = 0
      while not self._cancel:
          rows = await conn.fetch("SELECT pid, query FROM pg_stat_activity WHERE state='active'")
          for r in rows:
              if re.search(self.regex, r["query"]):
                  hits += 1
                  if hits == self.after_n_matches:
                      await conn.execute("SELECT pg_terminate_backend($1)", r["pid"])
                      self._fired = True
                      return
          await asyncio.sleep(0.1)
  ```

**【如何验证】**
1. 单元（with testcontainers PG）：开 1000 个并发 `BEGIN; SELECT pg_sleep(2)` → injector match `pg_sleep(2)` after_n_matches=500 → 第 500 个 sleep 被 terminate（其 client 报 `57P01 admin_shutdown`），其余 999 完成；断 hit 计数 = 500 ± 0。
2. INVESTIGATE-CHAOS-001：跑 toxiproxy compose override + `python -c "from injectors.pg_fault import _toxi_latency_smoke; _toxi_latency_smoke(50)"` → 简单 `SELECT 1` RTT 增加 ≥50ms（容差 ±20ms）。

**【依赖与并行】**
- 阻塞于 T02；并行 T03/T05/T06/T07。
- INVESTIGATE-CHAOS-001 闭合前 latency / drop_connection 用 iptables；toxiproxy 实现可后置。

---

### TASK W13-T05：注入器 — Redis 故障（partition / failover / cluster CROSSSLOT）

**【是什么】**
- `injectors/redis_fault.py`：kind `redis_fault`，子模式 `sentinel_partition | sentinel_failover | cluster_crossslot | down`。

**【参数】**
- `sentinel_partition`: `{ duration_s: int, isolate: ["master"|"replicas"|"sentinels"] }` — iptables drop sentinel ↔ master 流量。
- `sentinel_failover`: `{}` — `docker compose stop redis-master`，等 sentinel 选新主，restart 老 master 作 replica。
- `cluster_crossslot`: `{}` — 不注入故障；这是 compose.overrides/cluster.yml 启用 + workload 故意构造跨 slot Lua 调用，invariant 检 CROSSSLOT 错。
- `down`: `{ duration_s: int }` — `docker compose stop redis` 后 `start`。

**【为什么】**
- v2 §5 WS-13 三种 Redis chaos 全要；CROSSSLOT 验防回归（v2 INVESTIGATE-008 闭合前必跑）。

**【做什么】**
1. `sentinel_partition` 用 `iptables` 在 master 容器内 `OUTPUT -p tcp --dport 26379 -j DROP`（隔离 sentinel）或 `INPUT -s <sentinel-ip> -j DROP`。
2. `sentinel_failover` 时序：stop master → poll sentinel `SENTINEL get-master-addr-by-name mymaster` 直到返回新 IP（含 quorum 等待时间，30s timeout）→ start 老 master（变 replica）。
3. `cluster_crossslot` 验 cluster 已健康（CLUSTER INFO），不做注入；workload 侧自然触发。
4. `down` 简单 stop/start。

**【不做什么】**
- ❌ 不用 `DEBUG SLEEP`（master 假死会被 sentinel 当不响应 → 跟 failover 同效，但行为不可控）。
- ❌ 不修 sentinel 配置 down-after-milliseconds 太短（< 5s 会误判）；保 30s 默认。

**【怎么做】**
- iptables 注入要求 sidecar 容器或主容器有 `cap_add: NET_ADMIN`；在 compose.base.yml 给 redis-master 加：
  ```yaml
  cap_add: [NET_ADMIN]
  ```
- failover 验证：
  ```python
  async def wait_for_new_master(self, old_addr, timeout=30):
      deadline = time.time() + timeout
      while time.time() < deadline:
          new = await self._sentinel_get_master()
          if new != old_addr: return new
          await asyncio.sleep(0.5)
      raise TimeoutError
  ```

**【如何验证】**
1. 单元：mock sentinel cli，断顺序 stop → 检测 → start。
2. 集成：起 sentinel compose；inject `sentinel_failover`；运行简单 `SET k v / GET k` workload；invariant `redis_master_addr_changed` PASS 且 `keys_observed_lost == 0`。
3. CROSSSLOT：跑 cluster compose；workload `EVAL "return redis.call('GET',KEYS[1]) .. redis.call('GET',KEYS[2])" 2 a b`（a 与 b 落不同 slot）→ 返回 `CROSSSLOT Keys in request don't hash to the same slot`；invariant `redis_crossslot_observed` PASS。

**【依赖与并行】**
- 阻塞于 T02；并行其它 injector tasks。

---

### TASK W13-T06：注入器 — ClickHouse 故障 + 网络（slowloris / xff）

**【是什么】**
- `injectors/ch_fault.py`：kind `ch_fault`，子模式 `down | latency`。
- `injectors/network.py`：kind `network`，子模式 `slowloris_emitter | xff_spoof`。
  - `slowloris_emitter` 是 workload-flavored injector（启动多个慢连接并保活）。
  - `xff_spoof` 不需要 injector，是 workload 行为；放这里只为统一接口。

**【为什么】**
- CH down 30s + archive flush 中断 → 验 R23 / WS-2 不变式。
- slowloris 跨协议 → REV-010 attacker scenario。
- XFF spoof → R4 / G14 / H12 验证。

**【做什么】**
1. `ch_fault.down`: `docker compose stop clickhouse` → wait `duration_s` → `start clickhouse` → `wait_for ClickHouse Pong`。
2. `ch_fault.latency`: toxiproxy CH 端口 latency；同 PG。
3. `network.slowloris_emitter`: 启 N 个 asyncio coroutine，每个开 socket 到网关，发 `GET /v1/messages HTTP/1.1\r\nHost: x\r\n`，然后每 5s 发 1 字节 header（never finish），保活 `duration_s`。
4. `network.xff_spoof`: 不在 injector 跑；在 workload 跑。injector kind 仍占位（NOP）以保证 schema 一致。

**【不做什么】**
- ❌ slowloris 不用第三方 lib（`slowloris.py` GitHub 上的工具）；自写避免依赖。
- ❌ XFF spoof 不真去模拟代理；只在 Python http client 设 `X-Forwarded-For: 1.2.3.4`。

**【怎么做】**
- `slowloris_emitter`：
  ```python
  async def _slow_conn(self):
      r, w = await asyncio.open_connection(self.host, self.port)
      w.write(b"POST /v1/messages HTTP/1.1\r\nHost: x\r\n")
      await w.drain()
      while not self._cancel:
          await asyncio.sleep(5)
          w.write(b"X-Slow: a\r\n"); await w.drain()
  ```

**【如何验证】**
1. CH down：start scenario `clickhouse-down-30s`；inject 30s down；workload 跑 archive 写 1000 条；invariant `archive_dlq_drained_after_recovery` PASS。
2. slowloris：start scenario `slowloris-anthropic-stream`；100 个慢连；invariant `gateway_max_concurrent_streams_held < cap` 与 `legit_request_rt_ok` 二者都 PASS。

**【依赖与并行】**
- 阻塞于 T02。

---

### TASK W13-T07：注入器 — 时钟漂移（clock_drift）

**【是什么】**
- `injectors/clock_drift.py`：kind `clock_drift`，参数 `{ service: str, offset_seconds: int }`。

**【为什么】**
- v2 REV-010 列 attacker scenario "时钟漂移"；HMAC ±300s、JWT exp、lease TTL 均依赖 clock。

**【做什么】**
- 用 `libfaketime`（compose service 启动时 `LD_PRELOAD=/usr/lib/x86_64-linux-gnu/faketime/libfaketime.so.1`，`FAKETIME="+2 seconds"`）。
- 或重启 service 用 `date -s` 配 capability `SYS_TIME`。
- 决策：`libfaketime` 不需 capability，**首选**。

**【不做什么】**
- ❌ 不动 host clock（伤害 CI runner）。
- ❌ 不用 `NTP` shenanigans。

**【怎么做】**
- compose.overrides/clock-drift.yml：
  ```yaml
  services:
    gateway:
      environment:
        - LD_PRELOAD=/usr/lib/x86_64-linux-gnu/faketime/libfaketime.so.1
        - FAKETIME=+2 seconds
  ```
- injector 在 `__enter__` 时通过 `docker compose up -d --force-recreate gateway` 切换 override。

**【如何验证】**
- 单元：起 ubuntu 容器 + libfaketime，`date` 返回 +2s。
- 集成 scenario `clock-drift-2s-rust-go`：仅 Rust gateway offset +2s；HMAC ±300s 配 1s window → 一些请求按 Rust 时钟过期；invariant `hmac_signed_requests_rejected_count > 0` + `wallets unaffected`。

**【依赖与并行】**
- 阻塞于 T02；并行其它 injector。

---

### TASK W13-T08：不变式 — 钱守恒（wallet_conservation）

**【是什么】**
- `invariants/wallet_conservation.py`：kind `wallet_conservation`，参数 `{ wallet_ids: [int]|"all", tolerance_micros: int=0 }`。

**【为什么】**
- v2 REV-005 范例：「1000 并发 × 60s，最终 `wallets.recharge_balance_micros` 变化值 == Σ(成功响应 cost_micros) ± 0」。

**【做什么】**
- 在 workload **开始前**采样 `wallets.recharge_balance_micros + wallets.subscription_balance_micros`（all balance types）记为 `start`。
- workload 末记 `end`。
- 同时从 Go web 抓 `Σ(成功响应里 cost_micros)`（HTTP 响应 header `X-GW-Cost-Micros` 或响应 JSON 字段 `usage.cost_micros`）记为 `sum_charged`。
- 断 `start - end == sum_charged ± tolerance_micros`。
- 还要验 `start - end == Σ(idempotency_keys 内 distinct 成功扣)`。

**【不做什么】**
- ❌ 不只看 `recharge_balance_micros`；subscription 余额也算。
- ❌ tolerance 默认 0；只在迁移期 dual-write 测试里允许 >0（并明确写在 scenario 里）。

**【怎么做】**
```python
class WalletConservationInvariant:
    def check(self):
        start = self._sum_balances_at(self.t0)
        end = self._sum_balances_at(now())
        charged = sum(r.cost_micros for r in self.responses if r.ok)
        delta = start - end
        if abs(delta - charged) > self.tol:
            return False, f"delta={delta} charged={charged} diff={delta-charged}"
        return True, {"delta": delta, "charged": charged}
```

**【如何验证】**
- 单元 with PG fixture：种 wallet=1000；模拟 5 个成功扣 100 + 2 个失败 50；断 PASS。
- 反例：种 + 注入 1 个「响应 OK 但 PG 未减」→ FAIL with diff=-100。

**【依赖与并行】**
- 阻塞于 T02；并行其它 invariant tasks。

---

### TASK W13-T09：不变式 — Idem 唯一性（idem_key_uniqueness）

**【是什么】**
- `invariants/idem_key_uniqueness.py`：kind `idem_key_uniqueness`，参数 `{ op: str, expected_unique_count: int|"auto" }`。

**【为什么】**
- WS-1 REV-FINAL-M2：同 IdemKey 多次提交必须只 1 次写入；其余复现。

**【做什么】**
- 查 `idempotency_keys WHERE op=$1`；count distinct `key` == expected。
- 与 workload 发出的总请求数对比；断 dedup factor 与预期一致。
- 进一步：对每 distinct key 抓 `response_jsonb`，断同 key 跨调用拿到字面 equal 的 response（用 Python json compare）。

**【不做什么】**
- ❌ 不假设 `idempotency_keys` schema；执行前 `\d idempotency_keys` 核对（v2 §0 重要 2）。

**【怎么做】**
- SQL：`SELECT key, response_jsonb FROM idempotency_keys WHERE op=$1 ORDER BY key`；Python collect。

**【如何验证】**
- 单元 with PG fixture：插入 3 行同 key + 不同 response → FAIL（数据破坏检测）；插入 3 行同 key + 同 response → PASS。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T10：不变式 — Redis PEL / DLQ 收敛（pel_drained）

**【是什么】**
- `invariants/pel_drained.py`：kind `pel_drained`，参数 `{ stream: str, group: str, max_pending_after_grace_s: int=10, grace_s: int=60 }`。

**【为什么】**
- WS-2 R22：XACK 失败留 PEL，需 XAUTOCLAIM 兜底；测时 inject 后等 grace_s，PEL 必 <= max。

**【做什么】**
- workload 结束后 sleep `grace_s` → `XPENDING <stream> <group>` 取 pending count → 断 ≤ max。
- 同时检 DLQ stream（`events:entity:dlq`）长度（XLEN）应 = scenario 预期。

**【怎么做】**
- `redis-cli XPENDING usage:records billing 0 + 10000` 或更简单 `XLEN`。

**【如何验证】**
- 单元 with redis fixture：种 50 entries unacked → PASS @ max=100；FAIL @ max=10。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T11：不变式 — RSS / FD 平稳（resource_stability）

**【是什么】**
- `invariants/resource_stability.py`：kind `resource_stability`，参数 `{ service: str, metric: rss|fd|threads|gor, max_drift_pct: float=10.0, baseline_window_s: int=30 }`。

**【为什么】**
- v1 §7 #4 + REV-005：RSS P95 24h 波动 ≤ ±10%；FD 平稳；permit/PEL 平稳。

**【做什么】**
- 抓 Prom `process_resident_memory_bytes{service=...}`（gateway、web）；baseline = workload 启动后 baseline_window_s 内平均；end = workload 末 baseline_window_s 内平均。
- 断 `abs(end-baseline)/baseline <= max_drift_pct/100`。

**【不做什么】**
- ❌ 不抓单点最大 RSS；抓窗口均值（防 GC spike）。

**【怎么做】**
- `scrape /metrics` 直 HTTP；Python `prometheus_client.parser`。

**【如何验证】**
- 单元 with fake metrics endpoint：rss 1G → 1G → PASS；1G → 1.5G → FAIL。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T12：不变式 — Panic-recovered 计数（panic_recovered_zero）

**【是什么】**
- `invariants/panic_recovered_zero.py`：kind `panic_recovered_zero`，参数 `{ exclude_metrics: [str]=[]}`。

**【为什么】**
- v1 §7 #4：`gw_*_panic_recovered_total` 在正常路径为 0。chaos scenario 允许在被注入 panic 的局部置 >0；invariant 提供 `exclude_metrics` 白名单。

**【做什么】**
- 抓所有 metric 名 match `.*_panic_recovered_total`；扣除 exclude；剩余必为 0。

**【如何验证】**
- 单元 with metrics endpoint：metric=5 → FAIL；metric in exclude → PASS。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T13：不变式 — 日志无 secret 泄漏（log_no_secret）

**【是什么】**
- `invariants/log_no_secret.py`：kind `log_no_secret`，参数 `{ patterns: [regex], sample_lines: int=10000 }`。

**【为什么】**
- WS-10 S7 / G10 + v1 DoD「日志 sample 1k 行 → 0 个明显 secret pattern」。

**【做什么】**
- `docker compose logs --tail=10000 gateway web`；逐行匹配 `patterns`（默认 `sk_[a-zA-Z0-9]{20,}`, `Bearer [A-Za-z0-9_\-\.]{20,}`, `eyJ[a-zA-Z0-9_\-\.]{20,}`(JWT), `password=`, email regex）；命中即 FAIL。

**【如何验证】**
- 单元：写 fixture log，1 行含 `sk_test_abcdefghijklmnopqrst` → FAIL。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T14：不变式 — settle dedup（settle_no_double）

**【是什么】**
- `invariants/settle_no_double.py`：kind `settle_no_double`，参数 `{ rid_list_from_workload: bool=true }`。

**【为什么】**
- WS-1 R6 / REV-FINAL-M8：同 `(rid, attempt_index)` 不得重复 settle。

**【做什么】**
- 查 `usage:records` Redis stream 全量；每条 entry `(rid, attempt_index)` 加入 set；count == XLEN（无重）。
- 同时查 `billing:settled:{rid}:{attempt}` 或 v2 REV-FINAL-M8 后采用的 XRANGE 自查方式。
- 同时查 Go 侧 `wallets` audit log（如有）：同 rid 不超过 1 行扣费。

**【如何验证】**
- 单元 with redis fixture：故意双 XADD 同 (rid,attempt) → FAIL。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T15：不变式 — service ready / liveness（service_healthy_at_end）

**【是什么】**
- `invariants/service_healthy.py`：kind `service_healthy`，参数 `{ services: [str], deadline_s: int=60 }`。

**【为什么】**
- 所有 SIGKILL/sentinel/CH-down scenario 必须以 service 全恢复结尾。

**【做什么】**
- 轮询 `/livez` + `/readyz`；deadline 内必 200。

**【如何验证】** 不再赘述。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T16：scenario — `pingora-sigkill-midflight`

**【是什么】**
- `scenarios/pingora-sigkill-midflight.yaml`。

**【不是什么】**
- ❌ 不是测某个 endpoint；是测网关被杀后客户端能否拿到合理错误且 wallet 守恒。

**【为什么】**
- WS-1 / WS-2 / WS-6 引用。R7 / R8 复发预防。

**【做什么】**
- 配 injector `process_signal{service=gateway, signal=SIGKILL, delay_ms=2000, restart_after_ms=15000}`。
- workload `aiohttp_concurrent{n=200, duration_s=30, endpoint=/v1/messages, body=anthropic_smoke}`。
- invariants：`wallet_conservation`、`pel_drained{stream=usage:records, group=billing}`、`service_healthy{services=[gateway,web]}`、`settle_no_double`、`log_no_secret`、`panic_recovered_zero{exclude_metrics=[gw_pingora_killed_panic_total]}`。

**【不做什么】**
- ❌ 不让 workload 在网关 down 期间没有错误重试（要打开 client 端 retry，否则得不到"重启后请求成功"的覆盖）。

**【怎么做】**
- YAML 模板填字段；references 字段：`ws: [WS-1, WS-2, WS-6]`, `rev_ids: [REV-FINAL-W4, REV-FINAL-W10, REV-FINAL-H6.1]`, `dod_callers: [WS-1, WS-2, WS-6]`。

**【如何验证】**
- `python -m tests.e2e.chaos.harness.runner tests/e2e/chaos/scenarios/pingora-sigkill-midflight.yaml`
- 报告 `.chaos_report/pingora-sigkill-midflight.json` `status: PASS`；`invariants` 数组每项 `ok: true`。

**【依赖与并行】** 阻塞于 T03/T08/T10/T11/T12/T13/T14/T15；并行其它 scenario。

---

### TASK W13-T17：scenario — `pg-terminate-mid-tx-deduct`（REV-005 范例落地）

**【是什么】**
- `scenarios/pg-terminate-mid-tx-deduct.yaml`。

**【为什么】**
- v2 REV-005 直接命名引用：1000 并发 Deduct，第 500 op 处主动 pg_terminate_backend。

**【做什么】**
- injector `pg_fault{kind=terminate_mid_op, match_query_regex="INSERT INTO idempotency_keys.*op='deduct'", after_n_matches=500}`。
- workload `aiohttp_concurrent{n=1000, duration_s=60, endpoint=/v1/wallet/deduct, body=deduct_smoke, idempotency_key_per_request=true}`。
- invariants：`wallet_conservation{tolerance_micros=0}`、`idem_key_uniqueness{op=deduct, expected_unique_count=auto}`、`service_healthy`。

**【不做什么】**
- ❌ 不让 1000 并发都用同一 idem key（那是 W13-T26）；本 scenario 每请求 unique key，测的是「中点 PG 闪断后 idem + tx 是否原子回滚」。

**【怎么做】**
- workload yaml：`idempotency_key_per_request: true` → harness 给每请求生成 `uuid4()`。

**【如何验证】**
- 报告 `.chaos_report/pg-terminate-mid-tx-deduct.json` PASS。
- 抽查 SQL：`SELECT count(*) FROM idempotency_keys WHERE op='deduct'` == count(成功响应 unique idem keys)。

**【依赖与并行】** 阻塞于 T04/T08/T09/T15；并行。

---

### TASK W13-T18：scenario — `redis-sentinel-failover-during-lease` + `redis-sentinel-partition-30s`

**【是什么】**
- 两个 yaml：sentinel failover 中点 lease 操作；30s partition 无 quorum。

**【为什么】**
- R5 / W12 / lease ledger 不变式；INVESTIGATE-CHAOS-002 顺带闭合。

**【做什么】**
- compose_overrides: [sentinel.yml]。
- injector：`redis_fault{kind=sentinel_failover}` / `redis_fault{kind=sentinel_partition, duration_s=30, isolate=[sentinels]}`。
- workload：连续发 settle 请求 100/s × 60s（触 EXTEND_LEASE_LUA、SETTLE_LUA）。
- invariants：`wallet_conservation`、`settle_no_double`、`pel_drained`、`service_healthy`、`lease_no_permanent_leak`（新增，T20）。

**【不做什么】**
- ❌ 不让客户端 abort；保 retry-with-backoff，测重试路径。

**【如何验证】** 报告 PASS；`gw_lease_redis_failover_recovered_total > 0`。

**【依赖与并行】** 阻塞于 T05/T08/T10/T14/T15/T20。

---

### TASK W13-T19：scenario — `redis-cluster-crossslot-settle`

**【是什么】**
- `scenarios/redis-cluster-crossslot-settle.yaml`。

**【为什么】**
- v2 INVESTIGATE-008 + v1 附录 B "SETTLE_LUA + XACK CROSSSLOT"。

**【做什么】**
- compose_overrides: [cluster.yml]。
- injector：无（cluster mode 即是注入条件）。
- workload：故意请求一个会使 SETTLE_LUA 跨 slot 的 op。harness 在 `workload kind=crossslot_probe` 内构造 `KEYS=["{a}counter", "{b}usage"]`（不同 hash tag）→ Lua eval 必返 `CROSSSLOT`。
- invariants：`redis_crossslot_observed`（新增 T20）+ `wallet_conservation`（保不会因 CROSSSLOT 误扣）+ `service_healthy`。

**【如何验证】** `XLEN usage:records` 与 workload 总请求数一致；CROSSSLOT 错被 Rust 网关识别并降级为 `GW_REDIS_CROSSSLOT_DETECTED` metric +1（INVESTIGATE-008 闭合后 Rust 已避免）。

**【依赖与并行】** 阻塞于 T05/T08/T15。

---

### TASK W13-T20：补充不变式 — `lease_no_permanent_leak` + `redis_crossslot_observed` + `mixed_version_consistency`

**【是什么】**
- 三个 invariant：
  - `lease_no_permanent_leak`：scenario 结束后 grace 60s，`task_freeze_pending` 表行数 == 0（或 sweeper 已 release）。
  - `redis_crossslot_observed`：metric `gw_redis_crossslot_detected_total >= expected_min`。
  - `mixed_version_consistency`：mixed-deploy scenario 末，新旧版本两侧 wallet 余额一致；dedup key 集合一致。

**【为什么】** R9 / R5 / INVESTIGATE-008 / C4 部署顺序。

**【做什么】** 三个独立小文件 `invariants/lease_no_permanent_leak.py`、`invariants/redis_crossslot_observed.py`、`invariants/mixed_version_consistency.py`。

**【如何验证】** 单元 fixtures 与上面同款。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T21：scenario — `clickhouse-down-30s` + `clickhouse-down-during-archive-flush`

**【是什么】**
- `scenarios/clickhouse-down-30s.yaml`、`scenarios/clickhouse-down-during-archive-flush.yaml`。

**【为什么】** WS-2 R23、WS-9 archive flush 路径。

**【做什么】**
- injector `ch_fault{kind=down, duration_s=30}`，时点配 archive flush 概率高的 workload。
- workload：高吞吐请求 200/s × 90s（保证 archive batch 在 down 期间产生）。
- invariants：`pel_drained{stream=usage:detailed_dlq, max_pending_after_grace_s=120}`（不锁原 stream，verify 进 DLQ 后被恢复）+ `wallet_conservation` + `service_healthy`。

**【不做什么】** ❌ 不让 archive worker 在 CH down 期间 panic 整服。

**【如何验证】** CH 起后 `SELECT count(*) FROM usage_detailed` ≥ workload 请求数 × per-request emit factor。

**【依赖与并行】** 阻塞于 T06/T08/T10/T15。

---

### TASK W13-T22：scenario — `mixed-deploy-go-new-rust-old` + 反向 + 部署门禁验证

**【是什么】**
- `scenarios/mixed-deploy-go-new-rust-old.yaml`、`scenarios/mixed-deploy-go-old-rust-new.yaml`。
- 配套 task：在 `fixtures/mixed-deploy/old/` 与 `fixtures/mixed-deploy/new/` 写两套 Dockerfile，从 git tag/branch checkout 不同代码版本。

**【为什么】** WS-1 / WS-8 / C4 / REV-003f：Go 先部署。`go-new-rust-old` 必须正常（兼容旧 Rust）；`go-old-rust-new` 必须被 CI **拒绝**（验 WS-12 `check-deploy-order.sh` 真的拦得住）。

**【做什么】**
- compose_overrides: [mixed-deploy.yml]。
- workload：跨版本读写：发请求到 nginx LB（50/50 路由到 new/old gateway）；Go web 也 new/old 各 1。
- `go-new-rust-old`：invariants `wallet_conservation` + `idem_key_uniqueness` + `mixed_version_consistency` + `service_healthy`。
- `go-old-rust-new`：harness 期望 CI lint 在 scenario 启动**前**就 abort；scenario 顶层有 `expect_pre_launch_block: true`；invariant 验 `pre_launch_block_triggered{rule=check-deploy-order}`。

**【不做什么】**
- ❌ 不在本 task 实际写 "old" image 的代码版本固定逻辑；可用 `--build-arg GIT_REF=<sha>` 让 Docker build checkout 旧 SHA。INVESTIGATE-CHAOS-005：旧 SHA 选取（建议 main 分支前一周末 SHA 作 baseline）。

**【如何验证】** 报告 PASS；INVESTIGATE-CHAOS-005 闭合后 SHA 写入 `scenario.yaml` 注释。

**【依赖与并行】** 阻塞于 T02 + WS-12 部署门禁 lint。

---

### TASK W13-T23：scenario — `slowloris-anthropic-stream` + `slowloris-openai-chat`

**【是什么】**
- 两个 yaml。

**【为什么】** WS-3 / WS-6 / REV-010 跨协议 attacker。

**【做什么】**
- injector `network{kind=slowloris_emitter, n=100, duration_s=120, endpoint=...}`。
- workload 并行：另外 50 个 **正常**请求；invariant `legit_p99_latency_ms < 5000` + `slowloris_connections_capped`（metric `gw_slowloris_connections_max <= cap`）。
- 同时验 `panic_recovered_zero`、`wallet_conservation`、`pel_drained`。

**【不做什么】** ❌ 不让 slowloris 撑爆 host TCP，故 n=100 上限；CI 容器 fd ulimit 至少 4096。

**【如何验证】** 报告 PASS。

**【依赖与并行】** 阻塞于 T06。

---

### TASK W13-T24：scenario — `xff-spoof-rate-limit-bypass` + `xff-spoof-audit-log-poison`

**【是什么】**
- 两个 yaml。

**【为什么】** WS-5 R4 / WS-7 / G14 / H12。

**【做什么】**
- workload `aiohttp_concurrent` 自带 header `X-Forwarded-For: <random ip>` 每请求换；总 5000 unique；endpoint `/v1/messages`（限速桶）。
- 不 inject `trusted_proxies`；让网关默认 trusted_proxies=[]。
- invariants：`rate_limit_keyed_by_peer{not_xff}` —— 抓 `gw_ratelimit_block_total{key_source=peer_addr|xff}`，断 `key_source=peer_addr` ≥ 5000 - 限速窗口 - 容差；`key_source=xff` == 0。
- 第二 scenario：发 5000 unique XFF；查 audit 日志（Go web 写 `audit_log` 表）每行 `client_ip` 列必 == workload runner 真实出口 IP，不是 XFF。

**【如何验证】** 报告 PASS。

**【依赖与并行】** 阻塞于 T06 + invariant 新增 `rate_limit_key_source`（T25）。

---

### TASK W13-T25：补充不变式 — `rate_limit_key_source` + `audit_log_client_ip_correct` + `hmac_replay_rejected`

**【是什么】**
- 三个 invariant 文件。

**【为什么】** 测 R4 / R2 / G14。

**【做什么】**
- `rate_limit_key_source`：抓 Prom，断 `key_source=xff` 计数 == 0。
- `audit_log_client_ip_correct`：查 PG `audit_log` 列 client_ip；与 workload 出口 IP 比较。
- `hmac_replay_rejected`：scenario `hmac-replay-window` 内 5min 内重发同 nonce + signature；断网关返 401（不 200，不 503）；metric `gw_hmac_replay_rejected_total > 0`。

**【如何验证】** 单元 + 集成同款。

**【依赖与并行】** 阻塞于 T02；并行。

---

### TASK W13-T26：scenario — `concurrent-deduct-1000` + `concurrent-deduct-1000-pg-blip` + `outbox-processor-race-3replicas`

**【是什么】**
- 三个 yaml。

**【为什么】** WS-1 REV-FINAL-M1 / M2 / M11 直接落地。

**【做什么】**
- `concurrent-deduct-1000`：1000 并发**同 idem key**（验幂等）；invariant `wallet_conservation` + `idem_key_uniqueness{expected_unique_count=1}` + `successful_response_count==1000`（所有返同样 prior response shape）。
- `concurrent-deduct-1000-pg-blip`：同 + injector `pg_fault{terminate_mid_op, after_n_matches=500}`。
- `outbox-processor-race-3replicas`：compose `--scale web=3`；workload 触发 outbox 写入 1000 行；invariant 抓 PG `payment_outbox` 历史：每行 `processed_by_replica` 唯一 + 总 processed == 1000；并断 `FOR UPDATE SKIP LOCKED` 真的生效（用 `pg_stat_activity` 在 chaos 中采样无 worker 等行锁）。

**【不做什么】** ❌ 同 idem key 测不要让客户端各发不同 body；body 必须 byte-equal，否则 hash mismatch 触另一路径。

**【如何验证】** PASS。

**【依赖与并行】** 阻塞于 T04/T08/T09。

---

### TASK W13-T27：scenario — auth / jwt / hmac / apikey-cache 一组

**【是什么】**
- 5 yaml：`jwt-alg-confusion`、`jwt-kid-static-pem-fallback`、`hmac-replay-window`、`apikey-cache-stale-after-revoke`、`license-warn-to-enforce-flip`。

**【为什么】** WS-5 R1/R2/R3/R13 + G12 + REV-004e。

**【做什么】**
- `jwt-alg-confusion`：workload 用 RS256 部署的 public key 当 HS256 secret 签发 JWT，发到网关；invariant `jwt_alg_confusion_rejected{count > 0}` + `wallet_conservation`（没有越权扣）。
- `jwt-kid-static-pem-fallback`：JWT header.kid 设未知值；invariant 抓 metric `gw_jwt_kid_unknown_rejected_total > 0`（REV-FINAL-V20 fail-closed 后该计数必 >0）。
- `hmac-replay-window`：先发签名请求 OK；3min 内同 nonce 重发；断 401；5min 后再发要 401（如启 nonce_redis）或 200（如默认 none，但 metric 计数 >0）；scenario yaml 同时 lock `signature_secret.replay_check` 配置矩阵。
- `apikey-cache-stale-after-revoke`：用 apikey 发请求 OK；调 Admin Revoke；workload 立刻同 key 发请求；invariant 抓响应 401 在 1s 内（不是 60s 缓存窗口）+ `cache_invalidation_observed_within_1s`。
- `license-warn-to-enforce-flip`：scenario 启动时 `license.enforce=warn`；workload 模拟 30 个 license 违例请求 + 30 个合法；invariant `warn_violations_logged_count == 30` + `requests_blocked == 0`；scenario 第二阶段切 `enforce`（用 control-plane API 或 ConfigReload）；workload 重发；invariant `requests_blocked == 30`。

**【不做什么】** ❌ 不在测试里 hardcode 真实 RS256 key；用 ephemeral generated keypair（per-scenario）。

**【如何验证】** PASS。

**【依赖与并行】** 阻塞于 T02/T08/T25。

---

### TASK W13-T28：Makefile 集成 `make chaos-smoke` / `make chaos-full` / `make chaos-scenario NAME=`

**【是什么】**
- 修 `AILinkMax-Go/Makefile` 与 `ailinkmax-gateway/Makefile`：
  ```
  chaos-smoke:
      ./scripts/chaos-run.sh --markers smoke
  chaos-full:
      ./scripts/chaos-run.sh --markers smoke,full,long_soak --timeout 24h
  chaos-scenario:
      ./scripts/chaos-run.sh --scenario $(NAME)
  chaos-clean:
      rm -rf .chaos_report/
      docker compose -f tests/e2e/chaos/fixtures/compose.base.yml down -v
  ```
- 同步 `make.ps1`、`scripts/chaos-run.sh`、`scripts/chaos-run.ps1`。

**【为什么】** v2 §5 WS-13 明列两个 make target。

**【做什么】**
- `scripts/chaos-run.sh`：枚举 `tests/e2e/chaos/scenarios/*.yaml`；按 markers / 名字过滤；对每个 yaml 调用 `python -m tests.e2e.chaos.harness.runner <yaml> --report-dir .chaos_report`；最后聚合：任何 FAIL 即 `exit 1`。
- `chaos-smoke` markers=smoke，覆盖 §0.4 表中 ~15 个 P0 scenario（包括 5 个 auth、3 个 wallet、4 个生命周期、3 个 attacker）。
- `chaos-full` 包含 `long_soak` marker（24h soak scenario `oom-prevention-rss-soak-24h` + `panic-recovered-zero-soak-24h`）。

**【不做什么】**
- ❌ 不在 `chaos-smoke` 跑 24h soak（CI 会超时）。
- ❌ 不让 `chaos-full` 默认在本机跑（需 explicit `make chaos-full FORCE=1`）。

**【如何验证】**
1. `make chaos-smoke` 在干净 working tree 跑通；输出 `.chaos_report/*.json` 全 PASS（修任何 bug 前应至少有 ≥1 个 FAIL，作为「测真的有效」证据；修复后全 PASS）。
2. `make chaos-scenario NAME=pingora-sigkill-midflight` 单跑通。
3. `make chaos-clean` 后 `.chaos_report/` 与所有 docker volume 消失。

**【依赖与并行】** 阻塞于所有 scenario task；建议 D7 完成。

---

### TASK W13-T29：CI 集成 — PR 必跑 smoke / launch 必跑 full / DoD 反向 lint

**【是什么】**
- `.github/workflows/chaos-smoke.yml`（CI workflow）：每 PR `make chaos-smoke`。
- `.github/workflows/chaos-full.yml`：手动触发（workflow_dispatch），lauch gate 前一日跑。
- `scripts/check-chaos-dod-refs.sh`：扫各 WS sub-plan，断 `scenario_name` 引用都存在于 `tests/e2e/chaos/scenarios/`；倒过来：每个 scenario yaml 的 `references.dod_callers` 字段非空。

**【为什么】** REV-006 §7 #15（本计划新加）：DoD 引用 chaos scenario 必须双向存在。

**【做什么】**
- 工作流：runner ubuntu-latest，docker enabled；checkout 双仓（Go 仓为主，Rust 仓 submodule 或 secondary checkout）；`make chaos-smoke`；artifact `.chaos_report/`。
- lint 脚本：grep 各 plan md 文件 `scenario_name:` / `chaos:` 键；做双向 set 对比。

**【不做什么】** ❌ 不把 chaos-full 设默认 PR runner（太贵）。

**【如何验证】**
1. 故意改一个 WS sub-plan 引用一个不存在的 scenario → `make check` (含 `check-chaos-dod-refs.sh`) FAIL。
2. 故意删一个 scenario yaml `references.dod_callers` → 同样 FAIL。
3. CI workflow YAML lint 通过（`actionlint`）。

**【依赖与并行】** 阻塞于 T28。

---

### TASK W13-T30：文档 — `docs/business/chaos/README.md` + `scenarios.md` + runbook

**【是什么】**
- `docs/business/chaos/README.md`：harness 总述（如何加 scenario、如何写 invariant、如何 reproduce 失败）。
- `docs/business/chaos/scenarios.md`：从 yaml 自动生成（脚本 `scripts/gen-chaos-doc.py`）的 35 个 scenario 索引表（name / markers / WS refs）。
- `docs/business/chaos/runbook-failures.md`：当 CI chaos-smoke FAIL 时，按 scenario 名查的 runbook 段。

**【为什么】** REV-006 §7 #13 runbook 完整性；CLAUDE.md 文档 sync 要求。

**【做什么】**
- README 包含：架构图（小）、scenario YAML 字段说明、injector / invariant kind 列表（自动从代码导出）、扩展指南。
- scenarios.md：跑 `python scripts/gen-chaos-doc.py > docs/business/chaos/scenarios.md`。
- runbook：每 scenario 1 段：失败常见原因、排查命令、relevant log query、escalation owner。

**【不做什么】** ❌ 不写「成功后做什么」，只写失败 runbook（成功路径不需文档）。

**【如何验证】**
1. `ls docs/business/chaos/{README.md,scenarios.md,runbook-failures.md}` 全存在。
2. `scripts/gen-chaos-doc.py` idempotent：连跑 2 次 git diff 空。
3. README 内 yaml 字段全集与 `scenario.schema.json` 字段对得上（grep 对比）。

**【依赖与并行】** 阻塞于 T01 + 所有 scenario 任务（拿到完整列表才能生成 doc）。

---

## §4. PASS/FAIL JSON Schema（`.chaos_report/<name>.json`）

```json
{
  "$schema": "...",
  "type": "object",
  "required": ["name", "status", "started_at", "ended_at", "markers", "invariants"],
  "properties": {
    "name": {"type": "string"},
    "status": {"enum": ["PASS", "FAIL", "ERROR"]},
    "started_at": {"type": "number"},
    "ended_at": {"type": "number"},
    "markers": {"type": "array", "items": {"type": "string"}},
    "invariants": {
      "type": "array",
      "items": {
        "type": "object",
        "required": ["kind", "ok"],
        "properties": {
          "kind": {"type": "string"},
          "ok": {"type": "boolean"},
          "detail": {}
        }
      }
    },
    "errors": {"type": "array", "items": {"type": "string"}},
    "metrics_snapshot": {"type": "object"},
    "compose_overrides": {"type": "array", "items": {"type": "string"}},
    "references": {"type": "object"}
  }
}
```

聚合报告 `.chaos_report/_summary.json` 由 `chaos-run.sh` 写出，含 `total / pass / fail / error` 与每 scenario 状态。

---

## §5. INVESTIGATE 列表（执行前必须闭合或挂起）

| ID | 调查项 |
|---|---|
| **INVESTIGATE-CHAOS-001** | Toxiproxy 容器在 docker-compose 内能否 1 行启动 + Python client 注入 latency/drop；不行就降级到 iptables/tc。owner: T04。 |
| **INVESTIGATE-CHAOS-002** | Redis Sentinel 在 docker-compose 跨容器网络 + announce-ip 配置能否真选举（已知 docker DNS 与 Sentinel announce-ip 有交互坑）。owner: T02 + T05。 |
| **INVESTIGATE-CHAOS-003** | 生产 Redis 当前部署是 standalone 还是 Cluster？v2 §99 INVESTIGATE-008 联动。不论结果，CROSSSLOT scenario 都跑；但 invariants 内 `expected_min` 取决于当前部署。owner: T19 + WS-10。 |
| **INVESTIGATE-CHAOS-004** | Go 仓 / Rust 仓 chaos 目录是双份完整 mirror 还是 Rust 主 + Go 引用？影响 CI checkout 流程。owner: T01 + WS-12。 |
| **INVESTIGATE-CHAOS-005** | mixed-deploy 的 "old" 版本 SHA 如何固定？建议 launch 前 7 天的 main HEAD。owner: T22 + WS-15。 |
| **INVESTIGATE-CHAOS-006** | `libfaketime` 在 alpine vs debian-slim 基础镜像下行为差异（已知 alpine musl 不兼容 libfaketime stable 版）。owner: T07。 |
| **INVESTIGATE-CHAOS-007** | `pg_stat_activity.query` 在 prepared statement 下是否暴露文本（影响 terminate_mid_op regex 匹配）；若为占位符则改为 application_name 匹配。owner: T04。 |
| **INVESTIGATE-CHAOS-008** | `compose.base.yml` Go web service build path（在 Rust 仓 compose 内引 Go 仓代码）跨仓 build context 是否合法；可能要分 PR：先在 Rust 仓加 placeholder image，CI 跨仓 build 后注入。owner: T02 + WS-12。 |
| **INVESTIGATE-CHAOS-009** | 24h soak 在 CI runner 不现实；考虑 self-hosted runner 或转为 nightly schedule。owner: T28 + WS-10。 |
| **INVESTIGATE-CHAOS-010** | `outbox-processor-race-3replicas` 的 `processed_by_replica` 列：当前 outbox 表是否有这列？若无要 WS-1 / WS-14 加 migration。owner: T26 + WS-1 / WS-14。 |

每条闭合记录入 `docs/issues/2026-06-17-investigates/INVESTIGATE-CHAOS-<NN>.md`（v2 §99 同款）。

---

## §6. DoD（本 WS 闭合标准）

1. §3 所有 30 个 task 全 `completed`。
2. `make chaos-smoke` 本地 + CI 全 PASS（35 个 scenario 中 marker=smoke 子集）。
3. `make chaos-full` 在 self-hosted 长跑 PASS（35 个 scenario 全集，含 24h soak）。
4. `.chaos_report/_summary.json` 写出且 `fail==0, error==0`。
5. `scripts/check-chaos-dod-refs.sh` 双向引用 lint PASS。
6. §0.4 stable name 表与 `tests/e2e/chaos/scenarios/` 文件名严格一致；CI lint 阻断遗漏。
7. INVESTIGATE-CHAOS-* 全部闭合或挂起（挂起需 v2 §99 同款记录）。
8. 文档（README / scenarios / runbook）生成且 sync rule 通过 `make check`。
9. 已对本子计划做一轮对抗复审（派 sonnet 对每 scenario 检：是否真测了声称的 root cause；不变式是否抓得住 sin 反例）。

---

## §7. 对抗复审清单（D10 复审日聚焦本 WS）

- **F1**：每个 scenario 的 invariants 是否能抓 v1 / v2 列出的具体反例？随机抽 5 个，注入合成 sin（即时改业务代码引入 bug），跑 scenario 应 FAIL；FAIL 抓 = OK，FAIL 不抓 = invariant 弱。
- **F2**：`pingora-sigkill-midflight` 在 SIGKILL 之前与之后均必须有请求 in-flight；否则 scenario 退化为 "kill idle service"。
- **F3**：1000 并发 Deduct 是否真到 PG 侧并发？通过 `pg_stat_activity` 同时 active count 抽样验证；若被客户端 connection pool 瓶颈卡到 20，整 scenario 无效。
- **F4**：`mixed-deploy-go-old-rust-new` 的 `expect_pre_launch_block` 真验拦截 — 故意 disable lint 跑一次，scenario 应 FAIL（说明 lint 真的拦着）。
- **F5**：所有不变式默认开 race-detector / sanitizer？harness 跑 Python 测试侧；Rust/Go 业务侧应在 `make e2e-race`（WS-13 不负责，但 references 字段引用）。

---

## §8. 与其他 WS 的契约接口

- **WS-1 / WS-2 / WS-5 / WS-6 / WS-8**：各自 sub-plan 的 DoD 段必含字段 `chaos_scenarios: [<stable_name>, ...]`；该字段被 `scripts/check-chaos-dod-refs.sh` 反向验证。
- **WS-12**：把 `check-chaos-dod-refs.sh` 加进 `make check`（WS-12 编写 lint 框架，本计划提供脚本）。
- **WS-14**：本计划要求 `outbox-processor-race-3replicas` 验证 `payment_outbox` 行级 SKIP LOCKED；若 outbox 表 schema 需新列（如 processed_by_replica，验证用），WS-14 在 migration 编号表 §5 内分配。
- **WS-15**：本计划 `license-warn-to-enforce-flip` 与 `feature-flag-rollback-jwt-strict` scenario 调 control-plane flag API；WS-15 必须提供该 API 与对应 flag spec。
- **WS-10**：本计划暴露 metric `gw_chaos_scenario_pass_total{scenario}`，由 WS-10 接入 Grafana。

---

— 文档结束 —
