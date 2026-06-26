# Sensitive Filter Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Rust gateway consume the Go-owned global sensitive-filter policy, apply reversible request/response substitutions in the hot path, and mask detailed archive surfaces before `usage:detailed` persistence.

**Architecture:** Add shared policy types in `gw-core`, a compiled regex engine in `gw-runtime`, and a `sensitive_filter_policy` cache/cold-loader in `gw-store`. Thread request-local transit state through `ReqCtx`, apply substitutions around upstream request rendering and response body filtering, and mask all detailed archive surfaces before `DetailedArchive::try_send`. Replace the old raw-archive invariant with a policy-engine-only masking invariant.

**Tech Stack:** Rust 1.88, workspace crates `gw-core`, `gw-runtime`, `gw-store`, `gw-proxy`, `gw-ledger`, Rust `regex`, Moka entity caches, Pingora filters.

## Global Constraints

- Policy scope is global only and keyed as `entity:sensitive_filter_policy:global`.
- Regex engine is Rust `regex`; invalid snapshots are rejected.
- Empty/missing policy is a no-op.
- Invalid newly loaded policy keeps the last good compiled policy where available.
- Reversible maps are request-local memory only and never persisted.
- Archive masking must happen before `usage:detailed` XADD.
- The previous `no_detailed_redaction` invariant must be replaced, not bypassed.
- Use TDD: write the failing test, run it and verify the expected failure, then implement the minimal code.

## AI 实施防错指南（每步开工前必读）

### MUST DO

1. **先 Read，再写**：动 `crates/gw-store/src/loader.rs`、`crates/gw-store/src/entity_store.rs`、`crates/gw-store/src/stream_consumer.rs`、`crates/gw-proxy/src/gateway.rs`、`crates/gw-proxy/src/ctx.rs`、`crates/gw-proxy/src/filters/upstream.rs`、`crates/gw-proxy/src/filters/response.rs` 之前，先 Read 整个文件，对齐既有 `load_entity` 模式、`Versioned<T>` 结构与 `ReqCtx` 现存字段。**不要**凭计划截图猜剩余 boilerplate。
2. **Surrogate 必须不可被用户原文构造出来**：见 Task 2 的精确格式约定。永远不要用 `<replacement>-sf-<seq>` —— 它可以被上游或用户载荷里的明文复现，导致逆向时把无辜字符串还原成机密。
3. **Compiled policy 缓存按 version 维度**：Compile 一次正则非常贵（数百 µs ~ ms 级），每个请求 compile 会把热路径毁掉。在 `EntityStore` 里加一个 `Mutex<Option<(u64, Arc<CompiledSensitiveFilterPolicy>)>>`，version 相同则复用 Arc，version 跳变才重新 compile。`ReqCtx` 只持有 `Option<Arc<CompiledSensitiveFilterPolicy>>`，**不持有原始 Policy**。
4. **archive 掩膜在 `try_send` 前**：4 个 `CapturedSurface`（downstream request、upstream request、upstream response、downstream response）的 headers + body **都要**经过 `mask_archive_*`；改完用 `rg "self\.detailed_archive\.try_send" crates/gw-proxy/src` 自检一次，每个出现点上方应有 mask 调用，否则就是漏。
5. **流式 reverse 必须维护 tail buffer**：上游响应是 chunk 流，一个 surrogate 可能跨 chunk 边界。`ReverseStream` 必须 buffer 最长可能的 surrogate 长度 − 1 个字节直到下个 chunk 或 EOS。看见 EOS 才 flush tail。
6. **`scripts/check-invariants.sh` 的 `no_detailed_redaction` 块要替换不要新增**：当前在 `scripts/check-invariants.sh:572-583`。替换后用 `bash scripts/check-invariants.sh` 跑一次确认所有 lint 仍 PASS。
7. **每个 cargo test 步骤都加 `--locked`**：lock file 漂移会让排错从 5 分钟变 5 小时。

### MUST NOT DO

1. **不要**写"源码字符串 grep" 风格的测试（如 `assert!(src.contains("..."))`）—— 这种测试只验证文本存在，不验证行为，重构会被它绑死。本计划 Task 7 提供了真正的行为测试，照抄。
2. **不要**在 `replace_transit_request` 里用全局原子计数器生成 surrogate seq —— 必须是 per-call 的局部计数器（或 `TransitMap` 内部计数）。跨请求共享会泄漏请求边界，且让 surrogate 不可复现（不利于测试）。
3. **不要**在热路径里调用 `EntityStore::sensitive_filter_policy_or_load` 超过一次：每请求只在 request_filter 阶段调一次，结果存进 `ReqCtx`；其余阶段从 `ReqCtx` 取。
4. **不要**给 `CapturedSurface::summary` 整个再过一遍 mask —— summary 是结构化字段（model name、status code、用量数字），不是用户文本。只对原始 headers/body 应用 mask。
5. **不要**修改 `gw-ledger` 的 detailed_archive XADD 协议（字段数、字段顺序、压缩 magic）。本次只在 `try_send` 前修改入参字节，不动持久化布局。
6. **不要**在 `EntityStore::sensitive_filter_policy_or_load` cold load 失败时返回错误中断请求 —— 缺失策略 = 无规则 = 直通，按 spec 是 no-op。
7. **不要**在 PG cold loader 里把 `sensitive_filter_groups`/`sensitive_filter_rules` JOIN 之后用 `ORDER BY priority` —— 必须按 `(priority ASC, id ASC)` 双键，且 `jsonb_agg` 显式带 `ORDER BY` 子句，否则跨节点观察到的策略顺序会抖。
8. **不要**对响应每个 chunk 都分配 `ReverseStream` —— 整个响应一个 `ReverseStream`，初始化时机是首次看到 transit map 存在的第一个 chunk。

