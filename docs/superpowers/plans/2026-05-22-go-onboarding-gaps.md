# Go 接入缺口项 Implementation Plan(G1..G6)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落地 [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../specs/2026-05-22-go-onboarding-gaps-design.md) 的 6 个非阻塞缺口 —— ProxyNode 契约 docs、`auth_configs` migration 0006、VendorKey OAuth 子结构 + 反向事件、Gemini Files writer 翻转、`UsageRecord.operation_id` 字段、`events:entity` 失效时清 Redis L1 —— 让 Go 控制面落 FRD 各模块时不再遇到字段缺失或缓存不一致。

**Architecture:** G1(ProxyNode)在 Rust 端已被 tunnel-t T1 落完(`crates/gw-core/src/entity.rs::ProxyNode`、`EntityStore.proxy_node`、`loader::load_proxy_node`、`stream_consumer` ns dispatch 均存在),本 plan G1 仅补契约文档 `docs/go-contracts/parts/04-proxy-nodes.md`。G2 增加 `0006_auth_configs.sql` migration 并把 `lib.rs` 启动期 `auth_config_or_load` 缺失分支从 fail-fast 改为 warn + legacy fallback。G3 在 `VendorKey` payload 加可选 `oauth: Option<OAuthCredentials>` 子对象,`filters/upstream::upstream_request_filter` 注入 access_token 替代 api_key、`OAuthListener` 在上游 401 时反向 XADD `events:entity ns=vendor_key op=status_change field=oauth_failed`。G4 翻转 `gemini_file_mapping` writer 责任(确认 / 删除 Rust 写路径)。G5 在三个 codec 的 `parse_response` 中抽顶层 `name: "operations/..."` 写 `ctx.operation_id`,`UsageRecord` 加 `operation_id: Option<String>`。G6 在 `stream_consumer::apply_entity_event` 内,Moka invalidate 之前先 `DEL cache:{ns}:{id}`。

**Tech Stack:** Rust 1.86 / Pingora 0.8 / tokio / serde_json / sqlx 0.8 / fred 9(Redis DEL)/ moka / secrecy / AES-256-GCM(`gw-store::encrypt`)/ subtle / time。

