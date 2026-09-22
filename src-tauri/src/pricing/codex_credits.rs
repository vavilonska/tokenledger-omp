//! Codex credit rate card, verified 2026-09-07:
//! https://learn.chatgpt.com/docs/pricing#token-rates
//! https://learn.chatgpt.com/docs/agent-configuration/speed
//! Kept separate from API catalogs, promotions, Batch/Flex and API Fast rates.
use super::{ModelPricing, TokenBreakdown};

pub const CREDITS_PER_USD: f64 = 25.0;

/// Returns USD equivalent at the user's anchor (2,500 credits = $100).
/// Only published credit rates are supported. Spark/unlisted models remain unknown.
pub fn equivalent_usd(pricing: &ModelPricing, model: &str, tokens: TokenBreakdown) -> Option<f64> {
    let canonical = pricing.supplement.canonical_name(model).unwrap_or(model);
    let base = canonical.strip_suffix("-fast").unwrap_or(canonical);
    let fast = tokens.is_fast || base != canonical;
    static DATED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"-\d{4}-?\d{2}-?\d{2}$").expect("valid date regex")
    });
    let base = DATED.replace(base, "");
    let (input, cached, output, fast_multiplier) = match base.as_ref() {
        "gpt-6-astra" => (250.0, 25.0, 1250.0, 2.5),
        "gpt-5.6-sol" => (100.0, 10.0, 500.0, 2.5),
        "gpt-5.6-terra" => (50.0, 5.0, 300.0, 2.5),
        "gpt-5.6-luna" => (5.0, 0.5, 30.0, 2.5),
        "gpt-5.6-cyber" => (312.5, 31.25, 1875.0, 2.5),
        "gpt-5.5" => (125.0, 12.5, 750.0, 2.5),
        "gpt-5.4" => (62.5, 6.25, 375.0, 2.0),
        "gpt-5.4-mini" => (18.75, 1.875, 113.0, 2.0),
        _ => return None,
    };
    // The credit card publishes input/read/output rates; cache writes are not charged.
    // Do not silently apply API-only long-context or discount rules to this card.
    Some(
        (tokens.input as f64 * input
            + tokens.cache_read as f64 * cached
            + tokens.output as f64 * output)
            / 1_000_000.0
            / CREDITS_PER_USD
            * if fast { fast_multiplier } else { 1.0 },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::test_bundled_pricing;

    #[test]
    fn astra_and_sol_credit_fast_differ_from_api_fast() {
        let pricing = test_bundled_pricing();
        for (model, api, credit) in [("gpt-6-astra", 20.0, 25.0), ("gpt-5.6-sol", 8.0, 10.0)] {
            let tokens = TokenBreakdown {
                input: 1_000_000,
                is_fast: true,
                ..Default::default()
            };
            assert_eq!(
                pricing.estimated_cost_dollars(model, tokens, false),
                Some(api)
            );
            assert_eq!(equivalent_usd(&pricing, model, tokens), Some(credit));
        }
    }

    #[test]
    fn card_does_not_inherit_api_discounts_cache_writes_or_long_context() {
        let pricing = test_bundled_pricing();
        let tokens = TokenBreakdown {
            input: 300_000,
            cache_read: 100_000,
            cache_write_5m: 100_000,
            output: 20_000,
            ..Default::default()
        };
        assert_eq!(equivalent_usd(&pricing, "gpt-6-astra", tokens), Some(4.1));
        assert_eq!(
            equivalent_usd(&pricing, "gpt-6-astra-ultra-fast", tokens),
            Some(10.25)
        );
        assert!(equivalent_usd(&pricing, "gpt-5.3-codex-spark", tokens).is_none());
        assert!(equivalent_usd(&pricing, "gpt-6-astra-unknown", tokens).is_none());
    }
}
