use super::*;

const ASSIGNMENT_GLOBAL: &str = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [{ id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } }]
pools = [{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;

fn assigned_pool(projects: &[&str]) -> ModelAccessEdit {
    ModelAccessEdit::ReplacePool {
        pool: AccountPool {
            id: CredentialRef::new("local-pool").unwrap(),
            name: None,
            members: vec![CredentialRef::new("local-connection").unwrap()],
            projects: projects
                .iter()
                .map(|p| CredentialRef::new(*p).unwrap())
                .collect(),
            roles: vec![CredentialRef::new("project_leader").unwrap()],
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        },
    }
}

#[test]
fn assigning_pools_approves_each_rcli_account_its_own_profile_destination() {
    let global = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["key-account", "local-account"]
connections = [
  { id = "key-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "key-account", endpoint = { kind = "http", url = "http://127.0.0.1:9/v1" }, model = { id = "m", author = "openai-compatible" }, auth = { method = "api_key", credential_ref = "key-ref" } },
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "pool", members = ["key-connection", "local-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;
    let edit = ModelAccessEdit::ReplacePool {
        pool: AccountPool {
            id: CredentialRef::new("pool").unwrap(),
            name: None,
            members: vec![
                CredentialRef::new("key-connection").unwrap(),
                CredentialRef::new("local-connection").unwrap(),
            ],
            projects: vec![CredentialRef::new("demo").unwrap()],
            roles: vec![CredentialRef::new("project_leader").unwrap()],
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        },
    };
    let (_, config) = edit_model_access(global, "", &edit, &[]).unwrap();
    let access = config.settings.model_access.as_ref().unwrap();
    let destinations = &config.settings.allowed_destinations;
    assert!(destinations.contains(&CredentialRef::new("local").unwrap()));
    for connection in &access.connections {
        let profile = configured_profile_ref(connection).unwrap();
        assert!(destinations.contains(&profile), "{profile:?}");
    }
    assert_eq!(
        configured_profile_ref(&access.connections[0]),
        Some(CredentialRef::new("key-ref").unwrap())
    );
    assert_ne!(
        configured_profile_ref(&access.connections[0]),
        configured_profile_ref(&access.connections[1])
    );
}

#[test]
fn assigning_a_pool_to_a_project_approves_its_accounts_and_a_local_destination() {
    let (bytes, config) =
        edit_model_access(ASSIGNMENT_GLOBAL, "", &assigned_pool(&["demo"]), &[]).unwrap();
    let account = CredentialRef::new("local-account").unwrap();
    assert_eq!(config.settings.routing.approved_accounts, vec![account]);
    let connection = &config.settings.model_access.as_ref().unwrap().connections[0];
    assert_eq!(
        config.settings.allowed_destinations,
        vec![
            CredentialRef::new("local").unwrap(),
            configured_profile_ref(connection).unwrap()
        ]
    );
    let (_, again) =
        edit_model_access(&bytes, "", &assigned_pool(&["demo", "other"]), &[]).unwrap();
    assert_eq!(again.settings.routing.approved_accounts.len(), 1);
    assert_eq!(
        again.settings.allowed_destinations,
        config.settings.allowed_destinations
    );
}

#[test]
fn connections_are_saved_with_plain_http_only_to_this_machine() {
    let connection = |url: &str| ModelConnection {
        id: CredentialRef::new("lab-connection").unwrap(),
        provider: CredentialRef::new("openai-compatible").unwrap(),
        harness: CredentialRef::new("rcli").unwrap(),
        protocol: CredentialRef::new("openai-chat").unwrap(),
        account_ref: CredentialRef::new("local-account").unwrap(),
        endpoint: ModelEndpoint::Http { url: url.into() },
        model: ModelReference {
            id: "m".into(),
            author: CredentialRef::new("openai-compatible").unwrap(),
        },
        auth: ModelAuthentication::ApiKey {
            credential_ref: CredentialRef::new("lab-key").unwrap(),
        },
    };
    let remote = connection("http://192.0.2.1:8000/v1");
    let refused = |result: Result<(String, EffectiveConfig), ConfigError>| {
        assert_eq!(result.unwrap_err().0, "account_url_invalid");
    };
    refused(edit_model_access(
        ASSIGNMENT_GLOBAL,
        "",
        &ModelAccessEdit::CreateConnection {
            connection: remote.clone(),
        },
        &[],
    ));
    refused(edit_model_access(
        ASSIGNMENT_GLOBAL,
        "",
        &assigned_pool(&["demo"]),
        std::slice::from_ref(&remote),
    ));
    let (bytes, _) = edit_model_access(
        ASSIGNMENT_GLOBAL,
        "",
        &ModelAccessEdit::CreateConnection {
            connection: connection("http://127.0.0.1:8000/v1"),
        },
        &[],
    )
    .unwrap();
    refused(edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::ReplaceConnection { connection: remote },
        &[],
    ));
    edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::ReplaceConnection {
            connection: connection("https://models.example.test/v1"),
        },
        &[],
    )
    .unwrap();
}

#[test]
fn assigning_a_signed_in_pool_allows_its_connection_as_the_master_target() {
    let global = r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["codex-account"]
connections = [{ id = "codex-connection", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-account", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile" } }]
pools = [{ id = "codex-pool", members = ["codex-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;
    let edit = ModelAccessEdit::ReplacePool {
        pool: AccountPool {
            id: CredentialRef::new("codex-pool").unwrap(),
            name: None,
            members: vec![CredentialRef::new("codex-connection").unwrap()],
            projects: vec![CredentialRef::new("demo").unwrap()],
            roles: vec![CredentialRef::new("project_leader").unwrap()],
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        },
    };
    let (bytes, config) = edit_model_access(global, "", &edit, &[]).unwrap();
    let connection = CredentialRef::new("codex-connection").unwrap();
    assert!(config.settings.allowed_destinations.contains(&connection));
    let (_, again) = edit_model_access(&bytes, "", &edit, &[]).unwrap();
    assert_eq!(
        again.settings.allowed_destinations,
        config.settings.allowed_destinations
    );
}

#[test]
fn replacing_a_keyless_connection_replaces_its_own_destination_approval() {
    let (mut bytes, config) =
        edit_model_access(ASSIGNMENT_GLOBAL, "", &assigned_pool(&["demo"]), &[]).unwrap();
    let mut connection = config.settings.model_access.unwrap().connections[0].clone();
    for n in 0..70 {
        connection.model.id = format!("model-{n}");
        let edit = ModelAccessEdit::ReplaceConnection {
            connection: connection.clone(),
        };
        let (next, config) = edit_model_access(&bytes, "", &edit, &[]).unwrap();
        assert_eq!(
            config.settings.allowed_destinations,
            vec![
                CredentialRef::new("local").unwrap(),
                configured_profile_ref(&connection).unwrap()
            ]
        );
        assert_eq!(
            config.settings.routing.approved_accounts,
            vec![CredentialRef::new("local-account").unwrap()]
        );
        bytes = next;
    }
}

#[test]
fn removing_a_connected_account_drops_its_approvals() {
    let global = r#"version = 1
timeout_ms = 2000
allowed_destinations = ["owner-target"]

[routing]
approved_accounts = ["owner-account"]

[model_access]
accounts = ["codex-account", "local-account"]
connections = [
  { id = "codex-connection", provider = "openai", harness = "codex", protocol = "native", account_ref = "codex-account", endpoint = { kind = "native" }, model = { id = "native-default", author = "openai" }, auth = { method = "native_managed", profile_ref = "codex-profile" } },
  { id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = { kind = "http", url = "http://127.0.0.1:11434/v1" }, model = { id = "qwen2.5:0.5b", author = "openai-compatible" }, auth = { method = "none" } },
]
pools = [{ id = "pool", members = ["codex-connection", "local-connection"], projects = [], roles = [], fallback_allowed = false }]
"#;
    let pool = |members: &[&str]| ModelAccessEdit::ReplacePool {
        pool: AccountPool {
            id: CredentialRef::new("pool").unwrap(),
            name: None,
            members: members
                .iter()
                .map(|m| CredentialRef::new(*m).unwrap())
                .collect(),
            projects: vec![CredentialRef::new("demo").unwrap()],
            roles: vec![CredentialRef::new("project_leader").unwrap()],
            fallback_allowed: false,
            provider_quota_policy: ProviderQuotaPolicy::default(),
        },
    };
    let refs = |values: &[&str]| -> Vec<CredentialRef> {
        values
            .iter()
            .map(|v| CredentialRef::new(*v).unwrap())
            .collect()
    };
    let (bytes, config) = edit_model_access(
        global,
        "",
        &pool(&["codex-connection", "local-connection"]),
        &[],
    )
    .unwrap();
    let local = config.settings.model_access.as_ref().unwrap().connections[1].clone();
    let local_target = configured_profile_ref(&local).unwrap();
    assert_eq!(
        config.settings.routing.approved_accounts,
        refs(&["owner-account", "codex-account", "local-account"])
    );
    assert_eq!(
        config.settings.allowed_destinations,
        vec![
            CredentialRef::new("owner-target").unwrap(),
            CredentialRef::new("codex-connection").unwrap(),
            local_target.clone()
        ]
    );
    let (bytes, _) = edit_model_access(&bytes, "", &pool(&["local-connection"]), &[]).unwrap();
    let removal = ModelAccessEdit::RemoveConnectedAccount {
        id: CredentialRef::new("codex-connection").unwrap(),
    };
    let (_, config) = edit_model_access(&bytes, "", &removal, &[]).unwrap();
    assert_eq!(
        config.settings.routing.approved_accounts,
        refs(&["owner-account", "local-account"])
    );
    assert_eq!(
        config.settings.allowed_destinations,
        vec![CredentialRef::new("owner-target").unwrap(), local_target]
    );
}

#[test]
fn an_edit_keeps_a_routing_table_that_approves_nothing() {
    let global = "version = 1\n\n[routing]\npriority_weight = 2\n";
    let edit = ModelAccessEdit::AddAccount {
        account_ref: CredentialRef::new("account").unwrap(),
    };
    let (bytes, config) = edit_model_access(global, "", &edit, &[]).unwrap();
    assert!(!bytes.contains("approved_accounts"), "{bytes}");
    assert!(!bytes.contains("allowed_destinations"), "{bytes}");
    assert_eq!(config.settings.routing.priority_weight, 2);
}

#[test]
fn approving_more_than_sixty_four_accounts_names_the_limit() {
    let connections: Vec<String> = (0..65)
            .map(|index| format!(r#"{{ id = "c{index}", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "a{index}", endpoint = {{ kind = "http", url = "http://127.0.0.1:11434/v1" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}"#))
            .collect();
    let accounts: Vec<String> = (0..65).map(|index| format!("\"a{index}\"")).collect();
    let pools: Vec<String> = (0..5)
            .map(|pool| {
                let members: Vec<String> = (pool * 13..pool * 13 + 13)
                    .map(|index| format!("\"c{index}\""))
                    .collect();
                format!(
                    r#"{{ id = "p{pool}", members = [{}], projects = ["demo"], roles = [], fallback_allowed = false }}"#,
                    members.join(", ")
                )
            })
            .collect();
    let global = format!(
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [{}]\nconnections = [{}]\npools = [{}]\n",
        accounts.join(", "),
        connections.join(", "),
        pools.join(", ")
    );
    let edit = ModelAccessEdit::AddAccount {
        account_ref: CredentialRef::new("spare").unwrap(),
    };
    let failure = edit_model_access(&global, "", &edit, &[]).unwrap_err();
    assert_eq!(failure.0, "approved_accounts_limit");
    assert!(plain_problem(&failure.0).unwrap().contains("64"));
    let (_, config) = edit_model_access(
        &global.replace(r#"projects = ["demo"]"#, "projects = []"),
        "",
        &edit,
        &[],
    )
    .unwrap();
    assert!(config.settings.routing.approved_accounts.is_empty());
}

fn many_connections_global(count: usize) -> String {
    let connections: Vec<String> = (0..count)
            .map(|index| format!(r#"{{ id = "c{index}", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "http://127.0.0.1:11434/v1" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}"#))
            .collect();
    format!(
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\nconnections = [{}]\npools = []\n",
        connections.join(", ")
    )
}

fn sized_pool(id: &str, members: usize, projects: &[&str]) -> AccountPool {
    AccountPool {
        id: CredentialRef::new(id).unwrap(),
        name: None,
        members: (0..members)
            .map(|index| CredentialRef::new(format!("c{index}")).unwrap())
            .collect(),
        projects: projects
            .iter()
            .map(|p| CredentialRef::new(*p).unwrap())
            .collect(),
        roles: vec![CredentialRef::new("project_leader").unwrap()],
        fallback_allowed: false,
        provider_quota_policy: ProviderQuotaPolicy::default(),
    }
}

#[test]
fn a_pool_holds_at_most_fifteen_accounts() {
    let global = many_connections_global(16);
    let full = sized_pool("work", 15, &[]);
    let (bytes, config) = edit_model_access(
        &global,
        "",
        &ModelAccessEdit::CreatePool { pool: full },
        &[],
    )
    .unwrap();
    assert_eq!(
        config.settings.model_access.unwrap().pools[0].members.len(),
        15
    );
    let grown = ModelAccessEdit::ReplacePool {
        pool: sized_pool("work", 16, &[]),
    };
    assert_eq!(
        edit_model_access(&bytes, "", &grown, &[]).unwrap_err().0,
        "pool_member_limit"
    );
    let created = ModelAccessEdit::CreatePool {
        pool: sized_pool("other", 16, &[]),
    };
    assert_eq!(
        edit_model_access(&bytes, "", &created, &[]).unwrap_err().0,
        "pool_member_limit"
    );
    assert_eq!(
        plain_problem("pool_member_limit"),
        Some("A pool can hold up to 15 accounts")
    );
    for code in [
        "account_not_found",
        "project_not_found",
        "model_access_read_only",
        "model_access_edit_bounds",
    ] {
        let sentence = plain_problem(code).unwrap();
        assert!(sentence.ends_with('.') && !sentence.contains('_'), "{code}");
    }
}

#[test]
fn the_same_account_may_belong_to_many_pools() {
    let global = many_connections_global(2);
    let (bytes, _) = edit_model_access(
        &global,
        "",
        &ModelAccessEdit::CreatePool {
            pool: sized_pool("first", 2, &[]),
        },
        &[],
    )
    .unwrap();
    let (_, config) = edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::CreatePool {
            pool: sized_pool("second", 1, &["demo"]),
        },
        &[],
    )
    .unwrap();
    let pools = config.settings.model_access.unwrap().pools;
    assert_eq!(pools.len(), 2);
    assert!(
        pools
            .iter()
            .all(|pool| pool.members.contains(&CredentialRef::new("c0").unwrap()))
    );
}

fn pools_of(config: &EffectiveConfig) -> Vec<AccountPool> {
    config.settings.model_access.clone().unwrap().pools
}

fn projects_of(pools: &[AccountPool], id: &str) -> Vec<String> {
    pools
        .iter()
        .find(|pool| pool.id.as_str() == id)
        .unwrap()
        .projects
        .iter()
        .map(|project| project.as_str().to_owned())
        .collect()
}

#[test]
fn a_project_moves_to_another_pool_or_to_none_in_one_edit() {
    let global = many_connections_global(2);
    let (bytes, _) = edit_model_access(
        &global,
        "",
        &ModelAccessEdit::CreatePool {
            pool: sized_pool("first", 1, &["demo", "other"]),
        },
        &[],
    )
    .unwrap();
    let mut second = sized_pool("second", 2, &[]);
    second.roles.clear();
    let (bytes, _) = edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::CreatePool { pool: second },
        &[],
    )
    .unwrap();
    let assign = |pool: Option<&str>| ModelAccessEdit::AssignProjectPool {
        project: CredentialRef::new("demo").unwrap(),
        pool: pool.map(|id| CredentialRef::new(id).unwrap()),
    };
    let (bytes, config) = edit_model_access(&bytes, "", &assign(Some("second")), &[]).unwrap();
    let pools = pools_of(&config);
    assert_eq!(projects_of(&pools, "first"), ["other"]);
    assert_eq!(projects_of(&pools, "second"), ["demo"]);
    assert!(
        pools[1]
            .roles
            .contains(&CredentialRef::new(LEADER_ROLE).unwrap())
    );
    let (bytes, config) = edit_model_access(&bytes, "", &assign(None), &[]).unwrap();
    let pools = pools_of(&config);
    assert_eq!(projects_of(&pools, "first"), ["other"]);
    assert!(projects_of(&pools, "second").is_empty());
    assert_eq!(pools.len(), 2);
    assert_eq!(
        edit_model_access(&bytes, "", &assign(Some("missing")), &[])
            .unwrap_err()
            .0,
        "pool_not_found"
    );
    let full: Vec<String> = (0..POOL_PROJECTS_MAX).map(|i| format!("p{i}")).collect();
    let full: Vec<&str> = full.iter().map(String::as_str).collect();
    let (bytes, _) = edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::ReplacePool {
            pool: sized_pool("second", 2, &full),
        },
        &[],
    )
    .unwrap();
    assert_eq!(
        edit_model_access(&bytes, "", &assign(Some("second")), &[])
            .unwrap_err()
            .0,
        "pool_project_limit"
    );
    assert_eq!(
        plain_problem("pool_project_limit"),
        Some("A pool can serve up to 64 projects. Pick another pool for this project.")
    );
}

#[test]
fn deleting_a_pool_takes_it_off_every_project_that_uses_it() {
    let global = many_connections_global(1);
    let (bytes, _) = edit_model_access(
        &global,
        "",
        &ModelAccessEdit::CreatePool {
            pool: sized_pool("work", 1, &["demo", "other"]),
        },
        &[],
    )
    .unwrap();
    let remove = ModelAccessEdit::RemovePool {
        id: CredentialRef::new("work").unwrap(),
    };
    let (_, config) = edit_model_access(&bytes, "", &remove, &[]).unwrap();
    assert!(pools_of(&config).is_empty());
    assert_eq!(config.settings.model_access.unwrap().connections.len(), 1);
    assert_eq!(
        edit_model_access(&global, "", &remove, &[]).unwrap_err().0,
        "pool_not_found"
    );
}

#[test]
fn deleting_an_account_takes_it_out_of_its_pools_and_deletes_pools_it_leaves_empty() {
    let global = many_connections_global(2).replace(
            "connections = [{ id = \"c0\", provider = \"openai-compatible\", harness = \"rcli\", protocol = \"openai-chat\", account_ref = \"local-account\"",
            "connections = [{ id = \"c0\", provider = \"openai-compatible\", harness = \"rcli\", protocol = \"openai-chat\", account_ref = \"solo-account\"",
        ).replace("accounts = [\"local-account\"]", "accounts = [\"local-account\", \"solo-account\"]");
    let mut bytes = global;
    for pool in [
        sized_pool("both", 2, &["demo"]),
        sized_pool("only", 1, &["other"]),
    ] {
        bytes = edit_model_access(&bytes, "", &ModelAccessEdit::CreatePool { pool }, &[])
            .unwrap()
            .0;
    }
    let c0 = CredentialRef::new("c0").unwrap();
    let (_, config) = edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::RemoveConnectedAccount { id: c0.clone() },
        &[],
    )
    .unwrap();
    let catalog = config.settings.model_access.unwrap();
    assert_eq!(catalog.pools.len(), 1);
    assert_eq!(catalog.pools[0].id.as_str(), "both");
    assert_eq!(
        catalog.pools[0].members,
        [CredentialRef::new("c1").unwrap()]
    );
    assert_eq!(projects_of(&catalog.pools, "both"), ["demo"]);
    assert!(catalog.connections.iter().all(|c| c.id != c0));
    assert_eq!(
        catalog.accounts,
        [CredentialRef::new("local-account").unwrap()]
    );
}

#[test]
fn a_pool_needs_at_least_one_account() {
    let global = many_connections_global(1);
    let (bytes, _) = edit_model_access(
        &global,
        "",
        &ModelAccessEdit::CreatePool {
            pool: sized_pool("work", 1, &["demo"]),
        },
        &[],
    )
    .unwrap();
    for edit in [
        ModelAccessEdit::ReplacePool {
            pool: sized_pool("work", 0, &["demo"]),
        },
        ModelAccessEdit::CreatePool {
            pool: sized_pool("empty", 0, &[]),
        },
    ] {
        assert_eq!(
            edit_model_access(&bytes, "", &edit, &[]).unwrap_err().0,
            "pool_needs_account"
        );
    }
    assert_eq!(
        plain_problem("pool_needs_account"),
        Some("A pool needs at least one account. Add another account first, or delete the pool.")
    );
}

#[test]
fn a_pool_name_is_optional_bounded_and_renamable() {
    let global = many_connections_global(1);
    let mut pool = sized_pool("work", 1, &[]);
    let (bytes, config) = edit_model_access(
        &global,
        "",
        &ModelAccessEdit::CreatePool { pool: pool.clone() },
        &[],
    )
    .unwrap();
    assert_eq!(config.settings.model_access.unwrap().pools[0].name, None);
    assert!(!bytes.contains("name ="));
    pool.name = Some("Daily work".into());
    let (bytes, config) = edit_model_access(
        &bytes,
        "",
        &ModelAccessEdit::ReplacePool { pool: pool.clone() },
        &[],
    )
    .unwrap();
    let stored = &config.settings.model_access.unwrap().pools[0];
    assert_eq!(stored.name.as_deref(), Some("Daily work"));
    assert_eq!(stored.id.as_str(), "work");
    for invalid in ["", "   ", "tab\there", &"x".repeat(65)] {
        pool.name = Some(invalid.into());
        let edit = ModelAccessEdit::ReplacePool { pool: pool.clone() };
        assert_eq!(
            edit_model_access(&bytes, "", &edit, &[]).unwrap_err().0,
            "pool_name_bounds"
        );
    }
    pool.name = Some("é".repeat(64));
    edit_model_access(&bytes, "", &ModelAccessEdit::ReplacePool { pool }, &[]).unwrap();
}

/// DEC-180 Task 20 Q8: an HTTP account stored for a harness that runs listed masters and
/// fits it is its own master target once its pool is assigned, as a signed-in account is;
/// one stored for a harness without the master role gets none.
#[test]
fn a_listed_master_account_is_its_own_master_target() {
    let connection = |harness: &str| -> ModelConnection {
        serde_json::from_value(serde_json::json!({
            "id": "listed-connection",
            "provider": "google",
            "harness": harness,
            "protocol": "openai-chat",
            "account_ref": "listed-account",
            "endpoint": {"kind": "http", "url": "http://127.0.0.1:9/v1"},
            "model": {"id": "m-1", "author": "google"},
            "auth": {"method": "none"},
        }))
        .unwrap()
    };
    let listed = connection("standin-acp");
    assert!(crate::master_session::listed_master("standin-acp").is_ok());
    assert_eq!(connection_destination(&listed), Some(listed.id.clone()));
    let other = connection("gemini-cli");
    assert!(crate::master_session::listed_master("gemini-cli").is_err());
    assert_eq!(connection_destination(&other), None);
}

#[test]
fn unassigned_pools_approve_nothing() {
    let (_, config) = edit_model_access(ASSIGNMENT_GLOBAL, "", &assigned_pool(&[]), &[]).unwrap();
    assert!(config.settings.routing.approved_accounts.is_empty());
    assert!(config.settings.allowed_destinations.is_empty());
}

#[test]
fn legacy_configuration_keeps_its_identity_and_default_routing() {
    let root = tempfile::tempdir().unwrap();
    let mut config = resolve_config(&root.path().join("g"), &root.path().join("p")).unwrap();
    config
        .descriptors
        .retain(|name, _| !name.starts_with("routing."));
    config.id =
        ConfigId::new("472693a858862960f058c909a411a19aa8f8d03e94240e0aa4ccb8c1ad4f3e0c").unwrap();
    let mut stored = serde_json::to_value(&config).unwrap();
    assert!(stored["settings"].get("routing").is_none());
    let decoded: EffectiveConfig = serde_json::from_value(stored.clone()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded.settings.routing, RoutingPolicy::default());
    assert_eq!(serde_json::to_value(decoded).unwrap(), stored);
    stored["settings"]["routing"] = serde_json::to_value(RoutingPolicy {
        fallback_allowed: false,
        ..RoutingPolicy::default()
    })
    .unwrap();
    assert!(
        serde_json::from_value::<EffectiveConfig>(stored)
            .unwrap()
            .validate()
            .is_err()
    );
}

#[test]
fn routing_fields_merge_with_provenance_and_shared_capacity_stays_global() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("g");
    let project = root.path().join("p");
    std::fs::write(&global,
            "[routing]\npreference_order=['beta','alpha']\napproved_accounts=['account']\nscheduling='StrictPriority'").unwrap();
    std::fs::write(
        &project,
        "[routing]\nmailbox_capacity=4\nfallback_allowed=false",
    )
    .unwrap();
    let config = resolve_config(&global, &project).unwrap();
    config.validate().unwrap();
    assert_eq!(config.settings.routing.preference_order[0].as_str(), "beta");
    assert_eq!(
        config.descriptors["routing.preference_order"].origin,
        Origin::Global
    );
    assert_eq!(
        config.descriptors["routing.mailbox_capacity"].origin,
        Origin::Project
    );
    assert_eq!(
        config.descriptors["routing.scheduling"].effect_timing,
        EffectTiming::Immediate
    );
    assert!(!config.settings.routing.fallback_allowed);
    std::fs::write(&project, "[routing]\nscheduling='FairShare'").unwrap();
    assert!(resolve_config(&global, &project).is_err());
    std::fs::write(&project,
            "[routing.global_capacity]\nslots=999\ncpu_millis=1000\nmemory_mib=512\nprocesses=4\noutput_bytes=1048576\nwall_time_ms=60000\nspend_microunits=0").unwrap();
    assert!(resolve_config(&global, &project).is_err());
}

#[test]
fn real_file_override_and_metadata() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    std::fs::write(
        &global,
        "version = 1\ntimeout_ms = 2500\nqueue_capacity = 8",
    )
    .unwrap();
    std::fs::write(&project, "version = 1\nqueue_capacity = 3").unwrap();
    let cfg = resolve_config(&global, &project).unwrap();
    assert_eq!(cfg.settings.queue_capacity, 3);
    assert_eq!(cfg.settings.timeout_ms, 2500);
    assert_eq!(cfg.descriptors["queue_capacity"].origin, Origin::Project);
    assert_eq!(cfg.descriptors["timeout_ms"].origin, Origin::Global);
}

#[test]
fn malformed_reload_preserves_last_valid_configuration() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    let mut state = ConfigState {
        current: resolve_config(&global, &project).unwrap(),
    };
    let original = state.current.clone();
    for invalid in [
        "queue_capacity = 0",
        "timeout_ms = -1",
        "credential_value = 'secret'",
        "version = 2",
        "bad toml",
        "allowed_destinations = ['../token']",
    ] {
        std::fs::write(&project, invalid).unwrap();
        assert!(state.reload(&global, &project).is_err(), "{invalid}");
        assert_eq!(state.current, original);
    }
}

#[test]
fn newer_settings_file_is_refused_as_newer_than_engine() {
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global.toml");
    let project = root.path().join("project.toml");
    let newer = format!("version = {}\ntimeout_ms = 2000\n", SETTINGS_VERSION + 1);
    std::fs::write(&global, &newer).unwrap();
    let refused = resolve_config(&global, &project).unwrap_err().to_string();
    assert!(
        refused.starts_with("settings_newer_than_engine:"),
        "{refused}"
    );
    assert_eq!(std::fs::read_to_string(&global).unwrap(), newer);
    std::fs::write(&global, "version = 0\n").unwrap();
    assert_eq!(
        resolve_config(&global, &project).unwrap_err().to_string(),
        "unsupported config version"
    );
    std::fs::write(&global, format!("version = {SETTINGS_VERSION}\n")).unwrap();
    assert!(resolve_config(&global, &project).is_ok());
}

/// `plugins.when_turned_off` is a working owner setting: absent it reads
/// `refuse_new_calls`, adds no descriptor and keeps the config id; set in the global or
/// a project file it reads `stop_runs`, takes effect at once and names its origin.
#[test]
fn plugins_when_turned_off_is_a_working_setting() {
    use kernel_plugin_host::usage::TurnOff;
    let version = format!("version = {SETTINGS_VERSION}\n");
    let plain = resolve_config_bytes(&version, "").unwrap();
    assert_eq!(
        plain.settings.plugins.when_turned_off,
        TurnOff::RefuseNewCalls
    );
    assert!(!plain.descriptors.contains_key("plugins.when_turned_off"));
    assert_eq!(
        plain.id,
        from_settings(Settings::default(), plain.descriptors.clone())
            .unwrap()
            .id
    );
    let set = format!("{version}[plugins]\nwhen_turned_off = \"stop_runs\"\n");
    for (global, project, origin) in [
        (set.as_str(), "", Origin::Global),
        (
            version.as_str(),
            "[plugins]\nwhen_turned_off = \"stop_runs\"\n",
            Origin::Project,
        ),
    ] {
        let config = resolve_config_bytes(global, project).unwrap();
        assert_eq!(config.settings.plugins.when_turned_off, TurnOff::StopRuns);
        let descriptor = &config.descriptors["plugins.when_turned_off"];
        assert_eq!(descriptor.value, serde_json::json!("stop_runs"));
        assert_eq!(descriptor.default, serde_json::json!("refuse_new_calls"));
        assert_eq!(descriptor.origin, origin);
        assert_eq!(descriptor.effect_timing, EffectTiming::Immediate);
        assert_ne!(config.id, plain.id);
        config.validate().unwrap();
    }
    assert!(
        resolve_config_bytes(
            &format!("{version}[plugins]\nwhen_turned_off = \"stop\"\n"),
            ""
        )
        .is_err()
    );
    assert!(resolve_config_bytes(&format!("{version}[plugins]\nother = 1\n"), "").is_err());
}

/// `plugins.essential_unlocked` is a working owner setting: absent it reads false and
/// adds no descriptor; set true in a file it reads true, takes effect at once, names its
/// origin and lists with the other plugin setting; a value that is no bool is refused.
#[test]
fn plugins_essential_unlocked_is_a_working_setting() {
    let version = format!("version = {SETTINGS_VERSION}\n");
    let plain = resolve_config_bytes(&version, "").unwrap();
    assert!(!plain.settings.plugins.essential_unlocked);
    assert!(!plain.descriptors.contains_key("plugins.essential_unlocked"));
    let set = format!("{version}[plugins]\nessential_unlocked = true\n");
    let config = resolve_config_bytes(&set, "").unwrap();
    assert!(config.settings.plugins.essential_unlocked);
    let descriptor = &config.descriptors["plugins.essential_unlocked"];
    assert_eq!(descriptor.value, serde_json::json!(true));
    assert_eq!(descriptor.default, serde_json::json!(false));
    assert_eq!(descriptor.origin, Origin::Global);
    assert_eq!(descriptor.effect_timing, EffectTiming::Immediate);
    assert!(config.descriptors.contains_key("plugins.when_turned_off"));
    assert_ne!(config.id, plain.id);
    config.validate().unwrap();
    assert!(
        resolve_config_bytes(
            &format!("{version}[plugins]\nessential_unlocked = \"yes\"\n"),
            ""
        )
        .is_err()
    );
}
