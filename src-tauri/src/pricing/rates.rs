#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelRates {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_write_per_million: f64,
    pub cache_read_per_million: f64,
    pub input_above_200k_per_million: Option<f64>,
    pub output_above_200k_per_million: Option<f64>,
    pub cache_write_above_200k_per_million: Option<f64>,
    pub cache_read_above_200k_per_million: Option<f64>,
    pub cache_read_is_explicit: bool,
    pub long_context_threshold_tokens: u64,
    pub fast_multiplier: f64,
    pub ultrafast_multiplier: Option<f64>,
}

impl ModelRates {
    pub fn new(input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            input_per_million,
            output_per_million,
            cache_write_per_million: input_per_million,
            cache_read_per_million: input_per_million * 0.1,
            input_above_200k_per_million: None,
            output_above_200k_per_million: None,
            cache_write_above_200k_per_million: None,
            cache_read_above_200k_per_million: None,
            cache_read_is_explicit: true,
            long_context_threshold_tokens: 200_000,
            fast_multiplier: 1.0,
            ultrafast_multiplier: None,
        }
    }

    pub fn scaled(self, factor: f64) -> Self {
        Self {
            input_per_million: self.input_per_million * factor,
            output_per_million: self.output_per_million * factor,
            cache_write_per_million: self.cache_write_per_million * factor,
            cache_read_per_million: self.cache_read_per_million * factor,
            input_above_200k_per_million: self
                .input_above_200k_per_million
                .map(|rate| rate * factor),
            output_above_200k_per_million: self
                .output_above_200k_per_million
                .map(|rate| rate * factor),
            cache_write_above_200k_per_million: self
                .cache_write_above_200k_per_million
                .map(|rate| rate * factor),
            cache_read_above_200k_per_million: self
                .cache_read_above_200k_per_million
                .map(|rate| rate * factor),
            cache_read_is_explicit: self.cache_read_is_explicit,
            long_context_threshold_tokens: self.long_context_threshold_tokens,
            fast_multiplier: 1.0,
            ultrafast_multiplier: self
                .ultrafast_multiplier
                .map(|multiplier| multiplier / factor),
        }
    }

    pub fn cost_dollars(
        self,
        tokens: TokenBreakdown,
        apply_long_context_rates: bool,
    ) -> Option<f64> {
        let use_long_context =
            apply_long_context_rates && tokens.prompt_tokens() > self.long_context_threshold_tokens;
        let select = |base: f64, long_context: Option<f64>| {
            if use_long_context {
                long_context.unwrap_or(base)
            } else {
                base
            }
        };
        let input_rate = select(self.input_per_million, self.input_above_200k_per_million);
        let output_rate = select(self.output_per_million, self.output_above_200k_per_million);
        let cache_write_rate = select(
            self.cache_write_per_million,
            self.cache_write_above_200k_per_million,
        );
        let cache_read_rate = select(
            self.cache_read_per_million,
            self.cache_read_above_200k_per_million,
        );
        let cache_write_1h_rate = input_rate * 2.0;
        let cost = tokens.input as f64 * input_rate
            + tokens.output as f64 * output_rate
            + tokens.cache_write_5m as f64 * cache_write_rate
            + tokens.cache_write_1h as f64 * cache_write_1h_rate
            + tokens.cache_read as f64 * cache_read_rate;
        let multiplier = match tokens.speed {
            RequestSpeed::Standard => 1.0,
            RequestSpeed::Fast => self.fast_multiplier,
            RequestSpeed::Ultrafast => self.ultrafast_multiplier?,
        };
        Some(cost / 1_000_000.0 * multiplier)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestSpeed {
    #[default]
    Standard,
    Fast,
    Ultrafast,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "StoredTokenBreakdown")]
pub struct TokenBreakdown {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
    pub speed: RequestSpeed,
}

// Legacy OpenCode facts stored the complete breakdown with an is_fast boolean.
// Keep that compatibility at the deserialize boundary, never as runtime state.
#[derive(serde::Deserialize)]
struct StoredTokenBreakdown {
    input: u64,
    cache_write_5m: u64,
    cache_write_1h: u64,
    cache_read: u64,
    output: u64,
    #[serde(default)]
    speed: Option<RequestSpeed>,
    #[serde(default)]
    is_fast: bool,
}

impl From<StoredTokenBreakdown> for TokenBreakdown {
    fn from(stored: StoredTokenBreakdown) -> Self {
        Self {
            input: stored.input,
            cache_write_5m: stored.cache_write_5m,
            cache_write_1h: stored.cache_write_1h,
            cache_read: stored.cache_read,
            output: stored.output,
            speed: stored.speed.unwrap_or(if stored.is_fast {
                RequestSpeed::Fast
            } else {
                RequestSpeed::Standard
            }),
        }
    }
}

