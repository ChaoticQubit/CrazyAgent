use super::*;

/// A Codex `exec --json` error line whose message is the provider's error body, as
/// research/runner.rs agent_failure reads it.
fn codex_error(code: &str) -> String {
    let body = serde_json::json!({
        "error": {"message": "The model `x` does not exist.", "type": "invalid_request_error", "code": code}
    });
    serde_json::json!({"type": "error", "message": body.to_string()}).to_string()
}

/// A Claude Code stream-json assistant line whose own `error` field names a code, as the
/// harness writes it for a failed API call (DEC-150 MG-M1).
fn claude_error(code: &str) -> String {
    serde_json::json!({
        "type": "assistant", "uuid": "assistant-1", "session_id": "native-session",
        "parent_tool_use_id": null, "error": code,
        "message": {"id": "message-1", "type": "message", "role": "assistant", "model": "<synthetic>",
            "content": [{"type": "text", "text": "There's an issue with the selected model (claude-x). It may not exist or you may not have access to it."}]}
    })
    .to_string()
}

/// A refusal is read only from the error fields the harness writes on its stream, never from
/// text the model wrote or an unstructured line (DEC-150 MG-M1, Q-6): Claude Code's
/// stream-json assistant `error`, and Codex's provider error body code or `codexErrorInfo`.
#[test]
fn a_failed_run_is_a_refusal_only_when_a_harness_error_field_names_the_provider_code() {
    let said = |harness: &str, text: &str| {
        match harness {
        "claude-code" => serde_json::json!({
            "type": "assistant", "uuid": "assistant-2", "session_id": "native-session",
            "parent_tool_use_id": null,
            "message": {"id": "message-2", "type": "message", "role": "assistant", "model": "claude-x",
                "content": [{"type": "text", "text": text}]}
        })
        .to_string(),
        _ => serde_json::json!({"type": "item.completed", "item": {"id": "item-1", "type": "agent_message", "text": text}})
            .to_string(),
    }
    };
    assert!(refused("claude-code", &claude_error("model_not_found")));
    assert!(refused("codex", &codex_error("model_not_found")));
    assert!(refused(
        "codex",
        r#"{"type":"turn.failed","error":{"message":"x","codexErrorInfo":"modelNotFound"}}"#
    ));
    assert!(!refused("claude-code", &claude_error("rate_limit")));
    assert!(!refused("codex", &codex_error("rate_limit_exceeded")));
    assert!(!refused("codex", &codex_error("model_not_found_yet")));
    assert!(
        !refused(
            "claude-code",
            r#"API Error: 404 {"error":"model_not_found"}"#
        ),
        "a text-mode line is no structured error"
    );
    for harness in ["claude-code", "codex"] {
        assert!(
            !refused(harness, &said(harness, "model_not_found modelNotFound")),
            "{harness}: the model's own words are never a refusal"
        );
        assert!(!refused(harness, ""));
    }
}

#[test]
fn an_answer_verifies_a_known_refusal_fails_and_anything_else_stays_pending_with_why() {
    let refusal = codex_error("model_not_found");
    assert_eq!(
        outcome("codex", Ended::Answered, None),
        (Tested::Verified, None)
    );
    assert_eq!(
        outcome("codex", Ended::Failed, Some(&refusal)),
        (Tested::Failed, None)
    );
    assert_eq!(
        outcome("codex", Ended::Answered, Some(&refusal)),
        (Tested::Verified, None),
        "an answer is never read as a refusal"
    );
    let (tested, reason) = outcome("codex", Ended::Failed, Some("Agent stopped"));
    assert_eq!(tested, Tested::Pending);
    assert!(reason.unwrap().starts_with("Not reported by OpenAI"));
    let (tested, reason) = outcome("claude-code", Ended::Failed, None);
    assert_eq!(tested, Tested::Pending);
    assert!(reason.unwrap().starts_with("Not reported by Anthropic"));
    assert_eq!(
        outcome(
            "claude-code",
            Ended::Failed,
            Some(&claude_error("model_not_found"))
        ),
        (Tested::Failed, None)
    );
    for ended in [Ended::TimedOut, Ended::Stopped] {
        let (tested, reason) = outcome("codex", ended, Some(&refusal));
        assert_eq!(tested, Tested::Pending, "{ended:?}");
        let reason = reason.unwrap();
        assert!(!reason.is_empty() && reason.len() <= REASON_BYTES_MAX);
    }
}

#[test]
fn every_run_that_never_started_says_why_by_the_harness_name() {
    for harness in ["claude-code", "codex"] {
        let name = crate::harnesses::spec(harness).unwrap().name;
        let reasons: Vec<String> = [
            NotStarted::NotInstalled,
            NotStarted::NoAccount,
            NotStarted::Paused,
            NotStarted::UrlAccount,
            NotStarted::CouldNotStart,
        ]
        .into_iter()
        .map(|why| not_started(harness, why))
        .collect();
        for reason in &reasons {
            assert!(reason.contains(name), "{reason}");
            assert!(reason.len() <= REASON_BYTES_MAX, "{reason}");
        }
        let mut distinct = reasons.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), reasons.len());
    }
}

#[test]
fn only_claude_code_and_codex_have_a_test_launch() {
    assert!(supported("claude-code") && supported("codex"));
    for harness in ["rcli", "opencode", "claude", ""] {
        assert!(!supported(harness), "{harness}");
    }
}
