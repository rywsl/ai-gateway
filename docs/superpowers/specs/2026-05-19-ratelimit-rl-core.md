# RL-Core — Per-User / Per-Key RPM + 429 分类（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 5 R1/R2/R4/R10）
  - [2026-05-19-routing-r-e-failover.md](./2026-05-19-routing-r-e-failover.md)（per-key RPM 失败时切下一 key）
  - [2026-05-19-key-health-k-b-vendor-error-hook.md](./2026-05-19-key-health-k-b-vendor-error-hook.md)（K-B `ErrorOutcome` 加 RateLimitKind 字段）
  - [2026-05-19-ratelimit-rl-adaptive.md](./2026-05-19-ratelimit-rl-adaptive.md)（RL-Adaptive 消费 RateLimitKind）
- **范围**：
  - 在 `gw-auth::ratelimit` 上加 **per-vendor-key RPM** 闸门（当前只有 per-user）；
  - 新增 **429 分类语义层** `RateLimitKind` 枚举 + 各 vendor analyzer 解析 429 头部归一化；
  - K-B `ErrorOutcome` 加 `rate_limit_kind: Option<RateLimitKind>` 字段，供 RL-Adaptive / detailed_log 消费；
  - per-key RPM 失败的行为：**触发 R-E 切下一 key**，不计 K-A 失败样本（不是 key 的问题，是用户配置/上游限速）。
- **不在范围**：
  - AIMD 自适应学习（→ RL-Adaptive，本 spec 仅暴露 RateLimitKind 输入）；
  - IP 限流（→ RL-IP）；
  - TPM（v1 不做）；
  - daily/monthly quota（**归 Go**，Rust 不实现）。

> RL-Core 是桶 5 的入口 spec；RPM 限速形态完整 + 429 通用语义统一。RL-Adaptive 与 RL-IP 在此之上扩展。

---

## 1. 背景

`gw-auth::ratelimit::RateLimitGuard` 当前只做 per-user RPM（`per_user: DashMap<UserId, Arc<DefaultDirectRateLimiter>>`）。但实际场景：

- 某 vendor_key 限速 60 RPM（vendor 上游硬上限），多个用户共用该 key → 单 user 不超 limit 但全体超 → 上游 429 风暴；
- VendorKey.rpm_limit 字段在 schema 里已存在但 routing 完全不用；
- 429 现状由 K-B `analyze` 处理但不区分 RPM/TPM/Daily/Monthly/Concurrent 五种成因，所有 429 同等 cooldown 30s。

Aether `rate_limit/detector.py` 把 429 分类后供 adaptive_rpm 学习。Rust 端需要等价分类，并把 per-key RPM 闸门补上。

## 2. 设计决策

### ADR-RLC-1 RateLimitKind 枚举

```rust
// crates/gw-auth/src/ratelimit/kind.rs

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RateLimitKind {
    /// 每分钟请求数限制
    Rpm,
    /// 每分钟 token 数限制（部分 vendor 显式区分）
    Tpm,
    /// 每日累计限制
    Daily,
    /// 每月累计限制
    Monthly,
    /// 并发限制
    Concurrent,
    /// 上游未明示，按通用 429 处理
    Unknown,
}

#[derive(Clone, Debug)]
pub struct RateLimitInfo {
    pub kind: RateLimitKind,
    pub retry_after_secs: Option<u32>,
    /// 上游声明的限制值（如 60 = 60 RPM）；解析 X-RateLimit-Limit-* 头
    pub limit_value: Option<u32>,
    /// 上游声明的剩余值
    pub remaining: Option<u32>,
    /// reset 时间（UNIX ms）
    pub reset_at_ms: Option<u64>,
}
```

### ADR-RLC-2 vendor analyzer 解析 429 头部

K-B `UpstreamErrorAnalyzer::analyze` 输入是 raw headers + body_preview。RL-Core 提供共享 helper：