### 常见 false-pass 陷阱

- **缓存 hit 让测试看起来对**：跑 `cargo test -p gw-store --locked -- --test-threads=1` 排除测试间脏数据。
- **`#[serde(rename_all = "snake_case")]` 漏写**：Go 发出的字段是 snake_case，Rust 默认是 PascalCase，漏写就静默忽略所有字段并落到 default。Task 1 的 reject_unknown_target 测试就是为了把这种 silent default 钉死。
- **rust regex 不等于 RE2 不等于 Go regex**：先 lookahead `(?=...)`、后向引用 `\1` 在 Rust `regex` crate 里都不支持。校验时必须用同一引擎；Go 侧 `regexp.Compile` 已经是 RE2 的 Go port，Rust 侧 compile 报错回 keep-last-good，不要 panic。

---

## File Structure

- Modify `crates/gw-core/src/entity.rs`: add sensitive-filter policy wire types.
- Modify `crates/gw-runtime/src/lib.rs`: export new engine.
- Create `crates/gw-runtime/src/sensitive_filter.rs`: compile and apply rules.
- Modify `crates/gw-store/src/loader.rs`: load global policy from Redis/PG.
- Modify `crates/gw-store/src/entity_store.rs`: policy cache/accessors.
- Modify `crates/gw-store/src/stream_consumer.rs`: invalidate `sensitive_filter_policy`.
- Modify `crates/gw-proxy/src/ctx.rs`: request-local transit map/reverse stream buffer.
- Modify `crates/gw-proxy/src/filters/upstream.rs`: apply request substitution after outbound body render.
- Modify `crates/gw-proxy/src/filters/response.rs` or `gateway.rs`: reverse response substitution.
- Modify `crates/gw-proxy/src/gateway.rs`: mask request/response archive surfaces before `try_send`.
- Modify `crates/gw-ledger/src/detailed_archive.rs`: update module docs from raw wire-fidelity wording to filtered archive wording and keep serialization tests passing.
- Modify `scripts/check-invariants.sh`: replace raw no-redaction rule with policy-engine invariant.
- Modify `docs/architecture.md`, `docs/spec/06-billing-ledger.md`, and workflow docs mentioning raw detailed archive.
- Add/extend tests in `gw-core`, `gw-runtime`, `gw-store`, `gw-proxy`, and `gw-ledger`.

## Task 1: Core Wire Types

**Files:**
- Modify: `crates/gw-core/src/entity.rs`

**Interfaces:**
- Produces: `SensitiveFilterPolicy`, `SensitiveFilterGroup`, `SensitiveFilterRule`, `SensitiveFilterMode`, `SensitiveFilterTarget`.
- Consumes later: `gw-runtime` engine and `gw-store` loader.

- [ ] **Step 1: Write failing serde tests**

Add tests near other `entity.rs` tests:

```rust
#[test]
fn sensitive_filter_policy_deserializes_go_shape() {
    let raw = r#"{
      "id":"global",
      "version":12,
      "groups":[{
        "id":"g1",
        "name":"Default",
        "enabled":true,
        "priority":1000,
        "rules":[{
          "id":"r1",
          "name":"AK",
          "enabled":true,
          "priority":1,
          "mode":"archive_mask",
          "pattern":"ak-[A-Za-z0-9_-]+",
          "replacement":"<AK>",
          "targets":["archive","request_body"]
        }]
      }]
    }"#;
    let policy: SensitiveFilterPolicy = serde_json::from_str(raw).unwrap();
    assert_eq!(policy.id, "global");
    assert_eq!(policy.groups[0].rules[0].mode, SensitiveFilterMode::ArchiveMask);
    assert_eq!(policy.groups[0].rules[0].targets[0], SensitiveFilterTarget::Archive);
}

#[test]
fn sensitive_filter_policy_rejects_unknown_target() {
    let raw = r#"{
      "id":"global",
      "version":1,
      "groups":[{
        "id":"g1","name":"Default","enabled":true,"priority":1,
        "rules":[{
          "id":"r1","name":"Bad","enabled":true,"priority":1,
          "mode":"archive_mask","pattern":"x","replacement":"<X>","targets":["bad"]
        }]
      }]
    }"#;
    assert!(serde_json::from_str::<SensitiveFilterPolicy>(raw).is_err());
}
```

