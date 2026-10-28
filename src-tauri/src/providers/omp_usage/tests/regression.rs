mod forks;

use chrono::{DateTime, Days, Local};
use serde_json::{json, Value};

use super::*;
use crate::{
    models::UsageHistory,
    pricing::RequestSpeed,
    providers::{
        log_usage::{usage_event_key, LogFileFingerprint},
        omp_usage::session::SessionUsageEvent,
    },
};

fn auxiliary(id: &str, input: u64, output: u64) -> Value {
    json!({"type":"model_usage", "id":id, "parentId":"fast", "purpose":"title",
        "timestamp":"2026-07-12T14:00:00Z", "provider":"openai-codex", "model":"gpt-6.1-sol",
        "usage":{"input":input,"output":output,"cacheRead":0,"cacheWrite":0,"reasoningTokens":50,
            "cost":{"input":input as f64 * 2.0 / 1_000_000.0,
                "output":output as f64 * 10.0 / 1_000_000.0,
                "total":(input as f64 * 2.0 + output as f64 * 10.0) / 1_000_000.0}}})
}

fn source_scan(
    storage: &Storage,
    home: &std::path::Path,
    identity: &str,
    pricing: &ModelPricing,
) -> (OmpScanOutcome, [UsageHistory; 3]) {
    let now = Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap();
    source_scan_at(storage, home, identity, pricing, now)
}

fn source_scan_at(
    storage: &Storage,
    home: &std::path::Path,
    identity: &str,
    pricing: &ModelPricing,
    now: DateTime<Utc>,
) -> (OmpScanOutcome, [UsageHistory; 3]) {
    let mut daily = DailyUsageAccumulator::default();
    let mut session = DailyUsageAccumulator::default();
    let mut weekly = DailyUsageAccumulator::default();
    let outcome = scan_sessions_into(
        storage,
        SessionSource {
            directory: &home.join("agent/sessions"),
            cache_key: "omp",
            identity_key: identity,
            label: "Oh My Pi",
            allowed_models: &[],
        },
        now,
        pricing,
        Some(now - chrono::Duration::hours(5)),
        Some(now - chrono::Duration::days(7)),
        &mut daily,
        &mut session,
        &mut weekly,
    )
    .unwrap();
    (
        outcome,
        [
            daily.build(now, "test"),
            session.build(now, "test"),
            weekly.build(now, "test"),
        ],
    )
}

fn legacy_values(events: &[SessionUsageEvent]) -> Value {
    let mut values = serde_json::to_value(events).unwrap();
    for value in values.as_array_mut().unwrap() {
        let value = value.as_object_mut().unwrap();
        for key in [
            "speed",
            "speed_evidence",
            "entry_kind",
            "purpose",
            "recorded_cost",
            "inherited_from_fork",
            "cache_usage",
        ] {
            value.remove(key);
        }
        value.insert("is_fast".into(), json!(true));
    }
    values
}

fn entry_key(value: &Value) -> Result<String, serde_json::Error> {
    let event: SessionUsageEvent = serde_json::from_value(value.clone())?;
    usage_event_key(&(
        "entry",
        event.id.as_deref().unwrap(),
        event.timestamp,
        &event.model,
    ))
}