```rust
// crates/gw-auth/src/ratelimit/detector.rs

/// 从 429 响应头解析 RateLimitInfo。
/// 各 vendor 头部格式不同：
///   - OpenAI:  x-ratelimit-limit-requests / x-ratelimit-remaining-requests / x-ratelimit-reset-requests
///              x-ratelimit-limit-tokens / x-ratelimit-remaining-tokens / x-ratelimit-reset-tokens
///   - Anthropic: anthropic-ratelimit-requests-* / anthropic-ratelimit-tokens-* / anthropic-ratelimit-input-tokens-*
///   - Gemini: 通常 status=PERMISSION_DENIED + body "quota exceeded"
///   - Codex/Vertex/Kiro: vendor 私有头
pub fn detect_rate_limit_kind(
    status: u16,
    headers: &[(String, String)],
    body_preview: &[u8],
    vendor_id: &VendorId,
) -> RateLimitInfo {
    if status != 429 && !(status == 403 && body_contains_quota(body_preview)) {
        return RateLimitInfo {
            kind: RateLimitKind::Unknown,
            retry_after_secs: None,
            limit_value: None,
            remaining: None,
            reset_at_ms: None,
        };
    }

    // 1. 解析 vendor 特定头
    if let Some(info) = vendor_specific_detect(vendor_id, headers) {
        return info;
    }

    // 2. 解析通用头（OpenAI 风格）
    if let Some(info) = openai_style_detect(headers) {
        return info;
    }

    // 3. 解析 retry-after 头
    let retry_after = parse_retry_after(headers);

    // 4. 解析 body 关键词（"daily" / "monthly" / "concurrent"）
    let kind_from_body = detect_kind_from_body(body_preview);

    RateLimitInfo {
        kind: kind_from_body.unwrap_or(RateLimitKind::Unknown),
        retry_after_secs: retry_after,
        limit_value: None,
        remaining: None,
        reset_at_ms: None,
    }
}

fn vendor_specific_detect(vendor: &VendorId, headers: &[(String, String)]) -> Option<RateLimitInfo> {
    match vendor.0.as_str() {
        "anthropic" | "claude_code" => anthropic_detect(headers),
        "openai" | "codex" => openai_style_detect(headers),
        "gemini" | "gemini_cli" | "vertex_ai" => gemini_detect(headers),
        _ => None,
    }
}

fn anthropic_detect(headers: &[(String, String)]) -> Option<RateLimitInfo> {
    // anthropic-ratelimit-requests-limit / remaining / reset
    // anthropic-ratelimit-tokens-limit / remaining / reset
    // 优先 tokens（TPM 紧时通常 token 先耗尽）
    let token_limit = parse_header_u32(headers, "anthropic-ratelimit-tokens-limit");
    let token_remaining = parse_header_u32(headers, "anthropic-ratelimit-tokens-remaining");
    let token_reset = parse_header_rfc3339(headers, "anthropic-ratelimit-tokens-reset");

    if let (Some(limit), Some(remaining)) = (token_limit, token_remaining) {
        if remaining < limit / 10 {  // 余量 < 10% 认为是 TPM 触发
            return Some(RateLimitInfo {
                kind: RateLimitKind::Tpm,
                retry_after_secs: token_reset.map(|t| (t - now_ms()) / 1000) as Option<u32>,
                limit_value: Some(limit),
                remaining: Some(remaining),
                reset_at_ms: token_reset,
            });
        }
    }

    let req_limit = parse_header_u32(headers, "anthropic-ratelimit-requests-limit");
    let req_remaining = parse_header_u32(headers, "anthropic-ratelimit-requests-remaining");
    if let (Some(limit), Some(remaining)) = (req_limit, req_remaining) {
        return Some(RateLimitInfo {
            kind: RateLimitKind::Rpm,
            retry_after_secs: parse_retry_after(headers),
            limit_value: Some(limit),
            remaining: Some(remaining),
            reset_at_ms: None,
        });
    }
    None
}

fn openai_style_detect(headers: &[(String, String)]) -> Option<RateLimitInfo> {
    // x-ratelimit-* 同款逻辑
    ...
}

fn gemini_detect(headers: &[(String, String)]) -> Option<RateLimitInfo> {
    // Gemini 不发标准 ratelimit 头；通常 status=403 + body
    None
}

fn detect_kind_from_body(body: &[u8]) -> Option<RateLimitKind> {
    let b = std::str::from_utf8(body).unwrap_or("").to_ascii_lowercase();
    if b.contains("daily limit") || b.contains("daily quota") {
        Some(RateLimitKind::Daily)
    } else if b.contains("monthly limit") || b.contains("monthly quota") {
        Some(RateLimitKind::Monthly)
    } else if b.contains("concurrent") || b.contains("too many concurrent") {
        Some(RateLimitKind::Concurrent)
    } else if b.contains("rpm") || b.contains("requests per minute") {
        Some(RateLimitKind::Rpm)
    } else if b.contains("tpm") || b.contains("tokens per minute") {
        Some(RateLimitKind::Tpm)
    } else {
        None
    }
}
```

