# Part 04 — Proxy Nodes(Hub / Tunnel 节点契约)

> 本 part 描述 **Rust 网关 tunnel client 与 Go Hub 之间** 的代理节点契约。索引见 [main.md](../main.md)。本 part 引入 = [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md) §1 + [`docs/superpowers/specs/2026-05-22-tunnel-t-design.md`](../../superpowers/specs/2026-05-22-tunnel-t-design.md) ADR-T-3。

## 4.1 概览

| 接触点 | Writer | Reader | 说明 |
|---|---|---|---|
| PgSQL `proxy_nodes` | Go(Hub heartbeat ingestion + 管理 UI)| Rust(EntityStore 冷加载)| 节点元数据权威 |
| Redis `entity:proxy_node:<id>` | Go(write-through 可选)| Rust(冷加载写回)| L1 cache |
| `events:entity` ns=`proxy_node` | Go | Rust StreamConsumer | 节点变更通知 |
| `/api/internal/hub/heartbeat` | (远端节点 → Go)| Go | Rust **不**接收 |
| `/api/internal/hub/node-status` | Go 暴露 | 前端 / 运维 | Rust **不**接收 |
| WS endpoint(`ws_endpoint` 字段)| 远端节点暴露 | Rust tunnel client 拨号 | 仅 Rust → 远端单向 |

## 4.2 `proxy_nodes.payload` — `ProxyNodeEntry`

```json
{
  "node_id": "pn_xxx",
  "display_name": "us-west-1-a",
  "region": "us-west-1",
  "ws_endpoint": "wss://node-xxx.example.com:443/tunnel",
  "status": "active",            // active | draining | offline
  "latency_ms": 42,
  "last_heartbeat_ts_ms": 1716345678901,
  "target_whitelist": ["api.openai.com:443", "api.anthropic.com:443"],
  "provider_affinity": ["v_openai"],
  "key_id_affinity": ["vk_xxx", "vk_yyy"],
  "extra": {}                    // serde_json::Value,forward-compat
}
```

ID = `node_id`(前缀 `pn_`)。

### 4.2.1 `status` 语义

| 状态 | 含义 | Rust 行为 |
|---|---|---|
| `active` | 心跳正常,可拨号 | 加入 `resolve_tunnel` 候选 |
| `draining` | 不接新流量,已有 stream 继续 | `resolve_tunnel` 排除;`TunnelStack` 保留 transport 直到 stream 结束 |
| `offline` | 心跳超时 / 主动下线 | `TunnelStack` drop 对应 `ReconnectingTransport` |

## 4.3 Heartbeat 责任分工

### 4.3.1 远端节点 → Go

`/api/internal/hub/heartbeat` 由 **Go** 服务;远端节点周期性上报。Rust 端**完全不接收**节点心跳。Heartbeat payload 字段由 Go 端 + 节点协议自决,本契约不规定。

### 4.3.2 Go → 写 PgSQL + XADD

收到心跳后,Go 端:

1. `UPDATE proxy_nodes SET payload = jsonb_set(payload, '{last_heartbeat_ts_ms}', new_ts), version = version + 1 WHERE id = $1`;
2. 若 status 从 offline → active(连续 3 次心跳),同步更新 `payload.status`;
3. `XADD events:entity * ns proxy_node id <node_id> v <new_v> op upsert`。

### 4.3.3 心跳超时状态机(Go 责任)

| 超时窗口 | 判定 |
|---|---|
| < 30s 未收 | 不动(网络抖动容忍)|
| 90s 未收 | status → draining + XADD |
| 300s 未收 | status → offline + XADD |

Rust 自身**不**做超时判定;以 Go 写的 status 为准(参 [gaps spec ADR-G1-4](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md#adr-g1-4-status-转换不在-rust-触发))。

## 4.4 Rust 读路径

详见 [`docs/superpowers/specs/2026-05-22-go-onboarding-gaps-design.md`](../../superpowers/specs/2026-05-22-go-onboarding-gaps-design.md) ADR-G1-1 / G1-3 与 [`2026-05-22-tunnel-t-design.md`](../../superpowers/specs/2026-05-22-tunnel-t-design.md) ADR-T-2 / T-3。要点:

- `TunnelStack` 启动期冷加载 `proxy_nodes` 全表;
- `StreamConsumer` 收到 ns=`proxy_node` 事件 → 触发 `TunnelStack::refresh()`(全量重读 status != offline 的节点)→ ArcSwap;
- `DashMap<node_id, Arc<ReconnectingTransport>>` 按 node_id 持有 transport;node 软删除 / status = offline → drop transport。

## 4.5 监控

Go 端:

| 指标 | 阈值 |
|---|---|
| `hub_heartbeat_lag_secs{node_id}` | > 60 |
| `proxy_node_status_distribution{status}` | `offline` 比例 > 20% → 告警 |
| `proxy_node_upsert_total` | counter |

Rust 端 `gateway_tunnel_*` 详见 [`docs/superpowers/specs/2026-05-22-tunnel-t-design.md`](../../superpowers/specs/2026-05-22-tunnel-t-design.md) §遥测段。

## 4.6 migration 0005 模板(已 merged,本节供历史对照)

```sql
CREATE TABLE proxy_nodes (
  id          TEXT PRIMARY KEY,
  payload     JSONB NOT NULL,
  version     BIGINT NOT NULL DEFAULT 0,
  updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  deleted_at  TIMESTAMPTZ NULL
);
CREATE INDEX proxy_nodes_status_idx ON proxy_nodes ((payload->>'status'));
```

落点:`crates/gw-store/migrations/0005_proxy_nodes.sql`(随 tunnel-t T1 一同 merged,本 spec 仅补契约文档)。

## 4.7 未决问题

1. 节点出口 IP 池 / 健康抖动指标的 schema 演化:`extra` JSONB 字段做兜底,稳定字段沉淀进 `ProxyNodeEntry` 主体。
2. 节点级双向心跳(Rust 主动 ping 节点 + 节点主动 ping Hub)是否要 reconcile:v1 由两条独立链路各自维护,运维侧观察 metric 差异。
