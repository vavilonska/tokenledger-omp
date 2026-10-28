mod catalog;
mod codecs;
pub mod codex_credits;
mod model_pricing;
mod rates;
mod store;
mod supplement;

pub use catalog::PricingCatalog;
pub use model_pricing::ModelPricing;
pub use rates::{ModelRates, RequestSpeed, TokenBreakdown};
pub use store::PricingStore;
pub use supplement::PricingSupplement;

#[cfg(test)]
pub fn test_bundled_pricing() -> ModelPricing {
    ModelPricing::new(
        PricingSupplement::decode(include_bytes!("../../resources/pricing_supplement.json"))
            .expect("bundled pricing supplement must be valid"),
        codecs::catalog_from_compact(include_bytes!(
            "../../resources/pricing_litellm_snapshot.json"
        ))
        .expect("bundled LiteLLM pricing must be valid"),
        codecs::catalog_from_compact(include_bytes!(
            "../../resources/pricing_models_dev_snapshot.json"
        ))
        .expect("bundled models.dev pricing must be valid"),
    )
}

#[cfg(test)]
mod bundled_resource_tests {
    use super::{test_bundled_pricing, TokenBreakdown};

    #[test]
    fn bundled_alias_rules_compile_and_reference_known_models() {
        let pricing = test_bundled_pricing();
        let raw_supplement: serde_json::Value =
            serde_json::from_slice(include_bytes!("../../resources/pricing_supplement.json"))
                .unwrap();
        assert_eq!(
            pricing.supplement.alias_rules.len(),
            raw_supplement["alias_rules"].as_array().unwrap().len(),
            "every bundled alias regex must compile in Rust"
        );
        for rule in &pricing.supplement.alias_rules {
            let base = rule
                .canonical
                .strip_suffix("-ultrafast")
                .or_else(|| rule.canonical.strip_suffix("-fast"))
                .unwrap_or(&rule.canonical);
            assert!(
                pricing.resolve(base).is_some(),
                "alias canonical '{}' references no known base model",
                rule.canonical
            );
        }
        for base in pricing.supplement.fast_multipliers.keys() {
            assert!(
                pricing.resolve(base).is_some(),
                "fast multiplier base '{base}' resolves against no pricing source"
            );
        }
    }

    #[test]
    fn known_log_models_and_aliases_match_expected_rates() {
        let pricing = test_bundled_pricing();
        let assert_rates = |model: &str, expected: [f64; 4]| {
            let rates = pricing.resolve(model).unwrap_or_else(|| panic!("{model}"));
            assert_eq!(
                [
                    rates.input_per_million,
                    rates.cache_write_per_million,
                    rates.cache_read_per_million,
                    rates.output_per_million,
                ],
                expected,
                "{model}"
            );
        };
        assert_eq!(
            pricing
                .resolve("claude-sonnet-4-5-20250929")
                .unwrap()
                .input_per_million,
            3.0
        );
        assert_eq!(
            pricing
                .resolve("gpt-5.6-sol-ultra-fast")
                .unwrap()
                .input_per_million,
            8.0
        );
        assert_eq!(
            pricing
                .resolve("claude-fable-5-thinking-xhigh")
                .unwrap()
                .output_per_million,
            50.0
        );
        assert_rates("gpt-5.6-high-fast", [8.0, 10.0, 0.8, 40.0]);
        assert_rates("gpt-6-sol-high-fast", [4.0, 5.0, 0.4, 20.0]);
        assert_rates("gpt-6-luna-medium-fast", [0.2, 0.25, 0.02, 1.0]);
        assert_rates("gpt-daybreak-red-latest", [12.5, 15.625, 1.25, 75.0]);
        assert_rates("gpt-daybreak-blue-latest", [4.0, 5.0, 0.4, 20.0]);
        assert_rates("daybreak-blue-latest", [4.0, 5.0, 0.4, 20.0]);
        assert_rates("gpt-5.6-terra-max-fast", [4.0, 5.0, 0.4, 24.0]);
        assert_rates("gpt-5.6-luna-high-fast", [0.4, 0.5, 0.04, 2.4]);
        assert_rates("claude-opus-5", [5.0, 6.25, 0.5, 25.0]);
        assert_rates(
            "claude-opus-5[1m]-thinking-max-fast",
            [10.0, 12.5, 1.0, 50.0],
        );
        assert_rates("claude-sonnet-5", [2.0, 2.5, 0.2, 10.0]);
        assert_rates("kimi-k3-code-max", [3.0, 3.0, 0.3, 15.0]);
        assert_rates("cursor-grok-4.5-high-fast", [4.0, 4.0, 1.0, 18.0]);
        assert_rates("cursor-grok-4.6-medium", [2.0, 2.0, 0.5, 6.0]);
        assert_rates("grok-4.6-fast-high", [4.0, 4.0, 1.0, 12.0]);
        assert_eq!(
            pricing.resolve("grok-build-0.1").unwrap().input_per_million,
            1.0
        );
        assert_eq!(
            pricing
                .resolve("cursor-grok-4.5-xhigh")
                .unwrap()
                .input_per_million,
            pricing.resolve("grok-4.5").unwrap().input_per_million
        );
        assert_eq!(
            pricing
                .resolve("cursor-grok-4.5-fast-xhigh")
                .unwrap()
                .output_per_million,
            pricing.resolve("grok-4.5-fast").unwrap().output_per_million
        );
        assert!(pricing.resolve("glm-5.2-bogus").is_none());
    }

