# R-D — AffinityManager(API Key → Vendor Key 亲和)(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md)(AffinityDimension 作为 ScoringDimension 的成员)
  - Aether 来源:`src/services/scheduling/affinity_manager.py`
- **范围**：在 `gw-routing` 内新增 `AffinityManager`,记录"某 ApiKeyId 最近使用过哪个 VendorKey"的亲和关系:
  - 进程内 `DashMap<ApiKeyId, AffinityRecord>` 缓存,带 TTL;
  - 作为独立 ScoringDimension(`AffinityDimension`),让"同 user 的连续请求落同一 vendor_key"以最大化 upstream prompt cache 命中;
  - 与 CacheAffinity dimension(基于 message_hash) **正交可叠加**;
  - Vendor key 失败 / cooldown / 删除时自动失效该 vendor_key 在所有亲和关系中的绑定;
  - 可选 Redis 持久化(跨 Rust 副本)留 v2,v1 纯进程内。
- **不在范围**：
  - cache_affinity dimension(基于 message_hash,在 R-AB);
  - sticky strategy(基于 conversation_id,在 R-G);
  - upstream prompt cache 命中率统计(由 K-D quota_extractor 间接反映)。

> R-D 与 R-AB 的 CacheAffinityDimension 区别:**R-D 是用户级**(同 user 跨请求亲和),**CacheAffinity 是请求级**(同 prompt 前缀亲和)。两者可同时启用,效果叠加。

---

## 1. 背景

Aether `services/scheduling/affinity_manager.py`(已在桶 3 调研时摘录)做的事:

> 每个 API Key 使用某个 Provider+Key 后,在缓存 TTL 期内,应该继续使用同一个 Key,这样可以最大化利用提供商的 Prompt Caching 机制。

机制:

- key = api_key_id(用户的 API key 视角);
- value = (provider_id, endpoint_id, vendor_key_id, last_used_at);
- TTL 默认 30 分钟(可配,与 prompt cache TTL 对齐);
- Vendor 关闭 cache 支持 → 失效所有相关亲和;
- Vendor key 故障 → 失效该 key 的所有亲和绑定。

Rust 网关需要等效能力。设计为独立 store + 一个 ScoringDimension,与 K-A / R-C 平级。

## 2. 设计决策

### ADR-RD-1 数据结构

```rust
// crates/gw-routing/src/affinity.rs

pub struct AffinityManager {
    bindings: DashMap<ApiKeyId, AffinityRecord>,
    ttl_ms: u64,
}

#[derive(Clone)]
pub struct AffinityRecord {
    pub vendor_key_id: KeyId,
    pub bound_at_ms: u64,
    pub last_hit_ms: u64,
}
```

TTL 默认 1800_000 ms(30 分钟,与 Anthropic prompt cache 5min/1h 区间适配)。

### ADR-RD-2 接口

```rust
impl AffinityManager {
    pub fn record_hit(&self, api_key_id: &ApiKeyId, vendor_key_id: &KeyId);

    /// 查询某 api_key 的亲和绑定(若已过期返回 None)。
    pub fn get(&self, api_key_id: &ApiKeyId) -> Option<KeyId>;

    /// 失效:vendor_key 不可用时调,清掉所有指向该 vendor_key 的 binding。
    pub fn invalidate_vendor_key(&self, vendor_key_id: &KeyId);

    /// 失效:vendor 关闭 cache 支持时,清掉指向该 vendor 的所有 binding(by vendor_id)。
    /// 实施需要遍历;DashMap iter 在写并发下成本高 — 改放 v2 时再优化。
    pub fn invalidate_vendor(&self, vendor_id: &VendorId, store: &EntityStore);

    /// 周期清扫:扫所有 binding,删超 TTL 的(后台 task 调用)。
    pub fn sweep_expired(&self) -> u32;
}
```

### ADR-RD-3 record 时机

`gw-proxy::filters::logging`(请求结束)成功响应时调:

```rust
if let Some(ok) = ctx.upstream_status.filter(|s| (200..400).contains(s)) {
    if let (Some(akid), Some(vkid)) = (&ctx.api_key_id, &ctx.picked_vendor_key) {
        ctx.affinity_manager.record_hit(akid, &vkid.key_id);
    }
}
```

错误响应**不** record(避免坏 vendor_key 被锁定)。

### ADR-RD-4 失效时机

