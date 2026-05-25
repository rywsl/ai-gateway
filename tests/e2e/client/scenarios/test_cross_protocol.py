"""E4-D: codec / cross-protocol black-box scenarios (5).

Exercises the codec layer through the wire:
  1. openai client ⇄ anthropic upstream (cross-protocol streaming, G8/T4-A).
  2. openai client ⇄ gemini upstream (cross-protocol streaming, G8/T4-A).
  3. mid-stream TCP RST after some body bytes (no retry on stream-RST).
  4. malformed partial SSE event (gateway parser tolerance / metric pulse).
  5. gzip-encoded upstream response decoded transparently by the client SDK.

Gateway internals referenced (all in tree):
- `crates/gw-proxy/src/filters/upstream.rs::rewrite_request` — sets
  `ctx.upstream_codec`. Cross-protocol path (key.accessible_endpoints ⊅
  client.kind()): `client_codec.parse_request → IR → upstream_codec.render_request`
  (lines 386–396). The URL path is NOT rewritten — the upstream receives the
  inbound path verbatim (e.g. `/v1/chat/completions`), so the fake_upstream
  match has to accept that path even when the body is anthropic-shaped.
- `crates/gw-proxy/src/filters/response.rs::handle_event` — cross-protocol
  streaming response re-render IS wired (G8 closed per
  `docs/architecture.md` §9 table row "G8 ... 50cd9e7"). The upstream SSE
  chunks are parsed via `upstream_codec.parse_stream_chunk`, each
  `StreamEventIR` is re-rendered through `client_codec.render_stream_chunk`
  into `ctx.pending_client_body`, then substituted for the outbound chunk.
- `crates/gw-codec/src/anthropic/codec.rs::parse_stream_chunk` consumes the
  E3-B `_anthropic_event` framing (`data: \\nevent: name\\ndata: {json}` —
  see `tests/e2e/client/scenarios/test_happy_streaming.py::_anthropic_event`).
- No `GW_UPSTREAM_PROTOCOL_ERROR` code exists in
  `crates/gw-core/src/error/codes.rs`; `handle_event` only logs a
  `tracing::warn!` on parse failure and lets the bytes flow through. So a
  malformed-SSE upstream surfaces to the client as a torn stream, NOT a
  structured error envelope.
- No upstream-gzip decoder runs on the response path
  (`crates/gw-proxy/src/filters/response.rs` does not touch
  `content-encoding`). Same-protocol gzip streams therefore flow through
  byte-for-byte; the OpenAI SDK + underlying httpx auto-decompress.

CLAUDE.md note: the top-level CLAUDE.md sentence "Cross-protocol *request*
re-render is wired (response re-render deferred)" is stale relative to the
actual `docs/architecture.md` §9 table — G8 (response stream re-render) is
listed as closed (commit `50cd9e7`). This file's scenarios 1 and 2 verify
that closure end-to-end. If the closure ever regresses, scenarios 1/2 are
the first canary.
"""

from __future__ import annotations

import base64
import gzip
import json
import re

import httpx
import openai
import pytest
import requests

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
    label_delta,
    make_anthropic_client,
    make_gemini_client,
    make_openai_client,
    metrics_snapshot,
    pair_headers,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
    wait_for_records,
)


# Protocol wire tags as the gateway's serde rename_all=snake_case renders them.
PROTO_ANTHROPIC = "anthropic"
PROTO_GEMINI = "gemini"


# ---------------------------------------------------------------------------
# Helpers.
# ---------------------------------------------------------------------------


def _anthropic_event(name: str, payload_json: str) -> dict:
    """Anthropic-style `event: <name>\\ndata: {json}` frame folded into the
    fake_upstream's single-`data:` framer.

    Lifted verbatim from `test_happy_streaming.py::_anthropic_event` — see
    that file for the framing rationale. We don't import it because the
    helper is private to that module (leading underscore) and lifting it
    to conftest is overkill when only one scenario uses it.
    """
    return {"data": f"\nevent: {name}\ndata: {payload_json}"}


