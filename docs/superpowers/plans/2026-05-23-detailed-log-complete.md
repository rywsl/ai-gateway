# 完整请求/响应归档(detailed log complete) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert `usage:detailed` from a sampled / 4 KiB-preview / 64 KiB-cap / redacted debug stream into a wire-fidelity archive stream:5 per-surface XADD per request (downstream/upstream × request/response + summary),分级压缩(raw/lz4/zstd),per-surface async trigger via bounded mpsc + flusher pool;remove all redaction and truncation;Go side becomes mandatory consumer.

**Architecture:** New `gw-ledger::detailed_archive` module hosts the `DetailedArchive` flusher pool (mpsc + N workers). Each Pingora filter that completes a surface (downstream request body read / upstream rewrite / upstream response EOS / downstream response EOS / logging) calls `archive.try_send(CapturedSurface{...})` with an `Arc<Bytes>` body reference. Workers pick `Encoding` by payload size,compress,XADD to `usage:detailed`. Hot path stays at clone-Arc + try_send. Failure modes:drop entry + metric (no DLQ).

**Tech Stack:** Rust 1.86, Pingora 0.8, fred (Redis), tokio mpsc, `lz4_flex` (new), `zstd` (new at workspace level), `serde_json`, `base64`.

**Authoritative spec:** [`docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md`](../specs/2026-05-23-detailed-log-complete-design.md)

---

## File structure

### New files

- `crates/gw-ledger/src/encoding.rs` — `Encoding` enum,`pick_encoding(len, cfg)`,`encode(json_bytes, cfg) -> (Encoding, Vec<u8>)`,`decode(encoding, bytes) -> Vec<u8>` (test-only on decode).
- `crates/gw-ledger/src/detailed_archive.rs` — `Kind` enum,payload types (`RequestPayload` / `ResponsePayload` / `SummaryPayload`),`CapturedSurface` (the mpsc message),`DetailedArchive { tx }`,`start(redis, cfg) -> Self` with N flusher workers,`try_send(...)`.
- `crates/gw-ledger/tests/encoding.rs` — tier-selection + round-trip unit tests.
- `crates/gw-ledger/tests/detailed_archive_redis.rs` — Redis-backed integration test (`#[ignore]`).

### Modified files

- `Cargo.toml` (workspace) — add `lz4_flex` + `zstd` to `[workspace.dependencies]`.
- `crates/gw-ledger/Cargo.toml` — pull `lz4_flex`,`zstd`,`base64` (already present? confirm),`tokio` (already).
- `crates/gw-ledger/src/lib.rs` — `pub mod encoding; pub mod detailed_archive;` and re-exports;**delete** old `pub mod log_sink; pub mod detailed_stream;`.
- `crates/gw-ledger/src/log_sink.rs` — **delete file**.
- `crates/gw-ledger/src/detailed_stream.rs` — **delete file**.
- `crates/gw-ledger/tests/redis_sink.rs` — **delete file**.
- `crates/gw-proxy/Cargo.toml` — add `gw-ledger` dep if not present (it is).
- `crates/gw-proxy/src/detailed_log.rs` — strip `request_body_preview` / `response_body_preview` / `redacted_preview` / `PREVIEW_MAX_BYTES`,convert `DetailedLogRecord` to the `SummaryPayload` shape.
- `crates/gw-proxy/src/ctx.rs` — add 4 body accumulators + 2 header snapshots.
- `crates/gw-proxy/src/filters/request.rs` — capture `downstream_request` body + headers at end of `process()`.
- `crates/gw-proxy/src/filters/upstream.rs::rewrite_request` — capture `upstream_request` at the point where `ctx.cached_body = outbound`.
- `crates/gw-proxy/src/filters/response.rs` — accumulate upstream raw bytes (stream + non-stream),accumulate downstream-bound bytes (after re-render),emit `upstream_response` + `downstream_response` on EOS.
- `crates/gw-proxy/src/gateway.rs` — `response_filter` snapshots upstream response headers; add `Arc<DetailedArchive>` field on `LlmGateway`.
- `crates/gw-proxy/src/filters/logging.rs` — emit `summary` to archive; remove `redacted_preview` import and tracing-only log; preserve error-path emission.
- `crates/gw-gateway/src/lib.rs` — bootstrap `DetailedArchive::start` and pass into `LlmGateway::new`.
- `crates/gw-gateway/src/config.rs` — delete `DebugCfg.sampling_rate` / `include_request_body` / `include_response_body`; add `DetailedLogCfg` + sub-types.
- `config/default.toml` — delete `[debug]` (sampling/include_body fields); add `[detailed_log]` table + sub-tables.
- `crates/gw-gateway/src/metrics_catalog.rs` — remove `gw_detailed_log_oversized_total`; add new metric names per spec §8.
- `crates/gw-gateway/tests/metrics_catalog.rs` — sync.
- `scripts/check-invariants.sh` — add `no_detailed_redaction` and `no_detailed_truncation`.
- `docs/architecture.md` — §3 (Redactor),§4.1 (`usage:detailed` row),§7 (invariants).
- `docs/workflow.md` — `DetailedLog` filter section rewrite.
- `docs/go-contracts/parts/02-go-as-consumer.md` — §2.3 rewrite.
- `docs/go-contracts/main.md` — §3.3 table.
- `CLAUDE.md` — Observability bullet.

---

## Phase 1 — Foundation: encoding tiers

### Task 1: Add `lz4_flex` and `zstd` workspace deps

**Files:**
- Modify: `Cargo.toml` (root)
- Modify: `crates/gw-ledger/Cargo.toml`

- [ ] **Step 1: Add to workspace `[workspace.dependencies]` table in root `Cargo.toml`**

Find the section that already lists `brotli`/`flate2` (~lines 76-77). Append:

```toml
lz4_flex = { version = "0.11", default-features = false, features = ["safe-encode", "safe-decode", "std"] }
zstd = { version = "0.13", default-features = false }
```

`safe-encode`/`safe-decode` keep `lz4_flex` from pulling unsafe code paths; the default `frame` feature is fine. `zstd` default features include the C wrapper which is what we want for compatibility with Go `klauspost/compress/zstd` (zstandard format frame).

- [ ] **Step 2: Wire deps into `crates/gw-ledger/Cargo.toml`**

Open the file and locate the `[dependencies]` table. Add:

```toml
lz4_flex = { workspace = true }
zstd = { workspace = true }
base64 = { workspace = true }
```

If `base64` is not in workspace deps yet, add it under `[workspace.dependencies]` in the root `Cargo.toml` first:

```toml
base64 = "0.22"
```

- [ ] **Step 3: Verify the workspace builds**

```bash
cargo check -p gw-ledger
```

Expected: clean build (no test code yet that references the new crates).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/gw-ledger/Cargo.toml Cargo.lock
git commit -m "feat(ledger): add lz4_flex + zstd workspace deps for detailed archive"
```

---

### Task 2: `encoding` module — `Encoding` enum + tier picker

**Files:**
- Create: `crates/gw-ledger/src/encoding.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Create: `crates/gw-ledger/tests/encoding.rs`

- [ ] **Step 1: Create the failing test file**

Create `crates/gw-ledger/tests/encoding.rs`:

```rust
//! Tier-selection + round-trip unit tests for `gw_ledger::encoding`.

use gw_ledger::encoding::{pick_encoding, Encoding, TierThresholds};

const T: TierThresholds = TierThresholds {
    small_threshold_bytes: 1024,
    medium_threshold_bytes: 65_536,
};

#[test]
fn picks_raw_below_small_threshold() {
    assert_eq!(pick_encoding(0, &T), Encoding::Raw);
    assert_eq!(pick_encoding(1023, &T), Encoding::Raw);
}

#[test]
fn picks_lz4_at_small_threshold_inclusive() {
    assert_eq!(pick_encoding(1024, &T), Encoding::Lz4);
    assert_eq!(pick_encoding(65_535, &T), Encoding::Lz4);
}

#[test]
fn picks_zstd_at_medium_threshold_inclusive() {
    assert_eq!(pick_encoding(65_536, &T), Encoding::Zstd);
    assert_eq!(pick_encoding(8_388_608, &T), Encoding::Zstd);
}

#[test]
fn encoding_str_matches_spec() {
    assert_eq!(Encoding::Raw.as_str(), "raw+json");
    assert_eq!(Encoding::Lz4.as_str(), "lz4+json");
    assert_eq!(Encoding::Zstd.as_str(), "zstd+json");
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p gw-ledger --test encoding
```

Expected: FAIL with `unresolved import gw_ledger::encoding`.

- [ ] **Step 3: Create `crates/gw-ledger/src/encoding.rs` with minimal types**

```rust
//! Tiered compression for `usage:detailed` archive payloads.
//!
//! Picks one of three encodings based on uncompressed JSON size; matches the
//! decision table in [`docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md`]
//! §ADR-D-3.

/// Encoding tag written to the `encoding` field on each Redis stream entry.
/// Consumers branch on this before decompressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Raw,
    Lz4,
    Zstd,
}

impl Encoding {
    pub const fn as_str(self) -> &'static str {
        match self {
            Encoding::Raw => "raw+json",
            Encoding::Lz4 => "lz4+json",
            Encoding::Zstd => "zstd+json",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TierThresholds {
    pub small_threshold_bytes: usize,
    pub medium_threshold_bytes: usize,
}

/// Pick an encoding based on the uncompressed payload length.
pub fn pick_encoding(len: usize, t: &TierThresholds) -> Encoding {
    if len < t.small_threshold_bytes {
        Encoding::Raw
    } else if len < t.medium_threshold_bytes {
        Encoding::Lz4
    } else {
        Encoding::Zstd
    }
}
```

- [ ] **Step 4: Expose the module from `lib.rs`**

Open `crates/gw-ledger/src/lib.rs` and add:

```rust
pub mod encoding;
```

(Leave the existing module declarations untouched for now; they get removed in Phase 4.)

- [ ] **Step 5: Run the test to verify it passes**

```bash
cargo test -p gw-ledger --test encoding
```

Expected: PASS — 4/4.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-ledger/src/encoding.rs crates/gw-ledger/src/lib.rs crates/gw-ledger/tests/encoding.rs
git commit -m "feat(ledger): encoding::pick_encoding tier selector for detailed archive"
```

---

### Task 3: `encoding::encode` — actually compress

**Files:**
- Modify: `crates/gw-ledger/src/encoding.rs`
- Modify: `crates/gw-ledger/tests/encoding.rs`

- [ ] **Step 1: Add failing tests**

Append to `crates/gw-ledger/tests/encoding.rs`:

```rust
use gw_ledger::encoding::{encode, EncodeError, CompressionCfg};

fn cfg() -> CompressionCfg {
    CompressionCfg {
        thresholds: T,
        zstd_level: 3,
    }
}

