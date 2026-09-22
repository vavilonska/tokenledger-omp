use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Days, Local, NaiveDate, Utc};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use walkdir::WalkDir;

use crate::{
    pricing::{ModelPricing, TokenBreakdown},
    providers::{codex::local_usage::estimate_token_cost, daily_usage::DailyUsageAccumulator},
    storage::Storage,
};

mod session;

use super::log_usage::{load_or_parse_log, LogCacheError};
use session::{parse_jsonl, SessionUsageEvent};

#[derive(Debug, thiserror::Error)]
pub enum OmpUsageError {
    #[error("Oh My Pi usage database is unavailable")]
    Database,
    #[error(transparent)]
    LogCache(#[from] LogCacheError),
}

const WEEKLY_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
const ANCHOR_CLUSTER_SECONDS: i64 = 5 * 60;

#[derive(Debug, Default)]
pub struct OmpScanOutcome {
    pub included: bool,
    pub events: Vec<PricedOmpEvent>,
    pub weekly_anchors: Vec<QuotaCycleAnchor>,
}

#[derive(Debug, Clone)]
pub struct PricedOmpEvent {
    pub timestamp: DateTime<Utc>,
    pub model: String,
    pub total: u64,
    pub cost: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct QuotaCycleAnchor {
    pub started_at: DateTime<Utc>,
    pub scheduled_reset_at: DateTime<Utc>,
    pub used_percent: f64,
}

pub(super) struct SessionSource<'a> {
    pub directory: &'a Path,
    pub cache_key: &'a str,
    pub label: &'a str,
    /// Empty means all models. Otherwise compare only explicitly known pricing aliases.
    pub allowed_models: &'a [&'a str],
}

