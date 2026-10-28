use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    cost.and_then(|cost| historical_speed(model, tokens, cost))
        .map_or((None, None), |(speed, evidence)| {
            (Some(speed), Some(evidence))
        })
}

fn historical_speed(
    model: &str,
    tokens: [u64; 4],
    cost: &RecordedCost,
) -> Option<(RequestSpeed, SpeedEvidence)> {
    // Frozen producer USD/million buckets, NOT the mutable display pricing catalog.
    // OMP 18.13.2 models.db and bundled calculateCost confirm this exact Terra ID:
    // free ordinary cache writes, no long-context/time-based premium, priority=2.5.
    // Flex=0.5 is not Standard; ultrafast is rejected by the producer. Do not infer
    // aliases or unsupported tiers from today's API prices (whose Fast multiplier is 2).
    let (rates, evidence, ultrafast) = if model == "gpt-6.1-terra" {
        (
            [1.0, 5.0, 0.05, 0.0],
            SpeedEvidence::Omp18132TerraCost,
            false,
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
        (rates, SpeedEvidence::Omp1811Cost, true)
    };
    if !cost.valid || cost.total.is_none_or(|total| total <= 0.0) {
        return None;
    }
    let recorded = [cost.input, cost.output, cost.cache_read, cost.cache_write];
    if tokens
        .iter()
        .zip(recorded)
        .any(|(tokens, cost)| *tokens > 0 && cost.is_none())
    {
        return None;
    }
    let mut matched = None;
    for (speed, multiplier) in [
        (RequestSpeed::Standard, 1.0),
        (RequestSpeed::Fast, 2.5),
        (RequestSpeed::Ultrafast, 8.0),
    ] {
        if speed == RequestSpeed::Ultrafast && !ultrafast {
            continue;
        }
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
    matched.map(|speed| (speed, evidence))
}
