"""T4-B (G5) docker e2e: `UsageRecord.operation_id` flows from Gemini
long-running-operation responses through to the `usage:records` stream.

Validates the G5 deliverable end-to-end against a live gateway brought up
by `make e2e-up`:

- Gemini `parse_response` (`crates/gw-codec/src/gemini/codec.rs`) extracts
  the top-level `name` field into `ResponseIR.id` (T1-B).
- `response_body_filter::handle_non_stream_chunk` (T1-B) promotes
  `ResponseIR.id` to `ctx.operation_id` ONLY when it starts with
  `"operations/"`. Normal `:generateContent` responses have no `name`
  and `ctx.operation_id` stays `None`.
- The logging filter (`crates/gw-proxy/src/filters/logging.rs`) writes
  `ctx.operation_id` into `UsageRecord.operation_id` (T1-B / T0). The
  field is `Option<String>` with `#[serde(skip_serializing_if =
  "Option::is_none")]`, so the absent case is encoded as a missing JSON
  key (not `null`).

----------------------------------------------------------------------------
Harness notes — deviations from the original T4-B plan snippet
----------------------------------------------------------------------------
* The plan referenced an HTTP path of `:predictLongRunning` and a
  TOML-file fake-upstream layout. NEITHER applies here:
  - The Gemini codec only routes paths matching
    `:generateContent` / `:streamGenerateContent`
    (see `Gemini::detect_request`). `:predictLongRunning` is NOT
    accepted by the v1 codec, so a request to that path 404s at the
    request filter before reaching `parse_response`.
  - fake-upstream uses runtime JSON scenarios via
    `FakeUpstreamClient.set_scenario`, not TOML files.
* Therefore the LRO-response test posts to `:generateContent` with a
  fake-upstream body whose top-level `name` starts with `operations/`.
  The codec doesn't care about the request path matching LRO semantics
  — it only inspects the response body. This isolates the G5 wiring
  from the request-routing surface.
* The "normal chat → null operation_id" test uses the canonical
  `:generateContent` body (no `name` field). Both tests assert directly
  against the `usage:records` payload through `usage_records_consumer`.
"""
from __future__ import annotations

import json

from conftest import (
    BALANCE_LARGE,
    FakeUpstreamClient,
    FakeWebClient,
    make_gemini_client,
    seed_passthrough,
    wait_for_records,
)


PROTO_GEMINI = "gemini"
GEMINI_MODEL = "gemini-1.5-pro"


# ---------------------------------------------------------------------------
# Per-test response bodies.
#
# Both bodies must deserialise cleanly into `GeminiResponseBody` (defined
# inside `parse_response`). That struct only reads `name`, `modelVersion`,
# and `usageMetadata`; the rest of the payload exists so the google-genai
# SDK (which the gateway hands back the body to verbatim) can construct
# its own `GenerateContentResponse`. `usageMetadata` is required so the
# logging filter has non-zero tokens to record.
# ---------------------------------------------------------------------------


# A top-level `name: "operations/..."` is the surface long-running
# operations expose; G5 promotes any such id into `ctx.operation_id`.
_GEMINI_LRO_BODY = json.dumps(
    {
        "name": "operations/test-op-12345",
        "metadata": {"@type": "VideoGenerationMetadata"},
        "done": False,
        # We append a candidate + usageMetadata so the SDK accepts the body
        # and the logging filter records non-zero tokens — `parse_response`
        # only reads the three fields above but the SDK's response model
        # validates candidate shape downstream.
        "candidates": [
            {
                "content": {
                    "role": "model",
                    "parts": [{"text": "queued"}],
                },
                "finishReason": "STOP",
                "index": 0,
            }
        ],
        "modelVersion": GEMINI_MODEL,
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 2,
            "totalTokenCount": 7,
        },
    }
)


_GEMINI_NORMAL_BODY = json.dumps(
    {
        "candidates": [
            {
                "content": {"role": "model", "parts": [{"text": "hi"}]},
                "finishReason": "STOP",
                "index": 0,
            }
        ],
        "modelVersion": GEMINI_MODEL,
        "usageMetadata": {
            "promptTokenCount": 5,
            "candidatesTokenCount": 2,
            "totalTokenCount": 7,
        },
    }
)


