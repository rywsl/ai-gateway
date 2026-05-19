#!/usr/bin/env bash
# Smoke test: seed PG with a user + api_key, then send a real curl request
# and assert auth passes (the request will fail later in routing for lack of
# seeded vendor_key, but the failure code MUST NOT be GW_AUTH_INVALID_APIKEY).
#
# Pre-req: docker compose up -d (gateway + postgres + redis).

set -euo pipefail

KEY="test-key-001"
KEY_ID="ak-test-001"
USER_ID="user-001"

# 1. Seed PG.
HASH_HEX=$(printf "%s" "$KEY" | shasum -a 256 | cut -d' ' -f1)
HASH_JSON_ARRAY=$(python3 -c "
import sys
hex_str = '$HASH_HEX'
bytes_list = [int(hex_str[i:i+2], 16) for i in range(0, len(hex_str), 2)]
print(','.join(str(b) for b in bytes_list))
")

docker compose exec -T postgres psql -U postgres -d gateway <<SQL
INSERT INTO users (id, payload, version) VALUES (
  '$USER_ID',
  '{"user_id":"$USER_ID","tier":"free","status":"active","markup":1.0,"max_concurrent_streams":100}'::jsonb,
  1
) ON CONFLICT (id) DO NOTHING;

INSERT INTO api_keys (id, payload, version) VALUES (
  '$KEY_ID',
  '{"key_id":"$KEY_ID","user_id":"$USER_ID","status":"active","hash":[$HASH_JSON_ARRAY],"markup":1.0}'::jsonb,
  1
) ON CONFLICT (id) DO NOTHING;
SQL

# 2. Seed balance counter (so the 402-on-zero-balance gate clears).
docker compose exec -T redis redis-cli SET "counter:user:$USER_ID:balance_micro" 1000000

# 3. Hit the gateway.
RESPONSE=$(curl -sS -X POST http://localhost:8000/v1/chat/completions \
  -H "x-api-key: $KEY" \
  -H "x-api-key-id: $KEY_ID" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4","messages":[{"role":"user","content":"hello"}],"stream":true}')

echo "Response: $RESPONSE"

# 4. Assert: auth passed (no GW_AUTH_INVALID_APIKEY).
if echo "$RESPONSE" | grep -q "GW_AUTH_INVALID_APIKEY"; then
  echo "FAIL: auth still rejecting — cold loader not wired"
  exit 1
fi

# Expected next-stage failure: no model_group seeded yet, so routing fails.
if echo "$RESPONSE" | grep -qE "GW_ROUTING|GW_REQUEST_MODEL|GW_BACKEND"; then
  echo "PASS: auth succeeded; failed downstream as expected"
  exit 0
fi

echo "UNEXPECTED: response did not match auth-success or known downstream-failure patterns"
exit 1
