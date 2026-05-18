# Issue: status_class bucket collapses all short-circuits to "err"

## Symptom

After eight short-circuit requests in Phase B (six 401s, one 400, one
400 from unknown-protocol detection), `/metrics` reported:

```text
# TYPE gateway_started_total counter
gateway_started_total 1

# TYPE gateway_requests_total counter
gateway_requests_total{status_class="err"} 8
```

All eight requests landed in the single `status_class="err"` bucket
even though their actual HTTP responses spanned 400 and 401. The
metric becomes useless for distinguishing "client error" vs
"upstream error" vs "gateway 5xx" when most traffic is short-circuit.

## Root cause

`crates/gw-proxy/src/filters/logging.rs:42-53`:

```rust
let status_class = match (err.is_some(), ctx.upstream_status) {
    (_, Some(s)) => match s / 100 { 1=>"1xx", 2=>"2xx", 3=>"3xx", 4=>"4xx", 5=>"5xx", _=>"other" },
    (true, None)  => "err",
    (false, None) => "early",
};
```

For short-circuit error paths (auth/codec/quota rejection), Pingora
never reaches the upstream so `ctx.upstream_status` stays `None`, and
`request_filter` returns `Err(short_circuit(status))` so `err` is
`Some(...)`. The branch hit is `(true, None) => "err"`.

The actual HTTP status the client received IS carried inside the
`pingora::Error::HTTPStatus(status)` returned by `short_circuit(...)`
(see `crates/gw-proxy/src/filters/early.rs:39-45`) — we just never
extract it. The data is there, the metric label code just doesn't
read it.

A second issue: `ctx.upstream_status` is named after the upstream
hop, but the metric pretends to be a general request-status label.
For successful pass-through requests the metric would be correct;
for short-circuit it's blind to the real 4xx/5xx breakdown. The
`gateway_requests_total{status_class=...}` metric as currently
wired only differentiates between successful upstream responses,
not between gateway-internal rejection categories.

## Suggested fix (do not implement here — recording only)

In `logging.rs`, when `err.is_some()` and `upstream_status` is
`None`, downcast the `pingora::Error` to inspect its
`ErrorType::HTTPStatus` and use that to bucket as 4xx/5xx. The
`short_circuit` helper already encodes the right status; we just
need to surface it. A second option is to stash the status into a
new `ctx.short_circuit_status: Option<u16>` field at every emit
site, which is more explicit but more invasive.

## Cardinality check

The label `status_class` ranges over {`1xx`,`2xx`,`3xx`,`4xx`,
`5xx`,`other`,`err`,`early`} — eight values, bounded. No
high-cardinality risk. The metric registry currently exposes only
two series total (`gateway_started_total`, `gateway_requests_total`)
and no labels carry user/key/model identifiers, which matches the
ADR-014 cardinality guidance. PASS on cardinality, FAIL on
expressiveness for short-circuit paths.

## Files referenced

- `crates/gw-proxy/src/filters/logging.rs:42-58`
- `crates/gw-proxy/src/filters/early.rs:39-45`
- `crates/gw-proxy/src/ctx.rs:86,122`
