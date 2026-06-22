# 设计：错误消息规则改写机制

- 日期：2026-06-22
- 范围：`AILinkMax-Go` 管理面、前端、Go -> Rust 配置契约；`ailinkmax-gateway` Rust 数据面热路径错误响应。
- 非范围：不改变稳定 `GW_*` 错误码含义，不让 Go 参与请求热路径，不引入脚本/正则/表达式引擎，不改变 failover、计费或 key health 判定。

## 已确认决策

1. 管理面新增独立全局页 `/admin/error-messages`，而不是只藏在单个 provider 抽屉里。
2. 规则按多维上下文命中：provider、model、endpoint、api format、请求路径、客户端协议、HTTP status、gateway code、上游错误消息片段等。
3. 命中规则后只改写对客户端返回的 error `message` 字段；`code` / `gateway_code` / status / type 保持稳定。
4. Rust 必须使用本地缓存，热路径不得查 Postgres 或 Redis。
5. 匹配算法使用维度树逐级短路。一个维度未命中就跳过该分支；空维度使用 wildcard 分支。
6. 配置是声明式数据。禁止 regex、脚本、循环、递归引用、动态表达式和远程回调。
7. 文本匹配只允许 bounded case-insensitive substring，沿用现有 8 KiB 扫描上限思想，避免 ReDoS 和大错误体拖慢热路径。

## 背景

当前 Go 管理面已经在 provider 配置里保存 `error_passthrough_rules`，前端 provider 抽屉里也有“错误透传规则”弹窗。但现状存在三个问题：

- 规则是 provider 局部配置，不适合表达跨 provider、model、endpoint 的统一用户提示。
- 前端文案称“正则 + 状态码”，而 Rust 当前真实实现是大小写不敏感 substring allow/deny。
- Rust 只消费 boot-time `[error.passthrough]` 全局配置；管理面写入的 provider `config.error_passthrough_rules` 没有形成热更新本地缓存。

本设计把“错误透传/遮蔽”升级为“错误消息改写规则”。它面向用户提示：例如用户用文本对话接口请求生图任务时，返回明确动作建议，而不是透出上游含混错误。

## 目标

- 管理员能在一个页面集中创建、搜索、启停、排序和测试错误消息规则。
- 规则能覆盖 provider + model + endpoint + protocol + status + gateway code + upstream message 等常见错误维度。
- Rust 在本地内存中以编译后的树形结构匹配规则，热路径开销稳定且可预测。
- 规则发布后所有 Rust 实例通过现有 `events:entity` 机制热更新。
- 错误响应保持协议兼容：OpenAI、Anthropic、Gemini、Kiro 的 envelope 形状不变。
- 旧 provider `error_passthrough_rules` 可迁移或保留为 legacy 展示，但新机制是唯一的消息改写入口。

## 非目标

- 不暴露上游原始错误详情作为默认行为。
- 不允许管理员写 regex、JS、Lua、WASM、CEL、JSONPath 递归查询或其他可循环求值能力。
- 不做多语言模板渲染。本期 `message` 是一段固定字符串。
- 不支持运行时按用户、余额、IP 等高基数字段配置规则。
- 不修改 Rust 错误分类器对 retry、cooldown、OAuth signal、key health 的判定。

## 规则语义

一条规则包含：

```json
{
  "id": "emr_image_wrong_endpoint",
  "name": "Image task requested through chat endpoint",
  "enabled": true,
  "priority": 100,
  "match": {
    "provider_ids": ["v_openai"],
    "models": ["gpt-image-2-max"],
    "endpoint_ids": [],
    "api_formats": ["openai"],
    "request_paths": ["/v1/chat/completions", "/v1/responses"],
    "client_protocols": ["openai"],
    "http_statuses": [400, 404, 422],
    "gateway_codes": ["GW_UPSTREAM_4XX"],
    "upstream_message_contains": ["image", "chat", "unsupported"]
  },
  "message": "该模型是生图模型，请使用图片生成接口提交任务，不要通过文本对话接口调用。",
  "notes": "运营说明，可选，不返回给用户"
}
```

