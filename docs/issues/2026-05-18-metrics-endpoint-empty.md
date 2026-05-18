# Issue: /metrics endpoint returns 0 bytes

## Symptom

During the post-build Docker smoke test the metrics endpoint returned
`200 OK` with `content-length: 0`, even after sending requests that
exercised the proxy:

```text
$ curl -i http://localhost:9090/metrics
HTTP/1.1 200 OK
content-type: text/plain
content-length: 0
```

Requests that should have produced telemetry (`POST /v1/chat/completions`
returning `401`) did not change the response — the body stayed empty.

## Root cause

`metrics-exporter-prometheus` returns the rendered contents of the
**global metrics registry** on every scrape. Until at least one metric
has been registered (i.e. some code path has called `metrics::counter!`
/ `metrics::gauge!` / `metrics::histogram!` and incremented or recorded
a value), the registry is empty and the exporter responds with a
zero-byte body. A healthy 200/empty response is therefore
*indistinguishable* from a broken exporter — the listener is up, the
global recorder is installed, the registry just has nothing to render.

Walking the code paths that emit metrics on this branch
(`feat/llm-gateway-impl`):

- `crates/gw-ledger/src/freeze.rs` — only on freeze-leak recovery, which
  requires a billed request to have completed.
- `crates/gw-ledger/src/flusher.rs` — channel-full / billing-error
  paths, again gated on real billing traffic.
- `crates/gw-proxy/src/filters/logging.rs:223` — `gateway_tpm_consumed_total`,
  but the emit is **nested** inside a four-tuple guard at line 85:
  `if let (Some(auth_user_id), Some(effective), Some(model), Some(picked)) = …`.
  All four of those are populated *only* after auth + routing succeed.

For a 401 short-circuit the request never reaches routing, so
`merged_perm` / `picked_vendor_key` / `model` are all `None`, and the
guard never opens. A gateway that has only served 4xx responses since
boot therefore emits **zero** metric calls, leaving the registry empty.

The `PrometheusBuilder::new().with_http_listener(addr).install()` call
in `crates/gw-gateway/src/observability.rs:42-45` was working
correctly. The HTTP listener was up (proven by the 200 response) and
the global recorder was installed (proven by the next step). No version
or path mismatch was involved.

## Reproduction

```text
$ docker compose up -d
[gateway] Up (healthy)

# Before any traffic
$ curl -i http://localhost:9090/metrics
HTTP/1.1 200 OK
content-type: text/plain
content-length: 0

# Trigger 401 short-circuits and a 400
$ curl -s -o /dev/null -w "%{http_code}\n" \
    -X POST http://localhost:8000/v1/chat/completions \
    -H "Content-Type: application/json" \
    -d '{"model":"gpt-4","stream":true,"messages":[]}'
401

$ curl -s -o /dev/null -w "%{http_code}\n" \
    -H "Authorization: Bearer fake" \
    -X POST http://localhost:8000/v1/chat/completions \
    -H "Content-Type: application/json" -d '{}'
401

$ curl -s -o /dev/null -w "%{http_code}\n" http://localhost:8000/
400

# After 3 short-circuits — still empty on the original code
$ curl -i http://localhost:9090/metrics
HTTP/1.1 200 OK
content-type: text/plain
content-length: 0
```

Gateway logs confirmed the requests reached the proxy and were rejected
in `request_filter` / `early_request_filter`:

```text
ERROR Fail to filter request: HTTPStatus context: gateway short-circuit
      cause: gw_short_circuit, status: 401, POST /v1/chat/completions
```

Pingora *does* fire its `logging` callback for these short-circuit
responses (verified by inspecting `crates/gw-proxy/src/gateway.rs:177`
and pingora's behavior of calling `logging` for every completed
request, including failures from `request_filter`). The callback runs;
it just falls through the four-tuple guard without emitting anything.

## Fix

Two minimal additions, both targeting the same one-line concern: make
sure at least one metric is registered on a path that always runs.

1. `crates/gw-gateway/src/observability.rs` — after a successful
   `PrometheusBuilder::install()`, increment a `gateway_started_total`
   counter. Guarantees `/metrics` is non-empty before any traffic and
   doubles as a restart-counting signal.

2. `crates/gw-proxy/src/filters/logging.rs` — at the very top of
   `run()`, emit `gateway_requests_total{status_class=...}` *before*
   the existing four-tuple `Some(..)` guard. Pingora calls `logging`
   on every completed request (including short-circuits), so this
   counter increments on every code path. The label `status_class` is
   one of `1xx`/`2xx`/`3xx`/`4xx`/`5xx`/`other`/`err`/`early`, which
   keeps cardinality bounded.

See the commit on `feat/llm-gateway-impl` for the exact diff.

No changes to `Dockerfile`, `docker-compose.yml`, `Cargo.toml`, or
`Cargo.lock`.

## Verification

After rebuilding and restarting via `docker compose build gateway &&
docker compose up -d`, the endpoint immediately returns a non-empty
body, and the request counter increments as expected:

```text
$ curl http://localhost:9090/metrics
# TYPE gateway_started_total counter
gateway_started_total 1

# After 3 short-circuit requests:
$ curl http://localhost:9090/metrics
# TYPE gateway_requests_total counter
gateway_requests_total{status_class="err"} 3

# TYPE gateway_started_total counter
gateway_started_total 1
```

The `status_class="err"` label is correct: pingora's `request_filter`
short-circuit path returns an `HTTPStatus` `pingora::Error` to the
proxy machinery, which is then passed to the `logging` callback as
`Some(err)`. The classifier sees `err.is_some()` with no
`upstream_status` recorded (we never reached `response_filter` since
no upstream was contacted) and selects `"err"`. A successful upstream
response would yield `"2xx"`.

## Why this wasn't caught earlier

- The two unit tests in `gw-proxy/tests/` that touch the logging path
  (`error_emit.rs`, `freeze_lifecycle.rs`, `end_to_end_openai.rs`) do
  not scrape `/metrics`. They verify behavior of `ctx`, the ledger
  channel, and the wire codec, but never check that the exporter
  registry contains anything.
- The closest thing to an e2e smoke test was the manual
  `docker compose` run that produced this issue, and it did not assert
  on the metrics body — only on the HTTP status code.
- `PrometheusBuilder::install()` returns `Ok(())` even when the
  registry is empty; there is no boot-time signal that scrapes will be
  blank until traffic registers something.

## Follow-up

- Add an integration test that boots the gateway, makes one request,
  scrapes `/metrics`, and asserts on the presence of
  `gateway_started_total` and `gateway_requests_total`. This is
  doable in the existing `crates/gw-proxy/tests` harness if the
  gateway is started as a child process, or in `crates/gw-gateway/tests`
  using `testcontainers`.
- Consider promoting more of the existing internal counters
  (`gateway_billing_*`, `gateway_freeze_leak_recovered_total`) to also
  register at startup with `metrics::describe_counter!` so their
  `# TYPE` lines appear in the exposition even when the value is
  still zero. This makes Prometheus alerts on
  `rate(gateway_billing_records_lost_total[5m])` deploy-safe — they
  won't fire `no_data` on a healthy idle gateway.
- The TPM counter and BillingRecord emit are still gated on the same
  four-tuple guard. That is correct (no point billing a request that
  never picked an upstream), but it would be worth adding a parallel
  counter dimension for short-circuit reasons (`auth_failed`,
  `rate_limited`, `codec_error`, …) so ops can graph why short-circuits
  are happening without grepping logs.
