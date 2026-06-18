# Warn-Only Protocol

## 1. Start

Warn-only starts after the fix release is deployed to production and stable for at least one full business period. The default observation window is 14 days.

## 2. Metrics

| Audit | Metric | Threshold |
|---|---|---|
| R1 | `gw_auth_legacy_jwt_warn_total` | Less than 1% of relevant auth traffic. |
| R2 | `gw_auth_signature_replay_window_warn_total` | Product decision required; nonce enforcement may need a grace window. |
| R3 | `gw_auth_jwt_kid_fallback_warn_total` | Less than 1% of relevant auth traffic. |
| R4 | `gw_xff_trust_warn_total` | Zero for public-internet deployments. |

Metric names are protocol names. If WS-10 exposes different final names, update this table and the advisory Fix sections in the same PR.

## 3. Daily Check

Every 24h, the security owner records a short metric note in each advisory timeline.

## 4. Enforce Switch PR

The enforce switch is a separate PR. It must include metric history, a 7-day trend when available, and at least two reviewers including the WS-15 owner. Warn-only cannot extend beyond 60 days without reopening an incident.
