# 2026-05-22 — Go 接入阻塞项 Spec(非流式 + Mgmt Token)

> **Status**: spec(待审阅,实现不在本 spec 范围)
> **日期**: 2026-05-22
> **作者**: 协作设计(Claude + 用户)
> **触发**: Go 网站端 FRD(`golang-website-frd.md`)落地前的网关侧 gap 盘点。
> **范围**: 解锁 Go 控制面对接的两个硬阻塞点 —— **B1 非流式响应** 与 **B2 Management Token 验证**。
> **不在范围**:
>   - 跨协议非流式响应 re-render(留 codec-s4 v2)。
>   - Aether 前端 DTO 兼容层(Go 自治)。
>   - 任何 Go 侧业务实现细节(参 FRD)。
>
> **Companion**:
>   - [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](2026-05-22-go-onboarding-gaps-design.md) — 非阻塞缺口项。
>   - [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) — 本 spec 引入的 Go 侧契约变更。

> 一句话定义:把 Go 接入网关前**最先撞墙的两件事**(同步调用被 400 + admin 监控面 401)从 spec 层面闭环,作为后续 plan 的输入。

---

## 0. 阻塞项清单

| # | 阻塞 | 当前症状 | 影响面 |
|---|------|----------|--------|
| B1 | 非流式响应被 `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400 拒 | OpenAI Chat / Anthropic Messages / Gemini Generate 同步调用全 400 | FRD §6.7 usage 真实回放、所有非流式 SDK / 测试调用 |
| B2 | Management Token 验证 = 501 / 401 | `/healthz/circuits` 等 admin 端点 401;`ManagementTokenEntry` 未上线 | FRD §6.2 用户 mgmt token、§6.8 管理监控面 |

两者互相独立,可并行 / 任意顺序落地。

---

## 1. B1 — 非流式响应支持

### 1.1 Status

REJECTED_BY_DESIGN。`crates/gw-proxy/src/filters/request.rs` 在 IR 解出 `is_stream == false` 时短路 `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` 400(见 [`docs/architecture.md`](../../architecture.md) §8 "Deliberate behaviours" 第 1 条)。

原因:v1 三个 codec 的 `parse_response` 全部 `unimplemented!()`,若放过非流式会导致 Rust 静默写 `cost_micro = 0`(由 `no_cost_micro` 不变量保护,真正风险是 `usage.output_tokens = 0` 无法计费)。这条 fail-closed 是临时围栏,本 spec 拆除围栏。

### ADR-B1-1 — 各 codec 实现 `parse_response`

**Decision**: `ProtocolCodec::parse_response(&self, body: &[u8]) -> Result<ResponseIR, CodecError>` 从 `unimplemented!()` 改为真正反序列化,产出携带 `usage` 字段的 `ResponseIR`。

实现位点(三处并行):

| Codec | 文件 | 关键字段映射 |
|---|---|---|
| OpenAI | `crates/gw-codec/src/openai/codec.rs::parse_response` | `usage.prompt_tokens → input_tokens`;`usage.completion_tokens → output_tokens`;`usage.prompt_tokens_details.cached_tokens → cache_read_input_tokens`(若存在)|
| Anthropic | `crates/gw-codec/src/anthropic/codec.rs::parse_response` | `usage.input_tokens / output_tokens / cache_creation_input_tokens / cache_read_input_tokens` 直接对应 |
| Gemini | `crates/gw-codec/src/gemini/codec.rs::parse_response` | `usageMetadata.promptTokenCount → input_tokens`;`candidatesTokenCount → output_tokens`;`cachedContentTokenCount → cache_read_input_tokens` |

**失败处理**:任一 codec parse 失败,返回 `CodecError::ParseResponse`。上层 `response_body_filter` 不阻塞响应转发(已发给客户端),但 `UsageRecord.usage_source` 标 `fallback_conservative`,沿用 `freeze_amount_micro` 估算。Metric `gw_codec_parse_response_failure_total{codec, upstream_status}`。

**Rejected alternative**:不走 ResponseIR,直接在 proxy 层用 regex 抓 usage 字段。被否决理由:跨协议 usage 字段命名 / 嵌套各不同;codec 是唯一应当承担 wire ⇄ IR 责任的地方,proxy 层不应当再有协议分支。

### ADR-B1-2 — `request_filter` 移除非流式 guard

**Decision**: 删除 `crates/gw-proxy/src/filters/request.rs` 中 `if !ir.stream { short_circuit(GW_REQUEST_NONSTREAM_NOT_SUPPORTED, 400) }` 短路。

`ctx.idempotent = !is_stream` 赋值**保留**:非流式天然幂等,可被 routing-r-e v2 的同 key 重试逻辑利用。

### ADR-B1-3 — `response_body_filter` 非流式分支

**Decision**: `crates/gw-proxy/src/filters/response.rs::response_body_filter` 当 `ctx.is_stream == false` 走新分支:

1. 累积上游完整 body 到 `ctx.upstream_body_accum: BytesMut`(新字段);
2. `end_of_stream == true` 时调用 `upstream_codec.parse_response(&accum)`;
3. 成功 → 把 `ResponseIR.usage` 写到 `ctx.usage_accum`,清 accum,`ctx.usage_source = UsageSource::Upstream`;
4. 失败 → metric +1,`ctx.usage_source = UsageSource::FallbackConservative`,`ctx.usage_accum` 保留 freeze 估算的等价值;
5. 流式路径(`is_stream == true`)行为完全不变。

**不变量**:cross-protocol re-render(G8)只在流式路径生效;非流式跨协议直接走 ADR-B1-4。

### ADR-B1-4 — 跨协议非流式直接 503

**Decision**: 在 `crates/gw-proxy/src/filters/upstream.rs::pick_peer` 末段,若 `ctx.client_codec.kind() != picked_endpoint_kind && ctx.is_stream == false`,短路 `503 GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`。

**Why**: 同协议非流式(80% 用户场景)优先打通;跨协议非流式需要在 `response_body_filter` 把整个 `ResponseIR` 用 client codec render 回去 —— 比 G8 的 per-chunk SSE 复杂度高一阶,属于 codec-s4 v2 范畴。比让 Go 等所有响应路径就绪要好。

**Rejected alternative**:在响应阶段 render。被否决理由:同步响应的 render 需要 client codec 接受完整 `ResponseIR`(含 tool_calls / multimodal / thinking),涉及 codec API 扩面;G8 之前 codec 只承诺 `RequestIR` / `StreamEventIR` 两个 IR。

### 1.2 Acceptance

- OpenAI / Anthropic / Gemini 客户端 → 同协议上游同步调用返回 200,`UsageRecord.usage.output_tokens > 0` 且与上游响应 `usage` 一致;
- 跨协议非流式返回 503 `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED`;
- 现有所有流式测试通过,无回归;
- `usage_source = upstream`(parse 成功)/ `fallback_conservative`(parse 失败)枚举值都被覆盖;
- 新测:`crates/gw-proxy/tests/non_stream_e2e.rs` + e2e fake_client 同协议 3 个 SDK 同步场景。

### 1.3 Go 侧契约影响

**无新增字段**。`UsageRecord.usage_source` 枚举值已含 `fallback_conservative`(参 [`docs/go-contracts/parts/02-go-as-consumer.md`](../../go-contracts/parts/02-go-as-consumer.md) §2.1.2),Go settler 拿到 `fallback_conservative` 后按估算 cost 兜底结算,行为已契约化。

---

## 2. B2 — Management Token 验证(T6-A)

### 2.1 Status

PARTIAL。[`docs/workflow.md`](../../workflow.md) §2.2 step 1 显示 `is_management_path(&path)` 已识别管理路径,但目前返回 501;[`docs/architecture.md`](../../architecture.md) §6.1 显示 `/healthz/circuits / /healthz/key/{id} / /healthz/resilience` 当前固定 401。`ManagementTokenEntry` 类型 / `management_tokens` 表 / mgmt token 黑名单 namespace 均未上线。

FRD 直接相关章节:
- §6.2 用户中心 `/api/me/management-tokens`(用户自己管理 mgmt token);
- §6.8 管理监控 `/api/admin/monitoring/*` / `cache/*` / `trace/*`(管理员级访问)。

### ADR-B2-1 — 引入 `ManagementTokenEntry`

**Decision**: 新增 entity,字段:

```rust
// crates/gw-core/src/entity.rs
pub struct ManagementTokenEntry {
    pub token_id: String,          // 前缀 mt_
    pub user_id: UserId,
    pub scope: MgmtScope,          // admin_full | admin_read | user_self
    pub hash: Vec<u8>,             // SHA-256(secret)
    pub expires_at: Option<OffsetDateTime>,
    pub status: TokenStatus,       // active | disabled
    pub display_name: String,
}
```

入 `EntityStore.mgmt_token` + `loader::load_mgmt_token` + `StreamConsumer` dispatch(ns = `management_token`)+ Moka 5 min TTL(与 `apikey` 同档)。PgSQL migration 编号 `0007_management_tokens.sql`(`0005_proxy_nodes` 已存在、`0006_auth_configs` 由 gaps spec G2 占用)。

**Hash 范式**:与 `ApiKeyEntry.hash` 一致 —— SHA-256(secret),constant-time 比对(`subtle::ConstantTimeEq`)。**禁止**存明文;Go 侧创建时一次性返回完整 token,之后用户只能凭 hash 验证。

### ADR-B2-2 — Token 格式 + 验证位点

**Token wire format**: `mt_<token_id>.<secret>`,通过 **`Authorization: Mgmt mt_<token_id>.<secret>`** header 传递(与现有 `crates/gw-auth/src/mgmt.rs::parse_mgmt_authorization` 解析的 `Mgmt <token>` 形态对齐;而非 `Bearer`,避免与业务 API key / JWT 路径混淆)。Aether 前端的管理面请求需要使用 `Mgmt` 而非 `Bearer`。

**验证位点**:在 `crates/gw-proxy/src/filters/request.rs::is_management_path` 分支:

1. 取 `Authorization: Mgmt <token>` 通过 `parse_mgmt_authorization`,缺失 / 格式错 → 401 `GW_AUTH_MGMT_TOKEN_MISSING`;
2. `mt_<id>` 不匹配前缀 → 401 `GW_AUTH_MGMT_TOKEN_INVALID`;
3. `store.mgmt_token_or_load(token_id)` 失败 → 401 `GW_AUTH_MGMT_TOKEN_INVALID`;
4. SHA-256(secret) 与 `entry.hash` constant-time 比对失败 → 401 `GW_AUTH_MGMT_TOKEN_INVALID`(故意与 step 3 同码,避免 enumeration);
5. `expires_at` 已过 → 401 `GW_AUTH_MGMT_TOKEN_EXPIRED`;
6. `status != active` → 401 `GW_AUTH_MGMT_TOKEN_DISABLED`;
7. 黑名单命中(`mgmt_token:blacklist:<token_id>`)→ 401 `GW_AUTH_MGMT_TOKEN_REVOKED`;
8. `scope` 与请求路径不匹配 → 403 `GW_AUTH_MGMT_TOKEN_SCOPE_DENIED`;
9. 通过 → `ctx.mgmt_user_id = entry.user_id`;`ctx.mgmt_scope = entry.scope`;请求按管理面路径继续。

### ADR-B2-3 — `/healthz/*` 解锁

**Decision**: `crates/gw-gateway/src/health` 在三个 token-gated 路由前插入 mgmt token 校验:

- `/healthz/circuits` — 需 `scope >= admin_read`,返回当前所有 `vendor_key` 的 cooldown / 健康状态 JSON;
- `/healthz/key/{id}` — 需 `scope >= admin_read`,返回单 key 健康详情;
- `/healthz/resilience` — 需 `scope >= admin_read`,返回各 vendor breaker 状态。

`/livez` / `/readyz` 保持无认证(load balancer 健康检查依赖)。

校验 helper 抽取到 `crates/gw-auth/src/mgmt_token.rs::verify_mgmt_token`,proxy filter 与 health server 共用同一实现。

### ADR-B2-4 — Mgmt token 撤销 = Redis 黑名单

**Decision**: 复用 [G6 JWT 黑名单](../../architecture.md#21-crate-responsibilities) 的 `RedisJwtBlacklist` 抽象,改名为 `RedisTokenBlacklist`,支持两个 namespace:

- `jwt:blacklist:<jti>` —— 现有 JWT 黑名单;
- `mgmt_token:blacklist:<token_id>` —— 新增 mgmt token 黑名单。

Go 在用户主动撤销 / 管理员强制 revoke 时 `SET mgmt_token:blacklist:<token_id> "1" EX <剩余有效期>`。Rust 验证通过后做一次 `EXISTS`,命中即 401(`GW_AUTH_MGMT_TOKEN_REVOKED`)。

**FailMode**:与 JWT 黑名单同 —— `FailClosed`(默认,Redis 不可达则继续 401)/ `FailOpen`(放行)。由 `AuthConfig.blacklist_fail_mode` 单一开关控制(对 JWT / mgmt 共用)。

**Rejected alternative**:在 PgSQL `management_tokens.status` 上做撤销(置 `disabled`)。被否决理由:status 变更走 events:entity 通知有秒级 lag,而 Redis EXISTS 是即时。撤销要求即时生效。

### ADR-B2-5 — Scope 矩阵

| Scope | 允许访问 |
|---|---|
| `user_self` | `/api/me/*`、用户自己的资源(rely on `mgmt_user_id == 资源 user_id`)|
| `admin_read` | 上加 `/api/admin/monitoring/*`、`/healthz/*` 只读 endpoint |
| `admin_full` | 上加所有 `/api/admin/*`(CRUD)|

Rust 网关只裁判 `/healthz/*` 与已识别的 `is_management_path` 前缀;`/api/*` 的细分路径 scope 判定由 Go 侧实现(Rust 不路由这些路径)。

### 2.2 Acceptance

- 携带 `Authorization: Mgmt mt_xxx.<secret>` + `scope >= admin_read` 访问 `/healthz/circuits` 返回 200 + JSON;
- 过期 token → 401 `GW_AUTH_MGMT_TOKEN_EXPIRED`;
- 错误 secret → 401 `GW_AUTH_MGMT_TOKEN_INVALID`(与"token 不存在"同码);
- `scope = user_self` 访问 `admin_read` 端点 → 403 `GW_AUTH_MGMT_TOKEN_SCOPE_DENIED`;
- `SET mgmt_token:blacklist:<id>` 后再请求 → 401 `GW_AUTH_MGMT_TOKEN_REVOKED`,1s 内生效;
- `/livez` / `/readyz` 无 token 仍 200;
- 新测:`crates/gw-gateway/tests/healthz_mgmt_token.rs` + e2e mgmt token 场景。

### 2.3 Go 侧契约影响

**新增**:
- entity ns = `management_token`(events:entity 新 ns + EntityStore + loader + StreamConsumer dispatch);
- PgSQL 新表 `management_tokens`(migration 0007;参见 [contract-change §1.5](../../go-contracts/contract-change/2026-05-22-go-onboarding.md#15-migration-0007-模板-供-go-端参考));
- Redis 新 key namespace `mgmt_token:blacklist:*`(写入语义参考 `jwt:blacklist:*`)。

详见 [`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md) §1。

---

## 3. 实施顺序提示(非 plan)

不写 plan,仅记录可并行 / 串行约束:

- **B1 / B2 互相独立**,可并行落地。
- **B1 内部**:ADR-B1-1(三个 codec)可并行 → ADR-B1-2(删 guard) → ADR-B1-3(response 分支)→ ADR-B1-4(cross-protocol 503,可与 B1-3 并行)。
- **B2 内部**:ADR-B2-1(entity + 0006 migration) → ADR-B2-2(verify helper + filter 分支) → ADR-B2-3(/healthz 解锁) → ADR-B2-4(黑名单)。其中 B2-4 可与 B2-2/3 并行(黑名单是补充验证步骤)。

---

## 4. 风险与回滚

| 风险 | 缓解 |
|---|---|
| OpenAI 上游 `usage` 字段缺失(legacy 模型 / 自托管端点)| `usage_source = fallback_conservative`,Rust 不阻塞,Go settler 拿到 estimated 仍可结算(已有契约支持)|
| `mgmt_token_id` 与 `api_key_id` 命名空间相似导致误用 | token_id 强制前缀 `mt_` 与 `ak_` 区分;ADR-B2-2 step 2 在前缀不匹配时即拒 |
| `/healthz/*` 解锁后被探测 | 仍要求 `scope >= admin_read`,无 token / 错 token 401;`/livez` / `/readyz` 是唯一公开 endpoint |
| 非流式开放后 freeze 估算偏低 → 透支 | Go EstimatePricing 推算 `conservative_per_token_micro` 已覆盖 output tokens 上限(参 [`docs/go-contracts/parts/01-go-as-producer.md`](../../go-contracts/parts/01-go-as-producer.md) §1.5.2);新场景不引入新公式 |
| B2 上线前 admin UI 已经依赖 mgmt token | Go 侧 mgmt token 生成 UI 与 Rust 侧验证可独立部署;Go 先上 → 用户能创建 token 但管理面访问 401(无回归,只是不可用)|

回滚:两个 ADR 块均可独立 revert。B1 revert = 恢复 `is_stream == false` 短路即可(无数据迁移);B2 revert = 移除 verify 调用,`/healthz/*` 回到 401(无数据迁移,黑名单 key 保留无害)。

---

## 5. 关联

- 主架构:[`docs/architecture.md`](../../architecture.md) §6.1 Listeners、§8 Deliberate behaviours(第 1 条 = B1 围栏)
- 请求流:[`docs/workflow.md`](../../workflow.md) §2.2 step 1(B2 当前 501)、§2.2 step 9(B1 当前 400 短路)、§2.6 response_body_filter
- Go 契约:[`docs/go-contracts/parts/02-go-as-consumer.md`](../../go-contracts/parts/02-go-as-consumer.md) §2.1.2 `UsageRecord.usage_source`
- 契约变更记录:[`docs/go-contracts/contract-change/2026-05-22-go-onboarding.md`](../../go-contracts/contract-change/2026-05-22-go-onboarding.md)
- 同日缺口 spec:[`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](2026-05-22-go-onboarding-gaps-design.md)