#[test]
fn terra_schema_eight_reparse_corrects_retained_facts_and_deleted_source_periods() {
    for invalid_tier in [false, true] {
        let directory = tempdir().unwrap();
        let database = directory.path().join("terra.db");
        let storage = Storage::open(&database).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 8, 0, 0).unwrap();
        let calls: Vec<_> = [("standard", 1.0), ("priority", 2.5)]
            .into_iter()
            .map(|(id, multiplier)| {
                let mut call = json!({"type":"model_usage","id":id,
                    "timestamp":"2026-10-28T07:28:38.096Z",
                    "purpose":"task-completion-probe","provider":"openai-codex",
                    "model":"gpt-6.1-terra",
                    "usage":{"input":549,"output":31,"cacheRead":60544,"cacheWrite":0,
                        "totalTokens":61124,
                        "cost":{"input":0.000549 * multiplier,"output":0.000155 * multiplier,
                            "cacheRead":0.0030272 * multiplier,"cacheWrite":0,
                            "total":0.0037312 * multiplier}}});
                if invalid_tier {
                    call["serviceTier"] = json!("invalid-tier");
                }
                call
            })
            .collect();
        write_session(directory.path(), "terra.jsonl", &calls);
        let source = directory.path().join("agent/sessions/terra.jsonl");
        let mut old =
            serde_json::to_value(parse_jsonl(&fs::read_to_string(&source).unwrap())).unwrap();
        for event in old.as_array_mut().unwrap() {
            event["speed"] = Value::Null;
            event["speed_evidence"] = Value::Null;
        }
        storage
            .record_usage_events("omp", "account-a", 1, old.as_array().unwrap(), entry_key)
            .unwrap();
        let fingerprint =
            LogFileFingerprint::from_metadata(&fs::metadata(&source).unwrap()).unwrap();
        storage
            .save_log_events(
                "omp",
                &source,
                fingerprint.size,
                fingerprint.modified_nanos,
                &json!({"schema_version":8,"events":old}).to_string(),
            )
            .unwrap();
        let mut snapshots = Vec::new();
        for credits in [false, true] {
            let mut pricing = crate::pricing::test_bundled_pricing();
            pricing.codex_credit_mode = credits;
            let (outcome, histories) =
                source_scan_at(&storage, directory.path(), "account-a", &pricing, now);
            assert_eq!(outcome.events.len(), 2, "reparse must preserve event keys");
            for history in &histories {
                let period = history.today.as_ref().unwrap();
                assert_eq!(period.tokens, 122248);
                assert_eq!(period.estimate_complete, !invalid_tier);
                assert!(period.unknown_models.is_empty());
                if !invalid_tier {
                    assert!((period.estimated_cost_usd.unwrap() - 0.0111936).abs() < 1e-12);
                }
            }
            snapshots.push(serde_json::to_value(&histories).unwrap());
        }
        let retained: Vec<SessionUsageEvent> =
            storage.load_usage_events("omp", "account-a", 1).unwrap();
        assert_eq!(retained.len(), 2);
        for event in &retained {
            assert_eq!(
                event.speed,
                if invalid_tier {
                    None
                } else if event.id.as_deref() == Some("priority") {
                    Some(RequestSpeed::Fast)
                } else {
                    Some(RequestSpeed::Standard)
                }
            );
        }
        fs::remove_file(source).unwrap();
        drop(storage);
        let reopened = Storage::open(&database).unwrap();
        for (index, credits) in [false, true].into_iter().enumerate() {
            let mut pricing = crate::pricing::test_bundled_pricing();
            pricing.codex_credit_mode = credits;
            let (outcome, histories) =
                source_scan_at(&reopened, directory.path(), "account-a", &pricing, now);
            assert_eq!(outcome.events.len(), 2);
            assert_eq!(serde_json::to_value(&histories).unwrap(), snapshots[index]);
            assert!(
                !source_scan_at(&reopened, directory.path(), "account-b", &pricing, now)
                    .0
                    .included
            );
        }
    }
}

