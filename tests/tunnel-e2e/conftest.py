"""Fixtures for the local tunnel-e2e suite."""

from __future__ import annotations

import hashlib
import json
import os
import time
from typing import Callable, Iterable

import httpx
import psycopg
import pytest
import redis
from cryptography.hazmat.primitives.ciphers.aead import AESGCM


BALANCE_LARGE = 1_000_000_000
PROTO_OPENAI = "open_ai"
KEK_HEX = os.environ.get("GW_KEK_HEX", "deadbeef" * 8)
TUNNEL_REFRESH_WAIT_SECS = float(os.environ.get("TUNNEL_E2E_REFRESH_WAIT_SECS", "6"))
UPSTREAM_HOST = "example.com"


def _env(name: str) -> str:
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f"required env var {name!r} not set")
    return value


def sha256_bytes(plaintext: str) -> list[int]:
    return list(hashlib.sha256(plaintext.encode("utf-8")).digest())


def encrypt_upstream_key(
    plaintext: str,
    kek_hex: str = KEK_HEX,
    kek_kid: str = "primary",
) -> dict:
    kek_bytes = bytes.fromhex(kek_hex)
    if len(kek_bytes) != 32:
        raise ValueError(f"KEK must decode to 32 bytes, got {len(kek_bytes)}")
    nonce = os.urandom(12)
    out = AESGCM(kek_bytes).encrypt(nonce, plaintext.encode("utf-8"), None)
    ciphertext, auth_tag = out[:-16], out[-16:]
    return {
        "ciphertext": list(ciphertext),
        "nonce": list(nonce),
        "auth_tag": list(auth_tag),
        "kek_kid": kek_kid,
    }


class FakeWebClient:
    def __init__(self, base: str) -> None:
        self.base = base.rstrip("/")
        self.session = httpx.Client(base_url=self.base, timeout=10.0)

    def close(self) -> None:
        self.session.close()

    def put_entity(self, table: str, entity_id: str, payload: dict) -> None:
        resp = self.session.post(f"/entity/{table}/{entity_id}", json=payload)
        resp.raise_for_status()

    def set_balance(self, user_id: str, micros: int) -> None:
        resp = self.session.post(f"/balance/{user_id}", json={"micros": micros})
        resp.raise_for_status()

    def set_raw_counter(self, key: str, value: int) -> None:
        resp = self.session.post("/counter-raw", json={"key": key, "value": value})
        resp.raise_for_status()

    def reset(self) -> None:
        resp = self.session.post("/admin/reset", timeout=30.0)
        resp.raise_for_status()


class FakeUpstreamClient:
    def __init__(self, base: str) -> None:
        self.base = base.rstrip("/")
        self.session = httpx.Client(base_url=self.base, timeout=10.0)

    def close(self) -> None:
        self.session.close()

    def set_scenario(self, scenario: dict) -> None:
        resp = self.session.post("/admin/scenario", json=scenario)
        resp.raise_for_status()


@pytest.fixture(scope="session")
def pg_conn() -> Iterable[psycopg.Connection]:
    conn = psycopg.connect(_env("PG_DSN"), autocommit=True)
    try:
        yield conn
    finally:
        conn.close()


@pytest.fixture(scope="session")
def redis_client() -> Iterable[redis.Redis]:
    cli = redis.Redis.from_url(_env("REDIS_URL"), decode_responses=True)
    try:
        yield cli
    finally:
        cli.close()


@pytest.fixture(scope="session")
def gateway_client() -> Iterable[httpx.Client]:
    with httpx.Client(base_url=_env("ALI_GATEWAY_URL"), timeout=15.0) as client:
        yield client


@pytest.fixture(scope="session")
def metrics_client() -> Iterable[httpx.Client]:
    with httpx.Client(base_url=_env("ALI_METRICS_URL"), timeout=10.0) as client:
        yield client


@pytest.fixture(scope="session")
def fake_web() -> Iterable[FakeWebClient]:
    client = FakeWebClient(_env("FAKE_WEB_URL"))
    try:
        yield client
    finally:
        client.close()


@pytest.fixture(scope="session")
def fake_upstream() -> Iterable[FakeUpstreamClient]:
    client = FakeUpstreamClient(_env("FAKE_UPSTREAM_ADMIN_URL"))
    try:
        yield client
    finally:
        client.close()


@pytest.fixture(autouse=True)
def isolate_state(fake_web: FakeWebClient, fake_upstream: FakeUpstreamClient) -> None:
    fake_web.reset()
    fake_upstream.set_scenario(
        {
            "routes": [
                {
                    "match": {"method": "POST", "path_regex": "^/v1/chat/completions$"},
                    "response": {
                        "kind": "sse",
                        "delay_ms": 0,
                        "events": [
                            {"data": '{"choices":[{"delta":{"content":"hi"}}]}'},
                            {"data": "[DONE]"},
                        ],
                    },
                }
            ]
        }
    )


@pytest.fixture
def seed_nodes(
    pg_conn: psycopg.Connection,
    redis_client: redis.Redis,
) -> Callable[..., None]:
    def _seed(*nodes: dict) -> None:
        with pg_conn.cursor() as cur:
            cur.execute("DELETE FROM proxy_nodes")
            for node in nodes:
                cur.execute(
                    "INSERT INTO proxy_nodes (id, payload, version, deleted_at) "
                    "VALUES (%s, %s::jsonb, 1, NULL)",
                    (node["id"], json.dumps(node)),
                )
                redis_client.xadd(
                    "events:entity",
                    {"ns": "proxy_node", "id": node["id"], "op": "upsert"},
                )
        time.sleep(TUNNEL_REFRESH_WAIT_SECS)

    return _seed


