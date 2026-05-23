# Go 接入阻塞项 Implementation Plan(B1 + B2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 解锁 Go 控制面对接的两个硬阻塞 —— B1 同协议非流式响应、B2 Management Token 鉴权(`/healthz/*` 由 401 → 200);为 Go 落 FRD §6.2/§6.7/§6.8 扫清路。

**Architecture:** B1 在 `crates/gw-codec/{openai,anthropic,gemini}/codec.rs` 解锁 `parse_response`(unimplemented → 真实 serde 解析),`request_filter` 删除非流式 guard,`response_body_filter` 新增 non-stream end-of-stream 分支累积上游 body 并调 `parse_response` 抽 usage,跨协议非流式在 `pick_peer` 末端短路 503。B2 引入 `ManagementTokenEntry` entity(migration 0007,`0005_proxy_nodes`/`0006_auth_configs` 已先占),`crates/gw-auth/src/mgmt.rs` 增 `verify_mgmt_token` helper(SHA-256 + scope 比对 + 现有 `parse_mgmt_authorization` 复用),`filters::request` 管理路径分支接入校验,`gw-gateway::health` 三个 token-gated 路由从硬 401 改为校验后 200;mgmt 黑名单复用 `RedisJwtBlacklist` 扩 prefix。

**Tech Stack:** Rust 1.86 / Pingora 0.8 / tokio / serde_json / sqlx 0.8(runtime-checked,无 offline cache)/ fred 9 / moka(EntityStore L1)/ subtle(constant-time)/ time(OffsetDateTime)/ sha2(SHA-256)/ secrecy。

**Source spec**: [`docs/superpowers/specs/2026-05-22-go-onboarding-blockers-design.md`](../specs/2026-05-22-go-onboarding-blockers-design.md) — 实施前先读完。本 plan 执行该 spec 的全部 ADR-B1-* 与 ADR-B2-*。

