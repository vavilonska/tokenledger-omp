use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Days, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use walkdir::WalkDir;

use crate::{
    models::{CycleModelUsage, QuotaWindow, ResetCycleUsage, UsageHistory, UsagePeriod},
    pricing::{ModelPricing, ModelRates, RequestSpeed, TokenBreakdown},
    storage::{CycleRecovery, Storage, StoredResetCycle},
};

use super::CodexError;
use crate::providers::{
    daily_usage::DailyUsageAccumulator,
    log_usage::{parse_log_timestamp, retain_log_usage, usage_event_key, LogCacheError},
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
    #[serde(
        default,
        alias = "is_fast",
        deserialize_with = "deserialize_codex_speed"
    )]
    pub speed: RequestSpeed,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
}

// Codex's legacy boolean was derived directly from its persisted request tier.
fn deserialize_codex_speed<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<RequestSpeed, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StoredSpeed {
        Current(RequestSpeed),
        Legacy(bool),
    }
    Ok(match StoredSpeed::deserialize(deserializer)? {
        StoredSpeed::Current(speed) => speed,
        StoredSpeed::Legacy(true) => RequestSpeed::Fast,
        StoredSpeed::Legacy(false) => RequestSpeed::Standard,
    })
}

const LOG_CACHE_SCHEMA_VERSION: u8 = 6;
const CYCLE_CLUSTER_SECONDS: i64 = 5 * 60;

struct CycleBucket {
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    scheduled_reset_at: DateTime<Utc>,
    used_percent: f64,
    observed_tokens: u64,
    recovered_tokens: u64,
    recovery: Option<CycleRecovery>,
    accumulator: DailyUsageAccumulator,
    model_usage: HashMap<String, CycleModelUsage>,
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
    // Retained facts are needed even when their source files and recent anchors are gone.
    let events = scan_codex_events(storage, &homes, NaiveDate::MIN, account_identity)?;
    let persisted_cycles = match account_identity {
        Some(identity) => storage.load_reset_cycles(
            "codex",
            identity,
            cycle_storage_key(pricing.codex_credit_mode),
        )?,
        None => Vec::new(),
    };

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
    let mut cycle_buckets = restore_cycle_buckets(anchors, &persisted_cycles, now);
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
            event.cost.is_none() && !model_has_rates(&event.timestamp, &event.model, pricing),
        );
    }
    let includes_pi = match pi_usage::scan_into(
        storage,
        now,
        pricing,
        "codex",
        account_identity,
        &mut accumulator,
    ) {
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
            "{} logs · purchased/PAYG Codex credit estimate · 2,500 credits = $100 · separate from API pricing",
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
    let mut fresh_cycles = build_cycle_history(cycle_buckets, now, source_note);
    usage.weekly_cycle = current_weekly_period(
        &mut fresh_cycles,
        weekly_start,
        now,
        quota_used_percent(quotas, "weekly"),
    )
    .or_else(|| {
        cycle_period(
            weekly_accumulator,
            now,
            source_note,
            quota_used_percent(quotas, "weekly"),
        )
    });
    usage.weekly_cycles = persist_cycles(
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
            observed_tokens: 0,
            recovered_tokens: 0,
            recovery: None,
            accumulator: DailyUsageAccumulator::default(),
            model_usage: HashMap::new(),
        })
        .collect()
}

fn restore_cycle_buckets(
    mut anchors: Vec<omp_usage::QuotaCycleAnchor>,
    persisted: &[StoredResetCycle],
    now: DateTime<Utc>,
) -> Vec<CycleBucket> {
    let mut buckets: Vec<_> = persisted
        .iter()
        .map(|record| {
            let cycle = &record.cycle;
            CycleBucket {
                started_at: cycle.started_at,
                ended_at: cycle.ended_at,
                scheduled_reset_at: cycle.scheduled_reset_at,
                used_percent: cycle.usage.quota_used_percent.unwrap_or_default(),
                observed_tokens: 0,
                recovered_tokens: 0,
                recovery: Some(record.recovery.clone()),
                accumulator: DailyUsageAccumulator::default(),
                model_usage: HashMap::new(),
            }
        })
        .collect();
    // Reject conflicts before clustering: an interior anchor close to a stored
    // end must not swallow a valid anchor exactly at that end.
    anchors.retain(|candidate| {
        if candidate.started_at > now {
            return false;
        }
        if let Some(bucket) = buckets.iter_mut().find(|bucket| {
            (bucket.started_at - candidate.started_at)
                .num_seconds()
                .abs()
                <= CYCLE_CLUSTER_SECONDS
        }) {
            bucket.used_percent = bucket.used_percent.max(candidate.used_percent);
            return false;
        }
        // Closed boundaries are authoritative. An open snapshot cannot be split
        // inside its attested coverage without inventing a token attribution.
        if let Some(bucket) = buckets.iter_mut().find(|bucket| {
            candidate.started_at > bucket.started_at
                && match bucket.ended_at {
                    Some(end) => candidate.started_at < end,
                    None => bucket
                        .recovery
                        .as_ref()
                        .is_some_and(|recovery| candidate.started_at <= recovery.covered_until),
                }
        }) {
            if bucket.ended_at.is_none() {
                if let Some(recovery) = &mut bucket.recovery {
                    recovery.boundary_conflict = true;
                }
            }
            return false;
        }
        true
    });
    for mut candidate in build_cycle_buckets(anchors, now) {
        // Recompute adjacency only after rejecting conflicting candidates.
        candidate.ended_at = None;
        buckets.push(candidate);
    }
    buckets.sort_by_key(|bucket| bucket.started_at);
    for index in 0..buckets.len().saturating_sub(1) {
        if buckets[index].ended_at.is_none() {
            buckets[index].ended_at = Some(buckets[index + 1].started_at);
        }
    }
    buckets
}

fn add_cycle_event(
    buckets: &mut [CycleBucket],
    timestamp: DateTime<Utc>,
    model: &str,
    tokens: u64,
    cost: Option<f64>,
    source: &str,
    unknown_model: bool,
) {
    let Some(bucket) = buckets.iter_mut().rev().find(|bucket| {
        timestamp >= bucket.started_at
            && bucket.ended_at.is_none_or(|ended_at| timestamp < ended_at)
    }) else {
        return;
    };
    let model = model.trim();
    let model = if model.is_empty() {
        "Unattributed"
    } else {
        model
    };
    let entry = bucket
        .model_usage
        .entry(model.to_lowercase())
        .or_insert_with(|| CycleModelUsage {
            model: model.to_owned(),
            total_tokens: 0,
            request_count: Some(0),
            cost_usd: None,
            cost_complete: true,
        });
    entry.total_tokens = entry.total_tokens.saturating_add(tokens);
    entry.request_count = entry.request_count.map(|count| count.saturating_add(1));
    if let Some(cost) = cost {
        entry.cost_usd = Some(entry.cost_usd.unwrap_or_default() + cost);
    } else {
        entry.cost_complete = false;
    }
    bucket.observed_tokens = bucket.observed_tokens.saturating_add(tokens);
    if bucket
        .recovery
        .as_ref()
        .is_some_and(|recovery| timestamp <= recovery.covered_until)
    {
        bucket.recovered_tokens = bucket.recovered_tokens.saturating_add(tokens);
    }
    let date = timestamp.with_timezone(&Local).date_naive();
    if let Some(cost) = cost {
        add_priced_event(&mut bucket.accumulator, date, model, tokens, cost, source);
    } else if tokens > 0 {
        bucket
            .accumulator
            .add_unpriced_variant(date, tokens, model, source);
        if unknown_model {
            bucket.accumulator.add_unknown_model(date, model);
        }
    }
}