#[allow(clippy::too_many_arguments)]
pub fn scan_into(
    storage: &Storage,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    expected_account_id: Option<&str>,
    session_start: Option<DateTime<Utc>>,
    weekly_start: Option<DateTime<Utc>>,
    accumulator: &mut DailyUsageAccumulator,
    session_accumulator: &mut DailyUsageAccumulator,
    weekly_accumulator: &mut DailyUsageAccumulator,
) -> Result<OmpScanOutcome, OmpUsageError> {
    let home = omp_home();
    scan_home_into(
        storage,
        &home,
        now,
        pricing,
        expected_account_id,
        session_start,
        weekly_start,
        accumulator,
        session_accumulator,
        weekly_accumulator,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_home_into(
    storage: &Storage,
    home: &Path,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    expected_account_id: Option<&str>,
    session_start: Option<DateTime<Utc>>,
    weekly_start: Option<DateTime<Utc>>,
    accumulator: &mut DailyUsageAccumulator,
    session_accumulator: &mut DailyUsageAccumulator,
    weekly_accumulator: &mut DailyUsageAccumulator,
) -> Result<OmpScanOutcome, OmpUsageError> {
    if !same_codex_account(home, expected_account_id)? {
        crate::app_warn!(
            "plugin:omp",
            "Oh My Pi Codex usage skipped because its OAuth account does not match Codex"
        );
        return Ok(OmpScanOutcome::default());
    }

    let weekly_anchors = read_weekly_anchors(
        &home.join("agent").join("agent.db"),
        expected_account_id,
        now,
    )
    .unwrap_or_else(|error| {
        crate::app_warn!(
            "plugin:omp",
            "Oh My Pi weekly quota anchors are unavailable; continuing with session token usage: {error}"
        );
        Vec::new()
    });

    let mut outcome = scan_sessions_into(
        storage,
        SessionSource {
            directory: &home.join("agent").join("sessions"),
            cache_key: "omp",
            label: "Oh My Pi",
            allowed_models: &[],
        },
        now,
        pricing,
        session_start,
        weekly_start,
        accumulator,
        session_accumulator,
        weekly_accumulator,
    )?;
    outcome.weekly_anchors = weekly_anchors;
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn scan_sessions_into(
    storage: &Storage,
    source: SessionSource<'_>,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    session_start: Option<DateTime<Utc>>,
    weekly_start: Option<DateTime<Utc>>,
    accumulator: &mut DailyUsageAccumulator,
    session_accumulator: &mut DailyUsageAccumulator,
    weekly_accumulator: &mut DailyUsageAccumulator,
) -> Result<OmpScanOutcome, LogCacheError> {
    let since_date = now
        .with_timezone(&Local)
        .date_naive()
        .checked_sub_days(Days::new(30))
        .unwrap_or(NaiveDate::MIN);
    let since = since_date
        .and_hms_opt(0, 0, 0)
        .and_then(|value| value.and_local_timezone(Local).earliest())
        .map(|value| value.to_utc())
        .unwrap_or_else(|| now - chrono::Duration::days(31));
    let paths = discover_files(source.directory);
    let mut seen_paths = HashSet::with_capacity(paths.len());
    let mut events = Vec::new();
    for path in paths {
        seen_paths.insert(path.clone());
        if let Some(parsed) = load_or_parse_log(storage, source.cache_key, &path, 1, |content| {
            parse_jsonl(content)
                .into_iter()
                .filter(|event| {
                    source.allowed_models.is_empty()
                        || source
                            .allowed_models
                            .contains(&pricing.display_family(&event.model).as_str())
                })
                .collect::<Vec<_>>()
        })? {
            events.extend(parsed);
        }
    }
    storage
        .prune_log_events(source.cache_key, &seen_paths)
        .map_err(LogCacheError::from)?;
    events.sort_by_key(|event| event.timestamp);
    // References keep the response identifiers out of an extra allocation per event.
    let mut seen_ids = HashSet::new();
    let mut seen_responses = HashSet::new();
    let mut included = false;
    let mut priced_events = Vec::new();

    for event in events.iter().filter(|event| event.timestamp <= now) {
        // Entry IDs are only eight hex characters. A fork inherits the timestamp and
        // model too, while an unrelated request may legitimately reuse the short ID.
        let duplicate_id = event
            .id
            .as_deref()
            .is_some_and(|id| !seen_ids.insert((id, event.timestamp, event.model.as_str())));
        let duplicate_response = event
            .response_id
            .as_deref()
            .is_some_and(|id| !seen_responses.insert(id));
        if duplicate_id || duplicate_response {
            continue;
        }
        let Some(cost) = estimate_token_cost(
            &event.timestamp,
            &event.model,
            TokenBreakdown {
                input: event.input,
                output: event.output,
                cache_read: event.cache_read,
                cache_write_5m: event.cache_write.saturating_sub(event.cache_write_1h),
                cache_write_1h: event.cache_write_1h,
                is_fast: event.is_fast,
            },
            pricing,
        ) else {
            let date = event.timestamp.with_timezone(&Local).date_naive();
            if event.timestamp >= since {
                accumulator.add_unknown_model(date, &event.model);
            }
            if session_start.is_some_and(|start| event.timestamp >= start) {
                session_accumulator.add_unknown_model(date, &event.model);
            }
            if weekly_start.is_some_and(|start| event.timestamp >= start) {
                weekly_accumulator.add_unknown_model(date, &event.model);
            }
            priced_events.push(PricedOmpEvent {
                timestamp: event.timestamp,
                model: event.model.clone(),
                total: event.total,
                cost: None,
            });
            included = true;
            continue;
        };
        let date = event.timestamp.with_timezone(&Local).date_naive();
        if event.timestamp >= since {
            add_event(accumulator, date, event, cost, source.label);
        }
        if session_start.is_some_and(|start| event.timestamp >= start) {
            add_event(session_accumulator, date, event, cost, source.label);
        }
        if weekly_start.is_some_and(|start| event.timestamp >= start) {
            add_event(weekly_accumulator, date, event, cost, source.label);
        }
        priced_events.push(PricedOmpEvent {
            timestamp: event.timestamp,
            model: event.model.clone(),
            total: event.total,
            cost: Some(cost),
        });
        included = true;
    }

    Ok(OmpScanOutcome {
        included,
        events: priced_events,
        weekly_anchors: Vec::new(),
    })
}

fn read_weekly_anchors(
    path: &Path,
    expected_account_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Vec<QuotaCycleAnchor>, OmpUsageError> {
    let Some(account_id) = expected_account_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let connection = open_read_only(path)?;
    let mut statement = connection
        .prepare(
            "SELECT resets_at, COALESCE(used_fraction, 0.0)
             FROM usage_history
             WHERE provider = 'openai-codex' AND label = '7 days'
               AND account_id = ?1 AND resets_at IS NOT NULL AND recorded_at <= ?2
             ORDER BY resets_at ASC",
        )
        .map_err(|_| OmpUsageError::Database)?;
    let rows = statement
        .query_map(params![account_id, now.timestamp_millis()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(|_| OmpUsageError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| OmpUsageError::Database)?;

    let mut clustered: Vec<(i64, f64)> = Vec::new();
    for (reset_ms, used_fraction) in rows {
        if let Some((cluster_reset, cluster_used)) = clustered.last_mut() {
            if reset_ms.saturating_sub(*cluster_reset) <= ANCHOR_CLUSTER_SECONDS * 1000 {
                *cluster_reset = (*cluster_reset).min(reset_ms);
                *cluster_used = cluster_used.max(used_fraction);
                continue;
            }
        }
        clustered.push((reset_ms, used_fraction));
    }

    Ok(clustered
        .into_iter()
        .filter(|(_, used_fraction)| *used_fraction > 0.0)
        .filter_map(|(reset_ms, used_fraction)| {
            let scheduled_reset_at = DateTime::from_timestamp_millis(reset_ms)?;
            let started_at = scheduled_reset_at
                .checked_sub_signed(chrono::Duration::seconds(WEEKLY_WINDOW_SECONDS))?;
            Some(QuotaCycleAnchor {
                started_at,
                scheduled_reset_at,
                used_percent: (used_fraction * 100.0).clamp(0.0, 100.0),
            })
        })
        .collect())
}

fn add_event(
    accumulator: &mut DailyUsageAccumulator,
    date: NaiveDate,
    event: &SessionUsageEvent,
    cost: f64,
    source: &str,
) {
    accumulator.add_variant(date, event.total, cost, event.model.trim(), source);
}

fn discover_files(directory: &Path) -> Vec<PathBuf> {
    let directory = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    let mut paths = WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
        })
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn same_codex_account(
    home: &Path,
    expected_account_id: Option<&str>,
) -> Result<bool, OmpUsageError> {
    let Some(expected) = expected_account_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(false);
    };
    let database = home.join("agent").join("agent.db");
    if !database.is_file() {
        return Ok(false);
    }
    let connection = open_read_only(&database)?;
    let data = connection
        .query_row(
            "SELECT data FROM auth_credentials \
             WHERE provider = 'openai-codex' AND credential_type = 'oauth' \
               AND disabled_cause IS NULL \
             ORDER BY updated_at DESC LIMIT 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|_| OmpUsageError::Database)?;
    let account_id = data
        .as_deref()
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|document| {
            document
                .get("accountId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    Ok(account_id.as_deref() == Some(expected))
}

fn open_read_only(path: &Path) -> Result<Connection, OmpUsageError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| OmpUsageError::Database)?;
    connection
        .busy_timeout(Duration::from_millis(750))
        .map_err(|_| OmpUsageError::Database)?;
    Ok(connection)
}

fn omp_home() -> PathBuf {
    std::env::var_os("OMP_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_directory().join(".omp"))
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, fs};

    use chrono::{TimeZone, Utc};
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{read_weekly_anchors, same_codex_account, scan_home_into, OmpScanOutcome};
    use crate::{
        pricing::{ModelPricing, ModelRates, PricingCatalog, PricingSupplement},
        providers::daily_usage::DailyUsageAccumulator,
        storage::Storage,
    };

    fn create_databases(home: &std::path::Path, account: &str) {
        fs::create_dir_all(home.join("agent")).unwrap();
        let auth = Connection::open(home.join("agent").join("agent.db")).unwrap();
        auth.execute_batch(
            "CREATE TABLE auth_credentials (provider TEXT, credential_type TEXT, data TEXT, \
             disabled_cause TEXT, updated_at INTEGER);
             CREATE TABLE usage_history (
               recorded_at INTEGER,
               provider TEXT,
               account_id TEXT,
               label TEXT,
               used_fraction REAL,
               resets_at INTEGER
             );",
        )
        .unwrap();
        auth.execute(
            "INSERT INTO auth_credentials VALUES ('openai-codex', 'oauth', ?1, NULL, 1)",
            [serde_json::json!({"accountId": account}).to_string()],
        )
        .unwrap();
        auth.execute(
            "INSERT INTO usage_history VALUES (
               1783864800000, 'openai-codex', ?1, '7 days', 0.2, 1784469600000
             )",
            [account],
        )
        .unwrap();

        write_session(
            home,
            "parent.jsonl",
            &[message("abc01234", "2026-07-12T14:00:00Z")],
        );
    }

    #[test]
    fn oauth_account_must_match_without_exposing_credentials() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        assert!(same_codex_account(directory.path(), Some("account-a")).unwrap());
        assert!(!same_codex_account(directory.path(), Some("account-b")).unwrap());
        assert!(!same_codex_account(directory.path(), None).unwrap());
    }

    #[test]
    fn weekly_history_uses_server_reset_anchors() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 14, 0, 0).unwrap();
        let anchors = read_weekly_anchors(
            &directory.path().join("agent").join("agent.db"),
            Some("account-a"),
            now,
        )
        .unwrap();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].used_percent, 20.0);
        assert_eq!(
            anchors[0].scheduled_reset_at.timestamp_millis(),
            1784469600000
        );
        assert_eq!(
            anchors[0].started_at,
            anchors[0].scheduled_reset_at - chrono::Duration::days(7)
        );
    }

    fn message(id: &str, timestamp: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message", "id": id, "timestamp": timestamp,
            "message": {
                "role": "assistant", "provider": "openai-codex", "model": "gpt-test",
                "usage": {
                    "input": 100, "output": 20, "cacheRead": 50, "cacheWrite": 10,
                    "totalTokens": 180, "cost": {"total": 0}
                }
            }
        })
    }

    fn write_session(home: &std::path::Path, name: &str, messages: &[serde_json::Value]) {
        let path = home.join("agent").join("sessions").join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut content = String::from("{\"type\":\"session\",\"id\":\"session\"}\n");
        for message in messages {
            content.push_str(&message.to_string());
            content.push('\n');
        }
        fs::write(path, content).unwrap();
    }

    fn scan(storage: &Storage, home: &std::path::Path, account: &str) -> OmpScanOutcome {
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap();
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-test".into(), ModelRates::new(4.0, 20.0))]),
                ..PricingCatalog::default()
            },
            PricingCatalog::default(),
        );
        scan_home_into(
            storage,
            home,
            now,
            &pricing,
            Some(account),
            None,
            None,
            &mut DailyUsageAccumulator::default(),
            &mut DailyUsageAccumulator::default(),
            &mut DailyUsageAccumulator::default(),
        )
        .unwrap()
    }

    #[test]
    fn session_append_refreshes_cached_usage_without_stats_database() {
        use std::io::Write;
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        assert!(!directory.path().join("stats.db").exists());
        let initial = scan(&storage, directory.path(), "account-a");
        assert_eq!(
            initial.events.iter().map(|event| event.total).sum::<u64>(),
            180
        );
        assert!(initial.events[0].cost.is_some_and(|cost| cost > 0.0));
        let mut log = fs::OpenOptions::new()
            .append(true)
            .open(directory.path().join("agent/sessions/parent.jsonl"))
            .unwrap();
        writeln!(log, "{}", message("def05678", "2026-07-12T14:01:00Z")).unwrap();
        let refreshed = scan(&storage, directory.path(), "account-a");
        assert_eq!(
            refreshed
                .events
                .iter()
                .map(|event| event.total)
                .sum::<u64>(),
            360
        );
        assert_eq!(
            scan(&storage, directory.path(), "account-a").events.len(),
            2
        );
    }

    #[test]
    fn forks_deduplicate_inherited_calls_but_not_reused_short_ids() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        write_session(
            directory.path(),
            "fork.jsonl",
            &[
                message("abc01234", "2026-07-12T14:00:00Z"),
                message("abc01234", "2026-07-12T14:01:00Z"),
            ],
        );
        let outcome = scan(&storage, directory.path(), "account-a");
        assert_eq!(outcome.events.len(), 2);
        assert_eq!(
            outcome.events.iter().map(|event| event.total).sum::<u64>(),
            360
        );
    }

    #[test]
    fn task_summary_does_not_duplicate_nested_child_usage() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let mut summary = message("1234abcd", "2026-07-12T14:00:00Z");
        summary["message"]["role"] = "toolResult".into();
        summary["message"]["toolName"] = "task".into();
        write_session(directory.path(), "parent.jsonl", &[summary]);
        write_session(
            directory.path(),
            "parent/child.jsonl",
            &[message("abc01234", "2026-07-12T14:00:00Z")],
        );
        let outcome = scan(&storage, directory.path(), "account-a");
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(outcome.events[0].total, 180);
    }

    #[test]
    fn mismatched_account_does_not_include_session_usage() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let outcome = scan(&storage, directory.path(), "account-b");
        assert!(!outcome.included);
        assert!(outcome.events.is_empty());
        assert!(outcome.weekly_anchors.is_empty());
    }

    #[test]
    fn missing_quota_history_does_not_block_session_tokens() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        Connection::open(directory.path().join("agent/agent.db"))
            .unwrap()
            .execute_batch("DROP TABLE usage_history")
            .unwrap();
        let outcome = scan(&storage, directory.path(), "account-a");
        assert!(outcome.included);
        assert_eq!(outcome.events[0].total, 180);
        assert!(outcome.weekly_anchors.is_empty());
    }

    #[test]
    fn response_ids_deduplicate_but_anonymous_equal_usage_remains_independent() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let mut first = message("abc01234", "2026-07-12T14:00:00Z");
        first["message"]["responseId"] = "resp-unique".into();
        let mut copied = first.clone();
        copied["id"] = "def05678".into();
        let mut anonymous = message("ignored", "2026-07-12T14:00:00Z");
        anonymous.as_object_mut().unwrap().remove("id");
        write_session(directory.path(), "parent.jsonl", &[first]);
        write_session(
            directory.path(),
            "copy.jsonl",
            &[copied, anonymous.clone(), anonymous],
        );
        let outcome = scan(&storage, directory.path(), "account-a");
        assert_eq!(outcome.events.len(), 3);
        assert_eq!(
            outcome.events.iter().map(|event| event.total).sum::<u64>(),
            540
        );
    }

    #[test]
    fn unknown_models_mark_each_active_cycle_incomplete_and_reach_history() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let mut unknown = message("def05678", "2026-07-12T14:00:00Z");
        unknown["message"]["model"] = "unknown-omp-model".into();
        write_session(directory.path(), "unknown.jsonl", &[unknown]);
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap();
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-test".into(), ModelRates::new(4.0, 20.0))]),
                ..PricingCatalog::default()
            },
            PricingCatalog::default(),
        );
        let mut daily = DailyUsageAccumulator::default();
        let mut session = DailyUsageAccumulator::default();
        let mut weekly = DailyUsageAccumulator::default();
        let outcome = scan_home_into(
            &storage,
            directory.path(),
            now,
            &pricing,
            Some("account-a"),
            Some(now - chrono::Duration::hours(5)),
            Some(now - chrono::Duration::days(7)),
            &mut daily,
            &mut session,
            &mut weekly,
        )
        .unwrap();
        assert!(outcome
            .events
            .iter()
            .any(|event| event.model == "unknown-omp-model" && event.cost.is_none()));
        for accumulator in [daily, session, weekly] {
            let period = accumulator.build(now, "test").last_30_days.unwrap();
            assert!(!period.estimate_complete);
            assert_eq!(period.unknown_models, ["unknown-omp-model"]);
        }
    }

    #[test]
    fn matching_account_usage_is_folded_into_each_active_cycle() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 14, 0, 0).unwrap();
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-test".into(), ModelRates::new(4.0, 20.0))]),
                ..PricingCatalog::default()
            },
            PricingCatalog::default(),
        );
        let mut all = DailyUsageAccumulator::default();
        let mut session = DailyUsageAccumulator::default();
        let mut weekly = DailyUsageAccumulator::default();

        let outcome = scan_home_into(
            &Storage::open(&directory.path().join("cache.db")).unwrap(),
            directory.path(),
            now,
            &pricing,
            Some("account-a"),
            Some(now - chrono::Duration::hours(5)),
            Some(now - chrono::Duration::days(7)),
            &mut all,
            &mut session,
            &mut weekly,
        )
        .unwrap();

        assert!(outcome.included);
        for history in [
            all.build(now, "test"),
            session.build(now, "test"),
            weekly.build(now, "test"),
        ] {
            let period = history.last_30_days.unwrap();
            assert_eq!(period.tokens, 180);
            let variants = period.model_breakdown.unwrap().models[0]
                .variants
                .clone()
                .unwrap();
            assert_eq!(variants[0].model, "Oh My Pi");
        }
    }
}
