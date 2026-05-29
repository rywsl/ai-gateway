//! 稳定错误码字典（ADR-020）—— 永不在小版本中变更含义。

pub const GW_AUTH_INVALID_JWT: &str = "GW_AUTH_INVALID_JWT";
pub const GW_AUTH_INVALID_APIKEY: &str = "GW_AUTH_INVALID_APIKEY";
pub const GW_AUTH_NO_KEY_PERMISSION: &str = "GW_AUTH_NO_KEY_PERMISSION";
pub const GW_AUTH_IP_BLOCKED: &str = "GW_AUTH_IP_BLOCKED";
pub const GW_AUTH_IP_NOT_ALLOWED: &str = "GW_AUTH_IP_NOT_ALLOWED";
pub const GW_AUTH_SIGNATURE_INVALID: &str = "GW_AUTH_SIGNATURE_INVALID";
pub const GW_QUOTA_BALANCE_ZERO: &str = "GW_QUOTA_BALANCE_ZERO";
pub const GW_QUOTA_RPM_EXCEEDED: &str = "GW_QUOTA_RPM_EXCEEDED";
pub const GW_QUOTA_TPM_EXCEEDED: &str = "GW_QUOTA_TPM_EXCEEDED";
pub const GW_QUOTA_CONCURRENT_EXCEEDED: &str = "GW_QUOTA_CONCURRENT_EXCEEDED";
/// Protected-mode lease credits insufficient to cover the request's estimated cost.
/// HTTP 402. Distinct from `GW_QUOTA_BALANCE_ZERO`: user may have non-zero balance
/// but this Pod's lease share is exhausted; client may retry after next Go push.
pub const GW_QUOTA_LEASE_INSUFFICIENT: &str = "GW_QUOTA_LEASE_INSUFFICIENT";
/// Protected-mode lease has expired (now > expires_at_ms + lease_grace_ms) and
/// Go has not pushed a renewal. HTTP 503 — this is an ops signal, not a user
/// error. Triggers `gw_credit_lease_expired_total` for paging.
pub const GW_QUOTA_LEASE_EXPIRED: &str = "GW_QUOTA_LEASE_EXPIRED";
pub const GW_REQUEST_PROTOCOL_UNKNOWN: &str = "GW_REQUEST_PROTOCOL_UNKNOWN";
pub const GW_REQUEST_INPUT_TOO_LARGE: &str = "GW_REQUEST_INPUT_TOO_LARGE";
pub const GW_REQUEST_COST_TOO_HIGH: &str = "GW_REQUEST_COST_TOO_HIGH";
pub const GW_REQUEST_CONTENT_REJECTED: &str = "GW_REQUEST_CONTENT_REJECTED";
pub const GW_REQUEST_MODEL_NOT_FOUND: &str = "GW_REQUEST_MODEL_NOT_FOUND";
pub const GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL: &str = "GW_REQUEST_CLI_NO_SAME_PROTOCOL_CHANNEL";
pub const GW_ROUTING_NO_AVAILABLE_CHANNEL: &str = "GW_ROUTING_NO_AVAILABLE_CHANNEL";
pub const GW_UPSTREAM_TIMEOUT: &str = "GW_UPSTREAM_TIMEOUT";
pub const GW_UPSTREAM_5XX: &str = "GW_UPSTREAM_5XX";
pub const GW_UPSTREAM_4XX: &str = "GW_UPSTREAM_4XX";
pub const GW_UPSTREAM_OVERLOADED: &str = "GW_UPSTREAM_OVERLOADED";
pub const GW_UPSTREAM_CANCELLED: &str = "GW_UPSTREAM_CANCELLED";
pub const GW_BACKEND_UNAVAILABLE: &str = "GW_BACKEND_UNAVAILABLE";
pub const GW_COLD_LOAD_TIMEOUT: &str = "GW_COLD_LOAD_TIMEOUT";
pub const GW_GATEWAY_INTERNAL: &str = "GW_GATEWAY_INTERNAL";

