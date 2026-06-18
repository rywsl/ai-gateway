# WS-14 数据库迁移协调 — TDD 子计划

> **状态**：草案（v2 §5 WS-14 的执行级展开）
> **关联**：v1 `docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §6.1/§6.2；v2 `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §0 重要 1+2、§5 WS-14、REV-003g、REV-006 §7 #11
> **Owner**：Track G（SRE，v2 §6.2 表）
> **工时**：2-3 人日（v2 §5 表 WS-14 行）
> **双仓 mirror**：`AILinkMax-Go/docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md` ⇄ `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md`

---

## 0. 阅读须知（每个执行者必读）

**v2 §0 重要 2 的强制要求在此完全适用**：本计划里的 SQL 草稿、表名、行号声明都是**假设**，必须用 Read/Bash 核对真实源码后再下笔。已有的本计划反证：

- v2 §5 WS-14 表写"WS-1 `00035_idempotency_keys.sql`"，但 `ls AILinkMax-Go/migrations/` 显示 **00035..00066 已被占用**（最后一行 `00066_credit_lease_budget_policies.sql`）。v2 表的编号是**陈旧草案**，本计划重排为 **00067..00073**。
- v2 §5 WS-14 表写"WS-8 Rust `0013_model_pricing_extended.sql` 加列 nullable"，但 `grep "CREATE TABLE" crates/gw-store/migrations/*.sql` 显示 Rust 端真实表名是 **`pricings`**（无 `model_pricing` 表）。本计划改为 `0013_pricings_extended.sql`，并在 §跨仓部分声明 Go 侧对应表为 `pricings_projection`（v1 `00038_pricings_projection.sql` 与 `00048_estimate_pricings_projection.sql` 共同体现）。
- v2 §5 WS-14 表写"WS-9 Rust `0014_billing_indexes.sql`"，与 v1 §SLQ3 "apikey 加覆盖索引" 描述方向相反（apikey 表 Go-side `00006_api_keys.sql` 与 Rust-side `0001_initial.sql` 都有；SLQ3 的实际语义是 Go-side 索引）。本计划将 Rust `0014` 限定为 Rust-side 真正的 billing 热查询（`task_records`/`vendor_keys` 覆盖索引），并新增 Go-side `00073_billing_covering_indexes.sql` 给 SLQ3 落地。

**核对原则**：每个 TDD 任务 §6（HOW）下笔前，先 `ls AILinkMax-Go/migrations/ && ls ailinkmax-gateway/crates/gw-store/migrations/` 重抽最新最大号；与本计划任一编号冲突即就地 +1，并同步更新 §跨仓部署顺序表与 §1.2 编号分配表。冲突日志写入 §99。

---

## 1. 范围与编号分配（REV-003g 落地）

### 1.1 REV-003g 合并禁令

v2 REV-003g：**WS-14 编号分配完成前，任何新 migration PR（含 WS-1/WS-5/WS-8/WS-9 owner 的草稿）禁止合并**。落地手段：

- 在仓库根 `.github/CODEOWNERS` 增加两条临时规则（本计划任务 W14-T00 落地）：
  - `AILinkMax-Go/migrations/* @<track-G-lead>`
  - `ailinkmax-gateway/crates/gw-store/migrations/* @<track-G-lead>`
- 在 CI 增加 `scripts/check-migration-number.sh`（任务 W14-T11 落地），检查 PR diff 内新增的 `migrations/*.sql` 必须出现在本计划 §1.2 表内；不在表内即 fail。
- 任务 W14-T00 完成（CODEOWNERS 合并 + 本计划 §1.2 表锁定）后才能解除其他 WS 的 migration PR 冻结。

### 1.2 编号分配表（**本计划锁定的权威版本**）

> 下表替代 v2 §5 WS-14 草案表。每个 W14-T01..T10 任务对应一行。

| ID | 仓 | 文件 | 表/动作 | 来源 ID | Owner WS |
|---|---|---|---|---|---|
| W14-T01 | Go | `migrations/00067_idempotency_keys.sql` | CREATE TABLE `idempotency_keys` (op,key,response_jsonb,...) | M1 / v1 S1.1 | WS-1 |
| W14-T02 | Go | `migrations/00068_payment_outbox.sql` | CREATE TABLE `payment_outbox` + indexes | M2/G4 / v1 S1.4 | WS-1 |
| W14-T03 | Go | `migrations/00069_refund_outbox.sql` | CREATE TABLE `refund_outbox` + indexes | M3/G2 / v1 S1.7 | WS-1 |
| W14-T04 | Go | `migrations/00070_subscription_upgrade_outbox.sql` | CREATE TABLE `subscription_upgrade_outbox` | REV-FINAL-M10 | WS-1 |
| W14-T05 | Go | `migrations/00071_wallet_outbox_commit_ts.sql` | ALTER `wallet_counter_outbox` ADD `commit_timestamp` | G3 / REV-004a | WS-1 |
| W14-T06 | Go | `migrations/00072_user_subscriptions_quota_check.sql` | ALTER `user_subscriptions` ADD CHECK | G1 | WS-1 |
| W14-T07 | Go | `migrations/00073_billing_covering_indexes.sql` | 覆盖索引（apikey/wallet/billing 热查询） | SLQ3 | WS-9 |
| W14-T08 | Go | `migrations/00074_default_user_group_seed.sql` | seed default group + users.group_id NOT NULL | REV-FINAL-V15 | WS-5 |
| W14-T09 | Rust | `crates/gw-store/migrations/0012_task_freeze_pending.sql` | CREATE TABLE `task_freeze_pending` | R9 | WS-1 |
| W14-T10 | Rust | `crates/gw-store/migrations/0013_pricings_extended.sql` | ALTER `pricings` ADD 新列（nullable） | C1 / REV-FINAL-H8.5 | WS-8 |
| W14-T11 | Rust | `crates/gw-store/migrations/0014_billing_query_indexes.sql` | Rust-side 热查询覆盖索引（`task_records`/`vendor_keys`） | SLQ3-Rust 部分 | WS-9 |
| W14-T12 | Rust | `crates/gw-store/migrations/0015_quota_state_outbox.sql` | CREATE TABLE `quota_state_outbox` | R23 / REV-FINAL-W12 | WS-2 |

> **注**：v2 草案 `00041` (default_user_group_seed) 落到 W14-T08 的 `00074`，因为前 6 个 Go migration 占 00067..00072 + SLQ3 占 00073 + V15 占 00074。

### 1.3 跨仓部署顺序（与 v1 §6.1 D0、REV-003f 一致）

| Phase | 顺序 | 内容 |
|---|---|---|
| D0 | 1 | W14-T00（CODEOWNERS + check-migration-number.sh）合并 |
| D0 | 2 | W14-T01..T10 在各自 WS PR 内并行起草，但**不合并** |
| D1 | 3 | Go T01..T08 合并到 `main`（按编号顺序），同 PR 不部署 |
| D1 | 4 | Rust T09..T11 合并到 `main`，同 PR 不部署 |
| D2 staging | 5 | W14-T12（staging down 验证脚本）跑 up→down→up 闭环 |
| D2 staging | 6 | W14-T13（跨仓部署演练）验证 Rust先部署 → Go后部署 顺序 |
| D3 prod | 7 | 按 REV-003f：**Rust 先部署**（含 T09/T10/T11）→ **Go 后部署**（含 T01..T08）。Go 部署前 readiness gate 必须看到 Rust 已上线 ≥ 10 min |

### 1.4 Rust duplicate `0004_` prefix 处置（per `ailinkmax-gateway/CLAUDE.md` §Toolchain & build pitfalls）

实读 `ls crates/gw-store/migrations/0004*` 显示：

```
0004_capabilities.sql   (Jun 8 16:04)
0004_task_worker.sql    (May 26 14:39)
```

CLAUDE.md 已显式承认这是已存在的 **deliberate divergence**，未来不修。**本计划的 0012/0013/0014 严禁也采用 duplicate prefix**。落地手段：

- 任务 W14-T11 的 `scripts/check-migration-number.sh` 对 Rust 端增强：扫 `crates/gw-store/migrations/`，若同前缀（前 4 位）出现 ≥ 2 个文件，且其中至少一个不在 `{0004_capabilities.sql, 0004_task_worker.sql}` whitelist 内，CI fail。
- whitelist 仅这两个文件硬编码；新增 duplicate 即误用，需另派 PR 改文件名（不在本计划范围）。

