use super::*;
use crate::config::{AccountPool, ModelConnection, ModelReference};
use crate::project_setup::{HarnessSlot, StoredSetup, TeamPlan};
use std::os::unix::fs::PermissionsExt;

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
    }
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
                prompt: "Write apple.txt".into()
            },
            Delegation {
                harness: Some("codex".into()),
                prompt: "List the files".into()
            },
            Delegation {
                harness: Some("claude-code".into()),
                prompt: "Review the notes".into()
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
        pick_account(&accounts, &|_| 0, &paused)
            .unwrap()
            .connection
            .as_str(),
        "local-a"
    );
    assert_eq!(
        pick_account(&accounts, &busy_a, &paused)
            .unwrap()
            .connection
            .as_str(),
        "local-b"
    );
    paused.insert(reference("local-b"));
    assert_eq!(
        pick_account(&accounts, &busy_a, &paused)
            .unwrap()
            .connection
            .as_str(),
        "local-a"
    );
    paused.insert(reference("local-a"));
    assert!(pick_account(&accounts, &|_| 0, &paused).is_none());
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
    let none = stored("team-solo", vec![]);
    assert_eq!(
        admit(
            &tx,
            &with_pool,
            &none,
            None,
            "Alone",
            TaskSource::Owner,
            None,
            16
        )
        .unwrap(),
        Err("team_has_no_sub_agents")
    );
    tx.commit().unwrap();
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
        ("stopped".into(), None, Some("The engine restarted".into()))
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
    assert_eq!(delegation_path("claude"), DelegationPath::Reply);
    assert_eq!(delegation_path("gemini"), DelegationPath::Unavailable);
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
    let executable = base.join("codex");
    std::fs::write(&executable, b"#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    assert!(
        cold.messages.iter().any(|m| m.state == MessageState::Note
            && m.text.starts_with("Keeping sub-agents warm was turned off")),
        "{cold:#?}"
    );
    assert!(!team.has_live_agents());
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
    let executable = base.join("codex");
    std::fs::write(&executable, b"#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let commits = std::process::Command::new("/usr/bin/git")
        .args(["-C", root.to_str().unwrap(), "rev-list", "--count", "HEAD"])
        .env_clear()
        .env("HOME", &base)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(commits.status.success());
    assert_eq!(String::from_utf8_lossy(&commits.stdout).trim(), "1");
    let exclude = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
    assert_eq!(exclude.matches("/worktrees/").count(), 1, "{exclude}");
}
