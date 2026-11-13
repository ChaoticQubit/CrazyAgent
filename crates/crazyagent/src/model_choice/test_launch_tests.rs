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

/// DEC-150 addendum 9e (leader 18:58): RCLI gets a test launch beside Claude Code and
/// Codex, so an owner's RCLI id can be verified; no other harness has one.
#[test]
fn claude_code_codex_and_rcli_have_a_test_launch() {
    assert!(supported("claude-code") && supported("codex") && supported("rcli"));
    for harness in ["opencode", "claude", ""] {
        assert!(!supported(harness), "{harness}");
    }
}

/// DEC-150 addendum 9e (leader 19:01, 19:08): an RCLI test launch reads only the provider
/// failure the host named and the account's provider. An answer verifies the id; a 404
/// fails it only from OpenAI or Anthropic; OpenRouter, every other failure, a timeout and a
/// stop leave it pending with the provider's account screen name.
#[test]
fn an_rcli_test_fails_an_id_only_on_a_404_from_openai_or_anthropic() {
    use crate::rcli_host::RcliProviderFailure as Failure;
    for provider in ["openai", "anthropic", "openrouter"] {
        assert_eq!(
            rcli_outcome(provider, RcliEnded::Answered),
            (Tested::Verified, None),
            "{provider}"
        );
    }
    for provider in ["openai", "anthropic"] {
        assert_eq!(
            rcli_outcome(provider, RcliEnded::Failed(Some(Failure::NotFound))),
            (Tested::Failed, None),
            "{provider}"
        );
    }
    let (tested, reason) = rcli_outcome("openrouter", RcliEnded::Failed(Some(Failure::NotFound)));
    assert_eq!(tested, Tested::Pending);
    assert!(reason.unwrap().starts_with("Not reported by OpenRouter: "));
    for failure in [
        None,
        Some(Failure::RateLimited),
        Some(Failure::QuotaExhausted),
        Some(Failure::InvalidAuthentication),
    ] {
        let (tested, reason) = rcli_outcome("openai", RcliEnded::Failed(failure));
        assert_eq!(tested, Tested::Pending, "{failure:?}");
        assert!(
            reason.unwrap().starts_with("Not reported by OpenAI API: "),
            "{failure:?}"
        );
    }
    for ended in [RcliEnded::TimedOut, RcliEnded::Stopped] {
        let (tested, reason) = rcli_outcome("anthropic", ended);
        assert_eq!(tested, Tested::Pending, "{ended:?}");
        let reason = reason.unwrap();
        assert!(!reason.is_empty() && reason.len() <= REASON_BYTES_MAX);
    }
}

/// DEC-150 addendum 9e (leader 19:01): an RCLI test that started no run says why, naming
/// the provider the id is tested on as the account screens do; with no set-up account of
/// that provider in the pool it says so and the id stays pending.
#[test]
fn an_rcli_test_that_never_started_says_why_by_the_provider_name() {
    for (provider, name) in [
        ("openai", "OpenAI API"),
        ("anthropic", "Anthropic API"),
        ("openrouter", "OpenRouter"),
    ] {
        assert_eq!(
            rcli_not_started(provider, NotStarted::NoAccount),
            format!("No set-up {name} account in this pool.")
        );
        let reasons: Vec<String> = [
            NotStarted::NotInstalled,
            NotStarted::NoAccount,
            NotStarted::Paused,
            NotStarted::CouldNotStart,
        ]
        .into_iter()
        .map(|why| rcli_not_started(provider, why))
        .collect();
        for reason in &reasons {
            assert!(
                !reason.is_empty() && reason.len() <= REASON_BYTES_MAX,
                "{reason}"
            );
        }
        assert!(reasons[2].contains(name) && reasons[3].contains(name));
        let mut distinct = reasons.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), reasons.len());
    }
}

/// One pool member account for the RCLI pick rows: a connection of a harness, provider and
/// protocol, keyed or not. Inputs: the id, harness, provider, protocol and whether it holds
/// a key. Output: the connection.
fn member(
    id: &str,
    harness: &str,
    provider: &str,
    protocol: &str,
    keyed: bool,
) -> crate::config::ModelConnection {
    use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
    let reference = |text: &str| CredentialRef::new(text).unwrap();
    ModelConnection {
        id: reference(id),
        provider: reference(provider),
        harness: reference(harness),
        protocol: reference(protocol),
        account_ref: reference(&format!("{id}-account")),
        endpoint: ModelEndpoint::Http {
            url: "https://llm.example.com/v1".into(),
        },
        model: ModelReference {
            id: "own-model".into(),
            author: reference(provider),
        },
        auth: if keyed {
            ModelAuthentication::ApiKey {
                credential_ref: reference(&format!("{id}-key")),
            }
        } else {
            ModelAuthentication::None {}
        },
    }
}

/// DEC-150 addendum 9e (leader 19:01): an RCLI test runs on the pool's first account of the
/// id's keyed provider that is set up and not paused. A subscription or URL account never
/// runs it; with every set-up one paused it says so, and with none it says no account.
#[test]
fn an_rcli_test_runs_on_the_pool_s_first_set_up_account_of_the_id_s_provider() {
    let openai = member("openai-a", "rcli", "openai", "openai-chat", true);
    let router_unset = member("router-a", "rcli", "openrouter", "openai-chat", true);
    let router = member("router-b", "rcli", "openrouter", "openai-chat", true);
    let anthropic = member(
        "anthropic-a",
        "rcli",
        "anthropic",
        "anthropic-messages",
        true,
    );
    let url = member("url-a", "rcli", "openai-compatible", "openai-chat", true);
    let codex = member("codex-a", "codex", "openai", "native", false);
    let members = [&codex, &url, &openai, &router_unset, &router, &anthropic];
    let set_up = |connection: &crate::config::ModelConnection| connection.id != router_unset.id;
    let never = |_: &crate::config::ModelConnection| false;
    let picked = |provider: &str| {
        rcli_pick(&members, provider, set_up, never).map(|connection| connection.id.as_str())
    };
    assert_eq!(picked("openrouter"), Ok("router-b"));
    assert_eq!(picked("openai"), Ok("openai-a"));
    assert_eq!(picked("anthropic"), Ok("anthropic-a"));
    let only_url = [&codex, &url];
    assert_eq!(
        rcli_pick(&only_url, "openai", set_up, never).map(|c| c.id.as_str()),
        Err(NotStarted::NoAccount)
    );
    let routers = [&router_unset, &router];
    assert_eq!(
        rcli_pick(&routers, "openrouter", |_| false, never).map(|c| c.id.as_str()),
        Err(NotStarted::NoAccount)
    );
    assert_eq!(
        rcli_pick(
            &routers,
            "openrouter",
            |_| true,
            |c| c.id == router_unset.id
        )
        .map(|c| c.id.as_str()),
        Ok("router-b")
    );
    assert_eq!(
        rcli_pick(&routers, "openrouter", |_| true, |_| true).map(|c| c.id.as_str()),
        Err(NotStarted::Paused)
    );
    let unkeyed = member("router-c", "rcli", "openrouter", "openai-chat", false);
    assert_eq!(
        rcli_pick(&[&unkeyed], "openrouter", |_| true, never).map(|c| c.id.as_str()),
        Err(NotStarted::NoAccount)
    );
}