字段约定：

- 每个 match 数组为空表示该维度 wildcard。
- `priority` 数字越小越优先；同优先级按 `id` 字典序稳定排序。
- `models` 使用用户请求中的 client-facing model 名称；Rust 已有 request context 中的 `ctx.model` 可作为输入。
- `endpoint_ids` 使用 Go 管理面 endpoint id。Rust 当前错误路径不总是保存 endpoint id；实现期需要在成功选择 upstream endpoint 后把它写入 request context。
- `api_formats` 使用管理面 api format 字符串，例如 `openai`、`anthropic`、`gemini`、`kiro` 或更细的 `openai:responses`。Rust 编译时规范化到内部 protocol / shape。
- `gateway_codes` 匹配最终 envelope 中将返回的稳定 `GW_*` code。
- `upstream_message_contains` 只做大小写不敏感 substring，最多扫描提取出的消息前 8 KiB。

命中后只替换 message。原响应中的 `code`、`gateway_code`、HTTP status、协议错误 type/status 不变。

## 匹配树

Rust 编译规则时生成不可变 `CompiledErrorMessageRules`：

```text
root
  provider_id | *
    model | *
      endpoint_id | *
        api_format | *
          request_path | *
            client_protocol | *
              gateway_code | *
                http_status | *
                  leaves: [CompiledRule]
```

热路径匹配流程：

1. 构造 `ErrorMessageMatchCtx`，只包含本请求已知的小字段。
2. 从根节点开始，每一层最多访问两个分支：精确值分支和 wildcard 分支。
3. 如果某层两个分支都不存在，整棵子树直接短路，不继续扫描。
4. 到叶子后按 priority/id 遍历少量候选。
5. 对候选执行 `upstream_message_contains` 检查；第一条命中即返回 message。
6. 没有命中则保留原有 passthrough/mask 后的 message。

树形匹配禁止任何循环：

- 规则不能引用其他规则。
- 规则不能包含 parent/child 关系。
- 编译结果是 acyclic map/tree。
- 热路径没有 while-until-match 之类的动态求值；只有固定维度深度的 for/branch。

为防止配置滥用，Go 侧和 Rust 编译侧都应设置硬限制：

- 单条规则每个维度最多 50 个值。
- `upstream_message_contains` 最多 10 个片段，每个片段 1 到 256 字符。
- 全局启用规则最多 5000 条。
- 展开后的叶子候选总量超过阈值时拒绝发布或拒绝加载，Rust 保留旧快照。

## Go 数据模型

新增表 `error_message_rules`：

```sql
CREATE TABLE IF NOT EXISTS error_message_rules (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  enabled BOOLEAN NOT NULL DEFAULT true,
  priority INTEGER NOT NULL DEFAULT 1000,
  match JSONB NOT NULL DEFAULT '{}'::jsonb,
  message TEXT NOT NULL,
  notes TEXT,
  version BIGINT NOT NULL DEFAULT 1,
  created_at BIGINT NOT NULL,
  updated_at BIGINT NOT NULL,
  deleted_at BIGINT
);

CREATE INDEX IF NOT EXISTS idx_error_message_rules_enabled_priority
  ON error_message_rules (enabled, priority, id)
  WHERE deleted_at IS NULL;
```

Go 是规则权威写入方。所有 create/update 必须校验：

- `name`、`message` 非空。
- `priority` 在 `0..=1_000_000`。
- 所有 match 字段都是字符串或整数数组，不接受对象嵌套。
- `http_statuses` 只能是 `100..=599`。
- `message` 最大 1000 字符。
- `notes` 最大 4000 字符。
- 不接受 regex 元字符检查不是安全边界；真正边界是字段语义只按普通字符串处理，不编译 regex。

## Go API

新增 provider 或独立模块下的 admin API：

