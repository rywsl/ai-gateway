"""T4-A (B1) docker e2e: same-protocol non-stream + cross-protocol 503.

Validates the full B1 deliverable end-to-end through the docker stack:

- `codec.parse_response` extracts `usage` from each vendor's non-stream
  response body (T1-A/B/C unlocked the three codec implementations).
- `response_body_filter::handle_non_stream_chunk` accumulates the upstream
  body chunks and invokes `parse_response` on EOS, populating `ctx.usage`
  (T2-B in `crates/gw-proxy/src/filters/response.rs`).
- `request_filter` no longer short-circuits non-stream requests (T2-A
  removed the `is_stream == false` guard).
- The ledger emits a `usage:records` entry with `is_stream=false` and
  `usage_source="upstream"` (the freeze-time `Estimated` fallback only
  kicks in when `output_tokens == 0`).
- Cross-protocol + non-stream short-circuits with `503` + the
  `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` envelope at the
  tail of `pick_peer` (ADR-B1-4 — `crates/gw-proxy/src/filters/upstream.rs`
  lines ~230-254).

----------------------------------------------------------------------------
Harness notes — deviations from the original T4-A plan snippet
----------------------------------------------------------------------------
The plan's draft referenced two fixtures (`seeded_keys`,
`usage_records_consumer`) and a `crates/e2e-fake-upstream/scenarios/*.toml`
file layout that DO NOT exist in this harness. The existing convention
(see `test_happy_streaming.py`, `test_cross_protocol.py`) is:

1. Per-test entity seeding via `seed_passthrough(...)` (NOT a global
   `seeded_keys` dict).
2. Per-test fake_upstream scenario POSTed at runtime via
   `fake_upstream.set_scenario(...)` (NOT TOML files — `crates/e2e-fake-upstream`
   has no `scenarios/` directory; the `Scenario` DSL is JSON, deserialised
   in `crates/e2e-fake-upstream/src/scenario.rs`).
3. Ledger observation either via the count-only `fake_web.usage_stats()`
   or — when payload assertions are needed — via the `usage_records_consumer`
   fixture this commit ADDS to `conftest.py`, which talks to Redis
   directly. Redis is now exposed on the host at 6379 (docker-compose
   publish) so the host pytest process can read `usage:records`.

If the harness conventions ever drift, the failure surfaces here will
point at the offending fixture, not at the gateway under test.
"""

from __future__ import annotations

import json

import pytest

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
    make_anthropic_client,
    make_gemini_client,
    make_openai_client,
    seed_model_group,
    seed_passthrough,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
    wait_for_records,
)


# Protocol wire tags as they appear in `vendor_keys.accessible_endpoints`
# (ProtocolKind serde rename_all="snake_case"). `PROTO_OPENAI` is exported
# from conftest (`"open_ai"`); the other two only appear in this file.
PROTO_ANTHROPIC = "anthropic"
PROTO_GEMINI = "gemini"


# ---------------------------------------------------------------------------
# Per-vendor non-stream response bodies.
#
# These mirror the minimum shape each vendor codec's `parse_response`
# accepts AND the minimum shape each SDK's response model accepts. The
# numeric values are arbitrary but non-zero so the
# `usage.output_tokens > 0` assertions stay green.
#
# The shapes match the plan's TOML snippets (`non_stream_*_happy.toml`)
# but live inline because the harness's fake_upstream is driven by
# runtime JSON, not on-disk TOML — see the module docstring.
# ---------------------------------------------------------------------------


_OPENAI_NON_STREAM_BODY = json.dumps(
    {
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "gpt-4-turbo",
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
            "prompt_tokens_details": {"cached_tokens": 1},
        },
    }
)

_ANTHROPIC_NON_STREAM_BODY = json.dumps(
    {
        "id": "msg_fake",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "hi"}],
        "model": "claude-3-5-sonnet-20241022",
        "stop_reason": "end_turn",
        "stop_sequence": None,
        "usage": {
            "input_tokens": 5,
            "output_tokens": 2,
            "cache_read_input_tokens": 1,
        },
    }
)

_GEMINI_NON_STREAM_BODY = json.dumps(
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
        "modelVersion": "gemini-1.5-pro",
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 2,
            "totalTokenCount": 7,
            "cachedContentTokenCount": 1,
        },
    }
)