---

## 2. 全局 DoD（v2 REV-006 §7 #11 落地）

合并到 main 前每一条都必须达成：

1. **§1.2 表内每个 .sql 文件存在**。Go migration 字面包含 `-- +goose Up` 与 `-- +goose Down`；Rust migration 保持该仓惯例的裸 SQL，并提供同名 `.down.sql` sidecar（HOW 字面要求见各 §6）。
2. **W14-T12 的 down-restore 脚本在 staging snapshot 上闭环通过**：snapshot → up → down → 验 schema 与 snapshot **byte-equivalent**（用 `pg_dump --schema-only` diff，零行差异）。
3. **W14-T11 的 CI gate** 阻断未在 §1.2 表的 migration PR 合并。
4. **W14-T13 跨仓部署演练报告** 写入 `docs/issues/2026-06-17-investigates/WS-14-staging-rehearsal.md`，包含 Rust→Go 顺序的 5 分钟、10 分钟、20 分钟切片下 staging readiness gate 行为日志。
5. **runbook 入口** `docs/runbooks/migrations-2026-06-17.md` 包含每个 .sql 的回滚命令、预期时长、failure mode 三栏（v2 REV-006 §7 #13）。
6. **v2 §0 重要 2 核对记录** 写入 §99：每个 W14-T0N 任务作者签字"已 `Read` cited file:line 并核对一致"。

---

## 3. 任务 W14-T00：REV-003g CODEOWNERS + 合并禁令

**【是什么 What it IS】**
- 一个 4 行的 git PR，修改 `AILinkMax-Go/.github/CODEOWNERS` 与 `ailinkmax-gateway/.github/CODEOWNERS`（若不存在则创建），加入两条 owner 规则把 `migrations/*` 路径绑定到 Track G lead 的 GitHub handle。
- 同 PR 包含本计划 §1.2 编号分配表的 markdown snapshot（注释 `# locked by WS-14 W14-T00, 2026-06-17`），任何 owner 变更必须改本计划同行 + CODEOWNERS。

**【不是什么 What it is NOT】**
- ❌ 不是写 SQL（那是 T01..T11）。
- ❌ 不修改全局 `CODEOWNERS` 其他行（保持现有 module owner 不变）。
- ❌ 不是 GitHub branch protection 规则（branch protection 是仓库管理员动作；本任务只贡献 owner 文件，由 manager 顺手开启 "require code owner review"）。

**【为什么 Why this exists / why bug occurs】**
- 根因（v2 REV-003g）：v1 §6 没有指定 migration owner，导致 8 个新 migration 可由 8 个 author 同时取号 → goose 编号冲突在 git merge 时双胞胎产生（已在历史出现 Rust 端 `0004_capabilities.sql` vs `0004_task_worker.sql` 重号，CLAUDE.md §Toolchain 显式承认）。
- 引用：v2 §2 REV-003g + `ailinkmax-gateway/CLAUDE.md` §Toolchain & build pitfalls 段。

**【做什么 DO】**
1. 在 Go 仓建 `.github/CODEOWNERS`（如已存在 append）：`migrations/ @<track-G-handle>`、`docs/superpowers/plans/2026-06-17-WS-14-* @<track-G-handle>`。
2. 在 Rust 仓建 `.github/CODEOWNERS`（同模式）：`crates/gw-store/migrations/ @<track-G-handle>`、`docs/superpowers/plans/2026-06-17-WS-14-* @<track-G-handle>`。
3. 通知 manager 在两仓 main branch protection 启用 "Require review from Code Owners"。
4. 给 WS-1/5/8/9 owner 群发"Migration freeze lifted after W14-T00 merged"邮件。

**【不做什么 DON'T】**
- ❌ 不要把 owner 写成个人邮箱（GitHub CODEOWNERS 只识别 `@handle` 或 team slug）。
- ❌ 不要把 freeze 期间已存在的 draft PR 强行 merge "因为它已经按编号写好"——必须等本任务 + §1.2 锁定。
- ❌ 不要假定 manager 一定会启 branch protection；若 24h 内未启，再次催促并在 §99 记 INVESTIGATE。

**【怎么做 HOW (具体)】**
- 字面命令（在 `AILinkMax-Go/` 仓内）：
  ```bash
  mkdir -p .github
  printf 'migrations/ @<track-G-handle>\ndocs/superpowers/plans/2026-06-17-WS-14-* @<track-G-handle>\n' >> .github/CODEOWNERS
  git add .github/CODEOWNERS docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md
  git commit -m "WS-14 W14-T00: lock migrations/ to Track G + plan snapshot"
  ```
- Rust 仓同步骤，路径换成 `crates/gw-store/migrations/`。
- `<track-G-handle>` 字面替换为 Track G lead 的 GitHub handle（v2 §6.2 表"Track G"行人选；若未指派，先用 `@<repo-admins-team>` 占位，并立 INVESTIGATE）。

**【如何验证 VERIFY】**
- `gh pr view --json reviewRequests` 在两仓 W14-T00 PR 上展示 Code Owner 自动 reviewer 被加入。
- 在两仓各开一个故意修改 `migrations/<任意>.sql` 的 draft PR，GitHub UI 出现 "Code owner review required" 红 X。
- 合并 W14-T00 后跑 `bash -n scripts/check-migration-number.sh`（T11 落地前可暂用 dummy 占位），无语法错误。

---

## 4. 任务 W14-T01：Go `00067_idempotency_keys.sql`

**【是什么 What it IS】**
- 单文件 `AILinkMax-Go/migrations/00067_idempotency_keys.sql`，包含 `-- +goose Up` + `-- +goose Down` 两段。
- Up 段建表 `idempotency_keys` (定义来自 v1 §6 S1.1 行 207-218 草稿)。Down 段反向 DROP TABLE。

**【不是什么 What it is NOT】**
- ❌ 不修改 `modules/wallet/business/idem.go` / `idem_pg.go`（这是 WS-1 Track A 的 T01-T02）。
- ❌ 不写 partition 分月策略（v2 §5 WS-14 表注释 "partitioned by month" 是建议；落地需 WS-1 owner 评估 row volume 后再加，否则 partition 维护成本 > 收益。本任务建普通表，partition 后续单独 migration）。
- ❌ 不复用 `00037_users_apikeys_payload_triggers.sql` 的 trigger（idempotency 是写一次只读，不要 trigger）。

**【为什么 Why this exists / why bug occurs】**
- 根因（v1 §6 M1+M2）：原 Redis SETNX 幂等键在 wallet tx rollback 后**不能回滚**，留下"幻读已扣"键 → 二次请求 short-circuit 返成功 0 扣费。
- 引用：v1 §6 line 188-189（M1/M2 修订）+ v2 REV-FINAL-M1。

**【做什么 DO】**
1. Read `AILinkMax-Go/migrations/00066_credit_lease_budget_policies.sql` 抄 goose 头部格式（line 1 是 `-- +goose Up`，无 transaction directive 因为单事务安全）。
2. Read `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` line 207-219 取 schema 草稿。
3. 落到字面 SQL（见 §6 HOW）。
4. 同 PR 不修改 `idem.go`（那是 WS-1 PR；本 migration PR 只引入 schema）。

**【不做什么 DON'T】**
- ❌ 不要 `CREATE TABLE IF NOT EXISTS` 后跟 `CREATE INDEX IF NOT EXISTS` 而 down 段只 DROP TABLE——index 跟 table 一起没。但若分两个 migration（不在本任务），down 必须分两段。
- ❌ 不要在 Down 段 `DROP TABLE idempotency_keys CASCADE`（CASCADE 会连锁删 FK 引用，破坏 down byte-equivalence 验证）。
- ❌ 不要默认 `status` 列允许 'pending' 值（v1 S1.1 line 213 显式 `CHECK (status IN ('committed'))`，意为只在 InTx commit 后写入；'pending' 是设计错误）。