fn build_cycle_history(
    buckets: Vec<CycleBucket>,
    now: DateTime<Utc>,
    source_note: &str,
) -> Vec<StoredResetCycle> {
    buckets
        .into_iter()
        .filter_map(|mut bucket| {
            let had_snapshot = bucket.recovery.is_some();
            let recovery = bucket.recovery.unwrap_or(CycleRecovery {
                baseline_tokens: bucket.observed_tokens,
                covered_until: now,
                boundary_conflict: false,
            });
            let missing_tokens = if had_snapshot {
                recovery
                    .baseline_tokens
                    .saturating_sub(bucket.recovered_tokens)
            } else {
                0
            };
            if missing_tokens > 0 {
                bucket.accumulator.add_unpriced_variant(
                    bucket.started_at.with_timezone(&Local).date_naive(),
                    missing_tokens,
                    "Unattributed",
                    "Retained facts unavailable",
                );
                let entry = bucket
                    .model_usage
                    .entry("unattributed".to_owned())
                    .or_insert_with(|| CycleModelUsage {
                        model: "Unattributed".to_owned(),
                        total_tokens: 0,
                        request_count: None,
                        cost_usd: None,
                        cost_complete: false,
                    });
                entry.total_tokens = entry.total_tokens.saturating_add(missing_tokens);
                // A recovered balance is not an observed request, even when it
                // shares the fallback model name with a real event.
                entry.request_count = None;
                entry.cost_complete = false;
            }
            let mut usage = cycle_period(
                bucket.accumulator,
                now,
                source_note,
                Some(bucket.used_percent),
            )
            .or_else(|| {
                had_snapshot.then_some(UsagePeriod {
                    tokens: 0,
                    estimated_cost_usd: Some(0.0),
                    estimated_limit_usd: None,
                    quota_used_percent: Some(bucket.used_percent),
                    cost_estimated: true,
                    estimate_complete: true,
                    model_breakdown: None,
                    unknown_models: Vec::new(),
                })
            })?;
            if recovery.boundary_conflict && usage.tokens > 0 {
                usage.estimate_complete = false;
                usage.estimated_limit_usd = None;
            }
            let mut model_usage: Vec<_> = bucket.model_usage.into_values().collect();
            model_usage.sort_by(|left, right| {
                right
                    .total_tokens
                    .cmp(&left.total_tokens)
                    .then_with(|| left.model.cmp(&right.model))
            });
            Some(StoredResetCycle {
                cycle: ResetCycleUsage {
                    started_at: bucket.started_at,
                    ended_at: bucket.ended_at,
                    scheduled_reset_at: bucket.scheduled_reset_at,
                    usage,
                    model_usage: (!model_usage.is_empty()).then_some(model_usage),
                },
                recovery,
            })
        })
        .collect()
}

fn current_weekly_period(
    cycles: &mut [StoredResetCycle],
    weekly_start: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    used_percent: Option<f64>,
) -> Option<UsagePeriod> {
    let start = weekly_start?;
    let cycle = cycles
        .iter_mut()
        .map(|record| &mut record.cycle)
        .find(|cycle| {
            cycle.started_at <= now
                && cycle.ended_at.is_none_or(|end| now < end)
                && (start >= cycle.started_at
                    || (start - cycle.started_at).num_seconds().abs() <= CYCLE_CLUSTER_SECONDS)
        })?;
    // Use the same recovered subtotal as history, including its missing balance.
    let usage = &mut cycle.usage;
    usage.quota_used_percent = used_percent;
    usage.estimated_limit_usd = usage
        .estimated_cost_usd
        .filter(|cost| usage.estimate_complete && *cost > 0.0)
        .zip(used_percent.filter(|percent| *percent > 0.0))
        .map(|(cost, percent)| cost / (percent / 100.0));
    Some(usage.clone())
}

fn cycle_storage_key(credit_mode: bool) -> &'static str {
    if credit_mode {
        "weekly-credits-v1"
    } else {
        "weekly-api-v2"
    }
}

fn persist_cycles(
    storage: &Storage,
    account_identity: Option<&str>,
    mut cycles: Vec<StoredResetCycle>,
    credit_mode: bool,
) -> Result<Vec<ResetCycleUsage>, CodexError> {
    cycles.sort_by_key(|record| record.cycle.started_at);
    if let Some(identity) = account_identity {
        storage.save_reset_cycles("codex", identity, cycle_storage_key(credit_mode), &cycles)?;
    }
    cycles.reverse();
    Ok(cycles.into_iter().map(|record| record.cycle).collect())
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
            .filter(|cost| period.estimate_complete && *cost > 0.0)
            .map(|cost| cost / (used_percent / 100.0));
    }
    Some(period)
}

fn scan_codex_events(
    storage: &Storage,
    homes: &[PathBuf],
    since_date: NaiveDate,
    account_identity: Option<&str>,
) -> Result<Vec<TokenEvent>, CodexError> {
    let mut events = retain_log_usage(
        storage,
        "codex",
        account_identity.unwrap_or_default(),
        "codex",
        LOG_CACHE_SCHEMA_VERSION,
        4,
        &discover_session_files(homes),
        parse_jsonl,
        |event: &TokenEvent| {
            if let Some(id) = event.source_id.as_deref() {
                Ok(id.to_owned())
            } else {
                usage_event_key(&(event.timestamp, &event.model))
            }
        },
        |_| true,
    )
    .map_err(|error| match error {
        LogCacheError::Storage(_) => CodexError::Storage,
        LogCacheError::Encode(_) => CodexError::LocalUsage,
    })?;
    events.retain(|event| event.timestamp.with_timezone(&Local).date_naive() >= since_date);
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
    let mut current_speed = RequestSpeed::Standard;
    let mut previous_totals: Option<RawUsage> = None;
    let mut saw_session_meta = false;
    let mut session_id: Option<String> = None;
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
                current_speed = request_speed(tier);
            }
            if let Some(model) = model_name(object.get("payload")) {
                current_model = Some(model);
            }
            continue;
        }

        if object_type == Some("session_meta") && !saw_session_meta {
            saw_session_meta = true;
            session_id = payload
                .and_then(|value| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);
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
                current_speed = request_speed(tier);
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
        let response_id = payload
            .get("response_id")
            .or_else(|| payload.get("responseId"))
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let entry_id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let source_id = if let Some(response) = response_id {
            Some(
                usage_event_key(&("response", response))
                    .expect("Codex response identity is serializable"),
            )
        } else if let Some(session) = session_id.as_deref() {
            Some(
                usage_event_key(&("session", session, timestamp_raw, entry_id))
                    .expect("Codex session identity is serializable"),
            )
        } else {
            entry_id.map(|entry| {
                usage_event_key(&("entry", entry)).expect("Codex entry identity is serializable")
            })
        };
        events.push(TokenEvent {
            source_id,
            timestamp,
            model,
            input: usage.input,
            cached: usage.cached.min(usage.input),
            output: usage.output,
            reasoning: usage.reasoning,
            total: usage.total,
            speed: service_tier(payload)
                .map(request_speed)
                .unwrap_or(current_speed),
        });
    }
    events
}

fn request_speed(tier: &str) -> RequestSpeed {
    match tier {
        "ultrafast" => RequestSpeed::Ultrafast,
        "fast" | "priority" => RequestSpeed::Fast,
        _ => RequestSpeed::Standard,
    }
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
        // Empty/cancelled replies have no usage event, regardless of whether
        // this price basis happens to recognize their model.
        if event.total == 0
            && event.input == 0
            && event.cached == 0
            && event.output == 0
            && event.reasoning == 0
        {
            continue;
        }
        let key = (
            event.source_id.as_deref(),
            event.timestamp,
            event.model.as_str(),
        );
        if !seen.insert(key) {
            continue;
        }
        let cost = estimate_cost(event, pricing);
        let unknown_model =
            cost.is_none() && !model_has_rates(&event.timestamp, &event.model, pricing);
        add_cycle_event(
            cycle_buckets,
            event.timestamp,
            &event.model,
            event.total,
            cost,
            "Codex",
            unknown_model,
        );
        let date = event.timestamp.with_timezone(&Local).date_naive();
        if let Some(cost) = cost {
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
                accumulator.add_unpriced_variant(date, event.total, &event.model, "Codex");
                if unknown_model {
                    accumulator.add_unknown_model(date, &event.model);
                }
            }
            if session_start.is_some_and(|start| event.timestamp >= start) {
                session_accumulator.add_unpriced_variant(date, event.total, &event.model, "Codex");
                if unknown_model {
                    session_accumulator.add_unknown_model(date, &event.model);
                }
            }
            if weekly_start.is_some_and(|start| event.timestamp >= start) {
                weekly_accumulator.add_unpriced_variant(date, event.total, &event.model, "Codex");
                if unknown_model {
                    weekly_accumulator.add_unknown_model(date, &event.model);
                }
            }
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
            speed: event.speed,
            ..TokenBreakdown::default()
        },
        pricing,
    )
}

