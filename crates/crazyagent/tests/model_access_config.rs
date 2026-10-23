use crazyagent::config::{EffectTiming, EffectiveConfig, Origin, SettingScope, resolve_config};
use serde_json::json;

const HTTP_CATALOG: &str = r#"
[model_access]
accounts = ["account"]

[[model_access.connections]]
id = "connection"
provider = "any-provider"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "account"
endpoint = { kind = "http", url = "https://serving.example.invalid/v1" }
model = { id = "lab/model:version", author = "model-author" }
auth = { method = "none" }

[[model_access.pools]]
id = "pool"
members = ["connection"]
projects = ["project"]
roles = ["worker"]
fallback_allowed = true
"#;

fn load_catalog(
    global: &str,
    project: &str,
) -> Result<EffectiveConfig, crazyagent::config::ConfigError> {
    let directory = tempfile::tempdir().unwrap();
    let global_path = directory.path().join("global.toml");
    let project_path = directory.path().join("project.toml");
    std::fs::write(&global_path, global).unwrap();
    std::fs::write(&project_path, project).unwrap();
    resolve_config(&global_path, &project_path)
}

#[test]
fn absent_model_access_preserves_configuration_shape() {
    let config = load_catalog("", "").unwrap();
    config.validate().unwrap();
    assert!(
        config
            .descriptors
            .values()
            .all(|entry| entry.origin == Origin::Default)
    );
    assert!(!config.descriptors.contains_key("model_access"));
    assert!(
        serde_json::to_value(config.settings)
            .unwrap()
            .get("model_access")
            .is_none()
    );
}

#[test]
fn model_access_preserves_global_and_project_knowledge_metadata() {
    for (global_knowledge, project_knowledge) in [
        ("", ""),
        ("[knowledge]\ntext_bytes_max = 2048\n", ""),
        ("", "[knowledge]\ntext_bytes_max = 1024\n"),
        (
            "[knowledge]\ntext_bytes_max = 2048\n",
            "[knowledge]\ntext_bytes_max = 1024\n",
        ),
    ] {
        let baseline = load_catalog(global_knowledge, project_knowledge).unwrap();
        baseline.validate().unwrap();
        let config = load_catalog(
            &format!("{HTTP_CATALOG}\n{global_knowledge}"),
            project_knowledge,
        )
        .expect("model accounts must coexist with the resolved knowledge settings");
        config.validate().unwrap();
        assert_eq!(config.settings.knowledge, baseline.settings.knowledge);
        assert_eq!(config.settings.routing, baseline.settings.routing);
        assert_eq!(config.descriptors.len(), baseline.descriptors.len() + 1);
        for (name, descriptor) in &baseline.descriptors {
            assert_eq!(config.descriptors.get(name), Some(descriptor));
        }
        let descriptor = &config.descriptors["model_access"];
        assert_eq!(descriptor.origin, Origin::Global);
        assert_eq!(descriptor.scope, SettingScope::Global);
        assert_eq!(descriptor.effect_timing, EffectTiming::NewTask);
        assert_eq!(descriptor.default, serde_json::Value::Null);
        assert_ne!(config.id, baseline.id);
        let round_trip: EffectiveConfig =
            serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        round_trip.validate().unwrap();
        assert_eq!(round_trip, config);
    }
}

#[test]
fn four_claude_and_two_codex_profiles_remain_distinct() {
    let accounts = [
        "claude1", "claude2", "claude3", "claude4", "codex1", "codex2",
    ];
    let mut input = format!("[model_access]\naccounts = {accounts:?}\n");
    for account in accounts {
        let (provider, harness) = if account.starts_with("claude") {
            ("anthropic", "claude-code")
        } else {
            ("openai", "codex")
        };
        input.push_str(&format!(
            "\n[[model_access.connections]]\nid='{account}'\nprovider='{provider}'\nharness='{harness}'\nprotocol='native'\naccount_ref='{account}'\nendpoint={{kind='native'}}\nmodel={{id='chosen-model',author='{provider}'}}\nauth={{method='native_managed',profile_ref='profile-{account}'}}\n"
        ));
    }
    input.push_str(&format!(
        "\n[[model_access.pools]]\nid='development'\nmembers={accounts:?}\nprojects=['project']\nroles=['leader','worker']\nfallback_allowed=true\n"
    ));
    let config = load_catalog(&input, "").unwrap();
    config.validate().unwrap();
    let value = serde_json::to_value(config.settings).unwrap();
    let catalog = &value["model_access"];
    assert_eq!(catalog["accounts"].as_array().unwrap().len(), 6);
    assert_eq!(catalog["connections"].as_array().unwrap().len(), 6);
    assert_eq!(catalog["pools"][0]["members"], json!(accounts));
    for (index, account) in accounts.iter().enumerate() {
        assert_eq!(catalog["connections"][index]["account_ref"], *account);
        assert_eq!(
            catalog["connections"][index]["auth"]["profile_ref"],
            format!("profile-{account}")
        );
    }
}

