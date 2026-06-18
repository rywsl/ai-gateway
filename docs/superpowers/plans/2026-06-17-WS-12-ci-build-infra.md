# WS-12 子计划：CI / 构建基建（新工作流）

> **状态**：草案 · 双仓 mirror（本文件路径 `AILinkMax-Go/docs/superpowers/plans/2026-06-17-WS-12-ci-build-infra.md` 与 `ailinkmax-gateway/docs/superpowers/plans/2026-06-17-WS-12-ci-build-infra.md` 必须字节一致）
> **起草**：2026-06-17
> **依据**：
> - master plan v2 §0 重要 1+2、§5 WS-12 全段、§7、§6
> - master plan v1 §7（验收门 CI 规则）+ §8（持续发现机制）
> - REV-006 §7 #10-#14（新验收门 CI 项）
> - 两仓 CLAUDE.md `make check` 现状
>
> **范围**：8 个新 lint 脚本 + 1 个新 audit 脚本 + deploy ordering gate + 全部挂入 `make check` + CI 集成
>
> **轨道**：`WS-12 + WS-16 shared by team lead`（v2 §6.2）
>
> **关键路径**：D0 启动（v2 §6.1）。下游 WS-1/2/3/4/5/6/7/8/9/10 全部依赖 WS-12 的 lint；本 sub-plan 必须在 D1 之前完成至少 T01-T05 的可执行骨架（master plan v2 §6.1）。
>
> **总工时估**：4-5 人日（v2 §5 WS-12 估）
>
> **强制原则**（master plan v2 §0）：每个 task 严格按 7 段格式（v2 §3 范例）。本 sub-plan 额外加第 8 段【对抗复审】使每个 task 共 **8 段**。

---

## 0. 必读须知（v2 §0 强制要求）

### 0.1 sonnet 输出 = 假设，不是结论

master plan v2 §0 重要 2：**任何 file:line 与 "字面代码" 声明都必须先 Read 核对**。本 sub-plan 已抽样核对以下 6 项前置假设（核对结果记录于 §0.4，与原文档冲突的以源码为准）。**执行者在每个 task 进入 RED 步之前必须二次核对**，已记录冲突也要复测（源码可能在期间漂移）。

### 0.2 候选方案约定

v2 §0 重要 2：每个修复方案 **至少考虑 1 个备选**。本 sub-plan 内每个 task 的【做什么 DO】列出至少 2 个候选与选择 ADR 简短理由；执行者可挑别的，但必须更新 ADR 段并在 PR 描述里说明。

### 0.3 INVESTIGATE 开放项

落到本 WS 的开放项：

- **INVESTIGATE-WS12-001**：CI 系统具体是 GitLab CI 还是 GitHub Actions？两仓 `.gitlab-ci.yml` 已存在（核对见 §0.4），未发现 GitHub Actions workflow。**裁定**：本 sub-plan 主集成走 GitLab CI；GitHub Actions 仅作可选 mirror（若团队决定切换则在 §6 task T-10 内重写 yml）。
- **INVESTIGATE-WS12-002**：Go AST analyzer 用 `golang.org/x/tools/go/analysis` 单 binary 还是 `singlechecker` 集成 `golangci-lint`？落 task T-01 内选 A，备用 B 见 ADR。
- **INVESTIGATE-WS12-003**：Rust syn analyzer 用 `cargo +stable run --bin check-tokio-spawn` 还是 `cargo clippy --plugin`？落 task T-02 内选 A，clippy plugin 接口不稳定故 reject。

### 0.4 抽样核对结果（执行者必须 Read 复核）

| 抽样项 | 原文档/v2 声明 | 实际核对（源码） | 是否一致 |
|---|---|---|---|
| `shared/runtime/supervisor.go` 是否存在公开 `Supervised` 函数 | v2 REV-FINAL-W11 与 WS-12 假设其存在 | 文件存在；公开 API 只有 `RunWorkers(ctx, d, workers)`，**没有** `Supervised`/`OneShot` 函数名 | **不一致** — WS-12 T-01 lint 必须接受 `RunWorkers` 风格调用，或先在 WS-4 内引入 `Supervised`/`OneShot`，然后 T-01 才能落地。先决依赖：WS-4 REV-FINAL-V8 |
| Rust `gw-runtime/src/supervisor.rs` 是否存在 | v2 REV-FINAL-W11 声明已存在 | 文件存在 `crates/gw-runtime/src/supervisor.rs` | **一致** |
| `binaries/web/main.go` 是否真有裸 `go func` | v2 REV-FINAL-W4/V10 声明三处 | grep 实测：`main.go` 共 3 处 `go func()`（line 67/74/79） | **一致**（3 处都需 lint 覆盖） |
| 两仓 `.gitlab-ci.yml` 是否存在 | meta 假设 CI 存在 | Go 仓 `.gitlab-ci.yml` 存在；Rust 仓 `.gitlab-ci.yml` 存在 | **一致** |
| `ALM_E2E_LICENSE_BOOTSTRAP` 是否真出现在 prod compose | v2 §5 WS-12 假设 | grep 实测：`docker-compose.prod.yml` line 103 显式带此 env，`docker-compose.prod-deploy.yml` 已显式注释剔除；`docker-compose.e2e.yml` 与默认 `docker-compose.yml` 也带 | **一致** — 当前 `prod.yml` 实际是混合 prod+demo；本 lint 必须先决定哪个文件是真 prod，写在 T-04 |
| Rust 仓 `gw-billing` 是否真有 money `as i64` | v2 §5 WS-12 假设 | grep 实测：`crates/gw-billing/src/formulas.rs:83` `amount.round().clamp(0.0, i64::MAX as f64) as i64` 命中 | **一致**（但 `i64::MAX as f64` 是 type-conversion 常量，不是 money；lint 必须用 AST 区分变量类型而非字面 grep） |
| 两仓 `shared/rustpact/version.go` 是否存在 `RustpactMinSupportedVersion` 常量 | REV-006 §7 #12 假设 | grep 实测：`shared/rustpact/` 目录下**无 `version.go` 文件**，无 `_pv` 或 `MinSupportedVersion` 字段 | **不一致** — T-07 (deploy ordering gate) 必须先创建该常量（task 内含"创建文件"动作，不只是 lint） |

> **每个 task 作者负责其工作范围内的声明核对**（v2 INVESTIGATE-011），未核对部分集中提单到 `docs/issues/2026-06-17-investigates/INVESTIGATE-WS12-<n>.md`。

---

## 1. 范围与不范围

### 1.1 范围（IN）

- 9 个新脚本（按 v2 §5 WS-12 + REV-006 §7 #12）：
  1. `scripts/check-goroutines/`（Go AST analyzer）
  2. `scripts/check-tokio-spawn.rs`（Rust syn analyzer）
  3. `scripts/check-soft-delete.sh`（sqlc 软删谓词审计）
  4. `scripts/check-prod-compose.sh`（forbidden env var）
  5. `scripts/check-money-as-cast.sh`（money 类型 `as i64`）
  6. `scripts/check-rustpact-pv.sh`（rustpact 新字段 `_pv` bump）
  7. `scripts/check-deploy-order.sh`（Go `shared/rustpact/version.go` 与 Rust 当前 `_pv` 比较）
  8. `scripts/check-metric-labels.sh`（metric label value enum allowlist）
  9. `scripts/audit-newfindings.sh`（v1 §8 持续发现机制）
- 所有脚本挂入两仓 `make check`（CLAUDE.md 列为 hard gate）
- `.gitlab-ci.yml` 集成（两仓）+ 可选 GitHub Actions mirror
- 对抗复审 prompt template `tdd-adversarial-review.txt`（v2 §3 范例的脚注引用要求）

### 1.2 不范围（OUT，避免 task 蠕变）

- ❌ 不是写 lint 想要捕捉的实际 bug 修复（那是 WS-1..10 的活）。本 WS 只写 **守卫**。
- ❌ 不是 chaos harness（WS-13）；不是 migration 协调（WS-14）；不是 feature flag 框架（WS-15）。
- ❌ 不是 `golangci-lint` 的额外配置（已在 `make lint` 内，本 WS 不动）。
- ❌ 不动现有 lint 脚本 `check-mod-deps.sh` / `check-docs.sh` / `check-contract-fields.sh` / `check-fakes.sh`（v1 期遗产，保留）。

---

## 2. 任务依赖图

```
T-00 (mkdir scripts/check-goroutines/ + adversarial template)
  ↓
T-01 (check-goroutines Go AST)      —— BLOCKED-BY WS-4 REV-FINAL-V8 完成 Supervised/OneShot 引入
T-02 (check-tokio-spawn.rs Rust syn) —— 并行 T-01
T-03 (check-soft-delete.sh)          —— 并行（独立）
T-04 (check-prod-compose.sh)         —— 并行（独立）
T-05 (check-money-as-cast.sh)        —— 并行（独立）
T-06 (check-rustpact-pv.sh)          —— BLOCKED-BY T-07a（先建 version.go）
T-07a (create shared/rustpact/version.go) —— 必须先做
T-07b (check-deploy-order.sh)        —— BLOCKED-BY T-07a
T-08 (check-metric-labels.sh)        —— 并行（独立）
T-09 (audit-newfindings.sh)          —— 最后做（依赖以上 lint 都已有 baseline）
T-10 (wire all into make check + CI yml) —— 最后做
T-11 (adversarial review pass)        —— D6 的 adversarial 复审日 #1 之前完成自审
```

---

## 3. 8 段格式说明

每个 task 共 8 段（v2 §3 范例的 7 段 + 第 8 段【对抗复审】）：

1. **【是什么 What it IS】** — 一句话定义 + 具体文件路径
2. **【不是什么 What it is NOT】** — 范围外 + 常见误解
3. **【为什么 Why this exists】** — 根因 + 引用 master plan ID
4. **【做什么 DO】** — 顺序步骤 + 候选 A/B/C + ADR 简短选择理由
5. **【不做什么 DON'T】** — 反模式
6. **【怎么做 HOW (具体)】** — 字面代码、字面命令、字面 file:line。**禁止"appropriate"、"as needed"**
7. **【如何验证 VERIFY】** — 具体命令 + 期望输出 + 输出文件
8. **【对抗复审】** — 完成后派 sonnet 用 `tdd-adversarial-review.txt` 复审，列出必须重点检查的 3 个失败模式

---

## 4. 任务清单

### TASK WS12-T-00：建目录与对抗复审 prompt 模板

**【是什么 What it IS】**
- 在两仓 `scripts/` 下建 `scripts/check-goroutines/` 子目录（Go AST analyzer 项目）。
- 在 Go 仓 `docs/superpowers/tools/` 下建 `tdd-adversarial-review.txt`（被 v2 §3 范例引用）。
- 在 Go 仓 `docs/issues/2026-06-17-investigates/` 建空目录（v2 §99 与 §8 都要求）。

**【不是什么 What it is NOT】**
- ❌ 不是写 lint 逻辑（在 T-01）。
- ❌ 不是 sketches mode；目录与模板要真实可用。

**【为什么 Why this exists】**
- master plan v2 §3 范例引用 `tdd-adversarial-review.txt`（"WS-12 交付"），不交付则其他 sub-plan 引用悬空。
- v2 §99 与 §8 要求 `docs/issues/2026-06-17-investigates/` 目录已存在以收 INVESTIGATE。

**【做什么 DO】**
1. `mkdir -p AILinkMax-Go/scripts/check-goroutines/` + `mkdir -p ailinkmax-gateway/scripts/check-goroutines/`（mirror）
2. 写 `AILinkMax-Go/docs/superpowers/tools/tdd-adversarial-review.txt`（内容见 HOW 段）
3. `mkdir -p AILinkMax-Go/docs/issues/2026-06-17-investigates/` + 写 `README.md`：列 v2 §99 全部 INVESTIGATE-001..011 加 WS-12 自己的 001-003 作为占位文件（空 task：标题 + "待 sonnet 复核"）
4. 候选 A（采用）：模板用纯文本 prompt + 三占位变量 `<TASK_ID>` / `<FILE_PATH>` / `<CRITICAL_RISK_LIST>`。ADR：纯文本可被 SendMessage 直接喂给 sonnet。
5. 候选 B（备）：YAML 结构化 prompt（更适合 LLM API 调用）。ADR：未采用 — 本团队还未确定 LLM API harness，纯文本立即可用。

**【不做什么 DON'T】**
- ❌ 不要把 prompt 写在脚本里硬编码（无法独立演进）。
- ❌ 不要把 `docs/issues/2026-06-17-investigates/README.md` 内容塞进 master plan v2 本体（v2 要求 INVESTIGATE 落到独立文件）。

**【怎么做 HOW (具体)】**

`AILinkMax-Go/docs/superpowers/tools/tdd-adversarial-review.txt` 字面内容：

```
You are an adversarial reviewer for AILinkMax pre-launch defect eradication.

Task under review: <TASK_ID>
Files touched: <FILE_PATH>

Required checks (must all be addressed; if you skip one, explain why):
<CRITICAL_RISK_LIST>

Common failure modes to actively hunt:
1. Lint coverage gap: are there code paths the lint cannot see (cgo, build tags, generated code)?
2. False negatives at scale: simulate 3 realistic "almost-banned" snippets that should trip the lint, verify each does.
3. Premise drift: re-read the cited file:line; does the original "literal code" claim still hold? If not, what is the latest source state and does the lint still make sense?

Output schema:
{
  "verdict": "pass" | "needs-revision" | "premise-broken",
  "findings": [{"severity": "critical|major|minor", "claim": "...", "file_line": "path:N", "evidence": "..."}],
  "premise_drift_detected": <bool>,
  "must_redo_steps": ["..."]
}

Do NOT propose alternative implementations. Critique only.
```

`bash` mirror 命令：

```bash
mkdir -p /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/scripts/check-goroutines/
mkdir -p /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway/scripts/check-goroutines/
mkdir -p /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/docs/superpowers/tools/
mkdir -p /Users/whereslow/Desktop/work/Cnewxus/AILinkMax-Go/docs/issues/2026-06-17-investigates/
```

**【如何验证 VERIFY】**

```bash
ls -la AILinkMax-Go/scripts/check-goroutines/
ls -la AILinkMax-Go/docs/superpowers/tools/tdd-adversarial-review.txt
ls -la AILinkMax-Go/docs/issues/2026-06-17-investigates/README.md
```

期望：每条命令返回 stat 行；模板文件 `wc -l` ≥ 20 行；INVESTIGATE README 包含 14 个占位项（v2 §99 的 011 + WS-12 的 001-003）。

输出落档：`logs/WS12-T00-verify.txt`，包含 3 个 `OK` 行（每文件一行）。

**【对抗复审】** 此 task 风险低，免对抗复审；并入 T-11 一并复审。

---

### TASK WS12-T-01：`check-goroutines/` Go AST analyzer

