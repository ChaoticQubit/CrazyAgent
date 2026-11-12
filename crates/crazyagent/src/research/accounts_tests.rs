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
