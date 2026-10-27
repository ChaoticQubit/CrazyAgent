use super::*;
use crate::config::{AccountPool, ModelConnection, ModelReference};
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn connection(id: &str, url: &str, protocol: &str, auth: ModelAuthentication) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference(protocol),
        account_ref: reference("local-account"),
        endpoint: ModelEndpoint::Http { url: url.into() },
        model: ModelReference {
            id: "qwen2.5:0.5b".into(),
            author: reference("openai-compatible"),
        },
        auth,
    }
}

fn managed(id: &str, harness: &str, model: &str) -> ModelConnection {
    ModelConnection {
        id: reference(id),
        provider: reference(if harness == "codex" {
            "openai"
        } else {
            "anthropic"
        }),
        harness: reference(harness),
        protocol: reference("native"),
        account_ref: reference("local-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: model.into(),
            author: reference("openai-compatible"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference(&format!("{id}-profile")),
        },
    }
}

fn access(project: &str, members: &[&str]) -> ModelAccess {
    let none = || ModelAuthentication::None {};
    ModelAccess {
        accounts: vec![reference("local-account")],
        connections: vec![
            managed("codex-signed-in", "codex", "gpt-5.5"),
            managed("claude-signed-in", "claude", "native-default"),
            connection(
                "local-a",
                "http://127.0.0.1:11434/v1",
                "openai-chat",
                none(),
            ),
            connection(
                "local-b",
                "http://127.0.0.1:11435/v1",
                "openai-responses",
                none(),
            ),
            connection(
                "anthropic-local",
                "http://127.0.0.1:11434",
                "anthropic-messages",
                none(),
            ),
            connection(
                "keyed",
                "https://api.example.org/v1",
                "openai-chat",
                ModelAuthentication::ApiKey {
                    credential_ref: reference("key-reference"),
                },
            ),
        ],
        pools: vec![AccountPool {
            id: reference("team-pool"),
            name: None,
            members: members.iter().map(|m| reference(m)).collect(),
            projects: vec![reference(project)],
            roles: vec![reference("project_leader")],
            fallback_allowed: false,
            provider_quota_policy: Default::default(),
        }],
    }
}

fn stored(project: &str, sub: Vec<HarnessSlot>) -> StoredSetup {
    StoredSetup {
        project_id: ProjectId::new(project).unwrap(),
        name: "Team".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: sub,
            min_agents: 1,
            max_agents: 4,
        },
        documents: vec![],
        folder: "/private/tmp/unused".into(),
        created_ms: 1,
        revision: 1,
    }
}

/// Writes a stand-in harness `codex` in its own install folder beside the engine data, as a
/// real install sits outside the engine's folder whose secrets the sandbox walls off.
/// Takes the engine's base folder and the script. Returns the executable path.
fn harness(base: &Path, script: &[u8]) -> PathBuf {
    let install = base.join("harness");
    std::fs::create_dir_all(&install).unwrap();
    let executable = install.join("codex");
    std::fs::write(&executable, script).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    executable
}

fn database() -> (tempfile::TempDir, Connection) {
    let directory = tempfile::tempdir().unwrap();
    let connection = Connection::open(directory.path().join("engine.db")).unwrap();
    initialize(&connection).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    (directory, connection)
}

#[test]
fn delegation_lines_are_read_from_a_master_reply() {
    let reply = "I will split this.\nDELEGATE: Write apple.txt\n- delegate codex: List the files\n`DELEGATE claude-code: Review the notes`\nDELEGATE Bad Name: nope\nDELEGATE:   \nNot a DELEGATE: line";
    assert_eq!(
        parse_delegations(reply),
        [
            Delegation {
                harness: None,
                prompt: "Write apple.txt".into(),
                role: None,
            },
            Delegation {
                harness: Some("codex".into()),
                prompt: "List the files".into(),
                role: None,
            },
            Delegation {
                harness: Some("claude-code".into()),
                prompt: "Review the notes".into(),
                role: None,
            },
        ]
    );
    let many = (0..20)
        .map(|i| format!("DELEGATE: task {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(parse_delegations(&many).len(), DELEGATIONS_PER_REPLY_MAX);
    let long = format!("DELEGATE: {}", "x".repeat(TASK_PROMPT_BYTES_MAX + 1));
    assert!(parse_delegations(&long).is_empty());
}

#[test]
fn only_keyless_endpoint_accounts_that_fit_the_harness_are_eligible() {
    let project = ProjectId::new("team-accounts").unwrap();
    let all = access(
        "team-accounts",
        &["local-a", "keyed", "anthropic-local", "local-b", "local-a"],
    );
    let Accounts::Usable(codex) = eligible_accounts(&all, &project, "codex") else {
        panic!("pool assigned");
    };
    assert_eq!(
        codex
            .iter()
            .map(|a| a.connection.as_str())
            .collect::<Vec<_>>(),
        ["local-a", "local-b"]
    );
    let Accounts::Usable(claude) = eligible_accounts(&all, &project, "claude-code") else {
        panic!("pool assigned");
    };
    assert_eq!(
        claude
            .iter()
            .map(|a| a.connection.as_str())
            .collect::<Vec<_>>(),
        ["anthropic-local"]
    );
    assert_eq!(
        eligible_accounts(&all, &ProjectId::new("other").unwrap(), "codex"),
        Accounts::NoPool
    );
}

#[test]
fn managed_profiles_are_listed_only_for_their_own_harness() {
    let project = ProjectId::new("team-managed").unwrap();
    let all = access(
        "team-managed",
        &["claude-signed-in", "local-a", "codex-signed-in", "keyed"],
    );
    let Accounts::Usable(codex) = eligible_accounts(&all, &project, "codex") else {
        panic!("pool assigned");
    };
    assert_eq!(
        codex,
        [
            TeamAccount {
                connection: reference("local-a"),
                reach: TeamReach::Endpoint {
                    base_url: "http://127.0.0.1:11434/v1".into(),
                    model: "qwen2.5:0.5b".into(),
                },
            },
            TeamAccount {
                connection: reference("codex-signed-in"),
                reach: TeamReach::Managed {
                    profile_ref: reference("codex-signed-in-profile"),
                    model: Some("gpt-5.5".into()),
                },
            },
        ]
    );
    let Accounts::Usable(claude) = eligible_accounts(&all, &project, "claude-code") else {
        panic!("pool assigned");
    };
    assert_eq!(
        claude,
        [TeamAccount {
            connection: reference("claude-signed-in"),
            reach: TeamReach::Managed {
                profile_ref: reference("claude-signed-in-profile"),
                model: None,
            },
        }],
        "the claude alias maps to Claude Code and native-default leaves the model to the account"
    );
}

#[test]
fn work_spreads_across_pool_members_and_skips_paused_accounts() {
    let project = ProjectId::new("team-spread").unwrap();
    let Accounts::Usable(accounts) = eligible_accounts(
        &access("team-spread", &["local-a", "local-b"]),
        &project,
        "codex",
    ) else {
        panic!("pool assigned");
    };
    let mut paused = BTreeSet::new();
    let busy_a = |c: &CredentialRef| usize::from(c.as_str() == "local-a");
    assert_eq!(
        pick_account(
            &accounts,
            &|_| 0,
            &paused,
            &crate::scheduling::Choice::default()
        )
        .unwrap()
        .connection
        .as_str(),
        "local-a"
    );
    assert_eq!(
        pick_account(
            &accounts,
            &busy_a,
            &paused,
            &crate::scheduling::Choice::default()
        )
        .unwrap()
        .connection
        .as_str(),
        "local-b"
    );
    paused.insert(reference("local-b"));
    assert_eq!(
        pick_account(
            &accounts,
            &busy_a,
            &paused,
            &crate::scheduling::Choice::default()
        )
        .unwrap()
        .connection
        .as_str(),
        "local-a"
    );
    paused.insert(reference("local-a"));
    assert!(
        pick_account(
            &accounts,
            &|_| 0,
            &paused,
            &crate::scheduling::Choice::default()
        )
        .is_none()
    );
}

#[test]
fn admission_follows_the_team_plan_and_the_pool() {
    let (_directory, mut connection) = database();
    let setup = stored(
        "team-admit",
        vec![
            HarnessSlot {
                harness: "codex".into(),
                agents: 2,
                tools: true,
            },
            HarnessSlot {
                harness: "claude".into(),
                agents: 1,
                tools: false,
            },
        ],
    );
    let with_pool = access("team-admit", &["local-a"]);
    let tx = connection.transaction().unwrap();
    let codex = admit(
        &tx,
        &with_pool,
        &setup,
        Some("codex"),
        "Write a.txt",
        TaskSource::Master,
        Some(7),
        10,
    )
    .unwrap()
    .unwrap();
    let claude = admit(
        &tx,
        &with_pool,
        &setup,
        Some("claude-code"),
        "Review",
        TaskSource::Owner,
        None,
        11,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        admit(
            &tx,
            &with_pool,
            &setup,
            Some("gemini"),
            "Nope",
            TaskSource::Master,
            None,
            12
        )
        .unwrap(),
        Err("team_harness_not_in_plan")
    );
    assert_eq!(
        admit(
            &tx,
            &with_pool,
            &setup,
            None,
            "  ",
            TaskSource::Master,
            None,
            12
        )
        .unwrap(),
        Err("team_task_invalid")
    );
    let rows: Vec<(i64, String, bool, String, Option<i64>)> = tx
        .prepare("SELECT id,harness,tools,status,turn FROM team_tasks ORDER BY id")
        .unwrap()
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        [
            (codex, "codex".into(), true, "queued".into(), Some(7)),
            (claude, "claude".into(), false, "needs_pool".into(), None),
        ],
        "tools come from the plan; claude has no anthropic account in this pool"
    );
    let unassigned = stored("team-unassigned", setup.team.sub_harnesses.clone());
    let auto = admit(
        &tx,
        &with_pool,
        &unassigned,
        None,
        "Anything",
        TaskSource::Owner,
        None,
        13,
    )
    .unwrap()
    .unwrap();
    let status: String = tx
        .query_row("SELECT status FROM team_tasks WHERE id=?1", [auto], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "needs_pool");
    for index in 2..TASKS_ACTIVE_MAX {
        admit(
            &tx,
            &with_pool,
            &setup,
            None,
            &format!("task {index}"),
            TaskSource::Master,
            None,
            14,
        )
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        admit(
            &tx,
            &with_pool,
            &setup,
            None,
            "one too many",
            TaskSource::Master,
            None,
            15
        )
        .unwrap(),
        Err("team_queue_full")
    );
    let codex_only = stored(
        "team-codex-only",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: true,
        }],
    );
    assert_eq!(
        admit(
            &tx,
            &access("team-codex-only", &["local-a", "anthropic-local"]),
            &codex_only,
            Some("claude-code"),
            "Outside the plan",
            TaskSource::Master,
            None,
            16
        )
        .unwrap(),
        Err("team_harness_not_in_plan"),
        "a runnable harness with an account is still refused outside a chosen plan"
    );
    tx.commit().unwrap();
}

