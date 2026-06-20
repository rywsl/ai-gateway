# Hot Path Async Delivery Implementation Plan

> **SUPERSEDED AFTER PARALLEL AUDIT:** Do not implement this file directly.
> Use `docs/superpowers/plans/2026-06-20-hot-path-async-delivery-audited.md`.
> The first draft kept the right direction, but parallel review found must-fix
> gaps in overflow full/closed handling, shutdown drain, archive truncation,
> metrics catalog snippets, and deterministic tests.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the synchronous bridge from response-body processing when archive capture does not use disk, and replace request-tail synchronous Redis billing bypasses with supervised asynchronous must-deliver delivery.

**Architecture:** Pingora 0.8 keeps `response_body_filter` synchronous, so the fast path must be pure synchronous CPU/memory work, not an async function wrapped in `block_in_place`. Response archive disk spill becomes an explicit storage mode; the default hot path is bounded memory-only capture with truncation. Billing delivery keeps the existing Redis batch flusher and DLQ cascade, but adds a primary delivery overflow queue that preserves `LedgerItem.clear` so full-channel delivery never performs synchronous Redis XADD in the request path.

**Tech Stack:** Rust 1.88, Pingora 0.8, Tokio, fred Redis, sqlx, serde JSONL, existing `gw_runtime::SpillRoot`, existing `gw_runtime::Supervisor`.

## Global Constraints

- Do not change external request/response protocol behavior.
- Do not drop billing records because the primary mpsc channel is full.
- Preserve the DEL rule: only delete `billing:inflight:<instance>:<request_id>:<attempt>` after the matching `UsageRecord` is confirmed in `usage:records`.
- Do not use `block_in_place` on the response hot path when `proxy.archive.storage = "memory"`.
- Do not add per-request disk I/O to the default response archive path.
- Keep disk writes only for explicit archive spill mode, billing overload fallback, shutdown drain, or crash recovery files.
- Maintain bounded memory for response archive capture.
- Existing ignored Docker tests remain ignored.

---

## File Structure

- Modify `config/default.toml`
  - Add `[proxy.archive] storage = "memory"` and document the memory/disk tradeoff.

- Modify `crates/gw-gateway/src/config.rs`
  - Add `ProxyArchiveStorage` enum.
  - Parse `proxy.archive.storage`.
  - Validate `soft_cap_bytes <= hard_cap_bytes`.

- Modify `crates/gw-proxy/src/ctx.rs`
  - Add `ArchiveStorage`.
  - Add synchronous memory-only append/read helpers.
  - Keep existing async spill helpers available only for `ArchiveStorage::DiskSpill`.

- Modify `crates/gw-proxy/src/filters/response.rs`
  - Add `run_memory_sync(...)`.
  - Keep async `run(...)` for explicit disk-spill mode.
  - Convert CLI body filtering into sync and async variants only where needed.

- Modify `crates/gw-proxy/src/gateway.rs`
  - Route `response_body_filter` to `run_memory_sync` when archive storage is memory.
  - Keep `block_in_place` only for disk-spill mode and spilled error-passthrough reads.

- Modify `crates/gw-proxy/tests/archive_state.rs`
  - Add memory-only no-spill tests.
  - Add hard-cap-before-spill regression.

- Modify `crates/gw-proxy/tests/cli_filter_chain.rs`
  - Update direct calls to the sync CLI filter.

- Create `crates/gw-ledger/src/primary_delivery.rs`
  - Own the primary overflow queue and worker.
  - Preserve `LedgerItem` including `ClearKey`.
  - Drain overflow items through the same `flush(...)` function used by the main flusher.

- Modify `crates/gw-ledger/src/lib.rs`
  - Export `primary_delivery` only if needed by tests.

- Modify `crates/gw-ledger/src/flusher.rs`
  - Derive serde for `LedgerItem` and `ClearKey`.
  - Add `PrimaryDelivery`.
  - Replace full/closed sync XADD bypass with primary overflow enqueue.

- Modify `crates/gw-ledger/tests/flusher.rs`
  - Replace sync-bypass expectations with async-overflow expectations.

- Modify `crates/gw-ledger/tests/inflight_clear_via_flusher.rs`
  - Replace full/closed sync-bypass tests with overflow-delivery tests.

- Modify `crates/gw-gateway/src/metrics_catalog.rs`
  - Replace sync-bypass metric descriptions with async-overflow metric descriptions.

---

### Task 1: Archive Storage Config And Validation

**Files:**
- Modify: `config/default.toml:276-279`
- Modify: `crates/gw-gateway/src/config.rs:1546-1574`
- Test: `crates/gw-gateway/src/config.rs` unit tests near existing auto-sizing tests

**Interfaces:**
- Produces: `ProxyArchiveStorage`
  - `Memory`
  - `DiskSpill`
- Produces: `ProxyArchiveConfig.storage: ProxyArchiveStorage`
- Later tasks consume `cfg.proxy.archive.storage`.

- [ ] **Step 1: Write the failing config test**

Add this test near the existing default TOML config tests in `crates/gw-gateway/src/config.rs`:

```rust
#[test]
fn proxy_archive_storage_defaults_to_memory() {
    let defaults = include_str!("../../../config/default.toml");
    let mut cfg: Config = Figment::new()
        .merge(Toml::string(defaults))
        .extract()
        .expect("default.toml parses");
    cfg.apply_auto_sizing(MachineSizing {
        effective_cpus: 4,
        effective_memory_bytes: 4 * 1024 * 1024 * 1024,
    });
    cfg.validate().expect("default.toml validates");
    cfg.validate_resolved_sizing()
        .expect("default sizing resolves");

    assert_eq!(cfg.proxy.archive.storage, ProxyArchiveStorage::Memory);
    assert_eq!(cfg.proxy.archive.soft_cap_bytes, 4 * 1024 * 1024);
    assert_eq!(cfg.proxy.archive.hard_cap_bytes, 8 * 1024 * 1024);
}

#[test]
fn proxy_archive_accepts_disk_spill_storage() {
    let defaults = include_str!("../../../config/default.toml");
    let overrides = r#"
        [proxy.archive]
        storage = "disk_spill"
        soft_cap_bytes = 1024
        hard_cap_bytes = 4096
    "#;
    let mut cfg: Config = Figment::new()
        .merge(Toml::string(defaults))
        .merge(Toml::string(overrides))
        .extract()
        .expect("config parses");
    cfg.apply_auto_sizing(MachineSizing {
        effective_cpus: 4,
        effective_memory_bytes: 4 * 1024 * 1024 * 1024,
    });
    cfg.validate().expect("disk_spill archive config validates");
    cfg.validate_resolved_sizing()
        .expect("sizing resolves");

    assert_eq!(cfg.proxy.archive.storage, ProxyArchiveStorage::DiskSpill);
    assert_eq!(cfg.proxy.archive.soft_cap_bytes, 1024);
    assert_eq!(cfg.proxy.archive.hard_cap_bytes, 4096);
}

#[test]
fn proxy_archive_rejects_soft_cap_above_hard_cap() {
    let defaults = include_str!("../../../config/default.toml");
    let overrides = r#"
        [proxy.archive]
        soft_cap_bytes = 8192
        hard_cap_bytes = 4096
    "#;
    let mut cfg: Config = Figment::new()
        .merge(Toml::string(defaults))
        .merge(Toml::string(overrides))
        .extract()
        .expect("config parses");
    cfg.apply_auto_sizing(MachineSizing {
        effective_cpus: 4,
        effective_memory_bytes: 4 * 1024 * 1024 * 1024,
    });
    let err = cfg.validate().unwrap_err();

    assert!(
        err.to_string()
            .contains("proxy.archive.soft_cap_bytes must be <= hard_cap_bytes"),
        "{err:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p gw-gateway proxy_archive_
```

Expected: compile failure because `ProxyArchiveStorage` and `storage` do not exist.

- [ ] **Step 3: Add config enum and defaults**

In `crates/gw-gateway/src/config.rs`, update the archive config block:

```rust
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProxyArchiveStorage {
    Memory,
    DiskSpill,
}

fn default_proxy_archive_storage() -> ProxyArchiveStorage {
    ProxyArchiveStorage::Memory
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyArchiveConfig {
    #[serde(default = "default_proxy_archive_storage")]
    pub storage: ProxyArchiveStorage,
    #[serde(default = "default_proxy_archive_soft_cap_bytes")]
    pub soft_cap_bytes: usize,
    #[serde(default = "default_proxy_archive_hard_cap_bytes")]
    pub hard_cap_bytes: usize,
    #[serde(default = "default_proxy_archive_spill_dir")]
    pub spill_dir: std::path::PathBuf,
}

impl Default for ProxyArchiveConfig {
    fn default() -> Self {
        Self {
            storage: default_proxy_archive_storage(),
            soft_cap_bytes: default_proxy_archive_soft_cap_bytes(),
            hard_cap_bytes: default_proxy_archive_hard_cap_bytes(),
            spill_dir: default_proxy_archive_spill_dir(),
        }
    }
}
```

In `Config::validate`, add this check near the other archive/runtime validation:

```rust
if self.proxy.archive.soft_cap_bytes > self.proxy.archive.hard_cap_bytes {
    anyhow::bail!(
        "invalid config: proxy.archive.soft_cap_bytes must be <= hard_cap_bytes"
    );
}
```

In `config/default.toml`, change the archive block to:

```toml
[proxy.archive]
# memory: response hot path stores bounded archive bytes in memory and truncates
# at hard_cap_bytes; no block_in_place bridge is used for archive capture.
# disk_spill: preserves the legacy spill-to-disk behavior for large bodies and
# uses the synchronous Pingora bridge only when disk I/O is required.
storage = "memory"
soft_cap_bytes = 4194304
hard_cap_bytes = 8388608
spill_dir = "/data/cnexus/spill/archive"
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p gw-gateway proxy_archive_
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add config/default.toml crates/gw-gateway/src/config.rs
git commit -m "config: add response archive storage mode"
```

---

### Task 2: Synchronous Memory Archive API

**Files:**
- Modify: `crates/gw-proxy/src/ctx.rs:33-238`
- Test: `crates/gw-proxy/tests/archive_state.rs`

**Interfaces:**
- Consumes: `ProxyArchiveStorage` from Task 1 conceptually; this crate gets its own runtime enum.
- Produces: `ArchiveStorage`
  - `Memory`
  - `DiskSpill`
- Produces: `ArchiveCaps.storage: ArchiveStorage`
- Produces: `ArchiveState::append_chunk_sync(...)`
- Produces: `ArchiveState::read_for_emit_sync(...)`

- [ ] **Step 1: Write failing archive tests**

Add these tests to `crates/gw-proxy/tests/archive_state.rs`:

```rust
fn memory_caps(soft: usize, hard: usize) -> ArchiveCaps {
    ArchiveCaps {
        storage: gw_proxy::ctx::ArchiveStorage::Memory,
        soft_cap_bytes: soft,
        hard_cap_bytes: hard,
    }
}

#[test]
fn memory_archive_does_not_create_spill_files_above_soft_cap() {
    let tmp = TempDir::new().unwrap();
    let mut a = ArchiveState::default();
    let chunk = vec![7u8; 4096];

    a.append_chunk_sync(&chunk, &memory_caps(1024, 8192), "req-memory", "upstream");

    assert!(matches!(a, ArchiveState::InMemory(_)));
    assert_eq!(a.len(), 4096);
    let remaining: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
    assert!(remaining.is_empty(), "memory storage must not create files");
}

#[test]
fn memory_archive_truncates_at_hard_cap_without_spill() {
    let mut a = ArchiveState::default();
    let first = vec![1u8; 3000];
    let second = vec![2u8; 3000];

    a.append_chunk_sync(&first, &memory_caps(1024, 4096), "req-memory-hard", "upstream");
    a.append_chunk_sync(&second, &memory_caps(1024, 4096), "req-memory-hard", "upstream");

    assert!(a.is_truncated());
    assert_eq!(a.len(), 3000);
    assert_eq!(a.dropped(), 3000);
}

#[tokio::test]
async fn disk_spill_checks_hard_cap_before_first_spill() {
    let tmp = TempDir::new().unwrap();
    let root = Arc::new(SpillRoot::new(SpillRootConfig {
        root_dir: tmp.path().to_path_buf(),
        max_total_bytes: 1 << 30,
        buffer_size: 4096,
    }));
    root.ensure_dir().await.unwrap();

    let mut a = ArchiveState::default();
    let chunk = vec![9u8; 5000];
    let mut disk_caps = caps(2048, 4096);
    disk_caps.storage = gw_proxy::ctx::ArchiveStorage::DiskSpill;

    a.append_chunk(&chunk, &disk_caps, &root, "req-hard-first", "upstream")
        .await;

    assert!(a.is_truncated());
    assert_eq!(a.len(), 0);
    assert_eq!(a.dropped(), 5000);
    let remaining: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
    assert!(remaining.is_empty(), "hard-cap rejection must happen before spill create");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p gw-proxy --test archive_state
```

