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

import asyncio
import re
import threading
import time
from dataclasses import dataclass, field
from queue import SimpleQueue, Empty
from typing import Literal

import httpx

from soak.seed import SoakSeedHandles
from soak.workload import RequestSpec, build_request


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


# ---------------------------------------------------------------------------
# Error-code extraction
# ---------------------------------------------------------------------------

# Best-effort parser for the error_code embedded in the gateway's
# OpenAI/Anthropic/Gemini error envelopes. All three families wrap the
# code under `{"error": {"code": "..."}}` per
# crates/gw-codec/src/*/codec.rs::render_error. SSE error frames also
# embed the same envelope as a `data: ` line — we grep for the first one.
_ERR_CODE_RE = re.compile(rb'"code"\s*:\s*"(GW_[A-Z_]+)"')


def _extract_error_code(body_bytes: bytes) -> str | None:
    m = _ERR_CODE_RE.search(body_bytes)
    return m.group(1).decode() if m else None


# ---------------------------------------------------------------------------
# Per-request async runner
# ---------------------------------------------------------------------------

async def _run_one_request(
    client: httpx.AsyncClient,
    spec: RequestSpec,
    handles: SoakSeedHandles,
    timeout_s: float,
) -> RequestOutcome:
    req = build_request(spec, handles)
    t0 = time.perf_counter()
    status: int | None = None
    error_code: str | None = None
    bytes_received = 0
    failed = False
    try:
        async with client.stream(
            req["method"], req["path"],
            headers=req["headers"],
            json=req["json"],
            timeout=timeout_s,
        ) as resp:
            status = resp.status_code
            buf = bytearray()
            async for chunk in resp.aiter_bytes():
                bytes_received += len(chunk)
                buf.extend(chunk)
                if bytes_received > 64 * 1024:
                    # Cap memory per response — we only need the head to
                    # find the error_code envelope. Real content can be
                    # streamed indefinitely without OOM.
                    break
            error_code = _extract_error_code(bytes(buf))
    except (httpx.HTTPError, asyncio.TimeoutError):
        failed = True

    latency_ms = (time.perf_counter() - t0) * 1000.0
    return RequestOutcome(
        protocol=spec.protocol,
        shape=spec.shape,
        cross_protocol=spec.cross_protocol,
        status=status,
        error_code=error_code,
        latency_ms=latency_ms,
        bytes_received=bytes_received,
        failed_client_side=failed,
    )


# ---------------------------------------------------------------------------
# Per-thread loop
# ---------------------------------------------------------------------------

def _thread_loop(
    thread_idx: int,
    cfg: SoakConfig,
    handles: SoakSeedHandles,
    in_queue: SimpleQueue,
    stop_event: threading.Event,
    outcomes: list[RequestOutcome],
    outcomes_lock: threading.Lock,
) -> None:
    """Run one OS thread. Owns one asyncio loop + one AsyncClient."""
    async def runner() -> None:
        limits = httpx.Limits(
            max_connections=cfg.max_inflight_per_thread * 4,
            max_keepalive_connections=cfg.max_inflight_per_thread * 2,
        )
        async with httpx.AsyncClient(
            base_url=cfg.gateway_url,
            limits=limits,
            timeout=cfg.per_request_timeout_s,
        ) as client:
            sem = asyncio.Semaphore(cfg.max_inflight_per_thread)
            inflight: set[asyncio.Task] = set()

            async def fire(spec: RequestSpec) -> None:
                async with sem:
                    out = await _run_one_request(
                        client, spec, handles, cfg.per_request_timeout_s
                    )
                    with outcomes_lock:
                        outcomes.append(out)

            # Drain the queue until stop_event AND queue is empty.
            while True:
                try:
                    spec = in_queue.get(timeout=0.05)
                except Empty:
                    if stop_event.is_set() and not inflight:
                        break
                    # Reap finished tasks to bound the set size.
                    inflight = {t for t in inflight if not t.done()}
                    await asyncio.sleep(0)
                    continue
                task = asyncio.create_task(fire(spec))
                inflight.add(task)
                inflight = {t for t in inflight if not t.done()}

            # Final grace: await all in-flight up to drain_grace_s.
            if inflight:
                try:
                    await asyncio.wait_for(
                        asyncio.gather(*inflight, return_exceptions=True),
                        timeout=cfg.drain_grace_s,
                    )
                except asyncio.TimeoutError:
                    pass

    asyncio.run(runner())
