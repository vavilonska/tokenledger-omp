use std::{collections::HashMap, path::PathBuf, sync::Arc};

use chrono::{DateTime, Days, Local, Utc};

use crate::{
    models::UsageHistory, pricing::ModelPricing, providers::daily_usage::DailyUsageAccumulator,
    storage::Storage,
};

use super::{
    database::{has_hosted_usage, read_database, DatabaseRead},
    paths::OpenCodePaths,
    record::{CostProvenance, OpenCodeUsageEvent, UsageRecord},
    OpenCodeError,
};

const SCAN_DAYS: i64 = 33;
const LEDGER_SCHEMA_VERSION: u8 = 1;
const LEDGER_IDENTITY: &str = "local";
pub(crate) const USAGE_SOURCE_NOTE: &str =
    "From your OpenCode local database; missing costs use catalog estimates";

#[derive(Debug)]
pub(crate) struct OpenCodeUsageScan {
    pub(crate) usage: UsageHistory,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct OpenCodeUsageScanner {
    paths: OpenCodePaths,
    storage: Arc<Storage>,
}

impl OpenCodeUsageScanner {
    pub(crate) fn new(paths: OpenCodePaths, storage: Arc<Storage>) -> Self {
        Self { paths, storage }
    }

    #[cfg(test)]
    pub(crate) fn for_paths(paths: Vec<PathBuf>) -> Self {
        let data_directory = paths
            .first()
            .and_then(|path| path.parent())
            .unwrap_or_else(|| std::path::Path::new("."))
            .to_path_buf();
        let storage = Arc::new(Storage::open(&data_directory.join("ledger-test.db")).unwrap());
        Self::new(OpenCodePaths::for_data_directory(data_directory), storage)
    }

    pub(crate) fn scan(
        &self,
        now: DateTime<Utc>,
        pricing: &ModelPricing,
    ) -> Result<Option<OpenCodeUsageScan>, OpenCodeError> {
        let paths = self.paths.database_files()?;
        self.scan_paths(paths, now, pricing)
    }

    pub(crate) fn scan_paths(
        &self,
        mut paths: Vec<PathBuf>,
        now: DateTime<Utc>,
        pricing: &ModelPricing,
    ) -> Result<Option<OpenCodeUsageScan>, OpenCodeError> {
        paths.sort();
        paths.dedup();
        self.scan_sorted_paths(paths, now, pricing)
    }

    fn scan_sorted_paths(
        &self,
        paths: Vec<PathBuf>,
        now: DateTime<Utc>,
        pricing: &ModelPricing,
    ) -> Result<Option<OpenCodeUsageScan>, OpenCodeError> {
        let cutoff_ms = (now - chrono::Duration::days(SCAN_DAYS)).timestamp_millis();
        let mut events = Vec::new();
        let mut usable_databases = 0_usize;
        let mut failed_databases = 0_usize;
        for path in paths {
            match read_database(&path, cutoff_ms) {
                Ok(DatabaseRead::Missing) => {}
                Ok(DatabaseRead::Usable(database)) => {
                    usable_databases += 1;
                    events.extend(database.events);
                }
                Err(()) => failed_databases += 1,
            }
        }
        let events = deduplicate_events(events, pricing);
        self.storage
            .record_usage_events(
                "opencode",
                LEDGER_IDENTITY,
                LEDGER_SCHEMA_VERSION,
                &events,
                OpenCodeUsageEvent::event_key,
            )
            .map_err(|_| OpenCodeError::UsageStorage)?;
        let events = self
            .storage
            .load_usage_events::<OpenCodeUsageEvent>(
                "opencode",
                LEDGER_IDENTITY,
                LEDGER_SCHEMA_VERSION,
            )
            .map_err(|_| OpenCodeError::UsageStorage)?;
        if usable_databases == 0 && events.is_empty() {
            return if failed_databases == 0 {
                Ok(None)
            } else {
                Err(OpenCodeError::DatabaseUnreadable)
            };
        }
        let mut warnings = Vec::new();
        if failed_databases > 0 {
            crate::app_warn!(
                "plugin:opencode",
                "{failed_databases} local database(s) could not be read; usable sources remain"
            );
            warnings.push(
                "Some OpenCode databases could not be read; available local usage is shown.".into(),
            );
        }

        Ok(Some(OpenCodeUsageScan {
            usage: aggregate_history(
                events.into_iter().map(|event| event.into_usage(pricing)),
                now,
            ),
            warnings,
        }))
    }

