use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Days, Local, NaiveDate, Utc};
use rusqlite::{config::DbConfig, params, Connection, OpenFlags, OptionalExtension};

use crate::{
    pricing::{ModelPricing, TokenBreakdown},
    providers::{
        codex::local_usage::{estimate_token_cost, model_has_rates},
        daily_usage::DailyUsageAccumulator,
    },
    storage::Storage,
};

mod discovery;
mod session;

use super::log_usage::LogCacheError;
use discovery::{discover_omp_files, local_directory, local_file_state, LocalFileState};
use session::{
    merge_session_event, normalize_anonymous_occurrences, parse_session_usage, SessionUsageEvent,
};

#[cfg(test)]
use walkdir::WalkDir;

#[cfg(test)]
use session::parse_jsonl;

#[derive(Debug, thiserror::Error)]
pub enum OmpUsageError {
    #[error("Oh My Pi usage database is unavailable")]
    Database,
    #[error("Oh My Pi session files could not be discovered")]
    Discovery,
    #[error(transparent)]
    LogCache(#[from] LogCacheError),
}

const OMP_CACHE_KEY: &str = "omp";
pub(super) const OMP_CODEX_IMAGE_MODEL: &str = "gpt-image-2-codex";
const WEEKLY_WINDOW_SECONDS: i64 = 7 * 24 * 60 * 60;
const ANCHOR_CLUSTER_SECONDS: i64 = 5 * 60;

#[derive(Debug, Default)]
#[cfg(test)]
pub struct OmpScanOutcome {
    pub included: bool,
    pub events: Vec<PricedOmpEvent>,
    pub weekly_anchors: Vec<QuotaCycleAnchor>,
}

#[derive(Debug, Clone)]
#[cfg(test)]
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
    pub exhausted_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
pub(super) struct SessionSource<'a> {
    pub directory: &'a Path,
    pub cache_key: &'a str,
    pub identity_key: &'a str,
    pub label: &'a str,
    /// Empty means all models. Otherwise compare only explicitly known pricing aliases.
    pub allowed_models: &'a [&'a str],
}

#[derive(Default)]
pub(super) struct OmpUsage {
    events: Vec<SessionUsageEvent>,
    pub weekly_anchors: Vec<QuotaCycleAnchor>,
}

impl OmpUsage {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn aggregate_into(
        &self,
        now: DateTime<Utc>,
        pricing: &ModelPricing,
        session_start: Option<DateTime<Utc>>,
        weekly_start: Option<DateTime<Utc>>,
        accumulator: &mut DailyUsageAccumulator,
        session_accumulator: &mut DailyUsageAccumulator,
        weekly_accumulator: &mut DailyUsageAccumulator,
        add_cycle_event: impl FnMut(DateTime<Utc>, &str, u64, Option<f64>, bool),
    ) -> bool {
        aggregate_session_events(
            &self.events,
            "Oh My Pi",
            now,
            pricing,
            session_start,
            weekly_start,
            accumulator,
            session_accumulator,
            weekly_accumulator,
            add_cycle_event,
        )
    }
}

pub(super) fn retain(
    storage: &Storage,
    now: DateTime<Utc>,
    expected_account_id: Option<&str>,
) -> Result<OmpUsage, OmpUsageError> {
    retain_home(storage, &omp_home(), now, expected_account_id)
}

fn retain_home(
    storage: &Storage,
    home: &Path,
    now: DateTime<Utc>,
    expected_account_id: Option<&str>,
) -> Result<OmpUsage, OmpUsageError> {
    let Some(weekly_anchors) = read_profile(home, expected_account_id, now)? else {
        crate::app_warn!(
            "plugin:omp",
            "Oh My Pi Codex usage skipped because its OAuth account could not be matched to Codex"
        );
        return Ok(OmpUsage::default());
    };

    // Keep ownership under the original, untrimmed Codex identity key.
    let identity_key =
        crate::hashing::sha256_hex(expected_account_id.unwrap_or_default().as_bytes());
    let discovered = discover_omp_files(home, &[]).map_err(|_| OmpUsageError::Discovery)?;
    let discovery_complete = discovered.is_complete();
    if !discovery_complete {
        crate::app_warn!(
            "plugin:omp",
            "An Oh My Pi session could not be verified; cached sessions will not be pruned"
        );
    }
    let events = retain_session_events(
        storage,
        OMP_CACHE_KEY,
        &identity_key,
        discovered.paths(),
        |path| discovered.open(path),
        |path| discovered.accepts_legacy_path(path),
        discovery_complete,
        |_| true,
    )?;
    Ok(OmpUsage {
        events: select_session_events(events, now, true),
        weekly_anchors,
    })
}