def _static_json_scenario(body: str) -> dict:
    """Single-route fake-upstream scenario returning `body` as
    `application/json` with status 200. Method-only match so both the
    `/v1beta/...` and `/v1/...` Gemini paths share the same route."""
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


# ---------------------------------------------------------------------------
# Scenarios.
# ---------------------------------------------------------------------------


def test_predict_long_running_extracts_operation_id(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    usage_records_consumer,
) -> None:
    """G5 (T1-B): a Gemini response body with top-level
    `name: "operations/..."` lands in `UsageRecord.operation_id`.

    The route is `:generateContent` (the only Gemini path the v1 codec
    routes); the LRO-style response body is what triggers G5. See the
    module docstring for why this isn't `:predictLongRunning`.
    """
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_GEMINI,
        model=GEMINI_MODEL,
        group="gemini-lro-grp",
        vendor_id="v-gemini-lro",
        key_id="vk-gemini-lro",
        user_id="u-gemini-lro",
        api_key_id="ak-gemini-lro",
        api_key_plaintext="sk-gemini-lro",
    )
    fake_web.set_balance(seeded["user_id"], BALANCE_LARGE)
    fake_upstream.set_scenario(_static_json_scenario(_GEMINI_LRO_BODY))

    client = make_gemini_client(gateway_url, "sk-gemini-lro", "ak-gemini-lro")
    # google-genai's `generate_content` POSTs to `:generateContent`.
    resp = client.models.generate_content(
        model=GEMINI_MODEL,
        contents="make video",
    )
    # The SDK doesn't expose `name`, so we only sanity-check that the
    # request succeeded. The real assertion is on the usage record.
    assert resp is not None

    xlen = wait_for_records(fake_web, min_xlen=1)
    assert xlen >= 1, f"expected >=1 usage:records entry, got {xlen}"
    record = usage_records_consumer.next_record(timeout=5.0)
    assert record.get("operation_id") == "operations/test-op-12345", (
        f"expected operation_id='operations/test-op-12345', got {record!r}"
    )


def test_normal_chat_has_null_operation_id(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    usage_records_consumer,
) -> None:
    """G5 negative: a normal `:generateContent` response (no top-level
    `name`) emits a usage record WITHOUT `operation_id`.

    `UsageRecord.operation_id` is `Option<String>` serialised with
    `skip_serializing_if = "Option::is_none"`, so the key is omitted
    entirely from the JSON payload — `record.get("operation_id")` is
    `None`, NOT the JSON null `None`.
    """
    seeded = seed_passthrough(
        fake_web,
        protocol=PROTO_GEMINI,
        model=GEMINI_MODEL,
        group="gemini-normal-grp",
        vendor_id="v-gemini-normal",
        key_id="vk-gemini-normal",
        user_id="u-gemini-normal",
        api_key_id="ak-gemini-normal",
        api_key_plaintext="sk-gemini-normal",
    )
    fake_web.set_balance(seeded["user_id"], BALANCE_LARGE)
    fake_upstream.set_scenario(_static_json_scenario(_GEMINI_NORMAL_BODY))

    client = make_gemini_client(gateway_url, "sk-gemini-normal", "ak-gemini-normal")
    resp = client.models.generate_content(
        model=GEMINI_MODEL,
        contents="hi",
    )
    assert resp.text and "hi" in resp.text, f"unexpected resp.text={resp.text!r}"

    xlen = wait_for_records(fake_web, min_xlen=1)
    assert xlen >= 1, f"expected >=1 usage:records entry, got {xlen}"
    record = usage_records_consumer.next_record(timeout=5.0)
    # Both shapes are acceptable: missing key (skip_serializing_if) OR
    # explicit JSON null (defensive). The presence of a non-null value
    # is the only failure.
    op_id = record.get("operation_id")
    assert op_id is None, (
        f"expected operation_id absent/null on a normal chat, got "
        f"{op_id!r} (record={record!r})"
    )