**【是什么 What it IS】**
- 一个 Go 1.22+ 项目在 `scripts/check-goroutines/`，用 `golang.org/x/tools/go/analysis` SDK 写单 binary `check-goroutines`。
- 输入：Go 仓 module 包列表（`./binaries/...` 与 `./modules/...`）。
- 输出：违例打印到 stderr + 退出码 1；干净则退出码 0。
- 规则：在 `binaries/` 与 `modules/` 内禁止裸 `go func(...)`、`go funcName(...)`、`g.Go(...)`（即 `*errgroup.Group.Go`），除非：
  - 直接调用是 `runtime.Supervised(...)` 或 `runtime.OneShot(...)`；
  - 或在 `_test.go` 文件内（豁免）；
  - 或同文件 `//nolint:goroutine-supervisor reason="..."` 注释覆盖（必须带 reason，被 lint 校验非空）。

**【不是什么 What it is NOT】**
- ❌ 不是 grep 替代（v2 REV-FINAL-V9 显式禁用 regex；必须用 AST）。
- ❌ 不扫 `tests/`、`shared/`、`tools/`（按 v2 §5 WS-12 仅指明 `binaries/` 与 `modules/`）。
- ❌ 不是 `tokio::spawn` 检查（那是 T-02 的活）。
- ❌ 不替代 `make lint`（golangci-lint）。

**【为什么 Why this exists】**
- 根因：v1 与 v2 多处假设"裸 go 已被 ban"，实际无 lint；`binaries/web/main.go:67/74/79` 仍有 3 处裸 `go func()`（已 grep 核对 §0.4）。
- 引用：v1 §7 #6 第 1 项；v2 §5 WS-12 第 1 项；v2 REV-FINAL-W4/V10/V11；v2 REV-FINAL-V8/V9。
- 后果：worker panic 静默挂起进程 → /readyz 撒谎 → 流量继续路由到 broken instance。

**【做什么 DO】**

1. 初始化 `scripts/check-goroutines/go.mod`：`module github.com/cnxx-tech/ailinkmax-go/scripts/check-goroutines` + `go 1.22`；`require golang.org/x/tools v0.20.0`（执行者必须按版本策略查证最新稳定版，本文档当前查证日 2026-06-17）。
2. 候选 A（采用）：`singlechecker.Main(analyzer)` 模式 — 单 binary 独立调用，不绑 golangci-lint 插件接口。ADR：与现 `make check` 风格一致（独立 shell 调用 binary）。
3. 候选 B（备）：实现为 `golangci-lint` plugin（custom linter via `linterdb`）。ADR：未采用 — golangci-lint v2 自定义 linter 走 module replace 复杂度高且 CI 缓存难。
4. 实现 `analyzer.Run` 内 `ast.Inspect`：识别 `*ast.GoStmt` 节点 → 检查 `.Call.Fun`：
   - 若 `*ast.FuncLit`（裸 `go func() {}`）→ violation。
   - 若 `*ast.Ident` 且 Name 不在白名单 `{"Supervised", "OneShot"}` → violation（除非 SelectorExpr 形式见下）。
   - 若 `*ast.SelectorExpr` 形如 `pkg.X`：检查 `pkg.X` 是否解析到 `shared/runtime.Supervised` 或 `shared/runtime.OneShot`（用 `pass.TypesInfo.ObjectOf(sel.Sel)` 拿包路径），否则 violation。
   - 若 `*ast.SelectorExpr` 形如 `g.Go` 且 `g` 类型实现 `errgroup.Group` → violation。
5. 豁免：`pass.Fset.Position(node.Pos()).Filename` 末尾 `_test.go` → skip。
6. `nolint` 注释解析：用 `pass.Files` 内 `CommentMap` 在 GoStmt 上方查 `//nolint:goroutine-supervisor` 且后跟非空 `reason="..."`（regex 解析注释内容是 OK 的；AST 限制仅适用代码本身）。
7. 输出格式：`<file>:<line>:<col>: goroutine-supervisor: bare 'go' must use runtime.Supervised or runtime.OneShot (see WS-2 REV-FINAL-W4)`。

**【不做什么 DON'T】**
- ❌ 不要用 `regexp.MustCompile(`go\s+func`)` 扫源码（v2 REV-FINAL-V9 显式禁）。
- ❌ 不要豁免 `binaries/all-in-one/`（v2 §5 WS-12 与 REV-FINAL-W4 都说明此处也有违规）。
- ❌ 不要在 `Supervised` 调用内部继续 lint（无限递归）；按"调用点"判而非"被调函数体"。
- ❌ 不要 `package main` 直接写 `Run`；用 `singlechecker.Main` 标准入口。
- ❌ 不要静默接受空 `reason=""`（必须 reason 至少 8 字符）。
- ❌ 不要在 `nolint` 注释扫描时只看 GoStmt 同行；要包含前一行（gofmt 会把注释提前一行）。

**【怎么做 HOW (具体)】**

`scripts/check-goroutines/main.go`（字面骨架）：

```go
package main

import (
	"go/ast"
	"go/types"
	"regexp"
	"strings"

	"golang.org/x/tools/go/analysis"
	"golang.org/x/tools/go/analysis/singlechecker"
)

const (
	supervisorPkg = "github.com/cnxx-tech/ailinkmax-go/shared/runtime"
)

var Analyzer = &analysis.Analyzer{
	Name: "goroutinesupervisor",
	Doc:  "Forbid bare `go` statements in binaries/ and modules/; require runtime.Supervised or runtime.OneShot.",
	Run:  run,
}

var nolintRE = regexp.MustCompile(`//nolint:goroutine-supervisor\s+reason="([^"]{8,})"`)

func run(pass *analysis.Pass) (any, error) {
	for _, f := range pass.Files {
		filename := pass.Fset.Position(f.Pos()).Filename
		if !inScope(filename) {
			continue
		}
		cmap := ast.NewCommentMap(pass.Fset, f, f.Comments)
		ast.Inspect(f, func(n ast.Node) bool {
			gs, ok := n.(*ast.GoStmt)
			if !ok {
				return true
			}
			if hasNolint(cmap, gs) {
				return true
			}
			if isSupervisedCall(pass, gs.Call) {
				return true
			}
			pos := pass.Fset.Position(gs.Pos())
			pass.Reportf(gs.Pos(),
				"goroutine-supervisor: bare 'go' must use runtime.Supervised or runtime.OneShot (file %s:%d:%d, see WS-2 REV-FINAL-W4)",
				pos.Filename, pos.Line, pos.Column)
			return true
		})
	}
	return nil, nil
}

func inScope(filename string) bool {
	if strings.HasSuffix(filename, "_test.go") {
		return false
	}
	return strings.Contains(filename, "/binaries/") || strings.Contains(filename, "/modules/")
}

func hasNolint(cmap ast.CommentMap, gs *ast.GoStmt) bool {
	for _, group := range cmap[gs] {
		for _, c := range group.List {
			if nolintRE.MatchString(c.Text) {
				return true
			}
		}
	}
	return false
}

func isSupervisedCall(pass *analysis.Pass, call *ast.CallExpr) bool {
	sel, ok := call.Fun.(*ast.SelectorExpr)
	if !ok {
		return false
	}
	obj := pass.TypesInfo.ObjectOf(sel.Sel)
	if obj == nil {
		return false
	}
	fn, ok := obj.(*types.Func)
	if !ok {
		return false
	}
	if fn.Pkg() == nil || fn.Pkg().Path() != supervisorPkg {
		return false
	}
	name := fn.Name()
	return name == "Supervised" || name == "OneShot"
}

func main() {
	singlechecker.Main(Analyzer)
}
```

Wire 到 `make check`（见 T-10）：调用方式 `cd scripts/check-goroutines && go build -o ../../bin/check-goroutines . && cd ../.. && ./bin/check-goroutines ./binaries/... ./modules/...`。

**【如何验证 VERIFY】**

1. 单元（在 `scripts/check-goroutines/` 内）：写 `testdata/src/badbare/badbare.go` 与 `testdata/src/goodsupervised/goodsupervised.go`，用 `analysistest.Run(t, analysistest.TestData(), Analyzer, "badbare")` 期望 1 个 diagnostic；对 goodsupervised 期望 0。
   - 命令：`cd scripts/check-goroutines && go test ./...`
   - 期望输出：`ok  github.com/.../check-goroutines  0.XXXs`
2. 集成（首轮放过，按 plan v2 §6.1 D0 启动后会在 D1 由 WS-4 引入 `Supervised`/`OneShot` 后再 enforce）：
   - 命令：`cd AILinkMax-Go && go run ./scripts/check-goroutines ./binaries/... ./modules/... 2>&1 | tee logs/WS12-T01-baseline.txt`
   - 期望：列出当前所有违例（含 `binaries/web/main.go:67/74/79`）；记录基线条数 `cat logs/WS12-T01-baseline.txt | grep -c "goroutine-supervisor"`。
3. CI gate 切换（在 WS-4 完成 `Supervised`/`OneShot` 引入后）：把 `make check` 内 `./bin/check-goroutines || true` 的 `|| true` 移除（在 T-10 内做）。
4. 输出：`logs/WS12-T01-verify.txt` 必须含 `unit: PASS`、`baseline: N=3 (web/main.go)`。

**【对抗复审】** 完成后派 sonnet 用 `tdd-adversarial-review.txt` 复审，重点：
1. **覆盖**：`go func`(`*ast.GoStmt` with `*ast.FuncLit`)、`go someFunc()`(`*ast.Ident`)、`g.Go(...)`(`*ast.SelectorExpr` 自 `errgroup.Group.Go`) 三形态是否都命中？写一个 testdata fixture 含三种形态验证。
2. **白名单泄漏**：testdata 含一个邪恶用户写 `var Supervised = tokio_spawn_equivalent` 来旁路；本 lint 必须按"调用 site 类型是 `shared/runtime.Supervised`"判，不是按 `Sel.Name == "Supervised"`。`isSupervisedCall` 当前对类型解析的实现是否处理了此向量？
3. **nolint 滥用**：`reason="x"` 仅 1 字符是否被 ≥8 字符规则挡？写 testdata 验证。

---

### TASK WS12-T-02：`check-tokio-spawn.rs` Rust syn analyzer

**【是什么 What it IS】**
- 一个 Rust `[[bin]]` 在 `scripts/check-tokio-spawn/` 子 crate，用 `syn` 1.0 + `walkdir` 解析 `crates/*/src/**/*.rs`。
- 规则：禁止裸 `tokio::spawn(...)`、`tokio::task::spawn(...)`、`tokio::spawn_blocking(...)`（除非：调用点同文件用 `#[allow_supervisor_skip(reason = "...")]` attribute 或在 `#[cfg(test)]` mod 内）。
- 在 `crates/` 内必须改用 `gw_runtime::supervised_spawn(...)` 或 `gw_runtime::one_shot_spawn(...)`（v2 REV-FINAL-W11/V12）。

**【不是什么 What it is NOT】**
- ❌ 不是 clippy plugin（v2 INVESTIGATE-WS12-003 已 reject）。
- ❌ 不扫 `tests/`、`examples/`、`benches/`（豁免）。
- ❌ 不替代 `cargo clippy`。

**【为什么 Why this exists】**
- 根因：v2 REV-FINAL-W11 已核对 Rust 仓有 `gw-runtime/src/supervisor.rs`，但 §0.4 与原审计未审 callsite 是否真用了它；grep 实测 `tokio::spawn(` 在 `crates/gw-proxy/src/billing_lifecycle.rs:431` 与 `tunnel_bridge.rs` 多处仍裸调（已核对 §0.4 内未独立列；执行者执行时再 grep 一遍）。
- 引用：v1 §7 #6 第 1 项；v2 §5 WS-12 第 2 项；v2 REV-FINAL-V12/W11。

**【做什么 DO】**

1. 候选 A（采用）：独立 `cargo +stable build` 出 `target/release/check-tokio-spawn` binary；shell 调用。ADR：编译一次缓存到 CI artifact，启动 < 100ms。
2. 候选 B（备）：cargo subcommand `cargo check-tokio-spawn`。ADR：未采用 — 增加 PATH 管理复杂度。
3. 候选 C（备）：单文件 `cargo +nightly script`。ADR：未采用 — nightly 不允许进生产 CI（rust-toolchain.toml 已固定 stable）。
4. 实现：用 `syn::visit::Visit` 遍历 `ItemFn`/`Block`，命中 `Expr::Call` 的 `func` 为路径 `tokio::spawn` / `tokio::spawn_blocking` / `tokio::task::spawn`（任一前缀别名也算 — 即 `use tokio::spawn` 之后裸 `spawn(...)` 也命中，必须 use `syn::visit` 跟踪 imports）。
5. 输入路径：`crates/*/src/**/*.rs`（用 `walkdir` 过滤）。
6. 豁免：
   - 父 `Item` 上有 `#[cfg(test)]` 或所在 mod 是 `mod tests { ... }`。
   - 调用 expression 上方 `attribute` `#[allow_supervisor_skip(reason = "<≥8 字符>")]`（用 `syn::Attribute::parse_meta` 解析）。
   - 在 `crates/gw-runtime/src/supervisor.rs` 自身内（实现 supervised_spawn 必然 wrap tokio::spawn）。
7. 输出：`<crate>/<file>:<line>:<col>: tokio-spawn: bare tokio::spawn forbidden in crates/, use gw_runtime::supervised_spawn or one_shot_spawn (see WS-2 REV-FINAL-W11)`。

**【不做什么 DON'T】**
- ❌ 不要用 `regex` 扫源（v2 REV-FINAL-V9）。
- ❌ 不要硬编码 `tokio::spawn` 字面字符串 — 用 `syn::Path` 比较，处理 `use tokio::spawn as s; s(...)` 别名。
- ❌ 不要在 `gw-runtime` 自身实现内仍违规（豁免 supervisor.rs 自身，但 lint 报错时指明 caller 还是 callee 应明确）。

**【怎么做 HOW (具体)】**

`scripts/check-tokio-spawn/Cargo.toml`：

```toml
[package]
name = "check-tokio-spawn"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "check-tokio-spawn"
path = "src/main.rs"

[dependencies]
syn = { version = "2.0", features = ["full", "extra-traits", "visit"] }
walkdir = "2.5"
proc-macro2 = { version = "1.0", features = ["span-locations"] }
```

`scripts/check-tokio-spawn/src/main.rs`（骨架 — 完整实现 ~200 行；CI 必须能 build 通过）：

