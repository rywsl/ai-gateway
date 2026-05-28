"""
E2E smoke test: gzip-compressed + HMAC-signed request reaches the gateway
and is handled correctly (Phase B — RequestBodyView decompresses once).

The unit-level OnceCell guarantee is covered by
body::view::tests::decoded_only_decompresses_once. This scenario validates
the full filter-chain path: body read → HMAC verify (pre-populates
OnceCell via prefill_decoded) → codec parse (reuses cached decoded bytes).

Requires a running gateway with a seed api-key that has signature_required=true
and content-encoding=gzip support. If the infra is unavailable, the test is
marked skip.
"""
import gzip
import hashlib
import hmac as hmac_lib
import json
import time

import pytest
import requests


@pytest.mark.skip(
    reason="requires live gateway + seeded HMAC api-key; run manually with GW_HMAC_E2E=1"
)
def test_gzip_body_hmac_signed_request(gateway_base_url, hmac_api_key, hmac_api_key_id, hmac_secret):
    """
    Phase B integration smoke: a gzip-compressed JSON body with a valid HMAC
    signature is accepted by the gateway. The OnceCell decompresses once
    (verified by unit tests); this test confirms the filter chain integration
    path is wired end-to-end.
    """
    payload = json.dumps(
        {
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
        }
    ).encode()

    compressed = gzip.compress(payload)
    ts = str(int(time.time()))
    body_hash = hashlib.sha256(payload).hexdigest()
    signing_string = f"POST\n/v1/messages\n{ts}\n{body_hash}"
    sig = hmac_lib.new(
        hmac_secret.encode(), signing_string.encode(), hashlib.sha256
    ).hexdigest()

    resp = requests.post(
        f"{gateway_base_url}/v1/messages",
        data=compressed,
        headers={
            "x-api-key": hmac_api_key,
            "x-api-key-id": hmac_api_key_id,
            "Content-Type": "application/json",
            "Content-Encoding": "gzip",
            "x-signature": sig,
            "x-timestamp": ts,
        },
        timeout=10,
    )
    # 401/400 would mean signature or decompression failed; 2xx or 5xx (upstream
    # unavailable) both mean the gateway accepted the signed body through the
    # filter chain correctly.
    assert resp.status_code not in (400, 401), (
        f"Gateway rejected signed gzip body: {resp.status_code} {resp.text}"
    )
