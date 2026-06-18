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
    print("PyYAML is required for check-flags-schema.sh", file=sys.stderr)
    sys.exit(2)

root = Path("flags")
schema_path = root / "_schema.yaml"
schema = yaml.safe_load(schema_path.read_text())["fields"]
ok = True

for path in sorted(root.glob("*.yaml")):
    if path.name == "_schema.yaml":
        continue
    data = yaml.safe_load(path.read_text())
    for key, spec in schema.items():
        if isinstance(spec, dict) and spec.get("required") and key not in data:
            print(f"{path}: missing required field {key}")
            ok = False
    if not re.match(r"^[a-z][a-z0-9_]*$", str(data.get("name", ""))):
        print(f"{path}: invalid name pattern")
        ok = False
    if data.get("name") != path.stem:
        print(f"{path}: name {data.get('name')} != filename")
        ok = False
    flag_type = data.get("type")
    if flag_type not in schema["type"]["enum"]:
        print(f"{path}: invalid type {flag_type}")
        ok = False
    if flag_type == "enum":
        values = data.get("enum_values")
        if not isinstance(values, list) or not values:
            print(f"{path}: enum flag must declare enum_values")
            ok = False
        elif data.get("default") not in values:
            print(f"{path}: default {data.get('default')} not in enum_values")
            ok = False
    if not isinstance(data.get("rollback_steps"), list) or not data.get("rollback_steps"):
        print(f"{path}: rollback_steps must be a non-empty list")
        ok = False
    if not isinstance(data.get("mixed_version_safe"), bool):
        print(f"{path}: mixed_version_safe must be bool")
        ok = False

if not ok:
    sys.exit(1)
PY