Expected: compile failure because `ArchiveStorage`, `ArchiveCaps.storage`, and `append_chunk_sync` do not exist.

- [ ] **Step 3: Add sync memory archive API**

In `crates/gw-proxy/src/ctx.rs`, update the archive types:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveStorage {
    Memory,
    DiskSpill,
}

#[derive(Debug, Clone, Copy)]
pub struct ArchiveCaps {
    pub storage: ArchiveStorage,
    pub soft_cap_bytes: usize,
    pub hard_cap_bytes: usize,
}

impl Default for ArchiveCaps {
    fn default() -> Self {
        Self {
            storage: ArchiveStorage::Memory,
            soft_cap_bytes: 4 * 1024 * 1024,
            hard_cap_bytes: 8 * 1024 * 1024,
        }
    }
}
```

Add this method to `impl ArchiveState` before the async `append_chunk`:

```rust
pub fn append_chunk_sync(
    &mut self,
    chunk: &[u8],
    caps: &ArchiveCaps,
    request_id: &str,
    surface: &'static str,
) {
    let new_total = self.len().saturating_add(chunk.len() as u64);
    if new_total > caps.hard_cap_bytes as u64 {
        let written = self.len();
        *self = Self::Truncated {
            written,
            dropped: chunk.len() as u64,
        };
        metrics::counter!("gw_archive_truncated_total").increment(1);
        tracing::debug!(
            target: "gw_proxy::archive",
            request_id,
            surface,
            bytes_written = written,
            bytes_dropped = chunk.len(),
            "archive_memory_hard_cap_truncated"
        );
        return;
    }

    match self {
        Self::InMemory(buf) => {
            buf.extend_from_slice(chunk);
        }
        Self::Spilled { written, .. } => {
            *written = written.saturating_add(chunk.len() as u64);
            metrics::counter!("gw_archive_memory_append_to_spilled_total").increment(1);
        }
        Self::Truncated { dropped, .. } => {
            *dropped = dropped.saturating_add(chunk.len() as u64);
        }
    }
}

pub fn read_for_emit_sync(&self) -> Vec<u8> {
    match self {
        Self::InMemory(b) => b.to_vec(),
        Self::Truncated { .. } => Vec::new(),
        Self::Spilled { .. } => Vec::new(),
    }
}
```

At the start of the existing async `append_chunk`, add a hard-cap check before the `match self`:

```rust
let new_total = self.len().saturating_add(chunk.len() as u64);
if new_total > caps.hard_cap_bytes as u64 {
    let written = self.len();
    *self = Self::Truncated {
        written,
        dropped: chunk.len() as u64,
    };
    metrics::counter!("gw_archive_truncated_total").increment(1);
    return;
}

