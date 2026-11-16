use super::{handover, no_account_line};
use crate::config::ModelConnection;
use crate::credentials::SecretString;
use crate::harnesses::manifest;
use crate::harnesses::registry::Registry;
use crate::model_gateway::relay::{LOCAL_PLACEHOLDER, Relay, is_relay_token};

const SENTINEL: &str = "sk-sentinel-saved-key-0123456789abcdef";
const GEMINI_URL: &str = "https://generativelanguage.googleapis.com";
const GEMINI_ENV: &str = concat!(
    "accounts:\n",
    "  gemini:\n",
    "    env:\n",
    "      GOOGLE_GEMINI_BASE_URL: \"{relay_url}\"\n",
    "      GEMINI_API_KEY: \"{token}\"\n",
    "      GEMINI_MODEL: \"{model}\"\n",
);
const OPENAI_ENV: &str = concat!(
    "accounts:\n",
    "  openai-chat:\n",
    "    env:\n",
    "      OPENAI_BASE_URL: \"{relay_url}\"\n",
    "      OPENAI_API_KEY: \"{token}\"\n",
);

/// The manifest of a listed harness on a shared driver for these rows. Inputs: the wire
/// formats it lists under `account_protocols`, none for no such key, and the text of its
/// `accounts` block, empty for none. Output: the manifest text.
fn listed(protocols: &[&str], accounts: &str) -> String {
    let mut text = concat!(
        "schema: 1\nkind: harness\nid: \"rowharness\"\nname: \"Row Harness\"\n",
        "binaries:\n  - \"rowharness\"\nheadless: \"rowharness for these rows\"\n",
        "model_selection: true\naccount_kinds:\n  - \"api_key\"\n",
        "  - \"open_ai_compatible_url\"\n  - \"local\"\n",
        "roles:\n  - \"agent\"\nsub_agents: \"app\"\npiped:\n  driver: \"jsonl-turn\"\n",
    )
    .to_owned();
    if !protocols.is_empty() {
        text.push_str("account_protocols:\n");
        for protocol in protocols {
            text.push_str(&format!("  - \"{protocol}\"\n"));
        }
    }
    text.push_str(accounts);
    text
}

fn built(protocols: &[&str], accounts: &str) -> Registry {
    Registry::build(&[("rowharness", &listed(protocols, accounts))]).expect("row manifest loads")
}

fn refusal(protocols: &[&str], accounts: &str) -> (&'static str, String) {
    let refused = Registry::build(&[("rowharness", &listed(protocols, accounts))])
        .expect_err("row manifest is refused");
    (refused.code, refused.message)
}

/// One pool account as the engine stores it. Inputs: its wire format, its address and
/// whether a key was saved with it. Output: the connection.
fn account(protocol: &str, url: &str, keyed: bool) -> ModelConnection {
    let auth = if keyed {
        serde_json::json!({"method": "api_key", "credential_ref": "row-key"})
    } else {
        serde_json::json!({"method": "none"})
    };
    serde_json::from_value(serde_json::json!({
        "id": "row-account",
        "provider": "google",
        "harness": "rcli",
        "protocol": protocol,
        "account_ref": "row-login",
        "endpoint": {"kind": "http", "url": url},
        "model": {"id": "gemini-2.5-pro", "author": "google"},
        "auth": auth,
    }))
    .unwrap()
}

fn sign_in() -> ModelConnection {
    serde_json::from_value(serde_json::json!({
        "id": "row-account",
        "provider": "google",
        "harness": "rowharness",
        "protocol": "native",
        "account_ref": "row-login",
        "endpoint": {"kind": "native"},
        "model": {"id": "native-default", "author": "google"},
        "auth": {"method": "native_managed", "profile_ref": "row-profile"},
    }))
    .unwrap()
}

fn sentinel_relay(upstream: &str) -> Relay {
    Relay::start(
        upstream,
        SecretString::new(SENTINEL.as_bytes().to_vec()).unwrap(),
    )
    .expect("relay starts")
}

