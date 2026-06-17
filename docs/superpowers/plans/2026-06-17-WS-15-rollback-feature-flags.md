# WS-15 TDD Sub-Plan — Rollback / Feature-Flag Strategy

> **Parent**: `docs/issues/2026-06-17-prelaunch-eradication-master-plan-v2.md` §5 WS-15 + REV-004d + REV-004e
> **Prereq v1**: `docs/issues/2026-06-17-prelaunch-eradication-master-plan.md` §7 #9 ("回滚计划")
> **Track**: G (SRE)，与 WS-10 + WS-14 同人
> **Estimate**: 3–4 人日
> **Audience**: 不假设熟悉 Rust / Go 的执行者
> **Critical reminder (§0 重要 2)**: sonnet 输出是 **假设**。本 sub-plan 已在 hypothesis-verification 阶段核对：
> - Go 端 config loader = **koanf v2** (TOML + `ALM_` env)，文件 `shared/platform/config/config.go`。**不是 viper**（v2 §5 WS-15 笔误）。`viper` 在 Go 仓不存在；不要按 v2 字面引入 viper，仍用 koanf。
> - Rust 端 config loader = **figment**（TOML + `GW_` env，`__` 分隔），文件 `crates/gw-gateway/src/config.rs`。
> - 现有 boolean-style flag 已在 figment Config 中存在（如 `[tunnel].enabled`、`[egress].disabled`、`[security].trust_xff`、`[signature].required`、`[protection].enabled`），所以 WS-15 **不引入新框架**，只把所有 risky flag 集中到 `[flags]` 命名空间 + 同时落 `flags/*.yaml` 作为人类可读的"flag 注册表"。
> - 命名差异铁律：Rust `[flags]` 表用 `GW_FLAGS__<NAME>` 覆盖；Go `[flags]` 表用 `ALM_FLAGS__<NAME>` 覆盖。YAML 注册表是双仓共用的 ledger，TOML 才是实际 runtime 配置。

---

## 0. 阅读指令（必读，否则 PR 拒）

每个 task 严格 **8 段**：
1. **【是什么 What it IS】** — 一句话定义产出物 + 字面文件路径。
2. **【不是什么 What it is NOT】** — 范围外 + 反模式。
3. **【为什么 Why】** — 根因，引用原审计 ID + sonnet ID。
4. **【做什么 DO】** — 顺序动作列表。
5. **【不做什么 DON'T】** — 诱人的错误方案。
6. **【怎么做 HOW】** — 字面代码 + 字面命令 + 字面 file:line。
7. **【如何验证 VERIFY】** — 测试命令 + 期望字面输出（禁止 "passes tests"）。
8. **【回滚 / 默认值 / mixed-version 约束 Rollback】** — flag 默认值、跨仓同步要求、降级路径。

WS-15 特有要求（覆盖所有 task）：
- **默认值最保守**：所有新 flag 在落地 PR 中必须默认到 "no new behavior"（与 v1 一致）。
- **每个 PR description 必填**："flag 名 + 默认值 + 计划切换日期 + 回滚步骤"。`scripts/check-pr-description.sh`（T15.7）在 CI 强制。
- **跨仓同步约束**：license + R1 strict_jwt + R7 502 + M2 webhook 5xx 涉及 Go+Rust 双侧；切换顺序见 T15.9 deploy ordering。

---

## 1. Task Index

| ID | 名称 | 工时 | 依赖 | 跨仓 |
|---|---|---|---|---|
| T15.1 | 设计 `flags/` YAML 注册表 schema + 4 个初始 yaml | 0.5d | — | 双仓 mirror |
| T15.2 | Go `[flags]` 配置块 + `shared/platform/config` 解析 | 0.5d | T15.1 | Go-only |
| T15.3 | Rust `[flags]` 配置块 + `gw-gateway/config.rs` 解析 | 0.5d | T15.1 | Rust-only |
| T15.4 | `flags/license_enforce.yaml` enum 解析 + 双仓接线（REV-004e） | 0.5d | T15.2 + T15.3 | 双仓 |
| T15.5 | `flags/jwt_strict.yaml` bool + Rust 接线（REV-004d / REV-FINAL-V19） | 0.25d | T15.3 | Rust 主，Go 协 |
| T15.6 | `flags/payment_webhook_5xx.yaml` bool + Go 接线（M2） | 0.25d | T15.2 | Go-only |
| T15.7 | `flags/r7_502_on_parse_fail.yaml` bool + Rust 接线（R7/T17） | 0.25d | T15.3 | Rust-only |
| T15.8 | `scripts/check-pr-description.sh` + CI gate | 0.25d | T15.1 | 双仓 mirror |
| T15.9 | Runbook `docs/runbook/feature-flags.md` + deploy ordering 表 | 0.5d | T15.4–T15.7 | 双仓 mirror |
| T15.10 | Mixed-version flag 默认值 lint（防止 Go-new ↔ Rust-old 时 flag 被偷打开） | 0.25d | T15.2 + T15.3 + T15.9 | 双仓 mirror |

合计 **3.75 人日**，落在 v2 估值 3–4d 区间。

---

## T15.1 — `flags/` YAML 注册表 schema + 4 个初始 yaml

### 1. 【是什么】
双仓共享的 flag **注册表**目录。每个 risky flag 一个 yaml，记录"谁 / 默认值 / 切换计划 / 回滚 / 跨仓侧"。**这是 ledger，不是运行时配置源** —— 运行时仍走 koanf/figment 的 `[flags]` 表（T15.2/T15.3）。

字面路径（双仓完全 mirror，byte-identical）：
- `AILinkMax-Go/flags/license_enforce.yaml`
- `AILinkMax-Go/flags/jwt_strict.yaml`
- `AILinkMax-Go/flags/payment_webhook_5xx.yaml`
- `AILinkMax-Go/flags/r7_502_on_parse_fail.yaml`
- `AILinkMax-Go/flags/_schema.yaml`（schema spec）
- `ailinkmax-gateway/flags/*.yaml`（同四份）

### 2. 【不是什么】
- **不是 runtime config**：进程启动 **不** 读这个目录；只是 PR-time 审计 ledger。
- **不是 Open Feature / LaunchDarkly 接入**：禁止引入第三方 SDK；本项目 flag 都是 boot-time、reload 走 SIGHUP（与现有 `[tunnel].enabled` 同模式）。
- **不是 dynamic flag**：所有 flag 改值要重启进程；不写 hot-reload 通道。

### 3. 【为什么】
v1 §7 #9 只说 "每个 PR 有 revert plan" 但未给具体 spec；v2 §5 WS-15 + REV-004d/e 明确要求 yaml 注册表 + PR description 强制 + runbook。原因：sonnet `ace010fa51cdc9da1` (Meta 审查) 发现 v1 没有 "flag inventory"，导致每个 risky 改动 PR 各自定义 toggle，无法在 launch gate 统一审计是否所有 flag 默认关闭。

### 4. 【做什么】
1. 写 `_schema.yaml`，定义注册表字段：`name`、`type`、`default`、`owner_ws`、`go_repo_path`、`rust_repo_path`、`switch_plan`、`rollback_steps`、`mixed_version_safe`。
2. 为 4 个初始 flag 各写 yaml，**默认值全部最保守**（warn / false）。
3. `make check`（Go 仓）/ `cargo xtask check-flags`（Rust 仓）解析 yaml，schema 不合规则失败。

### 5. 【不做什么】
- 不要在 yaml 里放 secret。
- 不要在 yaml 里写"自动按时间切换"的逻辑 —— 切换始终走 PR + ops 决定，yaml 只是 ledger。
- 不要让 yaml 与 TOML 默认值漂移；T15.10 lint 强制一致。

### 6. 【怎么做】

`flags/_schema.yaml`：
```yaml
# Schema for flags/*.yaml — WS-15 ledger format
# Validated by scripts/check-flags-schema.sh (T15.8) at make check.
fields:
  name:         { required: true, type: string, pattern: '^[a-z][a-z0-9_]*$' }
  type:         { required: true, enum: [bool, enum] }
  enum_values:  { required_if_type: enum, type: list }
  default:      { required: true }                      # value must appear in enum_values when type=enum
  owner_ws:     { required: true, type: string }        # e.g. "WS-1, WS-15"
  description:  { required: true, type: string }
  go_repo_path: { required: false, type: string }       # null if Rust-only
  rust_repo_path:{ required: false, type: string }      # null if Go-only
  switch_plan:
    introduced_release: { required: true, type: string }
    warn_window_days:   { required: false, type: int }  # for enum flags
    enforce_release:    { required: false, type: string }
    delete_legacy_release: { required: false, type: string }
  rollback_steps:       { required: true, type: list }  # ordered shell-level steps
  mixed_version_safe:   { required: true, type: bool }  # T15.10 lint reads this
```

