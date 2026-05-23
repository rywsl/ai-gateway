from soak.log_scan import PANIC_PATTERN, scan_panic_lines


SAMPLE_GOOD = """\
2026-05-22T14:30:00Z INFO  gateway: started
2026-05-22T14:30:01Z WARN  gw_health: key vk-1 cooled down for 30s
2026-05-22T14:30:02Z ERROR gw_routing: no available channel for group g
2026-05-22T14:30:03Z INFO  gateway: served 1000 requests
"""

SAMPLE_BAD = """\
2026-05-22T14:30:42.123Z gateway thread 'tokio-runtime-worker' panicked at 'index out of bounds: the len is 4 but the index is 99', src/foo.rs:42
note: run with `RUST_BACKTRACE=1` for backtrace
2026-05-22T14:30:43.000Z gateway assertion failed: x == y
"""


def test_scan_panic_lines_passes_normal_warn_error():
    matched = scan_panic_lines(SAMPLE_GOOD)
    assert matched == []


def test_scan_panic_lines_catches_panic_and_assertion():
    matched = scan_panic_lines(SAMPLE_BAD)
    assert len(matched) == 3
    assert any("panicked at" in m for m in matched)
    assert any("RUST_BACKTRACE" in m for m in matched)
    assert any("assertion failed" in m for m in matched)


def test_scan_panic_pattern_is_exposed_for_reporting():
    # Spec §7.1.1 — the report includes the exact pattern that was matched,
    # so consumers can see what was searched.
    assert "panicked at" in PANIC_PATTERN
    assert "RUST_BACKTRACE" in PANIC_PATTERN
    assert "assertion failed" in PANIC_PATTERN
    assert "fatal" in PANIC_PATTERN.lower()
