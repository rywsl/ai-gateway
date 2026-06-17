# WS-16 安全披露 / CVE 流程 — TDD sub-plan

> Parent: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §5 WS-16
> v1 reference: `AILinkMax-Go/docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §7（验收门）
> Drafted: 2026-06-17 · Owner: team-lead (shared with WS-12)
> Effort: 2–3 人日（流程 + 模板 + 内部审计；不含 fix 本身）
> Branch / worktree: 此 WS 的产出 **不是**代码补丁；产出物是 `docs/security/`、`runbook/security/` 下的流程与模板文件 + 一份内审 CSV。所有任务在一个共享分支 `chore/ws-16-security-disclosure` 上落地，再 cherry-pick 到 R1-R4 的 fix-PR。

---

## 0. 阅读须知（必读，禁止跳过）

**强制要求 1**：本 sub-plan 的每个任务条目使用 **8 段强制格式**。缺一段 PR 不予合并：

1. **【是什么 What it IS】** — 一句话定义产出物 + 具体文件路径。
2. **【不是什么 What it is NOT】** — 范围外 + 常见误解。
3. **【为什么 Why】** — 根因 + 原始审计 ID（R1-R4）+ sonnet 发现 ID。
4. **【做什么 DO】** — 顺序排列的动作清单。
5. **【不做什么 DON'T】** — 诱人但错误的方案、反模式。
6. **【怎么做 HOW】** — 字面命令、字面文件路径、字面模板片段。**禁止 "see code"、"appropriate"**。
7. **【如何验证 VERIFY】** — 具体命令 + 期望输出。**禁止 "passes tests"**。
8. **【协同 / Cross-WS】** — 与 WS-15（flag）、WS-5（fix）、WS-11（前端通告）、WS-10（runbook）的对接点 + 同 PR / 跨 PR 顺序。

**强制要求 2（最关键）**：v2 §0 重要 2 — **sonnet 输出（含原始审计 R1-R4 file:line）是假设，不是结论**。在执行任何 Task 之前，已经按下面"§源码核对结果"重读过 R1-R4。**结果如下，本 sub-plan 已据此调整严重度**：

### 源码核对结果（已 Read 过下列文件）

| 原始声明 | 文件 / 行 | 字面代码核对 | 结论 |
|---|---|---|---|
| **R1**：legacy `verify_jwt` 只校验 `exp`、无 iss/aud/nbf | `ailinkmax-gateway/crates/gw-auth/src/jwt.rs` L39-58 | `let mut val = Validation::new(Algorithm::RS256); val.validate_exp = true;` — **仅** `validate_exp = true`，无 iss/aud/nbf 配置 | **VERIFIED**。`crates/gw-proxy/src/filters/request.rs` L289 在 `jwt_validator == None` 时落入此分支 → 默认配置（未部署 `AuthConfig.global`）即走 legacy 路径。**P0 不变** |
| **R2**：HMAC ±300s 仅校验时间戳，无 nonce/jti | `ailinkmax-gateway/crates/gw-auth/src/signature.rs` L127 `if (now - ts).abs() > 300 {…}` | 该函数及周边无 nonce / jti 缓存路径；`SignatureCheckResult::Valid` 唯一拒绝是 timestamp drift。**任何 5 分钟内捕获的合法签名可无限重放** | **VERIFIED P0 不变** |
| **R3**：JWT kid 静默回退 static_pem_rs256，一把密钥被偷 = 全局伪造 | `ailinkmax-gateway/crates/gw-auth/src/jwt.rs` L211-219 | `if let (Some(kid), Some(jwks)) = … chosen = jwks.resolve(kid).await;` → `if chosen.is_none() { chosen = self.cfg.static_pem_rs256.clone(); }`。**无 kid 白名单 / allowlist 交叉校验**；只要 RS256，resolver 返回 None 即回退 static PEM | **VERIFIED P0 不变** |
| **R4**：`X-Forwarded-For` 用未验证值做 rate-limit key，暴力破解可绕过 | `ailinkmax-gateway/crates/gw-proxy/src/filters/early.rs` L240-258 + `crates/gw-gateway/src/config.rs` L355-371 | **已存在 `trust_xff: bool` 配置默认 `false`**；只在 operator 显式开启时才读 XFF。注释明确警告 "public-internet deployment with this flag on lets any client forge the header" | **降级为 P1 footgun**：默认配置安全；P0 仅在 operator 错开启 `trust_xff=true` 且裸暴露公网时存在。**本 WS 仍按 P0 流程走**（外部发现者无从知道你的配置），但内部通告与升级建议要明确"默认配置不受影响" |

**对执行 agent 的硬性要求**：进入任何 Task 前重读上表对应源码段；如再次发现不符（例如 R3 在 fix 落地后已加 kid allowlist），把该 Task 的 severity 与披露窗口在本文件改成 P1 / 7 天，**并保留改动 diff 链接到 PR 描述**。

---

## 1. WS-16 总目标（一句话）

为 R1-R4 这四个上线前发现的 P0 安全缺陷建立一套可重复执行的"发现 → embargo → 内审 → 客户通告 → 协调披露 → CVE → changelog → warn-only 切 enforce"流程；并以 R1-R4 为首批 case 跑完整流程，留下文件化的产出（不只是口头流程）。

---

## 2. 范围定义（明确边界）

**在范围内（IN）：**
- 安全披露流程文档（embargo 长度、决策权、放行条件）
- 客户通告决策矩阵（谁、何时、何渠道、什么内容）
- CVE 申请（**仅当 product 公开**；详见 Task 16.4）
- changelog / 升级建议模板
- 14-day warn-only 模式协同（与 WS-15 feature flag）
- 外部 reporter 协调披露脚本（如有外部发现者）
- 安全公告 Markdown 模板 + 注意事项
- 内部审计：affected customers / 暴露面清单（CSV + 查询脚本）
- R1-R4 各一次完整流程跑通 + 4 份 advisory draft

**不在范围内（OUT）：**
- R1-R4 的代码修复（属 WS-5）
- license enforce 切换（属 WS-15；本 WS 只提供"客户通告"模板）
- 监控告警阈值改动（属 WS-10）
- 公关稿 / 法务声明（除模板外不替业务方决定）

---

## 3. 任务列表（依赖顺序）

```
T16.0  流程框架文件初始化（docs/security/ 目录约定）
T16.1  embargo & 决策矩阵 → docs/security/disclosure-process.md
T16.2  内部审计 — affected customers 清单（R1-R4 各一份 CSV）
T16.3  安全公告模板（Markdown）+ 注意事项
T16.4  CVE 申请决策树（含 "若 product 开源" 分支）
T16.5  外部 reporter coordinated disclosure SOP
T16.6  changelog & 升级建议模板
T16.7  14-day warn-only 协同（与 WS-15 衔接）
T16.8  R1 — legacy verify_jwt 跨租户 完整流程跑通
T16.9  R2 — HMAC 5min replay 完整流程跑通
T16.10 R3 — kid 伪造 / static_pem fallback 完整流程跑通
T16.11 R4 — XFF 绕过 完整流程跑通（按 P1 降级走但保留 P0 模板）
T16.12 launch gate 验收（与 v1 §7 接驳）
```

依赖 DAG：

```
T16.0 ──► T16.1 ──► T16.2 ─┬─► T16.8/9/10/11 ──► T16.12
                          │
                T16.3 ────┼─► T16.8/9/10/11
                          │
                T16.4 ────┤
                T16.5 ────┤
                T16.6 ────┤
                T16.7 ◄── 依赖 WS-15 flag 框架 (REV-004e)
