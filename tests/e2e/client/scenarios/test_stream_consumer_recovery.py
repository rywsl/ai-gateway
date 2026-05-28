"""Phase C e2e: StreamConsumer recovers from Redis FLUSHALL (NOGROUP self-heal).

After a FLUSHALL the consumer groups disappear. The next XREADGROUP returns
NOGROUP. The ManagedLoop Supervisor detects Outcome::Recover and calls
ensure() which recreates the groups. Within 5s the consumer resumes and
gw_errors_total{kind=background_worker,code=recover} should have incremented.

Requires a live gateway + Redis. Skipped in agent worktree where e2e infra
is not available.
"""

from __future__ import annotations

import time

import pytest

from conftest import label_delta, metrics_snapshot


@pytest.mark.skip(reason="e2e infra not available in agent worktree")
def test_stream_consumer_recovers_after_flushall(
    redis_client,
    metrics_url: str,
) -> None:
    """FLUSHALL → NOGROUP → Supervisor Recover → groups recreated within 5s."""
    pre = metrics_snapshot("gw_errors_total", metrics_url)

    # Flush all Redis state — this removes the consumer groups created at boot.
    redis_client.flushall()

    # Wait up to 5s for the background worker to hit NOGROUP and emit a Recover.
    deadline = time.monotonic() + 5.0
    delta = 0.0
    while time.monotonic() < deadline:
        post = metrics_snapshot("gw_errors_total", metrics_url)
        delta = label_delta(pre, post, 'kind="background_worker",code="recover"')
        if delta >= 1:
            break
        time.sleep(0.25)

    assert delta >= 1, (
        f"expected gw_errors_total{{kind=background_worker,code=recover}} to increment "
        f"by at least 1 after FLUSHALL within 5s, got delta={delta}"
    )