**【怎么做 HOW (具体)】**
- 文件 `AILinkMax-Go/migrations/00067_idempotency_keys.sql` 完整内容：
  ```sql
  -- +goose Up
  CREATE TABLE idempotency_keys (
      op            text         NOT NULL,
      key           text         NOT NULL,
      status        text         NOT NULL CHECK (status IN ('committed')),
      response_jsonb jsonb       NOT NULL,
      created_at    timestamptz  NOT NULL DEFAULT now(),
      PRIMARY KEY (op, key)
  );
  CREATE INDEX idempotency_keys_created_at_idx ON idempotency_keys (created_at);

  -- +goose Down
  DROP INDEX IF EXISTS idempotency_keys_created_at_idx;
  DROP TABLE IF EXISTS idempotency_keys;
  ```
- 与 v1 草稿差异说明：v1 line 218 写 `CREATE INDEX idempotency_keys_created_at_idx`（无 IF NOT EXISTS）；本任务保留同款（保持与 v1 字面一致便于 audit），但 Down 段 DROP 用 IF EXISTS 防 re-run failure。

**【如何验证 VERIFY】**
- 本地 `make migrate` 跑通无错（`AILinkMax-Go/Makefile` 既有 target）。
- `psql -c "\d idempotency_keys"` 显示 5 列 + PK + 1 index。
- `goose -dir migrations postgres "<url>" down` 完整撤销，`\d idempotency_keys` 报 "Did not find any relation"。
- `pg_dump --schema-only` 前后 diff 仅这 1 表 1 index 出现/消失，无其他扰动。

---

## 5. 任务 W14-T02：Go `00068_payment_outbox.sql`

**【是什么 What it IS】**
- 单文件 `AILinkMax-Go/migrations/00068_payment_outbox.sql`，Up 建表 + 2 index，Down 反向。
- 表 schema 来自 v1 §6 S1.4 line 237-248 草稿。

**【不是什么 What it is NOT】**
- ❌ 不写 `payment_outbox_processor` worker（那是 WS-1 T3 Go 代码任务）。
- ❌ 不改 `modules/payment/business/webhook.go`（WS-1 T3 同 PR）。
- ❌ 不为 `payment_id` 加 FK 引用 `payments(id)`：v1 S1.4 line 239 注释"一笔 payment 一行"，但若 payment 行因 GDPR delete 被 cascade，outbox 就丢失审计。outbox 历史不可与 payment 强绑。

**【为什么 Why this exists / why bug occurs】**
- 根因（M2/G4）：当前 payment.webhook.Receive 在同步路径调 wallet.Credit，webhook 失败 → 客户款已入但 wallet 没记 → 客诉。outbox 解耦后 webhook 只写 outbox，worker 异步 credit + 重试。
- 引用：v1 §6 line 191（M2）+ line 235-248（S1.4 草稿）。

**【做什么 DO】**
1. Read v1 line 237-248 抄 schema。
2. 加 `attempt_count int NOT NULL DEFAULT 0` 字段（v1 S1.6 line 263 提到 `attempt++`，但 v1 S1.4 草稿表里没列 attempt 列——补上，否则 processor 无地方持久化 attempt）。
3. 加 `last_error text NULL`（v1 未显式但 processor failure mode 必须留 oracle）。
4. 落字面 SQL 见 §6。

**【不做什么 DON'T】**
- ❌ 不要把 `payload jsonb` 改 `payload jsonb NOT NULL DEFAULT '{}'::jsonb`——空 payload 是 bug（webhook 必须给完整 payload），DEFAULT 会掩盖。
- ❌ 不要 partial unique on `(payment_id) WHERE status='pending'`——v1 S1.4 line 239 `UNIQUE` 应用全状态（防同 payment_id 二次插入）。

**【怎么做 HOW (具体)】**
- 文件 `AILinkMax-Go/migrations/00068_payment_outbox.sql` 完整内容：
  ```sql
  -- +goose Up
  CREATE TABLE payment_outbox (
      id              BIGSERIAL PRIMARY KEY,
      payment_id      TEXT        NOT NULL UNIQUE,
      payload         JSONB       NOT NULL,
      request_hash    TEXT        NOT NULL,
      status          TEXT        NOT NULL CHECK (status IN ('pending','committed','dead')),
      attempt_count   INT         NOT NULL DEFAULT 0,
      last_error      TEXT,
      next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
      created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
      committed_at    TIMESTAMPTZ
  );
  CREATE INDEX payment_outbox_due_idx
      ON payment_outbox (next_attempt_at, id)
      WHERE status = 'pending';

  -- +goose Down
  DROP INDEX IF EXISTS payment_outbox_due_idx;
  DROP TABLE IF EXISTS payment_outbox;
  ```
- 增量于 v1 草稿：`request_hash` + `attempt_count` + `last_error` + `committed_at` + `dead` 状态（处理 attempt > max 后 hand-off 到人工）；`payment_id` 使用现有 Go 订单 ID 的 `TEXT` 形态。

**【如何验证 VERIFY】**
- `psql -c "\d payment_outbox"` 显示 10 列 + PK + 1 partial index。
- `EXPLAIN SELECT id FROM payment_outbox WHERE status='pending' AND next_attempt_at<=now() LIMIT 100;` 显示 `Index Scan using payment_outbox_due_idx`（非 Seq Scan）。
- Down 验证同 W14-T01。

---

## 6. 任务 W14-T03：Go `00069_refund_outbox.sql`

**【是什么 What it IS】**
- 文件 `AILinkMax-Go/migrations/00069_refund_outbox.sql`。schema 与 W14-T02 同模式，键改 `refund_id`。

**【不是什么 What it is NOT】**
- ❌ 不与 `payment_outbox` 共享表（refund 与 payment 是独立 outbox channel，避免 status 状态机交错）。
- ❌ 不写 Refund.Create 守门逻辑（WS-1 T7 Go 代码）。

**【为什么】**
- 根因（M3/G2）：admin 双批 refund 在不同 tx 都通过 `pending refunds < total_paid` 检查，并发可超退。Refund 也需 outbox，且 Create 前置查 `payment_outbox.status='committed'`。
- 引用：v1 §6 line 192（M3）+ line 266-268（S1.7）。

**【DO】**
1. 抄 W14-T02 schema，键改 `refund_id TEXT NOT NULL UNIQUE`，payload 仍 jsonb。
2. 加 `payment_id TEXT NOT NULL DEFAULT ''`（refund 必须能溯源 payment；默认空串用于历史/手工退款兼容），并加 `refund_outbox_payment_idx` index 服务 Refund.Create 的双查。

**【DON'T】**
- ❌ 不 FK 到 `payment_outbox.payment_id`（同 W14-T02 原因）。
- ❌ 不在本表写 `total_refunded` 累积列（那是 wallet 视图职责）。

**【HOW】**
```sql
-- +goose Up
CREATE TABLE refund_outbox (
    id              BIGSERIAL PRIMARY KEY,
    refund_id       TEXT        NOT NULL UNIQUE,
    payment_id      TEXT        NOT NULL DEFAULT '',
    payload         JSONB       NOT NULL,
    request_hash    TEXT        NOT NULL,
    status          TEXT        NOT NULL CHECK (status IN ('pending','committed','dead')),
    attempt_count   INT         NOT NULL DEFAULT 0,
    last_error      TEXT,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    committed_at    TIMESTAMPTZ
);
CREATE INDEX refund_outbox_due_idx
    ON refund_outbox (next_attempt_at, id)
    WHERE status = 'pending';
CREATE INDEX refund_outbox_payment_idx
    ON refund_outbox (payment_id);

-- +goose Down
DROP INDEX IF EXISTS refund_outbox_payment_idx;
DROP INDEX IF EXISTS refund_outbox_due_idx;
DROP TABLE IF EXISTS refund_outbox;
```

**【VERIFY】**
- `\d refund_outbox` 显示 10 列 + 2 index。
- `EXPLAIN SELECT 1 FROM refund_outbox WHERE payment_id=$1 AND status='committed'` 使用 `refund_outbox_payment_idx`。

---

## 7. 任务 W14-T04：Go `00070_subscription_upgrade_outbox.sql`

**【是什么】**
- 文件 `AILinkMax-Go/migrations/00070_subscription_upgrade_outbox.sql`。表 `subscription_upgrade_outbox` 给 G7（Upgrade 非原子）落地。

**【不是什么】**
- ❌ 不修改 `subscription` 主表 schema（那是已上线，不动）。
- ❌ 不实现 Upgrade reconciler（WS-1 T11）。

