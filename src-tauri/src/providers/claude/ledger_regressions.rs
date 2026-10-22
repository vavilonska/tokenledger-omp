use std::{collections::HashMap, fs, path::Path};

use chrono::{TimeZone, Utc};
use tempfile::tempdir;

use super::{event_key, parse_jsonl, scan_local_usage, ClaudeTokenEvent, UsageSources};
use crate::{
    models::UsageHistory,
    pricing::{ModelPricing, ModelRates, PricingCatalog, PricingSupplement},
    storage::Storage,
};

fn scan(storage: &Storage, root: &Path, account: &str) -> UsageHistory {
    let prices = ModelPricing::new(
        PricingSupplement::default(),
        PricingCatalog {
            entries: HashMap::from([("claude-test".into(), ModelRates::new(1.0, 1.0))]),
            ..PricingCatalog::default()
        },
        PricingCatalog::default(),
    );
    scan_local_usage(
        storage,
        Utc.with_ymd_and_hms(2026, 2, 21, 12, 0, 0).unwrap(),
        &prices,
        "claude",
        Some(account),
        UsageSources {
            configured_roots: &[root.to_owned()],
            include_standard_roots: false,
            include_pi: false,
        },
    )
    .unwrap()
}

fn assert_usage(history: &UsageHistory, tokens: u64, cost: f64) {
    let total = history.last_30_days.as_ref().unwrap();
    assert_eq!(total.tokens, tokens);
    assert!((total.estimated_cost_usd.unwrap() - cost).abs() < 1e-12);
}

fn project(root: &Path) -> std::path::PathBuf {
    let project = root.join("projects/project");
    fs::create_dir_all(&project).unwrap();
    project
}

#[test]
fn fuller_copy_survives_same_scan_later_scan_deleted_source_and_restart() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("claude");
    let project = project(&root);
    let full = project.join("a.jsonl");
    let stale = project.join("z.jsonl");
    fs::write(&full, r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":2.0,"message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":100,"speed":"fast"}}}"#).unwrap();
    fs::write(&stale, r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":0.1,"message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":10}}}"#).unwrap();
    let database = directory.path().join("usage.db");
    let storage = Storage::open(&database).unwrap();
    let expected = scan(&storage, &root, "account-a");
    assert_usage(&expected, 200, 2.0);
    let retained: Vec<ClaudeTokenEvent> =
        storage.load_usage_events("claude", "account-a", 1).unwrap();
    assert_eq!(retained.len(), 1);
    assert!(retained[0].has_speed && retained[0].is_fast);
    fs::remove_file(full).unwrap();
    assert_eq!(scan(&storage, &root, "account-a"), expected);
    assert!(scan(&storage, &root, "account-b").daily.is_empty());
    fs::remove_file(stale).unwrap();
    drop(storage);
    let restarted = Storage::open(&database).unwrap();
    assert_eq!(scan(&restarted, &root, "account-a"), expected);
}

#[test]
fn parent_and_each_advisor_survive_scan_copy_replay_with_or_without_ids() {
    // The anonymous case deliberately uses the same model and timestamp for every role.
    for identity in [
        r#""requestId":"r","message":{"id":"m","#,
        r#""requestId":"r","message":{"#,
        r#""message":{"#,
    ] {
        let directory = tempdir().unwrap();
        let root = directory.path().join("claude");
        let project = project(&root);
        let path = project.join("a.jsonl");
        let copy = project.join("z.jsonl");
        let line = format!(
            r#"{{"timestamp":"2026-02-20T12:00:00Z","costUSD":1.0,{identity}"model":"claude-test","usage":{{"input_tokens":100,"output_tokens":50,"iterations":[{{"type":"advisor_message","model":"claude-test","input_tokens":10,"output_tokens":2}}]}}}}}}"#
        );
        fs::write(&path, &line).unwrap();
        let database = directory.path().join("usage.db");
        let storage = Storage::open(&database).unwrap();
        let expected = scan(&storage, &root, "account-a");
        assert_usage(&expected, 162, 1.000012);
        fs::write(&copy, &line).unwrap();
        assert_eq!(scan(&storage, &root, "account-a"), expected);
        assert!(scan(&storage, &root, "account-b").daily.is_empty());
        let multiple = line.replace(
            r#""output_tokens":2}]"#,
            r#""output_tokens":2},{"type":"advisor_message","model":"claude-test","input_tokens":10,"output_tokens":2}]"#,
        );
        fs::write(&path, multiple).unwrap();
        assert_usage(&scan(&storage, &root, "account-a"), 174, 1.000024);
        fs::remove_file(path).unwrap();
        fs::remove_file(copy).unwrap();
        drop(storage);
        let restarted = Storage::open(&database).unwrap();
        assert_usage(&scan(&restarted, &root, "account-a"), 174, 1.000024);
    }
}

#[test]
fn source_reparse_repairs_legacy_advisor_that_overwrote_parent_key() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("claude");
    let path = project(&root).join("usage.jsonl");
    // The legacy advisor is larger than its parent: a plain max(tokens) cannot repair it.
    let line = r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":1.0,"message":{"model":"claude-test","usage":{"input_tokens":100,"output_tokens":50,"iterations":[{"type":"advisor_message","model":"claude-test","input_tokens":200,"output_tokens":2}]}}}"#;
    fs::write(&path, line).unwrap();
    let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
    let mut legacy = serde_json::to_value(parse_jsonl(line)).unwrap();
    for event in legacy.as_array_mut().unwrap() {
        event.as_object_mut().unwrap().remove("advisor_index");
        event.as_object_mut().unwrap().remove("parent_event_key");
    }
    let legacy: Vec<ClaudeTokenEvent> = serde_json::from_value(legacy).unwrap();
    storage
        .record_usage_events("claude", "account-a", 1, &legacy, event_key)
        .unwrap();
    let fingerprint = crate::providers::log_usage::LogFileFingerprint::from_metadata(
        &fs::metadata(&path).unwrap(),
    )
    .unwrap();
    storage
        .save_log_events(
            "claude",
            &path,
            fingerprint.size,
            fingerprint.modified_nanos,
            &serde_json::json!({"schema_version":4,"events":legacy}).to_string(),
        )
        .unwrap();
    // A different account must not steal the newly split child before the owner repairs it.
    assert!(scan(&storage, &root, "account-b").daily.is_empty());
    assert_usage(&scan(&storage, &root, "account-a"), 352, 1.000202);
    let retained: Vec<ClaudeTokenEvent> =
        storage.load_usage_events("claude", "account-a", 1).unwrap();
    assert_eq!(retained.len(), 2);
    assert_eq!(
        retained
            .iter()
            .filter(|event| event.advisor_index.is_some())
            .count(),
        1
    );
    fs::remove_file(path).unwrap();
    assert_usage(&scan(&storage, &root, "account-a"), 352, 1.000202);
}

