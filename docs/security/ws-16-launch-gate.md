# WS-16 Launch Gate

Status: BLOCKED_EXTERNAL_RELEASE_GATES

This file separates local artifact completeness from launch approval. Local artifact checks may pass while launch remains blocked.

## Local Artifact Gate

- [x] Advisory R1-R4 files exist.
- [x] Advisory R1-R4 frontmatter includes `cve_id` and `status`.
- [x] Advisory R1-R4 status is `embargoed`, not `draft`.
- [x] Internal-audit CSV and review files exist for R1-R4.
- [x] Review files explicitly mark external production-data audit status.
- [x] CVE decision path is documented.
- [x] Warn-only protocol is documented.
- [x] CHANGELOG security entries exist for R1-R4.
- [x] `SECURITY.md` is present and mirrored in both repositories.
- [x] Process docs exist: disclosure process, CVE decision tree, external reporter SOP, warn-only protocol.
- [x] INVESTIGATE-WS16-A through INVESTIGATE-WS16-E records exist.

## Launch Blockers

- [ ] R1 advisory `status: released`.
- [ ] R2 advisory `status: released`.
- [ ] R3 advisory `status: released`.
- [ ] R4 advisory `status: released` or written decision to keep it internal-only.
- [ ] R1-R4 affected-customer CSVs are regenerated from an approved production read-only source.
- [ ] R1-R4 audit review files contain two real reviewers.
- [ ] R1-R3 CVE decisions are changed from `pending` to either `CVE-...` or `not-requested`.
- [ ] Fix release version and production rollout evidence are written into each advisory.
- [ ] Warn-only metric protocol is active or completed where required.

Launch cannot rely on `draft` advisories. An `embargoed` advisory requires a team-lead and security-owner written extension, and the extension must reference the blocker above.
