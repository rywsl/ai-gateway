# Decisions — v2-proxy-core-cleanup

- 2026-05-24 T1 HMAC rework: `signature_required` global config is ORed with the per-api-key flag at the proxy request filter, but HMAC secret material is always sourced only from `ApiKeyEntry.signature_secret`; there is deliberately no fallback to the api-key hash.
- 2026-05-24 T1 HMAC rework: compressed signed bodies keep `downstream_request_body` as the raw wire bytes for archive fidelity, while `ctx.cached_body` is replaced with the decoded bytes before codec parse and upstream rewrite.
