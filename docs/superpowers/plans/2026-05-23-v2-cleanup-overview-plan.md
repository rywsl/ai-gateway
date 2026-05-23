# v2 Cleanup Overview Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 [v2-cleanup-overview spec](../specs/2026-05-23-v2-cleanup-overview-design.md):commit 已写好的 spec 文件 + 已加好的 CLAUDE.md §v2 cleanup status 节,并在 `scripts/check-invariants.sh` 新增 `no_v1_markers_in_prod` lint(warn 模式默认,dry-run 状态)。**6 个 sub-spec 不在本 plan 范围**——每个 sub-spec 各自独立走 brainstorming → spec → plan → impl 流程,在自己合入时同 PR 收口对应 v1 标记。

**Architecture:** 三处改动:`scripts/check-invariants.sh` 新增 lint 块(warn 模式时不 fail,仅打印 hit + note);`CLAUDE.md` 与 spec 文件已就位,合入这次 commit 后即可用。grep gate 用 `GW_V2_GATE_MODE` env 控制 warn/error,本 plan 落 warn 默认,6 sub-spec 全部合入后才由后续 plan 切 error。

**Tech Stack:** Bash 4+、GNU grep、git。**不依赖** `rg` — 项目脚本一律用 `grep -rnE` 风格,保持一致。

---

## File Structure

| 文件 | 状态 | 责任 |
|---|---|---|
| `docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md` | **已写**(untracked) | overview spec |
| `CLAUDE.md` | **已改**(modified) | 新增 §v2 cleanup status 一节 |
| `scripts/check-invariants.sh` | **待改** | 新增 `no_v1_markers_in_prod` lint(warn 模式默认) |
| `docs/superpowers/plans/2026-05-23-v2-cleanup-overview-plan.md` | **本文件** | 实施跟踪 |

不引入新文件、不改 `crates/` 下任何 Rust 源码。

---

## Task 1: 在 `check-invariants.sh` 加 `no_v1_markers_in_prod` lint(warn 默认)

**Files:**
- Modify: `scripts/check-invariants.sh` — 在 `no_double_rustls` lint 结束(L329)之后、`# PENDING lints` 块(L331)之前插入新 lint。

- [ ] **Step 1: 读当前脚本第 318–334 行,确认插入点**

Run:
```bash
sed -n '318,334p' scripts/check-invariants.sh
```

Expected: 看到 `no_double_rustls` lint 块结尾(`fi` 在 L329),紧跟一行空行,然后 L331 起是 `# ===` 分隔线 + `# PENDING lints` 注释块。**插入点在 L329 的 `fi` 之后、L331 的 `# ===` 之前**(即原 L330 空行)。

- [ ] **Step 2: 用 Edit 工具在指定锚点插入新 lint 块**

锚点(old_string)和替换(new_string)如下。锚点用 `no_double_rustls` 块结尾的 `fi` + 后续空行 + `# ===` 分隔线,确保唯一定位。

old_string:
```bash
fi

# ===========================================================================
# PENDING lints — target code has not landed yet. Uncomment in the named phase.
# ===========================================================================
```

new_string:
```bash
fi

# ---------------------------------------------------------------------------
# no_v1_markers_in_prod (v2-cleanup overview §6.3) — production crates must not
# carry v1 placeholder markers. Three-phase rollout:
#   warn  (default, current)   — print hits, do NOT fail
#   error (after all six sub-specs land) — fail CI
# Toggle: GW_V2_GATE_MODE=warn|error
# Tracked tokens: unimplemented!(), todo!(), TODO(v2), wave-7, wave-8,
# "for v1:", "placeholder for v1".
# Excludes: anything under a tests/ directory (overview §1.2 carve-out for
# test fixtures named *Stub / Dummy* / placeholder_e2e_*).
# ---------------------------------------------------------------------------
v1_mode="${GW_V2_GATE_MODE:-warn}"
echo "[no_v1_markers_in_prod] no v1 placeholder markers in crates/ (mode: $v1_mode)"
v1_hits=$(grep -rnE \
    -e 'unimplemented!\(' \
    -e 'todo!\(' \
    -e 'TODO\(v2\)' \
    -e 'wave-7' -e 'wave-8' \
    -e 'placeholder for v1' \
    -e 'for v1:' \
    --include='*.rs' \
    --exclude-dir=tests \
    crates/ 2>/dev/null || true)
if [ -n "$v1_hits" ]; then
  printf '%s\n' "$v1_hits"
  if [ "$v1_mode" = "error" ]; then
    bad "v1 placeholder markers remain (gate mode: error)"
  else
    note "v1 placeholder markers remain (gate mode: warn — not failing)"
  fi
else
  ok "no v1 placeholder markers in crates/"
fi

# ===========================================================================
# PENDING lints — target code has not landed yet. Uncomment in the named phase.
# ===========================================================================
```