#[test]
fn encode_raw_passthrough() {
    let input = b"hello".to_vec();
    let (enc, out) = encode(input.clone(), &cfg()).expect("encode");
    assert_eq!(enc, Encoding::Raw);
    assert_eq!(out, input);
}

#[test]
fn encode_lz4_shrinks_repetitive() {
    let input = "abc".repeat(2_000); // 6 KiB, falls in lz4 tier; highly compressible
    let bytes = input.into_bytes();
    let (enc, out) = encode(bytes.clone(), &cfg()).expect("encode");
    assert_eq!(enc, Encoding::Lz4);
    assert!(out.len() < bytes.len() / 4, "lz4 should shrink repetitive bytes meaningfully");
}

#[test]
fn encode_zstd_shrinks_large_repetitive() {
    let input = "xyz".repeat(50_000); // 150 KiB, zstd tier
    let bytes = input.into_bytes();
    let (enc, out) = encode(bytes.clone(), &cfg()).expect("encode");
    assert_eq!(enc, Encoding::Zstd);
    assert!(out.len() < bytes.len() / 10, "zstd should shrink very repetitive bytes hard");
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p gw-ledger --test encoding
```

Expected: FAIL with `unresolved import gw_ledger::encoding::encode` (and friends).

- [ ] **Step 3: Implement `encode` + `CompressionCfg` + `EncodeError`**

Append to `crates/gw-ledger/src/encoding.rs`:

```rust
#[derive(Debug, Clone, Copy)]
pub struct CompressionCfg {
    pub thresholds: TierThresholds,
    pub zstd_level: i32,
}

#[derive(Debug)]
pub enum EncodeError {
    /// `zstd` returned an error. `lz4_flex` infallibly encodes; `Raw` never errors.
    Zstd(std::io::Error),
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Zstd(e) => write!(f, "zstd encode failed: {e}"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// Encode a JSON-serialized payload. The caller owns input; we return the
/// (possibly identical) compressed bytes. `Raw` returns the input unchanged.
pub fn encode(payload: Vec<u8>, cfg: &CompressionCfg) -> Result<(Encoding, Vec<u8>), EncodeError> {
    let enc = pick_encoding(payload.len(), &cfg.thresholds);
    let out = match enc {
        Encoding::Raw => payload,
        Encoding::Lz4 => lz4_flex::compress_prepend_size(&payload),
        Encoding::Zstd => zstd::encode_all(payload.as_slice(), cfg.zstd_level).map_err(EncodeError::Zstd)?,
    };
    Ok((enc, out))
}
```

> **Note for the next task:** `lz4_flex::compress_prepend_size` writes a `u32` little-endian length prefix that `decompress_size_prepended` consumes. Go consumers using `pierrec/lz4` will need to read that prefix; document this in §6 of the spec already accounts for it via "lz4+json" being the framed-with-size variant.

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p gw-ledger --test encoding
```

Expected: PASS — 7/7.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-ledger/src/encoding.rs crates/gw-ledger/tests/encoding.rs
git commit -m "feat(ledger): encoding::encode implements raw/lz4/zstd compression tiers"
```

---

### Task 4: `encoding::decode` — round-trip helper (test-only API)

**Files:**
- Modify: `crates/gw-ledger/src/encoding.rs`
- Modify: `crates/gw-ledger/tests/encoding.rs`

- [ ] **Step 1: Add failing round-trip test**

Append to `crates/gw-ledger/tests/encoding.rs`:

```rust
use gw_ledger::encoding::decode;

#[test]
fn round_trip_raw() {
    let input = b"under-one-kib".to_vec();
    let (enc, encoded) = encode(input.clone(), &cfg()).unwrap();
    assert_eq!(enc, Encoding::Raw);
    let decoded = decode(enc, &encoded).unwrap();
    assert_eq!(decoded, input);
}

#[test]
fn round_trip_lz4() {
    let input = "lz4-tier-".repeat(200).into_bytes(); // ~ 1.8 KiB
    let (enc, encoded) = encode(input.clone(), &cfg()).unwrap();
    assert_eq!(enc, Encoding::Lz4);
    let decoded = decode(enc, &encoded).unwrap();
    assert_eq!(decoded, input);
}

#[test]
fn round_trip_zstd() {
    let input = "zstd-tier-".repeat(20_000).into_bytes(); // ~ 200 KiB
    let (enc, encoded) = encode(input.clone(), &cfg()).unwrap();
    assert_eq!(enc, Encoding::Zstd);
    let decoded = decode(enc, &encoded).unwrap();
    assert_eq!(decoded, input);
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p gw-ledger --test encoding
```

Expected: FAIL with `unresolved import gw_ledger::encoding::decode`.

- [ ] **Step 3: Implement `decode`**

Append to `crates/gw-ledger/src/encoding.rs`:

```rust
#[derive(Debug)]
pub enum DecodeError {
    Lz4(lz4_flex::block::DecompressError),
    Zstd(std::io::Error),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Lz4(e) => write!(f, "lz4 decode failed: {e}"),
            DecodeError::Zstd(e) => write!(f, "zstd decode failed: {e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Test-side helper that inverts `encode`. Production code in this crate
/// never decodes; only the Redis-backed integration test does.
pub fn decode(enc: Encoding, bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    match enc {
        Encoding::Raw => Ok(bytes.to_vec()),
        Encoding::Lz4 => lz4_flex::decompress_size_prepended(bytes).map_err(DecodeError::Lz4),
        Encoding::Zstd => zstd::decode_all(bytes).map_err(DecodeError::Zstd),
    }
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p gw-ledger --test encoding
```

Expected: PASS — 10/10.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-ledger/src/encoding.rs crates/gw-ledger/tests/encoding.rs
git commit -m "feat(ledger): encoding::decode helper for round-trip verification"
```

---

## Phase 2 — Foundation: archive types + flusher

### Task 5: `detailed_archive` types — `Kind`,payloads,`CapturedSurface`

**Files:**
- Create: `crates/gw-ledger/src/detailed_archive.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: Add module to `lib.rs`**

Append to `crates/gw-ledger/src/lib.rs`:

```rust
pub mod detailed_archive;
```

- [ ] **Step 2: Create the file with the type set**

Create `crates/gw-ledger/src/detailed_archive.rs`:

```rust
//! Detailed archive — per-surface, wire-fidelity request/response capture.
//!
//! Spec: `docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md`.
//! Each Pingora filter that completes a surface calls `DetailedArchive::try_send`
//! with a [`CapturedSurface`]; a background flusher pool encodes (raw/lz4/zstd)
//! and XADDs to `usage:detailed`.

use bytes::Bytes;
use gw_core::ids::{ApiKeyId, ModelName, RequestId, TraceId, UserId};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    DownstreamRequest,
    UpstreamRequest,
    UpstreamResponse,
    DownstreamResponse,
    Summary,
}

impl Kind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::DownstreamRequest => "downstream_request",
            Kind::UpstreamRequest => "upstream_request",
            Kind::UpstreamResponse => "upstream_response",
            Kind::DownstreamResponse => "downstream_response",
            Kind::Summary => "summary",
        }
    }
}

/// JSON payload for `kind = *_request`.
#[derive(Debug, Serialize)]
pub struct RequestPayload {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
    /// Base64-standard encoding of the raw body bytes.
    pub body_b64: String,
}

/// JSON payload for `kind = *_response`.
#[derive(Debug, Serialize)]
pub struct ResponsePayload {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_b64: String,
}

/// JSON payload for `kind = summary`. Matches the surviving fields on the
/// historical `DetailedLogRecord` (body previews removed).
#[derive(Debug, Serialize)]
pub struct SummaryPayload {
    pub user_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub model: Option<ModelName>,
    pub client_protocol: Option<String>,
    pub upstream_protocol: Option<String>,
    pub http_status: u16,
    pub error_code: Option<String>,
    pub is_stream: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub tunnel_resolution: Option<String>,
}

/// Mpsc message: a single surface captured at its trigger point.
/// The flusher pool consumes these, picks an encoding, and XADDs.
pub struct CapturedSurface {
    pub ts_ms: i64,
    pub request_id: RequestId,
    pub trace_id: TraceId,
    pub kind: Kind,
    pub payload: SurfacePayload,
}

pub enum SurfacePayload {
    Request(RequestPayload, Arc<Bytes>),
    Response(ResponsePayload, Arc<Bytes>),
    Summary(SummaryPayload),
}
```

> **Why `Arc<Bytes>` next to the JSON payload struct:** the body is huge and held by `Bytes` (refcount). The headers/method/status are small and live inline. The flusher reads `payload.body_b64` from `Arc<Bytes>` lazily (`base64::encode` over the bytes) so we don't pay encoding cost on the hot path.

- [ ] **Step 3: Verify compile**

```bash
cargo check -p gw-ledger
```

Expected: clean compile.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-ledger/src/detailed_archive.rs crates/gw-ledger/src/lib.rs
git commit -m "feat(ledger): detailed_archive types (Kind, payloads, CapturedSurface)"
```

---

### Task 6: `DetailedArchive` flusher pool — start + try_send

**Files:**
- Modify: `crates/gw-ledger/src/detailed_archive.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: Append config + struct to `detailed_archive.rs`**

```rust
use crate::encoding::{encode, CompressionCfg, Encoding};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use fred::clients::RedisPool;
use fred::interfaces::StreamsInterface;
use fred::types::{XCap, XCapKind, XCapTrim};
use tokio::sync::mpsc;

/// Boot-time config; mirrors the `[detailed_log]` table in `config/default.toml`.
#[derive(Debug, Clone)]
pub struct DetailedArchiveCfg {
    pub stream: String,
    pub maxlen: u64,
    pub channel_capacity: usize,
    pub workers: usize,
    pub single_payload_soft_cap_bytes: usize,
    pub compression: CompressionCfg,
}

/// Sender handle held by `LlmGateway`; cheaply cloneable.
#[derive(Clone)]
pub struct DetailedArchive {
    tx: mpsc::Sender<CapturedSurface>,
}

impl DetailedArchive {
    pub fn start(redis: RedisPool, cfg: DetailedArchiveCfg) -> Self {
        let (tx, rx) = mpsc::channel::<CapturedSurface>(cfg.channel_capacity);
        let workers = cfg.workers.max(1);
        let shared_rx = std::sync::Arc::new(tokio::sync::Mutex::new(rx));
        for _ in 0..workers {
            let rx = shared_rx.clone();
            let cfg = cfg.clone();
            let redis = redis.clone();
            tokio::spawn(worker(rx, redis, cfg));
        }
        Self { tx }
    }

    /// Best-effort enqueue. On a full channel we drop + metric. Never blocks.
    pub fn try_send(&self, surface: CapturedSurface) {
        let kind_label = surface.kind.as_str();
        if self.tx.try_send(surface).is_err() {
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "channel_full",
                "kind" => kind_label,
            )
            .increment(1);
        }
    }
}

fn cap(maxlen: u64) -> XCap {
    XCap::try_from((XCapKind::MaxLen, XCapTrim::AlmostExact, maxlen))
        .expect("XCap::try_from infallible for (MaxLen, AlmostExact, u64)")
}

async fn worker(
    rx: std::sync::Arc<tokio::sync::Mutex<mpsc::Receiver<CapturedSurface>>>,
    redis: RedisPool,
    cfg: DetailedArchiveCfg,
) {
    loop {
        let surface = {
            let mut guard = rx.lock().await;
            match guard.recv().await {
                Some(s) => s,
                None => return, // channel closed → exit worker
            }
        };
        handle(surface, &redis, &cfg).await;
    }
}

async fn handle(surface: CapturedSurface, redis: &RedisPool, cfg: &DetailedArchiveCfg) {
    let CapturedSurface { ts_ms, request_id, trace_id, kind, payload } = surface;
    let kind_label = kind.as_str();

    let json_bytes = match serialize_payload(payload, cfg.single_payload_soft_cap_bytes, kind_label) {
        Some(b) => b,
        None => return,
    };

    let pre_compress_len = json_bytes.len();
    metrics::histogram!("gw_detailed_log_pre_compress_bytes", "kind" => kind_label)
        .record(pre_compress_len as f64);

    let start = std::time::Instant::now();
    let (encoding, encoded) = match encode(json_bytes, &cfg.compression) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, "detailed archive encode failed; dropping");
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "compress_failed",
                "kind" => kind_label,
            )
            .increment(1);
            return;
        }
    };
    let elapsed = start.elapsed().as_secs_f64();
    metrics::histogram!("gw_detailed_log_compress_seconds", "encoding" => encoding.as_str())
        .record(elapsed);
    metrics::histogram!(
        "gw_detailed_log_payload_bytes",
        "kind" => kind_label,
        "encoding" => encoding.as_str(),
    )
    .record(encoded.len() as f64);

    let client = redis.next();
    let r: Result<String, _> = client
        .xadd(
            cfg.stream.as_str(),
            false,
            cap(cfg.maxlen),
            "*",
            vec![
                ("ts_ms", ts_ms.to_string()),
                ("request_id", request_id.as_ref().to_string()),
                ("trace_id", trace_id.as_ref().to_string()),
                ("kind", kind_label.to_string()),
                ("encoding", encoding.as_str().to_string()),
            ],
        )
        .await;

    // The XADD signature above takes name/value pairs; we'll add the `payload`
    // bytes via a second `xadd` argument shape if fred supports it; otherwise
    // fold all fields into a single call. See Step 3 below.

    // Placeholder — replaced in Step 3.
    let _ = r;
    let _ = encoded;
}

