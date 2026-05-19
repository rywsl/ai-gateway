# K-C — OAuth Token 缓存 + 刷新（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界，未含实现）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 3 K1 + K2）
  - [2026-05-19-key-health-k-a-state-machine.md](./2026-05-19-key-health-k-a-state-machine.md)（K-A 接收 cooldown）
  - [2026-05-19-key-health-k-b-vendor-error-hook.md](./2026-05-19-key-health-k-b-vendor-error-hook.md)（K-B 发出 OAuthSignal）
  - [2026-05-19-adapter-layer-design.md](./2026-05-19-adapter-layer-design.md)（adapter 层的 OAuth refresh 钩子）
- **范围**：在 `gw-adapter` 内承载 OAuth token 的运行时缓存 + 懒刷新 + 失败标记三态机：
  - **缓存**：access_token / refresh_token / expires_at 进程内 Arc + Redis entity 同步；
  - **懒刷新**：上游 401 触发 K-B `OAuthSignal::TokenExpired` → K-C 异步刷新；
  - **状态前缀**：`[ACCOUNT_BLOCK]` / `[OAUTH_REFRESH_FAILED]` / `[OAUTH_EXPIRED]` 通过 K-A 的 cooldown_reason 表达，**不**新建额外存储。
- **不在范围**：
  - 具体 vendor 的 OAuth endpoint（→ 后续 vendor adapter plan）；
  - 用户登录 OAuth（这是 Go 管理面职责，与上游 OAuth 完全不同）；
  - quota probe（→ K-D）。

> K-C 把 OAuth refresh 从 Aether `provider/oauth_token.py` 等价迁到 Rust adapter 层，通过 K-A 暴露不可用态给 routing。

---

## 1. 背景

某些上游用 OAuth 而非 API key 鉴权——Claude Code OAuth、Gemini CLI OAuth、Vertex AI service account JWT。这些 token：

- 有效期短（1h-12h）；
- 过期后需要用 refresh_token 拿新 access_token；
- 刷新失败时（refresh_token 也过期）需要标记账号需人工介入；
- 同一 key 在多副本网关下，刷新动作应当串行（避免 race condition）；
- 账号被服务方主动 deactivate 时，再刷新也救不回来——需要"账号级 block"标记，与"token 级 expire"区分。

Aether `provider/oauth_token.py` + `pool/account_state.py` 实现了完整状态机。本 spec 在 Rust adapter 层落地，**复用 K-A 的 cooldown 机制**作为对 routing 的暴露面，不新建独立 store。

## 2. 关键判断

### ADR-KC-1 OauthCache 在 `gw-adapter`，不在 gw-health

K-A 是"通用 key 健康"；K-C 是"OAuth 特化机制"。OAuth 概念只在某些 vendor 存在（claude_code / gemini_cli / vertex_ai），不应当污染 gw-health。

依赖方向：

```
gw-core    ◄── gw-adapter::oauth
gw-store   ◄── gw-adapter::oauth      （Redis entity:vendor_key + counter，复用通用 protocol）
gw-health  ◄── gw-adapter::oauth      （刷新失败/账号 block 时调 apply_cooldown）
gw-adapter ◄── gw-proxy / gw-task-runtime
```

### ADR-KC-2 数据形态

```rust
// gw-adapter::oauth::cache

pub struct OauthCache {
    /// 主索引：KeyId → 该 vendor key 的 OAuth 状态。
    by_key: DashMap<KeyId, Arc<KeyOauthState>>,
    /// 跨副本 lock：通过 Redis SETNX 实现（v1 简化：可不做，让多副本各自尝试刷新；
    /// 多个副本同时拿到 refresh_token 调上游通常是允许的）。
    /// 见未决问题 #1。
    refresh_lock_provider: Arc<dyn RefreshLockProvider>,
    health_store: Arc<KeyHealthStore>,
}

pub struct KeyOauthState {
    /// 当前 access_token（明文；保护参 ADR-017 加密策略）
    pub access_token: ArcSwap<SecretString>,
    /// refresh_token（明文，同上）
    pub refresh_token: ArcSwap<SecretString>,
    /// access_token 过期时间（UNIX millis）
    pub expires_at_ms: AtomicU64,
    /// 上次刷新尝试时间（用于退避）
    pub last_refresh_attempt_ms: AtomicU64,
    /// 是否处于刷新中（CAS lock；防止单进程多线程同时刷新）
    pub refreshing: AtomicBool,
}
```

