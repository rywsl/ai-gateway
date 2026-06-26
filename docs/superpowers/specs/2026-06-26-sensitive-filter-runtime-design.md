# Sensitive Filter Runtime Enforcement Design

## Context

Go will own the global sensitive-filter policy and publish it as
`entity:sensitive_filter_policy:global`. Rust owns the gateway hot path:
downstream request capture, upstream request rendering, upstream response
handling, downstream response rendering, and detailed archive emission.

Current gateway docs and invariants intentionally forbid detailed archive
redaction. This feature changes that contract: detailed archive becomes a
filtered wire archive when a global sensitive-filter policy is enabled.

## Goals

- Load one Go-owned global sensitive-filter policy in Rust.
- Apply reversible substitutions before upstream model requests.
- Reverse those substitutions before sending responses to the client.
- Apply irreversible archive masking before `usage:detailed` is written.
- Keep streaming response replacement correct when surrogate text is split
  across chunks.
- Keep request-path failures bounded and observable.

## Non-Goals

- No Rust admin API for editing policies.
- No per-model, per-provider, per-path, per-tenant, per-user, or per-API-key
  policy binding in v1.
- No PCRE engine or unbounded backtracking regex.
- No persistence of reversible mappings.
- No attempt to reverse archive placeholders.

## Entity Contract

Rust accepts a new entity namespace:

```text
ns=sensitive_filter_policy
id=global
```

`gw-store` loads `entity:sensitive_filter_policy:global` through the existing
Redis to PostgreSQL cold-load pattern when possible. The Redis envelope is:

```text
v   policy version
enc json
d   policy JSON
```

If Redis is empty, the cold loader reads from Go-owned PostgreSQL tables and
constructs the same payload shape. The SQL must select only non-deleted,
enabled groups and rules sorted by policy order.

`stream_consumer::apply_entity_event` invalidates the Moka entry on
`ns=sensitive_filter_policy id=global`.

## Rust Types

Add policy types in `gw-core::entity` so `gw-store`, `gw-proxy`, and tests share
one wire shape:

```rust
pub struct SensitiveFilterPolicy {
    pub id: String,
    pub version: u64,
    pub groups: Vec<SensitiveFilterGroup>,
}

pub struct SensitiveFilterGroup {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub rules: Vec<SensitiveFilterRule>,
}

pub struct SensitiveFilterRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub priority: i32,
    pub mode: SensitiveFilterMode,
    pub pattern: String,
    pub replacement: String,
    pub targets: Vec<SensitiveFilterTarget>,
}
```

`SensitiveFilterMode` values:

- `transit_reversible`
- `archive_mask`

Targets:

- `request_header`
- `request_body`
- `response_header`
- `response_body`
- `archive`

Unknown targets cause snapshot rejection. Missing optional fields default to
disabled-safe values only when backward compatibility requires it; invalid
patterns reject the entire new snapshot.

## Runtime Engine

Implement the compiled engine in `gw-runtime` or a new dependency-safe helper
module that can be consumed by both `gw-store` tests and `gw-proxy`.

Responsibilities:

- compile regexes with Rust `regex`;
- preserve ordered group/rule evaluation;
- apply archive masking with `replace_all`;
- apply transit substitutions and return both transformed bytes and a
  request-local reverse map;
- maintain a streaming reverse replacer with a rolling tail buffer of
  `max_surrogate_len - 1` bytes;
- expose counters for successful replacements and snapshot load failures.

Suggested public surface:

```rust
pub struct CompiledSensitiveFilterPolicy;

pub struct TransitMap;

impl CompiledSensitiveFilterPolicy {
    pub fn compile(policy: &SensitiveFilterPolicy) -> Result<Self, CompileError>;
    pub fn mask_archive_bytes(&self, target: SensitiveFilterTarget, input: &[u8]) -> Bytes;
    pub fn replace_transit_request(&self, target: SensitiveFilterTarget, input: &[u8]) -> (Bytes, TransitMap);
    pub fn reverse_transit_response(&self, map: &TransitMap, input: &[u8], eos: bool) -> Bytes;
}
```

These method names and behavior are the implementation contract. The concrete
return type may use `Bytes`, `BytesMut`, or `Cow<[u8]>` only if the tests prove
unchanged semantics and no Pingora session object enters the engine API.

## Reversible Transit Semantics

For each `transit_reversible` match in a request:

1. Rust records the original matched text in request-local memory.
2. Rust generates a unique surrogate from the rule replacement prefix, request
   short ID, and match sequence.
3. Rust replaces the upstream-bound request text with that surrogate.
4. Rust reverses any surrogate found in the upstream response before sending it
   downstream.

The generated surrogate must be unique within a request. This prevents the
ambiguous case where two different originals match one rule and both would
otherwise become the same placeholder.