```

---

## 4. 任务细则（8 段格式）

---

### T16.0 — 流程框架文件初始化

1. **【What it IS】** 创建 `AILinkMax-Go/docs/security/` 目录与 `INDEX.md`（mirror 到 `ailinkmax-gateway/docs/security/INDEX.md`），定义本 WS 所有 artifact 的存放规则与命名约定。
2. **【What it is NOT】** 不是"安全策略 / Security Policy"对外文档；不是 SECURITY.md（那是公开仓库面对发现者的文件，由 T16.5 产出）。
3. **【Why】** v1 §7 与 v2 §5 WS-16 均要求"留下文件化产出"。无目录约定 → 每个 PR 各放各处 → audit 不可追溯。
4. **【DO】**
   - 创建两仓 `docs/security/INDEX.md`
   - 创建两仓 `docs/security/advisories/` 子目录（advisory draft 落点）
   - 创建两仓 `docs/security/internal-audit/` 子目录（CSV / 暴露面落点）
   - 在两仓 `README` / `CLAUDE.md` 里加一行索引引用
5. **【DON'T】**
   - DON'T 把 advisory 公开版本直接落在 `docs/security/`；公开版本走 GitHub Security Advisories（GHSA）流程（T16.5）。
   - DON'T 给 `internal-audit/` 加 `.gitignore` 排除 — CSV 内容已是聚合统计，无 PII；但**禁止落客户名、token、email**（命名用 `tenant_id`）。
6. **【HOW】**
   ```
   mkdir -p AILinkMax-Go/docs/security/advisories
   mkdir -p AILinkMax-Go/docs/security/internal-audit
   mkdir -p ailinkmax-gateway/docs/security/advisories
   mkdir -p ailinkmax-gateway/docs/security/internal-audit
   ```
   `INDEX.md` 内容字面如下（两仓相同）：
   ```markdown
   # Security artifacts index
   - disclosure-process.md — embargo & decision matrix (T16.1)
   - advisories/<advisory-id>.md — advisory drafts (T16.3 / T16.8-11)
   - internal-audit/<audit-id>.csv — affected-customer audits (T16.2)
   - cve-decision-tree.md — CVE filing tree (T16.4)
   - external-reporter-sop.md — coordinated disclosure SOP (T16.5)
   - changelog-template.md — release notes template (T16.6)
   - warn-only-protocol.md — 14-day warn-only protocol (T16.7)
   ```
7. **【VERIFY】**
   ```
   ls AILinkMax-Go/docs/security/INDEX.md && \
     ls ailinkmax-gateway/docs/security/INDEX.md && \
     diff AILinkMax-Go/docs/security/INDEX.md ailinkmax-gateway/docs/security/INDEX.md
   ```
   期望 exit 0 且无 diff 输出。
8. **【Cross-WS】** 与 WS-10 runbook 目录约定保持平行（`runbook/` 与 `docs/security/` 同级）；WS-12 lint 可加一条 "`docs/security/INDEX.md` 必须 mirror 双仓"（建议；非阻塞）。

---

### T16.1 — embargo 期 & 决策矩阵

1. **【What it IS】** `docs/security/disclosure-process.md`（双仓）：定义 48h embargo 起算点、可延长条件、放行条件、决策角色与 RACI。
2. **【What it is NOT】** 不定义"如何修 R1-R4"（属 WS-5）；不替法务定义对外口径。
3. **【Why】** v2 §5 WS-16 "走 embargo（48h）" 是一句话规定；缺执行细节会被 sonnet 标为 §0 §7 "禁止含糊词" 违例。
4. **【DO】**
   - 写明 embargo **起算点**：从 R1-R4 fix 的第一个 commit 进入 main / private branch 起算（**不是**从 reporter 通知起算 — 那是 T16.5 外部 reporter 协调披露口径）。
   - 写明 **48h** 默认 + **延长条件**（最多 +72h，由 team-lead + sec-owner 共同书面同意）。
   - 写明 **放行条件**（**必须全部满足**）：(a) fix 已部署到 prod + 14-day warn-only 期已起跑（见 T16.7），(b) 内审清单已发出（T16.2），(c) advisory draft 已 reviewed，(d) CVE 决策已做（T16.4）。
   - 写明 **决策角色 RACI**（Responsible / Accountable / Consulted / Informed）。
5. **【DON'T】**
   - DON'T 让 embargo 无限延长。最长 48h+72h=120h 硬上限；超 → 自动放行并记录 incident。
   - DON'T 把"修好"作为单一放行条件（必须 prod 部署 + warn-only 起跑，否则修了但未生效 = 客户仍受影响）。
   - DON'T 在 embargo 期内向**任何**外部渠道（含支持工单、status page、social media）透露漏洞性质。
6. **【HOW】** `disclosure-process.md` 字面骨架：
   ```markdown
   # Disclosure process
   ## 1. Embargo
   - 起算点：fix 首个 commit landing main / private fix branch（git sha 记录于 advisory）
   - 默认时长：48h
   - 延长：最多 +72h；需 team-lead + sec-owner 书面 approve（GitHub PR review or issue comment）
   - 硬上限：120h；超时自动 publish (no exceptions)

   ## 2. Release gate（全部满足才能解除 embargo）
   - [ ] fix 已部署到 prod 且 ≥1 个完整业务时段稳定运行
   - [ ] 14-day warn-only 已起跑（见 warn-only-protocol.md）
   - [ ] 内审 CSV 已签字（见 internal-audit/<audit-id>.csv）
   - [ ] advisory draft 已 review (advisories/<id>.md 含 ≥2 approvers)
   - [ ] CVE 决策结果已写入 advisory frontmatter

   ## 3. RACI
   | 角色 | 职责 |
   |---|---|
   | Sec-owner (R) | 起草 advisory、决定 CVE、协调 reporter |
   | Team-lead (A) | embargo 延长 approve、放行 approve |
   | WS-5 owner (C) | fix landing 时间预估 |
   | WS-15 owner (C) | warn-only flag 切换时间 |
   | WS-11 owner (I) | 前端 SDK 升级时间窗 |
   | Support (I) | 客户通告渠道执行 |
   ```
7. **【VERIFY】**
   ```
   grep -E "起算点|48h|120h|放行" AILinkMax-Go/docs/security/disclosure-process.md | wc -l
   ```
   期望 ≥ 4 行命中。
8. **【Cross-WS】** "放行条件 (b) 14-day warn-only" 直接依赖 WS-15 REV-004e 的 `[license] enforce` flag 框架；本任务**只写口径**，flag 实现属 WS-15 T15.x。

---

### T16.2 — 内部审计：affected customers 清单

1. **【What it IS】** 4 份 CSV：`internal-audit/R1-affected.csv`、`R2-affected.csv`、`R3-affected.csv`、`R4-affected.csv`，每份列出可能受影响的 tenant_id + 暴露指标 + 时间窗口；以及 1 份产出脚本 `internal-audit/queries.sql` / `queries.sh`（可重跑）。
2. **【What it is NOT】** 不是"通知"本身（T16.3 advisory + 客户邮件外发渠道执行）；不是 PII dump（**禁止落 email/name/token**）。
3. **【Why】** v2 §5 WS-16 第 2 步"通知 affected customers（如有）"。无清单 → 无法精确通告 → 要么全量通告（吓走客户）、要么不通告（违 SLA）。
4. **【DO】**
   - R1（legacy verify_jwt）：查"使用了 RS256 JWT 且 **未部署 AuthConfig.global** 的 tenant"。判定字段：`auth_configs` 表里 tenant 是否有 row → 无 row 即走 legacy。落到 CSV：`tenant_id, first_seen_jwt_ts, last_seen_jwt_ts, request_count_7d`。
   - R2（HMAC 5min replay）：查"启用 HMAC 签名（`required=true`）的 tenant"。判定字段：`signature_secret` 配置存在或 `vendor_keys.signature_required = true`。落到 CSV：`tenant_id, signature_required_since, request_count_7d, distinct_ip_count_7d`。`distinct_ip_count` 高 + token leak suspicion 是优先通告条件。
   - R3（kid fallback）：查"配置了 `static_pem_rs256` 且同时启用了 jwks resolver 的 tenant"（即两者并存 → 静默回退面存在）。判定字段：`auth_configs.static_pem_rs256 IS NOT NULL` AND `auth_configs.jwks_url IS NOT NULL`。落到 CSV：`tenant_id, static_pem_age_days, jwks_url, last_kid_seen`。
   - R4（XFF 绕过）：查"`[security] trust_xff = true` 且 deploy 暴露公网"。判定字段：config snapshot（如有 config audit table）+ deploy mode。**因 R4 已降级为 P1 footgun**，CSV 一栏 `severity_for_this_tenant` 标记 `p0_if_public_internet` / `p1_lb_only`。
   - 全部 4 份 CSV 由 `queries.sql` / `queries.sh` 自动重跑，**不允许手工填**。
5. **【DON'T】**
   - DON'T 把 email / personal-name / api-key / jwt 内容写进 CSV。**只允许 tenant_id（短哈希前 12 字符可接受）+ 聚合计数**。
   - DON'T 用业务数据库直连查询 — 走 read-replica / data warehouse 镜像，避免在 incident 流程下打垮 prod。
   - DON'T 用 LLM 生成 SQL 后不 review 就跑 — 跨租户 join 错一处 = 第二次泄漏。每条 query peer-review。
6. **【HOW】**
   - `queries.sql` 骨架：
     ```sql
     -- R1: tenants without AuthConfig.global → fall to legacy verify_jwt
     SELECT t.tenant_id,
            MIN(j.created_at) AS first_seen_jwt_ts,
            MAX(j.created_at) AS last_seen_jwt_ts,
            COUNT(*) FILTER (WHERE j.created_at > now() - interval '7 days') AS request_count_7d
     FROM tenants t
     LEFT JOIN auth_configs ac ON ac.tenant_id = t.tenant_id
     JOIN jwt_seen j ON j.tenant_id = t.tenant_id
     WHERE ac.tenant_id IS NULL
     GROUP BY t.tenant_id;
     ```
     （如表名不一致，按 `AILinkMax-Go/migrations/` 实际表名替换。**若发现表不存在或字段名异，按 v2 §0 重要 2，更新本 sub-plan 而非硬塞 query**。）
   - 输出文件 header（CSV）：第一行 `# generated by queries.sql at <git-sha> on <UTC ts>`；第二行字段名。
   - 跑法：`psql -A -F"," -f queries.sql > internal-audit/R1-affected.csv`
   - **审阅人**：sec-owner + 1 个非 sec-owner（防单人错）。在 `internal-audit/<id>.csv` 配套 `<id>.review.md` 写明 reviewer + UTC ts。
