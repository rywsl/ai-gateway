# Learnings — v2-go-contracts-update

## 2026-05-24 Session Init
- This plan is ZERO Rust code — documentation only under docs/go-contracts/
- §2.11 items #2 and #3 must change from "follow-up" to "specified"
- §3.9 item #1 must change from "当前未用" to "specified"
- flusher.rs has wrap_for_dlq + dlq_xadd already implemented (Rust side done)
- VendorKey fields: forced_capabilities (Vec<String>), exclusive_capability_match (bool) 
- Go naming: ApiKeyEntry.force_capabilities (deliberate naming diff, documented in architecture.md §Deliberate behaviours)

## 2026-05-24 T3 — exclusive_capability_match index + JSONB shape contract
- Added §3.2.5.2 "Capability 字段契约" to 03-shared-schema.md (lines 184-258)
- Covers: JSONB shape, top-level column mapping, partial index SQL, dual-write invariant, naming diff table, sync rule
- Updated §3.9 #1 from "当前未用，留 follow-up" to "已规定，见 §3.2.5.2"
- Partial index: idx_vendor_keys_excl_cap WHERE exclusive_capability_match = true
- Naming asymmetry (forced_ vs force_) confirmed deliberate in architecture.md §8 line 245

## 2026-05-24 S-contracts T2: Ghost-freeze reconciliation
- §2.5.5 added with 4-step algorithm: scan → compare → release → audit
- Trigger: Go periodic task (5 min) or Rust pod health-check failure detection
- Rust precondition: frozen_micro:<replica_id> subkeys (current code uses shared AtomicI64, no replica_id)
- Rust-side change deferred to S-ledger spec; contract only documents the Go interface
- §2.5.3 line 401 updated: removed "v1 留 follow-up", now references §2.5.5
- §2.11 #3 updated from "不涵盖，留 follow-up" to "已规定，见 §2.5.5"
- Risk mitigations: 3-check grace period, amount threshold with alert, settle-time overdraft detection as backstop
- Redis key pattern uses hash-tags: counter:user:{user:<uid>}:frozen_micro:<replica_id>

## 2026-05-24 S-contracts T1: Go DLQ replay contract
- §2.2.3 expanded from 5-bullet recommendation to full specification with: consumer group creation, XREADGROUP pattern, processing pipeline, idempotency, retry backoff (1s/2s/4s/8s/16s), startup recovery sequence, out-of-order tolerance, monitoring table
- §2.2.3 header changed from "推荐" to "必需" (v2 three-tier DLQ makes Go drainer mandatory)
- §2.2.5 added: dlq_layer extension (mem/disk/redis) with Go drainer obligations, backward compat rule (missing field = "redis")
- §2.11 #2 updated: strikethrough "已规定", references §2.2.3 and §2.2.5
- flusher.rs wrap_for_dlq uses { "payload": <inner>, "dlq_meta": { failure_reason, retry_count } } but contract doc uses { "dlq_meta": { ... }, "usage_record": { ... } } — contract doc is Go-facing authority
- Startup recovery order: drain DLQ pending → drain DLQ new → start main stream consumer
- dlq_layer grep count: 9 occurrences (requirement was ≥ 3)