def _static_json_scenario(body: str) -> dict:
    """Build a single-route fake_upstream scenario returning `body` as
    application/json with status 200 — the `Static` ResponseSpec variant
    in `crates/e2e-fake-upstream/src/scenario.rs`."""
    return {
        "routes": [
            {
                # Method-only match (no path constraint) — same pattern the
                # cross-protocol streaming scenarios use, so the upstream
                # accepts both `/v1/chat/completions` (openai), `/v1/messages`
                # (anthropic) and `/v1beta/models/.../generateContent` (gemini)
                # uniformly.
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


# ---------------------------------------------------------------------------
# Same-protocol non-stream happy paths (one per vendor SDK).
#
# Each test:
# 1. Seeds the 5-tier entity graph via `seed_passthrough(...)` (matching
#    `test_happy_streaming.py`).
# 2. Installs a `Static` fake_upstream route that returns the vendor's
#    canonical non-stream JSON body.
# 3. Drives a `stream=False` SDK call, asserts the SDK decoded a non-zero
#    completion-token count.
# 4. Asserts a `usage:records` entry was XADD'd; if `usage_records_consumer`
#    is available (Redis exposed), additionally pops the record and asserts
#    `is_stream=false`, `usage.output_tokens > 0`, `usage_source="upstream"`.
# ---------------------------------------------------------------------------


def _assert_record_shape(record: dict) -> None:
    """Shared post-conditions on the popped `usage:records` payload."""
    assert record["is_stream"] is False, f"expected is_stream=false, got {record!r}"
    assert record["usage"]["output_tokens"] > 0, (
        f"expected non-zero output_tokens, got {record['usage']!r}"
    )
    assert record["usage_source"] == "upstream", (
        f"expected usage_source=upstream, got {record['usage_source']!r}"
    )


def test_openai_non_stream_returns_usage(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    usage_records_consumer,
) -> None:
    """OpenAI chat.completions.create(stream=False) — B1 happy path."""
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_OPENAI,
        model="gpt-4-turbo",
        group="openai-ns-grp",
        vendor_id="v-openai-ns",
        key_id="vk-openai-ns",
        user_id="u-ns-openai",
        api_key_id="ak-ns-openai",
        api_key_plaintext="sk-ns-openai",
    )
    fake_web.set_balance(seeded["user_id"], BALANCE_LARGE)
    fake_upstream.set_scenario(_static_json_scenario(_OPENAI_NON_STREAM_BODY))

    client = make_openai_client(gateway_url, "sk-ns-openai", "ak-ns-openai")
    resp = client.chat.completions.create(
        model="gpt-4-turbo",
        messages=[{"role": "user", "content": "hi"}],
        stream=False,
    )
    assert resp.choices[0].message.content == "hi"
    assert resp.usage.completion_tokens > 0

    xlen = wait_for_records(fake_web, min_xlen=1)
    assert xlen >= 1, f"expected >=1 usage:records entry, got {xlen}"
    record = usage_records_consumer.next_record(timeout=5.0)
    _assert_record_shape(record)


def test_anthropic_non_stream_returns_usage(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    usage_records_consumer,
) -> None:
    """Anthropic messages.create(stream=False) — B1 happy path."""
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_ANTHROPIC,
        model="claude-3-5-sonnet-20241022",
        group="anthropic-ns-grp",
        vendor_id="v-anthropic-ns",
        key_id="vk-anthropic-ns",
        user_id="u-ns-anthropic",
        api_key_id="ak-ns-anthropic",
        api_key_plaintext="sk-ns-anthropic",
    )
    fake_web.set_balance(seeded["user_id"], BALANCE_LARGE)
    fake_upstream.set_scenario(_static_json_scenario(_ANTHROPIC_NON_STREAM_BODY))

    client = make_anthropic_client(gateway_url, "sk-ns-anthropic", "ak-ns-anthropic")
    resp = client.messages.create(
        model="claude-3-5-sonnet-20241022",
        max_tokens=10,
        messages=[{"role": "user", "content": "hi"}],
        stream=False,
    )
    assert resp.usage.output_tokens > 0
    # Anthropic SDK exposes content as a list of TextBlock; assert the
    # joined text is "hi" — same shape the upstream body promises.
    assert any(
        getattr(block, "text", "") == "hi" for block in resp.content
    ), f"expected 'hi' content block, got {resp.content!r}"

    xlen = wait_for_records(fake_web, min_xlen=1)
    assert xlen >= 1, f"expected >=1 usage:records entry, got {xlen}"
    record = usage_records_consumer.next_record(timeout=5.0)
    _assert_record_shape(record)


def test_gemini_non_stream_returns_usage(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    usage_records_consumer,
) -> None:
    """Gemini generate_content (non-stream) — B1 happy path."""
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_GEMINI,
        model="gemini-1.5-pro",
        group="gemini-ns-grp",
        vendor_id="v-gemini-ns",
        key_id="vk-gemini-ns",
        user_id="u-ns-gemini",
        api_key_id="ak-ns-gemini",
        api_key_plaintext="sk-ns-gemini",
    )
    fake_web.set_balance(seeded["user_id"], BALANCE_LARGE)
    fake_upstream.set_scenario(_static_json_scenario(_GEMINI_NON_STREAM_BODY))

    client = make_gemini_client(gateway_url, "sk-ns-gemini", "ak-ns-gemini")
    resp = client.models.generate_content(
        model="gemini-1.5-pro",
        contents="hi",
    )
    # google-genai exposes the joined text directly + the usage metadata
    # on the response object. Both must be non-empty.
    assert resp.text and "hi" in resp.text, f"expected 'hi' in text, got {resp.text!r}"
    assert resp.usage_metadata.candidates_token_count > 0

    xlen = wait_for_records(fake_web, min_xlen=1)
    assert xlen >= 1, f"expected >=1 usage:records entry, got {xlen}"
    record = usage_records_consumer.next_record(timeout=5.0)
    _assert_record_shape(record)