/// Whether the model has base rates for this display basis, independently of request speed.
pub(crate) fn model_has_rates(
    timestamp: &DateTime<Utc>,
    raw_model: &str,
    pricing: &ModelPricing,
) -> bool {
    let model = if raw_model.trim() == "codex-auto-review" {
        auto_review_fallback(timestamp)
    } else {
        raw_model.trim()
    };
    let canonical = pricing.supplement.canonical_name(model).unwrap_or(model);
    let base = canonical
        .strip_suffix("-ultrafast")
        .or_else(|| canonical.strip_suffix("-fast"))
        .unwrap_or(canonical);
    if pricing.codex_credit_mode {
        crate::pricing::codex_credits::equivalent_usd(pricing, base, TokenBreakdown::default())
            .is_some()
    } else {
        pricing.resolve(base).is_some() || pricing.resolve(canonical).is_some()
    }
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
    let (rate_model, suffix_speed) = if let Some(base) = canonical.strip_suffix("-ultrafast") {
        (base, RequestSpeed::Ultrafast)
    } else if let Some(base) = canonical.strip_suffix("-fast") {
        (base, RequestSpeed::Fast)
    } else {
        (canonical, RequestSpeed::Standard)
    };
    let rates = match pricing.resolve(rate_model) {
        Some(rates) => rates,
        None if suffix_speed == RequestSpeed::Fast && tokens.speed != RequestSpeed::Ultrafast => {
            // A catalog may contain only an already-priced Fast model.
            return codex_cost(
                pricing.resolve(canonical)?,
                tokens,
                canonical,
                RequestSpeed::Standard,
            );
        }
        None => return None,
    };
    let speed = if tokens.speed == RequestSpeed::Standard {
        suffix_speed
    } else {
        tokens.speed
    };
    codex_cost(rates, tokens, rate_model, speed)
}

fn codex_cost(
    mut rates: ModelRates,
    mut tokens: TokenBreakdown,
    model: &str,
    speed: RequestSpeed,
) -> Option<f64> {
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
    tokens.speed = speed;
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
    };
    use crate::{
        pricing::{
            test_bundled_pricing, ModelPricing, ModelRates, PricingCatalog, PricingSupplement,
            RequestSpeed, TokenBreakdown,
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
        assert_eq!(events[0].speed, RequestSpeed::Fast);
        assert_eq!(events[1].speed, RequestSpeed::Standard);
        let mut pricing = test_bundled_pricing();
        let api = aggregate(events.clone(), now, &pricing);
        pricing.codex_credit_mode = true;
        let credit = aggregate(events, now, &pricing);
        // Cached input is subtracted from input, reasoning is already part of output.
        assert!((api.today.unwrap().estimated_cost_usd.unwrap() - 2.61).abs() < 1e-9);
        assert!((credit.today.unwrap().estimated_cost_usd.unwrap() - 2.61).abs() < 1e-9);
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
        assert_eq!(events[0].speed, RequestSpeed::Fast);
        assert_eq!(events[1].speed, RequestSpeed::Standard);
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
    fn codex_distinct_sessions_revisions_appends_and_accounts_keep_real_usage() {
        fn journal(session: &str, input: u64, minute: u32) -> String {
            format!(
                "{}\n{}",
                serde_json::json!({"type": "session_meta", "payload": {"id": session}}),
                serde_json::json!({
                    "timestamp": format!("2026-07-10T08:{minute:02}:00Z"),
                    "type": "event_msg",
                    "payload": {"type": "token_count", "model": "gpt-5.5", "info": {
                        "last_token_usage": {"input_tokens": input, "output_tokens": 10, "total_tokens": input + 10}
                    }}
                }),
            )
        }
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        let sessions = home.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let first = sessions.join("first.jsonl");
        let second = sessions.join("second.jsonl");
        fs::write(&first, journal("first-session", 100, 0)).unwrap();
        fs::write(&second, journal("second-session", 100, 0)).unwrap();
        let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
        let since = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 12, 0, 0).unwrap();
        let pricing = test_bundled_pricing();
        let total = |account| {
            aggregate(
                scan_codex_events(&storage, std::slice::from_ref(&home), since, Some(account))
                    .unwrap(),
                now,
                &pricing,
            )
            .daily
            .iter()
            .map(|day| day.tokens)
            .sum::<u64>()
        };
        assert_eq!(total("account-a"), 220);
        fs::write(&first, journal("first-session", 130, 0)).unwrap();
        assert_eq!(total("account-a"), 250);
        fs::write(
            &first,
            format!(
                "{}\n{}",
                journal("first-session", 130, 0),
                journal("first-session", 45, 1)
            ),
        )
        .unwrap();
        fs::copy(&first, sessions.join("fork.jsonl")).unwrap();
        assert_eq!(total("account-a"), 305);
        assert_eq!(total("account-b"), 0);
        fs::write(
            &second,
            format!(
                "{}\n{}",
                journal("second-session", 100, 0),
                journal("second-session", 50, 2)
            ),
        )
        .unwrap();
        assert_eq!(total("account-b"), 60);
        assert_eq!(total("account-a"), 305);
        fs::write(&first, "").unwrap();
        fs::remove_file(sessions.join("fork.jsonl")).unwrap();
        fs::remove_file(&second).unwrap();
        assert_eq!(total("account-a"), 305);
        assert_eq!(total("account-b"), 60);
    }

    #[test]
    fn codex_recovers_compatible_deleted_source_cache_without_inventing_history() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
        storage.save_log_events("codex", &home.join("sessions/deleted.jsonl"), 10, 20,
            r#"{"schema_version":4,"events":[{"timestamp":"2026-07-10T08:00:00Z","model":"gpt-5.5","input":100,"cached":0,"output":10,"reasoning":0,"total":110,"is_fast":false}]}"#).unwrap();
        let since = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 12, 0, 0).unwrap();
        let history = aggregate(
            scan_codex_events(&storage, std::slice::from_ref(&home), since, None).unwrap(),
            now,
            &test_bundled_pricing(),
        );
        assert_eq!(history.daily.iter().map(|day| day.tokens).sum::<u64>(), 110);
        let retained = aggregate(
            scan_codex_events(&storage, &[home], since, None).unwrap(),
            now,
            &test_bundled_pricing(),
        );
        assert_eq!(retained, history);
    }

    #[test]
    fn codex_usage_survives_archive_delete_and_restart() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        let active = home.join("sessions/rollout.jsonl");
        let archived = home.join("archived_sessions/renamed.jsonl");
        let database = directory.path().join("usage-test.db");
        fs::create_dir_all(active.parent().unwrap()).unwrap();
        fs::create_dir_all(archived.parent().unwrap()).unwrap();
        fs::write(&active, r#"{"type":"session_meta","payload":{"id":"session-retention"}}
{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#).unwrap();
        let since = NaiveDate::from_ymd_opt(2026, 7, 1).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 12, 0, 0).unwrap();
        let pricing = test_bundled_pricing();
        let storage = Storage::open(&database).unwrap();
        let original = aggregate(
            scan_codex_events(&storage, std::slice::from_ref(&home), since, None).unwrap(),
            now,
            &pricing,
        );
        assert_eq!(
            original.daily.iter().map(|day| day.tokens).sum::<u64>(),
            110
        );
        fs::copy(&active, &archived).unwrap();
        let copied = aggregate(
            scan_codex_events(&storage, std::slice::from_ref(&home), since, None).unwrap(),
            now,
            &pricing,
        );
        assert_eq!(copied, original);
        fs::remove_file(&active).unwrap();
        let moved = aggregate(
            scan_codex_events(&storage, std::slice::from_ref(&home), since, None).unwrap(),
            now,
            &pricing,
        );
        assert_eq!(moved, original);
        fs::remove_file(&archived).unwrap();
        drop(storage);
        let restarted = Storage::open(&database).unwrap();
        let retained = aggregate(
            scan_codex_events(&restarted, &[home], since, None).unwrap(),
            now,
            &pricing,
        );
        assert_eq!(retained, original);
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

        let initial =
            scan_codex_events(&storage, std::slice::from_ref(&home), since, None).unwrap();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].speed, RequestSpeed::Standard);

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

        let refreshed = scan_codex_events(&storage, &[home], since, None).unwrap();
        assert_eq!(refreshed.len(), 2);
        assert_eq!(refreshed.iter().map(|event| event.total).sum::<u64>(), 195);
        assert!(refreshed
            .iter()
            .all(|event| event.speed == RequestSpeed::Standard));
    }

    #[test]
    fn schema_six_recovers_ultrafast_from_source_and_keeps_it_after_deletion() {
        let directory = tempdir().unwrap();
        let home = directory.path().join("codex");
        fs::create_dir_all(home.join("sessions")).unwrap();
        let path = home.join("sessions/ultrafast.jsonl");
        fs::write(&path, r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","service_tier":"ultrafast"}}
{"timestamp":"2026-07-10T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#).unwrap();
        let path = fs::canonicalize(path).unwrap();
        let fingerprint = LogFileFingerprint::from_metadata(&fs::metadata(&path).unwrap()).unwrap();
        let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
        storage.save_log_events("codex", &path, fingerprint.size, fingerprint.modified_nanos,
            r#"{"schema_version":5,"events":[{"timestamp":"2026-07-10T10:00:00Z","model":"gpt-6-astra","input":100,"cached":0,"output":10,"reasoning":0,"total":110,"is_fast":false}]}"#).unwrap();
        let events = scan_codex_events(
            &storage,
            std::slice::from_ref(&home),
            NaiveDate::MIN,
            Some("account"),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].speed, RequestSpeed::Ultrafast);
        fs::remove_file(path).unwrap();
        let retained =
            scan_codex_events(&storage, &[home], NaiveDate::MIN, Some("account")).unwrap();
        assert_eq!(retained, events);
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
            None,
        )
        .unwrap();

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, "codex-auto-review");
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
    fn period_breakdown_retains_unpriced_tokens() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-07-10T08:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.4","info":{"last_token_usage":{"input_tokens":1000,"output_tokens":100,"total_tokens":1100}}}}
{"timestamp":"2026-07-10T09:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.3-codex","info":{"last_token_usage":{"input_tokens":800,"output_tokens":100,"total_tokens":900}}}}
{"timestamp":"2026-07-10T10:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"future-unpriced-model","info":{"last_token_usage":{"input_tokens":400,"output_tokens":100,"total_tokens":500}}}}"#;
        let pricing = test_bundled_pricing();
        let history = aggregate(parse_jsonl(content), now, &pricing);
        let today = history.today.unwrap();
        let breakdown = today.model_breakdown.unwrap();

        assert_eq!(today.tokens, 2_500);
        assert_eq!(today.unknown_models, ["future-unpriced-model"]);
        assert_eq!(
            breakdown
                .models
                .iter()
                .map(|entry| entry.model.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.4", "gpt-5.3-codex", "Other"]
        );
        assert_eq!(breakdown.source_note, "From your Codex logs (estimated)");
    }

    #[test]
    fn unknown_only_usage_retains_tokens_without_inventing_cost() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let content = r#"{"timestamp":"2026-07-10T10:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"future-unpriced-model","info":{"last_token_usage":{"input_tokens":400,"output_tokens":100,"total_tokens":500}}}}"#;
        let pricing = test_bundled_pricing();
        let history = aggregate(parse_jsonl(content), now, &pricing);

        let today = history.today.unwrap();
        assert_eq!(today.tokens, 500);
        assert_eq!(today.estimated_cost_usd, None);
        assert!(!today.estimate_complete);
        assert_eq!(history.last_30_days.unwrap().estimated_cost_usd, None);
        assert_eq!(history.daily[0].tokens, 500);
        assert_eq!(history.daily[0].estimated_cost_usd, None);
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
            source_id: None,
            timestamp: Utc::now(),
            model: "test-model".into(),
            input: 1_000_000,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 1_000_000,
            speed: RequestSpeed::Standard,
        };
        assert_eq!(estimate_cost(&event, &pricing), Some(2.0));
        assert_eq!(
            estimate_cost(
                &TokenEvent {
                    speed: RequestSpeed::Fast,
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
            source_id: None,
            timestamp: Utc::now(),
            model: "test-model".into(),
            input: 1_000_000,
            cached: 1_000_000,
            output: 0,
            reasoning: 0,
            total: 1_000_000,
            speed: RequestSpeed::Standard,
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
            source_id: None,
            timestamp: Utc::now(),
            model: "gpt-5.5-pro".into(),
            input: 100_000,
            cached: 100_000,
            output: 0,
            reasoning: 0,
            total: 100_000,
            speed: RequestSpeed::Standard,
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
            source_id: None,
            timestamp: Utc::now(),
            model: "gpt-5.4".into(),
            input,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: input,
            speed: RequestSpeed::Standard,
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
            source_id: None,
            timestamp: Utc::now(),
            model: "gpt-5.5-fast".into(),
            input: 100_000,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 100_000,
            speed: RequestSpeed::Fast,
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
            source_id: None,
            timestamp: Utc::now(),
            model: "vendor-model-fast".into(),
            input: 100_000,
            cached: 0,
            output: 0,
            reasoning: 0,
            total: 100_000,
            speed: RequestSpeed::Fast,
        };

        assert_eq!(estimate_cost(&event, &pricing), Some(0.6));
    }
}

