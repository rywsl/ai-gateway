#!/usr/bin/env python3

import argparse
import json
import re
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import quote


ROOT = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class Target:
    name: str
    path: Path
    const: str
    package: str


TARGETS = (
    Target(
        "claude_code",
        Path("crates/gw-core/src/impersonate/claude_code.rs"),
        "PACKAGE_VERSION_POOL",
        "@anthropic-ai/claude-code",
    ),
    Target(
        "gemini_cli",
        Path("crates/gw-core/src/impersonate/gemini_cli.rs"),
        "CLI_VERSION_POOL",
        "@google/gemini-cli",
    ),
    Target(
        "openai_codex",
        Path("crates/gw-core/src/impersonate/openai_codex.rs"),
        "CODEX_VERSION_POOL",
        "@openai/codex",
    ),
)


def latest_npm(pkg: str) -> str:
    url = f"https://registry.npmjs.org/{quote(pkg, safe='@/')}/latest"
    request = urllib.request.Request(
        url,
        headers={
            "Accept": "application/json",
            "User-Agent": "ailinkmax-gateway-cli-version-drift",
        },
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        data = json.load(response)
    version = data.get("version")
    if not isinstance(version, str) or not version:
        raise ValueError(f"npm latest response for {pkg} did not include version")
    return version


def _const_re(const: str) -> re.Pattern[str]:
    return re.compile(
        rf"const\s+{re.escape(const)}\s*:\s*&\[\s*&str\s*\]\s*=\s*&\[(?P<body>.*?)\]\s*;",
        re.DOTALL,
    )


def current_pool(text: str, const: str) -> list[str]:
    match = _const_re(const).search(text)
    if not match:
        raise ValueError(f"could not find const {const}")
    return re.findall(r'"([^"]+)"', match.group("body"))


def rewrite(text: str, const: str, pool: list[str]) -> str:
    replacement = f'const {const}: &[&str] = &[{", ".join(json.dumps(v) for v in pool)}];'
    new_text, count = _const_re(const).subn(replacement, text, count=1)
    if count != 1:
        raise ValueError(f"could not rewrite const {const}")
    return new_text


def _refresh_pool(pool: list[str], latest: str) -> list[str]:
    if latest in pool:
        return pool
    return [*pool, latest][-4:]


def selftest() -> int:
    sample = """
fn before() {}
const CODEX_VERSION_POOL: &[&str] = &["0.4.0", "0.4.1", "0.5.0"];
fn after() {}
"""
    parsed = current_pool(sample, "CODEX_VERSION_POOL")
    assert parsed == ["0.4.0", "0.4.1", "0.5.0"], parsed
    rewritten = rewrite(sample, "CODEX_VERSION_POOL", ["0.4.1", "0.5.0", "0.6.0", "0.7.0"])
    assert (
        'const CODEX_VERSION_POOL: &[&str] = &["0.4.1", "0.5.0", "0.6.0", "0.7.0"];'
        in rewritten
    ), rewritten
    assert current_pool(rewritten, "CODEX_VERSION_POOL") == [
        "0.4.1",
        "0.5.0",
        "0.6.0",
        "0.7.0",
    ]
    print("selftest ok")
    return 0


def main(write: bool) -> int:
    drift_found = False
    for target in TARGETS:
        path = ROOT / target.path
        text = path.read_text()
        pool = current_pool(text, target.const)
        latest = latest_npm(target.package)
        if latest in pool:
            continue

        drift_found = True
        refreshed = _refresh_pool(pool, latest)
        print(
            f"DRIFT {target.name}: {target.package} latest={latest} "
            f"pool={','.join(pool)}"
        )
        if write:
            path.write_text(rewrite(text, target.const, refreshed))

    return 0 if write else int(drift_found)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="rewrite drifted pools")
    parser.add_argument("--selftest", action="store_true", help="run offline parse/rewrite selftest")
    args = parser.parse_args()

    if args.selftest:
        raise SystemExit(selftest())

    try:
        raise SystemExit(main(args.write))
    except (OSError, urllib.error.URLError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(2)