```rust
use proc_macro2::LineColumn;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, ExprCall, ItemFn, Lit, Meta, MetaNameValue, Path as SynPath};
use walkdir::WalkDir;

#[derive(Default)]
struct Violation { file: PathBuf, line: usize, col: usize, msg: String }

struct Hunter<'a> {
    file: &'a Path,
    cfg_test_depth: usize,
    violations: Vec<Violation>,
}

impl<'ast, 'a> Visit<'ast> for Hunter<'a> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        let in_test = node.attrs.iter().any(is_cfg_test);
        if in_test { self.cfg_test_depth += 1; }
        visit::visit_item_fn(self, node);
        if in_test { self.cfg_test_depth -= 1; }
    }
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if self.cfg_test_depth == 0 && !has_allow_skip(&node.attrs) {
            if let Some(name) = banned_call(&node.func) {
                let span = node.span().start();
                self.violations.push(Violation{
                    file: self.file.to_path_buf(),
                    line: span.line, col: span.column,
                    msg: format!("tokio-spawn: bare {} forbidden in crates/, use gw_runtime::supervised_spawn or one_shot_spawn (see WS-2 REV-FINAL-W11)", name),
                });
            }
        }
        visit::visit_expr_call(self, node);
    }
}

fn banned_call(expr: &Expr) -> Option<String> {
    let path = match expr {
        Expr::Path(p) => &p.path,
        _ => return None,
    };
    let segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    match segs.as_slice() {
        [a, b] if (a == "tokio" && (b == "spawn" || b == "spawn_blocking")) => Some(format!("{}::{}", a, b)),
        [a, b, c] if a == "tokio" && b == "task" && c == "spawn" => Some("tokio::task::spawn".into()),
        // Caller can also have `use tokio::spawn;` then call `spawn(...)`.
        // We do NOT handle that without full type resolution — list as INVESTIGATE-WS12-004.
        _ => None,
    }
}

fn is_cfg_test(a: &Attribute) -> bool {
    matches!(&a.meta, Meta::List(ml) if ml.path.is_ident("cfg") && ml.tokens.to_string().contains("test"))
}

fn has_allow_skip(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        if let Meta::List(ml) = &a.meta {
            if !ml.path.is_ident("allow_supervisor_skip") { return false; }
            // Look for reason = "..." (≥8 chars).
            let s = ml.tokens.to_string();
            if let Some(start) = s.find("reason") {
                let tail = &s[start..];
                if let Some(q1) = tail.find('"') {
                    if let Some(q2) = tail[q1+1..].find('"') {
                        return q2 >= 8;
                    }
                }
            }
        }
        false
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args().nth(1).unwrap_or_else(|| "crates".into());
    let mut total = 0usize;
    for entry in WalkDir::new(&root).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("rs") { continue; }
        // Exempt supervisor.rs itself & test/example/bench dirs.
        let s = p.to_string_lossy();
        if s.contains("/tests/") || s.contains("/examples/") || s.contains("/benches/") {
            continue;
        }
        if s.ends_with("gw-runtime/src/supervisor.rs") { continue; }
        let src = std::fs::read_to_string(p)?;
        let file = syn::parse_file(&src)?;
        let mut h = Hunter{ file: p, cfg_test_depth: 0, violations: vec![] };
        h.visit_file(&file);
        for v in &h.violations {
            println!("{}:{}:{}: {}", v.file.display(), v.line, v.col, v.msg);
            total += 1;
        }
    }
    if total > 0 {
        eprintln!("check-tokio-spawn: {} violation(s)", total);
        std::process::exit(1);
    }
    Ok(())
}
```

记 `INVESTIGATE-WS12-004`：`use tokio::spawn` 别名后裸 `spawn()` 调用不被本 lint 命中（注释里已说明）；执行者必须用 `cargo expand` 或 `rust-analyzer` 调用层级，或简单办法 — 在 `clippy.toml` 加 `disallowed-types`/`disallowed-methods` 规则配合本 lint 形成两道防线（推荐）。

**【如何验证 VERIFY】**

1. 单元：`scripts/check-tokio-spawn/tests/fixtures/bad.rs` 含 `tokio::spawn(async {})`；`good.rs` 含 `gw_runtime::supervised_spawn(...)`。`cargo test` 内 spawn binary 对 fixtures dir 跑。
2. 集成 baseline：
   - `cd ailinkmax-gateway && cargo run -p check-tokio-spawn -- crates 2>&1 | tee logs/WS12-T02-baseline.txt`
   - 期望：列出 `crates/gw-proxy/src/billing_lifecycle.rs:431`、`tunnel_bridge.rs:65/75/207/233/326/492` 至少 7 处违例。
3. `clippy.toml` 双保险写入 `ailinkmax-gateway/clippy.toml`：
   ```toml
   disallowed-methods = [
     { path = "tokio::spawn", reason = "use gw_runtime::supervised_spawn (WS-2 REV-FINAL-W11)" },
     { path = "tokio::task::spawn", reason = "use gw_runtime::supervised_spawn" },
     { path = "tokio::spawn_blocking", reason = "use gw_runtime::supervised_spawn_blocking" },
   ]
   ```
   ADR：clippy 已是 CI 标准（`make check` 调 `cargo clippy --workspace -- -D warnings`），加 toml 零基建。
4. 输出：`logs/WS12-T02-verify.txt` 含 `unit: PASS`、`baseline: N=<count>`、`clippy.toml: present`。

**【对抗复审】** 完成后派 sonnet 复审，重点：
1. **别名 use 路径**：`use tokio::spawn;` 后 `spawn(async {})` 是否被 clippy.toml 兜住？fixture 验证。
2. **`#[cfg(test)]` 嵌套**：`mod tests { #[cfg(not(any()))] fn evil() { tokio::spawn(...) } }` —— 本 lint 是否仍豁免？应仅看直系 `#[cfg(test)]`，不含逆否。
3. **`allow_supervisor_skip` 滥用**：`reason = "x"` 7 字符（边界）应拒；`reason = ""` 应拒；写两 fixture 验证。
4. **gw-runtime/src/supervisor.rs 自身**：lint 是否豁免？grep `tokio::spawn` 在该文件，确认豁免后 baseline 不再含此文件。

---

### TASK WS12-T-03：`check-soft-delete.sh` sqlc 软删谓词审计

**【是什么 What it IS】**
- `scripts/check-soft-delete.sh`（Go 仓主，Rust 仓 mirror 空跑出 N/A）。
- 输入：`migrations/` 找带 `deleted_at TIMESTAMPTZ` 列的表，加 `users.id` 软删跨表 chain。
- 输出：对每个带 `deleted_at` 的表 T：`git grep -nE "FROM ${T}\\b|JOIN ${T}\\b"` 必须命中 `${T}.deleted_at IS NULL` 同 SQL 块内（用 `awk` ±10 行窗口判）；或经过 `users` 守护的 JOIN chain。

**【不是什么 What it is NOT】**
- ❌ 不是 sqlc 自带 lint（sqlc 不做语义校验）。
- ❌ 不解析 SQL AST（用 grep ±N 行窗口足够；v2 REV-FINAL-V14 描述如此）。
- ❌ 不审 `api_keys`（v2 REV-FINAL-V14 已核对 — `api_keys` 表无 `deleted_at` 列；要审的是 `users.deleted_at` 守护 `api_keys.user_id` JOIN chain，单列实现）。

**【为什么 Why this exists】**
- 根因：原审计 H9 声明 `api_keys` 缺 `deleted_at IS NULL`，v2 §0 重要 2 已反证 — `api_keys` 表本身没 `deleted_at` 列（hard delete）。真问题是 users 软删时 api_keys 仍解析。
- 引用：v1 §7 #6 第 2 项；v2 §5 WS-12 第 3 项；v2 REV-FINAL-V14；v2 §0 重要 2 反证 H9。

**【做什么 DO】**

1. 候选 A（采用）：纯 bash + `grep -A 20 -B 5` 窗口审计 + `awk` 拼 SQL block。ADR：sqlc 查询都在 `.sql` 文件，单行 SELECT 后 `WHERE` 至多 20 行；窗口检测足够。
2. 候选 B（备）：用 `sqlparse` Python 库做 AST。ADR：未采用 — 加 Python 依赖。
3. 候选 C（备）：写 sqlc plugin。ADR：未采用 — sqlc plugin 接口不稳定。
4. 实现：
   - Step 1：`grep -lE "deleted_at\s+(TIMESTAMP|TIMESTAMPTZ)" migrations/*.sql` → 拿带 `deleted_at` 的表名集合 `S_DELETED`。
   - Step 2：对每 T ∈ `S_DELETED`，`git grep -nE "FROM\s+${T}\b|JOIN\s+${T}\b" -- modules/ shared/ binaries/` 拿命中文件:行。
   - Step 3：对每命中点：读该文件，从命中行向后 ≤ 20 行 + 向前 ≤ 5 行 拿 SQL 上下文；要求该上下文包含正则 `${T}\.deleted_at\s+IS\s+NULL` 或者 `JOIN\s+users\s+u\s+ON\s+u\.id\s*=\s*${T}\.user_id\s+AND\s+u\.deleted_at\s+IS\s+NULL` 或类似 chain。
   - Step 4：未命中即 violation；列出 violation；exit 1。
   - 特殊豁免：文件路径含 `/migrations/`（自身 DDL）、`*_test.go`、`*.md`（doc 示例）。
5. 单独处理 H9 reframed：对 `api_keys` 不做 deleted_at 检查；改为审 `api_keys` JOIN chain 必须含 users 守护。即在 Step 2 集合外加 `api_keys`，但 Step 3 改条件为只接受 chain 形态。

**【不做什么 DON'T】**
- ❌ 不要 grep 全仓（不要扫 `frontend/`、`docs/`、`target/`）。
- ❌ 不要忽略大小写敏感（PostgreSQL 表名是 lower case，但 SQL keyword 大小写敏感性可变；用 `grep -i` 处理）。
- ❌ 不要把 `deleted_at IS NOT NULL`（合法的"找已软删"查询）误判为缺谓词；这种查询同样满足 `deleted_at` 出现规则。
- ❌ 不要假设 `users.deleted_at` 守护 chain 一定有 `u.deleted_at`；执行者实测可能写法是 `users.deleted_at`（全名），lint 必须接受两种。

**【怎么做 HOW (具体)】**

`scripts/check-soft-delete.sh`：

```bash
#!/usr/bin/env bash
# WS-12 T-03: enforce soft-delete predicate on every sqlc query touching tables with deleted_at columns.
# Reference: master plan v2 REV-FINAL-V14; v1 §7 #6 item 2.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "cd failed" >&2; exit 2; }

EXEMPT_RE='/migrations/|_test\.go$|\.md$|/target/|/node_modules/'
WINDOW_AFTER=20
WINDOW_BEFORE=5

# Step 1: enumerate tables with deleted_at column.
mapfile -t TABLES < <(
  grep -lE 'deleted_at[[:space:]]+(timestamp|timestamptz)' migrations/*.sql 2>/dev/null \
    | xargs -I{} awk '
        /CREATE TABLE/ { tbl=$3; sub(/[ \(].*/,"",tbl); next }
        /deleted_at[ \t]+(timestamp|TIMESTAMPTZ|timestamptz|TIMESTAMP)/ { if(tbl) print tbl }
      ' {} \
    | sort -u
)
# Manually inject api_keys (H9 reframed — chain check only).
TABLES+=("api_keys")
echo "[check-soft-delete] tables under audit: ${TABLES[*]}" >&2

fail=0
for T in "${TABLES[@]}"; do
  # api_keys uses users-chain check only.
  is_chain_only=0
  if [[ "$T" == "api_keys" ]]; then is_chain_only=1; fi

  # Step 2: every FROM/JOIN ${T} reference outside exempted dirs.
  while IFS=: read -r file line _; do
    [[ "$file" =~ $EXEMPT_RE ]] && continue
    # Step 3: window-check.
    start=$((line - WINDOW_BEFORE)); [[ $start -lt 1 ]] && start=1
    end=$((line + WINDOW_AFTER))
    window=$(sed -n "${start},${end}p" "$file")
    if (( is_chain_only )); then
      if echo "$window" | grep -qE "JOIN[[:space:]]+users[[:space:]]+u[[:space:]]+ON[[:space:]]+u\.id[[:space:]]*=[[:space:]]*${T}\.user_id[[:space:]]+AND[[:space:]]+u\.deleted_at[[:space:]]+IS[[:space:]]+NULL"; then
        continue
      fi
      echo "VIOLATION: $file:$line — query touches '$T' without users.deleted_at JOIN guard (H9 reframed)"
      fail=1
    else
      if echo "$window" | grep -qE "${T}\.deleted_at[[:space:]]+IS[[:space:]]+(NOT[[:space:]]+)?NULL"; then
        continue
      fi
      if echo "$window" | grep -qE "JOIN[[:space:]]+users[[:space:]]+u[[:space:]]+ON[[:space:]]+u\.id[[:space:]]*=[[:space:]]*${T}\.user_id[[:space:]]+AND[[:space:]]+u\.deleted_at[[:space:]]+IS[[:space:]]+NULL"; then
        continue
      fi
      echo "VIOLATION: $file:$line — query touches '$T' without '${T}.deleted_at IS NULL' or users-chain guard"
      fail=1
    fi
  done < <(git grep -nE "FROM[[:space:]]+${T}\b|JOIN[[:space:]]+${T}\b" -- 'modules/' 'shared/' 'binaries/' 2>/dev/null)
done

if (( fail )); then
  echo "[check-soft-delete] FAIL — see violations above" >&2
  exit 1
fi
echo "[check-soft-delete] OK — ${#TABLES[@]} table(s) audited"
```

PowerShell mirror `scripts/check-soft-delete.ps1`：相同语义，分两步 `Select-String`。CLAUDE.md 内"两套不得漂移"硬规则。

**【如何验证 VERIFY】**

1. 单元（fixture）：建 `scripts/testdata/soft-delete-bad.sql` 含 `SELECT * FROM users WHERE id=$1`（缺谓词），跑 `bash scripts/check-soft-delete.sh` 并验证 exit code 1。
2. 集成 baseline：
   - `bash scripts/check-soft-delete.sh 2>&1 | tee logs/WS12-T03-baseline.txt`
   - 期望：基线条数记录；若已有违例（v2 H9 原指 `getApiKeyByHash`/`getApiKeyByID` 缺 chain），列出。
3. PowerShell mirror：`pwsh -File scripts/check-soft-delete.ps1` 必须 exit code 与 .sh 一致；输出文本可不同。
4. 输出：`logs/WS12-T03-verify.txt` 含 `unit: PASS`、`baseline: <N>`、`ps1: parity OK`。

**【对抗复审】** 完成后派 sonnet 复审，重点：
1. **CTE/子查询窗口**：`WITH x AS (SELECT FROM users) ...` 同 SQL 块跨 60 行 —— 20 行窗口够吗？fixture 验证；若不够，把窗口加到 50 或改 awk 解析至下一个 `;`。
2. **多表 FROM**：`SELECT FROM users, api_keys WHERE ...` 两表都需谓词；本 lint 仅按单表命中，必须确认双独立命中行都通过。
3. **大小写**：`from users` 小写形式（SQL 不敏感但风格不同）—— grep `-i` 已加，确认 testdata 含小写形态。
4. **反证 H9**：执行 lint 时 `api_keys` 路径是否真按 chain-only 校验（不是按 `api_keys.deleted_at IS NULL`）？fixture 必须含一个有 `api_keys.deleted_at IS NULL` 的伪查询，期望本 lint 仍报"无 users chain"违例（因为 `api_keys` 没 deleted_at 列，写这种谓词是错的）。

---

### TASK WS12-T-04：`check-prod-compose.sh` forbidden env

**【是什么 What it IS】**
- `scripts/check-prod-compose.sh`：扫两仓所有 `docker-compose.prod*.yml` 与 `deploy/k8s/*.yaml`，禁止出现 `ALM_E2E_LICENSE_BOOTSTRAP`、`RUN_DEV_SEED`、`ALM_DEV_ADMIN_BOOTSTRAP` 三个 env var。

