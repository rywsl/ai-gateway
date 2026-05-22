"""Parse `crates/gw-core/src/error/codes.rs` for the set of GW_* error codes.

Used at orchestrator startup to validate (a) the variant→error_code
mapping in the spec's Appendix A, and (b) the all-codes-in-catalog
invariant (§7.1.4). We deliberately parse the source file at runtime
rather than embed a snapshot, so catalog drift fails the soak loudly
(per spec C6: no calibration phase, no silent staleness).
"""
from __future__ import annotations

import re
from pathlib import Path

_CONST_LINE = re.compile(r"^pub const (GW_[A-Z0-9_]+)\s*:")


def parse_codes_catalog(codes_rs_path: Path) -> set[str]:
    """Return the set of GW_* constants defined in `codes_rs_path`.

    Raises FileNotFoundError if the file does not exist (this is a hard
    fail: the soak cannot run without the catalog).
    """
    text = codes_rs_path.read_text(encoding="utf-8")
    out: set[str] = set()
    for line in text.splitlines():
        m = _CONST_LINE.match(line.strip())
        if m:
            out.add(m.group(1))
    if not out:
        raise RuntimeError(
            f"no GW_* constants parsed from {codes_rs_path} — file shape changed?"
        )
    return out