7. **【VERIFY】**
   ```
   for f in R1 R2 R3 R4; do
     test -f AILinkMax-Go/docs/security/internal-audit/${f}-affected.csv || exit 1
     test -f AILinkMax-Go/docs/security/internal-audit/${f}-affected.review.md || exit 1
     # 字段不允许出现 email / @ 符号 / token-shaped 字符串
     ! grep -E "@|sk-[A-Za-z0-9]{20}|eyJ[A-Za-z0-9_-]+\." AILinkMax-Go/docs/security/internal-audit/${f}-affected.csv
   done
   ```
   期望全部 exit 0。
8. **【Cross-WS】** R3 CSV 的 `static_pem_age_days` 字段直接是 WS-5 R3 fix 验收门的输入（fix 上线时按 age 倒序通告 → 老租户先收）。CSV 字段命名跟 WS-5 fix-PR commit message 引用保持一致。

---

### T16.3 — 安全公告 Markdown 模板

1. **【What it IS】** `docs/security/advisories/_template.md`：一份可 copy 重命名为 `<advisory-id>.md` 的骨架；规定字段、注意事项、必须 / 可选段落。
2. **【What it is NOT】** 不是 CVE 申请文本（CVE Form 由 T16.4 决定）；不是 release notes（T16.6）；不是公关稿。
3. **【Why】** 4 个 advisory（R1-R4）必须风格一致；无模板 → 4 人写 4 风格 → 客户阅读成本高、信息漏配。
4. **【DO】** 模板含以下 frontmatter 与段落（顺序固定）：
   - YAML frontmatter：`id`、`title`、`audit_origin`（R1/R2/R3/R4）、`severity`（P0/P1）、`cvss_v3_1`、`embargo_started_utc`、`embargo_ends_utc`、`disclosure_target_utc`、`cve_id`（待申）、`status`（draft/embargoed/released）
   - § Summary（≤3 句，无技术细节）
   - § Affected versions / configurations
   - § Impact（具体 — 不是"may be affected"，而是"未部署 AuthConfig.global 的 tenant，攻击者可用任意有效 JWT 跨租户访问任意租户资源"）
   - § Workaround（如有；对 R4 是 "set `[security].trust_xff = false` 或验证你的 LB strips client XFF"）
   - § Fix（fix 上线版本号 + git sha + flag 切换日历）
   - § Upgrade guide（升级到 X 版本 + flag 设置；与 T16.6 changelog 双向链接）
   - § Credit（外部 reporter 致谢；若无写 "Internal pre-launch audit"）
   - § Timeline（discovery → embargo → notification → fix → publish 每步带 UTC ts）
5. **【DON'T】**
   - DON'T 在 § Impact 写 "Could potentially be exploited" — 含糊；必须写"何种配置下、攻击者掌握什么、能拿到什么"。
   - DON'T 在 embargo 期内填入"全部 affected customer 列表"原文 — 客户单可让攻击者反向定位目标。链接到内审 CSV 即可。
   - DON'T 把 advisory id 与 CVE id 混淆。advisory id 内部生成（如 `AILMX-2026-001`）；CVE id 是 MITRE/NVD 分配（如 `CVE-2026-XXXXX`）。
   - DON'T 在 embargo 期内将 advisory commit 到 public repo（用 private fork 或 GHSA draft）。
