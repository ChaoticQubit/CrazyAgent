mod research_support;

use crazyagent::config::{ModelAccess, ModelAuthentication, ModelConnection};
use crazyagent::protocol::CredentialRef;
use crazyagent::research::accounts::{AccountPick, pick_account};
use crazyagent::research::runner::ResearchAccount;
use sha2::Digest;
use std::os::unix::fs::PermissionsExt;

const ACCESS: &str = r#"
accounts = ["anthropic-local-account", "claude-account", "codex-account", "key-account", "local-account", "remote-account"]
connections = [
  { id = "claude-connection", provider = "anthropic", harness = "claude", protocol = "native", account_ref = "claude-account", endpoint = { kind = "native" }, model = { id = "native-default", author = "anthropic" }, auth = { method = "native_managed", profile_ref = "claude-profile" } },
  { id = "key-connection", provider = "openai", harness = "rcli", protocol = "openai-chat", account_ref = "key-account", endpoint = { kind = "http", url = "https://api.openai.com/v1" }, model = { id = "gpt-5", author = "openai" }, auth = { method = "api_key", credential_ref = "openai-key" } },
  { id = "codex-connection", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-account", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile" } },
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
  { id = "anthropic-local-connection", provider = "anthropic-compatible", harness = "claude", protocol = "anthropic-messages", account_ref = "anthropic-local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434" }, model = { id = "qwen2.5:0.5b", author = "anthropic-compatible" }, auth = { method = "none" } },
  { id = "remote-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "remote-account", endpoint = { kind = "http", url = "https://models.example.test/v1" }, model = { id = "big-model", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [
  { id = "unassigned-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false },
  { id = "mixed-pool", members = ["claude-connection", "key-connection", "codex-connection", "local-connection"], projects = ["project-a"], roles = ["project_leader"], fallback_allowed = false },
  { id = "claude-pool", members = ["claude-connection"], projects = ["project-claude"], roles = ["project_leader"], fallback_allowed = false },
  { id = "anthropic-pool", members = ["claude-connection", "anthropic-local-connection", "local-connection"], projects = ["project-anthropic"], roles = ["project_leader"], fallback_allowed = false },
  { id = "remote-pool", members = ["remote-connection"], projects = ["project-remote"], roles = ["project_leader"], fallback_allowed = false },
]
"#;

fn access() -> ModelAccess {
    let access: ModelAccess = toml::from_str(ACCESS).unwrap();
    assert_eq!(access.connections.len(), 6);
    access
}

fn signed_in(connection: &ModelConnection) -> bool {
    matches!(
        &connection.auth,
        ModelAuthentication::NativeManaged { profile_ref }
            if matches!(profile_ref.as_str(), "codex-profile" | "claude-profile")
    )
}

fn signed_out(_: &ModelConnection) -> bool {
    false
}

fn connection(access: &ModelAccess, id: &str) -> ModelConnection {
    access
        .connections
        .iter()
        .find(|connection| connection.id.as_str() == id)
        .cloned()
        .unwrap()
}

#[test]
fn a_project_without_an_assigned_pool_needs_one() {
    let access = access();
    assert_eq!(
        pick_account(&access, "project-unassigned", "codex", signed_in),
        AccountPick::NeedsPool
    );
    assert_eq!(
        pick_account(&ModelAccess::default(), "project-a", "codex", signed_in),
        AccountPick::NeedsPool
    );
}

#[test]
fn a_mixed_pool_gives_codex_its_first_signed_in_managed_account() {
    let access = access();
    assert_eq!(
        pick_account(&access, "project-a", "codex", signed_in),
        AccountPick::Account {
            connection: CredentialRef::new("codex-connection").unwrap(),
            account: ResearchAccount::ManagedCodex {
                connection: connection(&access, "codex-connection"),
                model: None,
            },
        }
    );
}

#[test]
fn a_signed_out_managed_account_falls_through_to_a_keyless_endpoint() {
    let access = access();
    assert_eq!(
        pick_account(&access, "project-a", "codex", signed_out),
        AccountPick::Account {
            connection: CredentialRef::new("local-connection").unwrap(),
            account: ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen2.5:0.5b".into(),
            },
        }
    );
    assert_eq!(
        pick_account(&access, "project-remote", "codex", signed_out),
        AccountPick::Account {
            connection: CredentialRef::new("remote-connection").unwrap(),
            account: ResearchAccount::Endpoint {
                base_url: "https://models.example.test/v1".into(),
                model: "big-model".into(),
            },
        }
    );
}