fn serialize_payload(
    payload: SurfacePayload,
    soft_cap: usize,
    kind_label: &'static str,
) -> Option<Vec<u8>> {
    let result = match payload {
        SurfacePayload::Request(mut req, body) => {
            req.body_b64 = B64.encode(body.as_ref());
            serde_json::to_vec(&req)
        }
        SurfacePayload::Response(mut resp, body) => {
            resp.body_b64 = B64.encode(body.as_ref());
            serde_json::to_vec(&resp)
        }
        SurfacePayload::Summary(s) => serde_json::to_vec(&s),
    };
    match result {
        Ok(v) if v.len() > soft_cap => {
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "oversize",
                "kind" => kind_label,
            )
            .increment(1);
            None
        }
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(target: "gw_ledger", error = %e, "detailed archive serialize failed");
            metrics::counter!(
                "gw_detailed_log_drop_total",
                "reason" => "serialize_failed",
                "kind" => kind_label,
            )
            .increment(1);
            None
        }
    }
}
```

> **Step 3 will replace the placeholder XADD with the real `payload` binary field; fred's `xadd` is variadic over key-value pairs and the byte-vec arg type — confirm the right call shape against `crates/gw-ledger/src/log_sink.rs:73` (the existing single-field example) before this step closes.**

- [ ] **Step 2: Look at the existing XADD call shape in `log_sink.rs` and adjust**

Read `crates/gw-ledger/src/log_sink.rs:73-81` for the pattern; mirror it. The existing pattern is `("payload", bytes)` — a single field tuple. We need 6 fields: `ts_ms`, `request_id`, `trace_id`, `kind`, `encoding`, `payload`. The fred `xadd` signature accepts a `Vec<(&str, RedisValue)>` (or similar); use that.

Replace the body of `handle()` after the `encode()` call with:

```rust
    let fields: Vec<(&'static str, fred::types::RedisValue)> = vec![
        ("ts_ms", ts_ms.into()),
        ("request_id", request_id.as_ref().to_string().into()),
        ("trace_id", trace_id.as_ref().to_string().into()),
        ("kind", kind_label.into()),
        ("encoding", encoding.as_str().into()),
        ("payload", encoded.into()),
    ];
    let client = redis.next();
    let r: Result<String, _> = client
        .xadd(cfg.stream.as_str(), false, cap(cfg.maxlen), "*", fields)
        .await;
    if let Err(e) = r {
        tracing::warn!(
            target: "gw_ledger",
            error = %e,
            stream = %cfg.stream,
            "detailed archive XADD failed; dropping"
        );
        metrics::counter!(
            "gw_detailed_log_drop_total",
            "reason" => "xadd_failed",
            "kind" => kind_label,
        )
        .increment(1);
    } else {
        metrics::counter!(
            "gw_detailed_log_emit_total",
            "kind" => kind_label,
            "encoding" => encoding.as_str(),
        )
        .increment(1);
    }
