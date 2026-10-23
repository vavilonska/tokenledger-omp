use super::*;

fn write_fork_journal(
    home: &std::path::Path,
    name: &str,
    fork: bool,
    entries: &[Value],
) -> std::path::PathBuf {
    let path = home.join("agent/sessions").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let header = if fork {
        json!({"type":"session","id":"fork","parentSession":"original",
            "timestamp":"2026-07-12T14:30:00Z"})
    } else {
        json!({"type":"session","id":"original","timestamp":"2026-07-12T13:00:00Z"})
    };
    let content = std::iter::once(header)
        .chain(entries.iter().cloned())
        .map(|entry| entry.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, content).unwrap();
    path
}

fn widget_pair() -> (Value, Value) {
    let mut original = auxiliary("widget", 1000, 100);
    original["purpose"] = json!("job-widget");
    let mut inherited = original.clone();
    inherited["usage"]["cost"] =
        json!({"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0});
    (original, inherited)
}

fn assert_scan_totals(
    storage: &Storage,
    home: &std::path::Path,
    count: usize,
    tokens: u64,
    complete: bool,
) {
    for credits in [false, true] {
        let mut pricing = crate::pricing::test_bundled_pricing();
        pricing.codex_credit_mode = credits;
        let (outcome, histories) = source_scan(storage, home, "account-a", &pricing);
        assert_eq!(outcome.events.len(), count);
        assert_eq!(
            outcome.events.iter().all(|event| event.cost.is_some()),
            complete
        );
        for history in histories {
            let period = history.last_30_days.unwrap();
            assert_eq!(period.tokens, tokens);
            assert_eq!(period.estimate_complete, complete);
            if complete {
                assert!(period.estimated_cost_usd.unwrap() > 0.0);
            }
        }
    }
}

#[test]
fn fork_original_wins_both_file_orders_and_keeps_new_child_requests() {
    for original_first in [false, true] {
        let directory = tempdir().unwrap();
        let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
        let (original, inherited) = widget_pair();
        let mut child = auxiliary("new-child", 500, 50);
        child["timestamp"] = json!("2026-07-12T14:40:00Z");
        let (original_name, fork_name) = if original_first {
            ("a.jsonl", "z.jsonl")
        } else {
            ("z.jsonl", "a.jsonl")
        };
        write_fork_journal(directory.path(), original_name, false, &[original]);
        write_fork_journal(directory.path(), fork_name, true, &[inherited, child]);
        assert_scan_totals(&storage, directory.path(), 2, 1650, true);
        let retained: Vec<SessionUsageEvent> =
            storage.load_usage_events("omp", "account-a", 1).unwrap();
        let widget = retained
            .iter()
            .find(|event| event.id.as_deref() == Some("widget"))
            .unwrap();
        assert_eq!(widget.session_id.as_deref(), Some("original"));
        assert_eq!(widget.inherited_from_fork, Some(false));
        assert_eq!(widget.speed, Some(RequestSpeed::Standard));
        let child = retained
            .iter()
            .find(|event| event.id.as_deref() == Some("new-child"))
            .unwrap();
        assert_eq!(child.session_id.as_deref(), Some("fork"));
        assert_eq!(child.inherited_from_fork, Some(false));
        assert!(
            !source_scan(
                &storage,
                directory.path(),
                "account-b",
                &crate::pricing::test_bundled_pricing()
            )
            .0
            .included
        );
    }
}

#[test]
fn fork_later_refresh_and_deleted_original_cannot_erase_retained_billing_evidence() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("cache.db");
    let storage = Storage::open(&database).unwrap();
    let (original, inherited) = widget_pair();
    let original_path = write_fork_journal(directory.path(), "a.jsonl", false, &[original]);
    assert_scan_totals(&storage, directory.path(), 1, 1100, true);
    write_fork_journal(directory.path(), "z.jsonl", true, &[inherited]);
    assert_scan_totals(&storage, directory.path(), 1, 1100, true);
    fs::remove_file(original_path).unwrap();
    drop(storage);
    let storage = Storage::open(&database).unwrap();
    assert_scan_totals(&storage, directory.path(), 1, 1100, true);
    assert_scan_totals(&storage, directory.path(), 1, 1100, true);
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained[0].session_id.as_deref(), Some("original"));
    assert_eq!(retained[0].speed, Some(RequestSpeed::Standard));
}

