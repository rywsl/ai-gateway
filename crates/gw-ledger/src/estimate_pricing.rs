use gw_core::entity::EstimatePricing;

pub fn compute_freeze_estimate(
    pricing: &EstimatePricing,
    input_tokens: u64,
    request_max_tokens: Option<u64>,
    user_markup: f64,
) -> u64 {
    let max_output_by_hard_cap = (pricing.hard_cap_tokens as u64).saturating_sub(input_tokens);
    let output_cap = request_max_tokens
        .unwrap_or(pricing.default_max_tokens as u64)
        .min(max_output_by_hard_cap);
    let token_count = input_tokens.saturating_add(output_cap);
    let base = token_count.saturating_mul(pricing.conservative_per_token_micro);
    ((base as f64) * conservative_safety_factor(pricing.safety_factor) * user_markup).ceil() as u64
}

fn conservative_safety_factor(value: f64) -> f64 {
    if value.is_finite() {
        value.max(1.0)
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gw_core::entity::EstimatePricing;
    use gw_core::ids::ModelName;

    #[test]
    fn conservative_freeze_uses_input_plus_capped_output_and_safety_factor() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 10,
            safety_factor: 1.5,
            default_max_tokens: 100,
            hard_cap_tokens: 120,
        };

        let estimate = compute_freeze_estimate(&pricing, 20, Some(200), 1.0);

        assert_eq!(estimate, 1_800);
    }

    #[test]
    fn conservative_freeze_uses_default_output_when_request_omits_max_tokens() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 2,
            safety_factor: 1.0,
            default_max_tokens: 50,
            hard_cap_tokens: 100,
        };

        assert_eq!(compute_freeze_estimate(&pricing, 10, None, 2.0), 240);
    }

    #[test]
    fn conservative_freeze_clamps_bad_safety_factor() {
        let pricing = EstimatePricing {
            model: ModelName::from("gpt-4o"),
            conservative_per_token_micro: 10,
            safety_factor: 0.5,
            default_max_tokens: 10,
            hard_cap_tokens: 20,
        };

        assert_eq!(compute_freeze_estimate(&pricing, 10, None, 1.0), 200);
    }
}