- [ ] **Step 2: Run test and verify it fails**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-core sensitive_filter_policy --locked
```

Expected: FAIL because types do not exist.

- [ ] **Step 3: Implement types**

Add to `crates/gw-core/src/entity.rs`:

```rust
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SensitiveFilterPolicy {
    pub id: String,
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub groups: Vec<SensitiveFilterGroup>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SensitiveFilterGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub rules: Vec<SensitiveFilterRule>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct SensitiveFilterRule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    pub mode: SensitiveFilterMode,
    pub pattern: String,
    pub replacement: String,
    #[serde(default)]
    pub targets: Vec<SensitiveFilterTarget>,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveFilterMode {
    TransitReversible,
    ArchiveMask,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveFilterTarget {
    RequestHeader,
    RequestBody,
    ResponseHeader,
    ResponseBody,
    Archive,
}
```

- [ ] **Step 4: Run test and verify it passes**

```bash
cargo +1.88.0 test -p gw-core sensitive_filter_policy --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-core/src/entity.rs
git commit -m "feat(gw-core): add sensitive filter policy entity"
```

## Task 2: Runtime Regex Engine

**Files:**
- Create: `crates/gw-runtime/src/sensitive_filter.rs`
- Modify: `crates/gw-runtime/src/lib.rs`

**Interfaces:**
- Consumes: `gw_core::entity::SensitiveFilterPolicy`.
- Produces: `CompiledSensitiveFilterPolicy`, `TransitMap`, streaming reverse replacement.

- [ ] **Step 1: Write failing engine tests**

Create `crates/gw-runtime/src/sensitive_filter.rs` with tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::*;

    fn policy() -> SensitiveFilterPolicy {
        SensitiveFilterPolicy {
            id: "global".to_string(),
            version: 1,
            groups: vec![SensitiveFilterGroup {
                id: "g1".to_string(),
                name: "Default".to_string(),
                enabled: true,
                priority: 1,
                rules: vec![
                    SensitiveFilterRule {
                        id: "mask".to_string(),
                        name: "Mask".to_string(),
                        enabled: true,
                        priority: 1,
                        mode: SensitiveFilterMode::ArchiveMask,
                        pattern: "ak-[A-Za-z0-9_-]+".to_string(),
                        replacement: "<AK>".to_string(),
                        targets: vec![SensitiveFilterTarget::Archive],
                    },
                    SensitiveFilterRule {
                        id: "transit".to_string(),
                        name: "Transit".to_string(),
                        enabled: true,
                        priority: 2,
                        mode: SensitiveFilterMode::TransitReversible,
                        pattern: "sk-[A-Za-z0-9_-]+".to_string(),
                        replacement: "sk-safe".to_string(),
                        targets: vec![SensitiveFilterTarget::RequestBody, SensitiveFilterTarget::ResponseBody],
                    },
                ],
            }],
        }
    }

    #[test]
    fn archive_mask_replaces_enabled_archive_rules() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let out = compiled.mask_archive_bytes(SensitiveFilterTarget::Archive, b"ak-secret keep");
        assert_eq!(out.as_ref(), b"<AK> keep");
    }

    #[test]
    fn transit_replacement_is_unique_and_reversible() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) = compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one sk-two");
        assert_ne!(out.as_ref(), b"sk-one sk-two");
        assert_eq!(map.len(), 2);
        let restored = compiled.reverse_transit_response(&map, out.as_ref(), true);
        assert_eq!(restored.as_ref(), b"sk-one sk-two");
    }

    #[test]
    fn streaming_reverse_handles_split_surrogate() {
        let compiled = CompiledSensitiveFilterPolicy::compile(&policy()).unwrap();
        let (out, map) = compiled.replace_transit_request(SensitiveFilterTarget::RequestBody, b"sk-one");
        let split = out.len() - 2;
        let mut stream = ReverseStream::new(&map);
        let a = stream.push(&out[..split], false);
        let b = stream.push(&out[split..], true);
        let mut joined = Vec::new();
        joined.extend_from_slice(&a);
        joined.extend_from_slice(&b);
        assert_eq!(joined, b"sk-one");
    }
}
```

Expected before implementation: FAIL because public types do not exist.

- [ ] **Step 2: Implement engine**

Implement:

```rust
pub struct CompiledSensitiveFilterPolicy { rules: Vec<CompiledRule> }
struct CompiledRule { rule: gw_core::entity::SensitiveFilterRule, re: regex::Regex }

pub struct TransitMap {
    /// Each entry is (surrogate_bytes, original_bytes). Ordered by surrogate length DESC
    /// so the streaming reverse picks the longest match first.
    entries: Vec<(bytes::Bytes, bytes::Bytes)>,
}

pub struct ReverseStream {
    entries: Vec<(bytes::Bytes, bytes::Bytes)>,
    /// Buffered tail that may contain a prefix of a surrogate spanning a chunk boundary.
    tail: Vec<u8>,
    /// Length of the longest surrogate in `entries`. The streaming reverse holds back
    /// up to (max_surrogate_len - 1) trailing bytes until EOS or until enough new
    /// bytes arrive to disambiguate.
    max_surrogate_len: usize,
}
```

**Surrogate format (collision-safe — DO NOT change without updating tests):**

```text
__sf_<rule_id_hex>_<call_local_seq>__
```

where:
- `<rule_id_hex>` is the rule ID hashed with `xxhash::xxh64` then hex-formatted to 16 chars. Using the hash (not the raw id) avoids leaking id chars that could appear in user text and keeps surrogate length bounded.
- `<call_local_seq>` is a 0-indexed counter scoped to a single `replace_transit_request` call.
- The `__sf_` prefix and `__` suffix make the surrogate cheap to scan for and statistically very unlikely to appear in user text (collision probability is bounded by the 64-bit hash + sequence).

Worked example: rule id `"r1"` → `xxh64("r1")` → some 16-hex value like `7e3a8d51020b41cc`. First surrogate of a request is `__sf_7e3a8d51020b41cc_0__`.

**Surrogate uniqueness contract:**
- Surrogates are unique within one `TransitMap` (i.e., one request).
- When the same `Rule` matches N times in `request_body`, the engine emits `__sf_<hash>_0__`, `__sf_<hash>_1__`, ..., `__sf_<hash>_N-1__`, and the map records each pair separately so the reverse stream restores the exact original substring for each occurrence (originals may differ — `sk-one` vs `sk-two`).
- Use the regex crate's `regex::Regex::replace_all` with a closure that pushes into the map and emits the next surrogate. Do **not** use a global atomic — use a local `u64` counter passed to the closure.

Use Rust `regex::Regex` (NOT `regex_lite`; the validators must match what compile-time rejection does on the gw-core side). On compile error, fail the whole `compile()` — the caller decides whether to keep the last-good policy.

**Streaming reverse algorithm:** for each chunk:
1. Prepend `self.tail` to the new chunk, clear `self.tail`.
2. Scan from the start, trying to match a surrogate at each position (longest-first by sorted entries).
3. When a surrogate matches, emit the original and advance past it.
4. When the cursor is at position `p` and `chunk.len() - p < max_surrogate_len` and `!eos`, move `chunk[p..]` into `self.tail` and stop.
5. On `eos`, never buffer — flush everything (any leftover is byte-passthrough).

- [ ] **Step 3: Export module**

Modify `crates/gw-runtime/src/lib.rs`:

```rust
pub mod sensitive_filter;
pub use sensitive_filter::{CompiledSensitiveFilterPolicy, ReverseStream, TransitMap};
```

- [ ] **Step 4: Run runtime tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-runtime sensitive_filter --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-runtime/src/lib.rs crates/gw-runtime/src/sensitive_filter.rs
git commit -m "feat(gw-runtime): add sensitive filter engine"
```

## Task 3: Entity Store Loader and Invalidation

**Files:**
- Modify: `crates/gw-store/src/loader.rs`
- Modify: `crates/gw-store/src/entity_store.rs`
- Modify: `crates/gw-store/src/stream_consumer.rs`

**Interfaces:**
- Consumes: `SensitiveFilterPolicy`.
- Produces: `EntityStore::sensitive_filter_policy_or_load`.

- [ ] **Step 1: Write failing invalidation test**

Add to `stream_consumer.rs` tests:

```rust
#[tokio::test]
async fn apply_entity_event_invalidates_sensitive_filter_policy() {
    let store = Arc::new(EntityStore::new());
    store.sensitive_filter_policy.insert(
        "global".to_string(),
        Arc::new(Versioned {
            version: 1,
            value: Arc::new(SensitiveFilterPolicy { id: "global".to_string(), version: 1, groups: vec![] }),
        }),
    ).await;
    apply_entity_event(&store, "sensitive_filter_policy", "global").await;
    assert!(store.sensitive_filter_policy.get("global").await.is_none());
}
```

Expected: FAIL because cache field does not exist.

- [ ] **Step 2: Add cache and loader accessor**

Modify `EntityStore`:

```rust
pub sensitive_filter_policy: Cache<String, Arc<Versioned<SensitiveFilterPolicy>>>,
```

Build with `max_capacity(16)` and `time_to_live(Duration::from_secs(300))`.

Add:

```rust
pub async fn sensitive_filter_policy_or_load(&self) -> Option<Arc<Versioned<SensitiveFilterPolicy>>> {
    let id = "global".to_string();
    if let Some(hit) = self.sensitive_filter_policy.get(&id).await { return Some(hit); }
    let loader = self.loader.as_ref()?.clone();
    let deadline = self.cold_load_timeout;
    self.sensitive_filter_policy.try_get_with_by_ref(&id, async move {
        with_cold_load_timeout("sensitive_filter_policy", deadline, async move {
            load_sensitive_filter_policy(&loader, "global").await
        }).await.map(Arc::new).ok_or_else(|| anyhow::anyhow!(COLD_LOAD_MISS_SENTINEL))
    }).await.ok()
}

pub async fn invalidate_sensitive_filter_policy(&self, id: &str) {
    self.sensitive_filter_policy.invalidate(&id.to_string()).await;
}
```

Add `load_sensitive_filter_policy` to `loader.rs`. Use `load_entity` for Redis L1 and a direct PostgreSQL fallback that assembles `SensitiveFilterPolicy` in SQL:

```rust
pub async fn load_sensitive_filter_policy(
    ctx: &LoaderCtx,
    id: &str,
) -> anyhow::Result<Versioned<SensitiveFilterPolicy>> {
    load_entity(ctx, "sensitive_filter_policy", id, SENSITIVE_FILTER_POLICY_SQL, |value| {
        serde_json::from_value(value.clone()).map_err(Into::into)
    }).await
}

/// PG cold-load: assemble the full policy as JSONB in one query.
/// The query is parameterised on $1 = id (always "global" today).
/// Returns `(payload jsonb, version bigint)`.
/// Missing row => empty policy + version 0, NOT an error (no-op semantics).
pub const SENSITIVE_FILTER_POLICY_SQL: &str = r#"
WITH meta AS (
    SELECT id, version
    FROM sensitive_filter_policy_meta
    WHERE id = $1
),
rules AS (
    SELECT
        r.group_id,
        jsonb_build_object(
            'id',          r.id,
            'name',        r.name,
            'enabled',     r.enabled,
            'priority',    r.priority,
            'mode',        r.mode,
            'pattern',     r.pattern,
            'replacement', r.replacement,
            'targets',     to_jsonb(r.targets)
        ) AS rule_json,
        r.priority AS rule_priority,
        r.id       AS rule_id
    FROM sensitive_filter_rules r
    WHERE r.deleted_at IS NULL
),
groups AS (
    SELECT
        g.id,
        jsonb_build_object(
            'id',          g.id,
            'name',        g.name,
            'description', COALESCE(g.description, ''),
            'enabled',     g.enabled,
            'priority',    g.priority,
            'rules',       COALESCE(
                (
                    SELECT jsonb_agg(rule_json ORDER BY rule_priority ASC, rule_id ASC)
                    FROM rules WHERE rules.group_id = g.id
                ),
                '[]'::jsonb
            )
        ) AS group_json,
        g.priority AS group_priority
    FROM sensitive_filter_groups g
    WHERE g.deleted_at IS NULL
)
SELECT
    jsonb_build_object(
        'id',      COALESCE((SELECT id FROM meta), $1),
        'version', COALESCE((SELECT version FROM meta), 0),
        'groups',  COALESCE(
            (SELECT jsonb_agg(group_json ORDER BY group_priority ASC, group_json->>'id' ASC) FROM groups),
            '[]'::jsonb
        )
    ) AS payload,
    COALESCE((SELECT version FROM meta), 0)::bigint AS version
"#;
```

> Note: the query always returns exactly one row even when the policy is absent (empty groups, version 0). This avoids the `not found in pg` error path in `load_entity` and gives Rust the no-op-by-default semantics required by the spec.

- [ ] **Step 3: Add stream consumer arm**

In `apply_entity_event`:

```rust
"sensitive_filter_policy" => {
    store.invalidate_sensitive_filter_policy(id).await;
}
```

- [ ] **Step 4: Run store tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-store sensitive_filter_policy --locked
cargo +1.88.0 test -p gw-store apply_entity_event_invalidates_sensitive_filter_policy --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-store/src/loader.rs crates/gw-store/src/entity_store.rs crates/gw-store/src/stream_consumer.rs
git commit -m "feat(gw-store): load sensitive filter policy"
```

## Task 4: Request-Local Transit State

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`
- Modify: `crates/gw-proxy/src/filters/upstream.rs`

**Interfaces:**
- Consumes: `CompiledSensitiveFilterPolicy`.
- Produces: request-local `TransitMap` on `ReqCtx`.

- [ ] **Step 1: Write failing upstream filter unit test**

Define a shared test helper module so Tasks 4, 5, 6 don't each invent their own. Create `crates/gw-proxy/src/filters/sensitive_filter_test_support.rs` (gated under `#[cfg(test)]`):

```rust
#![cfg(test)]
use gw_core::entity::*;
use gw_runtime::CompiledSensitiveFilterPolicy;

pub(crate) fn test_sensitive_filter_policy() -> SensitiveFilterPolicy {
    SensitiveFilterPolicy {
        id: "global".into(),
        version: 1,
        groups: vec![SensitiveFilterGroup {
            id: "g1".into(),
            name: "Default".into(),
            enabled: true,
            priority: 1,
            rules: vec![
                SensitiveFilterRule {
                    id: "transit".into(),
                    name: "Transit".into(),
                    enabled: true,
                    priority: 1,
                    mode: SensitiveFilterMode::TransitReversible,
                    pattern: "sk-[A-Za-z0-9_-]+".into(),
                    replacement: "sk-safe".into(),
                    targets: vec![
                        SensitiveFilterTarget::RequestBody,
                        SensitiveFilterTarget::ResponseBody,
                    ],
                },
                SensitiveFilterRule {
                    id: "mask".into(),
                    name: "Mask".into(),
                    enabled: true,
                    priority: 2,
                    mode: SensitiveFilterMode::ArchiveMask,
                    pattern: "ak-[A-Za-z0-9_-]+".into(),
                    replacement: "<AK>".into(),
                    targets: vec![SensitiveFilterTarget::Archive],
                },
            ],
        }],
    }
}

pub(crate) fn test_compiled_archive_policy() -> CompiledSensitiveFilterPolicy {
    CompiledSensitiveFilterPolicy::compile(&test_sensitive_filter_policy()).unwrap()
}

pub(crate) fn test_transit_body_and_map() -> (bytes::Bytes, gw_runtime::TransitMap) {
    let compiled = test_compiled_archive_policy();
    let (out, map) = compiled.replace_transit_request(
        SensitiveFilterTarget::RequestBody,
        b"sk-secret",
    );
    (bytes::Bytes::from(out.to_vec()), map)
}
```

Add a focused unit test in `filters/upstream.rs`:

```rust
#[cfg(test)]
mod sensitive_filter_tests {
    use super::*;
    use crate::filters::sensitive_filter_test_support::*;

    #[test]
    fn apply_sensitive_filter_to_upstream_body_stores_transit_map() {
        let compiled = test_compiled_archive_policy();
        let mut ctx = ReqCtx::default();
        let out = apply_sensitive_filter_to_upstream_body(
            &mut ctx,
            &compiled,
            bytes::Bytes::from_static(b"sk-secret"),
        );
        assert_ne!(out.as_ref(), b"sk-secret");
        let map = ctx
            .sensitive_filter_transit
            .as_ref()
            .expect("transit map must be installed");
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn apply_sensitive_filter_no_match_keeps_body_and_transit_empty() {
        let compiled = test_compiled_archive_policy();
        let mut ctx = ReqCtx::default();
        let out = apply_sensitive_filter_to_upstream_body(
            &mut ctx,
            &compiled,
            bytes::Bytes::from_static(b"hello"),
        );
        assert_eq!(out.as_ref(), b"hello");
        assert!(ctx.sensitive_filter_transit.is_none());
    }
}
```

Expected: FAIL because fields/helper do not exist.

- [ ] **Step 2: Add ReqCtx fields**

Add to `ReqCtx`:

```rust
pub sensitive_filter_transit: Option<gw_runtime::TransitMap>,
pub sensitive_filter_reverse_stream: Option<gw_runtime::ReverseStream>,
```

Initialize both to `None`. Reset both in `reset_response_attempt_state`.

- [ ] **Step 3: Apply substitution after outbound render**

In `filters/upstream::rewrite_request`, after `apply_outbound_adapter` and before `archive_upstream_request`:

```rust
let outbound = if let Some(compiled) = ctx.sensitive_filter_policy.as_ref() {
    apply_sensitive_filter_to_upstream_body(ctx, compiled, outbound)
} else {
    outbound
};
```

If the compiled policy is held outside ctx, pass it from `gateway.rs` into the rewrite function. The request must not call Go or Redis on the hot path for every chunk; use the cached/compiled policy.

- [ ] **Step 4: Run proxy upstream tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-proxy apply_sensitive_filter_to_upstream_body_stores_transit_map --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs crates/gw-proxy/src/filters/upstream.rs
git commit -m "feat(gw-proxy): substitute sensitive request data"
```

## Task 5: Response Reverse Replacement

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/gateway.rs` only if the outer filter must call the helper.

**Interfaces:**
- Consumes: `ReqCtx.sensitive_filter_transit`.
- Produces: downstream response body with surrogates restored to originals.

- [ ] **Step 1: Write failing response tests**

Add tests in `filters/response.rs` for a pure helper:

```rust
#[test]
fn reverse_sensitive_filter_response_restores_non_stream_body() {
    let (body, map) = test_transit_body_and_map();
    let mut ctx = ReqCtx::default();
    ctx.sensitive_filter_transit = Some(map);
    let restored = reverse_sensitive_filter_response(&mut ctx, body, true);
    assert_eq!(restored.as_ref(), b"sk-secret");
}

#[test]
fn reverse_sensitive_filter_response_handles_split_stream_body() {
    let (body, map) = test_transit_body_and_map();
    let split = body.len() - 2;
    let mut ctx = ReqCtx::default();
    ctx.sensitive_filter_transit = Some(map);
    let a = reverse_sensitive_filter_response(&mut ctx, body.slice(..split), false);
    let b = reverse_sensitive_filter_response(&mut ctx, body.slice(split..), true);
    let mut joined = Vec::new();
    joined.extend_from_slice(&a);
    joined.extend_from_slice(&b);
    assert_eq!(joined, b"sk-secret");
}
```

Expected: FAIL because helper does not exist.

- [ ] **Step 2: Implement response reverse helper**

Implement:

```rust
pub(crate) fn reverse_sensitive_filter_response(ctx: &mut ReqCtx, body: Bytes, eos: bool) -> Bytes
```

If `ctx.sensitive_filter_transit` is `None`, return `body`. Otherwise initialize `ctx.sensitive_filter_reverse_stream` from the map and use it for every chunk.

- [ ] **Step 3: Call helper before downstream forwarding**

Call `reverse_sensitive_filter_response` after codec rendering has produced the client-bound bytes and before the body is returned to Pingora. Ensure archive accumulators receive the same downstream-bound bytes that the client sees, then archive masking will run before persistence in Task 6.

- [ ] **Step 4: Run response tests**

```bash
cargo +1.88.0 test -p gw-proxy reverse_sensitive_filter_response --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/filters/response.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(gw-proxy): reverse sensitive response surrogates"
```

## Task 6: Archive Masking Before Detailed Archive XADD

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-ledger/src/detailed_archive.rs` module docs and existing serialization tests.

**Interfaces:**
- Consumes: compiled policy.
- Produces: all `CapturedSurface` payloads masked before `try_send`.

- [ ] **Step 1: Write failing archive masking test**

Add a pure helper test in `gateway.rs`:

```rust
#[test]
fn mask_archive_request_surface_masks_headers_and_body() {
    let compiled = test_compiled_archive_policy();
    let headers = vec![("authorization".to_string(), "Bearer ak-secret".to_string())];
    let body = std::sync::Arc::new(bytes::Bytes::from_static(b"{\"key\":\"ak-secret\"}"));
    let (headers, body) = mask_archive_request_surface(&compiled, headers, body);
    assert_eq!(headers[0].1, "Bearer <AK>");
    assert_eq!(body.as_ref().as_ref(), b"{\"key\":\"<AK>\"}");
}
```

Expected: FAIL because helper does not exist.

- [ ] **Step 2: Implement archive masking helpers**

Add helpers:

```rust
fn mask_archive_headers(compiled: &CompiledSensitiveFilterPolicy, headers: Vec<(String, String)>) -> Vec<(String, String)>
fn mask_archive_body(compiled: &CompiledSensitiveFilterPolicy, body: Arc<Bytes>) -> Arc<Bytes>
fn mask_archive_request_surface(...)
fn mask_archive_response_surface(...)
```

Use `SensitiveFilterTarget::Archive` for both headers and body in v1, matching the global archive target semantics.

- [ ] **Step 3: Apply helpers to every `CapturedSurface`**

Before each `self.detailed_archive.try_send(CapturedSurface { ... })` for:

- downstream request around `gateway.rs` request filter;
- upstream request around `upstream_request_filter`;
- upstream response around response emit;
- downstream response around response emit.

Do not mask summary payload unless it includes free-form body/header data. Keep existing loopback header removal.

- [ ] **Step 4: Run archive tests**

```bash
cargo +1.88.0 test -p gw-proxy mask_archive_request_surface --locked
cargo +1.88.0 test -p gw-ledger detailed_archive --locked
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-ledger/src/detailed_archive.rs
git commit -m "feat(gw-proxy): mask detailed archive surfaces"
```

## Task 7: Policy Compilation and Hot Path Access

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`
- Modify: `crates/gw-proxy/src/ctx.rs` if compiled policy is stored per request.

**Interfaces:**
- Consumes: `EntityStore::sensitive_filter_policy_or_load`.
- Produces: no per-chunk Redis/PG calls; compiled policy is reused safely.

- [ ] **Step 1: Write failing behavior test (not a source-string grep)**

> **AI 提醒：** 之前的草案用 `assert!(src.contains(...))` 验证集成 —— 这是反模式。文本存在不等于行为正确，且重构（renaming/移动）会无理由地把测试爆掉。改成下面的真实行为测试。

Add to `gateway.rs` tests module (or a dedicated `gateway_sensitive_filter_tests.rs` integration file under `crates/gw-proxy/tests/`):

```rust
#[tokio::test]
async fn ctx_carries_compiled_policy_after_request_init() {
    // EntityStore stub seeded with one archive_mask rule.
    let store = test_store_with_policy(sample_policy());

    // Drive the part of request_filter that loads + compiles the policy.
    let mut ctx = ReqCtx::default();
    init_sensitive_filter_for_request(&store, &mut ctx).await;

    let compiled = ctx
        .sensitive_filter_policy
        .as_ref()
        .expect("policy should be cached on ctx");
    let out = compiled.mask_archive_bytes(b"ak-x");
    assert_eq!(out.as_ref(), b"<AK>");
}

#[tokio::test]
async fn ctx_policy_is_none_when_store_returns_none() {
    let store = test_store_without_policy();
    let mut ctx = ReqCtx::default();
    init_sensitive_filter_for_request(&store, &mut ctx).await;
    assert!(ctx.sensitive_filter_policy.is_none());
}

#[tokio::test]
async fn invalid_policy_keeps_last_good_compiled() {
    let store = test_store_with_policy(sample_policy());
    let mut ctx = ReqCtx::default();
    init_sensitive_filter_for_request(&store, &mut ctx).await;
    assert!(ctx.sensitive_filter_policy.is_some());

    store.replace_policy_with_invalid_regex().await; // installs "[invalid"
    let mut ctx2 = ReqCtx::default();
    init_sensitive_filter_for_request(&store, &mut ctx2).await;
    // Compile failed → ctx2 carries None (the per-request init does not
    // surface the previous request's compiled policy), but the store's
    // last-good compiled is retained for future requests.
    assert!(ctx2.sensitive_filter_policy.is_none());
    assert!(store.last_good_compiled_present().await);
}
```

Expected: FAIL because `init_sensitive_filter_for_request`, `EntityStore::replace_policy_with_invalid_regex`, and the by-version compiled-policy cache do not exist yet.

- [ ] **Step 2: Load + compile once per request, cache by version on EntityStore**

Add to `EntityStore`:

```rust
compiled_sensitive_filter_policy: tokio::sync::Mutex<Option<(u64, Arc<CompiledSensitiveFilterPolicy>)>>,
```

Implement `EntityStore::compiled_sensitive_filter_policy_or_compile()`:

```rust
pub async fn compiled_sensitive_filter_policy_or_compile(
    &self,
) -> Option<Arc<CompiledSensitiveFilterPolicy>> {
    let policy = self.sensitive_filter_policy_or_load().await?;
    let mut slot = self.compiled_sensitive_filter_policy.lock().await;
    if let Some((v, c)) = slot.as_ref() {
        if *v == policy.version {
            return Some(c.clone());
        }
    }
    match CompiledSensitiveFilterPolicy::compile(policy.value.as_ref()) {
        Ok(c) => {
            let c = Arc::new(c);
            *slot = Some((policy.version, c.clone()));
            Some(c)
        }
        Err(err) => {
            tracing::warn!(error = %err, "sensitive_filter: compile failed, keeping last good");
            // metric: gw_sensitive_filter_compile_errors_total
            slot.as_ref().map(|(_, c)| c.clone()) // last good, if any
        }
    }
}
```

In `gateway.rs`, expose a small helper used by both production code and tests:

```rust
pub(crate) async fn init_sensitive_filter_for_request(
    store: &EntityStore,
    ctx: &mut ReqCtx,
) {
    ctx.sensitive_filter_policy = store
        .compiled_sensitive_filter_policy_or_compile()
        .await;
}
```

Call it once from `request_filter`, before upstream rewrite. **Do not** call it from `upstream_request_filter` or any per-chunk path.

- [ ] **Step 3: Run gateway policy tests**

```bash
cargo +1.88.0 test -p gw-proxy sensitive_filter_policy_loaded_once_before_upstream_rewrite --locked
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-proxy/src/ctx.rs
git commit -m "feat(gw-proxy): load sensitive filter policy"
```

## Task 8: Update Invariants and Documentation

**Files:**
- Modify: `scripts/check-invariants.sh`
- Modify: `docs/architecture.md`
- Modify: `docs/spec/06-billing-ledger.md`
- Modify: docs mentioning raw detailed archive from `rg -n "wire-fidelity|no_detailed_redaction|redaction" docs crates scripts`

**Interfaces:**
- Produces: documented new filtered archive contract.

- [ ] **Step 1: Write failing invariant check**

Run:

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
./scripts/check-invariants.sh
```

Expected before edits: FAIL if new sensitive-filter code introduces words caught by `no_detailed_redaction`, or PASS for the wrong reason while docs still state raw archive. Record the current output in the task notes.

- [ ] **Step 2: Replace invariant**

Change the block labelled `no_detailed_redaction` to:

```bash
echo "[sensitive_filter_archive_boundary] archive masking must use sensitive filter engine only"
hits=$(grep -RnE 'redact_body|redact_headers|redact_message_content|redacted_preview|REDACT_HEADER_DENYLIST|REDACT_HEADER_ALLOWLIST' \
    crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v '^Binary file' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  bad "ad hoc detailed archive redaction helper present; use sensitive_filter engine"
else
  ok "no ad hoc detailed archive redaction helpers"
fi
grep -Rq "sensitive_filter" crates/gw-proxy/src || bad "sensitive filter archive boundary missing in gw-proxy"
```

- [ ] **Step 3: Update docs (explicit file + section enumeration — do not "search and replace globally")**

Run `rg -n "wire-fidelity|no_detailed_redaction|wire archive|redaction" docs/` and process each hit one by one. Do not delete unrelated content. The known locations as of this plan:

- `docs/architecture.md`: §Deliberate behaviours block mentioning "wire-fidelity archive" and the `no_detailed_redaction` invariant — replace with the sensitive-filter boundary description.
- `docs/spec/06-billing-ledger.md`: the `usage:detailed` section's "raw, unredacted" wording.
- `docs/workflow.md`: the archive emit step in the filter chain section.

In each location, the new wording MUST state, in this order:

1. Request and response bodies (and headers, for archive surfaces) **may** be masked before the `usage:detailed` XADD, when the sensitive-filter policy contains enabled `archive_mask` rules.
2. Reversible transit maps are request-local memory only — they are never serialized into `usage:detailed` and never persisted to PG/Redis.
3. The archive is byte-for-byte equal to the wire only when the global sensitive-filter policy has zero enabled `archive_mask` rules — this is the policy default.
4. Cross-link: `crates/gw-proxy/src/filters/sensitive_filter.rs` and `crates/gw-runtime/src/sensitive_filter.rs`.

**Do not** introduce a new top-level doc; extend existing sections in place.

- [ ] **Step 4: Run invariant check**

```bash
./scripts/check-invariants.sh
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/check-invariants.sh docs/architecture.md docs/spec/06-billing-ledger.md
git commit -m "docs: document sensitive filter archive boundary"
```

## Task 9: Runtime Verification

**Files:**
- Modify only if verification exposes defects in files from prior tasks.

- [ ] **Step 1: Run focused tests**

```bash
cd /Users/whereslow/Desktop/work/Cnewxus/ailinkmax-gateway
cargo +1.88.0 test -p gw-core sensitive_filter_policy --locked
cargo +1.88.0 test -p gw-runtime sensitive_filter --locked
cargo +1.88.0 test -p gw-store sensitive_filter_policy --locked
cargo +1.88.0 test -p gw-proxy sensitive_filter --locked
```

Expected: PASS.

- [ ] **Step 2: Run existing archive/response gates**

```bash
cargo +1.88.0 test -p gw-ledger --test dlq --test flusher --test inflight_clear_via_flusher --locked
cargo +1.88.0 test -p gw-proxy --test archive_state --test response_hot_path --locked
```

Expected: PASS.

- [ ] **Step 3: Run workspace checks**

```bash
cargo +1.88.0 fmt --all -- --check
cargo +1.88.0 check --workspace --lib --bins --locked
./scripts/check-invariants.sh
```

Expected: PASS.

- [ ] **Step 4: Commit verification fixes**

Run this step only when Steps 1-3 required code changes. Replace the file list with the exact files changed by those fixes:

```bash
git add crates/gw-proxy/src/gateway.rs crates/gw-runtime/src/sensitive_filter.rs
git commit -m "fix(gateway): stabilize sensitive filter runtime"
```

## Plan Self-Review

- Spec coverage: covers entity load, regex compile, reversible transit, streaming reverse, archive masking, invariants, and tests.
- Placeholder scan: cleaned in 2026-06-26 revision — surrogate format pinned; SENSITIVE_FILTER_POLICY_SQL inline; shared test-helper module declared; Task 7 source-grep test replaced by real behavior tests; Task 8 doc-update locations enumerated.
- Type consistency: Go snapshot names align with Rust serde names and namespace `sensitive_filter_policy`.
- Scope check: Go/admin implementation is intentionally in the companion Go plan.

### Revision 2026-06-26 — AI-implementation hardening

- Added the **AI 实施防错指南** block: explicit MUST DO / MUST NOT DO with this codebase's landmines (per-version compiled cache, surrogate collision safety, streaming tail buffer, `try_send` mask coverage check).
- Task 2 surrogate format pinned to `__sf_<xxh64(rule_id)>_<call_local_seq>__` — the previous `<replacement>-sf-<seq>` could collide with user text and let the reverse stream restore arbitrary substrings back to secrets.
- Task 2 streaming reverse algorithm spelled out (tail buffer of `max_surrogate_len - 1` bytes, `eos` semantics).
- Task 3 `SENSITIVE_FILTER_POLICY_SQL` now inline with the full CTE — including the "missing row ⇒ empty policy + version 0, never an error" branch needed for no-op semantics.
- Task 7 source-string regression test replaced with three behavior tests (load + cache hit, no-policy ⇒ None, invalid policy keeps last good). Compiled policy is now cached on `EntityStore` keyed by version, not recompiled per-request.
- Task 4 / 5 / 6 share a single `sensitive_filter_test_support.rs` helper module — no more invented test fixtures per task.
- Task 8 docs-update step lists each file/section explicitly instead of asking the implementer to "globally replace wording".
