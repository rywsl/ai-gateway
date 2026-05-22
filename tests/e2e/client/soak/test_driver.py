"""Unit tests for the soak driver scaffolding (no live HTTP)."""
from soak.driver import SoakConfig, SoakDriverReport


def test_soak_config_defaults_match_spec_11_3():
    # Spec §11.3 — full row. Any change requires updating the spec first.
    c = SoakConfig()
    assert c.duration_s == 60
    assert c.target_rps == 833
    assert c.n_threads == 8
    assert c.max_inflight_per_thread == 64
    assert c.per_request_timeout_s == 30
    assert c.drain_grace_s == 5
    assert c.drain_quiescence_s == 10
    assert c.drain_grace_max_s == 60
    assert c.n_users == 20
    assert c.chaos_mode == "off"


def test_soak_config_smoke():
    # Spec §11.3 — smoke column.
    c = SoakConfig.smoke()
    assert c.duration_s == 10
    assert c.target_rps == 100
    assert c.n_threads == 2
    assert c.max_inflight_per_thread == 16
    assert c.n_users == 5


def test_driver_report_target_count_is_product():
    rpt = SoakDriverReport.empty(target_count=60 * 833)
    assert rpt.target_count == 49980
    assert rpt.requests_sent == 0
    assert rpt.requests_completed == 0


import asyncio
import threading

import httpx
import pytest

from soak.driver import RequestOutcome, _run_one_request, SoakConfig
from soak.seed import SoakSeedHandles, SOAK_MODEL_OPENAI
from soak.workload import RequestSpec


@pytest.mark.asyncio
async def test_run_one_request_records_status_and_latency(monkeypatch):
    transport = httpx.MockTransport(lambda r: httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        text='data: {"choices":[{"delta":{"content":"hi"}}]}\n\ndata: [DONE]\n\n',
    ))
    client = httpx.AsyncClient(transport=transport, timeout=5.0,
                               base_url="http://gateway.local")

    spec = RequestSpec(
        protocol="open_ai", shape="stream", cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1", api_key_id="ak-1", api_key_plaintext="sk-1",
    )
    handles = SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1")],
        groups_by_kind={"open_ai": "g", "anthropic": "g", "gemini": "g", "cross": "g"},
        models_by_kind={"open_ai": SOAK_MODEL_OPENAI,
                        "anthropic": "x", "gemini": "y"},
    )

    out = await _run_one_request(client, spec, handles, timeout_s=5)
    assert isinstance(out, RequestOutcome)
    assert out.status == 200
    assert out.failed_client_side is False
    assert out.latency_ms > 0
    assert out.bytes_received > 0
    assert out.protocol == "open_ai"
    assert out.shape == "stream"


@pytest.mark.asyncio
async def test_run_one_request_records_client_side_failure():
    transport = httpx.MockTransport(lambda r: (_ for _ in ()).throw(
        httpx.ConnectError("simulated")
    ))
    client = httpx.AsyncClient(transport=transport, timeout=1.0,
                               base_url="http://gateway.local")
    spec = RequestSpec(
        protocol="open_ai", shape="non_stream", cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1", api_key_id="ak-1", api_key_plaintext="sk-1",
    )
    handles = SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1")],
        groups_by_kind={"open_ai": "g", "anthropic": "g", "gemini": "g", "cross": "g"},
        models_by_kind={"open_ai": SOAK_MODEL_OPENAI,
                        "anthropic": "x", "gemini": "y"},
    )
    out = await _run_one_request(client, spec, handles, timeout_s=1)
    assert out.failed_client_side is True
    assert out.status is None
