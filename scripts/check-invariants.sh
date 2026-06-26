#!/usr/bin/env bash
# Invariant lint harness for the v2 integration.
# Authoritative spec: docs/architecture.md.
#
# Each check below is one §5.2 lint. Checks that are GREEN at the current
# phase run unconditionally. Checks whose target code has not landed yet are
# kept as PENDING blocks (commented), labelled with the phase that activates
# them, so the full target set stays visible to reviewers.
#
# Usage: bash scripts/check-invariants.sh
# Exit 0 = all active invariants hold; non-zero = a violation was found.

# Note: `-e` is intentionally omitted — many grep-based lints below use the
# pattern `grep ... && bad ...`, which relies on grep's non-zero "no match"
# exit not being treated as fatal. The `cd` above is hard-guarded instead.
set -uo pipefail
cd "$(dirname "$0")/.." || { echo "FATAL: cannot cd to repo root" >&2; exit 2; }

fail=0
note() { printf '  \033[33m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32mok\033[0m   %s\n' "$*"; }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$*"; fail=1; }

# ---------------------------------------------------------------------------
# crate_layering (§5.2) — dependency-edge form (robust; no string false-positives)
# Supporting crates MUST NOT depend on business/execution crates.
# Execution crates MUST NOT depend on each other.
# ---------------------------------------------------------------------------
echo "[crate_layering] supporting-layer must not depend on business/execution crates"
SUPPORTING="gw-core gw-codec gw-codec-macros gw-store gw-auth gw-ledger gw-runtime"
FORBIDDEN_FOR_SUPPORTING="gw-adapter gw-health gw-task gw-task-runtime gw-proxy gw-billing gw-gateway gw-task-worker"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  meta="$(cargo metadata --no-deps --format-version 1 2>/dev/null)"
  for crate in $SUPPORTING; do
    deps="$(echo "$meta" | jq -r --arg c "$crate" \
      '.packages[] | select(.name==$c) | .dependencies[] | select(.kind == null) | .name')"
    for forb in $FORBIDDEN_FOR_SUPPORTING; do
      if echo "$deps" | grep -qx "$forb"; then
        bad "$crate depends on $forb (supporting → business/execution forbidden)"
      fi
    done
  done
  # Execution crates must not depend on each other.
  proxy_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-proxy") | .dependencies[] | select(.kind == null) | .name')"
  if echo "$proxy_deps" | grep -qx "gw-task-runtime"; then
    bad "gw-proxy depends on gw-task-runtime (execution ↔ execution forbidden)"
  fi
  # gw-routing is business-layer as of Phase 1c (depends on gw-health).
  # Verify it does not depend on any execution crate.
  FORBIDDEN_FOR_ROUTING="gw-proxy gw-gateway gw-task-runtime gw-task-worker"
  routing_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-routing") | .dependencies[] | select(.kind == null) | .name')"
  for forb in $FORBIDDEN_FOR_ROUTING; do
    if echo "$routing_deps" | grep -qx "$forb"; then
      bad "gw-routing depends on $forb (business → execution forbidden)"
    fi
  done
  [ "$fail" -eq 0 ] && ok "no forbidden supporting/execution edges"
else
  note "skipped: cargo + jq required"
fi

echo "[crate_layering] gw-capability is business; must not depend on routing/proxy/adapter/tunnel-client/gateway"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  cap_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-capability") | .dependencies[] | select(.kind == null) | .name')"
  for forb in gw-routing gw-proxy gw-adapter gw-tunnel-client gw-gateway; do
    if echo "$cap_deps" | grep -qx "$forb"; then
      bad "gw-capability depends on $forb (business-layer rule)"
    fi
  done
  [ "$fail" -eq 0 ] && ok "gw-capability layering ok"
fi

echo "[crate_layering] gw-tunnel-client business; must not depend on execution layer"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  tc_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-tunnel-client") | .dependencies[] | select(.kind == null) | .name')"
  for forb in gw-proxy gw-gateway gw-task-runtime gw-routing; do
    if echo "$tc_deps" | grep -qx "$forb"; then
      bad "gw-tunnel-client depends on $forb (business → execution forbidden)"
    fi
  done
  [ "$fail" -eq 0 ] && ok "gw-tunnel-client layering ok"
fi