The original matched text and reverse map must never be written to logs,
`usage:records`, `usage:detailed`, Redis, PostgreSQL, or ClickHouse.

## Gateway Integration Points

### Request Capture and Upstream Request

The existing gateway captures downstream request bodies and later stores the
final upstream-bound body. Transit replacement must happen after the request has
been normalized/rendered for the chosen upstream protocol and before the
upstream request is sent.

Required behavior:

- the upstream service receives surrogate values;
- archive masking later sees already substituted upstream request bytes and
  still applies `archive_mask` rules;
- downstream request archive is masked before persistence and must not rely on
  transit replacement.

### Response Body

Non-streaming response:

- accumulate or use the existing response body path;
- reverse transit surrogates before writing the downstream body;
- then archive-mask the downstream response surface before detailed archive
  emission.

Streaming/SSE response:

- reverse surrogates chunk-by-chunk with a rolling tail buffer;
- never flush the last `max_surrogate_len - 1` bytes until enough following
  bytes arrive or EOS is reached;
- on EOS, flush the remaining tail after replacement.

### Headers

Transit header replacement is supported only for configured targets. Header
replacement must skip internal gateway control headers and hop-by-hop headers.
Archive header masking applies before detailed archive serialization.

## Archive Masking Boundary

Masking must occur before Rust writes `usage:detailed`. This is the durable
boundary that ensures Redis stream entries and Go ClickHouse inserts contain
sanitized archive data.

Apply `archive_mask` rules to all detailed archive surfaces:

- downstream request method, path, selected headers, and body;
- upstream request method, URI, selected headers, and body;
- upstream response status, selected headers, and body;
- downstream response status, selected headers, and body.

Masking should operate on bytes as UTF-8 lossily only when required by regex
processing. Non-UTF-8 bytes are preserved as much as possible; if lossy
conversion occurs, increment a metric and annotate detailed archive metadata.

## Entity Store Changes

Add to `EntityStore`:

```rust
pub sensitive_filter_policy: Cache<String, Arc<Versioned<SensitiveFilterPolicy>>>
```

Add:

```rust
pub async fn sensitive_filter_policy_or_load(&self) -> Option<Arc<Versioned<SensitiveFilterPolicy>>>
pub async fn invalidate_sensitive_filter_policy(&self, id: &str)
```

Use key `"global"`. TTL should be 300 seconds because this is an auth/security
adjacent policy and must respond quickly to operator changes.

## Invariant and Documentation Changes

The current `no_detailed_redaction` invariant must be replaced. The new
invariant should forbid ad hoc redaction helpers while allowing the named
sensitive-filter engine.

New invariant intent:

```text
detailed archive masking must go through the sensitive filter policy engine
```

Update:

- `scripts/check-invariants.sh`
- `docs/architecture.md`
- `docs/spec/06-billing-ledger.md`
- any detailed-log workflow text that currently states archive is always raw
  wire data.

## Failure Behavior

- Empty or missing policy: no-op filtering.
- Invalid newly loaded policy: keep last good compiled policy and increment
  `gw_sensitive_filter_policy_load_failed_total`.
- No last good policy: fail open with no filtering and emit a warning. This
  avoids blocking production traffic on control-plane corruption.
- Regex replacement runtime errors should not occur with compiled Rust regex;
  unexpected errors fail open for the affected surface and increment a metric.

Metrics:

```text
gw_sensitive_filter_policy_loaded_total
gw_sensitive_filter_policy_load_failed_total
gw_sensitive_filter_replacements_total{mode,target,rule_id}
gw_sensitive_filter_stream_tail_flush_total
gw_sensitive_filter_lossy_utf8_total{target}
```

## Tests

Rust unit tests:

- snapshot deserialization accepts Go payload shape;
- invalid mode, invalid target, and invalid regex reject the new snapshot;
- archive masking applies ordered rules and excludes disabled groups/rules;
- transit replacement creates unique surrogates for multiple distinct matches;
- reverse replacement restores original text for non-stream responses;
- streaming reverse replacement handles a surrogate split across chunks;
- archive masking runs before detailed archive serialization;
- `events:entity ns=sensitive_filter_policy id=global` invalidates the cache;
- missing policy is a no-op.

Integration tests:

- fake upstream receives a surrogate in the request body;
- downstream client receives the original value when upstream echoes the
  surrogate;
- `usage:detailed` stores placeholders rather than original sensitive values.

## Acceptance Criteria

- Rust hot path consumes the Go-owned global policy without per-request Go calls.
- Reversible mappings are request-local only.
- Streaming and non-streaming responses both reverse transit surrogates.
- `usage:detailed` entries are masked before Redis persistence.
- The previous raw-archive invariant is updated rather than bypassed.