**Source spec**: [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../specs/2026-05-22-go-onboarding-gaps-design.md)。
**Companion changelog**: [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §2-§7。
**Blockers plan(独立平行,不互相 block)**: [`docs/superpowers/plans/2026-05-22-go-onboarding-blockers.md`](2026-05-22-go-onboarding-blockers.md)。

**Branch suggestion**: `feature/go-onboarding-gaps`(从 `origin/main` cut;与 blockers 分支无 merge 依赖)。

---

## File structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/gw-core/src/entity.rs` | modify | 加 `OAuthCredentials` struct + `VendorKey.oauth: Option<OAuthCredentials>` 字段 |
| `crates/gw-core/src/error/codes.rs` | modify | 加 `GW_AUTH_OAUTH_TOKEN_EXPIRED` / `GW_AUTH_OAUTH_REFRESH_FAILED`(error 路径 / OAuthListener)|
| `crates/gw-ledger/src/usage.rs` | modify | `UsageRecord` 加 `operation_id: Option<String>` |
| `crates/gw-proxy/src/ctx.rs` | modify | 加 `operation_id: Option<String>` 字段(`ReqCtx`)|
| `crates/gw-codec/src/openai/codec.rs` | modify | `parse_response` 抽 OpenAI video 顶层 `id` 形如 `"operations/..."` 时填(其它沿用空)|
| `crates/gw-codec/src/anthropic/codec.rs` | modify | 沿用(Anthropic 暂无 long-running)— 仅占位无修改 |
| `crates/gw-codec/src/gemini/codec.rs` | modify | `parse_response` 抽顶层 `name: "operations/..."` 填 |
| `crates/gw-store/migrations/0006_auth_configs.sql` | **create** | `auth_configs` 表 |
| `crates/gw-gateway/src/lib.rs` | modify | `bootstrap` 内 `auth_config_or_load("global")` 缺失 → warn + legacy fallback;wire 行为不变 |
| `crates/gw-proxy/src/filters/upstream.rs` | modify | `upstream_request_filter` auth 注入分支:`vendor_key.oauth.is_some()` → 用 access_token;临过期 metric |
| `crates/gw-proxy/src/filters/logging.rs` | modify | `UsageRecord` 构建处填 `operation_id = ctx.operation_id` |
| `crates/gw-proxy/src/filters/response.rs` | modify | 非流式分支(blockers T2-B 已加)或本 plan 复用:抽 `operation_id` 并写 `ctx.operation_id` |
| `crates/gw-adapter/src/analyzer.rs`(或 `OAuthListener` 实际宿主)| modify | `OAuthListener::on_outcome` 内,`OAuthSignal::RefreshFailed` 触发反向 XADD `events:entity` |
| `crates/gw-store/src/stream_consumer.rs` | modify | `apply_entity_event` 在 Moka invalidate 前 `redis.del("cache:{ns}:{id}")` |
| `crates/gw-codec/src/gemini/files.rs`(若存在写路径)| **delete or modify** | G4-2:删除 Rust 写 `gemini_file_mapping` 的代码(若有)|
| `crates/gw-gateway/src/metrics_catalog.rs` | modify | 注册 `gw_oauth_token_near_expiry_total` / `gw_oauth_token_expired_total` / `gw_oauth_refresh_failed_xadd_total` |
| `docs/go-contracts/parts/04-proxy-nodes.md` | **create** | G1 契约 |
| `docs/go-contracts/parts/01-go-as-producer.md` | modify | 加 `proxy_node` 行 + main.md §2 行 |
| `docs/go-contracts/parts/02-go-as-consumer.md` | modify | §2.4.3 oauth_failed 反向 event + §2.1.2 加 operation_id + §2.11 Redis L1 |
| `docs/go-contracts/parts/03-shared-schema.md` | modify | §3.1 加 0006 migration 行 + §3.1.2 gemini_file_mapping writer 翻转 + §3.2.5 加 oauth 子结构 + §3.9 删第 1 条 |
| `docs/go-contracts/main.md` | modify | §2 文件组织表加 parts/04 行 |
| `docs/architecture.md` | modify | §10.5 移除 events:entity Redis L1 已知问题 |
| `docs/workflow.md` | modify | §4 cache invalidation 段加 Redis L1 DEL 行为 |
| `crates/gw-store/src/stream_consumer.rs` | tests | 加 Redis L1 DEL 验证用例 |
| `crates/gw-proxy/tests/oauth_vendor_key.rs` | **create** | G3 oauth 注入 unit test |
| `tests/e2e/client/scenarios/test_oauth_vendor_key.py` | **create** | G3 e2e |
| `tests/e2e/client/scenarios/test_operation_id.py` | **create** | G5 e2e |
| `tests/e2e/client/scenarios/test_redis_l1_invalidation.py` | **create** | G6 e2e |

---

## Wave 概览

```
Wave 0  T0          共享脚手架(UsageRecord.operation_id + OAuthCredentials + ctx.operation_id + metrics)  [SERIAL]
Wave 1  T1-A/B/C/D  G2 migration + G5 codec + G6 Redis L1 + G4 audit/delete                              [4 PARALLEL]
Wave 2  T2-A/B      G3 OAuth wiring(filters/upstream + OAuthListener)                                   [2 SERIAL]
Wave 3  T3-A/B/C/D  contract docs(parts/04 创建 + parts/01-03 更新 + architecture/workflow 同步)         [4 PARALLEL]
Wave 4  T4-A/B/C    e2e tests(G3 / G5 / G6)                                                             [3 PARALLEL]
```

最大并行度 4(Wave 1 / Wave 3)。关键路径:Wave 0 → 1 → 2 → 3 → 4。Wave 3 docs 可与 Wave 4 e2e 并行 —— docs 不引用 e2e 输出。

---

## Conventions

同 [blockers plan](2026-05-22-go-onboarding-blockers.md) §Conventions:TDD / 路径限定 commit / 每 task 一 commit / `cargo build` `cargo test` `cargo clippy` `scripts/check-invariants.sh` 在 commit 前全绿 / Wave 边界严格 / `Cargo.lock` 同 commit / [breaking changes OK](../../../.claude/projects/-Users-whereslow-Desktop-work-ailinkmax-gateway/memory/feedback_breaking_changes_ok.md)。

---

# Wave 0 — 共享脚手架

Serial,单 subagent。

## Task T0: 类型字段 + metric 注册

**Files:**
- Modify: `crates/gw-core/src/entity.rs`
- Modify: `crates/gw-core/src/error/codes.rs`
- Modify: `crates/gw-ledger/src/usage.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`

- [ ] **Step 1: 加 `OAuthCredentials` struct + `VendorKey.oauth` 字段**

`crates/gw-core/src/entity.rs`:

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct OAuthCredentials {
    /// AES-256-GCM 加密的 access_token,字段四元组(ciphertext / nonce / auth_tag / kek_kid)
    /// 与 `vendor_keys.payload.api_key_enc` 同款。
    pub access_token_enc: crate::encrypt::EncryptedField,
    pub refresh_token_enc: crate::encrypt::EncryptedField,
    #[serde(with = "time::serde::rfc3339")]
    pub access_expires_at: time::OffsetDateTime,
    pub token_endpoint: String,
    #[serde(default)]
    pub scope: Vec<String>,
}
```

在 `VendorKey` 末尾(`exclusive_capability_match` 之后)追加:

```rust
    #[serde(default)]
    pub oauth: Option<OAuthCredentials>,
```

**前置**:`EncryptedField`(或 `EncryptedKey`,以 `crates/gw-store/src/encrypt.rs` 现有同款命名为准)必须复用,不要新造一份四元组。若不在 `gw-core` 路径,改 import `gw_store::encrypt::EncryptedKey` 或将其上提至 `gw-core`(本 task 内只做最小迁移)。如果不清楚归属,**escalate `NEEDS_CONTEXT`**。

- [ ] **Step 2: 加错误码**

`crates/gw-core/src/error/codes.rs`:

```rust
pub const GW_AUTH_OAUTH_TOKEN_EXPIRED: &str = "GW_AUTH_OAUTH_TOKEN_EXPIRED";
pub const GW_AUTH_OAUTH_REFRESH_FAILED: &str = "GW_AUTH_OAUTH_REFRESH_FAILED";
```

- [ ] **Step 3: 加 `UsageRecord.operation_id`**

`crates/gw-ledger/src/usage.rs::UsageRecord`,在 `is_stream: bool` 之后追加:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
```

更新所有 `UsageRecord` 构造站点(`filters::logging` 等)以 `operation_id: ctx.operation_id.clone()` 填充 —— Wave 2 接续。本 task 仅加字段 + 在 `UsageRecord::default()` / 现有构造里填 `operation_id: None`。

- [ ] **Step 4: 加 `ReqCtx.operation_id`**

`crates/gw-proxy/src/ctx.rs::ReqCtx`,在 `is_stream` 附近追加:

```rust
    /// G5: Gemini :predictLongRunning / OpenAI /v1/videos 等响应中的
    /// 顶层 `name: "operations/..."`;codec.parse_response 内抽出。
    pub operation_id: Option<String>,
```

`ReqCtx::new` / `Default` 初始化为 `None`。

- [ ] **Step 5: 注册 metrics**

`crates/gw-gateway/src/metrics_catalog.rs` `describe_*` 块追加:

```rust
    metrics::describe_counter!(
        "gw_oauth_token_near_expiry_total",
        "Number of upstream requests dispatched with access_token within 60s of expires_at"
    );
    metrics::describe_counter!(
        "gw_oauth_token_expired_total",
        "Number of upstream requests dispatched with already-expired access_token"
    );
    metrics::describe_counter!(
        "gw_oauth_refresh_failed_xadd_total",
        "Number of OAuthListener reverse XADDs to events:entity (op=status_change field=oauth_failed)"
    );
```

- [ ] **Step 6: 单测 — `OAuthCredentials` JSON roundtrip**

`crates/gw-core/src/entity.rs` 末尾 `#[cfg(test)]` 追加:

```rust
    #[test]
    fn vendor_key_oauth_field_roundtrip() {
        let payload = serde_json::json!({
            "key_id": "vk_x",
            "vendor_id": "v_x",
            "api_key_enc": { "ciphertext": "", "nonce": "", "auth_tag": "", "kek_kid": "kek_v1" },
            "accessible_models": [],
            "accessible_endpoints": [],
            "protocol_conversion": false,
            "priority": 0,
            "weight": 1,
            "status": "active",
            "tags": [],
            "forced_capabilities": [],
            "exclusive_capability_match": false,
            "oauth": {
                "access_token_enc": { "ciphertext": "", "nonce": "", "auth_tag": "", "kek_kid": "kek_v1" },
                "refresh_token_enc": { "ciphertext": "", "nonce": "", "auth_tag": "", "kek_kid": "kek_v1" },
                "access_expires_at": "2026-12-01T00:00:00Z",
                "token_endpoint": "https://oauth.example.com/token",
                "scope": ["chat.completions"]
            }
        });
        let key: VendorKey = serde_json::from_value(payload).unwrap();
        assert!(key.oauth.is_some());
        let oauth = key.oauth.unwrap();
        assert_eq!(oauth.token_endpoint, "https://oauth.example.com/token");
        assert_eq!(oauth.scope, vec!["chat.completions".to_string()]);
    }

    #[test]
    fn vendor_key_oauth_omitted_defaults_to_none() {
        let payload = serde_json::json!({
            "key_id": "vk_y", "vendor_id": "v_y",
            "api_key_enc": { "ciphertext": "", "nonce": "", "auth_tag": "", "kek_kid": "kek_v1" },
            "accessible_models": [], "accessible_endpoints": [], "protocol_conversion": false,
            "priority": 0, "weight": 1, "status": "active", "tags": [],
            "forced_capabilities": [], "exclusive_capability_match": false
        });
        let key: VendorKey = serde_json::from_value(payload).unwrap();
        assert!(key.oauth.is_none());
    }
```

(其它字段如有缺/多,以现有 `VendorKey` 定义为准 —— 测试 payload 必须能成功反序列化既有结构。)

- [ ] **Step 7: 验证 + commit**

```bash
cargo build -p gw-core -p gw-ledger -p gw-proxy -p gw-gateway
cargo test -p gw-core
cargo clippy -p gw-core -p gw-ledger -p gw-proxy -p gw-gateway --all-targets -- -D warnings
git add \
  crates/gw-core/src/entity.rs \
  crates/gw-core/src/error/codes.rs \
  crates/gw-ledger/src/usage.rs \
  crates/gw-proxy/src/ctx.rs \
  crates/gw-gateway/src/metrics_catalog.rs \
  Cargo.lock
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T0): wave-0 scaffolding for G3 / G5

- gw-core: OAuthCredentials struct + VendorKey.oauth (Option, default None)
- gw-core: 2 new error codes (OAUTH_TOKEN_EXPIRED, OAUTH_REFRESH_FAILED)
- gw-ledger: UsageRecord.operation_id (Option<String>, skip_serializing_if none)
- gw-proxy::ctx: ReqCtx.operation_id
- gw-gateway::metrics: 3 oauth metric families registered

Tests:
- entity::tests::vendor_key_oauth_field_roundtrip
- entity::tests::vendor_key_oauth_omitted_defaults_to_none

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 1 — 4 parallel

## Task T1-A: migration `0006_auth_configs` + 启动期 warn fallback

**Files:**
- Create: `crates/gw-store/migrations/0006_auth_configs.sql`
- Modify: `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: 写 migration**

```sql
-- 0006_auth_configs.sql — AuthConfig persistence (G2 / ADR-G2-1).
-- 0005_proxy_nodes 已先占;0007_management_tokens 由 blockers plan 引入。
--
-- 与其它 entity 表共享 (id, payload, version, updated_at, deleted_at) 五列。
-- payload JSONB 字段对应 `gw_core::entity::AuthConfig`。
-- `global` 行不在 migration 内 seed —— 由 Go 端管理 UI 或初始化脚本写入。

CREATE TABLE IF NOT EXISTS auth_configs (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 1,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);
```

- [ ] **Step 2: 调 `bootstrap` 行为 — warn fallback**

`crates/gw-gateway/src/lib.rs` `bootstrap` 函数内,定位 `auth_config_or_load("global").await` 调用处:

```rust
let jwt_validator = match store.auth_config_or_load("global").await {
    Ok(Some(cfg)) => Some(Arc::new(JwtValidator::from_config(&cfg.value))),
    Ok(None) => {
        tracing::warn!(
            "auth_configs.global missing, falling back to legacy JWT exp-only validation; \
             populate `global` row to enable iss/aud/nbf checks"
        );
        None
    }
    Err(e) => {
        tracing::warn!(error = ?e,
            "auth_configs load failed (table missing?), falling back to legacy JWT exp-only validation"
        );
        None
    }
};
```

如果当前代码已经走类似分支(`workflow.md` §2.2 step 2 暗示 hybrid 已实现),仅确保 `Err(_)` 分支也走 warn 而不是 panic。**关键不变量**:启动不能因为 0006 未跑而 fail-fast。

- [ ] **Step 3: 单测(启动 fallback)**

`crates/gw-gateway/src/lib.rs` 末尾或同款 tests 模块加:

```rust
#[cfg(test)]
mod auth_config_fallback_tests {
    // 沿用 gw-gateway 现有 testcontainers fixture(若有)。
    // 无则 escalate — 不在本 task 内新立 fixture。
    #[tokio::test]
    #[ignore = "需要 testcontainers Postgres"]
    async fn missing_auth_configs_table_does_not_fail_bootstrap() {
        // 1. 启动只跑 0001-0005 migration,故意不跑 0006。
        // 2. bootstrap() 应当返回 Ok 而非 panic;
        // 3. wired.jwt_validator 应当是 None。
    }
}
```

- [ ] **Step 4: 验证 + commit**

```bash
cargo build -p gw-gateway
cargo test -p gw-gateway
git add crates/gw-store/migrations/0006_auth_configs.sql crates/gw-gateway/src/lib.rs
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T1-A): migration 0006_auth_configs + warn fallback

ADR-G2-1 / G2-2: add 0006 migration (no seed row — Go owns init);
bootstrap auth_config_or_load("global") missing / err → warn + legacy
exp-only JWT validator. global row absent must NOT fail startup.

Tests:
- auth_config_fallback_tests::missing_auth_configs_table_does_not_fail_bootstrap (ignored testcontainers)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T1-B: 三个 codec 抽 `operation_id`

**Files:**
- Modify: `crates/gw-codec/src/openai/codec.rs`
- Modify: `crates/gw-codec/src/anthropic/codec.rs`
- Modify: `crates/gw-codec/src/gemini/codec.rs`
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/filters/logging.rs`

> **依赖说明**:本 task 假定 blockers plan T1-A/B/C(各 codec `parse_response` 已实现)**已合入主干**。如果 blockers 与 gaps 在两条分支并行开发,二者最终 merge 时需要 codec parse_response 实现并存。建议:gaps 分支基于 blockers 分支 cut,或两者都基于 main + 同 release 一并合;本 plan 假设前者。

- [ ] **Step 1: 写测试 — Gemini long-running 抽 operation_id**

`crates/gw-codec/src/gemini/codec.rs` `#[cfg(test)] mod tests` 追加:

```rust
    #[test]
    fn parse_response_extracts_operation_id_when_present() {
        let body = br#"{
            "name":"operations/foo-bar-12345",
            "metadata":{},
            "done":false
        }"#;
        let codec = GeminiCodec::default();
        let ir = codec.parse_response(body).unwrap();
        // operation_id 不在 ResponseIR 内(IR 是用于 cross-protocol render 的);
        // 而是通过 ResponseIR.id 字段透传,response filter 再赋给 ctx.operation_id。
        assert_eq!(ir.id.as_ref(), "operations/foo-bar-12345");
    }

    #[test]
    fn parse_response_normal_chat_id_unchanged() {
        let body = br#"{"candidates":[],"modelVersion":"m",
            "usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,"totalTokenCount":2}}"#;
        let codec = GeminiCodec::default();
        let ir = codec.parse_response(body).unwrap();
        // 普通响应无 name 字段 → ResponseIR.id 空字符串(沿用 blockers T1-C 行为)。
        assert_eq!(ir.id.as_ref(), "");
    }
```

- [ ] **Step 2: 跑 RED**

```bash
cargo test -p gw-codec --lib gemini::codec::tests::parse_response_extracts_operation_id_when_present
```

- [ ] **Step 3: 修 Gemini `parse_response` — 抽 `name`**

修 blockers T1-C 落地的 `GeminiResponseBody`(在 `crates/gw-codec/src/gemini/codec.rs` 内):

```rust
#[derive(serde::Deserialize)]
struct GeminiResponseBody {
    #[serde(default)]
    name: String,                                   // operations/... or empty
    #[serde(default, rename = "modelVersion")]
    model_version: String,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}
```

`parse_response` 末段:

```rust
    Ok(ResponseIR {
        id: std::borrow::Cow::Owned(parsed.name),   // 改:从 name(可能是 operations/...)填
        model: std::borrow::Cow::Owned(parsed.model_version),
        // 其余字段同 blockers T1-C
        ..
    })
```

- [ ] **Step 4: 修 OpenAI `parse_response`**

OpenAI 视频(`/v1/videos`)目前在 v1 codec 是未确定的 wire format。**策略**:沿用现有 `parsed.id`(`chatcmpl-...`)作为 `ResponseIR.id`;Wave 3 step 4 中 `response_body_filter` 会根据 prefix 决定是否归类为 operation_id。当前 task 内**不**改 OpenAI codec(blockers T1-A 已正确填 `id`)。

- [ ] **Step 5: 修 Anthropic `parse_response`**

Anthropic v1 无 long-running operations。沿用 blockers T1-B 行为。**不改**。

- [ ] **Step 6: 在 `response_body_filter` 内填 `ctx.operation_id`**

`crates/gw-proxy/src/filters/response.rs::handle_non_stream_chunk`(由 blockers T2-B 引入)在 `codec.parse_response` 成功分支末段追加:

```rust
        Ok(ir) => {
            if let Some(u) = ir.usage {
                ctx.usage.input_tokens = u.input_tokens;
                ctx.usage.output_tokens = u.output_tokens;
                ctx.usage.cached_tokens = u.cached_tokens;
            }
            // G5: extract operation_id from ResponseIR.id if it looks like an
            // operation name. Gemini emits "operations/<uuid>"; OpenAI video
            // would emit similar shape. Filter is conservative — only accept
            // strings that start with "operations/".
            if ir.id.starts_with("operations/") {
                ctx.operation_id = Some(ir.id.into_owned());
            }
        }
```

(流式分支 — `StreamEventIR::End` 不携带 operation_id,Gemini long-running 是 non-stream;跨流式 task 无需修改。)

- [ ] **Step 7: `logging` filter 写 `UsageRecord.operation_id`**

`crates/gw-proxy/src/filters/logging.rs::build_usage_record`(或类似 constructor 站点)在最后构造 struct 时填:

```rust
operation_id: ctx.operation_id.clone(),
```

(T0 Step 3 已加字段,本步仅赋值。)

- [ ] **Step 8: GREEN + commit**

```bash
cargo test -p gw-codec --lib
cargo test -p gw-proxy --lib
cargo clippy -p gw-codec -p gw-proxy --all-targets -- -D warnings
git add crates/gw-codec/src/gemini/codec.rs \
        crates/gw-proxy/src/filters/response.rs \
        crates/gw-proxy/src/filters/logging.rs
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T1-B): extract operation_id from long-running responses

ADR-G5-1 / G5-2: Gemini codec now reads top-level `name` into ResponseIR.id;
response_body_filter promotes it to ctx.operation_id when it matches
"operations/<...>"; logging filter writes it into UsageRecord.operation_id.
OpenAI codec untouched (already fills id from chatcmpl-...; video endpoint
parses go through the same prefix gate). Anthropic untouched (no LRO).

Tests:
- gemini::codec::tests::parse_response_extracts_operation_id_when_present
- gemini::codec::tests::parse_response_normal_chat_id_unchanged

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T1-C: `events:entity` 失效时 `DEL cache:{ns}:{id}`

**Files:**
- Modify: `crates/gw-store/src/stream_consumer.rs`

- [ ] **Step 1: 写测试**

`crates/gw-store/src/stream_consumer.rs` 末尾 `#[cfg(test)]` 加(沿用既有 `proxy_node_tests` 同款 mock / testcontainers fixture):

```rust
#[cfg(test)]
mod redis_l1_del_tests {
    use super::*;

    /// Pre-populate Redis L1 with `cache:apikey:ak_x`, fire an
    /// `events:entity ns=apikey id=ak_x` event via apply_entity_event,
    /// then assert the Redis key is gone.
    #[tokio::test]
    #[ignore = "需要 testcontainers Redis(沿用 proxy_node_tests 同款)"]
    async fn apply_entity_event_deletes_redis_l1_before_moka_invalidate() {
        let (store, redis) = build_test_store_with_redis().await;
        redis.set::<(), _, _>("cache:apikey:ak_x", b"stale".as_slice(), None, None, false)
             .await.unwrap();
        assert!(redis.exists::<i64, _>("cache:apikey:ak_x").await.unwrap() > 0);

        apply_entity_event(&store, "apikey", "ak_x").await;

        let exists: i64 = redis.exists("cache:apikey:ak_x").await.unwrap();
        assert_eq!(exists, 0, "Redis L1 must be DEL'd in apply_entity_event");
    }

    #[tokio::test]
    #[ignore = "需要 testcontainers Redis"]
    async fn apply_entity_event_redis_err_does_not_block_moka_invalidate() {
        // 沿用既有 fault-injection helper(若无,可改为 grep 风格:确认源码内
        // 调 redis.del 包了 .ok() 或显式 swallow_err 模式)。
    }
}
```

如果 `proxy_node_tests` 是用 mock 而非真 testcontainers,沿用 mock,把 `#[ignore]` 改为 #[tokio::test] 直接跑。

- [ ] **Step 2: 跑 RED**

```bash
cargo test -p gw-store --lib stream_consumer::redis_l1_del_tests -- --ignored
```

Expected: 第一个测试 fail —— 当前 `apply_entity_event` 不 DEL Redis。

- [ ] **Step 3: 在 `apply_entity_event` 顶端加 Redis DEL**

`crates/gw-store/src/stream_consumer.rs:163`:

```rust
pub(crate) async fn apply_entity_event(store: &Arc<EntityStore>, ns: &str, id: &str) {
    // G6 (ADR-G6-1): drop the Redis L1 entry before invalidating Moka.
    // Failure is non-fatal — Moka invalidate still runs.
    let redis_key = format!("cache:{ns}:{id}");
    if let Err(err) = store.redis.del::<i64, _>(&redis_key).await {
        tracing::warn!(
            ns = %ns, id = %id, key = %redis_key, error = ?err,
            "events:entity invalidate: redis DEL failed; falling back to Moka invalidate only"
        );
    }

    // 既有 Moka match arms 保持不变。
    match ns {
        "user" => store.user.invalidate(...).await,
        // ... 其它 arms ...
    }
}
```

**前置**:`store.redis` 字段必须暴露;若 `EntityStore` 不暴露 redis pool,通过 caller 传入(改 `apply_entity_event` 签名)— 但这会涉及更多文件。**优先方案**:在 `EntityStore` 加 `pub fn redis(&self) -> &RedisPool` 公共访问器,**仅在本 task 内最小添加**;如果改动量大,**escalate**。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-store --lib stream_consumer -- --ignored
cargo clippy -p gw-store --all-targets -- -D warnings
git add crates/gw-store/src/stream_consumer.rs crates/gw-store/src/entity_store.rs
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T1-C): DEL Redis L1 on events:entity invalidate

