use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Days, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use walkdir::WalkDir;

use crate::{
    models::{QuotaWindow, ResetCycleUsage, UsageHistory, UsagePeriod},
    pricing::{ModelPricing, ModelRates, TokenBreakdown},
    storage::Storage,
};

use super::CodexError;
use crate::providers::{
    daily_usage::DailyUsageAccumulator,
    log_usage::{load_or_parse_log, parse_log_timestamp, LogCacheError},
    omp_usage, pi_usage,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenEvent {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub reasoning: u64,
    pub total: u64,
    pub is_fast: bool,
}

const LOG_CACHE_SCHEMA_VERSION: u8 = 4;
const HISTORY_LOOKBACK_DAYS: u64 = 120;
const CYCLE_CLUSTER_SECONDS: i64 = 5 * 60;

struct CycleBucket {
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    scheduled_reset_at: DateTime<Utc>,
    used_percent: f64,
    accumulator: DailyUsageAccumulator,
}

pub fn scan_local_usage(
    storage: &Storage,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    quotas: &[QuotaWindow],
    expected_account_id: Option<&str>,
    account_identity: Option<&str>,
) -> Result<UsageHistory, CodexError> {
    let mut usage = scan_usage_basis(
        storage,
        now,
        pricing,
        quotas,
        expected_account_id,
        account_identity,
    )?;
    let mut credit_pricing = ModelPricing::new(
        crate::pricing::PricingSupplement::decode(include_bytes!(
            "../../../resources/pricing_supplement.json"
        ))
        .expect("valid bundled credit aliases"),
        crate::pricing::PricingCatalog::default(),
        crate::pricing::PricingCatalog::default(),
    );
    credit_pricing.codex_credit_mode = true;
    usage.credit_usage = Some(Box::new(scan_usage_basis(
        storage,
        now,
        &credit_pricing,
        quotas,
        expected_account_id,
        account_identity,
    )?));
    Ok(usage)
}

fn scan_usage_basis(
    storage: &Storage,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    quotas: &[QuotaWindow],
    expected_account_id: Option<&str>,
    account_identity: Option<&str>,
) -> Result<UsageHistory, CodexError> {
    let home = home_directory();
    let configured_home = crate::provider_environment::value("CODEX_HOME");
    let homes = codex_homes(configured_home.as_deref().map(OsStr::new), &home);
    let since_date = now
        .with_timezone(&Local)
        .date_naive()
        .checked_sub_days(Days::new(HISTORY_LOOKBACK_DAYS))
        .unwrap_or(NaiveDate::MIN);
    let events = scan_codex_events(storage, &homes, since_date)?;

    let session_start = super::cycle_start(quotas, "session", now);
    let weekly_start = super::cycle_start(quotas, "weekly", now);
    let mut accumulator = DailyUsageAccumulator::default();
    let mut session_accumulator = DailyUsageAccumulator::default();
    let mut weekly_accumulator = DailyUsageAccumulator::default();
    let omp_outcome = match omp_usage::scan_into(
        storage,
        now,
        pricing,
        expected_account_id,
        session_start,
        weekly_start,
        &mut accumulator,
        &mut session_accumulator,
        &mut weekly_accumulator,
    ) {
        Ok(outcome) => outcome,
        Err(_) => {
            crate::app_warn!(
                "plugin:omp",
                "Oh My Pi usage history could not be folded into Codex"
            );
            omp_usage::OmpScanOutcome::default()
        }
    };
    let mut anchors = omp_outcome.weekly_anchors.clone();
    if let Some(anchor) = current_weekly_anchor(quotas, now) {
        anchors.push(anchor);
    }
    let mut cycle_buckets = build_cycle_buckets(anchors, now);
    aggregate_into(
        &events,
        now,
        pricing,
        &mut accumulator,
        session_start,
        &mut session_accumulator,
        weekly_start,
        &mut weekly_accumulator,
        &mut cycle_buckets,
    );
    for event in &omp_outcome.events {
        add_cycle_event(
            &mut cycle_buckets,
            event.timestamp,
            &event.model,
            event.total,
            event.cost,
            "Oh My Pi",
        );
    }
    let includes_pi = match pi_usage::scan_into(storage, now, pricing, "codex", &mut accumulator) {
        Ok(includes_pi) => includes_pi,
        Err(_) => {
            crate::app_warn!(
                "plugin:pi",
                "pi usage history could not be folded into Codex"
            );
            false
        }
    };
    let mut sources = vec!["Codex"];
    if omp_outcome.included {
        sources.push("Oh My Pi");
    }
    if includes_pi {
        sources.push("pi");
    }
    let source_note = if pricing.codex_credit_mode {
        format!(
            "{} logs · Codex credit estimate · 2,500 credits = $100 · separate from API pricing",
            sources.join(" + ")
        )
    } else {
        format!("{} logs · API list price estimate", sources.join(" + "))
    };
    let source_note = source_note.as_str();
    let mut usage = accumulator.build(now, source_note);
    usage.session_cycle = cycle_period(
        session_accumulator,
        now,
        source_note,
        quota_used_percent(quotas, "session"),
    );
    usage.weekly_cycle = cycle_period(
        weekly_accumulator,
        now,
        source_note,
        quota_used_percent(quotas, "weekly"),
    );
    let fresh_cycles = build_cycle_history(cycle_buckets, now, source_note);
    usage.weekly_cycles = merge_persisted_cycles(
        storage,
        account_identity,
        fresh_cycles,
        pricing.codex_credit_mode,
    )?;
    Ok(usage)
}

fn current_weekly_anchor(
    quotas: &[QuotaWindow],
    now: DateTime<Utc>,
) -> Option<omp_usage::QuotaCycleAnchor> {
    let quota = quotas.iter().find(|quota| quota.id == "weekly")?;
    let scheduled_reset_at = quota.resets_at.filter(|reset| *reset > now)?;
    let period_seconds = i64::try_from(quota.period_seconds).ok()?;
    let started_at =
        scheduled_reset_at.checked_sub_signed(chrono::Duration::seconds(period_seconds))?;
    Some(omp_usage::QuotaCycleAnchor {
        started_at,
        scheduled_reset_at,
        used_percent: quota.used_percent.clamp(0.0, 100.0),
    })
}

fn build_cycle_buckets(
    mut anchors: Vec<omp_usage::QuotaCycleAnchor>,
    now: DateTime<Utc>,
) -> Vec<CycleBucket> {
    anchors.retain(|anchor| anchor.started_at <= now);
    anchors.sort_by_key(|anchor| anchor.started_at);
    let mut merged: Vec<omp_usage::QuotaCycleAnchor> = Vec::new();
    for anchor in anchors {
        if let Some(previous) = merged.last_mut() {
            let delta = anchor
                .started_at
                .signed_duration_since(previous.started_at)
                .num_seconds();
            if delta <= CYCLE_CLUSTER_SECONDS {
                previous.started_at = previous.started_at.min(anchor.started_at);
                previous.scheduled_reset_at =
                    previous.scheduled_reset_at.min(anchor.scheduled_reset_at);
                previous.used_percent = previous.used_percent.max(anchor.used_percent);
                continue;
            }
        }
        merged.push(anchor);
    }
    (0..merged.len())
        .map(|index| CycleBucket {
            started_at: merged[index].started_at,
            ended_at: merged.get(index + 1).map(|anchor| anchor.started_at),
            scheduled_reset_at: merged[index].scheduled_reset_at,
            used_percent: merged[index].used_percent,
            accumulator: DailyUsageAccumulator::default(),
        })
        .collect()
}

fn add_cycle_event(
    buckets: &mut [CycleBucket],
    timestamp: DateTime<Utc>,
    model: &str,
    tokens: u64,
    cost: Option<f64>,
    source: &str,
) {
    let Some(bucket) = buckets.iter_mut().rev().find(|bucket| {
        timestamp >= bucket.started_at
            && bucket.ended_at.is_none_or(|ended_at| timestamp < ended_at)
    }) else {
        return;
    };
    let date = timestamp.with_timezone(&Local).date_naive();
    if let Some(cost) = cost {
        add_priced_event(&mut bucket.accumulator, date, model, tokens, cost, source);
    } else if tokens > 0 {
        bucket.accumulator.add_unknown_model(date, model);
    }
}

fn build_cycle_history(
    buckets: Vec<CycleBucket>,
    now: DateTime<Utc>,
    source_note: &str,
) -> Vec<ResetCycleUsage> {
    buckets
        .into_iter()
        .filter_map(|bucket| {
            let usage = cycle_period(
                bucket.accumulator,
                now,
                source_note,
                Some(bucket.used_percent),
            )?;
            Some(ResetCycleUsage {
                started_at: bucket.started_at,
                ended_at: bucket.ended_at,
                scheduled_reset_at: bucket.scheduled_reset_at,
                usage,
            })
        })
        .collect()
}

fn merge_persisted_cycles(
    storage: &Storage,
    account_identity: Option<&str>,
    fresh: Vec<ResetCycleUsage>,
    credit_mode: bool,
) -> Result<Vec<ResetCycleUsage>, CodexError> {
    let Some(identity) = account_identity else {
        let mut fresh = fresh;
        fresh.sort_by_key(|cycle| std::cmp::Reverse(cycle.started_at));
        return Ok(fresh);
    };
    let key = if credit_mode {
        "weekly-credits-v1"
    } else {
        "weekly-api-v2"
    };
    let mut cycles = storage.load_reset_cycles("codex", identity, key)?;
    for cycle in fresh {
        let key = cycle
            .started_at
            .timestamp()
            .div_euclid(CYCLE_CLUSTER_SECONDS);
        if let Some(existing) = cycles.iter_mut().find(|existing| {
            existing
                .started_at
                .timestamp()
                .div_euclid(CYCLE_CLUSTER_SECONDS)
                == key
        }) {
            *existing = cycle;
        } else {
            cycles.push(cycle);
        }
    }
    cycles.sort_by_key(|cycle| cycle.started_at);
    for index in 0..cycles.len().saturating_sub(1) {
        cycles[index].ended_at = Some(cycles[index + 1].started_at);
    }
    storage.save_reset_cycles("codex", identity, key, &cycles)?;
    cycles.reverse();
    Ok(cycles)
}

fn quota_used_percent(quotas: &[QuotaWindow], id: &str) -> Option<f64> {
    quotas
        .iter()
        .find(|quota| quota.id == id)
        .map(|quota| quota.used_percent.clamp(0.0, 100.0))
}

fn cycle_period(
    accumulator: DailyUsageAccumulator,
    now: DateTime<Utc>,
    source_note: &str,
    used_percent: Option<f64>,
) -> Option<UsagePeriod> {
    let mut period = accumulator.build(now, source_note).last_30_days?;
    if let Some(used_percent) = used_percent.filter(|percent| *percent > 0.0) {
        period.quota_used_percent = Some(used_percent);
        period.estimated_limit_usd = period
            .estimated_cost_usd
            .filter(|cost| *cost > 0.0)
            .map(|cost| cost / (used_percent / 100.0));
    }
    Some(period)
}

fn scan_codex_events(
    storage: &Storage,
    homes: &[PathBuf],
    since_date: NaiveDate,
) -> Result<Vec<TokenEvent>, CodexError> {
    let mut events = Vec::new();
    let paths = discover_session_files(homes);
    let mut seen_paths = HashSet::with_capacity(paths.len());

    for path in paths {
        seen_paths.insert(path.clone());
        let Some(parsed) = load_or_parse_log(
            storage,
            "codex",
            &path,
            LOG_CACHE_SCHEMA_VERSION,
            parse_jsonl,
        )
        .map_err(|error| match error {
            LogCacheError::Storage(_) => CodexError::Storage,
            LogCacheError::Encode(_) => CodexError::LocalUsage,
        })?
        else {
            continue;
        };
        events.extend(
            parsed
                .into_iter()
                .filter(|event| event.timestamp.with_timezone(&Local).date_naive() >= since_date),
        );
    }
    storage.prune_log_events("codex", &seen_paths)?;
    Ok(events)
}

fn codex_homes(configured_home: Option<&OsStr>, home: &Path) -> Vec<PathBuf> {
    if let Some(configured_home) = configured_home.filter(|value| !value.is_empty()) {
        let configured_home = configured_home.to_string_lossy();
        if !configured_home.trim().is_empty() {
            return configured_home
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| expand_home(value, home))
                .collect();
        }
    }
    vec![home.join(".codex")]
}

