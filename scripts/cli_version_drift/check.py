#!/usr/bin/env python3

import argparse
import json
import re
import tempfile
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path


KEEP_RECENT = 4


@dataclass(frozen=True)
class Target:
    package: str
    relpath: str
    const_name: str


TARGETS = (
    Target(
        package="@anthropic-ai/claude-code",
        relpath="crates/gw-core/src/impersonate/claude_code.rs",
        const_name="PACKAGE_VERSION_POOL",
    ),
    Target(
        package="@google/gemini-cli",
        relpath="crates/gw-core/src/impersonate/gemini_cli.rs",
        const_name="CLI_VERSION_POOL",
    ),
    Target(
        package="@openai/codex",
        relpath="crates/gw-core/src/impersonate/openai_codex.rs",
        const_name="CODEX_VERSION_POOL",
    ),
)


@dataclass(frozen=True)
class Drift:
    target: Target
    latest: str
    before: tuple[str, ...]
    after: tuple[str, ...]

    @property
    def changed(self) -> bool:
        return self.before != self.after


def _const_re(const_name: str) -> re.Pattern[str]:
    return re.compile(
        rf"(?P<prefix>const\s+{re.escape(const_name)}\s*:\s*&\[\s*&str\s*\]\s*=\s*&\[)"
        r"(?P<body>[^\]]*)"
        r"(?P<suffix>\]\s*;)"
    )


def parse_const_pool(source: str, const_name: str) -> list[str]:
    match = _const_re(const_name).search(source)
    if not match:
        raise ValueError(f"missing const pool: {const_name}")
    return re.findall(r'"([^"]+)"', match.group("body"))


def next_pool(existing: list[str], latest: str) -> list[str]:
    versions = existing if latest in existing else [*existing, latest]
    return versions[-KEEP_RECENT:]


def render_const_pool(source: str, const_name: str, versions: list[str]) -> str:
    rendered = ", ".join(f'"{version}"' for version in versions)

    def replace(match: re.Match[str]) -> str:
        return f"{match.group('prefix')}{rendered}{match.group('suffix')}"

    updated, count = _const_re(const_name).subn(replace, source, count=1)
    if count != 1:
        raise ValueError(f"missing const pool: {const_name}")
    return updated


def fetch_latest_version(package: str) -> str:
    package_path = urllib.parse.quote(package, safe="")
    request = urllib.request.Request(
        f"https://registry.npmjs.org/{package_path}/latest",
        headers={
            "Accept": "application/json",
            "User-Agent": "ailinkmax-cli-version-drift/1.0",
        },
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        payload = json.loads(response.read().decode("utf-8"))
    version = payload.get("version")
    if not isinstance(version, str) or not version:
        raise ValueError(f"npm latest response for {package} did not include a version")
    return version


def check_target(root: Path, target: Target, latest: str | None = None) -> Drift:
    path = root / target.relpath
    source = path.read_text()
    before = parse_const_pool(source, target.const_name)
    latest = latest if latest is not None else fetch_latest_version(target.package)
    after = next_pool(before, latest)
    return Drift(target=target, latest=latest, before=tuple(before), after=tuple(after))


def write_target(root: Path, drift: Drift) -> None:
    path = root / drift.target.relpath
    source = path.read_text()
    path.write_text(render_const_pool(source, drift.target.const_name, list(drift.after)))


def run_check(root: Path, write: bool) -> int:
    drifts = [check_target(root, target) for target in TARGETS]
    changed = [drift for drift in drifts if drift.changed]
    if write:
        for drift in changed:
            write_target(root, drift)
            print(
                f"updated {drift.target.relpath} {drift.target.const_name}: "
                f"{list(drift.before)} -> {list(drift.after)}"
            )
        if not changed:
            print("no cli version drift")
        return 1 if changed else 0

    for drift in drifts:
        state = "missing" if drift.changed else "present"
        print(
            f"{drift.target.package} latest {drift.latest} {state} in "
            f"{drift.target.relpath}::{drift.target.const_name}"
        )
    return 1 if changed else 0


def run_selftest() -> None:
    source = 'const PACKAGE_VERSION_POOL: &[&str] = &["0.1.0", "0.1.1", "0.1.2", "0.1.3"];\n'
    assert parse_const_pool(source, "PACKAGE_VERSION_POOL") == ["0.1.0", "0.1.1", "0.1.2", "0.1.3"]
    assert next_pool(["0.1.0", "0.1.1", "0.1.2", "0.1.3"], "0.1.4") == [
        "0.1.1",
        "0.1.2",
        "0.1.3",
        "0.1.4",
    ]
    assert next_pool(["0.1.1", "0.1.2", "0.1.3", "0.1.4"], "0.1.4") == [
        "0.1.1",
        "0.1.2",
        "0.1.3",
        "0.1.4",
    ]
    assert next_pool(["0.1.0", "0.1.1", "0.1.2", "0.1.3", "0.1.4"], "0.1.4") == [
        "0.1.1",
        "0.1.2",
        "0.1.3",
        "0.1.4",
    ]
    updated = render_const_pool(source, "PACKAGE_VERSION_POOL", ["0.1.1", "0.1.2", "0.1.3", "0.1.4"])
    assert updated == 'const PACKAGE_VERSION_POOL: &[&str] = &["0.1.1", "0.1.2", "0.1.3", "0.1.4"];\n'
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        fixture = root / "crates/gw-core/src/impersonate/claude_code.rs"
        fixture.parent.mkdir(parents=True)
        fixture.write_text(source)
        drift = check_target(
            root,
            Target("@example/pkg", "crates/gw-core/src/impersonate/claude_code.rs", "PACKAGE_VERSION_POOL"),
            latest="0.1.4",
        )
        assert drift.changed
        write_target(root, drift)
        assert parse_const_pool(fixture.read_text(), "PACKAGE_VERSION_POOL") == [
            "0.1.1",
            "0.1.2",
            "0.1.3",
            "0.1.4",
        ]
    print("selftest ok")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--write", action="store_true", help="append missing npm latest versions to const pools")
    parser.add_argument("--selftest", action="store_true")
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    if args.selftest:
        run_selftest()
        return 0
    return run_check(args.repo_root, args.write)


if __name__ == "__main__":
    raise SystemExit(main())
