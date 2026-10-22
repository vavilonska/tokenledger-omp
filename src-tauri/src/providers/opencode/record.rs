use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::pricing::{ModelPricing, TokenBreakdown};

pub(crate) const GO_PROVIDER_ID: &str = "opencode-go";
pub(super) const EPOCH_MILLISECONDS_THRESHOLD: i64 = 100_000_000_000;
const HOSTED_PROVIDER_IDS: [&str; 2] = [GO_PROVIDER_ID, "opencode"];
const MAX_TOKEN_VALUE: f64 = 1_000_000_000_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CostProvenance {
    Exact,
    Estimated,
}

#[derive(Debug, Clone)]
pub(crate) struct UsageRecord {
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) model: String,
    pub(crate) tokens: u64,
    pub(crate) cost: Option<f64>,
    pub(crate) cost_provenance: CostProvenance,
    pub(crate) incomplete_cost: bool,
    pub(crate) unknown_model: bool,
    pub(crate) cache_usage: Option<(u64, u64)>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ParsedMessage {
    pub(super) message_id: String,
    session_id: String,
    pub(super) timestamp: DateTime<Utc>,
    provider_id: String,
    model: String,
    tokens: Option<ParsedTokens>,
    cost: StoredCost,
}

impl ParsedMessage {
    fn pricing_details(
        &self,
        parts: &[ParsedPart],
        pricing: &ModelPricing,
    ) -> (ParsedTokens, Option<f64>, CostProvenance, bool) {
        let part_tokens = parts
            .iter()
            .filter_map(|part| part.tokens)
            .fold(ParsedTokens::default(), ParsedTokens::saturating_add);
        let tokens = self
            .tokens
            .filter(|tokens| tokens.total > 0 || part_tokens.total == 0)
            .unwrap_or(part_tokens);
        let (cost, provenance, incomplete) = match self.cost {
            StoredCost::Exact(cost) => (Some(cost), CostProvenance::Exact, false),
            StoredCost::Invalid | StoredCost::Missing if !parts.is_empty() => {
                cost_from_parts(parts, &self.provider_id, &self.model, pricing)
            }
            StoredCost::Invalid => (None, CostProvenance::Exact, tokens.total > 0),
            StoredCost::Missing => {
                estimate_cost(pricing, &self.provider_id, &self.model, tokens.breakdown).map_or(
                    (None, CostProvenance::Estimated, tokens.total > 0),
                    |cost| (Some(cost), CostProvenance::Estimated, false),
                )
            }
        };
        (tokens, cost, provenance, incomplete)
    }

    pub(super) fn into_usage(self, parts: &[ParsedPart], pricing: &ModelPricing) -> UsageRecord {
        let (tokens, cost, cost_provenance, incomplete_cost) = self.pricing_details(parts, pricing);
        let has_rates = |model: &str| {
            let canonical = pricing.supplement.canonical_name(model).unwrap_or(model);
            let base = canonical
                .strip_suffix("-ultrafast")
                .or_else(|| canonical.strip_suffix("-fast"))
                .unwrap_or(canonical);
            pricing.resolve(base).is_some() || pricing.resolve(canonical).is_some()
        };
        let unknown_model = (cost.is_none() || incomplete_cost)
            && self.model != "Unattributed"
            && !has_rates(&self.model)
            && !has_rates(&format!("{}/{}", self.provider_id, self.model));

        UsageRecord {
            timestamp: self.timestamp,
            model: self.model,
            tokens: tokens.total,
            cache_usage: tokens.cache_complete.then_some((
                tokens.breakdown.cache_read,
                tokens
                    .breakdown
                    .input
                    .saturating_add(tokens.breakdown.cache_read)
                    .saturating_add(tokens.breakdown.cache_write_5m)
                    .saturating_add(tokens.breakdown.cache_write_1h)
                    .saturating_add(tokens.breakdown.cache_write_12h),
            )),
            cost,
            cost_provenance,
            incomplete_cost,
            unknown_model,
        }
    }
}

/// Numeric billing fields only; never retains a message's text or authentication data.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OpenCodeUsageEvent {
    pub(super) message: ParsedMessage,
    pub(super) parts: Vec<ParsedPart>,
}

impl OpenCodeUsageEvent {
    pub(super) fn key(&self) -> (&str, &str) {
        (&self.message.session_id, &self.message.message_id)
    }

