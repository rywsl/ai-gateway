# R-G — Sticky Strategy TTL 修复(v2026-05-19)

- **日期**：2026-05-19
- **状态**：Draft(待审阅,仅契约与边界)
- **作者**：协作设计(Claude + 用户)
- **关联**：
  - 现状:`crates/gw-routing/src/strategy/sticky.rs`(已知 leak)
  - [2026-05-19-routing-r-ab-multi-score.md](./2026-05-19-routing-r-ab-multi-score.md)(Sticky 作为 SelectorAny variant 保留)
- **范围**：修复 `Sticky` 的 `bindings: DashMap<String, (KeyId, Instant)>` leak:
  - 用 `moka::sync::Cache` 替换 DashMap,自带 TTL 过期 + LRU 淘汰;
  - `pick_ordered` 接口对齐 R-AB 的 SmallVec 排序列表;
  - 行政 invalidate 接口(K-A 标 vendor_key 长冷却时清相关 binding);
  - capacity 上限(防内存爆,默认 100k binding)。
- **不在范围**：
  - Sticky 的"按 conversation_id 路由"语义本身(不变);
  - 持久化跨副本(同 R-D,v2 留 Redis);
  - 其他 strategy(R-AB 已重写)。

> R-G 是修小 bug,但 spec 化为了让 sticky 也对齐 R-AB 的 SelectorAny::pick 返回 SmallVec 形态。

---

## 1. 背景

当前 `Sticky::bindings` 用 DashMap:

```rust
pub bindings: DashMap<String, (KeyId, Instant)>,
```

注释明说:

> TODO(v2): periodic sweep_expired() or migrate to moka::Cache with TTL.
> Today this DashMap grows monotonically — entries past STICKY_TTL are
> re-bound on access but never evicted, so abandoned conversations leak.

3 个问题:

1. **内存 leak**:DashMap 不自动驱逐 TTL 过期项;
2. **冷热不一致**:STICKY_TTL = 30 min,但被访问的 binding 永远不过期(因为再次访问会被 re-bind);未被访问的 binding 永远残留;
3. **接口不对齐 R-AB**:`Strategy::pick -> Option<&Arc<VendorKey>>`,R-AB 改 SmallVec。

## 2. 设计决策

### ADR-RG-1 用 moka Cache 替换 DashMap

```rust
use moka::sync::Cache;
use std::time::Duration;

pub struct Sticky {
    bindings: Cache<String, KeyId>,
}

impl Sticky {
    pub fn new(ttl: Duration, capacity: u64) -> Self {
        Self {
            bindings: Cache::builder()
                .time_to_live(ttl)
                .max_capacity(capacity)
                .build(),
        }
    }
}
```

默认 `ttl = 30 min`,`capacity = 100_000`。

moka 自带:

- TTL 过期(被动驱逐:get 时检查;主动驱逐:后台任务);
- LRU(超 capacity 时驱最近最少用的);
- 并发安全;

撤销 `Instant` 字段 — moka 内部已经持有 insertion time。

### ADR-RG-2 pick_ordered 接口(对齐 R-AB)

```rust
impl Sticky {
    pub fn pick_ordered(
        &self,
        candidates: &[Arc<VendorKey>],
        ctx: &PickCtx<'_>,
    ) -> SmallVec<[Arc<VendorKey>; 8]> {
        let Some(conv_id) = ctx.conversation_id else {
            // 无 conversation_id:fallback 到按 priority desc 排
            return sort_by_priority_desc(candidates);
        };

        // 查 binding
        if let Some(bound_kid) = self.bindings.get(conv_id) {
            // 找到 binding,把对应 candidate 放第一位;其余按 priority desc
            if let Some((idx, _)) = candidates.iter().enumerate()
                .find(|(_, k)| k.key_id == bound_kid)
            {
                let mut out = SmallVec::with_capacity(candidates.len());
                out.push(Arc::clone(&candidates[idx]));
                for (i, k) in candidates.iter().enumerate() {
                    if i != idx { out.push(Arc::clone(k)); }
                }
                return sort_after_first_by_priority(out);
            }
            // bound key 不在 candidate list(可能已被 filter 掉)
            // → 清掉 binding,fallthrough 重新 bind
            self.bindings.invalidate(conv_id);
        }

        // 没 binding 或 bound key 已失效 → 选第一个 candidate(按 priority desc 后)
        // 并 bind
        let sorted = sort_by_priority_desc(candidates);
        if let Some(first) = sorted.first() {
            self.bindings.insert(conv_id.to_owned(), first.key_id.clone());
        }
        sorted
    }

    /// K-A invalidation hook 调用:某 vendor_key 长冷却 → 清掉所有指向它的 binding
    pub fn invalidate_vendor_key(&self, vkid: &KeyId) {
        // moka 没有 reverse index;实际方案:
        //   方案 A: iterate Cache::iter() — moka::sync 支持但成本 O(N)
        //   方案 B: 维护 secondary index(KeyId -> Vec<conv_id>)
        //   方案 C: 不主动 invalidate,等 binding 自然 TTL 过期或 pick 时检测失效
        //
        // v1 选 C — pick_ordered 内部已经处理"bound key 不在 candidate list"的情况,
        // 即 K-A 把它 cooldown 后 list_candidates 不带它 → bound 自动失效 + 重 bind。
        //
        // **不需要显式 invalidate**——避免维护 secondary index 的复杂度。
        let _ = vkid;  // no-op
    }
}

fn sort_by_priority_desc(c: &[Arc<VendorKey>]) -> SmallVec<[Arc<VendorKey>; 8]> {
    let mut out: SmallVec<_> = c.iter().map(Arc::clone).collect();
    out.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.key_id.0.cmp(&b.key_id.0)));
    out
}

fn sort_after_first_by_priority(mut v: SmallVec<[Arc<VendorKey>; 8]>) -> SmallVec<[Arc<VendorKey>; 8]> {
    if v.len() > 1 {
        let first = v.remove(0);
        let mut rest = v;
        rest.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.key_id.0.cmp(&b.key_id.0)));
        let mut out = SmallVec::with_capacity(rest.len() + 1);
        out.push(first);
        out.extend(rest);
        out
    } else {
        v
    }
}
```

