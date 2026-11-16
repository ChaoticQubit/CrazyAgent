#![cfg(unix)]

mod research_support;

use crazyagent::config::{ModelAccess, ModelAuthentication, ModelConnection};
use crazyagent::protocol::CredentialRef;
use crazyagent::research::accounts::{AccountPick, pick_account, pick_scheduled, pick_unpaused};
use crazyagent::research::runner::ResearchAccount;
use crazyagent::scheduling::{AccountPlan, Choice, Pin, ScheduleChange};
use crazyagent::usage;
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

/// The pick of mixed-pool's keyed OpenAI account, which Codex reaches through the engine
/// key relay since DEC-141 PA-4 and which mixed-pool lists ahead of its Codex sign-in.
fn keyed() -> AccountPick {
    AccountPick::Account {
        connection: CredentialRef::new("key-connection").unwrap(),
        account: Box::new(ResearchAccount::Endpoint {
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-5".into(),
            key: Some(CredentialRef::new("openai-key").unwrap()),
        }),
    }
}

/// Pauses only the keyed account, so a pick goes on to the members after it.
/// Input: a connection. Output: the end of its pause.
fn key_paused(id: &CredentialRef) -> Option<i64> {
    (id.as_str() == "key-connection").then_some(3_000)
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

/// Codex takes mixed-pool's members in pool order: the keyed account listed first, then,
/// while that one is paused, its own signed-in account, never Claude Code's sign-in.
#[test]
fn a_mixed_pool_gives_codex_its_keyed_account_then_its_first_signed_in_managed_account() {
    let access = access();
    assert_eq!(
        pick_account(&access, "project-a", "codex", signed_in),
        keyed()
    );
    assert_eq!(
        pick_unpaused(&access, "project-a", "codex", signed_in, key_paused),
        AccountPick::Account {
            connection: CredentialRef::new("codex-connection").unwrap(),
            account: Box::new(ResearchAccount::ManagedCodex {
                connection: connection(&access, "codex-connection"),
                model: None,
            }),
        }
    );
}

/// A signed-out Codex account is passed over for the keyless endpoint after it. The keyed
/// account ahead of both needs no sign-in, so it is paused here to reach them.
#[test]
fn a_signed_out_managed_account_falls_through_to_a_keyless_endpoint() {
    let access = access();
    assert_eq!(
        pick_account(&access, "project-a", "codex", signed_out),
        keyed()
    );
    assert_eq!(
        pick_unpaused(&access, "project-a", "codex", signed_out, key_paused),
        AccountPick::Account {
            connection: CredentialRef::new("local-connection").unwrap(),
            account: Box::new(ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen2.5:0.5b".into(),
                key: None,
            }),
        }
    );
    assert_eq!(
        pick_account(&access, "project-remote", "codex", signed_out),
        AccountPick::Account {
            connection: CredentialRef::new("remote-connection").unwrap(),
            account: Box::new(ResearchAccount::Endpoint {
                base_url: "https://models.example.test/v1".into(),
                model: "big-model".into(),
                key: None,
            }),
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
    assert_eq!(folder, runtime.folder("project-a"));
    assert!(folder.starts_with(&runtime.home));
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
            account: Box::new(ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen2.5:0.5b".into(),
                key: None,
            }),
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
            account: Box::new(ResearchAccount::ManagedClaude {
                connection: connection(&access, "claude-connection"),
                model: None,
            }),
        }
    );
    let anthropic = AccountPick::Account {
        connection: CredentialRef::new("anthropic-local-connection").unwrap(),
        account: Box::new(ResearchAccount::AnthropicEndpoint {
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        }),
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
            account: Box::new(ResearchAccount::Endpoint {
                base_url: "http://127.0.0.1:11434/v1".into(),
                model: "qwen2.5:0.5b".into(),
                key: None,
            }),
        }
    );
    assert_eq!(
        pick_account(&access, "project-anthropic", "rcli", signed_in),
        AccountPick::NoUsableAccount
    );
}

