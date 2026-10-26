#![cfg(unix)]

use crazyagent::accounts::{AccountDraft, Provider, SignInMethod};
use crazyagent::config::{AccountPool, ModelAccessEdit, ProviderQuotaPolicy};
use crazyagent::engine::bootstrap::EngineService;
use crazyagent::engine::owner::{ModelAccessCollection, NativeAuthMethod};
use crazyagent::protocol::CredentialRef;
use crazyagent_desktop::bridge::{ActionOutcome, NativeBridge};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn accounts_commands_are_registered_in_all_four_places() {
    let build = include_str!("../build.rs");
    let bindings = include_str!("../src/lib.rs");
    let capability = include_str!("../capabilities/main-window.json");
    let client = include_str!("../../src/engine-client.ts");
    for command in [
        "desktop_account_catalog",
        "desktop_list_accounts",
        "desktop_add_account",
        "desktop_rename_account",
        "desktop_remove_account",
        "desktop_check_account",
        "desktop_local_models",
    ] {
        assert_eq!(
            build.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} in build.rs"
        );
        assert!(
            bindings.contains(&format!("async fn {command}(")),
            "{command} handler"
        );
        assert_eq!(
            bindings
                .lines()
                .filter(|line| line.trim() == format!("{command},"))
                .count(),
            1,
            "{command} in invoke_handler"
        );
        let allowed = format!("\"allow-{}\"", command.replace('_', "-"));
        assert_eq!(
            capability.matches(&allowed).count(),
            1,
            "{command} permission"
        );
        assert_eq!(
            client.matches(&format!("\"{command}\"")).count(),
            1,
            "{command} in engine-client.ts"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_accounts_commands_reach_the_real_engine() -> TestResult<()> {
    let root = tempfile::Builder::new()
        .prefix("ca-dacct-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await?;
    let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
    assert_eq!(
        bridge.account_catalog().await.unwrap_err().code,
        "not_connected"
    );
    bridge.connect().await?;

    let catalog = bridge.account_catalog().await.unwrap();
    assert_eq!(
        catalog[1].methods[0].label,
        "Sign in with your Claude subscription"
    );
    let draft = AccountDraft {
        provider: Provider::Openrouter,
        method: SignInMethod::ApiKey,
        name: "Desk router".into(),
        url: None,
        model: Some("openai/gpt-4o".into()),
    };
    assert_eq!(
        bridge
            .add_account(draft.clone(), None)
            .await
            .unwrap_err()
            .code,
        "account_key_required"
    );
    let secret = format!("sk-desk-{}", std::process::id());
    let added = bridge
        .add_account(draft, Some(secret.clone()))
        .await
        .unwrap();
    assert_eq!(added.status_label, "Key saved");
    let json = serde_json::to_string(&added)?;
    assert!(!json.contains(&secret) && json.contains("\"provider_name\":\"OpenRouter\""));
    let claude = bridge
        .add_account(
            AccountDraft {
                provider: Provider::Claude,
                method: SignInMethod::Subscription,
                name: String::new(),
                url: None,
                model: None,
            },
            None,
        )
        .await
        .unwrap();
    let page = bridge.list_accounts(None, 1).await.unwrap();
    assert_eq!(page.accounts.len(), 1);
    let rest = bridge
        .list_accounts(page.next.map(|id| id.as_str().to_owned()), 200)
        .await
        .unwrap();
    assert_eq!(rest.accounts[0].status_label, "Not signed in");
    assert!(rest.next.is_none());
    let renamed = bridge
        .rename_account(claude.id.as_str(), "Personal Claude".into())
        .await
        .unwrap();
    assert_eq!(renamed.name, "Personal Claude");
    bridge.remove_account(added.id.as_str()).await.unwrap();
    assert_eq!(
        bridge
            .remove_account(added.id.as_str())
            .await
            .unwrap_err()
            .code,
        "account_not_found"
    );
    assert_eq!(
        bridge
            .local_models("http://10.1.2.3:11434/v1".into())
            .await
            .unwrap_err()
            .code,
        "account_local_url_required"
    );
    let models = bridge
        .local_models("http://127.0.0.1:11434/v1".into())
        .await
        .unwrap();
    assert!(
        models.iter().any(|model| model == "qwen2.5:0.5b"),
        "{models:?}"
    );
    let checked = bridge.check_account(claude.id.as_str()).await.unwrap();
    assert_eq!(checked.account.status_label, "Not signed in");

    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_configures_a_pooled_key_account_from_the_saved_key() -> TestResult<()> {
    let root = tempfile::Builder::new()
        .prefix("ca-dkey-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await?;
    let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
    bridge.connect().await?;
    let secret = format!("sk-dkey-{}", std::process::id());
    let added = bridge
        .add_account(
            AccountDraft {
                provider: Provider::Openrouter,
                method: SignInMethod::ApiKey,
                name: "Saved router".into(),
                url: None,
                model: Some("openai/gpt-4o".into()),
            },
            Some(secret.clone()),
        )
        .await
        .unwrap();
    let page = bridge
        .model_access_page(ModelAccessCollection::Pools, false)
        .await
        .unwrap();
    assert!(page.fresh && page.writable);
    let pool = AccountPool {
        id: CredentialRef::new("saved-router-pool").unwrap(),
        name: None,
        members: vec![added.id.clone()],
        projects: Vec::new(),
        roles: Vec::new(),
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
    };
    let receipt = bridge
        .edit_model_access(&page.ticket, ModelAccessEdit::CreatePool { pool }, 1)
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::Applied);
    let inspected = bridge
        .inspect_model_access(ModelAccessCollection::Connections, added.id.as_str())
        .await
        .unwrap();
    assert!(inspected.fresh && inspected.writable);
    let configured = bridge
        .configure_account_profile(&inspected.ticket, None, 2)
        .await
        .unwrap_err();
    assert_eq!(configured.code, "rcli_private_key_writer_unavailable");
    let settings = std::fs::read_to_string(&global)?;
    assert!(settings.contains(added.id.as_str()) && !settings.contains(&secret));

    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn desktop_account_actions_cancel_and_delete_on_the_real_engine() -> TestResult<()> {
    let root = tempfile::Builder::new()
        .prefix("ca-dact-")
        .tempdir_in("/private/tmp")?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))?;
    let global = root.path().join("global.toml");
    std::fs::write(&global, "version = 1\ntimeout_ms = 5000\n")?;
    std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600))?;
    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let mut service = EngineService::start_with_settings_sources(
        root.path(),
        "e.sock",
        &global,
        &root.path().join("project.toml"),
        &stopped,
    )
    .await?;
    let mut bridge = NativeBridge::new(Some(root.path().join("e.sock")));
    bridge.connect().await?;
    let claude = |name: &str| AccountDraft {
        provider: Provider::Claude,
        method: SignInMethod::Subscription,
        name: name.into(),
        url: None,
        model: None,
    };
    let first = bridge
        .add_account(claude("Claude one"), None)
        .await
        .unwrap();
    let second = bridge
        .add_account(claude("Claude two"), None)
        .await
        .unwrap();
    let pooled = bridge
        .add_account(
            AccountDraft {
                provider: Provider::Openrouter,
                method: SignInMethod::ApiKey,
                name: "Pooled router".into(),
                url: None,
                model: Some("openai/gpt-4o".into()),
            },
            Some(format!("sk-dact-{}", std::process::id())),
        )
        .await
        .unwrap();

    let detail = bridge
        .inspect_model_access(ModelAccessCollection::Connections, first.id.as_str())
        .await
        .unwrap();
    let started = bridge
        .start_native_auth(&detail.ticket, NativeAuthMethod::ClaudeOfficial)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&started)?["code"],
        "native_auth_host_unavailable"
    );
    let listed = bridge.list_accounts(None, 200).await.unwrap().accounts;
    let shown = listed
        .iter()
        .find(|account| account.id == first.id)
        .expect("the account is listed");
    assert_eq!(
        shown.status_label, "Not signed in",
        "a sign-in that failed is not shown as signing in"
    );
    let detail = bridge
        .inspect_model_access(ModelAccessCollection::Connections, first.id.as_str())
        .await
        .unwrap();
    assert_eq!(
        bridge
            .cancel_native_auth(&detail.ticket)
            .await
            .err()
            .unwrap()
            .code,
        "native_auth_not_active",
        "cancelling a sign-in that already stopped is reported as not running"
    );

    let page = bridge
        .model_access_page(ModelAccessCollection::Pools, false)
        .await
        .unwrap();
    let pool = AccountPool {
        id: CredentialRef::new("actions-pool").unwrap(),
        name: None,
        members: vec![pooled.id.clone()],
        projects: Vec::new(),
        roles: Vec::new(),
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::BoundedAttempt,
    };
    let receipt = bridge
        .edit_model_access(&page.ticket, ModelAccessEdit::CreatePool { pool }, 1)
        .await
        .unwrap();
    assert_eq!(receipt.outcome, ActionOutcome::Applied);

    for account in [&first, &second] {
        bridge.remove_account(account.id.as_str()).await.unwrap();
    }
    let left = bridge.list_accounts(None, 200).await.unwrap().accounts;
    assert_eq!(
        left.iter()
            .map(|account| account.id.clone())
            .collect::<Vec<_>>(),
        vec![pooled.id.clone()],
        "both selected accounts are gone and the pooled one stays"
    );
    bridge.remove_account(pooled.id.as_str()).await.unwrap();
    assert!(
        bridge
            .list_accounts(None, 200)
            .await
            .unwrap()
            .accounts
            .is_empty()
    );
    let pools = bridge
        .model_access_page(ModelAccessCollection::Pools, false)
        .await
        .unwrap();
    assert!(
        pools.items.is_empty(),
        "the pool its only account left is deleted with it: {:?}",
        pools.items
    );

    let cleanup = tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await;
    assert!(matches!(cleanup, Ok(Ok(()))));
    Ok(())
}