    pub(super) fn event_key(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(&self.key()).map(|key| crate::hashing::sha256_hex(&key))
    }

    pub(super) fn is_better_than(&self, current: &Self, pricing: &ModelPricing) -> bool {
        let quality = |event: &Self| {
            let (tokens, cost, provenance, incomplete) =
                event.message.pricing_details(&event.parts, pricing);
            (
                cost.is_some() && !incomplete,
                cost.is_some(),
                tokens.total,
                provenance == CostProvenance::Exact,
                cost.unwrap_or_default(),
            )
        };
        quality(self) > quality(current)
    }

    pub(super) fn into_usage(self, pricing: &ModelPricing) -> UsageRecord {
        self.message.into_usage(&self.parts, pricing)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ParsedTokens {
    breakdown: TokenBreakdown,
    total: u64,
    #[serde(default)]
    cache_complete: bool,
}

impl ParsedTokens {
    fn saturating_add(self, other: Self) -> Self {
        Self {
            cache_complete: (self.cache_complete || self.total == 0)
                && (other.cache_complete || other.total == 0),
            breakdown: TokenBreakdown {
                input: self.breakdown.input.saturating_add(other.breakdown.input),
                cache_write_5m: self
                    .breakdown
                    .cache_write_5m
                    .saturating_add(other.breakdown.cache_write_5m),
                cache_write_1h: self
                    .breakdown
                    .cache_write_1h
                    .saturating_add(other.breakdown.cache_write_1h),
                cache_write_12h: self
                    .breakdown
                    .cache_write_12h
                    .saturating_add(other.breakdown.cache_write_12h),
                cache_read: self
                    .breakdown
                    .cache_read
                    .saturating_add(other.breakdown.cache_read),
                output: self.breakdown.output.saturating_add(other.breakdown.output),
                speed: match (self.breakdown.speed, other.breakdown.speed) {
                    (crate::pricing::RequestSpeed::Ultrafast, _)
                    | (_, crate::pricing::RequestSpeed::Ultrafast) => {
                        crate::pricing::RequestSpeed::Ultrafast
                    }
                    (crate::pricing::RequestSpeed::Fast, _)
                    | (_, crate::pricing::RequestSpeed::Fast) => crate::pricing::RequestSpeed::Fast,
                    _ => crate::pricing::RequestSpeed::Standard,
                },
            },
            total: self.total.saturating_add(other.total),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum StoredCost {
    Missing,
    Invalid,
    Exact(f64),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(super) struct ParsedPart {
    tokens: Option<ParsedTokens>,
    cost: StoredCost,
}

pub(super) fn parse_message(
    session_id: String,
    message_id: String,
    column_timestamp: Option<i64>,
    value: &Value,
) -> Option<ParsedMessage> {
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let provider_id = provider_id(value)?;
    if !HOSTED_PROVIDER_IDS.contains(&provider_id.as_str()) {
        return None;
    }
    let timestamp = column_timestamp
        .and_then(timestamp_from_number)
        .or_else(|| {
            value
                .pointer("/time/created")
                .and_then(timestamp_from_value)
        })
        .or_else(|| value.get("time_created").and_then(timestamp_from_value))?;
    let model = model_id(value).unwrap_or_else(|| "Unattributed".into());
    Some(ParsedMessage {
        session_id,
        message_id,
        timestamp,
        provider_id,
        model,
        tokens: value.get("tokens").and_then(parse_tokens),
        cost: stored_cost(value, &["cost", "costUSD"]),
    })
}

pub(super) fn parse_part(value: &Value) -> Option<ParsedPart> {
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("step-finish" | "step_finish")
    ) {
        return None;
    }
    Some(ParsedPart {
        tokens: value.get("tokens").and_then(parse_tokens),
        cost: stored_cost(value, &["cost", "costUSD"]),
    })
}

pub(super) fn provider_id(value: &Value) -> Option<String> {
    text_at(
        value,
        &[
            "/providerID",
            "/providerId",
            "/provider_id",
            "/model/providerID",
            "/model/providerId",
        ],
    )
}

pub(super) fn timestamp_from_value(value: &Value) -> Option<DateTime<Utc>> {
    finite_number(value)
        .and_then(|number| timestamp_from_number(number.trunc() as i64))
        .or_else(|| {
            value
                .as_str()
                .and_then(super::super::log_usage::parse_log_timestamp)
        })
}

pub(super) fn timestamp_from_number(value: i64) -> Option<DateTime<Utc>> {
    let milliseconds = if value.unsigned_abs() < EPOCH_MILLISECONDS_THRESHOLD as u64 {
        value.saturating_mul(1_000)
    } else {
        value
    };
    DateTime::from_timestamp_millis(milliseconds)
}

fn cost_from_parts(
    parts: &[ParsedPart],
    provider_id: &str,
    model: &str,
    pricing: &ModelPricing,
) -> (Option<f64>, CostProvenance, bool) {
    let mut total = 0.0;
    let mut has_cost = false;
    let mut estimated = false;
    let mut incomplete = false;
    for part in parts {
        match part.cost {
            StoredCost::Exact(cost) => {
                total += cost;
                has_cost = true;
            }
            StoredCost::Missing => {
                let estimate = part.tokens.and_then(|tokens| {
                    estimate_cost(pricing, provider_id, model, tokens.breakdown)
                });
                if let Some(cost) = estimate {
                    total += cost;
                    has_cost = true;
                    estimated = true;
                } else if part.tokens.is_some_and(|tokens| tokens.total > 0) {
                    incomplete = true;
                }
            }
            StoredCost::Invalid => incomplete = true,
        }
    }
    (
        has_cost.then_some(total),
        if estimated {
            CostProvenance::Estimated
        } else {
            CostProvenance::Exact
        },
        incomplete,
    )
}

fn estimate_cost(
    pricing: &ModelPricing,
    provider_id: &str,
    model: &str,
    tokens: TokenBreakdown,
) -> Option<f64> {
    if tokens.total_tokens() == 0 || model == "Unattributed" {
        return None;
    }
    pricing
        .estimated_cost_dollars(model, tokens, true)
        .or_else(|| pricing.estimated_cost_dollars(&format!("{provider_id}/{model}"), tokens, true))
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

fn model_id(value: &Value) -> Option<String> {
    text_at(
        value,
        &[
            "/modelID",
            "/modelId",
            "/model_id",
            "/model/modelID",
            "/model/modelId",
            "/model/id",
        ],
    )
}

fn text_at(value: &Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(Value::as_str)
            .and_then(non_empty)
    })
}

fn parse_tokens(value: &Value) -> Option<ParsedTokens> {
    let object = value.as_object()?;
    let input = integer_at(object, &["input"]);
    let output = integer_at(object, &["output"]);
    let reasoning = integer_at(object, &["reasoning"]);
    let cache_read = value
        .pointer("/cache/read")
        .and_then(nonnegative_integer)
        .unwrap_or_default();
    let cache_write = value
        .pointer("/cache/write")
        .and_then(nonnegative_integer)
        .unwrap_or_default();
    let recomputed = input
        .saturating_add(output)
        .saturating_add(reasoning)
        .saturating_add(cache_read)
        .saturating_add(cache_write);
    let reported = object.get("total").and_then(nonnegative_integer);
    Some(ParsedTokens {
        cache_complete: object.get("input").and_then(nonnegative_integer).is_some()
            && value
                .pointer("/cache/read")
                .and_then(nonnegative_integer)
                .is_some()
            && value
                .pointer("/cache/write")
                .and_then(nonnegative_integer)
                .is_some(),
        breakdown: TokenBreakdown {
            input,
            cache_write_5m: cache_write,
            cache_read,
            output: output.saturating_add(reasoning),
            ..TokenBreakdown::default()
        },
        total: reported.filter(|total| *total > 0).unwrap_or(recomputed),
    })
}

fn integer_at(object: &Map<String, Value>, names: &[&str]) -> u64 {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(nonnegative_integer))
        .unwrap_or_default()
}

fn nonnegative_integer(value: &Value) -> Option<u64> {
    finite_number(value)
        .filter(|value| *value >= 0.0)
        .map(|value| value.min(MAX_TOKEN_VALUE).trunc() as u64)
}

fn stored_cost(value: &Value, names: &[&str]) -> StoredCost {
    let Some(value) = names.iter().find_map(|name| value.get(*name)) else {
        return StoredCost::Missing;
    };
    match finite_number(value) {
        Some(cost) if cost >= 0.0 => StoredCost::Exact(cost),
        _ => StoredCost::Invalid,
    }
}

fn finite_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|value| value.is_finite())
}

fn non_empty(value: impl AsRef<str>) -> Option<String> {
    let value = value.as_ref().trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod retained_speed_regressions {
    use super::*;

    #[test]
    fn retained_twelve_hour_writes_survive_part_merging_and_pricing() {
        let parsed = |writes| {
            serde_json::from_value::<ParsedTokens>(serde_json::json!({
                "breakdown": {"input": 0, "cache_write_5m": 0, "cache_write_1h": 0,
                    "cache_write_12h": writes, "cache_read": 0, "output": 0},
                "total": writes, "cache_usage": [0, writes], "cache_complete": true
            }))
            .unwrap()
        };
        let merged = parsed(400_000).saturating_add(parsed(600_000));
        let message = ParsedMessage {
            message_id: "m".into(),
            session_id: "s".into(),
            timestamp: "2026-07-10T10:00:00Z".parse().unwrap(),
            provider_id: "opencode".into(),
            model: "gpt-6.1-sol".into(),
            tokens: Some(merged),
            cost: StoredCost::Missing,
        };
        let usage = message.into_usage(&[], &crate::pricing::test_bundled_pricing());
        assert_eq!(usage.tokens, 1_000_000);
        assert_eq!(usage.cache_usage, Some((0, 1_000_000)));
        assert_eq!(usage.cost, Some(10.0));
    }

    #[test]
    fn undifferentiated_source_writes_do_not_invent_a_twelve_hour_ttl() {
        let parsed = parse_tokens(&serde_json::json!({
            "input": 10, "output": 20, "reasoning": 0,
            "cache": {"read": 30, "write": 40}
        }))
        .unwrap();
        assert_eq!(parsed.breakdown.cache_write_5m, 40);
        assert_eq!(parsed.breakdown.cache_write_12h, 0);
        assert_eq!(parsed.total, 100);
    }

    #[test]
    fn nested_legacy_speed_facts_reprice_but_recorded_cost_remains_authoritative() {
        let mut stored = serde_json::json!({
            "message": {
                "message_id": "m", "session_id": "s", "timestamp": "2026-07-10T10:00:00Z",
                "provider_id": "opencode", "model": "gpt-6-astra",
                "tokens": { "total": 110, "breakdown": {
                    "input": 100, "output": 10, "cache_read": 0,
                    "cache_write_5m": 0, "cache_write_1h": 0, "is_fast": true
                }},
                "cost": "Missing"
            },
            "parts": []
        });
        let pricing = crate::pricing::test_bundled_pricing();
        let legacy: OpenCodeUsageEvent = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(
            legacy.message.tokens.unwrap().breakdown.speed,
            crate::pricing::RequestSpeed::Fast
        );
        let fast_cost = legacy.into_usage(&pricing).cost.unwrap();
        stored["message"]["tokens"]["breakdown"]["speed"] = serde_json::json!("standard");
        let standard: OpenCodeUsageEvent = serde_json::from_value(stored.clone()).unwrap();
        let roundtrip = serde_json::to_value(&standard).unwrap();
        assert!(roundtrip["message"]["tokens"]["breakdown"]
            .get("is_fast")
            .is_none());
        let standard_cost = standard.into_usage(&pricing).cost.unwrap();
        assert!((fast_cost - standard_cost * 2.0).abs() < 1e-12);
        stored["message"]["tokens"]["breakdown"]["speed"] = serde_json::json!("ultrafast");
        let ultra: OpenCodeUsageEvent = serde_json::from_value(stored.clone()).unwrap();
        assert!((ultra.into_usage(&pricing).cost.unwrap() - standard_cost * 6.0).abs() < 1e-12);
        stored["message"]["model"] = serde_json::json!("unpriced-model");
        stored["message"]["cost"] = serde_json::json!({"Exact": 0.75});
        let exact: OpenCodeUsageEvent = serde_json::from_value(stored).unwrap();
        let usage = exact.into_usage(&pricing);
        assert_eq!(usage.tokens, 110);
        assert_eq!(usage.cost, Some(0.75));
        assert_eq!(usage.cost_provenance, CostProvenance::Exact);
        assert!(!usage.incomplete_cost);
    }
}