/// Task 17: a listed harness on a keyed account gets the engine relay's loopback URL and
/// the relay's placeholder token, filled into the environment its manifest names; the saved
/// key is in no value it is given and in nothing the hand-over prints.
#[test]
fn a_keyed_account_is_handed_over_as_the_relay_url_and_its_placeholder_and_never_the_key() {
    let registry = built(&["gemini"], GEMINI_ENV);
    let harness = &registry.all()[0];
    let connection = account("gemini", GEMINI_URL, true);
    let relay = sentinel_relay(GEMINI_URL);
    let given = handover(harness, &connection, Some(&relay)).expect("keyed account handed over");
    assert_eq!(given.base_url(), relay.base_url());
    assert!(given.base_url().starts_with("http://127.0.0.1:"));
    assert_eq!(given.token(), relay.token());
    assert!(is_relay_token(given.token()));
    assert_eq!(given.model(), "gemini-2.5-pro");
    assert_eq!(
        given.env(),
        [
            ("GOOGLE_GEMINI_BASE_URL".to_owned(), relay.base_url()),
            ("GEMINI_API_KEY".to_owned(), relay.token().to_owned()),
            ("GEMINI_MODEL".to_owned(), "gemini-2.5-pro".to_owned()),
        ]
    );
    let shown = format!("{given:?}");
    assert!(!shown.contains(relay.token()), "{shown}");
    let mut seen = vec![
        given.base_url().to_owned(),
        given.token().to_owned(),
        given.model().to_owned(),
        shown,
    ];
    seen.extend(
        given
            .env()
            .iter()
            .flat_map(|(name, value)| [name.clone(), value.clone()]),
    );
    for text in &seen {
        assert!(!text.contains(SENTINEL), "{text}");
        assert!(!text.contains(GEMINI_URL), "{text}");
    }
}

/// Task 17: a keyless endpoint, such as a local model, is handed over as its own address
/// and the local placeholder; no relay runs for it.
#[test]
fn a_keyless_endpoint_is_handed_over_as_its_own_address_and_the_local_placeholder() {
    let registry = built(&["openai-chat"], OPENAI_ENV);
    let harness = &registry.all()[0];
    let connection = account("openai-chat", "http://127.0.0.1:11434/v1", false);
    let given = handover(harness, &connection, None).expect("keyless account handed over");
    assert_eq!(given.base_url(), "http://127.0.0.1:11434/v1");
    assert_eq!(given.token(), LOCAL_PLACEHOLDER);
    assert_eq!(
        given.env(),
        [
            (
                "OPENAI_BASE_URL".to_owned(),
                "http://127.0.0.1:11434/v1".to_owned()
            ),
            ("OPENAI_API_KEY".to_owned(), LOCAL_PLACEHOLDER.to_owned()),
        ]
    );
}

/// Task 17: no hand-over for an account the harness cannot run, for a sign-in a listed
/// harness does not take, for a keyed account without its relay, or with a relay a keyless
/// account does not need.
#[test]
fn a_hand_over_is_refused_for_an_account_the_harness_cannot_run_or_a_key_without_its_relay() {
    let registry = built(&["gemini"], GEMINI_ENV);
    let harness = &registry.all()[0];
    let code = |connection: &ModelConnection, relay: Option<&Relay>| {
        handover(harness, connection, relay).unwrap_err().0
    };
    assert_eq!(
        code(&account("gemini", GEMINI_URL, true), None),
        "account_key_relay_missing"
    );
    let relay = sentinel_relay(GEMINI_URL);
    assert_eq!(
        code(
            &account("gemini", "http://127.0.0.1:8080", false),
            Some(&relay)
        ),
        "account_relay_unexpected"
    );
    assert_eq!(
        code(
            &account("openai-chat", "https://api.openai.com/v1", true),
            Some(&relay)
        ),
        "account_not_compatible"
    );
    assert_eq!(code(&sign_in(), None), "account_not_compatible");
}

