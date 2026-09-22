use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::log_usage::parse_log_timestamp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SessionUsageEvent {
    pub(super) id: Option<String>,
    pub(super) response_id: Option<String>,
    pub(super) timestamp: DateTime<Utc>,
    pub(super) model: String,
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) cache_read: u64,
    /// Includes the one-hour cache-write bucket.
    pub(super) cache_write: u64,
    pub(super) cache_write_1h: u64,
    pub(super) total: u64,
    pub(super) is_fast: bool,
}

pub(super) fn parse_jsonl(content: &str) -> Vec<SessionUsageEvent> {
    let mut events = Vec::new();
    let mut has_header = false;
    let mut branch_tiers = HashMap::<String, bool>::new();

    for line in content.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            // A damaged entry or an in-progress append must not hide later complete entries.
            continue;
        };
        let entry_type = entry.get("type").and_then(Value::as_str);
        if entry_type == Some("session") {
            has_header = true;
            branch_tiers.clear();
            continue;
        }
        if !has_header {
            // In particular, title slots may precede the header. Arbitrary artifact JSONL
            // without a session header is not a usage journal.
            continue;
        }

        let inherited_tier = entry
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|parent| branch_tiers.get(parent))
            .copied()
            .unwrap_or(false);
        let branch_tier = if entry_type == Some("service_tier_change") {
            // ServiceTierByFamily is a per-family map, not a single wire-tier string.
            entry
                .get("serviceTier")
                .and_then(|tiers| tiers.get("openai"))
                .and_then(Value::as_str)
                == Some("priority")
        } else {
            inherited_tier
        };
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            // Every tree entry carries inherited state, even non-usage messages. Id-less
            // metadata cannot move a branch or overwrite another branch's tier.
            branch_tiers.insert(id.to_owned(), branch_tier);
        }

        let message = match entry_type {
            Some("message") => match entry.get("message") {
                Some(message)
                    if message.get("role").and_then(Value::as_str) == Some("assistant") =>
                {
                    message
                }
                _ => continue,
            },
            Some("model_usage") => &entry,
            _ => continue,
        };
        if message.get("provider").and_then(Value::as_str) != Some("openai-codex") {
            continue;
        }
        let Some(usage) = message.get("usage").and_then(Value::as_object) else {
            continue;
        };
        let Some(timestamp) = entry
            .get("timestamp")
            .and_then(parse_timestamp)
            .or_else(|| message.get("timestamp").and_then(parse_timestamp))
        else {
            continue;
        };
        let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
        let output = usage.get("output").and_then(Value::as_u64).unwrap_or(0);
        let cache_read = usage.get("cacheRead").and_then(Value::as_u64).unwrap_or(0);
        let cache_write = usage.get("cacheWrite").and_then(Value::as_u64).unwrap_or(0);
        let cache_write_1h = usage
            .get("cacheWrite1h")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let total = usage
            .get("totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                input
                    .saturating_add(output)
                    .saturating_add(cache_read)
                    .saturating_add(cache_write)
            });
        // A message-level wire tier describes only this call; it must not change the
        // inherited configuration of descendants. Current AssistantMessage omits it,
        // but honor it when a journal actually persists it.
        let is_fast = message
            .get("serviceTier")
            .and_then(explicit_tier)
            .or_else(|| message.get("service_tier").and_then(explicit_tier))
            .unwrap_or(branch_tier);
        events.push(SessionUsageEvent {
            id: entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            response_id: message
                .get("responseId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            timestamp,
            model: message
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            input,
            output,
            cache_read,
            cache_write,
            cache_write_1h,
            total,
            is_fast,
        });
    }
    events
}

fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    if let Some(raw) = value.as_str() {
        parse_log_timestamp(raw)
    } else {
        DateTime::from_timestamp_millis(value.as_i64()?)
    }
}

