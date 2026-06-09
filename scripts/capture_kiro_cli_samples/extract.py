#!/usr/bin/env python3
"""Offline Kiro CLI mitmproxy-flow redaction and evidence extraction.

This tool reads a mitmproxy dump or a small HAR-like JSON export, filters flows
that look relevant to Kiro CLI / comparison sample collection, and writes only
redacted artifacts. Raw flow files must stay outside this repository.
"""

from __future__ import annotations

import argparse
import base64
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable
from urllib.parse import parse_qsl, urlsplit


SENSITIVE_KEY_RE = re.compile(
    r"(^|[-_.])("
    r"authorization|auth|bearer|token|api[-_]?key|key|cookie|set[-_]?cookie|"
    r"signature|sig|credential|secret|session|account|user|userid|user[-_]?id|"
    r"machine|device|installation|identity|client[-_]?id|access[-_]?key|"
    r"refresh[-_]?token|id[-_]?token|jwt|password|passcode|quota[-_]?project"
    r")($|[-_.])",
    re.IGNORECASE,
)

SENSITIVE_VALUE_PATTERNS = (
    re.compile(r"Bearer\s+[A-Za-z0-9._~+/=-]+", re.IGNORECASE),
    re.compile(r"AWS4-HMAC-SHA256\s+[A-Za-z0-9_.,/=:;+\-\s]+", re.IGNORECASE),
    re.compile(r"(X-Amz-Signature=)[A-Fa-f0-9]{16,}"),
    re.compile(r"(Signature=)[A-Za-z0-9%+/=_-]{16,}", re.IGNORECASE),
    re.compile(r"(Credential=)[A-Za-z0-9%/+=:_-]{8,}", re.IGNORECASE),
    re.compile(r"(?i)(session|token|cookie|secret|signature)=([^;&\s]+)"),
)
SENSITIVE_JSON_VALUE_RE = re.compile(
    r'("(?P<key>[^"]*(?:authorization|auth|bearer|token|api[-_]?key|key|cookie|signature|sig|credential|secret|session|account|user[-_]?id|userid|machine|device|installation|identity|client[-_]?id|access[-_]?key|refresh[-_]?token|id[-_]?token|jwt|password|passcode|quota[-_]?project)[^"]*)"\s*:\s*)"(?P<value>[^"]*)"',
    re.IGNORECASE,
)

KIRO_HINT_RE = re.compile(r"kiro|amazonaws|bedrock|qdeveloper|codewhisperer", re.IGNORECASE)
COMPARISON_HINT_RE = re.compile(r"openai|anthropic|google|gemini|codex|claude", re.IGNORECASE)
STREAM_HINT_RE = re.compile(r"text/event-stream|eventstream|streamGenerateContent|data:|event:", re.IGNORECASE)
TOKEN_COUNT_KEYS = {
    "token_count",
    "token_counts",
    "input_tokens",
    "output_tokens",
    "total_tokens",
    "prompt_tokens",
    "completion_tokens",
    "cached_tokens",
}

MAX_BODY_PREVIEW = 16000
MAX_CHUNKS = 40
MAX_CHUNK_PREVIEW = 1200


@dataclass
class SimpleFlow:
    method: str
    url: str
    request_headers: dict[str, str]
    request_body: bytes
    status_code: int | None
    response_headers: dict[str, str]
    response_body: bytes


class StableRedactor:
    def __init__(self) -> None:
        self._values: dict[tuple[str, str], str] = {}
        self._counts: dict[str, int] = {}

    def placeholder(self, kind: str, value: Any = "") -> str:
        raw = "" if value is None else str(value)
        key = (kind, raw)
        if key not in self._values:
            self._counts[kind] = self._counts.get(kind, 0) + 1
            self._values[key] = f"<redacted-{kind}-{self._counts[kind]}>"
        return self._values[key]

    def redact_value(self, key: str, value: Any) -> Any:
        if _is_token_count_key(key):
            return value
        kind = _sensitive_kind(key)
        if kind:
            return self.placeholder(kind, value)
        if isinstance(value, str):
            return self.redact_string(value)
        return value

    def redact_string(self, value: str) -> str:
        out = value
        for pattern in SENSITIVE_VALUE_PATTERNS:
            out = pattern.sub(lambda match: _preserve_assignment(match, self), out)
        return _redact_json_string_assignments(out, self)

    def redact_json(self, value: Any, key: str = "") -> Any:
        if isinstance(value, dict):
            return {str(k): self.redact_json(v, str(k)) for k, v in value.items()}
        if isinstance(value, list):
            return [self.redact_json(item, key) for item in value]
        return self.redact_value(key, value)