/// Codex's mixed-pool members are the keyed account, its sign-in and the local endpoint, in
/// that order: each pause hands research to the next one, and with all three paused it waits
/// for the earliest pause to end.
#[test]
fn a_paused_pool_account_hands_research_to_the_next_member_or_waits_until_the_first_pause_ends() {
    let access = access();
    let codex = AccountPick::Account {
        connection: CredentialRef::new("codex-connection").unwrap(),
        account: Box::new(ResearchAccount::ManagedCodex {
            connection: connection(&access, "codex-connection"),
            model: None,
        }),
    };
    let local = AccountPick::Account {
        connection: CredentialRef::new("local-connection").unwrap(),
        account: Box::new(ResearchAccount::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        }),
    };
    assert_eq!(
        pick_unpaused(&access, "project-a", "codex", signed_in, key_paused),
        codex
    );
    let key_and_codex_paused = |id: &CredentialRef| match id.as_str() {
        "key-connection" => Some(3_000),
        "codex-connection" => Some(2_000),
        _ => None,
    };
    assert_eq!(
        pick_unpaused(
            &access,
            "project-a",
            "codex",
            signed_in,
            key_and_codex_paused
        ),
        local
    );
    let all_paused = |id: &CredentialRef| match id.as_str() {
        "key-connection" => Some(3_000),
        "codex-connection" => Some(2_000),
        "local-connection" => Some(1_000),
        _ => None,
    };
    let pick = pick_unpaused(&access, "project-a", "codex", signed_in, all_paused);
    assert_eq!(pick, AccountPick::Paused { until_ms: 1_000 });
    assert_eq!(
        pick.label(),
        "Every account in this project's pool that can run this research hit its rate limit"
    );
    assert_eq!(
        pick_unpaused(&access, "project-a", "codex", signed_out, all_paused),
        AccountPick::Paused { until_ms: 1_000 }
    );
    let claude_paused = |id: &CredentialRef| (id.as_str() == "claude-connection").then_some(5_000);
    assert_eq!(
        pick_unpaused(
            &access,
            "project-claude",
            "claude",
            signed_in,
            claude_paused
        ),
        AccountPick::Paused { until_ms: 5_000 }
    );
    assert_eq!(
        pick_unpaused(
            &access,
            "project-claude",
            "claude",
            signed_out,
            claude_paused
        ),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        pick_unpaused(&access, "project-claude", "codex", signed_in, claude_paused),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        pick_unpaused(
            &access,
            "project-unassigned",
            "codex",
            signed_in,
            all_paused
        ),
        AccountPick::NeedsPool
    );
    assert_eq!(
        pick_unpaused(&access, "project-a", "codex", signed_in, |_| None),
        pick_account(&access, "project-a", "codex", signed_in)
    );
}