fn expand_home(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        return home.to_path_buf();
    }
    if let Some(relative) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        return home.join(relative);
    }
    PathBuf::from(value)
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn discover_session_files(homes: &[PathBuf]) -> Vec<PathBuf> {
    let mut output = Vec::new();
    let mut seen_directories = HashSet::new();
    for home in homes {
        let sources = [home.join("sessions"), home.join("archived_sessions")]
            .into_iter()
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        let sources = if sources.is_empty() {
            vec![home.clone()]
        } else {
            sources
        };
        let mut seen_relative = HashSet::new();
        for source in sources {
            let source = fs::canonicalize(&source).unwrap_or(source);
            if !seen_directories.insert(source.clone()) {
                continue;
            }
            let mut source_files = WalkDir::new(&source)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.into_path())
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
                .collect::<Vec<_>>();
            source_files.sort();
            for path in source_files {
                let relative = path.strip_prefix(&source).unwrap_or(&path).to_path_buf();
                if seen_relative.insert(relative) {
                    output.push(path);
                }
            }
        }
    }
    output
}

pub fn parse_jsonl(content: &str) -> Vec<TokenEvent> {
    let mut current_model: Option<String> = None;
    let mut current_tier_is_fast = false;
    let mut previous_totals: Option<RawUsage> = None;
    let mut saw_session_meta = false;
    let mut replay_gate: Option<ChildReplayGate> = None;
    let mut events = Vec::new();

    for line in content.lines() {
        let is_turn_context = line.contains("\"type\":\"turn_context\"");
        let is_session_meta = !saw_session_meta && line.contains("\"type\":\"session_meta\"");
        let is_task_started = replay_gate.is_some() && line.contains("\"type\":\"task_started\"");
        let is_thread_settings = line.contains("\"type\":\"thread_settings_applied\"");
        if !is_turn_context
            && !is_session_meta
            && !is_task_started
            && !is_thread_settings
            && !line.contains("\"type\":\"token_count\"")
        {
            continue;
        }
        let Ok(object) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let object_type = object.get("type").and_then(Value::as_str);
        let payload = object.get("payload");

        if object_type == Some("turn_context") {
            if let Some(tier) = payload.and_then(service_tier) {
                current_tier_is_fast = matches!(tier, "fast" | "priority");
            }
            if let Some(model) = model_name(object.get("payload")) {
                current_model = Some(model);
            }
            continue;
        }

        if object_type == Some("session_meta") && !saw_session_meta {
            saw_session_meta = true;
            if payload.is_some_and(is_child_session_meta) {
                replay_gate = Some(
                    object
                        .get("timestamp")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .and_then(parse_log_timestamp)
                        .map(|timestamp| ChildReplayGate::UntilStartedAt(timestamp.timestamp()))
                        .unwrap_or(ChildReplayGate::UntilSelfTimedTaskStarted),
                );
            }
            continue;
        }

        let Some(payload) = payload else {
            continue;
        };
        if object_type != Some("event_msg") {
            continue;
        }

        let payload_type = payload.get("type").and_then(Value::as_str);
        if payload_type == Some("thread_settings_applied") {
            if let Some(tier) = service_tier(payload) {
                current_tier_is_fast = matches!(tier, "fast" | "priority");
            }
            continue;
        }

        if payload_type == Some("task_started") {
            if replay_gate.is_some_and(|gate| gate.is_cleared(payload, object.get("timestamp"))) {
                replay_gate = None;
            }
            continue;
        }

        if payload_type != Some("token_count") {
            continue;
        }
        let Some(timestamp_raw) = object
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::trim)
        else {
            continue;
        };
        let Some(timestamp) = parse_log_timestamp(timestamp_raw) else {
            continue;
        };
        let info = payload.get("info");
        let totals = info
            .and_then(|value| value.get("total_token_usage"))
            .map(RawUsage::from_value);

        if replay_gate.is_some() {
            if let Some(totals) = totals {
                previous_totals = Some(totals);
            }
            continue;
        }

        if totals.is_some_and(|totals| previous_totals == Some(totals)) {
            continue;
        }

        let usage = if let Some(last) = info.and_then(|value| value.get("last_token_usage")) {
            RawUsage::from_value(last)
        } else if let Some(totals) = totals {
            totals.subtracting(previous_totals)
        } else {
            continue;
        };
        if let Some(totals) = totals {
            previous_totals = Some(totals);
        }
        if usage.input == 0 && usage.cached == 0 && usage.output == 0 && usage.reasoning == 0 {
            continue;
        }
        let parsed_model = model_name(Some(payload)).or_else(|| model_name(info));
        let model = resolve_model(parsed_model, &mut current_model);
        events.push(TokenEvent {
            timestamp,
            model,
            input: usage.input,
            cached: usage.cached.min(usage.input),
            output: usage.output,
            reasoning: usage.reasoning,
            total: usage.total,
            is_fast: current_tier_is_fast,
        });
    }
    events
}