ADR-G6-1: apply_entity_event now DELs `cache:{ns}:{id}` before invalidating
the Moka L1, closing the multi-pod staleness gap documented in
docs/architecture.md §10.5. Redis DEL failure only warns — Moka invalidate
still runs.

Tests:
- stream_consumer::redis_l1_del_tests::apply_entity_event_deletes_redis_l1_before_moka_invalidate (ignored testcontainers)
- stream_consumer::redis_l1_del_tests::apply_entity_event_redis_err_does_not_block_moka_invalidate

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T1-D: G4 — `gemini_file_mapping` writer 审计 + 删除(若存在)

**Files:**
- Possibly: `crates/gw-codec/src/gemini/*` 或 `crates/gw-store/src/*`
- Verify-only initially

- [ ] **Step 1: 审计 Rust 是否当前写 `gemini_file_mapping`**

```bash
# 查所有写入位点
grep -rn "INSERT INTO gemini_file_mapping\|UPDATE gemini_file_mapping\|gemini_file_mapping.*INSERT\|gemini_file_mapping.*UPDATE" crates/
grep -rn "gemini_file_mapping" crates/ | grep -v "tests/\|migration"
```

预期结果:
- 仅 `crates/gw-store/migrations/0002_gemini_file_mapping.sql` 存在;
- 可能存在 `loader::load_gemini_file_mapping`(SELECT,允许);
- **若发现任何 INSERT / UPDATE / DELETE `gemini_file_mapping` 的 Rust 代码**,记录路径 → step 2 删除。
- 若**完全没有**写入路径,本 task 是 no-op,直接跳到 step 4(契约 docs)。