#[test]
fn a_pool_with_no_account_the_harness_can_use_says_so_plainly() {
    let access = access();
    let pick = pick_account(&access, "project-claude", "codex", signed_in);
    assert_eq!(pick, AccountPick::NoUsableAccount);
    assert_eq!(
        pick.label(),
        "No account in this project's pool can run this research"
    );
    assert_eq!(AccountPick::NeedsPool.label(), "Needs a pool");
}

#[test]
fn the_settings_resolver_reads_live_pool_assignment_and_the_project_folder() {
    use crazyagent::research::accounts::SettingsResolver;
    let runtime =
        research_support::EngineRuntime::with_projects(&[("project-a", "Sells house plants.")]);
    let root = runtime.root.clone();
    let global = root.join("research-global.toml");
    let project = root.join("research-project.toml");
    let projects = root.join("projects");
    std::fs::write(&project, "").unwrap();
    let catalog = |assigned: &str| {
        format!(
            "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"codex-account\", \"local-account\"]\n\n[[model_access.connections]]\nid = \"codex-connection\"\nprovider = \"openai\"\nharness = \"codex\"\nprotocol = \"native\"\naccount_ref = \"codex-account\"\nendpoint = {{ kind = \"native\" }}\nmodel = {{ id = \"native-default\", author = \"openai\" }}\nauth = {{ method = \"native_managed\", profile_ref = \"codex-profile\" }}\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = {{ kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }}\nmodel = {{ id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }}\nauth = {{ method = \"none\" }}\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"codex-connection\", \"local-connection\"]\nprojects = [{assigned}]\nroles = [\"project_leader\"]\nfallback_allowed = false\n"
        )
    };
    let unsigned = root
        .join("native-profiles")
        .join(format!("{:x}", sha2::Sha256::digest(b"codex-profile")))
        .join("home");
    std::fs::create_dir_all(&unsigned).unwrap();
    std::fs::set_permissions(&unsigned, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&global, catalog("")).unwrap();
    let resolver = SettingsResolver::new(&global, &project, &projects, &runtime.database).unwrap();
    let (folder, pick) = resolver.resolve("project-a", "codex").unwrap();
    assert_eq!(folder, projects.join("project-a"));
    assert_eq!(pick, AccountPick::NeedsPool);
    std::fs::write(&global, catalog("\"project-a\"")).unwrap();
    assert_eq!(
        resolver.resolve("project-a", "claude").unwrap().1,
        AccountPick::NoUsableAccount
    );
    let (_, pick) = resolver.resolve("project-a", "codex").unwrap();
    assert_eq!(
        pick,
        AccountPick::Account {
            connection: CredentialRef::new("local-connection").unwrap(),
            account: ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen2.5:0.5b".into(),
            },
        }
    );
    assert!(resolver.resolve("project-missing", "codex").is_none());
    assert!(resolver.resolve("../escape", "codex").is_none());
    std::os::unix::fs::symlink(&root, projects.join("project-link")).unwrap();
    assert!(resolver.resolve("project-link", "codex").is_none());
    assert!(SettingsResolver::new(&global, &project, &projects, &root.join("missing.db")).is_err());
}

#[test]
fn claude_code_gets_a_managed_claude_account_or_an_anthropic_endpoint_only() {
    let access = access();
    assert_eq!(
        pick_account(&access, "project-claude", "claude", signed_in),
        AccountPick::Account {
            connection: CredentialRef::new("claude-connection").unwrap(),
            account: ResearchAccount::ManagedClaude {
                connection: connection(&access, "claude-connection"),
                model: None,
            },
        }
    );
    let anthropic = AccountPick::Account {
        connection: CredentialRef::new("anthropic-local-connection").unwrap(),
        account: ResearchAccount::AnthropicEndpoint {
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen2.5:0.5b".into(),
        },
    };
    assert_eq!(
        pick_account(&access, "project-anthropic", "claude", signed_out),
        anthropic
    );
    assert_eq!(
        pick_account(&access, "project-anthropic", "claude-code", signed_out),
        anthropic
    );
    assert_eq!(
        pick_account(&access, "project-a", "claude", signed_out),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        pick_account(&access, "project-anthropic", "codex", signed_out),
        AccountPick::Account {
            connection: CredentialRef::new("local-connection").unwrap(),
            account: ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen2.5:0.5b".into(),
            },
        }
    );
    assert_eq!(
        pick_account(&access, "project-anthropic", "rcli", signed_in),
        AccountPick::NoUsableAccount
    );
}
