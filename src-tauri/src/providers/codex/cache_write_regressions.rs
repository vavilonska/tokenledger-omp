use super::*;
use chrono::TimeZone;
use serde_json::json;
use tempfile::tempdir;

fn usage(scale: u64) -> Value {
    json!({"input_tokens": 12_000 * scale, "output_tokens": 0,
        "total_tokens": 12_000 * scale,
        "input_tokens_details": {"cached_tokens": 4_000 * scale,
            "cache_write_tokens": 7_000 * scale,
            "cache_write_tokens_details": {"ttl_30m_tokens": 3_000 * scale,
                "ttl_12h_tokens": 4_000 * scale}}})
}

fn call(at: DateTime<Utc>, bucket: &str, usage: Value) -> Value {
    let mut info = json!({});
    info[bucket] = usage;
    json!({"type":"event_msg", "timestamp":at.to_rfc3339(),
        "payload":{"type":"token_count", "info":info}})
}

fn journal(entries: &[Value]) -> String {
    std::iter::once(json!({"type":"turn_context",
        "payload":{"model":"gpt-6.1-sol", "service_tier":"default"}}))
    .chain(entries.iter().cloned())
    .map(|entry| entry.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn official_cache_write_ttls_price_disjointly_and_survive_delta_and_retention() {
    let now = Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap();
    let content = journal(&[
        call(
            now - chrono::Duration::minutes(2),
            "total_token_usage",
            usage(1),
        ),
        call(
            now - chrono::Duration::minutes(1),
            "total_token_usage",
            usage(2),
        ),
        call(now, "total_token_usage", usage(2)),
    ]);
    let directory = tempdir().unwrap();
    let home = directory.path().join("codex");
    fs::create_dir_all(home.join("sessions")).unwrap();
    let path = home.join("sessions/cache.jsonl");
    fs::write(&path, &content).unwrap();
    let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
    let mut legacy = serde_json::to_value(parse_jsonl(&content)).unwrap();
    for event in legacy.as_array_mut().unwrap() {
        for key in [
            "cache_write_30m",
            "cache_write_12h",
            "usage_complete",
            "speed_known",
        ] {
            event.as_object_mut().unwrap().remove(key);
        }
    }
    storage
        .record_usage_events(
            "codex",
            "account-a",
            4,
            legacy.as_array().unwrap(),
            |event| {
                let event: TokenEvent = serde_json::from_value(event.clone())?;
                usage_event_key(&(event.timestamp, &event.model))
            },
        )
        .unwrap();
    let fingerprint = crate::providers::log_usage::LogFileFingerprint::from_metadata(
        &fs::metadata(&path).unwrap(),
    )
    .unwrap();
    storage
        .save_log_events(
            "codex",
            &path,
            fingerprint.size,
            fingerprint.modified_nanos,
            &json!({"schema_version":8, "events":legacy}).to_string(),
        )
        .unwrap();
    let events = scan_codex_events(
        &storage,
        std::slice::from_ref(&home),
        NaiveDate::MIN,
        Some("account-a"),
    )
    .unwrap();
    assert_eq!(events.len(), 2);
    let mut pricing = crate::pricing::test_bundled_pricing();
    for event in &events {
        assert_eq!(
            (
                event.input,
                event.cached,
                event.cache_write_30m,
                event.cache_write_12h
            ),
            (12_000, 4_000, 3_000, 4_000)
        );
        assert_eq!(event.cache_usage, Some((4_000, 12_000)));
        assert!((estimate_cost(event, &pricing).unwrap() - 0.0299).abs() < 1e-12);
    }
    fs::remove_file(path).unwrap();
    let retained = scan_codex_events(
        &storage,
        std::slice::from_ref(&home),
        NaiveDate::MIN,
        Some("account-a"),
    )
    .unwrap();
    assert_eq!(retained, events);
    assert!(
        scan_codex_events(&storage, &[home], NaiveDate::MIN, Some("account-b"))
            .unwrap()
            .is_empty()
    );
    pricing.codex_credit_mode = true;
    for event in &retained {
        assert!((estimate_cost(event, &pricing).unwrap() - 0.0164).abs() < 1e-12);
    }
}

#[test]
fn official_last_usage_and_conflicting_or_missing_cache_evidence_stay_partial() {
    let now = Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    let event = parse_jsonl(&journal(&[call(now, "last_token_usage", usage(1))])).remove(0);
    assert!((estimate_cost(&event, &pricing).unwrap() - 0.0299).abs() < 1e-12);
    for field in [
        "cache_write_tokens_details",
        "cache_write_tokens",
        "cached_tokens",
    ] {
        let mut broken = usage(1);
        broken["input_tokens_details"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        let event = parse_jsonl(&journal(&[call(now, "last_token_usage", broken)])).remove(0);
        assert_eq!(event.total, 12_000);
        assert_eq!(event.cache_usage, None);
        assert_eq!(estimate_cost(&event, &pricing), None);
    }
    let mut conflicting = usage(1);
    conflicting["cached_input_tokens"] = json!(3_999);
    let event = parse_jsonl(&journal(&[call(now, "last_token_usage", conflicting)])).remove(0);
    assert_eq!(estimate_cost(&event, &pricing), None);
    let mut impossible = usage(1);
    impossible["input_tokens"] = json!(10_999);
    let event = parse_jsonl(&journal(&[call(now, "last_token_usage", impossible)])).remove(0);
    assert_eq!(estimate_cost(&event, &pricing), None);
}

#[test]
fn explicit_unknown_and_conflicting_speed_keep_tokens_without_standard_fallback() {
    let now = Utc.with_ymd_and_hms(2026, 10, 22, 12, 0, 0).unwrap();
    let pricing = crate::pricing::test_bundled_pricing();
    for tier in [json!("future-tier"), Value::Null, json!(42), json!("")] {
        let mut event = call(now, "last_token_usage", usage(1));
        event["payload"]["service_tier"] = tier;
        let events = parse_jsonl(&journal(&[event]));
        assert_eq!(events[0].total, 12_000);
        assert!(!events[0].speed_known);
        let retained: Vec<TokenEvent> =
            serde_json::from_str(&serde_json::to_string(&events).unwrap()).unwrap();
        let period = aggregate(retained, now, &pricing).today.unwrap();
        assert_eq!(period.tokens, 12_000);
        assert!(!period.estimate_complete);
        assert_eq!(period.estimated_cost_usd, None);
    }
    let mut event = call(now, "last_token_usage", usage(1));
    event["payload"]["service_tier"] = json!("default");
    event["payload"]["thread_settings"] = json!({"service_tier":"priority"});
    assert!(!parse_jsonl(&journal(&[event]))[0].speed_known);
    let mut turn = json!({"type":"turn_context", "payload":{"service_tier":"unknown"}});
    let calls = [turn.clone(), call(now, "last_token_usage", usage(1))];
    assert!(!parse_jsonl(&journal(&calls))[0].speed_known);
    turn["payload"]["service_tier"] = json!("priority");
    let fast = parse_jsonl(&journal(&[turn, call(now, "last_token_usage", usage(1))])).remove(0);
    assert!((estimate_cost(&fast, &pricing).unwrap() - 0.0598).abs() < 1e-12);
}

#[test]
fn local_thirty_day_boundary_does_not_truncate_independent_cycles() {
    let today = Local
        .with_ymd_and_hms(2026, 10, 22, 12, 0, 0)
        .earliest()
        .unwrap();
    let now = today.to_utc();
    let entries: Vec<_> = [0, 29, 30, 45]
        .into_iter()
        .map(|days| {
            let date = today
                .date_naive()
                .checked_sub_days(Days::new(days))
                .unwrap();
            let at = date
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_local_timezone(Local)
                .earliest()
                .unwrap()
                .to_utc();
            call(at, "last_token_usage", usage(1))
        })
        .collect();
    let events = parse_jsonl(&journal(&entries));
    let mut daily = DailyUsageAccumulator::default();
    let mut session = DailyUsageAccumulator::default();
    let mut weekly = DailyUsageAccumulator::default();
    aggregate_into(
        &events,
        now,
        &crate::pricing::test_bundled_pricing(),
        &mut daily,
        Some(events[3].timestamp),
        &mut session,
        Some(events[3].timestamp),
        &mut weekly,
        &mut [],
    );
    assert_eq!(
        daily.build(now, "test").last_30_days.unwrap().tokens,
        24_000
    );
    assert_eq!(
        cycle_period(session, now, "test", None).unwrap().tokens,
        48_000
    );
    assert_eq!(
        cycle_period(weekly, now, "test", None).unwrap().tokens,
        48_000
    );
}
