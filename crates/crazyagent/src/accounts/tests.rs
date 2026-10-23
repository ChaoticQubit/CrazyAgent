use super::*;

fn draft(provider: Provider, method: SignInMethod) -> AccountDraft {
    AccountDraft {
        provider,
        method,
        name: String::new(),
        url: None,
        model: None,
    }
}

#[test]
fn catalog_lists_every_provider_with_plain_ordered_methods() {
    let catalog = catalog();
    let names: Vec<_> = catalog.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "ChatGPT / Codex",
            "Claude",
            "OpenRouter",
            "OpenAI API",
            "Anthropic API",
            "Any OpenAI-compatible URL",
            "Local models (Ollama)",
        ]
    );
    let labels = |provider: Provider| -> Vec<String> {
        catalog
            .iter()
            .find(|entry| entry.provider == provider)
            .unwrap()
            .methods
            .iter()
            .map(|method| method.label.clone())
            .collect()
    };
    assert_eq!(
        labels(Provider::Chatgpt),
        ["Sign in with ChatGPT", "Use an API key"]
    );
    assert_eq!(
        labels(Provider::Claude),
        ["Sign in with your Claude subscription", "Use an API key"]
    );
    assert_eq!(labels(Provider::Openrouter), ["Use an API key"]);
    assert_eq!(labels(Provider::OpenaiApi), ["Use an API key"]);
    assert_eq!(labels(Provider::AnthropicApi), ["Use an API key"]);
    assert_eq!(labels(Provider::OpenaiCompatible), ["Connect a URL"]);
    assert_eq!(labels(Provider::Local), ["Connect a URL"]);
    let local = &catalog[6].methods[0];
    assert_eq!(local.url, Field::Required);
    assert_eq!(
        local.url_preset.as_deref(),
        Some("http://127.0.0.1:11434/v1")
    );
    assert_eq!(local.key, Field::Hidden);
    assert_eq!(local.model, Field::Required);
    let compatible = &catalog[5].methods[0];
    assert_eq!(
        (compatible.url, compatible.key, compatible.model),
        (Field::Required, Field::Optional, Field::Required)
    );
    let openrouter = &catalog[2].methods[0];
    assert_eq!(openrouter.url, Field::Hidden);
    assert_eq!(
        openrouter.url_preset.as_deref(),
        Some("https://openrouter.ai/api/v1")
    );
    assert_eq!(openrouter.key, Field::Required);
    let wire = serde_json::to_string(&catalog).unwrap();
    assert!(!wire.contains("rcli") && !wire.contains("native"));
}

#[test]
fn subscription_drafts_build_managed_native_connections() {
    for (provider, harness, author) in [
        (Provider::Chatgpt, "codex", "openai"),
        (Provider::Claude, "claude", "anthropic"),
    ] {
        let mut value = draft(provider, SignInMethod::Subscription);
        value.name = "  Work  ".into();
        let (connection, details) = value.build(false, "0123456789abcdef0123").unwrap();
        assert_eq!(connection.harness.as_str(), harness);
        assert_eq!(connection.protocol.as_str(), "native");
        assert_eq!(connection.provider.as_str(), author);
        assert_eq!(connection.endpoint, ModelEndpoint::Native {});
        assert!(matches!(
            connection.auth,
            ModelAuthentication::NativeManaged { .. }
        ));
        assert_eq!(details.name, "Work");
        assert_eq!(details.provider, provider);
        assert_eq!(method_of(&connection, provider), SignInMethod::Subscription);
        assert!(value.build(true, "0123456789abcdef0123").is_err());
    }
}

#[test]
fn api_key_drafts_use_presets_and_require_a_key_and_model() {
    for (provider, url, protocol) in [
        (
            Provider::Chatgpt,
            "https://api.openai.com/v1",
            "openai-chat",
        ),
        (
            Provider::OpenaiApi,
            "https://api.openai.com/v1",
            "openai-chat",
        ),
        (
            Provider::Claude,
            "https://api.anthropic.com/v1",
            "anthropic-messages",
        ),
        (
            Provider::AnthropicApi,
            "https://api.anthropic.com/v1",
            "anthropic-messages",
        ),
        (
            Provider::Openrouter,
            "https://openrouter.ai/api/v1",
            "openai-chat",
        ),
    ] {
        let mut value = draft(provider, SignInMethod::ApiKey);
        assert_eq!(
            value.build(true, "0123456789abcdef0123").unwrap_err().0,
            "account_model_required"
        );
        value.model = Some("some-model".into());
        assert_eq!(
            value.build(false, "0123456789abcdef0123").unwrap_err().0,
            "account_key_required"
        );
        let (connection, details) = value.build(true, "0123456789abcdef0123").unwrap();
        assert_eq!(connection.endpoint, ModelEndpoint::Http { url: url.into() });
        assert_eq!(connection.protocol.as_str(), protocol);
        assert_eq!(connection.harness.as_str(), "rcli");
        assert!(matches!(
            connection.auth,
            ModelAuthentication::ApiKey { .. }
        ));
        assert_eq!(details.name, provider_name(provider));
        assert_eq!(method_of(&connection, provider), SignInMethod::ApiKey);
        value.url = Some("http://127.0.0.1:1/v1".into());
        assert_eq!(
            value.build(true, "0123456789abcdef0123").unwrap_err().0,
            "account_url_not_allowed"
        );
    }
}

