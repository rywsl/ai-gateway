# RUST-W6 response 502 report

Status: DONE_WITH_CONCERNS

Files changed:
- crates/gw-proxy/src/filters/response.rs
- .sdd/RUST-W6-response-502-report.md

Change summary:
- Added a focused regression test for non-stream cross-protocol parse failure.
- Changed handle_non_stream_chunk so same-protocol parse failures still preserve vendor pass-through, but cross-protocol parse failures set GW_UPSTREAM_PROTOCOL_DECODE_ERROR and do not restore raw upstream bytes to upstream_body_accum.
- Did not touch gw-ledger, gw-runtime, gw-task, Go files, or docs.

TDD evidence:
- RED attempt 1: cargo test -p gw-proxy cross_protocol -- --nocapture began compiling gw-proxy dependencies but stayed silent during gw-proxy compilation for several minutes; interrupted to avoid an unbounded run.
- RED attempt 2: cargo test -p gw-proxy cross_protocol_parse_failure_sets_decode_error_and_drops_raw_body -- --nocapture was blocked by Cargo build directory lock held by unrelated running gw-store cargo test jobs.

GREEN evidence:
- Blocked: unrelated running commands cargo test -p gw-store quota_state_writer_persists_outbox_in_pg_transaction and cargo test -p gw-store quota_state_outbox_processor_uses_locked_due_rows_and_state_steps hold the shared target build lock. I did not kill them because they are outside this task ownership.

M8 / scope notes:
- No lease, freeze, overflow, record_actual, refund, or record_actual_delta code was changed.
- No protected-to-frozen behavior was changed.
- The response filter now supplies the preset upstream error code consumed by existing fail_to_proxy JSON error handling.

Concerns:
- I could not complete a green cargo test before this report because unrelated gw-store test processes were already using the shared Cargo target lock.
- Existing plan text mentions W6-T08 work in fail_to_proxy, but current gateway fail_to_proxy already serializes preset upstream_error_code values as JSON responses. This patch stays in the assigned likely file and fixes the blocker that raw cross-protocol bytes were still preserved for forwarding.