#[test]
fn deleted_legacy_source_keeps_attested_fact_without_inventing_parent_cost() {
    let directory = tempdir().unwrap();
    let root = directory.path().join("claude");
    let database = directory.path().join("usage.db");
    let storage = Storage::open(&database).unwrap();
    let line = r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","message":{"model":"claude-test","usage":{"input_tokens":10,"output_tokens":2}}}"#;
    // Exactly what schema 1 retained after its request-only advisor replaced the parent.
    let mut legacy = parse_jsonl(line);
    legacy[0].parent_event_key = None;
    storage
        .record_usage_events("claude", "account-a", 1, &legacy, event_key)
        .unwrap();
    drop(storage);
    let reopened = Storage::open(&database).unwrap();
    assert_usage(&scan(&reopened, &root, "account-a"), 12, 0.000012);
    assert!(scan(&reopened, &root, "account-b").daily.is_empty());
}

#[test]
fn snapshot_merge_converges_across_orders_and_scan_batches() {
    let copies = [
        r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":100}}}"#,
        r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":0.1,"message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":10,"speed":"fast"}}}"#,
        r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":2.0,"message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":100,"speed":"standard"}}}"#,
    ];
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        for scan_after in [vec![2], vec![0, 2], vec![1, 2], vec![0, 1, 2]] {
            let dir = tempdir().unwrap();
            let root = dir.path().join("claude");
            let project = project(&root);
            let db = dir.path().join("usage.db");
            let storage = Storage::open(&db).unwrap();
            for (index, copy) in order.into_iter().enumerate() {
                fs::write(project.join(format!("{index}.jsonl")), copies[copy]).unwrap();
                if scan_after.contains(&index) {
                    scan(&storage, &root, "a");
                }
            }
            let expected = scan(&storage, &root, "a");
            assert_usage(&expected, 200, 2.0);
            let facts: Vec<ClaudeTokenEvent> = storage.load_usage_events("claude", "a", 1).unwrap();
            assert_eq!(facts.len(), 1);
            assert!(facts[0].has_speed && !facts[0].is_fast && !facts[0].speed_conflict);
            assert_eq!(facts[0].speed_evidence_rank, Some((true, 200)));
            fs::remove_dir_all(project).unwrap();
            drop(storage);
            let reopened = Storage::open(&db).unwrap();
            assert_eq!(scan(&reopened, &root, "a"), expected);
        }
    }
}

