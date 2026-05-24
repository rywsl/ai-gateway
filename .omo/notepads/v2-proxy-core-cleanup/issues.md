# Issues — v2-proxy-core-cleanup

- 2026-05-24 T1 HMAC rework: pre-existing compile drift in `gw-proxy` had `filters::logging::run` expecting `ratelimit` while `LlmGateway::logging` did not pass it; fixed while making the required test-red run compile far enough to hit the new HMAC API failure.