/// Task 17: the `accounts` block hands over only the relay URL, the placeholder and the
/// model, through plain environment names, none of those the load check refuses (search
/// path, home, temp folder, shell, locale, git, proxy and the engine's own; the launch
/// keeps every other engine pin by setting it last), one entry for each wire format the
/// manifest lists by its full name, and every refusal names the setting.
#[test]
fn an_accounts_block_hands_over_only_the_relay_url_the_placeholder_and_the_model() {
    built(&["gemini"], GEMINI_ENV);
    built(&[], "");
    let env = |name: &str, value: &str| {
        format!("accounts:\n  gemini:\n    env:\n      {name}: \"{value}\"\n")
    };
    assert_eq!(
        refusal(&["gemini"], &env("gemini_key", "{token}")),
        (
            manifest::VALUE,
            "rowharness: accounts.gemini.env: gemini_key is not a variable name of capital letters, digits and _.".to_owned()
        )
    );
    for owned in [
        "PATH",
        "HOME",
        "TMPDIR",
        "SHELL",
        "TERM",
        "LANG",
        "TZ",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "DYLD_INSERT_LIBRARIES",
        "LD_PRELOAD",
        "GIT_CONFIG_GLOBAL",
        "LC_ALL",
        "CRAZYAGENT_WARDEN_FD",
    ] {
        assert_eq!(
            refusal(&["gemini"], &env(owned, "{token}")),
            (
                manifest::VALUE,
                format!(
                    "rowharness: accounts.gemini.env: {owned} is not a variable an account entry may set."
                )
            )
        );
    }
    for other in ["home", "session", "prompt", "cwd", "key_file"] {
        assert_eq!(
            refusal(&["gemini"], &env("GEMINI_DIR", &format!("{{{other}}}/x"))),
            (
                manifest::PLACEHOLDER,
                format!(
                    "rowharness: accounts.gemini.env.GEMINI_DIR: {{{other}}} is not a value an account hands over; use {{relay_url}}, {{token}} or {{model}}."
                )
            )
        );
    }
    assert_eq!(
        refusal(
            &["gemini"],
            "accounts:\n  gemini:\n    env:\n      GEMINI_API_KEY: 7\n"
        ),
        (
            manifest::TYPE,
            "rowharness: accounts.gemini.env.GEMINI_API_KEY: this must be text in quotes."
                .to_owned()
        )
    );
    assert_eq!(
        refusal(&["gemini"], "accounts:\n  gemini:\n    env: \"x\"\n"),
        (
            manifest::TYPE,
            "rowharness: accounts.gemini.env: this must be a block of settings.".to_owned()
        )
    );
    assert_eq!(
        refusal(&["gemini"], &format!("{GEMINI_ENV}  openai-chat:\n    env:\n      OPENAI_API_KEY: \"{{token}}\"\n")),
        (
            manifest::VALUE,
            "rowharness: accounts.openai-chat: this wire format is not listed under account_protocols.".to_owned()
        )
    );
    assert_eq!(
        refusal(&["gemini"], ""),
        (
            manifest::MISSING,
            "rowharness: accounts: gemini is listed under account_protocols and needs an entry here."
                .to_owned()
        )
    );
    assert_eq!(
        refusal(&["openai*"], OPENAI_ENV),
        (
            manifest::VALUE,
            "rowharness: account_protocols: openai* stands for many wire formats; a harness on a shared driver lists each one by its full name.".to_owned()
        )
    );
}

/// Task 17: a team start whose pool has no account for a listed harness gets one plain
/// sentence from the engine, naming each kind of account that fits it once, by provider and
/// sign-in method (rv-code T17 r1 F2: never a whole provider whose sign-in does not fit),
/// or saying that none does yet.
#[test]
fn a_listed_harness_names_the_kinds_of_account_that_fit_it_when_its_pool_has_none() {
    let line =
        |protocols: &[&str], accounts: &str| no_account_line(&built(protocols, accounts).all()[0]);
    assert_eq!(
        line(&["openai-chat"], OPENAI_ENV),
        "No account in this project's pool can run Row Harness. Accounts that fit it: an OpenAI API key, an OpenRouter API key, an OpenAI-compatible URL or a local model (Ollama)"
    );
    assert_eq!(
        line(
            &["anthropic-messages"],
            "accounts:\n  anthropic-messages:\n    env:\n      ANTHROPIC_API_KEY: \"{token}\"\n"
        ),
        "No account in this project's pool can run Row Harness. Accounts that fit it: an Anthropic API key"
    );
    assert_eq!(
        line(&[], ""),
        "No account in this project's pool can run Row Harness, and no kind of account CrazyAgent can add fits it yet"
    );
}

const CHATGPT_ACCESS: &str = "chatgpt-sentinel-access-0123456789abcdef";
const RESPONSES_ENV: &str = concat!(
    "accounts:\n",
    "  openai-responses:\n",
    "    env:\n",
    "      OPENAI_BASE_URL: \"{relay_url}\"\n",
    "      OPENAI_API_KEY: \"{token}\"\n",
    "      OPENAI_MODEL: \"{model}\"\n",
);

/// A ChatGPT sign-in as the Add account screens store it: made with Codex, native wire
/// format, the engine's managed profile and no model of its own.
fn chatgpt_sign_in() -> ModelConnection {
    serde_json::from_value(serde_json::json!({
        "id": "row-chatgpt",
        "provider": "openai",
        "harness": "codex",
        "protocol": "native",
        "account_ref": "row-chatgpt-login",
        "endpoint": {"kind": "native"},
        "model": {"id": "native-default", "author": "openai"},
        "auth": {"method": "native_managed", "profile_ref": "row-chatgpt-profile"},
    }))
    .unwrap()
}

fn chatgpt_relay() -> Relay {
    use crate::model_gateway::relay::chatgpt::{ChatGptSource, ChatGptTokens};
    let tokens = ChatGptTokens {
        access: SecretString::bearer(CHATGPT_ACCESS.as_bytes().to_vec()).unwrap(),
        account_id: Some("acct-row".to_owned()),
        expires: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            + std::time::Duration::from_secs(3600),
    };
    let source = ChatGptSource::new(
        tokens,
        Box::new(|_: std::time::Duration, _: &dyn Fn() -> bool| {
            Err(crate::engine::EngineError("no renewal expected".into()))
        }),
    );
    Relay::start_chatgpt(std::sync::Arc::new(source)).expect("ChatGPT relay starts")
}