#[cfg(test)]
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
    let usage = retain_home(storage, home, now, expected_account_id)?;
    let mut outcome = collect_priced_events(
        &usage.events,
        "Oh My Pi",
        now,
        pricing,
        session_start,
        weekly_start,
        accumulator,
        session_accumulator,
        weekly_accumulator,
    );
    outcome.weekly_anchors = usage.weekly_anchors;
    Ok(outcome)
}

#[cfg(test)]
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
    let directory =
        fs::canonicalize(source.directory).unwrap_or_else(|_| source.directory.to_path_buf());
    let paths = discover_files(&directory);
    let events = retain_session_events(
        storage,
        source.cache_key,
        source.identity_key,
        &paths,
        |path| {
            #[cfg(test)]
            if source.cache_key == OMP_CACHE_KEY {
                return match local_file_state(path)? {
                    LocalFileState::Present(file) => Ok(Some(file)),
                    LocalFileState::Missing => Ok(None),
                    LocalFileState::Rejected => Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "invalid local log path",
                    )),
                };
            }
            // Recovery can visit an old path that is no longer a regular log.
            // Do not block by opening a FIFO or device in the independent source.
            match fs::metadata(path) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "local log path is not a regular file",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
            match fs::File::open(path) {
                Ok(file) => Ok(Some(file)),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        },
        |path| path.starts_with(&directory) || path.starts_with(source.directory),
        true,
        |event| {
            source.allowed_models.is_empty()
                || source
                    .allowed_models
                    .contains(&pricing.display_family(&event.model).as_str())
        },
    )?;
    let events = select_session_events(events, now, source.cache_key == OMP_CACHE_KEY);
    Ok(collect_priced_events(
        &events,
        source.label,
        now,
        pricing,
        session_start,
        weekly_start,
        accumulator,
        session_accumulator,
        weekly_accumulator,
    ))
}

#[allow(clippy::too_many_arguments)]
fn retain_session_events(
    storage: &Storage,
    cache_key: &str,
    identity_key: &str,
    paths: &[PathBuf],
    opener: impl Fn(&Path) -> io::Result<Option<fs::File>>,
    accepts_legacy_path: impl Fn(&Path) -> bool,
    discovery_complete: bool,
    include: impl Fn(&SessionUsageEvent) -> bool,
) -> Result<Vec<SessionUsageEvent>, LogCacheError> {
    let exclude_image_usage = cache_key == OMP_CACHE_KEY;
    super::log_usage::retain_log_usage_with_normalization(
        storage,
        cache_key,
        identity_key,
        "codex",
        // The event schema is unchanged; compatible caches can still recover text usage.
        11,
        1,
        paths,
        opener,
        accepts_legacy_path,
        exclude_image_usage,
        discovery_complete,
        |content| parse_session_usage(content, exclude_image_usage),
        normalize_anonymous_occurrences,
        |event: &SessionUsageEvent| {
            if let Some(id) = event.response_id.as_deref() {
                super::log_usage::usage_event_key(&("response", id))
            } else if let Some(id) = event.id.as_deref() {
                super::log_usage::usage_event_key(&("entry", id, event.timestamp, &event.model))
            } else if event.anonymous_occurrence <= 1 {
                // Keep the first occurrence's legacy key and its original account ownership.
                super::log_usage::usage_event_key(&(
                    &event.session_id,
                    event.timestamp,
                    &event.model,
                ))
            } else {
                super::log_usage::usage_event_key(&(
                    &event.session_id,
                    event.timestamp,
                    &event.model,
                    event.anonymous_occurrence,
                ))
            }
        },
        |event| (!exclude_image_usage || !event.is_image_usage()) && include(event),
        Some(merge_session_event),
    )
}

