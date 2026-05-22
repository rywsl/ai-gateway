"""Request-spec sampling + 3 protocols × 2 shapes wire builders (spec §6.3).

The driver constructs requests directly with httpx — NOT through SDKs —
because:
  - openai / anthropic / google-genai sync clients don't compose with
    asyncio + 833 rps cleanly;
  - the soak's job is to exercise the gateway, not validate the SDKs
    (which are covered by existing scenario tests).
"""
from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Literal

from soak.seed import SoakSeedHandles


Protocol = Literal["open_ai", "anthropic", "gemini"]
Shape = Literal["stream", "non_stream"]
PayloadBucket = Literal["small", "medium", "large"]


@dataclass(frozen=True)
class RequestSpec:
    protocol: Protocol
    shape: Shape
    cross_protocol: bool
    payload_size_bucket: PayloadBucket
    user_id: str
    api_key_id: str
    api_key_plaintext: str


@dataclass(frozen=True)
class WorkloadMix:
    protocol_weights: dict[Protocol, int]
    shape_weights: dict[Shape, int]
    cross_protocol_pct: int
    payload_size_weights: dict[PayloadBucket, int]

    @classmethod
    def default(cls) -> "WorkloadMix":
        return cls(
            protocol_weights={"open_ai": 33, "anthropic": 33, "gemini": 34},
            shape_weights={"stream": 50, "non_stream": 50},
            cross_protocol_pct=10,
            payload_size_weights={"small": 70, "medium": 25, "large": 5},
        )


def _weighted_choice(rng: random.Random, weights: dict[str, int]) -> str:
    """Sample a key from a dict of {value: weight}. Weights must sum > 0."""
    keys = list(weights.keys())
    vals = list(weights.values())
    return rng.choices(keys, weights=vals, k=1)[0]


def sample_request_spec(
    mix: WorkloadMix,
    handles: SoakSeedHandles,
    rng: random.Random,
) -> RequestSpec:
    proto = _weighted_choice(rng, mix.protocol_weights)
    shape = _weighted_choice(rng, mix.shape_weights)
    cross = (rng.randint(1, 100) <= mix.cross_protocol_pct)
    bucket = _weighted_choice(rng, mix.payload_size_weights)
    uid, kid, plain = rng.choice(handles.users)
    return RequestSpec(
        protocol=proto,  # type: ignore[arg-type]
        shape=shape,     # type: ignore[arg-type]
        cross_protocol=cross,
        payload_size_bucket=bucket,  # type: ignore[arg-type]
        user_id=uid,
        api_key_id=kid,
        api_key_plaintext=plain,
    )


def _content_for(bucket: PayloadBucket) -> str:
    """Per spec §6.3, vary payload size across small/medium/large buckets."""
    if bucket == "small":
        return "hi"
    if bucket == "medium":
        return "hi " * 64
    return "hi " * 512


def build_request(spec: RequestSpec, handles: SoakSeedHandles) -> dict:
    """Build the wire-side request dict the driver POSTs via httpx.

    Returns: {"method": str, "path": str, "headers": dict, "json": dict}.
    """
    headers = {
        "x-api-key": spec.api_key_plaintext,
        "x-api-key-id": spec.api_key_id,
        "content-type": "application/json",
    }
    content = _content_for(spec.payload_size_bucket)

    if spec.protocol == "open_ai":
        model = handles.models_by_kind["open_ai"]
        body = {
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "stream": spec.shape == "stream",
            "max_tokens": 64,
        }
        return {"method": "POST", "path": "/v1/chat/completions",
                "headers": headers, "json": body}

    if spec.protocol == "anthropic":
        model = handles.models_by_kind["anthropic"]
        body = {
            "model": model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": 64,
        }
        if spec.shape == "stream":
            body["stream"] = True
        return {"method": "POST", "path": "/v1/messages",
                "headers": headers, "json": body}

    # gemini
    model = handles.models_by_kind["gemini"]
    path = (
        f"/v1beta/models/{model}:streamGenerateContent"
        if spec.shape == "stream"
        else f"/v1beta/models/{model}:generateContent"
    )
    body = {
        "contents": [{"parts": [{"text": content}], "role": "user"}],
        "generationConfig": {"maxOutputTokens": 64},
    }
    return {"method": "POST", "path": path, "headers": headers, "json": body}
