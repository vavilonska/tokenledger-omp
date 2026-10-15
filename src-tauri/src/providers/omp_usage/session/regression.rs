use serde_json::{json, Value};

use super::{
    normalize_anonymous_occurrences, parse_jsonl, RequestSpeed, SessionUsageEvent, SpeedEvidence,
};

fn auxiliary(input: u64, output: u64, multiplier: f64) -> Value {
    json!({
        "type": "model_usage", "parentId": "fast", "timestamp": "2026-09-08T01:02:03Z",
        "purpose": "memory", "provider": "openai-codex", "model": "gpt-6.1-sol",
        "usage": {"input": input, "output": output, "cacheRead": 0, "cacheWrite": 0,
            "cost": {"input": input as f64 * 2.0 * multiplier / 1_000_000.0,
                "output": output as f64 * 10.0 * multiplier / 1_000_000.0,
                "total": (input as f64 * 2.0 + output as f64 * 10.0) * multiplier / 1_000_000.0}}
    })
}

fn journal(entries: &[Value]) -> String {
    [
        json!({"type": "session", "id": "session"}),
        json!({"type": "service_tier_change", "id": "fast", "serviceTier": {"openai": "priority"}}),
    ]
    .into_iter()
    .chain(entries.iter().cloned())
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn real_memory_cost_evidence_recovers_standard_despite_priority_branch() {
    let first = auxiliary(279, 145, 1.0);
    let mut second = auxiliary(270, 120, 1.0);
    second["usage"]["reasoningTokens"] = json!(50);
    let events = parse_jsonl(&journal(&[first, second]));
    for event in &events {
        assert_eq!(event.speed, Some(RequestSpeed::Standard));
        assert_eq!(event.speed_evidence, Some(SpeedEvidence::Omp1811Cost));
        assert_eq!(event.entry_kind.as_deref(), Some("model_usage"));
        assert_eq!(event.purpose.as_deref(), Some("memory"));
    }
    assert_eq!(
        (events[0].input, events[0].output, events[0].total),
        (279, 145, 424)
    );
    assert_eq!(
        (events[1].input, events[1].output, events[1].total),
        (270, 120, 390)
    );
    let encoded = serde_json::to_string(&events).unwrap();
    let retained: Vec<SessionUsageEvent> = serde_json::from_str(&encoded).unwrap();
    assert_eq!(retained[0].speed, Some(RequestSpeed::Standard));
    assert!(retained[0].recorded_cost.is_some());
}

#[test]
fn cache_warm_preserves_verified_fast_and_ultrafast_producer_multipliers() {
    for (multiplier, expected) in [(2.5, RequestSpeed::Fast), (8.0, RequestSpeed::Ultrafast)] {
        let mut call = auxiliary(279, 145, multiplier);
        call["purpose"] = json!("cache-warm");
        assert_eq!(parse_jsonl(&journal(&[call]))[0].speed, Some(expected));
    }
}

#[test]
fn missing_conflicting_and_zero_cost_evidence_stays_unknown() {
    let mut absent = auxiliary(279, 145, 1.0);
    absent["usage"].as_object_mut().unwrap().remove("cost");
    let mut missing_bucket = auxiliary(279, 145, 1.0);
    missing_bucket["usage"]["cost"]
        .as_object_mut()
        .unwrap()
        .remove("output");
    let mut conflicting = auxiliary(279, 145, 1.0);
    conflicting["usage"]["cost"]["total"] = json!(0.004016);
    let zero = auxiliary(0, 0, 1.0);
    let mut unsupported = auxiliary(279, 145, 1.0);
    unsupported["model"] = json!("unproven-model");
    let mut negative = auxiliary(279, 145, 1.0);
    negative["usage"]["cost"]["cacheRead"] = json!(-1.0);
    for event in parse_jsonl(&journal(&[
        absent,
        missing_bucket,
        conflicting,
        zero,
        unsupported,
        negative,
    ])) {
        assert_eq!(event.speed, None);
        assert_eq!(event.speed_evidence, None);
    }
}

#[test]
fn explicit_request_tier_wins_but_invalid_or_conflicting_explicit_tiers_do_not_fall_back() {
    let mut explicit = auxiliary(279, 145, 1.0);
    explicit["serviceTier"] = json!("ultrafast");
    let mut conflict = explicit.clone();
    conflict["service_tier"] = json!("default");
    let mut invalid = explicit.clone();
    invalid["serviceTier"] = json!("unknown-tier");
    let mut assistant = json!({"type":"message", "parentId":"fast", "timestamp":"2026-09-08T01:02:03Z",
        "message":{"role":"assistant", "provider":"openai-codex", "model":"gpt-6.1-sol",
            "serviceTier":"unknown-tier", "usage":{"input":279,"output":145}}});
    let mut events = parse_jsonl(&journal(&[explicit, conflict, invalid, assistant.clone()]));
    assert_eq!(events.remove(0).speed, Some(RequestSpeed::Ultrafast));
    assert!(events.iter().all(|event| event.speed.is_none()));
    assistant["message"]["serviceTier"] = json!("default");
    assert_eq!(
        parse_jsonl(&journal(&[assistant]))[0].speed,
        Some(RequestSpeed::Standard)
    );
}

#[test]
fn legacy_boolean_is_unknown_and_identity_and_anonymous_occurrences_survive() {
    let mut events = parse_jsonl(&journal(&[
        auxiliary(279, 145, 1.0),
        auxiliary(279, 145, 1.0),
    ]));
    normalize_anonymous_occurrences(&mut events);
    let mut legacy = serde_json::to_value(&events).unwrap();
    for (index, event) in legacy.as_array_mut().unwrap().iter_mut().enumerate() {
        let event = event.as_object_mut().unwrap();
        event.remove("speed");
        event.remove("speed_evidence");
        event.remove("recorded_cost");
        event.remove("entry_kind");
        event.remove("purpose");
        event.insert("is_fast".into(), json!(index == 0));
    }
    let retained: Vec<SessionUsageEvent> = serde_json::from_value(legacy).unwrap();
    assert_eq!(retained.len(), 2);
    for (index, event) in retained.iter().enumerate() {
        assert_eq!(event.speed, None);
        assert_eq!(event.total, 424);
        assert_eq!(event.session_id.as_deref(), Some("session"));
        assert_eq!(event.anonymous_occurrence, index as u64 + 1);
    }
}

#[test]
fn cache_warm_requires_consistent_disjoint_bucket_costs() {
    let mut warm = auxiliary(100, 20, 2.5);
    warm["purpose"] = json!("cache-warm");
    warm["usage"]["cacheRead"] = json!(50);
    warm["usage"]["cost"]["cacheRead"] = json!(0.0000125);
    warm["usage"]["cost"]["total"] = json!(0.0010125);
    let mut conflict = warm.clone();
    conflict["usage"]["cost"]["input"] = json!(0.00025);
    conflict["usage"]["cost"]["output"] = json!(0.00075);
    let events = parse_jsonl(&journal(&[warm, conflict]));
    assert_eq!(events[0].speed, Some(RequestSpeed::Fast));
    assert_eq!(events[0].total, 170);
    // Same total but conflicting per-bucket evidence is not a valid speed match.
    assert_eq!(events[1].speed, None);
}

#[test]
fn producer_evidence_does_not_accept_display_api_multipliers() {
    for multiplier in [2.0, 6.0] {
        let events = parse_jsonl(&journal(&[auxiliary(279, 145, multiplier)]));
        assert_eq!(events[0].speed, None);
    }
}

#[test]
fn orchestration_buckets_are_merged_before_speed_inference_without_readding_total() {
    for reported_total in [None, Some(10)] {
        let mut call = auxiliary(2, 2, 2.5);
        call["usage"]["orchestration"] = json!({"input":3,"output":3});
        if let Some(total) = reported_total {
            call["usage"]["totalTokens"] = json!(total);
        }
        let event = parse_jsonl(&journal(&[call])).remove(0);
        assert_eq!(
            (event.input, event.output, event.cache_read, event.total),
            (5, 5, 0, 10)
        );
        assert_eq!(event.speed, Some(RequestSpeed::Standard));
        assert!(event.has_complete_usage());
        let retained: SessionUsageEvent =
            serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(
            (retained.input, retained.output, retained.total),
            (5, 5, 10)
        );
        let source = retained.orchestration.unwrap();
        assert_eq!((source.input, source.output, source.cache_read), (3, 3, 0));
    }
    let mut cached = auxiliary(2, 2, 2.5);
    cached["usage"]["orchestration"] = json!({"input":3,"output":3,"cacheRead":2});
    cached["usage"]["cost"]["cacheRead"] = json!(0.0000002);
    cached["usage"]["cost"]["total"] = json!(0.0000602);
    let event = parse_jsonl(&journal(&[cached])).remove(0);
    assert_eq!(
        (event.input, event.output, event.cache_read, event.total),
        (5, 5, 2, 12)
    );
    assert_eq!(event.speed, Some(RequestSpeed::Standard));
}

#[test]
fn malformed_or_unsupported_orchestration_cannot_be_mistaken_for_speed() {
    for invalid in [
        Value::Null,
        json!([]),
        json!({"input":-1}),
        json!({"input":"3"}),
        json!({"input":3.5}),
        json!({"cacheWrite":3}),
        json!({"input":u64::MAX}),
    ] {
        let mut call = auxiliary(2, 2, 2.5);
        call["usage"]["orchestration"] = invalid;
        let event = parse_jsonl(&journal(&[call.clone()])).remove(0);
        assert_eq!(event.speed, None);
        assert!(!event.has_complete_usage());
        call["serviceTier"] = json!("priority");
        let explicit = parse_jsonl(&journal(&[call])).remove(0);
        assert_eq!(explicit.speed, Some(RequestSpeed::Fast));
        assert!(!explicit.has_complete_usage());
    }
}

#[test]
fn producer_free_cache_writes_and_long_context_do_not_use_api_rates() {
    let mut writes = auxiliary(1, 1, 1.0);
    writes["usage"]["cacheWrite"] = json!(7);
    writes["usage"]["cost"]["cacheWrite"] = json!(0.0);
    let event = parse_jsonl(&journal(&[writes])).remove(0);
    assert_eq!((event.cache_write, event.total), (7, 9));
    assert_eq!(event.speed, Some(RequestSpeed::Standard));
    for (model, factor) in [("gpt-6.1-sol", 1.0), ("gpt-6-astra", 5.0)] {
        let mut long = auxiliary(272_001, 1, factor);
        long["model"] = json!(model);
        let event = parse_jsonl(&journal(&[long])).remove(0);
        assert_eq!(event.speed, Some(RequestSpeed::Standard));
        assert_eq!(event.input, 272_001);
    }
}
