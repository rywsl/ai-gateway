# Learnings — v2-legacy-cleanup

## 2026-05-25 Plan Start
- internal_err() helper at gw-codec/src/common.rs:24 — use for all P0 panic replacements
- mgmt_bail! macro at gw-proxy/src/filters/request.rs:74 — template for P1 bail! macro
- Metrics catalog: gw-gateway/src/metrics_catalog.rs METRICS array + gw-gateway/tests/metrics_catalog.rs test
- HardCapCfg defaults: 1_000_000 / 5_000_000_000 — MUST match exactly
- BillingCfg pattern in config.rs is the template for AuthCfg wireup
- IpBlockEntry: Moka cache already exists (max 10k), ipblock_snapshot() consumer exists
- no_v1_markers_in_prod lint is error mode — no TODO(v2)/unimplemented!/todo!() allowed

## 2026-05-25 Task 6
- gw-codec response renderers now treat malformed ResponseIR invariants (non-Assistant response messages, non-text Assistant content, missing OpenAI tool_calls on ToolCalls finish) as `internal_err()` Results instead of process panics.
- The previous `panic!("expected ...")` occurrences were all in codec module tests/assertion helpers; regression coverage now asserts Err for each invariant path.
- 2026-05-25 task-11: IpBlockEntry now has Redis→PG cold loader by id, `EntityStore::ipblock_or_load`, and bootstrap fail-closed PG preload in 100-row pages.
