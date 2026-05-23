"""Panic-class log scan — spec §7.3 + C3.

Deliberately narrow: WARN/ERROR are NOT scanned (the gateway emits them
on normal cooldown / quota / health paths). The patterns here are
100%-bug signals — any match is a real bug to file, with no whitelist
escape hatch.
"""
from __future__ import annotations

import re

# Single regex with alternation; flags=re.IGNORECASE handles `fatal` and
# the panic-thread variants. `\b` on `fatal` avoids matching `fatality` /
# `fatalistic` etc. in arbitrary log payloads.
PANIC_PATTERN = (
    r"panicked at"
    r"|RUST_BACKTRACE"
    r"|thread '.*' panicked"
    r"|assertion failed"
    r"|\bfatal\b"
)

_RE = re.compile(PANIC_PATTERN, re.IGNORECASE)


def scan_panic_lines(text: str) -> list[str]:
    """Return every line in `text` that matches the panic pattern."""
    return [line for line in text.splitlines() if _RE.search(line)]
