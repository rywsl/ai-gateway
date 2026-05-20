# Pingora logs every short-circuit at ERROR level

**Filed:** 2026-05-19
**Severity:** Medium (alerting noise; clean responses to clients)
**Status:** Open

## Symptom

Every short-circuit response (auth failure, codec rejection, upstream-filter
short-circuit) produces a `pingora_proxy` ERROR-level log line on top of
whatever JSON envelope `error_emit::send_error` wrote to the client.

Reproduced against the v1.1 image with a mix of 9 short-circuit requests
(3 auth/protocol 4xx + 6 warm-path 503s):

```
$ docker compose logs gateway 2>&1 | grep '"level":"ERROR"' | wc -l
9
```

Sample line:

```json
{"level":"ERROR","fields":{"message":"Fail to filter request:  HTTPStatus
context: gateway short-circuit cause:  gw_short_circuit, status: 401, POST
/v1/chat/completions, Host: localhost:8000"},"target":"pingora_proxy"}
```

## Root cause

`pingora_proxy` calls `error!()` whenever `request_filter` /
`upstream_peer` / `early_request_filter` returns `Err(...)`. The default
`ProxyHttp::suppress_error_log(session, ctx, error) -> bool` returns
`false`, so the gateway's intentional short-circuits land in the error
stream alongside *real* gateway bugs.

For client-induced 4xx and the 503s that have already produced a
structured JSON envelope, ERROR is the wrong severity:

- The client already has a per-vendor error envelope on the wire — there's
  no recovery action for an operator.
- Mixing intentional rejections with real `Fail to proxy` errors makes the
  log stream useless for triage and breaks any alerting that pages on
  ERROR volume.

## Impact

- **Alert noise.** Any ERROR-volume alert (e.g. SLOs that page on
  >N ERRORs/min) trips on normal traffic shape.
- **Log spend.** A misconfigured client doing 100 req/s of bad auth burns
  ~6k ERROR entries/min on `pingora_proxy`.
- **Triage signal lost.** Real bugs (e.g. `gw_no_model_in_ctx` from a
  filter-ordering regression) drown in client-fault noise.

## Fix

Implement `ProxyHttp::suppress_error_log` on `LlmGateway` to silence the
known short-circuit shape. The `short_circuit` helper in
`crates/gw-proxy/src/filters/early.rs` always builds:

```rust
Error::because(
    ErrorType::HTTPStatus(status),
    "gateway short-circuit",
    Error::new(ErrorType::Custom("gw_short_circuit")),
)
```

So suppression matches the outer `etype == HTTPStatus(_)` AND the inner
`cause.etype == Custom("gw_short_circuit")`. That's specific enough that
genuine `Fail to proxy` errors (connect refused, upstream timeout,
pingora-internal failures) still log at ERROR.

The `logging` filter still receives the error and increments
`gateway_requests_total{status_class=...}` (with the correct 4xx / 5xx
bucket per Task 4 of the 2026-05-19 warm-path fix), so observability does
not regress.

## Verification

After the fix, the same 9-request smoke produces zero
`pingora_proxy` ERROR lines. Metrics still bucket correctly:

```
gateway_requests_total{status_class="4xx"} 3
gateway_requests_total{status_class="5xx"} 6
```

## Resolution (2026-05-19+)

Resolved by implementing `ProxyHttp::suppress_error_log` on `LlmGateway`
in `crates/gw-proxy/src/gateway.rs`. The predicate uses pingora's
`Error::root_etype()` to walk the cause chain: suppression fires only
when the outer `etype` is `HTTPStatus(_)` AND the root cause's `etype`
is exactly `Custom("gw_short_circuit")` — the marker that
`filters::early::short_circuit` stamps on every gateway-authored
rejection. Any other error (upstream connect refused, upstream timeout,
an internal sanity-check that bubbles a bare `HTTPStatus` without our
marker cause) falls through to the default `false` and still logs at
ERROR.

Live-verified against the docker compose stack with a mix of 3
auth/codec 4xx + 5 warm-path 503s:

| Stream                                            | Pre-fix | Post-fix |
| ------------------------------------------------- | ------- | -------- |
| `docker compose logs gateway \| grep '"level":"ERROR"' \| wc -l` | 9       | 0        |

Post-fix `gateway_requests_total` partitions correctly:

```
gateway_requests_total{status_class="4xx"} 3
gateway_requests_total{status_class="5xx"} 5
```

(Smoke shape differs by one 503 vs the original 6-503 repro because
`scripts/smoke-warm-path.sh` only fires one 503 itself; the four
follow-up curl loops bring the total to five. Bucket partitioning is
the load-bearing check, not absolute counts.)
