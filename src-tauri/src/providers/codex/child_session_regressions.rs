use std::fs;

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde_json::json;

use super::{
    aggregate_into, build_cycle_buckets, build_cycle_history, estimate_cost, parse_jsonl,
    scan_codex_events, TokenEvent,
};
use crate::{
    pricing::{test_bundled_pricing, RequestSpeed},
    providers::{daily_usage::DailyUsageAccumulator, log_usage::LogFileFingerprint, omp_usage},
    storage::Storage,
};

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 18, 0, 0, 0).unwrap()
}

fn turn_id(millis: i64) -> String {
    format!(
        "{:08x}-{:04x}-7000-8000-000000000000",
        millis >> 16,
        millis & 0xffff
    )
}

fn child_rollout(write_delay: i64, created_at: Option<&str>) -> String {
    let write_time = (start() + Duration::seconds(write_delay)).to_rfc3339();
    let usage = |timestamp: DateTime<Utc>, input, cached, output| {
        json!({
            "timestamp": timestamp.to_rfc3339(), "type": "event_msg",
            "payload": {"type": "token_count", "info": {"total_token_usage": {
                "input_tokens": input, "cached_input_tokens": cached,
                "output_tokens": output, "total_tokens": input + output
            }}}
        })
    };
    let first_live = usage(start(), 1100, 540, 120);
    [
        json!({"timestamp": write_time, "type": "session_meta", "payload": {
            "id": "late-child", "parent_thread_id": "parent", "timestamp": created_at
        }}),
        json!({"type": "turn_context", "payload": {"model": "gpt-5.5", "service_tier": "fast"}}),
        // Replayed task timestamps can be written alongside the child metadata.
        json!({"timestamp": write_time, "type": "event_msg", "payload": {
            "type": "task_started", "started_at": start().timestamp() - 2,
            "turn_id": turn_id(start().timestamp_millis() - 1900)
        }}),
        usage(start() - Duration::seconds(1), 1000, 500, 100),
        json!({"timestamp": write_time, "type": "event_msg", "payload": {
            "type": "task_started", "started_at": start().timestamp(),
            "turn_id": turn_id(start().timestamp_millis() + 200)
        }}),
        first_live.clone(),
        first_live,
        usage(start() + Duration::days(1), 1300, 620, 160),
        usage(
            start() + Duration::days(1) + Duration::seconds(1),
            1400,
            660,
            180,
        ),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n")
}

fn assert_live_usage(events: &[TokenEvent]) {
    assert_eq!(events.len(), 3);
    assert_eq!(events.iter().map(|event| event.total).sum::<u64>(), 480);
    assert_eq!(events[0].timestamp, start());
    assert_eq!(events[0].input, 100);
    assert_eq!(events[0].cached, 40);
    assert_eq!(events[0].output, 20);
    assert_eq!(events[0].cache_usage, Some((40, 100)));
    assert!(events.iter().all(|event| event.speed == RequestSpeed::Fast));
    assert!(events.iter().all(|event| event.source_id.is_some()));
}

#[test]
fn creation_time_opens_live_child_tasks_without_including_inherited_usage() {
    for delay in [0, 1, 60] {
        let events = parse_jsonl(&child_rollout(delay, Some("2026-10-18T00:00:00.100Z")));
        assert_live_usage(&events);
    }
    // A genuine creation boundary after the inherited/current task still rejects both,
    // even if its record timestamp happens to be earlier.
    assert!(parse_jsonl(&child_rollout(0, Some("2026-10-18T00:00:01Z"))).is_empty());
}

#[test]
fn absent_or_invalid_creation_time_preserves_the_original_replay_gate() {
    for created_at in [None, Some("not-a-timestamp")] {
        assert_live_usage(&parse_jsonl(&child_rollout(0, created_at)));
        assert!(parse_jsonl(&child_rollout(1, created_at)).is_empty());
    }
}

#[test]
fn schema_eight_rescans_skipped_children_without_duplicating_or_reassigning_facts() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("codex");
    let sessions = home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let source = sessions.join("late-child.jsonl");
    fs::write(&source, child_rollout(1, Some("2026-10-18T00:00:00Z"))).unwrap();
    let source = fs::canonicalize(source).unwrap();
    let fingerprint = LogFileFingerprint::from_metadata(&fs::metadata(&source).unwrap()).unwrap();
    let database = directory.path().join("usage.db");
    let storage = Storage::open(&database).unwrap();
    let known = parse_jsonl(
        r#"{"type":"session_meta","payload":{"id":"known-root"}}
{"timestamp":"2026-10-17T23:00:00Z","type":"event_msg","payload":{"type":"token_count","model":"gpt-5.5","info":{"last_token_usage":{"input_tokens":10,"output_tokens":1,"total_tokens":11}}}}"#,
    );
    storage
        .record_usage_events("codex", "account-a", 1, &known, |event| {
            Ok(event.source_id.clone().unwrap())
        })
        .unwrap();
    // This is the persisted result produced when schema seven never cleared its gate.
    storage
        .save_log_events(
            "codex",
            &source,
            fingerprint.size,
            fingerprint.modified_nanos,
            r#"{"schema_version":7,"events":[]}"#,
        )
        .unwrap();
    let scan = |storage: &Storage, account| {
        scan_codex_events(
            storage,
            std::slice::from_ref(&home),
            NaiveDate::MIN,
            Some(account),
        )
        .unwrap()
    };
    let recovered = scan(&storage, "account-a");
    // Storage orders facts by key rather than log position.
    assert_eq!(recovered.len(), 4);
    assert!(recovered.contains(&known[0]));
    let mut ordered: Vec<_> = recovered
        .iter()
        .filter(|event| event.source_id != known[0].source_id)
        .cloned()
        .collect();
    ordered.sort_by_key(|event| event.timestamp);
    assert_live_usage(&ordered);
    assert_eq!(scan(&storage, "account-a"), recovered);
    fs::copy(&source, sessions.join("copied-child.jsonl")).unwrap();
    assert_eq!(scan(&storage, "account-a"), recovered);
    assert!(scan(&storage, "account-b").is_empty());
    drop(storage);
    fs::remove_file(&source).unwrap();
    fs::remove_file(sessions.join("copied-child.jsonl")).unwrap();
    let storage = Storage::open(&database).unwrap();
    assert_eq!(scan(&storage, "account-a"), recovered);
    assert!(scan(&storage, "account-b").is_empty());
}

