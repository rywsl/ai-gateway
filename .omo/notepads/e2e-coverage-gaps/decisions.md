# Decisions

## 2026-05-25 Plan Start
- Regex from entity store JSONB payload (no DDL migration)
- Endpoint: /healthz/strategies following existing healthz pattern
- Hash input: entire cached_body raw bytes, no JSON normalization
- None regex = skip regex check; Some(pattern) = compile + budget check
- Cold start latency endpoint: 200 + empty array []
- Empty cached_body: message_hash stays None
- SHA-256 to u64 truncation acceptable (hint, not identity)
- Regex is api-key-tier only, direct copy during merge (not 5-tier)
- MergedPermissions field: pub model_whitelist_regex: Option<String> (no serde)
- Compilation of regex happens in gw-proxy (Task 4), NOT gw-core
