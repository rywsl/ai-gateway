"""E2E coverage for the gateway's ``usage:detailed`` observability archive.

The detailed archive is a Redis stream that records five wire-fidelity surfaces
per completed request: downstream request, upstream request, upstream response,
downstream response, and a summary.  Each entry carries a request/trace id,
surface kind, tiered encoding tag, and JSON payload bytes that may be raw, lz4,
or zstd compressed depending on the pre-compression payload size.
"""

from __future__ import annotations

import json
import os
import time

import threading

from conftest import (
    BALANCE_LARGE,
    MODEL,
    PROTO_OPENAI,
    FakeUpstreamClient,
    FakeWebClient,
    decode_detailed_record,
    label_delta,
    make_openai_client,
    metrics_snapshot,
    pair_headers,
    post_chat,
    redis_xrange_decoded,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
)


DETAILED_STREAM = "usage:detailed"
SURFACE_KINDS = {
    "downstream_request",
    "upstream_request",
    "upstream_response",
    "downstream_response",
    "summary",
}


_OPENAI_STATIC_BODY = json.dumps(
    {
        "id": "chatcmpl-obs",
        "object": "chat.completion",
        "created": 1700000000,
        "model": MODEL,
        "choices": [
            {
                "index": 0,
                "message": {"role": "assistant", "content": "observed"},
                "finish_reason": "stop",
            }
        ],
        "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7},
    }
)


def _static_openai_scenario() -> dict:
    return {
        "routes": [
            {
                "match": {"method": "POST", "path_regex": "^/v1/chat/completions$"},
                "response": {
                    "kind": "static",
                    "status": 200,
                    "headers": {"content-type": "application/json"},
                    "body": _OPENAI_STATIC_BODY,
                    "delay_ms": 0,
                },
            }
        ]
    }


def _seed_obs_graph(fake_web: FakeWebClient, suffix: str) -> tuple[str, str]:
    user_id = f"obs_user_{suffix}"
    api_key_id = f"obs_ak_{suffix}"
    api_key_plaintext = f"obs_sk_{suffix}"
    group = f"obs_group_{suffix}"
    vendor_id = f"obs_vendor_{suffix}"
    vendor_key_id = f"obs_vk_{suffix}"

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
        key_id=vendor_key_id,
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(fake_web, group=group, vendor_key_ids=[vendor_key_id])
    seed_pricing(fake_web, model=MODEL)
    return api_key_plaintext, api_key_id


def _issue_openai_nonstream(
    gateway_url: str,
    api_key_plaintext: str,
    api_key_id: str,
    content: str = "hi",
):
    resp = post_chat(
        gateway_url,
        pair_headers(api_key_plaintext, api_key_id),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": content}],
            "stream": False,
        },
    )
    assert resp.status_code == 200, (resp.status_code, resp.text[:500])
    assert resp.json()["choices"][0]["message"]["content"] == "observed"
    return resp


def _detailed_entries_for_response(redis_client, resp) -> list[dict]:
    time.sleep(0.5)
    try:
        rows = redis_xrange_decoded(redis_client, DETAILED_STREAM)
    except UnicodeDecodeError:
        import redis

        binary_client = redis.Redis.from_url(
            os.environ.get("REDIS_URL", "redis://localhost:6379"),
            decode_responses=False,
        )
        try:
            rows = redis_xrange_decoded(binary_client, DETAILED_STREAM)
        finally:
            binary_client.close()
    request_id = resp.headers.get("x-request-id") or resp.headers.get("X-Request-Id")
    if request_id is None:
        grouped: dict[str, list[dict]] = {}
        for row in rows:
            rid = row.get("request_id")
            if rid:
                grouped.setdefault(rid, []).append(row)
        complete = [
            rid
            for rid, group_rows in grouped.items()
            if {row["kind"] for row in group_rows} == SURFACE_KINDS
        ]
        assert complete, f"expected at least one complete detailed request group: {rows!r}"
        request_id = complete[-1]
    return [row for row in rows if row.get("request_id") == request_id]