### ADR-RLC-3 K-B `ErrorOutcome` 扩字段

```rust
// crates/gw-adapter/src/error_analyzer.rs（K-B 同 PR 改）

pub struct ErrorOutcome {
    pub cooldown: Option<CooldownDirective>,
    pub oauth_signal: Option<OAuthSignal>,
    pub count_as_failure: bool,
    /// **新增**：429 分类（v1 用 RateLimitKind）
    pub rate_limit_info: Option<RateLimitInfo>,
}
```

`DefaultErrorAnalyzer::analyze` 在 status=429 时调 `detect_rate_limit_kind`，把结果挂到 `rate_limit_info`。vendor 特化 analyzer 可以覆写。

RL-Adaptive 在 logging filter 后读 `ctx.rate_limit_info` 做学习。

### ADR-RLC-4 RateLimitGuard 扩展 per-key RPM

```rust
// crates/gw-auth/src/ratelimit/guard.rs

pub struct RateLimitGuard {
    /// per-user RPM（已有）
    pub per_user: DashMap<UserId, Arc<DefaultDirectRateLimiter>>,
    /// per-vendor-key RPM（新加）
    pub per_key: DashMap<KeyId, Arc<KeyRateLimiter>>,
    /// per-user 并发流（已有）
    pub per_user_streams: DashMap<UserId, Arc<AtomicUsize>>,
    /// 全局并发流计数（已有）
    pub total_streams: Arc<AtomicUsize>,
}

pub struct KeyRateLimiter {
    /// 当前生效的 RPM（可能被 RL-Adaptive 动态下调）
    pub effective_rpm: AtomicU32,
    /// 配置的 RPM 上限（来自 VendorKey::rpm_limit）
    pub configured_rpm: u32,
    /// 实际限速器
    pub limiter: ArcSwap<DefaultDirectRateLimiter>,
}

impl RateLimitGuard {
    /// 用户级 check（已有）
    pub fn check_user(&self, perm: &MergedPermissions) -> Result<RateLimitTicket, ErrorIR<'static>>;

    /// 新增：per-key check。在 R-AB Selector::pick 之后、R-E failover 之前调。
    /// 失败时返回特殊错误，R-E 看到此错误**直接 SwitchKey**，不算 K-A 失败样本。
    pub fn check_key(&self, key_id: &KeyId, configured_rpm: u32)
        -> Result<KeyRateLimitTicket, ErrorIR<'static>>;
}

#[derive(Debug, Clone, Copy)]
pub struct KeyRateLimitTicket {
    pub key_id: KeyId,
    pub acquired_at_ms: u64,
}
```

### ADR-RLC-5 per-key check 集成点

`gw-proxy::filters::upstream::upstream_peer`（参 R-E ADR-RE-5）流程：

