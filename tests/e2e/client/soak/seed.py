"""Bulk seed for the soak harness — see spec §6.4.

One-shot population of users / api_keys / vendors / vendor_keys /
model_groups / pricings sufficient to drive 50k+ mixed-protocol
requests through the gateway. Every model_group uses fixed_order
strategy with cross_vendor_fallback=False so retry is disabled and
1 client request ⇒ ≤1 upstream call (spec C5).
"""
from __future__ import annotations

from dataclasses import dataclass

from conftest import (
    BALANCE_LARGE,
    PROTO_OPENAI,
    FakeWebClient,
    seed_model_group,
    seed_pricing,
    seed_user_apikey,
    seed_vendor,
    seed_vendor_key,
)


# Model names per protocol — the gateway uses `model` as the routing key.
SOAK_MODEL_OPENAI = "gpt-4"
SOAK_MODEL_ANTHROPIC = "claude-3-haiku-20240307"
SOAK_MODEL_GEMINI = "gemini-2.0-flash"

PROTO_ANTHROPIC = "anthropic"
PROTO_GEMINI = "gemini"


@dataclass
class SoakSeedHandles:
    """What `seed_soak` returns — the soak driver pulls user/group/model
    references from this dataclass at request-construction time."""
    users: list[tuple[str, str, str]]
    """[(user_id, api_key_id, api_key_plaintext), ...] — 20 entries by default."""
    groups_by_kind: dict[str, str]
    """{'open_ai': 'soak-open_ai', 'anthropic': ..., 'gemini': ..., 'cross': ...}"""
    models_by_kind: dict[str, str]
    """{'open_ai': 'gpt-4', 'anthropic': '...', 'gemini': '...'} — used for
    the request body. Cross requests reuse the OpenAI model name (the
    client sends OpenAI wire but the vendor_key is Anthropic-only)."""


def seed_soak(fake_web: FakeWebClient, n_users: int = 20) -> SoakSeedHandles:
    """Populate the entire entity graph for one soak run.

    Idempotency: the soak fixture wipes state via `fake_web.reset()` BEFORE
    each test (see conftest.isolate_state), so this function assumes a clean
    slate. Callers MUST NOT invoke it twice without an intervening reset.
    """
    # ── 20 users + 20 api_keys, all with the same allowed_groups so the
    # auth merge step doesn't reject any soak request. ────────────────────
    users: list[tuple[str, str, str]] = []
    for i in range(n_users):
        uid = f"u-soak-{i:03d}"
        kid = f"ak-soak-{i:03d}"
        plain = f"sk-soak-{i:03d}"
        seed_user_apikey(
            fake_web,
            user_id=uid,
            api_key_id=kid,
            api_key_plaintext=plain,
            allowed_groups=[
                "soak-open_ai", "soak-anthropic", "soak-gemini", "soak-cross",
            ],
            rpm_limit=100_000,  # well above 833 rps per user
        )
        fake_web.set_balance(uid, BALANCE_LARGE)
        users.append((uid, kid, plain))

    # ── 3 vendors, one per protocol family. ──────────────────────────────
    base = "http://fake-upstream:9000"
    seed_vendor(fake_web, vendor_id="v-soak-open_ai", base_urls=[base])
    seed_vendor(fake_web, vendor_id="v-soak-anthropic", base_urls=[base])
    seed_vendor(fake_web, vendor_id="v-soak-gemini", base_urls=[base])

    # ── 3 same-protocol vendor_keys + 1 cross-protocol vendor_key. ──────
    # Cross-proto: the gateway accepts the request as OpenAI wire but the
    # vendor_key is anthropic-only with protocol_conversion=true, forcing
    # the two-end re-render path. We use `gpt-4` as the model name (same as
    # SOAK_MODEL_OPENAI) so the gateway's model → group lookup picks the
    # cross group.
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-open_ai",
        vendor_id="v-soak-open_ai",
        model=SOAK_MODEL_OPENAI,
        accessible_endpoints=[PROTO_OPENAI],
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-anthropic",
        vendor_id="v-soak-anthropic",
        model=SOAK_MODEL_ANTHROPIC,
        accessible_endpoints=[PROTO_ANTHROPIC],
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-gemini",
        vendor_id="v-soak-gemini",
        model=SOAK_MODEL_GEMINI,
        accessible_endpoints=[PROTO_GEMINI],
    )
    seed_vendor_key(
        fake_web,
        key_id="vk-soak-cross",
        vendor_id="v-soak-anthropic",
        model=SOAK_MODEL_OPENAI,         # client sends OpenAI wire for `gpt-4`
        accessible_endpoints=[PROTO_ANTHROPIC],
        protocol_conversion=True,         # re-render OpenAI ⇆ Anthropic
    )

    # ── 4 model_groups — each pins one vendor_key, fixed_order, no
    # cross-vendor-fallback. C5: retry disabled, 1 request ⇒ ≤1 upstream
    # call so account-consistency invariant can be strict ==. ─────────────
    for group, key in [
        ("soak-open_ai", "vk-soak-open_ai"),
        ("soak-anthropic", "vk-soak-anthropic"),
        ("soak-gemini", "vk-soak-gemini"),
        ("soak-cross", "vk-soak-cross"),
    ]:
        seed_model_group(
            fake_web,
            group=group,
            vendor_key_ids=[key],
            strategy="fixed_order",
            cross_vendor_fallback=False,
        )

    # ── 3 pricings — one per protocol model. ─────────────────────────────
    for m in (SOAK_MODEL_OPENAI, SOAK_MODEL_ANTHROPIC, SOAK_MODEL_GEMINI):
        seed_pricing(fake_web, model=m)

    return SoakSeedHandles(
        users=users,
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
