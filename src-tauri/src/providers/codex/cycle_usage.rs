//! Pairs complete local thread lifetimes with the service's thread consumption.
//! Account quotas supply boundaries only; they are never the denominator here.
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::BufRead;
use std::path::PathBuf;

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

use crate::providers::{
    daily_usage::DailyUsageAccumulator,
    log_usage::{load_or_parse_log, LogCacheError},
};
use crate::{
    models::UsagePeriod,
    pricing::{ModelPricing, TokenBreakdown},
    storage::Storage,
};

use super::local_usage::estimate_token_cost;

pub const MATCHED_SOURCE: &str = "Codex + Oh My Pi · matched token and quota usage";
pub const MAX_QUERY_THREADS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CycleTokenEvent {
    pub key: String,
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub total: u64,
    pub input: u64,
    pub cached: u64,
    pub cache_write: u64,
    pub cache_write_1h: u64,
    pub output: u64,
    pub is_fast: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadSample {
    pub thread_id: String,
    pub created_at: DateTime<Utc>,
    /// Latest timestamp in the complete local journal, including completion markers.
    pub observed_through: DateTime<Utc>,
    pub source: String,
    pub events: Vec<CycleTokenEvent>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadQuery {
    pub thread_id: String,
    pub created_at: DateTime<Utc>,
    pub descendant_thread_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadUsageResponse {
    pub data_as_of: Option<DateTime<Utc>>,
    pub threads: Vec<ThreadUsage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadUsage {
    pub thread_id: String,
    pub data_status: String,
    pub usage_source: String,
    pub five_hour_limit_percent: Option<f64>,
    pub weekly_limit_percent: Option<f64>,
    pub balance_usage_credits: Option<String>,
    pub groups: Vec<ThreadUsageGroup>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreadUsageGroup {
    pub model: String,
    pub speed: String,
    pub five_hour_limit_percent: Option<f64>,
    pub weekly_limit_percent: Option<f64>,
    pub balance_usage_credits: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ObservedThreadUsage {
    pub data_as_of: DateTime<Utc>,
    pub usage: ThreadUsage,
}

/// Metadata is cached separately from disposable transcript parsing and from the
/// daily ledger. A deleted, damaged or truncated journal cannot supply a new sample.
/// Membership and exact token facts in the account-owned ledger establish provenance.
pub fn capture_samples(
    storage: &Storage,
    cache_key: &str,
    identity_key: &str,
    paths: &[PathBuf],
    parse: impl Fn(&str) -> Vec<ThreadSample>,
    owned: &HashMap<String, CycleTokenEvent>,
    earliest: Option<DateTime<Utc>>,
) -> Result<Vec<ThreadSample>, LogCacheError> {
    let Some(earliest) = earliest else {
        return Ok(Vec::new());
    };
    let mut previous: HashMap<String, ThreadSample> = storage
        .load_usage_events::<ThreadSample>(cache_key, identity_key, 1)?
        .into_iter()
        .map(|sample| (sample.thread_id.clone(), sample))
        .collect();
    let mut samples = Vec::new();
    let mut conflicting_threads = HashSet::new();
    let mut request_owners: HashMap<String, String> = previous
        .values()
        .flat_map(|sample| {
            sample
                .events
                .iter()
                .map(|event| (event.key.clone(), sample.thread_id.clone()))
        })
        .collect();
    for path in paths {
        // Inspect only the leading metadata before opening a full old transcript.
        // The daily ledger retains its original scan independently.
        if journal_created_at(path).is_none_or(|created| created < earliest) {
            continue;
        }
        if let Some(parsed) = load_or_parse_log(storage, cache_key, path, 1, &parse)? {
            for sample in parsed {
                if sample.events.is_empty()
                    || !sample
                        .events
                        .iter()
                        .all(|event| owned.get(&event.key) == Some(event))
                {
                    continue;
                }
                let overlaps: Vec<_> = sample
                    .events
                    .iter()
                    .filter_map(|event| {
                        request_owners
                            .get(&event.key)
                            .filter(|owner| *owner != &sample.thread_id)
                    })
                    .cloned()
                    .collect();
                if !overlaps.is_empty() {
                    conflicting_threads.extend(overlaps);
                    conflicting_threads.insert(sample.thread_id.clone());
                    continue;
                }
                if let Some(prior) = previous.get(&sample.thread_id) {
                    // An older complete prefix does not erase calls or coverage
                    // already observed before a journal was truncated.
                    let present: HashMap<_, _> = sample
                        .events
                        .iter()
                        .map(|event| (&event.key, event))
                        .collect();
                    if prior.created_at != sample.created_at
                        || prior.observed_through > sample.observed_through
                        || prior
                            .events
                            .iter()
                            .any(|event| present.get(&event.key) != Some(&event))
                    {
                        continue;
                    }
                }
                storage.record_usage_events(
                    cache_key,
                    identity_key,
                    1,
                    std::slice::from_ref(&sample),
                    |sample| Ok(sample.thread_id.clone()),
                )?;
                previous.insert(sample.thread_id.clone(), sample.clone());
                for event in &sample.events {
                    request_owners.insert(event.key.clone(), sample.thread_id.clone());
                }
                samples.push(sample);
            }
        }
    }
    storage.prune_log_events(cache_key, &paths.iter().cloned().collect())?;
    let retained: HashMap<String, ThreadSample> = storage
        .load_usage_events::<ThreadSample>(cache_key, identity_key, 1)?
        .into_iter()
        .map(|sample| (sample.thread_id.clone(), sample))
        .collect();
    samples.retain(|sample| {
        !conflicting_threads.contains(&sample.thread_id)
            && retained.get(&sample.thread_id) == Some(sample)
    });
    Ok(samples)
}

pub fn journal_created_at(path: &std::path::Path) -> Option<DateTime<Utc>> {
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file).lines().take(16) {
        let line = line.ok()?;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(&line).ok()?;
        let body = match value.get("type").and_then(serde_json::Value::as_str) {
            Some("session_meta") => value.get("payload")?,
            Some("session") => &value,
            _ => continue,
        };
        return body
            .get("timestamp")
            .or_else(|| value.get("timestamp"))
            .and_then(serde_json::Value::as_str)
            .and_then(crate::providers::log_usage::parse_log_timestamp);
    }
    None
}

/// Copies/archives with the same lifetime are one sample. Conflicting revisions or
/// overlapping request identities across different threads are excluded, not guessed.
pub fn unique_samples(samples: Vec<ThreadSample>, now: DateTime<Utc>) -> Vec<ThreadSample> {
    let mut threads: HashMap<String, ThreadSample> = HashMap::new();
    let mut invalid = HashSet::new();
    for mut sample in samples {
        sample
            .events
            .sort_by_key(|event| (event.timestamp, event.key.clone()));
        let keys: HashSet<_> = sample.events.iter().map(|event| &event.key).collect();
        if sample.created_at > sample.observed_through
            || sample.observed_through > now
            || keys.len() != sample.events.len()
            || sample.events.iter().any(|event| {
                event.timestamp < sample.created_at || event.timestamp > sample.observed_through
            })
        {
            continue;
        }
        if let Some(previous) = threads.get_mut(&sample.thread_id) {
            if previous.created_at != sample.created_at || previous.source != sample.source {
                invalid.insert(sample.thread_id.clone());
            } else if previous.events == sample.events {
                previous.observed_through = previous.observed_through.max(sample.observed_through);
            } else if sample.events.starts_with(&previous.events) {
                *previous = sample;
            } else if !previous.events.starts_with(&sample.events) {
                invalid.insert(sample.thread_id.clone());
            }
        } else {
            threads.insert(sample.thread_id.clone(), sample);
        }
    }
    let mut owners = HashMap::new();
    for sample in threads.values() {
        for event in &sample.events {
            if let Some(other) = owners.insert(&event.key, &sample.thread_id) {
                if other != &sample.thread_id {
                    invalid.insert(other.clone());
                    invalid.insert(sample.thread_id.clone());
                }
            }
        }
    }
    let mut result: Vec<_> = threads
        .into_values()
        .filter(|sample| !invalid.contains(&sample.thread_id))
        .collect();
    result.sort_by_key(|sample| std::cmp::Reverse(sample.created_at));
    result
}

fn included_balance(balance: Option<&str>) -> bool {
    balance.is_none_or(|value| {
        value
            .parse::<f64>()
            .is_ok_and(|value| value.is_finite() && value == 0.0)
    })
}

fn percent(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value >= 0.0)
}

fn group_identity(model: &str, speed: &str, pricing: &ModelPricing) -> Option<(String, bool)> {
    let fast = match speed {
        "standard" => false,
        "fast" => true,
        _ => return None,
    };
    Some((pricing.display_family(model), fast))
}

fn matched_percent(
    sample: &ThreadSample,
    row: &ThreadUsage,
    weekly: bool,
    pricing: &ModelPricing,
) -> Option<f64> {
    if row.data_status != "available"
        || row.usage_source != "included_plan"
        || !included_balance(row.balance_usage_credits.as_deref())
        || row.groups.is_empty()
    {
        return None;
    }
    let reported = percent(if weekly {
        row.weekly_limit_percent
    } else {
        row.five_hour_limit_percent
    })?;
    if reported <= 0.0 {
        return None;
    }
    let local: BTreeSet<_> = sample
        .events
        .iter()
        .map(|event| {
            (
                pricing.display_family(&event.model),
                event.is_fast
                    || pricing
                        .supplement
                        .canonical_name(&event.model)
                        .unwrap_or(&event.model)
                        .ends_with("-fast"),
            )
        })
        .collect();
    let mut remote = BTreeSet::new();
    let mut sum = 0.0;
    for group in &row.groups {
        if !included_balance(group.balance_usage_credits.as_deref()) {
            return None;
        }
        let value = percent(if weekly {
            group.weekly_limit_percent
        } else {
            group.five_hour_limit_percent
        })?;
        if value > 0.0 {
            remote.insert(group_identity(&group.model, &group.speed, pricing)?);
        }
        sum += value;
    }
    if remote != local || (sum - reported).abs() > 1e-6 * reported.max(1.0) {
        return None;
    }
    Some(reported)
}

/// Lifetime percentages are usable only when the complete lifetime and service
/// watermark fit this exact reset window. Cross-window threads are not prorated.
pub fn matched_period(
    samples: &[ThreadSample],
    observations: &HashMap<String, ObservedThreadUsage>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    weekly: bool,
    pricing: &ModelPricing,
) -> Option<UsagePeriod> {
    let mut accumulator = DailyUsageAccumulator::default();
    let mut total_percent = 0.0;
    let cutoff = observations
        .values()
        .map(|observed| observed.data_as_of)
        .max()?;
    for sample in samples {
        let Some(observed) = observations.get(&sample.thread_id) else {
            continue;
        };
        if observed.data_as_of != cutoff
            || observed.usage.thread_id != sample.thread_id
            || sample.created_at < started_at
            || observed.data_as_of < sample.observed_through
            || observed.data_as_of > now
            || ended_at
                .is_some_and(|end| observed.data_as_of >= end || sample.observed_through >= end)
        {
            continue;
        }
        let Some(share) = matched_percent(sample, &observed.usage, weekly, pricing) else {
            continue;
        };
        // One unpriced call excludes its whole thread; never pair a partial cost
        // numerator with that thread's full consumption denominator.
        let costs: Option<Vec<_>> = sample
            .events
            .iter()
            .map(|event| {
                estimate_token_cost(
                    &event.timestamp,
                    &event.model,
                    TokenBreakdown {
                        input: event.input,
                        cache_read: event.cached,
                        cache_write_5m: event.cache_write.saturating_sub(event.cache_write_1h),
                        cache_write_1h: event.cache_write_1h,
                        output: event.output,
                        is_fast: event.is_fast,
                    },
                    pricing,
                )
                .filter(|cost| cost.is_finite() && *cost >= 0.0)
            })
            .collect();
        let Some(costs) = costs else {
            continue;
        };
        for (event, cost) in sample.events.iter().zip(costs) {
            accumulator.add_variant(
                event.timestamp.with_timezone(&Local).date_naive(),
                event.total,
                cost,
                &event.model,
                &sample.source,
            );
        }
        total_percent += share;
    }
    if !total_percent.is_finite() || total_percent <= 0.0 {
        return None;
    }
    let basis = if pricing.codex_credit_mode {
        "purchased/PAYG credit estimate"
    } else {
        "API list price estimate"
    };
    let note = format!("{MATCHED_SOURCE} · {basis} · locally recorded sessions as of {} · excludes unmatched, cross-cycle and non-included usage", cutoff.to_rfc3339());
    let mut period = accumulator.build(now, &note).last_30_days?;
    period.quota_used_percent = Some(total_percent);
    period.estimated_limit_usd = period
        .estimated_cost_usd
        .filter(|cost| *cost > 0.0)
        .map(|cost| cost / (total_percent / 100.0));
    Some(period)
}

/// Supplementary-scan fallback must preserve daily totals without reviving the
/// previous account-wide-denominator formula or stale matched samples.
pub fn clear_cycles(usage: &mut crate::models::UsageHistory) {
    usage.session_cycle = None;
    usage.weekly_cycle = None;
    usage.weekly_cycles.clear();
    if let Some(credits) = usage.credit_usage.as_mut() {
        clear_cycles(credits);
    }
}

pub(crate) fn discard_legacy_cycles(usage: &mut crate::models::UsageHistory) {
    fn matched(period: &UsagePeriod) -> bool {
        period
            .model_breakdown
            .as_ref()
            .is_some_and(|detail| detail.source_note.starts_with(MATCHED_SOURCE))
    }
    if usage
        .session_cycle
        .as_ref()
        .is_some_and(|period| !matched(period))
    {
        usage.session_cycle = None;
    }
    if usage
        .weekly_cycle
        .as_ref()
        .is_some_and(|period| !matched(period))
    {
        usage.weekly_cycle = None;
    }
    usage.weekly_cycles.retain(|cycle| matched(&cycle.usage));
    if let Some(credits) = usage.credit_usage.as_mut() {
        discard_legacy_cycles(credits);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::test_bundled_pricing;
    use chrono::{Duration, TimeZone};

    fn instant(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 7, hour, 0, 0).unwrap()
    }

    fn sample(id: &str, source: &str) -> ThreadSample {
        ThreadSample {
            thread_id: id.into(),
            created_at: instant(8),
            observed_through: instant(10),
            source: source.into(),
            events: vec![CycleTokenEvent {
                key: format!("request-{id}"),
                timestamp: instant(9),
                model: "gpt-6.1-sol".into(),
                total: 1_100_000,
                input: 1_000_000,
                cached: 0,
                cache_write: 0,
                cache_write_1h: 0,
                output: 100_000,
                is_fast: false,
            }],
        }
    }

    fn observed(id: &str) -> ObservedThreadUsage {
        ObservedThreadUsage {
            data_as_of: instant(11),
            usage: ThreadUsage {
                thread_id: id.into(),
                data_status: "available".into(),
                usage_source: "included_plan".into(),
                five_hour_limit_percent: Some(10.0),
                weekly_limit_percent: Some(2.0),
                balance_usage_credits: None,
                groups: vec![ThreadUsageGroup {
                    model: "gpt-6.1-sol".into(),
                    speed: "standard".into(),
                    five_hour_limit_percent: Some(10.0),
                    weekly_limit_percent: Some(2.0),
                    balance_usage_credits: None,
                }],
            },
        }
    }

    fn period(
        samples: &[ThreadSample],
        observations: &HashMap<String, ObservedThreadUsage>,
    ) -> Option<UsagePeriod> {
        matched_period(
            samples,
            observations,
            instant(7),
            None,
            instant(12),
            false,
            &test_bundled_pricing(),
        )
    }

    #[test]
    fn costs_tokens_and_percent_use_exactly_the_same_local_threads() {
        let samples = [
            sample("codex", "Codex"),
            sample("omp", "Oh My Pi"),
            sample("unmatched", "Codex"),
        ];
        let observations = HashMap::from([
            ("codex".into(), observed("codex")),
            ("omp".into(), observed("omp")),
        ]);
        let result = period(&samples, &observations).unwrap();
        assert_eq!(result.tokens, 2_200_000);
        assert_eq!(result.estimated_cost_usd, Some(11.0));
        assert_eq!(result.quota_used_percent, Some(20.0));
        assert_eq!(result.estimated_limit_usd, Some(55.0));
        let mut credits = test_bundled_pricing();
        credits.codex_credit_mode = true;
        let credit_result = matched_period(
            &samples,
            &observations,
            instant(7),
            None,
            instant(12),
            false,
            &credits,
        )
        .unwrap();
        assert_eq!(credit_result.estimated_cost_usd, Some(6.0));
        assert_eq!(credit_result.estimated_limit_usd, Some(30.0));
        assert!(result
            .model_breakdown
            .unwrap()
            .source_note
            .starts_with(MATCHED_SOURCE));
    }

    #[test]
    fn lifetime_is_not_prorated_across_windows_or_service_cutoffs() {
        let mut samples = [sample("one", "Codex")];
        let mut observations = HashMap::from([("one".into(), observed("one"))]);
        samples[0].created_at = instant(6);
        assert!(period(&samples, &observations).is_none());
        samples[0].created_at = instant(8);
        observations.get_mut("one").unwrap().data_as_of = instant(9);
        assert!(period(&samples, &observations).is_none());
        observations.get_mut("one").unwrap().data_as_of = instant(13);
        assert!(period(&samples, &observations).is_none());
        observations.get_mut("one").unwrap().data_as_of = instant(11);
        assert!(matched_period(
            &samples,
            &observations,
            instant(7),
            Some(instant(11)),
            instant(12),
            false,
            &test_bundled_pricing()
        )
        .is_none());
    }

    #[test]
    fn null_unavailable_nonincluded_paid_and_uncovered_groups_are_not_zero() {
        let samples = [sample("one", "Codex")];
        let initial = observed("one");
        let rejected: [fn(&mut ObservedThreadUsage); 9] = [
            |row| row.usage.five_hour_limit_percent = None,
            |row| row.usage.data_status = "unavailable".into(),
            |row| row.usage.usage_source = "unknown".into(),
            |row| row.usage.balance_usage_credits = Some("0.01".into()),
            |row| row.usage.groups[0].balance_usage_credits = Some("NaN".into()),
            |row| row.usage.groups[0].five_hour_limit_percent = None,
            |row| row.usage.groups[0].five_hour_limit_percent = Some(5.0),
            |row| row.usage.groups[0].model = "gpt-6-astra".into(),
            |row| row.usage.groups[0].speed = "fast".into(),
        ];
        for mutation in rejected {
            let mut row = initial.clone();
            mutation(&mut row);
            assert!(period(&samples, &HashMap::from([("one".into(), row)])).is_none());
        }
    }

    #[test]
    fn an_unknown_price_excludes_the_whole_lifetime() {
        let mut samples = [sample("one", "Codex")];
        samples[0].events.push(CycleTokenEvent {
            key: "unknown".into(),
            model: "unpublished-model".into(),
            ..samples[0].events[0].clone()
        });
        let mut row = observed("one");
        row.usage.groups[0].five_hour_limit_percent = Some(5.0);
        row.usage.groups.push(ThreadUsageGroup {
            model: "unpublished-model".into(),
            speed: "standard".into(),
            five_hour_limit_percent: Some(5.0),
            weekly_limit_percent: Some(1.0),
            balance_usage_credits: None,
        });
        assert!(period(&samples, &HashMap::from([("one".into(), row)])).is_none());
    }

    #[test]
    fn different_service_watermarks_are_not_combined() {
        let samples = [sample("one", "Codex"), sample("two", "Oh My Pi")];
        let mut older = observed("one");
        older.data_as_of -= Duration::minutes(1);
        let result = period(
            &samples,
            &HashMap::from([("one".into(), older), ("two".into(), observed("two"))]),
        )
        .unwrap();
        assert_eq!(result.tokens, 1_100_000);
        assert_eq!(result.quota_used_percent, Some(10.0));
    }

    #[test]
    fn explicitly_priced_fast_model_alias_matches_the_service_fast_group() {
        let mut local = sample("one", "Codex");
        local.events[0].model = "gpt-6.1-sol-fast".into();
        let mut row = observed("one");
        row.usage.groups[0].speed = "fast".into();
        assert!(period(&[local], &HashMap::from([("one".into(), row)])).is_some());
    }

    #[test]
    fn duplicate_archives_and_inherited_request_ids_do_not_inflate_samples() {
        let one = sample("one", "Codex");
        assert_eq!(
            unique_samples(vec![one.clone(), one.clone()], instant(12)).len(),
            1
        );
        let mut inherited = sample("child", "Oh My Pi");
        inherited.events[0].key = one.events[0].key.clone();
        assert!(unique_samples(vec![one.clone(), inherited], instant(12)).is_empty());
        let mut revision = one.clone();
        revision.events[0].input += 1;
        assert!(unique_samples(vec![one, revision], instant(12)).is_empty());
    }

    #[test]
    fn sample_coverage_cannot_regress_change_accounts_or_survive_missing_journals() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("test.db")).unwrap();
        let path = directory.path().join("sample.jsonl");
        let mut full = sample("one", "Codex");
        let mut second = full.events[0].clone();
        second.key = "second-call".into();
        second.timestamp += Duration::minutes(1);
        full.events.push(second);
        let owned = full
            .events
            .iter()
            .map(|event| (event.key.clone(), event.clone()))
            .collect();
        let parse = |content: &str| {
            serde_json::from_str::<ThreadSample>(content.lines().nth(1).unwrap())
                .into_iter()
                .collect::<Vec<_>>()
        };
        let write_sample = |sample: &ThreadSample| {
            std::fs::write(
                &path,
                format!(
                    "{}\n{}",
                    serde_json::json!({"type":"session","timestamp":sample.created_at}),
                    serde_json::to_string(sample).unwrap()
                ),
            )
            .unwrap()
        };
        write_sample(&full);
        assert_eq!(
            capture_samples(
                &storage,
                "samples",
                "account-a",
                std::slice::from_ref(&path),
                parse,
                &owned,
                Some(instant(7)),
            )
            .unwrap()
            .len(),
            1
        );
        assert!(capture_samples(
            &storage,
            "samples",
            "account-b",
            std::slice::from_ref(&path),
            parse,
            &owned,
            Some(instant(7)),
        )
        .unwrap()
        .is_empty());
        let mut inherited = full.clone();
        inherited.thread_id = "child".into();
        write_sample(&inherited);
        assert!(capture_samples(
            &storage,
            "samples",
            "account-a",
            std::slice::from_ref(&path),
            parse,
            &owned,
            Some(instant(7))
        )
        .unwrap()
        .is_empty());
        // Within one fresh capture, neither file order may decide which copy of
        // an inherited request is allowed to supply a consumption numerator.
        write_sample(&full);
        let copy = directory.path().join("copy.jsonl");
        std::fs::write(
            &copy,
            format!(
                "{}\n{}",
                serde_json::json!({"type":"session","timestamp":inherited.created_at}),
                serde_json::to_string(&inherited).unwrap()
            ),
        )
        .unwrap();
        for (index, order) in [
            vec![path.clone(), copy.clone()],
            vec![copy.clone(), path.clone()],
        ]
        .into_iter()
        .enumerate()
        {
            assert!(capture_samples(
                &storage,
                &format!("overlap-{index}"),
                "account-a",
                &order,
                parse,
                &owned,
                Some(instant(7))
            )
            .unwrap()
            .is_empty());
        }
        let mut prefix = full.clone();
        prefix.events.pop();
        prefix.observed_through -= Duration::minutes(1);
        write_sample(&prefix);
        assert!(capture_samples(
            &storage,
            "samples",
            "account-a",
            std::slice::from_ref(&path),
            parse,
            &owned,
            Some(instant(7)),
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            storage
                .load_usage_events::<ThreadSample>("samples", "account-a", 1)
                .unwrap()[0],
            full
        );
        std::fs::remove_file(&path).unwrap();
        assert!(capture_samples(
            &storage,
            "samples",
            "account-a",
            std::slice::from_ref(&path),
            parse,
            &owned,
            Some(instant(7)),
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn cycle_failure_preserves_daily_usage_and_recursively_clears_credit_cycles() {
        let row = period(
            &[sample("one", "Codex")],
            &HashMap::from([("one".into(), observed("one"))]),
        )
        .unwrap();
        let mut usage = crate::models::UsageHistory {
            today: Some(row.clone()),
            weekly_cycle: Some(row.clone()),
            ..Default::default()
        };
        usage.credit_usage = Some(Box::new(crate::models::UsageHistory {
            weekly_cycle: Some(row),
            ..Default::default()
        }));
        clear_cycles(&mut usage);
        assert!(usage.today.is_some());
        assert!(usage.weekly_cycle.is_none());
        assert!(usage.credit_usage.unwrap().weekly_cycle.is_none());
    }

    #[test]
    fn old_metadata_skips_full_transcript_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("test.db")).unwrap();
        let path = directory.path().join("old.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\nnot-valid-json",
                serde_json::json!({"type":"session","timestamp":instant(6)})
            ),
        )
        .unwrap();
        let result = capture_samples(
            &storage,
            "samples",
            "account",
            &[path],
            |_| panic!("older transcript must not be parsed"),
            &HashMap::new(),
            Some(instant(7)),
        )
        .unwrap();
        assert!(result.is_empty());
    }
}