echo "[crate_layering] gw-task-runtime business; must not depend on execution layer"
if command -v cargo >/dev/null && command -v jq >/dev/null; then
  task_runtime_deps="$(echo "$meta" | jq -r '.packages[] | select(.name=="gw-task-runtime") | .dependencies[] | select(.kind == null) | .name')"
  for forb in gw-proxy gw-gateway gw-task-worker; do
    if echo "$task_runtime_deps" | grep -qx "$forb"; then
      bad "gw-task-runtime depends on $forb (business → execution forbidden)"
    fi
  done
  [ "$fail" -eq 0 ] && ok "gw-task-runtime layering ok"
fi

# ---------------------------------------------------------------------------
# no_billing_crate (§5.2) — gw-billing is allowed only as the task-level
# formula crate. It must not become a dependency of supporting crates.
# ---------------------------------------------------------------------------
echo "[no_billing_crate] gw-billing limited to workspace member and business deps"
if grep -rn 'gw-billing' crates/{gw-core,gw-codec,gw-codec-macros,gw-store,gw-auth,gw-ledger}/Cargo.toml 2>/dev/null; then
  bad "supporting crate references gw-billing"
else
  ok "gw-billing not referenced by supporting crates"
fi

# ---------------------------------------------------------------------------
# no_proxy_worker_dep (§5.2) — gw-proxy and gw-task-runtime must not interdepend.
# (gw-task-runtime is a SUPPORTING layer crate; this check is now non-trivially-true.)
# ---------------------------------------------------------------------------
echo "[no_proxy_worker_dep] gw-proxy must not depend on gw-task-runtime"
if grep -rn 'gw-task-runtime' crates/gw-proxy/Cargo.toml 2>/dev/null; then
  bad "gw-proxy/Cargo.toml references gw-task-runtime"
else
  ok "gw-proxy does not depend on gw-task-runtime"
fi

echo "[no_task_runtime_imports_proxy] gw-task-runtime must not depend on gw-proxy"
if grep -rn 'gw-proxy' crates/gw-task-runtime/Cargo.toml 2>/dev/null; then
  bad "gw-task-runtime/Cargo.toml references gw-proxy"
else
  ok "gw-task-runtime does not depend on gw-proxy"
fi

# ---------------------------------------------------------------------------
# no_worker_addr_config / no_gateway_addr_config (§5.2, C-8)
# gateway config/code must not name a worker address, and vice versa.
# gw-task-runtime exists as a SUPPORTING crate; only the gateway side is checked here.
# ---------------------------------------------------------------------------
echo "[no_worker_addr_config] gateway must not hardcode a worker host/port"
if grep -rnE '(^|[^[:alnum:]_])(worker[_-]?host|worker[_-]?port|TASK_WORKER_(URL|HOST))([^[:alnum:]_]|$)' config/ crates/gw-gateway 2>/dev/null; then
  bad "gateway config/code names a worker address"
else
  ok "gateway carries no worker address"
fi

# ---------------------------------------------------------------------------
# no_cost_micro (§5.2) — Rust no longer computes cost.
#
# Whitelist: test absence-assertions of the form
# `assert!(!s.contains("cost_micro"), …)` that verify the field is gone.
# Real violations (`pub cost_micro: u64`, `.cost_micro`, etc.) still trip.
# ---------------------------------------------------------------------------
echo "[no_cost_micro] no cost_micro field in workspace"
if grep -rnE '\bcost_micro\b' crates/ 2>/dev/null \
    | grep -vE 'assert!\(!.*\.contains\("cost_micro"\)' \
    | grep -q .
then
  grep -rnE '\bcost_micro\b' crates/ 2>/dev/null \
      | grep -vE 'assert!\(!.*\.contains\("cost_micro"\)'
  bad "cost_micro field remains"
else
  ok "no cost_micro field"
fi

echo "[no_billing_cost_per_token] gw-billing must not contain cost_per_token or price_per_token"
if grep -rnE '\bcost_per_token\b|\bprice_per_token\b' crates/gw-billing/src 2>/dev/null; then
  bad "gw-billing contains per-token pricing fields"
else
  ok "gw-billing carries no per-token pricing fields"
fi

