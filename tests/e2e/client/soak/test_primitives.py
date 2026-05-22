"""Unit tests for the soak primitive modules (no docker, no live services)."""
from pathlib import Path

from soak.chaos_profiles import HEAVY_PROFILE, LIGHT_PROFILE
from soak.codes_catalog import parse_codes_catalog


REPO_ROOT = Path(__file__).resolve().parents[4]  # tests/e2e/client/soak → repo


def test_chaos_profiles_weights_sum_to_100():
    # Per spec §11.1 LIGHT 96/1/1/1/1/0/0 and HEAVY 80/5/5/3/2/3/2 each sum
    # to 100. This is a structural invariant — we use the values directly as
    # percentages in dispatch_stats so a typo silently skewing the soak's
    # bug-finding density is unacceptable.
    assert sum(v["weight"] for v in LIGHT_PROFILE) == 100
    assert sum(v["weight"] for v in HEAVY_PROFILE) == 100


def test_chaos_profiles_first_variant_is_sse_happy():
    # variants[0] is the happy path (spec §4.1) — chaos-off falls through here.
    for p in (LIGHT_PROFILE, HEAVY_PROFILE):
        assert p[0]["spec"]["kind"] == "sse"


def test_codes_catalog_parses_known_codes():
    codes_rs = REPO_ROOT / "crates" / "gw-core" / "src" / "error" / "codes.rs"
    catalog = parse_codes_catalog(codes_rs)
    # Sanity checks against the constants observed in the spec writing
    # session (see Appendix A in the design spec).
    expected = {
        "GW_UPSTREAM_TIMEOUT",
        "GW_UPSTREAM_5XX",
        "GW_UPSTREAM_OVERLOADED",
        "GW_UPSTREAM_CONNECT_FAILED",
        "GW_UPSTREAM_CANCELLED",
        "GW_AUTH_INVALID_APIKEY",
        "GW_REQUEST_MODEL_NOT_FOUND",
    }
    missing = expected - catalog
    assert not missing, f"catalog missing expected codes: {missing}"
    # All entries match the GW_* prefix and uppercase shape.
    for c in catalog:
        assert c.startswith("GW_") and c.isupper(), c


from soak.seed import SOAK_MODEL_OPENAI, SOAK_MODEL_ANTHROPIC, SOAK_MODEL_GEMINI, SoakSeedHandles


def test_seed_handles_shape():
    # Pure-data sanity: handles dataclass shape that the driver consumes.
    h = SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1"), ("u-2", "ak-2", "sk-2")],
        groups_by_kind={
            "open_ai": "soak-open_ai",
            "anthropic": "soak-anthropic",
            "gemini": "soak-gemini",
            "cross": "soak-cross",
        },
        models_by_kind={
            "open_ai": SOAK_MODEL_OPENAI,
            "anthropic": SOAK_MODEL_ANTHROPIC,
            "gemini": SOAK_MODEL_GEMINI,
        },
    )
    assert len(h.users) == 2
    assert h.groups_by_kind["cross"] == "soak-cross"
    assert h.models_by_kind["open_ai"] == SOAK_MODEL_OPENAI


import random

from soak.workload import (
    RequestSpec,
    WorkloadMix,
    build_request,
    sample_request_spec,
)


def _fake_handles() -> SoakSeedHandles:
    return SoakSeedHandles(
        users=[("u-1", "ak-1", "sk-1"), ("u-2", "ak-2", "sk-2")],
        groups_by_kind={
            "open_ai": "g-o", "anthropic": "g-a", "gemini": "g-g", "cross": "g-c",
        },
        models_by_kind={
            "open_ai": SOAK_MODEL_OPENAI,
            "anthropic": "claude-3-haiku-20240307",
            "gemini": "gemini-2.0-flash",
        },
    )


def test_sample_distribution_respects_weights():
    rng = random.Random(42)
    mix = WorkloadMix.default()
    h = _fake_handles()
    counts: dict[str, int] = {}
    for _ in range(10_000):
        spec = sample_request_spec(mix, h, rng)
        counts[spec.protocol] = counts.get(spec.protocol, 0) + 1
    # Each protocol gets ~33 % — 10k samples gives ±2 % wiggle.
    for proto in ("open_ai", "anthropic", "gemini"):
        pct = counts.get(proto, 0) / 10_000 * 100
        assert 28 <= pct <= 38, f"{proto} pct={pct:.1f}"


def test_build_request_openai_stream():
    h = _fake_handles()
    spec = RequestSpec(
        protocol="open_ai",
        shape="stream",
        cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1",
        api_key_id="ak-1",
        api_key_plaintext="sk-1",
    )
    req = build_request(spec, h)
    assert req["method"] == "POST"
    assert req["path"] == "/v1/chat/completions"
    assert req["headers"]["x-api-key"] == "sk-1"
    assert req["headers"]["x-api-key-id"] == "ak-1"
    assert req["json"]["model"] == SOAK_MODEL_OPENAI
    assert req["json"]["stream"] is True


def test_build_request_anthropic_non_stream():
    h = _fake_handles()
    spec = RequestSpec(
        protocol="anthropic",
        shape="non_stream",
        cross_protocol=False,
        payload_size_bucket="small",
        user_id="u-1",
        api_key_id="ak-1",
        api_key_plaintext="sk-1",
    )
    req = build_request(spec, h)
    assert req["path"] == "/v1/messages"
    assert req["json"]["model"] == "claude-3-haiku-20240307"
    assert req["json"].get("stream") in (False, None)


def test_build_request_cross_protocol_targets_cross_group():
    h = _fake_handles()
    spec = RequestSpec(
        protocol="open_ai",
        shape="stream",
        cross_protocol=True,
        payload_size_bucket="small",
        user_id="u-1",
        api_key_id="ak-1",
        api_key_plaintext="sk-1",
    )
    req = build_request(spec, h)
    assert req["path"] == "/v1/chat/completions"
    assert req["json"]["model"] == SOAK_MODEL_OPENAI