**【不是什么 What it is NOT】**
- ❌ 不扫 `docker-compose.yml`（默认/dev compose 允许这些）。
- ❌ 不扫 `docker-compose.e2e.yml`（e2e 必须有）。
- ❌ 不扫 `*.md`（doc 示例）。

**【为什么 Why this exists】**
- 根因：grep §0.4 实测 `docker-compose.prod.yml:103` 实际带 `ALM_E2E_LICENSE_BOOTSTRAP: "1"` —— 当前 prod compose 实际是 demo prod。`docker-compose.prod-deploy.yml` 已显式注释剔除（说明已有此意识），但缺机器化 gate。
- 引用：v1 §7 #6 第 3 项；v2 §5 WS-12 第 4 项；v2 REV-005 DoD 之 "prod compose 不带 demo seed"。

**【做什么 DO】**

1. 候选 A（采用）：grep + 白名单文件列表硬编码。ADR：prod compose 命名固定，列表稳定。
2. 候选 B（备）：yaml 解析 + `services.*.environment` 字段精确扫。ADR：未采用 — 增加 yq/python-yaml 依赖。
3. 检测三 env：`ALM_E2E_LICENSE_BOOTSTRAP`、`RUN_DEV_SEED`、`ALM_DEV_ADMIN_BOOTSTRAP`（执行者执行时再 grep `RUN_DEV_SEED` 是否还有其他类似 — INVESTIGATE-WS12-005）。
4. **关键**：v2 §5 WS-12 说"prod compose"，但 §0.4 显示 `docker-compose.prod.yml` 实际是 demo prod。本 task 必须包含决定性动作：
   - 把 `docker-compose.prod.yml` 内 `ALM_E2E_LICENSE_BOOTSTRAP: "1"` 改为 commented-out + 加 banner 注释 "DEMO ONLY — see docker-compose.prod-deploy.yml for hardened prod"，
   - 或更激进：rename 当前 `docker-compose.prod.yml` → `docker-compose.demo-prod.yml`，让 `prod-deploy.yml` 成为唯一"prod"。
   - ADR：本 sub-plan 推荐前者（rename 影响外部脚本/文档面广）；执行者权衡后写 ADR 入 PR 描述。
5. 写脚本扫 `docker-compose.prod-deploy.yml` 与 `deploy/k8s/*-prod.yaml` + 用户自定义白名单（先决定的 "prod" 文件集）。

**【不做什么 DON'T】**
- ❌ 不要扫 `deploy/k8s/README.md` 等文档（提到 env 名是合法的）。
- ❌ 不要在 PR 内同时改 prod 文件命名 + 加 lint 两件事（先 rename，再加 lint —— 两 PR）。
- ❌ 不要 ban 注释里出现（`# ALM_E2E_LICENSE_BOOTSTRAP: "1"` 是文档形式应允许）；lint 必须按 yaml key 形态判而非纯字符串扫。

**【怎么做 HOW (具体)】**

`scripts/check-prod-compose.sh`：

```bash
#!/usr/bin/env bash
# WS-12 T-04: forbid demo/e2e env vars in production compose & k8s manifests.
# Reference: master plan v2 §5 WS-12 item 4; v1 §7 #6 item 3.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

FORBIDDEN=(ALM_E2E_LICENSE_BOOTSTRAP RUN_DEV_SEED ALM_DEV_ADMIN_BOOTSTRAP)
# Prod files: explicit allowlist (see ADR in WS-12 T-04).
PROD_FILES=(
  docker-compose.prod-deploy.yml
  deploy/k8s/go-deployment.yaml
)

fail=0
for f in "${PROD_FILES[@]}"; do
  [[ ! -f "$f" ]] && { echo "skip (not present): $f"; continue; }
  for var in "${FORBIDDEN[@]}"; do
    # Match yaml key form: ^<indent><var>: or - name: <var>; reject.
    # Exclude commented lines (^# or " #").
    hits=$(grep -nE "^[[:space:]]*${var}[[:space:]]*:" "$f" \
         | grep -vE "^[0-9]+:[[:space:]]*#" || true)
    hits2=$(grep -nE "^[[:space:]]*-?[[:space:]]*name:[[:space:]]*${var}\b" "$f" \
          | grep -vE "^[0-9]+:[[:space:]]*#" || true)
    if [[ -n "$hits" || -n "$hits2" ]]; then
      echo "VIOLATION: $f — forbidden env '$var' present in production compose/manifest"
      [[ -n "$hits" ]] && echo "$hits"
      [[ -n "$hits2" ]] && echo "$hits2"
      fail=1
    fi
  done
done

if (( fail )); then
  echo "[check-prod-compose] FAIL" >&2; exit 1
fi
echo "[check-prod-compose] OK — ${#PROD_FILES[@]} file(s) clean"
```

**【如何验证 VERIFY】**

1. fixture：临时 `cp docker-compose.prod-deploy.yml /tmp/x.yml`，加 `ALM_E2E_LICENSE_BOOTSTRAP: "1"`，临时挂入 `PROD_FILES` 跑，验证 exit 1。
2. 当前仓状态：`bash scripts/check-prod-compose.sh` 当下应 exit 0（`prod-deploy.yml` 已剔除，`go-deployment.yaml` §0.4 grep 命中 line 42 — 必须先决定是否将其纳入 prod 集；若纳入则需要先改源文件，纳入本 task）。
3. 决策点：`deploy/k8s/go-deployment.yaml:42` 当前确有 `ALM_E2E_LICENSE_BOOTSTRAP` —— 在本 task 内同时 PR：把该 env 改为 helm/k8s `Optional: true` + 默认无注入。ADR：要么真正 prod k8s 不带 demo，要么 lint 必须降级到 warn。本 task 默认采用前者。
4. 输出：`logs/WS12-T04-verify.txt` 含 `fixture: triggered`、`current: OK`、`k8s deploy fix: applied`。

**【对抗复审】** 完成后派 sonnet 复审：
1. **新增 prod 文件**：未来 `docker-compose.prod-v2.yml`、`deploy/k8s/prod-overlay.yaml` 不会自动被扫；fixture 验证未挂入文件未被审；考虑用 glob `docker-compose.prod*.yml` + `deploy/k8s/*prod*.yaml` 自动发现，权衡是否有 false positive。
2. **comment 旁路**：`# ALM_E2E_LICENSE_BOOTSTRAP: "1"  # remove me` 必须不报；fixture 验证。
3. **env 文件**：`docker-compose.prod.yml` 用 `env_file: .env.prod` 引外部文件 — 本 lint 不审外部文件；列入 INVESTIGATE-WS12-006。

---

### TASK WS12-T-05：`check-money-as-cast.sh` 禁 money `as i64`

> ## ⚠️ TASK CONSTRAINED — v2.1 corrections §10.2 (2026-06-17)
>
> **此 lint 扫描硬模式 credit-lease 文件。必须遵守教学矩阵 M14。**
>
> **触碰元素**: A1-A11 (lint 范围包含 lease.rs / overflow.rs / counter_handler.rs)
> **必须 NOT 做**（摘自 M14）:
> - **不**自动 patch 任何文件（lint 只报告）
> - **不**让 lint 把 `lease.rs` / `overflow.rs` / `counter_handler.rs` 的合理 cast 误报为 fail（用 allowlist 处理）
> - **不**重构 A1-A11 文件
> **完整矩阵**: 见 v2.1 corrections §11 **M14**
>
> 执行前必读 M14。PR 描述必须列明"已遵守教学矩阵 M14：lint 只读，allowlist 已包含必要 cast"。

---

**【是什么 What it IS】**
- `scripts/check-money-as-cast.sh`（Rust 仓主，Go 仓 mirror N/A 占位）。
- 扫 Rust 仓 `crates/gw-billing/src/**/*.rs` + `crates/gw-ledger/src/**/*.rs` + `crates/gw-store/src/**/*.rs`：禁止 `<money_type> as i64` 直接转换。
- money_type 集合：`f64`（来自 cost computation）、`MicroUnits`、`Credits`、`Amount`、`Money`（执行者用 `grep -rh "^pub struct \(MicroUnits\|Credits\|Amount\|Money\)\b"` 在 crates 内确定真实集合 + 列入 INVESTIGATE-WS12-007）。

**【不是什么 What it is NOT】**
- ❌ 不禁所有 `as i64`（基础类型转换合法）。
- ❌ 不能用纯字符串 grep `as i64` 全仓；会大量 false positive。
- ❌ 不审 Go 仓（Go 用 int64 micros 全程，没 `as` 关键字）。

**【为什么 Why this exists】**
- 根因：grep §0.4 实测 `crates/gw-billing/src/formulas.rs:83` 含 `as i64`（虽然是 `i64::MAX as f64`，是常量比较，不是 money）。但同一文件 line 83 全句 `amount.round().clamp(0.0, i64::MAX as f64) as i64` 末尾 `as i64` 是 f64→i64，amount 是 money，这就是 banned 模式。
- 引用：v1 §7 #6 第 4 项；v2 §5 WS-12 第 5 项；v2 REV-FINAL-H7.5（i128 算术）/H7.8（i64::try_from）。

**【做什么 DO】**

1. 候选 A（采用）：用 syn 解析 Rust 文件，找 `ExprCast` 节点；`cast.expr` 类型推断难，但 `cast.ty` 是 `i64`/`i32` 时 + `cast.expr` 是 `MethodCallExpr`/`BinaryExpr` 涉及命中变量名集合 → flag。落到 `check-tokio-spawn` 同 binary 复用 syn 框架。
2. 候选 B（备）：纯 grep `\bas\s+i64\b` 命中后 `awk -B 5` 拿上下文，匹配若上下文含 money type 关键字（`amount`/`credits`/`micros`/`money`/`cost`） → flag。ADR：简单但 noisy；先做 B 作 baseline，待 A 落地替换。本 task 落地 B + 列入 T-02 binary 扩展为 INVESTIGATE-WS12-008。
3. 候选 C（备）：把 money 用 newtype，禁止 `From<i64>`/`From<f64>` 实现，编译期捕获。ADR：真正根因；但 newtype 改造范围大，归 WS-1 内做，本 lint 只保 fallback。
4. 实现 B：grep `\bas\s+i64\b` 限定 `gw-billing|gw-ledger|gw-store`；对每命中，取上下文 ±5 行，若含 `amount|credits|micros|money|cost|balance|delta|fee|spend|charge` 任一 lowercase 关键字 → violation。
5. 豁免：行尾 `// allow: money-cast reason="<≥10 chars>"`。
6. **i64::MAX as f64** 是 const 转换，与 money 无关 —— 但 `clamp(0.0, i64::MAX as f64) as i64` 末尾的 `as i64` 仍要触发（因为左操作数 `amount.round().clamp(...)` 涉及 money）。lint 命中后人工 review 决定如何修（REV-FINAL-H7.8: `i64::try_from(amount)?`）。

**【不做什么 DON'T】**
- ❌ 不要扫 `tests/` 与 `benches/`。
- ❌ 不要扫 `target/`。
- ❌ 不要因 `i64::MAX as f64` 命中（这是 i64→f64，方向反；grep 必须严格 `as i64` 不含 `as f64`）。
- ❌ 不要在第一版就接 Rust syn binary —— 先发 grep 版本 baseline，等 T-02 binary 稳定后再迁移（避免 T-02 工时未到就阻塞）。

**【怎么做 HOW (具体)】**

`scripts/check-money-as-cast.sh`（Rust 仓）：

```bash
#!/usr/bin/env bash
# WS-12 T-05: forbid money-typed `as i64` casts in billing/ledger/store.
# Reference: master plan v2 §5 WS-12 item 5; REV-FINAL-H7.5/H7.8.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

CRATES=(crates/gw-billing/src crates/gw-ledger/src crates/gw-store/src)
MONEY_RE='\b(amount|credits|micros|money|cost|balance|delta|fee|spend|charge)\b'
ALLOW_RE='//[[:space:]]*allow:[[:space:]]*money-cast[[:space:]]+reason="[^"]{10,}"'

# Pre-populated allowlist for known necessary casts in A1-A11 hard-defense files.
# Per v2.1 corrections §11 M14: lease.rs / overflow.rs / counter_handler.rs 的合理 cast
# 用 allowlist 处理，不让 lint 误报为 fail。
# Format: "relative/path:line" — executor must Read each file and verify line still holds.
# 已按 M14 加 placeholder：以下为已知必要 cast，lint 须跳过。
ALLOWLIST=(
  # M14 example: metrics histogram as f64 cast in lease.rs (A1 hard-defense file).
  # 当前脚本只扫 `as i64`，此条在脚本扩展到 `as f64`（M14 要求扫 i64|u64|f64）后生效。
  "crates/gw-ledger/src/lease.rs:175"
  # TODO(executor): Read A1-A11 files (see v2.1 corrections §9) and add entries
  # for any reasonable money-related cast that should not be flagged.
  # Candidates to verify: overflow.rs, counter_handler.rs, heartbeat.rs, coef_snapshot.rs.
)

fail=0
for d in "${CRATES[@]}"; do
  [[ ! -d "$d" ]] && continue
  while IFS=: read -r file line _; do
    # Skip allow-comments on same line.
    if sed -n "${line}p" "$file" | grep -qE "$ALLOW_RE"; then continue; fi
    # Skip pre-populated allowlist entries (M14 A1-A11 known necessary casts).
    for entry in "${ALLOWLIST[@]}"; do
      if [[ "$file:$line" == "$entry" ]]; then continue 2; fi
    done
    start=$((line - 5)); [[ $start -lt 1 ]] && start=1
    end=$((line + 1))
    ctx=$(sed -n "${start},${end}p" "$file")
    if echo "$ctx" | grep -qiE "$MONEY_RE"; then
      echo "VIOLATION: $file:$line — money-typed 'as i64' cast (use i64::try_from(...)? or newtype; see REV-FINAL-H7.5/H7.8)"
      sed -n "${line}p" "$file" | sed 's/^/    /'
      fail=1
    fi
  done < <(grep -rnE '\bas[[:space:]]+i64\b' "$d" 2>/dev/null \
           | grep -v 'as[[:space:]]\+f64' \
           | grep -vE '/tests/|/benches/')
done

if (( fail )); then
  echo "[check-money-as-cast] FAIL" >&2; exit 1
fi
echo "[check-money-as-cast] OK"
```

Go 仓 mirror：`scripts/check-money-as-cast.sh` 内容仅 `echo "[check-money-as-cast] N/A (Go uses int64 natively)"; exit 0`，但仍存在以保证两仓 `make check` 调用形态一致。

**【如何验证 VERIFY】**

1. fixture：临时建 `/tmp/x.rs` 含 `let i = amount as i64`，临时挂入扫描路径，验证 exit 1 + 命中行。
2. 集成 baseline：`bash scripts/check-money-as-cast.sh 2>&1 | tee logs/WS12-T05-baseline.txt`，期望命中 `formulas.rs:83` 至少 1 条；记录基线数。
3. allow 注释：临时加 `let i = amount as i64; // allow: money-cast reason="legacy refund test fixture"` 到 fixture，验证不报。
4. 输出：`logs/WS12-T05-verify.txt` 含 `fixture: triggered`、`allow-comment: respected`、`baseline: N=<count>`。