**【为什么】**
- 根因（G7）：subscription.Upgrade 先把旧订阅置 expired，再开 Purchase tx；Purchase 失败留孤儿 expired 订阅 + 无新订阅。outbox 让 expire+purchase 在同 tx 写 outbox row，worker 异步执行 purchase 并能重试或 rollback。
- 引用：v1 §3 line 67（G7）+ v2 REV-FINAL-M10。

**【DO】**
1. schema 同 WS-1 T13 outbox 模式，键为 `old_sub_id TEXT NOT NULL UNIQUE REFERENCES user_subscriptions(id)`，payload 保存升级时计算好的 PurchaseInput / prorate / credit 数据。
2. 同 migration 扩展 `user_subscriptions_status_check`，允许中间态 `upgrading`。

**【DON'T】**
- ❌ 不要 FK 到 `subscriptions(id)`（同 W14-T02 原因）。

**【HOW】**
```sql
-- +goose Up
CREATE TABLE subscription_upgrade_outbox (
    id              BIGSERIAL PRIMARY KEY,
    old_sub_id      TEXT        NOT NULL UNIQUE REFERENCES user_subscriptions(id),
    payload         JSONB       NOT NULL,
    status          TEXT        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','committed','failed','dlq')),
    attempt         INTEGER     NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX subscription_upgrade_outbox_due_idx
    ON subscription_upgrade_outbox (next_attempt_at, id)
    WHERE status = 'pending';
ALTER TABLE user_subscriptions DROP CONSTRAINT IF EXISTS user_subscriptions_status_check;
ALTER TABLE user_subscriptions ADD CONSTRAINT user_subscriptions_status_check
    CHECK (status IN ('pending_payment','active','canceled','expired','upgrading'));

-- +goose Down
ALTER TABLE user_subscriptions DROP CONSTRAINT IF EXISTS user_subscriptions_status_check;
ALTER TABLE user_subscriptions ADD CONSTRAINT user_subscriptions_status_check
    CHECK (status IN ('pending_payment','active','canceled','expired'));
DROP INDEX IF EXISTS subscription_upgrade_outbox_due_idx;
DROP TABLE IF EXISTS subscription_upgrade_outbox;
```

**【VERIFY】**
- `\d subscription_upgrade_outbox`、due_idx 同 T02。

---

## 8. 任务 W14-T05：Go `00071_wallet_outbox_commit_ts.sql`

**【是什么】**
- 文件 `AILinkMax-Go/migrations/00071_wallet_outbox_commit_ts.sql`。ALTER 既有表 `wallet_counter_outbox` 加 `commit_timestamp` 列；落地 REV-004a（relay 按该列 ORDER BY，而非 id）。
- 真实表名验证：Read `AILinkMax-Go/migrations/00036_wallet_counter_outbox.sql` 确认表叫 `wallet_counter_outbox`（v2 草案"wallet_outbox"是简写）。

**【不是什么】**
- ❌ 不修改 `modules/wallet/business/counter_relay.go` ORDER BY（WS-1 T8 代码任务）。
- ❌ 不删 `id` 列（id 仍是 PK，commit_timestamp 仅排序使用）。

**【为什么】**
- 根因（G3）：relay 用 `ORDER BY id` 拉记录，但 id 是 bigserial 申请时间，不是 commit 时间；并发 tx 可能 id 小 commit 晚 → relay 先发后者，写陈旧余额到 Redis。
- 引用：v1 §3 line 64（G3）+ v2 REV-004a。

**【DO】**
1. Read `AILinkMax-Go/migrations/00036_wallet_counter_outbox.sql` 全文确认表名 + 列定义（核对 v2 §0 重要 2）。
2. ALTER ADD `commit_timestamp timestamptz NOT NULL DEFAULT clock_timestamp()`（注：DEFAULT 用 `clock_timestamp()` 取插入瞬时，与 `now()` 不同——`now()` 是事务开始时间，但本字段要的是 commit 时刻，**真正的 commit_ts 要 trigger BEFORE INSERT 设 `pg_xact_commit_timestamp(xmin)` 不现实因 commit 尚未发生；所以本字段语义是 "row insert 时刻"，relay 据此 ORDER 已足够近似 commit 顺序**）。
3. 加 index `(commit_timestamp, id)` 服务 relay 的 ORDER BY + LIMIT。
4. **必须先开 `track_commit_timestamp = on`**：Read `AILinkMax-Go/migrations/` 内是否已有 GUC 设置；若没有，在 `docs/runbooks/migrations-2026-06-17.md` 标注 ops 需手动在 postgresql.conf 启用 + restart（不能在 migration 里 ALTER SYSTEM）。

**【DON'T】**
- ❌ 不用 `pg_xact_commit_timestamp(xmin)` 作为列 DEFAULT—— xmin 在 INSERT RETURNING 时是 0，函数返回 NULL。
- ❌ 不删 `id` 列上的 PK。

**【HOW】**
```sql
-- +goose Up
ALTER TABLE wallet_counter_outbox
    ADD COLUMN commit_timestamp timestamptz NOT NULL DEFAULT clock_timestamp();
CREATE INDEX wallet_counter_outbox_commit_ts_idx
    ON wallet_counter_outbox (commit_timestamp, id);

-- +goose Down
DROP INDEX IF EXISTS wallet_counter_outbox_commit_ts_idx;
ALTER TABLE wallet_counter_outbox DROP COLUMN IF EXISTS commit_timestamp;
```

**【VERIFY】**
- `\d wallet_counter_outbox` 显示新列 + 新 index。
- Down 后 `\d wallet_counter_outbox` 不再有列与 index。
- Up 后 INSERT 一行，`SELECT commit_timestamp FROM wallet_counter_outbox ORDER BY id DESC LIMIT 1;` 返回 ≥ tx start time。

---

## 9. 任务 W14-T06：Go `00072_user_subscriptions_quota_check.sql`

**【是什么】**
- 文件 `AILinkMax-Go/migrations/00072_user_subscriptions_quota_check.sql`。给已存在 `user_subscriptions` 表加 CHECK constraint 防 cycle_used > cycle_quota + overage_cap。

**【不是什么】**
- ❌ 不实现 `CheckAndApply`（WS-1 T6 代码任务）。
- ❌ 不修改 `cycle_used_micros`/`cycle_quota_micros`/`overage_cap` 列定义（应已存在；本任务只加 CHECK）。

**【为什么】**
- 根因（G1）：Check + Apply 不在同 tx 也无 row lock，并发可超支。CHECK 是 defense-in-depth 兜底，应用层 race 漏到 DB 时还能 reject。
- 引用：v1 §3 line 62（G1）+ v1 line 272（"CHECK 防守"）。

**【DO】**
1. **先 Read** `AILinkMax-Go/migrations/` 找 `user_subscriptions` 表定义（grep 找 `CREATE TABLE user_subscriptions`），确认三列字面名 + dtype（v2 §0 重要 2）。若发现真实列名不是 `cycle_used_micros` / `cycle_quota_micros` / `overage_cap`，**就地改本任务 §6 字面 SQL**，并在 §99 记修正。
2. ALTER ADD CONSTRAINT。
3. 必须用 `NOT VALID` + 后续 `VALIDATE CONSTRAINT`，否则锁全表（per Postgres ALTER 文档；任务 W14-T13 部署演练验证此点）。

**【DON'T】**
- ❌ 不 inline `VALIDATE`，会持锁全表（这是 ops 灾难）。
- ❌ 不 ALTER 默认值（若现有数据违反，VALIDATE 会失败；本任务 Down 段需考虑此）。

**【HOW】**
```sql
-- +goose Up
ALTER TABLE user_subscriptions
    ADD CONSTRAINT user_subscriptions_quota_check
    CHECK (cycle_used_micros <= cycle_quota_micros + COALESCE(overage_cap, 0))
    NOT VALID;
-- 然后单独 VALIDATE（不持表锁，仅持 ShareUpdateExclusiveLock）
ALTER TABLE user_subscriptions VALIDATE CONSTRAINT user_subscriptions_quota_check;

-- +goose Down
ALTER TABLE user_subscriptions DROP CONSTRAINT IF EXISTS user_subscriptions_quota_check;
```

