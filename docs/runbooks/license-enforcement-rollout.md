# License Enforcement Rollout

This runbook coordinates the REV-FINAL-LICENSE-COMM rollout: ship license
enforcement in `warn` mode, observe for 14 days, notify affected customers, then
flip to `enforce` only after support and SRE sign off.

## Preconditions

- License middleware is deployed with `license_enforce=warn`.
- `license_block_total{mode="warn"}` is visible in Prometheus.
- Customer status export identifies unlicensed or invalid-license instances.
- `docs/comms/2026-06-17-license-enforcement.md` has been reviewed by support.

## Schedule

| Day | Action | Owner | Verification |
|---|---|---|---|
| D0 | Deploy `warn` mode | SRE on-call | `license_block_total{mode="warn"}` increments for would-block traffic |
| D0+1 | Send first customer notice batch | Comms | Affected-customer list has sent timestamps |
| D7 | Send second reminder | Comms | Email metrics captured |
| D12 | Send final reminder | Comms | Support queue has no unresolved blockers |
| D14 | Flip `enforce` in staging | SRE | Staging unlicensed request returns HTTP 402 |
| D14+24h | Flip `enforce` in production | SRE | Production metric switches to `license_block_total{mode="enforce"}` |
| D15 | Review unexpected 402 rate | SRE + Support | Incident notes or no-incident signoff recorded |

## Roll Forward

1. Confirm the D14 checklist is complete.
2. Set `license_enforce=enforce` in staging and run the license contract smoke.
3. Watch `license_block_total{mode="enforce"}` and frontend 402 handling.
4. Apply the same config to production during the approved window.
5. Keep support staffed for the first 24 hours after production enforce.

## Emergency Rollback

1. Set `license_enforce=warn`.
2. Confirm requests no longer return HTTP 402 for unlicensed instances.
3. Keep `license_block_total{mode="warn"}` visible so support can continue
   contacting affected customers.
4. Open a root-cause ticket before another enforce attempt.

## Alert Handling

| Symptom | Action |
|---|---|
| 402 rate spikes above expected affected-customer count | Roll back to `warn`, export affected tenants, contact support |
| Valid license reports blocked | Roll back to `warn`, collect license status payload, assign license owner |
| Frontend shows generic failure instead of license message | Keep `warn` or roll back, assign WS-11 owner |

## Verification

- Staging `warn`: unlicensed request succeeds and sets warning signal.
- Staging `enforce`: unlicensed request returns HTTP 402 with license reason.
- Production `warn` to `enforce` change has a timestamped SRE signoff.
- Rollback path was rehearsed in staging before production enforce.

## Owner

SRE owns the flag flip. Comms owns customer notices. Support owns affected
customer follow-up. Security owns launch-gate signoff.