#[cfg(test)]
mod speed_and_cycle_regressions {
    use super::*;
    use chrono::TimeZone;

    fn old_cycle(started_at: DateTime<Utc>, tokens: u64) -> ResetCycleUsage {
        let mut accumulator = DailyUsageAccumulator::default();
        accumulator.add_variant(
            started_at.with_timezone(&Local).date_naive(),
            tokens,
            999.0,
            "gpt-6-astra",
            "Codex",
        );
        ResetCycleUsage {
            started_at,
            ended_at: Some(started_at + chrono::Duration::days(6)),
            scheduled_reset_at: started_at + chrono::Duration::days(7),
            usage: cycle_period(accumulator, started_at, "old", Some(50.0)).unwrap(),
            model_usage: None,
        }
    }

    #[test]
    fn ultrafast_turn_and_event_override_survive_persistence_and_price_once() {
        let content = r#"{"type":"turn_context","payload":{"model":"gpt-6-astra","service_tier":"ultrafast"}}
{"timestamp":"2026-07-10T10:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}
{"timestamp":"2026-07-10T10:01:00Z","type":"event_msg","payload":{"type":"token_count","service_tier":"default","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#;
        let events = parse_jsonl(content);
        assert_eq!(events[0].speed, RequestSpeed::Ultrafast);
        assert_eq!(events[1].speed, RequestSpeed::Standard);
        let serialized = serde_json::to_value(&events[0]).unwrap();
        assert_eq!(serialized["speed"], "ultrafast");
        assert!(serialized.get("is_fast").is_none());
        assert_eq!(
            serde_json::from_value::<TokenEvent>(serialized.clone()).unwrap(),
            events[0]
        );
        let mut legacy = serialized;
        legacy.as_object_mut().unwrap().remove("speed");
        legacy["is_fast"] = Value::Bool(true);
        assert_eq!(
            serde_json::from_value::<TokenEvent>(legacy).unwrap().speed,
            RequestSpeed::Fast
        );
        let mut pricing = crate::pricing::test_bundled_pricing();
        for credit_mode in [false, true] {
            pricing.codex_credit_mode = credit_mode;
            let standard = estimate_cost(&events[1], &pricing).unwrap();
            let ultra = estimate_cost(&events[0], &pricing).unwrap();
            assert!((ultra - standard * 6.0).abs() < 1e-12);
            let mut alias = events[0].clone();
            alias.model.push_str("-ultrafast");
            assert_eq!(estimate_cost(&alias, &pricing), Some(ultra));
            alias.model = "gpt-5.5".into();
            assert_eq!(estimate_cost(&alias, &pricing), None);
        }
    }

    #[test]
    fn unpriced_tokens_survive_session_weekly_and_history_without_quota_extrapolation() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        let start = now - chrono::Duration::hours(5);
        let unknown = TokenEvent {
            timestamp: now - chrono::Duration::hours(1),
            model: "gpt-5.5".into(),
            input: 100,
            cached: 0,
            output: 10,
            reasoning: 0,
            total: 110,
            speed: RequestSpeed::Ultrafast,
            source_id: Some("unknown".into()),
        };
        for mixed in [false, true] {
            let mut events = vec![unknown.clone()];
            if mixed {
                events.push(TokenEvent {
                    speed: RequestSpeed::Standard,
                    source_id: Some("known".into()),
                    ..unknown.clone()
                });
            }
            let mut daily = DailyUsageAccumulator::default();
            let mut session = DailyUsageAccumulator::default();
            let mut weekly = DailyUsageAccumulator::default();
            let mut buckets = build_cycle_buckets(
                vec![omp_usage::QuotaCycleAnchor {
                    started_at: start,
                    scheduled_reset_at: start + chrono::Duration::days(7),
                    used_percent: 50.0,
                }],
                now,
            );
            aggregate_into(
                &events,
                now,
                &crate::pricing::test_bundled_pricing(),
                &mut daily,
                Some(start),
                &mut session,
                Some(start),
                &mut weekly,
                &mut buckets,
            );
            let historical = build_cycle_history(buckets, now, "test")
                .pop()
                .unwrap()
                .cycle
                .usage;
            for period in [
                daily.build(now, "test").today.unwrap(),
                cycle_period(session, now, "test", Some(50.0)).unwrap(),
                cycle_period(weekly, now, "test", Some(50.0)).unwrap(),
                historical,
            ] {
                assert_eq!(period.tokens, if mixed { 220 } else { 110 });
                assert_eq!(period.estimated_cost_usd.is_some(), mixed);
                assert!(!period.estimate_complete);
                assert_eq!(period.estimated_limit_usd, None);
                assert!(period.unknown_models.is_empty());
            }
        }
    }

    #[test]
    fn deleted_source_retained_facts_reprice_old_cycles_and_exclude_memory() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("usage.db");
        let storage = Storage::open(&database).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let omp_sessions = directory.path().join("omp/agent/sessions");
        fs::create_dir_all(&omp_sessions).unwrap();
        let memory_source = omp_sessions.join("memory.jsonl");
        fs::write(&memory_source, concat!(
            "{\"type\":\"session\",\"id\":\"memory-history\"}\n",
            "{\"type\":\"model_usage\",\"id\":\"known\",\"purpose\":\"memory\",\"timestamp\":\"2026-01-01T02:00:00Z\",\"provider\":\"openai-codex\",\"model\":\"gpt-6.1-sol\",\"serviceTier\":\"default\",\"usage\":{\"input\":279,\"output\":145,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":424}}\n",
            "{\"type\":\"model_usage\",\"id\":\"unknown\",\"purpose\":\"memory\",\"timestamp\":\"2026-01-01T03:00:00Z\",\"provider\":\"openai-codex\",\"model\":\"gpt-6.1-sol\",\"usage\":{\"input\":279,\"output\":145,\"cacheRead\":0,\"cacheWrite\":0,\"totalTokens\":424}}\n",
        )).unwrap();
        let scan_memory = |storage: &Storage, pricing: &ModelPricing| {
            omp_usage::scan_sessions_into(
                storage,
                omp_usage::SessionSource {
                    directory: &omp_sessions,
                    cache_key: "omp",
                    identity_key: "account",
                    label: "Oh My Pi",
                    allowed_models: &[],
                },
                now,
                pricing,
                None,
                None,
                &mut DailyUsageAccumulator::default(),
                &mut DailyUsageAccumulator::default(),
                &mut DailyUsageAccumulator::default(),
            )
            .unwrap()
        };
        let mut pricing = crate::pricing::test_bundled_pricing();
        assert_eq!(scan_memory(&storage, &pricing).events.len(), 2);
        let home = directory.path().join("codex");
        fs::create_dir_all(home.join("sessions")).unwrap();
        let source = home.join("sessions/old.jsonl");
        fs::write(&source, r#"{"timestamp":"2026-01-01T01:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-6-astra","info":{"last_token_usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}}"#).unwrap();
        scan_codex_events(
            &storage,
            std::slice::from_ref(&home),
            NaiveDate::MIN,
            Some("account"),
        )
        .unwrap();
        fs::remove_file(source).unwrap();
        fs::remove_file(memory_source).unwrap();
        drop(storage);
        let storage = Storage::open(&database).unwrap();
        let events = scan_codex_events(&storage, &[home], NaiveDate::MIN, Some("account")).unwrap();
        assert_eq!(events.len(), 1);
        let original = old_cycle(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(), 958);
        for credit_mode in [false, true] {
            pricing.codex_credit_mode = credit_mode;
            let memory = scan_memory(&storage, &pricing);
            assert_eq!(memory.events.len(), 2);
            assert!(memory.events.iter().all(|event| event.cost == Some(0.0)));
            storage
                .save_reset_cycles(
                    "codex",
                    "account",
                    cycle_storage_key(credit_mode),
                    &[StoredResetCycle {
                        cycle: original.clone(),
                        recovery: CycleRecovery {
                            baseline_tokens: original.usage.tokens,
                            covered_until: now,
                            boundary_conflict: false,
                        },
                    }],
                )
                .unwrap();
            let persisted = storage
                .load_reset_cycles("codex", "account", cycle_storage_key(credit_mode))
                .unwrap();
            let mut buckets = restore_cycle_buckets(Vec::new(), &persisted, now);
            let mut daily = DailyUsageAccumulator::default();
            aggregate_into(
                &events,
                now,
                &pricing,
                &mut daily,
                None,
                &mut DailyUsageAccumulator::default(),
                None,
                &mut DailyUsageAccumulator::default(),
                &mut buckets,
            );
            for event in &memory.events {
                add_cycle_event(
                    &mut buckets,
                    event.timestamp,
                    &event.model,
                    event.total,
                    event.cost,
                    "Oh My Pi",
                    false,
                );
            }
            assert!(daily.build(now, "test").daily.is_empty());
            let rebuilt = build_cycle_history(buckets, now, "test");
            let saved = persist_cycles(&storage, Some("account"), rebuilt, credit_mode).unwrap();
            assert_eq!(saved[0].started_at, original.started_at);
            assert_eq!(saved[0].ended_at, original.ended_at);
            assert_eq!(saved[0].scheduled_reset_at, original.scheduled_reset_at);
            assert_eq!(
                saved[0].usage.estimated_cost_usd,
                estimate_cost(&events[0], &pricing)
            );
            assert!(saved[0].usage.estimate_complete);
            assert_eq!(saved[0].usage.tokens, 958);
            let models = saved[0].model_usage.as_ref().unwrap();
            assert_eq!(models.len(), 2);
            assert!(!models.iter().any(|model| model.model == "Unattributed"));
            let memory_model = models
                .iter()
                .find(|model| model.model == "gpt-6.1-sol")
                .unwrap();
            assert_eq!(memory_model.total_tokens, 848);
            assert_eq!(memory_model.request_count, Some(2));
            assert_eq!(memory_model.cost_usd, Some(0.0));
            assert!(memory_model.cost_complete);
            assert_eq!(
                models
                    .iter()
                    .map(|model| model.request_count.unwrap())
                    .sum::<u64>(),
                3
            );
        }
    }