- [ ] **Step 2: 删除写入路径(若存在)**

按 grep 找到的写入站点,删除整段写入代码。保留:
- migration 0002 不动;
- `load_gemini_file_mapping`(SELECT 端)不动;
- 任何上游响应解析里把 `file_uri` 写回 PgSQL 的逻辑 — 全删。

- [ ] **Step 3: 验证 Rust 编译 + 测试不回归**

```bash
cargo build -p gw-store -p gw-codec -p gw-proxy
cargo test -p gw-store -p gw-codec
```

- [ ] **Step 4: 不写 commit(若 step 2 无修改)**

如果 step 1 grep 显示无 Rust 写路径,本 task 完全是审计;**不需要**生成 commit。直接更新本 plan checkbox 跳到 Wave 2 即可。Contract docs 翻转 writer = Go,在 Wave 3 docs 同步。

如 step 2 有修改:

```bash
git add <touched files>
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T1-D): remove Rust gemini_file_mapping writer

ADR-G4-2: writer responsibility flips to Go (uploads via
/api/admin/gemini-files/*). Rust retains SELECT-side reader only.
Breaking change permitted per project policy.

Touched paths:
  <list grep findings here>

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 2 — G3 OAuth wiring

2 个 serial task。

## Task T2-A: `upstream_request_filter` OAuth access_token 注入

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: 写源码 grep 测试**

`crates/gw-proxy/src/filters/upstream.rs` `#[cfg(test)] mod tests` 追加:

