use super::*;
use crate::config::{AccountPool, ModelReference};

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

/// One endpoint account as the add-account screen stores it, under the RCLI harness name.
/// Inputs: the id, URL, wire format and the key reference, none for a keyless one.
/// Output: the stored connection.
fn endpoint(id: &str, url: &str, protocol: &str, key: Option<&str>) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference(protocol),
        account_ref: reference("owner-account"),
        endpoint: ModelEndpoint::Http { url: url.into() },
        model: ModelReference {
            id: "some-model".into(),
            author: reference("openai-compatible"),
        },
        auth: match key {
            Some(key) => ModelAuthentication::ApiKey {
                credential_ref: reference(key),
            },
            None => ModelAuthentication::None {},
        },
    }
}

/// The model access of one project whose pool holds only `member`.
/// Input: the member's id. Output: the access.
fn pool_of(member: &str) -> ModelAccess {
    ModelAccess {
        accounts: vec![reference("owner-account")],
        connections: vec![
            endpoint(
                "openai-keyed",
                "https://api.openai.com/v1",
                "openai-chat",
                Some("openai-key"),
            ),
            endpoint(
                "anthropic-keyed",
                "https://api.anthropic.com/v1",
                "anthropic-messages",
                Some("anthropic-key"),
            ),
            endpoint("local", "http://127.0.0.1:11434/v1", "openai-chat", None),
        ],
        pools: vec![AccountPool {
            id: reference("research-pool"),
            name: None,
            members: vec![reference(member)],
            projects: vec![reference("research-project")],
            roles: vec![reference("project_leader")],
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

fn picked(member: &str, harness: &str) -> AccountPick {
    pick_account(&pool_of(member), "research-project", harness, |_| true)
}

fn account(member: &str, account: ResearchAccount) -> AccountPick {
    AccountPick::Account {
        connection: reference(member),
        account: Box::new(account),
    }
}

#[test]
fn research_runs_on_endpoint_accounts_with_or_without_a_key_by_wire_format() {
    assert_eq!(
        picked("openai-keyed", "codex"),
        account(
            "openai-keyed",
            ResearchAccount::Endpoint {
                base_url: "https://api.openai.com/v1".into(),
                model: "some-model".into(),
                key: Some(reference("openai-key")),
            }
        )
    );
    assert_eq!(
        picked("anthropic-keyed", "claude-code"),
        account(
            "anthropic-keyed",
            ResearchAccount::AnthropicEndpoint {
                base_url: "https://api.anthropic.com".into(),
                model: "some-model".into(),
                key: Some(reference("anthropic-key")),
            }
        ),
        "Claude Code adds /v1/messages itself"
    );
    assert_eq!(
        picked("local", "codex"),
        account(
            "local",
            ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "some-model".into(),
                key: None,
            }
        )
    );
    assert_eq!(
        picked("anthropic-keyed", "codex"),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        picked("openai-keyed", "claude-code"),
        AccountPick::NoUsableAccount
    );
    assert_eq!(picked("local", "claude-code"), AccountPick::NoUsableAccount);
}

/// One wait stands for several when no member can run now: a down provider's line beats a
/// pause, the first line found stays, two pauses keep the earlier end and a pause beats
/// having no usable account.
#[test]
fn a_down_providers_line_outranks_a_pause_and_the_first_line_found_is_kept() {
    let line = |text: &str| AccountPick::Unreached { line: text.into() };
    let paused = |until_ms| AccountPick::Paused { until_ms };
    assert_eq!(paused(900).or_wait(line("first")), line("first"));
    assert_eq!(line("first").or_wait(paused(400)), line("first"));
    assert_eq!(line("first").or_wait(line("second")), line("first"));
    assert_eq!(paused(900).or_wait(paused(400)), paused(400));
    assert_eq!(
        AccountPick::NoUsableAccount.or_wait(paused(900)),
        paused(900)
    );
    assert_eq!(
        AccountPick::NoUsableAccount.or_wait(AccountPick::NoUsableAccount),
        AccountPick::NoUsableAccount
    );
}

/// A read-only pick whose only reachable member is paused reports the down member's line,
/// as the pick did before failover, when it ran on the down member and failed at once; the
/// plain pick still gives the down member.
#[cfg(target_os = "macos")]
#[test]
fn a_read_only_pick_with_the_reachable_member_paused_keeps_the_down_providers_line() {
    let (_held, down) = crate::spawn_gate::refusing_tcp_port().unwrap();
    let up = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let up_port = up.local_addr().unwrap().port();
    let access = ModelAccess {
        accounts: vec![reference("owner-account")],
        connections: vec![
            endpoint(
                "down",
                &format!("http://127.0.0.1:{down}/v1"),
                "openai-chat",
                None,
            ),
            endpoint(
                "up",
                &format!("http://127.0.0.1:{up_port}/v1"),
                "openai-chat",
                None,
            ),
        ],
        pools: vec![],
    };
    let members = [reference("down"), reference("up")];
    let up_paused = |id: &CredentialRef| (id.as_str() == "up").then_some(5_000);
    let pick = |reach| {
        members_pick(
            &access,
            &members,
            "codex",
            &|_: &ModelConnection| false,
            &up_paused,
            &Choice::default(),
            reach,
        )
    };
    assert_eq!(
        pick(Reach::Reachable),
        AccountPick::Unreached {
            line: format!(
                "Can't reach the model provider at 127.0.0.1:{down}. Start it or pick another account in this pool, then try again."
            ),
        }
    );
    assert!(matches!(
        pick(Reach::Unprobed),
        AccountPick::Account { connection, .. } if connection.as_str() == "down"
    ));
}