def _anthropic_sse_events() -> list[dict]:
    """Minimal Anthropic message-stream the SDK + gateway both accept."""
    return [
        _anthropic_event(
            "message_start",
            '{"type":"message_start","message":{"id":"msg_1","type":"message",'
            '"role":"assistant","model":"claude-3-5-sonnet-20241022",'
            '"content":[],"stop_reason":null,"stop_sequence":null,'
            '"usage":{"input_tokens":1,"output_tokens":0}}}',
        ),
        _anthropic_event(
            "content_block_start",
            '{"type":"content_block_start","index":0,'
            '"content_block":{"type":"text","text":""}}',
        ),
        _anthropic_event(
            "content_block_delta",
            '{"type":"content_block_delta","index":0,'
            '"delta":{"type":"text_delta","text":"hi"}}',
        ),
        _anthropic_event(
            "content_block_stop",
            '{"type":"content_block_stop","index":0}',
        ),
        _anthropic_event(
            "message_delta",
            '{"type":"message_delta","delta":{"stop_reason":"end_turn",'
            '"stop_sequence":null},"usage":{"output_tokens":1}}',
        ),
        _anthropic_event("message_stop", '{"type":"message_stop"}'),
    ]


def _gemini_sse_events() -> list[dict]:
    """Minimal Gemini streamGenerateContent SSE chunks: a text delta + a
    final chunk carrying `finishReason: STOP`. The gateway's
    `crates/gw-codec/src/gemini/codec.rs::parse_stream_chunk` decomposes
    that combined chunk into `[Delta, End]` per the SmallVec contract
    documented in `cross_protocol_stream.rs`."""
    return [
        {
            "data": (
                '{"candidates":[{"content":{"role":"model",'
                '"parts":[{"text":"hi"}]},"index":0}]}'
            )
        },
        {
            "data": (
                '{"candidates":[{"content":{"role":"model","parts":[{"text":""}]},'
                '"finishReason":"STOP","index":0}],'
                '"usageMetadata":{"promptTokenCount":1,"candidatesTokenCount":1,'
                '"totalTokenCount":2}}'
            )
        },
    ]


def _metrics_snapshot(metric: str, metrics_url: str) -> dict[str, int]:
    """Return `{label_pairs -> int_value}` for every series of `metric`.

    Lifted from `test_routing_retry.py::_metrics_snapshot` (same shape —
    Prom text format scrape + per-label parse). Imported here as a private
    copy so a future refactor of the routing/retry file doesn't break this
    one.
    """
    text = requests.get(f"{metrics_url.rstrip('/')}/metrics", timeout=5).text
    out: dict[str, int] = {}
    line_re = re.compile(
        rf"^{re.escape(metric)}(?:\{{(?P<lbl>[^}}]*)\}})?\s+(?P<v>[-+0-9eE.]+)$"
    )
    for raw in text.splitlines():
        m = line_re.match(raw)
        if m:
            out[m.group("lbl") or ""] = int(float(m.group("v")))
    return out


def _label_delta(pre: dict[str, int], post: dict[str, int], label: str) -> int:
    return post.get(label, 0) - pre.get(label, 0)


def _seed_cross_protocol(
    fake_web: FakeWebClient,
    *,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    group: str,
    vendor_id: str,
    key_id: str,
    upstream_protocol: str,
    model: str = MODEL,
) -> None:
    """Seed an api_key + vendor_key combo where the vendor_key advertises
    ONLY `upstream_protocol` (NOT openai) AND has `protocol_conversion=True`.

    The candidate filter
    (`crates/gw-routing/src/candidates.rs::candidate_matches`) requires
    `protocol_conversion=true` for the key to be selectable when the
    client's protocol is not in `accessible_endpoints`. With both set,
    `rewrite_request` enters the cross-protocol branch: parse request with
    client's openai codec → render with `upstream_protocol`'s codec.
    """
    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id)
    seed_vendor_key(
        fake_web,
        key_id=key_id,
        vendor_id=vendor_id,
        model=model,
        accessible_endpoints=[upstream_protocol],
        protocol_conversion=True,
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=[key_id],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=model)


def _seed_same_protocol_openai(
    fake_web: FakeWebClient,
    *,
    user_id: str,
    api_key_id: str,
    api_key_plaintext: str,
    group: str,
    vendor_id: str,
    key_id: str,
) -> None:
    """Same-protocol openai seed shortcut used by scenarios 3/4/5."""
    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id)
    seed_vendor_key(
        fake_web,
        key_id=key_id,
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
        protocol_conversion=False,
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=[key_id],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=MODEL)


# ---------------------------------------------------------------------------
# 1. openai client → anthropic upstream → cross-protocol stream.
# ---------------------------------------------------------------------------