```rust
async fn upstream_peer(&self, ctx: &mut ReqCtx) -> Result<Box<HttpPeer>> {
    if ctx.failover.is_none() {
        // R-AB list + pick
        let candidates = list_candidates(...).await;
        let picks = ctx.selector.pick(&candidates, &ctx.pick_ctx);
        ctx.failover = Some(FailoverState::from_picks(picks));
    }

    let state = ctx.failover.as_mut().unwrap();
    loop {
        let key = match state.current() {
            Some(k) => k,
            None => return Err(emit(codes::GW_ROUTING_NO_FALLBACK_LEFT)),
        };

        // **新加**：per-key RPM 闸门
        match self.rate_limit_guard.check_key(&key.key_id, key.rpm_limit) {
            Ok(_) => {
                ctx.key_rate_limit_ticket = Some(_);
                return Ok(Box::new(http_peer_for(key)));
            }
            Err(_) => {
                // 该 key 本地 RPM 已超，直接切下一 key（不算 K-A 失败）
                state.advance_to_next_key();
                metrics::counter!("gw_ratelimit_key_skipped_total",
                    "key_id" => key.key_id.0.clone()).increment(1);
                continue;
            }
        }
    }
}
```

注意：per-key check 是**只增计数**（governor 内部）；不需要 RAII ticket 在请求结束时释放——governor 用 token bucket，1 token = 1 请求，自动 refill。

### ADR-RLC-6 配置驱动 effective_rpm

```rust
impl RateLimitGuard {
    /// RL-Adaptive 调用：动态下调某 key 的 RPM（学到上游真上限）
    pub fn override_key_rpm(&self, key_id: &KeyId, new_rpm: u32) {
        if let Some(limiter) = self.per_key.get(key_id) {
            limiter.effective_rpm.store(new_rpm, Ordering::Release);
            let q = Quota::per_minute(NonZeroU32::new(new_rpm.max(1)).unwrap());
            limiter.limiter.store(Arc::new(RateLimiter::direct(q)));
        }
    }

    /// VendorKey 配置变更（管理面）：重置回 configured_rpm
    pub fn reset_key_rpm(&self, key_id: &KeyId) {
        if let Some(limiter) = self.per_key.get(key_id) {
            let cfg = limiter.configured_rpm;
            limiter.effective_rpm.store(cfg, Ordering::Release);
            let q = Quota::per_minute(NonZeroU32::new(cfg.max(1)).unwrap());
            limiter.limiter.store(Arc::new(RateLimiter::direct(q)));
        }
    }
}
```

EntityStore 收到 VendorKey 失效事件时 `reset_key_rpm`，避免老 limiter 残留。

### ADR-RLC-7 IP blocklist 改进（顺带）

当前 `ip_filter::check_blocklist` O(N) 线性扫所有 ipblock entry。N 小（< 100）时无所谓；N > 1000 时慢。

倾向 v1 接受 O(N)，在 metric 监控 entry 数；超 1000 切 Patricia trie（独立 follow-up）。

### ADR-RLC-8 错误码

新增：

- `GW_QUOTA_RPM_KEY_EXCEEDED`（429，per-key RPM 触发；与 `GW_QUOTA_RPM_EXCEEDED` 用户级区分）

不动 `GW_QUOTA_RPM_EXCEEDED` / `GW_QUOTA_BALANCE_ZERO` / `GW_UPSTREAM_OVERLOADED`（已有）。

但**用户态收到的错误码**不应区分内部原因——用户不该知道"key RPM 满"还是"用户 RPM 满"，对外都是 `GW_QUOTA_RPM_EXCEEDED`。`GW_QUOTA_RPM_KEY_EXCEEDED` 仅在 detailed_log 内用做 root cause。

### ADR-RLC-9 性能预算

| 操作 | 预算 |
|---|---|
| `check_user` （已有 governor 单 check） | < 100 ns |
| `check_key`（同款） | < 100 ns |
| `detect_rate_limit_kind`（vendor 头解析） | < 5 µs |
| `override_key_rpm` | < 500 ns（governor 重建） |

per-key check 命中率（governor 直接通过）99%+；触发限速时跳到下一 key 的代价是 1 次 entity_store 查（cold path）。

### ADR-RLC-10 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_ratelimit_user_check_total` | result | counter：通过 / 拒绝 |
| `gw_ratelimit_key_check_total` | result | 同上 |
| `gw_ratelimit_key_skipped_total` | key_id | counter：per-key 限速跳到下一 key 次数 |
| `gw_ratelimit_kind_detected_total` | kind, vendor | counter：429 分类输出 |
| `gw_ratelimit_key_effective_rpm` | key_id | gauge：当前 effective_rpm（高基数注意） |