def _preserve_assignment(match: re.Match[str], redactor: StableRedactor) -> str:
    text = match.group(0)
    lowered = text.lower()
    if lowered.startswith("bearer "):
        return "Bearer " + redactor.placeholder("token", text[7:])
    if lowered.startswith("aws4-hmac-sha256"):
        return redactor.placeholder("signature", text)
    if len(match.groups()) >= 2 and match.group(1).lower() in {"session", "token", "cookie", "secret", "signature"}:
        return f"{match.group(1)}={redactor.placeholder(_sensitive_kind(match.group(1)) or 'secret', match.group(2))}"
    if len(match.groups()) >= 1:
        return match.group(1) + redactor.placeholder("signature", text)
    return redactor.placeholder("secret", text)


def _redact_json_string_assignments(value: str, redactor: StableRedactor) -> str:
    def repl(match: re.Match[str]) -> str:
        kind = _sensitive_kind(match.group("key")) or "secret"
        return f'{match.group(1)}"{redactor.placeholder(kind, match.group("value"))}"'

    return SENSITIVE_JSON_VALUE_RE.sub(repl, value)


def _sensitive_kind(key: str) -> str | None:
    normalized = _normalize_key(key)
    if normalized in {"user-agent", "tokens"}:
        return None if normalized == "user-agent" else "token"
    if not SENSITIVE_KEY_RE.search(normalized):
        return None
    if "cookie" in normalized:
        return "cookie"
    if "signature" in normalized or normalized.endswith("-sig") or "credential" in normalized:
        return "signature"
    if "machine" in normalized or "device" in normalized or "installation" in normalized:
        return "machine"
    if "account" in normalized:
        return "account"
    if "user" in normalized or "identity" in normalized or "client-id" in normalized:
        return "user"
    if "session" in normalized:
        return "session"
    if "auth" in normalized or "token" in normalized or "key" in normalized or "jwt" in normalized:
        return "token"
    return "secret"


def _is_token_count_key(key: str) -> bool:
    return _normalize_key(key).replace("-", "_") in TOKEN_COUNT_KEYS


def _normalize_key(key: str) -> str:
    with_separators = re.sub(r"([a-z0-9])([A-Z])", r"\1-\2", key)
    with_separators = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1-\2", with_separators)
    return with_separators.lower().replace("_", "-").replace(".", "-")


def load_flows(path: Path) -> list[SimpleFlow]:
    try:
        return load_mitmproxy_flows(path)
    except ModuleNotFoundError as exc:
        if exc.name != "mitmproxy":
            raise
        try:
            return load_json_flows(path)
        except Exception as json_exc:  # noqa: BLE001 - produce operator-friendly fallback message
            raise SystemExit(
                "ERROR: mitmproxy Python API is not installed and the input was not a "
                "supported JSON fallback. Install with `python3 -m pip install mitmproxy` "
                f"or pass a HAR-like JSON export. JSON fallback error: {json_exc}"
            ) from exc
    except Exception as mitm_exc:  # noqa: BLE001
        try:
            return load_json_flows(path)
        except Exception as json_exc:  # noqa: BLE001
            raise SystemExit(
                f"ERROR: failed to read {path} as mitmproxy flow ({mitm_exc}) or JSON fallback ({json_exc})."
            ) from mitm_exc