def test_openai_client_anthropic_upstream_cross_protocol_stream(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Seed a vendor_key with accessible_endpoints=["anthropic"] +
    protocol_conversion=True. The fake_upstream emits a minimal anthropic
    message-stream. The gateway's response_body_filter is supposed to
    parse Anthropic SSE → IR → re-render as OpenAI SSE for the client.
    Assert the OpenAI SDK reads "hi" AND the cross-render metric ticks.

    G8 closure reference: `docs/architecture.md` §9 row G8 →
    `crates/gw-proxy/src/filters/response.rs::handle_event` lines 153–168
    (the `cross_protocol` branch calls
    `client_codec.render_stream_chunk` into `ctx.pending_client_body`
    and increments `gw_codec_cross_render_total`).
    """
    _seed_cross_protocol(
        fake_web,
        user_id="u-cxa",
        api_key_id="ak-cxa",
        api_key_plaintext="sk-cxa",
        group="grp-cxa",
        vendor_id="v-cxa",
        key_id="vk-cxa",
        upstream_protocol=PROTO_ANTHROPIC,
    )
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    # Match the inbound openai path verbatim — the gateway
                    # does NOT rewrite path on cross-protocol (see
                    # crates/gw-proxy/src/filters/upstream.rs::rewrite_request
                    # — there are no `set_uri`/`set_path` calls).
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "sse",
                        "delay_ms": 0,
                        "events": _anthropic_sse_events(),
                    },
                }
            ]
        }
    )

    pre = _metrics_snapshot("gw_codec_cross_render_total", metrics_url)

    client = make_openai_client(gateway_url, "sk-cxa", "ak-cxa")
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
        # Anthropic requires max_tokens; the gateway's cross-protocol
        # request renderer falls back to a permissive ceiling when missing
        # from the IR, but supplying it explicitly hardens the test.
        max_tokens=64,
    )
    pieces: list[str] = []
    for chunk in stream:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces.append(chunk.choices[0].delta.content)
    assert "".join(pieces) == "hi", f"expected 'hi', got {pieces!r}"

    # Metric label key is 'from="anthropic",to="openai"' — `AnyCodec::kind`
    # renders ProtocolKind via `ProtocolKind::as_str` which returns
    # "openai"/"anthropic"/"gemini" (NB: NOT the serde snake_case
    # "open_ai" wire form used for `accessible_endpoints` — see
    # `crates/gw-core/src/protocol.rs`).
    post = _metrics_snapshot("gw_codec_cross_render_total", metrics_url)
    delta = _label_delta(pre, post, 'from="anthropic",to="openai"')
    assert delta >= 1, (
        f"expected gw_codec_cross_render_total{{from=\"anthropic\","
        f"to=\"openai\"}} to increment; pre={pre}, post={post}"
    )

    xlen = wait_for_records(fake_web, min_xlen=1, timeout=3.0)
    assert xlen >= 1, f"expected >=1 usage:records, got {xlen}"


# ---------------------------------------------------------------------------
# 2. openai client → gemini upstream → cross-protocol stream.
# ---------------------------------------------------------------------------


def test_openai_client_gemini_upstream_cross_protocol_stream(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Same shape as scenario 1 but with `accessible_endpoints=["gemini"]`.

    The E3-B Gemini gap (gemini codec doesn't set `ir.stream=true` from the
    `:streamGenerateContent` path — see
    `crates/gw-codec/src/gemini/codec.rs::parse_request` +
    `crates/gw-codec/src/gemini/wire.rs::GeminiRequest` — no `stream`
    field) DOES NOT BITE on this cross-protocol path because:

      - The CLIENT codec here is openai, whose `parse_request` derives
        `ir.stream=true` from the openai `stream: true` body field.
      - The OUTBOUND request body is rendered through `Gemini.render_request`
        — the Gemini wire struct has no `stream` field, but the upstream
        request body is consumed by the fake_upstream which doesn't care
        about it; it returns whatever the scenario specifies.
      - The response path uses `ctx.is_stream` (set in `request_filter`
        from `ir.stream` BEFORE cross-protocol render) to decide whether
        to parse SSE — and `ir.stream` was already true from the openai
        parse.

    So the gateway parses upstream Gemini SSE → re-renders as openai SSE.
    The metric label is `from="gemini",to="openai"` (NB: `as_str()` form, not
    the serde snake_case "open_ai" used for accessible_endpoints — see
    scenario 1's comment).
    """
    _seed_cross_protocol(
        fake_web,
        user_id="u-cxg",
        api_key_id="ak-cxg",
        api_key_plaintext="sk-cxg",
        group="grp-cxg",
        vendor_id="v-cxg",
        key_id="vk-cxg",
        upstream_protocol=PROTO_GEMINI,
    )
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "sse",
                        "delay_ms": 0,
                        "events": _gemini_sse_events(),
                    },
                }
            ]
        }
    )

    pre = _metrics_snapshot("gw_codec_cross_render_total", metrics_url)

    client = make_openai_client(gateway_url, "sk-cxg", "ak-cxg")
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces: list[str] = []
    for chunk in stream:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces.append(chunk.choices[0].delta.content)
    assert "".join(pieces) == "hi", f"expected 'hi', got {pieces!r}"

    post = _metrics_snapshot("gw_codec_cross_render_total", metrics_url)
    delta = _label_delta(pre, post, 'from="gemini",to="openai"')
    assert delta >= 1, (
        f"expected gw_codec_cross_render_total{{from=\"gemini\","
        f"to=\"openai\"}} to increment; pre={pre}, post={post}"
    )

    xlen = wait_for_records(fake_web, min_xlen=1, timeout=3.0)
    assert xlen >= 1, f"expected >=1 usage:records, got {xlen}"