- `GET /api/admin/error-messages/rules`
- `POST /api/admin/error-messages/rules`
- `GET /api/admin/error-messages/rules/{id}`
- `PATCH /api/admin/error-messages/rules/{id}`
- `DELETE /api/admin/error-messages/rules/{id}`
- `POST /api/admin/error-messages/rules/test`

列表支持分页和筛选：

- `search`
- `enabled`
- `provider_id`
- `model`
- `api_format`
- `gateway_code`
- `http_status`

`test` 接口接收一份 match context 和可选 upstream message，返回命中的 rule id、message 和匹配路径。它只调用 Go 侧同构匹配器，用于管理面预览；Rust 仍是运行时权威。

每次规则写入成功后，Go 发：

```text
XADD events:entity * ns error_message_rules id global v <max_version> op upsert
```

同时可选 write-through：

```text
HSET entity:error_message_rules:global v <max_version> enc json d <snapshot_json>
```

write-through 失败不回滚 Postgres；Rust 可冷加载 PgSQL。

## Rust 数据模型与缓存

新增 `gw-core::error::message_rules`：

```rust
pub struct ErrorMessageRule {
    pub id: String,
    pub enabled: bool,
    pub priority: u32,
    pub matcher: ErrorMessageMatcher,
    pub message: String,
}

pub struct ErrorMessageMatcher {
    pub provider_ids: Vec<String>,
    pub models: Vec<String>,
    pub endpoint_ids: Vec<String>,
    pub api_formats: Vec<String>,
    pub request_paths: Vec<String>,
    pub client_protocols: Vec<ProtocolKind>,
    pub http_statuses: Vec<u16>,
    pub gateway_codes: Vec<String>,
    pub upstream_message_contains: Vec<String>,
}
```

新增 `gw-store` loader：

- Redis key：`entity:error_message_rules:global`
- PgSQL fallback：`SELECT ... FROM error_message_rules WHERE enabled = true AND deleted_at IS NULL ORDER BY priority, id`
- TTL：300s 即可；热更新主要依赖 stream invalidation。

新增本地 holder：

```rust
pub struct ErrorMessageRuleStore {
    current: arc_swap::ArcSwap<CompiledErrorMessageRules>
}
```

启动时加载一次。加载失败时使用空规则集启动，并打 warn/metric；后续 stream 事件触发后台 reload。reload 编译失败时保留旧快照，避免坏配置让错误路径不可用。

`StreamConsumer` 增加 namespace：

```text
ns=error_message_rules id=global
```

该事件不需要让每个请求等待 reload；后台任务完成后原子替换。

## Rust 错误响应接入点

现有路径：

- `response_filter` 在 upstream status >= 400 时设置 `ctx.error_outcome` 和 `ctx.error_passthrough_active`。
- `response_body_filter` 在 EOS 读取 upstream body，解析 JSON，调用 `gw_codec::error_passthrough::render_error_to_client(...)` 渲染协议 envelope。
- `render_error_to_client` 当前提取 raw message，再套 allow/deny mask。

新逻辑：

1. 从 `PassthroughBody` 提取 raw upstream message。
2. 先执行现有 allow/deny mask，得到 base message。
3. 计算 gateway code。
4. 构造 `ErrorMessageMatchCtx`：
   - `provider_id`：picked vendor id
   - `model`：ctx.model
   - `endpoint_id`：ctx.selected_endpoint_id
   - `api_format`：upstream/client shape
   - `request_path`：原始 path
   - `client_protocol`
   - `http_status`
   - `gateway_code`
   - `upstream_message`
5. 查询本地 `ErrorMessageRuleStore`。
6. 命中则用 rule.message 替换 base message；未命中则使用 base message。

本机制应覆盖非流式和 SSE error frame。SSE 仍只输出一个 `event: error` frame。

## 前端设计

新增页面 `frontend/src/views/admin/ErrorMessageRules.vue`，菜单项放在“管理”组，建议位置在“提供商”和“模型管理”之间或“网关配置”附近。

页面结构：