| 触发 | 调用 |
|---|---|
| K-A 给 vendor_key 写长冷却(401/402/403/AccountBlocked) | `invalidate_vendor_key(vkid)` |
| K-C 给 vendor_key 标 AccountBlocked / RefreshFailed | 同上 |
| EntityStore 收到 vendor_key 删除事件 | `invalidate_vendor_key(vkid)` |
| Vendor 配置变更 cache=disabled | `invalidate_vendor(vendor_id, store)` |
| TTL 过期 | 后台 sweep_expired() |

关键:**K-A apply_cooldown 时同步调用 AffinityManager::invalidate_vendor_key**。这条放 K-A spec follow-up,或 R-D 提供 trait hook 让 K-A 注入。倾向后者:

```rust
// crates/gw-routing/src/health_hook.rs
pub trait HealthInvalidationHook: Send + Sync {
    fn on_cooldown_applied(&self, vendor_key_id: &KeyId, reason: CooldownReason);
}

impl HealthInvalidationHook for Arc<AffinityManager> {
    fn on_cooldown_applied(&self, vendor_key_id: &KeyId, reason: CooldownReason) {
        // 长冷却的 reason 才失效亲和,瞬时 cooldown(503 30s) 保留(避免高频抖动)
        if reason.is_long_term() {
            self.invalidate_vendor_key(vendor_key_id);
        }
    }
}
```

`CooldownReason::is_long_term()` = TTL > 600s 时 true(K-A 实现端做)。

### ADR-RD-5 AffinityDimension(R-AB 配套)

```rust
// crates/gw-routing/src/dimension/affinity.rs

pub struct AffinityDimension;

impl ScoringDimension for AffinityDimension {
    fn name(&self) -> &'static str { "affinity" }
    fn mutex_group(&self) -> Option<&'static str> { Some("distribution") }

    fn score(&self, key: &VendorKey, ctx: &PickCtx<'_>) -> f32 {
        let Some(am) = ctx.affinity_manager else { return 0.5; };
        let Some(bound) = am.get(ctx.api_key_id) else { return 0.5; };  // 无绑定时中性
        if bound == key.key_id { 1.0 } else { 0.0 }
    }
}
```

注意 mutex_group = "distribution" — 与 cache_affinity / load_balance 互斥(同一组只能选一个 distribution 维度)。

业务的常见组合:

| 用户类型 | 推荐 dim 组合 |
|---|---|
| 重复 prompt 大用户(coding helper) | affinity(w=1.0) + health(1.5) + quota(0.5) |
| 一次性 prompt 多变(广告生成) | load_balance(w=1.0) + health(1.5) + cost(0.5) |
| 多媒体长任务 | quota_balanced(1.5) + health(1.5) + latency(0.5) |

### ADR-RD-6 sweep 后台任务

```rust
pub async fn run_sweep_loop(manager: Arc<AffinityManager>, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        let n = manager.sweep_expired();
        metrics::counter!("gw_affinity_sweep_evicted_total").increment(n as u64);
    }
}
```

由 `gw-gateway::lib::Wired` 装配阶段启动,与 K-A 看门狗同类 lifecycle。默认 sweep 间隔 60 秒。

### ADR-RD-7 持久化(v2,本 spec 不实现)

跨副本场景:用户连续两次请求被 LB 路到不同 Rust 副本,副本 B 不知道副本 A 已经选了哪个 vendor_key → 亲和性失效。

Aether 用 Redis 持久化解决。v2 可加:

```
Redis key: affinity:<api_key_id>
HASH fields: vendor_key_id, bound_at_ms, last_hit_ms
TTL: 1800s
```

Rust 写本地 + 异步同步到 Redis;读时若本地 miss,fallback 读 Redis 一次填到本地。

**v1 不做**(进程内已经 95% 命中,1 个 user 通常打到同一副本通过 LB sticky)。

### ADR-RD-8 性能预算

| 操作 | 预算 |
|---|---|
| `record_hit` | < 100 ns(DashMap update) |
| `get` | < 50 ns(DashMap get) |
| `invalidate_vendor_key` | O(N) 全表扫;对 10k binding < 100 µs(可接受,失效是 cold path) |
| `sweep_expired` | O(N);1 分钟一次,10k binding < 100 µs |
| `AffinityDimension::score` | < 100 ns |

