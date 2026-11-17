use super::{
    AccountKind, KEY_RELAYED, account_fits, account_kinds_here, endpoint_base_url, keyed_runs_here,
    spec,
};
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

/// The owner-visible answer DEC-141 asks for: which harness runs on which account; a Gemini
/// API key, in its own wire format, runs on none of the three (Task 17, Task 21). Inputs:
/// the harness kind, the provider and the sign-in method. Output: whether it runs there.
fn runs_on(harness: HarnessKind, provider: Provider, method: SignInMethod) -> bool {
    use Provider::{AnthropicApi, Chatgpt, Claude, Gemini};
    match (harness, provider, method) {
        (HarnessKind::Codex, Chatgpt, SignInMethod::Subscription) => true,
        (HarnessKind::ClaudeCode, Claude, SignInMethod::Subscription) => true,
        (_, _, SignInMethod::Subscription) => false,
        (_, Gemini, _) => false,
        (HarnessKind::Rcli, ..) => true,
        (HarnessKind::ClaudeCode, Claude | AnthropicApi, SignInMethod::ApiKey) => true,
        (HarnessKind::ClaudeCode, ..) => false,
        (HarnessKind::Codex, Claude | AnthropicApi, _) => false,
        (HarnessKind::Codex, ..) => true,
        (HarnessKind::Listed(id), ..) => unreachable!("{id} is not a native kind"),
    }
}