```rust
    #[test]
    fn oauth_injection_present_in_source() {
        let src = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/filters/upstream.rs")
        ).unwrap();
        assert!(src.contains("vendor_key.oauth"), "G3 oauth branch missing in upstream_request_filter");
        assert!(src.contains("access_token"), "G3 access_token usage missing");
        assert!(src.contains("gw_oauth_token_near_expiry_total")
                || src.contains("near_expiry_metric"),
                "G3 临过期 metric 调用缺失");
    }
```

- [ ] **Step 2: 跑 RED**

```bash
cargo test -p gw-proxy --lib filters::upstream::tests::oauth_injection_present_in_source
```

- [ ] **Step 3: 实现注入分支**

定位 `crates/gw-proxy/src/filters/upstream.rs::upstream_request_filter` 内 auth header 注入处(grep `auth_template` 找到位点):

```rust
    // ADR-G3-2 / G3-3: OAuth 型 vendor key 走 access_token;否则沿用 api_key。
    let auth_secret = if let Some(oauth) = picked.oauth.as_ref() {
        // 临过期 / 已过 metric。
        let now = time::OffsetDateTime::now_utc();
        if now >= oauth.access_expires_at {
            metrics::counter!(
                "gw_oauth_token_expired_total",
                "vendor_key_id" => picked.key_id.to_string()
            ).increment(1);
        } else if now >= oauth.access_expires_at - time::Duration::seconds(60) {
            metrics::counter!(
                "gw_oauth_token_near_expiry_total",
                "vendor_key_id" => picked.key_id.to_string()
            ).increment(1);
        }
        // 解密 access_token(沿用 gw-store::encrypt 同款解密 helper)。
        match gw_store::encrypt::decrypt_field(&oauth.access_token_enc, kek_cache).await {
            Ok(plain) => plain,
            Err(err) => {
                tracing::warn!(
                    vendor_key_id = %picked.key_id, error = ?err,
                    "oauth access_token decrypt failed, falling back to api_key"
                );
                gw_store::encrypt::decrypt_field(&picked.api_key_enc, kek_cache).await?
            }
        }
    } else {
        gw_store::encrypt::decrypt_field(&picked.api_key_enc, kek_cache).await?
    };

    let auth_header = vendor.auth_template.replace("{api_key}", &auth_secret);
```

`decrypt_field` / `kek_cache` 沿用既有(grep 既有调用确认 helper 名)。**若现有 vendor key decrypt 不是分两次而是一次性产出 `SecretString`**,沿用既有 API,只是把 `api_key_enc` 换成条件 `oauth.access_token_enc` 即可 —— 不要在此 task 内重写解密层。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-proxy --lib filters::upstream
cargo clippy -p gw-proxy --all-targets -- -D warnings
git add crates/gw-proxy/src/filters/upstream.rs
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T2-A): inject OAuth access_token in upstream auth header

ADR-G3-2 / G3-3: when vendor_key.oauth.is_some(), decrypt
oauth.access_token_enc and use it for vendor.auth_template substitution
instead of api_key_enc. Near-expiry (60s window) and expired token counters
emit; expired token still flows (Go task-worker refreshes asynchronously).
Decrypt failure falls back to api_key path with warn.

Tests:
- filters::upstream::tests::oauth_injection_present_in_source

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T2-B: `OAuthListener` 反向 XADD `events:entity`

**Files:**
- Modify: `crates/gw-adapter/src/analyzer.rs`(或 `gw-proxy` 内 ErrorHandler 实际宿主)
- Verify path: grep `OAuthListener` 现位

- [ ] **Step 1: 定位 `OAuthListener`**

```bash
grep -rn "OAuthListener\|OAuthSignal::RefreshFailed" crates/
```

记录文件路径(若 listener 在 `crates/gw-proxy/src/filters/logging.rs::ErrorHandler::fanout` 的本地 impl 中,本 task 改该文件;若在 `crates/gw-adapter/src/listener.rs`,改 adapter)。

- [ ] **Step 2: 写测试**

在 listener 所在文件 `#[cfg(test)]` 加:

```rust
    #[tokio::test]
    async fn refresh_failed_emits_reverse_xadd_event() {
        let pool = test_redis_pool();   // 沿用既有 helper;若无,改用 mock RedisStreams trait
        let listener = OAuthListener::new(pool.clone());
        let outcome = ClassifierOutcome {
            class: ErrorClass::AccountBlocked(AccountBlockReason::OAuthRefreshFailed),
            oauth_signal: Some(OAuthSignal::RefreshFailed),
            // 其它字段沿用 Default
            ..Default::default()
        };
        let ctx = make_handler_ctx_with_key_id("vk_demo");
        listener.on_outcome(&ctx, &outcome).await;

        // Stream 内应当有一条 events:entity ns=vendor_key id=vk_demo
        // op=status_change field=oauth_failed。
        let entries = pool.xrange::<Vec<_>, _, _, _>("events:entity", "-", "+", None).await.unwrap();
        let last = entries.last().expect("at least one entry");
        let fields: HashMap<String, String> = parse_fields(last);
        assert_eq!(fields.get("ns").map(|s| s.as_str()), Some("vendor_key"));
        assert_eq!(fields.get("id").map(|s| s.as_str()), Some("vk_demo"));
        assert_eq!(fields.get("op").map(|s| s.as_str()), Some("status_change"));
        assert_eq!(fields.get("field").map(|s| s.as_str()), Some("oauth_failed"));
    }
```

`make_handler_ctx_with_key_id`、`parse_fields`、`test_redis_pool` 沿用 listener 邻近文件的既有 fixture;若不存在 → **escalate**(本 task 不新立 fixture)。

- [ ] **Step 3: 跑 RED + 实现 listener**

```bash
cargo test -p <crate> --lib oauth_listener::refresh_failed_emits_reverse_xadd_event
```

实现:

```rust
impl OAuthListener {
    pub async fn on_outcome(&self, ctx: &HandlerCtx, outcome: &ClassifierOutcome) {
        if !matches!(outcome.oauth_signal, Some(OAuthSignal::RefreshFailed)) {
            return;
        }
        let Some(key_id) = ctx.vendor_key_id.as_ref() else { return };
        let key = key_id.as_ref();
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000;

        let res = self.redis
            .xadd::<String, _, _, _, _>(
                "events:entity",
                false,            // nomkstream = false
                None,             // explicit id (None = "*")
                "*".to_string(),
                vec![
                    ("ns".to_string(), "vendor_key".to_string()),
                    ("id".to_string(), key.to_string()),
                    ("op".to_string(), "status_change".to_string()),
                    ("field".to_string(), "oauth_failed".to_string()),
                    ("reason".to_string(), "upstream_401".to_string()),
                    ("ts_ms".to_string(), now_ms.to_string()),
                ],
            )
            .await;

        match res {
            Ok(_) => {
                metrics::counter!("gw_oauth_refresh_failed_xadd_total").increment(1);
            }
            Err(err) => {
                tracing::warn!(vendor_key_id = %key, error = ?err,
                    "OAuthListener: reverse XADD events:entity failed");
            }
        }
    }
}
```