6. **【HOW】** 模板字面骨架（节选）：
   ```markdown
   ---
   id: AILMX-2026-XXX
   title: <ONE-LINE summary, no exploit detail>
   audit_origin: R[1|2|3|4]
   severity: P[0|1]
   cvss_v3_1: <vector>
   embargo_started_utc: 2026-MM-DDTHH:MM:SSZ
   embargo_ends_utc: 2026-MM-DDTHH:MM:SSZ
   disclosure_target_utc: 2026-MM-DDTHH:MM:SSZ
   cve_id: pending|CVE-2026-XXXXX|not-requested
   status: draft|embargoed|released
   ---

   ## Summary
   ## Affected versions / configurations
   ## Impact
   ## Workaround
   ## Fix
   ## Upgrade guide
   ## Credit
   ## Timeline
   ```
   注意事项段（写在模板尾部 HTML comment）：
   ```
   <!--
   - Summary 不超过 3 句、不含 exploit primitive
   - Impact 必须 concrete：何种配置 / 攻击者掌握 / 可得到
   - Workaround 若无写 "None — upgrade required"
   - 不在 advisory 列举 affected customer 名单；链 internal-audit/<id>.csv
   -->
   ```
7. **【VERIFY】**
   ```
   test -f AILinkMax-Go/docs/security/advisories/_template.md
   # 检查必备字段都在
   for k in id title audit_origin severity cvss_v3_1 embargo_started_utc embargo_ends_utc disclosure_target_utc cve_id status; do
     grep -q "^${k}:" AILinkMax-Go/docs/security/advisories/_template.md || { echo "missing $k"; exit 1; }
   done
   ```
   期望全部 exit 0。
8. **【Cross-WS】** § Fix 区段的 "flag 切换日历" 字段直接由 WS-15 REV-004e 提供（`flags/license_enforce.yaml` 的 `target_enforce_date` 字段）；本任务**只规定模板**，flag 由 WS-15 owner 维护。

---

### T16.4 — CVE 申请决策树

1. **【What it IS】** `docs/security/cve-decision-tree.md`：一个流程图 + 字面"是 / 否"问答；判定 R1-R4 各自是否申请 CVE、申请到哪个 CNA（CVE Numbering Authority）。
2. **【What it is NOT】** 不是 CVE 表单本身；不是 NVD 评分计算器（CVSS 3.1 由 advisory 模板填）。
3. **【Why】** v2 §5 WS-16 "CVE 申请（**若 product 公开**）"。AILinkMax 当前 **未完全开源**（部分组件如 `ailinkmax-gateway` 可能私仓 / 部分公开），需逐项判定。
4. **【DO】**
   - 决策树根：**该缺陷影响的代码是否在公开仓库 / 公开发布的版本**？
     - 否 → 不申请 CVE，**改走"内部 advisory + 客户通告"路径**；记录在 advisory `cve_id: not-requested` + 原因（"代码未公开，CVE 无对应 affected product 字段可填"）。
     - 是 → 进入下一步。
   - 第二层：**是否有 reporter（外部发现者）**？
     - 是 → 与 reporter 协商是否申请，**reporter 偏好优先**（部分研究者要求 CVE 用于学术）。
     - 否（内部发现）→ 由 sec-owner 自主决定；R1/R2/R3 严重度 P0 默认申请；R4 P1 footgun 默认不申请（但可申请）。
   - 第三层：CNA 选择 — 通过 **GitHub Security Advisories（GHSA）作为 CNA**（推荐），还是 **MITRE direct submission**？
     - GitHub 仓库 → GHSA（自动获得 GHSA + 申请 CVE）。
     - 非 GitHub / 私有 → MITRE web form。
5. **【DON'T】**
   - DON'T 闭源代码强申 CVE — MITRE 会拒，浪费 embargo 时间。
   - DON'T 用同一个 CVE 覆盖多个根因不同的缺陷。R1/R2/R3/R4 必须各申一个（如果都申）。
   - DON'T 在 CVE 描述里贴 PoC 代码 — 描述限于 impact 与 affected versions。
   - DON'T 在 CVE 公布前透露 CVE id 给非"Need to know"人员。
6. **【HOW】** 决策树字面骨架（mermaid 或 plain ASCII，二选一）：
   ```
   ┌──────────────────────────────┐
   │ R[1|2|3|4] code in public    │
   │ repo / public release ?      │
   └────┬──────────────────┬──────┘
        │ Yes              │ No
        ▼                  ▼
   ┌──────────┐    ┌────────────────────┐
   │ external │    │ skip CVE; advisory │
   │ reporter?│    │ → internal channel │
   └─┬──────┬─┘    └────────────────────┘
     │Yes   │No
     ▼      ▼
   reporter sec-owner
   choice   default:
            P0→file
            P1→optional
   ```
   每一节点旁写明"决策结果落到 advisory frontmatter `cve_id` 的取值"。
7. **【VERIFY】**
   ```
   grep -E "public repo|external reporter|CNA|GHSA|not-requested" AILinkMax-Go/docs/security/cve-decision-tree.md | wc -l
   ```
   期望 ≥ 5 行命中。
8. **【Cross-WS】** 决策结果影响 advisory `cve_id` 字段（T16.3 模板）。**若决策"申请"**：CVE 申请的发起时间必须早于 embargo 结束 48h（CVE 分配通常 1-3 工作日），否则 advisory publish 时 cve_id 仍 `pending`。

---

### T16.5 — 外部 reporter 协调披露 SOP

1. **【What it IS】** `docs/security/external-reporter-sop.md`：当外部研究者通过 security@ 邮箱 / SECURITY.md 报告漏洞时的 SOP（含 SECURITY.md 模板本身）。
2. **【What it is NOT】** 不是法律协议（SLA、奖金、bug bounty 由业务侧另行规定）；R1-R4 已是**内部审计发现**，本任务的产出仅在"未来出现外部 reporter"时启用，但 SOP 仍需此次落成。
3. **【Why】** v2 §5 WS-16 "coordinated disclosure with discoverer (if external)"。无 SOP → reporter 接收渠道不明 → 流入支持工单 / 公开 GitHub issue → embargo 失效。
4. **【DO】**
   - 创建公开 `SECURITY.md`（两仓根目录），指向 security@ 邮箱 + GitHub Security Advisories 提交链接 + 公钥（PGP）。
   - SOP 写明：T+0 收到报告 → T+24h 内回复确认 → T+72h 内初步评估 → T+7 日内修复 ETA → embargo 期与 reporter 协商一致（默认 90 天上限或修好为准）。
   - reporter 致谢字段（写入 advisory § Credit）：在 reporter 同意下公开 handle / 组织。
   - 沟通模板：3 份 — "确认收到"、"评估完成 / 进入 embargo"、"准备公开 / 致谢确认"。
5. **【DON'T】**
   - DON'T 在公开渠道（GitHub issue、support 工单、Twitter DM）讨论未公开漏洞细节。
   - DON'T 单方面缩短或延长 embargo — 与 reporter 必须**书面**确认。
   - DON'T 在未与 reporter 同步前披露其身份或报告内容。
   - DON'T 用免费邮箱（Gmail/QQ）做 security@ 收件 — 用域名邮箱 + 双因素。