if matches!(caps.storage, ArchiveStorage::Memory) {
    self.append_chunk_sync(chunk, caps, request_id, surface);
    return;
}
```

Then remove the old duplicate `let new_total = ...` inside `append_chunk`.

- [ ] **Step 4: Update existing test helper**

Update the existing `caps(...)` helper in `crates/gw-proxy/tests/archive_state.rs`:

```rust
fn caps(soft: usize, hard: usize) -> ArchiveCaps {
    ArchiveCaps {
        storage: gw_proxy::ctx::ArchiveStorage::DiskSpill,
        soft_cap_bytes: soft,
        hard_cap_bytes: hard,
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run:

```bash
cargo test -p gw-proxy --test archive_state
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/gw-proxy/src/ctx.rs crates/gw-proxy/tests/archive_state.rs
git commit -m "proxy: add memory-only archive capture"
```

---

### Task 3: No-Bridge Response Hot Path

**Files:**
- Modify: `crates/gw-proxy/src/filters/response.rs:129-451`
- Modify: `crates/gw-proxy/src/gateway.rs:1524-1648`
- Modify: `crates/gw-gateway/src/lib.rs` where `archive_caps` is wired into `Wired`
- Test: `crates/gw-proxy/tests/response_hot_path.rs`
- Test: `crates/gw-proxy/tests/cli_filter_chain.rs`

**Interfaces:**
- Consumes: `ArchiveCaps.storage` and `ArchiveState::append_chunk_sync(...)` from Task 2.
- Produces: `filters::response::run_memory_sync(...)`.
- Produces: `filters::response::run_cli_body_filter_sync(...)`.

- [ ] **Step 1: Write failing static hot-path test**

Create `crates/gw-proxy/tests/response_hot_path.rs`:

```rust
#[test]
fn response_body_filter_has_memory_branch_before_block_in_place() {
    let src = include_str!("../src/gateway.rs");
    let memory_branch = src
        .find("filters::response::run_memory_sync")
        .expect("response_body_filter must call memory sync filter");
    let bridge = src
        .find("tokio::task::block_in_place")
        .expect("disk-spill fallback may still use block_in_place");

    assert!(
        memory_branch < bridge,
        "memory response path must be selected before block_in_place fallback"
    );
}

#[test]
fn response_memory_filter_does_not_await() {
    let src = include_str!("../src/filters/response.rs");
    let start = src
        .find("pub fn run_memory_sync")
        .expect("sync memory filter exists");
    let end = src[start..]
        .find("pub async fn run")
        .map(|n| start + n)
        .unwrap_or(src.len());
    let sync_filter = &src[start..end];

    assert!(
        !sync_filter.contains(".await"),
        "run_memory_sync must remain pure sync code"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p gw-proxy --test response_hot_path
```

Expected: FAIL because `run_memory_sync` does not exist.

- [ ] **Step 3: Add sync response filter entry point**

In `crates/gw-proxy/src/filters/response.rs`, add this sync entry point above the existing async `run`:

```rust
pub fn run_memory_sync(
    _session: &mut Session,
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut ReqCtx,
) -> Result<Option<Duration>> {
    check_deadline(ctx)?;

    if let Some(b) = body.as_ref() {
        ctx.bytes_sent_to_client
            .fetch_add(b.len() as u64, Ordering::Relaxed);
    }

    if matches!(
        ctx.vendor_exposure,
        crate::billing_lifecycle::VendorExposure::Sent
    ) && body.as_ref().map(|b| !b.is_empty()).unwrap_or(false)
    {
        crate::billing_lifecycle::transition_to(
            ctx,
            crate::billing_lifecycle::VendorExposure::Streaming,
        );
    }

    if let Some(b) = body.as_ref() {
        ctx.archive_upstream_response_body.append_chunk_sync(
            b,
            &ctx.archive_caps,
            ctx.request_id.as_ref(),
            "upstream",
        );
    }

    if ctx.client_shape.is_some_and(|shape| shape.is_cli()) {
        run_cli_body_filter_sync(body, end_of_stream, ctx)?;
        return Ok(None);
    }

    if !ctx.is_stream {
        let cross_protocol = is_cross_protocol(ctx);
        handle_non_stream_chunk(body.as_ref(), end_of_stream, ctx);
        if cross_protocol {
            if end_of_stream {
                if let Some(buf) = ctx.pending_client_body.take() {
                    *body = Some(buf.freeze());
                } else if let Some(orig) = ctx.upstream_body_accum.take() {
                    *body = Some(orig.freeze());
                }
            } else {
                *body = None;
            }
        }
        if let Some(b) = body.as_ref() {
            ctx.archive_downstream_response_body.append_chunk_sync(
                b,
                &ctx.archive_caps,
                ctx.request_id.as_ref(),
                "downstream",
            );
        }
        return Ok(None);
    }

    let Some(upstream_kind) = ctx.upstream_codec.as_ref().map(AnyCodec::kind) else {
        return Ok(None);
    };
    let upstream_wire = ctx.upstream_codec.as_ref().map(WireFormat::from_any_codec);
    let client_wire = ctx.client_codec.as_ref().map(WireFormat::from_any_codec);
    let cross_protocol = matches!((client_wire, upstream_wire), (Some(cw), Some(uw)) if cw != uw);
    let client_kind = ctx.client_codec.as_ref().map(AnyCodec::kind);

    if let Some(chunk) = body.as_ref() {
        let _ = crate::bounded::push_bounded_vec(
            &mut ctx.sse_leftover,
            chunk,
            crate::bounded::SSE_LEFTOVER_CAP_BYTES,
            crate::bounded::SSE_LEFTOVER_LABEL,
            gw_core::bounded_buf::OverflowPolicy::Truncate,
        );
    }

    loop {
        let Some((idx, sep_len)) = find_event_boundary(&ctx.sse_leftover) else {
            break;
        };
        let event_bytes: Vec<u8> = ctx.sse_leftover.drain(..idx).collect();
        ctx.sse_leftover.drain(..sep_len);
        let codec = codec_for(upstream_kind);
        handle_event(&codec, &event_bytes, ctx, cross_protocol, client_kind);
    }

    if end_of_stream && !ctx.sse_leftover.is_empty() {
        let tail: Vec<u8> = std::mem::take(&mut ctx.sse_leftover);
        let codec = codec_for(upstream_kind);
        handle_event(&codec, &tail, ctx, cross_protocol, client_kind);
    }

    if let Some(buf) = ctx.pending_client_body.take() {
        *body = Some(buf.freeze());
    }

    if let Some(b) = body.as_ref() {
        ctx.archive_downstream_response_body.append_chunk_sync(
            b,
            &ctx.archive_caps,
            ctx.request_id.as_ref(),
            "downstream",
        );
    }

    Ok(None)
}
```

Add this sync CLI helper next to the existing async `run_cli_body_filter`:

```rust
pub fn run_cli_body_filter_sync(
    body: &mut Option<Bytes>,
    end_of_stream: bool,
    ctx: &mut ReqCtx,
) -> Result<()> {
    if let Some(chunk) = body.as_ref() {
        let shape = ctx
            .client_shape
            .filter(ApiShape::is_cli)
            .ok_or_else(|| pingora::Error::new(pingora::ErrorType::Custom("gw_not_cli_request")))?;
        let passthrough = cli_passthrough_response_chunk(shape, chunk).into_owned();
        if passthrough.as_slice() != chunk.as_ref() {
            *body = Some(Bytes::copy_from_slice(&passthrough));
        }
        if ctx.is_stream {
            scan_cli_stream_chunk(shape, &passthrough, ctx);
        } else {
            let buf = ctx.upstream_body_accum.get_or_insert_with(BytesMut::new);
            let outcome = crate::bounded::push_bounded_bytes(
                buf,
                &passthrough,
                crate::bounded::UPSTREAM_BODY_ACCUM_CAP_BYTES,
                crate::bounded::UPSTREAM_BODY_ACCUM_LABEL,
                gw_core::bounded_buf::OverflowPolicy::Reject,
            );
            if matches!(outcome, crate::bounded::PushOutcome::Rejected { .. }) {
                return Err(pingora::Error::because(
                    pingora::ErrorType::CustomCode("GW_UPSTREAM_BODY_TRUNCATED", 502),
                    "upstream body exceeded bounded_buf cap",
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::OutOfMemory,
                        "upstream_body_accum cap exceeded",
                    )),
                ));
            }
        }
        ctx.archive_downstream_response_body.append_chunk_sync(
            &passthrough,
            &ctx.archive_caps,
            ctx.request_id.as_ref(),
            "cli_downstream",
        );
    }
    if end_of_stream && !ctx.is_stream {
        if let Some(buf) = ctx.upstream_body_accum.take() {
            apply_cli_full_usage(ctx.client_shape.unwrap(), &buf, ctx);
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Route gateway memory mode before bridge**

In `crates/gw-proxy/src/gateway.rs`, replace the current `let res = tokio::task::block_in_place(...)` block with:

```rust
let res = if matches!(ctx.archive_caps.storage, crate::ctx::ArchiveStorage::Memory) {
    filters::response::run_memory_sync(session, body, end_of_stream, ctx)
} else {
    tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(filters::response::run(session, body, end_of_stream, ctx))
    })
};
```

Keep the existing spilled error-passthrough `block_in_place` branch unchanged because it only runs when `ArchiveState::Spilled`.

- [ ] **Step 5: Wire gateway config to runtime archive caps**

In `crates/gw-gateway/src/lib.rs`, inside `bootstrap`, map config storage before `Ok((Wired { ... }))`:

```rust
let archive_storage = match cfg.proxy.archive.storage {
    config::ProxyArchiveStorage::Memory => gw_proxy::ctx::ArchiveStorage::Memory,
    config::ProxyArchiveStorage::DiskSpill => gw_proxy::ctx::ArchiveStorage::DiskSpill,
};
```

Then replace the existing `archive_caps` field in `Wired { ... }` with:

```rust
archive_caps: gw_proxy::ctx::ArchiveCaps {
    storage: archive_storage,
    soft_cap_bytes: cfg.proxy.archive.soft_cap_bytes,
    hard_cap_bytes: cfg.proxy.archive.hard_cap_bytes,
};
```

- [ ] **Step 6: Update CLI filter-chain test call**

In `crates/gw-proxy/tests/cli_filter_chain.rs`, replace:

```rust
response::run_cli_body_filter(&mut body, true, &mut ctx)
    .await
    .expect("cli body filter");
