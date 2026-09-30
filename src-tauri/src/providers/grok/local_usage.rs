use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Days, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    models::UsageHistory,
    pricing::{ModelPricing, TokenBreakdown},
    providers::{
        daily_usage::DailyUsageAccumulator,
        log_usage::{parse_log_timestamp, LogCacheError},
    },
    storage::Storage,
};

use super::GrokError;

const LOG_CACHE_SCHEMA_VERSION: u8 = 2;
const SOURCE_NOTE: &str = "From your Grok logs (estimated)";

#[derive(Debug, Clone)]
pub struct GrokLogUsageScanner {
    path: PathBuf,
}

impl GrokLogUsageScanner {
    pub fn new() -> Self {
        Self {
            path: log_path(
                &home_directory(),
                std::env::var("GROK_HOME")
                    .ok()
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty())
                    .as_deref(),
            ),
        }
    }

    #[cfg(test)]
    pub fn for_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn scan(
        &self,
        storage: &Storage,
        now: DateTime<Utc>,
        pricing: &ModelPricing,
    ) -> Result<UsageHistory, GrokError> {
        scan_path(storage, &self.path, now, pricing)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TokenEvent {
    #[serde(default)]
    source_pid: Option<i64>,
    timestamp: DateTime<Utc>,
    model: Option<String>,
    prompt: u64,
    cached_prompt: u64,
    completion: u64,
    reasoning: u64,
}

fn scan_path(
    storage: &Storage,
    path: &Path,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
) -> Result<UsageHistory, GrokError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => {
            crate::app_warn!("plugin:grok", "local usage log path is not a file");
            return Err(GrokError::LocalUsage);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            crate::app_warn!("plugin:grok", "local usage log metadata could not be read");
            return Err(GrokError::LocalUsage);
        }
    }

    let events = crate::providers::log_usage::retain_log_usage(
        storage,
        "grok",
        "",
        "grok",
        LOG_CACHE_SCHEMA_VERSION,
        1,
        &[path.to_path_buf()],
        parse_jsonl,
        |event: &TokenEvent| {
            crate::providers::log_usage::usage_event_key(&(
                event.source_pid,
                event.timestamp,
                &event.model,
            ))
        },
        |_| true,
    )
    .map_err(|error| match error {
        LogCacheError::Storage(_) => GrokError::Storage,
        LogCacheError::Encode(_) => GrokError::LocalUsage,
    })?;
    Ok(aggregate(events, now, pricing))
}

fn log_path(home: &Path, configured_home: Option<&str>) -> PathBuf {
    configured_home
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".grok"))
        .join("logs")
        .join("unified.jsonl")
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn parse_jsonl(content: &str) -> Vec<TokenEvent> {
    let mut model_by_pid = HashMap::<i64, String>::new();
    let mut events = Vec::new();

    for line in content.lines() {
        if !line.contains("inference_done") && !line.contains("model") {
            continue;
        }
        let Ok(object) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = object.get("msg").and_then(Value::as_str) else {
            continue;
        };
        let context = object
            .get("ctx")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let pid = object.get("pid").and_then(integer_i64);

        if let Some(model) = model_id(message, &context) {
            if let Some(pid) = pid {
                model_by_pid.insert(pid, model);
            }
            continue;
        }
        if message != "shell.turn.inference_done" {
            continue;
        }
        let Some(prompt) = context.get("prompt_tokens").and_then(integer_u64) else {
            continue;
        };
        let Some(timestamp) = object
            .get("ts")
            .and_then(Value::as_str)
            .and_then(parse_log_timestamp)
        else {
            continue;
        };
        let cached_prompt = context
            .get("cached_prompt_tokens")
            .and_then(integer_u64)
            .unwrap_or_default()
            .min(prompt);
        events.push(TokenEvent {
            source_pid: pid,
            timestamp,
            model: pid.and_then(|pid| model_by_pid.get(&pid).cloned()),
            prompt,
            cached_prompt,
            completion: context
                .get("completion_tokens")
                .and_then(integer_u64)
                .unwrap_or_default(),
            reasoning: context
                .get("reasoning_tokens")
                .and_then(integer_u64)
                .unwrap_or_default(),
        });
    }
    events
}