6. **【HOW】** `SECURITY.md` 字面骨架：
   ```markdown
   # Security policy
   ## Reporting a vulnerability
   - Email: security@<your-domain>
   - GitHub Security Advisories: <repo>/security/advisories/new
   - PGP key: <fingerprint>; <pubkey-url>

   ## What happens next
   - T+24h: ack
   - T+72h: initial assessment
   - T+7d: fix ETA
   - Embargo: agreed with reporter, default 90 days or fix-released, whichever first
   ```
   3 份沟通模板每份独立 `.md` 文件，置于 `docs/security/templates/reporter-*.md`。
7. **【VERIFY】**
   ```
   test -f AILinkMax-Go/SECURITY.md
   test -f ailinkmax-gateway/SECURITY.md
   diff AILinkMax-Go/SECURITY.md ailinkmax-gateway/SECURITY.md  # 双仓一致
   test -f AILinkMax-Go/docs/security/templates/reporter-ack.md
   test -f AILinkMax-Go/docs/security/templates/reporter-embargo.md
   test -f AILinkMax-Go/docs/security/templates/reporter-disclosure.md
   ```
   期望全部 exit 0 / 无 diff。
8. **【Cross-WS】** SECURITY.md 中"GitHub Security Advisories"链接的可用性依赖 GitHub 仓库是否启用 GHSA（仓库 Settings → Security → Private vulnerability reporting）；与 WS-12 CI 基建协同：CI 加一条 lint，确保 `SECURITY.md` 存在且双仓一致。

---

### T16.6 — changelog & 升级建议模板

1. **【What it IS】** `docs/security/changelog-template.md`：当 R1-R4 fix release 时写入 CHANGELOG.md 的字面段落模板；以及升级建议（"为什么升、升级步骤、回滚"）。
2. **【What it is NOT】** 不替换业务 CHANGELOG.md；只补充"Security"小节模板。
3. **【Why】** v2 §5 WS-16 "补 changelog + 升级建议"。无模板 → release 时 ad-hoc 写法可能漏 advisory 反链、漏 flag 切换说明。
4. **【DO】**
   - 模板含 `## Security` 小节字段：advisory id、CVE id、severity、required action（"upgrade to vX.Y.Z and set flag F"）、breaking changes（有 / 无）、warn-only 期长度。
   - 模板有"升级建议"子段：升级前 prerequisite、升级命令、验证命令、回滚命令。
   - 模板预留"opt-in"vs"opt-out"标注。R1-R3 fix 是**opt-in 默认 enforce**（因 P0），R4 是**opt-in 默认保留 trust_xff=false**。
5. **【DON'T】**
   - DON'T 在 CHANGELOG 里抄 advisory 全文 — 一句话 summary + advisory 链接即可。
   - DON'T 漏写回滚命令。任何强制行为变更必须有回滚（即使是 "downgrade to previous version"）。
   - DON'T 把 flag 默认值变更与 fix landing 同一 release —— **fix landing 是 release N，flag 默认翻转是 release N+1**（与 WS-15 REV-004d 一致）。