/// Writes settings with one local account in one pool serving `projects`, a TOML list, and
/// opens the resolver research picks through. Takes the engine runtime and the list. Returns
/// the resolver.
fn local_pool_resolver(
    runtime: &research_support::EngineRuntime,
    projects: &str,
) -> crazyagent::research::accounts::SettingsResolver {
    let root = runtime.root.clone();
    let global = root.join("research-global.toml");
    let project = root.join("research-project.toml");
    std::fs::write(&project, "").unwrap();
    std::fs::write(
        &global,
        format!("version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"local-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = {{ kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }}\nmodel = {{ id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }}\nauth = {{ method = \"none\" }}\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\"]\nprojects = {projects}\nroles = [\"project_leader\"]\nfallback_allowed = false\n"),
    )
    .unwrap();
    crazyagent::research::accounts::SettingsResolver::new(
        &global,
        &project,
        &root.join("projects"),
        &runtime.database,
    )
    .unwrap()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A research run that reports a rate limit goes back to the queue without spending an
/// attempt. The report comes from what the agent's harness wrote, so its account rests for the
/// run's own project only: another project in the pool still gets it, and nothing rests it
/// everywhere (SECAUDIT-RATELIMIT-FORGE). A longer rest already set is kept, and the account
/// comes back once the rest ends.
#[test]
fn a_rate_limited_research_run_holds_its_account_for_its_own_project_without_spending_an_attempt() {
    use crazyagent::research::jobs::{ResearchJobs, ResearchStatus};
    let runtime = research_support::EngineRuntime::with_projects(&[
        ("project-a", "Sells house plants."),
        ("project-b", "Bakes bread."),
    ]);
    let resolver = local_pool_resolver(&runtime, "[\"project-a\", \"project-b\"]");
    for project in ["project-a", "project-b"] {
        assert!(matches!(
            resolver.resolve(project, "codex").unwrap().1,
            AccountPick::Account { .. }
        ));
    }
    let jobs = ResearchJobs::open(&runtime.database).unwrap();
    let id = jobs
        .enqueue("project-a", "market_research", "codex", false)
        .unwrap();
    let held = jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
    assert_eq!(held.attempts, 1);
    let local = CredentialRef::new("local-connection").unwrap();
    let before = now_ms();
    let (status, until) = jobs.rate_limited(&held, &local).unwrap();
    assert_eq!(status, ResearchStatus::Queued);
    assert!(
        until >= before + 600_000 && until <= now_ms() + 600_000,
        "{until}"
    );
    let job = jobs.get(id).unwrap().unwrap();
    assert_eq!(job.status, ResearchStatus::Queued);
    assert_eq!(job.attempts, 0);
    assert_eq!(job.claimer, None);
    assert!(
        matches!(
            resolver.resolve("project-b", "codex").unwrap().1,
            AccountPick::Account { .. }
        ),
        "another project in the pool still gets the account"
    );
    let database = rusqlite::Connection::open(&runtime.database).unwrap();
    let everywhere: i64 = database
        .query_row("SELECT COUNT(*) FROM team_paused_accounts", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(everywhere, 0, "nothing rests the account everywhere");
    assert_eq!(
        resolver.resolve("project-a", "codex").unwrap().1,
        AccountPick::Paused { until_ms: until }
    );
    let longer = until + 3_600_000;
    let hold = "UPDATE team_project_pauses SET until_ms=?1 WHERE project='project-a' AND connection='local-connection'";
    assert_eq!(database.execute(hold, [longer]).unwrap(), 1);
    let held = jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
    assert_eq!(jobs.rate_limited(&held, &local).unwrap().1, longer);
    assert_eq!(
        resolver.resolve("project-a", "codex").unwrap().1,
        AccountPick::Paused { until_ms: longer }
    );
    assert_eq!(database.execute(hold, [now_ms() - 1]).unwrap(), 1);
    assert!(matches!(
        resolver.resolve("project-a", "codex").unwrap().1,
        AccountPick::Account { .. }
    ));
}

/// A reported limit asks the usage service for a poll at once, so the provider's own reading
/// can hold a full account everywhere, but reports within a minute of the last ask add no
/// poll: a run that keeps reporting cannot make the engine poll more than once a minute
/// (SECAUDIT-RATELIMIT-FORGE).
#[test]
fn a_reported_limit_asks_the_usage_service_for_at_most_one_poll_a_minute() {
    use crazyagent::research::jobs::ResearchJobs;
    let runtime =
        research_support::EngineRuntime::with_projects(&[("project-a", "Sells house plants.")]);
    let _resolver = local_pool_resolver(&runtime, "[\"project-a\"]");
    let database = rusqlite::Connection::open(&runtime.database).unwrap();
    usage::initialize(&database).unwrap();
    let asked = || -> Option<i64> {
        database
            .query_row(
                "SELECT requested_ms FROM usage_refresh WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .ok()
    };
    assert_eq!(asked(), None);
    let jobs = ResearchJobs::open(&runtime.database).unwrap();
    jobs.enqueue("project-a", "market_research", "codex", false)
        .unwrap();
    let local = CredentialRef::new("local-connection").unwrap();
    let report = || {
        let held = jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
        jobs.rate_limited(&held, &local).unwrap();
    };
    let before = now_ms();
    report();
    let first = asked().expect("the first report asks for a poll");
    assert!(first >= before && first <= now_ms(), "{first}");
    report();
    assert_eq!(
        asked(),
        Some(first),
        "a report within the minute asks again for nothing"
    );
    database
        .execute(
            "UPDATE usage_refresh SET requested_ms=requested_ms-60001 WHERE singleton=1",
            [],
        )
        .unwrap();
    let later = now_ms();
    report();
    assert!(
        asked().is_some_and(|at| at >= later),
        "a report a minute after the last ask asks again"
    );
}

/// Project-a's agent reports a limit on the pool's one account and the confirm poll answers.
/// Takes the engine runtime and the polled weekly use with its reset. Returns the resolver and
/// the end of project-a's own hold.
fn reported_then_polled(
    runtime: &research_support::EngineRuntime,
    used_percent: f64,
    resets_at_ms: i64,
) -> (crazyagent::research::accounts::SettingsResolver, i64) {
    let resolver = local_pool_resolver(runtime, "[\"project-a\", \"project-b\"]");
    let database = rusqlite::Connection::open(&runtime.database).unwrap();
    usage::initialize(&database).unwrap();
    let jobs = crazyagent::research::jobs::ResearchJobs::open(&runtime.database).unwrap();
    jobs.enqueue("project-a", "market_research", "codex", false)
        .unwrap();
    let held = jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
    let local = CredentialRef::new("local-connection").unwrap();
    let (_, hold) = jobs.rate_limited(&held, &local).unwrap();
    usage::record_observation(
        &database,
        &local,
        &usage::Observation {
            plan: None,
            five_hour: None,
            weekly: Some(usage::LimitWindow {
                used_percent,
                resets_at_ms: Some(resets_at_ms),
            }),
            provider: Some(usage::poll::Provider::Codex),
        },
        usage::LimitSource::Poll,
        now_ms(),
    )
    .unwrap();
    (resolver, hold)
}

/// When the confirm poll reads the account's week as used up, the provider's own reading holds
/// it for every project until its reset, not only for the project whose agent reported it; once
/// the reset passes, the other project takes it again while the reporting project's own hold
/// still runs (SECAUDIT-RATELIMIT-FORGE, design row 2).
#[test]
fn a_polled_full_window_holds_the_account_for_every_project_until_its_reset() {
    let runtime = research_support::EngineRuntime::with_projects(&[
        ("project-a", "Sells house plants."),
        ("project-b", "Bakes bread."),
    ]);
    let reset = now_ms() + 3_600_000;
    let (resolver, hold) = reported_then_polled(&runtime, 100.0, reset);
    assert!(hold < reset);
    for project in ["project-a", "project-b"] {
        assert_eq!(
            resolver.resolve(project, "codex").unwrap().1,
            AccountPick::Paused { until_ms: reset },
            "{project} waits for the provider's reset"
        );
    }
    let database = rusqlite::Connection::open(&runtime.database).unwrap();
    assert_eq!(
        database
            .execute(
                "UPDATE account_limits SET weekly_resets_ms=?1, weekly_polled_resets_ms=?1
                 WHERE account='local-connection'",
                [now_ms() - 1],
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        resolver.resolve("project-b", "codex").unwrap().1,
        AccountPick::Account { .. }
    ));
    assert_eq!(
        resolver.resolve("project-a", "codex").unwrap().1,
        AccountPick::Paused { until_ms: hold }
    );
}

/// When the confirm poll finds room on the account, the report stays project-a's own: project-b
/// keeps the account, and project-a gets it back when its hold ends (SECAUDIT-RATELIMIT-FORGE,
/// design row 4).
#[test]
fn a_poll_with_room_keeps_the_hold_to_the_reporting_project() {
    let runtime = research_support::EngineRuntime::with_projects(&[
        ("project-a", "Sells house plants."),
        ("project-b", "Bakes bread."),
    ]);
    let (resolver, hold) = reported_then_polled(&runtime, 40.0, now_ms() + 3_600_000);
    assert!(matches!(
        resolver.resolve("project-b", "codex").unwrap().1,
        AccountPick::Account { .. }
    ));
    assert_eq!(
        resolver.resolve("project-a", "codex").unwrap().1,
        AccountPick::Paused { until_ms: hold }
    );
    let database = rusqlite::Connection::open(&runtime.database).unwrap();
    assert_eq!(
        database
            .execute(
                "UPDATE team_project_pauses SET until_ms=?1 WHERE project='project-a'",
                [now_ms() - 1],
            )
            .unwrap(),
        1
    );
    assert!(matches!(
        resolver.resolve("project-a", "codex").unwrap().1,
        AccountPick::Account { .. }
    ));
}

#[test]
fn the_onboarding_pool_pick_skips_paused_accounts_and_waits_while_every_one_is_paused() {
    use crazyagent::research::accounts::SettingsResolver;
    use crazyagent::research::runner::ResearchHarness;
    let runtime =
        research_support::EngineRuntime::with_projects(&[("project-a", "Sells house plants.")]);
    let root = runtime.root.clone();
    let global = root.join("research-global.toml");
    let project = root.join("research-project.toml");
    std::fs::write(&project, "").unwrap();
    std::fs::write(
        &global,
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [\"anthropic-local-account\", \"local-account\", \"second-account\"]\n\n[[model_access.connections]]\nid = \"local-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.connections]]\nid = \"second-connection\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"second-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11435/v1\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.connections]]\nid = \"anthropic-local-connection\"\nprovider = \"anthropic-compatible\"\nharness = \"claude\"\nprotocol = \"anthropic-messages\"\naccount_ref = \"anthropic-local-account\"\nendpoint = { kind = \"http\", url = \"http://127.0.0.1:11434\" }\nmodel = { id = \"qwen2.5:0.5b\", author = \"anthropic-compatible\" }\nauth = { method = \"none\" }\n\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [\"local-connection\", \"second-connection\", \"anthropic-local-connection\"]\nprojects = [\"project-a\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n",
    )
    .unwrap();
    let resolver =
        SettingsResolver::new(&global, &project, &root.join("projects"), &runtime.database)
            .unwrap();
    let pool = CredentialRef::new("local-pool").unwrap();
    let endpoint = |id: &str, url: &str| AccountPick::Account {
        connection: CredentialRef::new(id).unwrap(),
        account: Box::new(ResearchAccount::Endpoint {
            base_url: url.into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        }),
    };
    let anthropic = AccountPick::Account {
        connection: CredentialRef::new("anthropic-local-connection").unwrap(),
        account: Box::new(ResearchAccount::AnthropicEndpoint {
            base_url: "http://127.0.0.1:11434".into(),
            model: "qwen2.5:0.5b".into(),
            key: None,
        }),
    };
    let both = [ResearchHarness::Codex, ResearchHarness::ClaudeCode];
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &both),
        endpoint("local-connection", "http://127.0.0.1:11434/v1")
    );
    let jobs = crazyagent::research::jobs::ResearchJobs::open(&runtime.database).unwrap();
    let pause = |connection: &str| {
        jobs.enqueue("project-a", "market_research", "codex", false)
            .unwrap();
        let held = jobs.claim("project-a", "runner-a", 1).unwrap().unwrap();
        let (_, until) = jobs
            .rate_limited(&held, &CredentialRef::new(connection).unwrap())
            .unwrap();
        let cancelled = jobs.request_cancel(held.id).unwrap();
        assert!(cancelled);
        until
    };
    let first = pause("local-connection");
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &both),
        endpoint("second-connection", "http://127.0.0.1:11435/v1")
    );
    let second = pause("second-connection");
    assert!(second >= first);
    assert_eq!(resolver.pool_pick("project-a", &pool, &both), anthropic);
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &[ResearchHarness::Codex]),
        AccountPick::Paused { until_ms: first }
    );
    let third = pause("anthropic-local-connection");
    assert!(third >= second);
    assert_eq!(
        resolver.pool_pick(
            "project-a",
            &pool,
            &[ResearchHarness::ClaudeCode, ResearchHarness::Codex]
        ),
        AccountPick::Paused { until_ms: first }
    );
    assert_eq!(
        resolver.pool_pick("project-b", &pool, &both),
        endpoint("local-connection", "http://127.0.0.1:11434/v1"),
        "the holds are project-a's own"
    );
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &[]),
        AccountPick::NoUsableAccount
    );
    assert_eq!(
        resolver.pool_pick(
            "project-a",
            &CredentialRef::new("absent-pool").unwrap(),
            &both
        ),
        AccountPick::NeedsPool
    );
}