#[test]
fn compatible_providers_and_explicit_auth_methods_resolve_without_secrets() {
    for provider in [
        "kimi",
        "deepseek",
        "nvidia",
        "meta-model-host",
        "self-hosted",
    ] {
        for auth in [
            "{ method = 'none' }",
            "{ method = 'api_key', credential_ref = 'key-alias' }",
            "{ method = 'oauth', credential_ref = 'oauth-alias' }",
        ] {
            let input = HTTP_CATALOG
                .replace("any-provider", provider)
                .replace("{ method = \"none\" }", auth);
            load_catalog(&input, "").unwrap().validate().unwrap();
        }
    }
    let ipv6 = HTTP_CATALOG.replace(
        "https://serving.example.invalid/v1",
        "http://[::1]:11434/v1",
    );
    load_catalog(&ipv6, "").unwrap().validate().unwrap();
}

#[test]
fn catalog_rejects_duplicate_dangling_oversized_and_incompatible_metadata() {
    let cases = [
        HTTP_CATALOG.replace(
            "accounts = [\"account\"]",
            "accounts = ['account', 'account']",
        ),
        HTTP_CATALOG.replace("account_ref = \"account\"", "account_ref = 'missing'"),
        HTTP_CATALOG.replace("members = [\"connection\"]", "members = ['missing']"),
        HTTP_CATALOG.replace(
            "members = [\"connection\"]",
            "members = ['connection','connection']",
        ),
        HTTP_CATALOG.replace("members = [\"connection\"]", "members = []"),
        HTTP_CATALOG.replace(
            "projects = [\"project\"]",
            "projects = ['project','project']",
        ),
        HTTP_CATALOG.replace("protocol = \"openai-compatible\"", "protocol = 'native'"),
        HTTP_CATALOG.replace(
            "{ method = \"none\" }",
            "{method='native_managed',profile_ref='profile'}",
        ),
        HTTP_CATALOG.replace("lab/model:version", &"m".repeat(257)),
        HTTP_CATALOG.replace(
            "accounts = [\"account\"]",
            &format!("accounts = {:?}", ["account", "account"]),
        ),
    ];
    for (index, input) in cases.iter().enumerate() {
        let error = load_catalog(input, "").unwrap_err();
        assert_eq!(error.0, "invalid_model_access", "case {index}");
    }
    let error = load_catalog("", HTTP_CATALOG).unwrap_err();
    assert_eq!(error.0, "model_access_requires_global_scope");
}

#[test]
fn catalog_rejects_secret_fields_and_unsafe_urls_without_echoing_values() {
    let secret = "UNSAFE_SECRET_MUST_NOT_APPEAR";
    let inputs = [
        HTTP_CATALOG.replace(
            "{ method = \"none\" }",
            &format!("{{method='api_key',api_key='{secret}'}}"),
        ),
        HTTP_CATALOG.replace(
            "{ method = \"none\" }",
            &format!("{{method='none',credential_ref='{secret}'}}"),
        ),
        HTTP_CATALOG.replace(
            "https://serving.example.invalid/v1",
            &format!("https://user:{secret}@host.invalid/v1"),
        ),
        HTTP_CATALOG.replace(
            "https://serving.example.invalid/v1",
            &format!("https://host.invalid/v1?key={secret}"),
        ),
        HTTP_CATALOG.replace(
            "https://serving.example.invalid/v1",
            "https://host.invalid/v1#fragment",
        ),
        HTTP_CATALOG.replace("https://serving.example.invalid/v1", "file:///tmp/models"),
        HTTP_CATALOG.replace(
            "https://serving.example.invalid/v1",
            "https://host.invalid/has space",
        ),
        HTTP_CATALOG
            .replace("protocol = \"openai-compatible\"", "protocol = 'native'")
            .replace(
                "{ kind = \"http\", url = \"https://serving.example.invalid/v1\" }",
                &format!("{{kind='native',secret='{secret}'}}"),
            ),
    ];
    for (index, input) in inputs.iter().enumerate() {
        let error = load_catalog(input, "").unwrap_err();
        assert!(!error.0.contains(secret), "secret echoed in case {index}");
        assert!(
            matches!(
                error.0.as_str(),
                "invalid_model_access" | "invalid_configuration_format"
            ),
            "case {index}: {}",
            error.0
        );
    }
}