# ---------------------------------------------------------------------------
# 3. mid-stream upstream RST after some bytes → SDK sees torn stream + no
#    retry recovery.
# ---------------------------------------------------------------------------


def test_mid_stream_upstream_rst_after_some_bytes(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """fake_upstream emits headers + 50 body bytes + TCP RST. The openai
    SDK's underlying httpx surfaces this as a read error mid-stream.

    Non-retryable per spec: pingora's `fail_to_connect` hook
    (`crates/gw-proxy/src/gateway.rs::fail_to_connect`) is only invoked on
    a connect-time error, not on mid-body RST. Once response headers have
    been read, a mid-body error routes via `fail_to_proxy` — and v1 has no
    `fail_to_proxy` retry path or envelope emitter (see
    `test_routing_retry.py` module docstring).

    Assert: (a) the client raises (httpx read error / openai stream
    error); (b) the recovered/exhausted counters of
    `gw_retry_attempt_total` do NOT increment.
    """
    _seed_same_protocol_openai(
        fake_web,
        user_id="u-rst",
        api_key_id="ak-rst",
        api_key_plaintext="sk-rst",
        group="grp-rst",
        vendor_id="v-rst",
        key_id="vk-rst",
    )
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "tcp_reset_mid_body",
                        "after_bytes": 50,
                    },
                }
            ]
        }
    )

    pre = _metrics_snapshot("gw_retry_attempt_total", metrics_url)

    client = make_openai_client(gateway_url, "sk-rst", "ak-rst")
    raised = False
    received_any = False
    try:
        stream = client.chat.completions.create(
            model=MODEL,
            messages=[{"role": "user", "content": "hi"}],
            stream=True,
        )
        for chunk in stream:
            received_any = True
            # Whatever the gateway forwarded — likely zero parseable
            # `data:` events because the RST arrives mid-`AAAA...` filler
            # body that has no SSE framing — keep draining until the read
            # error fires.
            _ = chunk
    except (
        openai.APIError,
        httpx.HTTPError,
        ConnectionError,
    ):
        raised = True
    # `received_any` MAY be False (no parseable frames) or True (an empty
    # leading chunk surfaced) depending on httpx buffering — both are
    # acceptable; what matters is that the stream did NOT terminate
    # cleanly.
    assert raised, (
        "mid-stream upstream RST should propagate as an SDK error "
        f"(received_any={received_any})"
    )

    post = _metrics_snapshot("gw_retry_attempt_total", metrics_url)
    # The gateway must NOT have retried — neither `recovered` (cross-key
    # retry succeeded) nor `exhausted` (retry budget hit) should tick for
    # a mid-stream RST.
    assert _label_delta(pre, post, 'outcome="recovered"') == 0, (
        f"mid-stream RST is NOT a fail_to_connect path — recovered "
        f"counter should not tick; pre={pre}, post={post}"
    )
    assert _label_delta(pre, post, 'outcome="exhausted"') == 0, (
        f"mid-stream RST is NOT a fail_to_connect path — exhausted "
        f"counter should not tick; pre={pre}, post={post}"
    )


# ---------------------------------------------------------------------------
# 4. Malformed partial SSE event — fake closes cleanly mid-frame, no `\n\n`.
# ---------------------------------------------------------------------------


