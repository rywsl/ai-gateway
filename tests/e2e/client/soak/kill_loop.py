"""Kill-during-flight chaos injector for the soak harness — spec §8.3.

A background thread that, every `tick_s` (1s), SIGKILLs the gateway container
with probability `prob_per_s`.  After each kill it sleeps `cooldown_s` (~2s) to
let docker-compose's `restart: unless-stopped` policy bring the container back
before another kill can fire — otherwise back-to-back kills could leave the
gateway down for the rest of the run and starve the request generators.

The gateway is killed via `docker compose -f <compose> kill -s SIGKILL
<service>`.  SIGKILL (not SIGTERM) is deliberate: it models the exact crash the
billing-record-durability fix targets — a process death with NO graceful
flush, so the inflight Redis key is whatever state it was in at the instant of
death.  The Go reconciler must then recover (or dedup) every affected attempt.

This module touches NO gateway code and only shells out to `docker compose`,
matching the existing harness convention (`conftest.gateway_compose_logs_since`
et al.).  `KillLoop.kills` records each fired kill's wall-clock offset + the
docker exit status so the soak report can correlate kills with reconciliation.
"""
from __future__ import annotations

import random
import subprocess
import threading
import time
from dataclasses import dataclass, field


# Compose file path is relative to `tests/e2e/client/` (the pytest CWD set by
# the Makefile `cd tests/e2e/client`), matching the other helpers in
# conftest.py that pass `-f ../docker-compose.yml`.
_COMPOSE_FILE = "../docker-compose.yml"
_GATEWAY_SERVICE = "gateway"


@dataclass
class KillEvent:
    offset_s: float
    returncode: int
    stderr: str


@dataclass
class KillLoopReport:
    enabled: bool
    prob_per_s: float
    ticks: int = 0
    kills_attempted: int = 0
    kills_succeeded: int = 0
    kills_failed: int = 0
    events: list[KillEvent] = field(default_factory=list)


def _docker_kill(compose_file: str, service: str, signal: str = "SIGKILL") -> subprocess.CompletedProcess:
    return subprocess.run(
        ["docker", "compose", "-f", compose_file, "kill", "-s", signal, service],
        capture_output=True, text=True, timeout=30,
    )


class KillLoop:
    """Background SIGKILL injector. Use as `start()` / `stop()` around a run.

    The loop is driven off a monotonic clock and a stop `threading.Event`, so
    `stop()` returns promptly even mid-cooldown.  All randomness uses a seeded
    `random.Random` so a failing run is reproducible (the soak driver seeds its
    own RNG the same way).
    """

    def __init__(
        self,
        *,
        prob_per_s: float,
        rng_seed: int = 0xCAFE,
        tick_s: float = 1.0,
        cooldown_s: float = 2.0,
        compose_file: str = _COMPOSE_FILE,
        service: str = _GATEWAY_SERVICE,
    ) -> None:
        self.prob_per_s = prob_per_s
        self.tick_s = tick_s
        self.cooldown_s = cooldown_s
        self.compose_file = compose_file
        self.service = service
        self._rng = random.Random(rng_seed)
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._t0 = 0.0
        self.report = KillLoopReport(enabled=prob_per_s > 0.0, prob_per_s=prob_per_s)

    # ── lifecycle ────────────────────────────────────────────────────────

    def start(self) -> None:
        if self.prob_per_s <= 0.0:
            return  # disabled — no thread, report stays empty/enabled=False
        self._t0 = time.perf_counter()
        self._thread = threading.Thread(target=self._loop, name="soak-kill-loop", daemon=True)
        self._thread.start()

    def stop(self) -> KillLoopReport:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=self.cooldown_s + 35)
        return self.report

    # ── loop body ────────────────────────────────────────────────────────

    def _loop(self) -> None:
        while not self._stop.is_set():
            # Wait one tick (interruptible).
            if self._stop.wait(self.tick_s):
                break
            self.report.ticks += 1
            if self._rng.random() >= self.prob_per_s:
                continue
            # Fire a kill.
            self.report.kills_attempted += 1
            offset = time.perf_counter() - self._t0
            try:
                proc = _docker_kill(self.compose_file, self.service)
                rc = proc.returncode
                stderr = (proc.stderr or "").strip()[:500]
            except Exception as exc:  # noqa: BLE001 — record, never crash the loop
                rc = -1
                stderr = f"{type(exc).__name__}: {exc}"[:500]
            if rc == 0:
                self.report.kills_succeeded += 1
            else:
                self.report.kills_failed += 1
            self.report.events.append(KillEvent(offset_s=round(offset, 3), returncode=rc, stderr=stderr))
            # Cooldown so compose can restart before the next possible kill.
            if self._stop.wait(self.cooldown_s):
                break