    #[test]
    fn missing_historical_facts_preserve_tokens_and_boundaries_not_stale_amounts() {
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let original = old_cycle(now - chrono::Duration::days(180), 110);
        let stored = StoredResetCycle {
            cycle: original.clone(),
            recovery: CycleRecovery {
                baseline_tokens: original.usage.tokens,
                covered_until: now,
                boundary_conflict: false,
            },
        };
        for recovered in [false, true] {
            let mut buckets = restore_cycle_buckets(Vec::new(), std::slice::from_ref(&stored), now);
            if recovered {
                add_cycle_event(
                    &mut buckets,
                    original.started_at,
                    "gpt-6-astra",
                    50,
                    Some(0.5),
                    "Codex",
                    false,
                );
            }
            let cycles: Vec<_> = build_cycle_history(buckets, now, "test")
                .into_iter()
                .map(|record| record.cycle)
                .collect();
            assert_eq!(cycles.len(), 1);
            assert_eq!(cycles[0].ended_at, original.ended_at);
            assert_eq!(cycles[0].usage.tokens, 110);
            assert_eq!(cycles[0].usage.estimated_cost_usd, recovered.then_some(0.5));
            assert!(!cycles[0].usage.estimate_complete);
            assert_eq!(cycles[0].usage.estimated_limit_usd, None);
            assert!(cycles[0].usage.unknown_models.is_empty());
            assert_eq!(cycles[0].usage.quota_used_percent, Some(50.0));
            let models = cycles[0].model_usage.as_ref().unwrap();
            assert_eq!(
                models.iter().map(|model| model.total_tokens).sum::<u64>(),
                110
            );
            let unattributed = models
                .iter()
                .find(|model| model.model == "Unattributed")
                .unwrap();
            assert_eq!(unattributed.total_tokens, if recovered { 60 } else { 110 });
            assert_eq!(unattributed.request_count, None);
            if recovered {
                assert_eq!(
                    models
                        .iter()
                        .find(|model| model.model == "gpt-6-astra")
                        .unwrap()
                        .request_count,
                    Some(1)
                );
            }
        }
    }
}

#[cfg(test)]
mod model_classification_regressions {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn unsupported_speed_is_not_an_unknown_model_in_api_or_credit_dtos() {
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let mut pricing = crate::pricing::test_bundled_pricing();
        for credit_mode in [false, true] {
            pricing.codex_credit_mode = credit_mode;
            for (model, known) in [
                ("gpt-5.5", true),
                ("gpt-5.5-ultrafast", true),
                ("missing-model", false),
            ] {
                assert_eq!(model_has_rates(&now, model, &pricing), known);
                let history = aggregate(
                    vec![TokenEvent {
                        timestamp: now,
                        model: model.into(),
                        input: 100,
                        cached: 0,
                        output: 10,
                        reasoning: 0,
                        total: 110,
                        speed: RequestSpeed::Ultrafast,
                        source_id: None,
                    }],
                    now,
                    &pricing,
                );
                let dto = serde_json::to_value(history).unwrap();
                assert_eq!(dto["today"]["estimateComplete"], false);
                assert_eq!(dto["last30Days"]["estimateComplete"], false);
                assert_eq!(dto["today"]["tokens"], 110);
                assert!(dto["today"]["estimatedCostUsd"].is_null());
                assert_eq!(
                    dto["today"]["unknownModels"],
                    if known {
                        serde_json::json!([])
                    } else {
                        serde_json::json!([model])
                    }
                );
            }
        }
        pricing.codex_credit_mode = false;
        assert!(model_has_rates(&now, "gpt-5.3-codex", &pricing));
        pricing.codex_credit_mode = true;
        assert!(!model_has_rates(&now, "gpt-5.3-codex", &pricing));
    }
}