### ADR-RD-9 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_affinity_record_hit_total` | - | counter |
| `gw_affinity_get_total` | hit | counter:命中 / 未命中 |
| `gw_affinity_invalidate_vendor_key_total` | reason | counter |
| `gw_affinity_sweep_evicted_total` | - | counter |
| `gw_affinity_bindings_active` | - | gauge:当前 binding 数 |

### ADR-RD-10 测试

- record_hit + get 立刻命中 vendor_key_id 一致;
- TTL 过期 → get 返回 None;
- invalidate_vendor_key → 该 vkid 的所有 binding 清掉;
- sweep_expired → 老 binding 清,新 binding 留;
- AffinityDimension:有绑定且匹配 → 1.0;有绑定不匹配 → 0.0;无绑定 → 0.5;
- 并发 record + invalidate 不 panic。

## 3. 实施大纲

依赖:R-AB land(PickCtx 接口可注入 affinity_manager);K-A land(CooldownReason::is_long_term)。

1. 新建 `gw-routing/src/affinity.rs`;
2. 实施 `AffinityManager / AffinityRecord` + 接口;
3. K-A 加 `CooldownReason::is_long_term()` helper;
4. K-A 加 hook trait `HealthInvalidationHook`,KeyHealthStore::apply_cooldown 时调 hook;
5. `AffinityManager` 实现 `HealthInvalidationHook`,bootstrap 阶段注册;
6. `gw-gateway` 启动 sweep_loop tokio task;
7. `gw-proxy::ctx` 加 `affinity_manager: Option<Arc<AffinityManager>>` 字段;
8. `gw-proxy::filters::logging` 接入 record_hit;
9. AffinityDimension 加进 R-AB dimension 集合;
10. 单测覆盖;
11. 性能 bench(`gw-routing/benches/affinity.rs`)。

## 4. 未决问题

1. **TTL 30 分钟是否够**: Anthropic prompt cache 是 5 分钟,Gemini context cache 1 小时,OpenAI 暂无明确 TTL。30 分钟是折中。配置项暴露:`GW_AFFINITY__TTL_SECS`,按 ModelGroup 也可覆盖。
2. **per-(api_key, model) vs per-api_key 绑定粒度**: 当前 spec 是 `ApiKeyId -> KeyId`,一个 user 一份。但同一 user 用多个模型时(用一会儿 Sonnet 用一会儿 GPT-4o),粒度应该是 (ApiKey, Model)。倾向 v1 用更细粒度 `(ApiKeyId, ModelName) -> KeyId`,即便 binding 数翻几倍,DashMap 也轻松承担。改 spec 即可。
3. **是否区分 success/failure 比例**: 当前仅在 2xx record。但 vendor_key 偶尔 5xx 仍然总体可靠 → 是否要"连续 3 次成功才 bind"?倾向 v1 不引入连续计数(已经有 K-A 健康判断兜底);keep simple。
4. **AffinityManager 是 Arc<DashMap> 还是 builtin TTL 缓存(moka)**: moka 自带 TTL eviction,不需要后台 sweep。trade-off:moka 比 DashMap 内存占用更高(LRU 元数据)。v1 用 DashMap + sweep 简单;若发现性能问题再换 moka。
5. **invalidate_vendor 的 O(N) 扫**: 当 vendor 关闭 cache 时要扫全表找指向该 vendor 的 binding。10k binding 100 µs 可接受,但 100k binding 1 ms 就在锁竞争边缘。倾向加 secondary index(`vendor_id -> Vec<ApiKeyId>`),写时维护;v1 不做,binding 数限 10k 范围。
6. **跨副本 prompt cache 命中率影响**: v1 不持久化 → 第二个请求被 LB 路到另一副本时 binding 不存在 → 该 dim 给 0.5 中性 → multi-score 可能选不同 vendor_key → prompt cache miss。代价是 prompt 重计费(cache_creation_input_tokens > 0)。倾向 v1 接受,**Aether 也是同款代价,实际命中率仍可观**。
7. **affinity 与 K-A health 的协作**: K-A 把某 vendor_key 标 cooldown 1h,affinity 已经失效;但 cooldown 过期后,affinity 不会自动回来(那个 user 的 binding 已经被失效)。下次 user 请求会重新选 vendor_key + 重新 record。 这是期望行为(用户感知不到,只是 prompt cache 多 miss 一次)。

---

> R-D 是中等量级 spec,主要价值在 prompt cache 命中率。与 R-AB 的 cache_affinity 是不同维度(用户级 vs 请求级),正交可叠加。
