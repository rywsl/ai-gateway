"""Local conftest for the soak/ package.

The soak unit tests (test_primitives.py and any future module-level unit
tests) are pure Python with no live-service dependencies. The parent
conftest.py in tests/e2e/client/ registers two autouse fixtures
(_check_fakes_alive, isolate_state) that call requests.get / .post on
live docker-compose services. Overriding them here with no-op fixtures
prevents those network calls from failing test collection when the stack
is not running.

The FULL soak orchestrator test (test_chaos_soak.py, Task 21) is NOT in
this package — it lives in tests/e2e/client/scenarios/ and inherits the
real fixtures as normal.
"""
import pytest


@pytest.fixture(scope="session", autouse=True)
def _check_fakes_alive():  # type: ignore[override]
    """No-op override — soak unit tests have no live-service dependency."""


@pytest.fixture(autouse=True)
def isolate_state():  # type: ignore[override]
    """No-op override — soak unit tests have no state to isolate."""
