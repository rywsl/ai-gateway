# Learnings — v2-proxy-core-cleanup

## T2: TPM deferred deduction
- `governor::RateLimiter::check_n(NonZeroU32)` is the token-bucket deduction API; `check()` is the 1-unit RPM variant.
- `NonZeroU32::new(0)` returns `None` — must guard against zero-token deductions or `check_n` panics on unwrap.
- `logging::run` already has 8 params with `#[allow(clippy::too_many_arguments)]`; adding `ratelimit: &Arc<RateLimitGuard>` as 9th is acceptable since the function is called from exactly one site in `gateway.rs`.
- TPM deduction is fire-and-forget: response is already sent when `logging` runs, so TPM breach only affects the *next* request. This is by design — same pattern as the G3 daily-quota increment.
- Pre-existing `entity.rs` had a `format_args!` inside `.map(|x| ...)` that returns a temporary — had to fix to `format!` to unblock compilation.
## T3: Response deadline check (2026-05-24)

- `Timeouts` struct is computed in `upstream.rs` (pick_peer) and applied to `HttpPeer`, but NOT stashed on `ReqCtx`. Added `response_deadline: Option<Duration>` to `ReqCtx` so the response filter can check it. Stashed after `apply_to_peer` using `if ctx.is_stream { t.stream_total } else { t.total }`.
- `ctx.timing.start` is `Option<Instant>`, not plain `Instant`. Must use `if let` pattern matching.
- `check_deadline()` extracted as a standalone `fn(ctx: &ReqCtx) -> Result<()>` to enable unit testing without constructing a `pingora_proxy::Session` (which requires a live TCP stream).
- Pre-existing compilation failures in `request.rs` (borrow checker: `parse_body` borrows `ctx.cached_body` immutably while `emit_and_bail` borrows `ctx` mutably) and `upstream.rs` (missing `apply_forced_params_mutations` / `apply_forced_params_to_ir`) prevent `cargo test -p gw-proxy`. `cargo check -p gw-proxy` passes — the broken code is only reachable from test compilation that exercises the full lib.
- `Instant::now() - Duration::from_secs(N)` works for creating a past instant in tests (no `checked_sub` needed for reasonable values).

## T3b: ForcedMutations bitfield + metric (2026-05-24)

- `ForcedParams` lives on `ModelGroup` (entity.rs), not `VendorKey`. Had to thread `forced_params` from `pick_peer` (where `group` is resolved) through `ctx.forced_params` to `rewrite_request` (where the IR is rendered).
- `Mutations` has 9 fields (bits 0-8) with no `top_p` setter. `ForcedMutations` adds `set_top_p()` at bit 9 and includes `field_names()` for metric label iteration.
- When `forced_params` are non-empty, same-protocol passthrough is defeated (mutations non-empty). The else branch now does parse→mutate→render regardless of protocol match.
- `same_protocol_passthrough()` re-parses+re-renders when `mutations.is_empty() == false`, but does NOT apply mutations to the IR. The forced_params application must happen in the caller's else branch via `apply_forced_params_to_ir()`.
- Metric: `gw_forced_params_applied_total{field}` emitted via `metrics::counter!` — same pattern as `gw_tunnel_resolve_total` and `gw_codec_cross_render_lossy_total`.
- `ReqCtx` now carries both `forced_params: ForcedParams` and `forced_mutations: ForcedMutations` for downstream consumption (detailed-log, metrics).
