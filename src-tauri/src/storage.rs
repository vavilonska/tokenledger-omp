use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::{
    models::{AppSettings, DailyUsage, ProviderSnapshot, ResetCycleUsage},
    providers::CacheIdentity,
};

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("TokenLedger OMP data directory could not be created")]
    CreateDirectory(#[source] std::io::Error),
    #[error("TokenLedger OMP database is unavailable")]
    Database(#[from] rusqlite::Error),
    #[error("Cached TokenLedger OMP data is invalid")]
    InvalidCache(#[from] serde_json::Error),
    #[error("TokenLedger OMP database lock is unavailable")]
    Poisoned,
}

pub struct Storage {
    connection: Mutex<Connection>,
}

// Recovery evidence is storage-only; it is never part of the frontend DTO.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CycleRecovery {
    pub baseline_tokens: u64,
    pub covered_until: chrono::DateTime<chrono::Utc>,
    pub boundary_conflict: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StoredResetCycle {
    #[serde(flatten)]
    pub cycle: ResetCycleUsage,
    #[serde(rename = "_recovery")]
    pub recovery: CycleRecovery,
    #[serde(rename = "_exhaustedAt", skip_serializing_if = "Option::is_none")]
    pub exhausted_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub struct ProviderAccountUpdate {
    pub provider_family: String,
    pub identity_key: String,
    pub provider_id: String,
    pub payload: String,
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(StorageError::CreateDirectory)?;
        }
        let mut connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS provider_snapshots (
               provider_id TEXT PRIMARY KEY,
               payload TEXT NOT NULL,
               refreshed_at TEXT NOT NULL,
               identity_key TEXT
             );
             CREATE TABLE IF NOT EXISTS daily_usage (
               provider_id TEXT NOT NULL,
               date TEXT NOT NULL,
               tokens INTEGER NOT NULL,
               estimated_cost_usd REAL,
               estimate_complete INTEGER NOT NULL,
               PRIMARY KEY(provider_id, date)
             );
             CREATE TABLE IF NOT EXISTS log_file_cache (
               provider_id TEXT NOT NULL,
               path TEXT NOT NULL,
               size INTEGER NOT NULL,
               modified_nanos INTEGER NOT NULL,
               events_json TEXT NOT NULL,
               PRIMARY KEY(provider_id, path)
             );
             CREATE TABLE IF NOT EXISTS local_usage_events (
               provider_id TEXT NOT NULL,
               event_key TEXT NOT NULL,
               identity_key TEXT NOT NULL,
               schema_version INTEGER NOT NULL,
               event_json TEXT NOT NULL,
               PRIMARY KEY(provider_id, event_key)
             );
             CREATE INDEX IF NOT EXISTS local_usage_identity
               ON local_usage_events(provider_id, identity_key, schema_version);
             CREATE TABLE IF NOT EXISTS app_settings (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS panel_state (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               height INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS provider_environment (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               payload TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS provider_account_records (
               provider_family TEXT NOT NULL,
               identity_key TEXT NOT NULL,
               provider_id TEXT NOT NULL,
               payload TEXT NOT NULL,
               PRIMARY KEY(provider_family, identity_key),
               UNIQUE(provider_family, provider_id)
             );
             CREATE TABLE IF NOT EXISTS reset_cycle_usage (
               provider_id TEXT NOT NULL,
               identity_key TEXT NOT NULL,
               window_id TEXT NOT NULL,
               cycle_key INTEGER NOT NULL,
               payload TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(provider_id, identity_key, window_id, cycle_key)
             );",
        )?;
        {
            let transaction = connection.transaction()?;
            if !Self::has_column(&transaction, "log_file_cache", "modified_nanos")? {
                // Rebuild the legacy path-only primary key as (provider_id, path).
                transaction.execute_batch(
                    "ALTER TABLE log_file_cache RENAME TO legacy_log_file_cache;
                     CREATE TABLE log_file_cache (
                       provider_id TEXT NOT NULL,
                       path TEXT NOT NULL,
                       size INTEGER NOT NULL,
                       modified_nanos INTEGER NOT NULL,
                       events_json TEXT NOT NULL,
                       PRIMARY KEY(provider_id, path)
                     );",
                )?;
            }
            // Older releases could stop after either the rename, create, or copy.
            // The new column alone therefore does not establish migration completion.
            let legacy_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master
                 WHERE type = 'table' AND name = 'legacy_log_file_cache')",
                [],
                |row| row.get(0),
            )?;
            if legacy_exists {
                // Force fresh metadata checks while retaining parsed history whose
                // source may be gone. Already refreshed target rows take precedence.
                transaction.execute_batch(
                    "INSERT INTO log_file_cache(provider_id, path, size, modified_nanos, events_json)
                       SELECT provider_id, path, size, 0, events_json
                       FROM legacy_log_file_cache WHERE true
                       ON CONFLICT(provider_id, path) DO NOTHING;
                     DROP TABLE legacy_log_file_cache;",
                )?;
            }
            transaction.commit()?;
        }
        if !Self::has_column(&connection, "provider_snapshots", "identity_key")? {
            connection.execute(
                "ALTER TABLE provider_snapshots ADD COLUMN identity_key TEXT",
                [],
            )?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    #[cfg(test)]
    pub fn load_snapshot(
        &self,
        provider_id: &str,
    ) -> Result<Option<ProviderSnapshot>, StorageError> {
        self.load_snapshot_for_identity(provider_id, CacheIdentity::Unscoped)
    }

    pub fn load_snapshot_for_identity(
        &self,
        provider_id: &str,
        identity: CacheIdentity<'_>,
    ) -> Result<Option<ProviderSnapshot>, StorageError> {
        let connection = self.connection()?;
        let cached: Option<(String, Option<String>)> = connection
            .query_row(
                "SELECT payload, identity_key FROM provider_snapshots WHERE provider_id = ?1",
                [provider_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        cached
            .filter(|(_, cached_identity)| match identity {
                CacheIdentity::Unscoped => cached_identity.is_none(),
                CacheIdentity::Resolved(expected) => cached_identity.as_deref() == Some(expected),
                CacheIdentity::Unresolved => true,
            })
            .map(|json| {
                let mut snapshot: ProviderSnapshot = serde_json::from_str(&json.0)?;
                // Count quotas predate the unit field. The only count quota persisted by older
                // releases was request-based, so normalize it once at the cache boundary instead of
                // teaching every presentation surface to infer provider semantics.
                for quota in &mut snapshot.quotas {
                    if quota.format == crate::models::QuotaFormat::Count && quota.unit.is_none() {
                        quota.unit = Some("requests".into());
                    }
                }
                Ok(snapshot)
            })
            .transpose()
    }

    #[cfg(test)]
    pub fn save_snapshot(&self, snapshot: &ProviderSnapshot) -> Result<(), StorageError> {
        self.save_snapshot_for_identity(snapshot, None)
    }

    pub fn save_snapshot_for_identity(
        &self,
        snapshot: &ProviderSnapshot,
        identity_key: Option<&str>,
    ) -> Result<(), StorageError> {
        let payload = serde_json::to_string(snapshot)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO provider_snapshots(provider_id, payload, refreshed_at, identity_key)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_id) DO UPDATE SET
               payload = excluded.payload,
               refreshed_at = excluded.refreshed_at,
               identity_key = excluded.identity_key",
            params![
                snapshot.provider_id,
                payload,
                snapshot.refreshed_at.to_rfc3339(),
                identity_key,
            ],
        )?;
        transaction.execute(
            "DELETE FROM daily_usage WHERE provider_id = ?1",
            [&snapshot.provider_id],
        )?;
        for day in &snapshot.usage.daily {
            Self::insert_day(&transaction, &snapshot.provider_id, day)?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// Compact usage facts only; callers must never pass transcript or credential payloads.
    /// An event stays with its first observed account even when a shared log is rescanned.
    pub fn record_usage_events<T: serde::Serialize>(
        &self,
        provider_id: &str,
        identity_key: &str,
        schema_version: u8,
        events: &[T],
        event_key: impl Fn(&T) -> Result<String, serde_json::Error>,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO local_usage_events
                   (provider_id, event_key, identity_key, schema_version, event_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(provider_id, event_key) DO UPDATE SET
                   schema_version = excluded.schema_version,
                   event_json = excluded.event_json
                 WHERE local_usage_events.identity_key = excluded.identity_key
                   AND (local_usage_events.schema_version != excluded.schema_version
                     OR local_usage_events.event_json != excluded.event_json)",
            )?;
            for event in events {
                statement.execute(params![
                    provider_id,
                    event_key(event)?,
                    identity_key,
                    schema_version,
                    serde_json::to_string(event)?
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Reconcile source-attested legacy keys and their evidence in the same transaction as writes.
    #[allow(clippy::too_many_arguments)]
    pub fn record_usage_events_with_reconciliation<T>(
        &self,
        provider_id: &str,
        identity_key: &str,
        schema_version: u8,
        events: &[T],
        event_key: impl Fn(&T) -> Result<String, serde_json::Error>,
        ownership_key: impl Fn(&T) -> Result<String, serde_json::Error>,
        legacy_key: impl Fn(&T) -> Result<Option<String>, serde_json::Error>,
        reclassify: impl Fn(&T, &T) -> Option<T>,
        merge: impl Fn(&T, &T) -> T,
    ) -> Result<(), StorageError>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        {
            let mut lookup = transaction.prepare(
                "SELECT identity_key, event_json FROM local_usage_events
                 WHERE provider_id = ?1 AND event_key = ?2",
            )?;
            let mut insert = transaction.prepare(
                "INSERT INTO local_usage_events
                   (provider_id, event_key, identity_key, schema_version, event_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(provider_id, event_key) DO UPDATE SET
                   schema_version = excluded.schema_version, event_json = excluded.event_json
                 WHERE local_usage_events.identity_key = excluded.identity_key
                   AND (local_usage_events.schema_version != excluded.schema_version
                     OR local_usage_events.event_json != excluded.event_json)",
            )?;
            // Move children first: an old advisor may occupy the parent's key.
            // Every read, ownership check, transfer, deletion and parent write rolls back together.
            let mut reconciled = HashMap::new();
            let mut source_index: Option<HashMap<String, Vec<&T>>> = None;
            for event in events {
                let Some(old_key) = legacy_key(event)? else {
                    continue;
                };
                let key = event_key(event)?;
                if key == old_key {
                    continue;
                }
                let anchor = ownership_key(event)?;
                let prior: Option<(String, String)> = lookup
                    .query_row(params![provider_id, old_key], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?;
                if prior
                    .as_ref()
                    .is_some_and(|(owner, _)| owner != identity_key)
                {
                    continue;
                }
                let anchor_owner: Option<String> = transaction.query_row(
                    "SELECT identity_key FROM local_usage_events WHERE provider_id=?1 AND event_key=?2",
                    params![provider_id, anchor], |row| row.get(0),
                ).optional()?;
                if anchor_owner
                    .as_deref()
                    .is_some_and(|owner| owner != identity_key)
                {
                    continue;
                }
                let moved = if let Some((_, json)) = prior {
                    reclassify(&serde_json::from_str(&json)?, event)
                } else {
                    // A crash after parser-cache save can leave recoverable facts outside the journal.
                    // Index that same visible batch once, and apply the identical proof/ownership gate.
                    if source_index.is_none() {
                        let mut index = HashMap::<String, Vec<&T>>::new();
                        for source in events {
                            index.entry(event_key(source)?).or_default().push(source);
                        }
                        source_index = Some(index);
                    }
                    source_index
                        .as_ref()
                        .and_then(|index| index.get(&old_key))
                        .into_iter()
                        .flatten()
                        .filter_map(|source| reclassify(source, event))
                        .reduce(|left, right| merge(&left, &right))
                };
                let Some(moved) = moved else { continue };
                let target: Option<(String, String)> = lookup
                    .query_row(params![provider_id, key], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?;
                let moved = if let Some((owner, json)) = target {
                    if owner != identity_key {
                        continue;
                    }
                    merge(&serde_json::from_str(&json)?, &moved)
                } else {
                    moved
                };
                insert.execute(params![
                    provider_id,
                    key,
                    identity_key,
                    schema_version,
                    serde_json::to_string(&moved)?
                ])?;
                transaction.execute(
                    "DELETE FROM local_usage_events WHERE provider_id=?1 AND event_key=?2 AND identity_key=?3",
                    params![provider_id, old_key, identity_key],
                )?;
                reconciled.insert(old_key, event);
            }
            for event in events {
                let key = event_key(event)?;
                if reconciled
                    .get(&key)
                    .is_some_and(|source| reclassify(event, source).is_some())
                {
                    continue; // A recovered legacy cache cannot resurrect a consumed advisor key.
                }
                let anchor = ownership_key(event)?;
                if anchor != key {
                    let owner: Option<String> = transaction
                        .query_row(
                            "SELECT identity_key FROM local_usage_events
                             WHERE provider_id = ?1 AND event_key = ?2",
                            params![provider_id, anchor],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if owner.as_deref().is_some_and(|owner| owner != identity_key) {
                        continue;
                    }
                }
                let prior: Option<(String, String)> = lookup
                    .query_row(params![provider_id, key], |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                    .optional()?;
                let merged = if let Some((owner, json)) = prior {
                    if owner != identity_key {
                        continue;
                    }
                    Some(merge(&serde_json::from_str(&json)?, event))
                } else {
                    None
                };
                insert.execute(params![
                    provider_id,
                    key,
                    identity_key,
                    schema_version,
                    serde_json::to_string(merged.as_ref().unwrap_or(event))?
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_usage_events<T: serde::de::DeserializeOwned>(
        &self,
        provider_id: &str,
        identity_key: &str,
        schema_version: u8,
    ) -> Result<Vec<T>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT event_json FROM local_usage_events
             WHERE provider_id = ?1 AND identity_key = ?2 AND schema_version = ?3
             ORDER BY event_key",
        )?;
        let rows = statement
            .query_map(params![provider_id, identity_key, schema_version], |row| {
                row.get::<_, String>(0)
            })?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn has_usage_events(
        &self,
        provider_id: &str,
        identity_key: &str,
        schema_version: u8,
    ) -> Result<bool, StorageError> {
        self.connection()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM local_usage_events
             WHERE provider_id = ?1 AND identity_key = ?2 AND schema_version = ?3)",
                params![provider_id, identity_key, schema_version],
                |row| row.get(0),
            )
            .map_err(StorageError::from)
    }

    /// Legacy parsed caches have no account stamp. Only a matching persisted snapshot
    /// can establish provenance; unscoped local sources explicitly use an empty identity.
    pub fn recoverable_log_events(
        &self,
        provider_id: &str,
        identity_key: &str,
        identity_provider: &str,
    ) -> Result<Vec<(PathBuf, String)>, StorageError> {
        let connection = self.connection()?;
        if !identity_key.is_empty() {
            let prior: Option<String> = connection
                .query_row(
                    "SELECT identity_key FROM provider_snapshots WHERE provider_id = ?1",
                    [identity_provider],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            if prior.as_deref() != Some(identity_key) {
                return Ok(Vec::new());
            }
        }
        let mut statement = connection.prepare(
            "SELECT path, events_json FROM log_file_cache WHERE provider_id = ?1 ORDER BY path",
        )?;
        let rows = statement.query_map([provider_id], |row| {
            Ok((PathBuf::from(row.get::<_, String>(0)?), row.get(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn load_log_events(
        &self,
        provider_id: &str,
        path: &Path,
        size: u64,
        modified_nanos: i64,
    ) -> Result<Option<String>, StorageError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT events_json FROM log_file_cache
                 WHERE provider_id = ?1 AND path = ?2 AND size = ?3 AND modified_nanos = ?4",
                params![
                    provider_id,
                    path.to_string_lossy(),
                    size as i64,
                    modified_nanos
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(StorageError::from)
    }

    pub fn save_log_events(
        &self,
        provider_id: &str,
        path: &Path,
        size: u64,
        modified_nanos: i64,
        events_json: &str,
    ) -> Result<(), StorageError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO log_file_cache(path, size, modified_nanos, events_json, provider_id)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(provider_id, path) DO UPDATE SET
               size = excluded.size,
               modified_nanos = excluded.modified_nanos,
               events_json = excluded.events_json",
            params![
                path.to_string_lossy(),
                size as i64,
                modified_nanos,
                events_json,
                provider_id
            ],
        )?;
        Ok(())
    }

    pub fn remove_log_events(&self, provider_id: &str, path: &Path) -> Result<(), StorageError> {
        self.connection()?.execute(
            "DELETE FROM log_file_cache WHERE provider_id = ?1 AND path = ?2",
            params![provider_id, path.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn load_reset_cycles(
        &self,
        provider_id: &str,
        identity_key: &str,
        window_id: &str,
    ) -> Result<Vec<StoredResetCycle>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT payload, updated_at FROM reset_cycle_usage
             WHERE provider_id = ?1 AND identity_key = ?2 AND window_id = ?3
             ORDER BY cycle_key DESC",
        )?;
        let payloads = statement
            .query_map(params![provider_id, identity_key, window_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        payloads
            .into_iter()
            .map(|(payload, updated_at)| {
                #[derive(serde::Deserialize)]
                struct Payload {
                    #[serde(flatten)]
                    cycle: ResetCycleUsage,
                    #[serde(default, rename = "_recovery")]
                    recovery: Option<CycleRecovery>,
                    #[serde(default, rename = "_exhaustedAt")]
                    exhausted_at: Option<chrono::DateTime<chrono::Utc>>,
                }
                let payload: Payload = serde_json::from_str(&payload)?;
                let recovery = match payload.recovery {
                    Some(recovery) => recovery,
                    None => CycleRecovery {
                        baseline_tokens: payload.cycle.usage.tokens,
                        // Legacy snapshots only attest to facts existing at their last save.
                        covered_until: serde_json::from_value(serde_json::Value::String(
                            updated_at,
                        ))?,
                        boundary_conflict: false,
                    },
                };
                Ok(StoredResetCycle {
                    cycle: payload.cycle,
                    recovery,
                    exhausted_at: payload.exhausted_at,
                })
            })
            .collect()
    }

    pub fn save_reset_cycles(
        &self,
        provider_id: &str,
        identity_key: &str,
        window_id: &str,
        cycles: &[StoredResetCycle],
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for cycle in cycles {
            let payload = serde_json::to_string(cycle)?;
            let cycle_key = cycle.cycle.started_at.timestamp().div_euclid(300);
            transaction.execute(
                "INSERT INTO reset_cycle_usage(
                   provider_id, identity_key, window_id, cycle_key, payload, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(provider_id, identity_key, window_id, cycle_key) DO UPDATE SET
                   payload = excluded.payload,
                   updated_at = excluded.updated_at",
                params![
                    provider_id,
                    identity_key,
                    window_id,
                    cycle_key,
                    payload,
                    chrono::Utc::now().to_rfc3339(),
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn prune_log_events(
        &self,
        provider_id: &str,
        seen_paths: &HashSet<PathBuf>,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let cached_paths = {
            let mut statement =
                transaction.prepare("SELECT path FROM log_file_cache WHERE provider_id = ?1")?;
            let paths = statement
                .query_map([provider_id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            paths
        };
        for cached_path in cached_paths {
            if !seen_paths.contains(Path::new(&cached_path)) {
                transaction.execute(
                    "DELETE FROM log_file_cache WHERE provider_id = ?1 AND path = ?2",
                    params![provider_id, cached_path],
                )?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn load_settings(&self) -> Result<Option<AppSettings>, StorageError> {
        let connection = self.connection()?;
        let payload: Option<String> = connection
            .query_row("SELECT payload FROM app_settings WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        payload
            .map(|json| serde_json::from_str(&json).map_err(StorageError::from))
            .transpose()
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<(), StorageError> {
        self.save_settings_with_account_updates(settings, &[])
    }

    pub fn save_settings_with_account_updates(
        &self,
        settings: &AppSettings,
        account_updates: &[ProviderAccountUpdate],
    ) -> Result<(), StorageError> {
        let payload = serde_json::to_string(settings)?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for update in account_updates {
            transaction.execute(
                "INSERT INTO provider_account_records(
                   provider_family, identity_key, provider_id, payload
                 ) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(provider_family, identity_key) DO UPDATE SET
                   provider_id = excluded.provider_id,
                   payload = excluded.payload",
                params![
                    update.provider_family,
                    update.identity_key,
                    update.provider_id,
                    update.payload,
                ],
            )?;
        }
        transaction.execute(
            "INSERT INTO app_settings(id, payload) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
            [payload],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn load_provider_environment(
        &self,
    ) -> Result<Option<HashMap<String, String>>, StorageError> {
        let connection = self.connection()?;
        let payload: Option<String> = connection
            .query_row(
                "SELECT payload FROM provider_environment WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(payload.and_then(|json| serde_json::from_str(&json).ok()))
    }

    #[cfg(unix)]
    pub fn save_provider_environment(
        &self,
        environment: &HashMap<String, String>,
    ) -> Result<(), StorageError> {
        let payload = serde_json::to_string(environment)?;
        self.connection()?.execute(
            "INSERT INTO provider_environment(id, payload) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload",
            [payload],
        )?;
        Ok(())
    }

    pub fn load_provider_account_records(
        &self,
        provider_family: &str,
    ) -> Result<Vec<(String, String, String)>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT identity_key, provider_id, payload
             FROM provider_account_records
             WHERE provider_family = ?1
             ORDER BY provider_id",
        )?;
        let records = statement
            .query_map([provider_family], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)?;
        Ok(records)
    }

    pub fn load_all_provider_account_records(
        &self,
    ) -> Result<Vec<(String, String, String, String)>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT provider_family, identity_key, provider_id, payload
             FROM provider_account_records
             ORDER BY provider_family, provider_id, identity_key",
        )?;
        let records = statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)?;
        Ok(records)
    }

    pub fn load_observed_account_provider_ids(&self) -> Result<Vec<String>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT DISTINCT provider_id
             FROM provider_account_records
             ORDER BY provider_id",
        )?;
        let provider_ids = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)?;
        Ok(provider_ids)
    }

    pub fn save_provider_account_record(
        &self,
        provider_family: &str,
        identity_key: &str,
        provider_id: &str,
        payload: &str,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO provider_account_records(
               provider_family, identity_key, provider_id, payload
             ) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider_family, identity_key) DO UPDATE SET
               provider_id = excluded.provider_id,
               payload = excluded.payload",
            params![provider_family, identity_key, provider_id, payload],
        )?;
        Ok(())
    }

    pub fn load_panel_height(&self) -> Result<Option<u32>, StorageError> {
        let connection = self.connection()?;
        let height = connection
            .query_row("SELECT height FROM panel_state WHERE id = 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        Ok(height.and_then(|value| u32::try_from(value).ok()))
    }

    pub fn save_panel_height(&self, height: u32) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO panel_state(id, height) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET height = excluded.height",
            [i64::from(height)],
        )?;
        Ok(())
    }

    pub fn clear_panel_height(&self) -> Result<(), StorageError> {
        self.connection()?
            .execute("DELETE FROM panel_state WHERE id = 1", [])?;
        Ok(())
    }

    fn insert_day(
        transaction: &rusqlite::Transaction<'_>,
        provider_id: &str,
        day: &DailyUsage,
    ) -> Result<(), rusqlite::Error> {
        transaction.execute(
            "INSERT INTO daily_usage(
               provider_id, date, tokens, estimated_cost_usd, estimate_complete
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                provider_id,
                day.date,
                day.tokens as i64,
                day.estimated_cost_usd,
                day.estimate_complete as i64
            ],
        )?;
        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, StorageError> {
        self.connection.lock().map_err(|_| StorageError::Poisoned)
    }

    fn has_column(
        connection: &Connection,
        table: &str,
        column: &str,
    ) -> Result<bool, rusqlite::Error> {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(columns.iter().any(|name| name == column))
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, path::PathBuf};

    use chrono::Utc;
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::{CycleRecovery, ProviderAccountUpdate, Storage, StoredResetCycle};
    use crate::models::{
        AppSettings, DailyUsage, ModelUsageBreakdown, ModelUsageEntry, ProviderSnapshot,
        QuotaFormat, QuotaWindow, ResetCycleUsage, UsageHistory, UsagePeriod,
    };
    use crate::providers::CacheIdentity;

    #[test]
    fn reconciliation_rolls_back_rekey_if_a_later_write_fails() {
        use serde_json::{json, Value};
        let dir = tempdir().unwrap();
        let storage = Storage::open(&dir.path().join("usage.db")).unwrap();
        let old = json!({"id":"parent","tokens":12});
        let key = |event: &Value| Ok(event["id"].as_str().unwrap().to_owned());
        storage
            .record_usage_events("claude", "a", 1, std::slice::from_ref(&old), key)
            .unwrap();
        let child = json!({"id":"child","old":"parent","tokens":12});
        let invalid = json!({"id":"bad"});
        let result = storage.record_usage_events_with_reconciliation(
            "claude",
            "a",
            1,
            &[child, invalid],
            |event| {
                if event["id"] == "bad" {
                    serde_json::from_str::<String>("{")
                } else {
                    key(event)
                }
            },
            key,
            |event| Ok(event["old"].as_str().map(str::to_owned)),
            |old, new| (old["tokens"] == new["tokens"]).then(|| new.clone()),
            |_, new| new.clone(),
        );
        assert!(result.is_err());
        assert_eq!(
            storage
                .load_usage_events::<Value>("claude", "a", 1)
                .unwrap(),
            vec![old]
        );
    }

    #[test]
    fn completeness_merge_is_opt_in_and_child_ownership_cannot_cross_accounts() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
        let key = |event: &serde_json::Value| Ok(event["id"].as_str().unwrap().to_owned());
        let anchor = |event: &serde_json::Value| Ok(event["parent"].as_str().unwrap().to_owned());
        let merge = |old: &serde_json::Value, new: &serde_json::Value| {
            if old["tokens"].as_u64() >= new["tokens"].as_u64() {
                old.clone()
            } else {
                new.clone()
            }
        };
        let full = serde_json::json!({"id":"p","parent":"p","tokens":200,"cost":2.0});
        let stale = serde_json::json!({"id":"p","parent":"p","tokens":110,"cost":0.1});
        let child = serde_json::json!({"id":"c","parent":"p","tokens":12,"cost":0.000012});
        storage
            .record_usage_events("claude", "a", 1, std::slice::from_ref(&full), key)
            .unwrap();
        storage
            .record_usage_events_with_reconciliation(
                "claude",
                "a",
                1,
                &[stale],
                key,
                anchor,
                |_| Ok(None),
                |_, _| None,
                merge,
            )
            .unwrap();
        storage
            .record_usage_events_with_reconciliation(
                "claude",
                "b",
                1,
                &[child],
                key,
                anchor,
                |_| Ok(None),
                |_, _| None,
                merge,
            )
            .unwrap();
        let retained: Vec<serde_json::Value> = storage.load_usage_events("claude", "a", 1).unwrap();
        assert_eq!(retained, vec![full]);
        assert!(storage
            .load_usage_events::<serde_json::Value>("claude", "b", 1)
            .unwrap()
            .is_empty());
        let corrected = serde_json::json!({"id":"p","parent":"p","tokens":200,"cost":0.5});
        storage
            .record_usage_events("claude", "a", 1, std::slice::from_ref(&corrected), key)
            .unwrap();
        let repriced: Vec<serde_json::Value> = storage.load_usage_events("claude", "a", 1).unwrap();
        assert_eq!(repriced, vec![corrected]);
    }

    #[test]
    fn snapshot_round_trip_contains_no_credentials() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let snapshot = ProviderSnapshot {
            provider_id: "codex".into(),
            plan: Some("Plus".into()),
            quotas: Vec::new(),
            value_metrics: Vec::new(),
            status_metrics: Vec::new(),
            notices: Vec::new(),
            usage: UsageHistory {
                today: Some(UsagePeriod {
                    tokens: 42,
                    estimated_cost_usd: Some(0.12),
                    estimated_limit_usd: None,
                    quota_used_percent: None,
                    cost_estimated: true,
                    estimate_complete: true,
                    model_breakdown: Some(ModelUsageBreakdown {
                        models: vec![ModelUsageEntry {
                            model: "gpt-5.4".into(),
                            total_tokens: 42,
                            cache_read_tokens: None,
                            prompt_tokens: None,
                            cost_usd: Some(0.12),
                            variants: None,
                        }],
                        source_note: "From your Codex logs (estimated)".into(),
                    }),
                    unknown_models: Vec::new(),
                }),
                daily: vec![DailyUsage {
                    date: "2026-07-10".into(),
                    tokens: 42,
                    estimated_cost_usd: Some(0.12),
                    estimate_complete: true,
                }],
                ..UsageHistory::default()
            },
            warnings: Vec::new(),
            refreshed_at: Utc::now(),
        };

        storage.save_snapshot(&snapshot).unwrap();

        assert_eq!(storage.load_snapshot("codex").unwrap(), Some(snapshot));
        let bytes = std::fs::read(directory.path().join("tokenledger-omp.db")).unwrap();
        let database = String::from_utf8_lossy(&bytes);
        assert!(!database.contains("access_token"));
        assert!(!database.contains("refresh_token"));
    }

    #[test]
    fn reset_cycle_history_is_account_scoped_and_round_trips() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let cycle = ResetCycleUsage {
            started_at: chrono::DateTime::parse_from_rfc3339("2026-08-30T06:36:48Z")
                .unwrap()
                .to_utc(),
            ended_at: None,
            scheduled_reset_at: chrono::DateTime::parse_from_rfc3339("2026-09-06T06:36:48Z")
                .unwrap()
                .to_utc(),
            usage: UsagePeriod {
                tokens: 12,
                estimated_cost_usd: Some(1.5),
                estimated_limit_usd: Some(7.5),
                quota_used_percent: Some(20.0),
                cost_estimated: true,
                estimate_complete: true,
                model_breakdown: None,
                unknown_models: Vec::new(),
            },
            model_usage: Some(vec![
                crate::models::CycleModelUsage {
                    model: "gpt-6-astra".into(),
                    total_tokens: 8,
                    request_count: Some(2),
                    cost_usd: Some(1.5),
                    cost_complete: true,
                },
                crate::models::CycleModelUsage {
                    model: "Unattributed".into(),
                    total_tokens: 4,
                    request_count: None,
                    cost_usd: None,
                    cost_complete: false,
                },
            ]),
        };
        let cycle = StoredResetCycle {
            exhausted_at: Some(cycle.started_at + chrono::Duration::hours(1)),
            recovery: CycleRecovery {
                baseline_tokens: cycle.usage.tokens,
                covered_until: cycle.started_at,
                boundary_conflict: false,
            },
            cycle,
        };
        storage
            .save_reset_cycles("codex", "account-a", "weekly", std::slice::from_ref(&cycle))
            .unwrap();

        assert_eq!(
            storage
                .load_reset_cycles("codex", "account-a", "weekly")
                .unwrap(),
            vec![cycle.clone()]
        );
        assert!(storage
            .load_reset_cycles("codex", "account-b", "weekly")
            .unwrap()
            .is_empty());

        // Token-only model details from older caches retain their counts, but
        // cannot claim a complete zero-dollar estimate.
        let mut token_only = serde_json::to_value(&cycle.cycle).unwrap();
        for model in token_only["modelUsage"].as_array_mut().unwrap() {
            model.as_object_mut().unwrap().remove("costUsd");
            model.as_object_mut().unwrap().remove("costComplete");
        }
        storage
            .connection()
            .unwrap()
            .execute(
                "UPDATE reset_cycle_usage SET payload = ?1
                 WHERE provider_id = 'codex' AND identity_key = 'account-a'",
                rusqlite::params![serde_json::to_string(&token_only).unwrap()],
            )
            .unwrap();
        let loaded = storage
            .load_reset_cycles("codex", "account-a", "weekly")
            .unwrap();
        let models = loaded[0].cycle.model_usage.as_ref().unwrap();
        assert_eq!(loaded[0].exhausted_at, None);
        assert_eq!(models[0].total_tokens, 8);
        assert_eq!(models[0].request_count, Some(2));
        assert_eq!(models[1].total_tokens, 4);
        assert_eq!(models[1].request_count, None);
        assert!(models
            .iter()
            .all(|model| model.cost_usd.is_none() && !model.cost_complete));
        assert_eq!(loaded[0].cycle.usage.tokens, 12);
        assert_eq!(loaded[0].recovery.baseline_tokens, 12);

        // A pre-detail snapshot remains unknown at the persistence boundary.
        let mut legacy = serde_json::to_value(&cycle.cycle).unwrap();
        legacy.as_object_mut().unwrap().remove("modelUsage");
        storage
            .connection()
            .unwrap()
            .execute(
                "UPDATE reset_cycle_usage SET payload = ?1
                 WHERE provider_id = 'codex' AND identity_key = 'account-a'",
                rusqlite::params![serde_json::to_string(&legacy).unwrap()],
            )
            .unwrap();
        let loaded = storage
            .load_reset_cycles("codex", "account-a", "weekly")
            .unwrap();
        assert_eq!(loaded[0].cycle.model_usage, None);
        assert_eq!(loaded[0].cycle.usage.tokens, 12);
        assert_eq!(loaded[0].recovery.baseline_tokens, 12);
    }

    #[test]
    fn account_scoped_snapshot_is_visible_only_to_the_same_identity() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let snapshot = ProviderSnapshot {
            provider_id: "claude".into(),
            plan: Some("Max".into()),
            quotas: Vec::new(),
            value_metrics: Vec::new(),
            status_metrics: Vec::new(),
            notices: Vec::new(),
            usage: UsageHistory::default(),
            warnings: Vec::new(),
            refreshed_at: Utc::now(),
        };

        storage
            .save_snapshot_for_identity(&snapshot, Some("identity-a"))
            .unwrap();

        assert_eq!(
            storage
                .load_snapshot_for_identity("claude", CacheIdentity::Resolved("identity-a"))
                .unwrap(),
            Some(snapshot.clone())
        );
        assert!(storage
            .load_snapshot_for_identity("claude", CacheIdentity::Resolved("identity-b"))
            .unwrap()
            .is_none());
        assert!(storage.load_snapshot("claude").unwrap().is_none());
        assert_eq!(
            storage
                .load_snapshot_for_identity("claude", CacheIdentity::Unresolved)
                .unwrap(),
            Some(snapshot)
        );
    }

    #[test]
    fn provider_account_records_round_trip_by_family() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();

        storage
            .save_provider_account_record(
                "claude",
                "identity-a",
                "claude@12345678",
                r#"{"displayName":"Claude — Work"}"#,
            )
            .unwrap();

        assert_eq!(
            storage.load_provider_account_records("claude").unwrap(),
            [(
                "identity-a".to_owned(),
                "claude@12345678".to_owned(),
                r#"{"displayName":"Claude — Work"}"#.to_owned(),
            )]
        );
        assert!(storage
            .load_provider_account_records("codex")
            .unwrap()
            .is_empty());
        assert_eq!(
            storage.load_observed_account_provider_ids().unwrap(),
            ["claude@12345678"]
        );
    }

    #[test]
    fn account_records_and_settings_roll_back_together() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let original = AppSettings {
            theme: crate::models::ThemePreference::Light,
            ..AppSettings::default()
        };
        storage.save_settings(&original).unwrap();
        storage
            .save_provider_account_record("codex", "identity-a", "codex", r#"{"name":"old"}"#)
            .unwrap();
        let changed = AppSettings {
            theme: crate::models::ThemePreference::Dark,
            ..original.clone()
        };
        let updates = [
            ProviderAccountUpdate {
                provider_family: "codex".into(),
                identity_key: "identity-a".into(),
                provider_id: "codex".into(),
                payload: r#"{"name":"changed"}"#.into(),
            },
            ProviderAccountUpdate {
                provider_family: "codex".into(),
                identity_key: "identity-b".into(),
                provider_id: "codex".into(),
                payload: "{}".into(),
            },
        ];

        assert!(storage
            .save_settings_with_account_updates(&changed, &updates)
            .is_err());
        assert_eq!(storage.load_settings().unwrap(), Some(original));
        let records = storage.load_provider_account_records("codex").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].2, r#"{"name":"old"}"#);
    }

    #[test]
    fn legacy_snapshot_table_gains_the_account_identity_column() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("tokenledger-omp.db");
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                "CREATE TABLE provider_snapshots (
                   provider_id TEXT PRIMARY KEY,
                   payload TEXT NOT NULL,
                   refreshed_at TEXT NOT NULL
                 );",
            )
            .unwrap();
        drop(legacy);

        let storage = Storage::open(&path).unwrap();
        let connection = storage.connection().unwrap();

        assert!(Storage::has_column(&connection, "provider_snapshots", "identity_key").unwrap());
    }

    #[test]
    fn legacy_count_quota_cache_is_normalized_at_load_boundary() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let snapshot = ProviderSnapshot {
            provider_id: "cursor".into(),
            plan: None,
            quotas: vec![QuotaWindow {
                id: "requests".into(),
                label: "Requests".into(),
                used_percent: 25.0,
                resets_at: None,
                period_seconds: 2_592_000,
                format: QuotaFormat::Count,
                used_value: Some(25.0),
                limit_value: Some(100.0),
                unit: None,
                estimated: false,
                source_note: None,
            }],
            value_metrics: Vec::new(),
            status_metrics: Vec::new(),
            notices: Vec::new(),
            usage: UsageHistory::default(),
            warnings: Vec::new(),
            refreshed_at: Utc::now(),
        };

        storage.save_snapshot(&snapshot).unwrap();

        assert_eq!(
            storage.load_snapshot("cursor").unwrap().unwrap().quotas[0].unit,
            Some("requests".into())
        );
    }

    #[test]
    fn settings_round_trip_uses_the_same_disk_database() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let settings = AppSettings {
            always_show_pacing: true,
            ..AppSettings::default()
        };
        storage.save_settings(&settings).unwrap();
        assert_eq!(storage.load_settings().unwrap(), Some(settings));
    }

    #[test]
    fn panel_height_round_trip_is_independent_from_app_settings() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let settings = AppSettings::default();
        storage.save_settings(&settings).unwrap();

        assert_eq!(storage.load_panel_height().unwrap(), None);
        storage.save_panel_height(734).unwrap();

        assert_eq!(storage.load_panel_height().unwrap(), Some(734));
        assert_eq!(storage.load_settings().unwrap(), Some(settings));

        storage.clear_panel_height().unwrap();
        assert_eq!(storage.load_panel_height().unwrap(), None);
    }

    #[test]
    fn log_cache_pruning_is_scoped_to_a_provider() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let codex_old = PathBuf::from("/logs/codex-old.jsonl");
        let codex_current = PathBuf::from("/logs/codex-current.jsonl");
        let claude_current = PathBuf::from("/logs/claude-current.jsonl");
        for (provider, path) in [
            ("codex", &codex_old),
            ("codex", &codex_current),
            ("claude", &claude_current),
        ] {
            storage
                .save_log_events(provider, path, 10, 20, "[]")
                .unwrap();
        }

        storage
            .prune_log_events("codex", &HashSet::from([codex_current.clone()]))
            .unwrap();

        assert_eq!(
            storage
                .load_log_events("codex", &codex_old, 10, 20)
                .unwrap(),
            None
        );
        assert_eq!(
            storage
                .load_log_events("codex", &codex_current, 10, 20)
                .unwrap(),
            Some("[]".to_owned())
        );
        assert_eq!(
            storage
                .load_log_events("claude", &claude_current, 10, 20)
                .unwrap(),
            Some("[]".to_owned())
        );
    }

    #[test]
    fn providers_can_cache_the_same_path_independently() {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("tokenledger-omp.db")).unwrap();
        let shared = PathBuf::from("/synced/session.jsonl");
        storage
            .save_log_events("claude", &shared, 10, 20, "claude")
            .unwrap();
        storage
            .save_log_events("codex", &shared, 10, 20, "codex")
            .unwrap();

        assert_eq!(
            storage.load_log_events("claude", &shared, 10, 20).unwrap(),
            Some("claude".to_owned())
        );
        assert_eq!(
            storage.load_log_events("codex", &shared, 10, 20).unwrap(),
            Some("codex".to_owned())
        );
    }

    fn log_cache_migration_fixture(path: &std::path::Path, stage: &str) -> Connection {
        let connection = Connection::open(path).unwrap();
        if stage == "fresh" {
            return connection;
        }
        connection
            .execute_batch(
                "CREATE TABLE log_file_cache (
                   path TEXT PRIMARY KEY,
                   size INTEGER NOT NULL,
                   modified_millis INTEGER NOT NULL,
                   events_json TEXT NOT NULL,
                   provider_id TEXT NOT NULL DEFAULT ''
                 );
                 INSERT INTO log_file_cache VALUES
                   ('old.jsonl', 10, 20, '[]', 'codex'),
                   ('shared.jsonl', 11, 21, '[1]', 'codex');",
            )
            .unwrap();
        if stage == "old" {
            return connection;
        }
        connection
            .execute_batch("ALTER TABLE log_file_cache RENAME TO legacy_log_file_cache;")
            .unwrap();
        if stage == "renamed" {
            return connection;
        }
        connection
            .execute_batch(
                "CREATE TABLE log_file_cache (
                   provider_id TEXT NOT NULL,
                   path TEXT NOT NULL,
                   size INTEGER NOT NULL,
                   modified_nanos INTEGER NOT NULL,
                   events_json TEXT NOT NULL,
                   PRIMARY KEY(provider_id, path)
                 );",
            )
            .unwrap();
        if stage == "partial" {
            connection
                .execute_batch(
                    "INSERT INTO log_file_cache VALUES
                       ('codex', 'old.jsonl', 10, 0, '[]'),
                       ('codex', 'shared.jsonl', 99, 123, '[2]'),
                       ('codex', 'new-only.jsonl', 12, 456, '[3]'),
                       ('claude', 'shared.jsonl', 21, 321, '[4]');",
                )
                .unwrap();
        } else if stage == "copied" || stage == "complete" {
            connection
                .execute_batch(
                    "INSERT INTO log_file_cache
                     SELECT provider_id, path, size, 0, events_json FROM legacy_log_file_cache;",
                )
                .unwrap();
            if stage == "complete" {
                connection
                    .execute_batch("DROP TABLE legacy_log_file_cache;")
                    .unwrap();
            }
        }
        connection
    }

    #[test]
    fn log_cache_migration_recovers_every_persisted_stage_and_reopens_idempotently() {
        for stage in [
            "fresh", "old", "renamed", "created", "partial", "copied", "complete",
        ] {
            let directory = tempdir().unwrap();
            let path = directory.path().join("migration.db");
            drop(log_cache_migration_fixture(&path, stage));
            let expected = match stage {
                "fresh" => vec![],
                "partial" => vec![
                    ("claude", "shared.jsonl", 21, 321, "[4]"),
                    ("codex", "new-only.jsonl", 12, 456, "[3]"),
                    ("codex", "old.jsonl", 10, 0, "[]"),
                    ("codex", "shared.jsonl", 99, 123, "[2]"),
                ],
                _ => vec![
                    ("codex", "old.jsonl", 10, 0, "[]"),
                    ("codex", "shared.jsonl", 11, 0, "[1]"),
                ],
            };
            for _ in 0..2 {
                let storage = Storage::open(&path).unwrap();
                let connection = storage.connection().unwrap();
                let rows = connection
                    .prepare(
                        "SELECT provider_id, path, size, modified_nanos, events_json
                         FROM log_file_cache ORDER BY provider_id, path",
                    )
                    .unwrap()
                    .query_map([], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let expected_rows: Vec<_> = expected
                    .iter()
                    .map(|(provider, path, size, modified, events)| {
                        (
                            provider.to_string(),
                            path.to_string(),
                            *size,
                            *modified,
                            events.to_string(),
                        )
                    })
                    .collect();
                assert_eq!(rows, expected_rows, "{stage}");
                assert!(
                    Storage::has_column(&connection, "log_file_cache", "modified_nanos").unwrap()
                );
                assert!(
                    !Storage::has_column(&connection, "log_file_cache", "modified_millis").unwrap()
                );
                let legacy_exists: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master
                         WHERE type = 'table' AND name = 'legacy_log_file_cache')",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert!(!legacy_exists, "{stage}");
                let version: i64 = connection
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(version, 0);
                drop(connection);
                let recovered: Vec<_> = expected
                    .iter()
                    .filter(|(provider, ..)| *provider == "codex")
                    .map(|(_, path, _, _, events)| (PathBuf::from(*path), events.to_string()))
                    .collect();
                assert_eq!(
                    storage
                        .recoverable_log_events("codex", "", "codex")
                        .unwrap(),
                    recovered
                );
                assert_eq!(
                    storage
                        .load_log_events("codex", std::path::Path::new("old.jsonl"), 10, 20)
                        .unwrap(),
                    None
                );
            }
            // The old path-only primary key and millisecond NOT NULL constraint
            // must both be gone, including when upgrading an untouched old table.
            let storage = Storage::open(&path).unwrap();
            let shared = std::path::Path::new("shared.jsonl");
            for provider in ["codex", "claude"] {
                storage
                    .save_log_events(provider, shared, 30, 40, provider)
                    .unwrap();
            }
            drop(storage);
            let storage = Storage::open(&path).unwrap();
            for provider in ["codex", "claude"] {
                assert_eq!(
                    storage.load_log_events(provider, shared, 30, 40).unwrap(),
                    Some(provider.to_owned())
                );
            }
        }
    }

    #[test]
    fn log_cache_migration_failure_rolls_back_and_can_recover_on_next_open() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("migration.db");
        let connection = log_cache_migration_fixture(&path, "partial");
        connection
            .execute_batch(
                "DELETE FROM log_file_cache WHERE path = 'old.jsonl';
                 CREATE TRIGGER interrupt_cache_recovery BEFORE INSERT ON log_file_cache
                 WHEN NEW.path = 'shared.jsonl'
                 BEGIN SELECT RAISE(ABORT, 'fixture migration interruption'); END;",
            )
            .unwrap();
        drop(connection);
        assert!(Storage::open(&path).is_err());
        let connection = Connection::open(&path).unwrap();
        let counts: (i64, i64, i64) = connection
            .query_row(
                "SELECT (SELECT COUNT(*) FROM legacy_log_file_cache),
                        (SELECT COUNT(*) FROM log_file_cache),
                        (SELECT COUNT(*) FROM log_file_cache WHERE path = 'old.jsonl')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (2, 3, 0));
        connection
            .execute_batch("DROP TRIGGER interrupt_cache_recovery;")
            .unwrap();
        drop(connection);
        let storage = Storage::open(&path).unwrap();
        assert_eq!(
            storage
                .recoverable_log_events("codex", "", "codex")
                .unwrap(),
            vec![
                (PathBuf::from("new-only.jsonl"), "[3]".to_owned()),
                (PathBuf::from("old.jsonl"), "[]".to_owned()),
                (PathBuf::from("shared.jsonl"), "[2]".to_owned()),
            ]
        );
    }
}
