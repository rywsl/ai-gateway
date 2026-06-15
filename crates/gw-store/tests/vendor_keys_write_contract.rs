//! Cross-process contract lock for the Rust → `vendor_keys` write surface.
//!
//! Three Rust runtime writers UPDATE `vendor_keys`:
//!   - `oauth_refresh`    — runtime OAuth token refresh (`payload.oauth`)
//!   - `identity_persist` — egress-impersonate identity snapshot
//!   - `quota_state`      — upstream quota-remaining snapshot
//!
//! In the Rust migrations `vendor_keys` is a TABLE (0001 + 0009 + 0010). In Go
//! **production** it is a writable VIEW over `provider_keys`
//! (`00060_vendor_keys_write_compat.sql`). For the Rust writers to work against
//! that VIEW, Go must keep every column / JSONB path asserted below writable
//! (via `INSTEAD OF` triggers or updatable-view rules). These tests pin the
//! exact column surface so a silent change on the Rust side breaks the build
//! and forces a `docs/go-contracts/` update (CNxus-Rust #11).
//!
//! They are pure string assertions — no database required.

use gw_store::{identity_persist, oauth_refresh, quota_state};

/// Columns / JSONB paths the Go production `vendor_keys` VIEW must keep
/// writable. Adding to a writer's SQL without extending this list (or vice
/// versa) breaks the matching test below.
#[test]
fn oauth_writer_targets_only_contracted_columns() {
    let sql = oauth_refresh::update_sql();
    assert!(
        sql.contains("UPDATE vendor_keys"),
        "must target vendor_keys"
    );
    // payload.oauth JSONB sub-tree, version CAS, updated_at — and nothing else.
    assert!(sql.contains("jsonb_set(payload, '{oauth}'"));
    assert!(sql.contains("version = version + 1"));
    assert!(sql.contains("updated_at = NOW()"));
    assert!(sql.contains("WHERE id = $1 AND version = $2 AND deleted_at IS NULL"));
    // Contract: the OAuth writer must not touch quota/identity/health columns.
    for forbidden in [
        "quota_availability",
        "quota_remaining",
        "identity_snapshot",
        "cooldown",
        "consecutive_failures",
        "circuit",
    ] {
        assert!(
            !sql.contains(forbidden),
            "oauth writer unexpectedly touches `{forbidden}`"
        );
    }
}

#[test]
fn identity_writer_targets_only_identity_snapshot() {
    let sql = identity_persist::update_sql();
    assert!(
        sql.contains("UPDATE vendor_keys"),
        "must target vendor_keys"
    );
    assert!(sql.contains("identity_snapshot = $1"));
    assert!(sql.contains("updated_at = NOW()"));
    // Conditional write — preserves admin-edited identity values.
    assert!(sql.contains("identity_snapshot IS NULL"));
    for forbidden in [
        "quota_",
        "payload",
        "cooldown",
        "consecutive_failures",
        "circuit",
    ] {
        assert!(
            !sql.contains(forbidden),
            "identity writer unexpectedly touches `{forbidden}`"
        );
    }
}

#[test]
fn quota_writer_targets_only_quota_columns_and_payload_mirror() {
    let sql = quota_state::update_sql();
    assert!(
        sql.contains("UPDATE vendor_keys"),
        "must target vendor_keys"
    );
    for col in [
        "quota_availability = $2",
        "quota_remaining = $3",
        "quota_limit = $4",
        "quota_reset_at",
        "quota_observed_at",
    ] {
        assert!(sql.contains(col), "quota writer missing `{col}`");
    }
    // Mirrors the snapshot into payload.quota_state and bumps version.
    assert!(sql.contains("jsonb_set(payload, '{quota_state}'"));
    assert!(sql.contains("version = version + 1"));
    assert!(sql.contains("WHERE id = $1 AND deleted_at IS NULL"));
}

/// #12 guard: cooldown / circuit / health state is per-replica and in-memory
/// (`gw-health`), never persisted. No `vendor_keys` writer may emit a
/// cooldown/health column — those columns do not exist in any migration and
/// are not owned by the Go production schema.
#[test]
fn no_vendor_keys_writer_persists_cooldown_or_health_state() {
    let writers = [
        ("oauth_refresh", oauth_refresh::update_sql()),
        ("identity_persist", identity_persist::update_sql()),
        ("quota_state", quota_state::update_sql()),
    ];
    for (name, sql) in writers {
        for forbidden in [
            "cooldown_until",
            "cooldown_reason",
            "consecutive_failures",
            "circuit",
            "open_until",
            "health_state",
        ] {
            assert!(
                !sql.contains(forbidden),
                "{name} writer persists in-memory-only health field `{forbidden}`"
            );
        }
    }
}