    #[test]
    fn bundled_engine_prices_cache_and_long_context_buckets() {
        let pricing = test_bundled_pricing();
        let cost = pricing
            .estimated_cost_dollars(
                "claude-sonnet-4-5-20250929",
                TokenBreakdown {
                    input: 100_000,
                    cache_write_5m: 60_000,
                    cache_read: 50_000,
                    output: 20_000,
                    ..TokenBreakdown::default()
                },
                true,
            )
            .unwrap();
        assert!(cost > 0.0);
    }

    #[test]
    fn gpt_6_and_5_6_long_context_uses_published_request_wide_rates() {
        let pricing = test_bundled_pricing();
        for (model, input_rate, output_rate) in [
            ("gpt-6-sol", 2.0, 10.0),
            ("gpt-6-luna", 0.1, 0.5),
            ("gpt-5.6-terra", 2.0, 12.0),
        ] {
            let tokens = TokenBreakdown {
                input: 272_000,
                output: 1_000_000,
                ..TokenBreakdown::default()
            };
            let short = pricing.estimated_cost_dollars(model, tokens, true).unwrap();
            assert!((short - (0.272 * input_rate + output_rate)).abs() < 1e-9);
            let long_tokens = TokenBreakdown {
                input: 272_001,
                ..tokens
            };
            let long = pricing
                .estimated_cost_dollars(model, long_tokens, true)
                .unwrap();
            assert!((long - (0.272_001 * input_rate * 2.0 + output_rate * 1.5)).abs() < 1e-9);
        }
    }

    #[test]
    fn gpt_5_4_and_5_5_price_standard_long_context_but_not_unquoted_fast_long_context() {
        use super::RequestSpeed;
        let pricing = test_bundled_pricing();
        for (model, input_rate, output_rate, fast_multiplier) in
            [("gpt-5.4", 2.5, 15.0, 2.0), ("gpt-5.5", 5.0, 30.0, 2.5)]
        {
            for cache_read in [72_000, 72_001] {
                let tokens = TokenBreakdown {
                    input: 100_000,
                    cache_write_5m: 100_000,
                    cache_read,
                    output: 1_000,
                    ..Default::default()
                };
                let long = cache_read > 72_000;
                let input_cost = (200_000.0 + cache_read as f64 * 0.1) * input_rate;
                let expected = (input_cost * if long { 2.0 } else { 1.0 }
                    + 1_000.0 * output_rate * if long { 1.5 } else { 1.0 })
                    / 1_000_000.0;
                for prefix in ["", "openai/", "openai-codex/"] {
                    let alias = format!("{prefix}{model}-20260423-high");
                    let cost = pricing
                        .estimated_cost_dollars(&alias, tokens, true)
                        .unwrap();
                    assert!((cost - expected).abs() < 1e-9, "{alias}/{cache_read}");
                    for slug in [alias.clone(), format!("{alias}-fast")] {
                        let fast_tokens = TokenBreakdown {
                            speed: RequestSpeed::Fast,
                            ..tokens
                        };
                        for apply_long_context in [false, true] {
                            let fast = pricing.estimated_cost_dollars(
                                &slug,
                                fast_tokens,
                                apply_long_context,
                            );
                            if long {
                                assert_eq!(fast, None, "{slug}");
                            } else {
                                assert!(
                                    (fast.unwrap() - expected * fast_multiplier).abs() < 1e-9,
                                    "{slug}"
                                );
                            }
                        }
                    }
                    assert_eq!(
                        pricing
                            .estimated_cost_dollars(&format!("{alias}-fast"), tokens, true)
                            .is_none(),
                        long
                    );
                }
            }
            let tokens = TokenBreakdown {
                input: 300_000,
                output: 1_000,
                ..Default::default()
            };
            let cost = pricing.estimated_cost_dollars(model, tokens, true).unwrap();
            let expected = if model == "gpt-5.4" { 1.5225 } else { 3.045 };
            assert!((cost - expected).abs() < 1e-9);
        }
    }

