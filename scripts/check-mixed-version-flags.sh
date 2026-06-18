#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

python3 - <<'PY'
from pathlib import Path
import re
import sys

try:
    import yaml
except ImportError:
    print("PyYAML is required for check-mixed-version-flags.sh", file=sys.stderr)
    sys.exit(2)

root = Path.cwd()
sibling = root.parent / ("ailinkmax-gateway" if root.name == "AILinkMax-Go" else "AILinkMax-Go")
errors = []

ledger = {}
for path in sorted((root / "flags").glob("*.yaml")):
    if path.name == "_schema.yaml":
        continue
    data = yaml.safe_load(path.read_text())
    ledger[data["name"]] = data

field_names = {
    "license_enforce": "LicenseEnforce",
    "jwt_strict": "JWTStrict",
    "payment_webhook_5xx": "PaymentWebhook5xx",
    "r7_502_on_parse_fail": "R7_502OnParseFail",
}

go_path = root / "shared/platform/config/flags.go"
if not go_path.exists():
    go_path = sibling / "shared/platform/config/flags.go"
if go_path.exists():
    go_src = go_path.read_text()
    go_defaults = {}
    for name, field in field_names.items():
        match = re.search(rf"{field}:\s*(License\w+|true|false),", go_src)
        if not match:
            continue
        value = match.group(1)
        if value.startswith("License"):
            value = value.removeprefix("License").lower()
        go_defaults[name] = value
    for name, data in ledger.items():
        if data.get("go_repo_path") is None:
            continue
        value = go_defaults.get(name)
        if value is None:
            errors.append(f"Go default for {name} not parseable")
        elif str(data["default"]).lower() != str(value).lower():
            errors.append(f"Go default for {name} = {value}, yaml = {data['default']}")

rs_path = root / "crates/gw-gateway/src/config.rs"
if not rs_path.exists():
    rs_path = sibling / "crates/gw-gateway/src/config.rs"
if rs_path.exists():
    rs_src = rs_path.read_text()
    rust_defaults = {}
    match = re.search(
        r"fn default_license_enforce_mode\(\) -> LicenseEnforceMode \{\s*LicenseEnforceMode::(\w+)",
        rs_src,
    )
    if match:
        rust_defaults["license_enforce"] = match.group(1).lower()
    block = re.search(r"impl Default for FlagsCfg \{.*?Self \{(.*?)\}\s*\}", rs_src, re.S)
    if block:
        for line in block.group(1).splitlines():
            match = re.match(r"\s*([a-z0-9_]+):\s*(true|false|default_\w+\(\)),", line)
            if not match:
                continue
            name, value = match.groups()
            if name == "license_enforce":
                continue
            rust_defaults[name] = value
    for name, data in ledger.items():
        if data.get("rust_repo_path") is None:
            continue
        value = rust_defaults.get(name)
        if value is None:
            errors.append(f"Rust default for {name} not parseable")
        elif str(data["default"]).lower() != str(value).lower():
            errors.append(f"Rust default for {name} = {value}, yaml = {data['default']}")

if errors:
    print("FAIL: default-value drift between yaml ledger and Go/Rust code:")
    for error in errors:
        print(f"  - {error}")
    sys.exit(1)

unsafe = sorted(name for name, data in ledger.items() if not data.get("mixed_version_safe"))
print(f"OK: {len(ledger)} flags consistent across yaml/Go/Rust; mixed-version unsafe={','.join(unsafe) or 'none'}")
PY