#[test]
fn fork_polluted_legacy_journal_and_schema_seven_cache_recover_from_original() {
    for original_first in [false, true] {
        let directory = tempdir().unwrap();
        let database = directory.path().join("cache.db");
        let storage = Storage::open(&database).unwrap();
        let (original, inherited) = widget_pair();
        let (original_name, fork_name) = if original_first {
            ("a.jsonl", "z.jsonl")
        } else {
            ("z.jsonl", "a.jsonl")
        };
        let original_path = write_fork_journal(directory.path(), original_name, false, &[original]);
        let fork_path = write_fork_journal(directory.path(), fork_name, true, &[inherited]);
        for path in [&original_path, &fork_path] {
            let mut events =
                serde_json::to_value(parse_jsonl(&fs::read_to_string(path).unwrap())).unwrap();
            for event in events.as_array_mut().unwrap() {
                event.as_object_mut().unwrap().remove("inherited_from_fork");
            }
            let fingerprint =
                LogFileFingerprint::from_metadata(&fs::metadata(path).unwrap()).unwrap();
            storage
                .save_log_events(
                    "omp",
                    path,
                    fingerprint.size,
                    fingerprint.modified_nanos,
                    &json!({"schema_version":7,"events":events}).to_string(),
                )
                .unwrap();
            if path == &fork_path {
                storage
                    .record_usage_events(
                        "omp",
                        "account-a",
                        1,
                        events.as_array().unwrap(),
                        entry_key,
                    )
                    .unwrap();
            }
        }
        assert_scan_totals(&storage, directory.path(), 1, 1100, true);
        let retained: Vec<SessionUsageEvent> =
            storage.load_usage_events("omp", "account-a", 1).unwrap();
        assert_eq!(retained[0].session_id.as_deref(), Some("original"));
        assert_eq!(retained[0].speed, Some(RequestSpeed::Standard));
        let pricing = crate::pricing::test_bundled_pricing();
        let (api, _) = source_scan(&storage, directory.path(), "account-a", &pricing);
        assert!((api.events[0].cost.unwrap() - 0.003).abs() < 1e-12);
        fs::remove_file(original_path).unwrap();
        fs::remove_file(fork_path).unwrap();
        drop(storage);
        let storage = Storage::open(&database).unwrap();
        assert_scan_totals(&storage, directory.path(), 1, 1100, true);
        assert!(
            !source_scan(&storage, directory.path(), "account-b", &pricing)
                .0
                .included
        );
    }
}

#[test]
fn fork_without_original_never_fabricates_standard_or_drops_unknown_usage() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("cache.db");
    let storage = Storage::open(&database).unwrap();
    let (_, inherited) = widget_pair();
    let path = write_fork_journal(directory.path(), "fork.jsonl", true, &[inherited]);
    let mut legacy =
        serde_json::to_value(parse_jsonl(&fs::read_to_string(&path).unwrap())).unwrap();
    legacy[0]
        .as_object_mut()
        .unwrap()
        .remove("inherited_from_fork");
    storage
        .record_usage_events("omp", "account-a", 1, legacy.as_array().unwrap(), entry_key)
        .unwrap();
    assert_scan_totals(&storage, directory.path(), 1, 1100, false);
    fs::remove_file(path).unwrap();
    drop(storage);
    let storage = Storage::open(&database).unwrap();
    assert_scan_totals(&storage, directory.path(), 1, 1100, false);
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained[0].speed, None);
    assert_eq!(retained[0].total, 1100);
}