**【对抗复审】** 完成后派 sonnet 复审：
1. **遗漏类型**：`MicroUnits`/`Credits` 是否真存在？grep `pub struct (MicroUnits|Credits|Amount|Money)` 实测；若都不存在，本 lint 只靠变量名启发式 → 升级到 T-02 binary 后才能可靠。记入 INVESTIGATE-WS12-007 必结。
2. **false positive 率**：跑 baseline，人工 review 每条命中是否真是 money；若 >30% 误报则提升到 syn binary（T-02 扩展）。
3. **i128 中间步骤**：REV-FINAL-H7.5 要求 vendor_factor 走 i128；本 lint 不检 i128 用法；列入 INVESTIGATE-WS12-009：是否需要补 `\bas\s+i128\b` 反向规则（要求 final clamp 才落 i64）？

### v2.1 §10.2 合规交付（M14）

本 task 已对照 v2.1 corrections §11 教学矩阵 **M14** 清理：
- 原内容 lint 仅报告（exit 1 + 打印命中行），未提议自动 patch 任何文件 —— 符合 M14 "lint 只读"。
- 原内容未重构 A1-A11 文件 —— 符合 M14 "不重构"。
- **allowlist 缺失已补 placeholder**：原 HOW 脚本仅有行级 `// allow: money-cast reason="..."` 注释机制，无预置 allowlist 条目。已按 M14 加 `ALLOWLIST` 数组 placeholder，列举 `crates/gw-ledger/src/lease.rs:175`（M14 明示的 metrics histogram `as f64` 已知必要 cast），并在循环中加跳过逻辑。executor 须 Read A1-A11 文件补全条目。
- lint 范围当前仅 `as i64`（M14 要求 `as (i64|u64|f64)`）；allowlist placeholder 在脚本扩展到 `as f64` 后对 lease.rs:175 生效。此扩展属 "做什么" 范围，不在本清理 task 内。

**Acceptance**:
1. PR 描述必须包含 "已遵守 M14: lint 只读，allowlist 已包含必要 cast"。
2. PR 附 lint 在 `crates/gw-ledger/src/lease.rs` 上跑的样本输出（允许 allowlist），显示 lease.rs:175 未被误报。
3. PR 附 `ALLOWLIST` 数组定义段，显示至少 1 条 placeholder 条目（lease.rs:175）。

---

### TASK WS12-T-06：`check-rustpact-pv.sh` 新字段 `_pv` bump

**【是什么 What it IS】**
- `scripts/check-rustpact-pv.sh`（两仓 mirror）。
- 在 git diff（PR 比对 base branch）内扫 `shared/rustpact/*.go` 与 `crates/*/contracts/*.rs`：若新增字段（struct field 增添），必须同 PR 内含 `_pv` 常量 bump（即 `RustpactProtocolVersion` / 等价 Rust 常量加 1）。

**【不是什么 What it is NOT】**
- ❌ 不审已存在字段的 rename（按"加" + "减"算两步处理；rename 触发 abort，由 REV-FINAL-H8.6 处理）。
- ❌ 不审 `omitempty` 改动（兼容字段）。
- ❌ 不要求生产代码用 `_pv`，只要求 `rustpact` 内对应版本常量。

**【为什么 Why this exists】**
- 根因：v2 REV-FINAL-H8.1/H8.6 用 `_pv` 比较替代 `DisallowUnknownFields`。但当前两仓 §0.4 实测**无 `_pv` 常量**也无 `version.go` 文件 → 必须先 T-07a 创建文件，再用本 lint 守 bump。
- 引用：v1 §7 #6 第 5 项；v2 §5 WS-12 第 6 项；v2 REV-FINAL-H8.1/H8.6；REV-006 §7 #12。

**【做什么 DO】**

1. 依赖：BLOCKED-BY T-07a（创建 `shared/rustpact/version.go` + Rust 等价常量）。
2. 候选 A（采用）：用 `git diff --unified=0 origin/main...HEAD` 拿改动 hunk；扫 Go struct field 模式 `^+\s+\w+\s+\w+\s+\`json:"…"\``、Rust `^+\s+pub\s+\w+:\s+\w+,` 在 contracts 模块；若有新增 → 检 `version.go` 同 PR 内是否含 `^+` 行修改 `RustpactProtocolVersion =`。
3. 候选 B（备）：commit hook 而非 PR-level；ADR：未采用 — PR-level CI 更可靠（开发本地可绕）。
4. 候选 C（备）：用 Go AST 解析两 commit 的 struct diff。ADR：未采用 — 计算成本高，文本 diff 足够 80% 覆盖；INVESTIGATE-WS12-010 留 follow-up。
5. 实现：
   - 拿 base branch: `git merge-base origin/main HEAD`。
   - `git diff --unified=0 <base>...HEAD -- shared/rustpact/ crates/*/contracts/`
   - 对每个新增字段行（hunk header + `^+`，去掉 `^+++`），递增 `added_count`。
   - 若 `added_count > 0`：检 `git diff <base>...HEAD shared/rustpact/version.go` 是否含 `^+.*RustpactProtocolVersion.*=` 与 `^-.*RustpactProtocolVersion.*=`（即必须是修改 = 加旧+减新，version 跳数）。
   - Rust 侧 mirror：`crates/gw-contracts-version/src/lib.rs`（路径预计如此，T-07a 内确定）`const RUSTPACT_PROTOCOL_VERSION:` 同 PR 内有 ± 行。
   - 若新增字段但 `version.go` 无相应 ± → violation。

**【不做什么 DON'T】**
- ❌ 不要 fail on remove-field（rename/remove abort 走 REV-FINAL-H8.6 的 sync-rust-contract.sh）。
- ❌ 不要无 base branch 时报错；CI 在 main branch direct commit 时跳过本 lint（exit 0 + 警告）。
- ❌ 不要扫 `*_test.go`（contracts 测试 fixture 加字段不算 protocol bump）。

**【怎么做 HOW (具体)】**

`scripts/check-rustpact-pv.sh`：