### ADR-RG-3 与 R-E failover 的协同

R-E 拿到 sticky 输出的 SmallVec 后按顺序尝试。bound key 是首位 → 第一尝试;失败 → R-E 切到第二位(其余按 priority desc)。

这与 Aether 行为一致:sticky 优先粘,失败时按"剩余 candidate 排序"fallback。

### ADR-RG-4 capacity 满时的行为

`max_capacity = 100_000`。满时 moka 用 LRU 淘汰最久未访问的 binding。代价:

- 极端高 QPS(>1000 RPS) + 高 conversation 唯一性 → 100k 在几分钟内填满 → LRU 开始驱逐;
- 被驱逐的 conversation 下次访问时 binding 缺失 → 重新 bind 一个新 vendor_key;
- 用户感知:同一 conversation 中途切换 vendor_key,prompt cache miss 一次。

可接受。100k 容量约 ~3MB(每 entry ~32B);若需要更多,env 配置 `GW_STICKY__CAPACITY` 调高。

### ADR-RG-5 性能预算

| 操作 | 预算 |
|---|---|
| `bindings.get(conv_id)` 命中 | < 100 ns(moka 内部 RwLock + AtomicTime) |
| `bindings.get(conv_id)` miss | < 100 ns |
| `bindings.insert` | < 200 ns(可能触发 LRU 驱逐) |
| `pick_ordered` 命中 bound key | < 500 ns(查 + iter candidates 找 idx + clone) |
| `pick_ordered` 无 binding | < 1 µs(sort) |

### ADR-RG-6 metrics

| Metric | Label | 说明 |
|---|---|---|
| `gw_sticky_bind_total` | - | counter:新建 binding |
| `gw_sticky_hit_total` | bound_in_list | counter:命中(bound key 仍在 candidate list / 不在) |
| `gw_sticky_evict_total` | reason | counter:reason = ttl_expired / lru / explicit |
| `gw_sticky_bindings_active` | - | gauge(由 moka entry_count() 暴露) |

### ADR-RG-7 测试

- 第一次 pick(无 binding)→ 选 priority 最高的 + bind;
- 第二次 pick(命中 binding)→ bound key 首位;
- bound key 不在 candidate list → 清 binding + 重新选;
- 无 conversation_id → fallback priority desc;
- TTL 过期 binding(>30 min)→ 不再命中,重新 bind;
- capacity 满 → LRU 驱逐 + 新 binding;
- 并发:1000 task 同时 pick 不同 conv_id → 无 race,binding 数 == 1000。

## 3. 实施大纲

依赖:R-AB land(SelectorAny::Sticky 接口签名 pick_ordered)。

1. 改 `crates/gw-routing/src/strategy/sticky.rs`:
   - 撤销 `bindings: DashMap`;
   - 加 `moka::sync::Cache<String, KeyId>`;
   - 改 `pick` → `pick_ordered`,返回 SmallVec;
   - 删除 `sweep_expired` TODO(moka 自带);
2. 改 `crates/gw-routing/Cargo.toml` 加 `moka = { version = "0.12", features = ["sync"] }`(可能已有);
3. SelectorAny::Sticky 接口对齐;
4. 单测 + bench。

## 4. 未决问题

1. **TTL 与 R-D affinity_manager 是否合并**: 都是"conversation/api_key → vendor_key 亲和缓存",语义有重叠。区别:sticky 严格(同 conv 必同 key),affinity 软(分数贡献)。**保留分立**(用户可以单独启用 Sticky 而不用 AffinityDimension)。
2. **conversation_id 来源**: 当前 `PickCtx::conversation_id: Option<&'a str>`;客户端在请求 header(如 OpenAI 的 `X-Conversation-Id`)或 body 字段提供。本 spec 不动来源逻辑,gw-proxy 在装 PickCtx 时填。
3. **moka 异步 vs 同步**: moka 有 `sync` 与 `future` 两个 feature。pick_ordered 是同步函数,选 sync。
4. **conversation_id 可能很长**: HTTP header 限制下通常 < 256B,但用户可能塞 UUID + 时间戳形式。倾向 v1 不限制长度,monitor `gw_sticky_conversation_id_len` histogram 决定是否截断。
5. **bound key 在 list 但已 cooldown**: 当 binding 命中且 bound key 仍在 list(R-AB filter 让它过)但被 K-A 标短期 cooldown — R-AB 的 KeyHealthAvailable filter 应该把它过滤掉;若没过滤(filter 链未启用 health check)则 sticky 仍返第一位 → R-E 尝试失败切。这条由 K-A 与 R-AB 配置决定,Sticky 不参与决策。
6. **跨副本 sticky**: 与 R-D 同款问题。v1 不持久化;依赖 LB sticky(L4)把同 user 的请求路到同副本。

---

> R-G 是最小 spec,几乎全是 mechanical 修复。完成后 Sticky 不再 leak,且接口与 R-AB 对齐。