def load_mitmproxy_flows(path: Path) -> list[SimpleFlow]:
    from mitmproxy import io  # type: ignore[import-not-found]
    from mitmproxy.exceptions import FlowReadException  # type: ignore[import-not-found]

    out: list[SimpleFlow] = []
    try:
        with path.open("rb") as fh:
            reader = io.FlowReader(fh)
            for flow in reader.stream():
                request = getattr(flow, "request", None)
                if request is None:
                    continue
                response = getattr(flow, "response", None)
                out.append(
                    SimpleFlow(
                        method=request.method or "",
                        url=request.pretty_url or request.url or "",
                        request_headers={str(k): str(v) for k, v in request.headers.items()},
                        request_body=bytes(request.raw_content or b""),
                        status_code=getattr(response, "status_code", None) if response else None,
                        response_headers={str(k): str(v) for k, v in response.headers.items()} if response else {},
                        response_body=bytes(getattr(response, "raw_content", None) or b"") if response else b"",
                    )
                )
    except FlowReadException as exc:
        raise ValueError(str(exc)) from exc
    return out


def load_json_flows(path: Path) -> list[SimpleFlow]:
    data = json.loads(path.read_text(encoding="utf-8"))
    entries = data.get("log", {}).get("entries", data if isinstance(data, list) else data.get("entries", []))
    out: list[SimpleFlow] = []
    for entry in entries:
        req = entry.get("request", entry)
        resp = entry.get("response", {})
        url = req.get("url", "")
        request_body = _json_body_bytes(req.get("postData", req.get("body", "")))
        response_body = _json_body_bytes(resp.get("content", resp.get("body", "")))
        out.append(
            SimpleFlow(
                method=req.get("method", ""),
                url=url,
                request_headers=_headers_from_json(req.get("headers", {})),
                request_body=request_body,
                status_code=resp.get("status"),
                response_headers=_headers_from_json(resp.get("headers", {})),
                response_body=response_body,
            )
        )
    if not out:
        raise ValueError("no JSON flow entries found")
    return out


def _headers_from_json(value: Any) -> dict[str, str]:
    if isinstance(value, dict):
        return {str(k): str(v) for k, v in value.items()}
    if isinstance(value, list):
        out = {}
        for item in value:
            if isinstance(item, dict) and "name" in item:
                out[str(item["name"])] = str(item.get("value", ""))
        return out
    return {}


def _json_body_bytes(value: Any) -> bytes:
    if isinstance(value, bytes):
        return value
    if isinstance(value, str):
        return value.encode("utf-8", "replace")
    if isinstance(value, dict):
        if value.get("encoding") == "base64" and isinstance(value.get("text"), str):
            return base64.b64decode(value["text"])
        if isinstance(value.get("text"), str):
            return value["text"].encode("utf-8", "replace")
        if "mimeType" in value and "text" not in value:
            return b""
    return json.dumps(value, ensure_ascii=False, sort_keys=True).encode("utf-8")


def flow_matches(flow: SimpleFlow) -> bool:
    haystack = "\n".join(
        [
            flow.url,
            json.dumps(flow.request_headers),
            _decode(flow.request_body[:4096]),
            json.dumps(flow.response_headers),
            _decode(flow.response_body[:4096]),
        ]
    )
    return bool(KIRO_HINT_RE.search(haystack) or COMPARISON_HINT_RE.search(haystack))


def classify_flow(flow: SimpleFlow) -> str:
    if flow.status_code and flow.status_code >= 400:
        return "error"
    text = "\n".join(
        [
            json.dumps(flow.request_headers),
            json.dumps(flow.response_headers),
            _decode(flow.request_body[:4096]),
            _decode(flow.response_body[:4096]),
        ]
    )
    if STREAM_HINT_RE.search(text):
        return "stream"
    if COMPARISON_HINT_RE.search(flow.url + text) and not KIRO_HINT_RE.search(flow.url + text):
        return "comparison"
    return "non-stream"


def summarize_flow(flow: SimpleFlow, index: int, redactor: StableRedactor) -> dict[str, Any]:
    parsed = urlsplit(flow.url)
    query_pairs = parse_qsl(parsed.query, keep_blank_values=True)
    redacted_query = {key: redactor.redact_value(key, value) for key, value in query_pairs}
    request_preview = redact_body_preview(flow.request_body, redactor)
    response_preview = redact_body_preview(flow.response_body, redactor)
    return {
        "index": index,
        "category": classify_flow(flow),
        "request": {
            "method": flow.method,
            "scheme": parsed.scheme,
            "host": parsed.netloc,
            "path": parsed.path,
            "query_keys": [key for key, _ in query_pairs],
            "query": redacted_query,
            "headers": redact_headers(flow.request_headers, redactor),
            "body_preview": request_preview,
        },
        "response": {
            "status": flow.status_code,
            "headers": redact_headers(flow.response_headers, redactor),
            "body_preview": response_preview,
            "stream_hints": stream_hints(flow.response_body, redactor),
        },
    }