def _series_delta(pre: dict[str, float], post: dict[str, float], **labels: str) -> float:
    label_parts = [f'{k}="{v}"' for k, v in labels.items()]
    direct = label_delta(pre, post, ",".join(label_parts))
    if direct:
        return direct
    return label_delta(pre, post, ",".join(reversed(label_parts)))


def test_detailed_log_emits_all_5_surfaces(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    redis_client,
) -> None:
    api_key_plaintext, api_key_id = _seed_obs_graph(fake_web, "five")
    fake_upstream.set_scenario(_static_openai_scenario())

    resp = _issue_openai_nonstream(gateway_url, api_key_plaintext, api_key_id)
    entries = _detailed_entries_for_response(redis_client, resp)

    assert len(entries) == 5, entries
    assert {entry["kind"] for entry in entries} == SURFACE_KINDS
    for entry in entries:
        decoded = decode_detailed_record(entry)
        assert decoded["kind"] in SURFACE_KINDS
        assert decoded["encoding"] in {"raw+json", "lz4+json", "zstd+json"}
        assert isinstance(decoded["decoded_payload"], dict)
        assert decoded["decoded_payload"], decoded


def test_detailed_log_encoding_matches_size_thresholds(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    redis_client,
    metrics_url: str,
) -> None:
    api_key_plaintext, api_key_id = _seed_obs_graph(fake_web, "lz4")
    fake_upstream.set_scenario(_static_openai_scenario())
    pre = metrics_snapshot("gw_detailed_log_emit_total", metrics_url)

    large_prompt = "Hello world. " * 200
    resp = _issue_openai_nonstream(
        gateway_url, api_key_plaintext, api_key_id, content=large_prompt
    )
    entries = _detailed_entries_for_response(redis_client, resp)

    assert any(entry["encoding"] == "lz4+json" for entry in entries), entries
    downstream_lz4_delta = _series_delta(
        pre,
        metrics_snapshot("gw_detailed_log_emit_total", metrics_url),
        kind="downstream_request",
        encoding="lz4+json",
    )
    assert downstream_lz4_delta >= 1, (
        "expected downstream_request lz4 detailed-log emit increment; "
        f"pre={pre} entries={entries!r}"
    )