#[test]
fn smaller_speed_evidence_survives_but_its_exact_cost_does_not_price_larger_usage() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let path = project(&root).join("usage.jsonl");
    let storage = Storage::open(&dir.path().join("usage.db")).unwrap();
    fs::write(&path, r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":0.1,"message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":10,"speed":"fast"}}}"#).unwrap();
    scan(&storage, &root, "a");
    fs::write(&path, r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":100}}}"#).unwrap();
    scan(&storage, &root, "a");
    let facts: Vec<ClaudeTokenEvent> = storage.load_usage_events("claude", "a", 1).unwrap();
    assert!(facts[0].has_speed && facts[0].is_fast);
    assert_eq!(facts[0].speed_evidence_rank, Some((true, 110)));
    assert_eq!(facts[0].cost_usd, None);
    fs::remove_file(path).unwrap();
    scan(&storage, &root, "a");
    assert_eq!(
        storage
            .load_usage_events::<ClaudeTokenEvent>("claude", "a", 1)
            .unwrap(),
        facts
    );
}

#[test]
fn same_snapshot_cache_evidence_upgrades_and_equal_rank_price_conflicts_are_partial() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let path = project(&root).join("usage.jsonl");
    let storage = Storage::open(&dir.path().join("usage.db")).unwrap();
    let mut line = serde_json::json!({"timestamp":"2026-02-20T12:00:00Z","requestId":"r",
        "message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":50}}});
    fs::write(&path, line.to_string()).unwrap();
    scan(&storage, &root, "a");
    line["message"]["usage"]["cache_read_input_tokens"] = 0.into();
    line["message"]["usage"]["cache_creation_input_tokens"] = 0.into();
    fs::write(&path, line.to_string()).unwrap();
    scan(&storage, &root, "a");
    let facts: Vec<ClaudeTokenEvent> = storage.load_usage_events("claude", "a", 1).unwrap();
    assert_eq!(facts[0].cache_usage, Some((0, 100)));
    line["message"]["usage"]["speed"] = "fast".into();
    line["costUSD"] = 1.into();
    fs::write(&path, line.to_string()).unwrap();
    scan(&storage, &root, "a");
    line["message"]["usage"]["speed"] = "standard".into();
    line["costUSD"] = 2.into();
    fs::write(&path, line.to_string()).unwrap();
    let history = scan(&storage, &root, "a");
    let period = history.last_30_days.unwrap();
    assert_eq!(period.tokens, 150);
    assert!(!period.estimate_complete);
    assert_eq!(period.estimated_cost_usd, None);
    fs::remove_file(path).unwrap();
    let facts: Vec<ClaudeTokenEvent> = storage.load_usage_events("claude", "a", 1).unwrap();
    assert!(facts[0].pricing_conflict && facts[0].speed_conflict);
    assert_eq!(facts[0].cache_usage, Some((0, 100)));
}

#[test]
fn legacy_advisor_rekeys_atomically_with_speed_and_account_ownership() {
    for anonymous in [false, true] {
        let dir = tempdir().unwrap();
        let root = dir.path().join("claude");
        let path = project(&root).join("usage.jsonl");
        let db = dir.path().join("usage.db");
        let storage = Storage::open(&db).unwrap();
        let mut line = serde_json::json!({"timestamp":"2026-02-20T12:00:00Z","requestId":"r","costUSD":1.0,
            "message":{"model":"claude-test","usage":{"input_tokens":100,"output_tokens":50,
                "iterations":[{"type":"advisor_message","model":if anonymous {"claude-test-advisor"} else {"claude-test"},
                    "input_tokens":10,"output_tokens":2}]}}});
        if anonymous {
            line.as_object_mut().unwrap().remove("requestId");
        }
        let text = line.to_string();
        fs::write(&path, &text).unwrap();
        let mut old = parse_jsonl(&text);
        for event in &mut old {
            event.parent_event_key = None;
            event.advisor_index = None;
        }
        old[1].has_speed = true;
        old[1].is_fast = true;
        storage
            .record_usage_events("claude", "a", 1, &old, event_key)
            .unwrap();
        let fp = crate::providers::log_usage::LogFileFingerprint::from_metadata(
            &fs::metadata(&path).unwrap(),
        )
        .unwrap();
        storage
            .save_log_events(
                "claude",
                &path,
                fp.size,
                fp.modified_nanos,
                &serde_json::json!({"schema_version":4,"events":old}).to_string(),
            )
            .unwrap();
        assert!(scan(&storage, &root, "b").daily.is_empty());
        let history = scan(&storage, &root, "a");
        assert_eq!(history.last_30_days.as_ref().unwrap().tokens, 162);
        let facts: Vec<ClaudeTokenEvent> = storage.load_usage_events("claude", "a", 1).unwrap();
        assert_eq!(facts.len(), 2);
        let child = facts
            .iter()
            .find(|event| event.advisor_index.is_some())
            .unwrap();
        assert!(child.has_speed && child.is_fast);
        assert_eq!(child.speed_evidence_rank, Some((true, 12)));
        assert_eq!(scan(&storage, &root, "a"), history);
        fs::remove_file(path).unwrap();
        drop(storage);
        let reopened = Storage::open(&db).unwrap();
        assert_eq!(scan(&reopened, &root, "a"), history);
        assert_eq!(
            reopened
                .load_usage_events::<ClaudeTokenEvent>("claude", "a", 1)
                .unwrap(),
            facts
        );
    }
}

