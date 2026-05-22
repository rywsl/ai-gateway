"""Concurrent async driver for the soak harness — see spec §6.

Architecture: a ThreadPoolExecutor of N OS threads (breaks GIL), each
running its own asyncio loop + httpx.AsyncClient. The main thread runs
a rate emitter that sleeps `1/target_rps` between ticks and dispatches
a freshly-sampled RequestSpec to a round-robin worker queue.

This file is split across Tasks 11–13:
  - Task 11: SoakConfig + SoakDriverReport dataclasses.
  - Task 12: per-thread async runner (`_thread_loop`).
  - Task 13: rate emitter + livez probe + report aggregation.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Literal


ChaosMode = Literal["off", "light", "heavy"]


@dataclass(frozen=True)
class SoakConfig:
    """Spec §11.3 — pinned defaults. Use SoakConfig.smoke() for CI iter."""
    duration_s: int = 60
    target_rps: int = 833
    n_threads: int = 8
    max_inflight_per_thread: int = 64
    per_request_timeout_s: int = 30
    drain_grace_s: int = 5
    drain_quiescence_s: int = 10
    drain_grace_max_s: int = 60
    n_users: int = 20
    chaos_mode: ChaosMode = "off"
    gateway_url: str = "http://localhost:8000"
    livez_url: str = "http://localhost:8080/livez"
    readyz_url: str = "http://localhost:8080/readyz"

    @property
    def target_count(self) -> int:
        return self.duration_s * self.target_rps

    @classmethod
    def smoke(cls) -> "SoakConfig":
        return cls(
            duration_s=10,
            target_rps=100,
            n_threads=2,
            max_inflight_per_thread=16,
            drain_quiescence_s=5,
            drain_grace_max_s=30,
            n_users=5,
        )


@dataclass
class RequestOutcome:
    protocol: str
    shape: str
    cross_protocol: bool
    status: int | None
    error_code: str | None
    latency_ms: float
    bytes_received: int
    failed_client_side: bool


@dataclass
class SoakDriverReport:
    duration_ms: int
    target_count: int
    requests_sent: int
    requests_completed: int
    requests_failed_client_side: int
    by_status: dict[int, int] = field(default_factory=dict)
    by_error_code: dict[str, int] = field(default_factory=dict)
    by_protocol: dict[str, int] = field(default_factory=dict)
    by_shape: dict[str, int] = field(default_factory=dict)
    by_cross_proto: dict[bool, int] = field(default_factory=dict)
    latency_ms_p50: float | None = None
    latency_ms_p90: float | None = None
    latency_ms_p99: float | None = None
    latency_ms_max: float | None = None
    bytes_received_total: int = 0
    thread_cpu_us_total: dict[int, int] = field(default_factory=dict)
    livez_probes: int = 0
    livez_non_200: int = 0
    readyz_probes: int = 0
    readyz_non_200: int = 0

    @classmethod
    def empty(cls, *, target_count: int) -> "SoakDriverReport":
        return cls(
            duration_ms=0,
            target_count=target_count,
            requests_sent=0,
            requests_completed=0,
            requests_failed_client_side=0,
        )