`ArcSwap<SecretString>` 让"刷新写入 access_token + 读 access_token"并发安全且热路径无锁。

### ADR-KC-3 刷新流程

```rust
impl OauthCache {
    /// 请求路径上调用：拿到 access_token；过期时阻塞等刷新完。
    /// 这是 adapter::rewrite_auth 的核心调用点。
    pub async fn get_access_token(
        &self,
        key: &VendorKey,
        refresher: &dyn OAuthRefresher,
    ) -> Result<SecretString, OauthError> {
        let state = self.get_or_load(key).await?;
        let now = now_ms();

        // 1. 未过期 → 直接返回
        if state.expires_at_ms.load(Ordering::Acquire) > now + 60_000 {  // 60s 安全余量
            return Ok(state.access_token.load_full().as_ref().clone());
        }

        // 2. 过期：CAS 抢刷新槽位
        if state
            .refreshing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // 我们抢到了，执行刷新
            let result = self.do_refresh(&state, refresher, key).await;
            state.refreshing.store(false, Ordering::Release);
            result
        } else {
            // 别的 task 在刷新；等它完成
            self.wait_for_refresh(&state).await
        }
    }
}
```

`OAuthRefresher` trait 由 vendor adapter 实现：

```rust
#[async_trait]
pub trait OAuthRefresher: Send + Sync {
    /// 调上游刷新 token。返回新 access_token + 可能更新的 refresh_token + 新 expires_at。
    /// 失败时返回 RefreshError，K-C 据此决定走 OAuthRefreshFailed cooldown 还是
    /// AccountBlocked cooldown。
    async fn refresh(&self, current_refresh_token: &SecretString)
        -> Result<RefreshedToken, RefreshError>;
}

pub struct RefreshedToken {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,  // 可能旋转
    pub expires_in_secs: u32,
}

pub enum RefreshError {
    /// refresh_token 也无效（HTTP 4xx）→ 标记账号 block
    AccountBlocked { detail: String },
    /// 上游瞬时（5xx / 网络）→ 短退避后重试
    Transient { detail: String },
    /// 配置错误（如缺 refresh_token）→ 标记 RefreshFailed
    Permanent { detail: String },
}
```

### ADR-KC-4 失败标记与 K-A 集成

`do_refresh` 内部成功 / 失败的副作用：

```rust
async fn do_refresh(&self, state: &KeyOauthState, refresher: &dyn OAuthRefresher, key: &VendorKey)
    -> Result<SecretString, OauthError>
{
    state.last_refresh_attempt_ms.store(now_ms(), Ordering::Release);
    let current_refresh = state.refresh_token.load_full().as_ref().clone();

    match refresher.refresh(&current_refresh).await {
        Ok(refreshed) => {
            // 1. 更新 cache
            state.access_token.store(Arc::new(refreshed.access_token.clone()));
            if let Some(new_refresh) = refreshed.refresh_token {
                state.refresh_token.store(Arc::new(new_refresh));
            }
            state.expires_at_ms.store(
                now_ms() + (refreshed.expires_in_secs as u64) * 1000,
                Ordering::Release,
            );
            // 2. 通知 K-A：清除任何 OAuthRefreshing/OAuthRefreshFailed cooldown
            self.health_store.clear_cooldown(&key.key_id, /* all formats */ );
            // 3. 持久化到 Redis（参 ADR-KC-5）
            self.persist_to_store(&key.key_id, &state).await?;
            Ok(refreshed.access_token)
        }
        Err(RefreshError::AccountBlocked { detail }) => {
            // 账号死了，1h 冷却 + 写 reason 到 entity
            for fmt in all_api_formats() {
                self.health_store.apply_cooldown(&key.key_id, fmt, CooldownDirective {
                    reason: CooldownReason::AccountBlocked,
                    ttl_secs: 3600,
                    reset_consecutive: false,
                });
            }
            self.record_block_reason(&key.key_id, &detail).await?;
            Err(OauthError::AccountBlocked(detail))
        }
        Err(RefreshError::Transient { detail }) => {
            // 短退避 30s
            for fmt in all_api_formats() {
                self.health_store.apply_cooldown(&key.key_id, fmt, CooldownDirective {
                    reason: CooldownReason::OAuthRefreshFailed,
                    ttl_secs: 30,
                    reset_consecutive: false,
                });
            }
            Err(OauthError::Transient(detail))
        }
        Err(RefreshError::Permanent { detail }) => {
            // 配置错误，长冷却 + 标记
            for fmt in all_api_formats() {
                self.health_store.apply_cooldown(&key.key_id, fmt, CooldownDirective {
                    reason: CooldownReason::OAuthRefreshFailed,
                    ttl_secs: 1800,
                    reset_consecutive: false,
                });
            }
            self.record_block_reason(&key.key_id, &detail).await?;
            Err(OauthError::Permanent(detail))
        }
    }
}
```