#[test]
fn cache_only_legacy_advisor_reconciles_with_current_source_before_first_journal_write() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("claude");
    let path = project(&root).join("copy.jsonl");
    let db = dir.path().join("usage.db");
    let storage = Storage::open(&db).unwrap();
    let text = r#"{"timestamp":"2026-02-20T12:00:00Z","costUSD":1.0,"message":{"model":"claude-test","usage":{"input_tokens":100,"output_tokens":50,"iterations":[{"type":"advisor_message","model":"claude-advisor","input_tokens":10,"output_tokens":2}]}}}"#;
    fs::write(&path, text).unwrap();
    let mut old = parse_jsonl(text);
    for event in &mut old {
        event.advisor_index = None;
        event.parent_event_key = None;
    }
    old[1].has_speed = true;
    old[1].is_fast = true;
    let deleted_source = path.with_file_name("deleted.jsonl");
    storage
        .save_log_events(
            "claude",
            &deleted_source,
            1,
            0,
            &serde_json::json!({"schema_version":4,"events":old}).to_string(),
        )
        .unwrap();
    assert!(storage
        .load_usage_events::<ClaudeTokenEvent>("claude", "", 1)
        .unwrap()
        .is_empty());
    let expected = scan(&storage, &root, "");
    assert_eq!(expected.last_30_days.as_ref().unwrap().tokens, 162);
    let facts = storage
        .load_usage_events::<ClaudeTokenEvent>("claude", "", 1)
        .unwrap();
    assert_eq!(facts.len(), 2);
    assert!(facts
        .iter()
        .any(|event| event.advisor_index.is_some() && event.has_speed && event.is_fast));
    assert_eq!(scan(&storage, &root, ""), expected);
    fs::remove_file(path).unwrap();
    drop(storage);
    let reopened = Storage::open(&db).unwrap();
    assert_eq!(scan(&reopened, &root, ""), expected);
    assert_eq!(
        reopened
            .load_usage_events::<ClaudeTokenEvent>("claude", "", 1)
            .unwrap(),
        facts
    );
}

#[test]
fn equal_totals_with_different_buckets_never_borrow_cache_evidence() {
    for reverse in [false, true] {
        let dir = tempdir().unwrap();
        let root = dir.path().join("claude");
        let path = project(&root).join("usage.jsonl");
        let db = dir.path().join("usage.db");
        let storage = Storage::open(&db).unwrap();
        let copies = [
            r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","message":{"id":"m","model":"claude-test","usage":{"input_tokens":100,"output_tokens":50}}}"#,
            r#"{"timestamp":"2026-02-20T12:00:00Z","requestId":"r","message":{"id":"m","model":"claude-test","usage":{"input_tokens":50,"output_tokens":50,"cache_read_input_tokens":50,"cache_creation_input_tokens":0}}}"#,
        ];
        for index in if reverse { [1, 0] } else { [0, 1] } {
            fs::write(&path, copies[index]).unwrap();
            scan(&storage, &root, "a");
        }
        let expected = scan(&storage, &root, "a");
        assert!(!expected.last_30_days.as_ref().unwrap().estimate_complete);
        let facts = storage
            .load_usage_events::<ClaudeTokenEvent>("claude", "a", 1)
            .unwrap();
        assert_eq!(facts.len(), 1);
        assert!(facts[0].cache_conflict);
        assert_eq!(facts[0].cache_usage, None);
        fs::remove_file(path).unwrap();
        drop(storage);
        let reopened = Storage::open(&db).unwrap();
        assert_eq!(scan(&reopened, &root, "a"), expected);
    }
}