#[test]
fn skipped_child_with_no_remaining_source_cannot_invent_usage() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("codex");
    fs::create_dir_all(home.join("sessions")).unwrap();
    let source = home.join("sessions/deleted-child.jsonl");
    let storage = Storage::open(&directory.path().join("usage.db")).unwrap();
    storage
        .save_log_events(
            "codex",
            &source,
            0,
            0,
            r#"{"schema_version":7,"events":[]}"#,
        )
        .unwrap();
    assert!(scan_codex_events(&storage, &[home], NaiveDate::MIN, None)
        .unwrap()
        .is_empty());
}

#[test]
fn recovered_child_keeps_event_boundaries_raw_cost_and_subscription_cutoff() {
    let mut events = parse_jsonl(&child_rollout(60, Some("2026-10-18T00:00:00Z")));
    let now = start() + Duration::days(2);
    let mut future = events[0].clone();
    future.timestamp = now + Duration::seconds(1);
    events.push(future);
    events.push(events[0].clone());
    for credit_mode in [false, true] {
        let mut pricing = test_bundled_pricing();
        pricing.codex_credit_mode = credit_mode;
        let first_cost = estimate_cost(&events[0], &pricing).unwrap();
        let second_cost = estimate_cost(&events[1], &pricing).unwrap();
        let third_cost = estimate_cost(&events[2], &pricing).unwrap();
        let mut cycles = build_cycle_buckets(
            vec![
                omp_usage::QuotaCycleAnchor {
                    started_at: start(),
                    scheduled_reset_at: start() + Duration::days(7),
                    used_percent: 50.0,
                    exhausted_at: None,
                },
                omp_usage::QuotaCycleAnchor {
                    started_at: start() + Duration::days(1),
                    scheduled_reset_at: start() + Duration::days(8),
                    used_percent: 100.0,
                    exhausted_at: Some(start() + Duration::days(1)),
                },
            ],
            now,
        );
        let mut daily = DailyUsageAccumulator::default();
        let mut session = DailyUsageAccumulator::default();
        let mut weekly = DailyUsageAccumulator::default();
        aggregate_into(
            &events,
            now,
            &pricing,
            &mut daily,
            Some(start() + Duration::days(1)),
            &mut session,
            Some(start() + Duration::days(1)),
            &mut weekly,
            &mut cycles,
        );
        let history = build_cycle_history(cycles, now, "test");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].cycle.usage.tokens, 120);
        assert_eq!(history[1].cycle.usage.tokens, 360);
        assert_eq!(history[0].cycle.usage.estimated_cost_usd, Some(first_cost));
        assert_eq!(history[1].cycle.usage.estimated_cost_usd, Some(second_cost));
        let raw = daily.build(now, "test").last_30_days.unwrap();
        assert_eq!(raw.tokens, 480);
        assert_eq!(
            raw.estimated_cost_usd,
            Some(first_cost + second_cost + third_cost)
        );
        assert_eq!(session.build(now, "test").last_30_days.unwrap().tokens, 360);
        assert_eq!(weekly.build(now, "test").last_30_days.unwrap().tokens, 360);
    }
}