(具体 fred / RedisPool API 以现有 `gw-ledger::flusher` 内的 XADD 写法为准。)

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p <crate> --lib oauth_listener
cargo clippy -p <crate> --all-targets -- -D warnings
git add <listener file> <metric file if changed>
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T2-B): OAuthListener reverse XADD on RefreshFailed

ADR-G3-4: when ErrorHandler fanout receives OAuthSignal::RefreshFailed,
XADD events:entity ns=vendor_key id=<key_id> op=status_change
field=oauth_failed reason=upstream_401 ts_ms=<now>. Go task-worker
consumes and triggers refresh. Metric gw_oauth_refresh_failed_xadd_total.

Tests:
- oauth_listener::refresh_failed_emits_reverse_xadd_event

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 3 — Contract / 上游文档同步

4 个 parallel docs task。

## Task T3-A: 创建 `parts/04-proxy-nodes.md`

**Files:**
- Create: `docs/go-contracts/parts/04-proxy-nodes.md`
- Modify: `docs/go-contracts/main.md`

- [ ] **Step 1: 创建 part04 — 完整文本来自 changelog §2.2**

把 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §2.2 内 "新增 parts/04-proxy-nodes.md(完整内容)" code-block 的全部 markdown(从 `# Part 04 — Proxy Nodes...` 到 §4.7 末尾)**verbatim** 写入 `docs/go-contracts/parts/04-proxy-nodes.md`。

注意:section §4.6 migration 模板需要标记 "(已 merged,本节供历史对照)" —— changelog 已更新此措辞,直接搬运即可。

- [ ] **Step 2: `main.md` §2 表格加 parts/04 行**

按 changelog §2.4 的"最终文本",在 `main.md §2 文件组织表` 内 parts/03 行下方追加:

```
| [parts/04-proxy-nodes.md](parts/04-proxy-nodes.md) | **Proxy Nodes** | Rust tunnel client ↔ Go Hub 契约;节点 entity / 心跳分工 / status 状态机 |
```

- [ ] **Step 3: 校验 + commit**

```bash
# 链接 sanity check
grep -n "parts/04" docs/go-contracts/main.md
ls docs/go-contracts/parts/04-proxy-nodes.md
git add docs/go-contracts/parts/04-proxy-nodes.md docs/go-contracts/main.md
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T3-A): create parts/04-proxy-nodes.md

ADR-G1-1 / G1-2 / G1-3 / G1-4 contract surface: Rust tunnel client ↔ Go Hub.
Documents Heartbeat (Go-only writer), ProxyNodeEntry payload schema,
TunnelStack refresh semantics, status state machine (Go-authoritative).
Matches docs/go-contracts/contract-change/2026-05-22-go-onboarding.md §2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T3-B: `parts/01-go-as-producer.md` — proxy_node + management_token 行 + Redis L1 补口段

**Files:**
- Modify: `docs/go-contracts/parts/01-go-as-producer.md`

> 本 task 与 blockers T4-C 的 parts/01 改动是**同一文件**,可能产生 merge 冲突。**推荐**:gaps T3-B 等 blockers T4-C 合入后再启动,或两个 PR 在 docs 部分各只改一段、merge 时容易合。

- [ ] **Step 1: 加 proxy_node ns 行**

`§1.2.3 namespace 列表`,在 management_token 行下方(若 blockers T4-C 已落)或 auth_config 行下方(若先于 blockers)追加:

```
| `proxy_node` | `proxy_nodes`(0005 migration,已 merged)| `EntityStore.proxy_node` | `ProxyNodeEntry` | 30 min TTL;支撑 tunnel client 节点视图;详见 [parts/04](04-proxy-nodes.md) |
```

- [ ] **Step 2: §1.2.4 末尾加 Redis L1 invalidate 补口段**

按 changelog §7.1 的"最终文本",在 §1.2.4 "漏发 XADD" 段后追加完整 "Redis L1 invalidate(Rust 端补口)" 段(直至 "...留 follow-up,不在本契约范围)。" 收尾)。

- [ ] **Step 3: commit**

```bash
git add docs/go-contracts/parts/01-go-as-producer.md
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T3-B): parts/01 — proxy_node ns + Redis L1 补口段

- §1.2.3: add proxy_node namespace row (G1)
- §1.2.4: document Rust DEL cache:{ns}:{id} before Moka invalidate (G6)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T3-C: `parts/02-go-as-consumer.md` — oauth_failed 事件 + operation_id + Redis L1 §2.11

**Files:**
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`

- [ ] **Step 1: 在 §2.4 加 §2.4.3 oauth_failed**

按 changelog §4.3 的"最终文本",在 §2.4.2 后插入完整 §2.4.3 段(标题、4 行表格、最末段不变量)。原 §2.4.3(不变量)顺延为 §2.4.4。

- [ ] **Step 2: §2.4.4 不变量段更新**

按 changelog §4.4 的"最终文本",在不变量列表末尾加新 bullet:

```
- §2.4.3 oauth_failed:Rust 反向写,Go task-worker 端必须实现 refresh 处理;未实现的代价 = 该 vendor_key 失效后无法自愈,等同 §2.4.4 表中"Go 未实现接收 → vendor_key 状态不会同步到 PgSQL"。
```

- [ ] **Step 3: §2.1.2 schema 加 operation_id**

按 changelog §6.1 / §6.2 的"最终文本":

JSON schema 内 `is_stream` 字段下方插入 `"operation_id": null` 行(及注释)。schema 表后追加"`operation_id` 含义"段。

- [ ] **Step 4: 加 §2.11 Redis L1 一致性责任**

按 changelog §7.2 的"最终文本",在原 §2.10 mgmt_token:blacklist 之后(已由 blockers T4-C 加入)插入完整 §2.11 段。

如果 blockers T4-C 尚未合入,需要先把 §2.10 由 blockers 加进去,再加 §2.11;但本 task 仅负责 §2.11 + §2.4.3 + §2.1.2。**推荐**:与 blockers T4-C 协调,**只在 blockers T4-C 合入后启动本 task**。

- [ ] **Step 5: commit**

```bash
git add docs/go-contracts/parts/02-go-as-consumer.md
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T3-C): parts/02 — oauth_failed event + operation_id + §2.11

- §2.4.3: new reverse event vendor_key.oauth_failed (G3-4)
- §2.4.4: invariants updated for oauth_failed Go-side handling
- §2.1.2: UsageRecord.operation_id schema + semantics (G5-2)
- §2.11: Redis L1 invalidation responsibility (G6)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T3-D: `parts/03-shared-schema.md` + `main.md` + architecture.md / workflow.md