#[test]
fn sparse_orchestration_cache_reparses_schema_five_and_survives_sqlite_retention() {
    let pricing = crate::pricing::test_bundled_pricing();
    let cases = [
        (
            "input-output",
            json!({"input":3,"output":3}),
            None,
            Some((40, 163)),
        ),
        ("output-only", json!({"output":3000}), None, Some((40, 160))),
        ("empty-object", json!({}), None, Some((40, 160))),
        ("read-only", json!({"cacheRead":5}), None, Some((45, 165))),
        ("invalid-input", json!({"input":"3"}), None, None),
        ("invalid-read", json!({"cacheRead":-1}), None, None),
        ("not-object", Value::Null, None, None),
        ("missing-input", json!({}), Some("input"), None),
        ("missing-read", json!({}), Some("cacheRead"), None),
        ("missing-write", json!({}), Some("cacheWrite"), None),
    ];
    for (name, orchestration, missing, expected) in cases {
        let directory = tempdir().unwrap();
        let database = directory.path().join("sparse.db");
        let storage = Storage::open(&database).unwrap();
        let mut entry = json!({"type":"model_usage","id":name,
            "timestamp":"2026-07-12T14:00:00Z","provider":"openai-codex",
            "model":"gpt-6.1-sol","purpose":"title",
            "usage":{"input":100,"cacheRead":40,"cacheWrite":20,"cacheWrite1h":10,
                "output":9000,"orchestration":orchestration}});
        if let Some(missing) = missing {
            entry["usage"].as_object_mut().unwrap().remove(missing);
        }
        write_session(directory.path(), "sparse.jsonl", &[entry]);
        let source = directory.path().join("agent/sessions/sparse.jsonl");
        let events = parse_jsonl(&fs::read_to_string(&source).unwrap());
        let mut legacy = serde_json::to_value(&events).unwrap();
        for event in legacy.as_array_mut().unwrap() {
            event["cache_usage"] = Value::Null;
        }
        storage
            .record_usage_events(
                "omp",
                "sparse-account",
                1,
                legacy.as_array().unwrap(),
                entry_key,
            )
            .unwrap();
        let fingerprint =
            LogFileFingerprint::from_metadata(&fs::metadata(&source).unwrap()).unwrap();
        storage
            .save_log_events(
                "omp",
                &source,
                fingerprint.size,
                fingerprint.modified_nanos,
                &json!({"schema_version":5,"events":legacy}).to_string(),
            )
            .unwrap();
        let (outcome, histories) =
            source_scan(&storage, directory.path(), "sparse-account", &pricing);
        assert_eq!(
            outcome.events.len(),
            1,
            "{name}: reparse must not duplicate retained calls"
        );
        for history in &histories {
            let period = history.today.as_ref().unwrap();
            let model = &period.model_breakdown.as_ref().unwrap().models[0];
            assert_eq!(
                model.cache_read_tokens.zip(model.prompt_tokens),
                expected,
                "{name}"
            );
            for variant in model.variants.as_deref().unwrap_or_default() {
                assert_eq!(
                    variant.cache_read_tokens.zip(variant.prompt_tokens),
                    expected,
                    "{name}"
                );
            }
        }
        let dto = serde_json::to_value(&histories[0]).unwrap();
        let decoded: UsageHistory = serde_json::from_value(dto.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), dto);
        drop(storage);
        fs::remove_file(&source).unwrap();
        let reopened = Storage::open(&database).unwrap();
        let (retained, histories) =
            source_scan(&reopened, directory.path(), "sparse-account", &pricing);
        assert_eq!(
            retained.events.len(),
            1,
            "{name}: durable schema remains readable"
        );
        assert_eq!(
            serde_json::to_value(&histories[0]).unwrap(),
            dto,
            "{name}: deleted source"
        );
    }
}

#[test]
fn real_scanner_cache_buckets_include_orchestration_once_and_survive_retention() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
    let first = json!({"type":"model_usage","id":"cache-a","timestamp":"2026-07-12T14:00:00Z",
        "provider":"openai-codex","model":"gpt-6.1-sol","purpose":"title",
        "usage":{"input":100,"cacheRead":40,"cacheWrite":20,"cacheWrite1h":10,"output":900,
            "orchestration":{"input":10,"cacheRead":5,"output":20}}});
    let second = json!({"type":"model_usage","id":"cache-b","timestamp":"2026-07-11T14:00:00Z",
        "provider":"openai-codex","model":"gpt-6.1-sol","purpose":"title",
        "usage":{"input":900,"cacheRead":0,"cacheWrite":0,"output":1}});
    write_session(
        directory.path(),
        "cache.jsonl",
        &[first.clone(), first, second],
    );
    let pricing = crate::pricing::test_bundled_pricing();
    for retained in [false, true] {
        if retained {
            fs::remove_file(directory.path().join("agent/sessions/cache.jsonl")).unwrap();
        }
        let (outcome, histories) =
            source_scan(&storage, directory.path(), "cache-account", &pricing);
        assert_eq!(outcome.events.len(), 2);
        let daily = &histories[0];
        let today = daily.today.as_ref().unwrap();
        assert!(
            !today.estimate_complete,
            "unknown request speed must not hide cache usage"
        );
        let models = &today.model_breakdown.as_ref().unwrap().models;
        assert_eq!(
            (models[0].cache_read_tokens, models[0].prompt_tokens),
            (Some(45), Some(175))
        );
        let yesterday = &daily
            .yesterday
            .as_ref()
            .unwrap()
            .model_breakdown
            .as_ref()
            .unwrap()
            .models;
        assert_eq!(
            (yesterday[0].cache_read_tokens, yesterday[0].prompt_tokens),
            (Some(0), Some(900))
        );
        let models = &daily
            .last_30_days
            .as_ref()
            .unwrap()
            .model_breakdown
            .as_ref()
            .unwrap()
            .models;
        assert_eq!(
            (models[0].cache_read_tokens, models[0].prompt_tokens),
            (Some(45), Some(1_075))
        );
    }
}