/// Research follows the owner's schedule over mixed-pool's Codex members. With no schedule
/// it keeps pool order, so the keyed account comes first; under Use it or lose it the
/// accounts with a known weekly reset come before the keyed account, which has no reading;
/// with every account left out or held it waits for the held one's reset.
#[test]
fn research_follows_the_schedule_and_waits_for_a_held_account_to_reset() {
    let access = access();
    let directory = tempfile::tempdir().unwrap();
    let mut db = rusqlite::Connection::open(directory.path().join("engine.db")).unwrap();
    crazyagent::scheduling::initialize(&db).unwrap();
    usage::initialize(&db).unwrap();
    for (connection, weekly, resets) in [
        ("codex-connection", 95.0, 9000),
        ("local-connection", 10.0, 5000),
    ] {
        usage::record_observation(
            &db,
            &CredentialRef::new(connection).unwrap(),
            &usage::Observation {
                plan: None,
                five_hour: Some(usage::LimitWindow {
                    used_percent: 5.0,
                    resets_at_ms: None,
                }),
                weekly: Some(usage::LimitWindow {
                    used_percent: weekly,
                    resets_at_ms: Some(resets),
                }),
                provider: Some(usage::poll::Provider::Codex),
            },
            usage::LimitSource::Poll,
            1_000,
        )
        .unwrap();
    }
    let pool = CredentialRef::new("mixed-pool").unwrap();
    let choice = |db: &rusqlite::Connection| {
        Choice::read(db, &access, Some("project-a"), &[&pool], 1_000).unwrap()
    };
    let plan = |db: &mut rusqlite::Connection, id: &str, plan: AccountPlan| {
        let tx = db.transaction().unwrap();
        let change = ScheduleChange::Account {
            connection: CredentialRef::new(id).unwrap(),
            plan,
        };
        let version = crazyagent::scheduling::load(&tx).unwrap().version;
        let outcome = crazyagent::scheduling::apply(&tx, &access, &change, version).unwrap();
        assert_eq!(outcome, crazyagent::protocol::Outcome::Applied);
        tx.commit().unwrap();
    };
    let picked = |choice: &Choice| match pick_scheduled(
        &access,
        "project-a",
        "codex",
        signed_in,
        |_| None,
        choice,
    ) {
        AccountPick::Account { connection, .. } => connection.as_str().to_owned(),
        other => panic!("expected an account, got {other:?}"),
    };
    assert_eq!(
        picked(&Choice::default()),
        "key-connection",
        "without a schedule research keeps pool order"
    );
    assert_eq!(picked(&choice(&db)), "local-connection");
    plan(
        &mut db,
        "codex-connection",
        AccountPlan {
            rank: Some(1),
            ..AccountPlan::default()
        },
    );
    assert_eq!(picked(&choice(&db)), "local-connection");
    plan(
        &mut db,
        "local-connection",
        AccountPlan {
            pin: Some(Pin::UseLast),
            ..AccountPlan::default()
        },
    );
    assert_eq!(picked(&choice(&db)), "codex-connection");
    plan(
        &mut db,
        "codex-connection",
        AccountPlan {
            reserve_percent: 10,
            ..AccountPlan::default()
        },
    );
    assert_eq!(
        picked(&choice(&db)),
        "key-connection",
        "Use last puts local after the keyed account once codex is held"
    );
    plan(
        &mut db,
        "local-connection",
        AccountPlan {
            pin: Some(Pin::NeverUse),
            ..AccountPlan::default()
        },
    );
    assert_eq!(picked(&choice(&db)), "key-connection");
    plan(
        &mut db,
        "key-connection",
        AccountPlan {
            pin: Some(Pin::NeverUse),
            ..AccountPlan::default()
        },
    );
    assert_eq!(
        pick_scheduled(
            &access,
            "project-a",
            "codex",
            signed_in,
            |_| None,
            &choice(&db)
        ),
        AccountPick::Paused { until_ms: 9000 }
    );
}

