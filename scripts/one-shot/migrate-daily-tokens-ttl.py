#!/usr/bin/env python3
"""
migrate-daily-tokens-ttl.py — T-W3-18 (WS-3 bounded memory).

One-shot migration that scans Redis for `counter:user:*:daily_tokens:*` keys
and applies a 90-day TTL to any key that currently has no TTL set
(TTL == -1, i.e. PERSIST).

Background
----------
`gw-store::counter_store::incrby_with_expire` (T-W3-17) couples INCRBY with an
EXPIRE retry. On the prod fleet, however, daily-token counters predating that
fix may have an INCRBY success + EXPIRE failure recorded against them — Redis
treats them as PERSIST, and they accumulate forever (1 key per user per day,
so 30 days of pre-fix traffic = 30 keys/user that never collapse).

This script reconciles that legacy state. It MUST be run once, after the
T-W3-17 fix is deployed, and is intentionally **not** a recurring sweeper:
REV-FINAL-V6 specifically rejects the v1 "常驻 sweeper" design — once
T-W3-17 is in place, the only keys without TTL are the historic ones, and a
single SCAN pass is enough.

Usage
-----
    REDIS_URL=redis://localhost:6379/0 python3 migrate-daily-tokens-ttl.py
    REDIS_URL=redis://localhost:6379/0 python3 migrate-daily-tokens-ttl.py --dry-run

Exit codes
----------
    0  success (printed `patched=N skipped=M`)
    1  configuration error (REDIS_URL missing, redis import unavailable, …)
    2  runtime error during SCAN/TTL/EXPIRE

Safety
------
- Uses SCAN with COUNT=1000 (no KEYS) — does not block the Redis main thread.
- Only touches keys with TTL exactly -1 (never -2 = already gone; never >=0).
- `--dry-run` reports the patched count without issuing any EXPIRE call.
"""

import argparse
import os
import sys


TTL_SECONDS = 90 * 86400  # 90 days
KEY_PATTERN = "counter:user:*:daily_tokens:*"
SCAN_COUNT = 1000


def main() -> int:
    parser = argparse.ArgumentParser(
        description="One-shot TTL backfill for counter:user:*:daily_tokens:* keys (T-W3-18).",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Count keys that would be patched but do not issue EXPIRE.",
    )
    args = parser.parse_args()

    url = os.environ.get("REDIS_URL")
    if not url:
        print("error: REDIS_URL env var is required", file=sys.stderr)
        return 1

    try:
        import redis  # type: ignore[import]
    except ImportError:
        print(
            "error: the `redis` package is required (pip install redis)",
            file=sys.stderr,
        )
        return 1

    try:
        r = redis.from_url(url)
    except Exception as exc:  # pragma: no cover - connection errors
        print(f"error: cannot connect to {url}: {exc}", file=sys.stderr)
        return 2

    cursor = 0
    patched = 0
    skipped_with_ttl = 0
    skipped_gone = 0

    try:
        while True:
            cursor, keys = r.scan(
                cursor=cursor, match=KEY_PATTERN, count=SCAN_COUNT,
            )
            for key in keys:
                ttl = r.ttl(key)
                if ttl == -1:
                    if not args.dry_run:
                        r.expire(key, TTL_SECONDS)
                    patched += 1
                elif ttl == -2:
                    # Race: key vanished between SCAN and TTL. Harmless.
                    skipped_gone += 1
                else:
                    skipped_with_ttl += 1
            if cursor == 0:
                break
    except Exception as exc:  # pragma: no cover
        print(f"error: SCAN/TTL/EXPIRE failed: {exc}", file=sys.stderr)
        return 2

    mode = "dry-run" if args.dry_run else "applied"
    print(
        f"patched={patched} skipped_with_ttl={skipped_with_ttl} "
        f"skipped_gone={skipped_gone} mode={mode} ttl_secs={TTL_SECONDS}",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
