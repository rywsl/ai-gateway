# RL-IP — IP 限流 + IP allowlist（v2026-05-19）

- **日期**：2026-05-19
- **状态**：Draft（待审阅，仅契约与边界）
- **作者**：协作设计（Claude + 用户）
- **关联**：
  - [2026-05-19-gap-inventory.md](./2026-05-19-gap-inventory.md)（桶 5 R3 / R10 + 桶 15 SE1）
  - 现状参考：`crates/gw-auth/src/ip_filter.rs`（blocklist 已实现）
  - Aether 来源：`services/rate_limit/ip_limiter.py` + 类似 access_restrictions
- **范围**：
  - **IP 限流**（per-IP governor）：同 IP 每分钟最多 N 请求，DoS 防护；
  - **IP allowlist**（反向白名单）：与 blocklist 同框架；scope-aware（Global / User / ApiKey）；
  - **trust_xff 行为**：沿用现状（默认 false，使用 L4 socket peer；显式 true 时读 XFF）；
  - 与 K-A / R-E 不交叉（IP 限流是入口层防护，与 vendor key 无关）。
- **不在范围**：
  - CIDR 高效查找算法升级（Patricia trie，留 follow-up）；
  - geo-IP 屏蔽（独立 spec）；
  - DDoS 应用层防护（应该在 LB/CDN 层）；
  - 客户端身份限制（gw-auth::apikey 已做）。

> RL-IP 是入口层安全控件。与 RL-Core 并行实施。

---

## 1. 背景

`gw-auth::ip_filter::check_blocklist` 已经实现 scope-aware blocklist：

- IpBlockScope::Global：所有请求都查；
- IpBlockScope::User(uid)：仅该用户请求查；
- IpBlockScope::ApiKey(aid)：仅该 ApiKey 请求查；

但**缺**两个对称功能：

1. **IP 限流**：单 IP 高频请求 → 限速 → 拒服。这是 DoS 防护核心；
2. **IP allowlist**：与 blocklist 镜像；某些场景（企业部署）只允许办公网 IP 访问。

CLAUDE.md 已经登记 IP blocklist 与 trust_xff 的语义，本 spec 在此之上扩展。

## 2. 设计决策

### ADR-RLIP-1 IP 限流模块

```rust
// crates/gw-auth/src/ip_ratelimit.rs

pub struct IpRateLimitGuard {
    /// per-IP governor（与 RL-Core::RateLimitGuard 平级，但单独表）
    per_ip: DashMap<IpAddr, Arc<DefaultDirectRateLimiter>>,
    /// 全局每 IP 上限（默认 600 RPM = 10 RPS）
    global_per_ip_rpm: NonZeroU32,
    /// 内存上限：超过这个数的 IP entries 触发 LRU 驱逐（防内存爆）
    max_entries: usize,
}

impl IpRateLimitGuard {
    pub fn new(global_per_ip_rpm: u32) -> Self {
        Self {
            per_ip: DashMap::new(),
            global_per_ip_rpm: NonZeroU32::new(global_per_ip_rpm.max(1)).unwrap(),
            max_entries: 100_000,
        }
    }

    /// 在 early_request_filter 调用（client IP 解析后）。
    /// 失败返回 ErrorIR 429 with GW_QUOTA_IP_RPM_EXCEEDED。
    pub fn check(&self, ip: IpAddr) -> Result<(), ErrorIR<'static>> {
        let lim = self.per_ip
            .entry(ip)
            .or_insert_with(|| Arc::new(RateLimiter::direct(Quota::per_minute(self.global_per_ip_rpm))))
            .clone();
        if lim.check().is_err() {
            return Err(ip_rpm_err(ip));
        }
        Ok(())
    }

    /// 后台 LRU 驱逐（gw-gateway 启动 sweep task 调用）
    pub fn sweep_idle(&self) -> u32 {
        // DashMap 不支持 LRU；用 alternative：周期清空所有 entry（governor token 已耗尽的 IP 重新建即可）
        // 或用 moka::Cache 替代 DashMap（与 R-G Sticky 同款）。
        //
        // v1 简化：超 max_entries 时 truncate 所有 entries（清表重来）；
        // 代价是合法用户 token 桶被重置，但 governor refill 是 RPM/60s 频率，
        // 60s 内即可恢复。
        if self.per_ip.len() > self.max_entries {
            self.per_ip.clear();
            self.per_ip.len() as u32
        } else {
            0
        }
    }
}

fn ip_rpm_err(ip: IpAddr) -> ErrorIR<'static> {
    ErrorIR {
        category: ErrorCategory::ClientQuota,
        status: 429,
        upstream_status: None,
        code: Cow::Borrowed(codes::GW_QUOTA_IP_RPM_EXCEEDED),
        message: Cow::Owned(format!("ip {} exceeded rpm limit", ip)),
        retryable: false,
        upstream_raw: None,
        request_id: "",
        retry_after_secs: Some(60),
    }
}
```

