use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::pricing::RequestSpeed;

/// Only numeric billing evidence is retained, never request/response content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct RecordedCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
    total: Option<f64>,
    valid: bool,
}

impl RecordedCost {
    pub(super) fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let mut valid = true;
        let mut number = |key| {
            object.get(key).and_then(|value| {
                let number = value
                    .as_f64()
                    .filter(|number| number.is_finite() && *number >= 0.0);
                valid &= number.is_some();
                number
            })
        };
        let input = number("input");
        let output = number("output");
        let cache_read = number("cacheRead");
        let cache_write = number("cacheWrite");
        let total = number("total");
        Some(Self {
            input,
            output,
            cache_read,
            cache_write,
            total,
            valid,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpeedEvidence {
    ExplicitTier,
    AssistantBranch,
    Omp1811Cost,
    Omp18132TerraCost,
    Omp18186LunaCost,
    Omp18222Codex62Cost,
}

pub(super) fn explicit_tier(value: &Value) -> Option<RequestSpeed> {
    match value {
        Value::Object(tiers) => tiers.get("openai").and_then(explicit_tier),
        Value::String(tier) => match tier.as_str() {
            "default" | "auto" => Some(RequestSpeed::Standard),
            "priority" | "fast" => Some(RequestSpeed::Fast),
            "ultrafast" => Some(RequestSpeed::Ultrafast),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn resolve_speed(
    message: &Value,
    assistant: bool,
    branch: Option<RequestSpeed>,
    model: &str,
    tokens: [u64; 4],
    cost: Option<&RecordedCost>,
    usage: &Map<String, Value>,
) -> (Option<RequestSpeed>, Option<SpeedEvidence>) {
    let camel = message.get("serviceTier");
    let snake = message.get("service_tier");
    // Presence of invalid or contradictory explicit metadata must not fall back to a branch.
    if camel.is_some() || snake.is_some() {
        let speed = match (camel, snake) {
            (Some(left), Some(right)) => {
                let left = explicit_tier(left);
                (left == explicit_tier(right)).then_some(left).flatten()
            }
            (Some(value), None) | (None, Some(value)) => explicit_tier(value),
            (None, None) => unreachable!(),
        };
        return (speed, speed.map(|_| SpeedEvidence::ExplicitTier));
    }
    if assistant {
        return (branch, branch.map(|_| SpeedEvidence::AssistantBranch));
    }
    cost.and_then(|cost| historical_speed(model, tokens, cost, usage))
        .map_or((None, None), |(speed, evidence)| {
            (Some(speed), Some(evidence))
        })
}

fn historical_speed(
    model: &str,
    tokens: [u64; 4],
    cost: &RecordedCost,
    usage: &Map<String, Value>,
) -> Option<(RequestSpeed, SpeedEvidence)> {
    // Frozen producer USD/million buckets, NOT the mutable display pricing catalog.
    let is_codex_62 = matches!(model, "gpt-6.2-astra" | "gpt-6.2-sol");
    let (rates, evidence, priority, ultrafast) = if is_codex_62 {
        // OMP 18.22.2 models.db (openai-codex:0.169.x) and its bundled Codex
        // terminal-event handler: no long-context or time-based rates, free ordinary
        // writes, priority=2, and ultrafast=6 advertised only by Sol. Flex is not Standard.
        let (rates, ultrafast) = if model == "gpt-6.2-astra" {
            ([8.0, 40.0, 0.4, 0.0], None)
        } else {
            ([2.0, 10.0, 0.1, 0.0], Some(6.0))
        };
        (
            rates,
            SpeedEvidence::Omp18222Codex62Cost,
            Some(2.0),
            ultrafast,
        )
    } else if model == "gpt-6-luna" {
        // OMP 18.18.6 models.db confirms this exact ID's standard buckets.
        // Priority is listed, but its producer multiplier is unverified: infer Standard only.
        (
            [0.1, 0.5, 0.01, 0.0],
            SpeedEvidence::Omp18186LunaCost,
            None,
            None,
        )
    } else if model == "gpt-6.1-terra" {
        // OMP 18.13.2 models.db and bundled calculateCost confirm this exact Terra ID:
        // free ordinary cache writes, no long-context/time-based premium, priority=2.5.
        // Flex=0.5 is not Standard; ultrafast is rejected by the producer. Do not infer
        // aliases or unsupported tiers from today's API prices (whose Fast multiplier is 2).
        (
            [1.0, 5.0, 0.05, 0.0],
            SpeedEvidence::Omp18132TerraCost,
            Some(2.5),
            None,
        )
    } else {
        // Preserve the separately verified OMP 18.11 Sol/Astra producer semantics.
        // v18.11.0 packages/catalog/src/compat/rules/providers/openai-codex.kdl:
        // free writes, no API long-context surcharge, priority=2.5 / ultrafast=8.
        let model = model
            .strip_prefix("openai-codex/")
            .or_else(|| model.strip_prefix("openai/"))
            .unwrap_or(model);
        let rates = match model {
            "gpt-6.1-sol" => [2.0, 10.0, 0.1, 0.0],
            "gpt-6-astra" => [10.0, 50.0, 1.0, 0.0],
            _ => return None,
        };
        (rates, SpeedEvidence::Omp1811Cost, Some(2.5), Some(8.0))
    };
    if !cost.valid || cost.total.is_none_or(|total| total <= 0.0) {
        return None;
    }
    let recorded = [cost.input, cost.output, cost.cache_read, cost.cache_write];
    if is_codex_62
        && (recorded.iter().any(Option::is_none)
            || ["input", "output", "cacheRead", "cacheWrite"]
                .iter()
                .any(|key| usage.get(*key).and_then(Value::as_u64).is_none()))
    {
        return None;
    }
    if tokens
        .iter()
        .zip(recorded)
        .any(|(tokens, cost)| *tokens > 0 && cost.is_none())
    {
        return None;
    }
    let mut matched = None;
    for (speed, multiplier) in [
        (RequestSpeed::Standard, Some(1.0)),
        (RequestSpeed::Fast, priority),
        (RequestSpeed::Ultrafast, ultrafast),
    ] {
        let Some(multiplier) = multiplier else {
            continue;
        };
        let expected = std::array::from_fn::<_, 4, _>(|index| {
            tokens[index] as f64 * rates[index] * multiplier / 1_000_000.0
        });
        let close = |left: f64, right: f64| {
            (left - right).abs() <= 1e-12_f64.max(left.abs().max(right.abs()) * 1e-8)
        };
        if !recorded
            .iter()
            .zip(expected)
            .all(|(recorded, expected)| recorded.is_none_or(|recorded| close(recorded, expected)))
            || !close(cost.total?, expected.iter().sum())
        {
            continue;
        }
        if matched.is_some_and(|previous| previous != speed) {
            return None;
        }
        matched = Some(speed);
    }
    let speed = matched?;
    // A recorded premium count must agree, but it cannot replace bucket evidence.
    if is_codex_62
        && usage.get("premiumRequests").is_some_and(|reported| {
            reported.as_u64() != Some((speed != RequestSpeed::Standard) as u64)
        })
    {
        return None;
    }
    Some((speed, evidence))
}
