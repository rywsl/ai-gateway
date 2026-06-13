# P0 #20 — KEK rotation single-point-of-failure + signature_secret contract lie

> Implementation spec. Source of truth: Codex analysis (CNxus-Rust #20 / CNexus-Go #31).
> Scope: **Rust only**. Do **not** edit Go. Preserve the unrelated in-flight `ResetCost`
> control-plane work already in the working tree.

## Problem (as reproduced)

Issue #20 bundles three defects:

1. **`signature_secret` contract lie.** `docs/go-contracts/parts/03-shared-schema.md`
   (lines 35 & 121) claims `api_keys.signature_secret BYTEA` is "KEK 加密 at rest".
   Reality: migration `0008_signature_secret.sql` adds a plain `BYTEA`;
   `gw-store/src/loader.rs` (`API_KEY_SQL` / `API_KEY_BY_HASH_SQL`) projects the column
   as a raw integer byte-array via `jsonb_agg(get_byte(...))`, so
   `ApiKeyEntry.signature_secret: Option<Vec<u8>>` receives **plaintext**. There is no
   `decrypt(...)` step (contrast `load_vendor_key`, which decrypts `api_key_enc`).
   **Go owns writes** to this column; Rust only reads it.

2. **KEK single point of failure / "三层锁死".** `gw-gateway/src/kms.rs::init` loads
   exactly **one** KEK from `GW_KMS__KEK_HEX`. `KekCache` (encrypt.rs) implements a
   2-KEK dual-read state machine (`enter_dual_read` / `mark_reencrypting` /
   `drop_old_kek`), but boot can never hold more than one KEK. A restart **during a
   rotation window** can therefore only load one KEK and cannot decrypt data written
   under the other → data-availability deadlock.

3. **`rotate-kek` honesty.** The command is already disabled in
   `command_consumer.rs` (reason `kek_rotation_command_hook_missing`) and
   `go-contracts/parts/08` correctly says "disabled + reason". But
   `docs/design/2026-06-08-control-plane-sp-c-command-channel.md` (lines 14, 91) claims
   `✅ encrypt.rs DualRead 轮换已有 → 触发轮换流程`, i.e. that rotation is wired. It is not
   (no command-plane wiring, no at-rest re-encryption worker).

## Scope decision

- **signature_secret → truthful docs (Codex-allowed conservative path).** Rust cannot
  unilaterally encrypt this column because Rust never writes it; encryption-at-rest is a
  coordinated Rust+Go change (Codex owns the Go side). Implementing read-support for an
  envelope shape Go does not yet write would be speculative, untested-against-reality
  code and would unilaterally invent a cross-process wire format — contradicting
  "do not fake" and the sequencing "Go must not change storage until Rust supports the
  new contract". We instead **correct the docs to the as-built reality**, document the
  security risk + the recommended migration, and add a regression test that **locks** the
  plaintext loader contract so it cannot silently drift. Fail-closed semantics
  (`SecretUnavailable` → `GW_AUTH_SIGNATURE_NOT_CONFIGURED`) are already correct and stay.

- **KEK SPOF → multi-KEK bootstrap (Codex-preferred "dual-read bootstrap").** Allow boot
  to hold the `{active, previous}` resident set so a restart mid-rotation survives.

- **rotate-kek → keep disabled, fix the lying design doc.** Re-encryption worker is out
  of scope ("do not fake re-encryption completion").

## Changes

### D1 — signature_secret truthful contract + lock test
- `docs/go-contracts/parts/03-shared-schema.md` lines 35 & 121: replace the
  "KEK 加密 at rest" claim with the truth (raw `BYTEA` plaintext, **not** KEK-encrypted),
  a `risk:` note, and the `migration:` path (envelope + `decrypt_bytes` + Go writer,
  coordinated with CNexus-Go #31).
- `docs/spec/05-auth-permissions.md`: same correction if it repeats the claim.
- New characterization test in `gw-store/src/loader.rs` `#[cfg(test)]`:
  - a payload whose `signature_secret` is a JSON int-array decodes into
    `ApiKeyEntry.signature_secret` as those **exact raw bytes** (proves no decryption);
  - assert `API_KEY_SQL` / `API_KEY_BY_HASH_SQL` still use the `get_byte(...)` plaintext
    projection (locks the SQL contract; a future silent-encryption change breaks it).

### D2 — multi-KEK bootstrap (`gw-gateway`)
- `config.rs` `KmsCfg`: add `#[serde(default)] previous_kek_hex: Option<SecretString>`
  and `#[serde(default)] previous_key_id: Option<String>`.
- `kms.rs`: factor a pure, testable
  `fn build_kek_cache(active_kid, active_hex, previous: Option<(&str,&SecretString)>) -> Result<KekCache>`:
  decode active (32-byte hex), optionally decode previous, build
  `KekCache::with_active(map, Some(active_kid))` so active is pinned regardless of map
  order. Validate: previous hex/id must be supplied together; previous kid must differ
  from active. `init` becomes a thin wrapper.
- TDD unit tests (named with `kek` for `cargo test -p gw-gateway`): single-KEK,
  active+previous (both kids resolvable, active pinned), mismatched-pair error.
- `config/default.toml [kms]`: comment the new optional fields.

### D3 — KekCache state-machine tests (`gw-store/src/encrypt.rs`)
- Unit tests for: full `Healthy → DualRead → DualRead{reencrypting} → Healthy` cycle;
  refuse a second `enter_dual_read`; refuse `drop_old_kek` on the active kid / only kid /
  when not in dual-read; and that `decrypt` works for ciphertext under **both** KEKs
  during dual-read.

### D4 — rotate-kek doc honesty
- `docs/design/2026-06-08-control-plane-sp-c-command-channel.md` lines 14 & 91: state
  that the in-memory `KekCache` dual-read primitive **and** multi-KEK bootstrap exist, but
  the command-plane wiring + at-rest re-encryption worker are **not** built, so the command
  stays disabled (`kek_rotation_command_hook_missing`).
- No `command_consumer.rs` change. Existing `control_plane_wire_lock.rs` already pins
  rotate-kek `enabled:false` + reason — leave intact (preserve ResetCost additions).

### D5 — docs sync + verification
- `docs/architecture.md`: KMS/config table gains the `previous_kek_*` fields; the KEK
  rotation note reflects multi-KEK bootstrap + disabled command + missing re-enc worker.
- Verify: `cargo fmt --all -- --check`; `cargo test -p gw-store`;
  `cargo test -p gw-gateway` (kek/kms); `cargo test -p gw-core --test control_plane
  --test control_plane_wire_lock`. Report any Windows `link.exe` blocker.

## Out of scope (recommended follow-ups for Codex/Go)
- Encrypt `signature_secret` at rest end-to-end: agree the `BYTEA` envelope shape,
  add a binary-safe `decrypt_bytes` + backward-compatible loader decode (Rust), then have
  Go write the envelope (CNexus-Go #31).
- Live `rotate-kek` command + at-rest re-encryption worker.

## Acceptance (Codex review)
- Changes tightly scoped to Rust crates/docs/tests; `ResetCost` + gw-health changes untouched.
- Regression test locks the (now non-lying) plaintext signature_secret contract.
- rotate-kek stays disabled with reason; docs no longer claim it is complete.
- Multi-KEK bootstrap proven by tests; `cargo fmt --check` clean.