#[test]
fn scanner_does_not_invent_cache_zeros_from_missing_buckets_or_deleted_legacy_sources() {
    let pricing = crate::pricing::test_bundled_pricing();
    for missing in ["input", "cacheRead", "cacheWrite"] {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let mut event = auxiliary("cache-missing", 100, 1);
        event["usage"].as_object_mut().unwrap().remove(missing);
        write_session(directory.path(), "missing.jsonl", &[event]);
        let (_, histories) = source_scan(&storage, directory.path(), "cache-account", &pricing);
        let model = &histories[0]
            .today
            .as_ref()
            .unwrap()
            .model_breakdown
            .as_ref()
            .unwrap()
            .models[0];
        assert_eq!(model.cache_read_tokens, None, "{missing}");
        assert_eq!(model.prompt_tokens, None, "{missing}");
    }
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
    let raw = format!(
        "{{\"type\":\"session\",\"id\":\"legacy-session\"}}\n{}",
        auxiliary("legacy-cache", 100, 1)
    );
    let events = parse_jsonl(&raw);
    let mut legacy = serde_json::to_value(&events[0]).unwrap();
    legacy.as_object_mut().unwrap().remove("cache_usage");
    storage
        .record_usage_events("omp", "cache-account", 1, &[legacy], entry_key)
        .unwrap();
    let (_, histories) = source_scan(&storage, directory.path(), "cache-account", &pricing);
    let model = &histories[0]
        .today
        .as_ref()
        .unwrap()
        .model_breakdown
        .as_ref()
        .unwrap()
        .models[0];
    assert_eq!(model.cache_read_tokens, None);
    assert_eq!(model.prompt_tokens, None);
}

#[test]
fn parser_schema_refreshes_legacy_tier_without_rekeying_and_retains_repriceable_facts() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("cache.db");
    let storage = Storage::open(&database).unwrap();
    write_session(
        directory.path(),
        "auxiliary.jsonl",
        &[
            json!({"type":"service_tier_change","id":"fast","serviceTier":{"openai":"priority"}}),
            auxiliary("title-a", 279, 145),
            auxiliary("title-b", 270, 120),
        ],
    );
    let path = directory.path().join("agent/sessions/auxiliary.jsonl");
    let events = parse_jsonl(&fs::read_to_string(&path).unwrap());
    let legacy = legacy_values(&events);
    storage
        .record_usage_events("omp", "account-a", 1, legacy.as_array().unwrap(), entry_key)
        .unwrap();
    let fingerprint = LogFileFingerprint::from_metadata(&fs::metadata(&path).unwrap()).unwrap();
    storage
        .save_log_events(
            "omp",
            &path,
            fingerprint.size,
            fingerprint.modified_nanos,
            &json!({"schema_version":3,"events":legacy}).to_string(),
        )
        .unwrap();

    let mut pricing = crate::pricing::test_bundled_pricing();
    let (outcome, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
    assert_eq!(outcome.events.len(), 2);
    for history in histories {
        let period = history.last_30_days.unwrap();
        assert_eq!(period.tokens, 814);
        assert!(period.estimate_complete);
        assert!((period.estimated_cost_usd.unwrap() - 0.003748).abs() < 1e-12);
    }
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained.len(), 2);
    assert!(retained
        .iter()
        .all(|event| event.speed == Some(RequestSpeed::Standard)));
    assert!(retained.iter().all(|event| event.recorded_cost.is_some()));
    assert!(
        !source_scan(&storage, directory.path(), "account-b", &pricing)
            .0
            .included
    );
    pricing.codex_credit_mode = true;
    let (credits, _) = source_scan(&storage, directory.path(), "account-a", &pricing);
    for (id, expected_api, expected_credits) in
        [("title-a", 0.002008, 0.0502), ("title-b", 0.00174, 0.0435)]
    {
        let event = retained
            .iter()
            .find(|event| event.id.as_deref() == Some(id))
            .unwrap();
        let tokens = crate::pricing::TokenBreakdown {
            input: event.input,
            output: event.output,
            speed: event.speed.unwrap(),
            ..crate::pricing::TokenBreakdown::default()
        };
        pricing.codex_credit_mode = false;
        let api = crate::providers::codex::local_usage::estimate_token_cost(
            &event.timestamp,
            &event.model,
            tokens,
            &pricing,
        )
        .unwrap();
        assert!((api - expected_api).abs() < 1e-12);
        pricing.codex_credit_mode = true;
        let credit_usd = crate::providers::codex::local_usage::estimate_token_cost(
            &event.timestamp,
            &event.model,
            tokens,
            &pricing,
        )
        .unwrap();
        assert!((credit_usd * 25.0 - expected_credits).abs() < 1e-12);
    }
    assert_eq!(credits.events.len(), 2);
    fs::remove_file(path).unwrap();
    drop(storage);
    let storage = Storage::open(&database).unwrap();
    let changed_pricing = ModelPricing::new(
        PricingSupplement::default(),
        PricingCatalog {
            entries: HashMap::from([("gpt-6.1-sol".into(), ModelRates::new(4.0, 20.0))]),
            ..PricingCatalog::default()
        },
        PricingCatalog::default(),
    );
    let (repriced, _) = source_scan(&storage, directory.path(), "account-a", &changed_pricing);
    assert_eq!(repriced.events.len(), 2);
    assert!(
        (repriced
            .events
            .iter()
            .map(|event| event.cost.unwrap())
            .sum::<f64>()
            - 0.007496)
            .abs()
            < 1e-12
    );
    assert!(
        !source_scan(&storage, directory.path(), "account-b", &changed_pricing)
            .0
            .included
    );
}

