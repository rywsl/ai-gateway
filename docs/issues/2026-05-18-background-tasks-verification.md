# Issue: background tasks — only Ledger flusher actually runs

## What was checked

After Phase B/D exercised the proxy with 9 short-circuit requests
(none of which reach billing), I inspected the gateway logs and
Redis state for evidence of the four documented background tasks:

| Task | Defined in | Started in bootstrap? |
| :--- | :--- | :--- |
| Health server (`/livez`/`/readyz`) | `gw-gateway/src/health.rs` | yes (`tokio::spawn` line 211) |
| Ledger flusher (mpsc → Redis pipeline) | `gw-ledger/src/flusher.rs` | yes (line 200) |
| Stream consumer (entity/counter events) | `gw-store/src/stream_consumer.rs` | **no** |
| Cold-loader on cache miss | `gw-store/src/loader.rs` | **no** (see Phase D issue) |

## Stream consumer

```text
grep -rn 'StreamConsumer::start\|StreamConsumer::new' crates/gw-gateway/
# (no matches)
```

The `StreamConsumer` type is defined in
`crates/gw-store/src/stream_consumer.rs` and its `start()` method
creates the `events:entity` / `events:counter` consumer groups, but
its `consume_loop` is a `loop { sleep(1s); trace!(...) }` placeholder
("XREADGROUP not yet implemented", line 39 of that file). Even if it
were started, no events would propagate to `EntityStore`.

Confirmed by Redis state after running 9 requests:

```text
$ redis-cli XLEN events:entity     -> 0
$ redis-cli XLEN events:counter    -> 0
$ redis-cli KEYS '*'
  entity:user:user-001                       (seeded by hand in Phase D)
  entity:apikey:ak-test-001                  (seeded by hand in Phase D)
  counter:user:user-001:balance_micro        (seeded by hand in Phase D)
```

No consumer-group keys (`xpending`, `xinfo groups`) were created
because nobody called `xgroup_create`. The two streams themselves
do not exist either.

## Ledger flusher

The flusher task IS spawned at `gw-gateway/src/lib.rs:200`. It owns
an mpsc `Sender<BillingRecord>` and a tokio task that drains it
into a Redis pipeline (`INCRBY` balance + `XADD billing:records`).
No records were emitted because no request reached the post-routing
emit site in `gw-proxy/src/filters/logging.rs:151`:

```text
$ redis-cli XLEN billing:records     -> 0
```

The flusher is "started" in the sense that it's running and parked
on `mpsc::Receiver::recv`. There is currently no startup log line
proving it — bootstrap should emit one for ops visibility. Not a
correctness issue.

## Health server

Confirmed via curl in Phase A: `/livez` 200, `/readyz` 200 (after
the `READY` AtomicBool flips in `gw-gateway/src/lib.rs:154`).

## Cold-loader on cache miss

Not started; not invoked from any hot-path filter. Documented in the
companion Phase D issue.

## Summary

Of four advertised background subsystems, two are fully wired (health
server, ledger flusher), one is wired but is a no-op placeholder
(stream consumer is started — actually not even started; defined but
not started), and one is not wired at all (cold-loader). For a v1
that intends to authenticate real users, the stream consumer and
cold-loader must both come online before the warm path is reachable
from the network. v1 documentation should call these out as known
gaps; the verification confirms the gaps match what the source code
already admits.

## Files referenced

- `crates/gw-store/src/stream_consumer.rs:22-51`
- `crates/gw-gateway/src/lib.rs:200-219`
- `crates/gw-ledger/src/flusher.rs:26-59`
