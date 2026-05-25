ALTER TABLE api_keys ADD COLUMN IF NOT EXISTS signature_secret BYTEA;