6. **【HOW】** 模板字面骨架：
   ```markdown
   ## Security
   - [<advisory-id>] <one-line summary> ([CVE-2026-XXXXX])
     - Severity: P0
     - Required action: upgrade to v<X.Y.Z>, set `[<table>].<flag>` to `enforce` (current default: `warn`)
     - Breaking changes: <list or "none">
     - Warn-only window: 14 days starting <UTC date>
     - Advisory: docs/security/advisories/<id>.md

   ### Upgrade guide
   - Prerequisites: …
   - Upgrade: …
   - Verify: `curl -fsS https://<host>/livez && grep 'gw_auth_legacy_jwt_warn_total' /metrics`
   - Rollback: …
   ```
7. **【VERIFY】**
   ```
   grep -E "advisory-id|Required action|Warn-only window|Rollback" AILinkMax-Go/docs/security/changelog-template.md | wc -l
   ```
   期望 ≥ 4 行命中。
8. **【Cross-WS】** 升级 guide 的 verify 命令依赖 WS-10 暴露的新 metric（如 `gw_auth_legacy_jwt_warn_total`）；本 task 与 WS-10 约定 metric 名（一次性约定，不允许后改）。

---

### T16.7 — 14-day warn-only 协同（与 WS-15 衔接）

1. **【What it IS】** `docs/security/warn-only-protocol.md`：定义 R1-R3 fix 上线后**强制 14 天 warn-only**期的行为、metric、放行条件、回滚条件。
2. **【What it is NOT】** 不是 flag 实现（属 WS-15 REV-004e）；不是 metric 实现（属 WS-10）。
3. **【Why】** v2 §5 WS-16 "部署后强制 14 天 warn-only 模式（与 H7 license 同模式）"。无 protocol → 14 天起算点、warn 计数阈值、强制升 enforce 的条件全部模糊。
4. **【DO】**
   - 起算点：fix release **生产部署稳态**（≥1 个完整业务时段无 P0 incident）后起算 14 天。
   - 行为：fix 上线但 flag 保持 `warn`（R1 jwt_strict=false / R3 kid_allowlist_enforce=false / license enforce=warn 等）。每次违例**仍允许通过**但记 metric。
   - 必须暴露的 metric（具体名以 WS-10 约定为准）：
     - R1：`gw_auth_legacy_jwt_warn_total{tenant_id}` — 走 legacy verify_jwt 路径的请求数。
     - R2：`gw_auth_signature_replay_window_warn_total` — （注意：R2 是 fix=加 nonce/jti 缓存，不是 flag 模式；如有降级 flag 则同步加 metric）
     - R3：`gw_auth_jwt_kid_fallback_warn_total{tenant_id}` — kid resolver miss → static_pem 回退次数。
     - R4：`gw_xff_trust_warn_total` —（footgun，配置开启时计数）
   - 14 天内**每 24h** sec-owner 检查 metric；阈值 = 累计 warn < 1% 总请求 → 切 enforce；> 1% → 延长 14 天 + 通告 owner tenant。
   - 切 enforce 的 PR 必须**单独**提，不与 fix 同 PR；PR 描述写明 metric 历史。
5. **【DON'T】**
   - DON'T 在 fix landing 同时翻 enforce — 防止"修了但客户没升 SDK → 大面积 401"。
   - DON'T 把 warn 计数静默掉（如 sampling 1/100） — 必须精确 metric。
   - DON'T 14 天内不看 metric 就自动切 enforce — 必须有人工 review。
   - DON'T 让 warn-only 无限延长 — 硬上限 60 天；超 60 天 → 重启 incident（说明 fix 与客户使用不兼容，需要重新设计）。
6. **【HOW】** `warn-only-protocol.md` 字面骨架：
   ```markdown
   # Warn-only protocol (R1-R4)
   ## 1. 起算
   - fix release 生产部署稳态后起 14 天

   ## 2. metric（与 WS-10 约定）
   | Audit | Metric | Threshold |
   |---|---|---|
   | R1 | gw_auth_legacy_jwt_warn_total | <1% total |
   | R3 | gw_auth_jwt_kid_fallback_warn_total | <1% total |
   | R4 | gw_xff_trust_warn_total | =0 (footgun) |

   ## 3. 每 24h check
   - Sec-owner 看 metric → 写一条 short note 到 advisory § Timeline

   ## 4. enforce 切换 PR 模板
   - 单 PR，描述附 metric 历史截图 + 7d trend
   - reviewer ≥2，其中 1 个是 WS-15 owner
   ```
7. **【VERIFY】**
   ```
   grep -E "起算|metric|14 天|enforce 切换" AILinkMax-Go/docs/security/warn-only-protocol.md | wc -l
   ```
   期望 ≥ 4 行命中。
8. **【Cross-WS】** 直接耦合 WS-15 REV-004e（flag 实现）+ WS-10（metric 暴露）。本任务**仅规定 protocol**；任何 flag/metric 接口变化必须同步改本文件。

---

### T16.8 — R1 完整流程跑通

1. **【What it IS】** 4 份具体 artifact：
   - `advisories/AILMX-2026-001-R1.md`（用 _template.md 实例化）
   - `internal-audit/R1-affected.csv`（T16.2 输出）
   - changelog 片段（CHANGELOG.md 待 R1 fix release 时合入；本任务先写到 `advisories/AILMX-2026-001-R1.changelog.md`）
   - reporter 沟通记录（R1 是内部发现 → 留 `discovery: internal-pre-launch-audit`，无对外 reporter）
2. **【What it is NOT】** 不是 R1 的代码修复（属 WS-5）；不是 R1 fix release 行为本身。
3. **【Why】** 验证 T16.0-T16.7 流程 + 模板的可用性。先跑通 R1（最经典 P0），再复用到 R2/R3/R4。
4. **【DO】**
   - 实例化 advisory，frontmatter 填：
     - `id: AILMX-2026-001`
     - `audit_origin: R1`
     - `severity: P0`
     - `cvss_v3_1`：参考 — Authentication: None, Scope: Changed (跨租户), Conf/Integ High → AV:N/AC:L/PR:L/UI:N/S:C/C:H/I:H/A:N → CVSS 8.5（具体由 sec-owner 评）
     - `cve_id`：按 T16.4 决策树 — 若 `ailinkmax-gateway` 仓库公开 → 申请；否则 `not-requested`
   - Summary 字面：legacy JWT 验证路径仅校验 `exp` 时间戳，缺 `iss` / `aud` / `nbf`，导致一个 tenant 签发的有效 JWT 可在另一 tenant 通过验证，发起跨租户访问。
   - Impact：未部署 `AuthConfig.global`（即 `auth_configs` 表无对应 row）的 tenant 100% 受影响。
   - Workaround：deploy `AuthConfig.global` 强制走 `JwtValidator` 路径（与 fix 内容一致 — fix 是把 legacy 路径删掉 / 默认禁用）。
   - Fix：vX.Y.Z + git sha；flag `[auth].strict_jwt_validator` 由 false → true（warn-only 14 天后）。
   - 跑 T16.2 的 R1 query 生成 CSV，attach review.md。
5. **【DON'T】**
   - DON'T 在 Summary 写 "exploit by crafting a JWT with sub=victim_tenant" — 给 PoC。写一般化 impact 即可。
   - DON'T 把 `auth_configs` 表 schema 贴进 advisory — 内部细节，写到 internal docs。
   - DON'T 标 P1（已源码确证 P0）。
6. **【HOW】**
   ```
   cp AILinkMax-Go/docs/security/advisories/_template.md \
      AILinkMax-Go/docs/security/advisories/AILMX-2026-001-R1.md
   # 编辑 frontmatter + 各 § 段落，按 §DO 字面填入
   psql -A -F"," -f AILinkMax-Go/docs/security/internal-audit/queries.sql \
        > AILinkMax-Go/docs/security/internal-audit/R1-affected.csv  # 实际跑法以 T16.2 为准
   ```
7. **【VERIFY】**
   ```
   grep -q "^id: AILMX-2026-001$" AILinkMax-Go/docs/security/advisories/AILMX-2026-001-R1.md
   grep -q "^audit_origin: R1$" AILinkMax-Go/docs/security/advisories/AILMX-2026-001-R1.md
   grep -q "^severity: P0$" AILinkMax-Go/docs/security/advisories/AILMX-2026-001-R1.md
   test -f AILinkMax-Go/docs/security/internal-audit/R1-affected.csv
   test -f AILinkMax-Go/docs/security/internal-audit/R1-affected.review.md
   ```
   期望全部 exit 0。
8. **【Cross-WS】** Fix landing 顺序：WS-5 R1 PR → 本 advisory `status: embargoed` → WS-15 flag 默认 false → 14 天 warn-only → WS-15 flag 默认 true PR → 本 advisory `status: released`。**advisory release 与 flag 默认翻转同一周内**。

---

### T16.9 — R2 完整流程跑通

1. **【What it IS】** `advisories/AILMX-2026-002-R2.md` + `internal-audit/R2-affected.csv` + changelog 片段。
2. **【What it is NOT】** 不是 R2 的 nonce/jti 缓存实现（属 WS-5）；不在本任务设计 cache backend。
3. **【Why】** R2 已源码确证 P0：5 分钟内任何捕获的合法签名可重放任意次。
4. **【DO】**
   - frontmatter：
     - `id: AILMX-2026-002`
     - `audit_origin: R2`
     - `severity: P0`
     - `cvss_v3_1`：参考 — AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:H/A:N（需获得有效签名样本，攻击复杂度 H）→ ~5.9（具体由 sec-owner 评）
     - `cve_id`：按 T16.4 决策树
   - Summary：HMAC 请求签名验证仅校验 `timestamp` 在 ±300s 内，缺 nonce / jti 缓存，攻击者若截获一次合法签名可在 5 分钟内重放任意次同一请求。
   - Impact：所有启用 HMAC 签名（`signature_required = true`）的 tenant 受影响；攻击需获取传输中或日志中的签名样本。
   - Workaround：缩短 timestamp window（**警告**：会破坏正常时钟漂移容差；不推荐）；监控请求重复（看 T16.2 的 `distinct_ip_count` / 同 timestamp 计数）。
   - Fix：vX.Y.Z + git sha；新增 Redis-backed nonce 缓存（300s TTL）。
   - 跑 R2 query → CSV。
5. **【DON'T】**
   - DON'T 写 "5 分钟太短，攻击者抓不到样本" — 5 分钟在中间人 / token leak 场景足够。
   - DON'T 把 fix 后的 Redis key 命名贴进 advisory（内部细节）。
6. **【HOW】** 同 T16.8 流程；frontmatter 与段落按本任务 §DO。
7. **【VERIFY】**
   ```
   grep -q "^id: AILMX-2026-002$" AILinkMax-Go/docs/security/advisories/AILMX-2026-002-R2.md
   grep -q "^severity: P0$" AILinkMax-Go/docs/security/advisories/AILMX-2026-002-R2.md
   test -f AILinkMax-Go/docs/security/internal-audit/R2-affected.csv
   ```
   期望全部 exit 0。
8. **【Cross-WS】** R2 fix 不一定走 warn-only flag（nonce 缓存通常无 backward-compat 问题；nonce 在原协议已是"不送也通过"，加 enforce 是新增校验 → 可能需要 grace period 让客户端补 nonce 字段）。本任务在 advisory § Fix 写明 "warn-only 是否适用 由 WS-5 设计决定"，不替前者决定。

---

### T16.10 — R3 完整流程跑通

1. **【What it IS】** `advisories/AILMX-2026-003-R3.md` + `internal-audit/R3-affected.csv` + changelog 片段。
2. **【What it is NOT】** 不是 R3 的 kid allowlist 实现（属 WS-5）。
3. **【Why】** R3 已源码确证 P0：`jwks.resolve(kid)` 返回 None 时静默回退 `static_pem_rs256`，攻击者用任意 kid 即可触发此路径。若 static_pem 私钥泄露 = 全局签发任意 JWT。
4. **【DO】**
   - frontmatter：
     - `id: AILMX-2026-003`
     - `audit_origin: R3`
     - `severity: P0`
     - `cvss_v3_1`：参考 — AV:N/AC:H/PR:N/UI:N/S:C/C:H/I:H/A:N（攻击复杂度高：需 static_pem 泄露 + 知道 kid 字段可任意 → 但 impact 极高）→ ~8.0
     - `cve_id`：按 T16.4 决策树
   - Summary：JWT 验证在 JWKS resolver 返回 None 时静默回退到 `static_pem_rs256`，未对 `kid` 做 allowlist 交叉校验，导致一旦 static PEM 私钥泄露，攻击者可用任意 kid（含未配置的）签发可通过验证的 JWT。
   - Impact：同时配置了 `static_pem_rs256` 与 `jwks_url` 的 tenant；R3 audit CSV 列。
   - Workaround：删除 `static_pem_rs256` 配置（强制走 JWKS）；或在 fix 前 rotate static PEM 私钥。
   - Fix：vX.Y.Z + git sha；新增 kid allowlist + 默认禁用 static fallback（flag `[auth].kid_allowlist_enforce`）。
5. **【DON'T】**
   - DON'T 写 "需要 static_pem 私钥泄露才能利用" 当作减轻情节 — 私钥泄露是常见场景，且本缺陷使私钥泄露的 blast radius 从 "受影响 kid 范围" 扩到 "全 tenant"。
   - DON'T 漏列 § Workaround 的 "rotate static PEM" — 受影响 tenant 可能无法立即升级。
6. **【HOW】** 同 T16.8 流程。
7. **【VERIFY】**
   ```
   grep -q "^id: AILMX-2026-003$" AILinkMax-Go/docs/security/advisories/AILMX-2026-003-R3.md
   grep -q "^severity: P0$" AILinkMax-Go/docs/security/advisories/AILMX-2026-003-R3.md
   test -f AILinkMax-Go/docs/security/internal-audit/R3-affected.csv
   ```
   期望全部 exit 0。
8. **【Cross-WS】** R3 fix 与 R1 fix **共享 14 天 warn-only 窗口**（同一 `[auth]` table 下两个 flag 同时进入 warn-only）。advisory § Timeline 双向链接 R1 advisory。

---

### T16.11 — R4 完整流程跑通（P1 降级，但保留 P0 模板）

1. **【What it IS】** `advisories/AILMX-2026-004-R4.md` + `internal-audit/R4-affected.csv` + changelog 片段。
2. **【What it is NOT】** 不是 trust_xff 默认值变更（默认已是 false；属 WS-5 / WS-10 doc 改进）。
3. **【Why】** R4 已源码确证：`trust_xff` 默认 `false` → 默认配置安全；P0 仅在 operator 错开启 `trust_xff=true` 且裸暴露公网时成立。**已在 §0 源码核对结果中降级为 P1 footgun**。
4. **【DO】**
   - frontmatter：
     - `id: AILMX-2026-004`
     - `audit_origin: R4`
     - **`severity: P1`**（已降级；理由写到 § Impact 第一句）
     - `cvss_v3_1`：参考 — AV:N/AC:H/PR:N/UI:N/S:U/C:N/I:N/A:L（绕过 rate-limit / IP blocklist 实际是 DoS / quota bypass，非数据泄露）→ ~3.1；**但若 operator 错开启则 cvss 升至 P0**，advisory 第二条 cvss 字段写"vary by configuration"
     - `cve_id`：默认 `not-requested`（P1 footgun + 已有配置默认安全）；如外部研究者要求可申请
   - Summary：当 `[security].trust_xff = true` 时，gateway 信任客户端 `X-Forwarded-For`，用 header 值做 rate-limit / IP blocklist key；攻击者可伪造 header 绕过限制。**默认配置 `trust_xff = false` 不受影响**。
   - Impact：第一句强调"默认配置不受影响"；列出受影响条件——`trust_xff = true` AND gateway 直接面向 Internet（无信任 L7 LB）。
   - Workaround：**已是默认**——保持 `trust_xff = false`，或确保 LB strip client XFF。
   - Fix：**doc-only fix 优先**：在 config 注释、README、deploy guide 加更强警告；可选择加 boot-time refuse（`trust_xff = true` + 无 LB 健康检查 → 拒绝启动），但该决定属 WS-5。
5. **【DON'T】**
   - DON'T 在 advisory 提"P0"（除非源码核对发现 §0 表 R4 行结论有误）。
   - DON'T 把 R4 advisory 与 R1/R2/R3 合并 release —— P1 与 P0 节奏不同，合并 release 会迟缓 P0 通告。
   - DON'T 跳过 advisory（哪怕 P1）——已被发现的安全配置陷阱必须留 paper trail。
6. **【HOW】** 同 T16.8 流程；按本任务 §DO 修改 frontmatter severity = P1。
7. **【VERIFY】**
   ```
   grep -q "^id: AILMX-2026-004$" AILinkMax-Go/docs/security/advisories/AILMX-2026-004-R4.md
   grep -q "^severity: P1$" AILinkMax-Go/docs/security/advisories/AILMX-2026-004-R4.md
   grep -q "默认配置.*不受影响\|default.*not affected" AILinkMax-Go/docs/security/advisories/AILMX-2026-004-R4.md
   test -f AILinkMax-Go/docs/security/internal-audit/R4-affected.csv
   ```
   期望全部 exit 0。
8. **【Cross-WS】** 与 WS-10 runbook 协同：runbook 加一节"deploy checklist：`trust_xff` 评估"；与 WS-12 协同：CI lint 加 "config snapshot 检查 `trust_xff = true` 需附 LB 证据 sign-off"（建议，非阻塞）。

---

### T16.12 — launch gate 验收

1. **【What it IS】** WS-16 在 v1 §7 验收门 / v2 §7 上的对接 checklist 实例化；产出 `docs/security/ws-16-launch-gate.md`。
2. **【What it is NOT】** 不是替代 v1 §7（是其子集 + WS-16 specific 项）。
3. **【Why】** v2 §7 #14 "每个 P0 安全披露（R1-R4）必须经过 WS-16 流程；无对应安全告知/客户通告记录的 PR 拒合"。无 launch gate checklist → 拒合判断主观。
4. **【DO】** Launch gate 必须全部勾选才能 launch（v2 §6.1 D11 / D13）：
   - [ ] 4 份 advisory（R1/R2/R3/R4）均 `status: released`
   - [ ] 4 份 internal-audit CSV 存在且 reviewer 双签
   - [ ] CVE 申请决策已落 advisory frontmatter（`pending` / `CVE-...` / `not-requested`）
   - [ ] 14-day warn-only 已完成（R1/R3 metric < 1%；或本 launch 是 fix landing release，warn-only 在 launch 后才开始 — 此情况下 launch gate 改为 "warn-only protocol 文件存在 + metric 已暴露 + 起算 ts 已记录"）
   - [ ] changelog 片段已合入 CHANGELOG.md
   - [ ] SECURITY.md 在双仓存在且一致
   - [ ] disclosure-process.md、cve-decision-tree.md、external-reporter-sop.md、warn-only-protocol.md 均存在
5. **【DON'T】**
   - DON'T 把 launch gate 与 fix 验收门混淆（fix 验收门属 WS-5）。
   - DON'T 让 advisory `status: embargoed` 通过 launch gate（必须 released 或经书面延期 approve）。
6. **【HOW】** `ws-16-launch-gate.md` 字面骨架：
   ```markdown
   # WS-16 launch gate
   ## Checklist (all must pass)
   - [ ] advisory R1 status: released
   - [ ] advisory R2 status: released
   - [ ] advisory R3 status: released
   - [ ] advisory R4 status: released
   - [ ] internal-audit CSV × 4 with reviewer co-sign
   - [ ] CVE decision recorded in each advisory frontmatter
   - [ ] warn-only metric protocol active (or completed)
   - [ ] CHANGELOG.md Security 小节 entries × 4
   - [ ] SECURITY.md 双仓一致
   - [ ] process docs present: disclosure-process / cve-decision-tree / external-reporter-sop / warn-only-protocol
   ```
7. **【VERIFY】**
   ```
   test -f AILinkMax-Go/docs/security/ws-16-launch-gate.md
   # 自动检查所有 advisory status
   for i in 001 002 003 004; do
     f=$(ls AILinkMax-Go/docs/security/advisories/AILMX-2026-${i}-R*.md 2>/dev/null)
     test -n "$f" || { echo "missing advisory $i"; exit 1; }
     grep -E "^status: (released|embargoed)$" "$f" || { echo "bad status $f"; exit 1; }
   done
   ```
   期望全部 exit 0。
8. **【Cross-WS】** 与 v2 §6.1 D11 / D13 launch gate 接驳；与 WS-12 lint "WS-16 artifact 完整性 lint" 协同；与 WS-15 flag 切换 timeline 时序协同。

---

## 5. INVESTIGATE-N（开放项；按 v2 §0 重要 2 / P4 标记，不假定结论）

| ID | 问题 | Owner | 推进方式 |
|---|---|---|---|
| **INVESTIGATE-WS16-A** | `ailinkmax-gateway` 仓库当前可见性（公开 / 私有 / 部分公开 mirror）？影响 T16.4 CVE 决策树落地。 | sec-owner | 找 platform-lead 确认 GitHub repo settings；写一句话结论到 `docs/security/cve-decision-tree.md` 顶部 |
| **INVESTIGATE-WS16-B** | R2 fix 是否需要 warn-only？nonce 缺失场景下，旧客户端直接被 reject 还是 warn 14 天？需与 WS-5 R2 owner 同步。 | WS-5 R2 owner + sec-owner | 在 T16.9 advisory §Fix 写明结论 |
| **INVESTIGATE-WS16-C** | R3 `static_pem_rs256` 字段是否同时存在于 `auth_configs` 与其他配置表？T16.2 R3 query 是否覆盖完整？ | sec-owner | 跑 query 前与 WS-5 R3 owner 比对 `crates/gw-auth/src/jwt.rs` 配置来源 |
| **INVESTIGATE-WS16-D** | R4 是否还有除 `trust_xff = true` 外的 XFF 信任路径（如 vendor / tunnel header forwarding 隐式信任）？ | WS-5 R4 owner | 全仓 grep `X-Forwarded-For` / `x-forwarded-for`；现有已知点：`crates/gw-proxy/src/filters/early.rs:240` + `crates/gw-gateway/src/config.rs:362` |
| **INVESTIGATE-WS16-E** | 是否有 ailinkmax-gateway 历史 release 版本仍在客户生产？如有，advisory § Affected versions 需要列具体版本范围。 | sec-owner + WS-10 owner | 看 release 历史 + tenant 上报 binary 版本（如有 telemetry） |

执行任何 Task 落入上述 INVESTIGATE 范围时**不要假定结论**，先写到 PR 描述里。

---

## 6. 与其他 WS 的时序对齐

```
D0   T16.0 (frame), T16.1 (process), T16.4 (CVE tree),
     T16.5 (SOP), T16.6 (changelog), T16.7 (warn-only)
     — 所有"流程文档"可并行起草