```

Also delete the placeholder XADD lines and the stray `let _ = r; let _ = encoded;` at the end of `handle()` from Step 1.

- [ ] **Step 3: Verify compile**

```bash
cargo check -p gw-ledger
```

If `fred` rejects the `(&'static str, RedisValue)` tuple shape, swap to whatever shape `log_sink.rs:73` uses (`("payload", bytes)`). The fred API at v9.x typically accepts `impl Into<MultipleOrderedPairs>`; the safest call is one tuple per field passed as a `Vec`.

Iterate until clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-ledger/src/detailed_archive.rs
git commit -m "feat(ledger): DetailedArchive flusher pool (mpsc + workers + XADD)"
```

---

### Task 7: `try_send` drop counter unit test

**Files:**
- Create: `crates/gw-ledger/tests/detailed_archive.rs`

- [ ] **Step 1: Write failing test**

Create `crates/gw-ledger/tests/detailed_archive.rs`:

```rust
//! Unit-level checks for `DetailedArchive`. The Redis-backed end-to-end test
//! lives in `tests/detailed_archive_redis.rs` (gated on Docker).

use bytes::Bytes;
use gw_core::ids::{RequestId, TraceId};
use gw_ledger::detailed_archive::{
    CapturedSurface, DetailedArchive, DetailedArchiveCfg, Kind, RequestPayload, SurfacePayload,
};
use gw_ledger::encoding::{CompressionCfg, TierThresholds};
use std::sync::Arc;

fn cfg(capacity: usize) -> DetailedArchiveCfg {
    DetailedArchiveCfg {
        stream: "usage:detailed".to_string(),
        maxlen: 1_000_000,
        channel_capacity: capacity,
        workers: 1,
        single_payload_soft_cap_bytes: 8 * 1024 * 1024,
        compression: CompressionCfg {
            thresholds: TierThresholds {
                small_threshold_bytes: 1024,
                medium_threshold_bytes: 65_536,
            },
            zstd_level: 3,
        },
    }
}

fn surface(kind: Kind) -> CapturedSurface {
    CapturedSurface {
        ts_ms: 1,
        request_id: RequestId::from("r_test".to_string()),
        trace_id: TraceId::from("t_test".to_string()),
        kind,
        payload: SurfacePayload::Request(
            RequestPayload {
                method: "POST".to_string(),
                url: "/".to_string(),
                http_version: "HTTP/1.1".to_string(),
                headers: vec![],
                body_b64: String::new(),
            },
            Arc::new(Bytes::from_static(b"hi")),
        ),
    }
}

/// We can't construct a `RedisPool` without a real Redis. To exercise the
/// drop-on-full path without one, this test reaches into the channel via a
/// private helper. Marker: see `DetailedArchive::for_test_with_dummy_redis`
/// added in Step 3.
#[tokio::test]
async fn try_send_drops_when_channel_full() {
    let archive = DetailedArchive::for_test_with_dummy_workers(cfg(1));
    // First send fills the bounded channel (capacity 1).
    archive.try_send(surface(Kind::DownstreamRequest));
    // Second send must be dropped, not block.
    archive.try_send(surface(Kind::DownstreamRequest));
    // Metric assertions are skipped here; the integration test verifies the
    // counter increment. This test only proves try_send is non-blocking.
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p gw-ledger --test detailed_archive
```

Expected: FAIL with `unresolved import gw_ledger::detailed_archive::DetailedArchive::for_test_with_dummy_workers` (or compile error on the missing function).

- [ ] **Step 3: Add the test helper to `detailed_archive.rs`**

In `crates/gw-ledger/src/detailed_archive.rs`, add an `#[cfg(test)]`-gated constructor:

```rust
impl DetailedArchive {
    /// Test-only: spawn workers that immediately return on each message
    /// (no Redis call). Lets us cover the drop-on-full path without Docker.
    #[doc(hidden)]
    pub fn for_test_with_dummy_workers(cfg: DetailedArchiveCfg) -> Self {
        let (tx, mut rx) = mpsc::channel::<CapturedSurface>(cfg.channel_capacity);
        // Drain the channel slowly so the second `try_send` lands on a full
        // queue. A 50 ms sleep per recv is well above `try_send`'s deadline.
        tokio::spawn(async move {
            while let Some(_s) = rx.recv().await {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        });
        Self { tx }
    }
}
```

Place this `impl DetailedArchive { ... }` block **after** the existing `impl DetailedArchive { ... }`. (Two impl blocks for one type is legal.)

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p gw-ledger --test detailed_archive
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-ledger/src/detailed_archive.rs crates/gw-ledger/tests/detailed_archive.rs
git commit -m "test(ledger): DetailedArchive try_send is non-blocking when channel full"
```

---

## Phase 3 — Config + bootstrap

### Task 8: Config types `DetailedLogCfg` + sub-tables

**Files:**
- Modify: `crates/gw-gateway/src/config.rs`

- [ ] **Step 1: Add the new config types**

Find the existing `DebugCfg` (`crates/gw-gateway/src/config.rs:93-98`). Add **above** it:

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct DetailedLogCfg {
    #[serde(default = "default_detailed_enabled")]
    pub enabled: bool,
    #[serde(default = "default_detailed_stream")]
    pub stream: String,
    #[serde(default = "default_detailed_maxlen")]
    pub maxlen: u64,
    #[serde(default)]
    pub compression: DetailedCompressionCfg,
    #[serde(default)]
    pub channel: DetailedChannelCfg,
    #[serde(default)]
    pub flusher: DetailedFlusherCfg,
    #[serde(default)]
    pub limits: DetailedLimitsCfg,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedCompressionCfg {
    #[serde(default = "default_small_threshold")]
    pub small_threshold_bytes: usize,
    #[serde(default = "default_medium_threshold")]
    pub medium_threshold_bytes: usize,
    #[serde(default = "default_zstd_level")]
    pub zstd_level: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedChannelCfg {
    #[serde(default = "default_channel_capacity")]
    pub capacity: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedFlusherCfg {
    #[serde(default)]
    pub workers: usize, // 0 = auto
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetailedLimitsCfg {
    #[serde(default = "default_single_payload_soft_cap")]
    pub single_payload_soft_cap_bytes: usize,
}

fn default_detailed_enabled() -> bool { true }
fn default_detailed_stream() -> String { "usage:detailed".to_string() }
fn default_detailed_maxlen() -> u64 { 10_000_000 }
fn default_small_threshold() -> usize { 1024 }
fn default_medium_threshold() -> usize { 65_536 }
fn default_zstd_level() -> i32 { 3 }
fn default_channel_capacity() -> usize { 65_536 }
fn default_single_payload_soft_cap() -> usize { 8 * 1024 * 1024 }

impl Default for DetailedCompressionCfg {
    fn default() -> Self {
        Self {
            small_threshold_bytes: default_small_threshold(),
            medium_threshold_bytes: default_medium_threshold(),
            zstd_level: default_zstd_level(),
        }
    }
}

impl Default for DetailedChannelCfg {
    fn default() -> Self { Self { capacity: default_channel_capacity() } }
}

impl Default for DetailedFlusherCfg {
    fn default() -> Self { Self { workers: 0 } }
}

impl Default for DetailedLimitsCfg {
    fn default() -> Self {
        Self { single_payload_soft_cap_bytes: default_single_payload_soft_cap() }
    }
}
```

- [ ] **Step 2: Replace `DebugCfg` body**

The historical `DebugCfg` carries `sampling_rate / include_request_body / include_response_body`. The spec retires all three. Replace the struct (lines ~93-98) with an empty marker that figment can still bind even if `[debug]` appears in legacy `local.toml`:

```rust
/// Reserved table; historical `sampling_rate` / `include_*_body` knobs are
/// gone — detailed-log behaviour now lives under `[detailed_log]`. Kept as
/// an empty struct to keep figment happy when a stale `local.toml` still
/// contains a `[debug]` header.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DebugCfg {}
```

The `deny_unknown_fields` makes figment reject `sampling_rate = 0.0001` etc. with a clear error pointing the user at the migration — matches the user's `feedback_breaking_changes_ok` rule (no compat shims).

- [ ] **Step 3: Plumb the new field into the top-level `Config`**

Find the top-level `pub struct Config { ... }` (search `pub struct Config` in `crates/gw-gateway/src/config.rs`). Add:

```rust
    #[serde(default)]
    pub detailed_log: DetailedLogCfg,
```

If `DetailedLogCfg` needs a `Default` impl for `#[serde(default)]`, add one — all sub-fields already have defaults, so:

```rust
impl Default for DetailedLogCfg {
    fn default() -> Self {
        Self {
            enabled: default_detailed_enabled(),
            stream: default_detailed_stream(),
            maxlen: default_detailed_maxlen(),
            compression: DetailedCompressionCfg::default(),
            channel: DetailedChannelCfg::default(),
            flusher: DetailedFlusherCfg::default(),
            limits: DetailedLimitsCfg::default(),
        }
    }
}
```

- [ ] **Step 4: Remove the validation block for `debug.sampling_rate`**

Find lines ~280-284 (`if !(0.0..=1.0).contains(&self.debug.sampling_rate) { ... }`). Delete the block.

- [ ] **Step 5: Verify compile**

```bash
cargo check -p gw-gateway
```

Expected: clean (places that read `ctx.debug.include_*_body` may still compile because the field was on `DebugMode` not `DebugCfg` — those are decoupled).

- [ ] **Step 6: Commit**

```bash
git add crates/gw-gateway/src/config.rs
git commit -m "feat(gateway): replace [debug] sampling/include_*_body with [detailed_log] table"
```

---

### Task 9: `config/default.toml` — replace `[debug]` with `[detailed_log]`

**Files:**
- Modify: `config/default.toml`

- [ ] **Step 1: Edit the file**

Delete the `[debug]` block (lines ~42-46):

```toml
[debug]
# Sampled detailed request/response log. ADR-027.
sampling_rate = 0.0001
include_request_body = false
include_response_body = false
```

Replace with:

```toml
[detailed_log]
# Wire-fidelity request/response archive. Spec:
# docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md
enabled = true
stream = "usage:detailed"
maxlen = 10_000_000

[detailed_log.compression]
small_threshold_bytes  = 1024
medium_threshold_bytes = 65536
zstd_level = 3

[detailed_log.channel]
capacity = 65536

[detailed_log.flusher]
workers = 0   # 0 = num_cpus.saturating_div(4).max(1)

[detailed_log.limits]
single_payload_soft_cap_bytes = 8_388_608
```

- [ ] **Step 2: Verify the loader still parses**

```bash
cargo test -p gw-gateway config
```

If `gw-gateway` has no `config` test target, run instead:

```bash
cargo run --release -p gw-gateway 2>&1 | head -20
# expect early bootstrap log lines; kill after a few seconds with Ctrl-C
```

(Or just `cargo build -p gw-gateway --release && timeout 3 ./target/release/gw-gateway || true` if Postgres/Redis are not available — we just want figment to accept the TOML.)

- [ ] **Step 3: Commit**

```bash
git add config/default.toml
git commit -m "feat(config): replace [debug] sampling knobs with [detailed_log] table"
```

---

### Task 10: Bootstrap `DetailedArchive` in `gw-gateway/src/lib.rs` and pass into `LlmGateway`

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`

- [ ] **Step 1: Add the `DetailedArchive` field on `LlmGateway`**

Open `crates/gw-proxy/src/gateway.rs`. Find the `pub struct LlmGateway { ... }` definition (it follows the `MAX_CONNECT_RETRIES` constant, around line ~100). Add a new field:

```rust
    pub(crate) detailed_archive: std::sync::Arc<gw_ledger::detailed_archive::DetailedArchive>,
```

Find `impl LlmGateway { pub fn new(...) -> Self { ... } }` (search `fn new(`). Add `detailed_archive: Arc<DetailedArchive>` as the last constructor parameter, store it on the new instance.

- [ ] **Step 2: Bootstrap in `gw-gateway::lib::serve`**

Open `crates/gw-gateway/src/lib.rs`. Find the `let gw = LlmGateway::new(` call (`grep -n "LlmGateway::new" crates/gw-gateway/src/lib.rs`). Before that call, add:

```rust
    let archive_cfg = gw_ledger::detailed_archive::DetailedArchiveCfg {
        stream: cfg.detailed_log.stream.clone(),
        maxlen: cfg.detailed_log.maxlen,
        channel_capacity: cfg.detailed_log.channel.capacity,
        workers: if cfg.detailed_log.flusher.workers == 0 {
            (num_cpus::get() / 4).max(1)
        } else {
            cfg.detailed_log.flusher.workers
        },
        single_payload_soft_cap_bytes: cfg.detailed_log.limits.single_payload_soft_cap_bytes,
        compression: gw_ledger::encoding::CompressionCfg {
            thresholds: gw_ledger::encoding::TierThresholds {
                small_threshold_bytes: cfg.detailed_log.compression.small_threshold_bytes,
                medium_threshold_bytes: cfg.detailed_log.compression.medium_threshold_bytes,
            },
            zstd_level: cfg.detailed_log.compression.zstd_level,
        },
    };
    let detailed_archive = std::sync::Arc::new(
        gw_ledger::detailed_archive::DetailedArchive::start(redis_pool.clone(), archive_cfg),
    );
```

Where `redis_pool` is the existing `RedisPool` already constructed earlier in `serve()` (search the file for `RedisPool::new` or `pool` to find the canonical name; if it's `redis_ctx.pool` or similar, adapt accordingly).

If `num_cpus` is not already a workspace dep, prefer `std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)`:

```rust
        workers: if cfg.detailed_log.flusher.workers == 0 {
            (std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1) / 4)
                .max(1)
        } else {
            cfg.detailed_log.flusher.workers
        },
```

Add `detailed_archive.clone()` as the new argument to `LlmGateway::new(...)`.

- [ ] **Step 3: Verify build**

```bash
cargo build --workspace
```

Expected: clean compile.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-gateway/src/lib.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(gateway): bootstrap DetailedArchive flusher pool and inject into LlmGateway"
```

---

## Phase 4 — Capture: ReqCtx + 5 filter hooks

### Task 11: Add capture fields to `ReqCtx`

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs`

- [ ] **Step 1: Add 6 fields to `ReqCtx`**

Find the struct definition (`pub struct ReqCtx`, line ~43). Add a new section before `// --- Debug ---`:

```rust
    // --- Detailed archive (wire-fidelity capture) ---
    /// Raw client-facing request body bytes (before any codec render). `None`
    /// until `request_filter` has finished reading the body.
    pub downstream_request_body: Option<Arc<Bytes>>,
    /// Method+URL+headers snapshot for the downstream request. Captured at the
    /// same point as `downstream_request_body`.
    pub downstream_request_meta: Option<DownstreamRequestMeta>,
    /// Final upstream-bound request body bytes. Captured at the end of
    /// `upstream::rewrite_request` (after any codec re-render).
    pub upstream_request_body: Option<Arc<Bytes>>,
    /// Method+URL+headers snapshot for the upstream-bound request.
    pub upstream_request_meta: Option<UpstreamRequestMeta>,
    /// Accumulator for raw upstream response bytes (always-on; the legacy B1
    /// `upstream_body_accum` was streaming-conditional and is now subsumed).
    pub archive_upstream_response_body: bytes::BytesMut,
    /// Accumulator for downstream-bound (post-render) response bytes.
    pub archive_downstream_response_body: bytes::BytesMut,
    /// Upstream response headers + status snapshot, taken in `response_filter`.
    pub archive_upstream_response_meta: Option<ResponseMeta>,
    /// Downstream response headers + status snapshot, taken at EOS in
    /// `response_body_filter` (after any codec header rewrite via response_filter).
    pub archive_downstream_response_meta: Option<ResponseMeta>,
```

At the **top** of `ctx.rs` add the three meta types (above `pub struct ReqCtx`):

```rust
#[derive(Debug, Clone)]
pub struct DownstreamRequestMeta {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct UpstreamRequestMeta {
    pub method: String,
    pub url: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ResponseMeta {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}
```

- [ ] **Step 2: Initialize in `ReqCtx::new`**

Find `impl ReqCtx { pub fn new(...) -> Self { ... } }` (line ~182). At the end of the struct initializer (before the closing `}` of the `Self { ... }`), add:

```rust
            downstream_request_body: None,
            downstream_request_meta: None,
            upstream_request_body: None,
            upstream_request_meta: None,
            archive_upstream_response_body: bytes::BytesMut::new(),
            archive_downstream_response_body: bytes::BytesMut::new(),
            archive_upstream_response_meta: None,
            archive_downstream_response_meta: None,
```

- [ ] **Step 3: Verify compile**

```bash
cargo check -p gw-proxy
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs
git commit -m "feat(proxy): ReqCtx fields for wire-fidelity detailed archive capture"
```

---

### Task 12: Capture `downstream_request` in `filters::request::process`

**Files:**
- Modify: `crates/gw-proxy/src/filters/request.rs`

- [ ] **Step 1: Snapshot the body + meta right after `body_bytes`**

Find line 424-425 in `crates/gw-proxy/src/filters/request.rs`:

```rust
    let body_bytes = bytes::Bytes::from(body_buf);
    ctx.cached_body = Some(body_bytes.clone());
```

Immediately after `ctx.cached_body = Some(body_bytes.clone());`, insert:

```rust
    // Wire-fidelity archive: snapshot the raw downstream request. `cached_body`
    // is overwritten by `upstream::rewrite_request` on the cross-protocol path,
    // so the snapshot must happen now while we still have the original bytes.
    ctx.downstream_request_body = Some(std::sync::Arc::new(body_bytes.clone()));
    {
        use crate::ctx::DownstreamRequestMeta;
        let req = session.req_header();
        ctx.downstream_request_meta = Some(DownstreamRequestMeta {
            method: req.method.as_str().to_owned(),
            url: req.uri.to_string(),
            http_version: format!("{:?}", req.version),
            headers: req
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect(),
        });
    }
```

The headers loop uses `v.to_str().unwrap_or("")` — header bytes that aren't valid UTF-8 (rare but possible for binary value headers) get an empty string. That's deliberate; the spec accepts wire fidelity at the body level, not at the header value level.

- [ ] **Step 2: Wire the archive call site (still in this same task, but separate file)**

We need to invoke `archive.try_send(...)` after the snapshot is in place. The archive handle lives on `LlmGateway`; the `request_filter` callback (`crates/gw-proxy/src/gateway.rs:176`) has `&self` access. Update the gateway's `request_filter` to wrap the existing `filters::request::run(...)` call:

In `crates/gw-proxy/src/gateway.rs`, find `async fn request_filter(...)` (line 176). Replace:

```rust
    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        filters::request::run(
            session,
            ctx,
            &self.store,
            &self.ratelimit,
            &self.counters,
            self.global_rpm,
            self.trust_xff,
            self.jwt_validator.as_ref(),
            self.capability_resolver.as_ref(),
            &self.mgmt_blacklist,
        )
        .await
    }
```

with:

```rust
    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let short_circuit = filters::request::run(
            session,
            ctx,
            &self.store,
            &self.ratelimit,
            &self.counters,
            self.global_rpm,
            self.trust_xff,
            self.jwt_validator.as_ref(),
            self.capability_resolver.as_ref(),
            &self.mgmt_blacklist,
        )
        .await?;
        // Emit downstream_request surface (only if process() actually read the
        // body; some short-circuit paths bail before line 425 of request.rs).
        if let (Some(body), Some(meta)) =
            (ctx.downstream_request_body.clone(), ctx.downstream_request_meta.clone())
        {
            use gw_ledger::detailed_archive::{
                CapturedSurface, Kind, RequestPayload, SurfacePayload,
            };
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64
                    / 1_000_000,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                kind: Kind::DownstreamRequest,
                payload: SurfacePayload::Request(
                    RequestPayload {
                        method: meta.method,
                        url: meta.url,
                        http_version: meta.http_version,
                        headers: meta.headers,
                        body_b64: String::new(), // filled by flusher
                    },
                    body,
                ),
            });
        }
        Ok(short_circuit)
    }
```

- [ ] **Step 3: Verify compile**

```bash
cargo build -p gw-proxy
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/filters/request.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(proxy): capture downstream_request body+meta after request_filter body read"
```

---

### Task 13: Capture `upstream_request` in `upstream::rewrite_request`

**Files:**
- Modify: `crates/gw-proxy/src/filters/upstream.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`

- [ ] **Step 1: Snapshot at the end of `rewrite_request`**

Find `crates/gw-proxy/src/filters/upstream.rs:509`:

```rust
    ctx.cached_body = Some(outbound.clone());
    ctx.upstream_codec = Some(upstream_codec);
```

Immediately after `ctx.upstream_codec = Some(upstream_codec);`, insert (use the `upstream_request: &mut RequestHeader` parameter that's already in scope; check the function signature ~line 440 for the exact name):

```rust
    // Archive: snapshot the upstream-bound request now that `outbound` is the
    // wire form. `upstream_request` headers have already been mutated by this
    // point (Authorization injected etc.) so the archive sees what vendor sees.
    ctx.upstream_request_body = Some(std::sync::Arc::new(outbound.clone()));
    {
        use crate::ctx::UpstreamRequestMeta;
        ctx.upstream_request_meta = Some(UpstreamRequestMeta {
            method: upstream_request.method.as_str().to_owned(),
            url: upstream_request.uri.to_string(),
            http_version: format!("{:?}", upstream_request.version),
            headers: upstream_request
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect(),
        });
    }
```

Verify the exact name of the `&mut RequestHeader` parameter by reading the function signature at the top of `rewrite_request`.

- [ ] **Step 2: Emit the surface from `upstream_request_filter`**

In `crates/gw-proxy/src/gateway.rs`, find `async fn upstream_request_filter(...)` (line ~213). After the `filters::upstream::rewrite_request(...)` call:

```rust
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        filters::upstream::rewrite_request(session, upstream_request, ctx).await?;
        if let (Some(body), Some(meta)) = (
            ctx.upstream_request_body.clone(),
            ctx.upstream_request_meta.clone(),
        ) {
            use gw_ledger::detailed_archive::{
                CapturedSurface, Kind, RequestPayload, SurfacePayload,
            };
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                kind: Kind::UpstreamRequest,
                payload: SurfacePayload::Request(
                    RequestPayload {
                        method: meta.method,
                        url: meta.url,
                        http_version: meta.http_version,
                        headers: meta.headers,
                        body_b64: String::new(),
                    },
                    body,
                ),
            });
        }
        Ok(())
    }
```

- [ ] **Step 3: Verify compile**

```bash
cargo build -p gw-proxy
```

Expected: clean. If `time::OffsetDateTime` is not in scope, add `use time::OffsetDateTime;` (gw-proxy already depends on `time` via gw-core).

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/filters/upstream.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(proxy): capture upstream_request body+meta in upstream_request_filter"
```

---

### Task 14: Snapshot upstream response headers in `response_filter`

**Files:**
- Modify: `crates/gw-proxy/src/gateway.rs`

- [ ] **Step 1: Extend `response_filter`**

Find `async fn response_filter(...)` (line ~256). Replace:

```rust
    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        ctx.upstream_status = Some(upstream_response.status.as_u16());
        Ok(())
    }
```

with:

```rust
    async fn response_filter(
        &self,
        _session: &mut Session,
        upstream_response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        ctx.upstream_status = Some(upstream_response.status.as_u16());
        // Archive: snapshot the upstream response headers BEFORE any subsequent
        // mutation. The downstream snapshot is taken in response_body_filter at
        // EOS (after codec header rewrites).
        use crate::ctx::ResponseMeta;
        ctx.archive_upstream_response_meta = Some(ResponseMeta {
            status: upstream_response.status.as_u16(),
            headers: upstream_response
                .headers
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
                .collect(),
        });
        Ok(())
    }
```

- [ ] **Step 2: Verify compile**

```bash
cargo build -p gw-proxy
```

Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/gw-proxy/src/gateway.rs
git commit -m "feat(proxy): snapshot upstream response headers in response_filter"
```

---

### Task 15: Accumulate upstream + downstream response bodies in `response_body_filter`,emit on EOS

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs`
- Modify: `crates/gw-proxy/src/gateway.rs`

- [ ] **Step 1: In `filters::response::run`, always accumulate raw upstream chunks**

Open `crates/gw-proxy/src/filters/response.rs`. After the existing chunk-tracking line near the top:

```rust
    if let Some(b) = body.as_ref() {
        ctx.bytes_sent_to_client
            .fetch_add(b.len() as u64, Ordering::Relaxed);
    }
```

Add **immediately after**:

```rust
    // Archive: capture raw upstream chunk before any codec rewrite. Empty
    // EOS chunks are fine; the accumulator simply doesn't grow.
    if let Some(b) = body.as_ref() {
        ctx.archive_upstream_response_body.extend_from_slice(b);
    }
```

- [ ] **Step 2: Capture the downstream-bound bytes after codec mutation**

The function ends by either:
- Leaving `*body` set to the upstream bytes (same-protocol passthrough), OR
- Substituting `*body = Some(ctx.pending_client_body.take().unwrap().freeze())` (cross-protocol render).

Find the very end of the function (right before each `return Ok(...)` and before the implicit final `Ok(None)`). The simplest hook: capture `body.as_ref()` **after** all in-function mutation but **before** the return. We add a helper closure or just inline the same call site at each exit point.

Pingora gives us a single body slot, so a cleaner approach is: at the **bottom** of the function (after the `match` / cross-protocol branch is done but before return), insert:

```rust
    if let Some(b) = body.as_ref() {
        ctx.archive_downstream_response_body.extend_from_slice(b);
    }
```

> **Implementation note:** if `response.rs::run` has multiple early returns (e.g. for non-stream EOS), you may need to duplicate this capture at each exit point — or refactor into a `let result = (|| { ... })();` wrapper and capture once before returning `result`. Pick whichever produces fewer touchpoints; in this codebase the function is short enough that 2-3 capture sites is acceptable.

- [ ] **Step 3: Emit `upstream_response` + `downstream_response` on EOS**

In `crates/gw-proxy/src/gateway.rs`, find `fn response_body_filter(...)` (line ~266). Replace:

```rust
    fn response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        filters::response::run(session, body, end_of_stream, ctx)
    }
```

with:

```rust
    fn response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<Option<Duration>> {
        let res = filters::response::run(session, body, end_of_stream, ctx);
        if end_of_stream {
            self.emit_response_surfaces(ctx);
        }
        res
    }
```

Then in the `impl LlmGateway { ... }` (the inherent-impl block, **not** the `impl ProxyHttp`), add:

```rust
    fn emit_response_surfaces(&self, ctx: &mut ReqCtx) {
        use crate::ctx::ResponseMeta;
        use gw_ledger::detailed_archive::{
            CapturedSurface, Kind, ResponsePayload, SurfacePayload,
        };
        use std::sync::Arc;
        let now_ms = time::OffsetDateTime::now_utc().unix_timestamp_nanos() as i64 / 1_000_000;

        // Upstream response — pair with the headers snapshot taken in response_filter.
        if let Some(meta) = ctx.archive_upstream_response_meta.take() {
            let body = Arc::new(bytes::Bytes::from(
                std::mem::take(&mut ctx.archive_upstream_response_body),
            ));
            self.detailed_archive.try_send(CapturedSurface {
                ts_ms: now_ms,
                request_id: ctx.request_id.clone(),
                trace_id: ctx.trace_id.clone(),
                kind: Kind::UpstreamResponse,
                payload: SurfacePayload::Response(
                    ResponsePayload {
                        status: meta.status,
                        headers: meta.headers,
                        body_b64: String::new(),
                    },
                    body,
                ),
            });
        }
        // Downstream response — headers come from `session` would require borrowing
        // it again here; instead we reconstruct from upstream meta as a fallback,
        // OR we capture downstream headers at response_filter time too. v1 uses
        // the upstream-side header set (after response_filter runs they have not
        // been further mutated for same-protocol; for cross-protocol, codec
        // response-header rewrite is wired into response_filter — TODO if added later).
        let downstream_meta = ctx.archive_downstream_response_meta.take().unwrap_or_else(|| {
            ResponseMeta {
                status: ctx.upstream_status.unwrap_or(0),
                headers: Vec::new(),
            }
        });
        let body = Arc::new(bytes::Bytes::from(
            std::mem::take(&mut ctx.archive_downstream_response_body),
        ));
        self.detailed_archive.try_send(CapturedSurface {
            ts_ms: now_ms,
            request_id: ctx.request_id.clone(),
            trace_id: ctx.trace_id.clone(),
            kind: Kind::DownstreamResponse,
            payload: SurfacePayload::Response(
                ResponsePayload {
                    status: downstream_meta.status,
                    headers: downstream_meta.headers,
                    body_b64: String::new(),
                },
                body,
            ),
        });
    }
```

> **Implementation note:** For v1 the downstream header set is approximated from the upstream snapshot. If the codec ever mutates downstream headers (e.g. `content-type` rewrites on cross-protocol re-render), capture a separate `ResponseMeta` for downstream at the end of `response_filter` or via a new Pingora hook. Tracking as a follow-up; for v1 the body-level fidelity is the headline guarantee.

- [ ] **Step 4: Verify compile**

```bash
cargo build -p gw-proxy
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-proxy/src/filters/response.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(proxy): accumulate + emit upstream_response / downstream_response on EOS"
```

---

### Task 16: Rewrite `summary` emission in `filters::logging`

**Files:**
- Modify: `crates/gw-proxy/src/filters/logging.rs`
- Modify: `crates/gw-proxy/src/detailed_log.rs`

- [ ] **Step 1: Strip preview fields and helper from `detailed_log.rs`**

Open `crates/gw-proxy/src/detailed_log.rs`. Delete:

- The two `request_body_preview` / `response_body_preview` struct fields (lines 28-30)
- `pub const PREVIEW_MAX_BYTES: usize = 4 * 1024;`
- `pub fn redacted_preview(...) -> Option<String> { ... }` (entire body)

After deletion the file should look like:

```rust
//! Detailed per-request log record (summary kind for the archive stream).
//! Body previews moved to the `usage:detailed` archive (one entry per surface).

use gw_core::ids::{ApiKeyId, ModelName, RequestId, TraceId, UserId};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct DetailedLogRecord {
    pub ts_ms: u64,
    pub trace_id: TraceId,
    pub request_id: RequestId,
    pub user_id: Option<UserId>,
    pub api_key_id: Option<ApiKeyId>,
    pub model: Option<ModelName>,
    pub client_protocol: Option<String>,
    pub upstream_protocol: Option<String>,
    pub http_status: u16,
    pub error_code: Option<String>,
    pub is_stream: bool,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub latency_ms: u32,
    pub ttft_ms: Option<u32>,
    pub tunnel_resolution: Option<String>,
}
```

Delete the `use gw_core::traits::Redactor;` import too — it's no longer needed.

- [ ] **Step 2: Rewrite the `logging.rs` emission block**

Open `crates/gw-proxy/src/filters/logging.rs`. Find the block starting at line ~277 (`// DetailedLogRecord — emitted via tracing for v1 ...`) and ending at line ~332 (after the `tracing::info!(target: "gw_detailed_log", ...)` call).

Replace the entire block with:

```rust
        // -----------------------------------------------------------------
        // Detailed archive — summary surface. Body surfaces are emitted at
        // their respective trigger points (request / upstream / response).
        // -----------------------------------------------------------------
        use gw_ledger::detailed_archive::{
            CapturedSurface, Kind, SummaryPayload, SurfacePayload,
        };
        let summary = SummaryPayload {
            user_id: Some(effective.clone()),
            api_key_id: ctx.api_key_id.clone(),
            model: Some(model),
            client_protocol: ctx
                .client_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned()),
            upstream_protocol: ctx
                .upstream_codec
                .as_ref()
                .map(|c| c.kind().as_str().to_owned()),
            http_status: ctx.upstream_status.unwrap_or(0),
            error_code: err.map(|e| format!("{:?}", e.etype)),
            is_stream: ctx.is_stream,
            input_tokens: ctx.usage.input_tokens,
            output_tokens: ctx.usage.output_tokens,
            latency_ms,
            ttft_ms,
            tunnel_resolution: ctx.tunnel_resolution.clone(),
        };
        detailed_archive.try_send(CapturedSurface {
            ts_ms: now_ms as i64,
            request_id: ctx.request_id.clone(),
            trace_id: ctx.trace_id.clone(),
            kind: Kind::Summary,
            payload: SurfacePayload::Summary(summary),
        });
```

Then delete the `use crate::detailed_log::{redacted_preview, DetailedLogRecord};` import at the top (line ~5).

The `detailed_archive` binding must come from the `logging` filter's caller. Two options:

**(a) Thread it as a function argument.** Find the signature of the `logging` filter function (top of `logging.rs`) and add `detailed_archive: &gw_ledger::detailed_archive::DetailedArchive,` to it. Then in `gateway.rs::logging`, pass `&self.detailed_archive`.

**(b) Keep it on `&self` via the gateway.** The current Pingora trait method `logging(&self, session, e, ctx)` is on `LlmGateway`, so `self.detailed_archive` is reachable from the gateway side. The simplest refactor: move the "build summary + emit" code from `filters::logging::run` into a new method `LlmGateway::emit_summary_surface(&self, ctx, err, model, effective, now_ms, latency_ms, ttft_ms)` and call it from inside `logging` after `filters::logging::run` returns.

Pick (b) — it keeps the filter pure and matches how Tasks 12-15 wired the gateway methods. Lift the `summary = SummaryPayload { ... }` block out of `filters::logging::run` and into the new method on `LlmGateway`. The filter `run` returns `(model, effective, now_ms, latency_ms, ttft_ms, err)` as a tuple (or via `ctx`); the gateway then builds + emits.

- [ ] **Step 3: Verify compile**

```bash
cargo build -p gw-proxy
```

Iterate until clean.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-proxy/src/filters/logging.rs crates/gw-proxy/src/detailed_log.rs crates/gw-proxy/src/gateway.rs
git commit -m "feat(proxy): emit summary surface via DetailedArchive; drop body previews"
```

---

## Phase 5 — Cleanup: remove legacy redaction + preview + sink

### Task 17: Delete `gw-ledger/src/log_sink.rs`

**Files:**
- Delete: `crates/gw-ledger/src/log_sink.rs`
- Delete: `crates/gw-ledger/tests/redis_sink.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: Delete the files**

```bash
git rm crates/gw-ledger/src/log_sink.rs crates/gw-ledger/tests/redis_sink.rs
```

- [ ] **Step 2: Remove the module declaration**

Open `crates/gw-ledger/src/lib.rs`. Delete the line:

```rust
pub mod log_sink;
```

Also delete any re-exports of `DetailedLogSink` / `RedisStreamSink` / `StdoutSink` if present.

- [ ] **Step 3: Verify the workspace still compiles**

```bash
cargo build --workspace
```

If references to `gw_ledger::log_sink::*` exist anywhere else (`gw-proxy::error::listeners::ledger`, etc.), delete them or convert them to no-ops in the same commit.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-ledger/src/lib.rs
git commit -m "refactor(ledger): remove log_sink module (replaced by detailed_archive)"
```

---

### Task 18: Delete `gw-ledger/src/detailed_stream.rs`

**Files:**
- Delete: `crates/gw-ledger/src/detailed_stream.rs`
- Modify: `crates/gw-ledger/src/lib.rs`

- [ ] **Step 1: Delete the file**

```bash
git rm crates/gw-ledger/src/detailed_stream.rs
```

- [ ] **Step 2: Remove the module declaration**

In `crates/gw-ledger/src/lib.rs`, delete:

```rust
pub mod detailed_stream;
```

- [ ] **Step 3: Search for stray references and clean them up**

```bash
grep -rn "detailed_stream\|MAX_DETAILED_LOG_BYTES\|LogVerbosity\|redact_body\|redact_headers\|redact_message_content\|REDACT_HEADER_DENYLIST\|REDACT_HEADER_ALLOWLIST" crates/ docs/
```

For each remaining hit:
- In `crates/`: remove the reference. If it's in a test, delete the test.
- In `docs/`: leave it for the doc-sync task (Task 22).

- [ ] **Step 4: Verify the workspace compiles**

```bash
cargo build --workspace
cargo test --workspace --no-run
```

Expected: both clean.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(ledger): remove detailed_stream module (redaction + truncation gone)"
```

---

### Task 19: Update metrics catalog

**Files:**
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`
- Modify: `crates/gw-gateway/tests/metrics_catalog.rs`

- [ ] **Step 1: Open `metrics_catalog.rs`**

Read the file. Find the entry:

```rust
        name: "gw_detailed_log_oversized_total",
        ...
```

Delete that entry.

Add new entries (mirror the existing entry shape — same fields, just different `name` / `description`):

```rust
        // Detailed archive — drop counters
        MetricSpec {
            name: "gw_detailed_log_drop_total",
            kind: MetricKind::Counter,
            description: "Detailed-archive entries dropped (labels: reason, kind).",
        },
        // Detailed archive — throughput + payload sizes
        MetricSpec {
            name: "gw_detailed_log_emit_total",
            kind: MetricKind::Counter,
            description: "Detailed-archive entries emitted (labels: kind, encoding).",
        },
        MetricSpec {
            name: "gw_detailed_log_payload_bytes",
            kind: MetricKind::Histogram,
            description: "Compressed payload size per detailed-archive entry (labels: kind, encoding).",
        },
        MetricSpec {
            name: "gw_detailed_log_pre_compress_bytes",
            kind: MetricKind::Histogram,
            description: "Uncompressed JSON size per detailed-archive entry (labels: kind).",
        },
        MetricSpec {
            name: "gw_detailed_log_compress_seconds",
            kind: MetricKind::Histogram,
            description: "Compression duration per detailed-archive entry (labels: encoding).",
        },
        MetricSpec {
            name: "gw_detailed_log_channel_depth",
            kind: MetricKind::Gauge,
            description: "Detailed-archive mpsc channel depth.",
        },
```

Use the actual `MetricSpec` / `MetricKind` types defined in this file. Match field names exactly.

- [ ] **Step 2: Update the catalog test**

Open `crates/gw-gateway/tests/metrics_catalog.rs`. The test that asserts `gw_detailed_log_oversized_total` is present will fail — change it to assert the new names. Replace the literal:

```rust
        "gw_detailed_log_oversized_total",
```

with the new metric names. Keep alphabetical order if the test relies on it (read the test for context).

- [ ] **Step 3: Run the test**

```bash
cargo test -p gw-gateway --test metrics_catalog
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-gateway/src/metrics_catalog.rs crates/gw-gateway/tests/metrics_catalog.rs
git commit -m "feat(metrics): update catalog for detailed archive (drop_total + emit/payload/compress)"
```

---

### Task 20: Add invariant lints to `scripts/check-invariants.sh`

**Files:**
- Modify: `scripts/check-invariants.sh`

- [ ] **Step 1: Append the two new lint blocks**

Open `scripts/check-invariants.sh`. At the bottom of the file (before the final `exit "$fail"` line), append:

```bash
# ---------------------------------------------------------------------------
# no_detailed_redaction — detailed archive must not reintroduce redaction.
# Spec: docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md §10.2
# ---------------------------------------------------------------------------
echo "[no_detailed_redaction] redaction helpers must not appear in gw-ledger/gw-proxy"
hits=$(grep -RnE 'redact_body|redact_headers|redact_message_content|redacted_preview|REDACT_HEADER_DENYLIST|REDACT_HEADER_ALLOWLIST' \
    crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v '^Binary file' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  bad "detailed archive redaction helpers reintroduced"
else
  ok "no detailed-archive redaction helpers in gw-ledger/gw-proxy"
fi

# ---------------------------------------------------------------------------
# no_detailed_truncation — body preview / hard cap constants must not return.
# Spec: docs/superpowers/specs/2026-05-23-detailed-log-complete-design.md §10.2
# ---------------------------------------------------------------------------
echo "[no_detailed_truncation] truncation constants must not appear in gw-ledger/gw-proxy"
hits=$(grep -RnE 'PREVIEW_MAX_BYTES|MAX_DETAILED_LOG_BYTES' \
    crates/gw-ledger/src crates/gw-proxy/src 2>/dev/null | grep -v '^Binary file' || true)
if [ -n "$hits" ]; then
  echo "$hits"
  bad "detailed archive truncation constants reintroduced"
else
  ok "no detailed-archive truncation constants in gw-ledger/gw-proxy"
fi
```

- [ ] **Step 2: Run the script**

```bash
bash scripts/check-invariants.sh
```

Expected: green. If a stray reference remains (something we missed in Task 18), the lint will name the file and line — fix it and re-run.

- [ ] **Step 3: Commit**

```bash
git add scripts/check-invariants.sh
git commit -m "feat(lint): add no_detailed_redaction + no_detailed_truncation invariants"
```

---

## Phase 6 — Doc sync

### Task 21: Sync `CLAUDE.md` Observability bullet

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Find and rewrite the bullet**

In `CLAUDE.md` (project instructions),find the line:

```
- **Observability**: `usage:records` (billing), `usage:detailed` (verbose, three-level redaction, XADD MAXLEN), Prometheus `/metrics`, `/livez` + `/readyz`, admin `/healthz/*` (token-gated).
```

Replace with:

```
- **Observability**: `usage:records` (billing), `usage:detailed` (wire-fidelity archive — 5 per-surface XADD per request, tiered compression raw/lz4/zstd, no redaction, drop-on-outage; Go drainer mandatory), Prometheus `/metrics`, `/livez` + `/readyz`, admin `/healthz/*` (token-gated).
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): observability bullet — detailed log is now wire-fidelity archive"
```

---

### Task 22: Sync `docs/architecture.md` (§3 / §4.1 / §7)

**Files:**
- Modify: `docs/architecture.md`

- [ ] **Step 1: Edit §3 (Redactor)**

Find the `Redactor` trait section in `docs/architecture.md` §3. If the trait still serves other call sites (e.g. `usage:records` ext fields), leave the section but add a note:

> The `Redactor` trait is **not** used for the detailed archive (`usage:detailed`) — that stream is wire-fidelity per the [`detailed-log-complete-design`](superpowers/specs/2026-05-23-detailed-log-complete-design.md). It may still be used by other surfaces.

If the trait has no remaining callers, delete the section entirely. Search for usages first:

```bash
grep -rn "Redactor\|: dyn Redactor" crates/ --include='*.rs'
```

- [ ] **Step 2: Edit §4.1 (persistence table)**

Find the row for `usage:detailed`. Replace its description column with:

```
Wire-fidelity per-surface archive. 5 entries per request (downstream/upstream × request/response + summary). `encoding` ∈ {raw,lz4,zstd}+json picked by size. Headers + bodies stored raw (no redaction). XADD MAXLEN bounds Redis memory; Go drainer is mandatory consumer for 180d retention. Drop-on-outage (no DLQ). See [`detailed-log-complete-design`](superpowers/specs/2026-05-23-detailed-log-complete-design.md).
```

- [ ] **Step 3: Edit §7 (invariants)**

In the §7 invariants table / list, add two rows:

```
| no_detailed_redaction | scripts/check-invariants.sh | redact_*/REDACT_HEADER_* names forbidden in gw-ledger/gw-proxy |
| no_detailed_truncation | scripts/check-invariants.sh | PREVIEW_MAX_BYTES / MAX_DETAILED_LOG_BYTES forbidden in gw-ledger/gw-proxy |
```

(Adjust column shape to match the table that's already there.)

- [ ] **Step 4: Commit**

```bash
git add docs/architecture.md
git commit -m "docs(architecture): sync §3/§4.1/§7 for detailed archive rewrite"
```

---

### Task 23: Sync `docs/workflow.md` DetailedLog section

**Files:**
- Modify: `docs/workflow.md`

- [ ] **Step 1: Find the DetailedLog item in the filter chain**

Search for "DetailedLog" in `docs/workflow.md`. The current bullet says (around line 165):

```
5. **DetailedLog** — `crates/gw-ledger::log_sink::RedisStreamSink` XADDs to `usage:detailed` with `MAXLEN` trim. Body preview inclusion is gated by `ctx.debug` flags and the config sampling rate. Three-level `LogVerbosity` redaction sits in `crates/gw-ledger/src/detailed_stream.rs`.
```

Replace with:

```
5. **DetailedArchive** — `crates/gw-ledger::detailed_archive::DetailedArchive` flusher pool consumes a bounded mpsc; each filter that completes a surface (downstream request body read / upstream rewrite / upstream response EOS / downstream response EOS / `logging`) calls `archive.try_send(CapturedSurface{...})` with the raw bytes. Workers pick an encoding (raw / lz4 / zstd) by uncompressed payload size and XADD to `usage:detailed`. No body redaction, no header denylist, no preview truncation, no sampling — wire-fidelity per [`detailed-log-complete-design`](superpowers/specs/2026-05-23-detailed-log-complete-design.md). Drop-on-outage (channel full / oversize / serialize fail / XADD fail) only emit metrics; the request path is never blocked. Go drainer is the long-term archive sink (180 days).
```

- [ ] **Step 2: Commit**

```bash
git add docs/workflow.md
git commit -m "docs(workflow): rewrite DetailedLog filter section as DetailedArchive (per-surface)"
```

---

### Task 24: Sync `docs/go-contracts/parts/02-go-as-consumer.md` §2.3

**Files:**
- Modify: `docs/go-contracts/parts/02-go-as-consumer.md`

- [ ] **Step 1: Replace §2.3 in full**

Find `## 2.3 \`usage:detailed\` Stream（可选）` and the body that follows it (down to `## 2.4` heading). Replace the entire §2.3 block with the content from spec §6 (Go 协同) — paste the schema table,encoding decode rules,index-by-(request_id,kind) semantics,monitoring metrics,strength guarantees,single-direction note. Use the exact wording in the spec (copy verbatim with §-number cross-references updated).

The new heading should be:

```
## 2.3 `usage:detailed` Stream（必需 consumer / 完整归档）
```

- [ ] **Step 2: Commit**

```bash
git add docs/go-contracts/parts/02-go-as-consumer.md
git commit -m "docs(go-contracts): rewrite parts/02 §2.3 for mandatory wire-fidelity archive"
```

---

### Task 25: Sync `docs/go-contracts/main.md` §3.3 table

**Files:**
- Modify: `docs/go-contracts/main.md`

- [ ] **Step 1: Edit the `usage:detailed` row in the §3.3 stream table**

The current row reads (around line 74):

```
| `usage:detailed` | Rust DetailedLogSink | Go 可选 consumer | 由 Go 自决 |
```

Replace with:

```
| `usage:detailed` | Rust DetailedArchive flusher pool | Go drainer（**必需**） | `go-archive-drainer`（推荐） |
```

- [ ] **Step 2: Update §1 mermaid-style diagram if it references the stream**

Search the file for the diagram listing `usage:detailed` and update the note from "可选" to "必需 + 180d archive". The current diagram has `usage:detailed（可选）` (line ~26); change to `usage:detailed（必需,180d 归档）`.

- [ ] **Step 3: Commit**

```bash
git add docs/go-contracts/main.md
git commit -m "docs(go-contracts): main.md §3.3 — usage:detailed is now mandatory consumer"
```

---

## Phase 7 — Integration test + verification

### Task 26: Redis-backed integration test (`#[ignore]`)

**Files:**
- Create: `crates/gw-ledger/tests/detailed_archive_redis.rs`

- [ ] **Step 1: Write the test**

```rust
//! End-to-end: emit one `CapturedSurface` per kind, verify Redis Stream
//! receives 5 entries with the right encoding + decodable payload.
//! Requires Docker / a reachable Redis. Run via `cargo test -- --ignored`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use bytes::Bytes;
use fred::clients::RedisPool;
use fred::interfaces::{ClientLike, StreamsInterface};
use fred::types::{RedisConfig, ServerConfig};
use gw_core::ids::{RequestId, TraceId};
use gw_ledger::detailed_archive::{
    CapturedSurface, DetailedArchive, DetailedArchiveCfg, Kind, RequestPayload, ResponsePayload,
    SummaryPayload, SurfacePayload,
};
use gw_ledger::encoding::{decode, CompressionCfg, Encoding, TierThresholds};
use std::sync::Arc;
use std::time::Duration;

async fn make_pool() -> RedisPool {
    let url = std::env::var("GW_TEST_REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let mut cfg = RedisConfig::from_url(&url).expect("parse redis url");
    cfg.server = ServerConfig::Centralized {
        server: cfg.server.into_centralized().expect("centralized"),
    };
    let pool = RedisPool::new(cfg, None, None, None, 1).expect("RedisPool");
    pool.connect();
    pool.wait_for_connect().await.expect("connect");
    pool
}

fn cfg(stream: &str) -> DetailedArchiveCfg {
    DetailedArchiveCfg {
        stream: stream.to_string(),
        maxlen: 100_000,
        channel_capacity: 1024,
        workers: 2,
        single_payload_soft_cap_bytes: 8 * 1024 * 1024,
        compression: CompressionCfg {
            thresholds: TierThresholds { small_threshold_bytes: 1024, medium_threshold_bytes: 65_536 },
            zstd_level: 3,
        },
    }
}

#[tokio::test]
#[ignore = "needs Redis"]
async fn five_surfaces_round_trip_through_redis() {
    let pool = make_pool().await;
    let stream = format!("usage:detailed:test:{}", uuid::Uuid::new_v4().simple());
    let archive = DetailedArchive::start(pool.clone(), cfg(&stream));

    let req_id = RequestId::from("r_e2e".to_string());
    let trace = TraceId::from("t_e2e".to_string());

    // 1. downstream_request — small, expect raw+json
    archive.try_send(CapturedSurface {
        ts_ms: 1, request_id: req_id.clone(), trace_id: trace.clone(),
        kind: Kind::DownstreamRequest,
        payload: SurfacePayload::Request(
            RequestPayload {
                method: "POST".into(), url: "/v1/chat".into(),
                http_version: "HTTP/1.1".into(),
                headers: vec![("authorization".into(), "Bearer x".into())],
                body_b64: String::new(),
            },
            Arc::new(Bytes::from_static(b"{\"model\":\"x\"}")),
        ),
    });
    // 2. upstream_request — same shape
    archive.try_send(CapturedSurface {
        ts_ms: 2, request_id: req_id.clone(), trace_id: trace.clone(),
        kind: Kind::UpstreamRequest,
        payload: SurfacePayload::Request(
            RequestPayload {
                method: "POST".into(), url: "https://api.openai.com/v1/chat".into(),
                http_version: "HTTP/1.1".into(),
                headers: vec![("authorization".into(), "Bearer redacted".into())],
                body_b64: String::new(),
            },
            Arc::new(Bytes::from_static(b"{\"model\":\"x\"}")),
        ),
    });
    // 3. upstream_response — medium (lz4 tier)
    let medium = "x".repeat(4096);
    archive.try_send(CapturedSurface {
        ts_ms: 3, request_id: req_id.clone(), trace_id: trace.clone(),
        kind: Kind::UpstreamResponse,
        payload: SurfacePayload::Response(
            ResponsePayload { status: 200, headers: vec![], body_b64: String::new() },
            Arc::new(Bytes::from(medium.into_bytes())),
        ),
    });
    // 4. downstream_response — large (zstd tier)
    let large = "y".repeat(100_000);
    archive.try_send(CapturedSurface {
        ts_ms: 4, request_id: req_id.clone(), trace_id: trace.clone(),
        kind: Kind::DownstreamResponse,
        payload: SurfacePayload::Response(
            ResponsePayload { status: 200, headers: vec![], body_b64: String::new() },
            Arc::new(Bytes::from(large.into_bytes())),
        ),
    });
    // 5. summary
    archive.try_send(CapturedSurface {
        ts_ms: 5, request_id: req_id.clone(), trace_id: trace.clone(),
        kind: Kind::Summary,
        payload: SurfacePayload::Summary(SummaryPayload {
            user_id: None, api_key_id: None, model: None,
            client_protocol: Some("openai_chat".into()),
            upstream_protocol: Some("openai_chat".into()),
            http_status: 200, error_code: None, is_stream: false,
            input_tokens: 10, output_tokens: 20, latency_ms: 100, ttft_ms: Some(30),
            tunnel_resolution: Some("direct".into()),
        }),
    });

    // Wait for workers to drain
    tokio::time::sleep(Duration::from_millis(300)).await;

    let client = pool.next();
    let len: u64 = client.xlen(&stream).await.expect("xlen");
    assert_eq!(len, 5, "expected 5 entries, got {len}");

    let entries: Vec<(String, std::collections::HashMap<String, Vec<u8>>)> =
        client.xrange(&stream, "-", "+", None).await.expect("xrange");
    assert_eq!(entries.len(), 5);

    let mut seen_kinds = std::collections::HashSet::new();
    for (_id, fields) in &entries {
        let kind = std::str::from_utf8(fields.get("kind").expect("kind")).unwrap().to_string();
        let encoding = std::str::from_utf8(fields.get("encoding").expect("encoding")).unwrap().to_string();
        let payload = fields.get("payload").expect("payload").clone();
        let enc = match encoding.as_str() {
            "raw+json" => Encoding::Raw,
            "lz4+json" => Encoding::Lz4,
            "zstd+json" => Encoding::Zstd,
            other => panic!("unknown encoding {other}"),
        };
        let decoded = decode(enc, &payload).expect("decode");
        let json: serde_json::Value = serde_json::from_slice(&decoded).expect("parse");
        // Tier sanity: large response landed in zstd.
        if kind == "downstream_response" {
            assert_eq!(enc, Encoding::Zstd, "large body should land in zstd tier");
            let body_b64 = json.get("body_b64").and_then(|v| v.as_str()).unwrap();
            let decoded_body = B64.decode(body_b64).expect("b64");
            assert_eq!(decoded_body.len(), 100_000);
        }
        seen_kinds.insert(kind);
    }
    let all = ["downstream_request","upstream_request","upstream_response","downstream_response","summary"]
        .iter().map(|s| s.to_string()).collect::<std::collections::HashSet<_>>();
    assert_eq!(seen_kinds, all);

    // Cleanup
    let _: i64 = client.del(&stream).await.unwrap_or(0);
}
```

If `uuid::Uuid` is not in workspace deps, replace the stream name with `format!("usage:detailed:test:{}", std::process::id())` — uniqueness across test runs in the same process matters less than across CI shards. Add `uuid = { workspace = true, features = ["v4"] }` to `crates/gw-ledger/Cargo.toml`'s `[dev-dependencies]` if cleaner.

- [ ] **Step 2: Run the test (requires Docker / Redis)**

```bash
docker run -d --rm -p 6379:6379 --name gw-redis-test redis:7
cargo test -p gw-ledger --test detailed_archive_redis -- --ignored --nocapture
docker stop gw-redis-test
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/gw-ledger/tests/detailed_archive_redis.rs crates/gw-ledger/Cargo.toml
git commit -m "test(ledger): integration — 5 surfaces round-trip through Redis"
```

---

### Task 27: Full verification — clippy / fmt / test / invariants / smoke

**Files:** none (verification only)

- [ ] **Step 1: Format check**

```bash
cargo fmt --all -- --check
```

If anything fails, run `cargo fmt --all` then re-commit the fmt-only changes.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: clean. Fix any new lints.

- [ ] **Step 3: Default test suite**

```bash
cargo test --workspace
```

Expected: green.

- [ ] **Step 4: Invariant lints**

```bash
bash scripts/check-invariants.sh
```

Expected: all green, including the two new lints from Task 20.

- [ ] **Step 5: Smoke run**

Boot the binary against local Postgres + Redis:

```bash
GW_KMS__KEK_HEX=$(python3 -c 'import secrets; print(secrets.token_hex(32))') \
GW_REDIS__URL=redis://localhost:6379 \
GW_PGSQL__URL=postgres://postgres:postgres@localhost:5432/gateway \
  ./target/release/gw-gateway &
GW_PID=$!
sleep 2

# Make a request (adjust URL/headers as the gateway's `e2e` fake-upstream expects)
curl -sS -X POST http://localhost:8000/v1/chat/completions \
  -H 'authorization: Bearer test-key' \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-4o-mini","messages":[{"role":"user","content":"hi"}]}'

# Check Redis got 5 entries
redis-cli XLEN usage:detailed

kill $GW_PID
```

Expected: `XLEN usage:detailed` ≥ 5 after one request (more after additional requests).

- [ ] **Step 6: Sanity-check a decoded entry by hand**

```bash
redis-cli XRANGE usage:detailed - + COUNT 1
```

Look at the printed entry. Confirm `encoding` ∈ {`raw+json`, `lz4+json`, `zstd+json`} and `kind` is one of the 5 known values.

- [ ] **Step 7: Commit any post-verification doc tweaks**

If the smoke run revealed wording drift in `docs/`, fix it in a small follow-up commit. Otherwise skip this step.

---

## Self-review checklist (run after implementation, before marking done)

- [ ] Spec §3.1 schema fields all present in Redis XADD: `ts_ms / request_id / trace_id / kind / encoding / payload` (Task 6 Step 2 / Task 26).
- [ ] All 5 `kind` values reachable from filter chain (Tasks 12 / 13 / 15 / 16). For the happy non-stream path: 5 entries per request.
- [ ] No `redact_*` / `PREVIEW_MAX_BYTES` / `MAX_DETAILED_LOG_BYTES` / `sampling_rate` references remain (verified by Task 20 lints + Task 27 grep).
- [ ] `usage:detailed` MAXLEN is configurable; default 10 M (Task 9).
- [ ] Drop-on-outage paths emit the right `gw_detailed_log_drop_total{reason,kind}` labels (Task 6 handle()).
- [ ] Go contract docs (`parts/02 §2.3`, `main.md §3.3`) updated (Tasks 24 / 25).
- [ ] Spec verification anchors §13 #1-7 each have a matching test or smoke step:
  - #1 完整性: Task 26 (round-trip 5 surfaces of varying size)
  - #2 不脱敏: Task 26 (authorization header round-trips)
  - #3 per-surface: Task 26 (5 entries seen)
  - #4 错误路径 summary: needs an explicit test — **add as follow-up Task 28 if not already covered** by existing error-path tests (`crates/gw-proxy/tests/*`)
  - #5 解耦: enforced by architecture (mpsc), not a unit test. The flusher delay is bounded by mpsc capacity.
  - #6 drop 计量: Task 7 covers try_send drop; metric assertion left as observational
  - #7 分级压缩: Task 26 (large body lands in zstd; small in raw)
