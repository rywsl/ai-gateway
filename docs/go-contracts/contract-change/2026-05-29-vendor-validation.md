# 2026-05-29 — Vendor validation for egress impersonation

> Contract change for Go control-plane writes to `vendor_keys`.
> Related Rust issue record: [`docs/issues/egress-impersonate-known-gaps.md`](../../issues/egress-impersonate-known-gaps.md) EI-7 / EI-8.

## Scope

Rust does not own the admin write path for `vendor_keys`; `/admin/*` is verified by the gateway and reverse-proxied to the Go control plane. The only Rust write to `vendor_keys.identity_snapshot` is the lazy `write_if_null` path in `PgSnapshotWriter`, which must remain conditional on `identity_snapshot IS NULL`.

Go therefore owns these write-time validations and admin actions.

## Requirements

### EI-8: tunnel / impersonate exclusion at write time

Go MUST reject any vendor-key update that would make a key both:

- `egress_mode = Impersonate`, and
- tunnel-eligible, either by `tunnel_preference.require_tunnel = true` or by a control-plane node mapping that would route the key through a tunnel.

Rust still enforces the runtime defense in depth: `pick_peer` returns `GW_TUNNEL_IMPERSONATE_CONFLICT` if an impersonated request resolves to a tunnel peer. The Go write-time check prevents bad config from reaching traffic; the Rust runtime check remains the authoritative fail-closed guard.

### EI-7: regenerate identity admin action

Go MUST expose an admin action to regenerate a vendor key's impersonation identity. The PgSQL mutation is:

```sql
UPDATE vendor_keys
SET identity_snapshot = NULL,
    version = version + 1,
    updated_at = NOW()
WHERE id = $1;
```

The `version = version + 1` part is mandatory so the reloaded entity carries a fresh monotonic version. Cache invalidation is event-driven: clearing only `identity_snapshot` can leave a stale non-null snapshot in Moka or Redis L2 until TTL. After the event invalidates those caches, the next request cold-loads the updated row, calls `ensure_identity` with no snapshot, re-derives deterministically from the key id and profile, and lazy-persists the new snapshot through `PgSnapshotWriter::write_if_null`.

After the PgSQL update, Go MUST publish the normal entity invalidation:

```
XADD events:entity * ns vendor_key id <vendor_key_id> v <new_version> op upsert
```

or perform an equivalent immediate invalidation of `entity:vendor_key:<id>` plus every gateway pod's Moka entry. Version bump alone does not evict a currently cached Redis hash; the `events:entity` path is what makes Rust delete Redis L2 and invalidate Moka before the next load.

Until the Go admin action ships, the manual workaround is the SQL statement above plus the `events:entity` XADD for the target `vendor_keys.id`.

## Rust-side guarantees

- `PgSnapshotWriter::write_if_null` only writes when `identity_snapshot IS NULL`; it does not overwrite operator-provided non-null snapshots.
- `load_vendor_key` folds the dedicated `identity_snapshot` column into the decoded payload.
- The ignored Rust regression test `crates/gw-store/tests/identity_regenerate.rs` covers the required `NULL + version bump + events:entity invalidation` cache-miss flow.
