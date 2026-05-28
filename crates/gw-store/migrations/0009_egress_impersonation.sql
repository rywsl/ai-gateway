-- 0009_egress_impersonation.sql
-- Per-Vendor egress selection (rustls default | impersonate {profile}).
-- Per-VendorKey persisted impersonation identity snapshot.
-- Both nullable; existing rows deserialize to defaults via #[serde(default)].
ALTER TABLE vendors      ADD COLUMN IF NOT EXISTS egress_mode       JSONB DEFAULT NULL;
ALTER TABLE vendor_keys  ADD COLUMN IF NOT EXISTS identity_snapshot JSONB DEFAULT NULL;