// --- Wave 1+ codes (known-gaps closure) ---
// Auth: ApiKeyEntry.expires_at lifecycle (G1).
pub const GW_AUTH_KEY_EXPIRED: &str = "GW_AUTH_KEY_EXPIRED";
// Auth: ApiKeyEntry.ip_allowlist (G2).
pub const GW_AUTH_KEY_IP_NOT_ALLOWED: &str = "GW_AUTH_KEY_IP_NOT_ALLOWED";
// Auth: JWT claim validation — iss / aud / nbf, hybrid JWKS (G6).
pub const GW_AUTH_JWT_INVALID_ISS: &str = "GW_AUTH_JWT_INVALID_ISS";
pub const GW_AUTH_JWT_INVALID_AUD: &str = "GW_AUTH_JWT_INVALID_AUD";
pub const GW_AUTH_JWT_NOT_YET_VALID: &str = "GW_AUTH_JWT_NOT_YET_VALID";
pub const GW_AUTH_JWT_EXPIRED: &str = "GW_AUTH_JWT_EXPIRED";
// Auth: HMAC request signing (G7). `GW_AUTH_SIGNATURE_INVALID` already exists
// above; the missing-header and clock-drift variants are new.
pub const GW_AUTH_SIGNATURE_MISSING: &str = "GW_AUTH_SIGNATURE_MISSING";
pub const GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT: &str = "GW_AUTH_SIGNATURE_TIMESTAMP_DRIFT";
// Quota: daily token cap (G3).
pub const GW_QUOTA_DAILY_TOKENS_EXCEEDED: &str = "GW_QUOTA_DAILY_TOKENS_EXCEEDED";
// Routing: capability match via CapabilityResolver (G13).
pub const GW_ROUTING_NO_CAPABILITY_MATCH: &str = "GW_ROUTING_NO_CAPABILITY_MATCH";
// Upstream: connect / TLS failures surfaced via fail_to_connect retry (G10).
pub const GW_UPSTREAM_CONNECT_FAILED: &str = "GW_UPSTREAM_CONNECT_FAILED";
pub const GW_UPSTREAM_TLS_ERROR: &str = "GW_UPSTREAM_TLS_ERROR";
// Upstream: codec `parse_response` decode failure on non-streaming path (T1-A/B/C).
pub const GW_UPSTREAM_PROTOCOL_DECODE_ERROR: &str = "GW_UPSTREAM_PROTOCOL_DECODE_ERROR";

// --- Egress-impersonate loopback error classes (Task 4.4) ---
// Mapped from the `X-Egress-Error-Class` header emitted by gw-egress-impersonate.
pub const GW_UPSTREAM_CONNECT_TIMEOUT: &str = "GW_UPSTREAM_CONNECT_TIMEOUT";
pub const GW_UPSTREAM_CONNECT_REFUSED: &str = "GW_UPSTREAM_CONNECT_REFUSED";
pub const GW_UPSTREAM_READ_TIMEOUT: &str = "GW_UPSTREAM_READ_TIMEOUT";
pub const GW_UPSTREAM_H2_PROTOCOL: &str = "GW_UPSTREAM_H2_PROTOCOL";
pub const GW_UPSTREAM_BODY_TRUNCATED: &str = "GW_UPSTREAM_BODY_TRUNCATED";
pub const GW_UPSTREAM_PROXY_ERROR: &str = "GW_UPSTREAM_PROXY_ERROR";
pub const GW_UPSTREAM_PANIC: &str = "GW_UPSTREAM_PANIC";
pub const GW_UPSTREAM_UNKNOWN: &str = "GW_UPSTREAM_UNKNOWN";

// --- Tunnel codes (ADR-T-1 / spec §4) ---
pub const GW_TUNNEL_NO_NODE: &str = "GW_TUNNEL_NO_NODE";
pub const GW_TUNNEL_NO_LIVE: &str = "GW_TUNNEL_NO_LIVE";
pub const GW_TUNNEL_TARGET_DENIED: &str = "GW_TUNNEL_TARGET_DENIED";
pub const GW_TUNNEL_DNS_BLOCKED: &str = "GW_TUNNEL_DNS_BLOCKED";
/// A vendor configured for impersonate egress cannot also tunnel; emitted
/// when tunnel resolution returns `Use(_)` for an Impersonate vendor.
pub const GW_TUNNEL_IMPERSONATE_CONFLICT: &str = "GW_TUNNEL_IMPERSONATE_CONFLICT";