/// Settings whose `local-pool` holds one keyless loopback endpoint member per port, in port
/// order, and is assigned to project-a. Input: the members' ports. Output: the file text.
#[cfg(target_os = "macos")]
fn loopback_pool(ports: &[u16]) -> String {
    let accounts: Vec<String> = (0..ports.len())
        .map(|n| format!("\"account-{n}\""))
        .collect();
    let members: Vec<String> = (0..ports.len())
        .map(|n| format!("\"member-{n}\""))
        .collect();
    let mut text = format!(
        "version = 1\ntimeout_ms = 2000\nallowed_destinations = []\n\n[model_access]\naccounts = [{}]\n",
        accounts.join(", ")
    );
    for (n, port) in ports.iter().enumerate() {
        text.push_str(&format!("\n[[model_access.connections]]\nid = \"member-{n}\"\nprovider = \"openai-compatible\"\nharness = \"rcli\"\nprotocol = \"openai-chat\"\naccount_ref = \"account-{n}\"\nendpoint = {{ kind = \"http\", url = \"http://127.0.0.1:{port}/v1\" }}\nmodel = {{ id = \"qwen2.5:0.5b\", author = \"openai-compatible\" }}\nauth = {{ method = \"none\" }}\n"));
    }
    text.push_str(&format!("\n[[model_access.pools]]\nid = \"local-pool\"\nmembers = [{}]\nprojects = [\"project-a\"]\nroles = [\"project_leader\"]\nfallback_allowed = false\n", members.join(", ")));
    text
}