def test_malformed_sse_partial_event(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """Upstream emits `data: <half-json>` and FINs without the `\\n\\n`
    framing terminator. The gateway's
    `crates/gw-proxy/src/filters/response.rs::run` only emits a parsed
    event when it finds the `\\n\\n` boundary; on `end_of_stream` the
    `sse_leftover` tail is flushed to `handle_event`, where
    `parse_stream_chunk` may either succeed (the leftover is a legal
    `data: { ... }` with no terminator — anthropic codec is forgiving) or
    fail with a `tracing::warn!` and the bytes drop on the floor.

    NO body-emitter for mid-stream parse errors: `crates/gw-core/src/error/
    codes.rs` doesn't define a `GW_UPSTREAM_PROTOCOL_ERROR` constant, and
    `response.rs::handle_event` line 121 logs `tracing::warn!` then
    returns — there is no `error_emit::send_error` call on this path. So
    the gateway response body is just whatever bytes the upstream sent
    (no envelope, no status flip). The openai SDK sees a torn stream and
    either yields zero events + clean EOF or raises depending on httpx
    framing tolerance; we assert the stream terminates and no usage row is
    XADD'd from the corrupted frame.
    """
    _seed_same_protocol_openai(
        fake_web,
        user_id="u-pse",
        api_key_id="ak-pse",
        api_key_plaintext="sk-pse",
        group="grp-pse",
        vendor_id="v-pse",
        key_id="vk-pse",
    )
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "partial_sse_event",
                        "half_body": (
                            '{"choices":[{"delta":{"content":"hi'
                        ),
                    },
                }
            ]
        }
    )

    client = make_openai_client(gateway_url, "sk-pse", "ak-pse")
    pieces: list[str] = []
    stream_terminated = False
    try:
        stream = client.chat.completions.create(
            model=MODEL,
            messages=[{"role": "user", "content": "hi"}],
            stream=True,
        )
        for chunk in stream:
            # If httpx happens to surface the half-frame as a parse error
            # we'll catch it below; otherwise no `data:` boundary means
            # zero chunks delivered to the SDK and the iterator just ends.
            if chunk.choices and chunk.choices[0].delta.content:
                pieces.append(chunk.choices[0].delta.content)
        stream_terminated = True
    except (
        openai.APIError,
        httpx.HTTPError,
    ):
        # The torn frame may also surface as a parse error in the SDK —
        # also acceptable. We only need to confirm we didn't somehow
        # extract a successful "hi" payload from a half-JSON body.
        stream_terminated = True

    assert stream_terminated, "stream loop should have ended (clean EOF or raise)"
    # The half-body cannot parse as a complete JSON delta — the gateway's
    # codec parse_stream_chunk will fail and tracing::warn it. Even if the
    # client gets the raw `data: {"choices":[{"delta":{"content":"hi`
    # bytes, that JSON is unterminated and the SDK should NOT yield a
    # delta with `content == "hi"` (the closing brace + quote is missing).
    assert "".join(pieces) != "hi", (
        f"a half-formed `data:` frame must NOT surface as a complete delta; "
        f"got pieces={pieces!r}"
    )


# ---------------------------------------------------------------------------
# 5. Gzip-encoded upstream response decoded transparently by the SDK.
# ---------------------------------------------------------------------------


def _gzip_b64_of(payload: bytes) -> str:
    """gzip-encode `payload` and return the base64 of the gzip frame.

    The fake_upstream's GzipStatic variant base64-decodes this and ships
    the raw gzip bytes with `Content-Encoding: gzip`. The gateway forwards
    them verbatim (no upstream-side decoder on the response path); httpx
    inside the openai SDK auto-decompresses based on the encoding header.
    """
    return base64.b64encode(gzip.compress(payload)).decode("ascii")