@pytest.fixture
def seed_tunnel_key(fake_web: FakeWebClient) -> Callable[..., dict]:
    def _seed(
        *,
        suffix: str,
        require_tunnel: bool,
        vendor_base_url: str = f"http://{UPSTREAM_HOST}:9000",
    ) -> dict:
        ids = {
            "user_id": f"tn_user_{suffix}",
            "api_key_id": f"tn_ak_{suffix}",
            "api_key_plaintext": f"tn_sk_client_{suffix}",
            "vendor_id": f"tn_vendor_{suffix}",
            "key_id": f"tn_vk_{suffix}",
            "group": f"tn_group_{suffix}",
            "model": f"tn_model_{suffix}",
        }
        fake_web.put_entity(
            "users",
            ids["user_id"],
            {
                "user_id": ids["user_id"],
                "parent_id": None,
                "tier": "standard",
                "status": "active",
                "allowed_models": [],
                "allowed_channels": [],
                "allowed_groups": [ids["group"]],
                "rpm_limit": 1000,
                "tpm_limit": 0,
                "max_concurrent_streams": 100,
                "daily_quota_tokens": 0,
                "user_group_ids": [],
                "markup": 1.0,
                "debug_enabled": False,
            },
        )
        fake_web.put_entity(
            "api_keys",
            ids["api_key_id"],
            {
                "key_id": ids["api_key_id"],
                "user_id": ids["user_id"],
                "status": "active",
                "hash": sha256_bytes(ids["api_key_plaintext"]),
                "allowed_models": [],
                "allowed_channels": [],
                "rpm_limit": 0,
                "tpm_limit": 0,
                "markup": 1.0,
                "ip_allowlist": None,
                "expires_at": None,
                "signature_required": False,
                "signature_algo": "hmac_sha256",
                "force_capabilities": [],
                "match_mode_exclusive": False,
            },
        )
        fake_web.put_entity(
            "vendors",
            ids["vendor_id"],
            {
                "id": ids["vendor_id"],
                "display_name": ids["vendor_id"],
                "base_urls": [vendor_base_url],
                "auth_template": "Bearer {api_key}",
                "timeout_overrides": None,
                "default_proxy": None,
            },
        )
        fake_web.put_entity(
            "vendor_keys",
            ids["key_id"],
            {
                "key_id": ids["key_id"],
                "vendor_id": ids["vendor_id"],
                "api_key_enc": encrypt_upstream_key("sk-up"),
                "accessible_models": [ids["model"]],
                "model_mapping": {},
                "accessible_endpoints": [PROTO_OPENAI],
                "protocol_conversion": False,
                "priority": 0,
                "weight": 1,
                "rpm_limit": 0,
                "tpm_limit": 0,
                "max_usage_count": None,
                "proxy": None,
                "ip_allowlist": None,
                "expires_at": None,
                "status": "active",
                "tags": [],
                "forced_capabilities": [],
                "exclusive_capability_match": False,
                "tunnel_preference": {"require_tunnel": require_tunnel},
            },
        )
        fake_web.put_entity(
            "model_groups",
            ids["group"],
            {
                "group": ids["group"],
                "strategy": "fixed_order",
                "cross_vendor_fallback": False,
                "max_tokens_cap": None,
                "default_params": {},
                "forced_params": {},
                "vendor_key_ids": [ids["key_id"]],
            },
        )
        fake_web.put_entity(
            "models",
            ids["model"],
            {
                "name": ids["model"],
                "capabilities": [],
            },
        )
        fake_web.put_entity(
            "pricings",
            ids["model"],
            {
                "model": ids["model"],
                "input_price_micro_per_token": 1,
                "output_price_micro_per_token": 1,
                "max_output": 4096,
                "model_markup": 1.0,
                "connect_timeout_ms": None,
                "read_timeout_ms": None,
                "total_timeout_ms": None,
                "stream_total_timeout_ms": None,
            },
        )
        with psycopg.connect(_env("PG_DSN"), autocommit=True) as conn:
            with conn.cursor() as cur:
                cur.execute(
                    "INSERT INTO estimate_pricings (id, payload, version, deleted_at) "
                    "VALUES (%s, %s::jsonb, 1, NULL) "
                    "ON CONFLICT (id) DO UPDATE SET payload = EXCLUDED.payload, "
                    "version = estimate_pricings.version + 1, "
                    "updated_at = now(), deleted_at = NULL",
                    (
                        ids["model"],
                        json.dumps(
                            {
                                "model": ids["model"],
                                "conservative_per_token_micro": 1,
                                "safety_factor": 1.0,
                                "default_max_tokens": 16,
                                "hard_cap_tokens": 4096,
                            }
                        ),
                    ),
                )
        fake_web.set_balance(ids["user_id"], BALANCE_LARGE)
        fake_web.set_raw_counter(f"counter:user:{ids['user_id']}:base_micro", BALANCE_LARGE)
        time.sleep(1)
        return ids

    return _seed


def metrics_snapshot(metric: str, metrics_client: httpx.Client) -> dict[str, float]:
    import re

    text = metrics_client.get("/metrics").text
    out: dict[str, float] = {}
    line_re = re.compile(
        rf"^{re.escape(metric)}(?:\{{(?P<labels>[^}}]*)\}})?\s+(?P<value>[-+0-9eE.]+)$"
    )
    for raw in text.splitlines():
        match = line_re.match(raw)
        if match:
            out[match.group("labels") or ""] = float(match.group("value"))
    return out


def label_delta(pre: dict[str, float], post: dict[str, float], labels: str) -> float:
    return post.get(labels, 0) - pre.get(labels, 0)


def error_code(resp: httpx.Response) -> str | None:
    try:
        payload = resp.json()
    except ValueError:
        return None
    err = payload.get("error") if isinstance(payload, dict) else None
    if isinstance(err, dict):
        return err.get("code")
    return None