fn model_id(message: &str, context: &Map<String, Value>) -> Option<String> {
    let value = match message {
        "model changed" => context.get("model"),
        "model catalog: notifying clients" => context.get("current_model_id"),
        "backend_search: model switch" => context
            .get("model")
            .or_else(|| context.get("current_model_id"))
            .or_else(|| context.get("model_id")),
        "subagent model resolved" => context.get("model_id").or_else(|| context.get("model")),
        _ => None,
    }?;
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn aggregate(events: Vec<TokenEvent>, now: DateTime<Utc>, pricing: &ModelPricing) -> UsageHistory {
    let today = now.with_timezone(&Local).date_naive();
    let since = today.checked_sub_days(Days::new(30)).unwrap_or(today);
    let mut accumulator = DailyUsageAccumulator::default();

    for event in events {
        let date = event.timestamp.with_timezone(&Local).date_naive();
        if date < since {
            continue;
        }
        let Some(model) = event.model.as_deref() else {
            continue;
        };
        let tokens = TokenBreakdown {
            input: event.prompt.saturating_sub(event.cached_prompt),
            cache_read: event.cached_prompt,
            output: event.completion.saturating_add(event.reasoning),
            ..TokenBreakdown::default()
        };
        let total = event
            .prompt
            .saturating_add(event.completion)
            .saturating_add(event.reasoning);
        if let Some(cost) = pricing.estimated_cost_dollars(model, tokens, true) {
            accumulator.add(date, total, cost, model);
        } else if total > 0 {
            accumulator.add_unknown_model(date, model);
        }
    }
    accumulator.build(now, SOURCE_NOTE)
}

fn integer_u64(value: &Value) -> Option<u64> {
    finite_number(value)
        .filter(|number| *number >= 0.0)
        .map(|number| number.trunc() as u64)
}

fn integer_i64(value: &Value) -> Option<i64> {
    finite_number(value).map(|number| number.trunc() as i64)
}

fn finite_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|number: &f64| number.is_finite())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use chrono::{TimeZone, Utc};
    use tempfile::tempdir;

    use super::{aggregate, log_path, parse_jsonl, GrokLogUsageScanner};
    use crate::{
        pricing::{ModelPricing, ModelRates, PricingCatalog, PricingSupplement},
        providers::grok::GrokError,
        storage::Storage,
    };

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 18, 12, 0, 0).unwrap()
    }

    fn fixture_pricing() -> ModelPricing {
        // Fixed rates isolate process/model tracking and cache accounting from feed updates.
        let mut build = ModelRates::new(1.0, 2.0);
        build.cache_read_per_million = 0.2;
        let mut composer = ModelRates::new(3.0, 15.0);
        composer.cache_read_per_million = 0.5;
        ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: std::collections::HashMap::from([
                    ("grok-build".into(), build),
                    ("grok-composer-2.5-fast".into(), composer),
                ]),
                ..PricingCatalog::default()
            },
            PricingCatalog::default(),
        )
    }

    #[test]
    fn fixture_tracks_models_per_process_and_prices_usage() {
        let history = aggregate(
            parse_jsonl(include_str!("fixtures/usage.jsonl")),
            now(),
            &fixture_pricing(),
        );

        let today = history.today.unwrap();
        let yesterday = history.yesterday.unwrap();
        assert_eq!(today.tokens, 2_000_000);
        assert!((today.estimated_cost_usd.unwrap() - 3.0).abs() < 0.000_001);
        assert_eq!(yesterday.tokens, 2_000_000);
        assert!((yesterday.estimated_cost_usd.unwrap() - 18.0).abs() < 0.000_001);
        assert!(today.cost_estimated);
    }

    #[test]
    fn mid_process_switch_and_cached_prompt_use_the_active_rates() {
        let content = r#"{"ts":"2026-06-18T08:00:00Z","pid":7,"msg":"model changed","ctx":{"model":"grok-build"}}
{"ts":"2026-06-18T09:00:00Z","pid":7,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":1000000,"cached_prompt_tokens":800000,"completion_tokens":0}}
{"ts":"2026-06-18T10:00:00Z","pid":7,"msg":"backend_search: model switch","ctx":{"model_id":"grok-composer-2.5-fast"}}
{"ts":"2026-06-18T11:00:00Z","pid":7,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":1000000,"completion_tokens":0}}"#;
        let history = aggregate(parse_jsonl(content), now(), &fixture_pricing());

        assert!((history.today.unwrap().estimated_cost_usd.unwrap() - 3.36).abs() < 0.000_001);
    }

    #[test]
    fn unknown_models_warn_without_inflating_totals_and_unattributed_rows_drop() {
        let content = r#"{"ts":"2026-06-18T08:00:00Z","pid":1,"msg":"model changed","ctx":{"model":"grok-unknown-model"}}
{"ts":"2026-06-18T09:00:00Z","pid":1,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":1000000}}
{"ts":"2026-06-18T10:00:00Z","pid":2,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":1000000}}
{"ts":"2026-06-18T11:00:00Z","pid":3,"msg":"model changed","ctx":{"model":"grok-build"}}
{"ts":"2026-06-18T12:00:00Z","pid":3,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":500000}}"#;
        let history = aggregate(parse_jsonl(content), now(), &fixture_pricing());

        assert_eq!(history.today.as_ref().unwrap().tokens, 500_000);
        assert_eq!(history.unknown_models, ["grok-unknown-model"]);
        assert!(!history.today.unwrap().estimate_complete);
    }

    #[test]
    fn rows_outside_the_window_and_rows_without_prompt_tokens_are_ignored() {
        let content = r#"{"ts":"2026-05-01T08:00:00Z","pid":1,"msg":"model changed","ctx":{"model":"grok-build"}}
{"ts":"2026-05-01T09:00:00Z","pid":1,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":1000000}}
{"ts":"2026-06-18T09:00:00Z","pid":1,"msg":"shell.turn.inference_done","ctx":{"loop_index":3}}"#;
        let history = aggregate(parse_jsonl(content), now(), &fixture_pricing());
        assert!(history.today.is_none());
        assert!(history.daily.is_empty());
    }

    #[test]
    fn path_uses_override_or_cross_platform_home() {
        assert_eq!(
            log_path(Path::new("/users/me"), None),
            Path::new("/users/me/.grok/logs/unified.jsonl")
        );
        assert_eq!(
            log_path(Path::new("/ignored"), Some("/custom/grok")),
            Path::new("/custom/grok/logs/unified.jsonl")
        );
    }

    #[test]
    fn truncated_and_deleted_grok_log_reprices_retained_usage() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("usage.jsonl");
        let database = directory.path().join("usage.db");
        fs::write(&path, r#"{"ts":"2026-06-18T08:00:00Z","pid":7,"msg":"model changed","ctx":{"model":"grok-build"}}
{"ts":"2026-06-18T09:00:00Z","pid":7,"msg":"shell.turn.inference_done","ctx":{"prompt_tokens":100,"completion_tokens":10}}"#).unwrap();
        let prices = |input, output| {
            crate::pricing::ModelPricing::new(
                crate::pricing::PricingSupplement::default(),
                crate::pricing::PricingCatalog {
                    entries: std::collections::HashMap::from([(
                        "grok-build".into(),
                        crate::pricing::ModelRates::new(input, output),
                    )]),
                    ..crate::pricing::PricingCatalog::default()
                },
                crate::pricing::PricingCatalog::default(),
            )
        };
        let scanner = GrokLogUsageScanner::for_path(path.clone());
        let storage = Storage::open(&database).unwrap();
        let original = scanner.scan(&storage, now(), &prices(2.0, 4.0)).unwrap();
        assert_eq!(original.today.as_ref().unwrap().tokens, 110);
        assert!(
            (original.today.as_ref().unwrap().estimated_cost_usd.unwrap() - 0.00024).abs() < 1e-12
        );
        fs::write(&path, "").unwrap();
        assert_eq!(
            scanner.scan(&storage, now(), &prices(2.0, 4.0)).unwrap(),
            original
        );
        fs::remove_file(&path).unwrap();
        drop(storage);
        let restarted = Storage::open(&database).unwrap();
        let repriced = scanner.scan(&restarted, now(), &prices(4.0, 8.0)).unwrap();
        assert_eq!(repriced.today.as_ref().unwrap().tokens, 110);
        assert!((repriced.today.unwrap().estimated_cost_usd.unwrap() - 0.00048).abs() < 1e-12);
    }

    #[test]
    fn missing_log_is_honest_no_data_while_non_file_path_fails() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let log = directory.path().join("logs").join("unified.jsonl");
        let scanner = GrokLogUsageScanner::for_path(log.clone());
        let missing = scanner.scan(&storage, now(), &fixture_pricing()).unwrap();
        assert!(missing.daily.is_empty());

        fs::create_dir_all(&log).unwrap();
        let error = scanner
            .scan(&storage, now(), &fixture_pricing())
            .unwrap_err();
        assert!(matches!(error, GrokError::LocalUsage));
    }
}