#[test]
fn an_empty_plan_hands_work_to_any_runnable_harness_with_an_account() {
    let (_directory, mut connection) = database();
    let mut any = stored("team-any", vec![]);
    any.team.master_tools = true;
    any.team.max_agents = crate::project_setup::TEAM_AGENTS_MAX;
    assert!(any.team.validate().is_ok());
    let both = access("team-any", &["local-a", "anthropic-local"]);
    let tx = connection.transaction().unwrap();
    let hand = |access: &ModelAccess, setup: &StoredSetup, harness: Option<&str>| {
        admit(
            &tx,
            access,
            setup,
            harness,
            "Do the work",
            TaskSource::Master,
            None,
            20,
        )
        .unwrap()
    };
    let named_codex = hand(&both, &any, Some("codex")).unwrap();
    let named_claude = hand(&both, &any, Some("claude")).unwrap();
    let tie = hand(&both, &any, None).unwrap();
    let lighter = hand(&both, &any, None).unwrap();
    assert_eq!(
        hand(&both, &any, Some("gemini-cli")),
        Err("team_harness_cannot_run")
    );
    assert_eq!(
        hand(&both, &any, Some("no-such-harness")),
        Err("team_harness_not_in_plan")
    );

    let mut codex_pool = any.clone();
    codex_pool.project_id = ProjectId::new("team-any-codex").unwrap();
    let codex_access = access("team-any-codex", &["local-a"]);
    assert_eq!(
        hand(&codex_access, &codex_pool, Some("claude-code")),
        Err("team_harness_cannot_run"),
        "Claude Code has no account in this pool while Codex has one"
    );
    let only_codex = hand(&codex_access, &codex_pool, None).unwrap();

    let mut parked = any.clone();
    parked.project_id = ProjectId::new("team-any-parked").unwrap();
    let no_pool = ModelAccess::default();
    let parked_default = hand(&no_pool, &parked, None).unwrap();
    let parked_named = hand(&no_pool, &parked, Some("claude-code")).unwrap();

    let rows: Vec<(i64, String, bool, String)> = tx
        .prepare("SELECT id,harness,tools,status FROM team_tasks ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let queued = |id: i64, harness: &str| (id, harness.to_owned(), true, "queued".to_owned());
    let waiting = |id: i64, harness: &str| (id, harness.to_owned(), true, "needs_pool".to_owned());
    assert_eq!(
        rows,
        [
            queued(named_codex, "codex"),
            queued(named_claude, "claude-code"),
            queued(tie, "codex"),
            queued(lighter, "claude-code"),
            queued(only_codex, "codex"),
            waiting(parked_default, "codex"),
            waiting(parked_named, "claude-code"),
        ],
        "tools follow the master; a tie goes to the master's harness, then the least loaded wins"
    );
    tx.commit().unwrap();

    let team = status(&connection, &any).unwrap();
    assert_eq!(
        team.harnesses
            .iter()
            .map(|h| (
                h.harness.as_str(),
                h.role,
                h.agents_max,
                h.tools,
                h.runnable
            ))
            .collect::<Vec<_>>(),
        [
            ("codex", AgentRole::Master, 1, true, true),
            ("codex", AgentRole::Sub, 63, true, true),
            ("claude-code", AgentRole::Sub, 63, true, true),
        ],
        "an empty plan lists every runnable harness under the team maximum"
    );

    let mut solo = any.clone();
    solo.project_id = ProjectId::new("team-any-solo").unwrap();
    solo.team.max_agents = 1;
    let solo_access = access("team-any-solo", &["local-a"]);
    let tx = connection.transaction().unwrap();
    for harness in [None, Some("codex")] {
        assert_eq!(
            admit(
                &tx,
                &solo_access,
                &solo,
                harness,
                "Alone",
                TaskSource::Owner,
                None,
                30
            )
            .unwrap(),
            Err("team_has_no_sub_agents"),
            "a team maximum of one agent leaves room for the master only"
        );
    }
    tx.commit().unwrap();
    assert_eq!(
        status(&connection, &solo)
            .unwrap()
            .harnesses
            .iter()
            .map(|h| h.role)
            .collect::<Vec<_>>(),
        [AgentRole::Master]
    );
}

#[test]
fn an_empty_plan_runs_more_than_one_sub_agent_at_once() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored("team-any-run", vec![]);
    setup.team.max_agents = crate::project_setup::TEAM_AGENTS_MAX;
    setup.folder = folder.to_str().unwrap().into();
    let row = crate::protocol::StoredProject {
        project_id: setup.project_id.clone(),
        spec: crate::protocol::ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&row).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("team-any-run", &["local-a"]);
    let tx = connection.transaction().unwrap();
    for prompt in ["Write a.md", "Write b.md"] {
        assert_eq!(
            execute(
                &tx,
                &pool,
                &TeamAction::Delegate {
                    project_id: setup.project_id.clone(),
                    harness: None,
                    prompt: prompt.into(),
                }
            )
            .unwrap(),
            Outcome::Applied
        );
    }
    tx.commit().unwrap();
    let executable = harness(&base, b"#!/bin/sh\nexec /bin/sleep 30\n");
    let source = pool.clone();
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(source.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let subs = |team: &TeamStatus| {
        team.agents
            .iter()
            .filter(|a| a.role == AgentRole::Sub && a.status.live())
            .count()
    };
    loop {
        team.tick().unwrap();
        let now = status(&connection, &setup).unwrap();
        if subs(&now) >= 2 {
            assert!(
                now.tasks.iter().all(|t| t.status == TaskStatus::Running),
                "{now:#?}"
            );
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{now:#?}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    team.shutdown().unwrap();
}

#[test]
fn owner_actions_queue_messages_cancel_tasks_and_keep_agents_warm() {
    let (_directory, mut connection) = database();
    let setup = stored(
        "team-owner",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("team-owner", &["local-a"]);
    let project = setup.project_id.clone();
    let tx = connection.transaction().unwrap();
    for action in [
        TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the login page".into(),
        },
        TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write login.md".into(),
        },
        TeamAction::Delegate {
            project_id: project.clone(),
            harness: Some("codex".into()),
            prompt: "Write signup.md".into(),
        },
        TeamAction::KeepWarm {
            project_id: project.clone(),
            on: true,
        },
    ] {
        assert_eq!(
            execute(&tx, &pool, &action).unwrap(),
            Outcome::Applied,
            "{action:?}"
        );
    }
    tx.execute("UPDATE team_tasks SET status='running' WHERE id=2", [])
        .unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::Cancel {
                project_id: project.clone(),
                task: None
            }
        )
        .unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::Cancel {
                project_id: project.clone(),
                task: Some(99)
            }
        )
        .unwrap(),
        Outcome::Rejected {
            code: "team_task_missing".into()
        }
    );
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::Message {
                project_id: ProjectId::new("no-setup").unwrap(),
                text: "hi".into()
            }
        )
        .unwrap(),
        Outcome::Rejected {
            code: "project_setup_missing".into()
        }
    );
    assert!(
        TeamAction::Message {
            project_id: project.clone(),
            text: "x".repeat(MESSAGE_BYTES_MAX + 1)
        }
        .validate()
        .is_err()
    );
    tx.commit().unwrap();
    let team = status(&connection, &setup).unwrap();
    assert!(team.keep_warm);
    assert_eq!(
        team.tasks
            .iter()
            .map(|t| (t.prompt.as_str(), t.status, t.source))
            .collect::<Vec<_>>(),
        [
            ("Write login.md", TaskStatus::Cancelled, TaskSource::Owner),
            ("Write signup.md", TaskStatus::Running, TaskSource::Owner),
        ]
    );
    let requested: bool = connection
        .query_row(
            "SELECT cancel_requested FROM team_tasks WHERE id=2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(requested, "a running task is stopped by the runtime");
    assert_eq!(
        team.messages
            .iter()
            .map(|m| (m.author, m.state, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(
            MessageAuthor::Owner,
            MessageState::Pending,
            "Plan the login page"
        )]
    );
    assert_eq!(team.harnesses[0].delegation, Some(DelegationPath::Reply));
    assert_eq!(team.harnesses[1].agents_max, 1);
}

#[test]
fn the_team_status_tells_the_live_master_switch_and_why_it_could_not_start() {
    let (_directory, connection) = database();
    let setup = stored("live-switch", Vec::new());
    let switch = |connection: &Connection| {
        let team = status(connection, &setup).unwrap();
        (team.live_master, team.live_problem)
    };
    assert_eq!(switch(&connection), (None, None), "never set");
    master::set_live(&connection, &setup.project_id, true).unwrap();
    assert_eq!(switch(&connection), (Some(true), None));
    let line = "The live master could not start: no signed-in account.";
    assert!(master::problem(&connection, &setup.project_id, Some(line)).unwrap());
    master::set_live(&connection, &setup.project_id, false).unwrap();
    assert_eq!(switch(&connection), (Some(false), Some(line.to_owned())));
    master::problem(&connection, &setup.project_id, None).unwrap();
    assert_eq!(switch(&connection), (Some(false), None));
}

#[test]
fn the_owner_renames_a_sub_agent_and_the_team_status_names_every_agent() {
    let (_directory, mut connection) = database();
    let setup = stored(
        "team-names",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 2,
            tools: false,
        }],
    );
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let project = setup.project_id.clone();
    let pool = access("team-names", &["local-a"]);
    let agent = |role: &str| {
        connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES ('team-names',?1,'codex',0,'working',1,1)",
                [role],
            )
            .unwrap();
        connection.last_insert_rowid()
    };
    let master = agent("master");
    let first = agent("sub");
    let second = agent("sub");
    let unnamed = agent("sub");
    for sub in [first, second] {
        names::assign_persona(&connection, &project, sub).unwrap();
    }
    names::assign_role(&connection, &project, first, "onboarding-fix")
        .unwrap()
        .unwrap();
    let rename = |agent: i64, handle: &str| TeamAction::Rename {
        project_id: project.clone(),
        agent,
        handle: handle.into(),
    };
    let tx = connection.transaction().unwrap();
    assert_eq!(
        execute(&tx, &pool, &rename(second, "@docs-pass")).unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        execute(&tx, &pool, &rename(second, "onboarding-fix")).unwrap(),
        rejected("team_handle_taken")
    );
    assert_eq!(
        execute(&tx, &pool, &rename(second, "Docs Pass")).unwrap(),
        rejected("team_handle_invalid")
    );
    assert_eq!(
        execute(&tx, &pool, &rename(master, "lead")).unwrap(),
        rejected("team_agent_unknown")
    );
    for invalid in [rename(second, &"a".repeat(34)), rename(0, "docs")] {
        assert_eq!(
            execute(&tx, &pool, &invalid).unwrap_err().0,
            "team_handle_invalid"
        );
    }
    tx.commit().unwrap();
    let team = status(&connection, &setup).unwrap();
    let shown: Vec<(i64, &str, Option<&str>, Option<&str>)> = team
        .agents
        .iter()
        .map(|a| {
            (
                a.id,
                a.label.as_str(),
                a.persona.as_deref(),
                a.handle.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (master, "Master", None, None),
            (
                first,
                &*format!("{} (@onboarding-fix)", names::PERSONAS[0]),
                Some(names::PERSONAS[0]),
                Some("onboarding-fix")
            ),
            (
                second,
                &*format!("{} (@docs-pass)", names::PERSONAS[1]),
                Some(names::PERSONAS[1]),
                Some("docs-pass")
            ),
            (unnamed, &*format!("Sub-agent {unnamed}"), None, None),
        ]
    );
    assert_eq!(
        graph(&team)
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Agent)
            .map(|node| node.label.clone())
            .collect::<Vec<_>>(),
        [
            format!("{} (@onboarding-fix)", names::PERSONAS[0]),
            format!("{} (@docs-pass)", names::PERSONAS[1]),
            format!("Sub-agent {unnamed}"),
        ]
    );
    assert_eq!(
        names::resolve(&connection, &project, "@docs-pass").unwrap(),
        Some(second)
    );
}