### ADR-RLC-11 测试

- per-user 限速基本 case；
- per-key 限速触发 → 跳到下一 key（mock failover state）；
- per-key 限速触发 → 不增加 K-A failure 计数（验证 health_store mock）；
- detect_rate_limit_kind：各 vendor 429 头 → 正确 RateLimitKind；
- detect_rate_limit_kind：body 关键词识别 Daily/Monthly；
- override_key_rpm 后 governor 真正按新值限速；
- VendorKey 失效事件 → reset_key_rpm。

## 3. 实施大纲

依赖：R-AB land（FailoverState）；R-E land（SwitchKey 路径）；K-B land（ErrorOutcome）。

1. 新建 `crates/gw-auth/src/ratelimit/kind.rs`（RateLimitKind / RateLimitInfo）；
2. 新建 `crates/gw-auth/src/ratelimit/detector.rs`（detect_rate_limit_kind + vendor 解析器）；
3. 扩展 `crates/gw-auth/src/ratelimit/guard.rs`（per_key + KeyRateLimiter + check_key）；
4. K-B `ErrorOutcome::rate_limit_info` 字段（同 PR）；
5. K-B `DefaultErrorAnalyzer::analyze` 在 429 时调 detect_rate_limit_kind 填字段；
6. `gw-proxy::filters::upstream::upstream_peer` 接 check_key + 跳下一 key；
7. EntityStore VendorKey 失效事件触发 reset_key_rpm；
8. metrics + 测试。

## 4. 未决问题

1. **per-key RPM 失败不算 K-A failure 是否会让坏 key 永远不被 K-A trip**：理论上不应该——per-key RPM 是本地限速（不超就不会 429）；只有真的发到上游被拒才算 K-A failure。但若 effective_rpm 配错（远低于上游真实上限），key 本地一直被跳，K-A 永远不学到真实情况——key 看起来很健康但实际选不上。倾向加 metric `gw_ratelimit_key_skip_streak`，长期被跳的 key alert。
2. **per-key RPM 与 RL-Adaptive 学到的值的协作**：RL-Adaptive 学到上游真 RPM=50，override_key_rpm 改成 50。但 VendorKey.configured_rpm 仍是 60（管理面配的）。VendorKey 重新加载时该重置到 60 还是保持 50？倾向**重置到 60**（每次 entity 变更视为人工干预，学习重启）；RL-Adaptive 会重新观察 429 学回 50。
3. **TPM 是否实现**：v1 不做（spec 范围已声明）。但 RateLimitKind::Tpm 仍然存在，仅供 RL-Adaptive 与 detailed_log 用。
4. **rate_limit_info 传到 RL-Adaptive 的路径**：K-B 在 logging path 调 analyze → 写入 ctx；RL-Adaptive 同 logging path 后续读 ctx。这要求 ctx 上有该字段——`ReqCtx::rate_limit_info: Option<RateLimitInfo>`，由 K-B 写、RL-Adaptive 读。
5. **per-key RPM 失败时是否要在响应里告诉客户端**：客户端没收到任何错误（直接走 R-E SwitchKey 切下一 key 成功了），实际是网关内部限流，对外透明。这是期望行为。
6. **multi-replica per-key RPM 是各副本独立计数**：与 K-A 同款现状，governor 是进程内。Aether 用 Redis 同步过 — Rust v1 保持进程内（与 K-A 一致），多副本场景下实际有效 RPM = N × configured_rpm。后续 v2 加 Redis 同步（用 Lua script 原子 incr+compare）；本 spec 不做。
7. **`detect_rate_limit_kind` 与 `parse_retry_after`**：A-T0 spec 已经在 K-B 路径上调过 parse_retry_after（cooldown ttl）。本 spec 的 detector 内部也调一次——重复但无害（detect 内部用的是 vendor 头优先，retry_after 是 fallback）。

---

> RL-Core 把 RPM 体系完整化（user + key 双层）+ 429 通用语义统一。RL-Adaptive 在此之上做学习，RL-IP 与本 spec 并行扩 IP 维度。
