# Spec: Fix non-Unix build blocker in `SpillDiskFullHook`

Date: 2026-06-13
Scope: `crates/gw-runtime/src/health/disk.rs` only.

## Problem

`crates/gw-runtime/src/health/disk.rs` is compiled on every target (it is an
unconditional `pub mod disk;` in `health/mod.rs`), but its body is Unix-only:

- `use std::os::unix::ffi::OsStrExt;` — `std::os::unix` does not exist on Windows.
- `libc::statvfs` (type + function) — not present in `libc` on Windows GNU.
- `OsStr::as_bytes()` — provided by the Unix-only `OsStrExt` trait.

This breaks `cargo +1.88.0-x86_64-pc-windows-gnu` builds before any dependent
crate's tests (e.g. `gw-auth`) can run:

```
error[E0433]: failed to resolve: could not find `unix` in `os`
error[E0412]: cannot find type `statvfs` in crate `libc`
error[E0425]: cannot find function `statvfs` in crate `libc`
error[E0599]: no method named `as_bytes` found for reference `&OsStr`
```

## Fix

Preserve existing Unix behaviour exactly; make non-Unix targets compile by
turning the platform probe into a no-op:

1. `#[cfg(unix)]`-gate the `CString` / `OsStrExt` imports.
2. Split `SpillDiskFullHook::statvfs(&self) -> Option<(u64, u64)>` into:
   - `#[cfg(unix)]` — the existing `libc::statvfs` implementation, unchanged.
   - `#[cfg(not(unix))]` — returns `None`.
3. `check()` already maps `None` → `HealthSignal::Healthy`, so on non-Unix the
   hook is a healthy no-op. No semantic change on Unix.
4. Annotate the now-unused-on-non-Unix `path` field with
   `#[cfg_attr(not(unix), allow(dead_code))]` to keep `-D warnings` clean.

## Out of scope / non-goals

- No change to gateway health semantics on Unix.
- No change to any other file.
- Do not touch unrelated dirty working-tree changes.

## Acceptance criteria

1. `cargo fmt --all -- --check` passes.
2. The Windows-GNU focused build gets past `disk.rs`:
   ```
   $env:CARGO_TARGET_DIR='C:\cnxus-targets\p016-auth'
   cargo +1.88.0-x86_64-pc-windows-gnu test -p gw-auth --test merge missing_ -j 1
   ```
   Later unrelated failures are acceptable; report them exactly.