#[test]
fn source_less_legacy_boolean_preserves_anonymous_keys_ownership_and_unknown_tokens() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("cache.db");
    let storage = Storage::open(&database).unwrap();
    let mut anonymous = auxiliary("unused", 279, 145);
    anonymous.as_object_mut().unwrap().remove("id");
    write_session(
        directory.path(),
        "anonymous.jsonl",
        &[anonymous.clone(), anonymous],
    );
    let path = directory.path().join("agent/sessions/anonymous.jsonl");
    let mut events = parse_jsonl(&fs::read_to_string(&path).unwrap());
    crate::providers::omp_usage::session::normalize_anonymous_occurrences(&mut events);
    let legacy = legacy_values(&events);
    let anonymous_key = |value: &Value| -> Result<String, serde_json::Error> {
        let event: SessionUsageEvent = serde_json::from_value(value.clone())?;
        if event.anonymous_occurrence <= 1 {
            usage_event_key(&(&event.session_id, event.timestamp, &event.model))
        } else {
            usage_event_key(&(
                &event.session_id,
                event.timestamp,
                &event.model,
                event.anonymous_occurrence,
            ))
        }
    };
    storage
        .record_usage_events(
            "omp",
            "account-a",
            1,
            legacy.as_array().unwrap(),
            anonymous_key,
        )
        .unwrap();
    fs::remove_file(path).unwrap();
    drop(storage);
    let storage = Storage::open(&database).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    let (outcome, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
    assert_eq!(outcome.events.len(), 2);
    assert!(outcome.events.iter().all(|event| event.cost.is_none()));
    for history in histories {
        let period = history.last_30_days.unwrap();
        assert_eq!(period.tokens, 848);
        assert!(!period.estimate_complete);
        assert!(period.estimated_cost_usd.is_none());
    }
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained.len(), 2);
    assert!(retained.iter().all(|event| event.speed.is_none()));
    assert!(retained.iter().any(|event| event.anonymous_occurrence == 1));
    assert!(retained.iter().any(|event| event.anonymous_occurrence == 2));
    assert!(
        !source_scan(&storage, directory.path(), "account-b", &pricing)
            .0
            .included
    );
}