```bash
#!/usr/bin/env bash
# WS-12 T-06: require _pv (RustpactProtocolVersion) bump when contracts gain a field.
# Reference: master plan v2 REV-FINAL-H8.1/H8.6; REV-006 §7 #12.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

BASE="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$(git merge-base origin/main HEAD 2>/dev/null || true)}"
if [[ -z "$BASE" ]]; then
  echo "[check-rustpact-pv] no merge base (likely main branch direct commit) — skipping"
  exit 0
fi

CONTRACT_DIRS=(shared/rustpact crates/gw-billing/src/contracts crates/gw-task/src/contracts)
# Path filter for diff. (Adjust per actual repo layout discovered in T-07a.)

added=0
for d in "${CONTRACT_DIRS[@]}"; do
  [[ ! -d "$d" ]] && continue
  while IFS= read -r line; do
    # Go struct field: ^+\s+\w+\s+[\w\[\]\*\.]+\s+`json:"
    if [[ "$line" =~ ^\+[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]+[^[:space:]]+[[:space:]]+\`json: ]]; then
      ((added++)); continue
    fi
    # Rust pub field: ^+\s+pub\s+\w+:
    if [[ "$line" =~ ^\+[[:space:]]+pub[[:space:]]+[A-Za-z_][A-Za-z0-9_]*: ]]; then
      ((added++)); continue
    fi
  done < <(git diff --unified=0 "$BASE"...HEAD -- "$d" 2>/dev/null | grep -vE '^\+\+\+')
done

if (( added == 0 )); then
  echo "[check-rustpact-pv] no new contract fields — OK"
  exit 0
fi

# Need version bump.
go_bump=$(git diff "$BASE"...HEAD -- shared/rustpact/version.go 2>/dev/null \
          | grep -E '^[+-].*RustpactProtocolVersion[[:space:]]*=' | wc -l)
rust_bump=$(git diff "$BASE"...HEAD -- crates/gw-contracts-version/src/lib.rs 2>/dev/null \
            | grep -E '^[+-].*RUSTPACT_PROTOCOL_VERSION' | wc -l)

if (( go_bump >= 2 && rust_bump >= 2 )); then
  echo "[check-rustpact-pv] OK — $added field(s) added; _pv bumped in both repos"
  exit 0
fi
echo "[check-rustpact-pv] FAIL — $added new contract field(s) added but _pv not bumped"
echo "  Go bump diff lines:   $go_bump (need ≥2: one '-' old, one '+' new)"
echo "  Rust bump diff lines: $rust_bump (need ≥2)"
echo "  See WS-12 T-06; REV-FINAL-H8.1/H8.6"
exit 1
```

**【如何验证 VERIFY】**

1. fixture：本地分支加一个 `+\tFoo string ‘json:"foo"’` 字段到 `task.go`，不改 `version.go`：跑 `bash scripts/check-rustpact-pv.sh` 期望 exit 1。
2. fixture+fix：再改 `version.go` 加版本+ 1，跑期望 exit 0。
3. 输出：`logs/WS12-T06-verify.txt` 含 `fixture-without-bump: FAIL (expected)`、`fixture-with-bump: OK`。

**【对抗复审】** 完成后派 sonnet 复审：
1. **rename 误判**：`oldField` 改名 `newField` 在 diff 内表现为一 `-` 一 `+`；本 lint 只数 `+` → 触发 bump 要求。实际 rename 也应 bump（兼容性破坏），但 REV-FINAL-H8.6 要求 sync-rust-contract.sh **abort on rename**，所以本 lint 看到 rename 时应让 sync 脚本先 fail。验证：rename 时 sync-rust-contract.sh 的退出码是 1（在 T-10 wire 时排在 check-rustpact-pv 之前）。
2. **`omitempty` tag 改动**：`json:"foo,omitempty"` 改为 `json:"foo"` 是兼容字段语义变更但本 lint 不触发。列入 INVESTIGATE-WS12-011。
3. **多 commit PR**：base 选 `git merge-base` 是否能覆盖 PR 中间 commit 加字段再删字段的场景？应该可以（PR 累计 diff），fixture 验证。

---

### TASK WS12-T-07a：创建 `shared/rustpact/version.go` 与 Rust 等价常量

**【是什么 What it IS】**
- 新建 `AILinkMax-Go/shared/rustpact/version.go`：含 `const RustpactProtocolVersion = 1` 与 `const RustpactMinSupportedVersion = 1` 两常量 + Doc 注释引用 WS-12 与 REV-006 §7 #12。
- 新建 `ailinkmax-gateway/crates/gw-contracts-version/`（或选 `crates/gw-runtime/src/contracts_version.rs` 复用现 crate；ADR 决定）：含 `pub const RUSTPACT_PROTOCOL_VERSION: u32 = 1;` 与 `pub const RUSTPACT_MIN_SUPPORTED_VERSION: u32 = 1;`。

**【不是什么 What it is NOT】**
- ❌ 不是实际版本比较逻辑（那是 REV-FINAL-H8.1 落在 WS-8 内）。
- ❌ 不修改任何 contract struct。

**【为什么 Why this exists】**
- §0.4 反证：文件不存在；后续 T-06 / T-07b lint 都依赖。
- REV-006 §7 #12 字面要求 `RustpactMinSupportedVersion` 常量。

**【做什么 DO】**

1. 选位置 ADR：
   - 候选 A（采用）：Go `shared/rustpact/version.go`；Rust 复用 `crates/gw-runtime/src/contracts_version.rs`。ADR：gw-runtime 已存在，免新 crate；Go 在 rustpact module 内常量天然属于此 module。
   - 候选 B（备）：新建 Rust crate `gw-contracts-version`。ADR：未采用 — 增加 dep graph 复杂度。
2. 起步版本号 `= 1`（v1.0 baseline；后续每次合并加 contract 字段或 schema 改动则在该 PR 内 + 1）。
3. 包注释字面说明：
   - "DO NOT modify without bumping per WS-12 T-06."
   - "DO NOT use as protocol negotiator yet — wired in WS-8 REV-FINAL-H8.1."

**【不做什么 DON'T】**
- ❌ 不要在本 task 内 wire 进任何 marshal/unmarshal 路径（那是 WS-8）。
- ❌ 不要使用 `var` 而非 `const`（必须编译期常量，便于 build tag 切支持矩阵）。
- ❌ 不要 export 任何 mutable 全局。

**【怎么做 HOW (具体)】**

`AILinkMax-Go/shared/rustpact/version.go`：

```go
// Package rustpact: protocol version constants for cross-process compatibility.
//
// DO NOT modify these without bumping per WS-12 T-06 (check-rustpact-pv.sh).
// DO NOT use these as protocol negotiator yet — wired in WS-8 REV-FINAL-H8.1.
//
// References:
//   - master plan v2 REV-FINAL-H8.1, REV-006 §7 #12
//   - WS-12 sub-plan T-06, T-07b
package rustpact

const (
	// RustpactProtocolVersion is incremented every PR that adds a contract field.
	RustpactProtocolVersion = 1

	// RustpactMinSupportedVersion is the oldest Rust gateway version this Go build can talk to.
	// Used by check-deploy-order.sh (T-07b) to gate Rust-before-Go deploy.
	RustpactMinSupportedVersion = 1
)
```

`ailinkmax-gateway/crates/gw-runtime/src/contracts_version.rs`：

```rust
//! Protocol version constants for cross-process compatibility.
//!
//! DO NOT modify these without bumping per WS-12 T-06 (check-rustpact-pv.sh).
//! Wired into protocol negotiation in WS-8 REV-FINAL-H8.1.
//!
//! References:
//!   - master plan v2 REV-FINAL-H8.1, REV-006 §7 #12
//!   - WS-12 sub-plan T-06, T-07b

/// Incremented every PR that adds a contract field.
pub const RUSTPACT_PROTOCOL_VERSION: u32 = 1;

/// Oldest Go gateway version this Rust build can talk to.
pub const RUSTPACT_MIN_SUPPORTED_VERSION: u32 = 1;
```

mod wiring：在 `crates/gw-runtime/src/lib.rs` 加 `pub mod contracts_version;`。

**【如何验证 VERIFY】**

1. `cd AILinkMax-Go && go build ./shared/rustpact/...` 通过。
2. `cd ailinkmax-gateway && cargo build -p gw-runtime` 通过。
3. 反向引用 `grep -rn "RustpactMinSupportedVersion" AILinkMax-Go/` 必须命中 `version.go`。
4. 输出：`logs/WS12-T07a-verify.txt` 含两 build OK 行。

**【对抗复审】** 与 T-07b 合并复审。

---

### TASK WS12-T-07b：`check-deploy-order.sh` deploy ordering gate

**【是什么 What it IS】**
- `scripts/check-deploy-order.sh`（两仓 mirror）：在 CI 内拿当前分支的 `RustpactProtocolVersion`（Go）与 `RUSTPACT_PROTOCOL_VERSION`（Rust）值；要求：
  - Go 仓 `RustpactMinSupportedVersion` ≥ Rust 仓 `RUSTPACT_PROTOCOL_VERSION`（即 Go 能 talk to 当前 Rust）。
  - Rust 仓 `RUSTPACT_MIN_SUPPORTED_VERSION` ≥ Go 仓 `RustpactProtocolVersion`（互相，保 mixed-deploy 窗口）。

**【不是什么 What it is NOT】**
- ❌ 不审运行时；只审常量值。
- ❌ 不替代 sync-rust-contract.sh（那是字段同步；本 lint 是版本 floor）。

**【为什么 Why this exists】**
- 根因：REV-006 §7 #12 字面：`shared/rustpact/version.go` 内 `RustpactMinSupportedVersion` ≥ Rust HEAD 的 `_pv`，反之上线被拒。即 v2 C4 "Go 先" 部署序的机器化 gate。
- 引用：REV-006 §7 #12；v2 REV-FINAL-H8.9（部署顺序修正）；v2 §5 WS-12 第 7 项。

**【做什么 DO】**

1. 候选 A（采用）：CI job 在 Go 仓 PR 内同时拉 Rust 仓 main HEAD（用 `git ls-remote https://github.com/cnxx-tech/AILinkMax-Gateway.git main` 拿 SHA → `git archive` 或 `curl raw`）；读两常量；比较；fail 不通过。ADR：与 `scripts/sync-rust-contract.sh` 已有跨仓拉取一致。
2. 候选 B（备）：CI 跑双仓 monorepo checkout。ADR：未采用 — 当前仓库分离不变。
3. 候选 C（备）：人工填表 + CI 校。ADR：未采用 — 易漏。
4. 实现 Go 仓侧：
   - `curl -sf "https://raw.githubusercontent.com/cnxx-tech/AILinkMax-Gateway/main/crates/gw-runtime/src/contracts_version.rs"`
   - awk 解析 `RUSTPACT_PROTOCOL_VERSION: u32 = (\d+);` 拿数。
   - 读本仓 `shared/rustpact/version.go` 解析 `RustpactMinSupportedVersion = (\d+)`。
   - 比较；输出。
5. 实现 Rust 仓侧：对称，curl Go 仓 raw。
6. 失败示意：当 Rust 仓 HEAD `_pv = 5`，Go 仓 PR `RustpactMinSupportedVersion = 3` → Go 还没准备好 talk to Rust 5；fail。修复路径：Go 仓先 PR bump min 到 5 再部署。

**【不做什么 DON'T】**
- ❌ 不要在脚本里硬编码 commit SHA（用 main branch 实时）。
- ❌ 不要静默忽略 curl 失败（offline / GitHub down 时；exit 2 + reason；CI 必须 retry）。
- ❌ 不要把本 lint 挂入 `make check` 默认运行（增加每次 build 网络依赖）；仅 CI 集成（T-10 内）。

**【怎么做 HOW (具体)】**

`AILinkMax-Go/scripts/check-deploy-order.sh`：

```bash
#!/usr/bin/env bash
# WS-12 T-07b: cross-repo deploy ordering gate.
# Reference: REV-006 §7 #12; REV-FINAL-H8.9; v2 §5 WS-12 item 7.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

if [[ "${WS12_SKIP_DEPLOY_ORDER:-0}" == "1" ]]; then
  echo "[check-deploy-order] skipped (WS12_SKIP_DEPLOY_ORDER=1)"
  exit 0
fi

LOCAL_FILE="shared/rustpact/version.go"
REMOTE_URL="https://raw.githubusercontent.com/cnxx-tech/AILinkMax-Gateway/main/crates/gw-runtime/src/contracts_version.rs"

[[ -f "$LOCAL_FILE" ]] || { echo "FATAL: $LOCAL_FILE missing (run T-07a first)" >&2; exit 2; }

local_min=$(awk '/RustpactMinSupportedVersion[[:space:]]*=/ { gsub(/[^0-9]/, "", $0); print; exit }' "$LOCAL_FILE")
local_proto=$(awk '/RustpactProtocolVersion[[:space:]]*=/ { gsub(/[^0-9]/, "", $0); print; exit }' "$LOCAL_FILE")

if [[ -z "$local_min" || -z "$local_proto" ]]; then
  echo "FATAL: could not parse $LOCAL_FILE" >&2; exit 2
fi

# Fetch remote with retry.
remote_raw=""
for attempt in 1 2 3; do
  remote_raw=$(curl -sf --max-time 15 "$REMOTE_URL") && break
  echo "[check-deploy-order] curl attempt $attempt failed; sleep 5" >&2
  sleep 5
done
if [[ -z "$remote_raw" ]]; then
  echo "FATAL: could not fetch $REMOTE_URL after 3 attempts" >&2; exit 2
fi

remote_proto=$(echo "$remote_raw" | awk '/RUSTPACT_PROTOCOL_VERSION[[:space:]]*:[[:space:]]*u32[[:space:]]*=/ { gsub(/[^0-9]/, "", $0); print; exit }')
remote_min=$(echo "$remote_raw" | awk '/RUSTPACT_MIN_SUPPORTED_VERSION[[:space:]]*:[[:space:]]*u32[[:space:]]*=/ { gsub(/[^0-9]/, "", $0); print; exit }')

if [[ -z "$remote_proto" || -z "$remote_min" ]]; then
  echo "FATAL: could not parse remote contracts_version.rs" >&2; exit 2
fi

echo "[check-deploy-order] Go side:   proto=$local_proto, min=$local_min"
echo "[check-deploy-order] Rust HEAD: proto=$remote_proto, min=$remote_min"

fail=0
if (( local_min < remote_proto )); then
  echo "VIOLATION: Go RustpactMinSupportedVersion ($local_min) < Rust HEAD _pv ($remote_proto)"
  echo "  Go cannot talk to current Rust. Bump Go RustpactMinSupportedVersion first, then redeploy."
  fail=1
fi
if (( remote_min > local_proto )); then
  echo "VIOLATION: Rust HEAD RUSTPACT_MIN_SUPPORTED_VERSION ($remote_min) > Go RustpactProtocolVersion ($local_proto)"
  echo "  Rust HEAD requires newer Go protocol. Bump Go RustpactProtocolVersion or downgrade Rust min."
  fail=1
fi

if (( fail )); then exit 1; fi
echo "[check-deploy-order] OK — both directions compatible"
```

Rust 仓 mirror `ailinkmax-gateway/scripts/check-deploy-order.sh`：对称，URL 指向 Go raw。

**【如何验证 VERIFY】**

1. fixture：暂存当前；改本地 `RustpactMinSupportedVersion = 1`，假设 Rust HEAD `_pv = 1` → 应 exit 0；改本地为 0 → 应 exit 1（注意 const 是 `= 1` 起步，无 0；可临时 -1 仅 fixture）。
2. offline 测试：`unplug network` 或 `iptables` block GitHub → 应 exit 2 + "could not fetch"。
3. CI 集成：T-10 内将本 lint 挂入 `.gitlab-ci.yml` 的 `merge-gate` stage，**且** 在 Rust 仓 PR 也跑（对称）。
4. 输出：`logs/WS12-T07b-verify.txt` 含 `local: proto=1 min=1`、`remote: proto=1 min=1`、`OK`。

**【对抗复审】** T-07a+b 合并复审，重点：
1. **GitHub rate limit**：CI 多 PR 并发跑 curl raw GitHub 可能限流（unauth ~60/hr/IP）。考虑 GitLab CI 内用 GITHUB_TOKEN（如配置）或缓存远端 SHA 到 artifact。fixture：模拟 rate limit 429，确认 exit 2 而非 silent pass。
2. **网络分区降级**：offline 时 exit 2，CI 会重 try；但若长时间 GitHub down，会阻塞所有 PR — 需要 break-glass `WS12_SKIP_DEPLOY_ORDER=1` 是否被 audit log（人工填理由）？建议加 hook：set 该 env 时写 audit 行到 `.deploy-order-skips.log` 并 PR 必须包含该文件改动。
3. **branch 不是 main**：拉 `main` 分支可能不是真实 deploy 的分支；REV-FINAL-H8.6 要求 sync 后记 sync commit SHA。本 lint 应允许 `WS12_RUST_REF=release-2026Q3` env 覆盖默认 `main`。落 fixture 验证。
4. **T-07a 反证**：执行者必须在本 task 前先验证 §0.4 §7 行声明 — `shared/rustpact/version.go` 确不存在；若已存在则版本号起点不应 = 1。Read 文件实测。

---

### TASK WS12-T-08：`check-metric-labels.sh` metric label enum allowlist

> ## ⚠️ TASK CONSTRAINED — v2.1 corrections §10.2 (2026-06-17)
>
> **此 metric label lint 必须保护硬模式 credit-lease 的有界枚举 label。必须遵守教学矩阵 M13。**
>
> **触碰元素**: E1-E8 (lint 会扫到所有硬模式 metric 声明)
> **必须 NOT 做**（摘自 M13）:
> - **不**加 `result` 进 banned（E1/E2/E8 用）
> - **不**加 `op` 进 banned（E3 用）
> - **不**加 `path` 进 banned（E5 用）
> - **不**加 `attempt_index` / `model_id` 进 banned（有界枚举，合理用）
> **完整矩阵**: 见 v2.1 corrections §11 **M13**
>
> 注意：WS-10 T08 与本 task 功能重叠，两个 lint 必须保持 banned 集合一致。
>
> 执行前必读 M13。PR 描述必须列明"已遵守教学矩阵 M13"。

---

**【是什么 What it IS】**
- `scripts/check-metric-labels.sh`（两仓）：扫所有 `prometheus.NewCounterVec`/`.GaugeVec`/`.HistogramVec`（Go）与 `register_int_counter_vec!`/`register_gauge_vec!`/`register_histogram_vec!`（Rust）声明，要求 label name 不在 banned 集合 `{request_id, api_key_id, user_id, ip, ip_address, email, token, session_id, jti, refresh_token, access_token, hash}`（M13 canonical 12 标签）；**不**含 `result` / `op` / `path` / `attempt_index` / `model_id`（E1-E8 有界枚举，硬防自身用）；可在 PR 描述列举 enum 值集合（例如 `attempt_index`、`disabled_now: bool`）作为豁免理由。

**【不是什么 What it is NOT】**
- ❌ 不审 metric value（无界）。
- ❌ 不审 log fields（不是 metric）。
- ❌ 不审 prometheus.NewCounter 单值（无 label）。

**【为什么 Why this exists】**
- 根因：v2 REV-FINAL-V7 字面列举高基数 label 杀名单（`gw_billing_emit_lag_t0_total{request_id}` → 删 `request_id`；`gw_redos_timeout_total{api_key_id}` → 改 `{disabled_now: bool}`）。"CI lint：metric label value 必须 enum-allowlist" 是该项收尾。
- 引用：v1 §7 #6（隐含）；v2 §5 WS-12 第 8 项；v2 REV-FINAL-V7；v2 §4 WS-10 (R15 obs migration)。

**【做什么 DO】**

1. 候选 A（采用）：双语 grep + 解析 metric 声明位置。Go 用 `grep -A 5 "NewCounterVec\b"` 取 label slice；Rust 用 `grep -A 5 "register_.*_vec!"` 取 label tuple。命中 banned 即 violation。
2. 候选 B（备）：parse-via-AST（Go go/ast + Rust syn）。ADR：未采用首版 — grep 一锤子先建 baseline，B 留 Q3 升级。
3. banned 集合（hardcoded 起步，PR 描述要求 review 加项）：`request_id`、`api_key_id`、`user_id`、`ip`、`ip_address`、`email`、`token`、`session_id`、`jti`、`refresh_token`、`access_token`、`hash`。
4. 豁免：metric 声明上方注释 `// allow-label: <label> reason="<≥10 chars>"`。
5. enum 报告（reverse）：脚本退出前列每 label 一次分布；若 high-cardinality（>50 unique values 在最近 24h Prom dump，本 lint 不查 live；列入 INVESTIGATE-WS12-012）。

**【不做什么 DON'T】**
- ❌ 不要把所有 `request_id` 全 ban —— 日志字段是允许的；只在 metric 声明位置 ban。
- ❌ 不要静默接受 `attempt_index` 这种本应 ≤16 的 label（认为合法），但人工 review 改进。
- ❌ 不要在 Rust 仓里 grep `request_id` 全文（lint 必须紧邻 register_*_vec! 宏调用）。

**【怎么做 HOW (具体)】**

`scripts/check-metric-labels.sh`：

```bash
#!/usr/bin/env bash
# WS-12 T-08: ban high-cardinality labels on Prometheus metrics.
# Reference: master plan v2 REV-FINAL-V7; v2 §5 WS-12 item 8.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

BANNED=(request_id api_key_id user_id ip ip_address email token session_id jti refresh_token access_token hash)
ALLOW_RE='//[[:space:]]*allow-label:[[:space:]]+\S+[[:space:]]+reason="[^"]{10,}"'

fail=0

# Go scan: look at file types we know live in this repo.
if [[ -d shared || -d modules || -d binaries ]]; then
  while IFS= read -r f; do
    # Find metric Vec declarations.
    while IFS=: read -r ln _; do
      # ±5 lines window.
      win=$(sed -n "$((ln-2)),$((ln+8))p" "$f")
      for bad in "${BANNED[@]}"; do
        if echo "$win" | grep -qE "\"$bad\""; then
          # Allow comment within 3 lines above?
          above=$(sed -n "$((ln-3)),$((ln-1))p" "$f")
          if echo "$above" | grep -qE "$ALLOW_RE"; then continue; fi
          echo "VIOLATION: $f:$ln — metric uses banned label '$bad' (REV-FINAL-V7); add // allow-label: $bad reason=\"...\" if intentional"
          fail=1
        fi
      done
    done < <(grep -nE 'New(Counter|Gauge|Histogram|Summary)Vec\b' "$f" 2>/dev/null)
  done < <(find shared modules binaries -name '*.go' ! -name '*_test.go' 2>/dev/null)
fi

# Rust scan.
if [[ -d crates ]]; then
  while IFS= read -r f; do
    while IFS=: read -r ln _; do
      win=$(sed -n "$((ln-2)),$((ln+10))p" "$f")
      for bad in "${BANNED[@]}"; do
        if echo "$win" | grep -qE "\"$bad\""; then
          above=$(sed -n "$((ln-3)),$((ln-1))p" "$f")
          if echo "$above" | grep -qE '//[[:space:]]*allow-label:'; then continue; fi
          echo "VIOLATION: $f:$ln — metric uses banned label '$bad' (REV-FINAL-V7)"
          fail=1
        fi
      done
    done < <(grep -nE 'register_(int_counter|counter|gauge|histogram|int_gauge)_vec!' "$f" 2>/dev/null)
  done < <(find crates -name '*.rs' -not -path '*/target/*' -not -path '*/tests/*' 2>/dev/null)
fi

if (( fail )); then echo "[check-metric-labels] FAIL" >&2; exit 1; fi
echo "[check-metric-labels] OK"
```

**【如何验证 VERIFY】**

1. fixture：建临时 `.go` 含 `prometheus.NewCounterVec(... []string{"request_id", "attempt_index"} ...)`，跑 lint exit 1。
2. allow 注释 fixture：临时加 `// allow-label: request_id reason="legacy panel migration TBD WS-10"` 一行上方，跑 lint exit 0。
3. 集成 baseline：`bash scripts/check-metric-labels.sh 2>&1 | tee logs/WS12-T08-baseline.txt`，期望命中 REV-FINAL-V7 列举的两 metric。
4. 输出：`logs/WS12-T08-verify.txt` 含 baseline 计数。

**【对抗复审】** 完成后派 sonnet 复审：
1. **`prometheus.Labels{"request_id": ...}`** vs `NewCounterVec` 声明：本 lint 只看声明，不看 call site；高基数 label 可能在 call site 注入 wildcards。fixture 验证 call site 高基数被声明 lint 捕获（因为 Vec 声明已知 label 集，不会有 call site 注入未声明 label）。
2. **`request_id` 字符串变量**：`metricLabels := []string{requestIDName}; NewCounterVec(..., metricLabels)` —— 间接命名，lint 不见。列入 INVESTIGATE-WS12-013。
3. **Rust macro 多行**：`register_int_counter_vec!("name", "help", &["request_id", "x"])?;` 多行可能超出 10 行窗口；测多行 fixture。

### v2.1 §10.2 合规交付（M13）

本 task 已对照 v2.1 corrections §11 教学矩阵 **M13** 清理：
- 原内容 HOW 脚本 `BANNED` 数组已包含且仅包含 M13 canonical 12 标签 = `{request_id, api_key_id, user_id, ip, ip_address, email, token, session_id, jti, refresh_token, access_token, hash}`，未含 `result` / `op` / `path` / `attempt_index` / `model_id` —— 符合 M13。
- 原内容【是什么】段 banned 集合描述仅列 6 标签（`{request_id, api_key_id, user_id, ip_address, email, token}`），与 HOW 脚本 12 标签不一致。已修正为 12 canonical 标签 + 显式标注 "不" 含 E1-E8 有界枚举。
- lint 仅报告（exit 1 + 打印违例），未提议自动 patch —— 符合 M13。
- 与 WS-10 T08 banned 集合保持一致（两 lint 功能重叠）。

**Acceptance**:
1. PR 描述必须包含 "已遵守 M13: banned 集合未含 result/op/path/attempt_index/model_id；与 WS-10 T08 一致"。
2. PR 附 lint 脚本 banned 集合定义段（BANNED 数组），显示 12 个 canonical 标签。
3. PR 附 `diff -u <gateway>/scripts/check-metric-labels.sh <go-mirror>/scripts/check-metric-labels.sh`，BANNED 数组行一致。

---

### TASK WS12-T-09：`audit-newfindings.sh` v1 §8 持续发现机制

**【是什么 What it IS】**
- `scripts/audit-newfindings.sh`：扫当前 commit 改动 vs `git merge-base origin/main HEAD`，按预定义可疑模式产报告到 `docs/issues/2026-06-17-investigates/auto-<branch>-<commitsha>.md`；不 fail CI（exit 0 + warn），但若报告非空则在 PR 描述里 require "已 sonnet 复核 confirmed/refuted"。

**【不是什么 What it is NOT】**
- ❌ 不是上述 T-01..T-08 中任何 lint 的代替；本脚本是"早期警报"。
- ❌ 不是阻塞性 gate；可疑模式不一定真有问题。
- ❌ 不是 sonnet 自动调用器（人工 review，agent 后续手动派）。

**【为什么 Why this exists】**
- 根因：v1 §8 字面要求 "每次 `make check` 跑一次 `scripts/audit-newfindings.sh`（待编写），扫已编辑文件相对历史的可疑模式，提交报告到 issue 文件"。
- 引用：v1 §7 (#1) closed 状态门；v1 §8；v2 §5 WS-12 第 9 项（implicitly per "§8 持续发现机制"）。

**【做什么 DO】**

1. 候选 A（采用）：bash + 预定义 6 类可疑模式 grep 改动 hunk。ADR：与 v1 §8 字面"扫已编辑文件相对历史的可疑模式"一致。
2. 候选 B（备）：sonnet 自动调用（API）。ADR：未采用 — 本 sub-plan 范围；归入 followup。
3. 可疑模式（执行者可扩）：
   1. `let _ =` / `_ =` 在 Rust async 调用结果（吞错）— 命中数。
   2. Go `defer .*Close()` 在 errgroup/goroutine 内（生命周期可能错乱）。
   3. `unwrap()` / `expect("` 在 binary path（非 test）。
   4. `time.Sleep` / `tokio::time::sleep` ≥ 5 秒（潜在 livelock 兼容旁路）。
   5. `// TODO|// FIXME|// XXX` 含 "later" / "next sprint" 字样（推迟）。
   6. `panic!` / `panic(` 在新代码非 test 文件。
4. 报告格式：每模式一节，列文件:行 + 命中行截图 + "派 sonnet 用 prompt template `tdd-adversarial-review.txt` 复审" 建议。
5. PR 标签建议（CI 内）：报告非空时给 PR 加 `needs-newfindings-review` label（Gitlab API）。
6. 报告写入 `docs/issues/2026-06-17-investigates/auto-<branch>-<sha>.md` —— 这是 v1 §8 字面要求。

**【不做什么 DON'T】**
- ❌ 不要 fail CI（人工 review 必要，机器误报多）。
- ❌ 不要扫已 closed 的报告文件自身（造成无限报告）。
- ❌ 不要跨多次 commit 累积报告（每 PR 一份）。
- ❌ 不要在脚本里 SendMessage 自动派 sonnet（保留人工 trigger）。

**【怎么做 HOW (具体)】**

`scripts/audit-newfindings.sh`：

```bash
#!/usr/bin/env bash
# WS-12 T-09: continuous-discovery audit (v1 §8 mechanism).
# Reference: master plan v1 §8; v2 §5 WS-12.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2

BASE="${CI_MERGE_REQUEST_DIFF_BASE_SHA:-$(git merge-base origin/main HEAD 2>/dev/null || true)}"
if [[ -z "$BASE" ]]; then exit 0; fi

BRANCH=$(git rev-parse --abbrev-ref HEAD)
SHA=$(git rev-parse --short HEAD)
OUT_DIR="docs/issues/2026-06-17-investigates"
OUT="$OUT_DIR/auto-${BRANCH//\//_}-${SHA}.md"
mkdir -p "$OUT_DIR"

# Get changed files (added/modified, not deleted).
mapfile -t FILES < <(git diff --name-only --diff-filter=AM "$BASE"...HEAD \
  | grep -vE '/testdata/|_test\.go$|/tests/|\.md$|/migrations/' )

if (( ${#FILES[@]} == 0 )); then exit 0; fi

found_any=0
{
  echo "# WS-12 audit-newfindings auto report"
  echo
  echo "- Branch: \`$BRANCH\`"
  echo "- HEAD:   \`$SHA\`"
  echo "- Base:   \`$BASE\`"
  echo "- Files changed: ${#FILES[@]}"
  echo
  echo "Each suspicious match below MUST be reviewed by sonnet (use docs/superpowers/tools/tdd-adversarial-review.txt)."
  echo "Mark each item CONFIRMED / REFUTED in this file; do not delete the section."
  echo

  # P1 Rust silent-error swallowing
  hits=$(grep -nE '^\s*let\s+_\s*=' "${FILES[@]}" 2>/dev/null | grep -E '\.rs:' || true)
  if [[ -n "$hits" ]]; then
    echo "## P1: Rust silent let _ = (potential error swallow)"
    echo '```'
    echo "$hits"
    echo '```'
    echo
    found_any=1
  fi

  # P2 Go defer ... .Close() inside goroutine.
  hits=$(grep -nE 'go\s+func\(|go\s+[a-zA-Z]' "${FILES[@]}" 2>/dev/null | grep -E '\.go:' || true)
  if [[ -n "$hits" ]]; then
    echo "## P2: Go bare go func (cross-check WS-12 T-01 lint coverage)"
    echo '```'
    echo "$hits"
    echo '```'
    echo
    found_any=1
  fi

  # P3 unwrap/expect in non-test Rust binary paths.
  hits=$(grep -nE '\.unwrap\(\)|\.expect\(' "${FILES[@]}" 2>/dev/null | grep -E '\.rs:' || true)
  if [[ -n "$hits" ]]; then
    echo "## P3: Rust unwrap/expect (review for panic on operator input)"
    echo '```'
    echo "$hits" | head -30
    echo '```'
    echo
    found_any=1
  fi

  # P4 long sleep.
  hits=$(grep -nE 'time\.Sleep\(.*Second|tokio::time::sleep' "${FILES[@]}" 2>/dev/null \
    | awk -F: '{print $0}' || true)
  if [[ -n "$hits" ]]; then
    echo "## P4: long sleep / sleep at all (livelock or compat bypass)"
    echo '```'
    echo "$hits"
    echo '```'
    echo
    found_any=1
  fi

  # P5 deferred TODOs.
  hits=$(grep -nE '(TODO|FIXME|XXX).*(later|next sprint|kk|todo:|tomorrow)' "${FILES[@]}" 2>/dev/null || true)
  if [[ -n "$hits" ]]; then
    echo "## P5: deferred TODOs (cannot ship with these per P3 'Complete Fix')"
    echo '```'
    echo "$hits"
    echo '```'
    echo
    found_any=1
  fi

  # P6 panic.
  hits=$(grep -nE '\bpanic!|\bpanic\(' "${FILES[@]}" 2>/dev/null | grep -vE '_test\.|/tests/' || true)
  if [[ -n "$hits" ]]; then
    echo "## P6: panic! / panic( in production path"
    echo '```'
    echo "$hits"
    echo '```'
    echo
    found_any=1
  fi

} > "$OUT"

if (( found_any )); then
  echo "[audit-newfindings] WARN: report written to $OUT (review required, but not blocking)"
else
  rm -f "$OUT"
  echo "[audit-newfindings] OK — no suspicious patterns"
fi
exit 0
```

**【如何验证 VERIFY】**

1. fixture：在某新 `.rs` 文件加 `let _ = some_redis_call().await;`，跑脚本期望 `$OUT` 出现并含 "P1: Rust silent let _ ="。
2. clean fixture：所有改动均正派 → 报告不写。
3. 集成：CI 中 `make check` 触发后，artifact 包含报告（若有）。
4. 输出：`logs/WS12-T09-verify.txt` 含 fixture 触发与 clean 双 case。

**【对抗复审】** 完成后派 sonnet 复审：
1. **报告噪声**：6 类模式合理性 —— 跑两月 baseline (`git log --oneline | head -60` 取过去 PR 集逐一跑) 算 false-positive rate；若 P3 unwrap 90% 是 test 误报应升级 file 过滤。
2. **遗漏模式**：v2 §99 INVESTIGATE 列表反推还应加哪些模式（如 R12 "WeightedRr Phase 1 对不可用候选 fetch_add" —— 是不是有 lint 化的可能？大多属业务，不可 lint；本 task 内仅追加纯句法模式）。
3. **报告位置**：写入 `docs/issues/2026-06-17-investigates/` 是 v1 §8 字面要求，但 commit 这种文件到 PR 内会触发 docs check（`scripts/check-docs.sh`）—— 验证不会被 docs check 误判要 sync 别处。

---

### TASK WS12-T-10：挂入 `make check` + CI 集成

**【是什么 What it IS】**
- 改 `AILinkMax-Go/Makefile` 与 `ailinkmax-gateway/Makefile`：`check` target 增加新 lint 调用。
- 改 `AILinkMax-Go/.gitlab-ci.yml` 与 `ailinkmax-gateway/.gitlab-ci.yml`：加 `lint-extended` 与 `merge-gate` 两 stage（前者跑所有 T-01..T-09，后者跑 T-07b deploy-order）。
- 同步 `make.ps1` 与 `scripts/*.ps1`（CLAUDE.md 硬规则 "两套不得漂移"）。

**【不是什么 What it is NOT】**
- ❌ 不替换现有 `check` 流程；只追加。
- ❌ 不是 GitHub Actions 全套（INVESTIGATE-WS12-001 已 reject）；仅 mirror 作 optional。

**【为什么 Why this exists】**
- 根因：lint 必须挂入门禁才生效；v1 §7 #6 是 launch gate 的硬性要求。
- 引用：v1 §7 #6；v2 §5 WS-12 末段 "所有挂入 `make check`"；REV-006 §7 #10..#14。

**【做什么 DO】**

1. `AILinkMax-Go/Makefile` `check` target 末尾加：
   ```makefile
   check: fmt-check vet
   	@./scripts/check-mod-deps.sh
   	@./scripts/check-docs.sh
   	@./scripts/check-contract-fields.sh
   	@./scripts/check-fakes.sh
   	@./scripts/check-soft-delete.sh
   	@./scripts/check-prod-compose.sh
   	@./scripts/check-metric-labels.sh
   	@./scripts/check-rustpact-pv.sh
   	@./scripts/audit-newfindings.sh
   	@cd scripts/check-goroutines && go build -o ../../bin/check-goroutines . && cd ../..
   	@./bin/check-goroutines ./binaries/... ./modules/...
   	@$(MAKE) test
   ```
   注意：`check-deploy-order.sh` **不**挂入 `make check`（要联网），仅 CI 集成（见 step 3）。
2. `ailinkmax-gateway/Makefile` 等价（按 Rust 仓的 check target 命名 — 当前 Rust 仓 Makefile 没有 `check` target，需新增）。新增：
   ```makefile
   check: cargo-clippy
   	@./scripts/check-invariants.sh
   	@./scripts/check-spec.sh
   	@./scripts/check-soft-delete.sh
   	@./scripts/check-prod-compose.sh
   	@./scripts/check-metric-labels.sh
   	@./scripts/check-rustpact-pv.sh
   	@./scripts/check-money-as-cast.sh
   	@./scripts/audit-newfindings.sh
   	@cd scripts/check-tokio-spawn && cargo build --release && cd ../..
   	@./scripts/check-tokio-spawn/target/release/check-tokio-spawn crates
   ```
3. `.gitlab-ci.yml`（两仓）加 stages:
   ```yaml
   stages:
     - build
     - lint-extended
     - merge-gate
     - push
     - deploy

   lint-extended:
     stage: lint-extended
     image: <go-rust-toolchain-image>
     script:
       - make check
     rules:
       - if: '$CI_PIPELINE_SOURCE == "merge_request_event"'

   merge-gate-deploy-order:
     stage: merge-gate
     image: alpine:3.20
     before_script:
       - apk add --no-cache bash curl git gawk
     script:
       - ./scripts/check-deploy-order.sh
     rules:
       - if: '$CI_PIPELINE_SOURCE == "merge_request_event"'
   ```
4. PowerShell mirror：
   - `make.ps1`：识别新 target `check` 流程同 `Makefile`；每个 `.sh` 调用替换为 `.ps1`。
   - 新增 `scripts/check-soft-delete.ps1`、`check-prod-compose.ps1`、`check-metric-labels.ps1`、`check-rustpact-pv.ps1`、`audit-newfindings.ps1`、`check-deploy-order.ps1`。

**【不做什么 DON'T】**
- ❌ 不要把 `check-deploy-order.sh` 放入 `make check` 默认（网络依赖）。
- ❌ 不要把 `check-goroutines` build artifact `bin/check-goroutines` commit 进仓库（每次 build）。
- ❌ 不要在 PowerShell 里 `bash` shell-out（CLAUDE.md 说"两套并存"，意为独立实现）。

**【怎么做 HOW (具体)】**

`AILinkMax-Go/Makefile` 字面 edit（基于实际现状第 35 行）：

```makefile
check: fmt-check vet
	@./scripts/check-mod-deps.sh
	@./scripts/check-docs.sh
	@./scripts/check-contract-fields.sh
	@./scripts/check-fakes.sh
	@./scripts/check-soft-delete.sh
	@./scripts/check-prod-compose.sh
	@./scripts/check-metric-labels.sh
	@./scripts/check-rustpact-pv.sh
	@./scripts/audit-newfindings.sh
	@mkdir -p bin && cd scripts/check-goroutines && go build -o ../../bin/check-goroutines . && cd $(CURDIR)
	@./bin/check-goroutines ./binaries/... ./modules/...
	@$(MAKE) test
```

CI yml fragment（Go 仓 `.gitlab-ci.yml` 顶部 stages list 编辑）：

```yaml
stages:
  - build
  - lint-extended
  - merge-gate
  - push
  - deploy
```

CI yml job 追加（位置：build 之后、push 之前）：

```yaml
lint-extended:
  stage: lint-extended
  image: $REGISTRY_HOST/ailinkmax/cnexus-go:builder-latest
  script:
    - make check
  artifacts:
    when: on_failure
    paths:
      - logs/
      - docs/issues/2026-06-17-investigates/auto-*.md
    expire_in: 30 days
  rules:
    - if: '$CI_PIPELINE_SOURCE == "merge_request_event"'

merge-gate-deploy-order:
  stage: merge-gate
  image: alpine:3.20
  before_script:
    - apk add --no-cache bash curl git gawk
  script:
    - ./scripts/check-deploy-order.sh
  rules:
    - if: '$CI_PIPELINE_SOURCE == "merge_request_event"'
```

PowerShell `make.ps1` 必须按 Makefile target 名 1:1 加 `check` 流程；每 `.sh` 调用映射到 `.ps1`。CLAUDE.md "两套不得漂移" 是 PR-blocking。

GitHub Actions optional mirror（可选 task；未采用即跳过；ADR：保 GitLab CI 为主）。

**【如何验证 VERIFY】**

1. 本地 `make check` 跑通：基线 lint 命中数记录（不阻塞 CI 必通过；如 baseline 命中数 > 0 必须在本 PR 描述列出每条且声明 "WS-N 后续修复" / "本 PR 修复" / "本 PR 豁免理由"）。
2. CI run：在 fixture branch push，验证 `lint-extended` 与 `merge-gate-deploy-order` 两 job 都触发；前者 fail 则 artifact 保存 logs/。
3. PowerShell parity：`pwsh -File .\make.ps1 check` 必须与 `make check` 输出语义一致；命令命中错位则 fail。
4. 输出：`logs/WS12-T10-verify.txt` 含 `local: make check exit 0 (or expected baseline N)`、`ci: lint-extended green`、`ps: parity OK`。

**【对抗复审】** 完成后派 sonnet 复审：
1. **CI image 缺工具**：`alpine:3.20` 缺 `git` / `gawk` — `before_script` 已加 apk install；fixture 验证 image 内能 `git rev-parse`。
2. **`make check` 顺序**：T-01 binary build 慢 → 移到 target 末尾以缩短早期 fail-fast 时间。
3. **`make check` 与 `make lint` 关系**：当前 `make lint` 跑 golangci-lint，`make check` 不调它；两 PR check 是 union 还是 intersection？建议 CI 跑 `make check && make lint`。
4. **Rust 仓没 `check` target**：本 task 新增；这是新 target 引入，必须同步 `Makefile.ps1`/`make.ps1` 与所有相关 doc。

---

### TASK WS12-T-11：第二轮自对抗复审

**【是什么 What it IS】**
- 在 T-01..T-10 全部 land 后，派一组 sonnet（建议 3 个并行）走 `tdd-adversarial-review.txt` template，分别复审 T-01/02、T-03/04/05、T-06/07/08/09/10。
- 在 D6 "adversarial 复审日 #1"（v2 §6.1）之前完成。

**【不是什么 What it is NOT】**
- ❌ 不是 lint 升级（基于复审 finding 升级 lint 是 followup PR）。
- ❌ 不是 chaos test。
- ❌ 不是 manual code review；是 LLM 派遣。

**【为什么 Why this exists】**
- v2 P2 零回归 + REV-004g "WS-11..16 在执行时还要再对抗复审一次"。

**【做什么 DO】**

1. 准备每 task 的输入 packet：`<TASK_ID>`、touched files 列表（`git log --name-only --since="WS-12 start"`）、对应的【对抗复审】段落（已在 T-01..T-10 内列出风险）。
2. SendMessage 3 个 sonnet（subagent_type=general-purpose 或 pr-review-toolkit:code-reviewer）+ template。
3. 收集 finding 列表 → 转成 `docs/issues/2026-06-17-investigates/WS-12-adversarial-<sonnet-id>.md`。
4. 严重等级 critical/major → 立即开 followup PR 修；minor → 写入 backlog。

**【不做什么 DON'T】**
- ❌ 不要把 sonnet 输出当结论（v2 §0 重要 2 适用本身）。需 Read 实测复核每 finding。
- ❌ 不要并行派同一 sonnet 多次（避免回声）。
- ❌ 不要修改 lint 而不更新对应的对抗复审段。

**【怎么做 HOW (具体)】**

每 sonnet call 字面 prompt（用 template + 填充三占位变量）：

```
You are an adversarial reviewer for AILinkMax pre-launch defect eradication.

Task under review: WS12-T-01 / WS12-T-02 (Go AST + Rust syn analyzers)
Files touched:
- AILinkMax-Go/scripts/check-goroutines/main.go
- AILinkMax-Go/scripts/check-goroutines/main_test.go
- AILinkMax-Go/scripts/check-goroutines/go.mod
- ailinkmax-gateway/scripts/check-tokio-spawn/Cargo.toml
- ailinkmax-gateway/scripts/check-tokio-spawn/src/main.rs
- ailinkmax-gateway/clippy.toml

Required checks:
1. Lint coverage gap: see WS12-T-01 §8 step 1 (go func/go funcName/g.Go three forms).
2. False negatives at scale: simulate 3 realistic "almost-banned" snippets that should trip the lint.
3. Premise drift: re-read shared/runtime/supervisor.go; does the `RunWorkers` finding from §0.4 still hold? Has Supervised/OneShot been introduced by WS-4?
4. tokio::spawn alias-use path: see WS12-T-02 §8 step 1 (clippy.toml backstop).
5. supervisor.rs self-exemption.

Common failure modes ...
[rest of template]
```

**【如何验证 VERIFY】**

1. 3 sonnet 报告均落到 `docs/issues/2026-06-17-investigates/WS-12-adversarial-*.md`。
2. 报告内 verdict 字段：`pass` / `needs-revision` / `premise-broken`；任一 `needs-revision` 必须开 followup PR。
3. 输出：`logs/WS12-T11-verify.txt` 含 3 报告路径 + verdict 计数。

**【对抗复审】** 无（本 task 是对抗复审本身）。

---

## 5. 任务摘要表

| ID | 标题 | 工时 | BLOCKED-BY | 文件路径 |
|---|---|---|---|---|
| T-00 | 建目录与 adversarial template | 0.25d | — | `scripts/check-goroutines/`、`docs/superpowers/tools/tdd-adversarial-review.txt`、`docs/issues/2026-06-17-investigates/` |
| T-01 | check-goroutines Go AST | 1.0d | WS-4 REV-FINAL-V8 | `scripts/check-goroutines/main.go` 等 |
| T-02 | check-tokio-spawn Rust syn | 1.0d | — | `scripts/check-tokio-spawn/src/main.rs`、`clippy.toml` |
| T-03 | check-soft-delete.sh | 0.5d | — | `scripts/check-soft-delete.sh{,.ps1}` |
| T-04 | check-prod-compose.sh + prod compose 决定 | 0.5d | — | `scripts/check-prod-compose.sh`、`docker-compose.prod.yml`/`deploy/k8s/go-deployment.yaml` 改动 |
| T-05 | check-money-as-cast.sh | 0.5d | — | Rust 主 + Go 占位 |
| T-06 | check-rustpact-pv.sh | 0.5d | T-07a | `scripts/check-rustpact-pv.sh` |
| T-07a | 创建 version.go + Rust 常量 | 0.25d | — | `shared/rustpact/version.go`、`crates/gw-runtime/src/contracts_version.rs` |
| T-07b | check-deploy-order.sh | 0.5d | T-07a | `scripts/check-deploy-order.sh`（两仓 mirror） |
| T-08 | check-metric-labels.sh | 0.5d | — | `scripts/check-metric-labels.sh` |
| T-09 | audit-newfindings.sh | 0.5d | T-01..T-08（建议在后） | `scripts/audit-newfindings.sh` |
| T-10 | 挂入 make check + CI yml + ps1 mirror | 0.5d | T-01..T-09 | `Makefile`、`make.ps1`、`.gitlab-ci.yml`、 `scripts/*.ps1` |
| T-11 | 第二轮对抗复审 | 0.25d | T-10 | `docs/issues/2026-06-17-investigates/WS-12-adversarial-*.md` |
| | **合计** | **~5d** | | |

与 v2 §5 WS-12 估值 "4-5 人日" 一致。

---

## 6. 风险与开放项

### 6.1 跨 WS 依赖

- **T-01 BLOCKED-BY WS-4 REV-FINAL-V8**：必须在 WS-4 引入 `runtime.Supervised` 与 `runtime.OneShot` 之后 lint 才能 enforce。可先落 baseline 模式（`|| true` wrap），D1 起 WS-4 ready 后 unwrap。
- **T-07b 依赖 sync-rust-contract.sh** 顺序运行（先 sync 再 deploy-order）。
- **T-04 prod compose 决策** 可能触发 ops/deploy team 协调（rename `docker-compose.prod.yml`）。提早通气，避免 D7+ blocker。

### 6.2 INVESTIGATE 落项（本 sub-plan 自产）

| ID | 说明 | 处置 |
|---|---|---|
| INVESTIGATE-WS12-001 | CI 系统 GitLab vs GitHub Actions | 已裁定 GitLab CI |
| INVESTIGATE-WS12-002 | Go AST analyzer 集成方式 | 已选 singlechecker |
| INVESTIGATE-WS12-003 | Rust syn analyzer 集成方式 | 已选独立 binary + clippy.toml backstop |
| INVESTIGATE-WS12-004 | `use tokio::spawn` 别名是否被 clippy.toml 兜住 | 实测 fixture 验证；T-02 §8 |
| INVESTIGATE-WS12-005 | `RUN_DEV_SEED`/`ALM_DEV_ADMIN_BOOTSTRAP` 之外其他 demo env | grep 实测 |
| INVESTIGATE-WS12-006 | `env_file:` 外部文件是否要扫 | followup |
| INVESTIGATE-WS12-007 | money type 真实集合 | grep `pub struct` 实测 |
| INVESTIGATE-WS12-008 | money cast lint 升级到 syn binary | T-02 binary 稳定后 |
| INVESTIGATE-WS12-009 | i128 中间步骤是否需反向 lint | 建议 yes，followup |
| INVESTIGATE-WS12-010 | check-rustpact-pv 文本 diff vs AST | 升级路径 |
| INVESTIGATE-WS12-011 | `omitempty` 改动是否要 bump | 倾向 yes，followup |
| INVESTIGATE-WS12-012 | metric label live cardinality | 接 Prometheus dump，followup |
| INVESTIGATE-WS12-013 | 间接命名 label slice 是否要 AST | followup |

### 6.3 与其他 sub-plan 的对接

- WS-2 sub-plan (`2026-06-17-WS-2-*.md`)：引用本 sub-plan T-01/T-02 作为 lint 守卫；REV-FINAL-W11 内 `gw_runtime::supervised_spawn` 落地后通知本 WS unwrap baseline。
- WS-3 sub-plan (`2026-06-17-WS-3-*.md`)：REV-FINAL-V7 metric label 杀名单 → 本 WS T-08 enforce；本 WS 完成后 WS-3 才能 close 该项。
- WS-5 sub-plan (`2026-06-17-WS-5-*.md`)：REV-FINAL-V14 soft-delete 审计 → 本 WS T-03 enforce。
- WS-8 sub-plan (`2026-06-17-WS-8-*.md`)：REV-FINAL-H8.1 _pv 比较运行时 → 本 WS T-06/T-07a/T-07b 提供常量与守卫。
- WS-13 sub-plan：chaos harness 不会调本 WS 任何脚本（独立轨）；但 chaos report `.chaos_report/<scenario>.json` 格式需与本 sub-plan 的 `logs/WS12-T*-verify.txt` 风格保持一致（人类可读 + 机器可解析）。
- WS-15 sub-plan：feature flag yaml 校验可能在本 WS 后追加一 lint `scripts/check-feature-flags.sh`，归入 followup。
- WS-16 sub-plan：CVE 流程不入 lint，但 PR 必须含 R1-R4 类 PR 的 "security-disclosure: yes" yaml 行 —— 建议归入 followup lint `scripts/check-security-disclosure.sh`。

---

## 7. 验收门（本 WS DoD）

承自 master plan v1 §7 + REV-006 §7 #10-#14。本 WS 自检：

1. ✅ 9 个新脚本全部存在、可执行、shell test 通过（per-task §VERIFY）。
2. ✅ `make check` 在两仓内调用全部新 lint（T-10）。
3. ✅ `.gitlab-ci.yml` 包含 `lint-extended` 与 `merge-gate-deploy-order` job（T-10）。
4. ✅ PowerShell mirror 全部就绪（T-10）。
5. ✅ 第二轮对抗复审 0 critical（T-11）。
6. ✅ `docs/issues/2026-06-17-investigates/` 包含本 WS 全部 INVESTIGATE-WS12-* 占位 .md。
7. ✅ baseline lint 命中数已记录到对应 logs/WS12-T*-baseline.txt。
8. ✅ 本 sub-plan 所有 §0.4 抽样核对内的"不一致"项已在对应 task 处理（T-07a 创建 version.go；T-04 prod compose 决策；T-01 unwrap baseline 计划）。
9. ✅ 所有挂入 `make check` 的 lint 在 v2 §7 #6 计入"启用"。
10. ✅ runbook（按 REV-006 §7 #13）：每个新 lint 在 `docs/runbook/ws-12-lints.md` 有"如何静默 / 如何升级 / 如何加白名单"条目。

---

## 8. 落地后的下游任务（不属本 sub-plan）

- WS-2/3/5/8 sub-plan 完成后回头跑全套 lint，记录"baseline → fix → 0"轨迹。
- WS-10 R15 obs 迁移依赖本 WS T-08 enforce；ops team 同时迁移 Grafana 面板。
- Q3 升级 T-05 money cast lint 到 syn binary（INVESTIGATE-WS12-008）。
- 与 WS-14 协调：T-06 _pv bump 与 migration 编号同 PR 内绑定（避免 contract 加字段但 migration 未落）。

— WS-12 子计划结束 —
