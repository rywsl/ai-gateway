# Issue: hot-path auth has no PG/Redis cold loader — no real request can pass auth

## Symptom

Seeded `users` and `api_keys` in PG with a valid SHA-256 hash for
`x-api-key: test-key-001`, also wrote the same payload directly into
Redis at `entity:apikey:ak-test-001` (the L1 key shape `load_entity`
expects). Then sent:

```
POST /v1/chat/completions
x-api-key: test-key-001
x-api-key-id: ak-test-001
```

Expected: auth passes, downstream filter (model resolution / routing
/ vendor-key selection) is the first to fail, probably with
`GW_ROUTING_NO_AVAILABLE_CHANNEL` because no `vendor_key` was
seeded.

Observed: `HTTP 401 GW_AUTH_INVALID_APIKEY {"message":"api key
invalid"}`. Same response with or without the Redis L1 seed.

## Root cause

`crates/gw-store/src/entity_store.rs` is a bare `moka::future::Cache`
per entity type, hand-fed by writers. `EntityStore::apikey.get()` is
a pure local lookup — when the key isn't already in the in-memory
cache, it returns `None`. There is no `loader::load_entity(...)`
fallback wired into `Cache::get_with_by_ref` or anywhere on the auth
hot path.

`crates/gw-store/src/loader.rs` defines a Redis→PG fallback
(`load_entity`) that DOES read the seeded data correctly — but
nothing in the runtime calls it. `grep -rn 'load_entity' crates/`
returns only the loader's own definition and its `tests/`
integration test:

```
crates/gw-store/tests/entity_loader.rs:7:  use gw_store::loader::{LoaderCtx, load_entity};
crates/gw-store/src/loader.rs:23:           pub async fn load_entity<T, F>(...)
```

`crates/gw-gateway/src/lib.rs:177-182`:

```rust
let pg = gw_store::pg_client::connect(cfg.pgsql.url_str(), cfg.pgsql.max_connections)
    .await
    .context("postgres connect failed")?;
// Pg pool is held for future loader use; loader runs on demand from
// proxy filters and will pull a connection from the pool there.
let _ = pg;
```

The PG pool is opened (so the readiness probe succeeds and the
healthcheck cert is happy), validated for connectivity, then
**immediately dropped**. The `LoaderCtx { pg, redis, kek }` struct is
never constructed; no proxy filter ever receives a handle to it.
Bootstrap also drops `_kek_cache`, so encryption (`enc` field on the
L1 payload other than `"json"`) would also be unreadable even if the
loader path were wired.

Consequently:

1. `request_filter` step 1 calls `store.apikey.get(&akid)` which is
   pure Moka.
2. Moka is empty on a cold container — no warm-loader on boot, no
   stream consumer (see related Phase E issue), no pg fallback.
3. `verify_apikey` returns `GW_AUTH_INVALID_APIKEY`.

**End-state**: the gateway can NEVER serve a real request in v1 as
shipped. All eight short-circuit error paths exercised in Phase B
work, but the warm path is unreachable from the network. The
caches are write-only by event consumption, and the event consumer
is itself a placeholder (Phase E).

## What works

- PG schema is correct; seeded rows persist.
- `loader::load_entity` correctly reads the L1 hash shape I wrote
  by hand (verified independently by the existing integration test
  in `crates/gw-store/tests/entity_loader.rs`).
- The `EntityStore` Moka cache itself accepts inserts and respects
  TTL.

## Required wiring (recording only, do not implement)

To make the warm path live:

1. Construct `LoaderCtx { redis: redis.clone(), pg, kek: kek_cache }`
   in `bootstrap()` and stash it on `Wired`.
2. Hand the `LoaderCtx` (or a wrapper) into `LlmGateway::new` so
   filter code can call `load_entity` on cache miss.
3. Replace `store.apikey.get(&akid)` (and the parallel `user`,
   `vendor_key`, `pricing`, `model_group`, `jwks`, `ipblock` lookups)
   with a helper that does `cache.get_with_by_ref(key, async ||
   load_entity(...).map(Arc::new))` so the first miss populates the
   cache and subsequent hits stay fast.
4. Either wire the stream consumer (currently a `sleep(1s)`
   placeholder — see Phase E issue) or accept TTL-only invalidation
   for v1.

## Files referenced

- `crates/gw-store/src/entity_store.rs:33-73`
- `crates/gw-store/src/loader.rs:23-77`
- `crates/gw-gateway/src/lib.rs:162-231`
- `crates/gw-proxy/src/filters/request.rs:79-99`
- `crates/gw-auth/src/apikey.rs:10-22`
- `crates/gw-store/tests/entity_loader.rs:7-68` (the working
  reference impl of the chain)

## Reproduction

```
echo -n "test-key-001" | shasum -a 256    # b2d132fe..
# (seeded via SQL with payload.hash as JSON array of 32 bytes)

curl -sS -X POST http://localhost:8000/v1/chat/completions \
  -H "x-api-key: test-key-001" \
  -H "x-api-key-id: ak-test-001" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4","messages":[{"role":"user","content":"hello"}],"stream":true}'

# {"error":{"code":"GW_AUTH_INVALID_APIKEY","message":"api key invalid","type":"client_auth"}}
# HTTP 401
```