**关键边界**：cooldown 实际写在 K-A；K-C 只是 "调用方"。reason 字段直接复用 K-A 的 enum（参 K-A ADR-KA-2）。reason 前缀（Aether 用的 `[ACCOUNT_BLOCK]` 等）改为 K-A `CooldownReason` enum variant 的语义化命名，不再用字符串前缀。

### ADR-KC-5 持久化

OAuth token **必须**持久化（不能丢，丢了 = 用户重新授权）：

```
Redis entity: entity:vendor_key:<key_id>
  fields (在 VendorKey 之外，OAuth-specific 字段):
    oauth_access_token_enc    # ADR-017 加密后的 access_token
    oauth_refresh_token_enc   # 同上
    oauth_expires_at_ms
    oauth_last_refresh_ms
    oauth_block_reason        # 文字描述（可选，运维 dashboard 用）
    v                         # ADR-013 版本号
```

进程启动时 lazy load（不预热）；写入时通过 ADR-013 Entity 协议事件通知其他副本失效缓存。

加密：复用 `gw-store::encrypt::SecretCipher`（KMS-backed AES-256-GCM）。

### ADR-KC-6 跨副本 refresh 协调

v1 简化：**不做跨副本 lock**。多个副本同时收到 401 → 各自调上游刷新——大多数 OAuth 服务允许旋转，refresh_token 可能轮替；先到先得，后到的拿到失效的 refresh_token 标记 Transient/Permanent。

代价：

- 偶尔会有"刚刷新完又被旋转"的多余请求；
- refresh_token 旋转的 vendor（如 Google）下，后到的副本拿失败 → 喂 K-A OAuthRefreshFailed → 自动 30s 后再试 → 此时另一副本 v8 写入 Redis 已传播过来 → 用新 token；
- 边缘情况：在 entity 传播窗口内多个副本互相冲掉对方的 refresh_token。这风险与"OAuth 服务允许并发刷新"的协议假设共生，Aether 也是同款。

后续 plan（K-C v2）可以加 SETNX-based lock，但 v1 不引入。

### ADR-KC-7 请求路径接入

```rust
// 伪代码：gw-adapter::claude_code adapter rewrite_auth
impl UpstreamAdapter for ClaudeCodeAdapter {
    async fn rewrite_auth(&self, ctx: &mut AdapterCtx<'_>) -> Result<(), AdapterError> {
        let key = ctx.vendor_key();
        let token = self.oauth_cache.get_access_token(key, &self.refresher).await?;
        ctx.set_header("Authorization", format!("Bearer {}", token.expose()));
        Ok(())
    }
}
```

每次请求都调 `get_access_token`，但热路径只有未过期检查（< 50ns）；过期才进刷新分支。

### ADR-KC-8 K-B → K-C 信号通路

K-B 在 `analyze` 返回 `OAuthSignal::TokenExpired` / `RefreshFailed` / `AccountBlocked`，proxy 转给 K-C：

```rust
impl OauthCache {
    pub fn handle_signal(&self, key: &KeyId, signal: OAuthSignal) {
        match signal {
            OAuthSignal::TokenExpired => {
                // 标记过期；下次 get_access_token 会触发刷新
                if let Some(state) = self.by_key.get(key) {
                    state.expires_at_ms.store(0, Ordering::Release);
                }
            }
            OAuthSignal::RefreshFailed => {
                // 一般来说 K-C 自己在刷新流程已经标记 cooldown 了；这条来自上游 401 但未走 K-C 刷新通路（罕见）
                self.health_store.apply_cooldown(key, /* ... */);
            }
            OAuthSignal::AccountBlocked => {
                // 同上
            }
        }
    }
}
```

### ADR-KC-9 性能预算