def redact_headers(headers: dict[str, str], redactor: StableRedactor) -> dict[str, str]:
    return {name: str(redactor.redact_value(name, value)) for name, value in sorted(headers.items(), key=lambda item: item[0].lower())}


def redact_body_preview(body: bytes, redactor: StableRedactor) -> Any:
    text = _decode(body[:MAX_BODY_PREVIEW])
    if not text:
        return ""
    parsed = _parse_json(text)
    if parsed is not None:
        return redactor.redact_json(parsed)
    return redactor.redact_string(text)


def stream_hints(body: bytes, redactor: StableRedactor) -> list[dict[str, Any]]:
    text = _decode(body[:MAX_BODY_PREVIEW])
    hints: list[dict[str, Any]] = []
    current_event = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("event:"):
            current_event = stripped.partition(":")[2].strip()
            hints.append({"type": "event", "name": current_event})
        elif stripped.startswith("data:"):
            data = stripped.partition(":")[2].strip()
            parsed = _parse_json(data)
            hints.append(
                {
                    "type": "data",
                    "event": current_event,
                    "preview": redactor.redact_json(parsed) if parsed is not None else redactor.redact_string(data[:MAX_CHUNK_PREVIEW]),
                }
            )
        elif "event" in stripped.lower() or "usage" in stripped.lower() or "token" in stripped.lower():
            parsed = _parse_json(stripped)
            hints.append(
                {
                    "type": "chunk",
                    "preview": redactor.redact_json(parsed) if parsed is not None else redactor.redact_string(stripped[:MAX_CHUNK_PREVIEW]),
                }
            )
        if len(hints) >= MAX_CHUNKS:
            break
    return hints