```

with:

```rust
response::run_cli_body_filter_sync(&mut body, true, &mut ctx)
    .expect("cli body filter");
```

- [ ] **Step 7: Run tests to verify they pass**

Run:

```bash
cargo test -p gw-proxy --test response_hot_path
cargo test -p gw-proxy --test cli_filter_chain
cargo test -p gw-proxy --test archive_state
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/gw-proxy/src/filters/response.rs crates/gw-proxy/src/gateway.rs crates/gw-gateway/src/lib.rs crates/gw-proxy/tests/response_hot_path.rs crates/gw-proxy/tests/cli_filter_chain.rs
git commit -m "proxy: bypass async bridge for memory archive responses"
```

---

### Task 4: Primary Delivery Overflow Queue

**Files:**
- Create: `crates/gw-ledger/src/primary_delivery.rs`
- Modify: `crates/gw-ledger/src/lib.rs`
- Modify: `crates/gw-ledger/src/flusher.rs:48-70`
- Test: `crates/gw-ledger/src/primary_delivery.rs`

**Interfaces:**
- Consumes: `LedgerItem`, `ClearKey`, `flush(...)`, `DlqFallback`, `RedisCtx`.
- Produces: `PrimaryDelivery`.
- Produces: `PrimaryDelivery::new(tx)`.
- Produces: `PrimaryDelivery::enqueue(item).await`.
- Produces: `spawn_primary_overflow_worker(...) -> JoinHandle<()>`.

- [ ] **Step 1: Add serde derives to LedgerItem and ClearKey**

In `crates/gw-ledger/src/flusher.rs`, change the imports and derives:

```rust
use serde::{Deserialize, Serialize};
```

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerItem {
    pub record: UsageRecord,
    pub clear: Option<ClearKey>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClearKey {
    pub request_id: String,
    pub attempt_index: u32,
}
```

- [ ] **Step 2: Create primary delivery module with unit tests**

Create `crates/gw-ledger/src/primary_delivery.rs`:

```rust
use crate::flusher::LedgerItem;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct PrimaryDelivery {
    tx: mpsc::Sender<LedgerItem>,
    pending: Arc<AtomicUsize>,
}

impl PrimaryDelivery {
    pub fn new(tx: mpsc::Sender<LedgerItem>) -> Self {
        Self {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    pub async fn enqueue(&self, item: LedgerItem) -> Result<(), LedgerItem> {
        self.pending.fetch_add(1, Ordering::AcqRel);
        match self.tx.send(item).await {
            Ok(()) => {
                metrics::gauge!("gw_billing_primary_overflow_pending")
                    .set(self.pending() as f64);
                Ok(())
            }
            Err(err) => {
                self.pending.fetch_sub(1, Ordering::AcqRel);
                Err(err.0)
            }
        }
    }

    pub fn mark_drained(&self, count: usize) {
        self.pending.fetch_sub(count, Ordering::AcqRel);
        metrics::gauge!("gw_billing_primary_overflow_pending").set(self.pending() as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{UsageRecord, UsageStatus, VendorExposureState};
    use gw_core::ids::{KeyId, ModelName, UserId, VendorId};
    use gw_core::permissions::MarkupChainSnapshot;
    use gw_core::usage::UsageSnapshot;

    fn record() -> UsageRecord {
        UsageRecord {
            ts_ms: 1,
            request_id: "r".to_string(),
            trace_id: "t".to_string(),
            user_id: UserId::from("u"),
            child_id: None,
            api_key_id: None,
            client_protocol: "openai".to_string(),
            upstream_protocol: "openai".to_string(),
            model: ModelName::from("m"),
            vendor_id: VendorId::from("v"),
            vendor_key_id: KeyId::from("k"),
            status: UsageStatus::Ok,
            error_code: None,
            http_status: 200,
            usage: UsageSnapshot::default(),
            attempt_index: 0,
            vendor_exposure_state: VendorExposureState::Settled,
            usage_provenance: Default::default(),
            latency_ms: 0,
            ttft_ms: None,
            is_stream: false,
            freeze_amount_micro: 0,
            estimate_pricing_version: 0,
            markup_chain_snapshot: MarkupChainSnapshot::identity(),
            operation_id: None,
            pod_id: String::new(),
            credits_consumed: None,
            coef_table_version: None,
            attempted_keys: Vec::new(),
        }
    }

    #[tokio::test]
    async fn enqueue_preserves_clear_key() {
        let (tx, mut rx) = mpsc::channel(1);
        let delivery = PrimaryDelivery::new(tx);

        delivery
            .enqueue(LedgerItem {
                record: record(),
                clear: Some(crate::flusher::ClearKey {
                    request_id: "r".to_string(),
                    attempt_index: 7,
                }),
            })
            .await
            .expect("enqueue succeeds");

        let got = rx.recv().await.expect("item");
        let clear = got.clear.expect("clear key preserved");
        assert_eq!(clear.request_id, "r");
        assert_eq!(clear.attempt_index, 7);
    }

    #[tokio::test]
    async fn enqueue_returns_item_when_closed() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let delivery = PrimaryDelivery::new(tx);
        let item = LedgerItem {
            record: record(),
            clear: None,
        };

        let returned = delivery.enqueue(item).await.unwrap_err();

        assert_eq!(returned.record.request_id, "r");
        assert_eq!(delivery.pending(), 0);
    }
}
```

In `crates/gw-ledger/src/lib.rs`, add:

```rust
pub mod primary_delivery;
```