| 操作 | 预算 |
|---|---|
| `get_access_token` 命中未过期 | < 100 ns（一次 atomic load + ArcSwap load_full） |
| `get_access_token` 触发刷新（cold path） | 取决于上游 OAuth endpoint，期望 < 500 ms |
| `handle_signal(TokenExpired)` | < 50 ns |

### ADR-KC-10 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_oauth_refresh_total` | vendor, result | counter；result = ok / account_blocked / transient / permanent |
| `gw_oauth_refresh_duration_ms` | vendor | histogram |
| `gw_oauth_cache_hit_total` | vendor, result | counter；result = hit / miss / expired |
| `gw_oauth_concurrent_refresh_blocked_total` | vendor | counter；CAS 失败被阻塞等待的次数 |

## 3. 实施大纲

依赖：K-A land；K-B trait 定义可用；adapter spec ADR-A1 落地（Registry + AdapterCtx）。

1. 新建 `gw-adapter::oauth` 模块（`cache.rs` / `refresher.rs` / `errors.rs`）；
2. 实施 `OauthCache` + `KeyOauthState` + `OAuthRefresher` trait + `RefreshError` 枚举；
3. 实施 `get_access_token` 的 CAS lock 流程 + `wait_for_refresh` 通知；
4. 持久化集成：复用 `gw-store::entity` 写 `entity:vendor_key` 上的 OAuth 字段；
5. K-A 集成：刷新成功 → `clear_cooldown`；失败 → `apply_cooldown`；
6. K-B 信号通路 `handle_signal`；
7. 单测：
   - 单进程多线程：5 个 task 同时请求过期 token → 只有 1 个真正调 refresher；
   - 刷新成功 → access_token 更新 + cooldown 清；
   - AccountBlocked → cooldown 1h + entity reason 写入；
   - Transient → cooldown 30s + 下次自动重试；
   - 持久化 round-trip。

具体 vendor refresher 实现（claude_code / gemini_cli / vertex_ai）作为后续 plan。

## 4. 未决问题

1. **跨副本 refresh lock 是否需要**：v1 决议不做（接受多副本同时刷新的代价）。若发现 refresh_token 旋转 vendor 下错误率高，下个版本加 Redis SETNX-based lock（key=`lock:oauth_refresh:<key_id>`, TTL=10s）。
2. **OAuth refresh_token 自身过期了怎么办**：vendor 上游会返回 RefreshError::AccountBlocked 或 Permanent。K-C 标长冷却 + entity reason；管理面（Go 端）通过 dashboard 看到 → 通知用户重新授权 → 写入新 refresh_token → 触发 entity 失效事件 → 网关下次访问读到新 token。这条流程涉及 Go，本 spec 仅承担"标记 + 持久化"。
3. **token 加密的 KEK rotation 怎么影响 OauthCache**：复用 `gw-store::encrypt::SecretCipher` 的 KEK rotation 路径（ADR-017）。OAuth token 与 API key 一起被处理，无额外工作。
4. **expires_at 的安全边界**：v1 取 60s 余量（提前 60s 视为过期）。某些 vendor 文档不明确 expires_in 精度，可能造成"边界过期请求 401"。倾向加可配置 `oauth_safety_window_secs`，默认 60，按 vendor 调整。
5. **多 api_format 的 cooldown 一致性**：当前 K-C 在刷新失败时给"所有 api_format"统一 apply_cooldown。这是简化——OAuth token 一般是账号级，跨 api_format 共享。但若某 vendor token 只授权某个 api_format，需要 K-C 接受 api_format 列表参数。倾向 v1 一刀切，后续按需细化。
6. **OauthCache 与 quota_probe（K-D）的协作**：K-D 后台 probe 调上游需要 access_token——它必须通过 K-C 的 `get_access_token` 拿到，不直接读 entity（避免 race）。K-D spec 显式说明这一点。
7. **错误响应里的 retry-after vs 内部 refresh 退避**：上游 OAuth endpoint 也可能 429。K-C 内部要不要自己处理 OAuth endpoint 的 429？v1 倾向：上游 OAuth endpoint 429 视为 Transient → K-C 短退避 30s，**不**触发 K-A cooldown（K-A 关心的是 vendor key 健康，不是 OAuth endpoint 健康）。这与 K-B 路径不同（K-B 关心业务请求 429）。

---

> K-C 把 OAuth refresh 提到 Rust adapter；K-A 是它的可见性出口。vendor 具体 refresher（claude_code endpoint URL 与 body 格式、gemini_cli SA JWT 签名等）各自后续 plan。