#[test]
fn resolve_config_accepts_provider_neutral_accounts_and_a_scoped_pool() {
    let directory = tempfile::tempdir().unwrap();
    let global = directory.path().join("global.toml");
    let project = directory.path().join("project.toml");
    let baseline = resolve_config(&global, &project).unwrap();
    std::fs::write(
        &global,
        r#"
[model_access]
accounts = ["local-account", "api-account"]

[[model_access.connections]]
id = "local-connection"
provider = "self-hosted-service"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "local-account"
endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }
model = { id = "example/model-one", author = "independent-lab" }
auth = { method = "none" }

[[model_access.connections]]
id = "api-connection"
provider = "arbitrary-serving-company"
harness = "rcli"
protocol = "openai-compatible"
account_ref = "api-account"
endpoint = { kind = "http", url = "https://serving.example.invalid/v1" }
model = { id = "example/model-one", author = "independent-lab" }
auth = { method = "api_key", credential_ref = "credential-alias" }

[[model_access.pools]]
id = "worker-pool"
members = ["local-connection", "api-connection"]
projects = ["project-one"]
roles = ["worker"]
fallback_allowed = true
"#,
    )
    .unwrap();

    let config = resolve_config(&global, &project)
        .expect("provider-neutral accounts and scoped-pool metadata must resolve");
    config.validate().unwrap();
    let expected = json!({
        "accounts": ["local-account", "api-account"],
        "connections": [
            {
                "id": "local-connection",
                "provider": "self-hosted-service",
                "harness": "rcli",
                "protocol": "openai-compatible",
                "account_ref": "local-account",
                "endpoint": {"kind": "http", "url": "http://127.0.0.1:11434/v1"},
                "model": {"id": "example/model-one", "author": "independent-lab"},
                "auth": {"method": "none"}
            },
            {
                "id": "api-connection",
                "provider": "arbitrary-serving-company",
                "harness": "rcli",
                "protocol": "openai-compatible",
                "account_ref": "api-account",
                "endpoint": {"kind": "http", "url": "https://serving.example.invalid/v1"},
                "model": {"id": "example/model-one", "author": "independent-lab"},
                "auth": {"method": "api_key", "credential_ref": "credential-alias"}
            }
        ],
        "pools": [{
            "id": "worker-pool",
            "members": ["local-connection", "api-connection"],
            "projects": ["project-one"],
            "roles": ["worker"],
            "fallback_allowed": true
        }]
    });
    assert_eq!(
        serde_json::to_value(&config.settings).unwrap()["model_access"],
        expected
    );
    assert_eq!(config.descriptors.len(), baseline.descriptors.len() + 1);
    for (name, descriptor) in &baseline.descriptors {
        assert_eq!(config.descriptors.get(name), Some(descriptor));
    }
    let descriptor = &config.descriptors["model_access"];
    assert_eq!(descriptor.origin, Origin::Global);
    assert_eq!(descriptor.scope, SettingScope::Global);
    assert_eq!(descriptor.effect_timing, EffectTiming::NewTask);
    assert_eq!(descriptor.default, serde_json::Value::Null);
    assert_eq!(descriptor.value, expected);
    assert_ne!(config.id, baseline.id);
    assert_eq!(config.settings.routing, baseline.settings.routing);
    let round_trip: EffectiveConfig =
        serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
    round_trip.validate().unwrap();
    assert_eq!(round_trip, config);
}

#[test]
fn quota_policy_default_preserves_legacy_catalog_identity_and_serialization() {
    use crazyagent::config::ProviderQuotaPolicy;
    let legacy = load_catalog(HTTP_CATALOG, "").unwrap();
    assert_eq!(
        legacy.id.as_str(),
        "67fdcfb67c9a9426fccb0ffa5660119903c09fec0c4fc567eecb137366b79b87"
    );
    assert_eq!(
        legacy.settings.model_access.as_ref().unwrap().pools[0].provider_quota_policy,
        ProviderQuotaPolicy::BoundedAttempt
    );
    let explicit = load_catalog(
        &format!("{HTTP_CATALOG}\nprovider_quota_policy = 'bounded_attempt'\n"),
        "",
    )
    .unwrap();
    assert_eq!(explicit, legacy);
    let value = serde_json::to_value(&legacy).unwrap();
    assert!(
        value["settings"]["model_access"]["pools"][0]
            .get("provider_quota_policy")
            .is_none()
    );
    assert!(
        value["descriptors"]["model_access"]["value"]["pools"][0]
            .get("provider_quota_policy")
            .is_none()
    );
    let decoded: EffectiveConfig = serde_json::from_value(value).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, legacy);
}

