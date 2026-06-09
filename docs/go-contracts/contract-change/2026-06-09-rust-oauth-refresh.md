# Contract Change: Rust Runtime OAuth Refresh

- Date: 2026-06-09
- Status: Implemented in W6 K-C
- Authority: F2 in `docs/design/2026-06-09-roadmap-authority-and-scope.md`

## Ownership

Go remains the writer for OAuth provider configuration and initial vendor-key authorization seed data.
Rust is the sole runtime writer for refreshed `vendor_keys.payload.oauth` after K-C.
Go must not refresh or overwrite runtime vendor-key OAuth tokens after K-C.

## Rust Runtime Write-Back

Rust writes `vendor_keys.payload.oauth` with:

- `access_token_enc`: encrypted refreshed access token.
- `refresh_token_enc`: encrypted rotated refresh token when the provider returns one; the existing encrypted refresh token is preserved when the provider omits one.
- `access_expires_at`: refreshed expiry timestamp.
- `token_endpoint`: preserved from the previous payload.
- `scope`: refreshed scope if returned by provider, otherwise preserved from the previous payload.
- `quota_project_id`: preserved from the previous payload.

The write is optimistic-CAS:

```sql
UPDATE vendor_keys
SET payload = jsonb_set(payload, '{oauth}', $oauth_json::jsonb, true),
    version = version + 1,
    updated_at = NOW()
WHERE id = $key_id
  AND version = $expected_version
  AND deleted_at IS NULL
RETURNING version
```

After commit, Rust publishes:

```text
XADD events:entity * ns vendor_key id <key_id> op upsert v <new_version> ts_ms <now> source rust_oauth_refresh
```

## Single-Flight

Rust uses `SET lock:oauth:refresh:<key_id> <owner> NX PX <ttl_ms>` before contacting the token endpoint.
Lock losers reload `vendor_key` and use the winner's token.
CAS losers reload `vendor_key` and use the winner's token.

## `oauth_failed`

Post-K-C, Go must not consume `field=oauth_failed` or any OAuth status event as a refresh command.
If Rust emits an OAuth failure event, it is status-only.