#[cfg(test)]
mod persistent_cycle_recovery_regressions {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn day(value: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, value, 0, 0, 0).unwrap()
    }

    fn snapshot(tokens: u64, end: Option<DateTime<Utc>>) -> StoredResetCycle {
        StoredResetCycle {
            cycle: ResetCycleUsage {
                started_at: day(1),
                ended_at: end,
                scheduled_reset_at: day(8),
                usage: UsagePeriod {
                    tokens,
                    estimated_cost_usd: Some(999.0),
                    estimated_limit_usd: Some(1998.0),
                    quota_used_percent: Some(50.0),
                    cost_estimated: true,
                    estimate_complete: true,
                    model_breakdown: None,
                    unknown_models: Vec::new(),
                },
                model_usage: None,
            },
            recovery: CycleRecovery {
                baseline_tokens: tokens,
                covered_until: day(2),
                boundary_conflict: false,
            },
        }
    }

    fn seed_legacy(path: &Path, record: &StoredResetCycle, credit_mode: bool) {
        drop(Storage::open(path).unwrap());
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute(
                "INSERT INTO reset_cycle_usage
             (provider_id, identity_key, window_id, cycle_key, payload, updated_at)
             VALUES ('codex', 'account', ?1, ?2, ?3, ?4)",
                rusqlite::params![
                    cycle_storage_key(credit_mode),
                    record.cycle.started_at.timestamp().div_euclid(300),
                    serde_json::to_string(&record.cycle).unwrap(),
                    record.recovery.covered_until.to_rfc3339(),
                ],
            )
            .unwrap();
    }

    // Every refresh opens a new connection, so these assertions exercise persisted
    // coverage, not a process-local balance or a source-note heuristic.
    fn refresh(
        path: &Path,
        now: DateTime<Utc>,
        events: &[(DateTime<Utc>, u64, Option<f64>)],
        anchors: Vec<omp_usage::QuotaCycleAnchor>,
        credit_mode: bool,
    ) -> (Vec<ResetCycleUsage>, Option<UsagePeriod>) {
        let storage = Storage::open(path).unwrap();
        let persisted = storage
            .load_reset_cycles("codex", "account", cycle_storage_key(credit_mode))
            .unwrap();
        let mut buckets = restore_cycle_buckets(anchors, &persisted, now);
        for &(timestamp, tokens, cost) in events {
            add_cycle_event(
                &mut buckets,
                timestamp,
                "gpt-6-astra",
                tokens,
                cost,
                "Codex",
                false,
            );
        }
        let mut rebuilt = build_cycle_history(buckets, now, "test");
        let weekly = current_weekly_period(&mut rebuilt, Some(day(1)), now, Some(50.0));
        let history = persist_cycles(&storage, Some("account"), rebuilt, credit_mode).unwrap();
        (history, weekly)
    }

    fn unpriced_tokens(period: &UsagePeriod) -> u64 {
        period
            .model_breakdown
            .iter()
            .flat_map(|breakdown| &breakdown.models)
            .filter(|model| model.cost_usd.is_none())
            .map(|model| model.total_tokens)
            .sum()
    }

    #[test]
    fn new_requests_do_not_consume_legacy_coverage_after_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.db");
        let original = snapshot(100, None);
        for credit_mode in [false, true] {
            seed_legacy(&path, &original, credit_mode);
            let mut events = Vec::new();
            for count in 0..=2 {
                if count > 0 {
                    events.push((day(2) + Duration::hours(count), 50, Some(0.5)));
                }
                let (history, weekly) = refresh(&path, day(3), &events, Vec::new(), credit_mode);
                let usage = &history[0].usage;
                assert_eq!(usage.tokens, 100 + count as u64 * 50);
                assert_eq!(unpriced_tokens(usage), 100);
                assert!(!usage.estimate_complete);
                assert_eq!(usage.estimated_limit_usd, None);
                assert_eq!(weekly.as_ref(), Some(usage));
                let models = history[0].model_usage.as_ref().unwrap();
                assert_eq!(
                    models.iter().map(|model| model.total_tokens).sum::<u64>(),
                    usage.tokens
                );
                let unknown = models
                    .iter()
                    .find(|model| model.model == "Unattributed")
                    .unwrap();
                assert_eq!(unknown.total_tokens, 100);
                assert_eq!(unknown.request_count, None);
                assert_eq!(
                    models
                        .iter()
                        .find(|model| model.model == "gpt-6-astra")
                        .map(|model| model.request_count),
                    (count > 0).then_some(Some(count as u64))
                );
                let reopened = Storage::open(&path)
                    .unwrap()
                    .load_reset_cycles("codex", "account", cycle_storage_key(credit_mode))
                    .unwrap();
                assert_eq!(reopened[0].recovery, original.recovery);
                assert_eq!(reopened[0].cycle.model_usage, history[0].model_usage);
            }
        }
    }

    #[test]
    fn recovered_pre_snapshot_facts_clear_only_the_original_balance() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.db");
        for credit_mode in [false, true] {
            seed_legacy(&path, &snapshot(100, None), credit_mode);
        }
        let new_events = [(day(3), 50, Some(0.5)), (day(4), 50, Some(0.5))];
        let (before, _) = refresh(&path, day(5), &new_events, Vec::new(), false);
        assert_eq!(before[0].usage.tokens, 200);
        assert_eq!(unpriced_tokens(&before[0].usage), 100);
        let recovered = [new_events[0], new_events[1], (day(2), 100, Some(1.0))];
        for _ in 0..2 {
            let (after, weekly) = refresh(&path, day(5), &recovered, Vec::new(), false);
            assert_eq!(after[0].usage.tokens, 200);
            assert_eq!(unpriced_tokens(&after[0].usage), 0);
            assert!(after[0].usage.estimate_complete);
            assert_eq!(after[0].usage.estimated_cost_usd, Some(2.0));
            assert_eq!(after[0].usage.estimated_limit_usd, Some(4.0));
            assert_eq!(
                after[0].model_usage,
                Some(vec![CycleModelUsage {
                    model: "gpt-6-astra".into(),
                    total_tokens: 200,
                    request_count: Some(3),
                    cost_usd: Some(2.0),
                    cost_complete: true,
                }])
            );
            assert_eq!(weekly.as_ref(), Some(&after[0].usage));
        }
        let (credits, _) = refresh(&path, day(5), &new_events, Vec::new(), true);
        assert_eq!(unpriced_tokens(&credits[0].usage), 100);
        assert!(!credits[0].usage.estimate_complete);
    }

    #[test]
    fn current_week_and_history_share_recovered_partial_and_quota() {
        let original = snapshot(100, None);
        let mut buckets = restore_cycle_buckets(Vec::new(), &[original], day(3));
        add_cycle_event(
            &mut buckets,
            day(2),
            "gpt-6-astra",
            50,
            Some(0.5),
            "Codex",
            false,
        );
        let mut cycles = build_cycle_history(buckets, day(3), "test");
        // The live server percentage replaces the old percentage in both views.
        let weekly = current_weekly_period(&mut cycles, Some(day(1)), day(3), Some(25.0)).unwrap();
        assert_eq!(weekly, cycles[0].cycle.usage);
        assert_eq!(weekly.tokens, 100);
        assert!(!weekly.estimate_complete);
        assert_eq!(weekly.quota_used_percent, Some(25.0));
        assert_eq!(weekly.estimated_cost_usd, Some(0.5));
        assert_eq!(weekly.estimated_limit_usd, None);
    }

    #[test]
    fn conflicting_middle_anchor_keeps_closed_window_nonoverlapping() {
        let mut original = snapshot(200, Some(day(8)));
        original.recovery.covered_until = day(8);
        let anchors = [day(5), day(8) - Duration::minutes(2), day(8)]
            .map(|start| omp_usage::QuotaCycleAnchor {
                started_at: start,
                scheduled_reset_at: start + Duration::days(7),
                used_percent: 50.0,
            })
            .to_vec();
        let mut buckets = restore_cycle_buckets(anchors, &[original], day(9));
        for (timestamp, tokens) in [(day(3), 100), (day(6), 100), (day(8), 20)] {
            add_cycle_event(
                &mut buckets,
                timestamp,
                "gpt-6-astra",
                tokens,
                Some(1.0),
                "Codex",
                false,
            );
        }
        let cycles = build_cycle_history(buckets, day(9), "test");
        assert_eq!(cycles.len(), 2);
        assert_eq!(cycles[0].cycle.started_at, day(1));
        assert_eq!(cycles[0].cycle.ended_at, Some(day(8)));
        assert_eq!(cycles[0].cycle.usage.tokens, 200);
        assert!(cycles[0].cycle.usage.estimate_complete);
        assert_eq!(cycles[1].cycle.started_at, day(8));
        assert_eq!(cycles[1].cycle.usage.tokens, 20);
        assert_eq!(
            cycles
                .iter()
                .map(|record| record.cycle.usage.tokens)
                .sum::<u64>(),
            220
        );
    }

    #[test]
    fn open_coverage_conflict_stays_partial_but_later_anchor_can_close_it() {
        let mut original = snapshot(100, None);
        original.recovery.covered_until = day(4);
        let anchor = |start| omp_usage::QuotaCycleAnchor {
            started_at: day(start),
            scheduled_reset_at: day(8),
            used_percent: 50.0,
        };
        let mut buckets = restore_cycle_buckets(vec![anchor(3)], &[original], day(4));
        add_cycle_event(
            &mut buckets,
            day(2),
            "gpt-6-astra",
            100,
            Some(1.0),
            "Codex",
            false,
        );
        let mut cycles = build_cycle_history(buckets, day(4), "test");
        let weekly = current_weekly_period(&mut cycles, Some(day(3)), day(4), Some(50.0)).unwrap();
        assert_eq!(cycles.len(), 1);
        assert_eq!(weekly.tokens, 100);
        assert!(!weekly.estimate_complete);
        assert_eq!(weekly.estimated_limit_usd, None);
        let buckets = restore_cycle_buckets(vec![anchor(5)], &cycles, day(6));
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].ended_at, Some(day(5)));
        assert_eq!(buckets[1].started_at, day(5));
    }

    #[test]
    fn recovered_balance_keeps_unattributed_subtotal_but_not_cost_completeness() {
        for observed in [20, 100] {
            for cost in [None, Some(0.0), Some(0.25)] {
                let mut buckets = restore_cycle_buckets(Vec::new(), &[snapshot(100, None)], day(3));
                add_cycle_event(
                    &mut buckets,
                    day(2),
                    "  ",
                    observed,
                    cost,
                    "Oh My Pi",
                    false,
                );
                let cycles = build_cycle_history(buckets, day(3), "test");
                assert_eq!(
                    cycles[0].cycle.model_usage,
                    Some(vec![CycleModelUsage {
                        model: "Unattributed".into(),
                        total_tokens: 100,
                        request_count: (observed == 100).then_some(1),
                        cost_usd: cost,
                        cost_complete: observed == 100 && cost.is_some(),
                    }])
                );
                let dto = serde_json::to_value(&cycles[0].cycle).unwrap();
                assert_eq!(
                    dto["modelUsage"][0]["requestCount"].is_null(),
                    observed < 100
                );
            }
        }
    }

    #[test]
    fn missing_costs_remain_null_partial_and_zero_clean() {
        for (baseline, known_tokens) in [(100, 0), (100, 50), (0, 0)] {
            let mut buckets =
                restore_cycle_buckets(Vec::new(), &[snapshot(baseline, None)], day(3));
            aggregate_into(
                &[TokenEvent {
                    timestamp: day(2),
                    model: "missing-model".into(),
                    input: 0,
                    cached: 0,
                    output: 0,
                    reasoning: 0,
                    total: 0,
                    speed: RequestSpeed::Standard,
                    source_id: None,
                }],
                day(3),
                &crate::pricing::test_bundled_pricing(),
                &mut DailyUsageAccumulator::default(),
                None,
                &mut DailyUsageAccumulator::default(),
                None,
                &mut DailyUsageAccumulator::default(),
                &mut buckets,
            );
            if known_tokens > 0 {
                add_cycle_event(
                    &mut buckets,
                    day(2),
                    "gpt-6-astra",
                    known_tokens,
                    Some(0.5),
                    "Codex",
                    false,
                );
            }
            let cycles = build_cycle_history(buckets, day(3), "test");
            if baseline == 0 {
                assert_eq!(cycles[0].cycle.model_usage, None);
            } else {
                assert!(!cycles[0]
                    .cycle
                    .model_usage
                    .as_ref()
                    .unwrap()
                    .iter()
                    .any(|model| { model.model == "missing-model" }));
            }
            let usage = &cycles[0].cycle.usage;
            assert_eq!(usage.tokens, baseline);
            assert_eq!(usage.estimate_complete, baseline == 0);
            assert!(usage.unknown_models.is_empty());
            assert_eq!(usage.estimated_limit_usd, None);
            assert_eq!(
                usage.estimated_cost_usd,
                if baseline == 0 {
                    Some(0.0)
                } else {
                    (known_tokens > 0).then_some(0.5)
                }
            );
            let dto = serde_json::to_value(usage).unwrap();
            assert_eq!(
                dto["estimatedCostUsd"].is_null(),
                baseline > 0 && known_tokens == 0
            );
        }
    }
}

