# INVESTIGATE-WS16-E: Historical Customer Releases

Status: BLOCKED_EXTERNAL_TELEMETRY

Local finding: the checkout contains no authoritative customer deployment inventory, release telemetry, or production version matrix. The advisory `Affected Versions / Configurations` sections therefore describe configuration classes, not a final customer release range.

Required external evidence before release:
- List of customer-deployed gateway versions or image digests.
- Mapping from each deployed version to R1-R4 exposure.
- Security-owner sign-off on the advisory affected-version text.

Until that evidence exists, WS-16 is locally prepared but not launch-complete.