    #[test]
    fn gpt_pro_standard_long_context_uses_shared_published_rates() {
        let pricing = test_bundled_pricing();
        for model in ["gpt-5.4-pro", "gpt-5.5-pro"] {
            for prefix in ["", "openai/", "openai-codex/"] {
                for suffix in ["", "-20260423", "-20260423-high"] {
                    let alias = format!("{prefix}{model}{suffix}");
                    let rates = pricing.resolve(&alias).unwrap();
                    assert_eq!(rates.input_per_million, 30.0);
                    assert_eq!(rates.output_per_million, 180.0);
                    assert_eq!(rates.input_above_200k_per_million, Some(60.0));
                    assert_eq!(rates.output_above_200k_per_million, Some(270.0));
                    assert!(!rates.cache_read_is_explicit);
                    for (input, expected) in
                        [(272_000, 8.34), (272_001, 16.59006), (300_000, 18.27)]
                    {
                        let tokens = TokenBreakdown {
                            input,
                            output: 1_000,
                            ..Default::default()
                        };
                        let cost = pricing
                            .estimated_cost_dollars(&alias, tokens, true)
                            .unwrap();
                        assert!((cost - expected).abs() < 1e-9, "{alias}/{input}: {cost}");
                    }
                }
            }
        }
    }

    #[test]
    fn corrected_claude_and_cursor_prices_do_not_change_adjacent_models() {
        let pricing = test_bundled_pricing();
        let reads = TokenBreakdown {
            cache_read: 100_000,
            ..Default::default()
        };
        for model in ["claude-sonnet-5-5", "anthropic/claude-sonnet-5.5-high"] {
            assert_eq!(
                pricing.estimated_cost_dollars(model, reads, true),
                Some(0.01)
            );
        }
        for (model, expected) in [
            ("claude-sonnet-5", 0.02),
            ("claude-opus-5-5", 0.02),
            ("claude-fable-5-1", 0.025),
            ("claude-mythos-5-1", 0.025),
        ] {
            assert!(
                (pricing.estimated_cost_dollars(model, reads, true).unwrap() - expected).abs()
                    < 1e-9
            );
        }
        let output = TokenBreakdown {
            output: 100_000,
            ..Default::default()
        };
        assert_eq!(
            pricing.estimated_cost_dollars("grok-4.5-fast", output, true),
            Some(1.8)
        );
        assert_eq!(
            pricing.estimated_cost_dollars("cursor-grok-4.6-fast", output, true),
            Some(1.2)
        );
    }

    #[test]
    fn supported_models_have_three_api_and_purchased_credit_speeds() {
        use super::RequestSpeed;
        for credit_mode in [false, true] {
            let mut pricing = test_bundled_pricing();
            pricing.codex_credit_mode = credit_mode;
            for (model, input_rate) in [("gpt-6-astra", 10.0), ("gpt-6.1-sol", 2.0)] {
                for (speed, multiplier) in [
                    (RequestSpeed::Standard, 1.0),
                    (RequestSpeed::Fast, 2.0),
                    (RequestSpeed::Ultrafast, 6.0),
                ] {
                    let tokens = TokenBreakdown {
                        input: 1_000_000,
                        speed,
                        ..Default::default()
                    };
                    assert_eq!(
                        pricing.estimated_cost_dollars(model, tokens, false),
                        Some(input_rate * multiplier),
                        "{model}/{speed:?}/credits={credit_mode}"
                    );
                }
            }
        }
    }

