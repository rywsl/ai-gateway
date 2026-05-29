# Contract change — audit remediation (2026-05-30)

Two Rust↔Go cross-process touchpoints changed during the audit-remediation work on branch
`fix/audit-remediation-20260530`. Neither requires a Go deploy to keep working, but both are recorded here
per the CLAUDE.md sync rule. Related: [`../../issues/2026-05-29-functional-audit-findings.md`](../../issues/2026-05-29-functional-audit-findings.md),
[`../../issues/2026-05-29-aether-coverage-gaps.md`](../../issues/2026-05-29-aether-coverage-gaps.md).

## 1. `vendor_keys.payload.oauth.access_expires_at` — Rust now accepts both unix-int and RFC3339 string

**Was:** Rust decoded the field with `time::serde::timestamp` (unix-seconds **integer only**). The shared-schema
contract ([`../parts/03-shared-schema.md`](../parts/03-shared-schema.md)) documents it as an RFC3339 **string**
(`"2026-05-22T12:00:00Z"`). The mismatch failed the **whole** `VendorKeyRow` decode, so an OAuth vendor key
written per contract vanished from candidate selection (audit §1.7).

**Now:** `gw_store::loader::de::flexible_timestamp` accepts **either** a JSON integer (unix seconds) **or** an
RFC3339 string. No Go change required; the contract's RFC3339 form is now honored, and the legacy integer form
still works. **Recommended:** Go should emit the RFC3339 string as the contract specifies.

## 2. `Vendor.auth_template` — Rust now parses an optional header name

**Was:** Rust applied `auth_template.replace("{api_key}", …)` and injected the result into a hardcoded
`authorization` header, so native Gemini vendors (which authenticate via `x-goog-api-key`) could not work
(audit §3.3).

**Now:** `auth_template` may carry a header name: a template of the form `"<header-name>: <value-with-{api_key}>"`
injects under `<header-name>` (e.g. `"x-goog-api-key: {api_key}"` → header `x-goog-api-key`). A template with no
colon (e.g. `"Bearer {api_key}"`) keeps the legacy `authorization` behavior — **fully back-compatible**. A colon
with an invalid header-name token falls back leniently to `authorization` and ticks
`gw_upstream_auth_header_invalid_total`. **Recommended:** for native Gemini vendors, set
`auth_template = "x-goog-api-key: {api_key}"`.

## 3. Embeddings capability — BLOCKED on a Go-side contract (not yet implemented)

`/v1/embeddings` (audit coverage-gap §3.6) is **not** implemented in Rust. Robust routing needs Go to mark which
vendor keys are embeddings-capable (there is no `Capability::Embeddings` and the capability data is control-plane).
Until Go supplies that, a Rust-only implementation would misroute embeddings to chat-only keys; the gateway
continues to return a clean `400 GW_REQUEST_PROTOCOL_UNKNOWN`. **Action for Go:** decide whether to add an
embeddings capability to the vendor-key capability set; then the Rust side can detect + route + meter it.
