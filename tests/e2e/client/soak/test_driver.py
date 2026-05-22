"""Unit tests for the soak driver scaffolding (no live HTTP)."""
from soak.driver import SoakConfig, SoakDriverReport


def test_soak_config_defaults_match_spec_11_3():
    # Spec §11.3 — full row. Any change requires updating the spec first.
    c = SoakConfig()
    assert c.duration_s == 60
    assert c.target_rps == 833
    assert c.n_threads == 8
    assert c.max_inflight_per_thread == 64
    assert c.per_request_timeout_s == 30
    assert c.drain_grace_s == 5
    assert c.drain_quiescence_s == 10
    assert c.drain_grace_max_s == 60
    assert c.n_users == 20
    assert c.chaos_mode == "off"


def test_soak_config_smoke():
    # Spec §11.3 — smoke column.
    c = SoakConfig.smoke()
    assert c.duration_s == 10
    assert c.target_rps == 100
    assert c.n_threads == 2
    assert c.max_inflight_per_thread == 16
    assert c.n_users == 5


def test_driver_report_target_count_is_product():
    rpt = SoakDriverReport.empty(target_count=60 * 833)
    assert rpt.target_count == 49980
    assert rpt.requests_sent == 0
    assert rpt.requests_completed == 0