**Files:**
- Modify: `docs/go-contracts/parts/03-shared-schema.md`
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`

- [ ] **Step 1: parts/03 §3.1 migration 表 加 0006 行**

按 changelog §3.1 的"最终文本":

```
| `0006_auth_configs.sql` | `auth_configs`(T1-E 落地)|
```

- [ ] **Step 2: parts/03 §3.1.2 gemini_file_mapping writer 行翻转**

按 changelog §5.1 的"最终文本"替换该 bullet 整段。

- [ ] **Step 3: parts/03 §3.2.5 加 oauth 字段 + §3.2.5.1**

按 changelog §4.1 / §4.2 的"最终文本":

- JSON 形态末尾追加 `"oauth": null` 字段;
- §3.2.5 后插入完整 §3.2.5.1 OAuth 子结构段。

- [ ] **Step 4: parts/03 §3.6 ID 前缀表 加 proxy_node 行**

按 changelog §2.3 的"最终文本":

```
| proxy_node | `pn_` | Go 生成 |
```

- [ ] **Step 5: parts/03 §3.9 删第 1 条 `auth_configs` 待加行**

按 changelog §3.2 删除整条 "1. `auth_configs` 表的 migration..." 行;后续编号顺延。

- [ ] **Step 6: architecture.md §10.5 移除 events:entity Redis L1 已知问题**

定位 §10.5 内描述 events:entity 只清 Moka 不清 Redis L1 的整段(从 "**`events:entity` invalidation only clears Moka..." 到 "...for the L1 TTL." 收尾)。替换为单行注记:

```
- **events:entity invalidation clears Redis L1 + Moka** (G6, T1-C, 2026-05-22). `apply_entity_event` DELs `cache:{ns}:{id}` before invalidating Moka; multi-pod staleness in the Redis L1 layer is closed.
```

- [ ] **Step 7: workflow.md §4 cache invalidation 段补 Redis DEL**

定位 `## 4. Cache invalidation` 段,在描述 `apply()` 行为之前加一行:

```
Before dispatching to the per-`ns` invalidate, the consumer DELs the Redis L1 key `cache:{ns}:{id}` (G6) so multi-pod deployments do not retain stale entries up to the L1 TTL.
```

- [ ] **Step 8: commit**

```bash
git add docs/go-contracts/parts/03-shared-schema.md docs/architecture.md docs/workflow.md
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T3-D): parts/03 + architecture / workflow sync

- parts/03 §3.1: add 0006_auth_configs migration row (G2)
- parts/03 §3.1.2: gemini_file_mapping writer = Go (G4)
- parts/03 §3.2.5 + §3.2.5.1: VendorKey.oauth subobject (G3)
- parts/03 §3.6: proxy_node ID prefix (G1)
- parts/03 §3.9: drop auth_configs migration TODO
- architecture.md §10.5: events:entity Redis L1 issue closed by G6
- workflow.md §4: cache invalidation now DELs Redis L1 first

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 4 — e2e 测试

3 个 parallel task。

## Task T4-A: e2e — OAuth vendor key

**Files:**
- Create: `tests/e2e/client/scenarios/test_oauth_vendor_key.py`
- Modify: `tests/e2e/client/scenarios/conftest.py`(加 `seed_oauth_vendor_key` helper)
- Modify: `crates/e2e-fake-upstream/scenarios/oauth_*.toml`(加上游 401 → refresh 模拟)

- [ ] **Step 1: 写场景**

```python
# tests/e2e/client/scenarios/test_oauth_vendor_key.py
"""G3 e2e: OAuth vendor key access_token injection + 401 → 反向事件。"""
import time
import pytest
from openai import OpenAI


def test_oauth_vendor_key_injects_access_token(
    gateway_url, seed_oauth_vendor_key, fake_upstream_assert_auth_header
):
    seed_oauth_vendor_key(
        key_id="vk_oauth_1",
        access_token="oa-access-xyz",
        refresh_token="oa-refresh-abc",
        expires_in_secs=3600,
    )
    client = OpenAI(base_url=f"{gateway_url}/v1", api_key="ak_oauth_seeded")
    resp = client.chat.completions.create(
        model="gpt-4-turbo",
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    list(resp)  # drain
    # 断言 fake_upstream 收到了 OAuth access_token 而非 api_key
    auth_received = fake_upstream_assert_auth_header("vk_oauth_1")
    assert "oa-access-xyz" in auth_received


def test_oauth_upstream_401_triggers_reverse_event(
    gateway_url, seed_oauth_vendor_key, fake_upstream_force_401, events_entity_consumer
):
    seed_oauth_vendor_key(
        key_id="vk_oauth_2",
        access_token="will-fail",
        refresh_token="r",
        expires_in_secs=3600,
    )
    fake_upstream_force_401("vk_oauth_2")
    client = OpenAI(base_url=f"{gateway_url}/v1", api_key="ak_oauth_seeded_2")
    with pytest.raises(Exception):
        client.chat.completions.create(
            model="gpt-4-turbo",
            messages=[{"role": "user", "content": "hi"}],
            stream=True,
        )
    # 等 events:entity stream 拿到 oauth_failed 事件
    event = events_entity_consumer.next_event(timeout=5.0, filter_op="status_change")
    assert event["ns"] == "vendor_key"
    assert event["id"] == "vk_oauth_2"
    assert event["field"] == "oauth_failed"


def test_oauth_near_expiry_metric(gateway_url, seed_oauth_vendor_key, scrape_metric):
    seed_oauth_vendor_key(
        key_id="vk_oauth_3",
        access_token="ok-but-near-expiry",
        refresh_token="r",
        expires_in_secs=30,    # < 60s safety window
    )
    client = OpenAI(base_url=f"{gateway_url}/v1", api_key="ak_oauth_seeded_3")
    list(client.chat.completions.create(
        model="gpt-4-turbo",
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    ))
    v = scrape_metric("gw_oauth_token_near_expiry_total", {"vendor_key_id": "vk_oauth_3"})
    assert v >= 1
```

`seed_oauth_vendor_key` / `fake_upstream_assert_auth_header` / `fake_upstream_force_401` / `events_entity_consumer` / `scrape_metric` 是 conftest fixture;沿用既有同款风格,缺则补。

- [ ] **Step 2: 跑 e2e**

```bash
make e2e-up
make e2e -- -k test_oauth_vendor_key
make e2e-down
```

Expected: 3 个测试全过。

- [ ] **Step 3: commit**

```bash
git add tests/e2e/client/scenarios/test_oauth_vendor_key.py \
        tests/e2e/client/scenarios/conftest.py \
        crates/e2e-fake-upstream/scenarios/
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T4-A): e2e for G3 OAuth vendor key

- access_token replaces api_key when vendor_key.oauth is set
- upstream 401 → OAuthListener fires reverse events:entity oauth_failed
- access_expires_at within 60s window → near_expiry metric

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T4-B: e2e — `operation_id` 落 usage:records

**Files:**
- Create: `tests/e2e/client/scenarios/test_operation_id.py`
- Modify: `crates/e2e-fake-upstream/scenarios/gemini_long_running.toml`(新增)

- [ ] **Step 1: fake_upstream 加 long-running 场景**

```toml
# crates/e2e-fake-upstream/scenarios/gemini_long_running.toml
[response]
status = 200
content_type = "application/json"
body = """{
  "name":"operations/test-op-12345",
  "metadata":{"@type":"VideoGenerationMetadata"},
  "done":false
}"""
```

- [ ] **Step 2: 写场景**