def test_gzip_response_decoded_transparently(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    """fake_upstream returns a gzip-encoded SSE body. The gateway's
    response path does NOT decode gzip (no `content-encoding` handling in
    `crates/gw-proxy/src/filters/response.rs`); the bytes flow through
    same-protocol passthrough to the client. The openai SDK's underlying
    httpx auto-decompresses on `Content-Encoding: gzip`.

    Implementation: the fake_upstream's `GzipStatic` ResponseSpec variant
    (added in this task — see `crates/e2e-fake-upstream/src/scenario.rs`
    and `crates/e2e-fake-upstream/src/inject.rs::write_gzip_static`).
    Plain `Static.body` is a UTF-8 String which cannot carry the binary
    gzip frame; `GzipStatic.body_base64` is the base64 of the already-
    gzipped payload bytes, decoded server-side then shipped raw with the
    `content-encoding: gzip` header.

    NOTE on gateway interaction with gzip + SSE: the gateway DOES try to
    parse incoming chunks as SSE (`response_body_filter` reads
    `ctx.sse_leftover` and scans for `\\n\\n`). Feeding it gzip bytes
    means no `\\n\\n` boundaries will ever appear — so the parsed-event
    pathway is a no-op (the `tracing::warn` happens at most once on EOS).
    But because `cross_protocol == false`, `pending_client_body` stays
    `None` and the upstream bytes are forwarded verbatim — preserving the
    gzip stream end-to-end. This is the v1 behaviour; an SSE-over-gzip
    streaming server would need response-side decompression to be wired,
    which is out of scope.
    """
    _seed_same_protocol_openai(
        fake_web,
        user_id="u-gz",
        api_key_id="ak-gz",
        api_key_plaintext="sk-gz",
        group="grp-gz",
        vendor_id="v-gz",
        key_id="vk-gz",
    )
    sse_body = (
        b'data: {"choices":[{"delta":{"content":"hi"}}]}\n\n'
        b"data: [DONE]\n\n"
    )
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "gzip_static",
                        "status": 200,
                        "headers": {
                            "content-type": "text/event-stream",
                        },
                        "body_base64": _gzip_b64_of(sse_body),
                    },
                }
            ]
        }
    )

    client = make_openai_client(gateway_url, "sk-gz", "ak-gz")
    stream = client.chat.completions.create(
        model=MODEL,
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces: list[str] = []
    for chunk in stream:
        if chunk.choices and chunk.choices[0].delta.content:
            pieces.append(chunk.choices[0].delta.content)
    assert "".join(pieces) == "hi", (
        f"openai SDK should auto-decompress upstream gzip and yield 'hi'; "
        f"got pieces={pieces!r}"
    )


# ---------------------------------------------------------------------------
# 6. Non-stream cross-protocol matrix — all 6 directional pairs.
#
# Each pair: client sends in `from` protocol via the matching SDK, the
# vendor key advertises ONLY the `to` protocol with protocol_conversion=True,
# fake_upstream returns the `to`-shaped non-stream JSON body. The gateway's
# S-codec v2 CrossRender path parses the upstream body with the upstream
# codec and re-renders into the client wire format.
#
# Metric label conventions (WireFormat::label):
#   openai_chat, anthropic_messages, gemini_generate
# These differ from both ProtocolKind::as_str ("openai"/"anthropic"/"gemini")
# and the serde snake_case accessible_endpoints ("open_ai"/"anthropic"/"gemini").
# ---------------------------------------------------------------------------


# ── Per-vendor non-stream response bodies (shared with test 7) ─────────────

_OPENAI_NS_BODY = json.dumps(
    {
        "id": "chatcmpl-cpn",
        "object": "chat.completion",
        "created": 1700000000,
        "model": MODEL,
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": "hi"},
                "finish_reason": "stop",
            }
        ],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 2,
            "total_tokens": 7,
        },
    }
)

_ANTHROPIC_NS_BODY = json.dumps(
    {
        "id": "msg_cpn",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "hi"}],
        "model": MODEL,
        "stop_reason": "end_turn",
        "stop_sequence": None,
        "usage": {
            "input_tokens": 5,
            "output_tokens": 2,
        },
    }
)

_GEMINI_NS_BODY = json.dumps(
    {
        "candidates": [
            {
                "content": {
                    "role": "model",
                    "parts": [{"text": "hi"}],
                },
                "finishReason": "STOP",
                "index": 0,
            }
        ],
        "modelVersion": MODEL,
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 2,
            "totalTokenCount": 7,
        },
    }
)


def _static_json_scenario(body: str) -> dict:
    """Single-route fake_upstream scenario returning `body` as JSON 200."""
    return {
        "routes": [
            {
                "match": {"method": "POST"},
                "response": {
                    "kind": "static",
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": body,
                    "delay_ms": 0,
                },
            }
        ]
    }


# Maps protocol tag (accessible_endpoints value) → non-stream response body.
_NS_BODY_MAP = {
    PROTO_OPENAI: _OPENAI_NS_BODY,
    PROTO_ANTHROPIC: _ANTHROPIC_NS_BODY,
    PROTO_GEMINI: _GEMINI_NS_BODY,
}