#[test]
fn every_harness_kind_runs_on_the_same_accounts_its_catalog_entry_names() {
    let accounts = every_account();
    for harness in HarnessKind::ALL {
        let id = harness.as_str();
        let listed = account_kinds_here(spec(id).expect("every harness kind is in the catalog"));
        for (provider, method, _, connection) in &accounts {
            let keyed = matches!(connection.auth, ModelAuthentication::ApiKey { .. });
            assert_eq!(
                account_fits(id, connection),
                runs_on(harness, *provider, *method) && (!keyed || keyed_runs_here(id)),
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

#[test]
fn a_sign_in_never_fits_a_harness_whose_manifest_lists_no_subscription_account() {
    let sign_ins: Vec<ModelConnection> = every_account()
        .into_iter()
        .map(|(.., connection)| connection)
        .filter(|connection| connection.endpoint == ModelEndpoint::Native {})
        .collect();
    assert_eq!(sign_ins.len(), 2);
    let mut without = 0;
    for harness in super::registry::registry().all() {
        let id = harness.id();
        if spec(id)
            .unwrap()
            .account_kinds
            .contains(&AccountKind::Subscription)
        {
            continue;
        }
        without += 1;
        for connection in &sign_ins {
            let mut moved = connection.clone();
            moved.harness = CredentialRef::new(id).unwrap();
            assert!(!harness.account_fits(&moved), "{id}");
            assert!(!account_fits(id, &moved), "{id}");
        }
    }
    assert!(without >= 2, "RCLI and fx list no subscription account");
}

/// A one-harness manifest for the fit rows below. Inputs: its id, its piped driver, the one
/// wire format it lists and its `accounts` block text. Output: the manifest text.
fn fit_manifest(id: &str, driver: &str, protocol: &str, accounts: &str) -> String {
    format!(
        concat!(
            "schema: 1\nkind: harness\nid: \"{id}\"\nname: \"{id}\"\n",
            "binaries:\n  - \"{id}\"\nheadless: \"{id} for the fit rows\"\n",
            "model_selection: true\naccount_kinds:\n  - \"api_key\"\n",
            "  - \"open_ai_compatible_url\"\n  - \"local\"\n",
            "roles:\n  - \"agent\"\nsub_agents: \"app\"\npiped:\n  driver: \"{driver}\"\n",
            "account_protocols:\n  - \"{protocol}\"\n{accounts}",
        ),
        id = id,
        driver = driver,
        protocol = protocol,
        accounts = accounts
    )
}

/// The `accounts` block a listed fit-row harness needs for one wire format.
fn handed(protocol: &str) -> String {
    format!("accounts:\n  {protocol}:\n    env:\n      ROW_KEY: \"{{token}}\"\n")
}

/// Task 17 (DEC-180 addendum 2, answer 3): a Gemini API key account fits a harness whose
/// manifest lists the `gemini` wire format, whatever harness the account was stored with,
/// and no other: not Codex, Claude Code or RCLI, not a harness of the OpenAI format; `*`
/// alone still stands for every format.
#[test]
fn a_gemini_api_key_fits_only_a_harness_that_lists_the_gemini_wire_format() {
    let gemini: Provider =
        serde_json::from_value(serde_json::json!("gemini")).expect("the pool has a Gemini kind");
    let (connection, _) = AccountDraft {
        provider: gemini,
        method: SignInMethod::ApiKey,
        name: String::new(),
        url: None,
        model: Some("gemini-2.5-pro".to_owned()),
    }
    .build(true, NONCE)
    .expect("a Gemini key account builds");
    for harness in HarnessKind::ALL {
        assert!(
            !account_fits(harness.as_str(), &connection),
            "{}",
            harness.as_str()
        );
    }
    for harness in super::registry::registry().all() {
        assert_eq!(
            harness.account_fits(&connection),
            harness.speaks("gemini"),
            "{}",
            harness.id()
        );
    }
    let built = super::registry::Registry::build(&[
        (
            "gemini-row",
            &fit_manifest("gemini-row", "jsonl-turn", "gemini", &handed("gemini")),
        ),
        (
            "openai-row",
            &fit_manifest(
                "openai-row",
                "jsonl-turn",
                "openai-chat",
                &handed("openai-chat"),
            ),
        ),
        (
            "star-row",
            &fit_manifest("star-row", "codex-app-server", "*", ""),
        ),
    ])
    .expect("fit row manifests load");
    let fits =
        |id: &str, connection: &ModelConnection| built.get(id).unwrap().account_fits(connection);
    assert!(fits("gemini-row", &connection));
    assert!(!fits("openai-row", &connection));
    assert!(fits("star-row", &connection));
    assert!(built.get("star-row").unwrap().speaks(""));
    let mut moved = connection.clone();
    moved.harness = CredentialRef::new("codex").unwrap();
    assert!(fits("gemini-row", &moved));
    assert!(!fits("openai-row", &moved));
}

/// Task 17: every endpoint account the Add account screens can make fits a listed harness
/// that lists the wire format the account stores, and no listed harness that lists another.
/// A sign-in fits none of these: no endpoint account stores `openai-responses`, the one
/// format a ChatGPT sign-in fits through the ChatGPT relay (T22).
#[test]
fn every_endpoint_account_fits_a_listed_harness_by_the_wire_format_it_stores() {
    let accounts = every_account();
    let formats: std::collections::BTreeSet<String> = accounts
        .iter()
        .filter(|(.., connection)| connection.endpoint != ModelEndpoint::Native {})
        .map(|(.., connection)| connection.protocol.as_str().to_owned())
        .collect();
    assert!(formats.len() >= 2);
    for format in &formats {
        let built = super::registry::Registry::build(&[(
            "format-row",
            &fit_manifest("format-row", "jsonl-turn", format, &handed(format)),
        )])
        .expect("format row manifest loads");
        let harness = &built.all()[0];
        for (provider, method, _, connection) in &accounts {
            let expected = connection.endpoint != ModelEndpoint::Native {}
                && connection.protocol.as_str() == format;
            assert_eq!(
                harness.account_fits(connection),
                expected,
                "{format} on {provider:?} {method:?}"
            );
        }
    }
}

fn native_sign_in(provider: Provider) -> ModelConnection {
    every_account()
        .into_iter()
        .find(|(found, method, ..)| *found == provider && *method == SignInMethod::Subscription)
        .map(|(.., connection)| connection)
        .expect("the provider has a sign-in")
}

/// T22 R6 (DEC-180 addendum 2, answer 2): a ChatGPT sign-in fits Codex, as it always did,
/// and a listed harness that lists the `openai-responses` wire format, through the ChatGPT
/// relay; not a listed harness of another format, not another own adapter, and not once it
/// is stored with another harness. A Claude subscription still fits Claude Code only, and
/// an OpenAI API key still fits by the wire format it stores.
#[test]
fn a_chatgpt_sign_in_fits_a_listed_harness_that_lists_openai_responses_and_no_other() {
    let chatgpt = native_sign_in(Provider::Chatgpt);
    let claude = native_sign_in(Provider::Claude);
    let built = super::registry::Registry::build(&[
        (
            "responses-row",
            &fit_manifest(
                "responses-row",
                "jsonl-turn",
                "openai-responses",
                &handed("openai-responses"),
            ),
        ),
        (
            "chat-row",
            &fit_manifest(
                "chat-row",
                "jsonl-turn",
                "openai-chat",
                &handed("openai-chat"),
            ),
        ),
        (
            "own-row",
            &fit_manifest("own-row", "codex-app-server", "openai-responses", ""),
        ),
    ])
    .expect("ChatGPT fit row manifests load");
    let fits =
        |id: &str, connection: &ModelConnection| built.get(id).unwrap().account_fits(connection);
    assert!(fits("responses-row", &chatgpt));
    assert!(!fits("chat-row", &chatgpt));
    assert!(!fits("own-row", &chatgpt));
    for id in ["responses-row", "chat-row", "own-row"] {
        assert!(!fits(id, &claude), "{id}");
    }
    assert!(account_fits("codex", &chatgpt));
    assert!(!account_fits("claude-code", &chatgpt));
    assert!(account_fits("claude-code", &claude));
    assert!(!account_fits("codex", &claude));
    let mut moved = chatgpt.clone();
    moved.harness = CredentialRef::new("responses-row").unwrap();
    assert!(!fits("responses-row", &moved));
    let (openai_key, _) = AccountDraft {
        provider: Provider::OpenaiApi,
        method: SignInMethod::ApiKey,
        name: String::new(),
        url: None,
        model: Some("gpt-5".to_owned()),
    }
    .build(true, NONCE)
    .expect("an OpenAI key account builds");
    assert!(fits("chat-row", &openai_key));
    assert!(!fits("responses-row", &openai_key));
    for harness in super::registry::registry().all() {
        let expected = harness.id() == "codex"
            || harness
                .account_protocols()
                .iter()
                .any(|listed| listed == "openai-responses");
        assert_eq!(harness.account_fits(&chatgpt), expected, "{}", harness.id());
    }
}

/// T22 (tokens never reach agents): only the harness a sign-in was made with runs on that
/// sign-in's own credential copy. A listed harness a ChatGPT sign-in fits through the relay
/// does not, so a team never hands it the copy; endpoint accounts have no copy at all.
#[test]
fn only_the_harness_a_sign_in_was_made_with_runs_on_its_credential_copy() {
    let chatgpt = native_sign_in(Provider::Chatgpt);
    let claude = native_sign_in(Provider::Claude);
    let built = super::registry::Registry::build(&[(
        "responses-row",
        &fit_manifest(
            "responses-row",
            "jsonl-turn",
            "openai-responses",
            &handed("openai-responses"),
        ),
    )])
    .expect("ChatGPT fit row manifest loads");
    let listed = built.get("responses-row").unwrap();
    assert!(listed.account_fits(&chatgpt));
    assert!(!listed.signs_in_natively(&chatgpt));
    let bundled = super::registry::registry();
    let (codex, claude_code) = (
        bundled.get("codex").unwrap(),
        bundled.get("claude-code").unwrap(),
    );
    assert!(codex.signs_in_natively(&chatgpt));
    assert!(claude_code.signs_in_natively(&claude));
    assert!(!codex.signs_in_natively(&claude));
    assert!(!claude_code.signs_in_natively(&chatgpt));
    for (.., connection) in every_account()
        .into_iter()
        .filter(|(.., connection)| connection.endpoint != ModelEndpoint::Native {})
    {
        for harness in bundled.all().iter().chain([listed]) {
            assert!(
                !harness.signs_in_natively(&connection),
                "{}",
                harness.manifest().id
            );
        }
    }
}

#[test]
fn a_relayed_harness_is_offered_keyed_accounts_only_where_the_key_relay_starts() {
    let here = crate::model_gateway::relay::available();
    assert_eq!(here, cfg!(unix));
    let keyed: Vec<_> = every_account()
        .into_iter()
        .filter(|(.., connection)| matches!(connection.auth, ModelAuthentication::ApiKey { .. }))
        .collect();
    assert!(!keyed.is_empty());
    let listed = super::catalog(&super::Search::new(Vec::new(), None, Vec::new()));
    for id in KEY_RELAYED {
        let harness = spec(id).unwrap();
        assert!(harness.account_kinds.contains(&AccountKind::ApiKey));
        assert_eq!(keyed_runs_here(id), here);
        assert_eq!(
            account_kinds_here(harness).contains(&AccountKind::ApiKey),
            here
        );
        let entry = listed.iter().find(|entry| entry.id == id).unwrap();
        assert_eq!(entry.account_kinds, account_kinds_here(harness));
        if !here {
            assert!(
                keyed
                    .iter()
                    .all(|(.., connection)| !account_fits(id, connection))
            );
        }
    }
    let rcli = spec("rcli").unwrap();
    assert!(keyed_runs_here("rcli"));
    assert_eq!(account_kinds_here(rcli), rcli.account_kinds);
    for (provider, method, _, connection) in &keyed {
        assert_eq!(
            account_fits("rcli", connection),
            runs_on(HarnessKind::Rcli, *provider, *method)
        );
    }
}