- [ ] **Step 3: 用 `bash -n` 静态检查脚本语法**

Run:
```bash
bash -n scripts/check-invariants.sh
```

Expected: 无输出,退出码 0(`echo $?` 验证)。若报语法错,回到 Step 2 检查 heredoc / 引号闭合。

- [ ] **Step 4: 跑脚本验证 warn 模式输出格式正确**

Run:
```bash
bash scripts/check-invariants.sh 2>&1 | grep -A3 '\[no_v1_markers_in_prod\]'
```

Expected:
- 第一行: `[no_v1_markers_in_prod] no v1 placeholder markers in crates/ (mode: warn)`
- 随后几行是 grep hit(具体行数取决于代码现状,典型 20+ 行)
- 末尾一行: 黄色 `note: v1 placeholder markers remain (gate mode: warn — not failing)`(`note` 函数走 `\033[33m` 黄色)

如果脚本整体退出非 0,**与本 lint 无关** — 本 lint 在 warn 模式不增 fail 计数。其他 lint 若有 pre-existing fail,属独立问题,不在本 plan 范围。

- [ ] **Step 5: 跑 `GW_V2_GATE_MODE=error` 验证可硬阻塞**

Run:
```bash
GW_V2_GATE_MODE=error bash scripts/check-invariants.sh 2>&1 | tail -20
echo "exit=$?"
```

Expected:
- 输出含红色 `FAIL v1 placeholder markers remain (gate mode: error)`
- 末尾 `INVARIANT CHECK FAILED`
- `exit=1`

如果 exit=0,说明 grep 没匹到任何 token — 这意味着 baseline 已干净,不需要 cleanup,本 overview spec 也就失去意义。回到 spec 检查锚点 token 列表是否过严。

- [ ] **Step 6: 不 commit(留到 Task 2 三件套一起 commit)**

---

## Task 2: Commit overview 三件套

**Files:**
- Add(untracked): `docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md`
- Add(untracked): `docs/superpowers/plans/2026-05-23-v2-cleanup-overview-plan.md`(**本文件**)
- Modify: `CLAUDE.md`
- Modify: `scripts/check-invariants.sh`

- [ ] **Step 1: 看 git status,确认要 commit 的范围**

Run:
```bash
git status --short
```

Expected: 至少包含这 4 行(顺序无关):
```
 M CLAUDE.md
 M scripts/check-invariants.sh
?? docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md
?? docs/superpowers/plans/2026-05-23-v2-cleanup-overview-plan.md
```

若有**其他**修改或 untracked 文件,逐个判断是否属本次 commit:
- 属于 → 同样 stage;
- 不属于 → 让用户先 stash / commit / 决定,不要混入本次 commit。

- [ ] **Step 2: 看 diff 核对 CLAUDE.md 与 check-invariants.sh 改动**

Run:
```bash
git diff CLAUDE.md
git diff scripts/check-invariants.sh
```

Expected:
- `CLAUDE.md` diff: 在 `## Source layout` 之前新增 `## v2 cleanup status` 节(约 18 行),内容含"v1-era phrasing in this file and in docs/ is **stale**"、6 sub-spec 名、agent 操作规则。**无其他句子被改动**。
- `scripts/check-invariants.sh` diff: 在 `no_double_rustls` lint 与 `# PENDING lints` 注释块之间新增 `no_v1_markers_in_prod` 块(Task 1 Step 2 的 new_string)。**无其他 lint 被改动**。

- [ ] **Step 3: stage 四个文件**

Run:
```bash
git add docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md \
        docs/superpowers/plans/2026-05-23-v2-cleanup-overview-plan.md \
        CLAUDE.md \
        scripts/check-invariants.sh
```

- [ ] **Step 4: 创建 commit**