**Companion changelog**: [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §1(mgmt_token 相关)—— Wave 4 docs 同步对照。

**Branch suggestion**: `feature/go-onboarding-blockers`(从 `origin/main` cut)。

---

## File structure(实施前的全表)

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/gw-core/src/error/codes.rs` | modify | 新增 8 个错误码:1 个 cross-protocol non-stream + 7 个 mgmt token |
| `crates/gw-core/src/entity.rs` | modify | 新增 `ManagementTokenEntry`、`MgmtScope` enum、`TokenStatus` 复用现有 `KeyStatus` |
| `crates/gw-store/migrations/0007_management_tokens.sql` | **create** | `management_tokens` 表 |
| `crates/gw-store/src/loader.rs` | modify | `load_mgmt_token` + SQL 常量 |
| `crates/gw-store/src/entity_store.rs` | modify | `mgmt_token` Moka cache(5 min TTL)+ `mgmt_token_or_load` + `invalidate_mgmt_token` |
| `crates/gw-store/src/stream_consumer.rs` | modify | `apply_entity_event` 增 `management_token` ns 分支 |
| `crates/gw-codec/src/openai/codec.rs` | modify | `parse_response` 解 ChatCompletion `usage` |
| `crates/gw-codec/src/anthropic/codec.rs` | modify | `parse_response` 解 Message `usage` |
| `crates/gw-codec/src/gemini/codec.rs` | modify | `parse_response` 解 GenerateContentResponse `usageMetadata` |
| `crates/gw-proxy/src/ctx.rs` | modify | 加 `upstream_body_accum: Option<BytesMut>`、`mgmt_user_id: Option<UserId>`、`mgmt_scope: Option<MgmtScope>` |
| `crates/gw-proxy/src/filters/request.rs` | modify | (a) 删除 `is_stream == false` short-circuit;(b) `is_management_path` 分支接入 mgmt token verify |
| `crates/gw-proxy/src/filters/response.rs` | modify | 非流式分支:累 `upstream_body_accum` → EOS 调 `parse_response` → 填 `ctx.usage` |
| `crates/gw-proxy/src/filters/upstream.rs` | modify | `pick_peer` 末端:跨协议 + 非流式 → 短路 503 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` |
| `crates/gw-auth/src/mgmt.rs` | modify | 新增 `verify_mgmt_token` helper(scope 矩阵 + SHA-256 + blacklist 查询)|
| `crates/gw-auth/src/jwt_blacklist.rs` | modify | `RedisJwtBlacklist` 加 `check_mgmt(&token_id)` 方法,namespace 用 `mgmt_token:blacklist:` |
| `crates/gw-gateway/src/health.rs` | modify | `/healthz/circuits`、`/healthz/key/`、`/healthz/resilience` 改为 verify 后返回 200 |
| `crates/gw-gateway/src/lib.rs` | modify | `Wired` 加 `mgmt_blacklist: Arc<RedisJwtBlacklist>`(与 jwt 共用实例);`run_health_server` 传入 |
| `crates/gw-gateway/src/metrics_catalog.rs` | modify | 新增 `gw_codec_parse_response_failure_total{codec,upstream_status}`、`gw_mgmt_token_verify_total{outcome}` |
| `crates/gw-proxy/tests/non_stream_e2e.rs` | **create** | 3 codec 同协议非流式 in-process 用例 |
| `crates/gw-gateway/tests/healthz_mgmt_token.rs` | **create** | `/healthz/*` mgmt token 用例 |
| `tests/e2e/client/scenarios/test_non_stream.py` | **create** | docker e2e 同协议非流式 3 个 SDK 场景 |
| `tests/e2e/client/scenarios/test_mgmt_token.py` | **create** | docker e2e mgmt token + `/healthz` 场景 |
| `tests/e2e/fake_web_server` 或 conftest | modify | 增写 `management_tokens` 行 + mgmt blacklist key 的辅助方法 |
| `docs/architecture.md` | modify | §8 删第 1 条(non-stream 拒绝围栏)+ §6.1 listener 表更新 `/healthz/*` 状态 |
| `docs/workflow.md` | modify | §2.2 step 1 mgmt 分支描述、step 9 移除非流式 guard |
| `docs/go-contracts/parts/01-go-as-producer.md` | modify | per changelog §1.1 + §2.1 加 `management_token` / `proxy_node` 行 |
| `docs/go-contracts/parts/02-go-as-consumer.md` | modify | per changelog §1.4 加 §2.10 mgmt_token:blacklist |
| `docs/go-contracts/parts/03-shared-schema.md` | modify | per changelog §1.2 加 §3.2.13 ManagementTokenEntry + §1.3 ID 前缀表 |

---

## Wave 概览

```
Wave 0  T0       共享脚手架(error codes + ctx fields + entity types + migration) [SERIAL]
Wave 1  T1-A/B/C/D  parse_response × 3 + mgmt_token 持久化层               [4 PARALLEL]
Wave 2  T2-A/B/C/D  filter wiring(B1 三处 + B2 黑名单)                    [4 PARALLEL,但 T2-B 等 T1-A/B/C]
Wave 3  T3-A/B/C    mgmt verify helper + filter 接入 + /healthz 解锁       [SERIAL]
Wave 4  T4-A/B/C    e2e tests + 文档 sync                                 [3 PARALLEL]
```

最大并行度 4(Wave 1)。关键路径:Wave 0 → 1 → 2 → 3 → 4。Wave 2 内部 T2-B(response 非流式分支)需要 Wave 1 的 T1-A/B/C 完成才能被 wave 2 启动 —— 这是 Wave 2 在 Wave 1 之后的原因。

---

## Conventions(每个 task 都遵循)

1. **TDD**: 先写失败测试 → 跑 `cargo test -p <crate>` 确认 RED → 写最小实现 → 确认 GREEN → commit。
2. **路径限定 commit**: 每个 task 列出确切 `git add` 路径白名单;**禁止** `git add .` / `git add -A`。
3. **每个 subagent 一次 commit**;commit subject 见 task 末尾,body 可加 `Tests:` 列表。
4. **每 commit 前校验** :
   - `cargo build -p <touched_crate>` 干净;
   - `cargo test -p <touched_crate>` 全绿;
   - `cargo clippy -p <touched_crate> --all-targets -- -D warnings` 干净;
   - 触及共享文件 → `bash scripts/check-invariants.sh` 干净。
5. **Wave 边界**:Wave N+1 任一 task 不得在 Wave N 全部 commit 前启动。
6. **Cargo.lock drift**:仅 T0 / T1-D 可能引入(SHA-256 / subtle 等)。同 commit 内提交 `Cargo.lock`。
7. **Spec memory**:[breaking changes OK](../../../.claude/projects/-Users-whereslow-Desktop-work-ailinkmax-gateway/memory/feedback_breaking_changes_ok.md) —— 任何旧字段 / 旧 stream 不留兼容路径。

---

# Wave 0 — 共享脚手架

Serial,单 subagent。声明 Wave 1+ 全部依赖的新类型 / 错误码 / `ReqCtx` 字段 / migration,不实施任何 B1/B2 行为。

## Task T0: 错误码 + ReqCtx 字段 + ManagementTokenEntry/MgmtScope + migration 0007

**Files:**
- Modify: `crates/gw-core/src/error/codes.rs`
- Modify: `crates/gw-core/src/entity.rs`
- Modify: `crates/gw-proxy/src/ctx.rs`
- Create: `crates/gw-store/migrations/0007_management_tokens.sql`
- Modify: `Cargo.lock`(若 sha2/subtle 已有则免)

- [ ] **Step 1: 加新错误码常量**

编辑 `crates/gw-core/src/error/codes.rs`,在文件末尾按字母序合理位置追加:

```rust
pub const GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED: &str =
    "GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED";

pub const GW_AUTH_MGMT_TOKEN_MISSING: &str = "GW_AUTH_MGMT_TOKEN_MISSING";
pub const GW_AUTH_MGMT_TOKEN_INVALID: &str = "GW_AUTH_MGMT_TOKEN_INVALID";
pub const GW_AUTH_MGMT_TOKEN_EXPIRED: &str = "GW_AUTH_MGMT_TOKEN_EXPIRED";
pub const GW_AUTH_MGMT_TOKEN_DISABLED: &str = "GW_AUTH_MGMT_TOKEN_DISABLED";
pub const GW_AUTH_MGMT_TOKEN_REVOKED: &str = "GW_AUTH_MGMT_TOKEN_REVOKED";
pub const GW_AUTH_MGMT_TOKEN_SCOPE_DENIED: &str = "GW_AUTH_MGMT_TOKEN_SCOPE_DENIED";
```

如果该文件用 `static MAP` 或类似集中注册的形式,**追随既有写法**,不要新建第二种风格。

- [ ] **Step 2: 加 `MgmtScope` enum 与 `ManagementTokenEntry`**

编辑 `crates/gw-core/src/entity.rs`,在文件末尾(`IpBlockEntry` 之后)追加:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MgmtScope {
    UserSelf,
    AdminRead,
    AdminFull,
}

impl MgmtScope {
    /// 高 scope 是否覆盖低 scope:AdminFull > AdminRead > UserSelf。
    pub fn covers(self, required: MgmtScope) -> bool {
        use MgmtScope::*;
        match (self, required) {
            (AdminFull, _) => true,
            (AdminRead, AdminRead | UserSelf) => true,
            (UserSelf, UserSelf) => true,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ManagementTokenEntry {
    pub token_id: String,                 // 前缀 mt_
    pub user_id: crate::ids::UserId,
    pub scope: MgmtScope,
    #[serde(with = "crate::entity::bytes_base64")]
    pub hash: Vec<u8>,                    // SHA-256(secret)
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<time::OffsetDateTime>,
    #[serde(default = "default_active")]
    pub status: KeyStatus,                // 复用现有 KeyStatus(active / disabled / ...)
    pub display_name: String,
}

fn default_active() -> KeyStatus {
    KeyStatus::Active
}
```

**前置检查**:`crates/gw-core/src/entity.rs` 内必须已有:
- `KeyStatus` enum(含 `Active` 与 `Disabled` 至少这两 variant)—— 沿用 ApiKeyEntry 同款;
- `bytes_base64` serde module —— `ApiKeyEntry.hash` 已用,直接复用;若不存在,沿用 `ApiKeyEntry` 的 hash 字段对应的 serde 注解形式(可能是 `#[serde(with = "...")]` 不同名,以现存代码为准)。

如果 `KeyStatus` 不存在或不含 `Disabled`,**escalate `NEEDS_CONTEXT`** 而非自行加 variant。

- [ ] **Step 3: 加 `ReqCtx` 新字段**

编辑 `crates/gw-proxy/src/ctx.rs`,在 `pub struct ReqCtx { ... }` 内合适位置(建议放在 `picked_*` 之后、`timing` 之前)追加 3 个字段:

```rust
    /// B1: 非流式上游 body 累积(EOS 后传给 codec::parse_response 抽 usage)。
    /// 流式路径保持 None。
    pub upstream_body_accum: Option<bytes::BytesMut>,

    /// B2: management-path 鉴权通过后回填的 user_id。
    pub mgmt_user_id: Option<gw_core::ids::UserId>,

    /// B2: management-path 鉴权通过后回填的 scope。
    pub mgmt_scope: Option<gw_core::entity::MgmtScope>,
```

并在 `ReqCtx::new` / `Default` 实现中将三者初始化为 `None`(或 BytesMut 字段保持 `None`,只在 response_body_filter 真正进非流式分支时 `Some(BytesMut::new())`)。

- [ ] **Step 4: 写 migration 0007**

创建 `crates/gw-store/migrations/0007_management_tokens.sql`:

```sql
-- 0007_management_tokens.sql — ManagementTokenEntry persistence (B2 / ADR-B2-1).
-- 0005_proxy_nodes 与 0006_auth_configs 已先占编号。
--
-- 与其它 entity 表共享 (id, payload, version, updated_at, deleted_at) 五列。
-- payload JSONB 字段对应 `gw_core::entity::ManagementTokenEntry`。
-- hash 在 payload 内 base64 存储,不入独立列;Rust 用 SHA-256(secret) 比对。

CREATE TABLE IF NOT EXISTS management_tokens (
    id          TEXT        PRIMARY KEY,
    payload     JSONB       NOT NULL,
    version     BIGINT      NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ NULL
);

CREATE INDEX IF NOT EXISTS management_tokens_user_id_idx
    ON management_tokens ((payload->>'user_id'));
```

- [ ] **Step 5: 单测覆盖 `MgmtScope::covers`**

在 `crates/gw-core/src/entity.rs` 末尾追加 `#[cfg(test)] mod mgmt_scope_tests`:

```rust
#[cfg(test)]
mod mgmt_scope_tests {
    use super::MgmtScope::*;

    #[test]
    fn admin_full_covers_all() {
        assert!(AdminFull.covers(AdminFull));
        assert!(AdminFull.covers(AdminRead));
        assert!(AdminFull.covers(UserSelf));
    }

    #[test]
    fn admin_read_covers_admin_read_and_user_self() {
        assert!(AdminRead.covers(AdminRead));
        assert!(AdminRead.covers(UserSelf));
        assert!(!AdminRead.covers(AdminFull));
    }

    #[test]
    fn user_self_covers_only_self() {
        assert!(UserSelf.covers(UserSelf));
        assert!(!UserSelf.covers(AdminRead));
        assert!(!UserSelf.covers(AdminFull));
    }
}
```

- [ ] **Step 6: 跑全套验证**

```bash
cargo build -p gw-core -p gw-proxy
cargo test -p gw-core
cargo clippy -p gw-core -p gw-proxy --all-targets -- -D warnings
bash scripts/check-invariants.sh
```

Expected: 全绿。

- [ ] **Step 7: commit**

```bash
git add \
  crates/gw-core/src/error/codes.rs \
  crates/gw-core/src/entity.rs \
  crates/gw-proxy/src/ctx.rs \
  crates/gw-store/migrations/0007_management_tokens.sql
git diff --cached --stat   # 仅以上 4 文件;若有 Cargo.lock 漂移则一并 add
git commit -m "$(cat <<'EOF'
go-onboarding(T0): wave-0 scaffolding for B1/B2

- gw-core: 8 new error codes (1 cross-protocol non-stream, 7 mgmt token)
- gw-core::entity: MgmtScope + ManagementTokenEntry
- gw-proxy::ctx: upstream_body_accum / mgmt_user_id / mgmt_scope
- gw-store: migration 0007 management_tokens (0005 proxy_nodes / 0006 auth_configs already reserved)

Tests:
- gw-core: mgmt_scope_tests::{admin_full_covers_all, admin_read_covers_admin_read_and_user_self, user_self_covers_only_self}

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 1 — 并行构件

4 个 task 互相独立,可由 4 个 subagent 并行执行。每个 task 自带 TDD 测试,与其它 task 不共享文件。

## Task T1-A: OpenAI `parse_response`

**Files:**
- Modify: `crates/gw-codec/src/openai/codec.rs`
- Test: 同文件内 `#[cfg(test)] mod tests`(沿用既有 module)

- [ ] **Step 1: 写失败测试**

在 `crates/gw-codec/src/openai/codec.rs` 末尾 `#[cfg(test)] mod tests` 内追加:

```rust
    #[test]
    fn parse_response_extracts_usage() {
        let body = br#"{
            "id":"chatcmpl-abc",
            "object":"chat.completion",
            "created":1700000000,
            "model":"gpt-4-turbo",
            "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
            "usage":{
                "prompt_tokens":12,
                "completion_tokens":3,
                "total_tokens":15,
                "prompt_tokens_details":{"cached_tokens":7}
            }
        }"#;
        let codec = OpenAiCodec::default();
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(usage.cached_tokens, Some(7));
        assert_eq!(ir.model.as_ref(), "gpt-4-turbo");
    }

    #[test]
    fn parse_response_without_cached_tokens_field() {
        let body = br#"{"id":"x","object":"chat.completion","created":0,"model":"m",
            "choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}"#;
        let codec = OpenAiCodec::default();
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(usage.cached_tokens, None);
    }
```

- [ ] **Step 2: 跑测试确认 RED**

```bash
cargo test -p gw-codec --lib openai::codec::tests::parse_response_extracts_usage -- --nocapture
```

Expected: `panic: 'v1: openai non-stream response not implemented...'` 来自 `unimplemented!()`(line ~139)。

- [ ] **Step 3: 实现 `parse_response`**

在 `crates/gw-codec/src/openai/codec.rs:136-148` 区间替换 `unimplemented!` 调用为真实实现。先在 `openai/wire.rs` 或同文件加(若已有则跳过):

```rust
#[derive(serde::Deserialize)]
struct OpenAiResponseUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptDetails>,
}

#[derive(serde::Deserialize)]
struct OpenAiPromptDetails {
    #[serde(default)]
    cached_tokens: Option<u32>,
}

#[derive(serde::Deserialize)]
struct OpenAiResponseBody {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<OpenAiResponseUsage>,
}
```

然后 `parse_response`:

```rust
fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
    let parsed: OpenAiResponseBody = serde_json::from_slice(body).map_err(|e| ErrorIR {
        category: gw_core::error::ErrorCategory::ProtocolError,
        status: 502,
        code: std::borrow::Cow::Borrowed(gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
        message: std::borrow::Cow::Owned(format!("openai parse_response: {e}")),
        retryable: false,
        ..Default::default()
    })?;

    let usage = parsed.usage.map(|u| UsageIR {
        input_tokens: u.prompt_tokens,
        output_tokens: u.completion_tokens,
        cached_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
    });

    Ok(ResponseIR {
        id: std::borrow::Cow::Owned(parsed.id),
        model: std::borrow::Cow::Owned(parsed.model),
        messages: Vec::new(),       // v1 不解 choices,后续 cross-protocol response render 才需要
        finish_reason: None,
        usage,
        metadata: Default::default(),
    })
}
```

**前置**:`GW_UPSTREAM_PROTOCOL_DECODE_ERROR` 若不存在,沿用 `GW_UPSTREAM_INVALID_RESPONSE` 或在 T0 一并加(若发现缺失则补 T0)。如二者皆无,**escalate `NEEDS_CONTEXT`**。

- [ ] **Step 4: 跑测试确认 GREEN**

```bash
cargo test -p gw-codec --lib openai::codec::tests::parse_response -- --nocapture
```

Expected: 2 个新测试全绿;现有 openai 测试不回归。

- [ ] **Step 5: clippy + 全 crate test**

```bash
cargo clippy -p gw-codec --all-targets -- -D warnings
cargo test -p gw-codec
```

Expected: 全绿。

- [ ] **Step 6: commit**

```bash
git add crates/gw-codec/src/openai/codec.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T1-A): openai parse_response

ADR-B1-1 first leg: extract usage.prompt_tokens / completion_tokens /
prompt_tokens_details.cached_tokens from ChatCompletion body; map into
ResponseIR.usage. Decode failure → ErrorIR(502, GW_UPSTREAM_PROTOCOL_DECODE_ERROR).
choices array intentionally not parsed — v1 only needs usage; cross-protocol
non-stream render is deferred (B1-4 → 503).

Tests:
- openai::codec::tests::parse_response_extracts_usage
- openai::codec::tests::parse_response_without_cached_tokens_field

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T1-B: Anthropic `parse_response`

**Files:**
- Modify: `crates/gw-codec/src/anthropic/codec.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/gw-codec/src/anthropic/codec.rs` 末尾 `#[cfg(test)] mod tests` 追加:

```rust
    #[test]
    fn parse_response_extracts_usage() {
        let body = br#"{
            "id":"msg_abc",
            "type":"message",
            "role":"assistant",
            "content":[{"type":"text","text":"hi"}],
            "model":"claude-3-5-sonnet",
            "stop_reason":"end_turn",
            "usage":{
                "input_tokens":20,
                "output_tokens":5,
                "cache_read_input_tokens":15,
                "cache_creation_input_tokens":0
            }
        }"#;
        let codec = AnthropicCodec::default();
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.cached_tokens, Some(15));
        assert_eq!(ir.model.as_ref(), "claude-3-5-sonnet");
    }

    #[test]
    fn parse_response_no_cache_fields() {
        let body = br#"{"id":"x","type":"message","role":"assistant","content":[],
            "model":"m","stop_reason":"end_turn",
            "usage":{"input_tokens":3,"output_tokens":1}}"#;
        let codec = AnthropicCodec::default();
        let usage = codec.parse_response(body).unwrap().usage.unwrap();
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cached_tokens, None);
    }
```

- [ ] **Step 2: 跑测试确认 RED**

```bash
cargo test -p gw-codec --lib anthropic::codec::tests::parse_response_extracts_usage -- --nocapture
```

Expected: `unimplemented!` panic。

- [ ] **Step 3: 实现 `parse_response`**

替换 `crates/gw-codec/src/anthropic/codec.rs:135-148` 的 `unimplemented!()`,新增结构:

```rust
#[derive(serde::Deserialize)]
struct AnthropicResponseUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
    // cache_creation_input_tokens 暂不映射到 UsageIR(IR 无对应字段);
    // 待 codec-s4 v2 扩 UsageIR 后再补。
}

#[derive(serde::Deserialize)]
struct AnthropicResponseBody {
    #[serde(default)]
    id: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    usage: Option<AnthropicResponseUsage>,
}

fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
    let parsed: AnthropicResponseBody = serde_json::from_slice(body).map_err(|e| ErrorIR {
        category: gw_core::error::ErrorCategory::ProtocolError,
        status: 502,
        code: std::borrow::Cow::Borrowed(gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
        message: std::borrow::Cow::Owned(format!("anthropic parse_response: {e}")),
        retryable: false,
        ..Default::default()
    })?;

    let usage = parsed.usage.map(|u| UsageIR {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_tokens: u.cache_read_input_tokens,
    });

    Ok(ResponseIR {
        id: std::borrow::Cow::Owned(parsed.id),
        model: std::borrow::Cow::Owned(parsed.model),
        messages: Vec::new(),
        finish_reason: None,
        usage,
        metadata: Default::default(),
    })
}
```

- [ ] **Step 4: 验证 GREEN + clippy + commit**

```bash
cargo test -p gw-codec --lib anthropic::codec::tests::parse_response -- --nocapture
cargo clippy -p gw-codec --all-targets -- -D warnings
cargo test -p gw-codec
git add crates/gw-codec/src/anthropic/codec.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T1-B): anthropic parse_response

ADR-B1-1 second leg: extract usage.input_tokens / output_tokens /
cache_read_input_tokens from Message body. cache_creation_input_tokens
not yet on UsageIR (deferred to codec-s4 v2). Decode failure → 502.

Tests:
- anthropic::codec::tests::parse_response_extracts_usage
- anthropic::codec::tests::parse_response_no_cache_fields

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T1-C: Gemini `parse_response`

**Files:**
- Modify: `crates/gw-codec/src/gemini/codec.rs`

- [ ] **Step 1: 写失败测试**

在 `crates/gw-codec/src/gemini/codec.rs` 末尾 `#[cfg(test)] mod tests` 追加:

```rust
    #[test]
    fn parse_response_extracts_usage_metadata() {
        let body = br#"{
            "candidates":[{"content":{"parts":[{"text":"hi"}]},"finishReason":"STOP"}],
            "modelVersion":"gemini-1.5-pro",
            "usageMetadata":{
                "promptTokenCount":15,
                "candidatesTokenCount":4,
                "totalTokenCount":19,
                "cachedContentTokenCount":8
            }
        }"#;
        let codec = GeminiCodec::default();
        let ir = codec.parse_response(body).expect("parse_response ok");
        let usage = ir.usage.expect("usage present");
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.cached_tokens, Some(8));
        assert_eq!(ir.model.as_ref(), "gemini-1.5-pro");
    }

    #[test]
    fn parse_response_without_cached_field() {
        let body = br#"{"candidates":[],"modelVersion":"m",
            "usageMetadata":{"promptTokenCount":2,"candidatesTokenCount":1,"totalTokenCount":3}}"#;
        let codec = GeminiCodec::default();
        let usage = codec.parse_response(body).unwrap().usage.unwrap();
        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cached_tokens, None);
    }
```

- [ ] **Step 2: 跑测试 RED**

```bash
cargo test -p gw-codec --lib gemini::codec::tests::parse_response_extracts_usage_metadata
```

Expected: `unimplemented!`。

- [ ] **Step 3: 实现 `parse_response`**

替换 `crates/gw-codec/src/gemini/codec.rs:204-215` 的两个 `unimplemented!()`(注意 trait 有两个 `parse_response` 重载?根据 grep 仅一个 `parse_response<'a>` + 后续 unimplemented 一段,二者按现有方法签名各替换):

```rust
#[derive(serde::Deserialize)]
struct GeminiUsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    candidates_token_count: u32,
    #[serde(default, rename = "cachedContentTokenCount")]
    cached_content_token_count: Option<u32>,
}

#[derive(serde::Deserialize)]
struct GeminiResponseBody {
    #[serde(default, rename = "modelVersion")]
    model_version: String,
    #[serde(default, rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsageMetadata>,
}

fn parse_response<'a>(&self, body: &'a [u8]) -> Result<ResponseIR<'a>, ErrorIR<'static>> {
    let parsed: GeminiResponseBody = serde_json::from_slice(body).map_err(|e| ErrorIR {
        category: gw_core::error::ErrorCategory::ProtocolError,
        status: 502,
        code: std::borrow::Cow::Borrowed(gw_core::error::codes::GW_UPSTREAM_PROTOCOL_DECODE_ERROR),
        message: std::borrow::Cow::Owned(format!("gemini parse_response: {e}")),
        retryable: false,
        ..Default::default()
    })?;

    let usage = parsed.usage_metadata.map(|u| UsageIR {
        input_tokens: u.prompt_token_count,
        output_tokens: u.candidates_token_count,
        cached_tokens: u.cached_content_token_count,
    });

    Ok(ResponseIR {
        id: std::borrow::Cow::Borrowed(""),    // Gemini 响应无 id 字段
        model: std::borrow::Cow::Owned(parsed.model_version),
        messages: Vec::new(),
        finish_reason: None,
        usage,
        metadata: Default::default(),
    })
}
```

如果文件内同时存在第二个 `unimplemented!`(line 214 from grep),那是另一个方法 —— 检查方法签名后:
- 若是 `parse_response_with_metadata` 或类似变体,沿用相同序列化路径;
- 若是无法识别的 helper,**escalate `NEEDS_CONTEXT`**(不要盲改)。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-codec --lib gemini::codec::tests::parse_response
cargo clippy -p gw-codec --all-targets -- -D warnings
cargo test -p gw-codec
git add crates/gw-codec/src/gemini/codec.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T1-C): gemini parse_response

ADR-B1-1 third leg: extract usageMetadata.{promptTokenCount,
candidatesTokenCount, cachedContentTokenCount} from GenerateContentResponse.
Gemini responses carry no id field — leave ResponseIR.id empty.

Tests:
- gemini::codec::tests::parse_response_extracts_usage_metadata
- gemini::codec::tests::parse_response_without_cached_field

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T1-D: `ManagementTokenEntry` 持久化层

**Files:**
- Modify: `crates/gw-store/src/loader.rs`
- Modify: `crates/gw-store/src/entity_store.rs`
- Modify: `crates/gw-store/src/stream_consumer.rs`

- [ ] **Step 1: 加 `load_mgmt_token` SQL 常量与函数**

编辑 `crates/gw-store/src/loader.rs`,在文件已有 `load_*` 函数末尾(`load_jwks` 或 `load_ipblock` 附近)追加:

```rust
const MGMT_TOKEN_SELECT_SQL: &str = "
    SELECT payload::text, version
    FROM management_tokens
    WHERE id = $1 AND deleted_at IS NULL
";

pub async fn load_mgmt_token(
    pg: &sqlx::PgPool,
    token_id: &str,
) -> Result<Option<gw_core::Versioned<gw_core::entity::ManagementTokenEntry>>, StoreError> {
    let row: Option<(String, i64)> = sqlx::query_as(MGMT_TOKEN_SELECT_SQL)
        .bind(token_id)
        .fetch_optional(pg)
        .await?;
    let Some((payload, version)) = row else { return Ok(None) };
    let entry: gw_core::entity::ManagementTokenEntry =
        serde_json::from_str(&payload).map_err(StoreError::PayloadDecode)?;
    Ok(Some(gw_core::Versioned {
        version: version as u64,
        value: std::sync::Arc::new(entry),
    }))
}
```

**前置**:`StoreError::PayloadDecode` 必须已有,与 `load_user` / `load_apikey` 使用同款。若不同名,沿用既有错误 variant。

- [ ] **Step 2: 加 `EntityStore.mgmt_token` cache**

编辑 `crates/gw-store/src/entity_store.rs`,在 `EntityStore` struct 字段处加:

```rust
    pub mgmt_token:
        moka::future::Cache<String, Option<gw_core::Versioned<gw_core::entity::ManagementTokenEntry>>>,
```

在 `EntityStore::new` 内构造(沿用与 `apikey` 相同 5 min TTL):

```rust
    mgmt_token: moka::future::Cache::builder()
        .time_to_live(std::time::Duration::from_secs(300))
        .max_capacity(10_000)
        .build(),
```

加 `mgmt_token_or_load`(模仿 `apikey_or_load` 的冷加载链 —— Moka → Redis L1 → PgSQL cold-load):

```rust
pub async fn mgmt_token_or_load(
    &self,
    token_id: &str,
) -> Result<Option<gw_core::Versioned<gw_core::entity::ManagementTokenEntry>>, StoreError> {
    if let Some(v) = self.mgmt_token.get(token_id).await {
        return Ok(v);
    }
    // Redis L1 略(若 apikey_or_load 内有 Redis L1 路径,完全沿用)
    let loaded = crate::loader::load_mgmt_token(&self.pg, token_id).await?;
    self.mgmt_token
        .insert(token_id.to_string(), loaded.clone())
        .await;
    Ok(loaded)
}

pub async fn invalidate_mgmt_token(&self, token_id: &str) {
    self.mgmt_token.invalidate(token_id).await;
}
```

**前置**:如果 `apikey_or_load` 走的 Redis L1 路径远比这复杂,**遵循同款实现**;不要简化。

- [ ] **Step 3: `StreamConsumer` 分发 `management_token` ns**

编辑 `crates/gw-store/src/stream_consumer.rs::apply_entity_event`(或同款函数),在现有 ns dispatch `match ns { ... }` 内追加 arm(同 ns 与 `apikey` 等并列):

```rust
        "management_token" => {
            store.invalidate_mgmt_token(&id).await;
            // 同时 DEL Redis L1(G6 在 gaps plan 实现,本 task 暂沿用现有行为)
        }
```

- [ ] **Step 4: 写单测(stream_consumer dispatch + EntityStore happy path)**

在 `crates/gw-store/src/entity_store.rs` 末尾 `#[cfg(test)] mod entity_store_tests` 追加:

```rust
    #[tokio::test]
    async fn mgmt_token_or_load_caches_after_first_pgsql_hit() {
        // 沿用与 apikey_or_load 同款 testcontainers / mock pgsql 套件;
        // 仅断言:同 token_id 第二次调用走 Moka(不打 SQL counter)。
        // 若现有套件无 SQL counter,断言两次调用结果引用相同 Arc 即可。
    }
```

如果现有 `entity_store_tests` 全部走真实 testcontainers,**沿用同款**,此处补 mgmt_token 用例;不要新建独立 mock 框架。

在 `crates/gw-store/src/stream_consumer.rs` `#[cfg(test)] mod tests`(若有)追加:

```rust
    #[tokio::test]
    async fn apply_entity_event_invalidates_mgmt_token() {
        // 同款套件:构造 EntityStore,塞入 mgmt_token Moka,发出
        // ns=management_token id=<x> 事件,断言 Moka 不再命中 <x>。
    }
```

如果该文件无现成测试模块,**escalate `NEEDS_CONTEXT`** 询问是否补建测试套件(避免在此 task 内新立 testcontainers 依赖)。

- [ ] **Step 5: 验证 + commit**

```bash
cargo build -p gw-store
cargo test -p gw-store
cargo clippy -p gw-store --all-targets -- -D warnings
git add crates/gw-store/src/loader.rs crates/gw-store/src/entity_store.rs crates/gw-store/src/stream_consumer.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T1-D): mgmt_token persistence + EntityStore + stream invalidation

ADR-B2-1 second half (after T0 declared the type):
- loader::load_mgmt_token + SELECT SQL
- EntityStore.mgmt_token cache (5min TTL) + mgmt_token_or_load + invalidate
- stream_consumer dispatch: ns=management_token → invalidate_mgmt_token

Tests:
- entity_store_tests::mgmt_token_or_load_caches_after_first_pgsql_hit
- stream_consumer::tests::apply_entity_event_invalidates_mgmt_token

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 2 — Filter wiring

4 个 task。T2-A、T2-C、T2-D 互相独立可并行;T2-B 必须等 Wave 1 的 T1-A/B/C 全部 commit。

## Task T2-A: 删除非流式 guard

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-proxy/src/filters/request.rs` 末尾或 `crates/gw-proxy/tests/non_stream_e2e.rs`(本 task 内若不存在则**不创建**,留给 T4-A;此处只补 in-process unit test):

```rust
#[cfg(test)]
mod non_stream_unit {
    // 测试目标:同协议非流式请求不再被 GW_REQUEST_NONSTREAM_NOT_SUPPORTED 短路。
    // 由于 request_filter 是大型函数 + Pingora session 难 mock,本 unit test
    // 仅断言"非流式分支已被删除":在 source 中 grep 不到该 short-circuit。
    //
    // 真实端到端验证在 T4-A docker e2e。

    #[test]
    fn non_stream_guard_removed_from_source() {
        let src = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/filters/request.rs")
        ).expect("read source");
        assert!(
            !src.contains("GW_REQUEST_NONSTREAM_NOT_SUPPORTED"),
            "non-stream short-circuit must be removed in B1-2"
        );
    }
}
```

- [ ] **Step 2: 跑测试确认 RED**

```bash
cargo test -p gw-proxy --lib filters::request::non_stream_unit
```

Expected: 测试 fail —— 当前源代码内仍有 `GW_REQUEST_NONSTREAM_NOT_SUPPORTED`。

- [ ] **Step 3: 删除短路 block**

在 `crates/gw-proxy/src/filters/request.rs` 找到(grep 第 536-545 区间)如下 block:

```rust
    if !is_stream {
        // ... ErrorIR { code: GW_REQUEST_NONSTREAM_NOT_SUPPORTED, ... }
        // ... short_circuit(400);
    }
```

**整段删除**。保留:
- `ctx.is_stream = is_stream;`(已在 line 521)
- `ctx.idempotent = !is_stream;`(已在 line 525 —— 非流式天然幂等)
- 整体上后续 filter 流程对 `is_stream` 的所有其它分支保持不变。

- [ ] **Step 4: GREEN**

```bash
cargo test -p gw-proxy --lib filters::request::non_stream_unit
cargo build -p gw-proxy
cargo clippy -p gw-proxy --all-targets -- -D warnings
```

- [ ] **Step 5: commit**

```bash
git add crates/gw-proxy/src/filters/request.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T2-A): remove non-stream short-circuit in request_filter

ADR-B1-2: delete the `if !is_stream { short_circuit(400) }` block. Codec
parse_response (T1-A/B/C) now handles non-stream responses; response_body_filter
(T2-B) will accumulate and parse on EOS. ctx.is_stream / ctx.idempotent
assignments preserved.

Tests:
- filters::request::non_stream_unit::non_stream_guard_removed_from_source

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T2-B: `response_body_filter` 非流式分支

**Depends on**: T1-A / T1-B / T1-C 全部 commit。

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`

- [ ] **Step 1: 注册新 metric**

编辑 `crates/gw-gateway/src/metrics_catalog.rs`,在既有 `describe_*` 块(查 `gw_codec_cross_render_total` 附近)追加:

```rust
    metrics::describe_counter!(
        "gw_codec_parse_response_failure_total",
        "Number of times codec.parse_response returned an error on non-stream upstream body"
    );
```

label set: `{codec, upstream_status}`。

- [ ] **Step 2: 写失败测试(单元层)**

`crates/gw-proxy/src/filters/response.rs` 末尾 `#[cfg(test)] mod tests`(若不存在则创建)追加:

```rust
#[cfg(test)]
mod non_stream_branch_tests {
    use super::*;
    use bytes::BytesMut;
    use gw_codec::{AnyCodec, openai::OpenAiCodec};

    fn make_ctx_non_stream() -> ReqCtx {
        let mut ctx = ReqCtx::new(/* 沿用既有 ReqCtx::new 签名 */);
        ctx.is_stream = false;
        ctx.upstream_codec = Some(AnyCodec::OpenAi(OpenAiCodec::default()));
        ctx.upstream_body_accum = Some(BytesMut::new());
        ctx
    }

    #[test]
    fn non_stream_eos_parses_usage_into_ctx() {
        let mut ctx = make_ctx_non_stream();
        // Simulate two chunks then EOS.
        let chunk1 = bytes::Bytes::from_static(br#"{"id":"x","model":"gpt","#);
        let chunk2 = bytes::Bytes::from_static(
            br#""usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}}"#);
        let mut body = Some(chunk1);
        super::run(/* session mock */, &mut body, false, &mut ctx).unwrap();
        let mut body = Some(chunk2);
        super::run(&mut body, true, &mut ctx).unwrap();
        assert_eq!(ctx.usage.input_tokens, 7);
        assert_eq!(ctx.usage.output_tokens, 2);
    }
}
```

> 注:`run` 当前签名是 `pub fn run(_session: &mut Session, body: &mut Option<Bytes>, end_of_stream: bool, ctx: &mut ReqCtx)`,Session 是 Pingora 类型难以独立 mock。**实际策略**:抽取一个 pure helper `fn handle_non_stream_chunk(body: Option<&Bytes>, end_of_stream: bool, ctx: &mut ReqCtx)`,在 `run` 内调用,然后 unit test 直接调 helper(不带 Session)。
>
> Step 2 测试代码请按 helper 签名重写;Step 3 实现要先抽 helper。

- [ ] **Step 3: 实现 non-stream 分支**

在 `crates/gw-proxy/src/filters/response.rs::run` 中,定位 line 49-51 的:

```rust
    if !ctx.is_stream {
        return Ok(None);
    }
```

替换为:

```rust
    if !ctx.is_stream {
        handle_non_stream_chunk(body.as_ref(), end_of_stream, ctx);
        return Ok(None);
    }
```

在同文件加 `handle_non_stream_chunk`:

```rust
fn handle_non_stream_chunk(
    chunk: Option<&bytes::Bytes>,
    end_of_stream: bool,
    ctx: &mut ReqCtx,
) {
    // 1. 确保 accum 已就绪。
    if ctx.upstream_body_accum.is_none() {
        ctx.upstream_body_accum = Some(bytes::BytesMut::new());
    }

    // 2. 追加当前 chunk。
    if let Some(c) = chunk {
        ctx.upstream_body_accum
            .as_mut()
            .expect("accum just initialised")
            .extend_from_slice(c);
    }

    // 3. 仅在 EOS 解析。
    if !end_of_stream {
        return;
    }

    let accum = ctx.upstream_body_accum.take().unwrap_or_default();
    let body_bytes = accum.freeze();

    let codec = match ctx.upstream_codec.as_ref() {
        Some(c) => c,
        None => {
            // 没有 upstream_codec 不应发生,但兜底 —— 跳过 parse,usage 保留 freeze 估算。
            return;
        }
    };

    let codec_kind = codec.kind().as_str();   // 用于 metric label
    let upstream_status = ctx.upstream_status.unwrap_or(0);

    match codec.parse_response(&body_bytes) {
        Ok(ir) => {
            if let Some(u) = ir.usage {
                ctx.usage.input_tokens = u.input_tokens;
                ctx.usage.output_tokens = u.output_tokens;
                ctx.usage.cached_tokens = u.cached_tokens;
            }
        }
        Err(_err) => {
            metrics::counter!(
                "gw_codec_parse_response_failure_total",
                "codec" => codec_kind.to_string(),
                "upstream_status" => upstream_status.to_string()
            ).increment(1);
            // 保留 freeze 估算(由 logging filter 在 UsageRecord 中标 usage_source=fallback_conservative)。
        }
    }
}
```

**前置**:`ctx.usage` 写入策略 —— 若 `UsageAccumulator` 字段是 `pub`,直接赋值;若有 setter,沿用。`codec.kind().as_str()` 是否存在视 `ProtocolKind` 当前 API 而定;如不存在用 `format!("{:?}", codec.kind())` 兜底。

- [ ] **Step 4: 修 `logging` filter 以反映 fallback usage_source**(若 v1 logging 内 usage_source 写死 `Upstream`)

`crates/gw-proxy/src/filters/logging.rs` 找到构建 `UsageRecord` 处,把 `usage_source` 判定改为:

```rust
let usage_source = if ctx.usage.output_tokens > 0 {
    gw_ledger::UsageSource::Upstream
} else {
    // 非流式 + parse_response 失败,或 0-token 响应 → fallback to freeze estimate
    gw_ledger::UsageSource::Estimated
};
```

`Estimated` 与 `FallbackConservative` 二选一沿用既有 enum variant。

- [ ] **Step 5: GREEN + commit**

```bash
cargo test -p gw-proxy --lib filters::response
cargo clippy -p gw-proxy -p gw-gateway --all-targets -- -D warnings
cargo test -p gw-proxy -p gw-gateway
git add crates/gw-proxy/src/filters/response.rs \
        crates/gw-proxy/src/filters/logging.rs \
        crates/gw-gateway/src/metrics_catalog.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T2-B): response_body_filter non-stream branch

ADR-B1-3: accumulate upstream body into ctx.upstream_body_accum, call
upstream_codec.parse_response on EOS, populate ctx.usage with the extracted
input/output/cached tokens. Decode failure → metric
gw_codec_parse_response_failure_total + usage falls back to freeze estimate
(usage_source = Estimated in logging).

Tests:
- filters::response::non_stream_branch_tests::non_stream_eos_parses_usage_into_ctx

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T2-C: 跨协议非流式 503

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-proxy/src/filters/upstream.rs` 末尾 `#[cfg(test)] mod tests` 追加(若 `pick_peer` 无既有 unit 测试套件,先 grep 同文件确认;如果完全靠 e2e,把以下测试改为 source-grep 风格 —— 类似 T2-A 风格 —— 以避免新建套件):

```rust
    #[test]
    fn cross_protocol_non_stream_short_circuit_present_in_source() {
        let src = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/filters/upstream.rs")
        ).expect("read source");
        assert!(
            src.contains("GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED"),
            "cross-protocol non-stream short-circuit must be added in B1-4"
        );
    }
```

- [ ] **Step 2: 跑测试 RED**

```bash
cargo test -p gw-proxy --lib filters::upstream::tests::cross_protocol_non_stream_short_circuit_present_in_source
```

Expected: fail(源中暂无 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`)。

- [ ] **Step 3: 在 `pick_peer` 末端插入跨协议短路**

`crates/gw-proxy/src/filters/upstream.rs::pick_peer` 选 key + 决定上游 endpoint 之后(`HttpPeer::new` 构造**之前**),插入:

```rust
    // ADR-B1-4: cross-protocol non-stream is not yet supported on the response
    // side. Fail fast with 503 rather than letting the request reach an upstream
    // it cannot decode back to the client codec.
    let upstream_kind = picked_endpoint_kind;  // 已由前面选定,沿用变量名
    let client_kind = ctx.client_codec.as_ref().map(|c| c.kind());
    if matches!(client_kind, Some(ck) if ck != upstream_kind) && !ctx.is_stream {
        return Err(short_circuit(
            503,
            gw_core::error::codes::GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED,
        ));
    }
```

`short_circuit` / `picked_endpoint_kind` 名称沿用文件内既有同款;如本文件用的是 `ctx.upstream_codec` 已经被早就 stash,那就用 `ctx.upstream_codec.as_ref().unwrap().kind()`,**与 `request_filter` 对齐**。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-proxy --lib filters::upstream
cargo clippy -p gw-proxy --all-targets -- -D warnings
git add crates/gw-proxy/src/filters/upstream.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T2-C): cross-protocol non-stream → 503 short-circuit

ADR-B1-4: when client/upstream codec kind differ AND is_stream == false,
short-circuit with 503 GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED in
pick_peer (before HttpPeer::new). Cross-protocol response render is deferred
to codec-s4 v2.

Tests:
- filters::upstream::tests::cross_protocol_non_stream_short_circuit_present_in_source

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T2-D: Mgmt token 黑名单(扩 `RedisJwtBlacklist`)

**Files:**
- Modify: `crates/gw-auth/src/jwt_blacklist.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-auth/src/jwt_blacklist.rs` 末尾 `#[cfg(test)] mod tests`(若无则建):

```rust
#[cfg(test)]
mod mgmt_namespace_tests {
    use super::*;

    #[tokio::test]
    async fn mgmt_blacklist_uses_separate_key_prefix() {
        // Build with fake Redis (mock or testcontainers — 沿用既有 jwt 测试套件)。
        let pool = fake_redis_pool();   // helper from existing tests
        let bl = RedisJwtBlacklist::new(pool.clone());

        // SET mgmt_token:blacklist:t1 → check_mgmt("t1") returns true.
        pool.client().set("mgmt_token:blacklist:t1", "1", None, None, false).await.unwrap();
        assert!(bl.check_mgmt("t1", BlacklistFailMode::FailClosed).await);

        // JWT 同名 jti 不命中:check_jwt("t1") returns false.
        assert!(!bl.check_jwt("t1", BlacklistFailMode::FailClosed).await);
    }
}
```

如果文件内既有 `check` 方法名不同,沿用既有命名(如 `is_revoked`)。

- [ ] **Step 2: 跑测试 RED**

```bash
cargo test -p gw-auth --lib jwt_blacklist::mgmt_namespace_tests
```

Expected: fail —— `check_mgmt` 方法不存在。

- [ ] **Step 3: 在 `RedisJwtBlacklist` 加 `check_mgmt` 方法 + 把 jwt 路径改为 `check_jwt`**

```rust
const JWT_PREFIX: &str = "jwt:blacklist:";
const MGMT_PREFIX: &str = "mgmt_token:blacklist:";

impl RedisJwtBlacklist {
    pub async fn check_jwt(&self, jti: &str, mode: BlacklistFailMode) -> bool {
        self.check_keyed(&format!("{JWT_PREFIX}{jti}"), mode).await
    }

    pub async fn check_mgmt(&self, token_id: &str, mode: BlacklistFailMode) -> bool {
        self.check_keyed(&format!("{MGMT_PREFIX}{token_id}"), mode).await
    }

    async fn check_keyed(&self, key: &str, mode: BlacklistFailMode) -> bool {
        match self.pool.exists::<i64, _>(key).await {
            Ok(n) => n > 0,
            Err(_) => matches!(mode, BlacklistFailMode::FailClosed),
        }
    }
}
```

把所有现有 `check` / `is_revoked` 调用方更新为 `check_jwt`(grep 所有 caller — 应在 `request.rs::request_filter` 内有一处 JWT 验证后调用)。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-auth --lib jwt_blacklist
cargo clippy -p gw-auth -p gw-proxy --all-targets -- -D warnings
cargo test -p gw-auth -p gw-proxy
git add crates/gw-auth/src/jwt_blacklist.rs crates/gw-proxy/src/filters/request.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T2-D): mgmt token blacklist namespace

ADR-B2-4: RedisJwtBlacklist gains check_mgmt(token_id) using key prefix
`mgmt_token:blacklist:`, parallel to existing `jwt:blacklist:`. FailMode
shared with jwt path. Caller in request.rs updated from check → check_jwt.

Tests:
- jwt_blacklist::mgmt_namespace_tests::mgmt_blacklist_uses_separate_key_prefix

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 3 — Mgmt token verify path

3 个 serial task。T3-A 提供 helper,T3-B/C 各自接入。T3-B/C 在 T3-A commit 后可并行。

## Task T3-A: `verify_mgmt_token` helper

**Files:**
- Modify: `crates/gw-auth/src/mgmt.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-auth/src/mgmt.rs` 末尾 `#[cfg(test)] mod tests`(已有则追加):

```rust
#[cfg(test)]
mod verify_tests {
    use super::*;
    use gw_core::entity::{ManagementTokenEntry, MgmtScope, KeyStatus};
    use gw_core::Versioned;
    use std::sync::Arc;
    use time::OffsetDateTime;

    fn build_entry(secret: &str, scope: MgmtScope, expires_in_secs: i64) -> Versioned<ManagementTokenEntry> {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(secret.as_bytes()).to_vec();
        let entry = ManagementTokenEntry {
            token_id: "ak_demo".to_string(),
            user_id: "u_demo".into(),
            scope,
            hash,
            expires_at: Some(OffsetDateTime::now_utc() + time::Duration::seconds(expires_in_secs)),
            status: KeyStatus::Active,
            display_name: "test".into(),
        };
        Versioned { version: 1, value: Arc::new(entry) }
    }

    #[test]
    fn parse_token_splits_id_and_secret() {
        let parsed = split_mgmt_token_payload("mt_abc.s3cret").unwrap();
        assert_eq!(parsed.token_id, "mt_abc");
        assert_eq!(parsed.secret, "s3cret");
    }

    #[test]
    fn parse_rejects_missing_prefix() {
        assert!(split_mgmt_token_payload("xx_abc.s").is_none());
    }

    #[test]
    fn parse_rejects_missing_dot() {
        assert!(split_mgmt_token_payload("mt_abc").is_none());
    }

    #[test]
    fn verify_constant_time_succeeds_on_match() {
        let entry = build_entry("s3cret", MgmtScope::AdminRead, 3600);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Ok));
    }

    #[test]
    fn verify_fails_on_mismatch() {
        let entry = build_entry("s3cret", MgmtScope::AdminRead, 3600);
        let outcome = verify_mgmt_token_secret(&entry, "wrong", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Invalid));
    }

    #[test]
    fn verify_scope_denied() {
        let entry = build_entry("s3cret", MgmtScope::UserSelf, 3600);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::AdminRead);
        assert!(matches!(outcome, MgmtVerifyOutcome::ScopeDenied));
    }

    #[test]
    fn verify_expired() {
        let entry = build_entry("s3cret", MgmtScope::AdminFull, -1);
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Expired));
    }

    #[test]
    fn verify_disabled() {
        let mut entry = build_entry("s3cret", MgmtScope::AdminFull, 3600);
        Arc::get_mut(&mut entry.value).unwrap().status = KeyStatus::Disabled;
        let outcome = verify_mgmt_token_secret(&entry, "s3cret", MgmtScope::UserSelf);
        assert!(matches!(outcome, MgmtVerifyOutcome::Disabled));
    }
}
```

- [ ] **Step 2: 跑测试 RED**

```bash
cargo test -p gw-auth --lib mgmt::verify_tests
```

Expected: fail —— `verify_mgmt_token_secret`、`split_mgmt_token_payload`、`MgmtVerifyOutcome` 都不存在。

- [ ] **Step 3: 实现 helper**

在 `crates/gw-auth/src/mgmt.rs` 加:

```rust
use gw_core::entity::{KeyStatus, ManagementTokenEntry, MgmtScope};
use gw_core::Versioned;
use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq, Eq)]
pub enum MgmtVerifyOutcome {
    Ok,
    InvalidFormat,
    Invalid,           // hash 不匹配 / token_id 找不到 entity
    Expired,
    Disabled,
    ScopeDenied,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MgmtTokenParts<'a> {
    pub token_id: &'a str,
    pub secret: &'a str,
}

/// 拆 `mt_<id>.<secret>`;前缀必须 `mt_`,中间必须有 `.`。
pub fn split_mgmt_token_payload(raw: &str) -> Option<MgmtTokenParts<'_>> {
    if !raw.starts_with("mt_") {
        return None;
    }
    let dot_pos = raw.find('.')?;
    let token_id = &raw[..dot_pos];
    let secret = &raw[dot_pos + 1..];
    if secret.is_empty() {
        return None;
    }
    Some(MgmtTokenParts { token_id, secret })
}

/// 对加载好的 entry + 客户端提供的 secret + 要求 scope 做最终校验。
/// 不查 PgSQL、不查 Redis 黑名单(那两步在 caller / verify_with_store 内);
/// 仅做 SHA-256 比对 + 过期 / 状态 / scope 检查。
pub fn verify_mgmt_token_secret(
    entry: &Versioned<ManagementTokenEntry>,
    secret: &str,
    required_scope: MgmtScope,
) -> MgmtVerifyOutcome {
    use subtle::ConstantTimeEq;

    // 1. SHA-256 比对(constant-time)。
    let secret_hash = Sha256::digest(secret.as_bytes());
    let stored: &[u8] = entry.value.hash.as_slice();
    let eq: subtle::Choice = stored.ct_eq(secret_hash.as_slice());
    if !bool::from(eq) {
        return MgmtVerifyOutcome::Invalid;
    }

    // 2. status。
    if !matches!(entry.value.status, KeyStatus::Active) {
        return MgmtVerifyOutcome::Disabled;
    }

    // 3. expires_at。
    if let Some(exp) = entry.value.expires_at {
        if time::OffsetDateTime::now_utc() >= exp {
            return MgmtVerifyOutcome::Expired;
        }
    }

    // 4. scope 矩阵。
    if !entry.value.scope.covers(required_scope) {
        return MgmtVerifyOutcome::ScopeDenied;
    }

    MgmtVerifyOutcome::Ok
}
```

**Cargo.toml 依赖检查**:`gw-auth/Cargo.toml` 必须含 `sha2 = "0.10"`、`subtle = "2"`、`time` 已有;若 sha2/subtle 缺,补 + `Cargo.lock` 同 commit。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-auth --lib mgmt
cargo clippy -p gw-auth --all-targets -- -D warnings
git add crates/gw-auth/src/mgmt.rs crates/gw-auth/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
go-onboarding(T3-A): verify_mgmt_token helper

ADR-B2-2: pure-helper layer (no I/O) consumed by request_filter and health.
- split_mgmt_token_payload("mt_<id>.<secret>") → MgmtTokenParts
- verify_mgmt_token_secret(entry, secret, required_scope) → MgmtVerifyOutcome
  with constant-time SHA-256 compare, status/expires_at/scope checks.

Deps added (if absent): sha2, subtle.

Tests: 8 cases covering happy / format-error / hash-mismatch / scope /
expiry / disabled.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T3-B: `filters::request` mgmt-path verify 分支

**Depends on**: T3-A、T1-D、T2-D。

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`

- [ ] **Step 1: 写失败测试**

由于 `request_filter` 难单元化,沿用 T2-A 的"源码包含断言"风格:

```rust
#[cfg(test)]
mod mgmt_path_branch_tests {
    #[test]
    fn mgmt_branch_calls_verify_helper() {
        let src = std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/filters/request.rs")
        ).unwrap();
        assert!(src.contains("verify_mgmt_token_secret"), "T3-B helper integration missing");
        assert!(src.contains("MgmtVerifyOutcome::Ok"), "T3-B outcome handling missing");
        assert!(!src.contains("return Err(short_circuit(501"),
                "T3-B must remove the placeholder 501 short-circuit");
    }
}
```

- [ ] **Step 2: 跑测试 RED**

```bash
cargo test -p gw-proxy --lib filters::request::mgmt_path_branch_tests
```

Expected: fail。

- [ ] **Step 3: 修 `is_management_path` 分支(`crates/gw-proxy/src/filters/request.rs:61` 附近)**

在 `if gw_auth::mgmt::is_management_path(&req_path) { ... }` block 内:

```rust
if gw_auth::mgmt::is_management_path(&req_path) {
    let auth_hdr = session.req_header()
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(parsed_auth) = gw_auth::mgmt::parse_mgmt_authorization(auth_hdr) else {
        return Err(short_circuit_with_envelope(
            session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_MISSING,
        ).await);
    };
    let Some(parts) = gw_auth::mgmt::split_mgmt_token_payload(&parsed_auth.token) else {
        return Err(short_circuit_with_envelope(
            session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_INVALID,
        ).await);
    };

    let entry = match store.mgmt_token_or_load(parts.token_id).await {
        Ok(Some(e)) => e,
        Ok(None) | Err(_) => {
            return Err(short_circuit_with_envelope(
                session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_INVALID,
            ).await);
        }
    };

    // 黑名单(Redis 异步;沿用 jwt 同款 helper)。
    let blocklist_mode = /* 从 cfg / AuthConfig 读;沿用 jwt 同款 */;
    if mgmt_blacklist.check_mgmt(parts.token_id, blocklist_mode).await {
        return Err(short_circuit_with_envelope(
            session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_REVOKED,
        ).await);
    }

    // 决定所需 scope(管理路径粗判);后续 /api/admin/* 细分由 Go 处理,
    // 此处仅做最粗粒度:任何 /admin/* 至少要 admin_read,/admin/{user,...} 由 Go 决定。
    let required = MgmtScope::AdminRead;

    match gw_auth::mgmt::verify_mgmt_token_secret(&entry, parts.secret, required) {
        MgmtVerifyOutcome::Ok => {
            ctx.mgmt_user_id = Some(entry.value.user_id.clone());
            ctx.mgmt_scope = Some(entry.value.scope);
        }
        MgmtVerifyOutcome::InvalidFormat | MgmtVerifyOutcome::Invalid => {
            return Err(short_circuit_with_envelope(
                session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_INVALID,
            ).await);
        }
        MgmtVerifyOutcome::Expired => {
            return Err(short_circuit_with_envelope(
                session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_EXPIRED,
            ).await);
        }
        MgmtVerifyOutcome::Disabled => {
            return Err(short_circuit_with_envelope(
                session, &ctx, 401, codes::GW_AUTH_MGMT_TOKEN_DISABLED,
            ).await);
        }
        MgmtVerifyOutcome::ScopeDenied => {
            return Err(short_circuit_with_envelope(
                session, &ctx, 403, codes::GW_AUTH_MGMT_TOKEN_SCOPE_DENIED,
            ).await);
        }
    }
}
```

**前置**:
- `mgmt_blacklist` 必须从 `LlmGateway` 拿到(`self.mgmt_blacklist: Arc<RedisJwtBlacklist>` 字段);如果 `LlmGateway` 无此字段,**沿用 jwt 同款 Arc**(可能 jwt_blacklist 与 mgmt_blacklist 是同一 Arc 实例,共用 RedisJwtBlacklist)。如果 LlmGateway 不持有 jwt blacklist Arc 也,**escalate**。
- `short_circuit_with_envelope` 是 `request_filter` 内既有 helper,**沿用**;不要新造。如名字不同(可能是 `short_circuit_err` 或类似),**对齐既有调用**。
- 删除现有(若有)的 `return Err(short_circuit(501, ...))` 的 v1 占位短路。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-proxy --lib filters::request::mgmt_path_branch_tests
cargo build -p gw-proxy
cargo clippy -p gw-proxy --all-targets -- -D warnings
git add crates/gw-proxy/src/filters/request.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T3-B): mgmt-path verify branch in request_filter

ADR-B2-2: replace 501 placeholder in is_management_path branch with full
verify chain:
  parse_mgmt_authorization → split → mgmt_token_or_load → check_mgmt(blacklist)
    → verify_mgmt_token_secret → ctx.mgmt_user_id/mgmt_scope.

Returns 401 GW_AUTH_MGMT_TOKEN_{MISSING,INVALID,EXPIRED,DISABLED,REVOKED} or
403 GW_AUTH_MGMT_TOKEN_SCOPE_DENIED.

Tests:
- filters::request::mgmt_path_branch_tests::mgmt_branch_calls_verify_helper

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T3-C: `/healthz/*` 解锁

**Depends on**: T3-A、T1-D、T2-D。

**Files:**
- Modify: `crates/gw-gateway/src/health.rs`
- Modify: `crates/gw-gateway/src/lib.rs`

- [ ] **Step 1: 写失败测试**

`crates/gw-gateway/tests/healthz_mgmt_token.rs`(新建):

```rust
//! /healthz/* mgmt token gate (T3-C).
//!
//! 仅在 ignore 标签下跑(需要 testcontainers Postgres + Redis,沿用 e2e_*.rs
//! 同款基础设施)。

#![cfg(feature = "testcontainers-tests")]   // 沿用既有 cfg gate;若 crate 无,改为 #[ignore]

#[tokio::test]
#[ignore = "需要 testcontainers"]
async fn healthz_circuits_returns_401_without_mgmt_token() {
    let bundle = spawn_gateway_with_seeds(&[]).await;
    let resp = bundle
        .http_get("/healthz/circuits", None)
        .await;
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
#[ignore = "需要 testcontainers"]
async fn healthz_circuits_returns_200_with_admin_read_token() {
    let bundle = spawn_gateway_with_seeds(&[Seed::mgmt_token("mt_t1", "secret", MgmtScope::AdminRead)]).await;
    let resp = bundle
        .http_get("/healthz/circuits", Some(("Authorization", "Mgmt mt_t1.secret")))
        .await;
    assert_eq!(resp.status(), 200);
    let body = resp.text().await;
    assert!(body.starts_with("{") || body.starts_with("["), "expected JSON");
}

#[tokio::test]
#[ignore = "需要 testcontainers"]
async fn healthz_circuits_returns_403_with_user_self_scope() {
    let bundle = spawn_gateway_with_seeds(&[Seed::mgmt_token("mt_u", "s", MgmtScope::UserSelf)]).await;
    let resp = bundle
        .http_get("/healthz/circuits", Some(("Authorization", "Mgmt mt_u.s")))
        .await;
    assert_eq!(resp.status(), 403);
}
```

`spawn_gateway_with_seeds` / `Seed::mgmt_token` 是新 helper:沿用 e2e 既有 testcontainers fixture(类似 `tests/e2e_full_flow.rs` 的开头),**仅扩展不重写**。如果完全没有这种 fixture,把 3 个测试改为 `#[ignore]` + 留 stub 调用,实际验证全交给 T4-B docker e2e。

- [ ] **Step 2: 修 `/healthz/*` 路由**

`crates/gw-gateway/src/health.rs:97-110` 当前是:

```rust
        path if path.starts_with("/healthz/circuits")
            || path.starts_with("/healthz/key/")
            || path == "/healthz/resilience" =>
        {
            // ...
            let _ = write_response(&mut stream, 401, "Unauthorized", b"mgmt token required").await;
        }
```

替换为:

```rust
        path if path.starts_with("/healthz/circuits")
            || path.starts_with("/healthz/key/")
            || path == "/healthz/resilience" =>
        {
            match verify_healthz_request(&req_headers, &store, &mgmt_blacklist).await {
                Ok(_scope) => {
                    let body = build_healthz_body(path, &state).await;
                    let _ = write_response(&mut stream, 200, "OK", body.as_bytes()).await;
                }
                Err(HealthzAuthError::Missing) => {
                    let _ = write_response(&mut stream, 401, "Unauthorized",
                                           b"GW_AUTH_MGMT_TOKEN_MISSING").await;
                }
                Err(HealthzAuthError::Invalid) => {
                    let _ = write_response(&mut stream, 401, "Unauthorized",
                                           b"GW_AUTH_MGMT_TOKEN_INVALID").await;
                }
                Err(HealthzAuthError::Expired) => {
                    let _ = write_response(&mut stream, 401, "Unauthorized",
                                           b"GW_AUTH_MGMT_TOKEN_EXPIRED").await;
                }
                Err(HealthzAuthError::Disabled) => {
                    let _ = write_response(&mut stream, 401, "Unauthorized",
                                           b"GW_AUTH_MGMT_TOKEN_DISABLED").await;
                }
                Err(HealthzAuthError::Revoked) => {
                    let _ = write_response(&mut stream, 401, "Unauthorized",
                                           b"GW_AUTH_MGMT_TOKEN_REVOKED").await;
                }
                Err(HealthzAuthError::ScopeDenied) => {
                    let _ = write_response(&mut stream, 403, "Forbidden",
                                           b"GW_AUTH_MGMT_TOKEN_SCOPE_DENIED").await;
                }
            }
        }
```

加 helper `verify_healthz_request`:

```rust
enum HealthzAuthError {
    Missing,
    Invalid,
    Expired,
    Disabled,
    Revoked,
    ScopeDenied,
}

async fn verify_healthz_request(
    headers: &HashMap<String, String>,
    store: &Arc<gw_store::EntityStore>,
    blacklist: &Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
) -> Result<gw_core::entity::MgmtScope, HealthzAuthError> {
    let raw = headers
        .get("authorization")
        .ok_or(HealthzAuthError::Missing)?;
    let parsed = gw_auth::mgmt::parse_mgmt_authorization(raw)
        .ok_or(HealthzAuthError::Missing)?;
    let parts = gw_auth::mgmt::split_mgmt_token_payload(&parsed.token)
        .ok_or(HealthzAuthError::Invalid)?;

    let entry = store
        .mgmt_token_or_load(parts.token_id)
        .await
        .map_err(|_| HealthzAuthError::Invalid)?
        .ok_or(HealthzAuthError::Invalid)?;

    let mode = gw_auth::jwt_blacklist::BlacklistFailMode::FailClosed;
    if blacklist.check_mgmt(parts.token_id, mode).await {
        return Err(HealthzAuthError::Revoked);
    }

    use gw_auth::mgmt::MgmtVerifyOutcome;
    match gw_auth::mgmt::verify_mgmt_token_secret(
        &entry, parts.secret, gw_core::entity::MgmtScope::AdminRead,
    ) {
        MgmtVerifyOutcome::Ok => Ok(entry.value.scope),
        MgmtVerifyOutcome::Expired => Err(HealthzAuthError::Expired),
        MgmtVerifyOutcome::Disabled => Err(HealthzAuthError::Disabled),
        MgmtVerifyOutcome::ScopeDenied => Err(HealthzAuthError::ScopeDenied),
        _ => Err(HealthzAuthError::Invalid),
    }
}
```

加 `build_healthz_body`(根据 path 返回对应 JSON):

```rust
async fn build_healthz_body(path: &str, state: &HealthState) -> String {
    if path.starts_with("/healthz/circuits") {
        state.key_health.snapshot_json()        // 沿用 gw-health 既有方法;若无则 escalate
    } else if path.starts_with("/healthz/key/") {
        let id = path.trim_start_matches("/healthz/key/");
        state.key_health.key_snapshot_json(id)
    } else if path == "/healthz/resilience" {
        state.resilience.snapshot_json()
    } else {
        "{}".to_string()
    }
}
```

如果 `gw-health::KeyHealthStore` 没有 `snapshot_json`,**escalate**(不要在本 task 自造,留 follow-up 加最小占位返回 `{"status":"ok"}`)。

- [ ] **Step 3: `gw-gateway::lib` 串接 `store` + `mgmt_blacklist`**

`crates/gw-gateway/src/lib.rs` 内 `Wired` struct 加字段:

```rust
pub store: Arc<gw_store::EntityStore>,
pub mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,   // 复用 jwt blacklist 实例
```

(如果 `store` / `jwt_blacklist` 已在 Wired 内,跳过加字段,只补传递。)

`run_health_server` 签名扩展:

```rust
pub fn run_health_server(
    addr: SocketAddr,
    state: HealthState,
    store: Arc<gw_store::EntityStore>,
    mgmt_blacklist: Arc<gw_auth::jwt_blacklist::RedisJwtBlacklist>,
) -> JoinHandle<()> {
    // ...
}
```

`bootstrap` 入口处传入这两个 Arc。

- [ ] **Step 4: GREEN + commit**

```bash
cargo test -p gw-gateway --lib
cargo build -p gw-gateway
cargo clippy -p gw-gateway --all-targets -- -D warnings
git add crates/gw-gateway/src/health.rs crates/gw-gateway/src/lib.rs crates/gw-gateway/tests/healthz_mgmt_token.rs
git commit -m "$(cat <<'EOF'
go-onboarding(T3-C): /healthz/* mgmt token gate

ADR-B2-3 + B2-5: /healthz/{circuits,key/<id>,resilience} now verify
Authorization: Mgmt mt_<id>.<secret> via shared mgmt helpers.
Outcomes:
  Ok(scope >= admin_read) → 200 JSON snapshot
  Missing/Invalid/Expired/Disabled/Revoked → 401 + GW_AUTH_MGMT_TOKEN_*
  ScopeDenied → 403 GW_AUTH_MGMT_TOKEN_SCOPE_DENIED
/livez and /readyz remain unauthenticated.

Tests (ignored, testcontainers):
- healthz_mgmt_token::{returns_401_without_token, returns_200_with_admin_read,
                       returns_403_with_user_self_scope}

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

# Wave 4 — Tests + 文档同步

3 个 parallel task。

## Task T4-A: docker e2e 非流式场景

**Files:**
- Create: `tests/e2e/client/scenarios/test_non_stream.py`
- Modify: `tests/e2e/client/scenarios/conftest.py`(若需 helper)

- [ ] **Step 1: 写场景**

```python
# tests/e2e/client/scenarios/test_non_stream.py
"""B1 docker e2e: 3 SDK 同协议非流式请求(stream=False)成功落 usage:records。"""
import pytest
from openai import OpenAI
from anthropic import Anthropic
import google.genai as genai


@pytest.mark.parametrize("vendor", ["openai", "anthropic", "gemini"])
def test_non_stream_same_protocol_returns_usage(vendor, gateway_url, seeded_keys, usage_records_consumer):
    if vendor == "openai":
        client = OpenAI(base_url=f"{gateway_url}/v1", api_key=seeded_keys["openai"])
        resp = client.chat.completions.create(
            model="gpt-4-turbo",
            messages=[{"role": "user", "content": "hi"}],
            stream=False,
        )
        assert resp.choices[0].message.content
        assert resp.usage.completion_tokens > 0
    elif vendor == "anthropic":
        client = Anthropic(base_url=gateway_url, api_key=seeded_keys["anthropic"])
        resp = client.messages.create(
            model="claude-3-5-sonnet",
            max_tokens=10,
            messages=[{"role": "user", "content": "hi"}],
            stream=False,
        )
        assert resp.usage.output_tokens > 0
    elif vendor == "gemini":
        client = genai.Client(http_options={"base_url": gateway_url}, api_key=seeded_keys["gemini"])
        resp = client.models.generate_content(model="gemini-1.5-pro", contents="hi")
        assert resp.usage_metadata.candidates_token_count > 0

    # 等 ledger flush + 读 usage:records
    record = usage_records_consumer.next_record(timeout=5.0)
    assert record["is_stream"] is False
    assert record["usage"]["output_tokens"] > 0
    assert record["usage_source"] == "upstream"


def test_cross_protocol_non_stream_returns_503(gateway_url, seeded_keys):
    """B1-4: OpenAI client + Anthropic-only vendor key, stream=False → 503."""
    client = OpenAI(base_url=f"{gateway_url}/v1", api_key=seeded_keys["openai_to_anthropic"])
    with pytest.raises(Exception) as exc:
        client.chat.completions.create(
            model="claude-3-5-sonnet",
            messages=[{"role": "user", "content": "hi"}],
            stream=False,
        )
    assert "503" in str(exc.value) or "GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED" in str(exc.value)
```

`seeded_keys` / `usage_records_consumer` / `gateway_url` 是 e2e fixture,沿用 `tests/e2e/client/scenarios/conftest.py` 既有;若 `openai_to_anthropic` seed 不存在,补 fixture(同 `test_cross_protocol.py` 风格)。

- [ ] **Step 2: 在 `fake_upstream` 给 3 个 vendor 都加 non-stream happy path**

`crates/e2e-fake-upstream/scenarios/` 下沿用既有 JSON/TOML scenario,加 `non_stream_openai_happy.toml` / `non_stream_anthropic_happy.toml` / `non_stream_gemini_happy.toml`:

```toml
# non_stream_openai_happy.toml
[response]
status = 200
content_type = "application/json"
body = """{
  "id":"chatcmpl-fake",
  "object":"chat.completion",
  "created":1700000000,
  "model":"gpt-4-turbo",
  "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],
  "usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7,"prompt_tokens_details":{"cached_tokens":1}}
}"""
```

同样写 Anthropic / Gemini 对应非流式响应体。

- [ ] **Step 3: 跑 e2e**

```bash
make e2e-up
make e2e -- -k test_non_stream
make e2e-down
```

Expected: 4 个测试全通过(3 同协议 + 1 跨协议 503)。

- [ ] **Step 4: commit**

```bash
git add tests/e2e/client/scenarios/test_non_stream.py \
        tests/e2e/client/scenarios/conftest.py \
        crates/e2e-fake-upstream/scenarios/
git commit -m "$(cat <<'EOF'
go-onboarding(T4-A): e2e docker scenarios for B1 non-stream

3 SDK same-protocol non-stream happy paths + cross-protocol → 503 test.
Validates: codec.parse_response extracts usage, ctx.usage flows into
usage:records with usage_source=upstream, is_stream=false.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T4-B: docker e2e mgmt token 场景

**Files:**
- Create: `tests/e2e/client/scenarios/test_mgmt_token.py`
- Modify: `crates/e2e-fake-web/src/lib.rs` 或 conftest(增加 `seed_mgmt_token` helper + `mgmt_blacklist_set` helper)

- [ ] **Step 1: 写场景**

```python
# tests/e2e/client/scenarios/test_mgmt_token.py
"""B2 docker e2e: /healthz/{circuits,resilience} mgmt token gate."""
import requests


def test_healthz_without_token_returns_401(gateway_health_url):
    resp = requests.get(f"{gateway_health_url}/healthz/circuits")
    assert resp.status_code == 401
    assert b"GW_AUTH_MGMT_TOKEN_MISSING" in resp.content


def test_healthz_with_admin_read_token_returns_200(gateway_health_url, seed_mgmt_token):
    seed_mgmt_token("mt_r1", "secret_r1", scope="admin_read")
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_r1.secret_r1"},
    )
    assert resp.status_code == 200
    # body 是 JSON,粗略校验。
    assert resp.headers["content-type"].startswith("application/json") or resp.text.startswith(("{", "["))


def test_healthz_with_user_self_scope_returns_403(gateway_health_url, seed_mgmt_token):
    seed_mgmt_token("mt_u1", "secret_u1", scope="user_self")
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_u1.secret_u1"},
    )
    assert resp.status_code == 403
    assert b"GW_AUTH_MGMT_TOKEN_SCOPE_DENIED" in resp.content


def test_healthz_with_expired_token_returns_401(gateway_health_url, seed_mgmt_token):
    seed_mgmt_token("mt_e1", "secret_e1", scope="admin_read", expires_in_secs=-1)
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_e1.secret_e1"},
    )
    assert resp.status_code == 401
    assert b"GW_AUTH_MGMT_TOKEN_EXPIRED" in resp.content


def test_healthz_after_blacklist_returns_401(gateway_health_url, seed_mgmt_token, mgmt_blacklist_set):
    seed_mgmt_token("mt_b1", "secret_b1", scope="admin_full")
    # 先打通验证一次。
    assert requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_b1.secret_b1"},
    ).status_code == 200
    mgmt_blacklist_set("mt_b1", ttl_secs=60)
    # 等 EntityStore Moka TTL 内 — blacklist 检查独立于 entity cache,应即时生效。
    resp = requests.get(
        f"{gateway_health_url}/healthz/circuits",
        headers={"Authorization": "Mgmt mt_b1.secret_b1"},
    )
    assert resp.status_code == 401
    assert b"GW_AUTH_MGMT_TOKEN_REVOKED" in resp.content


def test_livez_still_no_auth(gateway_health_url):
    resp = requests.get(f"{gateway_health_url}/livez")
    assert resp.status_code == 200
```

- [ ] **Step 2: conftest 加 helper**

`tests/e2e/client/scenarios/conftest.py` 加:

```python
@pytest.fixture
def seed_mgmt_token(fake_web_admin_client):
    def _seed(token_id, secret, scope, expires_in_secs=3600):
        import hashlib, base64, datetime, json
        secret_hash = base64.b64encode(hashlib.sha256(secret.encode()).digest()).decode()
        payload = {
            "token_id": token_id,
            "user_id": "u_test",
            "scope": scope,
            "hash": secret_hash,
            "expires_at": (datetime.datetime.utcnow() + datetime.timedelta(seconds=expires_in_secs)).isoformat() + "Z",
            "status": "active",
            "display_name": f"test {token_id}",
        }
        fake_web_admin_client.put_entity("management_token", token_id, payload)
    return _seed


@pytest.fixture
def mgmt_blacklist_set(redis_client):
    def _set(token_id, ttl_secs=60):
        redis_client.set(f"mgmt_token:blacklist:{token_id}", "1", ex=ttl_secs)
    return _set
```

`fake_web_admin_client.put_entity` 是 e2e fake_web 的既有 API(写 PgSQL + XADD `events:entity`);沿用即可。

- [ ] **Step 3: 跑 e2e**

```bash
make e2e-up
make e2e -- -k test_mgmt_token
make e2e-down
```

Expected: 6 个测试全通过。

- [ ] **Step 4: commit**

```bash
git add tests/e2e/client/scenarios/test_mgmt_token.py \
        tests/e2e/client/scenarios/conftest.py
git commit -m "$(cat <<'EOF'
go-onboarding(T4-B): e2e docker scenarios for B2 mgmt token /healthz

Validates the full B2 chain: parse_mgmt_authorization → split →
mgmt_token_or_load (cold) → check_mgmt(blacklist) → verify_mgmt_token_secret →
scope check. Covers 6 paths: missing, ok+200, scope_denied+403, expired,
revoked, /livez unauth.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task T4-C: 文档同步(architecture.md / workflow.md / parts/01-03)

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/workflow.md`
- Modify: `docs/go-contracts/parts/01-go-as-producer.md`
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`
- Modify: `docs/go-contracts/parts/03-shared-schema.md`

- [ ] **Step 1: `docs/architecture.md` §8 删第 1 条**

定位 §8 "Deliberate behaviours" 表格的第 1 行:

```
| Non-stream completions rejected with `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400 | ... |
```

整行删除。后续行序号不变(表格无序号列)。

- [ ] **Step 2: `docs/architecture.md` §6.1 更新 `/healthz/*` 状态**

定位:

```
The `/healthz/*` admin routes return `401` until management-token validation lands (T6-A).
```

改为:

```
The `/healthz/*` admin routes verify `Authorization: Mgmt mt_<id>.<secret>` and require `scope >= admin_read`. See [`docs/superpowers/specs/2026-05-22-go-onboarding-blockers-design.md`](superpowers/specs/2026-05-22-go-onboarding-blockers-design.md) §2 (B2).
```

- [ ] **Step 3: `docs/workflow.md` §2.2 step 1 更新 mgmt 分支**

定位 "1. **Management-path branch**" 段,改为:

```
1. **Management-path branch** — `is_management_path(&path)` (e.g. `/admin/*`) sets `ctx.mgmt = true`, then verifies `Authorization: Mgmt mt_<id>.<secret>` via `gw_auth::mgmt::{parse_mgmt_authorization, split_mgmt_token_payload, verify_mgmt_token_secret}`. Outcomes: 401 `GW_AUTH_MGMT_TOKEN_{MISSING,INVALID,EXPIRED,DISABLED,REVOKED}` or 403 `GW_AUTH_MGMT_TOKEN_SCOPE_DENIED`. On success: `ctx.mgmt_user_id` / `ctx.mgmt_scope` populated.
```

- [ ] **Step 4: `docs/workflow.md` §2.2 step 9 移除 non-stream guard 描述**

定位 "9. **Non-stream guard**" 整段,删除。下一步 step 10 顺移为 step 9。

- [ ] **Step 5: `docs/go-contracts/parts/01-go-as-producer.md` §1.2.3 加 management_token 行**

按 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §1.1 的"最终文本",在 `auth_config` 行下方追加:

```
| `management_token` | `management_tokens`(0007 migration) | `EntityStore.mgmt_token` | `ManagementTokenEntry` | 5 min TTL;ID 前缀 `mt_` |
```

- [ ] **Step 6: `docs/go-contracts/parts/02-go-as-consumer.md` 加 §2.10 mgmt_token:blacklist**

按 changelog §1.4 的"最终文本",在 §2.9(未决问题)**之前** 插入完整 §2.10 内容(从 `## 2.10 mgmt_token:blacklist...` 到 `EX <remaining_seconds>` 整段)。原 §2.9 顺延为 §2.11。

- [ ] **Step 7: `docs/go-contracts/parts/03-shared-schema.md` 加 §3.2.13 与 §3.6 行**

按 changelog §1.2 / §1.3 的"最终文本":
- 在 §3.2.12 后追加完整 §3.2.13(从 `### 3.2.13 management_tokens.payload...` 到示例 JSON 结尾)。
- §3.6 ID 前缀表 `auth_config` 行下方追加 `| management_token | mt_ | Go 生成 |`。

- [ ] **Step 8: 校验**

```bash
# 链接检查(若仓库有 lychee / markdown-link-check):
# bash scripts/check-doc-links.sh   # 若存在
# 否则人工 grep 一下新加链接:
grep -n "2026-05-22-go-onboarding" docs/architecture.md docs/workflow.md
```

- [ ] **Step 9: commit**

```bash
git add docs/architecture.md docs/workflow.md \
        docs/go-contracts/parts/01-go-as-producer.md \
        docs/go-contracts/parts/02-go-as-consumer.md \
        docs/go-contracts/parts/03-shared-schema.md
git commit -m "$(cat <<'EOF'
go-onboarding(T4-C): docs sync — architecture / workflow / contract parts

- architecture.md §8 deliberate behaviours: drop the non-stream rejection
  row (B1 lifted it).
- architecture.md §6.1: /healthz/* now verifies Mgmt token (B2).
- workflow.md §2.2 step 1: mgmt-path verify chain documented.
- workflow.md §2.2 step 9: non-stream guard removed.
- parts/01 §1.2.3: management_token entity ns added.
- parts/02 §2.10: mgmt_token:blacklist:* contract added.
- parts/03 §3.2.13 + §3.6: ManagementTokenEntry payload + ID prefix.

Aligned with docs/go-contracts/contract-change/2026-05-22-go-onboarding.md §1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review checklist(实施时跑一遍)

跑过整个 plan 后,工程师应当验证:

1. **Spec coverage**: 把 spec 内每个 ADR-B*-* 都能指到具体 task 吗?
   - ADR-B1-1 → T1-A/B/C ✓
   - ADR-B1-2 → T2-A ✓
   - ADR-B1-3 → T2-B ✓
   - ADR-B1-4 → T2-C ✓
   - ADR-B2-1 → T0 + T1-D ✓
   - ADR-B2-2 → T3-A + T3-B ✓
   - ADR-B2-3 → T3-C ✓
   - ADR-B2-4 → T2-D ✓
   - ADR-B2-5 → T3-B(scope 矩阵粗判)+ Go 侧 admin scope 细分(不在网关范围)✓
2. **No placeholders**: 各 task 步骤是否都给出确切代码块 / 命令?(应当 yes;若发现 "TBD" / "实现 X" 而无代码,fix)
3. **Type consistency**: `ManagementTokenEntry.hash` 是 `Vec<u8>`、`MgmtScope` 全文一致、`MgmtVerifyOutcome` variant 全文一致、`check_mgmt`/`check_jwt` 命名一致。
4. **Migration 编号**: 0007 与 contract-change §1.5、blockers spec §2.3 一致(0005=proxy_nodes 已 merged,0006=auth_configs 留给 gaps plan)。

---

## 执行选择

**Plan complete and saved to `docs/superpowers/plans/2026-05-22-go-onboarding-blockers.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — 每个 task 一个 fresh subagent,task 之间两阶段 review,fast iteration。

**2. Inline Execution** — 当前 session 内按 task 顺序跑,batched checkpoint 给用户 review。

**Which approach?**

> 注:本 plan 与同日 [`2026-05-22-go-onboarding-gaps.md`](2026-05-22-go-onboarding-gaps.md)(待写,见任务 #6)互相独立;阻塞先行不影响 gaps plan 启动。
