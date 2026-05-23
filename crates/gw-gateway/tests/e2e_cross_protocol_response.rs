//! S-codec T9 — E2E cross-protocol non-stream response (v1 stub + deferral).
//!
//! ## Status
//!
//! **Deferred to a follow-up sub-task.** This file ships as a single
//! `#[ignore]`-tagged compile-only stub that documents what the test
//! *would* do, plus what infrastructure is still missing.
//!
//! ## Why deferred
//!
//! Every other gw-gateway integration test (`e2e_full_flow.rs`,
//! `e2e_tunnel.rs`) and the gw-proxy `end_to_end_openai.rs` smoke test
//! collectively defer the "spin up the actual Pingora server in-process
//! and issue a real HTTP request" wiring to v2 with the same rationale:
//!
//!   - `pingora::server::Server::run_forever` is `-> !` and consumes the
//!     `Server` value, so the same-process pattern requires a dedicated
//!     OS thread + a bind-port handshake we do not yet have.
//!   - The companion `e2e-fake-upstream` crate is a standalone binary
//!     consumed via docker-compose; in-process consumption from a
//!     `cargo test` requires plumbing a `serve_business` + `serve_admin`
//!     wrapper that we have not built.
//!   - The Python E2E suite under `tests/e2e/client/` is the established
//!     real-HTTP harness for cross-protocol scenarios — `make e2e` boots
//!     gateway + fake-web + fake-upstream + Postgres + Redis via
//!     docker-compose and pytest drives the SDKs against the live ports.
//!     It already covers cross-protocol *streaming*
//!     (`tests/e2e/client/scenarios/test_cross_protocol.py`), but
//!     does NOT yet cover cross-protocol non-stream after T6 wired
//!     `CrossRender` in the non-stream branch — the existing
//!     `test_non_stream.py::test_cross_protocol_non_stream_returns_503`
//!     is now stale (it still asserts the pre-T6 503 short-circuit).
//!
//! Building a new in-process Pingora harness for a single S-codec test
//! is the kind of "meaningful project on its own" that the v2 scope
//! comment in `e2e_full_flow.rs` calls out. The S-codec implementation
//! plan (`docs/superpowers/plans/2026-05-24-v2-codec-cleanup-plan.md`,
//! T9) sketched an `E2eHarness::start_with_fake_upstream()` helper, but
//! that helper does not yet exist in the workspace.
//!
//! ## Coverage already shipped (no E2E gap at the codec layer)
//!
//! The proxy-level wiring this test would assert against has already
//! landed and is exercised by lower-level tests:
//!
//!   - **T5** (`crates/gw-codec/tests/cross_protocol_matrix.rs` — the
//!     `adapt_lossy` matrix) covers the IR-shape side of all 16
//!     (from, to) combinations.
//!   - **T6** (`crates/gw-proxy/src/filters/response.rs::handle_non_stream_chunk`)
//!     wires `CrossRender::run` into the non-stream EOS path and
//!     increments `gw_codec_cross_render_nonstream_total{from,to}` on
//!     success / `gw_codec_cross_render_failure_total{...}` on failure.
//!     Cross-protocol detection there uses `WireFormat` (4-way), not
//!     `ProtocolKind` (3-way), so the OpenAI Chat vs OpenAI Responses
//!     distinction is preserved.
//!   - **T7** (`crates/gw-codec/tests/cross_protocol_matrix.rs` — the
//!     4×4 wire-bytes round-trip matrix) parses the rendered bytes
//!     back through the target codec and asserts on `usage` /
//!     `finish_reason` / lossy-field absence. This is the closest
//!     analogue to "client sees an OpenAI-shaped body" without
//!     actually running the proxy filter chain.
//!   - **T8** (`crates/gw-codec/tests/lossy_paths.rs`) verifies that
//!     `gw_codec_cross_render_lossy_total{from,to,field=...}` increments
//!     for `thinking` / `reasoning` / `reasoning_tokens` /
//!     `finish_reason.other` drops, and that the rendered bytes do not
//!     contain the dropped field.
//!
//! What is NOT yet asserted (and what this T9 v2 should add) is the
//! *integration* assertion: that an HTTP POST to `/v1/chat/completions`
//! at the live `:8000` business port, served by an Anthropic-only
//! vendor key, with a fake upstream returning an Anthropic-shaped JSON
//! body containing a `tool_use` block, lands in the OpenAI SDK as a
//! parsed `tool_calls[0].function.name = "get_weather"`. That assertion
//! crosses the request_filter → upstream_peer → upstream_request →
//! response_filter → response_body_filter pipeline end-to-end and is
//! the canary for the wiring T6 added.
//!
//! ## Follow-up tracker
//!
//! Two paths to close this:
//!
//!   1. **Cheap (preferred):** add a `test_cross_protocol_non_stream.py`
//!      under `tests/e2e/client/scenarios/` (or extend
//!      `test_cross_protocol.py` to include non-stream scenarios) AND
//!      update `test_non_stream.py::test_cross_protocol_non_stream_returns_503`
//!      to assert success instead of 503. Reuses the
//!      `FakeUpstreamClient` / `make_openai_client` / `seed_*` fixtures
//!      already in `tests/e2e/client/conftest.py`. The Prometheus
//!      counter assertion can read from
//!      `http://localhost:9090/metrics` (already host-exposed by
//!      `tests/e2e/docker-compose.yml`).
//!   2. **Expensive:** build the in-process `E2eHarness` in Rust
//!      (Pingora `Server` on a worker thread + `e2e-fake-upstream`
//!      `serve_business` / `serve_admin` on ephemeral ports +
//!      `hyper::Client` POSTing through Pingora). This unlocks the
//!      same harness for `e2e_full_flow.rs`, `e2e_tunnel.rs`, and
//!      `gw-proxy/tests/end_to_end_openai.rs`, all currently
//!      deferred to v2 with the same comment.
//!
//! ## Scenarios this test should cover (per S-codec spec §5.4)
//!
//! When the harness lands, the four scenarios sketched below must all
//! pass against the real proxy filter chain:
//!
//! | # | Client wire format | Upstream wire format | Asserts                                                                                            |
//! |---|--------------------|----------------------|----------------------------------------------------------------------------------------------------|
//! | 1 | `openai_chat`      | `anthropic_messages` | `body.choices[0].message.tool_calls[0].function.name == "get_weather"` + counter `from=anthropic_messages,to=openai_chat` |
//! | 2 | `anthropic_messages` | `openai_chat`     | `body.content[].type == "tool_use"` block + counter `from=openai_chat,to=anthropic_messages`       |
//! | 3 | `gemini_generate`  | `openai_chat`        | `body.candidates[0].content.parts[].functionCall` + counter `from=openai_chat,to=gemini_generate`  |
//! | 4 | `openai_chat`      | `anthropic_messages` | thinking lossy path: `gw_codec_cross_render_lossy_total{from=anthropic_messages,to=openai_chat,field=thinking}` ++ 1 and rendered body contains no `thinking` text |
//!
//! Run with (once the harness lands):
//!   `cargo test -p gw-gateway --test e2e_cross_protocol_response -- --ignored --nocapture`