#[cfg(test)]
mod cycle_model_usage_regressions {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn day(value: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 7, value, 0, 0, 0).unwrap()
    }

    fn anchors() -> Vec<omp_usage::QuotaCycleAnchor> {
        [day(1), day(8)]
            .map(|started_at| omp_usage::QuotaCycleAnchor {
                started_at,
                scheduled_reset_at: started_at + Duration::days(7),
                used_percent: 50.0,
            })
            .to_vec()
    }

    #[test]
    fn codex_cycle_models_are_complete_deduplicated_and_price_independent() {
        let event = |timestamp, model: &str, total, source: &str| TokenEvent {
            timestamp,
            model: model.into(),
            input: total,
            cached: 0,
            output: 0,
            reasoning: 0,
            total,
            speed: RequestSpeed::Standard,
            source_id: Some(source.into()),
        };
        let mut events = vec![
            event(day(1), " GPT-6-ASTRA ", 100, "first"),
            event(day(2), "gpt-6-astra", 0, "second"),
            event(day(2), "gpt-6-astra", 50, "second"),
            event(
                day(1) - Duration::seconds(1),
                "outside-before",
                500,
                "before",
            ),
            event(day(8), "next-cycle", 20, "next"),
            event(
                day(9) + Duration::seconds(1),
                "outside-future",
                500,
                "future",
            ),
            event(day(2), "missing-model", 0, "empty-unknown"),
        ];
        // Enough small models to be folded by the ordinary cost breakdown.
        for index in 0..8 {
            let name = format!("small-model-{index}");
            events.push(event(day(2), &name, index + 1, &name));
        }
        let mut unpriced = event(day(3), "gpt-5.5", 10, "unsupported-speed");
        unpriced.speed = RequestSpeed::Ultrafast;
        events.push(unpriced);
        let mut zero_total = event(day(4), "gpt-6-astra", 0, "zero-total-with-usage");
        zero_total.input = 9;
        events.push(zero_total);
        events.extend(events.clone());
        let mut pricing = crate::pricing::test_bundled_pricing();
        let mut api_models = None;
        for credit_mode in [false, true] {
            pricing.codex_credit_mode = credit_mode;
            let mut buckets = build_cycle_buckets(anchors(), day(9));
            aggregate_into(
                &events,
                day(9),
                &pricing,
                &mut DailyUsageAccumulator::default(),
                None,
                &mut DailyUsageAccumulator::default(),
                None,
                &mut DailyUsageAccumulator::default(),
                &mut buckets,
            );
            let cycles = build_cycle_history(buckets, day(9), "test");
            assert_eq!(cycles.len(), 2);
            let cycle = &cycles[0].cycle;
            assert_eq!(cycle.usage.tokens, 196);
            let models = cycle.model_usage.as_ref().unwrap();
            assert_eq!(models.len(), 10);
            assert_eq!(
                models.iter().map(|model| model.total_tokens).sum::<u64>(),
                cycle.usage.tokens
            );
            assert_eq!(
                models
                    .iter()
                    .filter_map(|model| model.request_count)
                    .sum::<u64>(),
                12
            );
            assert_eq!(models[0].model, "GPT-6-ASTRA");
            assert_eq!(models[0].total_tokens, 150);
            assert_eq!(models[0].request_count, Some(3));
            let expected_cost: f64 = events[..events.len() / 2]
                .iter()
                .filter(|event| event.model.trim().eq_ignore_ascii_case("gpt-6-astra"))
                .filter_map(|event| estimate_cost(event, &pricing))
                .sum();
            assert_eq!(models[0].cost_usd, Some(expected_cost));
            // Case-folded attribution can combine an unrecognized price identity
            // with priced events; keep the subtotal without claiming completeness.
            assert!(!models[0].cost_complete);
            assert!(models
                .iter()
                .filter(|model| model.model != "GPT-6-ASTRA")
                .all(|model| model.cost_usd.is_none() && !model.cost_complete));
            assert!(models
                .windows(2)
                .all(|pair| pair[0].total_tokens >= pair[1].total_tokens));
            assert!(!models.iter().any(|model| model.model == "Other"));
            assert_eq!(
                cycles[1].cycle.model_usage,
                Some(vec![CycleModelUsage {
                    model: "next-cycle".into(),
                    total_tokens: 20,
                    request_count: Some(1),
                    cost_usd: None,
                    cost_complete: false,
                }])
            );
            let dto = serde_json::to_value(cycle).unwrap();
            assert_eq!(dto["modelUsage"][0]["requestCount"], 3);
            let counts: Vec<_> = models
                .iter()
                .map(|model| (model.model.clone(), model.total_tokens, model.request_count))
                .collect();
            if credit_mode {
                assert_eq!(Some(counts), api_models);
            } else {
                api_models = Some(counts);
            }
        }
    }

    #[test]
    fn cycle_model_subtotals_zero_and_unknown_costs_survive_each_basis_storage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.db");
        let storage = Storage::open(&path).unwrap();
        for (credit_mode, cost) in [(false, 4.0), (true, 0.5)] {
            let mut buckets = build_cycle_buckets(anchors(), day(7));
            for (model, tokens, cost) in [
                ("priced-and-missing", 80, Some(cost)),
                ("PRICED-AND-MISSING", 20, None),
                ("priced-and-missing", 10, Some(cost / 2.0)),
                ("unknown", 10, None),
                ("free", 10, Some(0.0)),
            ] {
                add_cycle_event(&mut buckets, day(2), model, tokens, cost, "Codex", false);
            }
            let saved = persist_cycles(
                &storage,
                Some("account"),
                build_cycle_history(buckets, day(7), "test"),
                credit_mode,
            )
            .unwrap();
            let models = saved[0].model_usage.as_ref().unwrap();
            let mixed = models
                .iter()
                .find(|model| model.model == "priced-and-missing")
                .unwrap();
            assert_eq!(mixed.total_tokens, 110);
            assert_eq!(mixed.request_count, Some(3));
            assert_eq!(mixed.cost_usd, Some(cost * 1.5));
            assert!(!mixed.cost_complete);
            let unknown = models
                .iter()
                .find(|model| model.model == "unknown")
                .unwrap();
            assert_eq!(unknown.cost_usd, None);
            assert!(!unknown.cost_complete);
            let free = models.iter().find(|model| model.model == "free").unwrap();
            assert_eq!(free.cost_usd, Some(0.0));
            assert!(free.cost_complete);
            assert_eq!(saved[0].usage.estimated_cost_usd, Some(cost * 1.5));
            assert!(!saved[0].usage.estimate_complete);
            let reopened = Storage::open(&path)
                .unwrap()
                .load_reset_cycles("codex", "account", cycle_storage_key(credit_mode))
                .unwrap();
            assert_eq!(reopened[0].cycle, saved[0]);
        }
        let reopened = Storage::open(&path).unwrap();
        let read_cost = |credit_mode| {
            reopened
                .load_reset_cycles("codex", "account", cycle_storage_key(credit_mode))
                .unwrap()[0]
                .cycle
                .model_usage
                .as_ref()
                .unwrap()[0]
                .cost_usd
        };
        assert_eq!(read_cost(false), Some(6.0));
        assert_eq!(read_cost(true), Some(0.75));
    }

    #[test]
    fn omp_cycle_counts_real_assistant_and_auxiliary_calls_once_after_retention() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let content = [
            serde_json::json!({"type":"session","id":"session"}),
            serde_json::json!({"type":"message","id":"main","timestamp":"2026-07-02T00:00:00Z",
                "message":{"role":"assistant","provider":"openai-codex","model":"gpt-6-astra",
                    "responseId":"response-main","serviceTier":"default",
                    "usage":{"input":100,"output":10,"totalTokens":110}}}),
            serde_json::json!({"type":"model_usage","id":"title","parentId":"main",
                "timestamp":"2026-07-02T00:01:00Z","purpose":"title","provider":"openai-codex",
                "model":"gpt-5-mini","serviceTier":"default","usage":{"input":3,"output":4,"totalTokens":7}}),
            serde_json::json!({"type":"model_usage","id":"memory","parentId":"main",
                "timestamp":"2026-07-02T00:02:00Z","purpose":"memory","provider":"openai-codex",
                "model":"gpt-5-mini","serviceTier":"default","usage":{"input":6,"output":7,"totalTokens":13}}),
            serde_json::json!({"type":"message","id":"empty","timestamp":"2026-07-02T00:03:00Z",
                "message":{"role":"assistant","provider":"openai-codex","model":"gpt-6-astra",
                    "usage":{"input":0,"output":0,"totalTokens":0}}}),
            serde_json::json!({"type":"message","id":"missing","timestamp":"2026-07-02T00:04:00Z",
                "message":{"role":"assistant","provider":"openai-codex","model":"gpt-6-astra"}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        fs::write(sessions.join("main.jsonl"), &content).unwrap();
        fs::write(sessions.join("fork.jsonl"), &content).unwrap();
        let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
        let mut pricing = crate::pricing::test_bundled_pricing();
        let mut api_models = None;
        for credit_mode in [false, true] {
            pricing.codex_credit_mode = credit_mode;
            let outcome = omp_usage::scan_sessions_into(
                &storage,
                omp_usage::SessionSource {
                    directory: &sessions,
                    cache_key: "omp",
                    identity_key: "account",
                    label: "Oh My Pi",
                    allowed_models: &[],
                },
                day(9),
                &pricing,
                None,
                None,
                &mut DailyUsageAccumulator::default(),
                &mut DailyUsageAccumulator::default(),
                &mut DailyUsageAccumulator::default(),
            )
            .unwrap();
            assert_eq!(outcome.events.len(), 3);
            let main_cost = outcome
                .events
                .iter()
                .find(|event| event.model == "gpt-6-astra")
                .unwrap()
                .cost;
            let mini_costs: Vec<_> = outcome
                .events
                .iter()
                .filter(|event| event.model == "gpt-5-mini")
                .map(|event| event.cost)
                .collect();
            let mini_cost = mini_costs
                .iter()
                .flatten()
                .copied()
                .reduce(|sum, cost| sum + cost);
            assert!(main_cost.is_some());
            assert!(mini_cost.is_some());
            assert_eq!(mini_costs.iter().all(Option::is_some), !credit_mode);
            if credit_mode {
                assert_eq!(mini_cost, Some(0.0));
            }
            let mut buckets = build_cycle_buckets(anchors(), day(9));
            for event in outcome.events {
                add_cycle_event(
                    &mut buckets,
                    event.timestamp,
                    &event.model,
                    event.total,
                    event.cost,
                    "Oh My Pi",
                    false,
                );
            }
            let cycles = build_cycle_history(buckets, day(9), "test");
            let cycle = &cycles[0].cycle;
            assert_eq!(cycle.usage.tokens, 130);
            assert_eq!(
                cycle.model_usage,
                Some(vec![
                    CycleModelUsage {
                        model: "gpt-6-astra".into(),
                        total_tokens: 110,
                        request_count: Some(1),
                        cost_usd: main_cost,
                        cost_complete: true,
                    },
                    CycleModelUsage {
                        model: "gpt-5-mini".into(),
                        total_tokens: 20,
                        request_count: Some(2),
                        cost_usd: mini_cost,
                        cost_complete: mini_costs.iter().all(Option::is_some),
                    },
                ])
            );
            let counts: Vec<_> = cycle
                .model_usage
                .as_ref()
                .unwrap()
                .iter()
                .map(|model| (model.model.clone(), model.total_tokens, model.request_count))
                .collect();
            if credit_mode {
                assert_eq!(Some(counts), api_models);
            } else {
                api_models = Some(counts);
            }
        }
    }
}