fn explicit_tier(value: &Value) -> Option<bool> {
    match value {
        Value::Null => Some(false),
        Value::Object(tiers) => tiers.get("openai").map_or(Some(false), explicit_tier),
        Value::String(tier) => match tier.as_str() {
            "priority" => Some(true),
            "auto" | "default" | "flex" | "scale" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::parse_jsonl;

    fn call(id: &str, parent: Option<&str>) -> Value {
        json!({
            "type": "message", "id": id, "parentId": parent,
            "timestamp": "2026-09-08T01:02:03Z",
            "message": {
                "role": "assistant", "provider": "openai-codex", "model": "gpt-5",
                "responseId": format!("response-{id}"),
                "usage": {"input": 10, "output": 20, "cacheRead": 30, "cacheWrite": 40,
                    "cacheWrite1h": 15, "reasoningTokens": 8, "cost": {"total": 0}}
            }
        })
    }

    fn journal(entries: &[Value]) -> String {
        std::iter::once(json!({"type": "session", "id": "session"}))
            .chain(entries.iter().cloned())
            .map(|entry| entry.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn includes_auxiliary_calls_but_not_parent_task_summaries() {
        let assistant = call("assistant", None);
        let auxiliary = json!({
            "type": "model_usage", "id": "auxiliary", "parentId": "assistant",
            "timestamp": "2026-09-08 01:02:03.123456 UTC",
            "purpose": "title", "provider": "openai-codex", "model": "gpt-5-mini",
            "usage": {"input": 3, "output": 4, "totalTokens": 7}
        });
        let mut task_summary = call("summary", Some("assistant"));
        task_summary["message"]["role"] = json!("toolResult");
        task_summary["message"]["toolName"] = json!("task");
        let mut other_provider = call("other", None);
        other_provider["message"]["provider"] = json!("openai");
        let mut no_usage = call("no-usage", None);
        no_usage["message"].as_object_mut().unwrap().remove("usage");
        let events = parse_jsonl(&journal(&[
            assistant,
            auxiliary,
            task_summary,
            other_provider,
            no_usage,
        ]));
        assert_eq!(
            events.iter().map(|event| event.total).collect::<Vec<_>>(),
            [100, 7]
        );
        assert_eq!(events[0].response_id.as_deref(), Some("response-assistant"));
        assert_eq!(events[1].id.as_deref(), Some("auxiliary"));
        assert_eq!(events[1].timestamp.timestamp_subsec_millis(), 123);
    }

    #[test]
    fn counts_disjoint_buckets_without_readding_reasoning_or_one_hour_writes() {
        let first = call("first", None);
        let mut reported = call("reported", None);
        reported["message"]["usage"]["totalTokens"] = json!(123);
        let mut saturated = call("saturated", None);
        saturated["message"]["usage"]["input"] = json!(u64::MAX);
        let events = parse_jsonl(&journal(&[first, reported, saturated]));
        assert_eq!(events[0].total, 100);
        assert_eq!(
            (events[0].input, events[0].output, events[0].cache_read),
            (10, 20, 30)
        );
        assert_eq!((events[0].cache_write, events[0].cache_write_1h), (40, 15));
        assert_eq!(events[1].total, 123);
        assert_eq!(events[2].total, u64::MAX);
    }

    #[test]
    fn restores_branch_tiers_and_prefers_explicit_call_metadata() {
        let mut explicit = call("explicit", Some("fast"));
        explicit["message"]["service_tier"] = json!("default");
        let mut explicit_fast = call("explicit-fast", Some("standard"));
        explicit_fast["message"]["serviceTier"] = json!({"openai": "priority"});
        let events = parse_jsonl(&journal(&[
            call("root", None),
            json!({"type": "service_tier_change", "id": "fast", "parentId": "root",
                "serviceTier": {"openai": "priority", "anthropic": "default"}}),
            call("fast-call", Some("fast")),
            json!({"type": "service_tier_change", "id": "standard", "parentId": "fast",
                "serviceTier": null}),
            call("standard-call", Some("standard")),
            json!({"type": "title", "title": "Unrelated metadata"}),
            call("restored", Some("fast-call")),
            call("other-branch", Some("root")),
            explicit,
            call("after-explicit", Some("explicit")),
            explicit_fast,
            call("after-explicit-fast", Some("explicit-fast")),
            call("unknown-parent", Some("missing")),
        ]));
        assert_eq!(
            events.iter().map(|event| event.is_fast).collect::<Vec<_>>(),
            [false, true, false, true, false, false, true, true, false, false]
        );
    }

    #[test]
    fn tolerates_partial_lines_and_requires_a_session_header() {
        let first = call("first", None);
        let second = call("second", None);
        let artifact = format!("{first}\n{second}");
        assert!(parse_jsonl(&artifact).is_empty());
        let content = format!(
            "{{\"type\":\"title\",\"title\":\"Slot\"}}\n{}\n{{bad json\n{second}\n{{\"type\":",
            journal(&[first])
        );
        assert_eq!(
            parse_jsonl(&content)
                .iter()
                .map(|event| event.id.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }
}