- 顶部筛选：搜索、启用状态、provider、model、api format、gateway code、HTTP status。
- 主表：名称、优先级、状态、匹配摘要、返回消息、更新时间、操作。
- 规则编辑抽屉或弹窗：
  - 基本信息：name、enabled、priority、message、notes。
  - 匹配条件：provider 多选、model 输入/选择、endpoint 多选、api format 多选、request path 多值输入、client protocol 多选、gateway code 多值输入、HTTP status 多值输入、upstream message contains 多值输入。
  - 测试命中：输入上下文后调用 `/test`，显示命中规则和 message。

UI 不出现“正则”字样。文本匹配文案使用“包含片段”。

现有 `ErrorPassthroughRulesDialog`：

- 短期可保留，但文案改成 legacy/透传遮蔽，不再承载消息改写。
- 或在 provider 抽屉按钮跳转到 `/admin/error-messages?provider_id=<id>`，避免两个入口配置同类能力。

## 迁移策略

第一阶段不自动迁移 provider `config.error_passthrough_rules`，避免把“允许透传”误转换为“改写消息”。

上线后：

1. 新规则页默认空。
2. Rust 无规则时行为与现在一致。
3. 运维可手工为高频误用场景添加规则。
4. 后续如需要迁移 legacy passthrough rules，应做只读导入建议，而不是自动启用。

## 可观测性

新增指标：

- `gw_error_message_rules_snapshot_version`
- `gw_error_message_rules_reload_total{result="success|error"}`
- `gw_error_message_rule_match_total{rule_id}`
- `gw_error_message_rule_no_match_total`
- `gw_error_message_rule_compile_error_total`

日志：

- reload 成功：version、rule_count、compiled_node_count。
- reload 失败：错误摘要、保留的旧 version。
- 单请求命中不打 info 日志，只打 metric，避免热路径日志放大。

## 测试

Go：

- business validation：拒绝 regex/script/对象嵌套/过长片段/非法 status。
- CRUD API：创建、更新、启停、删除、分页筛选。
- test matcher：精确分支、wildcard 分支、priority 冲突、未命中。
- pusher：写入后发 `events:entity ns=error_message_rules id=global`。
- contract tests：新增前端 API 路由覆盖。

Rust：

- 编译器：把规则编译成树，wildcard + exact 同时存在时都能访问。
- 匹配器：每层未命中短路；priority/id 稳定选择第一条。
- 安全：拒绝超过阈值的规则快照；substring 扫描最多 8 KiB。
- reload：编译失败保留旧快照。
- rendering：OpenAI、Anthropic、Gemini、Kiro 非流式和 SSE 都只改 message。
- performance micro test：典型 5000 规则集下命中和未命中不访问 async runtime / Redis / PgSQL。

前端：

- 列表筛选和分页。
- 编辑表单校验。
- 测试命中结果展示。
- 菜单和路由注册。

## 文档同步

Go 侧实现需要同步：

- `docs/business/provider/api.md` 或新增 `docs/business/error-message-rules/api.md`
- `docs/business/provider/model.md` 或新增模块 model 文档
- `docs/rust-contract/parts/01-go-as-producer.md`
- `docs/rust-contract/parts/03-shared-schema.md`

Rust 侧实现需要同步：

- `docs/go-contracts/main.md`
- `docs/go-contracts/parts/01-go-as-producer.md`
- `docs/go-contracts/parts/03-shared-schema.md`
- `docs/workflow.md` 的 error passthrough/rendering 阶段
- `docs/architecture.md` 的 entity namespace / cache / deliberate behavior

## 验收标准

- 管理员能创建一条“生图模型通过文本对话接口调用”的规则。
- 请求触发上游 4xx 后，客户端收到协议兼容 error envelope，稳定 code 不变，message 被改成管理面配置文本。
- 禁用规则后，Rust 在无需重启的情况下回到原 message 行为。
- Rust 热路径 flamegraph / 单测证明不访问 Redis/Postgres，不执行 regex，不存在循环求值。
- 坏规则快照不会替换正在使用的旧快照。