# ---------------------------------------------------------------------------
# credit_lease_metric_names — the hard-mode credit-lease subsystem emits metric
# names through gw-ledger constants. Those constants must match the gateway
# metrics catalog (`gw_` prefix), and gw-proxy must not reintroduce split
# hardcoded series for the same signals.
# ---------------------------------------------------------------------------
echo "[credit_lease_metric_names] credit-lease metric constants must use gw_ names"
if grep -rnE 'pub const (CREDIT_LEASE_ACQUIRE_TOTAL|CREDIT_LEASE_ESTIMATE_CREDITS|CREDIT_OVERFLOW_GATE_TOTAL|CREDIT_OVERFLOW_LATENCY_MS|CREDIT_OVERFLOW_CONSUMED_WINDOW|PG_FALLBACK_TOTAL|COEF_TABLE_ACTIVE_VERSION|HEARTBEAT_SADD_TOTAL): &str = "(credit_|pg_fallback_total|coef_table_active_version|heartbeat_sadd_total)' crates/gw-ledger/src/metrics.rs 2>/dev/null; then
  bad "credit-lease metric constants are missing the gw_ prefix"
else
  ok "credit-lease metric constants use gw_ names"
fi

echo "[credit_lease_metric_names] gw-proxy must use gw-ledger metric constants for credit lease metrics"
if grep -rnE '"gw_credit_lease_acquire_total"|"gw_credit_overflow_gate_total"|"gw_credit_actual_exceeds_reserved_total"' crates/gw-proxy/src 2>/dev/null; then
  bad "gw-proxy has hardcoded credit-lease metric names"
else
  ok "gw-proxy credit-lease metrics use constants"
fi

echo "[metric_label_cardinality] Prometheus labels must not use M13 high-cardinality / PII names"
if bash scripts/check-metric-labels.sh; then
  ok "metric label cardinality lint passed"
else
  bad "metric label cardinality lint failed"
fi

echo "[feature_flag_registry] WS-15 flag ledger must match schema"
if bash scripts/check-flags-schema.sh; then
  ok "flag registry schema lint passed"
else
  bad "flag registry schema lint failed"
fi

echo "[feature_flag_defaults] WS-15 flag defaults must match yaml ledger"
if bash scripts/check-mixed-version-flags.sh; then
  ok "flag default drift lint passed"
else
  bad "flag default drift lint failed"
fi

echo "[runbook_completeness] WS-15 flags must have runbook coverage"
if bash scripts/check-runbook-completeness.sh; then
  ok "feature-flag runbook covers all flags"
else
  bad "feature-flag runbook coverage failed"
fi

echo "[ws12_prod_compose] production manifests must not contain demo bootstrap env"
if bash scripts/check-prod-compose.sh; then
  ok "production manifest bootstrap lint passed"
else
  bad "production manifest bootstrap lint failed"
fi

echo "[ws12_soft_delete] Rust repo soft-delete mirror"
if bash scripts/check-soft-delete.sh; then
  ok "soft-delete mirror lint passed"
else
  bad "soft-delete mirror lint failed"
fi

echo "[ws12_tokio_spawn_baseline] raw tokio::spawn baseline must not grow"
tokio_bin="${TMPDIR:-/tmp}/check-tokio-spawn.$$"
if rustc scripts/check-tokio-spawn.rs -o "$tokio_bin" && "$tokio_bin" crates; then
  ok "tokio spawn baseline lint passed"
else
  bad "tokio spawn baseline lint failed"
fi
rm -f "$tokio_bin"

echo "[ws12_money_cast_baseline] money-sensitive casts baseline must not grow"
if bash scripts/check-money-as-cast.sh; then
  ok "money cast baseline lint passed"
else
  bad "money cast baseline lint failed"
fi

echo "[ws12_rustpact_pv] protocol fields require rustpact version bump"
if bash scripts/check-rustpact-pv.sh; then
  ok "rustpact pv lint passed"
else
  bad "rustpact pv lint failed"
fi

echo "[ws12_newfindings] continuous-discovery audit"
if bash scripts/audit-newfindings.sh; then
  ok "new findings audit completed"
else
  bad "new findings audit failed"
fi

echo "[ws13_chaos_scenarios] stable chaos scenario registry must be complete"
if bash scripts/check-chaos-dod-refs.sh; then
  ok "chaos scenario registry complete"
else
  bad "chaos scenario registry failed"
fi