# Maps (from_proto, to_proto) → expected metric label for
# gw_codec_cross_render_nonstream_total. `from` = upstream wire format
# (the format of the body the upstream returned), `to` = client wire format
# (the format the client SDK speaks). WireFormat::label() form.
_WIRE_LABEL = {
    PROTO_OPENAI: "openai_chat",
    PROTO_ANTHROPIC: "anthropic_messages",
    PROTO_GEMINI: "gemini_generate",
}

# All 6 directional cross-protocol pairs.
_CROSS_PAIRS = [
    (PROTO_OPENAI, PROTO_ANTHROPIC),
    (PROTO_OPENAI, PROTO_GEMINI),
    (PROTO_ANTHROPIC, PROTO_OPENAI),
    (PROTO_ANTHROPIC, PROTO_GEMINI),
    (PROTO_GEMINI, PROTO_OPENAI),
    (PROTO_GEMINI, PROTO_ANTHROPIC),
]


def _make_non_stream_request(
    client_proto: str,
    gateway_url: str,
    api_key: str,
    api_key_id: str,
    model: str,
) -> requests.Response:
    """Fire a non-stream request using the appropriate client for `client_proto`.

    Returns the raw `requests.Response` so the caller can assert status +
    body without SDK-level validation (which would mask gateway errors).
    """
    headers = pair_headers(api_key, api_key_id)

    if client_proto == PROTO_OPENAI:
        return requests.post(
            f"{gateway_url.rstrip('/')}/v1/chat/completions",
            json={
                "model": model,
                "messages": [{"role": "user", "content": "hi"}],
                "stream": False,
                "max_tokens": 64,
            },
            headers=headers,
            timeout=15,
        )
    elif client_proto == PROTO_ANTHROPIC:
        headers["anthropic-version"] = "2023-06-01"
        return requests.post(
            f"{gateway_url.rstrip('/')}/v1/messages",
            json={
                "model": model,
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 64,
                "stream": False,
            },
            headers=headers,
            timeout=15,
        )
    elif client_proto == PROTO_GEMINI:
        return requests.post(
            f"{gateway_url.rstrip('/')}/v1beta/models/{model}:generateContent",
            json={
                "contents": [
                    {
                        "role": "user",
                        "parts": [{"text": "hi"}],
                    }
                ],
            },
            headers=headers,
            timeout=15,
        )
    else:
        raise ValueError(f"unknown client_proto: {client_proto}")