    #[test]
    fn ultrafast_aliases_do_not_conflate_ultra_reasoning_or_double_charge() {
        use super::RequestSpeed;
        for credit_mode in [false, true] {
            let mut pricing = test_bundled_pricing();
            pricing.codex_credit_mode = credit_mode;
            for (base, input_rate) in [("gpt-6-astra", 10.0), ("gpt-6.1-sol", 2.0)] {
                for (suffix, suffix_multiplier) in [
                    ("-ultrafast", 6.0),
                    ("-ultra-fast", 2.0),
                    ("-fast-ultra", 2.0),
                    ("-high-ultrafast", 6.0),
                    ("-ultrafast-max", 6.0),
                ] {
                    for prefix in ["", "openai/", "openai-codex/"] {
                        let model = format!("{prefix}{base}-20261001{suffix}");
                        assert_eq!(pricing.display_family(&model), base);
                        for (speed, multiplier) in [
                            (RequestSpeed::Standard, suffix_multiplier),
                            (RequestSpeed::Fast, 2.0),
                            (RequestSpeed::Ultrafast, 6.0),
                        ] {
                            let tokens = TokenBreakdown {
                                input: 1_000_000,
                                speed,
                                ..Default::default()
                            };
                            assert_eq!(
                                pricing.estimated_cost_dollars(&model, tokens, false),
                                Some(input_rate * multiplier),
                                "{model}/{speed:?}/credits={credit_mode}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn unsupported_ultrafast_and_unknown_suffixes_never_fall_back_to_standard() {
        use super::RequestSpeed;
        for credit_mode in [false, true] {
            let mut pricing = test_bundled_pricing();
            pricing.codex_credit_mode = credit_mode;
            for model in ["gpt-5.5", "gpt-6-sol", "gpt-6-luna", "claude-opus-5"] {
                let tokens = TokenBreakdown {
                    input: 100,
                    speed: RequestSpeed::Ultrafast,
                    ..Default::default()
                };
                assert_eq!(pricing.estimated_cost_dollars(model, tokens, false), None);
                for speed in [
                    RequestSpeed::Standard,
                    RequestSpeed::Fast,
                    RequestSpeed::Ultrafast,
                ] {
                    assert_eq!(
                        pricing.estimated_cost_dollars(
                            &format!("{model}-ultrafast"),
                            TokenBreakdown { speed, ..tokens },
                            false
                        ),
                        None
                    );
                }
            }
            for model in [
                "gpt-6-astra-unknown",
                "gpt-6.1-sol-ultrafast-bogus",
                "gpt-5.5-ultrafast-high",
                "gpt-6-astra-hyperfast",
            ] {
                assert_eq!(
                    pricing.estimated_cost_dollars(model, TokenBreakdown::default(), false),
                    None,
                    "{model}/credits={credit_mode}"
                );
            }
        }
    }

    #[test]
    fn three_speeds_preserve_api_long_context_and_credit_cache_write_rules() {
        use super::RequestSpeed;
        for model in ["gpt-6-astra", "gpt-6.1-sol"] {
            let mut pricing = test_bundled_pricing();
            for prompt_read in [52_000, 52_001] {
                let tokens = TokenBreakdown {
                    input: 100_000,
                    cache_write_5m: 100_000,
                    cache_write_12h: 20_000,
                    cache_read: prompt_read,
                    output: 10_000,
                    ..Default::default()
                };
                for credit_mode in [false, true] {
                    pricing.codex_credit_mode = credit_mode;
                    let base = pricing.estimated_cost_dollars(model, tokens, true).unwrap();
                    for (speed, multiplier) in [
                        (RequestSpeed::Standard, 1.0),
                        (RequestSpeed::Fast, 2.0),
                        (RequestSpeed::Ultrafast, 6.0),
                    ] {
                        let cost = pricing
                            .estimated_cost_dollars(model, TokenBreakdown { speed, ..tokens }, true)
                            .unwrap();
                        assert!((cost - base * multiplier).abs() < 1e-9);
                    }
                    if credit_mode {
                        let ordinary_input = TokenBreakdown {
                            input: 220_000,
                            cache_write_5m: 0,
                            cache_write_12h: 0,
                            ..tokens
                        };
                        assert_eq!(
                            pricing.estimated_cost_dollars(model, ordinary_input, false),
                            Some(base)
                        );
                    }
                }
            }
        }
    }
}