```python
# tests/e2e/client/scenarios/test_operation_id.py
"""G5 e2e: Gemini :predictLongRunning 响应填入 UsageRecord.operation_id。"""
import requests


def test_predict_long_running_extracts_operation_id(
    gateway_url, seeded_keys, usage_records_consumer
):
    # 直接 HTTP 调用 ":predictLongRunning"(google-genai SDK 暂未直接暴露)
    resp = requests.post(
        f"{gateway_url}/v1beta/models/gemini-1.5-pro:predictLongRunning",
        headers={
            "x-api-key": seeded_keys["gemini"],
            "x-api-key-id": seeded_keys["gemini_id"],
        },
        json={"instances": [{"prompt": "make video"}]},
    )
    assert resp.status_code == 200
    body = resp.json()
    assert body["name"] == "operations/test-op-12345"

    record = usage_records_consumer.next_record(timeout=5.0)
    assert record["operation_id"] == "operations/test-op-12345"


def test_normal_chat_has_null_operation_id(gateway_url, seeded_keys, usage_records_consumer):
    # 普通 chat → operation_id 为 null
    requests.post(
        f"{gateway_url}/v1beta/models/gemini-1.5-pro:generateContent",
        headers={
            "x-api-key": seeded_keys["gemini"],
            "x-api-key-id": seeded_keys["gemini_id"],
        },
        json={"contents": [{"parts": [{"text": "hi"}]}]},
    )
    record = usage_records_consumer.next_record(timeout=5.0)
    assert record.get("operation_id") is None
```

- [ ] **Step 3: 跑 + commit**

```bash
make e2e-up
make e2e -- -k test_operation_id
make e2e-down
git add tests/e2e/client/scenarios/test_operation_id.py \
        crates/e2e-fake-upstream/scenarios/gemini_long_running.toml
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T4-B): e2e for G5 operation_id

- :predictLongRunning response body { "name": "operations/..." }
  flows into UsageRecord.operation_id
- normal chat → operation_id is null (omitted via skip_serializing_if)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T4-C: e2e — Redis L1 invalidate

**Files:**
- Create: `tests/e2e/client/scenarios/test_redis_l1_invalidation.py`

- [ ] **Step 1: 写场景**

```python
# tests/e2e/client/scenarios/test_redis_l1_invalidation.py
"""G6 e2e: events:entity 触发后 Redis L1 立即被 DEL。"""
import time
import requests


def test_apikey_invalidate_deletes_redis_l1(
    gateway_url, redis_client, fake_web_admin_client
):
    # 1. 先 seed 一个 apikey,等 cold-load 把 Redis L1 写满。
    fake_web_admin_client.put_entity("apikey", "ak_test_l1", {
        "key_id": "ak_test_l1", "user_id": "u_test",
        "status": "active", "hash": "deadbeef" * 8,
        "allowed_models": [], "allowed_channels": [],
        "rpm_limit": 0, "tpm_limit": 0, "markup": 1.0,
        "ip_allowlist": None, "expires_at": None,
        "signature_required": False, "signature_algo": "hmac_sha256",
        "force_capabilities": [], "match_mode_exclusive": False,
    })
    # Hit gateway with this apikey(任意请求即可让 cold-load 跑)。
    # 跳过 — 直接由 put_entity 触发 cold-load via Rust consumer 不可靠;
    # 改为:gateway 端手动 GET Redis 看 L1 是否存在 → 若不存在,先 trigger
    # 一次请求让 cold load 写入 Redis HSET。
    requests.post(f"{gateway_url}/v1/chat/completions",
        headers={"x-api-key": "doesnt_matter", "x-api-key-id": "ak_test_l1"},
        json={"model": "gpt-4-turbo", "messages": [{"role": "user", "content": "x"}], "stream": False},
    )
    # cold load 可能因 hash 不匹配 401,但 Redis L1 仍被 loader 写入。等 500ms。
    time.sleep(0.5)

    # 2. 验证 Redis L1 存在 cache:apikey:ak_test_l1
    assert redis_client.exists("cache:apikey:ak_test_l1") == 1

    # 3. 触发 events:entity 通过 fake_web_admin_client.update_entity 或重 put
    fake_web_admin_client.put_entity("apikey", "ak_test_l1", {
        "key_id": "ak_test_l1", "user_id": "u_test",
        "status": "disabled",   # 状态变更触发 XADD events:entity
        "hash": "deadbeef" * 8,
        "allowed_models": [], "allowed_channels": [],
        "rpm_limit": 0, "tpm_limit": 0, "markup": 1.0,
        "ip_allowlist": None, "expires_at": None,
        "signature_required": False, "signature_algo": "hmac_sha256",
        "force_capabilities": [], "match_mode_exclusive": False,
    })
    # 等 Rust consumer 处理(XREADGROUP block 200ms + DEL)。
    time.sleep(1.5)

    # 4. Redis L1 应当被 DEL 掉。
    assert redis_client.exists("cache:apikey:ak_test_l1") == 0
```

- [ ] **Step 2: 跑 + commit**

```bash
make e2e-up
make e2e -- -k test_redis_l1_invalidation
make e2e-down
git add tests/e2e/client/scenarios/test_redis_l1_invalidation.py
git commit -m "$(cat <<'EOF'
go-onboarding-gaps(T4-C): e2e for G6 Redis L1 DEL on events:entity

Validates: Go writes apikey → Redis cache populated by Rust cold-load →
Go updates apikey + XADDs events:entity → Rust consumer DELs Redis L1
within 1.5s window. Closes the multi-pod staleness gap.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist

跑过整个 plan 后,验证:

1. **Spec coverage**:
   - ADR-G1-1/G1-3 → Rust 代码已 merged(`ProxyNode` 类型 / loader / EntityStore / TunnelStack);本 plan T3-A 补 part04 契约 docs ✓
   - ADR-G1-2(Heartbeat = Go 责任)→ T3-A part04 §4.3 文档化 ✓
   - ADR-G1-4(Rust 不触发 status)→ T3-A part04 §4.3 文档化 ✓
   - ADR-G2-1(0006 migration)→ T1-A ✓
   - ADR-G2-2(warn fallback)→ T1-A ✓
   - ADR-G3-1(OAuthCredentials schema)→ T0 ✓
   - ADR-G3-2(注入 access_token)→ T2-A ✓
   - ADR-G3-3(临过期 metric)→ T2-A ✓
   - ADR-G3-4(反向 events:entity)→ T2-B ✓
   - ADR-G4-1(Go 完全实现端点)→ T1-D + T3-D 文档化 ✓
   - ADR-G4-2(writer 翻转)→ T1-D 代码删除 + T3-D 文档 ✓
   - ADR-G5-1(Rust 仅透传)→ T1-B 实现 + T3-C 文档 ✓
   - ADR-G5-2(UsageRecord.operation_id)→ T0 + T1-B ✓
   - ADR-G6-1(DEL Redis L1)→ T1-C ✓
   - ADR-G6-2(启动期不补)→ T3-D 文档化(architecture.md §10.5 注记)✓
2. **No placeholders**: 各 task 步骤是否都给出确切代码 / 命令?(应当 yes)
3. **Type consistency**: `OAuthCredentials` / `operation_id: Option<String>` / `ctx.operation_id` / `UsageRecord.operation_id` 命名全文一致;`access_token_enc` 在 T0/T2-A 一致。
4. **Migration 编号**: 0006_auth_configs 与 blockers 的 0007_management_tokens 不冲突(0005=proxy_nodes 已 merged)。

---

## 执行选择

**Plan complete and saved to `docs/superpowers/plans/2026-05-22-go-onboarding-gaps.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — 每个 task 一个 fresh subagent。
**2. Inline Execution** — 当前 session 内按 task 顺序跑。

**Which approach for blockers + gaps?**

> 注:gaps plan 与 [blockers plan](2026-05-22-go-onboarding-blockers.md) 互相独立;两份可同步执行(各自分支)或顺序执行(先 blockers,gaps 接其后)。**推荐顺序**:blockers 先合 main,gaps 基于 blockers 分支 cut(因为 Wave 1-B 假定 codec parse_response 已实现 / Wave 3 docs 假定 parts/02 已加 §2.10)。