`flags/license_enforce.yaml`：
```yaml
name: license_enforce
type: enum
enum_values: [disabled, warn, enforce]
default: warn   # REV-004e: 14-day warn-only window precedes enforce
owner_ws: "WS-15, WS-16"
description: |
  Gateway license verification mode. `disabled` skips all checks (legacy);
  `warn` logs violations but allows traffic; `enforce` returns 402 GW_LICENSE_INVALID.
  Affects both Go admin API (binaries/web) and Rust gateway request_filter.
go_repo_path:   modules/license/business/enforce.go
rust_repo_path: crates/gw-auth/src/license.rs
switch_plan:
  introduced_release: N+0   # warn on day-1
  warn_window_days:   14
  enforce_release:    N+1
  delete_legacy_release: N+2
rollback_steps:
  - "Set ALM_FLAGS__LICENSE_ENFORCE=warn on Go pods (rolling restart, 30s/pod)"
  - "Set GW_FLAGS__LICENSE_ENFORCE=warn on Rust pods (rolling restart, 30s/pod)"
  - "Watch metric gateway_license_violation_total{mode=\"warn\"} return to baseline (~5 min)"
  - "If violations persist, set both to disabled (same rolling-restart procedure)"
mixed_version_safe: true   # warn is no-op on enforcement; safe in any mixed window
```

`flags/jwt_strict.yaml`：
```yaml
name: jwt_strict
type: bool
default: false  # REV-004d / REV-FINAL-V19: false in N+0, true in N+1, legacy deleted N+2
owner_ws: "WS-5, WS-15"
description: |
  When true, gw-auth's verify_jwt path rejects any token whose `iss`/`aud`
  fails strict match (R1 fix). When false (legacy), mismatches log a warning
  but allow the request — the panic-on-mismatch behaviour of the original
  jwt_validator is gated by this flag.
go_repo_path:   null
rust_repo_path: crates/gw-auth/src/jwt.rs
switch_plan:
  introduced_release: N+0
  enforce_release:    N+1   # default flipped to true
  delete_legacy_release: N+2
rollback_steps:
  - "Set GW_FLAGS__JWT_STRICT=false on Rust pods (rolling restart)"
  - "Watch gw_auth_jwt_strict_reject_total drop to 0 in <2 min"
  - "If clients still 401, also clear JWT cache: redis-cli DEL 'jwt:cache:*'"
mixed_version_safe: true   # Go side is unaware of this flag; Rust legacy path = old behaviour
```

`flags/payment_webhook_5xx.yaml`：
```yaml
name: payment_webhook_5xx
type: bool
default: false   # M2: keep legacy 2xx-with-error-body behaviour by default
owner_ws: "WS-1, WS-15"
description: |
  When true, payment webhook handlers return HTTP 5xx on persistent DB
  errors instead of the legacy 2xx-with-error-body (which caused providers
  to mark the webhook as delivered). M2 root-cause fix.
go_repo_path:   modules/payment/api/webhook.go
rust_repo_path: null
switch_plan:
  introduced_release: N+0
  enforce_release:    N+1   # default true
  delete_legacy_release: N+3
rollback_steps:
  - "Set ALM_FLAGS__PAYMENT_WEBHOOK_5XX=false on Go pods (rolling restart)"
  - "Watch payment_webhook_5xx_total drop to 0 (1 min)"
  - "Any 5xx already sent to provider: idempotency_keys row exists → provider retry is safe (verified by M2 InTx commit)"
mixed_version_safe: true   # Rust side untouched
```

`flags/r7_502_on_parse_fail.yaml`：
```yaml
name: r7_502_on_parse_fail
type: bool
default: false   # R7: legacy returns 200 with synthesized empty body
owner_ws: "WS-3, WS-6, WS-15"
description: |
  When true, gateway returns HTTP 502 GW_UPSTREAM_PARSE_FAIL when the
  upstream response body fails to parse (R7 / T17). When false (legacy),
  it silently returns 200 with an empty body, hiding upstream regressions.
go_repo_path:   null   # contract clients must consume 502 — see WS-11 contract update
rust_repo_path: crates/gw-proxy/src/filters/response.rs
switch_plan:
  introduced_release: N+0
  enforce_release:    N+1
  delete_legacy_release: N+2
rollback_steps:
  - "Set GW_FLAGS__R7_502_ON_PARSE_FAIL=false on Rust pods (rolling restart)"
  - "Watch gw_response_502_parse_fail_total drop to 0"
  - "Frontend: if blocking customer traffic, also redeploy frontend from previous tag (WS-11)"
mixed_version_safe: false   # frontend contract changed; mixed window needs Go new + Rust old
```

`scripts/check-flags-schema.sh`（双仓共用脚本，mirror）：
```bash
#!/usr/bin/env bash
# Validate flags/*.yaml against flags/_schema.yaml.
# Called by `make check` (Go) and `cargo xtask check-flags` (Rust).
set -euo pipefail
cd "$(dirname "$0")/.."
python3 -c "
import sys, yaml, pathlib, re
root = pathlib.Path('flags')
schema = yaml.safe_load((root/'_schema.yaml').read_text())['fields']
ok = True
for p in sorted(root.glob('*.yaml')):
    if p.name == '_schema.yaml': continue
    f = yaml.safe_load(p.read_text())
    # required-field check
    for k,spec in schema.items():
        if spec.get('required') and k not in f:
            print(f'{p}: missing required field {k}'); ok=False
    # name pattern
    if not re.match(r'^[a-z][a-z0-9_]*\$', f.get('name','')):
        print(f'{p}: invalid name pattern'); ok=False
    # filename ↔ name match
    if f['name'] != p.stem:
        print(f'{p}: name {f[\"name\"]} != filename'); ok=False
    # enum default ∈ enum_values
    if f['type'] == 'enum' and f['default'] not in f.get('enum_values',[]):
        print(f'{p}: default {f[\"default\"]} not in enum_values'); ok=False
sys.exit(0 if ok else 1)
"
```

### 7. 【如何验证】
- 命令：`bash scripts/check-flags-schema.sh` — 期望 exit 0、stdout 空。
- 反向测试：把 `flags/license_enforce.yaml` 的 `default` 改成 `enforce_strictly`（不在 enum 中），重跑 — 期望 exit 1 + stderr 含 `default enforce_strictly not in enum_values`。
- 命令：`make check`（Go 仓）— 期望 PASS（已挂入新 step）。
- 命令：`cargo xtask check-flags`（Rust 仓）— 期望 `OK: 4 flag files validated`。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- 单纯 yaml 修改可直接 revert PR；yaml 不影响运行时。
- yaml 注册表 `mixed_version_safe: true/false` 字段被 T15.10 lint 读取，强制 mixed window 期间 flag 必须为 false-equivalent（disabled/false/warn）。
- 默认值约束：本 task 落地后，4 个 yaml 默认全部 = 保守值（warn / false / false / false）；后续翻转走独立 PR。

---

## T15.2 — Go `[flags]` 配置块 + `shared/platform/config` 解析

### 1. 【是什么】
在 Go 端把"runtime feature flags"集中到 koanf 的 `flags` 命名空间。**字面文件**：
- 新增 `shared/platform/config/flags.go`
- 改 `shared/platform/config/config.go`（无 struct，只有 `Load`；保持现状，新增独立 `LoadFlags()`）
- 新增 `shared/platform/config/flags_test.go`

### 2. 【不是什么】
- 不引入新依赖（koanf 已在 `shared/platform/config/config.go:1-10`，直接复用）。
- 不替换 viper —— 仓里**没有 viper**，v2 §5 WS-15 "viper" 一词是笔误，本 sub-plan 改正。
- 不做 reload：只在进程启动时读一次；改值靠 rolling restart。

### 3. 【为什么】
v2 REV-004d/e + REV-004f/g 要求 risky 行为有 flag gate；现有 `[tunnel].enabled` 等 boolean flag 已分散到各 module config，所以本 task 集中到统一 `[flags]` namespace，方便 T15.8 PR-description gate + T15.10 mixed-version lint 一次性扫描。

### 4. 【做什么】
1. 在 `shared/platform/config/flags.go` 中定义 `type Flags struct { ... }` 与四个字段。
2. 在 `flags.go` 中实现 `LoadFlags(k *koanf.Koanf) (Flags, error)`，校验默认值与 yaml ledger 一致。
3. 在 `flags_test.go` 中写表驱动测试：默认值断言 + 环境变量覆盖断言 + 非法 enum 报错断言。
4. 更新 `docs/architecture.md` 模块表：新增 `flags` 子节点说明 `[flags]` 配置块。

### 5. 【不做什么】
- 不要把 flag 散落到各 module config（与 `[tunnel]` 不同 —— `[tunnel]` 是 subsystem 配置，flag 是行为 gate）。
- 不要在 `Flags` struct 里加方法（避免业务逻辑沉淀到 config 层）。
- 不要在 koanf 解析时调网络。

### 6. 【怎么做】

