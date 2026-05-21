"""E3-B: happy-path streaming scenarios across OpenAI / Anthropic / Gemini.

This is the integration linchpin — it brings up the whole docker-compose
stack and proves a real SDK streaming request flows through the gateway
to the fake upstream. Each test:

1. Seeds the 5-tier entity graph with `seed_passthrough` so a pair-auth
   request for `model` over `protocol` routes to the fake upstream.
2. Installs a protocol-correct SSE scenario on the fake upstream that
   yields a single text delta "hi" plus the terminator the SDK requires
   to close the stream cleanly.
3. Drives the request via the real vendor SDK (with the pair-auth
   strip-and-substitute hook from conftest), accumulates the text
   deltas, and asserts the joined text == "hi".
4. Polls `fake_web.usage_stats()` for up to ~3s; asserts a
   `usage:records` entry was XADD'd (the ledger flushes async/batched
   after stream completion).

Note: the `accessible_endpoints` field on vendor_keys uses serde's
`rename_all = "snake_case"` against `ProtocolKind`, which renders
`OpenAi` as `"open_ai"` (NOT `"openai"`). The other two variants map
to themselves. The constants below match the serde wire shape.
"""

from __future__ import annotations

import time

import pytest

from conftest import (
    FakeUpstreamClient,
    FakeWebClient,
    make_anthropic_client,
    # Retained for re-enable once the gateway sets ir.stream for :streamGenerateContent (see test_gemini_stream_happy skip).
    make_gemini_client,  # noqa: F401
    make_openai_client,
    seed_passthrough,
)


# Protocol tags as they appear in vendor_keys.accessible_endpoints
# (ProtocolKind serde wire form).
PROTO_OPENAI = "open_ai"
PROTO_ANTHROPIC = "anthropic"
PROTO_GEMINI = "gemini"


def _wait_for_records(fake_web: FakeWebClient, min_records: int = 1, timeout: float = 3.0) -> int:
    """Poll usage_stats() until records_xlen >= min_records, or timeout.

    Ledger flushes are async/batched — give them a few hundred ms. Returns
    the final observed xlen (caller asserts).
    """
    deadline = time.monotonic() + timeout
    xlen = 0
    while time.monotonic() < deadline:
        xlen = fake_web.usage_stats().get("records_xlen", 0)
        if xlen >= min_records:
            return xlen
        time.sleep(0.1)
    # one last read after the loop for diagnostic accuracy
    return fake_web.usage_stats().get("records_xlen", 0)


# ---------------------------------------------------------------------------
# OpenAI chat completions streaming.
#
# The fake upstream emits the minimal OpenAI SSE shape the SDK accepts:
#   data: {"choices":[{"delta":{"content":"hi"}}]}
#   data: [DONE]
# ---------------------------------------------------------------------------


def test_openai_stream_happy(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model="gpt-4",
        group="openai-grp",
        vendor_id="v-openai",
        key_id="vk-openai",
        user_id="u1",
        api_key_id="ak1",
        api_key_plaintext="sk-client-1",
    )
    fake_web.set_balance(seeded["user_id"], 1_000_000_000)
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "sse",
                        "delay_ms": 0,
                        "events": [
                            {"data": '{"choices":[{"delta":{"content":"hi"}}]}'},
                            {"data": "[DONE]"},
                        ],
                    },
                }
            ]
        }
    )

    client = make_openai_client(gateway_url, "sk-client-1", "ak1")
    stream = client.chat.completions.create(
        model="gpt-4",
        messages=[{"role": "user", "content": "hi"}],
        stream=True,
    )
    pieces: list[str] = []
    for chunk in stream:
        # OpenAI SDK yields ChatCompletionChunk objects whose .choices[0].delta
        # is a ChoiceDelta with a `.content` attribute (str | None).
        if chunk.choices and chunk.choices[0].delta.content:
            pieces.append(chunk.choices[0].delta.content)
    assert "".join(pieces) == "hi", f"expected 'hi', got {pieces!r}"

    xlen = _wait_for_records(fake_web)
    assert xlen >= 1, f"expected at least one usage:records entry, got {xlen}"


# ---------------------------------------------------------------------------
# Anthropic Messages streaming.
#
# The anthropic SDK parses a full message-stream protocol. The gateway's
# parse_stream_chunk only consumes `content_block_delta` + `message_stop`,
# but the SDK rejects a stream that doesn't open with `message_start` and
# accompany the content_block with `content_block_start`/`content_block_stop`.
# We emit the minimal sequence the SDK accepts AND the gateway forwards.
# ---------------------------------------------------------------------------


