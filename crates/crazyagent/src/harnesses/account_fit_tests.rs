use super::{AccountKind, account_fits, endpoint_base_url, spec};
use crate::accounts::{AccountDraft, Field, Provider, SignInMethod, catalog};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint};
use crate::coord::settings::HarnessKind;
use crate::protocol::CredentialRef;

const NONCE: &str = "0123456789abcdef0123";

/// Builds every account the add-account screens can make: each provider, each of its
/// sign-in methods, and for a method whose key is optional one account with a key and one
/// without. Output: the catalog kind each account counts as, with its stored connection.
fn every_account() -> Vec<(Provider, SignInMethod, AccountKind, ModelConnection)> {
    let mut accounts = Vec::new();
    for entry in catalog() {
        for method in &entry.methods {
            let keys: &[bool] = match method.key {
                Field::Hidden => &[false],
                Field::Required => &[true],
                Field::Optional => &[true, false],
            };
            for &key in keys {
                let draft = AccountDraft {
                    provider: entry.provider,
                    method: method.method,
                    name: String::new(),
                    url: (method.url == Field::Required && method.url_preset.is_none())
                        .then(|| "https://models.example.invalid/v1".to_owned()),
                    model: (method.model != Field::Hidden).then(|| "some-model".to_owned()),
                };
                let (connection, _) = draft.build(key, NONCE).expect("catalog account builds");
                let kind = match (method.method, entry.provider) {
                    (SignInMethod::Subscription, _) => AccountKind::Subscription,
                    (SignInMethod::ApiKey, _) => AccountKind::ApiKey,
                    (SignInMethod::Url, Provider::Local) => AccountKind::Local,
                    (SignInMethod::Url, _) => AccountKind::OpenAiCompatibleUrl,
                };
                accounts.push((entry.provider, method.method, kind, connection));
            }
        }
    }
    assert!(accounts.len() >= 10);
    accounts
}

/// The owner-visible answer DEC-141 asks for: which harness runs on which account. Inputs:
/// the harness kind, the provider and the sign-in method. Output: whether it runs there.
fn runs_on(harness: HarnessKind, provider: Provider, method: SignInMethod) -> bool {
    use Provider::{AnthropicApi, Chatgpt, Claude};
    match (harness, provider, method) {
        (HarnessKind::Codex, Chatgpt, SignInMethod::Subscription) => true,
        (HarnessKind::ClaudeCode, Claude, SignInMethod::Subscription) => true,
        (_, _, SignInMethod::Subscription) => false,
        (HarnessKind::Rcli, ..) => true,
        (HarnessKind::ClaudeCode, Claude | AnthropicApi, SignInMethod::ApiKey) => true,
        (HarnessKind::ClaudeCode, ..) => false,
        (HarnessKind::Codex, Claude | AnthropicApi, _) => false,
        (HarnessKind::Codex, ..) => true,
    }
}

#[test]
fn every_harness_kind_runs_on_the_same_accounts_its_catalog_entry_names() {
    let accounts = every_account();
    for harness in HarnessKind::ALL {
        let id = harness.as_str();
        let listed = spec(id)
            .expect("every harness kind is in the catalog")
            .account_kinds;
        for (provider, method, _, connection) in &accounts {
            assert_eq!(
                account_fits(id, connection),
                runs_on(harness, *provider, *method),
                "{id} on {provider:?} {method:?} {:?}",
                connection.auth
            );
        }
        for kind in [
            AccountKind::Subscription,
            AccountKind::ApiKey,
            AccountKind::OpenAiCompatibleUrl,
            AccountKind::Local,
        ] {
            let fits = accounts
                .iter()
                .any(|(_, _, of, connection)| *of == kind && account_fits(id, connection));
            assert_eq!(listed.contains(&kind), fits, "{id} works with {kind:?}");
        }
    }
}

#[test]
fn an_endpoint_account_fits_by_its_wire_format_whatever_harness_it_was_stored_with() {
    for (_, _, _, connection) in every_account() {
        if !matches!(connection.endpoint, ModelEndpoint::Http { .. }) {
            continue;
        }
        let answers = |stored: &str| {
            let mut moved = connection.clone();
            moved.harness = CredentialRef::new(stored).unwrap();
            HarnessKind::ALL.map(|harness| account_fits(harness.as_str(), &moved))
        };
        assert_eq!(answers("rcli"), answers("codex"));
        assert_eq!(answers("rcli"), answers("claude"));
    }
}

#[test]
fn claude_code_gets_an_anthropic_url_without_the_v1_it_adds_itself() {
    let stored = "https://api.anthropic.com/v1";
    assert_eq!(
        endpoint_base_url("claude-code", stored),
        "https://api.anthropic.com"
    );
    assert_eq!(
        endpoint_base_url("claude", "https://gateway.example.invalid/anthropic/v1/"),
        "https://gateway.example.invalid/anthropic"
    );
    assert_eq!(
        endpoint_base_url("claude-code", "https://gateway.example.invalid/anthropic"),
        "https://gateway.example.invalid/anthropic"
    );
    for harness in ["codex", "rcli"] {
        assert_eq!(endpoint_base_url(harness, stored), stored);
    }
    assert_eq!(
        endpoint_base_url("codex", "https://api.openai.com/v1/"),
        "https://api.openai.com/v1"
    );
}

#[test]
fn a_sign_in_fits_only_its_own_harness_and_unknown_harnesses_fit_nothing() {
    let accounts = every_account();
    let native: Vec<_> = accounts
        .iter()
        .filter(|(.., connection)| connection.endpoint == ModelEndpoint::Native {})
        .collect();
    assert_eq!(native.len(), 2);
    for (provider, .., connection) in native {
        let own = if *provider == Provider::Chatgpt {
            "codex"
        } else {
            "claude"
        };
        assert!(account_fits(own, connection));
        assert_eq!(
            account_fits("claude", connection),
            account_fits("claude-code", connection)
        );
        let mut other = connection.clone();
        other.harness = CredentialRef::new("rcli").unwrap();
        assert!(
            HarnessKind::ALL
                .iter()
                .all(|h| !account_fits(h.as_str(), &other))
        );
    }
    for (.., connection) in &accounts {
        assert!(!account_fits("opencode", connection));
        assert!(!account_fits("no-such-harness", connection));
        let mut oauth = connection.clone();
        oauth.auth = ModelAuthentication::Oauth {
            credential_ref: CredentialRef::new("oauth-ref").unwrap(),
        };
        assert!(
            HarnessKind::ALL
                .iter()
                .all(|h| !account_fits(h.as_str(), &oauth))
        );
    }
}