#[test]
fn the_owner_hands_off_plan_steps_the_master_proposed() {
    let (_directory, mut connection) = database();
    let setup = stored(
        "team-plan",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 2,
            tools: false,
        }],
    );
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let project = setup.project_id.clone();
    let pool = access("team-plan", &["local-a"]);
    let tx = connection.transaction().unwrap();
    let reply = append_message(
        &tx,
        &project,
        MessageAuthor::Master,
        "Two steps:\nDELEGATE: Write apple.txt\nDELEGATE gemini: Write pear.txt",
        MessageState::Reply,
        10,
    )
    .unwrap()
    .unwrap();
    let handed = admit(
        &tx,
        &pool,
        &setup,
        None,
        "Write apple.txt",
        TaskSource::Master,
        Some(reply),
        11,
    )
    .unwrap()
    .unwrap();
    tx.commit().unwrap();

    let team = status(&connection, &setup).unwrap();
    assert_eq!(
        team.plan,
        [
            PlanStep {
                reply,
                step: 0,
                harness: None,
                prompt: "Write apple.txt".into(),
                task: Some(handed),
            },
            PlanStep {
                reply,
                step: 1,
                harness: Some("gemini".into()),
                prompt: "Write pear.txt".into(),
                task: None,
            },
        ]
    );

    let tx = connection.transaction().unwrap();
    let hand = |step| TeamAction::HandOff {
        project_id: project.clone(),
        reply,
        step,
    };
    for (step, code) in [
        (0, "team_plan_step_handed"),
        (1, "team_harness_not_in_plan"),
        (2, "team_plan_step_missing"),
    ] {
        assert_eq!(
            execute(&tx, &pool, &hand(step)).unwrap(),
            Outcome::Rejected { code: code.into() },
            "step {step}"
        );
    }
    let owner_message = append_message(
        &tx,
        &project,
        MessageAuthor::Owner,
        "DELEGATE: Not a master step",
        MessageState::Pending,
        12,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::HandOff {
                project_id: project.clone(),
                reply: owner_message,
                step: 0,
            }
        )
        .unwrap(),
        Outcome::Rejected {
            code: "team_plan_step_missing".into()
        },
        "only a master reply holds plan steps"
    );
    let second = append_message(
        &tx,
        &project,
        MessageAuthor::Master,
        "DELEGATE: Write plum.txt",
        MessageState::Reply,
        13,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::HandOff {
                project_id: project.clone(),
                reply: second,
                step: 0,
            }
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    let team = status(&connection, &setup).unwrap();
    let plum = team.tasks.last().unwrap();
    assert_eq!(
        (plum.prompt.as_str(), plum.source, plum.status),
        ("Write plum.txt", TaskSource::Master, TaskStatus::Queued),
        "a handed-off step is the master's task, so its result goes back to the master"
    );
    assert_eq!(team.plan.len(), 1, "the plan is the latest master reply");
    assert_eq!(team.plan[0].task, Some(plum.id));
}