#[test]
fn url_drafts_validate_address_key_and_model() {
    let mut local = draft(Provider::Local, SignInMethod::Url);
    local.model = Some("qwen2.5:0.5b".into());
    let (connection, _) = local.build(false, "0123456789abcdef0123").unwrap();
    assert_eq!(
        connection.endpoint,
        ModelEndpoint::Http {
            url: "http://127.0.0.1:11434/v1".into()
        }
    );
    assert_eq!(connection.auth, ModelAuthentication::None {});
    assert_eq!(method_of(&connection, Provider::Local), SignInMethod::Url);
    assert_eq!(
        local.build(true, "0123456789abcdef0123").unwrap_err().0,
        "account_key_not_allowed"
    );
    local.url = Some("http://192.168.1.5:11434/v1".into());
    assert_eq!(
        local.build(false, "0123456789abcdef0123").unwrap_err().0,
        "account_local_url_required"
    );
    let mut any = draft(Provider::OpenaiCompatible, SignInMethod::Url);
    any.model = Some("m".into());
    assert_eq!(
        any.build(false, "0123456789abcdef0123").unwrap_err().0,
        "account_url_required"
    );
    any.url = Some("https://models.example.test/v1".into());
    let (open, _) = any.build(false, "0123456789abcdef0123").unwrap();
    assert_eq!(open.auth, ModelAuthentication::None {});
    let (keyed, _) = any.build(true, "0123456789abcdef0123").unwrap();
    assert!(matches!(keyed.auth, ModelAuthentication::ApiKey { .. }));
    for bad in [
        "ftp://example.test",
        "https://user:pw@example.test/v1",
        "https://example.test/v1?key=1",
        "not a url",
    ] {
        any.url = Some(bad.into());
        assert_eq!(
            any.build(false, "0123456789abcdef0123").unwrap_err().0,
            "account_url_invalid"
        );
    }
}

#[test]
fn drafts_reject_unknown_methods_bad_names_and_bad_nonces() {
    assert_eq!(
        draft(Provider::Openrouter, SignInMethod::Subscription)
            .build(false, "0123456789abcdef0123")
            .unwrap_err()
            .0,
        "account_method_unavailable"
    );
    let mut named = draft(Provider::Chatgpt, SignInMethod::Subscription);
    named.name = "x".repeat(ACCOUNT_NAME_MAX + 1);
    assert_eq!(
        named.build(false, "0123456789abcdef0123").unwrap_err().0,
        "account_name_invalid"
    );
    named.name = "a\u{202e}b".into();
    assert_eq!(
        named.build(false, "0123456789abcdef0123").unwrap_err().0,
        "account_name_invalid"
    );
    named.name = "ok".into();
    assert!(named.build(false, "short").is_err());
}

#[test]
fn account_count_bound_is_large_but_finite() {
    let refs = |count: usize| -> Vec<CredentialRef> {
        (0..count)
            .map(|index| CredentialRef::new(format!("account-{index}")).unwrap())
            .collect()
    };
    let catalog = |count| crate::config::ModelAccess {
        accounts: refs(count),
        ..Default::default()
    };
    assert!(catalog(crate::config::ACCOUNTS_MAX).validate().is_ok());
    assert!(catalog(crate::config::ACCOUNTS_MAX + 1).validate().is_err());
}

#[test]
fn statuses_have_plain_labels() {
    assert_eq!(AccountStatus::SignedIn.label(), "Signed in");
    assert_eq!(AccountStatus::NotSignedIn.label(), "Not signed in");
    assert_eq!(AccountStatus::KeySaved.label(), "Key saved");
    assert_eq!(AccountStatus::Unreachable.label(), "Can't reach this URL");
    assert_eq!(AccountStatus::SigningIn.label(), "Signing in...");
}