// ---------------------------------------------------------------------------
// Compile-only smoke: keep the imports the v2 harness will need wired so the
// follow-up PR does not also need to chase a fresh dependency graph. Marked
// `#[allow(unused)]` because none of these are exercised until the harness
// lands.
// ---------------------------------------------------------------------------

#[allow(unused_imports)]
use gw_codec::cross_render::WireFormat;

/// TODO_E2E: replace with the real test once the in-process Pingora harness
/// (or its Python counterpart under `tests/e2e/client/scenarios/`) is in
/// place. Until then, this `#[ignore]` placeholder anchors the test name
/// and keeps the file in the build, so a future PR can land the body
/// without first adding the test target to `gw-gateway/Cargo.toml`.
#[tokio::test]
#[ignore = "S-codec T9 v2 harness deferred — see module-level docs and \
            tests/e2e/client/scenarios/ for the existing Python E2E suite \
            that should grow the cross-protocol non-stream coverage"]
async fn cross_protocol_anthropic_to_openai_chat_tool_use_e2e_placeholder() {
    // TODO_E2E (S-codec T9 v2 follow-up):
    //   1. Start fake_upstream on an ephemeral port; register an Anthropic
    //      response containing a tool_use block (model claude-3-5-sonnet,
    //      stop_reason "tool_use", input tokens 10 / output tokens 5).
    //   2. Boot LlmGateway on an ephemeral business port pointed at the
    //      fake upstream; seed (user, api_key, vendor, vendor_key,
    //      model_group, pricing) with accessible_endpoints=["anthropic"]
    //      and protocol_conversion=true so an OpenAI-shaped client can
    //      route to the Anthropic key.
    //   3. POST /v1/chat/completions with `stream=false` and a
    //      `tools=[{"type":"function","function":{"name":"get_weather"}}]`
    //      payload, pair-header auth.
    //   4. Assert the OpenAI-shaped response body:
    //        body["choices"][0]["message"]["tool_calls"][0]["function"]["name"] == "get_weather"
    //        body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] contains "SF"
    //   5. Scrape /metrics and assert
    //        gw_codec_cross_render_nonstream_total{from="anthropic_messages",to="openai_chat"} >= 1
    //
    // Reference labels emitted by the proxy (verified against the live
    // metrics endpoint), so the v2 PR can assert exact label cardinality
    // without re-deriving:
    let _from = WireFormat::AnthropicMessages.label();
    let _to = WireFormat::OpenAiChat.label();
    debug_assert_eq!(_from, "anthropic_messages");
    debug_assert_eq!(_to, "openai_chat");

    // Intentionally do not panic — `#[ignore]` keeps this out of `cargo
    // test` by default, and on the rare manual `--ignored` run we want
    // visible "this is a stub" output rather than a red bar.
    eprintln!(
        "T9 v2 placeholder — wiring deferred; see module-level docs for \
         the follow-up plan."
    );
}