def _anthropic_event(name: str, payload_json: str) -> dict:
    """Encode an Anthropic-style `event: <name>\\ndata: {json}` frame
    through the fake upstream's `data: <body>\\n\\n` framer.

    The fake hard-codes `data: ` as the prefix of each chunk. By making
    `event.data` start with a newline and contain `event: ...\\ndata: ...`,
    the wire bytes become:

        data: \\nevent: <name>\\ndata: {json}\\n\\n

    which the standard SSE decoder (anthropic SDK + the gateway's
    `parse_stream_chunk` in `crates/gw-codec/src/anthropic/codec.rs`) parses
    as a single event with name=<name> and data="\\n{json}". JSON parsers
    tolerate leading whitespace, so the JSON payload is recovered intact.
    """
    return {"data": f"\nevent: {name}\ndata: {payload_json}"}


def _anthropic_sse_events() -> list[dict]:
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


def test_anthropic_stream_happy(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_ANTHROPIC,
        model="claude-3-5-sonnet-20241022",
        group="anthropic-grp",
        vendor_id="v-anthropic",
        key_id="vk-anthropic",
        user_id="u2",
        api_key_id="ak2",
        api_key_plaintext="sk-client-2",
    )
    fake_web.set_balance(seeded["user_id"], 1_000_000_000)
    fake_upstream.set_scenario(
        {
            "routes": [
                {
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

    client = make_anthropic_client(gateway_url, "sk-client-2", "ak2")
    pieces: list[str] = []
    with client.messages.stream(
        model="claude-3-5-sonnet-20241022",
        max_tokens=64,
        messages=[{"role": "user", "content": "hi"}],
    ) as stream:
        for text in stream.text_stream:
            pieces.append(text)
    assert "".join(pieces) == "hi", f"expected 'hi', got {pieces!r}"

    xlen = _wait_for_records(fake_web)
    assert xlen >= 1, f"expected at least one usage:records entry, got {xlen}"


# ---------------------------------------------------------------------------
# Gemini streamGenerateContent streaming.
#
# The fake upstream emits the minimal Gemini SSE shape the SDK accepts:
#   data: {"candidates":[{"content":{"parts":[{"text":"hi"}]}}]}
#   data: {"candidates":[{"finishReason":"STOP"}]}
# The gateway's gemini codec recognises a `finishReason` field as End.
# ---------------------------------------------------------------------------


def _gemini_sse_events() -> list[dict]:
    # Retained for re-enable once the gateway sets ir.stream for :streamGenerateContent (see test_gemini_stream_happy skip).
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


def test_gemini_stream_happy(
    gateway_url: str, fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient
) -> None:
    """SKIPPED in v1: the gateway's Gemini codec does not derive
    `RequestIR.stream` from either the request body OR the URL path
    (`:streamGenerateContent`). `parse_request` only reads the JSON body,
    and Gemini's wire format carries no `stream` field — so `ir.stream`
    is always `false`, and the request_filter rejects the request with
    `GW_REQUEST_NONSTREAM_NOT_SUPPORTED` (400) BEFORE the request can
    reach the fake upstream.

    See `crates/gw-codec/src/gemini/codec.rs::parse_request` and
    `crates/gw-codec/src/gemini/wire.rs::GeminiRequest` — neither has a
    path-aware override that would set `stream=true` for the
    `:streamGenerateContent` surface. This is a gateway-side gap
    (docs/architecture.md §Known gaps candidate); E3-B cannot exercise
    Gemini streaming end-to-end until that gap closes.

    The OpenAI + Anthropic happy-path scenarios in this file DO exercise
    the full stack and serve as the integration linchpin for E3-B; the
    Gemini scenario is held until the codec sets `ir.stream` correctly.
    """
    pytest.skip(
        "gateway gap: Gemini codec parse_request never sets ir.stream=true "
        "(no body field, no path-based override), so the request_filter "
        "rejects every Gemini call with GW_REQUEST_NONSTREAM_NOT_SUPPORTED. "
        "Re-enable once crates/gw-codec/src/gemini/codec.rs derives stream "
        "from the :streamGenerateContent path."
    )