#[test]
fn zero_token_unknown_tier_replies_do_not_make_priced_periods_partial() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
    let zero = json!({"type":"message","id":"cancelled","timestamp":"2026-07-12T14:00:00Z",
        "message":{"role":"assistant","provider":"openai-codex","model":"gpt-6.1-sol",
            "stopReason":"error","usage":{"input":0,"output":0,"totalTokens":0,"cost":{"total":0}}}});
    write_session(
        directory.path(),
        "zero.jsonl",
        &[auxiliary("paid", 279, 145), zero],
    );
    let (outcome, histories) = source_scan(
        &storage,
        directory.path(),
        "account-a",
        &crate::pricing::test_bundled_pricing(),
    );
    assert_eq!(outcome.events.len(), 1);
    for history in histories {
        let period = history.last_30_days.unwrap();
        assert_eq!(period.tokens, 424);
        assert!(period.estimate_complete);
        assert!(period.unknown_models.is_empty());
        assert!((period.estimated_cost_usd.unwrap() - 0.002008).abs() < 1e-12);
    }
}

#[test]
fn scan_dto_distinguishes_unknown_speed_from_unknown_model_in_both_price_bases() {
    for (model, tier, unknown_model) in [
        ("gpt-6.1-sol", None, false),
        ("gpt-5.5", Some("ultrafast"), false),
        ("unknown-omp-model", Some("default"), true),
    ] {
        for credits in [false, true] {
            let directory = tempdir().unwrap();
            let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
            let mut call = message("unpriced", "2026-07-12T14:00:00Z");
            call["message"]["model"] = json!(model);
            match tier {
                Some(tier) => call["message"]["serviceTier"] = json!(tier),
                None => {
                    call["message"]
                        .as_object_mut()
                        .unwrap()
                        .remove("serviceTier");
                }
            }
            let mut pricing = crate::pricing::test_bundled_pricing();
            pricing.codex_credit_mode = credits;
            let expected_unknown = if unknown_model {
                json!([model])
            } else {
                json!([])
            };
            for mixed in [false, true] {
                let mut calls = vec![call.clone()];
                if mixed {
                    calls.push(auxiliary("paid", 279, 145));
                }
                write_session(directory.path(), "partial.jsonl", &calls);
                let (outcome, histories) =
                    source_scan(&storage, directory.path(), "account-a", &pricing);
                assert_eq!(outcome.events.len(), if mixed { 2 } else { 1 });
                for history in histories {
                    let dto = serde_json::to_value(history).unwrap();
                    assert_eq!(dto["unknownModels"], expected_unknown);
                    for key in ["today", "last30Days"] {
                        let period = &dto[key];
                        assert_eq!(period["tokens"], json!(if mixed { 604 } else { 180 }));
                        assert_eq!(period["estimateComplete"], json!(false));
                        assert_eq!(period["unknownModels"], expected_unknown);
                        if mixed {
                            assert!(
                                (period["estimatedCostUsd"].as_f64().unwrap() - 0.002008).abs()
                                    < 1e-12
                            );
                        } else {
                            assert_eq!(period["estimatedCostUsd"], Value::Null);
                        }
                    }
                    assert_eq!(dto["daily"][0]["estimateComplete"], json!(false));
                    assert_eq!(
                        dto["daily"][0]["tokens"],
                        json!(if mixed { 604 } else { 180 })
                    );
                }
            }
        }
    }
}

#[test]
fn orchestration_and_producer_long_context_reprice_from_retained_merged_buckets() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("cache.db");
    let storage = Storage::open(&database).unwrap();
    let mut reported = auxiliary("orchestration-reported", 5, 5);
    reported["usage"]["input"] = json!(2);
    reported["usage"]["output"] = json!(2);
    reported["usage"]["orchestration"] = json!({"input":3,"output":3});
    reported["usage"]["totalTokens"] = json!(10);
    let mut derived = reported.clone();
    derived["id"] = json!("orchestration-derived");
    derived["usage"]
        .as_object_mut()
        .unwrap()
        .remove("totalTokens");
    write_session(
        directory.path(),
        "producer.jsonl",
        &[reported, derived, auxiliary("long-context", 272_001, 1)],
    );
    for credits in [false, true] {
        let mut pricing = crate::pricing::test_bundled_pricing();
        pricing.codex_credit_mode = credits;
        let (outcome, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
        assert_eq!(outcome.events.len(), 3);
        let expected = if credits { 0.544132 } else { 1.088139 };
        for history in histories {
            let period = history.last_30_days.unwrap();
            assert_eq!(period.tokens, 272_022);
            assert!(period.estimate_complete);
            assert!((period.estimated_cost_usd.unwrap() - expected).abs() < 1e-12);
        }
    }
    fs::remove_file(directory.path().join("agent/sessions/producer.jsonl")).unwrap();
    drop(storage);
    let storage = Storage::open(&database).unwrap();
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained.len(), 3);
    for event in retained
        .iter()
        .filter(|event| event.orchestration.is_some())
    {
        assert_eq!((event.input, event.output, event.total), (5, 5, 10));
        assert_eq!(event.speed, Some(RequestSpeed::Standard));
        assert!(event.has_complete_usage());
    }
    let (outcome, histories) = source_scan(
        &storage,
        directory.path(),
        "account-a",
        &crate::pricing::test_bundled_pricing(),
    );
    assert_eq!(outcome.events.len(), 3);
    for history in histories {
        let period = history.last_30_days.unwrap();
        assert_eq!(period.tokens, 272_022);
        assert!((period.estimated_cost_usd.unwrap() - 1.088139).abs() < 1e-12);
    }
}