#[test]
fn reopening_the_runtime_returns_interrupted_work_to_the_queue() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let connection = Connection::open(&database).unwrap();
    initialize(&connection).unwrap();
    connection
        .execute_batch(
            "INSERT INTO team_agents(project,role,harness,tools,status,task,created_ms,heartbeat_ms) VALUES ('team-restart','sub','codex',0,'working',1,1,1);
             INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms) VALUES
               ('team-restart','master','codex',0,'resume me','running',1,1),
               ('team-restart','owner','codex',0,'stop me','running',1,1);
             UPDATE team_tasks SET cancel_requested=1 WHERE prompt='stop me';
             INSERT INTO team_messages(project,author,text,state,created_ms) VALUES ('team-restart','owner','hello','sent',1);",
        )
        .unwrap();
    let open = |limits| {
        runtime::TeamRuntime::open(
            &database,
            &base.join("team"),
            std::collections::BTreeMap::new(),
            Box::new(|| None),
            limits,
        )
    };
    let broken = runtime::TeamLimits {
        attempts_max: 0,
        ..runtime::TeamLimits::default()
    };
    assert_eq!(open(broken).err().unwrap().0, "team_limits_invalid");
    let reopened = open(runtime::TeamLimits::default()).unwrap();
    assert!(!reopened.has_live_agents());
    let tasks: Vec<(String, String, Option<i64>)> = connection
        .prepare("SELECT prompt,status,agent FROM team_tasks ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tasks,
        [
            ("resume me".into(), "queued".into(), None),
            ("stop me".into(), "cancelled".into(), None)
        ]
    );
    let agent: (String, Option<i64>, Option<String>) = connection
        .query_row("SELECT status,task,error FROM team_agents", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(
        agent,
        (
            "stopped".into(),
            None,
            Some("Stopped when CrazyAgent closed".into())
        )
    );
    let message: String = connection
        .query_row("SELECT state FROM team_messages", [], |r| r.get(0))
        .unwrap();
    assert_eq!(message, "pending");
    let mode = std::fs::metadata(base.join("team").join("agents"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn team_plan_harness_names_map_to_launch_adapters_and_delegation_paths() {
    assert_eq!(launch_harness("codex"), Some("codex"));
    assert_eq!(launch_harness("claude"), Some("claude-code"));
    assert_eq!(launch_harness("claude-code"), Some("claude-code"));
    assert_eq!(launch_harness("gemini"), None);
    assert_eq!(launch_harness("no-such-harness"), None);
    assert_eq!(delegation_path("claude", false), DelegationPath::Reply);
    assert_eq!(delegation_path("codex", true), DelegationPath::Tool);
    assert_eq!(delegation_path("gemini", true), DelegationPath::Unavailable);
}

#[test]
fn the_runtime_parks_work_without_a_pool_and_never_starts_an_uninstalled_harness() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    initialize(&connection).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "team-parked",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    setup.folder = folder.to_str().unwrap().into();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let project = setup.project_id.clone();
    let tx = connection.transaction().unwrap();
    for action in [
        TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the page".into(),
        },
        TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write page.md".into(),
        },
    ] {
        assert_eq!(
            execute(&tx, &ModelAccess::default(), &action).unwrap(),
            Outcome::Applied
        );
    }
    tx.commit().unwrap();
    let live = std::sync::Arc::new(std::sync::Mutex::new(None::<ModelAccess>));
    let source = live.clone();
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::new(),
        Box::new(move || source.lock().unwrap().clone()),
        runtime::TeamLimits::default(),
    )
    .unwrap();

    team.tick().unwrap();
    let parked = status(&connection, &setup).unwrap();
    assert_eq!(
        parked
            .messages
            .iter()
            .map(|m| (m.author, m.state, m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (MessageAuthor::Owner, MessageState::Failed, "Plan the page"),
            (
                MessageAuthor::Engine,
                MessageState::Note,
                "The master could not start: Needs a pool."
            ),
        ]
    );
    assert_eq!(parked.tasks[0].status, TaskStatus::NeedsPool);
    assert!(parked.agents.is_empty());

    *live.lock().unwrap() = Some(access("team-parked", &["local-a"]));
    team.tick().unwrap();
    let waiting = status(&connection, &setup).unwrap();
    assert_eq!(
        waiting.tasks[0].status,
        TaskStatus::Queued,
        "a usable pool member releases the task"
    );
    assert!(
        waiting.agents.is_empty(),
        "codex has no registered executable, so nothing starts"
    );
    assert!(!team.has_live_agents());
    team.shutdown().unwrap();
}

