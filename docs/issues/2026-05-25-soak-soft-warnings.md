# 2026-05-25 e2e soak soft warnings

Full `make e2e-soak` completed successfully on git `94f8be6`:

- `test_soak_smoke` PASS
- `test_soak_chaos_off_60s` PASS
- `test_soak_chaos_heavy_60s` PASS

No blocking defect or hard invariant failure was found. The run did surface the
following non-blocking warnings that should be investigated separately.

## Low 2xx success rate under chaos-off

Report: `tests/e2e/client/.soak_report/2026-05-25T08-48-05Z-chaos-off-60s.json`

- Sent/completed: `49,980 / 49,980`
- Status split: `200=16,652`, `503=33,328`
- Soft warning: `success_rate_2xx_pct.actual_pct=33.32`, threshold `99.9`
- Gateway error signal: `GW_ROUTING_NO_AVAILABLE_CHANNEL=8,134`
- Accounting remained consistent: `usage_records_delta=16,652`, upstream hits `16,652`, diff `0`

Because this appears in `chaos_mode=off`, it is not explained by injected
upstream chaos. It may indicate seed capacity, routing health/cooldown, or soak
driver pressure causing channel exhaustion.

## High p99 latency under chaos-off

Same report:

- Soft warning: `latency_p99_ms.actual_ms=5704.878041000484`, threshold `200`
- Max latency: `12372.943499998655 ms`

This is also present without injected chaos and should be triaged with the low
success rate above.

## Heavy chaos records-to-2xx ratio warning

Report: `tests/e2e/client/.soak_report/2026-05-25T08-53-58Z-chaos-heavy-60s.json`

- Sent/completed: `49,980 / 49,980`
- Status split: `200=14,178`, `429=862`, `502=320`, `503=33,382`
- Client-side failures: `1,238`
- Soft warning: `records_to_2xx_ratio.ratio=1.171`, threshold `1.1`
- Accounting hard invariant still passed: `usage_records_delta=16,598`, upstream hits `16,598`, diff `0`

The hard accounting equality passed, so this does not block continued soak use.
It likely reflects successful billing records for some non-2xx upstream attempts
under heavy chaos and should be classified as either expected semantics or a
threshold/spec mismatch.

## Observability gap in RSS invariant

Both full reports show `rss_before_bytes=0` and `rss_after_bytes=0` while the
`rss_growth_under_threshold` invariant passes. This means the current soak run
did not actually measure gateway RSS growth, even though the invariant is
reported as passed.

This is a soak-harness observability gap, not a gateway blocker.