echo "[ws14_migration_numbers] migration ledger and down sidecars must be complete"
if bash scripts/check-migration-number.sh; then
  ok "migration ledger gate passed"
else
  bad "migration ledger gate failed"
fi

echo "[ws14_migration_roundtrip_static] migration roundtrip static prerequisites"
if bash scripts/verify-migrations-roundtrip.sh --static; then
  ok "migration roundtrip static prerequisites passed"
else
  bad "migration roundtrip static prerequisites failed"
fi

echo "[no_cli_auth_bypass] gw-codec CLI variants must not contain auth bypass markers"
if grep -rnE 'skip_auth|bypass_auth' crates/gw-codec/src/cli/ 2>/dev/null; then
  bad "gw-codec CLI variants contain auth bypass markers"
else
  ok "gw-codec CLI variants carry no auth bypass markers"
fi

echo "[task_crates_in_arch_doc] docs/architecture.md must list task crates"
missing_task_crates=""
for crate in gw-task gw-task-runtime gw-task-worker gw-billing; do
  if ! grep -q "$crate" docs/architecture.md 2>/dev/null; then
    missing_task_crates="${missing_task_crates}${crate} "
  fi
done
if [ -n "$missing_task_crates" ]; then
  bad "docs/architecture.md missing crate references: $missing_task_crates"
else
  ok "docs/architecture.md lists gw-task, gw-task-runtime, gw-task-worker, gw-billing"
fi

# ---------------------------------------------------------------------------
# NOTE: the three lints below use bare `grep ... && bad` with no exclusion
# filter. They are clean today because no test asserts absence of these
# names. If future test code adds `assert!(!s.contains("billing:records"))`,
# `assert!(!s.contains("markup_product"))`, or
# `assert!(!s.contains("FreezeGuard::settle"))`, mirror the no_cost_micro
# whitelist pattern above to avoid a false positive.
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# no_billing_records_stream (§5.2) — stream renamed to usage:records.
# The `usage:records:dlq` fallback (G11) is a *separate* stream name and is
# unaffected by the literal `"billing:records"` grep below.
# ---------------------------------------------------------------------------
echo "[no_billing_records_stream] billing:records stream name must not appear"
if grep -rn '"billing:records"' crates/ 2>/dev/null; then
  bad "billing:records stream name remains"
else
  ok "no billing:records reference"
fi

# ---------------------------------------------------------------------------
# no_cost_in_dlq (G11) — the DLQ wrapper must not introduce cost fields.
# Rust never computes cost; the DLQ payload is the original UsageRecord
# wrapped with `dlq_meta`. `cost_micro` must not appear in flusher source.
# ---------------------------------------------------------------------------
echo "[no_cost_in_dlq] cost_micro must not appear in gw-ledger flusher source"
if grep -nE '\bcost_micro\b' crates/gw-ledger/src/flusher.rs 2>/dev/null; then
  bad "cost_micro field appears in flusher.rs"
else
  ok "no cost_micro in flusher.rs"
fi

