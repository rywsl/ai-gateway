# WS-12 CI Lints

These lints are launch gates. Prefer fixing the underlying code. Use baselines only with a concrete review record.

| Gate | How to run | Allowlist / bypass |
|---|---|---|
| Tokio spawn baseline | `rustc scripts/check-tokio-spawn.rs -o /tmp/check-tokio-spawn && /tmp/check-tokio-spawn crates` | Existing findings live in `scripts/check-tokio-spawn.baseline`; new rows require review. |
| Money-sensitive casts | `./scripts/check-money-as-cast.sh` | Existing findings live in `scripts/check-money-as-cast.baseline`; prefer checked conversions. |
| Metric label cardinality | `./scripts/check-metric-labels.sh` | `// allow-label: <label> reason="..."` near the metric declaration. |
| Rustpact protocol version | `./scripts/check-rustpact-pv.sh` | Bump `RUSTPACT_PROTOCOL_VERSION` with added protocol fields. |
| Deploy order | `./scripts/check-deploy-order.sh` | `WS12_SKIP_DEPLOY_ORDER=1` is emergency-only and must be explained in the PR. |
| New findings audit | `./scripts/audit-newfindings.sh` | Warn-only. Use `WS12_AUDIT_WRITE=1` to persist a report. |
