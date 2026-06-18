# WS-15 Feature-Flag Runbook

## 1. Flag Inventory

| Flag | Type | Current default | Owner WS | Mixed-version safe |
|---|---|---|---|---|
| license_enforce | enum | warn | WS-15, WS-16 | yes |
| jwt_strict | bool | false | WS-5, WS-15 | yes |
| payment_webhook_5xx | bool | false | WS-1, WS-15 | yes |
| r7_502_on_parse_fail | bool | false | WS-3, WS-6, WS-15 | no |

## 2. Deploy Ordering

### Switching `license_enforce` From `warn` To `enforce`

1. Confirm the 14-day warn-only window and customer notice are complete.
2. Confirm `license_block_total{mode="warn"}` and violation reasons are below
   the launch threshold for seven days.
3. Deploy the Go side first with `ALM_FLAGS__LICENSE_ENFORCE=enforce`.
4. Wait five minutes and confirm admin/API traffic has no unexpected 402 spike.
5. Deploy the Rust side with `GW_FLAGS__LICENSE_ENFORCE=enforce`.
6. Hold the canary cell for 24 hours before global rollout.

### Rolling Back `license_enforce`

1. Roll Rust back first: set `GW_FLAGS__LICENSE_ENFORCE=warn` and restart.
2. Roll Go back second: set `ALM_FLAGS__LICENSE_ENFORCE=warn` and restart.
3. Watch `license_block_total{mode="enforce"}` stop increasing.
4. If customers remain blocked, set both sides to `disabled` and open a WS-16
   incident.

### Other Flags

1. `jwt_strict`: set `GW_FLAGS__JWT_STRICT=false` and restart Rust pods.
2. `payment_webhook_5xx`: set `ALM_FLAGS__PAYMENT_WEBHOOK_5XX=false` and
   restart Go pods.
3. `r7_502_on_parse_fail`: deploy the WS-11 frontend first, then set
   `GW_FLAGS__R7_502_ON_PARSE_FAIL=true`; rollback by setting it false.

## 3. Incident Playbooks

### 3.1 402 Spike After `license_enforce`

- Symptom: `license_block_total{mode="enforce"}` or public 402s rise above the
  canary threshold.
- Action: run the `license_enforce` rollback above.
- Verify: 402 rate returns to baseline and warn-mode metrics still expose the
  violation reasons.

### 3.2 401 Rate After `jwt_strict=true`

- Symptom: public 401 rate rises after the strict JWT flag flips.
- Action: set `GW_FLAGS__JWT_STRICT=false` and restart Rust pods.
- Verify: strict reject counters stop increasing and affected tokens are
  accepted by the legacy-compatible path.

### 3.3 Payment Webhook 502 Storm

- Symptom: `payment_webhook_5xx` produces sustained provider retry alerts.
- Action: set `ALM_FLAGS__PAYMENT_WEBHOOK_5XX=false` and restart Go pods.
- Verify: provider retries stop, and any persisted webhook idempotency rows are
  reconciled before manual replay.

### 3.4 Frontend Errors After `r7_502_on_parse_fail`

- Symptom: frontend reports a 502 surge immediately after the Rust flag flips.
- Action: set `GW_FLAGS__R7_502_ON_PARSE_FAIL=false` and restart Rust pods.
- Verify: client-visible 502s stop while parse-failure metrics remain visible
  for root-cause analysis.

## 4. Mixed-Version Matrix

| Go version | Rust version | license_enforce | jwt_strict | payment_webhook_5xx | r7_502_on_parse_fail |
|---|---|---|---|---|---|
| N | N | any | any | any | any after WS-11 frontend |
| N+1 | N | disabled or warn | false | true | false |
| N | N+1 | disabled or warn | false | any | false |
| N+1 | N+1 | any | any | any | any after WS-11 frontend |

Forbidden combinations are enforced by `scripts/check-mixed-version-flags.sh`
and deployment review. The `r7_502_on_parse_fail` flag is not mixed-version
safe because clients need the WS-11 frontend contract update first.