#[test]
fn quota_policy_strict_is_global_typed_and_changes_only_catalog_identity() {
    use crazyagent::config::ProviderQuotaPolicy;
    let legacy = load_catalog(HTTP_CATALOG, "").unwrap();
    let strict = load_catalog(
        &format!("{HTTP_CATALOG}\nprovider_quota_policy = 'strict'\n"),
        "",
    )
    .unwrap();
    assert_eq!(
        strict.settings.model_access.as_ref().unwrap().pools[0].provider_quota_policy,
        ProviderQuotaPolicy::Strict
    );
    assert_ne!(strict.id, legacy.id);
    assert_eq!(strict.settings.routing, legacy.settings.routing);
    assert_eq!(strict.descriptors.len(), legacy.descriptors.len());
    let descriptor = &strict.descriptors["model_access"];
    assert_eq!(descriptor.origin, Origin::Global);
    assert_eq!(descriptor.scope, SettingScope::Global);
    assert_eq!(descriptor.effect_timing, EffectTiming::NewTask);
    assert_eq!(
        descriptor.value["pools"][0]["provider_quota_policy"],
        "strict"
    );
    let decoded: EffectiveConfig =
        serde_json::from_slice(&serde_json::to_vec(&strict).unwrap()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, strict);
    assert_eq!(
        load_catalog(
            "",
            &format!("{HTTP_CATALOG}\nprovider_quota_policy = 'strict'\n")
        )
        .unwrap_err()
        .0,
        "model_access_requires_global_scope"
    );
}

#[test]
fn quota_policy_rejects_unknown_values_and_untyped_catalog_fields() {
    for value in ["'available'", "'unlimited'", "'Strict'", "true", "0", "{}"] {
        assert_eq!(
            load_catalog(
                &format!("{HTTP_CATALOG}\nprovider_quota_policy = {value}\n"),
                ""
            )
            .unwrap_err()
            .0,
            "invalid_configuration_format"
        );
    }
    assert!(
        load_catalog(
            &format!("{HTTP_CATALOG}\nprovider_quota_policy = 'strict'\nquota_remaining = 1\n"),
            ""
        )
        .is_err()
    );
}

#[test]
fn imp1038_unassigned_and_partial_catalog_scopes_round_trip() {
    for (projects, roles) in [
        ("[]", "[]"),
        ("[]", "[\"worker\"]"),
        ("[\"project\"]", "[]"),
    ] {
        let input = HTTP_CATALOG
            .replace(
                "projects = [\"project\"]",
                &format!("projects = {projects}"),
            )
            .replace("roles = [\"worker\"]", &format!("roles = {roles}"));
        let config = load_catalog(&input, "").expect("unassigned catalog scopes are valid");
        config.validate().unwrap();
        let pool = &config.settings.model_access.as_ref().unwrap().pools[0];
        assert_eq!(pool.projects.is_empty(), projects == "[]");
        assert_eq!(pool.roles.is_empty(), roles == "[]");
        assert_eq!(pool.members[0].as_str(), "connection");
        let restored: EffectiveConfig =
            serde_json::from_slice(&serde_json::to_vec(&config).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored, config);
    }
}

#[test]
fn imp1038_unassigned_catalog_retains_membership_uniqueness_and_bounds() {
    let unassigned = HTTP_CATALOG
        .replace("projects = [\"project\"]", "projects = []")
        .replace("roles = [\"worker\"]", "roles = []");
    let mut cases = vec![
        unassigned.replace("members = [\"connection\"]", "members = []"),
        unassigned.replace("members = [\"connection\"]", "members = ['missing']"),
        unassigned.replace(
            "members = [\"connection\"]",
            "members = ['connection','connection']",
        ),
        unassigned.replace("projects = []", "projects = ['project','project']"),
        unassigned.replace("roles = []", "roles = ['worker','worker']"),
    ];
    for field in ["projects", "roles"] {
        cases.push(unassigned.replace(
            &format!("{field} = []"),
            &format!(
                "{field} = {:?}",
                (0..65).map(|i| format!("scope{i}")).collect::<Vec<_>>()
            ),
        ));
    }
    for input in cases {
        assert_eq!(
            load_catalog(&input, "").unwrap_err().0,
            "invalid_model_access"
        );
    }
}