**【VERIFY】**
- `\d+ user_subscriptions` 显示 Check constraints 段含 `user_subscriptions_quota_check`。
- 故意 `UPDATE user_subscriptions SET cycle_used_micros = cycle_quota_micros + COALESCE(overage_cap,0) + 1 WHERE id=<test>;` 报 `ERROR: 23514: new row for relation "user_subscriptions" violates check constraint`。
- VALIDATE 步在 staging snapshot（含真实生产 row 量）耗时记录到 runbook（v2 REV-006 §7 #13）。

---

## 10. 任务 W14-T07：Go `00073_billing_covering_indexes.sql`

**【是什么】**
- 文件 `AILinkMax-Go/migrations/00073_billing_covering_indexes.sql`。给 v1 SLQ3 行 796 提到的"apikey 加覆盖索引"以及 billing reconciler 热查询加 covering index。

**【不是什么】**
- ❌ 不修改 `api_keys` 表 schema（只加 index）。
- ❌ 不替代 `0014_billing_query_indexes.sql`（Rust 端，W14-T11）——本任务是 Go-side。

**【为什么】**
- 根因（SLQ3）：当前查 apikey 的 `GetApiKeyByHash` / `GetApiKeyByID` 命中 hash/id index 但回表读 `user_id` / `is_active` / `is_locked` / `expires_at` / `signature_secret` 等列；billing 热查询还会按 user/status/time 读 wallet/payment 历史，缺覆盖索引会放大 buffer cache 抖动。
- 引用：v1 §WS-9 line 796。

**【DO】**
1. **先 Read** `AILinkMax-Go/migrations/00062_idx_api_keys_user_standalone_active.sql`、`AILinkMax-Go/migrations/00006_api_keys.sql`、`AILinkMax-Go/migrations/00035_gateway_entity_bridge.sql` 与 `modules/apikey/model/queries/api_keys.sql` 取真实列名（v2 §0 重要 2）。实读：`api_keys` 无 `status` 列，热查询读取 `is_active`/`is_locked`/`expires_at`/`signature_secret`。
2. CONCURRENTLY 建 index（不锁写）。
3. 必须 `-- +goose NO TRANSACTION`（CONCURRENTLY 不能在 tx 内）。
4. Down 同样 CONCURRENTLY DROP。

**【DON'T】**
- ❌ 不要在 `-- +goose Up` 段加 `BEGIN`（与 NO TRANSACTION 矛盾）。
- ❌ 不要 INCLUDE 大列（如 metadata jsonb），index 体积爆。

**【HOW】**
```sql
-- +goose NO TRANSACTION
-- +goose Up
CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_api_keys_hash_covering_active
    ON api_keys (key_hash)
    INCLUDE (id, user_id, is_standalone, is_active, is_locked, expires_at, signature_secret)
    WHERE deleted_at IS NULL;

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_api_keys_id_covering_active
    ON api_keys (id)
    INCLUDE (user_id, key_hash, is_standalone, is_active, is_locked, expires_at, signature_secret)
    WHERE deleted_at IS NULL;

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_wallet_tx_user_created_covering
    ON wallet_transactions (user_id, created_at DESC)
    INCLUDE (category, reason_code, amount_micros, link_type, link_id);

CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_payment_orders_user_status_created_covering
    ON payment_orders (user_id, status, created_at DESC)
    INCLUDE (wallet_id, amount_micros, payment_method, order_type, gateway_order_id);

-- +goose Down
DROP INDEX CONCURRENTLY IF EXISTS idx_payment_orders_user_status_created_covering;
DROP INDEX CONCURRENTLY IF EXISTS idx_wallet_tx_user_created_covering;
DROP INDEX CONCURRENTLY IF EXISTS idx_api_keys_id_covering_active;
DROP INDEX CONCURRENTLY IF EXISTS idx_api_keys_hash_covering_active;
```

**【VERIFY】**
- `EXPLAIN (BUFFERS) SELECT user_id, is_active, is_locked, expires_at FROM api_keys WHERE key_hash=$1 AND deleted_at IS NULL;` 输出含 `Index Only Scan` 且 `Heap Fetches: 0`。
- 本任务 author 必须把 Read 后修正的真实列名写回本计划 §HOW 部分（v2 §0 重要 2 闭环）。

---

## 11. 任务 W14-T08：Go `00074_default_user_group_seed.sql`

**【是什么】**
- 文件 `AILinkMax-Go/migrations/00074_default_user_group_seed.sql`。落地 REV-FINAL-V15：确认/seed 既有默认 user group `ug_default`，并把 `users.group_id` 改 NOT NULL。

**【不是什么】**
- ❌ 不创建 `user_groups` 表（它在 `00014_model_groups.sql` 或 `00031_user_groups_and_preferences.sql` 已存在；W14-T08 author 必须 Read 确认表名）。
- ❌ 不实现 fail-closed `defaultGroupID` 解析（那是 WS-5 Go 代码 S5.x）。

**【为什么】**
- 根因（v2 REV-FINAL-V15）：WS-5 改 user creation 走 group_id NOT NULL fail-closed；如果没有 default group 行存在，首用户创建即失败。必须 seed。
- 引用：v2 §4 line 355（REV-FINAL-V15）。

**【DO】**
1. **先 Read** `AILinkMax-Go/migrations/00031_user_groups_and_preferences.sql` 取真实 `user_groups` 表 schema（v2 §0 重要 2）。实读 schema 为 `id TEXT PRIMARY KEY`, `name TEXT NOT NULL UNIQUE`, `created_at BIGINT NOT NULL`, `updated_at BIGINT NOT NULL`；`00031` 已 seed `id='ug_default', name='Default'`。
2. INSERT seed（UPSERT 用 `ON CONFLICT (id) DO NOTHING`，保持既有 `ug_default` 契约）。
3. `users.group_id TEXT REFERENCES user_groups(id)` 已由 `00031_user_groups_and_preferences.sql` 创建；W14-T08 不再重复 ADD COLUMN。
4. 把现有 `users` 没有 group_id 的行回填 default group id。
5. ALTER 列 SET NOT NULL（必须先回填，否则 NOT NULL 会失败）。

**【DON'T】**
- ❌ 不要把真实 `TEXT` id schema 改成 UUID；既有契约是硬编码 `ug_default`。
- ❌ 不要在同 migration 里 ALTER + SET NOT NULL + 不回填——会失败。
- ❌ Down 段不要 DROP NOT NULL 后还 DROP COLUMN——可能毁前 migration 已加的列。本任务 Down 仅撤销本次的 SET NOT NULL（恢复 NULL），并删除本次 seed 行（如果别的 user 已经引用，删除会失败，但这是预期：down 段在 staging 验证时若发现 dependency，需要 truncate test data 再跑）。

**【HOW】**
```sql
-- +goose Up
-- Read 00031_user_groups_and_preferences.sql 确认 user_groups 表存在并有 TEXT id PK/name UNIQUE/BIGINT timestamps.
INSERT INTO user_groups (
    id, name, description, is_default, allowed_api_formats,
    scheduling_mode, model_group_bindings, rate_limit, created_at, updated_at
)
VALUES (
    'ug_default', 'Default', 'Default user group', TRUE, NULL,
    'cache_affinity', '[]'::jsonb, NULL,
    EXTRACT(EPOCH FROM now())::BIGINT * 1000,
    EXTRACT(EPOCH FROM now())::BIGINT * 1000
)
ON CONFLICT (id) DO NOTHING;

-- 回填 users.group_id（00049_users_default_group_backfill.sql 已为部分行回填，再次兜底）
UPDATE users
   SET group_id = 'ug_default'
 WHERE group_id IS NULL OR btrim(group_id) = '';

ALTER TABLE users ALTER COLUMN group_id SET NOT NULL;

-- +goose Down
ALTER TABLE users ALTER COLUMN group_id DROP NOT NULL;
```

**【VERIFY】**
- `SELECT 1 FROM user_groups WHERE id='ug_default' AND is_default;` 返回 1 行。
- `\d users` 显示 `group_id text NOT NULL`。
- 故意 `INSERT INTO users (...其他必填..., group_id) VALUES (..., NULL);` 报 NOT NULL 违反。
- Down 后 `\d users` 显示 group_id 又可 NULL。

---

## 12. 任务 W14-T09：Rust `0012_task_freeze_pending.sql`

**【是什么】**
- 文件 `ailinkmax-gateway/crates/gw-store/migrations/0012_task_freeze_pending.sql`。建表 `task_freeze_pending`，落地 R9（task_freeze.release 失败 → 永久泄漏 frozen 余额）的 sweeper 数据基础。