#[test]
fn malformed_orchestration_with_explicit_tier_is_unpriced_not_unknown_model() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
    let mut malformed = message("malformed", "2026-07-12T14:00:00Z");
    malformed["message"]["model"] = json!("gpt-6.1-sol");
    malformed["message"]["usage"]["orchestration"] = json!({"input":"unknown"});
    write_session(directory.path(), "malformed.jsonl", &[malformed]);
    for credits in [false, true] {
        let mut pricing = crate::pricing::test_bundled_pricing();
        pricing.codex_credit_mode = credits;
        let (outcome, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
        assert_eq!(outcome.events.len(), 1);
        assert_eq!(outcome.events[0].cost, None);
        for history in histories {
            let period = history.last_30_days.unwrap();
            assert_eq!(period.tokens, 180);
            assert!(!period.estimate_complete);
            assert_eq!(period.estimated_cost_usd, None);
            assert!(period.unknown_models.is_empty());
        }
    }
}

#[test]
fn only_explicit_memory_excludes_cost_without_losing_usage_in_both_price_bases() {
    for credits in [false, true] {
        let mut pricing = crate::pricing::test_bundled_pricing();
        pricing.codex_credit_mode = credits;
        for purpose in [
            Some("memory"),
            Some("title"),
            Some("cache-warm"),
            Some("other"),
            None,
        ] {
            for known_speed in [false, true] {
                let directory = tempdir().unwrap();
                let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
                let mut call = auxiliary("call", 279, 145);
                match purpose {
                    Some(purpose) => call["purpose"] = json!(purpose),
                    None => {
                        call.as_object_mut().unwrap().remove("purpose");
                    }
                }
                if !known_speed {
                    call["usage"].as_object_mut().unwrap().remove("cost");
                }
                // A forked copy must still be deduplicated before applying the exclusion.
                write_session(directory.path(), "purpose.jsonl", &[call.clone(), call]);
                let (outcome, histories) =
                    source_scan(&storage, directory.path(), "account-a", &pricing);
                let excluded = purpose == Some("memory");
                let expected = if excluded {
                    Some(0.0)
                } else {
                    known_speed.then_some(0.002008)
                };
                assert_eq!(outcome.events.len(), 1);
                assert_eq!(outcome.events[0].total, 424);
                if let Some(expected) = expected {
                    assert!((outcome.events[0].cost.unwrap() - expected).abs() < 1e-12);
                } else {
                    assert_eq!(outcome.events[0].cost, None);
                }
                for history in histories {
                    let period = history.last_30_days.unwrap();
                    assert_eq!(period.tokens, 424);
                    assert_eq!(period.estimate_complete, excluded || known_speed);
                    assert_eq!(period.estimated_cost_usd, outcome.events[0].cost);
                    assert!(period.unknown_models.is_empty());
                }
                let retained: Vec<SessionUsageEvent> =
                    storage.load_usage_events("omp", "account-a", 1).unwrap();
                assert_eq!(retained.len(), 1);
                assert_eq!(retained[0].purpose.as_deref(), purpose);
                assert_eq!(retained[0].recorded_cost.is_some(), known_speed);
                assert_eq!(retained[0].speed.is_some(), known_speed);
            }
        }
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let mut assistant = message("assistant", "2026-07-12T14:00:00Z");
        assistant["purpose"] = json!("memory");
        assistant["message"]["model"] = json!("gpt-6.1-sol");
        assistant["message"]
            .as_object_mut()
            .unwrap()
            .remove("serviceTier");
        write_session(directory.path(), "assistant.jsonl", &[assistant]);
        let (outcome, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
        assert_eq!(outcome.events[0].cost, None);
        for history in histories {
            let period = history.last_30_days.unwrap();
            assert_eq!(period.tokens, 180);
            assert!(!period.estimate_complete);
        }
    }
}

#[test]
fn explicit_cache_write_ttls_survive_schema_refresh_retention_and_memory_exclusion() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
    let mut call = auxiliary("writes", 1_000, 0);
    call["serviceTier"] = json!("default");
    call["usage"]["cacheRead"] = json!(4_000);
    call["usage"]["cacheWrite"] = json!(7_000);
    call["usage"]["cacheWrite12h"] = json!(4_000);
    write_session(directory.path(), "writes.jsonl", &[call.clone()]);
    let path = directory.path().join("agent/sessions/writes.jsonl");
    let fingerprint = LogFileFingerprint::from_metadata(&fs::metadata(&path).unwrap()).unwrap();
    let mut old = serde_json::to_value(parse_jsonl(&fs::read_to_string(&path).unwrap())).unwrap();
    old[0].as_object_mut().unwrap().remove("cache_write_12h");
    storage
        .save_log_events(
            "omp",
            &path,
            fingerprint.size,
            fingerprint.modified_nanos,
            &json!({"schema_version":6,"events":old}).to_string(),
        )
        .unwrap();
    let mut pricing = crate::pricing::test_bundled_pricing();
    let (_, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
    for history in histories {
        let period = history.last_30_days.unwrap();
        assert_eq!(period.tokens, 12_000);
        assert!((period.estimated_cost_usd.unwrap() - 0.0299).abs() < 1e-12);
    }
    fs::remove_file(&path).unwrap();
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained[0].cache_write_12h, 4_000);
    assert_eq!(retained[0].cache_usage, Some((4_000, 12_000)));
    pricing.codex_credit_mode = true;
    let (_, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
    for history in histories {
        assert!((history.last_30_days.unwrap().estimated_cost_usd.unwrap() - 0.0164).abs() < 1e-12);
    }
    call["id"] = json!("memory-writes");
    call["purpose"] = json!("memory");
    write_session(directory.path(), "memory.jsonl", &[call.clone()]);
    call["id"] = json!("invalid-writes");
    call["purpose"] = json!("title");
    call["usage"]["cacheWrite1h"] = json!(3_001);
    write_session(directory.path(), "invalid.jsonl", &[call]);
    let (outcome, histories) = source_scan(&storage, directory.path(), "account-a", &pricing);
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|event| event.cost == Some(0.0))
            .count(),
        1
    );
    assert_eq!(
        outcome
            .events
            .iter()
            .filter(|event| event.cost.is_none())
            .count(),
        1
    );
    for history in histories {
        let period = history.last_30_days.unwrap();
        assert_eq!(period.tokens, 36_000);
        assert!(!period.estimate_complete);
        assert!((period.estimated_cost_usd.unwrap() - 0.0164).abs() < 1e-12);
    }
}

#[test]
fn omp_last_thirty_days_include_today_and_twenty_nine_prior_local_dates() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 7, 12, 15, 0, 0).unwrap();
    let today = now.with_timezone(&Local).date_naive();
    let entries: Vec<_> = [29, 30]
        .into_iter()
        .map(|days| {
            let mut event = auxiliary(&format!("day-{days}"), 100, 0);
            let date = today.checked_sub_days(Days::new(days)).unwrap();
            event["timestamp"] = json!(date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .earliest()
                .unwrap()
                .to_utc()
                .to_rfc3339());
            event
        })
        .collect();
    write_session(directory.path(), "boundary.jsonl", &entries);
    let (outcome, histories) = source_scan(
        &storage,
        directory.path(),
        "account-a",
        &crate::pricing::test_bundled_pricing(),
    );
    assert_eq!(outcome.events.len(), 2);
    assert_eq!(histories[0].last_30_days.as_ref().unwrap().tokens, 100);
    assert_eq!(histories[0].daily.len(), 1);
}
