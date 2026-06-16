"""HTTP helpers shared by cnexus tunnel scenarios."""

from __future__ import annotations

from dataclasses import dataclass

import httpx


CHAT_PATH = "/v1/chat/completions"


def chat_headers(api_key: str, key_id: str) -> dict[str, str]:
    return {
        "x-api-key": api_key,
        "x-api-key-id": key_id,
        "content-type": "application/json",
    }


def post_chat(client: httpx.Client, ids: dict, *, stream: bool = False) -> httpx.Response:
    return client.post(
        CHAT_PATH,
        headers=chat_headers(ids["api_key_plaintext"], ids["api_key_id"]),
        json={
            "model": ids["model"],
            "messages": [{"role": "user", "content": "hi"}],
            "stream": stream,
        },
        timeout=15.0,
    )


@dataclass(frozen=True)
class Pair:
    direct: httpx.Response
    tunnel: httpx.Response

    def bodies_match(self) -> bool:
        return self.direct.content == self.tunnel.content

    def status_match(self) -> bool:
        return self.direct.status_code == self.tunnel.status_code


def direct_vs_tunnel(client: httpx.Client, direct_ids: dict, tunnel_ids: dict) -> Pair:
    return Pair(
        direct=post_chat(client, direct_ids, stream=True),
        tunnel=post_chat(client, tunnel_ids, stream=True),
    )