# ---------------------------------------------------------------------------
# Cross-protocol + non-stream → 503 fast-fail (ADR-B1-4).
#
# Seed an openai client on top of an anthropic-only vendor key
# (`accessible_endpoints=["anthropic"]`, `protocol_conversion=True`), then
# fire a non-stream request. The candidate filter accepts the key (protocol
# conversion makes it selectable), `pick_peer` then notices
# `upstream_kind=anthropic != client_kind=openai` AND `!ctx.is_stream`,
# emits the `GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED` envelope
# with status 503, and returns. No upstream traffic flows. The OpenAI SDK
# raises an APIStatusError on 503.
# ---------------------------------------------------------------------------


def test_cross_protocol_non_stream_returns_503(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
) -> None:
    user_id = "u-ns-cross"
    api_key_id = "ak-ns-cross"
    api_key_plaintext = "sk-ns-cross"
    group = "grp-ns-cross"
    vendor_id = "v-ns-cross"
    key_id = "vk-ns-cross"
    model = "claude-3-5-sonnet-20241022"

    seed_user_apikey(
        fake_web,
        user_id=user_id,
        api_key_id=api_key_id,
        api_key_plaintext=api_key_plaintext,
        allowed_groups=[group],
    )
    fake_web.set_balance(user_id, BALANCE_LARGE)
    seed_vendor(fake_web, vendor_id=vendor_id)
    # accessible_endpoints=["anthropic"] + protocol_conversion=True makes
    # the key picky'able for an openai client (see
    # `crates/gw-routing/src/candidates.rs::candidate_matches`).
    seed_vendor_key(
        fake_web,
        key_id=key_id,
        vendor_id=vendor_id,
        model=model,
        accessible_endpoints=[PROTO_ANTHROPIC],
        protocol_conversion=True,
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=[key_id],
        strategy="fixed_order",
    )
    seed_pricing(fake_web, model=model)

    # Install a scenario that 500s if accidentally hit, so a regression
    # (request reaching the upstream) is caught loudly instead of silently
    # exercising the cross-protocol non-stream branch.
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST"},
                    "response": {
                        "kind": "static",
                        "status": 500,
                        "headers": {"content-type": "application/json"},
                        "body": '{"error":"should not have been called"}',
                        "delay_ms": 0,
                    },
                }
            ]
        }
    )

    # Import lazily so test collection works without the SDK installed.
    import openai

    client = make_openai_client(gateway_url, api_key_plaintext, api_key_id)
    with pytest.raises(openai.APIStatusError) as exc_info:
        client.chat.completions.create(
            model=model,
            messages=[{"role": "user", "content": "hi"}],
            stream=False,
        )
    assert exc_info.value.status_code == 503, (
        f"expected 503, got {exc_info.value.status_code}: {exc_info.value!r}"
    )
    # OpenAI-style error envelope (rendered by gw-codec/openai render_error).
    # Body shape: `{"error":{"type":..., "message":..., "code":"GW_..."}}`.
    body = exc_info.value.response.json() if exc_info.value.response else {}
    err = body.get("error") if isinstance(body, dict) else None
    code = err.get("code") if isinstance(err, dict) else None
    assert code == "GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED", (
        f"expected GW_REQUEST_CROSS_PROTOCOL_NONSTREAM_NOT_SUPPORTED, got {code!r} "
        f"(body={body!r})"
    )

    # And no `usage:records` should be written — the request never made it
    # to logging_filter (the envelope was emitted by `pick_peer` itself).
    xlen = fake_web.usage_stats().get("records_xlen", 0)
    assert xlen == 0, f"expected no usage:records on 503, got {xlen}"