**【不是什么】**
- ❌ 不写 `freeze_sweeper.rs`（WS-1 T18 Rust 代码任务）。
- ❌ 不复用 `task_records` 表（task_records 是请求级 audit；本表是只在 release 失败窗口存在的 retry queue，语义独立）。

**【为什么】**
- 根因（R9）：当前 release_task_freeze 失败被 `let _ =` 吞，task_id 又是新 UUID 未持久化，无 sweeper 可回收。需 PG 表持久化"待 release"项，sweeper 定期重试。
- 引用：v1 §3 line 92（R9）+ v1 line 339-341（修订方案）。

**【DO】**
1. **先 Read** `ailinkmax-gateway/crates/gw-store/migrations/0011_proxy_nodes_protocol.sql` 与 `0010_vendor_key_quota_state.sql` 对齐 Rust 端 goose 风格（实读：Rust 端 migration **没有** `-- +goose Up/Down` 标记，是裸 SQL；这是该仓的惯例，本任务对齐——见 §99 已记此发现）。
2. CREATE TABLE 字面 SQL 见 §6。
3. Rust 端 down 脚本怎么处置？——见 §14（W14-T12）：Rust 仓 down 走 separate `0012_task_freeze_pending.down.sql` sidecar 文件，由 W14-T12 的 staging 验证脚本显式调用。

**【DON'T】**
- ❌ 不要加 `-- +goose Up`/`-- +goose Down`（与 Rust 仓既有惯例冲突；CLAUDE.md `Migrations live under crates/gw-store/migrations/ ... are not auto-applied; run them with psql -f`）。
- ❌ 不要 FK 到 `task_records(id)`（task_records 有 TTL；FK 会阻 CASCADE 清理）。

**【HOW】**
- 文件 `ailinkmax-gateway/crates/gw-store/migrations/0012_task_freeze_pending.sql`：
  ```sql
  -- 0012_task_freeze_pending.sql - persistent retry queue for failed
  -- task-freeze releases (R9 fix; WS-1 T18 sweeper consumes this table).
  CREATE TABLE IF NOT EXISTS task_freeze_pending (
      id              bigserial   PRIMARY KEY,
      user_id         text        NOT NULL,
      task_id         uuid        NOT NULL,
      frozen_micro    bigint      NOT NULL CHECK (frozen_micro > 0),
      attempt_count   int         NOT NULL DEFAULT 0,
      last_error      text        NULL,
      next_attempt_at timestamptz NOT NULL DEFAULT now(),
      created_at      timestamptz NOT NULL DEFAULT now(),
      updated_at      timestamptz NOT NULL DEFAULT now()
  );
  CREATE UNIQUE INDEX IF NOT EXISTS task_freeze_pending_task_idx
      ON task_freeze_pending (task_id);
  CREATE INDEX IF NOT EXISTS task_freeze_pending_due_idx
      ON task_freeze_pending (next_attempt_at, id);
  CREATE INDEX IF NOT EXISTS task_freeze_pending_user_task_idx
      ON task_freeze_pending (user_id, task_id);
  ```
- 配套 sidecar 文件 `ailinkmax-gateway/crates/gw-store/migrations/0012_task_freeze_pending.down.sql`：
  ```sql
  DROP INDEX IF EXISTS task_freeze_pending_user_task_idx;
  DROP INDEX IF EXISTS task_freeze_pending_due_idx;
  DROP INDEX IF EXISTS task_freeze_pending_task_idx;
  DROP TABLE IF EXISTS task_freeze_pending;
  ```

**【VERIFY】**
- `psql -f 0012_task_freeze_pending.sql <db>` 通过。
- `\d task_freeze_pending` 显示 9 列 + PK + 3 index。
- `psql -f 0012_task_freeze_pending.down.sql <db>` 完整撤销。
- W14-T11 的 CI gate 接受 Rust 端 `.down.sql` sidecar 模式（vs Go 端的 `-- +goose Down` 段）。

---

## 13. 任务 W14-T10：Rust `0013_pricings_extended.sql`

**【是什么】**
- 文件 `ailinkmax-gateway/crates/gw-store/migrations/0013_pricings_extended.sql`。ALTER `pricings` 表加 8 个新维度价格列（audio_input/audio_output/cache_creation_5m/cache_creation_1h/cache_read/web_search/prediction/reasoning），**全部 nullable**。
- 真实表名验证：v2 §5 草案写 `model_pricing`，但 Rust 仓实表名是 `pricings`（在 `0001_initial.sql`）。本计划已修正。

**【不是什么】**
- ❌ 不实现 settler 见 NULL → 拒结算 + 告警（WS-8 Rust 代码任务）。
- ❌ 不修改 Go-side `00038_pricings_projection.sql`（projection 是 Go 端单独 migration；若需扩列由 WS-8 Go owner 另开 migration，不在本计划）。

**【为什么】**
- 根因（C1 + REV-FINAL-H8.5）：UsageSnapshot 8 个新字段（audio/prediction/cache_tier/web_search）在 Rust IR 已存在但价格表无对应列，settler 算 cost 时漏算。
- 引用：v2 §4 line 405（REV-FINAL-H8.5）+ v1 §3 line 118（C1）。

**【DO】**
1. Read `ailinkmax-gateway/crates/gw-store/migrations/0001_initial.sql` 确认 `pricings` 表存在 + 真实列结构（v2 §0 重要 2）。
2. ALTER ADD 8 列 NUMERIC(18,9) NULL（micros 单价精度）。
3. **新列 NULL** 是 deliberate：v2 REV-FINAL-H8.5 要求 "settler 见 NULL → 拒结算 + 告警"，强制 ops 显式定价。
4. 配套 down sidecar 反向 DROP COLUMN。

**【DON'T】**
- ❌ 不要给新列 DEFAULT 0 —— 会让 settler 静默用 0 价 → 免费请求灾难。
- ❌ 不要在 ALTER 中加 NOT NULL（会让 ALTER 失败因为现有行无值）。

**【HOW】**
- 文件 `ailinkmax-gateway/crates/gw-store/migrations/0013_pricings_extended.sql`：
  ```sql
  -- 0013_pricings_extended.sql - REV-FINAL-H8.5 + C1: add 8 new usage
  -- dimensions to pricings. All NULL by default; settler treats NULL as
  -- "no price defined" and refuses to settle + emits gw_pricing_missing_total.
  -- Backfill is per-model ops responsibility; no automatic migration.
  ALTER TABLE pricings
      ADD COLUMN IF NOT EXISTS audio_input_micros_per_token       numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS audio_output_micros_per_token      numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS cache_creation_5m_micros_per_token numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS cache_creation_1h_micros_per_token numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS cache_read_micros_per_token        numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS web_search_micros_per_call         numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS prediction_micros_per_token        numeric(18,9) NULL,
      ADD COLUMN IF NOT EXISTS reasoning_micros_per_token         numeric(18,9) NULL;
  ```
- 配套 `ailinkmax-gateway/crates/gw-store/migrations/0013_pricings_extended.down.sql`：
  ```sql
  ALTER TABLE pricings
      DROP COLUMN IF EXISTS reasoning_micros_per_token,
      DROP COLUMN IF EXISTS prediction_micros_per_token,
      DROP COLUMN IF EXISTS web_search_micros_per_call,
      DROP COLUMN IF EXISTS cache_read_micros_per_token,
      DROP COLUMN IF EXISTS cache_creation_1h_micros_per_token,
      DROP COLUMN IF EXISTS cache_creation_5m_micros_per_token,
      DROP COLUMN IF EXISTS audio_output_micros_per_token,
      DROP COLUMN IF EXISTS audio_input_micros_per_token;
  ```

**【VERIFY】**
- `\d pricings` 显示 8 个新列且全为 NULL（无 row 有值，直到 ops 显式 UPDATE）。
- INSERT 一个测试 model 不指定新列，settler integration test（WS-8 owner 提供）观察到 `gw_pricing_missing_total` 触发并拒结算。

---

## 14. 任务 W14-T11：Rust `0014_billing_query_indexes.sql`

**【是什么】**
- 文件 `ailinkmax-gateway/crates/gw-store/migrations/0014_billing_query_indexes.sql`。Rust 端 billing 热查询覆盖索引（对应 v2 草案 "billing_indexes"）。
- 与 Go-side W14-T07 协同：Go 索引覆盖 `api_keys`（管理面查询），Rust 索引覆盖 `task_records` / `vendor_keys`（gateway 热路径）。