// --- OAuth codes (G3 / go-onboarding-gaps T0) ---
pub const GW_AUTH_OAUTH_TOKEN_EXPIRED: &str = "GW_AUTH_OAUTH_TOKEN_EXPIRED";
pub const GW_AUTH_OAUTH_REFRESH_FAILED: &str = "GW_AUTH_OAUTH_REFRESH_FAILED";

// --- Go-onboarding-blockers codes (B2 mgmt-token) ---
// B2: management-token authentication (Mgmt header → ManagementTokenEntry →
// SHA-256 verify → /healthz unlock). One code per failure mode for ops drilldown.
pub const GW_AUTH_MGMT_TOKEN_MISSING: &str = "GW_AUTH_MGMT_TOKEN_MISSING";
pub const GW_AUTH_MGMT_TOKEN_INVALID: &str = "GW_AUTH_MGMT_TOKEN_INVALID";
pub const GW_AUTH_MGMT_TOKEN_EXPIRED: &str = "GW_AUTH_MGMT_TOKEN_EXPIRED";
pub const GW_AUTH_MGMT_TOKEN_DISABLED: &str = "GW_AUTH_MGMT_TOKEN_DISABLED";
pub const GW_AUTH_MGMT_TOKEN_REVOKED: &str = "GW_AUTH_MGMT_TOKEN_REVOKED";
pub const GW_AUTH_MGMT_TOKEN_SCOPE_DENIED: &str = "GW_AUTH_MGMT_TOKEN_SCOPE_DENIED";

// --- Codec response / stream parse + cross-protocol re-render (S-codec spec §2.3 / §4) ---
pub const GW_CODEC_PARSE_RESPONSE: &str = "GW_CODEC_PARSE_RESPONSE";
pub const GW_CODEC_RENDER_RESPONSE: &str = "GW_CODEC_RENDER_RESPONSE";
pub const GW_CODEC_PARSE_STREAM_CHUNK: &str = "GW_CODEC_PARSE_STREAM_CHUNK";
pub const GW_CODEC_CROSS_RENDER_FAILED: &str = "GW_CODEC_CROSS_RENDER_FAILED";

// --- Response deadline (S-proxy-core T3) ---
pub const GW_RESPONSE_DEADLINE_EXCEEDED: &str = "GW_RESPONSE_DEADLINE_EXCEEDED";

// --- IP Rate Limit / Allowlist ---
pub const GW_QUOTA_IP_RPM_EXCEEDED: &str = "GW_QUOTA_IP_RPM_EXCEEDED";
pub const GW_AUTH_IP_NOT_IN_ALLOWLIST: &str = "GW_AUTH_IP_NOT_IN_ALLOWLIST";

// --- Task Worker ---
pub const GW_TASK_SUBMIT_FAILED: &str = "GW_TASK_SUBMIT_FAILED";
pub const GW_TASK_NOT_FOUND: &str = "GW_TASK_NOT_FOUND";
pub const GW_TASK_FREEZE_FAILED: &str = "GW_TASK_FREEZE_FAILED";
pub const GW_TASK_CANCELLED: &str = "GW_TASK_CANCELLED";

// --- fail_to_proxy surface (Phase E) ---
// Emitted when upstream closes the connection after body streaming started.
pub const GW_UPSTREAM_TRUNCATED: &str = "GW_UPSTREAM_TRUNCATED";
// Emitted when downstream client disconnects or the connection is unwritable.
pub const GW_CLIENT_DISCONNECTED: &str = "GW_CLIENT_DISCONNECTED";
// Emitted for malformed downstream requests that reach fail_to_proxy.
pub const GW_BAD_REQUEST: &str = "GW_BAD_REQUEST";
// Emitted when Pingora yields an HTTPStatus error (upstream protocol mismatch).
pub const GW_UPSTREAM_PROTOCOL: &str = "GW_UPSTREAM_PROTOCOL";