    pub(crate) fn has_hosted_usage(&self) -> bool {
        match self
            .storage
            .has_usage_events("opencode", LEDGER_IDENTITY, LEDGER_SCHEMA_VERSION)
        {
            Ok(true) => return true,
            Ok(false) => {}
            Err(error) => crate::app_warn!("plugin:opencode", "usage ledger probe failed: {error}"),
        }
        let paths = match self.paths.database_files() {
            Ok(paths) => paths,
            Err(_) => {
                crate::app_warn!(
                    "plugin:opencode",
                    "usage probe could not enumerate the local data directory"
                );
                return true;
            }
        };
        paths.iter().any(|path| match has_hosted_usage(path) {
            Ok(found) => found,
            Err(()) => {
                crate::app_warn!(
                    "plugin:opencode",
                    "usage probe could not read one local database"
                );
                false
            }
        })
    }
}

fn deduplicate_events(
    events: Vec<OpenCodeUsageEvent>,
    pricing: &ModelPricing,
) -> Vec<OpenCodeUsageEvent> {
    let mut deduplicated = HashMap::<(String, String), OpenCodeUsageEvent>::new();
    for candidate in events {
        let (session, message) = candidate.key();
        match deduplicated.entry((session.to_owned(), message.to_owned())) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if candidate.is_better_than(entry.get(), pricing) {
                    entry.insert(candidate);
                }
            }
        }
    }
    deduplicated.into_values().collect()
}

fn aggregate_history(
    records: impl IntoIterator<Item = UsageRecord>,
    now: DateTime<Utc>,
) -> UsageHistory {
    let today = now.with_timezone(&Local).date_naive();
    let since = today.checked_sub_days(Days::new(30)).unwrap_or(today);
    let mut accumulator = DailyUsageAccumulator::default();
    for record in records {
        if record.timestamp > now {
            continue;
        }
        let date = record.timestamp.with_timezone(&Local).date_naive();
        if date < since {
            continue;
        }
        if let Some(cost) = record.cost {
            match record.cost_provenance {
                CostProvenance::Exact => accumulator.add_exact(
                    date,
                    record.tokens,
                    cost,
                    &record.model,
                    record.cache_usage,
                ),
                CostProvenance::Estimated => {
                    accumulator.add(date, record.tokens, cost, &record.model, record.cache_usage)
                }
            }
        } else if record.tokens > 0 {
            accumulator.add_unpriced_variant(
                date,
                record.tokens,
                &record.model,
                &record.model,
                record.cache_usage,
            );
        }
        if record.tokens > 0 && (record.incomplete_cost || record.cost.is_none()) {
            accumulator.mark_incomplete(date);
            if record.unknown_model {
                accumulator.add_unknown_model(date, &record.model);
            }
        }
    }
    accumulator.build(now, USAGE_SOURCE_NOTE)
}

#[cfg(test)]
mod unit_tests {
    use chrono::{TimeZone, Utc};

    use super::{aggregate_history, deduplicate_events};
    use crate::providers::opencode::record::{
        parse_message, CostProvenance, OpenCodeUsageEvent, UsageRecord,
    };

    fn record(tokens: u64, cost: Option<f64>, exact: bool) -> UsageRecord {
        UsageRecord {
            cache_usage: None,
            timestamp: Utc.with_ymd_and_hms(2026, 7, 18, 10, 0, 0).unwrap(),
            model: "model".into(),
            tokens,
            cost,
            cost_provenance: if exact {
                CostProvenance::Exact
            } else {
                CostProvenance::Estimated
            },
            incomplete_cost: cost.is_none(),
            unknown_model: false,
        }
    }