/// A resolver over [`loopback_pool`] for project-a of `runtime`. Inputs: the runtime and the
/// members' ports. Output: the resolver.
#[cfg(target_os = "macos")]
fn loopback_resolver(
    runtime: &research_support::EngineRuntime,
    ports: &[u16],
) -> crazyagent::research::accounts::SettingsResolver {
    let global = runtime.root.join("research-global.toml");
    let project = runtime.root.join("research-project.toml");
    std::fs::write(&project, "").unwrap();
    std::fs::write(&global, loopback_pool(ports)).unwrap();
    crazyagent::research::accounts::SettingsResolver::new(
        &global,
        &project,
        &runtime.root.join("projects"),
        &runtime.database,
    )
    .unwrap()
}

/// The pick of loopback member `n` on `port` for Codex. Inputs: the member and its port.
/// Output: the pick.
#[cfg(target_os = "macos")]
fn loopback_member(n: usize, port: u16) -> AccountPick {
    AccountPick::Account {
        connection: CredentialRef::new(format!("member-{n}")).unwrap(),
        account: Box::new(ResearchAccount::Endpoint {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            model: "qwen2.5:0.5b".into(),
            key: None,
        }),
    }
}

/// A read-only pick (DEC-091 addendum) passes over a pool member whose loopback provider
/// refuses every connect and takes the next one, through both the project's pool and a pool
/// named outright. The plain pick research jobs and the service tick use makes no connect
/// and still gives the first member.
#[cfg(target_os = "macos")]
#[test]
fn a_read_only_pick_passes_over_a_member_whose_loopback_provider_refuses_connects() {
    use crazyagent::research::accounts::Reach;
    use crazyagent::research::runner::ResearchHarness;
    let runtime =
        research_support::EngineRuntime::with_projects(&[("project-a", "Sells house plants.")]);
    let (_held, down) = crazyagent::spawn_gate::refusing_tcp_port().unwrap();
    let up = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let up_port = up.local_addr().unwrap().port();
    let resolver = loopback_resolver(&runtime, &[down, up_port]);
    let pool = CredentialRef::new("local-pool").unwrap();
    let codex = [ResearchHarness::Codex];
    assert_eq!(
        resolver.pool_pick_with("project-a", &pool, &codex, Reach::Reachable),
        loopback_member(1, up_port)
    );
    assert_eq!(
        resolver
            .resolve_with("project-a", "codex", Reach::Reachable)
            .unwrap()
            .1,
        loopback_member(1, up_port)
    );
    assert_eq!(
        resolver.pool_pick("project-a", &pool, &codex),
        loopback_member(0, down)
    );
    assert_eq!(
        resolver.resolve("project-a", "codex").unwrap().1,
        loopback_member(0, down)
    );
}