**【不是什么】**
- ❌ 不修改既有 schema（只加 index）。
- ❌ 不替代 W14-T07（两者并存，覆盖不同表）。

**【为什么】**
- 根因（SLQ3）：billing/reconciler 高 QPS 查询缺覆盖索引。Rust gateway 在 request_filter 内查 vendor_keys / task_records 等 hot path，缺 covering index → buffer cache 抖动 + p99 抖动。
- 引用：v1 §WS-9 line 796 + v2 §5 WS-14 表 0014 行。

**【DO】**
1. **先 Read** `ailinkmax-gateway/crates/gw-store/migrations/0001_initial.sql` 取 `vendor_keys` / `task_records` 真实列名（v2 §0 重要 2）。
2. CREATE INDEX CONCURRENTLY（Rust 端无 goose tx 包装，但 CONCURRENTLY 仍需自己一行一行执行）。
3. 索引选择基于 EXPLAIN（任务 author 在 staging 跑 EXPLAIN 验证；本计划草案给出 2 个候选，author 必须验证后裁定）。

**【DON'T】**
- ❌ 不要不带 CONCURRENTLY（生产表加 index 锁写不可接受）。
- ❌ 不要 IF NOT EXISTS + CONCURRENTLY 在 transaction 内（CONCURRENTLY 自身不可在 tx；裸 psql -f 满足条件因为 Rust 端无 goose 包装 tx）。

**【HOW】**
- 文件 `ailinkmax-gateway/crates/gw-store/migrations/0014_billing_query_indexes.sql`：
  ```sql
  -- 0014_billing_query_indexes.sql - SLQ3 Rust portion. Covering indexes for
  -- gateway billing and routing hot-path lookups.
  CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_vendor_keys_active_lookup
      ON vendor_keys (id)
      INCLUDE (version, quota_availability, quota_reset_at, quota_remaining, quota_limit)
      WHERE deleted_at IS NULL;
  CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_task_records_user_status_created
      ON task_records (user_id, status, created_at DESC)
      INCLUDE (task_id, api_key_id, frozen_micro, actual_micro, settled_at);
  CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_task_records_api_key_status_created
      ON task_records (api_key_id, status, created_at DESC)
      INCLUDE (task_id, user_id, frozen_micro, actual_micro, settled_at);
  ```
- 配套 `ailinkmax-gateway/crates/gw-store/migrations/0014_billing_query_indexes.down.sql`：
  ```sql
  DROP INDEX CONCURRENTLY IF EXISTS idx_task_records_api_key_status_created;
  DROP INDEX CONCURRENTLY IF EXISTS idx_task_records_user_status_created;
  DROP INDEX CONCURRENTLY IF EXISTS idx_vendor_keys_active_lookup;
  ```

**【VERIFY】**
- author 在 staging 跑 `EXPLAIN (BUFFERS) <hot query>`，给本计划 §99 补一行实测的 plan + 选择理由。
- `gw_billing_query_p99_seconds`（若已暴露；否则 WS-10 owner 补 metric）在新 index 上线后下降。

---

## 15. 任务 W14-T12：Down-restore staging 自动验证

**【是什么】**
- 一个新 bash 脚本 `AILinkMax-Go/scripts/verify-migrations-roundtrip.sh`（mirror 到 `ailinkmax-gateway/scripts/verify-migrations-roundtrip.sh`，以及 `.ps1` 对应版本，per Go CLAUDE.md "scripts/*.sh 或 Makefile 的逻辑/参数/target 同步对应 scripts/*.ps1"）。
- 在 staging snapshot DB 上跑 up → down → up，比对 `pg_dump --schema-only` 输出 byte-equivalent。
- 落地 v2 REV-006 §7 #11（"所有 DB migration 必有 down 脚本，且 down 脚本在 staging snapshot 上验证可还原至 byte-equivalent"）。

**【不是什么】**
- ❌ 不验证数据（行内容）；仅验证 schema 结构。
- ❌ 不在生产跑；只在 staging snapshot 上跑。

**【为什么】**
- 根因：v1 §7 缺验收门"down 验证"。生产 migration 一旦回滚失败，DBA 只能手动恢复，RTO 不可控。

**【DO】**
1. 脚本 take 一个 staging snapshot（pg_restore 到一个临时 DB）。
2. 在该 DB 上 `goose -dir migrations up`（Go 端）或 `psql -f *.sql`（Rust 端，按 §1.2 表 W14-T01..T11 顺序）。
3. `pg_dump --schema-only` 存为 `up.sql`。
4. 反向：`goose down` 跑回所有 up 的 migration；Rust 端 sidecar `.down.sql` 反序执行。
5. `pg_dump --schema-only` 存为 `down.sql`。
6. 再 up，得 `up2.sql`，与 `up.sql` `diff` 应零差异。
7. `down.sql` 与 baseline snapshot 的 schema dump diff 应零差异。

**【DON'T】**
- ❌ 不直接 diff `pg_dump`（含 timestamp/oid 等噪声）；先 `pg_dump --schema-only --no-owner --no-privileges --no-comments`，再 `sed` 去掉残留 oid 行（或用 `--exclude-table-data` + `--no-publications` + `--no-subscriptions`）。
- ❌ 不在生产 DB 跑（脚本第一行 `if [[ "${DATABASE_URL:-}" == *prod* ]]; then exit 1; fi` 防误操作）。

**【HOW】**
- 文件 `AILinkMax-Go/scripts/verify-migrations-roundtrip.sh`（节选骨架，author 落完整逻辑）：
  ```bash
  #!/usr/bin/env bash
  set -euo pipefail
  SNAPSHOT="${1:?usage: $0 <staging-snapshot-dump>}"
  TARGET="${2:?usage: $0 <snapshot> <db-url>}"
  case "$TARGET" in *prod*) echo "refuse to run on prod" >&2; exit 1;; esac
  WORK=$(mktemp -d)
  trap 'rm -rf "$WORK"' EXIT
  pg_restore --clean --if-exists --no-owner --no-privileges -d "$TARGET" "$SNAPSHOT"
  pg_dump --schema-only --no-owner --no-privileges --no-comments -d "$TARGET" > "$WORK/baseline.sql"
  goose -dir migrations postgres "$TARGET" up
  pg_dump --schema-only --no-owner --no-privileges --no-comments -d "$TARGET" > "$WORK/up.sql"
  goose -dir migrations postgres "$TARGET" down-to 66  # before W14-T01 (00067)
  pg_dump --schema-only --no-owner --no-privileges --no-comments -d "$TARGET" > "$WORK/down.sql"
  diff -u "$WORK/baseline.sql" "$WORK/down.sql" || { echo "down restore not byte-equivalent" >&2; exit 1; }
  goose -dir migrations postgres "$TARGET" up
  pg_dump --schema-only --no-owner --no-privileges --no-comments -d "$TARGET" > "$WORK/up2.sql"
  diff -u "$WORK/up.sql" "$WORK/up2.sql" || { echo "up2 not byte-equivalent to up" >&2; exit 1; }
  echo "PASS: schema roundtrip byte-equivalent"
  ```
- Rust 端版本同模式，但用 `psql -f` 按文件名顺序应用 + 反序应用 `.down.sql`。
- `.ps1` mirror 见 `AILinkMax-Go/CLAUDE.md` 双套脚本规则。

**【VERIFY】**
- 在 staging 跑 `scripts/verify-migrations-roundtrip.sh <fresh-prod-snapshot> postgres://...staging...`，最后一行 print `PASS`。
- 把脚本写入 CI（W14-T13 协调），每个 PR diff 涉及 `migrations/` 时触发。
- 故意删一个 Down 段（如把 W14-T01 的 `DROP TABLE` 注释掉）应让脚本 fail（fail-injection 测试）。

---

## 16. 任务 W14-T13：跨仓部署顺序协调与 CI gate

**【是什么】**
- 两件事的组合任务：
  1. CI gate 脚本 `scripts/check-migration-number.sh`（两仓共享，per Go CLAUDE.md 双套脚本规则）。
  2. staging 跨仓部署演练 + 报告。