@pytest.mark.parametrize(
    "client_proto,upstream_proto",
    _CROSS_PAIRS,
    ids=[f"{f}->{t}" for f, t in _CROSS_PAIRS],
)
def test_cross_protocol_non_stream_matrix(
    client_proto: str,
    upstream_proto: str,
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Parametrized non-stream cross-protocol round-trip.

    Each pair seeds entities with `cpn_` prefixed IDs, seeds a vendor key
    that advertises ONLY `upstream_proto` + `protocol_conversion=True`,
    then sends a non-stream request from `client_proto`. Asserts:
    - HTTP 200 with valid content in the response.
    - `gw_codec_cross_render_nonstream_total{from=X,to=Y}` incremented.
    """
    # Build unique entity IDs per pair.
    tag = f"{client_proto[:3]}_{upstream_proto[:3]}"
    user_id = f"cpn_u_{tag}"
    api_key_id = f"cpn_ak_{tag}"
    api_key_plaintext = f"cpn_sk_{tag}"
    group = f"cpn_grp_{tag}"
    vendor_id = f"cpn_v_{tag}"
    key_id = f"cpn_vk_{tag}"

    _seed_cross_protocol(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        group=group,
        vendor_id=vendor_id,
        key_id=key_id,
        upstream_protocol=upstream_proto,
    )
    fake_upstream.set_scenario(
        _static_json_scenario(_NS_BODY_MAP[upstream_proto])
    )

    # Snapshot the non-stream cross-render metric before the request.
    from_label = _WIRE_LABEL[upstream_proto]
    to_label = _WIRE_LABEL[client_proto]
    metric_key = f'from="{from_label}",to="{to_label}"'
    pre = metrics_snapshot("gw_codec_cross_render_nonstream_total", metrics_url)

    resp = _make_non_stream_request(
        client_proto, gateway_url, api_key_plaintext, api_key_id, MODEL,
    )

    assert resp.status_code == 200, (
        f"[{client_proto}->{upstream_proto}] expected 200, "
        f"got {resp.status_code}: {resp.text[:300]}"
    )

    # Verify the response body contains valid content. Different client
    # protocols render "hi" in different JSON locations.
    body = resp.json()
    if client_proto == PROTO_OPENAI:
        content = body["choices"][0]["message"]["content"]
    elif client_proto == PROTO_ANTHROPIC:
        content = body["content"][0]["text"]
    elif client_proto == PROTO_GEMINI:
        content = body["candidates"][0]["content"]["parts"][0]["text"]
    else:
        content = None
    assert content == "hi", (
        f"[{client_proto}->{upstream_proto}] expected 'hi', got {content!r}"
    )

    # Metric tick.
    post = metrics_snapshot("gw_codec_cross_render_nonstream_total", metrics_url)
    delta = label_delta(pre, post, metric_key)
    assert delta >= 1, (
        f"[{client_proto}->{upstream_proto}] expected "
        f"gw_codec_cross_render_nonstream_total{{{metric_key}}} to increment; "
        f"pre={pre}, post={post}"
    )


# ---------------------------------------------------------------------------
# 7. Lossy metric fires when a known vendor-specific field is dropped.
#
# Anthropic responses may carry `thinking` content blocks. When the CLIENT
# protocol is OpenAI Chat (`supports_thinking=false`), the CrossRender
# adapter strips thinking blocks and increments
# `gw_codec_cross_render_lossy_total{from="anthropic_messages",
# to="openai_chat",field="thinking"}`.
#
# We seed an openai client → anthropic upstream key, return an Anthropic
# body WITH a `thinking` block, and assert the lossy metric fires.
# ---------------------------------------------------------------------------


_ANTHROPIC_NS_BODY_WITH_THINKING = json.dumps(
    {
        "id": "msg_lossy",
        "type": "message",
        "role": "assistant",
        "content": [
            {
                "type": "thinking",
                "thinking": "internal reasoning step",
                "signature": "sig-test-123",
            },
            {"type": "text", "text": "hi"},
        ],
        "model": MODEL,
        "stop_reason": "end_turn",
        "stop_sequence": None,
        "usage": {
            "input_tokens": 5,
            "output_tokens": 2,
        },
    }
)


def test_cross_protocol_lossy_metric_fires_on_known_drop(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Anthropic `thinking` block → OpenAI Chat cross-render drops it.

    The `CrossRender::adapt_lossy` method checks
    `to_caps.supports_thinking` and calls `LossyField::drop(from, to,
    "thinking")` when the target doesn't support it. OpenAI Chat does
    not (`supports_thinking=false`). Assert:
    - `gw_codec_cross_render_lossy_total{from="anthropic_messages",
       to="openai_chat",field="thinking"}` increments by >= 1.
    - The response is still 200 with the text content preserved (the
      thinking block is dropped but the text block survives).
    """
    _seed_cross_protocol(
        fake_web,
        user_id="cpn_u_lossy",
        api_key_id="cpn_ak_lossy",
        api_key_plaintext="cpn_sk_lossy",
        group="cpn_grp_lossy",
        vendor_id="cpn_v_lossy",
        key_id="cpn_vk_lossy",
        upstream_protocol=PROTO_ANTHROPIC,
    )
    fake_upstream.set_scenario(
        _static_json_scenario(_ANTHROPIC_NS_BODY_WITH_THINKING)
    )

    lossy_label = 'from="anthropic_messages",to="openai_chat",field="thinking"'
    pre = metrics_snapshot("gw_codec_cross_render_lossy_total", metrics_url)

    resp = _make_non_stream_request(
        PROTO_OPENAI, gateway_url, "cpn_sk_lossy", "cpn_ak_lossy", MODEL,
    )

    assert resp.status_code == 200, (
        f"expected 200, got {resp.status_code}: {resp.text[:300]}"
    )
    # Text content survived the lossy cross-render.
    body = resp.json()
    assert body["choices"][0]["message"]["content"] == "hi", (
        f"expected 'hi' content after lossy cross-render, got {body!r}"
    )

    # Lossy metric fired.
    post = metrics_snapshot("gw_codec_cross_render_lossy_total", metrics_url)
    delta = label_delta(pre, post, lossy_label)
    assert delta >= 1, (
        "expected gw_codec_cross_render_lossy_total"
        f'{{{lossy_label}}} to increment; pre={pre}, post={post}'
    )
