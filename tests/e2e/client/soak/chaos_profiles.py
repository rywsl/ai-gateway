"""Pinned chaos scenario profiles — see spec §11.1.

Per spec C6, these values are immutable. Changing them requires editing
the spec first; the comment above each PROFILE references the spec section.
"""
from __future__ import annotations

# A single happy SSE response that mirrors `scenario.rs::default_scenario`
# (2 events: a content delta + [DONE]). Used as variants[0] of every
# Weighted route — chaos-off mode falls through to it (spec §4.1).
_HAPPY_SSE = {
    "kind": "sse",
    "delay_ms": 0,
    "events": [
        {"data": '{"choices":[{"delta":{"content":"hi"}}]}'},
        {"data": "[DONE]"},
    ],
}


def _variant(weight: int, spec: dict) -> dict:
    return {"weight": weight, "spec": spec}


# Spec §11.1 — LIGHT: 96 % happy, 4 % faults distributed across partial /
# timeout / 429 / mid-body failures. Connection-class faults are reserved
# for HEAVY so the two profiles exercise different gateway paths.
LIGHT_PROFILE: list[dict] = [
    _variant(96, _HAPPY_SSE),
    _variant(1, {"kind": "tcp_reset_mid_body", "after_bytes": 256}),
    _variant(1, {"kind": "status_with_retry_after",
                 "status": 429, "retry_after_secs": 1,
                 "body": '{"error":"rate-limited"}'}),
    _variant(1, {"kind": "slow_body_byte_per_second",
                 "body": '{"choices":[{"delta":{"content":"slow"}}]}',
                 "bps": 4}),
    _variant(1, {"kind": "partial_sse_event", "half_body": 'data: {"choi'}),
    _variant(0, {"kind": "tcp_reset_after_headers"}),
    _variant(0, {"kind": "static", "status": 502,
                 "headers": {"content-type": "application/json"},
                 "body": '{"error":"bad gateway"}',
                 "delay_ms": 0}),
]

# Spec §11.1 — HEAVY: 80 % happy, 20 % faults skewed toward connection-class
# and rate-limit faults so retry / cooldown / health-degrade paths fire.
HEAVY_PROFILE: list[dict] = [
    _variant(80, _HAPPY_SSE),
    _variant(5, {"kind": "tcp_reset_mid_body", "after_bytes": 256}),
    _variant(5, {"kind": "status_with_retry_after",
                 "status": 429, "retry_after_secs": 1,
                 "body": '{"error":"rate-limited"}'}),
    _variant(3, {"kind": "slow_body_byte_per_second",
                 "body": '{"choices":[{"delta":{"content":"slow"}}]}',
                 "bps": 4}),
    _variant(2, {"kind": "partial_sse_event", "half_body": 'data: {"choi'}),
    _variant(3, {"kind": "tcp_reset_after_headers"}),
    _variant(2, {"kind": "static", "status": 502,
                 "headers": {"content-type": "application/json"},
                 "body": '{"error":"bad gateway"}',
                 "delay_ms": 0}),
]


def profile_for(mode: str) -> list[dict] | None:
    """Return the profile for `mode`, or `None` for `'off'`.

    `'off'` means: do not POST any weighted scenario; let fake-upstream
    serve its default happy SSE. (Equivalent to also POSTing a degenerate
    `[(100, sse_happy)]` Weighted, but skipping is simpler.)
    """
    if mode == "off":
        return None
    if mode == "light":
        return LIGHT_PROFILE
    if mode == "heavy":
        return HEAVY_PROFILE
    raise ValueError(f"unknown chaos mode: {mode}")


def to_scenario(profile: list[dict], path_regex: str = "^/v1/chat/completions$") -> dict:
    """Wrap a profile into the full Scenario JSON the fake-upstream
    admin endpoint accepts. Only one route is needed — chaos in soak
    is global, not per-path."""
    return {
        "routes": [
            {
                "match": {"method": "POST", "path_regex": path_regex},
                "response": {"kind": "weighted", "variants": profile},
            }
        ]
    }
