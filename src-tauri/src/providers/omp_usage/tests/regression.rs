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