#[test]
fn fork_cannot_override_invalid_or_conflicting_original_tier_with_known_speed() {
    for conflicting in [false, true] {
        for original_first in [false, true] {
            let directory = tempdir().unwrap();
            let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
            let (mut original, mut inherited) = widget_pair();
            original["serviceTier"] = json!(if conflicting {
                "priority"
            } else {
                "not-a-tier"
            });
            if conflicting {
                original["service_tier"] = json!("default");
            }
            inherited["serviceTier"] = json!("default");
            let (original_name, fork_name) = if original_first {
                ("a.jsonl", "z.jsonl")
            } else {
                ("z.jsonl", "a.jsonl")
            };
            write_fork_journal(directory.path(), original_name, false, &[original]);
            write_fork_journal(directory.path(), fork_name, true, &[inherited]);
            assert_scan_totals(&storage, directory.path(), 1, 1100, false);
            let retained: Vec<SessionUsageEvent> =
                storage.load_usage_events("omp", "account-a", 1).unwrap();
            assert_eq!(retained[0].session_id.as_deref(), Some("original"));
            assert_eq!(retained[0].speed, None);
        }
    }
}

#[test]
fn fork_with_different_token_snapshot_cannot_borrow_original_price() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
    let (original, mut inherited) = widget_pair();
    inherited["usage"]["input"] = json!(999_000);
    write_fork_journal(directory.path(), "a.jsonl", false, &[original]);
    write_fork_journal(directory.path(), "z.jsonl", true, &[inherited]);
    assert_scan_totals(&storage, directory.path(), 1, 1100, true);
    let retained: Vec<SessionUsageEvent> =
        storage.load_usage_events("omp", "account-a", 1).unwrap();
    assert_eq!(retained[0].input, 1000);
    assert_eq!(retained[0].output, 100);
    assert!(
        (source_scan(
            &storage,
            directory.path(),
            "account-a",
            &crate::pricing::test_bundled_pricing()
        )
        .0
        .events[0]
            .cost
            .unwrap()
            - 0.003)
            .abs()
            < 1e-12
    );
}

#[test]
fn fork_deleted_legacy_cache_cannot_erase_retained_original_evidence() {
    let directory = tempdir().unwrap();
    let storage = Storage::open(&directory.path().join("cache.db")).unwrap();
    let (original, inherited) = widget_pair();
    let original_path = write_fork_journal(directory.path(), "original.jsonl", false, &[original]);
    let fork_path = write_fork_journal(directory.path(), "fork.jsonl", true, &[inherited]);
    for path in [&original_path, &fork_path] {
        let mut events =
            serde_json::to_value(parse_jsonl(&fs::read_to_string(path).unwrap())).unwrap();
        events[0]
            .as_object_mut()
            .unwrap()
            .remove("inherited_from_fork");
        if path == &original_path {
            storage
                .record_usage_events("omp", "", 1, events.as_array().unwrap(), entry_key)
                .unwrap();
        } else {
            let fingerprint =
                LogFileFingerprint::from_metadata(&fs::metadata(path).unwrap()).unwrap();
            storage
                .save_log_events(
                    "omp",
                    path,
                    fingerprint.size,
                    fingerprint.modified_nanos,
                    &json!({"schema_version":7,"events":events}).to_string(),
                )
                .unwrap();
        }
        fs::remove_file(path).unwrap();
    }
    // An empty identity is the supported unscoped cache-provenance mode.
    for _ in 0..2 {
        let (outcome, histories) = source_scan(
            &storage,
            directory.path(),
            "",
            &crate::pricing::test_bundled_pricing(),
        );
        assert_eq!(outcome.events.len(), 1);
        assert!((outcome.events[0].cost.unwrap() - 0.003).abs() < 1e-12);
        for history in histories {
            let period = history.last_30_days.unwrap();
            assert_eq!(period.tokens, 1100);
            assert!(period.estimate_complete);
        }
    }
}
