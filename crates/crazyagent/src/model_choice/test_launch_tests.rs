use super::*;

/// A Codex `exec --json` error line whose message is the provider's error body, as
/// research/runner.rs agent_failure reads it.
fn codex_error(code: &str) -> String {
    let body = serde_json::json!({
        "error": {"message": "The model `x` does not exist.", "type": "invalid_request_error", "code": code}
    });
    serde_json::json!({"type": "error", "message": body.to_string()}).to_string()
}

#[test]
fn a_failed_run_is_a_refusal_only_when_its_output_names_the_provider_code_as_a_word() {
    assert!(refused("codex", &codex_error("model_not_found")));
    assert!(refused(
        "codex",
        r#"{"type":"turn.failed","error":{"message":"x","codexErrorInfo":"modelNotFound"}}"#
    ));
    assert!(refused(
        "claude-code",
        r#"API Error: 404 {"error":"model_not_found"}"#
    ));
    assert!(!refused("codex", &codex_error("rate_limit_exceeded")));
    assert!(!refused("codex", &codex_error("model_not_found_yet")));
    assert!(!refused("claude-code", "API Error: xmodel_not_found"));
    assert!(!refused("claude-code", "modelNotFound"));
    assert!(!refused("codex", ""));
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