/// The owner moved the project folder away before the master started. The agent and the
/// owner's note name the plain code of the failed file step, never the system's error text
/// or a path.
#[test]
fn a_master_whose_project_folder_is_gone_names_the_plain_file_code() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    initialize(&connection).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    let mut setup = stored("team-moved", vec![]);
    setup.folder = base.join("moved-away").to_str().unwrap().into();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("team-moved", &["local-a"]);
    let tx = connection.transaction().unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::Message {
                project_id: setup.project_id.clone(),
                text: "Plan the page".into(),
            },
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    let executable = harness(&base, b"#!/bin/sh\nexec /bin/sleep 30\n");
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(pool.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap();

    team.tick().unwrap();
    let failed = status(&connection, &setup).unwrap();
    let line = "the project folder could not be opened. (io_not_found)";
    assert_eq!(failed.agents.len(), 1, "{failed:#?}");
    assert_eq!(failed.agents[0].status, AgentStatus::Failed);
    assert_eq!(failed.agents[0].problem.as_deref(), Some(line));
    assert_eq!(
        failed
            .messages
            .iter()
            .map(|m| (m.author, m.state, m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (MessageAuthor::Owner, MessageState::Failed, "Plan the page"),
            (
                MessageAuthor::Engine,
                MessageState::Note,
                format!("The master could not start: {line}").as_str()
            ),
        ]
    );
    let shown = format!("{failed:?}");
    assert!(
        !shown.contains("os error") && !shown.contains("moved-away"),
        "{shown}"
    );
    assert!(!team.has_live_agents());
    team.shutdown().unwrap();
}

#[test]
fn a_paused_team_holds_its_work_until_the_owner_resumes_it() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    initialize(&connection).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "team-paused",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    setup.folder = folder.to_str().unwrap().into();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let project = setup.project_id.clone();
    let pool = access("team-paused", &["local-a"]);
    let tx = connection.transaction().unwrap();
    for action in [
        TeamAction::Pause {
            project_id: project.clone(),
        },
        TeamAction::Pause {
            project_id: project.clone(),
        },
        TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the page".into(),
        },
        TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write page.md".into(),
        },
        TeamAction::KeepWarm {
            project_id: project.clone(),
            on: true,
        },
    ] {
        assert_eq!(
            execute(&tx, &pool, &action).unwrap(),
            Outcome::Applied,
            "{action:?}"
        );
    }
    tx.commit().unwrap();
    assert!(status(&connection, &setup).unwrap().paused);
    let source = pool.clone();
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::new(),
        Box::new(move || Some(source.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap();

    team.tick().unwrap();
    team.tick().unwrap();
    let held = status(&connection, &setup).unwrap();
    assert!(held.paused);
    assert_eq!(
        held.messages
            .iter()
            .map(|m| (m.author, m.state))
            .collect::<Vec<_>>(),
        [(MessageAuthor::Owner, MessageState::Pending)],
        "a paused team keeps the owner's message for later"
    );
    assert_eq!(held.tasks[0].status, TaskStatus::Queued);
    assert_eq!(held.tasks[0].attempts, 0);
    assert!(held.agents.is_empty());
    assert!(!team.has_live_agents());

    let tx = connection.transaction().unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::Resume {
                project_id: project.clone(),
            }
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    team.tick().unwrap();
    let resumed = status(&connection, &setup).unwrap();
    assert!(!resumed.paused);
    assert_eq!(
        resumed
            .messages
            .iter()
            .map(|m| (m.author, m.state, m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (MessageAuthor::Owner, MessageState::Failed, "Plan the page"),
            (
                MessageAuthor::Engine,
                MessageState::Note,
                "The master could not start: codex is not installed."
            ),
        ],
        "after resume the engine tries to start the master again"
    );
    team.shutdown().unwrap();
}

#[test]
fn keeping_agents_warm_stops_when_a_warm_agent_dies_before_any_work() {
    warm_agents_ended(b"#!/bin/sh\nexit 1\n");
}

#[test]
fn a_sub_agent_whose_harness_breaks_its_protocol_names_the_launch_code() {
    let cold = warm_agents_ended(b"#!/bin/sh\nread line\necho not-json\nexec /bin/sleep 30\n");
    for agent in &cold.agents {
        assert_eq!(
            agent.problem.as_deref(),
            Some("The agent process closed. (launch_protocol_failed)"),
            "{cold:#?}"
        );
    }
    assert!(
        cold.messages
            .iter()
            .any(|m| m.text.contains(". (launch_protocol_failed)")),
        "{cold:#?}"
    );
}

/// Runs a project that keeps its two codex sub-agents warm on a stand-in harness until a
/// warm agent that ended before any work turns keeping warm off.
/// Input: the stand-in harness script. Output: the team status once both agents ended.
fn warm_agents_ended(script: &[u8]) -> TeamStatus {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "team-warm",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 2,
            tools: false,
        }],
    );
    setup.team.min_agents = 3;
    setup.folder = folder.to_str().unwrap().into();
    let row = crate::protocol::StoredProject {
        project_id: setup.project_id.clone(),
        spec: crate::protocol::ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&row).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("team-warm", &["local-a"]);
    let tx = connection.transaction().unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::KeepWarm {
                project_id: setup.project_id.clone(),
                on: true,
            }
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    let executable = harness(&base, script);
    let source = pool.clone();
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(source.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        team.tick().unwrap();
        if !status(&connection, &setup).unwrap().keep_warm {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{:#?}",
            status(&connection, &setup).unwrap()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for _ in 0..10 {
        team.tick().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let cold = status(&connection, &setup).unwrap();
    assert!(!cold.keep_warm);
    assert_eq!(
        cold.agents.len(),
        2,
        "the plan's two warm sub-agents start once and are not started again: {cold:#?}"
    );
    assert!(cold.agents.iter().all(|a| a.status == AgentStatus::Failed));
    assert_eq!(
        cold.agents
            .iter()
            .map(|a| a.label.as_str())
            .collect::<Vec<_>>(),
        [names::PERSONAS[0], names::PERSONAS[1]],
        "each sub-agent gets its own persona when it starts: {cold:#?}"
    );
    assert!(
        cold.messages.iter().any(|m| m.state == MessageState::Note
            && m.text.starts_with("Keeping sub-agents warm was turned off")),
        "{cold:#?}"
    );
    assert!(!team.has_live_agents());
    team.shutdown().unwrap();
    cold
}

#[test]
fn a_master_with_tools_on_starts_with_the_engine_delegate_tool() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "team-tool",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    setup.team.master_tools = true;
    setup.folder = folder.to_str().unwrap().into();
    let row = crate::protocol::StoredProject {
        project_id: setup.project_id.clone(),
        spec: crate::protocol::ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&row).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("team-tool", &["local-a"]);
    let tx = connection.transaction().unwrap();
    assert_eq!(
        execute(
            &tx,
            &pool,
            &TeamAction::Message {
                project_id: setup.project_id.clone(),
                text: "Split the work.".into(),
            }
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    let executable = harness(
        &base,
        b"#!/bin/sh\n/bin/cat \"$CODEX_HOME/config.toml\" > seen-config.toml\nexit 1\n",
    );
    let engine_folder = base.join("engine-bin");
    std::fs::create_dir(&engine_folder).unwrap();
    let engine = engine_folder.join("crazyagent");
    std::fs::write(&engine, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    let source = pool.clone();
    assert_eq!(
        runtime::TeamRuntime::open(
            &database,
            &base.join("team"),
            std::collections::BTreeMap::new(),
            Box::new(|| None),
            runtime::TeamLimits::default(),
        )
        .unwrap()
        .with_delegation_tool(&base.join("absent"))
        .err()
        .map(|e| e.0),
        Some("team_executable_invalid".into())
    );
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(source.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap()
    .with_delegation_tool(&engine)
    .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        team.tick().unwrap();
        let now = status(&connection, &setup).unwrap();
        if now.messages[0].state == MessageState::Failed {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{now:#?}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let team_now = status(&connection, &setup).unwrap();
    assert_eq!(team_now.harnesses[0].delegation, Some(DelegationPath::Tool));
    let master = team_now
        .agents
        .iter()
        .find(|a| a.role == AgentRole::Master)
        .unwrap();
    assert_eq!(
        master.worktree,
        Some(format!("worktrees/agent-{}", master.id)),
        "the master works in its own git worktree like every agent"
    );
    let seen = folder
        .join(master.worktree.as_deref().unwrap())
        .join("seen-config.toml");
    let config: toml::Table = std::fs::read_to_string(&seen)
        .unwrap_or_else(|e| panic!("the probe harness saw no config ({e}): {team_now:#?}"))
        .parse()
        .unwrap();
    let server = config["mcp_servers"][tool::SERVER_NAME].as_table().unwrap();
    assert_eq!(server["command"].as_str(), engine.to_str());
    let record = base
        .join("team")
        .join("agents")
        .join(format!("agent-{}", master.id))
        .join("tmp")
        .join(tool::DROP_FILE);
    assert_eq!(
        server["args"],
        toml::Value::Array(vec![
            tool::SERVER_COMMAND.into(),
            record.to_str().unwrap().into()
        ]),
        "the master records its calls in its own private temporary folder"
    );
    team.shutdown().unwrap();
}

#[test]
fn a_managed_pool_member_without_a_sign_in_never_starts_an_agent() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    initialize(&connection).unwrap();
    crate::project_setup::initialize(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "team-unsigned",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    setup.folder = folder.to_str().unwrap().into();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let project = setup.project_id.clone();
    let tx = connection.transaction().unwrap();
    for action in [
        TeamAction::Message {
            project_id: project.clone(),
            text: "Plan the page".into(),
        },
        TeamAction::Delegate {
            project_id: project.clone(),
            harness: None,
            prompt: "Write page.md".into(),
        },
    ] {
        assert_eq!(
            execute(&tx, &ModelAccess::default(), &action).unwrap(),
            Outcome::Applied
        );
    }
    tx.commit().unwrap();
    let executable = harness(&base, b"#!/bin/sh\nexit 1\n");
    let pool = access("team-unsigned", &["codex-signed-in"]);
    let mut team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(pool.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap();

    team.tick().unwrap();
    let parked = status(&connection, &setup).unwrap();
    assert_eq!(
        parked
            .messages
            .iter()
            .map(|m| (m.author, m.state, m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (MessageAuthor::Owner, MessageState::Failed, "Plan the page"),
            (
                MessageAuthor::Engine,
                MessageState::Note,
                "The master could not start: Sign in to an account in this project's pool to run codex."
            ),
        ]
    );
    assert_eq!(
        parked.tasks[0].status,
        TaskStatus::NeedsPool,
        "a pool whose only member is not signed in holds the task"
    );
    assert!(parked.agents.is_empty());
    assert!(!team.has_live_agents());
    team.shutdown().unwrap();
}

#[test]
fn a_fresh_master_reads_the_recent_conversation_within_bounds() {
    let (_directory, mut connection) = database();
    let project = ProjectId::new("team-history").unwrap();
    let tx = connection.transaction().unwrap();
    for (author, state, text) in [
        (
            MessageAuthor::Owner,
            MessageState::Answered,
            "Build the menu page",
        ),
        (
            MessageAuthor::Master,
            MessageState::Reply,
            "DELEGATE: Write menu.html",
        ),
        (
            MessageAuthor::Engine,
            MessageState::Note,
            "These tasks were not handed off",
        ),
        (
            MessageAuthor::Engine,
            MessageState::Pending,
            "Results from your sub-agents",
        ),
    ] {
        append_message(&tx, &project, author, text, state, 1)
            .unwrap()
            .unwrap();
    }
    let other = ProjectId::new("team-other").unwrap();
    append_message(
        &tx,
        &other,
        MessageAuthor::Owner,
        "secret plan",
        MessageState::Answered,
        1,
    )
    .unwrap()
    .unwrap();
    tx.commit().unwrap();
    let history = runtime::conversation_before(&connection, &project, 4, 4000).unwrap();
    assert_eq!(
        history,
        "Owner: Build the menu page\nMaster: DELEGATE: Write menu.html\n"
    );
    assert_eq!(
        runtime::conversation_before(&connection, &project, 1, 4000).unwrap(),
        ""
    );
    assert_eq!(
        runtime::conversation_before(&connection, &project, 4, 40).unwrap(),
        "Master: DELEGATE: Write menu.html\n",
        "the newest messages are kept when the bound is tight"
    );
}

#[test]
fn concurrent_agent_setups_create_one_project_repository() {
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let root = base.join("project");
    std::fs::create_dir(&root).unwrap();
    let starts = std::sync::Arc::new(std::sync::Barrier::new(8));
    let setups: Vec<_> = (0..8)
        .map(|index| {
            let (root, starts) = (root.clone(), starts.clone());
            let tmp = base.join(format!("tmp-{index}"));
            std::fs::create_dir(&tmp).unwrap();
            std::thread::spawn(move || {
                starts.wait();
                crate::research::runner::ensure_repository(&root, &tmp).map_err(|e| e.to_string())
            })
        })
        .collect();
    for setup in setups {
        setup.join().unwrap().unwrap();
    }
    let commits = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .args(["-C", root.to_str().unwrap(), "rev-list", "--count", "HEAD"])
            .env_clear()
            .env("HOME", &base)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(commits.status.success());
    assert_eq!(String::from_utf8_lossy(&commits.stdout).trim(), "1");
    let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches("/worktrees/").count(), 1, "{exclude}");
}

#[test]
fn every_team_agent_is_in_the_agent_register_with_the_owners_stop_and_restart() {
    use crate::agents::AgentState;
    use crate::agents::tests::{alive, until};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "team-register",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    setup.team.min_agents = 2;
    setup.folder = folder.to_str().unwrap().into();
    let row = crate::protocol::StoredProject {
        project_id: setup.project_id.clone(),
        spec: crate::protocol::ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&row).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("team-register", &["local-a"]);
    let warm = |connection: &mut Connection, on: bool| {
        let tx = connection.transaction().unwrap();
        assert_eq!(
            execute(
                &tx,
                &pool,
                &TeamAction::KeepWarm {
                    project_id: setup.project_id.clone(),
                    on,
                }
            )
            .unwrap(),
            Outcome::Applied
        );
        tx.commit().unwrap();
    };
    warm(&mut connection, true);
    let executable = harness(&base, b"#!/bin/sh\nexec /bin/sleep 300\n");
    let agents = std::sync::Arc::new(crate::agents::Agents::open(&base).unwrap());
    let source = pool.clone();
    let team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(source.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap()
    .with_agents(agents.clone());
    let service = runtime::TeamService::start(team, std::time::Duration::from_millis(50));
    let project = setup.project_id.clone();
    let groups = |connection: &Connection| {
        status(connection, &setup)
            .unwrap()
            .agents
            .iter()
            .filter(|a| a.status != AgentStatus::Stopped)
            .filter_map(|a| a.process_group)
            .count()
    };

    until("the warm sub-agent to start", || groups(&connection) == 1);
    let first = status(&connection, &setup).unwrap();
    assert_eq!(first.agents.len(), 1, "{first:#?}");
    let listed = agents
        .list(Some(&project), 0, crate::agents::LIST_MAX)
        .unwrap();
    assert_eq!(listed.len(), 1, "{listed:#?}");
    let id = listed[0].id.clone();
    assert_eq!(
        listed[0].label,
        format!("Team sub-agent {}", first.agents[0].id)
    );
    assert_eq!(listed[0].role, crate::agents::AgentRole::SubAgent);
    assert_eq!(listed[0].state, AgentState::Running);
    assert_eq!(
        (listed[0].harness.as_str(), listed[0].attempt),
        ("codex", 1)
    );
    assert!(listed[0].can_stop && listed[0].can_restart && !listed[0].can_interrupt);
    assert_eq!(agents.interrupt(&id).unwrap_err().0, "agent_not_busy");
    assert_eq!(agents.send(&id, "hello").unwrap_err().0, "agent_hosted");
    let group = first.agents[0].process_group.unwrap() as i32;
    assert!(alive(group));

    agents.restart(&id).unwrap();
    until("the restarted sub-agent", || {
        let view = agents.view(&id).unwrap();
        view.state == AgentState::Running && view.attempt == 2 && groups(&connection) == 1
    });
    until("the first process group to be gone", || !alive(group));
    let second = status(&connection, &setup).unwrap();
    assert_eq!(
        second.agents.len(),
        2,
        "one fresh team agent, no extra warm one: {second:#?}"
    );
    assert_eq!(second.agents[0].status, AgentStatus::Stopped);
    assert_eq!(
        agents
            .list(Some(&project), 0, crate::agents::LIST_MAX)
            .unwrap()
            .len(),
        1,
        "the restart continues the same register entry"
    );

    warm(&mut connection, false);
    let stopped = agents.stop(&id).unwrap();
    assert_eq!(stopped.state, AgentState::Stopped);
    assert!(!stopped.can_stop && stopped.can_restart);
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after = status(&connection, &setup).unwrap();
    assert_eq!(
        after.agents.len(),
        2,
        "a stopped agent is not replaced: {after:#?}"
    );
    assert!(
        after
            .agents
            .iter()
            .all(|a| a.status == AgentStatus::Stopped)
    );

    agents.restart(&id).unwrap();
    until("the agent restarted after its stop", || {
        let view = agents.view(&id).unwrap();
        view.state == AgentState::Running && view.attempt == 3 && groups(&connection) == 1
    });
    let third = status(&connection, &setup).unwrap();
    assert_eq!(third.agents.len(), 3, "{third:#?}");
    let group = third.agents[2].process_group.unwrap() as i32;

    service.stop();
    let closed = agents.view(&id).unwrap();
    assert_eq!(closed.state, AgentState::Closed);
    assert_eq!(closed.status, "Stopped when CrazyAgent closed");
    until("the last process group to be gone", || !alive(group));
}

fn project_rows(connection: &Connection, table: &str, project: &str) -> Vec<i64> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT id FROM {table} WHERE project=?1 ORDER BY id"
        ))
        .unwrap();
    statement
        .query_map([project], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn the_conversation_keeps_its_newest_rows_and_bounds_only_waiting_messages() {
    let (_directory, mut connection) = database();
    let project = ProjectId::new("team-window").unwrap();
    let other = ProjectId::new("team-other").unwrap();
    let tx = connection.transaction().unwrap();
    let neighbour = append_message(
        &tx,
        &other,
        MessageAuthor::Master,
        "kept",
        MessageState::Reply,
        0,
    )
    .unwrap()
    .unwrap();
    let waiting: Vec<i64> = (0..MESSAGES_WAITING_MAX)
        .map(|index| {
            append_message(
                &tx,
                &project,
                MessageAuthor::Owner,
                "wait",
                MessageState::Pending,
                index,
            )
            .unwrap()
            .unwrap()
        })
        .collect();
    assert_eq!(
        append_message(
            &tx,
            &project,
            MessageAuthor::Owner,
            "one more",
            MessageState::Pending,
            0
        )
        .unwrap(),
        Err("team_conversation_full")
    );
    let history: Vec<i64> = (0..MESSAGES_PER_PROJECT_MAX)
        .map(|index| {
            append_message(
                &tx,
                &project,
                MessageAuthor::Master,
                "reply",
                MessageState::Reply,
                index,
            )
            .unwrap()
            .unwrap()
        })
        .collect();
    let note = append_message(
        &tx,
        &project,
        MessageAuthor::Engine,
        "note",
        MessageState::Note,
        0,
    )
    .unwrap()
    .unwrap();
    tx.commit().unwrap();
    let rows = project_rows(&connection, "team_messages", "team-window");
    assert_eq!(rows.len() as i64, MESSAGES_PER_PROJECT_MAX);
    assert_eq!(
        rows[..waiting.len()],
        waiting[..],
        "waiting messages are never pruned"
    );
    let kept = rows.len() - waiting.len() - 1;
    assert_eq!(
        rows[waiting.len()..rows.len() - 1],
        history[history.len() - kept..]
    );
    assert_eq!(rows.last(), Some(&note));
    assert_eq!(
        project_rows(&connection, "team_messages", "team-other"),
        [neighbour]
    );
}

#[test]
#[should_panic(expected = "team_live_rows_fill_the_window")]
fn live_messages_filling_the_window_trip_the_retention_invariant() {
    let (_directory, mut connection) = database();
    let project = ProjectId::new("team-flooded").unwrap();
    let tx = connection.transaction().unwrap();
    for index in 0..MESSAGES_PER_PROJECT_MAX {
        tx.execute(
            "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES ('team-flooded','owner','x','pending',?1)",
            [index],
        )
        .unwrap();
    }
    let _ = append_message(
        &tx,
        &project,
        MessageAuthor::Engine,
        "note",
        MessageState::Note,
        0,
    );
}

#[test]
fn delivered_task_history_makes_room_while_undelivered_results_and_live_work_stay() {
    let (_directory, mut connection) = database();
    let setup = stored(
        "team-tasks",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 2,
            tools: false,
        }],
    );
    let pool = access("team-tasks", &["local-a"]);
    let insert = |tx: &Transaction<'_>, source: &str, status: &str, delivered: i64| {
        tx.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,delivered,created_ms) VALUES ('team-tasks',?1,'codex',0,'step',?2,?3,0)",
            params![source, status, delivered],
        )
        .unwrap();
        tx.last_insert_rowid()
    };
    let tx = connection.transaction().unwrap();
    let undelivered = insert(&tx, "master", "done", 0);
    let running = insert(&tx, "master", "running", 0);
    let owner_done = insert(&tx, "owner", "failed", 0);
    let delivered = insert(&tx, "master", "done", 1);
    for _ in 4..TASKS_PER_PROJECT_MAX {
        insert(&tx, "master", "cancelled", 1);
    }
    let admitted = admit(
        &tx,
        &pool,
        &setup,
        None,
        "new step",
        TaskSource::Owner,
        None,
        1,
    )
    .unwrap()
    .unwrap();
    let rows = project_rows(&tx, "team_tasks", "team-tasks");
    assert_eq!(rows.len() as i64, TASKS_PER_PROJECT_MAX);
    assert_eq!(rows[..4], [undelivered, running, delivered, rows[3]]);
    assert!(
        !rows.contains(&owner_done),
        "the oldest history row made room"
    );
    assert_eq!(rows.last(), Some(&admitted));

    tx.execute(
        "UPDATE team_tasks SET delivered=0, source='master', status='done' WHERE project='team-tasks' AND status<>'running'",
        [],
    )
    .unwrap();
    assert_eq!(
        admit(
            &tx,
            &pool,
            &setup,
            None,
            "refused",
            TaskSource::Owner,
            None,
            2
        )
        .unwrap(),
        Err("team_task_limit")
    );
    assert_eq!(
        project_rows(&tx, "team_tasks", "team-tasks"),
        rows,
        "a refusal deletes nothing"
    );
}

#[test]
fn a_conversation_master_s_delegate_calls_reach_the_team_queue_and_refusals_leave_a_note() {
    let (directory, mut connection) = database();
    let mut teams = Vec::new();
    for (project, sub) in [
        (
            "conversation-team",
            vec![HarnessSlot {
                harness: "codex".into(),
                agents: 2,
                tools: true,
            }],
        ),
        ("conversation-alone", vec![]),
    ] {
        let mut setup = stored(project, sub);
        if setup.team.sub_harnesses.is_empty() {
            setup.team.max_agents = 1;
        }
        connection
            .execute(
                "INSERT INTO project_setups VALUES (?1,?2)",
                params![
                    setup.project_id.as_str(),
                    serde_json::to_string(&setup).unwrap()
                ],
            )
            .unwrap();
        teams.push(setup.project_id);
    }
    let unplanned = ProjectId::new("conversation-unplanned").unwrap();
    let drop = directory.path().join(tool::DROP_FILE);
    let calls = [
        ("Write the README", None),
        ("Check the tests", Some("claude-code")),
    ]
    .iter()
    .enumerate()
    .map(|(id, (task, harness))| {
        let mut arguments = serde_json::json!({ "task": task });
        if let Some(harness) = harness {
            arguments["harness"] = (*harness).into();
        }
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": tool::TOOL_NAME, "arguments": arguments}})
        .to_string()
    })
    .collect::<Vec<_>>()
    .join("\n");
    let pool = access("conversation-team", &["codex-signed-in"]);
    let session = SessionId::new("conversation-session").unwrap();
    let turn = RequestId::new("conversation-turn").unwrap();
    let mut admitted = Vec::new();
    for project in teams.iter().chain([&unplanned]) {
        tool::serve(calls.as_bytes(), Vec::new(), &drop).unwrap();
        let proposed = tool::take(&drop);
        assert_eq!(proposed.len(), 2);
        assert!(!drop.exists());
        let tx = connection.transaction().unwrap();
        admitted.push(
            admit_conversation(&tx, &pool, project, (&session, &turn), &proposed, 1_000).unwrap(),
        );
        tx.commit().unwrap();
    }
    assert_eq!(admitted[0].len(), 1);
    assert_eq!(admitted[1..], [Vec::<i64>::new(), Vec::new()]);
    let task: (String, Option<i64>, String, bool, String, String) = connection
        .query_row(
            "SELECT source,turn,harness,tools,prompt,status FROM team_tasks WHERE id=?1",
            [admitted[0][0]],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        task,
        (
            "owner".into(),
            None,
            "codex".into(),
            true,
            "Write the README".into(),
            "queued".into()
        )
    );
    let tasks: i64 = connection
        .query_row("SELECT COUNT(*) FROM team_tasks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tasks, 1);
    let notes = |project: &ProjectId| -> Vec<(String, String, String)> {
        connection
            .prepare("SELECT author,state,text FROM team_messages WHERE project=?1 ORDER BY id")
            .unwrap()
            .query_map([project.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let note = |lines: &str| {
        vec![(
            "engine".to_owned(),
            "note".to_owned(),
            format!("The conversation master's tasks were not handed off:\n{lines}"),
        )]
    };
    assert_eq!(
        notes(&teams[0]),
        note("- Check the tests: that harness is not in the team plan")
    );
    assert_eq!(
        notes(&teams[1]),
        note(
            "- Write the README: the team plan has no sub-agents\n- Check the tests: the team plan has no sub-agents"
        )
    );
    assert_eq!(
        notes(&unplanned),
        note(
            "- Write the README: this project has no team plan\n- Check the tests: this project has no team plan"
        )
    );

    let tx = connection.transaction().unwrap();
    let setup = crate::project_setup::load_setup(&tx, &teams[0])
        .unwrap()
        .unwrap();
    let own = admit(
        &tx,
        &pool,
        &setup,
        None,
        "Write the changelog",
        TaskSource::Owner,
        None,
        2_000,
    )
    .unwrap()
    .unwrap();
    tx.commit().unwrap();
    let sources: Vec<(i64, TaskSource)> = status(&connection, &setup)
        .unwrap()
        .tasks
        .iter()
        .map(|task| (task.id, task.source))
        .collect();
    assert_eq!(
        sources,
        [
            (admitted[0][0], TaskSource::Master),
            (own, TaskSource::Owner)
        ],
        "the clients read a conversation master's task as the master's, the owner's own as theirs"
    );
}

#[test]
fn a_resolve_reaches_the_team_master_as_an_engine_message_and_a_sub_agent_as_one_pinned_task() {
    let (_directory, mut connection) = database();
    connection
        .execute_batch(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms) VALUES
               ('team-resolve','master','codex',1,'stopped',1,1),
               ('team-resolve','sub','codex',1,'stopped',1,1);
             INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES
               (1,'team-resolve','/r','/r/a','/r/.git/worktrees/a'),
               (2,'team-resolve','/r','/r/b','/r/.git/worktrees/b');",
        )
        .unwrap();
    let project = ProjectId::new("team-resolve").unwrap();
    let tx = connection.transaction().unwrap();
    admit_resolve(&tx, &project, 1, "Resolve the master's merge.", 5)
        .unwrap()
        .unwrap();
    admit_resolve(&tx, &project, 2, "Resolve the first merge.", 6)
        .unwrap()
        .unwrap();
    admit_resolve(&tx, &project, 2, "Resolve the newest merge.", 7)
        .unwrap()
        .unwrap();
    tx.commit().unwrap();
    let messages: Vec<(String, String, String)> = connection
        .prepare("SELECT author,text,state FROM team_messages WHERE project='team-resolve'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        messages,
        vec![(
            "engine".to_owned(),
            "Resolve the master's merge.".to_owned(),
            "pending".to_owned()
        )]
    );
    let tasks: Vec<(String, String, i64)> = connection
        .prepare(
            "SELECT t.prompt,t.status,p.agent FROM team_tasks t JOIN team_task_pins p ON p.task=t.id
             WHERE t.project='team-resolve'",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tasks,
        vec![(
            "Resolve the newest merge.".to_owned(),
            "queued".to_owned(),
            2
        )]
    );
}

#[test]
fn an_owner_shaped_codex_team_reads_the_same_in_the_status_graph_and_register_live_and_stopped() {
    use crate::agents::tests::{alive, until};
    let directory = tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = base.join("engine.db");
    let mut connection = Connection::open(&database).unwrap();
    crate::storage::initialize_authority_schemas(&connection).unwrap();
    let folder = base.join("project");
    std::fs::create_dir(&folder).unwrap();
    let mut setup = stored(
        "lb52-browsky",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 4,
            tools: true,
        }],
    );
    setup.team.min_agents = 5;
    setup.team.max_agents = 5;
    setup.folder = folder.to_str().unwrap().into();
    let row = crate::protocol::StoredProject {
        project_id: setup.project_id.clone(),
        spec: crate::protocol::ProjectSpec {
            name: setup.name.clone(),
            repositories: vec![],
        },
    };
    connection
        .execute(
            "INSERT INTO projects VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&row).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let pool = access("lb52-browsky", &["local-a", "local-b"]);
    let warm = |connection: &mut Connection, on: bool| {
        let tx = connection.transaction().unwrap();
        assert_eq!(
            execute(
                &tx,
                &pool,
                &TeamAction::KeepWarm {
                    project_id: setup.project_id.clone(),
                    on,
                }
            )
            .unwrap(),
            Outcome::Applied
        );
        tx.commit().unwrap();
    };
    warm(&mut connection, true);
    let executable = harness(&base, b"#!/bin/sh\nexec /bin/sleep 300\n");
    let agents = std::sync::Arc::new(crate::agents::Agents::open(&base).unwrap());
    let source = pool.clone();
    let team = runtime::TeamRuntime::open(
        &database,
        &base.join("team"),
        std::collections::BTreeMap::from([("codex", executable)]),
        Box::new(move || Some(source.clone())),
        runtime::TeamLimits::default(),
    )
    .unwrap()
    .with_agents(agents.clone());
    let service = runtime::TeamService::start(team, std::time::Duration::from_millis(50));
    let project = setup.project_id.clone();
    let running = |connection: &Connection| {
        status(connection, &setup)
            .unwrap()
            .agents
            .iter()
            .filter(|a| a.status.live() && a.process_group.is_some())
            .count()
    };
    until("four warm codex sub-agents", || running(&connection) == 4);

    let live = status(&connection, &setup).unwrap();
    let ids: Vec<i64> = live.agents.iter().map(|agent| agent.id).collect();
    assert_eq!(ids.len(), 4, "{live:#?}");
    let groups: Vec<i32> = live
        .agents
        .iter()
        .map(|agent| i32::try_from(agent.process_group.unwrap()).unwrap())
        .collect();
    assert!(groups.iter().all(|group| alive(*group)));
    let listed = agents
        .list(Some(&project), 0, crate::agents::LIST_MAX)
        .unwrap();
    let mut registered: Vec<String> = listed.iter().map(|entry| entry.label.clone()).collect();
    registered.sort();
    let mut expected: Vec<String> = ids
        .iter()
        .map(|id| format!("Team sub-agent {id}"))
        .collect();
    expected.sort();
    assert_eq!(
        registered, expected,
        "the register names each agent by its id"
    );
    for agent in &live.agents {
        assert_eq!(agent.role, AgentRole::Sub);
        let name = names::name(&connection, agent.id).unwrap();
        assert!(name.is_some(), "{agent:?}");
        assert_eq!(
            agent.label,
            names::label(AgentRole::Sub, agent.id, name.as_ref())
        );
        assert!(agent.account.is_some(), "{agent:?}");
    }
    let labels: std::collections::HashMap<i64, String> = live
        .agents
        .iter()
        .map(|agent| (agent.id, agent.label.clone()))
        .collect();

    let shape = [ids[0], ids[1], ids[2], ids[3], ids[2]];
    for (index, agent) in shape.iter().enumerate() {
        connection
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,attempts,result,delivered,created_ms,started_ms,finished_ms)
                 VALUES (?1,'owner','codex',1,?2,'done',?3,1,?4,1,?5,?5,?5)",
                params![
                    project.as_str(),
                    format!("Browsky task {}", index + 1),
                    agent,
                    format!("Result {}", index + 1),
                    now_ms()
                ],
            )
            .unwrap();
    }
    let edges = |graph: &TeamGraph| -> Vec<String> {
        graph.edges.iter().map(|edge| edge.to.clone()).collect()
    };
    let expected_edges: Vec<String> = shape.iter().map(|agent| format!("team-{agent}")).collect();

    let live = graph(&status(&connection, &setup).unwrap());
    let nodes: Vec<&GraphNode> = live
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Agent)
        .collect();
    assert_eq!(nodes.len(), 4, "{live:#?}");
    for (node, id) in nodes.iter().zip(&ids) {
        assert_eq!(node.label, labels[id]);
        assert!(node.status.is_some_and(AgentStatus::live), "{node:?}");
        assert_eq!(
            node.terminal,
            Some(crate::terminal::AgentId::team(*id).unwrap())
        );
        assert_eq!(node.parent.as_deref(), Some("group:codex"));
    }
    assert_eq!(edges(&live), expected_edges);

    warm(&mut connection, false);
    for entry in &listed {
        agents.stop(&entry.id).unwrap();
    }
    until("every stand-in process group to end", || {
        groups.iter().all(|group| !alive(*group))
    });
    until("every sub-agent to read stopped", || {
        status(&connection, &setup)
            .unwrap()
            .agents
            .iter()
            .all(|agent| agent.status == AgentStatus::Stopped)
    });
    let stopped = status(&connection, &setup).unwrap();
    assert_eq!(stopped.agents.len(), 4, "a stopped agent is not replaced");
    assert!(stopped.agents.iter().all(|agent| agent.task.is_none()));
    let stopped = graph(&stopped);
    let nodes: Vec<&GraphNode> = stopped
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Agent)
        .collect();
    assert_eq!(
        nodes.len(),
        4,
        "stopped sub-agents stay listed: {stopped:#?}"
    );
    for (node, id) in nodes.iter().zip(&ids) {
        assert_eq!(node.label, labels[id]);
        assert_eq!(node.status, Some(AgentStatus::Stopped));
        assert_eq!(node.status_label.as_deref(), Some("Stopped"));
        assert_eq!(node.terminal, None);
        assert!(node.account.is_some());
    }
    assert_eq!(
        edges(&stopped),
        expected_edges,
        "each task still points at the agent that ran it"
    );
    service.stop();
}

#[test]
fn the_owner_marks_one_project_s_notifications_seen_up_to_an_id_and_no_other_project_s() {
    let (_directory, mut connection) = database();
    let setup = stored(
        "team-seen",
        vec![HarnessSlot {
            harness: "codex".into(),
            agents: 1,
            tools: false,
        }],
    );
    connection
        .execute(
            "INSERT INTO project_setups VALUES (?1,?2)",
            params![
                setup.project_id.as_str(),
                serde_json::to_string(&setup).unwrap()
            ],
        )
        .unwrap();
    let project = setup.project_id.clone();
    let other = ProjectId::new("team-other").unwrap();
    let pool = access("team-seen", &["local-a"]);
    let raise = |project: &ProjectId, text: &str| {
        events::escalate(&connection, project, text, 5)
            .unwrap()
            .unwrap()
    };
    let first = raise(&project, "May Ada push to main?");
    let second = raise(&project, "May Ada delete the old branch?");
    let foreign = raise(&other, "May Grace spend on a new provider?");
    let third = raise(&project, "May Ada rename the crate?");
    let seen = |project: &ProjectId, upto: i64| TeamAction::Seen {
        project_id: project.clone(),
        upto,
    };
    let tx = connection.transaction().unwrap();
    assert_eq!(
        execute(&tx, &pool, &seen(&project, foreign)).unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        execute(&tx, &pool, &seen(&other, third)).unwrap(),
        rejected("project_setup_missing")
    );
    for upto in [0, -1] {
        assert_eq!(
            execute(&tx, &pool, &seen(&project, upto)).unwrap_err().0,
            "team_notification_missing"
        );
    }
    tx.commit().unwrap();
    let state = |project: &ProjectId| -> Vec<(i64, bool)> {
        events::notifications(&connection, Some(project), 10)
            .unwrap()
            .into_iter()
            .map(|item| (item.id, item.seen_ms.is_some()))
            .collect()
    };
    assert_eq!(
        state(&project),
        [(third, false), (second, true), (first, true)]
    );
    assert_eq!(state(&other), [(foreign, false)]);
}
