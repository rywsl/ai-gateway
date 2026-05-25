"""Graceful-shutdown scheduling shim.

The actual test logic lives in ``test_resilience._graceful_shutdown_drains_inflight_impl``.
This file exists solely to guarantee pytest runs the shutdown scenario *last*
(the ``z`` prefix sorts after every other module), because it docker-restarts the
gateway container, which would break any test that follows.
"""
from __future__ import annotations

from scenarios.test_resilience import _graceful_shutdown_drains_inflight_impl


def test_graceful_shutdown_drains_inflight(
    fake_web,
    fake_upstream,
    gateway_url: str,
    gateway_health_url: str,
    redis_client,
) -> None:
    _graceful_shutdown_drains_inflight_impl(
        fake_web,
        fake_upstream,
        gateway_url,
        gateway_health_url,
        redis_client,
    )