# ---------------------------------------------------------------------------
# no_currency_in_lease (credit-lease §16) — the credit-lease layer (lease.rs,
# coef.rs, credit_estimator.rs, protection_config.rs) must not reference
# currency, prices, or micro-units. Go owns all monetary computation; Rust
# holds opaque "credits" only.
# Files may not exist yet (created by later tasks) — missing files are silently
# skipped. Comment-only lines (// or * line-leader) are exempt so doc-boundary
# explanations don't trip the lint.
# ---------------------------------------------------------------------------
echo "[no_currency_in_lease] credit lease modules must not reference currency or pricing"
_currency_hits=$(grep -iEHn 'usd|cny|micro|cost|price' \
    crates/gw-ledger/src/lease.rs \
    crates/gw-ledger/src/coef.rs \
    crates/gw-ledger/src/credit_estimator.rs \
    crates/gw-ledger/src/protection_config.rs \
    2>/dev/null \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' \
    | grep -vE '^[^:]+:[0-9]+:[[:space:]]*\*' \
    || true)
if [ -n "$_currency_hits" ]; then
  echo "$_currency_hits"
  bad "credit lease modules reference currency or pricing (Rust must hold opaque credits only)"
else
  ok "no currency/pricing references in credit lease modules"
fi

# ---------------------------------------------------------------------------
# no_global_hmac_force (G7) — RETIRED 2026-06-08. The global
# `[signature].required` toggle is now a supported feature
# (docs/spec/05-auth-permissions.md); the prior spellings this lint grepped for
# (signature_required_workspace_wide, etc.) never matched the shipped
# `cfg.signature.required` field, so the check was inert. No replacement:
# per-key `ApiKeyEntry.signature_required` and the global toggle are both
# legitimate.
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# no_freezeguard_settle (§5.2) — FreezeGuard::release replaces settle.
# ---------------------------------------------------------------------------
echo "[no_freezeguard_settle] FreezeGuard::settle must be absent"
if grep -rnE '\.settle\(.*actual|FreezeGuard::settle\(' crates/ 2>/dev/null; then
  bad "FreezeGuard::settle remains"
else
  ok "no FreezeGuard::settle reference"
fi

# ---------------------------------------------------------------------------
# no_markup_product_scalar (§5.2) — MergedPermissions field is markup_chain.
# ---------------------------------------------------------------------------
echo "[no_markup_product_scalar] markup_product scalar must be absent"
if grep -rnE '\bmarkup_product\b' crates/ 2>/dev/null; then
  bad "markup_product scalar remains"
else
  ok "no markup_product reference"
fi

# ---------------------------------------------------------------------------
# no_legacy_breaker (§5.2) — gw-health replaces breaker.rs.
# ---------------------------------------------------------------------------
echo "[no_legacy_breaker] BreakerState / BreakerStore must not appear"
if grep -rnE '\bBreakerState\b|\bBreakerStore\b' crates/ 2>/dev/null; then
  bad "BreakerState / BreakerStore remains"
else
  ok "no breaker types remain"
fi

# ---------------------------------------------------------------------------
# no_legacy_retry (§5.2) — retry.rs deleted; failover lives in Phase 4 R-E.
# ---------------------------------------------------------------------------
echo "[no_legacy_retry] gw-routing/src/retry.rs must not exist"
if [ -f crates/gw-routing/src/retry.rs ]; then
  bad "crates/gw-routing/src/retry.rs still present"
else
  ok "no retry.rs"
fi

# ---------------------------------------------------------------------------
# no_patch_body (§5.2) — adapter trait v2 has no patch_body / BodyPatch.
# ---------------------------------------------------------------------------
echo "[no_patch_body] patch_body / BodyPatch must not appear"
if grep -rnE '\bfn patch_body\b|\bBodyPatch::' crates/ 2>/dev/null; then
  bad "patch_body / BodyPatch remains"
else
  ok "no patch_body / BodyPatch references"
fi

# ---------------------------------------------------------------------------
# e2e_fake_no_gw_codec_dep — the two black-box test fakes
# (e2e-fake-upstream, e2e-fake-web) sit at the gateway's external boundaries
# and must stay independent of the gateway's own crates. They may depend on
# `gw-core` only (for `ids::*` newtypes); they MUST NOT pull in
# gw-codec / gw-proxy / gw-gateway / gw-auth / gw-store / gw-ledger.
# Sharing a codec or store crate would make the fakes co-evolve with the
# code under test and silently mask wire-format / persistence drift.
#
# We scan both Cargo.toml (dependency declarations) and src/tests (Rust
# `use` / path imports). The regex anchors on a word boundary before the
# crate name and either `_` or `-` as the separator so `gw-core` /
# `gw_core` is NOT flagged, but `gw-codec`, `gw_proxy::`, etc. are.
# ---------------------------------------------------------------------------
echo "[e2e_fake_no_gw_codec_dep] e2e fake crates must not depend on gateway codec/auth/store/ledger/proxy/gateway"
FAKE_FORBIDDEN_RE='\bgw[_-](codec|proxy|gateway|auth|store|ledger)\b'
if grep -rnE "$FAKE_FORBIDDEN_RE" \
     crates/e2e-fake-upstream/Cargo.toml \
     crates/e2e-fake-web/Cargo.toml \
     crates/e2e-fake-upstream/src crates/e2e-fake-upstream/tests \
     crates/e2e-fake-web/src 2>/dev/null \
   | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//' \
   | grep -q .
then
  grep -rnE "$FAKE_FORBIDDEN_RE" \
     crates/e2e-fake-upstream/Cargo.toml \
     crates/e2e-fake-web/Cargo.toml \
     crates/e2e-fake-upstream/src crates/e2e-fake-upstream/tests \
     crates/e2e-fake-web/src 2>/dev/null \
   | grep -vE '^[^:]+:[0-9]+:[[:space:]]*//'
  bad "e2e fake crate depends on / imports a forbidden gateway crate"
else
  ok "e2e fake crates carry no gateway codec/auth/store/ledger/proxy/gateway dep"
fi

# ---------------------------------------------------------------------------
# no_dead_tunnel_use_alias (spec §8) — `pub use gw_tunnel_client` in
# gw-gateway must coexist with TunnelStack references in gw-proxy. Either
# both present (tunnel wired) or both absent (feature removed).
# ---------------------------------------------------------------------------
echo "[no_dead_tunnel_use_alias] gw-tunnel-client re-export must align with TunnelStack use"
re_export_count=$(grep -rn 'pub use gw_tunnel_client' crates/gw-gateway/src/ 2>/dev/null | wc -l | tr -d ' ')
tunnel_stack_count=$(grep -rn 'TunnelStack' crates/gw-proxy/src/ 2>/dev/null | wc -l | tr -d ' ')
if { [ "$re_export_count" -eq 0 ] && [ "$tunnel_stack_count" -ne 0 ]; } \
   || { [ "$re_export_count" -ne 0 ] && [ "$tunnel_stack_count" -eq 0 ]; }; then
  bad "tunnel re-export ($re_export_count) and TunnelStack ($tunnel_stack_count) must be both zero or both nonzero"
else
  ok "tunnel re-export / TunnelStack alignment ok"
fi

# ---------------------------------------------------------------------------
# tunnel_require_means_hard_fail (spec §8) — require_tunnel must be paired
# with GW_TUNNEL_NO_NODE (no silent Direct fallback per ADR-T-4). Each
# use-site source line must have a `GW_TUNNEL_NO_NODE` within 8 lines (vline through vline+7) below.
# Skips:
#  - the field definition in gw-core::entity (docstring above defines the
#    contract; no use-site code nearby)
#  - persistence-layer JSON decode tests (loader.rs)
#  - resolver.rs doc comment that names both tokens on the same line
#  - the default-build predicate helper; the caller owns hard-fail rendering
#  - in-file unit-test struct literals
#  - pure comment lines (the documentation match noted in the plan)
# ---------------------------------------------------------------------------
echo "[tunnel_require_means_hard_fail] require_tunnel implies GW_TUNNEL_NO_NODE"
violations=""
while IFS=: read -r vfile vline _; do
  case "$vfile" in
    */entity.rs|*/loader.rs|*/resolver.rs) continue;;
    *_test.rs|*/tests/*) continue;;
  esac
  vcontent=$(sed -n "${vline}p" "$vfile" 2>/dev/null)
  case "$vcontent" in
    *'//'*) continue;;
    *'is_some_and(|p| p.require_tunnel)'*) continue;;
    *'require_tunnel: true'*) continue;;
  esac
  if ! sed -n "${vline},$((vline+7))p" "$vfile" 2>/dev/null | grep -q 'GW_TUNNEL_NO_NODE'; then
    violations="${violations}${vfile}:${vline}: require_tunnel without GW_TUNNEL_NO_NODE within 8 lines\n"
  fi
done < <(grep -rn --include='*.rs' -E '\brequire_tunnel\b' crates/ 2>/dev/null)
if [ -n "$violations" ]; then
  bad "$(printf '%b' "$violations")"
else
  ok "require_tunnel always paired with GW_TUNNEL_NO_NODE"
fi

# ---------------------------------------------------------------------------
# no_double_rustls (spec §8) — only one rustls version may appear in
# Cargo.lock. T4's hickory bump removed the 0.21 entry.
# ---------------------------------------------------------------------------
echo "[no_double_rustls] Cargo.lock must hold exactly one rustls version"
if grep -q '^name = "rustls"' Cargo.lock 2>/dev/null; then
  v_count=$(grep -A1 '^name = "rustls"$' Cargo.lock | grep '^version = ' | sort -u | wc -l | tr -d ' ')
  if [ "$v_count" -gt 1 ]; then
    bad "Cargo.lock has $v_count distinct rustls versions; want 1"
    grep -B1 -A1 '^name = "rustls"$' Cargo.lock
  else
    ok "single rustls version"
  fi
else
  note "rustls not in Cargo.lock; skipping"
fi

# ---------------------------------------------------------------------------
# no_v1_markers_in_prod (v2-cleanup overview §6.3) — production crates must not
# carry v1 placeholder markers. Two-mode toggle (GW_V2_GATE_MODE):
#   warn   (default)   — print hits, do NOT fail (current state during
#                        v2-cleanup; the harness still ends in `all active
#                        invariants hold` unless another lint fails).
#   error              — print hits + bad FAIL; fail counter increments.
# Any other value falls through to warn (so a typo in CI never breaks the gate).
# Spec §6.3 also describes a *phased* operational rollout — overview lands in
# warn, each sub-spec lands its tokens, the final sub-spec flips to error.
# That phasing is driven by changing the env var, not the script.
# Tracked tokens: unimplemented!(...), todo!(...), TODO(v2), wave-7, wave-8,
# "for v1:", "placeholder for v1".
# Excludes: anything under a tests/ directory (overview §1.2 carve-out for
# test fixtures named *Stub / Dummy* / placeholder_e2e_*).
# ---------------------------------------------------------------------------
v1_mode="${GW_V2_GATE_MODE:-error}"
echo "[no_v1_markers_in_prod] no v1 placeholder markers in crates/ (mode: $v1_mode)"
v1_hits=$(grep -rnE \
    -e 'unimplemented!\(' \
    -e 'todo!\(' \
    -e 'TODO\(v2\)' \
    -e 'wave-7' -e 'wave-8' \
    -e 'placeholder for v1' \
    -e 'for v1:' \
    --include='*.rs' \
    --exclude-dir=tests \
    crates/ 2>/dev/null || true)
if [ -n "$v1_hits" ]; then
  printf '%s\n' "$v1_hits" | sed 's/^/  /'
  if [ "$v1_mode" = "error" ]; then
    bad "v1 placeholder markers remain (gate mode: error)"
  else
    note "v1 placeholder markers remain (gate mode: warn — not failing)"
  fi
else
  ok "no v1 placeholder markers in crates/"
fi

# ===========================================================================
# PENDING lints — target code has not landed yet. Uncomment in the named phase.
# ===========================================================================
#
# # no_gateway_addr_config (Phase 6: activates with gw-task-worker; symmetric to no_worker_addr_config)
# grep -rnE '(^|[^[:alnum:]_])(gateway[_-]?host|gateway[_-]?port|GATEWAY_(URL|HOST))([^[:alnum:]_]|$)' crates/gw-task-worker 2>/dev/null && bad "worker config/code names a gateway address"
#
# # no_ir_runtime_struct (Phase 2: codec S1 v2 schema is compile-time only; A10 strict form re-timed)
# grep -rnE 'IrRequest::new|IrResponse::new|IrRequest::default|IrResponse::default' crates/ && bad "IR runtime instantiation remains"
#
# # deploy_e2e_matrix (Phase 6/7: ADR-T16 acceptance tests)
# for t in 1g_0w 0g_1w n_m gateway_kill worker_kill; do
#   grep -rq "e2e_deploy_matrix_$t" crates/ || bad "missing deploy matrix test: $t"
# done

# ---------------------------------------------------------------------------
# sensitive_filter_archive_boundary — archive masking must use the policy engine.
# Spec: docs/spec/06-billing-ledger.md
# ---------------------------------------------------------------------------
echo "[sensitive_filter_archive_boundary] archive masking must use sensitive filter engine only"
hits=$(grep -RnE 'redact_body|redact_headers|redact_message_content|redacted_preview|REDACT_HEADER_DENYLIST|REDACT_HEADER_ALLOWLIST' \
    crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v '^Binary file' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  bad "ad hoc detailed archive redaction helper present; use sensitive_filter engine"
else
  ok "no ad hoc detailed archive redaction helpers"
fi
grep -Rq "sensitive_filter" crates/gw-proxy/src || bad "sensitive filter archive boundary missing in gw-proxy"

# ---------------------------------------------------------------------------
# no_detailed_truncation — body preview / hard cap constants must not return.
# Spec: docs/spec/06-billing-ledger.md
# ---------------------------------------------------------------------------
echo "[no_detailed_truncation] truncation constants must not appear in gw-ledger/gw-proxy"
hits=$(grep -RnE 'PREVIEW_MAX_BYTES|MAX_DETAILED_LOG_BYTES' \
    crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v '^Binary file' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  bad "detailed archive truncation constants reintroduced"
else
  ok "no detailed-archive truncation constants in gw-ledger/gw-proxy"
fi

# ---------------------------------------------------------------------------
# no_legacy_usage_source — billing-robustness W4 retires usage_source field.
# UsageRecord must use vendor_exposure_state + usage_provenance + attempt_index.
# Spec: docs/spec/06-billing-ledger.md
# ---------------------------------------------------------------------------
echo "[no_legacy_usage_source] legacy usage_source field must not appear in gw-ledger/gw-proxy"
if grep -rn "usage_source\s*:" crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v test | grep -v "//" | grep -q .; then
  grep -rn "usage_source\s*:" crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v test | grep -v "//"
  bad "legacy usage_source field present (use vendor_exposure_state + usage_provenance + attempt_index)"
else
  ok "no legacy usage_source field"
fi

echo "[wire_enum_lock] ProtocolKind / ApiShape wire forms locked"
if cargo test -p gw-core --test wire_enum_lock --quiet >/dev/null 2>&1; then
    ok "ProtocolKind / ApiShape wire forms locked"
else
    bad "wire enum drift — see docs/architecture.md"
fi

# ---------------------------------------------------------------------------
# no_cached_body — ctx.cached_body removed in Phase B (2026-05-28).
# Use ctx.body (RequestBodyView) instead.
# Spec: docs/architecture.md
# ---------------------------------------------------------------------------
echo "[no_cached_body] ctx.cached_body removed in Phase B (2026-05-28); use ctx.body (RequestBodyView) instead"
if rg -n 'cached_body' --type rust crates/ >/dev/null 2>&1; then
    echo "  FAIL: 'cached_body' identifier references found:"
    rg -n 'cached_body' --type rust crates/ 2>/dev/null | sed 's/^/    /'
    bad "cached_body identifier found; use ctx.body (RequestBodyView) API instead"
else
    ok "no cached_body identifier in Rust crates"
fi

# ---------------------------------------------------------------------------
# managed_loop_advisory (warn-only) — long-loop tokio::spawn in gw-store /
# gw-ledger should migrate to ManagedLoop + Supervisor::spawn.
# Phase C of 2026-05-28-systemic-fixes migrated StreamConsumer; remaining
# raw spawns in these crates should follow.
# WARN-MODE ONLY: this lint MUST NOT set fail=1.
# ---------------------------------------------------------------------------
echo "[managed_loop_advisory] long-loop tokio::spawn in gw-store/gw-ledger should use ManagedLoop"
WHITELIST_REGEX='managed_loop\.rs|stream_consumer\.rs.*Supervisor::spawn|identity_persist\.rs'
hits=$(rg -n 'tokio::spawn\(' crates/gw-store/src crates/gw-ledger/src 2>/dev/null \
       | grep -vE "$WHITELIST_REGEX" || true)
if [ -n "$hits" ]; then
    echo "  WARN: non-ManagedLoop tokio::spawn in worker crates:"
    echo "$hits" | sed 's/^/    /'
    echo "  (advisory; not a hard failure during Phase C migration)"
else
    ok "no unmigrated long-loop tokio::spawn"
fi

# gw-runtime is the sole owner of `Supervisor`. Upper crates may import it
# but must never re-export their own Supervisor type — that would mask the
# bottom-layer primitive and let a parallel implementation drift back in.
# ---------------------------------------------------------------------------
# supervisor_reexport_ban — Supervisor must come from gw_runtime only
# ---------------------------------------------------------------------------
if grep -RInE 'pub use .*::supervisor::Supervisor' crates/ \
    --include='*.rs' \
    --exclude-dir=gw-runtime >/dev/null 2>&1; then
    grep -RInE 'pub use .*::supervisor::Supervisor' crates/ \
        --include='*.rs' \
        --exclude-dir=gw-runtime
    bad "Supervisor must come from gw_runtime; remove the re-export(s) above."
else
    ok "no rogue Supervisor re-exports"
fi

if [ "$fail" -ne 0 ]; then
  echo "INVARIANT CHECK FAILED"
  exit 1
fi
echo "all active invariants hold"
