-- E4-A: boot-seeded global AuthConfig.
--
-- The gateway reads `auth_configs` id="global" exactly ONCE at boot
-- (crates/gw-gateway/src/lib.rs builds the JwtValidator from it) and holds
-- the resulting JwtValidator in memory for the process lifetime. To make
-- iss/aud enforcement deterministic for the JWT auth-failure scenarios, this
-- row MUST exist BEFORE the gateway boots.
--
-- Postgres runs every file in /docker-entrypoint-initdb.d/ during container
-- init, which happens while the DB is still coming up — i.e. strictly before
-- the `pg_isready` healthcheck passes, which is what the gateway's
-- `depends_on: postgres: service_healthy` waits for. So this row is guaranteed
-- present by the time the gateway connects. initdb only runs on a fresh data
-- dir; `make e2e` does `down -v` each run, so the volume is always fresh.
--
-- AuthConfig payload (verified against crates/gw-core/src/entity.rs::AuthConfig
-- and crates/gw-store/src/loader.rs::load_auth_config which does
-- `SELECT payload, version FROM auth_configs WHERE id=$1 AND deleted_at IS NULL`
-- then serde_json::from_value::<AuthConfig>):
--   expected_iss        = ["e2e-issuer"]
--   expected_aud        = ["e2e-aud"]
--   leeway_secs         = 30
--   require_nbf         = false
--   blacklist_fail_mode = "fail_closed"   (serde rename_all=snake_case)
--   static_secret_hs256 = UTF-8 bytes of "e2e-hs256-secret-key" as a JSON int
--                         array (serde deserializes Vec<u8> from a JSON array)
--   static_pem_rs256    = null            (RS256 resolves via the JWKS resolver
--                         wired at boot — EntityStoreJwksResolver — so no static
--                         PEM is needed; the JWKS-rotation scenario exercises it)
--
-- Keep these constants in sync with tests/e2e/client/conftest.py
-- (HS256_SECRET / JWT_ISS / JWT_AUD). If you change HS256_SECRET, regenerate
-- the static_secret_hs256 int array below with:
--   python3 -c 'print(list(b"e2e-hs256-secret-key"))'
-- A mismatch surfaces as a confusing GW_AUTH_INVALID_JWT (signature) failure
-- rather than the expected GW_AUTH_JWT_INVALID_ISS/_AUD.

CREATE TABLE IF NOT EXISTS auth_configs (
    id TEXT PRIMARY KEY,
    payload JSONB NOT NULL,
    version BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ NULL
);

INSERT INTO auth_configs (id, payload, version) VALUES (
    'global',
    '{"expected_iss":["e2e-issuer"],"expected_aud":["e2e-aud"],"leeway_secs":30,"require_nbf":false,"blacklist_fail_mode":"fail_closed","static_secret_hs256":[101,50,101,45,104,115,50,53,54,45,115,101,99,114,101,116,45,107,101,121],"static_pem_rs256":null}',
    1
)
ON CONFLICT (id) DO UPDATE SET payload = EXCLUDED.payload;