impl TokenBreakdown {
    pub fn prompt_tokens(self) -> u64 {
        self.input
            .saturating_add(self.cache_write_5m)
            .saturating_add(self.cache_write_1h)
            .saturating_add(self.cache_read)
    }

    pub fn total_tokens(self) -> u64 {
        self.prompt_tokens().saturating_add(self.output)
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelRates, RequestSpeed, TokenBreakdown};

    #[test]
    fn cost_uses_all_buckets_and_one_hour_cache_rate() {
        let mut rates = ModelRates::new(3.0, 15.0);
        rates.cache_write_per_million = 3.75;
        rates.cache_read_per_million = 0.3;
        let tokens = TokenBreakdown {
            input: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
            cache_read: 1_000_000,
            output: 1_000_000,
            speed: RequestSpeed::Standard,
        };
        assert!((rates.cost_dollars(tokens, true).unwrap() - 28.05).abs() < 0.000_001);
    }

    #[test]
    fn combined_prompt_selects_request_wide_long_context_rates() {
        let mut rates = ModelRates::new(3.0, 15.0);
        rates.cache_write_per_million = 3.75;
        rates.cache_read_per_million = 0.3;
        rates.input_above_200k_per_million = Some(6.0);
        rates.output_above_200k_per_million = Some(22.5);
        rates.cache_write_above_200k_per_million = Some(7.5);
        rates.cache_read_above_200k_per_million = Some(0.6);
        let tokens = TokenBreakdown {
            input: 100_000,
            cache_write_5m: 60_000,
            cache_read: 50_000,
            output: 20_000,
            ..TokenBreakdown::default()
        };
        assert!((rates.cost_dollars(tokens, true).unwrap() - 1.53).abs() < 0.000_001);
        assert!(rates.cost_dollars(tokens, false).unwrap() < 1.53);
    }

    #[test]
    fn exactly_200k_keeps_base_rates_and_fast_scales_cost() {
        let mut rates = ModelRates::new(3.0, 15.0);
        rates.input_above_200k_per_million = Some(6.0);
        rates.fast_multiplier = 2.5;
        let tokens = TokenBreakdown {
            input: 200_000,
            output: 10_000,
            speed: RequestSpeed::Fast,
            ..TokenBreakdown::default()
        };
        assert!((rates.cost_dollars(tokens, true).unwrap() - 1.875).abs() < 0.000_001);
    }

    #[test]
    fn large_output_alone_does_not_select_long_context_rates() {
        let mut rates = ModelRates::new(3.0, 15.0);
        rates.input_above_200k_per_million = Some(6.0);
        rates.output_above_200k_per_million = Some(22.5);
        let tokens = TokenBreakdown {
            input: 10_000,
            output: 300_000,
            ..TokenBreakdown::default()
        };
        assert!((rates.cost_dollars(tokens, true).unwrap() - 4.53).abs() < 0.000_001);
    }

    #[test]
    fn unsupported_ultrafast_rates_are_unpriced() {
        let tokens = TokenBreakdown {
            input: 1_000_000,
            speed: RequestSpeed::Ultrafast,
            ..Default::default()
        };
        let mut rates = ModelRates::new(2.0, 10.0);
        assert_eq!(rates.cost_dollars(tokens, false), None);
        rates.ultrafast_multiplier = Some(6.0);
        assert_eq!(rates.cost_dollars(tokens, false), Some(12.0));
    }

    #[test]
    fn legacy_nested_token_breakdown_migrates_only_at_deserialization() {
        #[derive(serde::Deserialize)]
        struct Fact {
            tokens: TokenBreakdown,
        }
        for (legacy_fast, speed) in [(false, RequestSpeed::Standard), (true, RequestSpeed::Fast)] {
            let mut value = serde_json::json!({
                "tokens": {
                    "input": 7, "cache_write_5m": 2, "cache_write_1h": 3,
                    "cache_read": 5, "output": 11, "is_fast": legacy_fast
                }
            });
            let fact: Fact = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(fact.tokens.speed, speed);
            assert_eq!(fact.tokens.total_tokens(), 28);
            for explicit in [
                RequestSpeed::Standard,
                RequestSpeed::Fast,
                RequestSpeed::Ultrafast,
            ] {
                value["tokens"]["speed"] = serde_json::to_value(explicit).unwrap();
                let fact: Fact = serde_json::from_value(value.clone()).unwrap();
                assert_eq!(fact.tokens.speed, explicit);
                let serialized = serde_json::to_value(fact.tokens).unwrap();
                assert!(serialized.get("is_fast").is_none());
                assert_eq!(
                    serde_json::from_value::<TokenBreakdown>(serialized).unwrap(),
                    fact.tokens
                );
            }
        }
    }
}
