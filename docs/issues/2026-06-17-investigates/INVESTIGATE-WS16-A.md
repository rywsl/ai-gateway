# INVESTIGATE-WS16-A: Repository Visibility And CVE Decision

Status: BLOCKED_EXTERNAL_DECISION

Local finding: this checkout cannot prove whether `ailinkmax-gateway`, derived release artifacts, or customer-deployed builds are public. Therefore R1, R2, and R3 advisories keep `cve_id: pending` until the security owner records the repository and release visibility decision.

Decision rule: if the vulnerable code or affected release is public, file separate CVE decisions for R1, R2, and R3. R4 remains `not-requested` by default because local source review classifies it as a default-safe P1 configuration footgun, unless security owner evidence shows public-internet exposure with `trust_xff = true`.

Required external evidence before release:
- Repository visibility or release distribution statement from the security owner.
- CNA route, if filing: GitHub Security Advisory / GHSA when available, otherwise MITRE direct submission.
- Advisory frontmatter updated from `pending` to `CVE-...` or `not-requested`.
