#!/usr/bin/env bash
# Invariant lint harness for the v2 integration.
# Authoritative spec: docs/superpowers/specs/2026-05-20-v2-integration-design.md §3 / §5.2.
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
SUPPORTING="gw-core gw-codec gw-codec-macros gw-store gw-auth gw-ledger"
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

# ---------------------------------------------------------------------------
# no_billing_crate (§5.2) — gw-billing must never exist as a crate.
# ---------------------------------------------------------------------------
echo "[no_billing_crate] gw-billing must not be a workspace member or dependency"
if grep -rn 'gw-billing' Cargo.toml crates/*/Cargo.toml 2>/dev/null; then
  bad "gw-billing referenced in a Cargo.toml"
else
  ok "no gw-billing crate reference"
fi

# ---------------------------------------------------------------------------
# no_proxy_worker_dep (§5.2) — gw-proxy and gw-task-runtime must not interdepend.
# (gw-task-runtime does not exist yet; this trivially holds but we assert it so
#  the check is live the moment that crate appears.)
# ---------------------------------------------------------------------------
echo "[no_proxy_worker_dep] gw-proxy must not depend on gw-task-runtime"
if grep -rn 'gw-task-runtime' crates/gw-proxy/Cargo.toml 2>/dev/null; then
  bad "gw-proxy/Cargo.toml references gw-task-runtime"
else
  ok "gw-proxy does not depend on gw-task-runtime"
fi

# ---------------------------------------------------------------------------
# no_worker_addr_config / no_gateway_addr_config (§5.2, C-8)
# gateway config/code must not name a worker address, and vice versa.
# gw-task-worker does not exist yet, so only the gateway side is checkable now.
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
# ---------------------------------------------------------------------------
echo "[no_billing_records_stream] billing:records stream name must not appear"
if grep -rn '"billing:records"' crates/ 2>/dev/null; then
  bad "billing:records stream name remains"
else
  ok "no billing:records reference"
fi

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

if [ "$fail" -ne 0 ]; then
  echo "INVARIANT CHECK FAILED"
  exit 1
fi
echo "all active invariants hold"