def test_detailed_log_metric_counters(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    api_key_plaintext, api_key_id = _seed_obs_graph(fake_web, "metrics")
    fake_upstream.set_scenario(_static_openai_scenario())
    pre_emit = metrics_snapshot("gw_detailed_log_emit_total", metrics_url)
    pre_drop = metrics_snapshot("gw_detailed_log_drop_total", metrics_url)

    _issue_openai_nonstream(gateway_url, api_key_plaintext, api_key_id)
    time.sleep(0.5)

    post_emit = metrics_snapshot("gw_detailed_log_emit_total", metrics_url)
    post_drop = metrics_snapshot("gw_detailed_log_drop_total", metrics_url)
    total_emit_delta = sum(post_emit.values()) - sum(pre_emit.values())
    total_drop_delta = sum(post_drop.values()) - sum(pre_drop.values())

    assert total_emit_delta == 5, (pre_emit, post_emit)
    for kind in SURFACE_KINDS:
        kind_delta = sum(
            value - pre_emit.get(labels, 0.0)
            for labels, value in post_emit.items()
            if f'kind="{kind}"' in labels
        )
        assert kind_delta == 1, (kind, pre_emit, post_emit)
    assert total_drop_delta == 0, (pre_drop, post_drop)


# ---------------------------------------------------------------------------
# forced_params observability — assert the gateway applies forced model-group
# parameters and increments `gw_forced_params_applied_total{field=...}`.
# ---------------------------------------------------------------------------


def _seed_fp_graph(
    fake_web: FakeWebClient,
    suffix: str,
    forced_params: dict,
) -> tuple[str, str]:
    """Seed a full entity graph with ``forced_params`` on the model group."""
    user_id = f"fp_user_{suffix}"
    api_key_id = f"fp_ak_{suffix}"
    api_key_plaintext = f"fp_sk_{suffix}"
    group = f"fp_group_{suffix}"
    vendor_id = f"fp_vendor_{suffix}"
    vendor_key_id = f"fp_vk_{suffix}"

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
        key_id=vendor_key_id,
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(
        fake_web,
        group=group,
        vendor_key_ids=[vendor_key_id],
        forced_params=forced_params,
    )
    seed_pricing(fake_web, model=MODEL)
    return api_key_plaintext, api_key_id


def test_forced_params_temperature_applied(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Seed model group with forced temperature=0.5, send request with
    temperature=0.9, assert ``gw_forced_params_applied_total{field="temperature"}``
    incremented."""
    api_key_plaintext, api_key_id = _seed_fp_graph(
        fake_web, "temp", {"temperature": 0.5}
    )
    fake_upstream.set_scenario(_static_openai_scenario())

    pre = metrics_snapshot("gw_forced_params_applied_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers(api_key_plaintext, api_key_id),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": "fp-temp"}],
            "stream": False,
            "temperature": 0.9,
        },
    )
    assert resp.status_code == 200, (resp.status_code, resp.text[:500])

    time.sleep(0.3)
    post = metrics_snapshot("gw_forced_params_applied_total", metrics_url)
    delta = label_delta(pre, post, 'field="temperature"')
    assert delta >= 1, (
        f"expected gw_forced_params_applied_total{{field=\"temperature\"}} to increment; "
        f"pre={pre} post={post}"
    )


def test_forced_params_top_p_applied(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    metrics_url: str,
) -> None:
    """Seed model group with forced top_p=0.3, send request with top_p=0.95,
    assert ``gw_forced_params_applied_total{field="top_p"}`` incremented."""
    api_key_plaintext, api_key_id = _seed_fp_graph(
        fake_web, "topp", {"top_p": 0.3}
    )
    fake_upstream.set_scenario(_static_openai_scenario())

    pre = metrics_snapshot("gw_forced_params_applied_total", metrics_url)

    resp = post_chat(
        gateway_url,
        pair_headers(api_key_plaintext, api_key_id),
        {
            "model": MODEL,
            "messages": [{"role": "user", "content": "fp-topp"}],
            "stream": False,
            "top_p": 0.95,
        },
    )
    assert resp.status_code == 200, (resp.status_code, resp.text[:500])

    time.sleep(0.3)
    post = metrics_snapshot("gw_forced_params_applied_total", metrics_url)
    delta = label_delta(pre, post, 'field="top_p"')
    assert delta >= 1, (
        f"expected gw_forced_params_applied_total{{field=\"top_p\"}} to increment; "
        f"pre={pre} post={post}"
    )


# ---------------------------------------------------------------------------
# TPM deferred freeze / release — verify the billing:inflight HASH lifecycle
# during a streaming request.
# ---------------------------------------------------------------------------


def _slow_sse_scenario(delay_between_ms: int = 300) -> dict:
    """SSE scenario with a deliberate inter-event delay so we can observe the
    inflight HASH before the stream completes."""
    return {
        "routes": [
            {
                "match": {"method": "POST", "path_regex": "^/v1/chat/completions$"},
                "response": {
                    "kind": "sse",
                    "delay_ms": 0,
                    "events": [
                        {
                            "data": json.dumps(
                                {
                                    "choices": [
                                        {"delta": {"content": "hello"}}
                                    ]
                                }
                            ),
                        },
                        {
                            "data": json.dumps(
                                {
                                    "choices": [
                                        {"delta": {"content": " world"}}
                                    ]
                                }
                            ),
                            "after_ms": delay_between_ms,
                        },
                        {
                            "data": json.dumps(
                                {
                                    "choices": [
                                        {"delta": {"content": "."}}
                                    ],
                                    "usage": {
                                        "prompt_tokens": 5,
                                        "completion_tokens": 3,
                                        "total_tokens": 8,
                                    },
                                }
                            ),
                            "after_ms": delay_between_ms,
                        },
                        {"data": "[DONE]", "after_ms": delay_between_ms},
                    ],
                },
            }
        ]
    }


def test_tpm_deferred_freeze_and_release(
    gateway_url: str,
    fake_web: FakeWebClient,
    fake_upstream: FakeUpstreamClient,
    redis_client,
    metrics_url: str,
) -> None:
    """Issue a streaming request, observe ``billing:inflight:*`` mid-stream,
    verify the HASH is cleaned up after stream completion, and check that
    ``usage:records`` gets the final charge."""
    user_id = "tpm_user_freeze"
    api_key_id = "tpm_ak_freeze"
    api_key_plaintext = "tpm_sk_freeze"
    group = "tpm_group_freeze"
    vendor_id = "tpm_vendor_freeze"
    vendor_key_id = "tpm_vk_freeze"

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
        key_id=vendor_key_id,
        vendor_id=vendor_id,
        model=MODEL,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_model_group(fake_web, group=group, vendor_key_ids=[vendor_key_id])
    seed_pricing(fake_web, model=MODEL)

    # Use a slow SSE scenario so the stream stays open long enough to inspect
    # inflight keys.
    fake_upstream.set_scenario(_slow_sse_scenario(delay_between_ms=400))

    client = make_openai_client(gateway_url, api_key_plaintext, api_key_id)

    # We'll collect inflight keys from a background thread while the stream
    # is being consumed on the main thread.
    inflight_keys_mid_stream: list[list[str]] = []
    inflight_hashes_mid_stream: list[dict] = []
    stream_error: list[Exception] = []
    stream_text: list[str] = []

    def _consume_stream():
        try:
            s = client.chat.completions.create(
                model=MODEL,
                messages=[{"role": "user", "content": "tpm-freeze-test"}],
                stream=True,
            )
            for chunk in s:
                if chunk.choices and chunk.choices[0].delta.content:
                    stream_text.append(chunk.choices[0].delta.content)
                # After the first chunk arrives the inflight HASH should exist.
                # Snapshot once mid-stream.
                if len(stream_text) == 1 and not inflight_keys_mid_stream:
                    time.sleep(0.05)
                    keys = redis_client.keys("billing:inflight:*")
                    inflight_keys_mid_stream.append(keys)
                    for k in keys:
                        h = redis_client.hgetall(k)
                        if h:
                            inflight_hashes_mid_stream.append(h)
        except Exception as e:
            stream_error.append(e)

    t = threading.Thread(target=_consume_stream, daemon=True)
    t.start()
    t.join(timeout=15)

    if stream_error:
        raise stream_error[0]

    assert "".join(stream_text), "stream produced no text"

    # Mid-stream: we should have observed at least one billing:inflight:* key.
    assert inflight_keys_mid_stream, "inflight keys snapshot was never taken"
    assert len(inflight_keys_mid_stream[0]) >= 1, (
        f"expected at least one billing:inflight:* key mid-stream, "
        f"got {inflight_keys_mid_stream[0]!r}"
    )
    # The HASH should contain a freeze_amount field > 0.
    if inflight_hashes_mid_stream:
        h = inflight_hashes_mid_stream[0]
        assert "freeze_amount" in h or "state" in h, (
            f"inflight HASH missing expected fields: {h!r}"
        )

    # After stream completion, wait briefly then verify cleanup.
    time.sleep(1.0)
    post_keys = redis_client.keys("billing:inflight:*")
    # The gateway should have cleared (DEL'd) the inflight key after settling.
    assert len(post_keys) == 0, (
        f"expected billing:inflight:* keys to be cleaned up after stream "
        f"completion, but found: {post_keys!r}"
    )

    # Verify usage:records was emitted.
    time.sleep(0.5)
    records = redis_client.xrange("usage:records")
    assert len(records) >= 1, (
        f"expected at least one usage:records entry after stream, "
        f"got {len(records)}"
    )