Run:
```bash
git commit -m "$(cat <<'EOF'
v2-cleanup(overview): land overview spec + CLAUDE.md status section + grep gate (warn)

Adds docs/superpowers/specs/2026-05-23-v2-cleanup-overview-design.md as the
v1->v2 cleanup index: Go-vs-Rust split, six sub-spec scope, dependency graph,
shared invariants, and item-ID reverse-lookup table. Six sub-specs (S-codec,
S-routing, S-ledger, S-proxy-core, S-admin-tunnel, S-contracts) each run
their own brainstorming -> spec -> plan -> impl cycle.

CLAUDE.md gains a v2 cleanup status section that flags v1-era phrasing in
this file and docs/ as stale wherever a sub-spec has landed. Existing L30
wave-7 wording is left to the S-proxy-core PR per overview section 4.1.

scripts/check-invariants.sh gains no_v1_markers_in_prod lint, defaulting to
warn mode. Set GW_V2_GATE_MODE=error to harden once all six sub-specs land.

docs/superpowers/plans/2026-05-23-v2-cleanup-overview-plan.md tracks the
three-task rollout of this commit itself.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Expected: pre-commit hook(若有)通过;commit 创建成功,输出含 `main <hash>` 与 `4 files changed`。

如 pre-commit hook fail:
- 看 hook 输出,**不要 `--no-verify`**;
- 修对应文件,重新 `git add` + 重新 `git commit`(新 commit,不要 `--amend`)。

- [ ] **Step 5: 验证 commit 落地 + working tree clean**

Run:
```bash
git log --oneline -1
git status
```

Expected:
- `git log` 顶部为新 commit,subject 含 `v2-cleanup(overview): land overview spec`
- `git status`: `nothing to commit, working tree clean`

---

## Task 3: 跑一次完整脚本,确认 lint 在 warn 模式下不破坏 CI

**Files:** 无修改,仅运行。

- [ ] **Step 1: 完整跑一次 check-invariants.sh,记录 lint 输出**

Run:
```bash
bash scripts/check-invariants.sh; echo "exit=$?"
```

Expected:
- 全部 lint 输出依次打印,其中含 `[no_v1_markers_in_prod] ... (mode: warn)` + hit 列表 + `note: ... (warn — not failing)` 段落。
- 整体退出码:**与本 lint 无关**。若其他 lint 全 ok,`exit=0`;若 pre-existing fail,`exit=1`(不是本 plan 问题)。
- 关键确认:`note:` 行存在,**`FAIL`(红色)行不存在**于 `no_v1_markers_in_prod` 段落。

- [ ] **Step 2: 用 `GW_V2_GATE_MODE=error` 跑一次,记录会 fail**

Run:
```bash
GW_V2_GATE_MODE=error bash scripts/check-invariants.sh > /tmp/v2-gate-error.log 2>&1
echo "exit=$?"
grep -c 'FAIL v1 placeholder markers' /tmp/v2-gate-error.log
```

Expected:
- `exit=1`
- `grep -c ...` 输出 `1`(精确一次 FAIL 来自本 lint)

- [ ] **Step 3: 不 commit(本 task 只验证,不改文件)**

---

## Self-Review

**1. Spec coverage:**

| spec §11 状态追踪条目 | 覆盖 |
|---|---|
| 本 overview 合入 | Task 2 commit |
| CLAUDE.md §v2 cleanup status 一节落地 | Task 2 commit(文件已改) |
| `scripts/check-invariants.sh` 新增 v1 marker gate(初始关闭) | Task 1 + Task 2 |
| S-codec / S-routing / S-ledger / S-proxy-core / S-admin-tunnel / S-contracts | **不在本 plan**(spec §9 已规定各自独立 cycle) |
| 6 sub-spec 全部合入后开启 grep gate error 模式 | **不在本 plan**(留给最后一个 sub-spec 合入时的 plan) |
| e2e soak 整体验收 | **不在本 plan**(留给最终验收 plan) |

无 spec 范围内的遗漏。

**2. Placeholder scan:** plan 内无 "TBD" / "implement later" / "Add appropriate error handling" / 不带代码的"write tests for the above"。每个 step 均含具体命令与预期。✓

**3. Type / token consistency:**
- lint 名 `no_v1_markers_in_prod` 一致出现于:Task 1 Step 2 代码、Step 4 / Step 5 grep 锚点、Task 3 Step 2 grep 锚点、commit message。
- env var `GW_V2_GATE_MODE` 一致出现于:Task 1 Step 2 代码、Step 5 命令、Task 3 Step 2 命令、commit message。
- 模式值 `warn` / `error` 一致。
- token 列表(`unimplemented!()`、`todo!()`、`TODO(v2)`、`wave-7`、`wave-8`、`for v1:`、`placeholder for v1`)与 overview spec §6.3 完全一致。✓