    #[test]
    fn duplicate_messages_choose_the_most_complete_deterministically() {
        let pricing = crate::pricing::test_bundled_pricing();
        let event = |tokens, cost: Option<f64>, model| {
            let mut value = serde_json::json!({
                "role": "assistant", "providerID": "opencode", "modelID": model,
                "tokens": {"input": tokens, "output": 0, "total": tokens}
            });
            if let Some(cost) = cost {
                value["cost"] = serde_json::json!(cost);
            }
            OpenCodeUsageEvent {
                message: parse_message(
                    "session".into(),
                    "message".into(),
                    Some(1_784_368_800_000),
                    &value,
                )
                .unwrap(),
                parts: Vec::new(),
            }
        };
        let records = deduplicate_events(
            vec![
                event(100, None, "gpt-6.1-sol"),
                event(200, Some(2.0), "gpt-6.1-sol"),
                event(300, None, "unpriced-model"),
            ],
            &pricing,
        )
        .into_iter()
        .map(|event| event.into_usage(&pricing))
        .collect::<Vec<_>>();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].tokens, 200);
        assert_eq!(records[0].cost, Some(2.0));
        assert_eq!(records[0].cost_provenance, CostProvenance::Exact);
    }

    #[test]
    fn exact_and_estimated_costs_keep_their_period_provenance() {
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let exact = aggregate_history([record(100, Some(1.0), true)], now);
        assert!(!exact.today.unwrap().cost_estimated);

        let estimated = aggregate_history([record(100, Some(1.0), false)], now);
        assert!(estimated.today.unwrap().cost_estimated);
    }
}

#[cfg(test)]
mod unpriced_regressions {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn unknown_and_partial_records_keep_tokens_and_only_known_costs() {
        let now = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
        for cost in [None, Some(0.5)] {
            let history = aggregate_history(
                [UsageRecord {
                    cache_usage: None,
                    timestamp: now,
                    model: "model".into(),
                    tokens: 110,
                    cost,
                    cost_provenance: CostProvenance::Exact,
                    incomplete_cost: true,
                    unknown_model: false,
                }],
                now,
            );
            let period = history.today.unwrap();
            assert_eq!(period.tokens, 110);
            assert_eq!(period.estimated_cost_usd, cost);
            assert!(!period.estimate_complete);
            assert!(period.unknown_models.is_empty());
            assert_eq!(history.daily[0].estimated_cost_usd, cost);
            let model = &period.model_breakdown.unwrap().models[0];
            assert_eq!(model.total_tokens, 110);
            assert_eq!(model.cost_usd, cost);
        }
    }
}

#[cfg(test)]
mod completeness_cause_regressions {
    use super::super::record::{parse_message, parse_part};
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn parsed_cost_gaps_do_not_mislabel_known_models_in_the_dto() {
        let now = Utc.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap();
        let pricing = crate::pricing::test_bundled_pricing();
        for (model, known) in [("gpt-6-astra", true), ("missing-model", false)] {
            for subtotal in [false, true] {
                let message = parse_message(
                    "s".into(),
                    "m".into(),
                    Some(now.timestamp()),
                    &serde_json::json!({
                        "role": "assistant", "providerID": "opencode", "modelID": model,
                        "tokens": { "input": 100, "output": 10, "total": 110 }, "cost": -1
                    }),
                )
                .unwrap();
                let parts = if subtotal {
                    vec![
                        parse_part(&serde_json::json!({
                            "type": "step-finish", "tokens": { "input": 50 }, "cost": 0.5
                        }))
                        .unwrap(),
                        parse_part(&serde_json::json!({
                            "type": "step-finish", "tokens": { "input": 60 }, "cost": -1
                        }))
                        .unwrap(),
                    ]
                } else {
                    Vec::new()
                };
                let record = message.into_usage(&parts, &pricing);
                assert_eq!(record.unknown_model, !known);
                let dto = serde_json::to_value(aggregate_history([record], now)).unwrap();
                assert_eq!(dto["today"]["tokens"], 110);
                assert_eq!(dto["today"]["estimateComplete"], false);
                assert_eq!(dto["last30Days"]["estimateComplete"], false);
                assert_eq!(
                    dto["today"]["estimatedCostUsd"],
                    serde_json::json!(subtotal.then_some(0.5))
                );
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
    }
}