新增错误码 `GW_QUOTA_IP_RPM_EXCEEDED` (429)。

### ADR-RLIP-2 IP allowlist

现 `IpBlockScope` enum 沿用，但加一个 `IpBlockMode` 区分黑/白名单：

```rust
// gw-core::entity::IpBlockEntry（扩字段）

pub struct IpBlockEntry {
    pub cidr: String,
    pub scope: IpBlockScope,           // 已有：Global / User / ApiKey
    pub mode: IpBlockMode,             // **新加**
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum IpBlockMode {
    Deny,   // blocklist 语义（已有）
    Allow,  // allowlist 语义（新）
}
```

新增 `IpAllowlistMode` 全局开关：

```rust
pub enum IpAllowlistMode {
    /// 关闭 allowlist 检查（默认）
    Off,
    /// 启用 allowlist：未在 allowlist 内的 IP 拒绝
    Enforce,
}
```

由 vendor entity 或全局 config 提供。

### ADR-RLIP-3 ip_filter 改造

```rust
// crates/gw-auth/src/ip_filter.rs（重写）

pub struct IpFilterCfg {
    pub allowlist_mode: IpAllowlistMode,
}

pub async fn check_ip(
    ip: IpAddr,
    user_id: Option<&UserId>,
    api_key_id: Option<&ApiKeyId>,
    store: &EntityStore,
    cfg: &IpFilterCfg,
) -> Result<(), ErrorIR<'static>> {
    let snapshot = store.ipblock_snapshot();

    // 1. 任意 scope 的 Deny 命中 → 拒绝
    for (_, entry) in &snapshot {
        if !scope_matches(&entry.value.scope, user_id, api_key_id) {
            continue;
        }
        if entry.value.mode != IpBlockMode::Deny {
            continue;
        }
        let Ok(net) = IpNet::from_str(&entry.value.cidr) else { continue; };
        if net.contains(&ip) {
            return Err(blocked_err(ip));
        }
    }

    // 2. allowlist 开启时，未命中任何 Allow → 拒绝
    if cfg.allowlist_mode == IpAllowlistMode::Enforce {
        let mut any_allow = false;
        let mut allow_hit = false;
        for (_, entry) in &snapshot {
            if !scope_matches(&entry.value.scope, user_id, api_key_id) {
                continue;
            }
            if entry.value.mode != IpBlockMode::Allow {
                continue;
            }
            any_allow = true;
            let Ok(net) = IpNet::from_str(&entry.value.cidr) else { continue; };
            if net.contains(&ip) {
                allow_hit = true;
                break;
            }
        }
        if any_allow && !allow_hit {
            return Err(not_in_allowlist_err(ip));
        }
        // 没有任何 Allow 条目 = allowlist 未配置 → 不拒
        // 这种情况下应该报警（"allowlist enforce 但表空"是误配）
    }

    Ok(())
}

fn scope_matches(scope: &IpBlockScope, uid: Option<&UserId>, aid: Option<&ApiKeyId>) -> bool {
    match scope {
        IpBlockScope::Global => true,
        IpBlockScope::User(u) => uid.map_or(false, |x| x == u),
        IpBlockScope::ApiKey(a) => aid.map_or(false, |x| x == a),
    }
}
```

