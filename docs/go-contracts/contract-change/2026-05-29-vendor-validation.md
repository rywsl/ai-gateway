# 2026-05-29 — Vendor validation for egress impersonation

> **Status**: Contract change record (Awaiting Go-side implementation)
> **Date**: 2026-05-29
> **Scope**: Go control-plane vendor writes and admin identity-regeneration action for Rust egress impersonation.
>
> Rust does not own `vendor_keys` writes. `/admin/*` requests are verified by Rust and reverse-proxied to the Go control plane, so write-time validation for these cases belongs in Go. Rust keeps the runtime fail-closed guard as the final authority.

---

## 0. Change index

| # | Item | Go-side responsibility | Contract touchpoint | Rust evidence |
|---|---|---|---|---|
| EI-8 | Tunnel + impersonate exclusion | Reject writes that make an impersonated vendor tunnel-eligible | `vendors.egress_mode`, `vendor_keys.payload.tunnel_preference`, proxy-node mapping metadata | Runtime error `GW_TUNNEL_IMPERSONATE_CONFLICT` in upstream path |
| EI-7 | Regenerate identity | Clear `vendor_keys.identity_snapshot` and bump `vendor_keys.version` in one admin action | `vendor_keys.identity_snapshot`, `vendor_keys.version` | `ensure_identity(..., None, profile)` re-derives and requests lazy persist |

## 1. EI-8 write-time exclusion

**Status: Awaiting Go-side implementation**

### 1.1 Contract summary

Go must reject any vendor or vendor-key update that would result in both conditions being true:

1. The owning vendor has `egress_mode = Impersonate`.
2. At least one key for that vendor is tunnel-eligible.

"Tunnel-eligible" includes either of these cases:

- `vendor_keys.payload.tunnel_preference.require_tunnel = true`
- Go-side node mapping metadata would route the key/vendor through a proxy node

This mirrors Rust's runtime fail-closed behavior, where the upstream path returns `GW_TUNNEL_IMPERSONATE_CONFLICT` instead of silently choosing Direct or Tunnel. The runtime guard remains authoritative even after Go adds write-time validation.

### 1.2 Go-side requirement

On vendor and vendor-key writes, Go must validate the post-write aggregate state, not only the changed row. A safe implementation checks:

- when changing a vendor to `egress_mode = Impersonate`, scan that vendor's active keys and node mappings for tunnel eligibility;
- when changing a key's `tunnel_preference` or node mapping, check whether the owning vendor is currently impersonated;
- when undeleting or reactivating a key, apply the same check as an upsert.

If the conflict exists, reject the admin write and leave persisted state unchanged. The error code is Go-owned, but it should mention the Rust runtime code `GW_TUNNEL_IMPERSONATE_CONFLICT` for operator correlation.

### 1.3 Cross-process touchpoints

- **PgSQL table**: `vendors`
  - field: `egress_mode` column or the equivalent projected shape used by Go
- **PgSQL table**: `vendor_keys`
  - field: `payload.tunnel_preference`
- **Go-owned mapping metadata**: any vendor/key to proxy-node mapping used to decide tunnel eligibility
- **Rust stream/cache invalidation**: normal entity version bump + `events:entity` invalidation path for accepted writes

## 2. EI-7 regenerate identity

**Status: Awaiting Go-side implementation**

### 2.1 Contract summary

Go must expose an explicit admin action to regenerate a vendor key's impersonation identity. The action does not write a new identity directly. It clears the stored snapshot and bumps the entity version:

```sql
UPDATE vendor_keys
SET identity_snapshot = NULL,
    version = version + 1,
    updated_at = NOW()
WHERE id = $1
  AND deleted_at IS NULL;
```

The version bump is mandatory. Rust caches vendor keys in-process and in Redis (`entity:vendor_key:<id>`). Without the version bump and normal invalidation/write-through behavior, a gateway pod can keep serving the old `identity_snapshot` until cache expiry.

### 2.2 Required runtime effect

After the admin action:

1. Rust observes the bumped version through the normal L1/L2/cold-load path.
2. `VendorKey.identity_snapshot` loads as `None`.
3. The next impersonated request calls `gw_core::impersonate::ensure_identity(key_id, None, profile)`.
4. `ensure_identity` deterministically re-derives the identity and returns `should_persist = true`.
5. Rust's existing lazy persist path writes the newly derived snapshot with the `IS NULL` guard in `identity_persist.rs`.

Rust's lazy persist rule remains unchanged: it must stay `write_if_null` and only write when `vendor_keys.identity_snapshot IS NULL`.

### 2.3 Manual workaround until shipped

Until the Go admin action exists, operators can run the equivalent SQL manually:

```sql
UPDATE vendor_keys
SET identity_snapshot = NULL,
    version = version + 1,
    updated_at = NOW()
WHERE id = '<vendor-key-id>';
```

Operators must also ensure the normal entity invalidation path reaches gateway pods. If invalidation is unavailable, restart or otherwise cold-load affected gateway pods before expecting the new identity to be used.

### 2.4 Rust evidence

- Loader reads the dedicated column into `VendorKey.identity_snapshot`: `crates/gw-store/src/loader.rs`
- Lazy persist remains guarded by `identity_snapshot IS NULL`: `crates/gw-store/src/identity_persist.rs`
- Regenerate/cold-load proof: `crates/gw-store/tests/identity_regenerate.rs`
