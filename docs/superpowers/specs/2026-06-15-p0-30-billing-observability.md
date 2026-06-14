# P0 #30 Rust Billing Page-On-Call Metrics

Status: implemented
Date: 2026-06-15
Related work item: Go #30 observability and alerting

## Background

The Go side exposes billing Prometheus metrics, split-worker scrape support,
alert rules, and SLO/runbook documentation. The Rust gateway owns the page-on-call
billing flusher metrics that those Go-side alert rules reference:

- `gw_billing_flusher_panic_total`: flusher tick panic caught by `catch_unwind`.
- `gw_billing_flusher_dead_total`: `emit()` observed that the flusher channel is closed.
- `gw_billing_emit_lag_t0_total`: a settled `UsageRecord` was not emitted within `emit_lag_t0_secs`.

Each metric is documented in the catalog as "ANY non-zero value = page on-call".

## Root Cause

`metrics-exporter-prometheus` renders only metric series whose handles have been
created through calls such as `counter!()` or `gauge!()`. Calling
`describe_counter!()` through `metrics_catalog::describe_all()` registers HELP
and TYPE metadata, but does not create a rendered counter series.

Before this change, the three incident-only counters were absent from `/metrics`
until the first incident incremented them. That left dashboards and alert rules
looking at an absent series rather than a healthy `0` baseline.

## Implementation

`crates/gw-gateway/src/metrics_catalog.rs` now defines:

- `PAGE_ON_CALL_BILLING_COUNTERS`, the authoritative list of the three page-on-call billing counters.
- `zero_init_page_on_call()`, which calls `counter!(name).increment(0)` for each counter.

`crates/gw-gateway/src/observability.rs` calls `zero_init_page_on_call()` during
metrics bootstrap immediately after `describe_all()`.

The real `gw_billing_emit_lag_t0_total` incident path keeps its existing
`request_id` and `attempt_index` labels. The boot-time baseline intentionally
creates a label-less zero series so `sum(increase(...))` alert expressions have
a stable healthy baseline without changing incident semantics.

## Tests

`crates/gw-gateway/tests/metrics_catalog.rs` covers:

- `describe_only_does_not_render_page_on_call_counters`: locks in the root cause
  that describe-only registration does not render the page-on-call counters.
- `boot_init_renders_page_on_call_counters_at_zero`: verifies `describe_all()`
  plus `zero_init_page_on_call()` renders each counter with a zero baseline.
- `page_on_call_set_is_subset_of_catalog`: verifies the zero-init list stays in
  the static metrics catalog and remains documented as page-on-call.

## Verification

Intended verification commands:

```bash
cargo test -p gw-gateway --test metrics_catalog
cargo test -p gw-gateway -p gw-ledger
cargo clippy -p gw-gateway --all-targets -- -D warnings
cargo fmt --all -- --check
```

Local Codex verification was limited by the Windows environment: the MSVC
toolchain has no `link.exe` on PATH, and the GNU toolchain fails in this
workspace path while linking native build scripts. `cargo fmt --all -- --check`
passed, and the Rust crate API signatures used by the new tests were verified
against the local cargo registry.