`shared/platform/config/flags.go`：
```go
package config

import (
	"fmt"

	"github.com/knadh/koanf/v2"
)

// LicenseEnforceMode mirrors flags/license_enforce.yaml enum_values.
type LicenseEnforceMode string

const (
	LicenseDisabled LicenseEnforceMode = "disabled"
	LicenseWarn     LicenseEnforceMode = "warn"
	LicenseEnforce  LicenseEnforceMode = "enforce"
)

// Flags is the typed view of the [flags] config block. Field names must match
// flags/*.yaml file stems (license_enforce → LicenseEnforce); the yaml ledger
// is authoritative — when adding a new flag, update both yaml and this struct
// in the same commit (T15.10 lint enforces parity).
type Flags struct {
	LicenseEnforce     LicenseEnforceMode `koanf:"license_enforce"`
	JWTStrict          bool               `koanf:"jwt_strict"`           // unused on Go side; declared for parity
	PaymentWebhook5xx  bool               `koanf:"payment_webhook_5xx"`
	R7_502OnParseFail  bool               `koanf:"r7_502_on_parse_fail"` // unused on Go side; declared for parity
}

// LoadFlags reads the [flags] block, applies conservative defaults, and
// validates enum values. Conservative defaults match flags/*.yaml `default`.
func LoadFlags(k *koanf.Koanf) (Flags, error) {
	f := Flags{
		LicenseEnforce:    LicenseWarn, // matches flags/license_enforce.yaml default
		JWTStrict:         false,
		PaymentWebhook5xx: false,
		R7_502OnParseFail: false,
	}
	if k.Exists("flags.license_enforce") {
		f.LicenseEnforce = LicenseEnforceMode(k.String("flags.license_enforce"))
	}
	switch f.LicenseEnforce {
	case LicenseDisabled, LicenseWarn, LicenseEnforce:
	default:
		return Flags{}, fmt.Errorf("config: flags.license_enforce=%q invalid (want disabled|warn|enforce)", f.LicenseEnforce)
	}
	if k.Exists("flags.jwt_strict") {
		f.JWTStrict = k.Bool("flags.jwt_strict")
	}
	if k.Exists("flags.payment_webhook_5xx") {
		f.PaymentWebhook5xx = k.Bool("flags.payment_webhook_5xx")
	}
	if k.Exists("flags.r7_502_on_parse_fail") {
		f.R7_502OnParseFail = k.Bool("flags.r7_502_on_parse_fail")
	}
	return f, nil
}
```

`shared/platform/config/flags_test.go`：
```go
package config

import (
	"testing"

	"github.com/knadh/koanf/v2"
)

func TestLoadFlags_Defaults(t *testing.T) {
	k := koanf.New(".")
	f, err := LoadFlags(k)
	if err != nil { t.Fatal(err) }
	if f.LicenseEnforce != LicenseWarn { t.Errorf("license default = %q, want warn", f.LicenseEnforce) }
	if f.JWTStrict || f.PaymentWebhook5xx || f.R7_502OnParseFail {
		t.Errorf("bool flags should default to false, got %+v", f)
	}
}

func TestLoadFlags_EnvOverride(t *testing.T) {
	t.Setenv("ALM_FLAGS__LICENSE_ENFORCE", "enforce")
	t.Setenv("ALM_FLAGS__PAYMENT_WEBHOOK_5XX", "true")
	k, err := Load("") // empty TOML path, env-only
	if err != nil { t.Fatal(err) }
	f, err := LoadFlags(k)
	if err != nil { t.Fatal(err) }
	if f.LicenseEnforce != LicenseEnforce { t.Errorf("want enforce, got %q", f.LicenseEnforce) }
	if !f.PaymentWebhook5xx { t.Errorf("want true") }
}

func TestLoadFlags_InvalidEnum(t *testing.T) {
	t.Setenv("ALM_FLAGS__LICENSE_ENFORCE", "ENFORCE_STRICTLY")
	k, err := Load("")
	if err != nil { t.Fatal(err) }
	if _, err := LoadFlags(k); err == nil {
		t.Fatal("expected error for invalid enum")
	}
}
```

### 7. 【如何验证】
- 命令：`go test ./shared/platform/config/... -run TestLoadFlags -v` — 期望 `--- PASS: TestLoadFlags_Defaults`、`TestLoadFlags_EnvOverride`、`TestLoadFlags_InvalidEnum` 全部 PASS。
- 命令：`go vet ./shared/platform/config/...` — 期望 0 警告。
- 命令：`golangci-lint run ./shared/platform/config/...` — 期望 0 issue。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- Revert：单 PR revert `flags.go` + `flags_test.go`；调用方在 T15.4–T15.6 才接线，本 task 单独 revert 无副作用。
- 默认值：全部 == yaml ledger `default`；CI lint T15.10 在 `make check` 阶段断言 yaml.default == Go default。
- Mixed-version：Go 旧（无 `[flags]` 块）与 Go 新共存时，旧版完全不读，新版默认 warn/false，**无行为差异**。

---

## T15.3 — Rust `[flags]` 配置块 + `gw-gateway/config.rs` 解析

### 1. 【是什么】
对应 Go T15.2 的 Rust 版本。**字面文件**：
- 改 `crates/gw-gateway/src/config.rs`（在现有 `Config` struct 加 `pub flags: FlagsCfg` 字段）
- 在同文件追加 `FlagsCfg` struct（紧贴 `ControlPlaneCfg` 之后，按字母序约 line 90）
- 改 `config/default.toml`（追加 `[flags]` block）

### 2. 【不是什么】
- 不引入新 crate；figment 已是基线（`crates/gw-gateway/src/config.rs:13-15`）。
- 不让 `FlagsCfg` 持有 secret —— flag 不是密钥。
- 不在 `validate()` 里做"业务"校验，只做 enum / range 校验。

### 3. 【为什么】
v2 REV-004d/e + REV-FINAL-V19。Rust gateway 中 R1（jwt_strict）与 R7（502 on parse fail）需要 boot-time gate；现有 `[tunnel]`/`[egress]`/`[security]` 已是同模式，本 task 沿用 figment 范式。

### 4. 【做什么】
1. 在 `crates/gw-gateway/src/config.rs` 第 ~73 行（紧跟 `pub control_plane: ControlPlaneCfg`）追加 `#[serde(default)] pub flags: FlagsCfg,`。
2. 在同文件追加 `FlagsCfg` + `LicenseEnforceMode` enum + 默认值常量。
3. 在 `Config::validate()` 中追加 flags 校验（enum 自动由 serde 校验，但加 `bail!` 兜底）。
4. 在 `config/default.toml` 追加 `[flags]` block（4 个 key，全部保守值）。
5. 在 `crates/gw-gateway/src/config.rs` 文末 `#[cfg(test)] mod flags_cfg_tests` 新增 4 个测试。

### 5. 【不做什么】
- 不要把 `LicenseEnforceMode` 放 `gw-core`；它属于 gateway 启动配置，按现有 pattern 留在 `gw-gateway/config.rs`。
- 不要使用 `#[serde(deny_unknown_fields)]` 在 `FlagsCfg` —— 后续加 flag 不希望旧二进制启动失败。
- 不要让 flag 值通过 control-plane 热更新 —— v1 spec 是 boot-time only。

### 6. 【怎么做】

