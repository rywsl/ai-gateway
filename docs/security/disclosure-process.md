# Disclosure Process

## 1. Embargo

- Start point: the first fix commit lands on `main` or a private fix branch.
- Default duration: 48h.
- Extension: at most +72h, with written approval from the team lead and security owner.
- Hard cap: 120h. If exceeded, publish or record an incident-level exception.

## 2. Release Gate

All items must be true before embargo is lifted:

- [ ] Fix deployed to production and stable for at least one full business period.
- [ ] Warn-only protocol started where required.
- [ ] Internal audit CSV and review file exist.
- [ ] Advisory draft has at least two approvers.
- [ ] CVE decision is recorded in advisory frontmatter.

## 3. RACI

| Role | Responsibility |
|---|---|
| Security owner | Draft advisories, drive CVE decision, coordinate external reporters. |
| Team lead | Accountable for embargo extension and publication approval. |
| WS-5 owner | Consulted on fix scope and deployment timing. |
| WS-15 owner | Consulted on warn-only and feature-flag timing. |
| WS-11 owner | Informed about frontend or SDK migration windows. |
| Support | Informed and executes approved customer notifications. |

During embargo, do not discuss exploit details in public issues, support tickets, status pages, or social channels.
