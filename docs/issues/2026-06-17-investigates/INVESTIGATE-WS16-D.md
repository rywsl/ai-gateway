# INVESTIGATE-WS16-D: X-Forwarded-For Trust Paths

Status: RESOLVED_SOURCE_CHECK

Local finding: source search found XFF identity handling in `crates/gw-proxy/src/filters/early.rs`, request wiring in `crates/gw-proxy/src/filters/request.rs`, and configuration validation in `crates/gw-gateway/src/config.rs`. The default remains `trust_xff = false`; production startup rejects `trust_xff = true` with an empty `runtime.trusted_proxies` list.

No additional implicit XFF trust path was found in the local source search. The remaining risk is operator configuration: public-internet deployments must not enable `trust_xff` unless all inbound traffic is forced through trusted proxies that strip untrusted client headers.

Residual external dependency: production config snapshots must be audited before release. The R4 CSV is schema-only until that data is available.
