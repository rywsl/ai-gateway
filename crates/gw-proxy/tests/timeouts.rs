//! G4 — Vendor.timeout_overrides precedence.
//!
//! Ladder: hardcoded default < Vendor.timeout_overrides < Pricing timeouts.
//! Pricing wins (most specific, per-model); Vendor wins over defaults so
//! operators can tune at the vendor level. Fields overlay independently.

use gw_proxy::filters::upstream::apply_timeout_precedence;

use gw_core::entity::{Pricing, TimeoutSpec, Vendor};

fn ts(connect: Option<u32>, read: Option<u32>) -> TimeoutSpec {
    TimeoutSpec {
        connect_ms: connect,
        read_ms: read,
        total_ms: None,
        stream_total_ms: None,
    }
}

fn vendor_with(timeouts: Option<TimeoutSpec>) -> Vendor {
    Vendor {
        id: "v-test".into(),
        display_name: "Test Vendor".to_string(),
        base_urls: Vec::new(),
        auth_template: String::new(),
        timeout_overrides: timeouts,
        default_proxy: None,
        contact: None,
        egress_mode: Default::default(),
    }
}

fn pricing_with(connect_ms: Option<u32>, read_ms: Option<u32>) -> Pricing {
    Pricing {
        model: "gpt-test".into(),
        input_price_micro_per_token: 0,
        output_price_micro_per_token: 0,
        max_output: 0,
        model_markup: 0.0,
        connect_timeout_ms: connect_ms,
        read_timeout_ms: read_ms,
        total_timeout_ms: None,
        stream_total_timeout_ms: None,
    }
}

#[test]
fn pricing_wins_over_vendor_when_both_set() {
    let v = vendor_with(Some(ts(Some(1000), None)));
    let p = pricing_with(Some(500), None);
    let eff = apply_timeout_precedence(&v, &p);
    assert_eq!(eff.connect_ms, Some(500));
}

#[test]
fn vendor_fills_when_pricing_absent() {
    let v = vendor_with(Some(ts(None, Some(30000))));
    let p = pricing_with(None, None);
    let eff = apply_timeout_precedence(&v, &p);
    assert_eq!(eff.read_ms, Some(30000));
}

#[test]
fn neither_means_none() {
    let v = vendor_with(None);
    let p = pricing_with(None, None);
    let eff = apply_timeout_precedence(&v, &p);
    assert!(eff.connect_ms.is_none() && eff.read_ms.is_none());
}

#[test]
fn pricing_overrides_vendor_field_by_field() {
    let v = vendor_with(Some(ts(Some(1000), Some(2000))));
    let p = pricing_with(Some(500), None);
    let eff = apply_timeout_precedence(&v, &p);
    assert_eq!(eff.connect_ms, Some(500)); // pricing wins
    assert_eq!(eff.read_ms, Some(2000)); // vendor unchanged
}

#[test]
fn pricing_total_overlays_when_set() {
    // Pricing has a flat total_timeout_ms field in v1; confirm it overlays
    // the Vendor.timeout_overrides.total_ms rung.
    let v = vendor_with(Some(TimeoutSpec {
        connect_ms: None,
        read_ms: None,
        total_ms: Some(9000),
        stream_total_ms: None,
    }));
    let mut p = pricing_with(None, None);
    p.total_timeout_ms = Some(4000);
    let eff = apply_timeout_precedence(&v, &p);
    assert_eq!(eff.total_ms, Some(4000)); // pricing wins
}
