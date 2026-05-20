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
# Capture status code AND body in one curl call.
HTTP_STATUS=$(curl -sS -o /tmp/smoke-warm-path.body -w "%{http_code}" -X POST http://localhost:8000/v1/chat/completions \
  -H "x-api-key: $KEY" \
  -H "x-api-key-id: $KEY_ID" \
  -H "Content-Type: application/json" \
  -d '{"model":"gpt-4","messages":[{"role":"user","content":"hello"}],"stream":true}')
BODY=$(cat /tmp/smoke-warm-path.body)

echo "HTTP $HTTP_STATUS"
echo "Body: $BODY"

# 4. Primary assertion: auth must NOT reject.
if [ "$HTTP_STATUS" = "401" ] || echo "$BODY" | grep -q "GW_AUTH_INVALID_APIKEY"; then
  echo "FAIL: auth still rejecting — cold loader not wired"
  exit 1
fi

# 5. Secondary check (informational): expect a downstream failure since no
# vendor_key/model_group is seeded. HTTP 5xx or a known GW_ROUTING/MODEL/BACKEND
# code in the body both qualify.
if [ "$HTTP_STATUS" -ge 500 ] || echo "$BODY" | grep -qE "GW_ROUTING|GW_REQUEST_MODEL|GW_BACKEND"; then
  echo "PASS: auth succeeded (HTTP $HTTP_STATUS); failed downstream as expected"
  exit 0
fi

# Auth passed but the response shape is something else (e.g. unexpected 2xx).
echo "UNEXPECTED: auth passed but downstream returned HTTP $HTTP_STATUS without a known failure marker"
exit 1