在 `crates/gw-gateway/src/config.rs` 第 24 行附近（struct 字段列表里），追加：
```rust
    /// Rollback / feature-flag block (WS-15). All defaults must match
    /// the yaml ledger under `flags/*.yaml` (T15.10 lint enforces parity).
    #[serde(default)]
    pub flags: FlagsCfg,
```

在 `ControlPlaneCfg` 之后（约 line 185）追加：
```rust
/// `[flags]` block — WS-15 rollback registry. Boot-time only; flipping a
/// value requires rolling restart. Defaults are intentionally conservative
/// (no new behaviour active); the yaml ledger in `flags/*.yaml` is the
/// source of truth for switch plans and rollback steps.
#[derive(Debug, Clone, Deserialize)]
pub struct FlagsCfg {
    #[serde(default = "default_license_enforce_mode")]
    pub license_enforce: LicenseEnforceMode,
    #[serde(default)]
    pub jwt_strict: bool,
    /// Declared for parity with Go side; Rust does not consume this flag.
    #[serde(default)]
    pub payment_webhook_5xx: bool,
    #[serde(default)]
    pub r7_502_on_parse_fail: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LicenseEnforceMode {
    Disabled,
    Warn,
    Enforce,
}

fn default_license_enforce_mode() -> LicenseEnforceMode {
    LicenseEnforceMode::Warn // mirrors flags/license_enforce.yaml default
}

impl Default for FlagsCfg {
    fn default() -> Self {
        Self {
            license_enforce: default_license_enforce_mode(),
            jwt_strict: false,
            payment_webhook_5xx: false,
            r7_502_on_parse_fail: false,
        }
    }
}
```

在 `Config::validate()`（line ~1412）末尾、`Ok(())` 之前追加：
```rust
        // WS-15: flags block has no cross-field invariants beyond serde enum
        // validation; mixed_version_safe is enforced offline by T15.10 lint.
        // (Empty block intentional; left in place so future cross-flag
        // invariants land here without surprising readers.)
        let _ = &self.flags;
```

在 `config/default.toml` 追加：
```toml
[flags]
# WS-15 rollback registry. See flags/*.yaml for switch plan + rollback steps.
license_enforce = "warn"          # disabled | warn | enforce ; REV-004e
jwt_strict = false                 # REV-004d / REV-FINAL-V19
payment_webhook_5xx = false        # declared for parity with Go (M2); Rust does not consume
r7_502_on_parse_fail = false       # R7 / T17
```

在 `config.rs` 文末追加测试模块：
```rust
#[cfg(test)]
mod flags_cfg_tests {
    use super::*;
    use figment::providers::{Format, Toml};
    use figment::Figment;

    #[test]
    fn flags_defaults_match_yaml_ledger() {
        let toml = include_str!("../../../config/default.toml");
        let cfg: Config = Figment::new().merge(Toml::string(toml)).extract().unwrap();
        assert_eq!(cfg.flags.license_enforce, LicenseEnforceMode::Warn);
        assert!(!cfg.flags.jwt_strict);
        assert!(!cfg.flags.payment_webhook_5xx);
        assert!(!cfg.flags.r7_502_on_parse_fail);
    }

    #[test]
    fn flags_env_override() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GW_FLAGS__LICENSE_ENFORCE", "enforce");
            jail.set_env("GW_FLAGS__JWT_STRICT", "true");
            let defaults = include_str!("../../../config/default.toml");
            let cfg: Config = Figment::new()
                .merge(Toml::string(defaults))
                .merge(figment::providers::Env::prefixed("GW_").split("__"))
                .extract().unwrap();
            assert_eq!(cfg.flags.license_enforce, LicenseEnforceMode::Enforce);
            assert!(cfg.flags.jwt_strict);
            Ok(())
        });
    }

    #[test]
    fn flags_invalid_enum_fails_parse() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("GW_FLAGS__LICENSE_ENFORCE", "ENFORCE_STRICTLY");
            let defaults = include_str!("../../../config/default.toml");
            let res: Result<Config, _> = Figment::new()
                .merge(Toml::string(defaults))
                .merge(figment::providers::Env::prefixed("GW_").split("__"))
                .extract();
            assert!(res.is_err(), "invalid enum must fail figment extract");
            Ok(())
        });
    }

    #[test]
    fn flags_block_missing_uses_defaults() {
        // Older config snapshot (mixed-version Rust-old): no [flags] block
        let cfg: Config = Figment::new()
            .merge(Toml::string(include_str!("../../../config/default.toml").replace("[flags]", "[_flags_disabled_for_test]").as_str()))
            .extract().unwrap();
        assert_eq!(cfg.flags.license_enforce, LicenseEnforceMode::Warn);
    }
}
```

### 7. 【如何验证】
- 命令：`cargo test -p gw-gateway --lib -- flags_cfg_tests` — 期望 `test flags_cfg_tests::flags_defaults_match_yaml_ledger ... ok` × 4。
- 命令：`cargo clippy -p gw-gateway --all-targets -- -D warnings` — 期望 0 warning。
- 命令：`cargo fmt --all -- --check` — 期望 exit 0。
- 命令：`./target/release/gw-gateway --help` 后 `GW_FLAGS__LICENSE_ENFORCE=enforce ./target/release/gw-gateway`（smoke），观察 stderr 含 `flags: FlagsCfg { license_enforce: Enforce, .. }` 启动日志（在 `crates/gw-gateway/src/lib.rs` 现有 `tracing::info!("config loaded")` 处由 `?cfg.flags` 输出）。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- Revert 单 PR 即可；`FlagsCfg` 仅在 T15.4/T15.5/T15.7 才被消费。
- 默认 = yaml ledger = warn/false 三个；T15.10 lint 强制 Rust default == yaml default。
- Mixed-version：旧 Rust 进程不识别 `[flags]` → figment 忽略；新 Rust 进程读旧 TOML（无 `[flags]`）→ `Default` 触发，全部保守值。两侧都 fail-safe。

---

## T15.4 — `license_enforce` 接线（REV-004e；双仓）

### 1. 【是什么】
把 `cfg.flags.license_enforce` 在 Go 与 Rust 两侧接入实际 enforcement 点。**字面文件**：
- Go：`modules/license/business/enforce.go`（如不存在，本 task 创建）
- Rust：`crates/gw-auth/src/license.rs`（如不存在，本 task 创建占位 + flag 接入点）
- 测试：`modules/license/business/enforce_test.go` + `crates/gw-auth/src/license.rs::tests`

### 2. 【不是什么】
- **不实现** 完整 license 校验逻辑（那是 WS-5 / WS-16 owner 的范畴）；本 task 只接入 flag-gate 框架 + warn-mode 日志计数 + enforce-mode 拒绝路径骨架。
- 不绕过 flag 直接 enforce —— 默认必须 `warn`。

### 3. 【为什么】
v2 REV-004e + REV-FINAL-LICENSE-COMM：H7（license 全开 = 客户锁出）必须有 14 天 warn-only 窗口。Flag 是这窗口的实现机制。

### 4. 【做什么】
1. Go：在 license module 引入 `func Enforce(ctx, flags Flags, claim License) error`，三分支：
   - `disabled` → return nil（绝对放行）
   - `warn` → 校验，违例 `metrics.LicenseViolation(reason).Inc()` 但 return nil
   - `enforce` → 校验，违例 return `errs.LicenseInvalid`（HTTP 402 `GW_LICENSE_INVALID`）
2. Rust：在 `gw-auth/src/license.rs` 同三分支，metric 名同 `gateway_license_violation_total{mode="warn"|"enforce", reason}`。
3. Go + Rust **共享 reason 字符串集合**：`"expired" | "feature_missing" | "node_count_exceeded"`（在 yaml ledger `description` 字段标注）。
4. 写 test：每个 mode × 每个 reason × {正常/违例} 矩阵。

### 5. 【不做什么】
- 不要让 `warn` mode 影响响应（响应 byte-identical 于 disabled）。
- 不要在 warn 日志中泄露 KEK / token。
- 不要让 enforce mode 跳过 metric 上报。

### 6. 【怎么做】

Go `modules/license/business/enforce.go`（新文件）：
```go
package business

import (
	"context"
	"errors"

	"github.com/cnxx-tech/AILinkMax-Go/shared/platform/config"
	"github.com/cnxx-tech/AILinkMax-Go/shared/platform/metrics"
)

var ErrLicenseInvalid = errors.New("GW_LICENSE_INVALID") // mapped to HTTP 402 in api layer

type License struct {
	Expired           bool
	MissingFeature    string // "" if all features present
	NodeCountExceeded bool
}

// Enforce gates license violations on the runtime flag. Returns nil in
// `disabled`/`warn` regardless of violation; returns ErrLicenseInvalid only
// when mode == enforce AND a violation exists.
func Enforce(ctx context.Context, mode config.LicenseEnforceMode, lic License) error {
	reason := violationReason(lic)
	if reason == "" {
		return nil
	}
	switch mode {
	case config.LicenseDisabled:
		return nil
	case config.LicenseWarn:
		metrics.LicenseViolationTotal.WithLabelValues("warn", reason).Inc()
		return nil
	case config.LicenseEnforce:
		metrics.LicenseViolationTotal.WithLabelValues("enforce", reason).Inc()
		return ErrLicenseInvalid
	default:
		// Should be unreachable; LoadFlags validates the enum.
		return ErrLicenseInvalid
	}
}

func violationReason(l License) string {
	switch {
	case l.Expired:
		return "expired"
	case l.MissingFeature != "":
		return "feature_missing"
	case l.NodeCountExceeded:
		return "node_count_exceeded"
	}
	return ""
}
```

Rust `crates/gw-auth/src/license.rs`（如不存在则新建；导出到 `crates/gw-auth/src/lib.rs`）：
```rust
//! License flag-gated enforcement. Defaults to `warn` so a misconfigured
//! deployment never locks itself out (REV-FINAL-LICENSE-COMM).
use gw_gateway::config::LicenseEnforceMode;
use prometheus::IntCounterVec;
use once_cell::sync::Lazy;

static LICENSE_VIOLATION: Lazy<IntCounterVec> = Lazy::new(|| {
    prometheus::register_int_counter_vec!(
        "gateway_license_violation_total",
        "License violations gated by [flags].license_enforce",
        &["mode", "reason"],
    ).expect("license metric register")
});

#[derive(Debug, Clone)]
pub struct License {
    pub expired: bool,
    pub missing_feature: Option<String>,
    pub node_count_exceeded: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("GW_LICENSE_INVALID")]
pub struct LicenseInvalid;

pub fn enforce(mode: LicenseEnforceMode, lic: &License) -> Result<(), LicenseInvalid> {
    let Some(reason) = violation_reason(lic) else { return Ok(()); };
    match mode {
        LicenseEnforceMode::Disabled => Ok(()),
        LicenseEnforceMode::Warn => {
            LICENSE_VIOLATION.with_label_values(&["warn", reason]).inc();
            Ok(())
        }
        LicenseEnforceMode::Enforce => {
            LICENSE_VIOLATION.with_label_values(&["enforce", reason]).inc();
            Err(LicenseInvalid)
        }
    }
}

fn violation_reason(l: &License) -> Option<&'static str> {
    if l.expired { Some("expired") }
    else if l.missing_feature.is_some() { Some("feature_missing") }
    else if l.node_count_exceeded { Some("node_count_exceeded") }
    else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lic_expired() -> License { License{expired:true,missing_feature:None,node_count_exceeded:false} }
    fn lic_ok() -> License { License{expired:false,missing_feature:None,node_count_exceeded:false} }

    #[test]
    fn disabled_allows_violation() {
        assert!(enforce(LicenseEnforceMode::Disabled, &lic_expired()).is_ok());
    }
    #[test]
    fn warn_allows_but_meters() {
        assert!(enforce(LicenseEnforceMode::Warn, &lic_expired()).is_ok());
        let m = LICENSE_VIOLATION.with_label_values(&["warn","expired"]).get();
        assert!(m >= 1);
    }
    #[test]
    fn enforce_rejects() {
        assert!(enforce(LicenseEnforceMode::Enforce, &lic_expired()).is_err());
    }
    #[test]
    fn ok_license_no_violation_any_mode() {
        for m in [LicenseEnforceMode::Disabled, LicenseEnforceMode::Warn, LicenseEnforceMode::Enforce] {
            assert!(enforce(m, &lic_ok()).is_ok());
        }
    }
}
```

### 7. 【如何验证】
- Go: `go test ./modules/license/business/... -run TestEnforce -v` — 期望 7 个子测试全部 PASS（3 mode × {ok, violation} + reason mapping）。
- Rust: `cargo test -p gw-auth license::tests` — 期望 4 个测试 PASS。
- Smoke: `ALM_FLAGS__LICENSE_ENFORCE=warn ./binaries/web/admin`，触发一个过期 license 请求 → curl `:9090/metrics | grep license_violation` — 期望 `gateway_license_violation_total{mode="warn",reason="expired"} 1`，HTTP 状态码 = 200。
- Smoke (enforce): 同上但 env 改 `enforce` → 期望 HTTP 402 + body 含 `GW_LICENSE_INVALID`。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- 默认值 = warn（双仓一致）。**回滚步骤**：
  1. `kubectl set env deploy/gw-gateway GW_FLAGS__LICENSE_ENFORCE=warn` → rolling restart
  2. `kubectl set env deploy/alm-web ALM_FLAGS__LICENSE_ENFORCE=warn` → rolling restart
  3. 监控 `gateway_license_violation_total{mode="warn"}` ~5 min 回到 baseline
  4. 仍异常：进一步降级到 `disabled`
- **跨仓同步**：license 是 Go 管理面 + Rust 数据面共同校验，**必须同步切换**。WS-15 deploy ordering（T15.9）规定：切 enforce 时 Go 先切，Rust 后切（避免 Rust enforce 而 Go 还允许签发新 token 的窗口）；切回 warn/disabled 时反序（Rust 先回退）。
- Mixed-version：mode=warn 是 byte-equivalent 于 disabled（无 403/402），永远 safe；mode=enforce **不允许** 在 mixed window 内打开 —— T15.10 lint 检测部署清单中 `_pv` 字段，若版本差 ≥1 则拒绝 `enforce` 值。

---

## T15.5 — `jwt_strict` 接线（REV-004d）

### 1. 【是什么】
把 `cfg.flags.jwt_strict` 接入 Rust `gw-auth` 的 `verify_jwt`。**字面文件**：`crates/gw-auth/src/jwt.rs`。

### 2. 【不是什么】
- 不修改 R1 的 panic 路径本体 —— legacy panic-on-mismatch 在 WS-5 S5.11 已被替换为 `bail!`。本 task 只是把"是否走严格 issuer/audience 校验"放到 flag 后面。
- 不在 Go 端接线（Go 不直接 verify JWT；通过 Rust gateway 的 management-token 路径）。

### 3. 【为什么】
v2 REV-FINAL-V19：legacy `verify_jwt` panic 改 feature flag → 一个 release 默认 false 观察 → 下一个 release 默认 true → 再下个 release 删 legacy。本 task 落"默认 false"的 release。

### 4. 【做什么】
1. 在 `crates/gw-auth/src/jwt.rs` 的 `verify_jwt(...)` 函数签名上加 `strict: bool` 参数（保持向下兼容：调用方读 `cfg.flags.jwt_strict`）。
2. `strict=true` 路径：iss/aud 严格匹配，失败 `bail!("GW_JWT_INVALID_ISS_AUD")`。
3. `strict=false` 路径：iss/aud 不匹配只 `tracing::warn!` + `gw_auth_jwt_strict_reject_total{outcome="warn_only"}` 计数，不拒绝。
4. 调用点 `crates/gw-gateway/src/lib.rs::request_filter`（grep `verify_jwt` 定位）传 `cfg.flags.jwt_strict`。
5. 测试：strict=true × 不匹配 → Err；strict=false × 不匹配 → Ok + counter +1；strict=true × 匹配 → Ok。

### 5. 【不做什么】
- 不要让 `strict=false` 也累 `outcome="reject"` 计数（指标污染）。
- 不要把 `strict` 参数泄露到外部 API。

### 6. 【怎么做】

`crates/gw-auth/src/jwt.rs`（核心 diff）：
```rust
pub fn verify_jwt(
    token: &str,
    keys: &JwtKeySet,
    expected_iss: &str,
    expected_aud: &str,
    strict: bool, // <-- WS-15 / REV-004d
) -> anyhow::Result<JwtClaims> {
    let claims = decode_unchecked(token, keys)?;
    let iss_match = claims.iss == expected_iss;
    let aud_match = claims.aud == expected_aud;
    if !iss_match || !aud_match {
        if strict {
            gw_auth_jwt_strict_reject_total!(outcome = "reject_strict").inc();
            anyhow::bail!("GW_JWT_INVALID_ISS_AUD");
        } else {
            gw_auth_jwt_strict_reject_total!(outcome = "warn_only").inc();
            tracing::warn!(target: "gw_auth::jwt", got_iss=%claims.iss, got_aud=%claims.aud, "jwt_strict=false: legacy mismatch tolerated");
        }
    }
    // ... rest of legacy checks (exp / nbf / sig) unchanged
    Ok(claims)
}
```

Macro `gw_auth_jwt_strict_reject_total!` 在 `crates/gw-auth/src/metrics.rs` 中按现有 lazy_static / `register_int_counter_vec!` 模式注册。

### 7. 【如何验证】
- `cargo test -p gw-auth jwt::tests::strict_true_rejects_iss_mismatch` PASS。
- `cargo test -p gw-auth jwt::tests::strict_false_tolerates_iss_mismatch_with_counter` PASS（断言 metric 增）。
- `cargo test -p gw-auth jwt::tests::strict_any_accepts_matching` PASS。
- Smoke: 启动两份 gateway（一份 `GW_FLAGS__JWT_STRICT=false`，一份 `=true`），用 iss 不匹配的 token 各请求一次，false-pod 返回 200 + counter `outcome="warn_only"`+1；true-pod 返回 401 + body 含 `GW_JWT_INVALID_ISS_AUD`。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- 默认 false。回滚：`GW_FLAGS__JWT_STRICT=false`（rolling restart）→ 监控 `gw_auth_jwt_strict_reject_total{outcome="reject_strict"}` 归 0。
- Mixed-version safe：Go 不消费此 flag；Rust 旧版本 = 原 legacy 行为 ≡ strict=false。
- N+1 release 切默认 true 时，需先确认 14 天内 `outcome="warn_only"` 速率 < 0.1% 总 JWT 请求（runbook 阈值）。

---

## T15.6 — `payment_webhook_5xx` 接线（M2）

### 1. 【是什么】
把 `cfg.flags.payment_webhook_5xx` 接入 Go payment webhook handler。**字面文件**：`modules/payment/api/webhook.go`。

### 2. 【不是什么】
- 不修改 M2 InTx / idempotency_keys 落盘逻辑（那是 WS-1 T01-T05 owner）；本 task 只把"持久 DB 错误时 HTTP 状态码"放到 flag 后面。
- 不 retroactively 修改已发出的 webhook response（无法做到）。

### 3. 【为什么】
M2 根因：legacy `2xx + error body` 让支付商认定 webhook delivered，错过 retry 机会 → 用户付款不到账。修法是返回 5xx 让支付商 retry。但有些第三方 webhook 中间件对 5xx 会 alert 风暴，所以 flag 默认 false（保持 legacy），一个 release 后切 true。

### 4. 【做什么】
1. 在 `modules/payment/api/webhook.go::handleWebhook` 中，DB 持久错误分支：
   - `flags.PaymentWebhook5xx == false` → 保持现状（`c.JSON(200, {error: "..."})`，同 v1）
   - `flags.PaymentWebhook5xx == true` → `c.AbortWithStatusJSON(502, {code:"PAYMENT_WEBHOOK_RETRY"})`
2. metric `payment_webhook_outcome_total{mode="legacy_200"|"new_5xx", outcome="db_persist_fail"}`
3. test: webhook 流入 + DB error 注入 × {flag=false → 200, flag=true → 502}。

### 5. 【不做什么】
- 不要把 transient error（如 PG context cancelled）也归到 "persist_fail" —— 那是另一类 retryable，已 200。
- 不要在 flag=true 下跳过 idempotency_keys 写入（M2 root-cause 已保证写入在 InTx 内）。

### 6. 【怎么做】

`modules/payment/api/webhook.go`（diff 片段）：
```go
func (h *Handler) handleWebhook(c *gin.Context) {
	// ... existing parse + InTx ...
	err := h.svc.IngestWebhook(ctx, payload)
	if errors.Is(err, business.ErrPersistFail) {
		if h.flags.PaymentWebhook5xx {
			metrics.PaymentWebhookOutcome.WithLabelValues("new_5xx", "db_persist_fail").Inc()
			c.AbortWithStatusJSON(http.StatusBadGateway, gin.H{
				"code": "PAYMENT_WEBHOOK_RETRY",
				"msg":  "transient error, please retry",
			})
			return
		}
		metrics.PaymentWebhookOutcome.WithLabelValues("legacy_200", "db_persist_fail").Inc()
		c.JSON(http.StatusOK, gin.H{"ok": false, "msg": err.Error()})
		return
	}
	// ... existing success path
}
```

注入 `h.flags Flags` 字段在 `modules/payment/api/router.go::NewHandler(...)` 签名上扩，由 `binaries/web/main.go` 在 wiring 时把 `cfg.flags` 传入。

### 7. 【如何验证】
- `go test ./modules/payment/api -run TestHandleWebhook_PersistFail_FlagOff` → 期望 `status=200, body.ok=false`。
- `go test ./modules/payment/api -run TestHandleWebhook_PersistFail_FlagOn` → 期望 `status=502, body.code=PAYMENT_WEBHOOK_RETRY`。
- e2e: `make test-frontend` 中追加 webhook 注入测试（两轮 flag 切换），都 PASS。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- 默认 false（保持 legacy 200 行为）。回滚：`ALM_FLAGS__PAYMENT_WEBHOOK_5XX=false`（rolling restart）→ `payment_webhook_outcome_total{mode="new_5xx"}` 不再增长。
- Mixed-version：webhook 是 Go-only 路径；Rust 端无关。两个 Go 版本共存时，旧版无 flag → 等价 false → safe。
- 切 true 必须配合**支付商 retry 策略调整**：runbook 列出三家主要支付商（Stripe/Alipay/WeChat Pay）对 502 的 retry behaviour 与 alert 阈值。

---

## T15.7 — `r7_502_on_parse_fail` 接线（R7 / T17）

### 1. 【是什么】
把 `cfg.flags.r7_502_on_parse_fail` 接入 Rust gateway response filter。**字面文件**：`crates/gw-proxy/src/filters/response.rs`。

### 2. 【不是什么】
- 不改 Codec 解析逻辑（那是 WS-3/WS-6 owner）；本 task 只把"解析失败时 HTTP 状态码"放到 flag 后面。
- 不影响 stream 已开始后的 mid-stream parse fail —— 那必然走 trailer/abort 路径，与本 flag 正交。

### 3. 【为什么】
R7 / T17：legacy 上游解析失败 → gateway 返回 200 + 空 body，前端表现为"模型回复了空内容"，掩盖了 upstream regression。修法是返回 502，但前端契约要同步改（WS-11）。所以 flag 默认 false，等 WS-11 frontend 落地后切 true。

### 4. 【做什么】
1. 在 `crates/gw-proxy/src/filters/response.rs::on_upstream_response_eof` 中，parse fail 分支：
   - flag=false → 现行（200 + 合成空 body，metric `gw_response_legacy_200_parse_fail_total++`）
   - flag=true → `bail_with_status(502, "GW_UPSTREAM_PARSE_FAIL")`，metric `gw_response_502_parse_fail_total++`
2. Flag 来自启动时构造的 `ProxyFilterCtx`，由 `crates/gw-gateway/src/lib.rs` wire `cfg.flags.r7_502_on_parse_fail` 传入。
3. 测试 + soak：触发一次伪上游"返回截断 JSON"场景，两份 flag 设置下分别断言响应。

### 5. 【不做什么】
- 不要在 flag=false 时静默 metric —— `legacy_200_parse_fail_total` 必须仍增长，用于评估切 true 风险。
- 不要在 flag=true 下吞掉 `Content-Length`：502 body 是结构化 error JSON。

### 6. 【怎么做】

`crates/gw-proxy/src/filters/response.rs`（diff 片段）：
```rust
if parse_failed {
    if ctx.flags.r7_502_on_parse_fail {
        crate::metrics::RESPONSE_502_PARSE_FAIL.inc();
        return Err(GwError::status(StatusCode::BAD_GATEWAY, "GW_UPSTREAM_PARSE_FAIL"));
    }
    crate::metrics::RESPONSE_LEGACY_200_PARSE_FAIL.inc();
    tracing::warn!(target: "gw_proxy::response", "upstream parse fail; legacy 200 path (flag=false)");
    // fall through to existing empty-body synthesis
}
```

`ProxyFilterCtx` 在 `crates/gw-proxy/src/lib.rs` 加 `pub flags: FlagsCfg` 字段（值由 `gw-gateway/src/lib.rs::bootstrap` 传入）。

### 7. 【如何验证】
- `cargo test -p gw-proxy response::tests::parse_fail_flag_off_returns_200` PASS。
- `cargo test -p gw-proxy response::tests::parse_fail_flag_on_returns_502` PASS。
- `make e2e-soak-smoke` 在 chaos=heavy 场景启用 fake-upstream `truncated-json` 注入，断言：
  - flag=false pod: `gw_response_legacy_200_parse_fail_total` > 0、客户端收 200。
  - flag=true pod: `gw_response_502_parse_fail_total` > 0、客户端收 502。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- 默认 false。回滚：`GW_FLAGS__R7_502_ON_PARSE_FAIL=false`（rolling restart）→ `gw_response_502_parse_fail_total` 归 0。
- **Mixed-version: NOT safe**（yaml `mixed_version_safe: false`）。原因：前端在 WS-11 才学会处理 502；切 true 必须满足：
  1. 前端已部署 WS-11 contract update（admin 检查 `web:version >= 1.7.0`）。
  2. WS-11 contract test `tests/contract/r7_502.spec.ts` PASS。
  3. T15.10 mixed-version lint 在 deploy CI 阶段二次确认。
- 跨仓 deploy ordering（T15.9）：切 true 时**先**部署 frontend → 24h 观察 → 再切 Rust pod 的 flag。

---

## T15.8 — `scripts/check-pr-description.sh` + CI gate

### 1. 【是什么】
CI 脚本，扫描 PR description / commit message 是否包含必填字段。**字面文件**：
- `AILinkMax-Go/scripts/check-pr-description.sh`
- `ailinkmax-gateway/scripts/check-pr-description.sh`（mirror，byte-identical）
- CI workflow `.github/workflows/check-pr.yml`（双仓各一份）

### 2. 【不是什么】
- 不是 PR template；template 是 `.github/PULL_REQUEST_TEMPLATE.md`，本 task **额外** 加一份并把 4 个必填字段列出，但 enforcement 在 script。
- 不替代人工 review。

### 3. 【为什么】
v2 §5 WS-15 "每个 PR 必须在描述里说明：feature flag 名 + 默认值 + 计划切换日期 + 回滚步骤"。无 CI gate 则会被忽略。

### 4. 【做什么】
1. 写 `.github/PULL_REQUEST_TEMPLATE.md`（双仓），含必填 4 字段 + 说明"若不涉及 flag 写 `flag: none`"。
2. 写 `scripts/check-pr-description.sh`：通过 `gh pr view --json body` 取 PR body，正则匹配 4 行：
   - `^Flag:\s+([a-z_]+|none)$`
   - `^Default:\s+\S+$`
   - `^Switch-Date:\s+(N\+\d+|none|\d{4}-\d{2}-\d{2})$`
   - `^Rollback:\s+.+$`
3. 在 `.github/workflows/check-pr.yml` 加 job，触发条件 `pull_request: [opened, edited, synchronize]`，调脚本，失败则 CI 红。
4. 当 `Flag:` ≠ `none` 时，进一步断言 yaml 注册表中存在该 flag（`test -f flags/<name>.yaml`）。

### 5. 【不做什么】
- 不要让脚本依赖网络/外部 service（除 `gh` CLI）。
- 不要对 `dependabot[bot]` 之外的机器人也跳过 —— 由 Track G 在 PR description 处理。

### 6. 【怎么做】

`.github/PULL_REQUEST_TEMPLATE.md`：
```md
## Change summary
...

## WS-15 feature-flag block (REQUIRED, do not delete)
Flag: <flag_name | none>
Default: <value | none>
Switch-Date: <YYYY-MM-DD or N+1 or none>
Rollback: <one-line description; full steps live in flags/<flag_name>.yaml>
```

`scripts/check-pr-description.sh`：
```bash
#!/usr/bin/env bash
set -euo pipefail
PR_NUMBER="${1:-${PR_NUMBER:?usage: $0 <pr_number>}}"
BODY=$(gh pr view "$PR_NUMBER" --json body --jq .body)
missing=0
check() { local label=$1 pattern=$2
  if ! grep -qE "$pattern" <<<"$BODY"; then
    echo "ERROR: PR description missing or malformed line: '$label'"
    missing=1
  fi
}
check "Flag"        '^Flag:[[:space:]]+([a-z][a-z0-9_]*|none)$'
check "Default"     '^Default:[[:space:]]+\S+$'
check "Switch-Date" '^Switch-Date:[[:space:]]+(N\+[0-9]+|none|[0-9]{4}-[0-9]{2}-[0-9]{2})$'
check "Rollback"    '^Rollback:[[:space:]]+.+$'
[ $missing -eq 0 ] || exit 1

FLAG=$(grep -E '^Flag:' <<<"$BODY" | awk '{print $2}')
if [ "$FLAG" != "none" ]; then
  if [ ! -f "flags/$FLAG.yaml" ]; then
    echo "ERROR: Flag: $FLAG declared but flags/$FLAG.yaml does not exist"
    exit 1
  fi
fi
echo "PR description WS-15 block: OK"
```

### 7. 【如何验证】
- 单元：`echo "Flag: license_enforce
Default: warn
Switch-Date: N+1
Rollback: ALM_FLAGS__LICENSE_ENFORCE=warn rolling restart" | PR_NUMBER=0 bash -c 'BODY=$(cat); ...'`（local test，模拟 gh pr view 输出）→ 期望 `PR description WS-15 block: OK`。
- 反向：删一行 `Default:` → 期望 exit 1 + `missing or malformed line: 'Default'`。
- CI：在测试 PR 上验证 workflow 运行结果，缺字段 → 红，齐全 → 绿。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- 单 PR revert 即可关闭 gate；不涉及 runtime。
- 不是 flag 本身，无 mixed-version 关切。

---

## T15.9 — Runbook + deploy ordering 表

### 1. 【是什么】
Ops runbook。**字面文件**：
- `AILinkMax-Go/docs/runbook/feature-flags.md`
- `ailinkmax-gateway/docs/runbook/feature-flags.md`（mirror）

### 2. 【不是什么】
- 不是 spec —— spec 在 yaml ledger 与 master plan v2 §5 WS-15。runbook 是"出事时怎么按按钮"的 step-by-step。
- 不含 secret / 内部 URL（PII / 凭证）。

### 3. 【为什么】
v2 REV-006 §7 #13："runbook 完整性：每个 §7 #6 列举的 lint 与新告警必有对应 runbook 条目；缺失即拒。" WS-15 引入 4 个 flag → 4 段 runbook + 1 段总表。

### 4. 【做什么】
1. 表 1：**Flag inventory**（4 行，等价 yaml ledger 摘要 + 当前默认）。
2. 表 2：**Deploy ordering**（切 enforce 与切回 warn/disabled 的次序）。
3. 段 3：**Incident playbook**（"license enforce 全开后 5 分钟内 402 突增" / "jwt_strict 切 true 后 401 速率 > 1%" / "webhook 502 后支付商告警" / "R7 502 后前端 5xx 突增" 四个 scenario）。
4. 段 4：**Mixed-version 表**（versions × flag-state 矩阵；哪些组合 ALLOWED / FORBIDDEN）。

### 5. 【不做什么】
- 不要把 runbook 写成"故事"；只列 step 1/step 2/step 3 + 验证命令 + 期望输出。
- 不要嵌入大段 Go/Rust 代码；引用 file:line 即可。

### 6. 【怎么做】

`docs/runbook/feature-flags.md` 骨架（节选）：
```md
# WS-15 Feature-Flag Runbook

## 1. Flag inventory (snapshot of flags/*.yaml)

| Flag | Type | Current default | Owner WS | Mixed-version safe |
|---|---|---|---|---|
| license_enforce      | enum  | warn   | WS-15,WS-16 | yes (warn=no-op) |
| jwt_strict           | bool  | false  | WS-5,WS-15  | yes              |
| payment_webhook_5xx  | bool  | false  | WS-1,WS-15  | yes              |
| r7_502_on_parse_fail | bool  | false  | WS-3,WS-6   | **NO**           |

## 2. Deploy ordering (cross-repo)

### Switching license_enforce: warn → enforce
1. Confirm 14-day warn-only window completed (REV-FINAL-LICENSE-COMM)
2. Confirm `gateway_license_violation_total{mode="warn"}` rate <0.1% in last 7d
3. PR: bump Go default in `shared/platform/config/flags.go` LicenseWarn → LicenseEnforce + yaml `default: enforce`
4. PR: bump Rust default in `crates/gw-gateway/src/config.rs::default_license_enforce_mode()` Warn → Enforce
5. Deploy Go first: `kubectl rollout restart deploy/alm-web` (30s/pod)
6. Wait 5 min, watch metric
7. Deploy Rust: `kubectl rollout restart deploy/gw-gateway`
8. Wait 24h on canary cell, then full rollout

### Rolling back license_enforce: enforce → warn (emergency)
1. Rust first: `kubectl set env deploy/gw-gateway GW_FLAGS__LICENSE_ENFORCE=warn` + restart
2. Go second: `kubectl set env deploy/alm-web ALM_FLAGS__LICENSE_ENFORCE=warn` + restart
3. Watch `gateway_license_violation_total{mode="enforce"}` ↓ 0
4. If still failing customer auth: set both to `disabled`, open WS-16 incident

(Other three flags follow same patterns; see yaml ledger rollback_steps.)

## 3. Incident playbooks

### 3.1 "402 spike after license_enforce on"
- Symptom: `gateway_license_violation_total{mode="enforce"}` > 100/min sustained
- Action: see §2 rollback-license_enforce
- Postmortem: collect violation reasons (`?reason="..."` label split) before rollback; attach to incident ticket

### 3.2 "401 rate >1% after jwt_strict=true"
- Symptom: `gw_auth_jwt_strict_reject_total{outcome="reject_strict"}` rising
- Action: `GW_FLAGS__JWT_STRICT=false` on Rust pods; restart
- Check JWT issuer config: `crates/gw-auth/src/jwt.rs::expected_iss` must match clients' actual `iss`

### 3.3 "Payment webhook 502 storm"
- Symptom: `payment_webhook_outcome_total{mode="new_5xx"}` > 50/min
- Action: `ALM_FLAGS__PAYMENT_WEBHOOK_5XX=false` on Go pods; restart
- Verify InTx commit logs: `kubectl logs deploy/alm-web | grep idempotency_keys` — every 502 must have a row written before the response

### 3.4 "Frontend errors after r7_502 flag flip"
- Symptom: frontend reports "Gateway returned 502" surge
- Action: `GW_FLAGS__R7_502_ON_PARSE_FAIL=false` on Rust pods; restart
- If frontend already deployed WS-11: investigate whether upstream parse-fail rate is real (not flag artifact); look at `gw_codec_parse_fail_total{from,to}`

## 4. Mixed-version flag matrix

| Go version | Rust version | license_enforce | jwt_strict | payment_webhook_5xx | r7_502_on_parse_fail |
|---|---|---|---|---|---|
| N | N   | any   | any   | any   | any   |
| N+1 | N | disabled/warn | any | true | **false** |
| N | N+1 | disabled/warn | false | any | **false** |
| N+1 | N+1 | any | any | any | any (if frontend ≥ 1.7.0) |

Forbidden combinations are enforced by T15.10 lint at deploy CI.
```

### 7. 【如何验证】
- `bash scripts/check-runbook-completeness.sh` (T15.10 引用)：脚本扫 `flags/*.yaml` 与 runbook 内 anchor，断言每个 flag name 在 runbook 出现 ≥ 1 次。
- 人工演练：在 staging 走一次 license_enforce warn→enforce→warn 往返，每个 step 按 runbook 执行，记录实际时间与差异。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- runbook 本身无 runtime 影响；revert PR 即可。
- 改 runbook 不需要切 flag；切 flag 必须先改 runbook（双方在同一 PR 内）。

---

## T15.10 — Mixed-version flag 默认值 lint

### 1. 【是什么】
CI lint，禁止"mixed-version 期间 flag 处于 unsafe 状态"。**字面文件**：
- `AILinkMax-Go/scripts/check-mixed-version-flags.sh`
- `ailinkmax-gateway/scripts/check-mixed-version-flags.sh`（mirror）

### 2. 【不是什么】
- 不是 flag 默认值的 source of truth —— yaml 才是。本 lint 只断言 Go default / Rust default / yaml default **三方一致** + mixed_version_safe 约束。
- 不在生产环境运行（CI only）。

### 3. 【为什么】
v2 REV-009 mixed-version + REV-010 attack 表 "跨版本部署窗口攻击"。手动维护三处默认值容易漂移；本 lint 在 `make check` 阶段守门。

### 4. 【做什么】
1. 解析 `flags/*.yaml` 取 `name` + `default` + `mixed_version_safe`。
2. 从 `shared/platform/config/flags.go` 解析 `LoadFlags` 中字面默认值。
3. 从 `crates/gw-gateway/src/config.rs` 解析 `FlagsCfg::default` 与 `default_license_enforce_mode()`。
4. 三方比较：任一不一致 → exit 1 with diff。
5. 解析当前 `deploy/*.yaml` 中 `_pv` 字段；若 Go _pv ≠ Rust _pv 且 deploy 清单中存在 `mixed_version_safe: false` 的 flag 处于 non-default → exit 1。

### 5. 【不做什么】
- 不要用 LLM 做 lint；只用 yaml/toml 解析 + regex。
- 不要在脚本里读 production env vars（部署侧不同 cluster 配置不在 lint 范围）。

### 6. 【怎么做】

`scripts/check-mixed-version-flags.sh`（双仓共用 python3 + bash）：
```bash
#!/usr/bin/env bash
set -euo pipefail
python3 <<'PY'
import yaml, pathlib, re, sys

errors = []

# 1. Parse yaml ledger
ledger = {}
for p in pathlib.Path('flags').glob('*.yaml'):
    if p.name == '_schema.yaml': continue
    d = yaml.safe_load(p.read_text())
    ledger[d['name']] = d

# 2. Parse Go defaults (best-effort regex on shared/platform/config/flags.go)
go_path = pathlib.Path('shared/platform/config/flags.go')
if go_path.exists():
    go_src = go_path.read_text()
    go_defaults = {}
    for name in ledger:
        # match `<Field>: <value>,` inside the LoadFlags initializer
        camel = ''.join(p.title() for p in name.split('_')).replace('Jwt','JWT').replace('R7502','R7_502')
        m = re.search(rf'{camel}:\s*(License\w+|true|false),', go_src)
        if m:
            v = m.group(1)
            # Map License* enum back to yaml value
            if v.startswith('License'):
                v = v.replace('License','').lower()
            go_defaults[name] = v
    for name, ledger_v in ledger.items():
        gv = go_defaults.get(name)
        if gv is None and ledger[name].get('go_repo_path') is not None:
            errors.append(f'Go default for {name} not parseable')
            continue
        if gv is not None and str(ledger_v['default']).lower() != str(gv).lower():
            errors.append(f'Go default for {name} = {gv}, yaml = {ledger_v["default"]}')

# 3. Parse Rust defaults (best-effort regex on crates/gw-gateway/src/config.rs)
rs_path = pathlib.Path('crates/gw-gateway/src/config.rs')
if rs_path.exists():
    rs_src = rs_path.read_text()
    rs_defaults = {}
    # default_license_enforce_mode() -> Warn | Disabled | Enforce
    m = re.search(r'fn default_license_enforce_mode\(\) -> LicenseEnforceMode \{\s*LicenseEnforceMode::(\w+)', rs_src)
    if m: rs_defaults['license_enforce'] = m.group(1).lower()
    # FlagsCfg::default — pick `<field>: <bool>,`
    block = re.search(r'impl Default for FlagsCfg \{.*?Self \{(.*?)\}\s*\}', rs_src, re.S)
    if block:
        for line in block.group(1).splitlines():
            ml = re.match(r'\s*([a-z_]+):\s*(true|false|default_\w+\(\)),', line)
            if ml and ml.group(1) != 'license_enforce':
                rs_defaults[ml.group(1)] = ml.group(2)
    for name, ledger_v in ledger.items():
        rv = rs_defaults.get(name)
        if rv is None and ledger[name].get('rust_repo_path') is not None:
            errors.append(f'Rust default for {name} not parseable')
            continue
        if rv is not None and str(ledger_v['default']).lower() != str(rv).lower():
            errors.append(f'Rust default for {name} = {rv}, yaml = {ledger_v["default"]}')

if errors:
    print('FAIL: default-value drift between yaml ledger and Go/Rust code:')
    for e in errors: print(f'  - {e}')
    sys.exit(1)
print(f'OK: {len(ledger)} flags consistent across yaml/Go/Rust')
PY
```

挂入 `make check`（Go 仓 Makefile target 追加 `bash scripts/check-mixed-version-flags.sh`），挂入 `scripts/check-invariants.sh`（Rust 仓追加相同行）。

### 7. 【如何验证】
- `bash scripts/check-mixed-version-flags.sh` → 期望 `OK: 4 flags consistent across yaml/Go/Rust`。
- 反向：把 `flags/jwt_strict.yaml::default` 改成 `true` 但不动 Go/Rust → 期望 `FAIL: ... Rust default for jwt_strict = false, yaml = True`。
- 反向：把 `crates/gw-gateway/src/config.rs::default_license_enforce_mode()` 返回值从 `Warn` 改成 `Enforce` → 期望 `FAIL: ... Rust default for license_enforce = enforce, yaml = warn`。

### 8. 【回滚 / 默认值 / mixed-version 约束】
- Lint 本身是 CI gate；revert 单 PR 即可。
- Lint 触发 fail 不应被 `-no-verify` 绕过 —— pre-commit hook 也加同一脚本（与 `scripts/check-invariants.sh` 并列）。
- 当确实需要切默认（如 N+1 release），步骤：
  1. 同一 PR 中：改 yaml `default` + Go `flags.go` default + Rust `config.rs` default + `config/default.toml`
  2. 改 deploy/staging 的 `_pv` bump
  3. 此 PR 必须 reference 上游 yaml 里 `switch_plan.enforce_release`
  4. `check-mixed-version-flags.sh` PASS

---

## 收尾：WS-15 完成度门 (DoD)

合并到 `main` 之前必须全部 PASS：

1. `bash scripts/check-flags-schema.sh` → exit 0（T15.1）
2. `go test ./shared/platform/config/... -run TestLoadFlags -v` 全绿（T15.2）
3. `cargo test -p gw-gateway --lib flags_cfg_tests` 全绿（T15.3）
4. `go test ./modules/license/business/... -run TestEnforce -v` + `cargo test -p gw-auth license::tests` 全绿（T15.4）
5. `cargo test -p gw-auth jwt::tests::strict_*` 全绿（T15.5）
6. `go test ./modules/payment/api -run TestHandleWebhook_PersistFail_*` 全绿（T15.6）
7. `cargo test -p gw-proxy response::tests::parse_fail_*` 全绿（T15.7）
8. CI gate `check-pr-description.sh` 在测试 PR 上验证 red/green 两态（T15.8）
9. `docs/runbook/feature-flags.md` 经一次 staging 演练 license_enforce 往返 PASS（T15.9）
10. `bash scripts/check-mixed-version-flags.sh` 在 `make check` 阶段绿（T15.10）
11. 4 个 `flags/*.yaml` 通过 `_schema.yaml` 校验，默认值全部 = 保守值
12. PR description 模板更新已落地双仓

---

## 跨 WS 依赖回看

- **WS-1**: M2 接线（T15.6）依赖 WS-1 InTx + idempotency_keys 已 land；未 land 时本 task 仍可写，但 e2e 跑不通。Track A 先合 M2 InTx，本 Track G 后接线。
- **WS-3 / WS-6**: R7 502 接线（T15.7）需要 `crates/gw-proxy/src/filters/response.rs` 已合并 parse-fail 分支（T17）。Track C 先合，本 Track G 后接线。
- **WS-5**: jwt_strict 接线（T15.5）需要 R1 panic 路径已被 `bail!` 替换（S5.11）。Track F2 先合。
- **WS-11**: r7_502 flag 切 true 必须等 WS-11 frontend 502 处理 land。
- **WS-16**: license_enforce 切 enforce 前 14 天必须由 WS-16 owner 发出客户通告（REV-FINAL-LICENSE-COMM）。
- **WS-12**: T15.8 + T15.10 lint 由 WS-12 CI 框架统一 wiring；本 sub-plan 仅写脚本，CI 集成在 WS-12 落地。
- **WS-14**: 无 migration 依赖（本 WS 不动 schema）。

## INVESTIGATE 标记

无（本 sub-plan 已在 hypothesis-verification 阶段确认所有 sonnet 假设，无悬而未决项）。

## ADR-style 记录："为什么选 YAML ledger + TOML runtime"

- **备选 A**：直接复用 `config/default.toml` 的 `[flags]` 块作为唯一 source of truth，不写 yaml ledger。
- **备选 B（采用）**：yaml ledger 作为 PR-time 审计 + runtime 仍走 TOML；T15.10 lint 强制一致。
- 理由：yaml ledger 显式编码 `switch_plan` / `rollback_steps` / `mixed_version_safe` 字段，TOML 不便表达这种结构（TOML 多行字符串可读性差，且 figment 不消费这些字段会带来 noise）。把"审计 metadata"与"runtime value"分两个文件，让 ops 看 yaml、runtime 看 TOML、lint 守一致性。