fn select_session_events(
    mut events: Vec<SessionUsageEvent>,
    now: DateTime<Utc>,
    exclude_image_usage: bool,
) -> Vec<SessionUsageEvent> {
    // Keep previously ledgered images intact without putting them back into metering.
    // Cancelled/error replies with no usage cannot make a priced period partial.
    events.retain(|event| {
        event.timestamp <= now
            && (!exclude_image_usage || !event.is_image_usage())
            && (event.total != 0
                || event.input != 0
                || event.output != 0
                || event.cache_read != 0
                || event.cache_write != 0
                || event.cache_write_1h != 0
                || event.cache_write_12h != 0)
    });
    events.sort_by_key(|event| event.timestamp);
    // References keep the response identifiers out of an extra allocation per event.
    let mut seen_ids = HashSet::new();
    let mut seen_responses = HashSet::new();
    let mut duplicates = Vec::new();
    for (index, event) in events.iter().enumerate() {
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
            duplicates.push(index);
        }
    }
    if !duplicates.is_empty() {
        let mut duplicates = duplicates.into_iter().peekable();
        let mut index = 0;
        events.retain(|_| {
            let keep = duplicates.next_if_eq(&index).is_none();
            index += 1;
            keep
        });
    }
    events
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn collect_priced_events(
    events: &[SessionUsageEvent],
    label: &str,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    session_start: Option<DateTime<Utc>>,
    weekly_start: Option<DateTime<Utc>>,
    accumulator: &mut DailyUsageAccumulator,
    session_accumulator: &mut DailyUsageAccumulator,
    weekly_accumulator: &mut DailyUsageAccumulator,
) -> OmpScanOutcome {
    let mut priced = Vec::with_capacity(events.len());
    let included = aggregate_session_events(
        events,
        label,
        now,
        pricing,
        session_start,
        weekly_start,
        accumulator,
        session_accumulator,
        weekly_accumulator,
        |timestamp, model, total, cost, _| {
            priced.push(PricedOmpEvent {
                timestamp,
                model: model.to_owned(),
                total,
                cost,
            });
        },
    );
    OmpScanOutcome {
        included,
        events: priced,
        weekly_anchors: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn aggregate_session_events(
    events: &[SessionUsageEvent],
    label: &str,
    now: DateTime<Utc>,
    pricing: &ModelPricing,
    session_start: Option<DateTime<Utc>>,
    weekly_start: Option<DateTime<Utc>>,
    accumulator: &mut DailyUsageAccumulator,
    session_accumulator: &mut DailyUsageAccumulator,
    weekly_accumulator: &mut DailyUsageAccumulator,
    mut add_cycle_event: impl FnMut(DateTime<Utc>, &str, u64, Option<f64>, bool),
) -> bool {
    let since_date = now
        .with_timezone(&Local)
        .date_naive()
        .checked_sub_days(Days::new(29))
        .unwrap_or(NaiveDate::MIN);
    for event in events {
        let cost = if event.entry_kind.as_deref() == Some("model_usage")
            && event.purpose.as_deref() == Some("memory")
        {
            // Exclude memory from display estimates, not from retained usage or requests.
            Some(0.0)
        } else {
            event
                .speed
                .filter(|_| event.has_complete_usage())
                .and_then(|speed| {
                    estimate_token_cost(
                        &event.timestamp,
                        &event.model,
                        TokenBreakdown {
                            input: event.input,
                            output: event.output,
                            cache_read: event.cache_read,
                            cache_write_5m: event
                                .cache_write
                                .saturating_sub(event.cache_write_1h)
                                .saturating_sub(event.cache_write_12h),
                            cache_write_1h: event.cache_write_1h,
                            cache_write_12h: event.cache_write_12h,
                            speed,
                        },
                        pricing,
                    )
                })
        };
        let unknown_model =
            cost.is_none() && !model_has_rates(&event.timestamp, &event.model, pricing);
        add_cycle_event(
            event.timestamp,
            &event.model,
            event.total,
            cost,
            unknown_model,
        );
        let date = event.timestamp.with_timezone(&Local).date_naive();
        let Some(cost) = cost else {
            if date >= since_date {
                accumulator.add_unpriced_variant(
                    date,
                    event.total,
                    &event.model,
                    label,
                    event.cache_usage,
                );
                if unknown_model {
                    accumulator.add_unknown_model(date, &event.model);
                }
            }
            if session_start.is_some_and(|start| event.timestamp >= start) {
                session_accumulator.add_unpriced_variant(
                    date,
                    event.total,
                    &event.model,
                    label,
                    event.cache_usage,
                );
                if unknown_model {
                    session_accumulator.add_unknown_model(date, &event.model);
                }
            }
            if weekly_start.is_some_and(|start| event.timestamp >= start) {
                weekly_accumulator.add_unpriced_variant(
                    date,
                    event.total,
                    &event.model,
                    label,
                    event.cache_usage,
                );
                if unknown_model {
                    weekly_accumulator.add_unknown_model(date, &event.model);
                }
            }
            continue;
        };
        if date >= since_date {
            add_event(accumulator, date, event, cost, label);
        }
        if session_start.is_some_and(|start| event.timestamp >= start) {
            add_event(session_accumulator, date, event, cost, label);
        }
        if weekly_start.is_some_and(|start| event.timestamp >= start) {
            add_event(weekly_accumulator, date, event, cost, label);
        }
    }

    !events.is_empty()
}

fn read_weekly_anchors(
    connection: &Connection,
    account_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<QuotaCycleAnchor>, OmpUsageError> {
    // Older profiles may not have this optional table. Fail on other read errors.
    let has_history: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_schema
                           WHERE name = 'usage_history' COLLATE NOCASE
                             AND type IN ('table', 'view'))",
            [],
            |row| row.get(0),
        )
        .map_err(|_| OmpUsageError::Database)?;
    if !has_history {
        return Ok(Vec::new());
    }
    let mut statement = connection
        .prepare(
            "SELECT resets_at, COALESCE(used_fraction, 0.0), recorded_at
             FROM main.usage_history
             WHERE provider = 'openai-codex' AND label = '7 days'
               AND account_id = ?1 AND resets_at IS NOT NULL AND recorded_at <= ?2
             ORDER BY resets_at ASC",
        )
        .map_err(|_| OmpUsageError::Database)?;
    let rows = statement
        .query_map(params![account_id, now.timestamp_millis()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| OmpUsageError::Database)?;

    let mut clustered: Vec<(i64, f64, Option<DateTime<Utc>>)> = Vec::new();
    for row in rows {
        let (reset_ms, used_fraction, recorded_ms) = row.map_err(|_| OmpUsageError::Database)?;
        let exhausted_at = (used_fraction >= 1.0)
            .then(|| DateTime::from_timestamp_millis(recorded_ms))
            .flatten();
        if let Some((cluster_reset, cluster_used, cluster_exhausted)) = clustered.last_mut() {
            if reset_ms.saturating_sub(*cluster_reset) <= ANCHOR_CLUSTER_SECONDS * 1000 {
                *cluster_reset = (*cluster_reset).min(reset_ms);
                *cluster_used = cluster_used.max(used_fraction);
                *cluster_exhausted = (*cluster_exhausted).into_iter().chain(exhausted_at).min();
                continue;
            }
        }
        clustered.push((reset_ms, used_fraction, exhausted_at));
    }

    Ok(clustered
        .into_iter()
        .filter(|(_, used_fraction, _)| *used_fraction > 0.0)
        .filter_map(|(reset_ms, used_fraction, exhausted_at)| {
            let scheduled_reset_at = DateTime::from_timestamp_millis(reset_ms)?;
            let started_at = scheduled_reset_at
                .checked_sub_signed(chrono::Duration::seconds(WEEKLY_WINDOW_SECONDS))?;
            Some(QuotaCycleAnchor {
                started_at,
                scheduled_reset_at,
                used_percent: (used_fraction * 100.0).clamp(0.0, 100.0),
                exhausted_at,
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
    accumulator.add_variant(
        date,
        event.total,
        cost,
        event.model.trim(),
        source,
        event.cache_usage,
    );
}

#[cfg(test)]
fn discover_files(directory: &Path) -> Vec<PathBuf> {
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

fn read_profile(
    home: &Path,
    expected_account_id: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Option<Vec<QuotaCycleAnchor>>, OmpUsageError> {
    let Some(account_id) = expected_account_id
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(None);
    };
    let Some(mut connection) = open_profile_db(home)? else {
        return Ok(None);
    };
    // Keep the account check and anchor reads in the same SQLite read snapshot.
    let transaction = connection
        .transaction()
        .map_err(|_| OmpUsageError::Database)?;
    if !same_codex_account(&transaction, account_id)? {
        return Ok(None);
    }
    match read_weekly_anchors(&transaction, account_id, now) {
        Ok(anchors) => {
            transaction.commit().map_err(|_| OmpUsageError::Database)?;
            Ok(Some(anchors))
        }
        Err(_) => {
            crate::app_warn!("plugin:omp", "Oh My Pi weekly usage history is unavailable");
            Ok(Some(Vec::new()))
        }
    }
}

fn same_codex_account(connection: &Connection, expected: &str) -> Result<bool, OmpUsageError> {
    connection
        .query_row(
            "SELECT CASE WHEN typeof(data) = 'text' AND json_valid(data) THEN
                       CASE WHEN json_type(data, '$.accountId') = 'text'
                            THEN json_extract(data, '$.accountId') = ?1
                            ELSE 0 END
                    ELSE 0 END
             FROM main.auth_credentials
             WHERE provider = 'openai-codex' AND credential_type = 'oauth'
               AND disabled_cause IS NULL
             ORDER BY updated_at DESC LIMIT 1",
            [expected],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map(|matched| matched.unwrap_or(false))
        .map_err(|_| OmpUsageError::Database)
}

fn open_profile_db(home: &Path) -> Result<Option<Connection>, OmpUsageError> {
    let Some(agent) = local_directory(&home.join("agent")).map_err(|_| OmpUsageError::Database)?
    else {
        return Ok(None);
    };
    let path = agent.join("agent.db");
    match local_file_state(&path).map_err(|_| OmpUsageError::Database)? {
        LocalFileState::Missing => return Ok(None),
        LocalFileState::Rejected => return Err(OmpUsageError::Database),
        LocalFileState::Present(_) => {}
    }
    // SQLite opens the paths itself, and its Windows VFS may not honor NOFOLLOW.
    // This rejects static redirects but is not atomic with SQLite's later opens.
    for name in ["agent.db-wal", "agent.db-shm", "agent.db-journal"] {
        match local_file_state(&agent.join(name)).map_err(|_| OmpUsageError::Database)? {
            LocalFileState::Missing | LocalFileState::Present(_) => {}
            LocalFileState::Rejected => return Err(OmpUsageError::Database),
        }
    }
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| OmpUsageError::Database)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .map_err(|_| OmpUsageError::Database)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)
        .map_err(|_| OmpUsageError::Database)?;
    connection
        .busy_timeout(Duration::from_millis(750))
        .map_err(|_| OmpUsageError::Database)?;
    Ok(Some(connection))
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
    mod regression;

    use std::{collections::HashMap, fs};

    use chrono::{TimeZone, Utc};
    use rusqlite::Connection;
    #[cfg(not(target_os = "macos"))]
    use tempfile::tempdir;

    #[cfg(target_os = "macos")]
    fn tempdir() -> std::io::Result<tempfile::TempDir> {
        // The system temp directory may be under /var, a symlink to /private/var.
        tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)
    }

    use super::{
        parse_jsonl, read_profile, read_weekly_anchors, scan_home_into, scan_sessions_into,
        OmpScanOutcome, SessionSource,
    };
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
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 14, 0, 0).unwrap();
        assert!(read_profile(directory.path(), Some("account-a"), now)
            .unwrap()
            .is_some());
        assert!(read_profile(directory.path(), Some("account-b"), now)
            .unwrap()
            .is_none());
        assert!(read_profile(directory.path(), None, now).unwrap().is_none());
    }

    #[test]
    fn weekly_history_uses_server_reset_anchors() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 14, 0, 0).unwrap();
        let anchors = read_profile(directory.path(), Some("account-a"), now)
            .unwrap()
            .unwrap();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].used_percent, 20.0);
        assert_eq!(anchors[0].exhausted_at, None);
        assert_eq!(
            anchors[0].scheduled_reset_at.timestamp_millis(),
            1784469600000
        );
        assert_eq!(
            anchors[0].started_at,
            anchors[0].scheduled_reset_at - chrono::Duration::days(7)
        );
    }

    #[test]
    fn weekly_exhaustion_uses_earliest_matching_millisecond_observation_in_reset_cluster() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let path = directory.path().join("agent/agent.db");
        let connection = Connection::open(&path).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap();
        let earliest = now - chrono::Duration::hours(1) + chrono::Duration::milliseconds(123);
        let reset = 1784469600000_i64;
        for (account, recorded, used, reset_ms) in [
            (
                "account-a",
                earliest + chrono::Duration::minutes(10),
                1.1,
                reset,
            ),
            ("account-a", earliest, 1.0, reset + 120_000),
            (
                "account-a",
                earliest - chrono::Duration::minutes(1),
                0.99,
                reset,
            ),
            (
                "account-b",
                earliest - chrono::Duration::minutes(2),
                1.0,
                reset,
            ),
            ("account-a", now + chrono::Duration::seconds(1), 1.0, reset),
            ("account-a", now, 0.25, reset + 604_800_000),
        ] {
            connection
                .execute(
                    "INSERT INTO usage_history VALUES (?1, 'openai-codex', ?2, '7 days', ?3, ?4)",
                    rusqlite::params![recorded.timestamp_millis(), account, used, reset_ms],
                )
                .unwrap();
        }
        let anchors = read_weekly_anchors(&connection, "account-a", now).unwrap();
        assert_eq!(anchors.len(), 2);
        assert_eq!(anchors[0].scheduled_reset_at.timestamp_millis(), reset);
        assert_eq!(anchors[0].used_percent, 100.0);
        assert_eq!(anchors[0].exhausted_at, Some(earliest));
        assert_eq!(anchors[1].exhausted_at, None);
        assert_eq!(anchors[1].used_percent, 25.0);
        let other = read_weekly_anchors(&connection, "account-b", now).unwrap();
        assert_eq!(other.len(), 1);
        assert_eq!(
            other[0].exhausted_at,
            Some(earliest - chrono::Duration::minutes(2))
        );
    }

    fn message(id: &str, timestamp: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "message", "id": id, "timestamp": timestamp,
            "message": {
                "role": "assistant", "provider": "openai-codex", "model": "gpt-test",
                "serviceTier": "default",
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
    fn retained_omp_usage_stays_with_its_matching_oauth_account() {
        use std::io::Write;
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
        assert_eq!(
            scan(&storage, directory.path(), "account-a")
                .events
                .iter()
                .map(|event| event.total)
                .sum::<u64>(),
            180
        );
        let auth = Connection::open(directory.path().join("agent/agent.db")).unwrap();
        let change_account = |account| {
            auth.execute(
                "UPDATE auth_credentials SET data = ?1 WHERE provider = 'openai-codex'",
                [serde_json::json!({"accountId": account}).to_string()],
            )
            .unwrap()
        };
        change_account("account-b");
        assert!(!scan(&storage, directory.path(), "account-a").included);
        assert!(!scan(&storage, directory.path(), "account-b").included);
        let path = directory.path().join("agent/sessions/parent.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", message("new-b", "2026-07-12T14:02:00Z")).unwrap();
        drop(file);
        assert_eq!(
            scan(&storage, directory.path(), "account-b")
                .events
                .iter()
                .map(|event| event.total)
                .sum::<u64>(),
            180
        );
        fs::remove_file(&path).unwrap();
        assert_eq!(
            scan(&storage, directory.path(), "account-b")
                .events
                .iter()
                .map(|event| event.total)
                .sum::<u64>(),
            180
        );
        change_account("account-a");
        assert_eq!(
            scan(&storage, directory.path(), "account-a")
                .events
                .iter()
                .map(|event| event.total)
                .sum::<u64>(),
            180
        );
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
    fn missing_or_unreadable_quota_history_does_not_block_session_tokens() {
        let directory = tempdir().unwrap();
        create_databases(directory.path(), "account-a");
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let profile = Connection::open(directory.path().join("agent/agent.db")).unwrap();
        for ddl in [
            "DROP TABLE usage_history",
            "CREATE TABLE usage_history (unexpected_column INTEGER)",
        ] {
            profile.execute_batch(ddl).unwrap();
            let outcome = scan(&storage, directory.path(), "account-a");
            assert!(outcome.included);
            assert_eq!(outcome.events[0].total, 180);
            assert!(outcome.weekly_anchors.is_empty());
        }
    }

    #[test]
    fn response_ids_deduplicate_but_anonymous_equal_usage_remains_independent() {
        use std::io::Write;
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
            &[copied, anonymous.clone(), anonymous.clone()],
        );
        let outcome = scan(&storage, directory.path(), "account-a");
        assert_eq!(outcome.events.len(), 3);
        assert_eq!(
            outcome.events.iter().map(|event| event.total).sum::<u64>(),
            540
        );
        let sessions = directory.path().join("agent/sessions");
        fs::copy(sessions.join("copy.jsonl"), sessions.join("mirror.jsonl")).unwrap();
        fs::rename(sessions.join("copy.jsonl"), sessions.join("moved.jsonl")).unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(sessions.join("moved.jsonl"))
            .unwrap();
        writeln!(file, "{anonymous}").unwrap();
        drop(file);
        assert_eq!(
            scan(&storage, directory.path(), "account-a").events.len(),
            4
        );
        let auth = Connection::open(directory.path().join("agent/agent.db")).unwrap();
        auth.execute(
            "UPDATE auth_credentials SET data = ?1 WHERE provider = 'openai-codex'",
            [serde_json::json!({"accountId": "account-b"}).to_string()],
        )
        .unwrap();
        assert!(!scan(&storage, directory.path(), "account-b").included);
        auth.execute(
            "UPDATE auth_credentials SET data = ?1 WHERE provider = 'openai-codex'",
            [serde_json::json!({"accountId": "account-a"}).to_string()],
        )
        .unwrap();
        fs::remove_dir_all(sessions).unwrap();
        drop(storage);
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let retained = scan(&storage, directory.path(), "account-a");
        assert_eq!(retained.events.len(), 4);
        assert_eq!(
            retained.events.iter().map(|event| event.total).sum::<u64>(),
            720
        );
    }

    #[test]
    fn deleted_legacy_cache_restores_anonymous_occurrences_without_recounting_first() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let mut anonymous = message("ignored", "2026-07-12T14:00:00Z");
        anonymous.as_object_mut().unwrap().remove("id");
        write_session(
            directory.path(),
            "legacy.jsonl",
            &[anonymous.clone(), anonymous],
        );
        let sessions = directory.path().join("agent/sessions");
        let path = sessions.join("legacy.jsonl");
        let events = parse_jsonl(&fs::read_to_string(&path).unwrap());
        storage
            .record_usage_events("omp", "", 1, &events[..1], |event| {
                super::super::log_usage::usage_event_key(&(
                    &event.session_id,
                    event.timestamp,
                    &event.model,
                ))
            })
            .unwrap();
        let mut legacy = serde_json::to_value(&events).unwrap();
        for event in legacy.as_array_mut().unwrap() {
            let event = event.as_object_mut().unwrap();
            event.remove("anonymous_occurrence");
            event.remove("speed");
            event.insert("is_fast".into(), serde_json::json!(true));
        }
        storage
            .save_log_events(
                "omp",
                &path,
                0,
                0,
                &serde_json::json!({"schema_version": 2, "events": legacy}).to_string(),
            )
            .unwrap();
        fs::remove_file(path).unwrap();
        let pricing = ModelPricing::new(
            PricingSupplement::default(),
            PricingCatalog {
                entries: HashMap::from([("gpt-test".into(), ModelRates::new(4.0, 20.0))]),
                ..PricingCatalog::default()
            },
            PricingCatalog::default(),
        );
        let outcome = scan_sessions_into(
            &storage,
            SessionSource {
                directory: &sessions,
                cache_key: "omp",
                identity_key: "",
                label: "Oh My Pi",
                allowed_models: &[],
            },
            Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap(),
            &pricing,
            None,
            None,
            &mut DailyUsageAccumulator::default(),
            &mut DailyUsageAccumulator::default(),
            &mut DailyUsageAccumulator::default(),
        )
        .unwrap();
        assert_eq!(outcome.events.len(), 2);
        assert_eq!(
            outcome.events.iter().map(|event| event.total).sum::<u64>(),
            360
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
            assert_eq!(period.tokens, 360);
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