fn service_tier(payload: &Value) -> Option<&str> {
    [
        payload
            .get("thread_settings")
            .and_then(|settings| settings.get("service_tier")),
        payload.get("service_tier"),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

#[derive(Debug, Clone, Copy)]
enum ChildReplayGate {
    UntilStartedAt(i64),
    UntilSelfTimedTaskStarted,
}

impl ChildReplayGate {
    fn is_cleared(self, payload: &Value, line_timestamp: Option<&Value>) -> bool {
        let Some(started_at) = payload.get("started_at").and_then(Value::as_f64) else {
            return false;
        };
        match self {
            Self::UntilStartedAt(gate) => started_at >= gate as f64,
            Self::UntilSelfTimedTaskStarted => line_timestamp
                .and_then(Value::as_str)
                .map(str::trim)
                .and_then(parse_log_timestamp)
                .is_some_and(|timestamp| started_at >= timestamp.timestamp() as f64),
        }
    }
}

fn is_child_session_meta(payload: &Value) -> bool {
    has_non_null_value(payload.get("forked_from_id"))
        || has_non_null_value(payload.get("parent_thread_id"))
        || payload.get("thread_source").and_then(Value::as_str) == Some("subagent")
        || payload
            .get("source")
            .and_then(|source| source.get("subagent"))
            .is_some_and(|value| has_non_null_value(Some(value)))
}

fn has_non_null_value(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(_) => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawUsage {
    input: u64,
    cached: u64,
    output: u64,
    reasoning: u64,
    total: u64,
}

impl RawUsage {
    fn from_value(value: &Value) -> Self {
        let input = integer(value, &["input_tokens", "prompt_tokens", "input"]);
        let cached = integer(
            value,
            &[
                "cached_input_tokens",
                "cache_read_input_tokens",
                "cached_tokens",
            ],
        );
        let output = integer(value, &["output_tokens", "completion_tokens", "output"]);
        let reasoning = integer(value, &["reasoning_output_tokens", "reasoning_tokens"]);
        let reported = integer(value, &["total_tokens"]);
        let recomputed = input + output + reasoning;
        Self {
            input,
            cached,
            output,
            reasoning,
            total: if reported > 0 || recomputed == 0 {
                reported
            } else {
                recomputed
            },
        }
    }

    fn subtracting(self, previous: Option<Self>) -> Self {
        let previous = previous.unwrap_or(Self {
            input: 0,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 0,
        });
        Self {
            input: self.input.saturating_sub(previous.input),
            cached: self.cached.saturating_sub(previous.cached),
            output: self.output.saturating_sub(previous.output),
            reasoning: self.reasoning.saturating_sub(previous.reasoning),
            total: self.total.saturating_sub(previous.total),
        }
    }
}

fn integer(value: &Value, keys: &[&str]) -> u64 {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .unwrap_or_default()
}

fn model_name(value: Option<&Value>) -> Option<String> {
    let value = value?;
    [
        value.get("model"),
        value.get("model_name"),
        value
            .get("metadata")
            .and_then(|metadata| metadata.get("model")),
    ]
    .into_iter()
    .flatten()
    .find_map(|value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn resolve_model(parsed: Option<String>, current_model: &mut Option<String>) -> String {
    if let Some(parsed) = parsed.as_ref() {
        *current_model = Some(parsed.clone());
    }
    parsed.or_else(|| current_model.clone()).unwrap_or_else(|| {
        *current_model = Some("gpt-5".into());
        "gpt-5".into()
    })
}

fn auto_review_fallback(timestamp: &DateTime<Utc>) -> &'static str {
    let date = timestamp.date_naive();
    [
        ((2026, 4, 23), "gpt-5.5"),
        ((2026, 3, 5), "gpt-5.4"),
        ((2026, 2, 5), "gpt-5.3-codex"),
        ((2025, 12, 11), "gpt-5.2-codex"),
        ((2025, 11, 13), "gpt-5.1-codex"),
        ((2025, 9, 15), "gpt-5-codex"),
        ((2025, 8, 7), "gpt-5"),
    ]
    .into_iter()
    .find(|((year, month, day), _)| {
        date >= NaiveDate::from_ymd_opt(*year, *month, *day).expect("valid release date")
    })
    .map(|(_, model)| model)
    .unwrap_or("gpt-5")
}

#[cfg(test)]
fn aggregate(events: Vec<TokenEvent>, now: DateTime<Utc>, pricing: &ModelPricing) -> UsageHistory {
    let mut accumulator = DailyUsageAccumulator::default();
    let mut session = DailyUsageAccumulator::default();
    let mut weekly = DailyUsageAccumulator::default();
    let mut cycles = Vec::new();
    aggregate_into(
        &events,
        now,
        pricing,
        &mut accumulator,
        None,
        &mut session,
        None,
        &mut weekly,
        &mut cycles,
    );
    accumulator.build(now, "From your Codex logs (estimated)")
}

#[allow(clippy::too_many_arguments)]
fn aggregate_into(
    events: &[TokenEvent],
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    accumulator: &mut DailyUsageAccumulator,
    session_start: Option<DateTime<Utc>>,
    session_accumulator: &mut DailyUsageAccumulator,
    weekly_start: Option<DateTime<Utc>>,
    weekly_accumulator: &mut DailyUsageAccumulator,
    cycle_buckets: &mut [CycleBucket],
) {
    let today = now.with_timezone(&Local).date_naive();
    let since = today.checked_sub_days(Days::new(30)).unwrap_or(today);
    let mut seen = HashSet::new();

    for event in events.iter().filter(|event| event.timestamp <= now) {
        let key = (
            event.timestamp,
            event.model.clone(),
            event.input,
            event.cached,
            event.output,
            event.reasoning,
            event.total,
            event.is_fast,
        );
        if !seen.insert(key) {
            continue;
        }
        let date = event.timestamp.with_timezone(&Local).date_naive();
        if let Some(cost) = estimate_cost(event, pricing) {
            add_cycle_event(
                cycle_buckets,
                event.timestamp,
                &event.model,
                event.total,
                Some(cost),
                "Codex",
            );
            if date < since {
                continue;
            }
            add_priced_event(accumulator, date, &event.model, event.total, cost, "Codex");
            if session_start.is_some_and(|start| event.timestamp >= start) {
                add_priced_event(
                    session_accumulator,
                    date,
                    &event.model,
                    event.total,
                    cost,
                    "Codex",
                );
            }
            if weekly_start.is_some_and(|start| event.timestamp >= start) {
                add_priced_event(
                    weekly_accumulator,
                    date,
                    &event.model,
                    event.total,
                    cost,
                    "Codex",
                );
            }
        } else if event.total > 0 {
            if date >= since {
                accumulator.add_unknown_model(date, &event.model);
            }
            if session_start.is_some_and(|start| event.timestamp >= start) {
                session_accumulator.add_unknown_model(date, &event.model);
            }
            if weekly_start.is_some_and(|start| event.timestamp >= start) {
                weekly_accumulator.add_unknown_model(date, &event.model);
            }
            add_cycle_event(
                cycle_buckets,
                event.timestamp,
                &event.model,
                event.total,
                None,
                "Codex",
            );
        }
    }
}

fn add_priced_event(
    accumulator: &mut DailyUsageAccumulator,
    date: NaiveDate,
    model: &str,
    tokens: u64,
    cost: f64,
    source: &str,
) {
    accumulator.add_variant(date, tokens, cost, model.trim(), source);
}

fn estimate_cost(event: &TokenEvent, pricing: &ModelPricing) -> Option<f64> {
    estimate_token_cost(
        &event.timestamp,
        &event.model,
        TokenBreakdown {
            input: event.input.saturating_sub(event.cached),
            cache_read: event.cached,
            output: event.output,
            is_fast: event.is_fast,
            ..TokenBreakdown::default()
        },
        pricing,
    )
}

pub(crate) fn estimate_token_cost(
    timestamp: &DateTime<Utc>,
    raw_model: &str,
    tokens: TokenBreakdown,
    pricing: &ModelPricing,
) -> Option<f64> {
    let display_model = raw_model.trim();
    let model = if display_model == "codex-auto-review" {
        auto_review_fallback(timestamp)
    } else {
        display_model
    };
    if pricing.codex_credit_mode {
        return crate::pricing::codex_credits::equivalent_usd(pricing, model, tokens);
    }
    let canonical = pricing.supplement.canonical_name(model).unwrap_or(model);
    let fast_base = canonical
        .strip_suffix("-fast")
        .filter(|base| !base.is_empty());
    let rate_model = fast_base.unwrap_or(canonical);
    let base_rates = pricing.resolve(rate_model);
    let rates = base_rates.or_else(|| pricing.resolve(model))?;
    let applies_fast_tier = if fast_base.is_some() {
        base_rates.is_some()
    } else {
        tokens.is_fast
    };
    Some(codex_cost(rates, tokens, rate_model, applies_fast_tier))
}

fn codex_cost(
    mut rates: ModelRates,
    mut tokens: TokenBreakdown,
    model: &str,
    fast_tier: bool,
) -> f64 {
    if let Some((input, output, cache_read)) = codex_long_context_rates(model) {
        rates.input_above_200k_per_million = Some(input);
        rates.output_above_200k_per_million = Some(output);
        rates.cache_read_above_200k_per_million = Some(cache_read);
        rates.cache_write_above_200k_per_million = Some(input * 1.25);
        rates.long_context_threshold_tokens = 272_000;
    }
    if codex_model_has_no_cache_discount(model) || !rates.cache_read_is_explicit {
        rates.cache_read_per_million = rates.input_per_million;
        rates.cache_read_above_200k_per_million = rates.input_above_200k_per_million;
    }
    rates.fast_multiplier = codex_priority_multiplier(model, rates);
    tokens.is_fast = fast_tier;
    rates.cost_dollars(tokens, true)
}

fn codex_priority_multiplier(model: &str, rates: ModelRates) -> f64 {
    match dated_base_model(model) {
        "gpt-5.5" | "gpt-5.5-pro" => 2.5,
        "gpt-5.4" | "gpt-5.4-pro" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => 2.0,
        _ if rates.fast_multiplier == 1.0 => 2.0,
        _ => rates.fast_multiplier,
    }
}

fn codex_model_has_no_cache_discount(model: &str) -> bool {
    matches!(dated_base_model(model), "gpt-5.4-pro" | "gpt-5.5-pro")
}

fn codex_long_context_rates(model: &str) -> Option<(f64, f64, f64)> {
    match dated_base_model(model) {
        "gpt-5.4" => Some((5.0, 22.5, 0.5)),
        "gpt-5.4-pro" => Some((60.0, 270.0, 60.0)),
        "gpt-5.5" => Some((10.0, 45.0, 1.0)),
        "gpt-5.5-pro" => Some((60.0, 270.0, 60.0)),
        "gpt-6-astra" => Some((20.0, 75.0, 2.0)),
        "gpt-5.6-sol" => Some((8.0, 30.0, 0.8)),
        "gpt-5.6-terra" => Some((4.0, 18.0, 0.4)),
        "gpt-5.6-luna" => Some((0.4, 1.8, 0.04)),
        _ => None,
    }
}

fn dated_base_model(model: &str) -> &str {
    let bytes = model.as_bytes();
    if bytes.len() >= 11 {
        let suffix = &bytes[bytes.len() - 11..];
        if suffix[0] == b'-'
            && suffix[1..5].iter().all(u8::is_ascii_digit)
            && suffix[5] == b'-'
            && suffix[6..8].iter().all(u8::is_ascii_digit)
            && suffix[8] == b'-'
            && suffix[9..11].iter().all(u8::is_ascii_digit)
        {
            return &model[..model.len() - 11];
        }
    }
    if bytes.len() >= 9 {
        let suffix = &bytes[bytes.len() - 9..];
        if suffix[0] == b'-' && suffix[1..].iter().all(u8::is_ascii_digit) {
            return &model[..model.len() - 9];
        }
    }
    model
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs, io, path::Path};

    use chrono::{NaiveDate, TimeZone, Utc};
    use tempfile::tempdir;

    use super::{
        aggregate, codex_homes, codex_long_context_rates, codex_priority_multiplier, cycle_period,
        discover_session_files, estimate_cost, parse_jsonl, scan_codex_events, TokenEvent,
        LOG_CACHE_SCHEMA_VERSION,
    };
    use crate::{
        pricing::{
            test_bundled_pricing, ModelPricing, ModelRates, PricingCatalog, PricingSupplement,
            TokenBreakdown,
        },
        providers::daily_usage::DailyUsageAccumulator,
        providers::log_usage::LogFileFingerprint,
        storage::Storage,
    };

    #[test]
    fn astra_turn_tier_reprices_each_event_for_api_and_credits() {
        let now = Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap();
        let content = r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","service_tier":"fast"}}
{"timestamp":"2026-09-07T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100000,"cached_input_tokens":20000,"output_tokens":1000,"reasoning_output_tokens":500,"total_tokens":101000}}}}
{"type":"turn_context","payload":{"model":"gpt-6-astra","service_tier":"default"}}
{"timestamp":"2026-09-07T10:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100000,"cached_input_tokens":20000,"output_tokens":1000,"total_tokens":101000}}}}"#;
        let events = parse_jsonl(content);
        assert!(events[0].is_fast);
        assert!(!events[1].is_fast);
        let mut pricing = test_bundled_pricing();
        let api = aggregate(events.clone(), now, &pricing);
        pricing.codex_credit_mode = true;
        let credit = aggregate(events, now, &pricing);
        // Cached input is subtracted from input, reasoning is already part of output.
        assert!((api.today.unwrap().estimated_cost_usd.unwrap() - 2.61).abs() < 1e-9);
        assert!((credit.today.unwrap().estimated_cost_usd.unwrap() - 3.045).abs() < 1e-9);
        assert_eq!(api.daily[0].tokens, 202_000);
        assert_eq!(credit.daily[0].tokens, 202_000);
    }

    #[test]
    fn astra_api_long_context_threshold_is_strict_and_applies_once() {
        let pricing = test_bundled_pricing();
        let cost = |input| {
            super::estimate_token_cost(
                &Utc::now(),
                "gpt-6-astra",
                TokenBreakdown {
                    input,
                    output: 1_000,
                    ..Default::default()
                },
                &pricing,
            )
            .unwrap()
        };
        assert!((cost(272_000) - 2.77).abs() < 1e-9);
        assert!((cost(272_001) - 5.51502).abs() < 1e-9);
    }

    #[test]
    fn parses_last_usage_and_tracks_turn_model() {
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"turn_context","payload":{"model":"gpt-5.5"}}
{"timestamp":"2026-07-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":10,"reasoning_output_tokens":5,"total_tokens":115}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, "gpt-5.5");
        assert_eq!(events[0].total, 115);
        assert_eq!(events[0].cached, 20);
    }

    #[test]
    fn cumulative_totals_become_deltas() {
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}
{"timestamp":"2026-07-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":160,"output_tokens":20,"total_tokens":180}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].input, 60);
        assert_eq!(events[1].output, 10);
    }

    #[test]
    fn auto_review_keeps_its_model_name() {
        let content = r#"{"timestamp":"2026-03-10T08:00:00Z","type":"turn_context","payload":{"model":"codex-auto-review"}}
{"timestamp":"2026-03-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(events[0].model, "codex-auto-review");
    }

    #[test]
    fn auto_review_uses_fallback_rates_but_keeps_its_breakdown_label() {
        let now = Utc.with_ymd_and_hms(2026, 3, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-03-10T08:00:00Z","type":"turn_context","payload":{"model":"codex-auto-review"}}
{"timestamp":"2026-03-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100000,"output_tokens":100000,"total_tokens":200000}}}}"#;
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-5.4".into(), ModelRates::new(2.0, 8.0))]),
                retrieved_at: None,
            },
            PricingCatalog::default(),
        );

        let history = aggregate(parse_jsonl(content), now, &pricing);
        let today = history.today.unwrap();
        let breakdown = today.model_breakdown.unwrap();

        assert_eq!(today.estimated_cost_usd, Some(1.0));
        assert!(today.unknown_models.is_empty());
        assert_eq!(breakdown.models.len(), 1);
        assert_eq!(breakdown.models[0].model, "codex-auto-review");
        assert_eq!(breakdown.models[0].cost_usd, Some(1.0));
    }

    #[test]
    fn subagent_replay_seeds_the_cumulative_baseline() {
        let content = r#"{"timestamp":"2026-05-12T08:03:00Z","type":"session_meta","payload":{"source":{"subagent":{"thread_spawn":true}}}}
{"timestamp":"2026-05-12T08:03:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":100,"output_tokens":200,"total_tokens":1200}}}}
{"timestamp":"2026-05-12T08:04:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1500,"cached_input_tokens":150,"output_tokens":300,"total_tokens":1800}}}}
{"timestamp":"2026-05-12T08:04:30Z","type":"event_msg","payload":{"type":"task_started","started_at":1}}
{"timestamp":"2026-05-12T08:05:00Z","type":"event_msg","payload":{"type":"task_started","started_at":9999999999}}
{"timestamp":"2026-05-12T08:06:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1600,"cached_input_tokens":160,"output_tokens":320,"total_tokens":1920}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input, 100);
        assert_eq!(events[0].cached, 10);
        assert_eq!(events[0].output, 20);
        assert_eq!(events[0].total, 120);
    }

    #[test]
    fn child_without_a_live_task_emits_no_replayed_usage() {
        let content = r#"{"timestamp":"2026-05-12T08:03:00Z","type":"session_meta","payload":{"forked_from_id":"parent"}}
{"timestamp":"2026-05-12T08:04:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120},"total_token_usage":{"input_tokens":1000,"output_tokens":200,"total_tokens":1200}}}}"#;
        assert!(parse_jsonl(content).is_empty());
    }

    #[test]
    fn child_without_a_metadata_timestamp_opens_only_on_its_live_task() {
        let content = r#"{"type":"session_meta","payload":{"forked_from_id":"parent"}}
{"timestamp":"2026-05-12T08:03:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"output_tokens":200,"total_tokens":1200}}}}
{"timestamp":"2026-05-12T08:03:30Z","type":"event_msg","payload":{"type":"task_started","started_at":1}}
{"timestamp":"2026-05-12T08:04:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1500,"output_tokens":300,"total_tokens":1800}}}}
{"timestamp":"2026-05-12T08:05:00Z","type":"event_msg","payload":{"type":"task_started","started_at":9999999999}}
{"timestamp":"2026-05-12T08:06:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1600,"output_tokens":320,"total_tokens":1920}}}}"#;
        let events = parse_jsonl(content);

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].input, 100);
        assert_eq!(events[0].output, 20);
        assert_eq!(events[0].total, 120);
    }

    #[test]
    fn null_parent_metadata_keeps_root_session_usage() {
        let content = r#"{"timestamp":"2026-05-12T08:03:00Z","type":"session_meta","payload":{"forked_from_id":null,"parent_thread_id":" "}}
{"timestamp":"2026-05-12T08:04:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":20,"total_tokens":120}}}}"#;
        assert_eq!(parse_jsonl(content).len(), 1);
    }

    #[test]
    fn service_tier_is_attached_to_each_usage_event() {
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":" fast "}}}
{"timestamp":"2026-07-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}
{"timestamp":"2026-07-10T08:02:00Z","type":"event_msg","payload":{"type":"thread_settings_applied","service_tier":"default"}}
{"timestamp":"2026-07-10T08:03:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55}}}}"#;
        let events = parse_jsonl(content);
        assert!(events[0].is_fast);
        assert!(!events[1].is_fast);
    }

    #[test]
    fn unchanged_cumulative_snapshot_is_not_counted_twice() {
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110},"total_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}
{"timestamp":"2026-07-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110},"total_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#;
        assert_eq!(parse_jsonl(content).len(), 1);
    }

    #[test]
    fn accepts_trimmed_timestamps_and_rejects_numeric_strings() {
        let content = r#"{"timestamp":" 2026-07-10T08:00:00Z ","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}
{"timestamp":"2026-07-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":"100","output_tokens":"10","total_tokens":"110"}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].total, 110);
    }

    #[test]
    fn parses_cross_device_timestamp_offsets() {
        let content = r#"{"timestamp":"2026-07-15 15:00:00.123456+03:00","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(
            events[0].timestamp.to_rfc3339(),
            "2026-07-15T12:00:00.123+00:00"
        );
    }

    #[test]
    fn codex_home_override_supports_multiple_comma_separated_paths() {
        let directory = tempdir().unwrap();
        let default_home = directory.path().join("home");
        let first = directory.path().join("codex-work");
        let second = directory.path().join("codex-personal");
        let configured = format!("{}, {}", first.display(), second.display());

        assert_eq!(
            codex_homes(Some(configured.as_ref()), &default_home),
            vec![first, second]
        );
        assert_eq!(
            codex_homes(Some("~/.codex-alt".as_ref()), &default_home),
            vec![default_home.join(".codex-alt")]
        );
        assert_eq!(
            codex_homes(None, &default_home),
            vec![default_home.join(".codex")]
        );
        assert_eq!(
            codex_homes(Some("  \t ".as_ref()), &default_home),
            vec![default_home.join(".codex")]
        );
    }

    #[test]
    fn active_sessions_win_over_matching_archived_paths() {
        let directory = tempdir().unwrap();
        let home = directory.path();
        let relative = "2026/07/rollout.jsonl";
        let active = home.join("sessions").join(relative);
        let archived = home.join("archived_sessions").join(relative);
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::create_dir_all(archived.parent().unwrap()).unwrap();
        fs::write(&active, "active").unwrap();
        fs::write(&archived, "archived").unwrap();

        assert_eq!(
            discover_session_files(&[home.to_path_buf()]),
            vec![fs::canonicalize(active).unwrap()]
        );
    }

    #[test]
    fn discovers_logs_under_a_symlinked_sessions_root() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        let real_sessions = directory.path().join("real-sessions");
        let log = real_sessions.join("2026/07/rollout.jsonl");
        fs::create_dir_all(log.parent().unwrap()).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(&log, "{}").unwrap();
        if create_directory_symlink(&real_sessions, &home.join("sessions")).is_err() {
            return;
        }

        assert_eq!(
            discover_session_files(&[home]),
            vec![fs::canonicalize(log).unwrap()]
        );
    }

    #[test]
    fn scan_cache_picks_up_changed_and_new_logs_without_using_config_tier() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        let sessions = home.join("sessions");
        let first = sessions.join("rollout-a.jsonl");
        let second = sessions.join("rollout-b.jsonl");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(home.join("config.toml"), "service_tier = \"priority\"").unwrap();
        fs::write(
            &first,
            r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#,
        )
        .unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let since = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();

        let initial = scan_codex_events(&storage, std::slice::from_ref(&home), since).unwrap();
        assert_eq!(initial.len(), 1);
        assert!(!initial[0].is_fast);

        fs::write(
            &first,
            r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":130,"output_tokens":10,"total_tokens":140}}}}"#,
        )
        .unwrap();
        fs::write(
            &second,
            r#"{"timestamp":"2026-07-10T09:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":50,"output_tokens":5,"total_tokens":55}}}}"#,
        )
        .unwrap();

        let refreshed = scan_codex_events(&storage, &[home], since).unwrap();
        assert_eq!(refreshed.len(), 2);
        assert_eq!(refreshed.iter().map(|event| event.total).sum::<u64>(), 195);
        assert!(refreshed.iter().all(|event| !event.is_fast));
    }

    #[test]
    fn schema_upgrade_reparses_cached_auto_review_events() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        let sessions = home.join("sessions");
        let path = sessions.join("rollout.jsonl");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            &path,
            r#"{"timestamp":"2026-03-10T08:00:00Z","type":"turn_context","payload":{"model":"codex-auto-review"}}
{"timestamp":"2026-03-10T08:01:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}}"#,
        )
        .unwrap();
        let path = fs::canonicalize(path).unwrap();
        let fingerprint = LogFileFingerprint::from_metadata(&fs::metadata(&path).unwrap()).unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        storage
            .save_log_events(
                "codex",
                &path,
                fingerprint.size,
                fingerprint.modified_nanos,
                r#"{"schema_version":2,"events":[{"timestamp":"2026-03-10T08:01:00Z","model":"gpt-5.4","input":10,"cached":0,"output":5,"reasoning":0,"total":15,"is_fast":false}]}"#,
            )
            .unwrap();

        let events = scan_codex_events(
            &storage,
            &[home],
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, "codex-auto-review");
        let cached = storage
            .load_log_events("codex", &path, fingerprint.size, fingerprint.modified_nanos)
            .unwrap()
            .unwrap();
        let cached = serde_json::from_str::<serde_json::Value>(&cached).unwrap();
        assert_eq!(cached["schema_version"], LOG_CACHE_SCHEMA_VERSION);
        assert_eq!(cached["events"][0]["model"], "codex-auto-review");
        assert!(cached["events"][0].get("pricing_model").is_none());
    }

    #[cfg(unix)]
    fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_directory_symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[test]
    fn newly_priced_models_produce_a_complete_estimate() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.6-sol","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#;
        let pricing = test_bundled_pricing();
        let history = aggregate(parse_jsonl(content), now, &pricing);
        assert_eq!(history.today.as_ref().unwrap().tokens, 110);
        assert!(history.today.as_ref().unwrap().estimated_cost_usd.is_some());
        assert!(history.today.as_ref().unwrap().estimate_complete);
        assert!(history.unknown_models.is_empty());
    }

    #[test]
    fn cycle_api_value_infers_the_full_allowance_from_used_share() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let mut accumulator = DailyUsageAccumulator::default();
        accumulator.add_variant(now.date_naive(), 1_000, 25.0, "gpt-test", "Codex");

        let period = cycle_period(accumulator, now, "test", Some(20.0)).unwrap();
        assert_eq!(period.estimated_cost_usd, Some(25.0));
        assert_eq!(period.estimated_limit_usd, Some(125.0));
        assert_eq!(period.quota_used_percent, Some(20.0));
    }

    #[test]
    fn daybreak_uses_sol_rates_without_losing_its_breakdown_identity() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-daybreak-blue-latest","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#;
        let pricing = test_bundled_pricing();
        let history = aggregate(parse_jsonl(content), now, &pricing);
        let today = history.today.as_ref().unwrap();
        assert_eq!(today.tokens, 110);
        assert_eq!(today.estimated_cost_usd, Some(0.0006));
        assert!(today.estimate_complete);
        assert_eq!(
            today.model_breakdown.as_ref().unwrap().models[0].model,
            "gpt-daybreak-blue-latest"
        );
        assert!(history.unknown_models.is_empty());
    }

    #[test]
    fn period_breakdown_uses_model_names_and_excludes_unpriced_usage() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.4","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":100,"total_tokens":1100}}}}
{"timestamp":"2026-07-10T09:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.3-codex","info":{"last_token_usage":{"input_tokens":800,"output_tokens":100,"total_tokens":900}}}}
{"timestamp":"2026-07-10T10:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"future-unpriced-model","info":{"last_token_usage":{"input_tokens":400,"output_tokens":100,"total_tokens":500}}}}"#;
        let pricing = test_bundled_pricing();
        let history = aggregate(parse_jsonl(content), now, &pricing);
        let today = history.today.unwrap();
        let breakdown = today.model_breakdown.unwrap();

        assert_eq!(today.tokens, 2_000);
        assert_eq!(today.unknown_models, ["future-unpriced-model"]);
        assert_eq!(
            breakdown
                .models
                .iter()
                .map(|entry| entry.model.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.4", "gpt-5.3-codex"]
        );
        assert_eq!(breakdown.source_note, "From your Codex logs (estimated)");
    }

    #[test]
    fn unknown_only_usage_does_not_create_spend_periods() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-07-10T10:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"future-unpriced-model","info":{"last_token_usage":{"input_tokens":400,"output_tokens":100,"total_tokens":500}}}}"#;
        let pricing = test_bundled_pricing();
        let history = aggregate(parse_jsonl(content), now, &pricing);

        assert!(history.today.is_none());
        assert!(history.last_30_days.is_none());
        assert!(history.daily.is_empty());
        assert_eq!(history.unknown_models, ["future-unpriced-model"]);
    }

    #[test]
    fn provider_fixture_parses_realistic_codex_jsonl() {
        let content = include_str!("../../../tests/fixtures/codex_session.jsonl");
        let events = parse_jsonl(content);
        assert_eq!(events.len(), 2);
        assert_eq!(events.iter().map(|event| event.total).sum::<u64>(), 225);
        assert!(events.iter().all(|event| event.model == "gpt-5.4"));
    }

    #[test]
    fn event_fast_tier_defaults_to_two_x_multiplier() {
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("test-model".into(), ModelRates::new(2.0, 8.0))]),
                retrieved_at: None,
            },
            PricingCatalog::default(),
        );
        let event = TokenEvent {
            timestamp: Utc::now(),
            model: "test-model".into(),
            input: 1_000_000,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 1_000_000,
            is_fast: false,
        };
        assert_eq!(estimate_cost(&event, &pricing), Some(2.0));
        assert_eq!(
            estimate_cost(
                &TokenEvent {
                    is_fast: true,
                    ..event
                },
                &pricing
            ),
            Some(4.0)
        );
    }

    #[test]
    fn missing_cache_rate_provenance_uses_full_input_price() {
        let mut inferred_rates = ModelRates::new(2.0, 8.0);
        inferred_rates.cache_read_is_explicit = false;
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("test-model".into(), inferred_rates)]),
                retrieved_at: None,
            },
            PricingCatalog::default(),
        );
        let event = TokenEvent {
            timestamp: Utc::now(),
            model: "test-model".into(),
            input: 1_000_000,
            cached: 1_000_000,
            output: 0,
            reasoning: 0,
            total: 1_000_000,
            is_fast: false,
        };

        assert_eq!(estimate_cost(&event, &pricing), Some(2.0));
    }

    #[test]
    fn pro_model_overrides_an_explicit_legacy_cache_discount() {
        let mut rates = ModelRates::new(2.0, 8.0);
        rates.cache_read_per_million = 0.2;
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-5.5-pro".into(), rates)]),
                retrieved_at: None,
            },
            PricingCatalog::default(),
        );
        let event = TokenEvent {
            timestamp: Utc::now(),
            model: "gpt-5.5-pro".into(),
            input: 100_000,
            cached: 100_000,
            output: 0,
            reasoning: 0,
            total: 100_000,
            is_fast: false,
        };

        assert_eq!(estimate_cost(&event, &pricing), Some(0.2));
    }

    #[test]
    fn codex_long_context_rates_start_above_272k_prompt_tokens() {
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-5.4".into(), ModelRates::new(2.5, 15.0))]),
                retrieved_at: None,
            },
            PricingCatalog::default(),
        );
        let event = |input| TokenEvent {
            timestamp: Utc::now(),
            model: "gpt-5.4".into(),
            input,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: input,
            is_fast: false,
        };

        assert!((estimate_cost(&event(272_000), &pricing).unwrap() - 0.68).abs() < 0.000_001);
        assert!((estimate_cost(&event(272_001), &pricing).unwrap() - 1.360_005).abs() < 0.000_001);
    }

    #[test]
    fn codex_long_context_rate_matrix_matches_supported_model_families() {
        for (model, expected) in [
            ("gpt-5.4", (5.0, 22.5, 0.5)),
            ("gpt-5.4-pro", (60.0, 270.0, 60.0)),
            ("gpt-5.5", (10.0, 45.0, 1.0)),
            ("gpt-5.5-pro", (60.0, 270.0, 60.0)),
            ("gpt-5.6-sol", (8.0, 30.0, 0.8)),
            ("gpt-5.6-terra", (4.0, 18.0, 0.4)),
            ("gpt-5.6-luna", (0.4, 1.8, 0.04)),
        ] {
            assert_eq!(codex_long_context_rates(model), Some(expected), "{model}");
        }
        assert_eq!(
            codex_long_context_rates("gpt-5.5-2026-04-23"),
            Some((10.0, 45.0, 1.0))
        );
        assert_eq!(codex_long_context_rates("gpt-5.3-codex"), None);
    }

    #[test]
    fn codex_priority_multiplier_matrix_overrides_generic_fast_prices() {
        let mut catalog_rates = ModelRates::new(2.0, 8.0);
        catalog_rates.fast_multiplier = 3.0;

        for model in ["gpt-5.5", "gpt-5.5-pro-20260423"] {
            assert_eq!(codex_priority_multiplier(model, catalog_rates), 2.5);
        }
        for model in [
            "gpt-5.4",
            "gpt-5.4-pro",
            "gpt-5.6-sol",
            "gpt-5.6-terra-2026-07-01",
            "gpt-5.6-luna",
        ] {
            assert_eq!(
                codex_priority_multiplier(model, catalog_rates),
                2.0,
                "{model}"
            );
        }
        assert_eq!(
            codex_priority_multiplier("future-model", catalog_rates),
            3.0
        );
        assert_eq!(
            codex_priority_multiplier("future-model", ModelRates::new(2.0, 8.0)),
            2.0
        );
    }

    #[test]
    fn fast_alias_uses_unscaled_base_rates_and_one_codex_multiplier() {
        let supplement = PricingSupplement::decode(
            br#"{"pricing":{},"fast_multipliers":{"gpt-5.5":2.5},"alias_rules":[]}"#,
        )
        .unwrap();
        let pricing = ModelPricing::new(
            supplement,
            PricingCatalog {
                entries: HashMap::from([("gpt-5.5".into(), ModelRates::new(2.0, 8.0))]),
                retrieved_at: None,
            },
            PricingCatalog::default(),
        );
        let event = TokenEvent {
            timestamp: Utc::now(),
            model: "gpt-5.5-fast".into(),
            input: 100_000,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 100_000,
            is_fast: true,
        };

        assert_eq!(estimate_cost(&event, &pricing), Some(0.5));
    }

    #[test]
    fn fast_only_catalog_model_keeps_its_existing_rate_without_a_second_multiplier() {
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog::default(),
            PricingCatalog {
                entries: HashMap::from([("vendor-model-fast".into(), ModelRates::new(6.0, 20.0))]),
                retrieved_at: None,
            },
        );
        let event = TokenEvent {
            timestamp: Utc::now(),
            model: "vendor-model-fast".into(),
            input: 100_000,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 100_000,
            is_fast: true,
        };

        assert_eq!(estimate_cost(&event, &pricing), Some(0.6));
    }
}