fn same_second_rollout(
    creation_millis: i64,
    marker: Option<&str>,
    current_id: Option<&str>,
    current_second: i64,
) -> String {
    let record_time = (start() + Duration::seconds(1)).to_rfc3339();
    let usage = |input, cached, output| {
        json!({"timestamp": record_time, "type": "event_msg", "payload": {
            "type": "token_count", "info": {"total_token_usage": {
                "input_tokens": input, "cached_input_tokens": cached,
                "output_tokens": output, "total_tokens": input + output
            }}
        }})
    };
    let baseline = usage(1000, 500, 100);
    let mut lines = vec![
        json!({"timestamp": record_time, "type": "session_meta", "payload": {
            "id": "child", "parent_thread_id": "parent",
            "timestamp": (start() + Duration::milliseconds(creation_millis)).to_rfc3339()
        }}),
        json!({"type": "turn_context", "payload": {"model": "gpt-5.5"}}),
        // The copied parent marker is not the child's initialization boundary.
        json!({"timestamp": record_time, "type": "event_msg", "payload": {
            "type": "thread_settings_applied", "thread_id": "parent"
        }}),
        json!({"timestamp": record_time, "type": "event_msg", "payload": {
            "type": "task_started", "started_at": start().timestamp(),
            "turn_id": turn_id(start().timestamp_millis() + creation_millis / 2)
        }}),
        baseline.clone(),
    ];
    if let Some(thread) = marker {
        lines.push(
            json!({"timestamp": record_time, "type": "event_msg", "payload": {
                "type": "thread_settings_applied", "thread_id": thread
            }}),
        );
        // A usage replay between the marker and live task is still only a baseline.
        lines.push(baseline);
    }
    lines.push(
        json!({"timestamp": record_time, "type": "event_msg", "payload": {
            "type": "task_started", "started_at": current_second, "turn_id": current_id
        }}),
    );
    let live = usage(1100, 540, 120);
    lines.push(live.clone());
    lines.push(live);
    lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_one_live_event(content: &str) {
    let events = parse_jsonl(content);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].total, 120);
    assert_eq!(events[0].input, 100);
    assert_eq!(events[0].cached, 40);
    assert_eq!(events[0].cache_usage, Some((40, 100)));
}

#[test]
fn same_second_parent_task_is_excluded_and_new_v7_child_task_is_counted() {
    for offset in [900, 1100] {
        let id = turn_id(start().timestamp_millis() + offset);
        let content =
            same_second_rollout(800, None, Some(&id), start().timestamp() + offset / 1000);
        assert_one_live_event(&content);
    }
}

#[test]
fn matching_child_marker_resolves_equal_milliseconds_and_legacy_ids_only_at_next_task() {
    let tied = turn_id(start().timestamp_millis() + 800);
    for id in [None, Some("legacy-child"), Some(tied.as_str())] {
        assert_one_live_event(&same_second_rollout(
            800,
            Some("child"),
            id,
            start().timestamp(),
        ));
    }
}

#[test]
fn missing_or_foreign_marker_cannot_guess_an_ambiguous_legacy_or_tied_task() {
    let tied = turn_id(start().timestamp_millis() + 800);
    for marker in [None, Some("parent"), Some("other-thread"), Some("")] {
        for id in [None, Some("legacy-child"), Some(tied.as_str())] {
            assert!(
                parse_jsonl(&same_second_rollout(800, marker, id, start().timestamp())).is_empty()
            );
        }
    }
}

#[test]
fn malformed_or_non_v7_ids_do_not_supply_creation_millisecond_evidence() {
    let valid = turn_id(start().timestamp_millis() + 900);
    assert_one_live_event(&same_second_rollout(
        800,
        None,
        Some(&valid.to_uppercase()),
        start().timestamp(),
    ));
    let mut invalid = vec![format!("{valid}x"), valid[..35].to_owned()];
    for (index, replacement) in [(8, "0"), (14, "4"), (19, "7"), (35, "g"), (0, "é")] {
        let mut id = valid.clone();
        id.replace_range(index..index + 1, replacement);
        invalid.push(id);
    }
    for id in invalid {
        assert!(parse_jsonl(&same_second_rollout(
            800,
            None,
            Some(&id),
            start().timestamp()
        ))
        .is_empty());
    }
}

#[test]
fn integral_creation_second_requires_task_identity_or_a_matching_child_boundary() {
    let tied = turn_id(start().timestamp_millis());
    for id in [None, Some("legacy-child"), Some(tied.as_str())] {
        for marker in [None, Some("parent")] {
            assert!(
                parse_jsonl(&same_second_rollout(0, marker, id, start().timestamp())).is_empty()
            );
        }
        assert_one_live_event(&same_second_rollout(
            0,
            Some("child"),
            id,
            start().timestamp(),
        ));
    }
    let later = turn_id(start().timestamp_millis() + 1);
    assert_one_live_event(&same_second_rollout(
        0,
        None,
        Some(&later),
        start().timestamp(),
    ));
    assert_one_live_event(&same_second_rollout(
        0,
        None,
        Some("legacy-child"),
        start().timestamp() + 1,
    ));
}