D1   T16.2 (audit CSV) — 依赖 WS-5 fix-PR 已识别确切 affected 字段
     T16.3 (advisory template)

D2-D4 T16.8 / T16.9 / T16.10 / T16.11 —
     advisory + CSV + changelog 落地；
     与 WS-5 fix-PR 并行（fix landing 同 PR 触发 advisory status: embargoed）

D6   adversarial 复审日 #1：包含 WS-16 文档审查（v2 §6.1 D6）

D7-D9  WS-15 flag 框架启用；R1/R3 进入 warn-only

D11  24h soak + launch gate 全检 → T16.12 在此首次执行

D12+ warn-only 14 天起跑；R1/R3 metric daily review

D14+ launch / enforce 切换（R1/R3）；advisory status: released
```

---

## 7. 产出清单（grep-able checklist）

落地后两仓应存在以下文件（mirror 一致）：

```
docs/security/INDEX.md
docs/security/disclosure-process.md
docs/security/cve-decision-tree.md
docs/security/external-reporter-sop.md
docs/security/changelog-template.md
docs/security/warn-only-protocol.md
docs/security/ws-16-launch-gate.md
docs/security/advisories/_template.md
docs/security/advisories/AILMX-2026-001-R1.md
docs/security/advisories/AILMX-2026-002-R2.md
docs/security/advisories/AILMX-2026-003-R3.md
docs/security/advisories/AILMX-2026-004-R4.md
docs/security/internal-audit/queries.sql
docs/security/internal-audit/R1-affected.csv
docs/security/internal-audit/R1-affected.review.md
docs/security/internal-audit/R2-affected.csv
docs/security/internal-audit/R2-affected.review.md
docs/security/internal-audit/R3-affected.csv
docs/security/internal-audit/R3-affected.review.md
docs/security/internal-audit/R4-affected.csv
docs/security/internal-audit/R4-affected.review.md
docs/security/templates/reporter-ack.md
docs/security/templates/reporter-embargo.md
docs/security/templates/reporter-disclosure.md
SECURITY.md
```

---

## 8. 修订记录

| 版本 | 日期 | 作者 | 变更 |
|---|---|---|---|
| 0.1 | 2026-06-17 | (initial) | 首稿；R4 按源码核对降为 P1 footgun；其余 R1/R2/R3 维持 P0；含 INVESTIGATE-WS16-A..E 开放项 |
