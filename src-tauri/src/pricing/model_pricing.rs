use std::{collections::HashMap, sync::Mutex};

use super::{ModelRates, PricingCatalog, PricingSupplement, TokenBreakdown};

pub struct ModelPricing {
    pub codex_credit_mode: bool,
    pub supplement: PricingSupplement,
    pub primary: PricingCatalog,
    pub secondary: PricingCatalog,
    memo: Mutex<HashMap<String, Option<ModelRates>>>,
}

impl ModelPricing {
    pub fn new(
        supplement: PricingSupplement,
        primary: PricingCatalog,
        secondary: PricingCatalog,
    ) -> Self {
        Self {
            codex_credit_mode: false,
            supplement,
            primary,
            secondary,
            memo: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(&self, model: &str) -> Option<ModelRates> {
        if let Some(cached) = self
            .memo
            .lock()
            .ok()
            .and_then(|memo| memo.get(model).copied())
        {
            return cached;
        }
        let resolved = self.resolve_uncached(model);
        if let Ok(mut memo) = self.memo.lock() {
            memo.insert(model.to_owned(), resolved);
        }
        resolved
    }

    pub fn estimated_cost_dollars(
        &self,
        model: &str,
        tokens: TokenBreakdown,
        apply_long_context_rates: bool,
    ) -> Option<f64> {
        if self.codex_credit_mode {
            return super::codex_credits::equivalent_usd(self, model, tokens);
        }
        let mut rates = self.resolve(model)?;
        if rates.fast_multiplier == 1.0
            && !self
                .supplement
                .canonical_name(model)
                .unwrap_or(model)
                .ends_with("-fast")
        {
            rates.fast_multiplier = self.supplement.fast_multiplier(model).unwrap_or(1.0);
        }
        Some(rates.cost_dollars(tokens, apply_long_context_rates))
    }

    /// Returns the stable display family used for provider exports that contain one slug per model
    /// variant. Alias rules are the same source of truth used for pricing; fast variants fold into
    /// their base family without guessing at otherwise unknown names.
    pub fn display_family(&self, model: &str) -> String {
        let canonical = self.supplement.canonical_name(model).unwrap_or(model);
        canonical
            .strip_suffix("-fast")
            .filter(|base| !base.is_empty())
            .unwrap_or(canonical)
            .to_owned()
    }

    fn resolve_uncached(&self, model: &str) -> Option<ModelRates> {
        if let Some(canonical) = self.supplement.canonical_name(model) {
            if canonical != model {
                return self.lookup(canonical).or_else(|| self.lookup(model));
            }
        }
        self.lookup(model)
    }

    fn lookup(&self, name: &str) -> Option<ModelRates> {
        if let Some(rates) = self.supplement.pricing.get(name) {
            return Some(*rates);
        }
        if let Some((_, rates)) = self.primary.find_exact(name) {
            return Some(rates);
        }
        if let Some(rates) = self.fast_variant(name) {
            return Some(rates);
        }
        if name.ends_with("-fast") {
            return self.secondary.find_exact(name).map(|(_, rates)| rates);
        }
        if let Some((_, rates)) = self.primary.find_fuzzy(name) {
            return Some(rates);
        }
        self.secondary.find_exact(name).map(|(_, rates)| rates)
    }

    fn fast_variant(&self, name: &str) -> Option<ModelRates> {
        let base = name.strip_suffix("-fast")?;
        if base.is_empty() {
            return None;
        }
        let (key, rates) = self.base_entry(base)?;
        let multiplier = if rates.fast_multiplier != 1.0 {
            rates.fast_multiplier
        } else {
            self.supplement
                .fast_multiplier(key)
                .or_else(|| self.supplement.fast_multiplier(base))?
        };
        Some(rates.scaled(multiplier))
    }

    fn base_entry<'a>(&'a self, base: &'a str) -> Option<(&'a str, ModelRates)> {
        if let Some(rates) = self.supplement.pricing.get(base) {
            return Some((base, *rates));
        }
        self.primary
            .find_exact(base)
            .or_else(|| self.primary.find_fuzzy(base))
            .or_else(|| self.secondary.find_exact(base))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::ModelPricing;
    use crate::pricing::{ModelRates, PricingCatalog, PricingSupplement, TokenBreakdown};

    fn rates(input: f64, output: f64) -> ModelRates {
        ModelRates::new(input, output)
    }

    fn pricing(
        supplement: Option<&str>,
        primary: &[(&str, ModelRates)],
        secondary: &[(&str, ModelRates)],
    ) -> ModelPricing {
        ModelPricing::new(
            supplement
                .map(|json| PricingSupplement::decode(json.as_bytes()).unwrap())
                .unwrap_or_default(),
            PricingCatalog {
                entries: primary
                    .iter()
                    .map(|(name, rates)| ((*name).to_owned(), *rates))
                    .collect::<HashMap<_, _>>(),
                retrieved_at: None,
            },
            PricingCatalog {
                entries: secondary
                    .iter()
                    .map(|(name, rates)| ((*name).to_owned(), *rates))
                    .collect::<HashMap<_, _>>(),
                retrieved_at: None,
            },
        )
    }

    #[test]
    fn supplement_alias_and_precedence_follow_contract() {
        let supplement = r#"{
          "pricing":{"auto":{"input_per_million":1.25,"output_per_million":6}},
          "alias_rules":[{"pattern":"^claude-4\\.5-sonnet(?:-thinking)?$","canonical":"claude-sonnet-4-5"}]
        }"#;
        let model_pricing = pricing(
            Some(supplement),
            &[
                ("auto", rates(99.0, 99.0)),
                ("claude-sonnet-4-5", rates(3.0, 15.0)),
            ],
            &[],
        );
        assert_eq!(
            model_pricing.resolve("auto").unwrap().input_per_million,
            1.25
        );
        assert_eq!(
            model_pricing
                .resolve("claude-4.5-sonnet-thinking")
                .unwrap()
                .input_per_million,
            3.0
        );
    }

    #[test]
    fn alias_miss_falls_back_to_raw_name() {
        let supplement =
            r#"{"pricing":{},"alias_rules":[{"pattern":"^gpt-x$","canonical":"missing"}]}"#;
        let model_pricing = pricing(Some(supplement), &[("gpt-x", rates(1.0, 2.0))], &[]);
        assert_eq!(
            model_pricing.resolve("gpt-x").unwrap().input_per_million,
            1.0
        );
    }

    #[test]
    fn fast_variant_requires_a_multiplier_or_secondary_exact_rate() {
        let no_multiplier = pricing(None, &[("gpt-9", rates(1.0, 2.0))], &[]);
        assert!(no_multiplier.resolve("gpt-9-fast").is_none());

        let supplement = r#"{"pricing":{},"fast_multipliers":{"gpt-5.5":2.5},"alias_rules":[]}"#;
        let with_multiplier = pricing(
            Some(supplement),
            &[("gpt-5.5-20260423", rates(5.0, 30.0))],
            &[],
        );
        assert_eq!(
            with_multiplier
                .resolve("gpt-5.5-fast")
                .unwrap()
                .input_per_million,
            12.5
        );

        let secondary = pricing(
            None,
            &[("gpt-9", rates(1.0, 2.0))],
            &[("gpt-9-fast", rates(2.5, 5.0))],
        );
        assert_eq!(
            secondary.resolve("gpt-9-fast").unwrap().input_per_million,
            2.5
        );
    }

    #[test]
    fn unknown_model_cost_is_none() {
        let model_pricing = pricing(None, &[], &[]);
        assert!(model_pricing
            .estimated_cost_dollars(
                "mystery",
                TokenBreakdown {
                    input: 100,
                    ..TokenBreakdown::default()
                },
                true,
            )
            .is_none());
    }

    #[test]
    fn secondary_catalog_is_exact_only() {
        let model_pricing = pricing(
            None,
            &[],
            &[("provider/secondary-model-20260715", rates(1.0, 2.0))],
        );
        assert!(model_pricing
            .resolve("provider/secondary-model-20260715")
            .is_some());
        assert!(model_pricing.resolve("secondary-model").is_none());
    }

    #[test]
    fn historical_opus_fast_alias_survives_catalog_retirement() {
        let pricing = ModelPricing::new(
            PricingSupplement::decode(include_bytes!("../../resources/pricing_supplement.json"))
                .unwrap(),
            PricingCatalog::default(),
            PricingCatalog::default(),
        );
        let cost = pricing
            .estimated_cost_dollars(
                "claude-4.6-opus-max-thinking-fast",
                TokenBreakdown {
                    input: 1_000_000,
                    cache_write_5m: 1_000_000,
                    cache_write_1h: 1_000_000,
                    cache_read: 1_000_000,
                    output: 1_000_000,
                    is_fast: true,
                },
                true,
            )
            .unwrap();
        // Historical Fast rates include the premium once, across the full context window.
        assert!((cost - 280.5).abs() < 1e-9);
    }

    #[test]
    fn sol_6_1_log_aliases_keep_the_new_cache_discount_and_fast_prices() {
        let pricing = crate::pricing::test_bundled_pricing();
        let tokens = TokenBreakdown {
            input: 100_000,
            cache_write_5m: 120_000,
            cache_read: 52_000,
            output: 10_000,
            ..Default::default()
        };
        for model in [
            "gpt-6.1-sol",
            "openai/gpt-6.1-sol-high",
            "openai-codex/gpt-6.1-sol-max",
            "gpt-6.1-sol-20260930-ultra",
        ] {
            let cost = pricing.estimated_cost_dollars(model, tokens, true).unwrap();
            assert!((cost - 0.6052).abs() < 1e-9, "{model}: {cost}");
            assert_eq!(pricing.display_family(model), "gpt-6.1-sol");
        }
        for model in [
            "gpt-6.1-sol-fast",
            "openai-codex/gpt-6.1-sol-high-fast",
            "openai/gpt-6.1-sol-fast-max",
        ] {
            let cost = pricing
                .estimated_cost_dollars(
                    model,
                    TokenBreakdown {
                        is_fast: true,
                        ..tokens
                    },
                    true,
                )
                .unwrap();
            assert!((cost - 1.2104).abs() < 1e-9, "{model}: {cost}");
        }
        assert_eq!(
            pricing.resolve("gpt-6-sol").unwrap().cache_read_per_million,
            0.2
        );
    }

    #[test]
    fn sol_6_1_combined_prompt_boundary_reprices_the_whole_request() {
        let pricing = crate::pricing::test_bundled_pricing();
        let tokens = TokenBreakdown {
            input: 100_000,
            cache_write_5m: 120_000,
            cache_read: 52_001,
            output: 10_000,
            ..Default::default()
        };
        for (long_context, fast, expected) in [
            (false, false, 0.6052001),
            (true, false, 1.1604002),
            (true, true, 2.3208004),
        ] {
            let cost = pricing
                .estimated_cost_dollars(
                    "gpt-6.1-sol",
                    TokenBreakdown {
                        is_fast: fast,
                        ..tokens
                    },
                    long_context,
                )
                .unwrap();
            assert!(
                (cost - expected).abs() < 1e-9,
                "{long_context}/{fast}: {cost}"
            );
        }
    }

    #[test]
    fn sol_6_1_credit_mode_has_no_api_long_context_or_cache_write_charge() {
        let mut pricing = crate::pricing::test_bundled_pricing();
        pricing.codex_credit_mode = true;
        let tokens = TokenBreakdown {
            input: 1_000_000,
            cache_read: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        assert_eq!(
            pricing.estimated_cost_dollars("openai-codex/gpt-6.1-sol-high", tokens, true),
            Some(12.1)
        );
        assert_eq!(
            pricing.estimated_cost_dollars(
                "gpt-6.1-sol-ultra-fast",
                TokenBreakdown {
                    is_fast: true,
                    ..tokens
                },
                true
            ),
            Some(24.2)
        );
        assert!(pricing
            .estimated_cost_dollars("gpt-6.1-sol-ultrafast", tokens, true)
            .is_none());
        assert!(pricing
            .estimated_cost_dollars("gpt-6.1-sol-none", tokens, true)
            .is_none());
    }
}
