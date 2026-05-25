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