/// When every member that could run has a loopback provider that refuses connects, the
/// read-only pick gives the first one's line, the line the owner saw before failover.
#[cfg(target_os = "macos")]
#[test]
fn a_read_only_pick_with_every_member_down_carries_the_first_down_providers_line() {
    use crazyagent::research::accounts::Reach;
    use crazyagent::research::runner::ResearchHarness;
    let runtime =
        research_support::EngineRuntime::with_projects(&[("project-a", "Sells house plants.")]);
    let (_first, first) = crazyagent::spawn_gate::refusing_tcp_port().unwrap();
    let (_second, second) = crazyagent::spawn_gate::refusing_tcp_port().unwrap();
    let resolver = loopback_resolver(&runtime, &[first, second]);
    let unreached = AccountPick::Unreached {
        line: format!(
            "Can't reach the model provider at 127.0.0.1:{first}. Start it or pick another account in this pool, then try again."
        ),
    };
    let pool = CredentialRef::new("local-pool").unwrap();
    assert_eq!(
        resolver.pool_pick_with(
            "project-a",
            &pool,
            &[ResearchHarness::Codex],
            Reach::Reachable
        ),
        unreached
    );
    assert_eq!(
        resolver
            .resolve_with("project-a", "codex", Reach::Reachable)
            .unwrap()
            .1,
        unreached
    );
}