def write_artifacts(records: list[dict[str, Any]], output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    categories = ("stream", "non-stream", "error", "comparison")
    write_jsonl(output_dir / "redacted-evidence.jsonl", records)
    for category in categories:
        write_jsonl(output_dir / f"{category}.jsonl", [record for record in records if record["category"] == category])
    (output_dir / "redacted-evidence.md").write_text(render_markdown(records), encoding="utf-8")


def write_jsonl(path: Path, records: Iterable[dict[str, Any]]) -> None:
    with path.open("w", encoding="utf-8") as fh:
        for record in records:
            fh.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")


def render_markdown(records: list[dict[str, Any]]) -> str:
    lines = [
        "# Redacted Kiro CLI Sample Evidence",
        "",
        "Raw mitmproxy flow files are intentionally not copied here. This file contains redacted previews only.",
        "",
        "## Summary",
        "",
    ]
    counts = {category: sum(1 for record in records if record["category"] == category) for category in ("stream", "non-stream", "error", "comparison")}
    for category, count in counts.items():
        lines.append(f"- {category}: {count}")
    for record in records:
        request = record["request"]
        response = record["response"]
        lines.extend(
            [
                "",
                f"## Flow {record['index']} - {record['category']}",
                "",
                f"- Request: `{request['method']} {request['path']}`",
                f"- Query keys: `{', '.join(request['query_keys'])}`",
                f"- Response status: `{response['status']}`",
                "",
                "### Request Headers",
                "",
                "```json",
                json.dumps(request["headers"], indent=2, ensure_ascii=False, sort_keys=True),
                "```",
                "",
                "### Request Body Preview",
                "",
                "```json",
                json.dumps(request["body_preview"], indent=2, ensure_ascii=False, sort_keys=True),
                "```",
                "",
                "### Response Headers",
                "",
                "```json",
                json.dumps(response["headers"], indent=2, ensure_ascii=False, sort_keys=True),
                "```",
                "",
                "### Response Body Preview",
                "",
                "```json",
                json.dumps(response["body_preview"], indent=2, ensure_ascii=False, sort_keys=True),
                "```",
                "",
                "### Stream Chunk/Event Hints",
                "",
                "```json",
                json.dumps(response["stream_hints"], indent=2, ensure_ascii=False, sort_keys=True),
                "```",
            ]
        )
    lines.append("")
    return "\n".join(lines)


def _decode(body: bytes) -> str:
    return body.decode("utf-8", "replace")


def _parse_json(text: str) -> Any | None:
    stripped = text.strip()
    if not stripped:
        return None
    try:
        return json.loads(stripped)
    except json.JSONDecodeError:
        return None


def run_self_test() -> None:
    redactor = StableRedactor()
    sample = {
        "headers": {
            "Authorization": "Bearer sk-live-secret",
            "Cookie": "sid=abc; token=xyz",
            "X-Amz-Content-Sha256": "abc123",
            "X-Amz-Security-Token": "aws-session-token",
            "X-User-Id": "user@example.com",
            "X-Machine-Id": "machine-123",
        },
        "query": {
            "X-Amz-Signature": "a" * 64,
            "session_id": "sess-123",
            "token": "query-secret-token",
            "safe": "keep",
        },
        "json": {
            "accountId": "acct-1",
            "machineId": "machine-camel",
            "deviceId": "device-camel",
            "sessionId": "session-camel",
            "accessKeyId": "access-key-camel",
            "refreshToken": "refresh-camel",
            "credentialId": "credential-camel",
            "outputTokens": 46,
            "token": "json-secret-token",
            "tokens": "json-secret-token-list",
            "input_tokens": 123,
            "output_tokens": 45,
            "token_count": 168,
            "event": "message_stop",
            "nested": {"refresh_token": "refresh-secret", "text": "ok"},
        },
        "aws": "AWS4-HMAC-SHA256 Credential=AKIA/20260610/us-east-1/service/aws4_request, SignedHeaders=host, Signature=abcdef1234567890abcdef",
    }
    redacted = redactor.redact_json(sample)
    rendered = json.dumps(redacted, sort_keys=True)
    forbidden = [
        "sk-live-secret",
        "sid=abc",
        "token=xyz",
        "aws-session-token",
        "user@example.com",
        "machine-123",
        "machine-camel",
        "device-camel",
        "session-camel",
        "access-key-camel",
        "refresh-camel",
        "credential-camel",
        "acct-1",
        "sess-123",
        "query-secret-token",
        "json-secret-token",
        "json-secret-token-list",
        "refresh-secret",
        "abcdef1234567890abcdef",
    ]
    leaks = [value for value in forbidden if value in rendered]
    assert not leaks, f"redaction leaked: {leaks}"
    assert '"input_tokens": 123' in rendered
    assert '"output_tokens": 45' in rendered
    assert '"outputTokens": 46' in rendered
    assert '"token_count": 168' in rendered
    assert '"event": "message_stop"' in rendered
    assert '"safe": "keep"' in rendered
    assert "<redacted-token-1>" in rendered or "<redacted-token-2>" in rendered
    print("redaction self-test passed")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="run redaction smoke tests")
    parser.add_argument("--input", "-i", type=Path, help="mitmproxy flow file or HAR-like JSON input")
    parser.add_argument("--output-dir", "-o", type=Path, help="directory for redacted artifacts")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    if args.self_test:
        run_self_test()
        return 0
    if not args.input or not args.output_dir:
        raise SystemExit("ERROR: expected --input <flow> and --output-dir <dir>, or --self-test")
    flows = [flow for flow in load_flows(args.input) if flow_matches(flow)]
    redactor = StableRedactor()
    records = [summarize_flow(flow, index + 1, redactor) for index, flow in enumerate(flows)]
    if not records:
        raise SystemExit(
            "ERROR: no Kiro CLI or comparison flows matched the input. "
            "Stop W9 intake here: capture real stream, non-stream, error, and comparison traffic before implementation."
        )
    write_artifacts(records, args.output_dir)
    print(f"wrote {len(records)} redacted flow record(s) to {args.output_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
