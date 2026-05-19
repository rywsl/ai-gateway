# StreamConsumer XREADGROUP parse error against redis:7-alpine

**Filed:** 2026-05-19
**Severity:** Low (single-pod deployments unaffected; cross-pod invalidation only)
**Status:** Open

## Symptom

Every poll cycle, the gateway logs the following warning against `redis:7-alpine`:

```
xreadgroup failed; backing off — Parse Error: Cannot convert to map.
```

The error repeats indefinitely on the StreamConsumer's polling interval, with no successful reads.

## Repro

```bash
docker compose up -d gateway redis
docker compose logs -f gateway | grep xreadgroup
```

Observe the parse-error line on every BLOCK_MS interval.

## Impact

- Local in-process caches still work via TTL (entity cache 300s, counter cache 1800s, model_group cache 3600s).
- **Cross-pod cache invalidation does NOT propagate** because the StreamConsumer never successfully consumes `events:entity` / `events:counter` messages.
- **Single-pod deployments are unaffected** — the in-process cache and the writer are co-located, so stale reads never cross a pod boundary.
- Multi-pod deployments will see stale cache entries until TTL expiry.

## Hypothesis

fred 9.4's `xreadgroup_map` deserializer expects a different reply shape than what `redis:7` returns when both streams are present but contain no new messages. Two plausible causes:

1. **RESP2 vs RESP3 protocol mismatch.** fred 9.x defaults to RESP2 unless `PerformanceConfig::protocol = Resp3` is set explicitly. The `xreadgroup_map` helper may assume the RESP3 map type for empty replies.
2. **Empty-result handling.** When no messages are available within `BLOCK_MS`, redis returns a nil-array reply. fred's map decoder may misinterpret nil as a malformed map instead of an empty result.

Either way, the failure is on the empty-reply path — the polling loop never sees any real messages because the parse error fires first.

## Recommended next steps (NOT to be fixed in this branch)

1. **Confirm wire-level shape.** Run:
   ```bash
   docker compose exec redis redis-cli XREADGROUP GROUP gw-X gw-X-consumer COUNT 64 BLOCK 200 STREAMS events:entity events:counter '>' '>'
   ```
   Capture whether the empty reply is `(nil)`, an empty array, or something else.

2. **If empty replies are the trigger**, switch to the raw `xreadgroup` (returning `Vec<XReadValue>`) and decode by hand, OR wrap the call in `Option`-aware logic that treats parse failures on empty replies as "no messages."

3. **If RESP-mismatch is the cause**, set `PerformanceConfig::default().protocol = Resp3` in `crates/gw-store/src/redis_client.rs` and re-test. Redis 7 supports RESP3 via `HELLO 3`.

## Workaround

None needed for the v1 single-pod deployment. Document this under "Known v1 limitations" in the README.
