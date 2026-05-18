-- seed.sql — minimal fixture for gw-gateway e2e tests.
--
-- Inserts the smallest viable set of entities to exercise the happy path:
--   - one main-account user with a 100 USD balance (in micro-units),
--   - one vendor (whose `base_url` is overridden at runtime to point at a
--     wiremock-style upstream),
--   - one vendor_key (gpt-4 accessible),
--   - one model_group bound to the vendor_key with `fixed_order` strategy,
--   - one pricing row for gpt-4.
--
-- The payload JSON shapes mirror the structs in gw-core::entity. Field names
-- use snake_case to match serde defaults.

INSERT INTO users (id, payload, version) VALUES
  ('u1',
   jsonb_build_object(
     'user_id', 'u1',
     'parent_id', null,
     'tier', 'standard',
     'status', 'Active',
     'allowed_models', jsonb_build_array('gpt-4'),
     'allowed_channels', jsonb_build_array(),
     'allowed_groups', jsonb_build_array(),
     'rpm_limit', 1000,
     'tpm_limit', 0,
     'max_concurrent_streams', 100,
     'daily_quota_tokens', 0,
     'user_group_ids', jsonb_build_array(),
     'markup', 1.0,
     'debug_enabled', false
   ),
   1);

INSERT INTO vendors (id, payload, version) VALUES
  ('v1',
   jsonb_build_object(
     'vendor_id', 'v1',
     'protocol', 'openai',
     -- `base_url` is overwritten by the test before the proxy reads it.
     'base_url', 'http://127.0.0.1:0',
     'health_check_path', '/v1/models',
     'timeout_ms', 30000
   ),
   1);

INSERT INTO vendor_keys (id, payload, version) VALUES
  ('vk1',
   jsonb_build_object(
     'key_id', 'vk1',
     'vendor_id', 'v1',
     -- `api_key` is stored encrypted in production; the e2e fixture uses a
     -- plaintext stub since the test seeds Redis directly with the
     -- decrypted form.
     'api_key', 'sk-fixture',
     'accessible_models', jsonb_build_array('gpt-4'),
     'model_mapping', jsonb_build_object(),
     'accessible_endpoints', jsonb_build_array(),
     'protocol_conversion', false,
     'priority', 0,
     'weight', 1,
     'rpm_limit', 0,
     'tpm_limit', 0,
     'max_usage_count', null,
     'proxy', null,
     'ip_allowlist', null,
     'expires_at', null,
     'status', 'Active',
     'tags', jsonb_build_array()
   ),
   1);

INSERT INTO model_groups (id, payload, version) VALUES
  ('mg1',
   jsonb_build_object(
     'id', 'mg1',
     'display_name', 'default-gpt4',
     'strategy', 'fixed_order',
     'candidates', jsonb_build_array('vk1'),
     'allowed_models', jsonb_build_array('gpt-4'),
     'description', 'fixture model group'
   ),
   1);

INSERT INTO pricings (id, payload, version) VALUES
  ('gpt-4',
   jsonb_build_object(
     'model', 'gpt-4',
     'input_per_million_micro', 30000000,
     'output_per_million_micro', 60000000,
     'cache_read_per_million_micro', 0,
     'cache_write_per_million_micro', 0
   ),
   1);
