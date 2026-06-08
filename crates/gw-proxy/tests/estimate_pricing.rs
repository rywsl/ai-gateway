use gw_core::entity::EstimatePricing;
use gw_core::ids::ModelName;

#[test]
fn request_freeze_estimate_uses_estimate_pricing_snapshot() {
    let pricing = EstimatePricing {
        model: ModelName::from("gpt-4o"),
        conservative_per_token_micro: 10,
        safety_factor: 1.0,
        default_max_tokens: 100,
        hard_cap_tokens: 200,
    };

    let estimate = gw_proxy::filters::request::freeze_estimate_with_pricing(
        20,
        Some(50),
        1.2,
        Some((&pricing, 9)),
    );

    assert_eq!(estimate.amount_micro, 840);
    assert_eq!(estimate.estimate_pricing_version, 9);
}

#[test]
fn request_freeze_estimate_falls_back_to_flat_formula_without_snapshot() {
    let estimate = gw_proxy::filters::request::freeze_estimate_with_pricing(20, None, 1.2, None);

    assert_eq!(estimate.amount_micro, 24);
    assert_eq!(estimate.estimate_pricing_version, 0);
}