/// T22 R7: a ChatGPT sign-in is handed to a listed harness that lists `openai-responses` as
/// the ChatGPT relay's loopback URL, its placeholder and the model chosen for the run,
/// filled into the manifest's `openai-responses` entry; the access token and the stored
/// `native-default` model are in no value it is given and in nothing it prints.
#[test]
fn a_chatgpt_sign_in_is_handed_over_as_the_chatgpt_relay_url_its_placeholder_and_the_run_model() {
    let registry = built(&["openai-responses"], RESPONSES_ENV);
    let harness = &registry.all()[0];
    let relay = chatgpt_relay();
    let given = super::chatgpt_handover(harness, &chatgpt_sign_in(), &relay, "gpt-5-codex");
    assert_eq!(
        given.as_ref().err().map(|problem| problem.0.as_str()),
        None,
        "the ChatGPT sign-in was not handed over"
    );
    let given = given.unwrap();
    assert_eq!(given.base_url(), relay.base_url());
    assert!(given.base_url().starts_with("http://127.0.0.1:"));
    assert!(given.base_url().ends_with("/backend-api/codex"));
    assert_eq!(given.token(), relay.token());
    assert!(is_relay_token(given.token()));
    assert_eq!(given.model(), "gpt-5-codex");
    assert_eq!(
        given.env(),
        [
            ("OPENAI_BASE_URL".to_owned(), relay.base_url()),
            ("OPENAI_API_KEY".to_owned(), relay.token().to_owned()),
            ("OPENAI_MODEL".to_owned(), "gpt-5-codex".to_owned()),
        ]
    );
    let shown = format!("{given:?}");
    let mut seen = vec![shown];
    seen.extend(
        given
            .env()
            .iter()
            .flat_map(|(name, value)| [name.clone(), value.clone()]),
    );
    for text in &seen {
        assert!(!text.contains(CHATGPT_ACCESS), "{text}");
        assert!(!text.contains("acct-row"), "{text}");
        assert!(!text.contains("native-default"), "{text}");
        assert!(!text.contains("chatgpt.com"), "{text}");
    }
}

/// T22 R7: a ChatGPT sign-in goes over only with a ChatGPT relay, a run model and a
/// harness that lists `openai-responses`; the Task 17 hand-over never hands it out with
/// its stored `native-default`; and a keyed account is never handed a ChatGPT relay.
#[test]
fn a_chatgpt_hand_over_needs_its_own_relay_a_run_model_and_a_responses_harness() {
    let responses = built(&["openai-responses"], RESPONSES_ENV);
    let harness = &responses.all()[0];
    let chatgpt = chatgpt_relay();
    let keyed = sentinel_relay("https://api.openai.com/v1");
    let code = |harness: &crate::harnesses::registry::Harness,
                connection: &ModelConnection,
                relay: &Relay,
                model: &str| {
        super::chatgpt_handover(harness, connection, relay, model)
            .err()
            .map(|problem| problem.0)
    };
    assert_eq!(
        code(harness, &chatgpt_sign_in(), &keyed, "gpt-5-codex").as_deref(),
        Some("account_chatgpt_relay_missing")
    );
    assert_eq!(
        code(harness, &chatgpt_sign_in(), &chatgpt, "").as_deref(),
        Some("account_model_missing")
    );
    assert_eq!(
        code(
            harness,
            &account("openai-responses", "https://api.openai.com/v1", true),
            &chatgpt,
            "gpt-5-codex"
        )
        .as_deref(),
        Some("account_not_compatible")
    );
    let mut claude = chatgpt_sign_in();
    claude.harness = crate::protocol::CredentialRef::new("claude").unwrap();
    assert_eq!(
        code(harness, &claude, &chatgpt, "gpt-5-codex").as_deref(),
        Some("account_not_compatible")
    );
    let chat = built(&["openai-chat"], OPENAI_ENV);
    assert_eq!(
        code(&chat.all()[0], &chatgpt_sign_in(), &chatgpt, "gpt-5-codex").as_deref(),
        Some("account_not_compatible")
    );
    assert_eq!(
        handover(harness, &chatgpt_sign_in(), Some(&chatgpt))
            .err()
            .map(|problem| problem.0)
            .as_deref(),
        Some("account_not_compatible")
    );
    assert_eq!(
        handover(
            harness,
            &account("openai-responses", "https://api.openai.com/v1", true),
            Some(&chatgpt)
        )
        .err()
        .map(|problem| problem.0)
        .as_deref(),
        Some("account_key_relay_missing")
    );
}