**【不是什么】**
- ❌ 不执行 prod 部署（那是 §1.3 D3 phase，独立 PR）。
- ❌ 不创建新的 deploy 工具（用既有 CI pipeline）。

**【为什么】**
- 根因（REV-003f）：Rust + Go 部署顺序约束（Rust 先，Go 后；间隔 ≥ 10 min）若错乱，触发 mixed-version 不变式破坏（C4 等）。需 staging 演练验证。
- CI gate 防 §1.2 表外的 migration 偷进 main。

**【DO】**
1. 写 `scripts/check-migration-number.sh`：
   - 扫 git diff 找 `migrations/*.sql` 新增；
   - 把文件名前缀（Go 5 位 / Rust 4 位）与本计划 §1.2 表的允许列表比对；
   - 不在表内即 exit 1，print "WS-14 §1.2 not updated; see docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md"。
   - 同时检查 Rust 端 duplicate prefix（§1.4 whitelist）。
2. 加入 `.github/workflows/<existing>.yml` 的 `make check` 链路。
3. 在 staging 执行演练：
   - T = 0：staging 部署 Rust HEAD（含 0012/0013/0014）。
   - T = 5 min：Go 部署 readiness check 必须看到 Rust 已上线（用 Rust `/healthz/migrations` endpoint，或 WS-8 owner 暴露 schema_version metric）。若 < 10 min，gate 拒（即使 staging 也演练此 gate）。
   - T = 10 min：Go 部署（含 00067..00074）。
   - 全程录 Prometheus 截图（`gw_codec_cross_render_total`、`wallet_outbox` lag、`gw_pricing_missing_total`）。
4. 报告写入 `docs/issues/2026-06-17-investigates/WS-14-staging-rehearsal.md`。

**【DON'T】**
- ❌ 不要在 staging 演练里跳过 5 min / 10 min 等待（演练就是要验证 gate；跳过等于没演练）。
- ❌ 不要在 CI gate 里硬编码 owner handle（owner 与 §1.2 表分离）。

**【HOW】**
- `scripts/check-migration-number.sh` 字面：
  ```bash
  #!/usr/bin/env bash
  set -euo pipefail
  BASE="${BASE_REF:-origin/main}"
  PLAN="docs/superpowers/plans/2026-06-17-WS-14-migration-coordination.md"
  test -f "$PLAN" || { echo "missing $PLAN" >&2; exit 1; }
  ALLOW_GO=$(grep -oE 'migrations/[0-9]{5}_[a-z_]+\.sql' "$PLAN" | sort -u)
  ALLOW_RUST=$(grep -oE 'crates/gw-store/migrations/[0-9]{4}_[a-z_]+\.sql' "$PLAN" | sort -u)
  CHANGED=$(git diff --name-only --diff-filter=A "$BASE"...HEAD | grep -E '(^migrations/|crates/gw-store/migrations/).*\.sql$' || true)
  for f in $CHANGED; do
    case "$f" in
      migrations/*)
        echo "$ALLOW_GO" | grep -qFx "$f" || { echo "FAIL: $f not listed in WS-14 §1.2" >&2; exit 1; } ;;
      crates/gw-store/migrations/*)
        echo "$ALLOW_RUST" | grep -qFx "$f" || { echo "FAIL: $f not listed in WS-14 §1.2" >&2; exit 1; } ;;
    esac
  done
  # Rust duplicate-prefix guard (§1.4)
  if [ -d crates/gw-store/migrations ]; then
    DUP=$(ls crates/gw-store/migrations/*.sql 2>/dev/null | xargs -n1 basename | awk '{print substr($0,1,4)}' | sort | uniq -d)
    for p in $DUP; do
      LIST=$(ls crates/gw-store/migrations/${p}_*.sql)
      WHITELIST="0004_capabilities.sql 0004_task_worker.sql"
      for f in $LIST; do
        bn=$(basename "$f")
        case " $WHITELIST " in *" $bn "*) continue;; esac
        echo "FAIL: Rust duplicate migration prefix $p outside whitelist" >&2; exit 1
      done
    done
  fi
  echo "PASS: migrations match WS-14 §1.2"
  ```

**【VERIFY】**
- CI gate：在两仓各开一个故意把 `migrations/00099_evil.sql` 加进去的 PR，CI fail。
- staging 报告 `docs/issues/2026-06-17-investigates/WS-14-staging-rehearsal.md` 存在且 review by Track G + WS-1 owner。

---

## 99. INVESTIGATE / 修正记录（v2 §0 重要 2 闭环）

每个 W14-T0N 任务作者在落地时把如下表新行写入此节：

| 时间 | 任务 | 发现 | 处置 |
|---|---|---|---|
| 2026-06-17 | 计划起草 | v2 §5 WS-14 表写 00035..00041，但 `ls AILinkMax-Go/migrations/` 显示 00035..00066 已被占用 | 重新分配为 00067..00074（见 §1.2） |
| 2026-06-17 | 计划起草 | v2 §5 WS-14 表写"model_pricing"，但 Rust 仓实表名是 `pricings` | W14-T10 改用 `0013_pricings_extended.sql` |
| 2026-06-17 | 计划起草 | v2 §5 WS-14 把"billing_indexes"放 Rust 0014，但 v1 SLQ3 描述指 Go 端 apikey 索引 | 拆为 Go-side W14-T07 `00073_billing_covering_indexes.sql` + Rust-side W14-T11 `0014_billing_query_indexes.sql` |
| 2026-06-17 | 计划起草 | Rust 端 migrations 实读无 `-- +goose Up/Down` 标记，是裸 SQL（per ailinkmax-gateway CLAUDE.md：not auto-applied, run with `psql -f`） | Rust 端 down 走 `.down.sql` sidecar，W14-T12 脚本反序执行 |
| 2026-06-17 | 计划起草 | Rust 已有 duplicate `0004_capabilities.sql` + `0004_task_worker.sql`（CLAUDE.md 显式承认） | W14-T13 CI gate 包含 whitelist；新增不得复制此模式 |
| 2026-06-17 | W2-T23 | WS-2 要求 `quota_state_outbox`，但 §1.2 Rust migration 编号表未分配该表 | 追加 W14-T12 `0015_quota_state_outbox.sql` + `.down.sql` sidecar |
| 2026-06-18 | W14-T07 | `api_keys` 真实列名为 `key_hash`, `user_id`, `is_standalone`, `is_active`, `is_locked`, `expires_at`, `signature_secret`, `deleted_at`；无 `status` 列 | `00073_billing_covering_indexes.sql` 改为 hash/id covering + wallet/payment billing covering indexes |
| 2026-06-17 | W14-T08 | user_groups 真实 schema = `id TEXT PRIMARY KEY`, `name TEXT NOT NULL UNIQUE`, `created_at/updated_at BIGINT`; existing seed id/name = `ug_default`/`Default`; users.group_id already TEXT from 00031 | `00074_default_user_group_seed.sql` keeps `ug_default`, backfills NULL/blank, and only drops NOT NULL in Down |
| 2026-06-18 | W14-T09 | `TaskId` 是 UUID newtype，`UserId` 是 String newtype；`task_records.user_id` 已是 TEXT | `0012_task_freeze_pending.sql` 使用 `task_id UUID`, `user_id TEXT`, `BIGSERIAL id`，避免新增 `gen_random_uuid()` 扩展依赖 |
| 2026-06-18 | W14-T11 | `vendor_keys` 基础实体表无 `vendor_id`/`key_id` 列；查询主键为 `id`，quota 列来自 `0010_vendor_key_quota_state.sql`; `task_records` 来自 `0004_task_worker.sql` | `0014_billing_query_indexes.sql` 使用 `vendor_keys(id) INCLUDE (...)` 与 `task_records(user_id/api_key_id,status,created_at)` 覆盖索引；staging EXPLAIN 仍需在真实数据集上补证据 |
| 2026-06-18 | W14-T00 | Track G GitHub handle 未在计划里给出；本地 CODEOWNERS 先绑定当前仓库 owner `@whereslow` | manager 若另有 Track G team slug，需后续替换 CODEOWNERS 同行 |

**关联 INVESTIGATE-011（v2 §99）**：本计划全部 W14-T0N 任务都涉及 file:line 声明核对，每个 author 必须在 PR description 写"已 Read 以下 file:line 并核对一致：[列表]"。

---

— WS-14 TDD 子计划结束 —
