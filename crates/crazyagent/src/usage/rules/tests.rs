use super::*;
use serde_json::json;

fn sidechain(version: &str, stop: Option<&str>, text: &str, output: u64) -> Value {
    json!({
        "type": "assistant",
        "isSidechain": true,
        "version": version,
        "message": {
            "id": "msg_a",
            "model": "claude-opus-4-5",
            "stop_reason": stop,
            "content": [{"type": "text", "text": text}],
            "usage": {"input_tokens": 10, "output_tokens": output}
        }
    })
}

#[test]
fn a_placeholder_line_reports_its_content_chars() {
    let usage = claude_usage(&sidechain("2.1.306", None, &"x".repeat(40), 1)).unwrap();
    assert_eq!(usage.estimated_chars, 40);
}

#[test]
fn the_placeholder_rule_ends_at_2_1_307() {
    assert_eq!(
        claude_usage(&sidechain("2.1.307", None, "abcd", 1))
            .unwrap()
            .estimated_chars,
        0
    );
    assert_eq!(
        claude_usage(&sidechain("2.2.0", None, "abcd", 1))
            .unwrap()
            .estimated_chars,
        0
    );
    assert_eq!(
        claude_usage(&sidechain("2.0.99", None, "abcd", 1))
            .unwrap()
            .estimated_chars,
        4
    );
    assert_eq!(
        claude_usage(&sidechain("1.9.9", None, "abcd", 1))
            .unwrap()
            .estimated_chars,
        4
    );
}

#[test]
fn a_finished_or_main_line_or_odd_version_is_no_placeholder() {
    assert_eq!(
        claude_usage(&sidechain("2.1.306", Some("end_turn"), "abcd", 1))
            .unwrap()
            .estimated_chars,
        0
    );
    assert_eq!(
        claude_usage(&sidechain("x.y.z", None, "abcd", 1))
            .unwrap()
            .estimated_chars,
        0
    );
    let mut main = sidechain("2.1.306", None, "abcd", 1);
    main["isSidechain"] = json!(false);
    assert_eq!(claude_usage(&main).unwrap().estimated_chars, 0);
}

#[test]
fn every_line_of_a_placeholder_message_adds_up_and_output_never_falls() {
    let mut first = claude_usage(&sidechain("2.1.306", None, &"x".repeat(40), 1)).unwrap();
    first.merge(claude_usage(&sidechain("2.1.306", None, &"y".repeat(44), 1)).unwrap());
    assert_eq!(first.settled().tokens.output, 21);
    let large = claude_usage(&sidechain("2.1.306", None, "ab", 900)).unwrap();
    assert_eq!(large.settled().tokens.output, 900);
}

#[test]
fn a_tool_call_counts_its_input_as_output_chars() {
    let mut value = sidechain("2.1.306", None, "", 1);
    value["message"]["content"] =
        json!([{"type": "tool_use", "input": {"a": 1}}, {"type": "text", "text": "ab"}]);
    assert_eq!(claude_usage(&value).unwrap().estimated_chars, 9);
}

#[test]
fn a_model_with_control_characters_is_not_kept() {
    let mut value = sidechain("2.1.306", None, "", 1);
    value["message"]["model"] = json!("bad\u{1b}[31m");
    assert_eq!(claude_usage(&value).unwrap().model, "");
}

#[test]
fn a_one_hour_write_and_a_web_search_are_read() {
    let value = json!({"type": "assistant", "message": {"usage": {
        "cache_creation_input_tokens": 30,
        "cache_creation": {"ephemeral_1h_input_tokens": 20},
        "server_tool_use": {"web_search_requests": 2},
        "speed": "fast"
    }}});
    let usage = claude_usage(&value).unwrap();
    assert_eq!(
        (
            usage.tokens.cache_write,
            usage.one_hour,
            usage.web_searches,
            usage.fast
        ),
        (30, 20, 2, true)
    );
}

#[test]
fn only_advisor_iterations_become_records_and_each_has_its_own_key() {
    let value = json!({"type": "assistant", "message": {"id": "msg_a", "model": "claude-opus-4-5", "usage": {
        "input_tokens": 1,
        "iterations": [
            {"type": "message", "input_tokens": 1},
            {"type": "advisor_message", "input_tokens": 2},
            {"type": "advisor_message", "model": "claude-fable-5-1", "cache_read_input_tokens": 3,
             "cache_creation": {"ephemeral_1h_input_tokens": 4}, "speed": "fast",
             "server_tool_use": {"web_search_requests": 1}}
        ]
    }}});
    let records = advisor_usages(&value);
    assert_eq!(
        records
            .iter()
            .map(|usage| (usage.key.as_deref().unwrap(), usage.model.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("claude:msg_a:advisor:0", "claude-opus-4-5"),
            ("claude:msg_a:advisor:1", "claude-fable-5-1")
        ]
    );
    assert_eq!(
        (
            records[1].tokens.cache_read,
            records[1].tokens.cache_write,
            records[1].one_hour
        ),
        (3, 4, 4)
    );
    assert_eq!(
        (records[1].fast, records[1].web_searches, records[0].fast),
        (true, 1, false)
    );
}

#[test]
fn a_line_without_advisor_iterations_has_no_advisor_records() {
    assert!(advisor_usages(&sidechain("2.1.306", None, "ab", 1)).is_empty());
    assert!(
        advisor_usages(&json!({"type": "user", "message": {"usage": {"iterations": []}}}))
            .is_empty()
    );
}

#[test]
fn timestamps_read_as_utc_milliseconds() {
    assert_eq!(timestamp_ms("1970-01-01T00:00:01Z"), Some(1000));
    assert_eq!(
        timestamp_ms("2026-11-09T02:16:05.123Z"),
        Some(1_794_190_565_123)
    );
    assert_eq!(timestamp_ms("2026-13-09T02:16:05Z"), None);
    assert_eq!(timestamp_ms("yesterday"), None);
}

#[test]
fn the_notice_keeps_codeburns_mit_text() {
    assert!(THIRD_PARTY_NOTICES.contains("Copyright (c) 2026 AgentSeal"));
    assert!(THIRD_PARTY_NOTICES.contains("Permission is hereby granted, free of charge"));
}