- [ ] **Step 3: Run tests to verify they pass**

Run:

```bash
cargo test -p gw-ledger primary_delivery
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/gw-ledger/src/primary_delivery.rs crates/gw-ledger/src/lib.rs crates/gw-ledger/src/flusher.rs
git commit -m "ledger: add primary delivery overflow queue"
```

---

### Task 5: Route Full Billing Emits Through Primary Overflow

**Files:**
- Modify: `crates/gw-ledger/src/flusher.rs`
- Test: `crates/gw-ledger/tests/flusher.rs`
- Test: `crates/gw-ledger/tests/inflight_clear_via_flusher.rs`

**Interfaces:**
- Consumes: `PrimaryDelivery` from Task 4.
- Produces: `Ledger.primary_overflow`.
- Produces: no Redis XADD in `Ledger::emit` full/closed branch.

- [ ] **Step 1: Write failing static test for no sync bypass in emit**

Add this test to `crates/gw-ledger/src/flusher.rs` test module:

```rust
#[test]
fn emit_full_branch_does_not_call_sync_xadd() {
    let src = include_str!("flusher.rs");
    let emit_start = src.find("pub async fn emit").expect("emit exists");
    let after_emit = src[emit_start..]
        .find("pub fn try_send")
        .map(|n| emit_start + n)
        .unwrap_or(src.len());
    let emit_body = &src[emit_start..after_emit];

    assert!(
        !emit_body.contains("sync_xadd"),
        "Ledger::emit must not synchronously XADD on the request path"
    );
    assert!(
        emit_body.contains("primary_overflow.enqueue"),
        "Ledger::emit must route overflow to primary delivery"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd
```

Expected: FAIL because current `emit` still calls `sync_xadd`.

- [ ] **Step 3: Add primary overflow field and worker startup**

In `crates/gw-ledger/src/flusher.rs`, import the new type:

```rust
use crate::primary_delivery::PrimaryDelivery;
```

Update `Ledger`:

```rust
pub struct Ledger {
    pub tx: mpsc::Sender<LedgerItem>,
    redis: RedisCtx,
    instance_id: String,
    dlq_fallback: DlqFallback,
    primary_overflow: PrimaryDelivery,
}
```

In `start_with_spill_root`, create the overflow channel after the primary channel:

```rust
let (tx, rx) = mpsc::channel::<LedgerItem>(channel_capacity);
let rx = Arc::new(Mutex::new(rx));
let (overflow_tx, overflow_rx) = mpsc::channel::<LedgerItem>(channel_capacity.max(1024));
let primary_overflow = PrimaryDelivery::new(overflow_tx);
```

Spawn the overflow worker before returning:

```rust
let overflow_handle = spawn_primary_overflow_worker(
    redis.clone(),
    instance_id.clone(),
    dlq_fallback.clone(),
    primary_overflow.clone(),
    overflow_rx,
    batch_size,
    flush_interval,
    shutdown.clone(),
    drain_budget,
);
```

Include `overflow_handle` in the returned handles vector:

```rust
(
    Self {
        tx,
        redis,
        instance_id,
        dlq_fallback,
        primary_overflow,
    },
    vec![
        flusher_handle,
        dlq_handle,
        frozen_handle,
        backlog_handle,
        overflow_handle,
    ],
)
```

- [ ] **Step 4: Add primary overflow worker**

Add this function near the existing flusher worker helpers:

```rust
fn spawn_primary_overflow_worker(
    redis: RedisCtx,
    instance_id: String,
    dlq_fallback: DlqFallback,
    primary_overflow: PrimaryDelivery,
    mut rx: mpsc::Receiver<LedgerItem>,
    batch_size: usize,
    flush_interval: Duration,
    shutdown: ShutdownToken,
    drain_budget: Duration,
) -> JoinHandle<()> {
    Supervisor::new("billing_primary_overflow", RestartPolicy::default(), shutdown).spawn(
        move |tok| {
            let redis = redis.clone();
            let instance_id = instance_id.clone();
            let dlq_fallback = dlq_fallback.clone();
            let primary_overflow = primary_overflow.clone();
            async move {
                let mut buf = Vec::with_capacity(batch_size);
                let mut ticker = interval(flush_interval);
                loop {
                    tokio::select! {
                        _ = tok.cancelled() => {
                            let deadline = tokio::time::Instant::now() + drain_budget;
                            while tokio::time::Instant::now() < deadline {
                                match rx.try_recv() {
                                    Ok(item) => buf.push(item),
                                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                                }
                                if buf.len() >= batch_size {
                                    let drained = buf.len();
                                    let mut taken = std::mem::take(&mut buf);
                                    flush(&redis, &instance_id, &dlq_fallback, &mut taken).await;
                                    primary_overflow.mark_drained(drained);
                                }
                            }
                            if !buf.is_empty() {
                                let drained = buf.len();
                                flush(&redis, &instance_id, &dlq_fallback, &mut buf).await;
                                primary_overflow.mark_drained(drained);
                            }
                            return Ok(());
                        }
                        maybe = rx.recv() => {
                            match maybe {
                                Some(item) => {
                                    buf.push(item);
                                    if buf.len() >= batch_size {
                                        let drained = buf.len();
                                        let mut taken = std::mem::take(&mut buf);
                                        flush(&redis, &instance_id, &dlq_fallback, &mut taken).await;
                                        primary_overflow.mark_drained(drained);
                                    }
                                }
                                None => break,
                            }
                        }
                        _ = ticker.tick() => {
                            if !buf.is_empty() {
                                let drained = buf.len();
                                let mut taken = std::mem::take(&mut buf);
                                flush(&redis, &instance_id, &dlq_fallback, &mut taken).await;
                                primary_overflow.mark_drained(drained);
                            }
                        }
                    }
                }
                Ok(())
            }
        },
    )
}
```

- [ ] **Step 5: Replace sync bypass in emit**

Replace the `Full` and `Closed` branches in `Ledger::emit` with:

```rust
Err(tokio::sync::mpsc::error::TrySendError::Full(item_back)) => {
    metrics::counter!("gw_billing_emit_path_total", "path" => "primary_overflow").increment(1);
    if let Err(item_back) = self.primary_overflow.enqueue(item_back).await {
        tracing::error!(
            target: "gw_ledger",
            "primary overflow channel closed; routing record to DLQ"
        );
        let envelope = DlqEnvelope::new(
            item_back.record.clone(),
            "primary_overflow_closed",
            0,
            DlqLayer::Mem,
            now_ms(),
        );
        route_to_dlq(&self.dlq_fallback, envelope).await;
        if item_back.clear.is_some() {
            metrics::counter!(
                "gw_billing_inflight_clear_total",
                "outcome" => "skipped_dlq"
            )
            .increment(1);
        }
    }
}
Err(tokio::sync::mpsc::error::TrySendError::Closed(item_back)) => {
    metrics::counter!("gw_billing_emit_path_total", "path" => "primary_closed").increment(1);
    metrics::counter!("gw_billing_flusher_dead_total").increment(1);
    tracing::error!(
        "usage record channel closed; primary flusher dead — routing to overflow"
    );
    if let Err(item_back) = self.primary_overflow.enqueue(item_back).await {
        let envelope = DlqEnvelope::new(
            item_back.record.clone(),
            "primary_and_overflow_closed",
            0,
            DlqLayer::Mem,
            now_ms(),
        );
        route_to_dlq(&self.dlq_fallback, envelope).await;
        if item_back.clear.is_some() {
            metrics::counter!(
                "gw_billing_inflight_clear_total",
                "outcome" => "skipped_dlq"
            )
            .increment(1);
        }
    }
}
```

Do not delete `sync_xadd` yet; Task 6 removes it after integration tests are updated.

- [ ] **Step 6: Update `from_parts` test constructor**

Update `Ledger::from_parts` to create a primary overflow channel:

```rust
let (overflow_tx, _overflow_rx) = mpsc::channel::<LedgerItem>(16);
Self {
    tx,
    redis,
    instance_id,
    dlq_fallback,
    primary_overflow: PrimaryDelivery::new(overflow_tx),
}
```

- [ ] **Step 7: Run tests**

Run:

```bash
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd
cargo test -p gw-ledger primary_delivery
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/gw-ledger/src/flusher.rs
git commit -m "ledger: route full emits through async overflow"
```

---

### Task 6: Preserve Inflight DEL Through Overflow Delivery

**Files:**
- Modify: `crates/gw-ledger/tests/inflight_clear_via_flusher.rs`
- Modify: `crates/gw-ledger/tests/flusher.rs`
- Modify: `crates/gw-ledger/src/flusher.rs`
- Modify: `crates/gw-gateway/src/metrics_catalog.rs`

**Interfaces:**
- Consumes: overflow worker from Task 5.
- Produces: updated tests proving overflow-delivered records still DEL inflight keys after Redis XADD.

- [ ] **Step 1: Replace full-bypass integration test**

In `crates/gw-ledger/tests/inflight_clear_via_flusher.rs`, rename `full_bypass_sync_xadd_dels_inflight_key` to:

```rust
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn full_channel_overflow_worker_xadd_dels_inflight_key() {
    let node = Redis::default().start().await.unwrap();
    let port = node.get_host_port_ipv4(6379).await.unwrap();
    let redis = RedisCtx::connect(&format!("redis://127.0.0.1:{port}"), 4)
        .await
        .unwrap();
    let instance = "test-inst-overflow".to_string();

    let inflight = InflightLedger::new((*redis.pool).clone(), instance.clone());
    inflight
        .register(&InflightRegistration {
            request_id: "req-overflow".to_string(),
            attempt_index: 0,
            user_id: "u".to_string(),
            vendor_key_id: "vk".to_string(),
            freeze_amount: 0,
            prompt_estimate_tokens: 0,
        })
        .await
        .unwrap();

    let key = InflightLedger::test_key(&instance, "req-overflow", 0);
    assert_eq!(redis.pool.exists::<u64, _>(&key).await.unwrap(), 1);

    let (_tmp, spill_root) = make_spill_root();
    let (ledger, _handles) = Ledger::start(
        redis.clone(),
        instance.clone(),
        1,
        1,
        Duration::from_millis(50),
        spill_root,
        DLQ_SHARDS,
        ShutdownToken::new(),
        Duration::from_secs(5),
    );

    ledger
        .emit(LedgerItem {
            record: fresh_usage_record("filler", 0),
            clear: None,
        })
        .await;
    ledger
        .emit(LedgerItem {
            record: fresh_usage_record("req-overflow", 0),
            clear: Some(ClearKey {
                request_id: "req-overflow".to_string(),
                attempt_index: 0,
            }),
        })
        .await;

    tokio::time::sleep(Duration::from_millis(500)).await;

    let xlen: u64 = redis.pool.xlen("usage:records").await.unwrap();
    assert!(xlen >= 1, "overflow worker delivered usage record");
    assert_eq!(
        redis.pool.exists::<u64, _>(&key).await.unwrap(),
        0,
        "overflow worker XADD success must DEL inflight key"
    );
}
```

- [ ] **Step 2: Replace closed-branch test**

Replace `closed_branch_routes_through_sync_xadd_with_del` with:

```rust
#[tokio::test]
#[ignore = "requires Docker (testcontainers Redis)"]
async fn closed_primary_routes_to_overflow_without_sync_xadd() {
    let src = include_str!("../src/flusher.rs");
    let emit_start = src.find("pub async fn emit").expect("emit exists");
    let after_emit = src[emit_start..]
        .find("pub fn try_send")
        .map(|n| emit_start + n)
        .unwrap_or(src.len());
    let emit_body = &src[emit_start..after_emit];

    assert!(!emit_body.contains("sync_xadd"));
    assert!(emit_body.contains("primary_overflow.enqueue"));
}
```

- [ ] **Step 3: Update flusher full-channel test wording**

In `crates/gw-ledger/tests/flusher.rs`, rename `emit_bypasses_to_sync_xadd_when_channel_full` to `emit_routes_to_overflow_when_channel_full`, and replace the comments with:

```rust
/// Must-deliver overflow: when the primary mpsc channel is full, `emit` routes
/// to the supervised primary overflow worker rather than performing a request-path
/// Redis XADD.
```

Keep the final Redis `xlen >= 1` assertion, but increase the wait to:

```rust
tokio::time::sleep(Duration::from_millis(600)).await;
```

- [ ] **Step 4: Delete unused sync bypass helpers**

After the updated tests pass, delete these functions from `crates/gw-ledger/src/flusher.rs` if no longer referenced:

```rust
async fn sync_xadd(redis: &RedisCtx, rec: &UsageRecord) -> Result<(), String> { ... }
async fn sync_del_inflight(redis: &RedisCtx, instance_id: &str, ck: &ClearKey) { ... }
```

If `sync_del_inflight` is only used by the removed bypass branch, remove it together with `sync_xadd`.

- [ ] **Step 5: Update metrics catalog text**

In `crates/gw-gateway/src/metrics_catalog.rs`, replace the help text for `gw_billing_mpsc_bypass_total` with an overflow metric entry:

```rust
MetricSpec {
    name: "gw_billing_emit_path_total",
    kind: MetricKind::Counter,
    labels: &["path"],
    help: "Billing emit path selected; path=channel is normal, path=primary_overflow means the primary queue was full and delivery moved to the supervised overflow worker.",
},
```

Remove catalog entries that only describe sync XADD bypass if they are no longer emitted.

- [ ] **Step 6: Run tests**

Run:

```bash
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd
cargo test -p gw-ledger primary_delivery
cargo test -p gw-ledger --test inflight_clear_via_flusher -- --ignored
cargo test -p gw-ledger --test flusher -- --ignored
```

Expected: unit tests PASS. Ignored Docker tests PASS when Docker is available; if Docker is unavailable, report that they were not run.

- [ ] **Step 7: Commit**

```bash
git add crates/gw-ledger/src/flusher.rs crates/gw-ledger/tests/inflight_clear_via_flusher.rs crates/gw-ledger/tests/flusher.rs crates/gw-gateway/src/metrics_catalog.rs
git commit -m "ledger: preserve inflight clearing through async overflow"
```

---

### Task 7: Gateway Wiring And End-To-End Checks

**Files:**
- Modify: `crates/gw-gateway/src/lib.rs`
- Modify: `crates/gw-gateway/src/config.rs`
- Test: existing workspace tests

**Interfaces:**
- Consumes: archive storage config from Task 1.
- Consumes: memory archive runtime enum from Task 2.
- Consumes: primary overflow worker from Task 5.
- Produces: startup wiring where default config uses memory archive and no response-body bridge.

- [ ] **Step 1: Add startup log for archive mode**

In `crates/gw-gateway/src/lib.rs`, after computing `archive_storage` and before building `Wired`, add:

```rust
tracing::info!(
    target: "gw_gateway",
    archive_storage = ?archive_storage,
    soft_cap_bytes = cfg.proxy.archive.soft_cap_bytes,
    hard_cap_bytes = cfg.proxy.archive.hard_cap_bytes,
    "response archive configured"
);
```

- [ ] **Step 2: Add startup log for primary overflow handle count**

After `Ledger::start_with_spill_root(...)` returns and before registering handles, add:

```rust
tracing::info!(
    target: "gw_gateway",
    ledger_workers = ledger_handles.len(),
    "ledger workers started"
);
```

Expected value after Task 5 is `5`.

- [ ] **Step 3: Run focused tests**

Run:

```bash
cargo test -p gw-gateway proxy_archive_storage
cargo test -p gw-proxy --test response_hot_path
cargo test -p gw-proxy --test archive_state
cargo test -p gw-ledger primary_delivery
cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd
```

Expected: PASS.

- [ ] **Step 4: Run broader compile check**

Run:

```bash
cargo check -p gw-gateway -p gw-proxy -p gw-ledger
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/gw-gateway/src/lib.rs crates/gw-gateway/src/config.rs
git commit -m "gateway: wire hot path archive and ledger overflow"
```

---

## Verification Matrix

- Response memory hot path:
  - `cargo test -p gw-proxy --test response_hot_path`
  - Confirms `run_memory_sync` exists and does not contain `.await`.
  - Confirms gateway chooses memory branch before any `block_in_place`.

- Archive bounds:
  - `cargo test -p gw-proxy --test archive_state`
  - Confirms memory mode does not create files.
  - Confirms hard cap wins before first disk spill.

- Config:
  - `cargo test -p gw-gateway proxy_archive_storage`
  - Confirms default is memory mode.
  - Confirms explicit `disk_spill` remains available.

- Billing overflow:
  - `cargo test -p gw-ledger primary_delivery`
  - `cargo test -p gw-ledger emit_full_branch_does_not_call_sync_xadd`
  - Confirms `Ledger::emit` no longer calls `sync_xadd`.
  - Confirms overflow preserves `ClearKey`.

- Redis integration:
  - `cargo test -p gw-ledger --test inflight_clear_via_flusher -- --ignored`
  - Confirms overflow delivery still deletes inflight keys after XADD.

- Workspace:
  - `cargo check -p gw-gateway -p gw-proxy -p gw-ledger`

---

## Operational Notes

- Default `[proxy.archive] storage = "memory"` removes response archive disk I/O from the hot path and truncates at `hard_cap_bytes`.
- Operators that require legacy large-body archive fidelity can set `storage = "disk_spill"` and accept that Pingora's synchronous response-body hook needs a bridge only for disk-capable mode.
- Billing full-channel delivery no longer performs request-path Redis XADD. It moves to a supervised overflow worker and still uses the existing `flush(...)` function, so XADD-confirmed records retain the existing inflight DEL behavior.
- Exact hard-crash durability for every post-response usage record would require appending every `LedgerItem` to disk before `emit()` returns. That is intentionally not the default because it adds per-request disk I/O. This plan preserves the current crash-safety model through inflight reconciliation while removing avoidable request-tail Redis sync work.

---

## Self-Review

- Spec coverage:
  - "不用磁盘的时候不走同步桥接": Task 1 sets memory archive default, Task 2 adds sync memory archive, Task 3 routes memory mode before `block_in_place`.
  - "业务需要": Existing response parsing, usage extraction, detailed archive enqueue, billing record emission, and inflight DEL rule are preserved.
  - "异步任务必须完成 / must-deliver": Tasks 4-6 add supervised overflow delivery and preserve `ClearKey`; failed primary queue no longer drops or synchronously XADDs.

- Placeholder scan:
  - The plan contains no placeholder tokens, deferred implementation markers, or unspecified edge handling.

- Type consistency:
  - `ProxyArchiveStorage` maps to `gw_proxy::ctx::ArchiveStorage`.
  - `ArchiveCaps.storage` is consumed by `run_memory_sync` and gateway routing.
  - `PrimaryDelivery::enqueue` consumes and returns `LedgerItem`, preserving `ClearKey`.