新错误码 `GW_AUTH_IP_NOT_IN_ALLOWLIST` (403)。

### ADR-RLIP-4 trust_xff 沿用

CLAUDE.md 已登记：

> `trust_xff` defaults to `false`. With it off, the IP blocklist matches the L4 socket peer (which behind an L7 LB is the LB's IP, not the client). Only enable it behind a trusted L7 LB that strips client-supplied XFF headers, otherwise the blocklist is trivially bypassable.

RL-IP 全部三个能力（IP 限流 + blocklist + allowlist）共用同一个 `client_ip_from_session` 解析逻辑（参 `gw-proxy::filters::early::client_ip_from_session`）。不改 trust_xff 行为。

### ADR-RLIP-5 接入顺序（gw-proxy::early）

```rust
async fn early_request_filter(&self, ctx: &mut ReqCtx, session: &Session) {
    let ip = client_ip_from_session(session, self.config.trust_xff);
    ctx.client_ip = Some(ip);

    // 1. IP blocklist（最严格，可短路）
    // 2. allowlist（若启用）
    if let Err(e) = check_ip(ip, None, None, &self.store, &self.ip_filter_cfg).await {
        // user_id/api_key_id 此时还没解析；仅 Global scope 生效
        return Err(emit_error(e));
    }

    // 3. IP 限流
    self.ip_rate_limit_guard.check(ip)?;

    // 后续：auth → 解出 user_id/api_key_id → 再 check_ip 一次（User/ApiKey scope）
    ...
}
```

注意：IP blocklist/allowlist 在 early 阶段（无 user_id）只查 Global scope；user_id 解出后还需要查 User/ApiKey scope 一次。这是当前 ip_filter 行为，本 spec 沿用。

### ADR-RLIP-6 配置

```toml
# config/default.toml 新增
[ip_filter]
allowlist_mode = "off"  # "off" | "enforce"

[ip_ratelimit]
global_per_ip_rpm = 600
max_entries = 100_000
sweep_interval_secs = 60
```

环境变量 `GW_IP_FILTER__ALLOWLIST_MODE` / `GW_IP_RATELIMIT__GLOBAL_PER_IP_RPM` 覆盖。

### ADR-RLIP-7 性能预算

| 操作 | 预算 |
|---|---|
| `check_ip` blocklist O(N) | < 50 µs（N=100 时） |
| `check_ip` allowlist O(N) | 同上 |
| `IpRateLimitGuard::check` | < 100 ns（governor 直接） |
| sweep（清表 100k entries） | < 5 ms（DashMap clear）|

热路径：blocklist + ratelimit（每请求）。100 IP entries 50 µs 可接受；1000+ 时需 Patricia trie 优化（follow-up）。

### ADR-RLIP-8 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_ip_blocklist_hit_total` | scope | counter：被 block 次数 |
| `gw_ip_allowlist_check_total` | result | counter：allowlist 通过 / 拒绝 |
| `gw_ip_ratelimit_check_total` | result | counter：通过 / 拒绝 |
| `gw_ip_ratelimit_entries_active` | - | gauge：当前 per_ip DashMap entry 数 |
| `gw_ip_ratelimit_sweep_total` | - | counter：sweep 触发次数 |
| `gw_ip_blocklist_entries` | - | gauge：blocklist 配置项数 |
| `gw_ip_allowlist_entries` | - | gauge：allowlist 配置项数 |

### ADR-RLIP-9 alerting

- allowlist_mode=Enforce 且 Allow 条目数 = 0 → critical alert（误配，所有合法请求都被拒）；
- 单 IP 触发限速次数 > 100/min → warning（可能 DDoS）；
- IpRateLimitGuard entries > 80% max_entries → warning（大量 IP 涌入，可能 reflection 攻击）。

### ADR-RLIP-10 测试

- IP 在 blocklist Global → 拒绝；
- IP 在 blocklist User(u1) + 请求带 u1 → 拒绝；
- IP 在 blocklist User(u1) + 请求带 u2 → 通过；
- allowlist_mode=Enforce + IP 不在 allowlist → 拒绝；
- allowlist_mode=Enforce + 表空 → 通过 + alert；
- allowlist_mode=Off + IP 不在 allowlist → 通过；
- IP 限速触发 → 429 GW_QUOTA_IP_RPM_EXCEEDED；
- 60s 后 token 桶恢复 → 通过；
- sweep 清表后 RPM 限制重置（合理行为）。

## 3. 实施大纲

依赖：无（与 RL-Core / RL-Adaptive 并行）。

1. 新建 `crates/gw-auth/src/ip_ratelimit.rs`（IpRateLimitGuard）；
2. `gw-core::entity::IpBlockEntry` 加 mode 字段；
3. `gw-core::entity::IpAllowlistMode` 全局枚举；
4. `crates/gw-auth/src/ip_filter.rs` 改写为 check_ip（含 allowlist 分支）；
5. `gw-gateway::config::Config` 加 ip_filter + ip_ratelimit 配置；
6. `gw-proxy::filters::early` 接入 IP 限流；
7. EntityStore loader 兼容旧 IpBlockEntry schema（mode 默认 Deny）；
8. metrics + 测试。

## 4. 未决问题

1. **IP 限流的 DashMap vs moka**：DashMap clear 是简单但暴力；moka LRU 更精细但内存更高。v1 用 DashMap + 周期 truncate，监控 entries 决定升级。
2. **IPv6 限流细粒度**：IPv6 通常 /64 整段属于同一用户（家庭网络），按完整 128bit 分桶不合理。倾向 v1 IPv4 按完整 32bit，IPv6 按 /64 前缀分桶——但实施复杂，v1 简化为完整地址，监控误判。
3. **trust_xff 与 IP 限流**：trust_xff=true 时 client_ip 来自 XFF 头（可伪造）→ 攻击者发不同 XFF 就绕过限速。这是 trust_xff 本身的安全前提（只在可信 LB 后启用），不是 IP 限流的责任。
4. **CIDR 解析性能**：当前每次请求 for-loop parse IpNet。可以在 entity 加载阶段 pre-parse 缓存 IpNet 对象。倾向 v1 接受 parse 开销（< 200 ns/CIDR），加 entity preprocessing 推 follow-up。
5. **allowlist 与 user/api_key scope 嵌套**：allowlist 表空表示"该 scope 无 allowlist 配置"——这等价于"不限制"还是"该 scope 全拒绝"？倾向"无配置 = 通过"（即 allowlist 仅当至少一条 Allow 时生效）；这与 Aether 一致。
6. **CIDR 配置变更生效延迟**：EntityStore 通过 events:entity 失效；新 entry 几十 ms 内生效。攻击场景下（管理员发现攻击者 IP → 立刻 blocklist）几十 ms 足够。
7. **CIDR 数量上限**：blocklist + allowlist 共 1000 条还能 O(N) 接受；10000 条性能严重劣化。entity 加 max entries 上限（默认 5000）+ alert。
8. **IP 限流配置 per-user override**：某些大客户(B2B)需要更高 IP RPM。倾向 v1 不支持（全局统一），后续按用户分级 follow-up。

---

> RL-IP 是入口层安全控件，与 RL-Core / RL-Adaptive 完全独立。完成后 IP 维度防护齐全。
