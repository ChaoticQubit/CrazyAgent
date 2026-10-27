use super::*;

use crate::config::{
    AccountPool, ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference,
};

use crate::harnesses::interactive::{InteractiveCommand, Transcript};

use crate::project_setup::{HarnessSlot, TeamPlan};

use crate::team::status;

use crate::terminal::FOLDER;

use crate::terminal::tmux::{SOCKET, Tmux, installed};

use std::process::{Command, Stdio};

use std::time::SystemTime;

const PROJECT: &str = "team-panes";

const ENDPOINT: &str = "local-a";

const TURN_OPEN: &str = r#"{"type":"user","message":{"content":"Plan the page"}}"#;

const TURN_DONE: &str = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done"}],"stop_reason":"end_turn"}}"#;

const WAIT: Duration = Duration::from_secs(30);

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    tmux: PathBuf,
    connection: Connection,
    setup: StoredSetup,
    access: ModelAccess,
}

impl Fixture {
    fn new(subs: u8) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-team-panes-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        let connection = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let owner = connection.unchecked_transaction().unwrap();
        crate::conversation::initialize(&owner).unwrap();
        owner.commit().unwrap();
        super::super::initialize(&connection).unwrap();
        crate::project_setup::initialize(&connection).unwrap();
        let folder = base.join("project");
        std::fs::create_dir(&folder).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: "Panes".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: TeamPlan {
                master: "codex".into(),
                master_tools: false,
                sub_harnesses: vec![HarnessSlot {
                    harness: "codex".into(),
                    agents: subs,
                    tools: false,
                }],
                min_agents: 1,
                max_agents: subs + 1,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
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
                params![PROJECT, serde_json::to_string(&row).unwrap()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO project_setups VALUES (?1,?2)",
                params![PROJECT, serde_json::to_string(&setup).unwrap()],
            )
            .unwrap();
        let access = ModelAccess {
            accounts: vec![reference("local-account")],
            connections: vec![ModelConnection {
                id: reference(ENDPOINT),
                provider: reference("openai-compatible"),
                harness: reference("rcli"),
                protocol: reference("openai-chat"),
                account_ref: reference("local-account"),
                endpoint: ModelEndpoint::Http {
                    url: "http://127.0.0.1:11434/v1".into(),
                },
                model: ModelReference {
                    id: "qwen2.5:0.5b".into(),
                    author: reference("openai-compatible"),
                },
                auth: ModelAuthentication::None {},
            }],
            pools: vec![AccountPool {
                id: reference("team-pool"),
                name: None,
                members: vec![reference(ENDPOINT)],
                projects: vec![reference(PROJECT)],
                roles: vec![reference("project_leader")],
                fallback_allowed: false,
                provider_quota_policy: Default::default(),
            }],
        };
        Self {
            _root: root,
            base,
            tmux,
            connection,
            setup,
            access,
        }
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
    }

    fn runtime(&self) -> TeamRuntime {
        let access = self.access.clone();
        TeamRuntime::open(
            &self.base.join("engine.db"),
            &self.base.join("team"),
            BTreeMap::from([("codex", self.base.join("codex"))]),
            Box::new(move || Some(access.clone())),
            TeamLimits::default(),
        )
        .unwrap()
    }

    fn agent(&self, role: AgentRole, harness: &str, status: AgentStatus) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,status,created_ms,heartbeat_ms) VALUES (?1,?2,?3,0,?4,?5,1,1)",
                params![PROJECT, role.name(), harness, ENDPOINT, status.name()],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    fn pane(&self, host: &Arc<dyn Host>, id: i64) -> PaneRecord {
        let file = self.base.join(format!("session-{id}.jsonl"));
        std::fs::write(&file, format!("{TURN_OPEN}\n{TURN_DONE}\n")).unwrap();
        let command = InteractiveCommand {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec cat".into()],
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::ClaudeSession { file },
        };
        let since = SystemTime::now();
        let pane = host.spawn(&AgentId::team(id).unwrap(), &command).unwrap();
        PaneRecord::new(&command.transcript, &command.cwd, since, pane.pid, None).unwrap()
    }

    fn keep(&self, id: i64, record: &PaneRecord) {
        self.connection
            .execute(
                "INSERT INTO team_panes(agent,record) VALUES (?1,?2)",
                params![id, record.encode().unwrap()],
            )
            .unwrap();
    }

    fn recorded(&self, id: i64) -> bool {
        self.connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM team_panes WHERE agent=?1)",
                [id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn status(&self) -> super::super::TeamStatus {
        status(&self.connection, &self.setup).unwrap()
    }

    fn agent_status(&self, id: i64) -> (AgentStatus, Option<String>) {
        let agent = self
            .status()
            .agents
            .into_iter()
            .find(|agent| agent.id == id)
            .unwrap();
        (agent.status, agent.problem)
    }

    fn note(&self, text: &str) -> bool {
        self.status()
            .messages
            .iter()
            .any(|message| message.state == MessageState::Note && message.text == text)
    }

    fn until(&self, team: &mut TeamRuntime, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(self) {
            assert!(Instant::now() < deadline, "{what}: {:#?}", self.status());
            team.tick().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.base.join(FOLDER).join(SOCKET))
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}

fn names(ids: &[i64]) -> Vec<AgentId> {
    ids.iter().map(|id| AgentId::team(*id).unwrap()).collect()
}

#[test]
fn a_restarted_engine_takes_over_live_agent_panes_and_ends_every_other_one() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let master = fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle);
    let dropped = fixture.agent(AgentRole::Sub, "claude-code", AgentStatus::Working);
    let unrecorded = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let record = fixture.pane(&host, master);
    fixture.keep(master, &record);
    let other = fixture.pane(&host, dropped);
    fixture.keep(dropped, &other);
    fixture.pane(&host, unrecorded);
    fixture.pane(&host, 9000);

    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    assert_eq!(
        host.alive().unwrap(),
        names(&[master]),
        "only the master that still fits the plan keeps its pane"
    );
    assert!(fixture.note(&format!(
        "The terminal of team agent {dropped} was closed: the team plan no longer has room for it."
    )));
    assert!(fixture.note(&format!(
        "The terminal of team agent {unrecorded} was closed: the engine has no record of it."
    )));
    fixture.until(&mut team, "the adopted master is idle", |f| {
        f.agent_status(master).0 == AgentStatus::Idle && f.recorded(master)
    });
    assert_eq!(fixture.agent_status(master).1, None);
    assert_eq!(
        std::fs::read_to_string(fixture.base.join(format!("session-{master}.jsonl")))
            .unwrap()
            .lines()
            .count(),
        2,
        "nothing was typed into the idle master"
    );

    let stray = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Stopped);
    fixture.pane(&host, stray);
    fixture.until(&mut team, "a pane no agent owns ends", |f| {
        f.note(&format!(
            "The terminal of team agent {stray} was closed: its agent had stopped."
        ))
    });
    assert_eq!(host.alive().unwrap(), names(&[master]));

    assert_eq!(team.shutdown().unwrap(), names(&[master]));
    assert_eq!(
        host.alive().unwrap(),
        names(&[master]),
        "the agent's session outlives the engine"
    );
    assert_eq!(
        fixture.agent_status(master),
        (AgentStatus::Stopped, Some(LEFT_RUNNING_NOTE.into()))
    );
    assert!(fixture.recorded(master));

    let mut again = fixture.runtime().with_terminal(host.clone()).unwrap();
    fixture.until(&mut again, "the next engine takes the master over", |f| {
        f.agent_status(master) == (AgentStatus::Idle, None) && f.recorded(master)
    });
    assert_eq!(again.shutdown().unwrap(), names(&[master]));
    assert_eq!(host.alive().unwrap(), names(&[master]));
    host.close(&[]).unwrap();
    assert!(host.alive().unwrap().is_empty());
}

#[test]
fn a_project_never_runs_more_than_fifteen_agent_terminals() {
    let fixture = Fixture::new(14);
    let host = fixture.host();
    let mut ids = vec![fixture.agent(AgentRole::Master, "codex", AgentStatus::Idle)];
    for _ in 0..15 {
        ids.push(fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle));
    }
    for id in &ids {
        let record = fixture.pane(&host, *id);
        fixture.keep(*id, &record);
    }

    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    let alive = host.alive().unwrap();
    assert_eq!(alive.len(), PANES_PER_PROJECT_MAX, "{alive:?}");
    assert!(!alive.contains(&AgentId::team(ids[15]).unwrap()));
    assert!(fixture.note(&format!(
        "The terminal of team agent {} was closed: the team plan no longer has room for it.",
        ids[15]
    )));

    let Accounts::Usable(accounts) =
        eligible_accounts(&fixture.access, &fixture.setup.project_id, "codex")
    else {
        panic!("the endpoint fits codex");
    };
    let before = fixture.status().agents.len();
    let refused = team
        .spawn_run(
            &fixture.setup,
            AgentRole::Sub,
            "codex",
            false,
            &accounts[0],
            None,
        )
        .unwrap();
    assert_eq!(
        refused,
        Err("the project already runs 15 agents, the most one project may run at once".into())
    );
    assert_eq!(
        fixture.status().agents.len(),
        before,
        "no agent was recorded"
    );

    fixture.until(&mut team, "every adopted agent has its pane record", |f| {
        ids[..PANES_PER_PROJECT_MAX]
            .iter()
            .all(|id| f.recorded(*id))
    });
    let mut left = team.shutdown().unwrap();
    assert_eq!(
        left.len(),
        PANES_PER_PROJECT_MAX,
        "every adopted agent keeps running"
    );
    left.sort();
    assert_eq!(host.alive().unwrap(), left);
    host.close(&[]).unwrap();
}

#[test]
fn a_removed_project_stops_its_agents_and_their_panes() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let sub = fixture.agent(AgentRole::Sub, "codex", AgentStatus::Idle);
    let record = fixture.pane(&host, sub);
    fixture.keep(sub, &record);
    let mut team = fixture.runtime().with_terminal(host.clone()).unwrap();
    fixture.until(&mut team, "the sub-agent is adopted", |f| {
        f.agent_status(sub).0 == AgentStatus::Idle
    });

    fixture
        .connection
        .execute("DELETE FROM project_setups WHERE project=?1", [PROJECT])
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while team.has_live_agents() || !host.alive().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "{:?}", host.alive());
        team.tick().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!fixture.recorded(sub));
    assert!(team.shutdown().unwrap().is_empty());
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection
        .query_row(sql, [PROJECT], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_full_project_still_stores_the_master_reply_and_delivers_sub_agent_results() {
    use crate::protocol::Outcome;
    use crate::team::{MESSAGES_PER_PROJECT_MAX, TASKS_PER_PROJECT_MAX, TeamAction};
    let mut fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    let tx = fixture.connection.transaction().unwrap();
    let waiting = append_message(
        &tx,
        &project,
        MessageAuthor::Owner,
        "An early message the master has not answered",
        MessageState::Pending,
        1,
    )
    .unwrap()
    .unwrap();
    tx.execute(
        "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,result,created_ms,finished_ms) VALUES (?1,'master','codex',0,'Early step','done','early result',1,1)",
        [PROJECT],
    )
    .unwrap();
    let undelivered = tx.last_insert_rowid();
    for index in 1..TASKS_PER_PROJECT_MAX {
        tx.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,delivered,created_ms) VALUES (?1,'master','codex',0,'Old step','done',1,?2)",
            params![PROJECT, index],
        )
        .unwrap();
    }
    for index in 1..MESSAGES_PER_PROJECT_MAX {
        let (author, state) = if index % 2 == 1 {
            (MessageAuthor::Owner, MessageState::Answered)
        } else {
            (MessageAuthor::Master, MessageState::Reply)
        };
        append_message(&tx, &project, author, "Old history", state, index)
            .unwrap()
            .unwrap();
    }
    tx.commit().unwrap();
    let all_messages = "SELECT COUNT(*) FROM team_messages WHERE project=?1";
    let all_tasks = "SELECT COUNT(*) FROM team_tasks WHERE project=?1";
    assert_eq!(
        count(&fixture.connection, all_messages),
        MESSAGES_PER_PROJECT_MAX
    );
    assert_eq!(count(&fixture.connection, all_tasks), TASKS_PER_PROJECT_MAX);

    let tx = fixture.connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &TeamAction::Message {
                project_id: project.clone(),
                text: "Build the landing page".into(),
            },
        )
        .unwrap(),
        Outcome::Applied,
        "message 2001 is accepted"
    );
    tx.commit().unwrap();
    let latest: i64 = fixture
        .connection
        .query_row(
            "SELECT MAX(id) FROM team_messages WHERE project=?1 AND author='owner'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();

    let state = |id: i64| -> Option<String> {
        Connection::open(fixture.base.join("engine.db"))
            .unwrap()
            .query_row("SELECT state FROM team_messages WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .unwrap()
    };
    assert_eq!(
        state(waiting).as_deref(),
        Some("pending"),
        "the unanswered owner message outlived the append that pruned history"
    );
    let mut team = fixture.runtime();
    team.finish_message(&project, waiting, "Noted.", TurnEnd::Success, None, vec![])
        .unwrap();
    team.finish_message(
        &project,
        latest,
        "Here is the plan.\nDELEGATE: Write the landing page",
        TurnEnd::Success,
        None,
        vec![],
    )
    .unwrap();
    let team_status = status(&fixture.connection, &fixture.setup).unwrap();
    let replies: Vec<&str> = team_status
        .messages
        .iter()
        .filter(|message| message.author == MessageAuthor::Master && message.id > latest)
        .map(|message| message.text.as_str())
        .collect();
    assert_eq!(
        replies,
        [
            "Noted.",
            "Here is the plan.\nDELEGATE: Write the landing page"
        ]
    );
    assert_eq!(state(latest).as_deref(), Some("answered"));
    assert_eq!(
        state(waiting),
        None,
        "once answered, the oldest message is history and made room for its own reply"
    );
    let handed: i64 = fixture
        .connection
        .query_row(
            "SELECT id FROM team_tasks WHERE project=?1 AND prompt='Write the landing page' AND source='master'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert!(count(&fixture.connection, all_messages) <= MESSAGES_PER_PROJECT_MAX);
    assert!(count(&fixture.connection, all_tasks) <= TASKS_PER_PROJECT_MAX);

    fixture
        .connection
        .execute(
            "UPDATE team_tasks SET status='done', result='page written', finished_ms=2 WHERE id=?1",
            [handed],
        )
        .unwrap();
    team.deliver(&fixture.setup).unwrap();
    let delivered: String = fixture
        .connection
        .query_row(
            "SELECT text FROM team_messages WHERE project=?1 AND author='engine' AND state='pending'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert!(delivered.contains("early result"), "{delivered}");
    assert!(delivered.contains("page written"), "{delivered}");
    assert_eq!(
        count(
            &fixture.connection,
            "SELECT COUNT(*) FROM team_tasks WHERE project=?1 AND delivered=0 AND source='master'"
        ),
        0
    );
    assert_eq!(
        fixture
            .connection
            .query_row(
                "SELECT delivered FROM team_tasks WHERE id=?1",
                [undelivered],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "the oldest undelivered result was kept until it was delivered"
    );
    assert!(count(&fixture.connection, all_messages) <= MESSAGES_PER_PROJECT_MAX);
}

#[test]
fn a_parked_session_s_sub_agent_results_wait_for_it_and_reach_it_once_resumed() {
    let fixture = Fixture::new(1);
    let db = &fixture.connection;
    for (id, created, phase) in [("first", 1, "parked"), ("second", 2, "configured")] {
        db.execute(
            "INSERT INTO master_sessions(id,project,created,version,phase,body) VALUES (?1,?2,?3,?3,?4,'{}')",
            params![id, PROJECT, created, phase],
        )
        .unwrap();
    }
    for (session, result) in [
        ("first", "parked copy drafted"),
        ("second", "live page built"),
    ] {
        db.execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,result,created_ms,finished_ms,session) VALUES (?1,'master','codex',0,'Work',?2,?3,1,2,?4)",
            params![PROJECT, "done", result, session],
        )
        .unwrap();
    }
    let delivered = |db: &Connection| -> Vec<String> {
        let mut statement = db
            .prepare(
                "SELECT text FROM team_messages WHERE project=?1 AND author='engine' ORDER BY id",
            )
            .unwrap();
        statement
            .query_map([PROJECT], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let waiting = |id: &str| crate::protocol::SessionId::new(id).unwrap();
    let mut team = fixture.runtime();
    team.deliver(&fixture.setup).unwrap();
    let texts = delivered(db);
    assert_eq!(texts.len(), 1);
    assert!(texts[0].contains("live page built"), "{}", texts[0]);
    assert!(!texts[0].contains("parked copy drafted"), "{}", texts[0]);
    assert_eq!(
        crate::master_sessions::results_waiting(db, &waiting("first")).unwrap(),
        1
    );
    assert_eq!(
        crate::master_sessions::results_waiting(db, &waiting("second")).unwrap(),
        0
    );

    db.execute(
        "UPDATE team_messages SET state='answered' WHERE project=?1 AND author='engine'",
        [PROJECT],
    )
    .unwrap();
    team.deliver(&fixture.setup).unwrap();
    assert_eq!(
        delivered(db).len(),
        1,
        "nothing more reaches the live session"
    );
    assert!(
        !team
            .active_projects()
            .unwrap()
            .contains(&fixture.setup.project_id),
        "results waiting only for a parked session do not wake the project every tick"
    );

    db.execute(
        "UPDATE master_sessions SET phase='parked' WHERE id='second'",
        [],
    )
    .unwrap();
    db.execute(
        "UPDATE master_sessions SET phase='configured' WHERE id='first'",
        [],
    )
    .unwrap();
    assert!(
        team.active_projects()
            .unwrap()
            .contains(&fixture.setup.project_id)
    );
    team.deliver(&fixture.setup).unwrap();
    let texts = delivered(db);
    assert_eq!(texts.len(), 2);
    assert!(texts[1].contains("parked copy drafted"), "{}", texts[1]);
    assert!(!texts[1].contains("live page built"), "{}", texts[1]);
    assert_eq!(
        crate::master_sessions::results_waiting(db, &waiting("first")).unwrap(),
        0
    );
}

#[test]
fn work_of_a_removed_project_ends_without_an_error_or_new_rows() {
    let fixture = Fixture::new(1);
    let project = fixture.setup.project_id.clone();
    fixture
        .connection
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'master','codex',0,'Write the page','running',1)",
            [PROJECT],
        )
        .unwrap();
    let task = fixture.connection.last_insert_rowid();
    fixture
        .connection
        .execute(
            "INSERT INTO team_messages(project,author,text,state,created_ms) VALUES (?1,'owner','Plan it','sent',1)",
            [PROJECT],
        )
        .unwrap();
    let message = fixture.connection.last_insert_rowid();
    for statement in [
        "DELETE FROM team_tasks WHERE project=?1",
        "DELETE FROM team_messages WHERE project=?1",
        "DELETE FROM project_setups WHERE project=?1",
        "DELETE FROM projects WHERE id=?1",
    ] {
        fixture.connection.execute(statement, [PROJECT]).unwrap();
    }
    let mut team = fixture.runtime();
    team.finish_task(task, "page written", TurnEnd::Success, None)
        .unwrap();
    team.finish_task(task, "", TurnEnd::Closed, Some("launch_closed"))
        .unwrap();
    team.finish_message(&project, message, "", TurnEnd::Interrupted, None, vec![])
        .unwrap();
    team.finish_message(&project, message, "Done", TurnEnd::Success, None, vec![])
        .unwrap();
    for table in ["team_tasks", "team_messages"] {
        assert_eq!(
            count(
                &fixture.connection,
                &format!("SELECT COUNT(*) FROM {table} WHERE project=?1")
            ),
            0,
            "{table}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_worker_writes_only_its_own_worktree_home_and_tmp() {
    let fixture = Fixture::new(2);
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let project = fixture.base.join("project");
    std::fs::write(project.join("owner.txt"), "owner\n").unwrap();
    let access = fixture.access.clone();
    let runtime = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap();
    let account = TeamAccount {
        connection: reference(ENDPOINT),
        reach: TeamReach::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
        },
    };
    let plan = |id: i64| {
        runtime
            .prepare(
                &fixture.setup,
                id,
                AgentRole::Sub,
                "codex",
                true,
                &account,
                false,
            )
            .unwrap()
            .1
    };
    let first = plan(1);
    let second = plan(2);
    let object = project.join(".git").join("objects");
    let head = String::from_utf8(
        crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&project)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .stdout,
    )
    .unwrap();
    let head = head.trim();
    let object = object.join(&head[..2]).join(&head[2..]);
    let fanout = object.parent().unwrap();
    let before = std::fs::read(&object).unwrap();
    let quoted = |path: &Path| format!("'{}'", path.display());
    let script = format!(
        "g='/usr/bin/git -c user.name=W -c user.email=w@crazyagent.invalid -c core.hooksPath=/dev/null'\n\
         echo mine > mine.txt && $g add mine.txt && $g commit -q -m mine && echo committed\n\
         (echo x > {sibling}) 2>/dev/null && echo sibling=written || echo sibling=refused\n\
         (echo x >> {owner}) 2>/dev/null && echo owner=written || echo owner=refused\n\
         (echo x > {fresh}) 2>/dev/null && echo project=written || echo project=refused\n\
         (ln {owner} linked.txt) 2>/dev/null && echo link=made || echo link=refused\n\
         (rm -f {object}; echo junk > {object}) 2>/dev/null; echo object=tried\n\
         (mv {fanout} {moved}) 2>/dev/null; echo fanout=tried\n",
        sibling = quoted(&second.working_dir.join("escaped.txt")),
        owner = quoted(&project.join("owner.txt")),
        fresh = quoted(&project.join("fresh.txt")),
        object = quoted(&object),
        fanout = quoted(fanout),
        moved = quoted(&fanout.with_extension("moved")),
    );
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&first.sandbox_profile)
            .args(["/bin/sh", "-c", &script])
            .current_dir(&first.working_dir)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", runtime.agent_folder(1).join("home"))
            .env("TMPDIR", runtime.agent_folder(1).join("tmp"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        text,
        "committed\nsibling=refused\nowner=refused\nproject=refused\nlink=refused\nobject=tried\nfanout=tried\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!second.working_dir.join("escaped.txt").exists());
    assert_eq!(
        std::fs::read_to_string(project.join("owner.txt")).unwrap(),
        "owner\n"
    );
    assert!(!project.join("fresh.txt").exists());
    assert_eq!(
        std::fs::read(&object).unwrap(),
        before,
        "the worker replaced a git object"
    );
    assert!(!fanout.with_extension("moved").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn a_managed_agent_runs_in_its_own_home_and_never_reaches_the_account_home() {
    let fixture = Fixture::new(1);
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(
        fixture.base.join("engine.db"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let access = fixture.access.clone();
    let runtime = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap();
    let profile = reference("managed-profile");
    let managed = runtime.custody.as_ref().unwrap().profile_home(&profile);
    std::fs::create_dir_all(&managed).unwrap();
    std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(managed.join("auth.json"), "{}").unwrap();
    let account = TeamAccount {
        connection: reference("managed-connection"),
        reach: TeamReach::Managed {
            profile_ref: profile,
            model: None,
        },
    };
    for (id, role) in [(1, AgentRole::Master), (2, AgentRole::Sub)] {
        let (_, plan, mode) = runtime
            .prepare(&fixture.setup, id, role, "codex", true, &account, false)
            .unwrap();
        let own = runtime.agent_folder(id).join("home");
        match mode {
            Mode::PerTurn { home } => assert_eq!(home, own),
            Mode::Kept { .. } => panic!("a managed agent must take its account per turn"),
        }
        let quoted = |path: &Path| format!("'{}'", path.display());
        let script = format!(
            "(echo x > {planted}) 2>/dev/null && echo plant=written || echo plant=refused\n\
             (cat {credential} > /dev/null) 2>/dev/null && echo read=done || echo read=refused\n\
             (echo x > {mine}) 2>/dev/null && echo own=written || echo own=refused\n",
            planted = quoted(&managed.join("AGENTS.md")),
            credential = quoted(&managed.join("auth.json")),
            mine = quoted(&own.join("note.txt")),
        );
        let output = crate::spawn_gate::output(
            Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(&plan.sandbox_profile)
                .args(["/bin/sh", "-c", &script])
                .current_dir(&plan.working_dir)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &own)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "plant=refused\nread=refused\nown=written\n",
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!managed.join("AGENTS.md").exists());
    }
}

#[test]
fn both_master_framings_name_the_harnesses_an_empty_plan_can_use() {
    let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
    let folder = root.path().canonicalize().unwrap().join("project");
    std::fs::create_dir_all(folder.join("context")).unwrap();
    std::fs::write(folder.join("context").join("brief.md"), "# Any\n").unwrap();
    let setup = StoredSetup {
        project_id: ProjectId::new(PROJECT).unwrap(),
        name: "Any".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: true,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: crate::project_setup::TEAM_AGENTS_MAX,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    };
    let listed = "Your sub-agents:\nThe team plan names no harness, so you may use any harness that can run now:\n- codex\n- claude-code\nThere is no per-harness limit, only the team maximum of 64 agents, counting you.\n";
    for text in [
        framing(&setup, true, &["codex", "claude-code"]),
        live_framing(&setup, true, &["codex", "claude-code"]),
    ] {
        assert!(text.contains(listed), "{text}");
    }
}

#[test]
fn the_master_framing_reads_only_a_bounded_regular_brief_and_never_blocks() {
    let root = tempfile::tempdir_in(std::fs::canonicalize("/tmp").unwrap()).unwrap();
    let base = root.path().canonicalize().unwrap();
    let folder = base.join("project");
    std::fs::create_dir_all(folder.join("context")).unwrap();
    let secret = base.join("secret.md");
    std::fs::write(&secret, "outside the project").unwrap();
    let setup = StoredSetup {
        project_id: ProjectId::new(PROJECT).unwrap(),
        name: "Panes".into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![HarnessSlot {
                harness: "codex".into(),
                agents: 1,
                tools: false,
            }],
            min_agents: 1,
            max_agents: 2,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    };
    let brief = folder.join("context").join("brief.md");
    let frame = || {
        let (sent, got) = channel();
        let setup = setup.clone();
        std::thread::spawn(move || sent.send(framing(&setup, false, &[])));
        got.recv_timeout(Duration::from_secs(5)).ok()
    };
    let mut wrong = Vec::new();

    std::fs::write(&brief, "# Panes\n\nPlan the page.\n").unwrap();
    let framed = frame();
    if !framed
        .as_deref()
        .is_some_and(|text| text.contains("Project brief:\n# Panes\n\nPlan the page.\n"))
    {
        wrong.push(("normal", framed));
    }

    std::fs::remove_file(&brief).unwrap();
    std::os::unix::fs::symlink(&secret, &brief).unwrap();
    let framed = frame();
    if !framed.as_deref().is_some_and(|text| {
        text.contains("context/brief.md is not a regular file") && !text.contains("outside")
    }) {
        wrong.push(("link", framed));
    }

    std::fs::remove_file(&brief).unwrap();
    assert!(
        crate::spawn_gate::status(Command::new("/usr/bin/mkfifo").arg(&brief))
            .unwrap()
            .success()
    );
    let framed = frame();
    if !framed
        .as_deref()
        .is_some_and(|text| text.contains("context/brief.md is not a regular file"))
    {
        wrong.push(("fifo", framed));
    }

    std::fs::remove_file(&brief).unwrap();
    let huge = 64 * 1024 * 1024;
    let mut text = String::from("# Panes\n\n");
    text.push_str(&"a".repeat(2 * crate::project_setup::CONTEXT_CHUNK_BYTES));
    std::fs::write(&brief, &text).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&brief)
        .unwrap()
        .set_len(huge)
        .unwrap();
    let framed = frame();
    if !framed.as_deref().is_some_and(|text| {
        text.contains("Project brief:\n# Panes\n\naaaa")
            && text.contains(&format!("The brief is {huge} bytes"))
            && text.len() < BRIEF_BYTES_MAX + 2048
    }) {
        wrong.push(("oversized", framed.map(|text| bound_text(&text, 200))));
    }

    std::fs::remove_file(&brief).unwrap();
    let framed = frame();
    if !framed
        .as_deref()
        .is_some_and(|text| text.contains("context/brief.md is missing"))
    {
        wrong.push(("missing", framed));
    }

    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_launch_error_that_closes_an_agent_names_its_plain_code_on_the_agent_and_its_task() {
    assert_eq!(
        launch_end(LaunchError("launch_deadline".into())),
        (TurnEnd::TimedOut, None)
    );
    assert_eq!(
        launch_end(LaunchError("launch_event_limit".into())),
        (TurnEnd::TooLong, None)
    );
    let (end, cause) = launch_end(LaunchError("launch_protocol_failed".into()));
    assert_eq!(end, TurnEnd::Closed);
    assert_eq!(
        with_code(PROCESS_CLOSED, cause.as_deref()),
        "The agent process closed. (launch_protocol_failed)"
    );
    assert_eq!(
        with_code(
            "The agent could not start",
            Some("No such file: /Users/mira/key")
        ),
        "The agent could not start. (engine_text_withheld)"
    );
    assert_eq!(with_code(PROCESS_CLOSED, None), PROCESS_CLOSED);
    assert_eq!(
        with_code(
            "The master stopped before it replied.",
            Some("launch_input_closed")
        ),
        "The master stopped before it replied. (launch_input_closed)"
    );
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime();
    fixture
        .connection
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,created_ms) VALUES (?1,'master','codex',0,'Write the page','running',1)",
            [PROJECT],
        )
        .unwrap();
    let task = fixture.connection.last_insert_rowid();
    team.finish_task(task, "", TurnEnd::Closed, Some("launch_input_closed"))
        .unwrap();
    let problem: String = fixture
        .connection
        .query_row("SELECT error FROM team_tasks WHERE id=?1", [task], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        problem,
        "The sub-agent stopped unexpectedly. (launch_input_closed)"
    );
}

/// The owner stops a sub-agent while its harness has not drawn its screen yet, which a
/// slow first launch, an update check or a sign-in screen can make last up to the whole
/// ready wait. The stop ends the agent, its pane and its process group within one bound,
/// not after the ready wait.
#[cfg(target_os = "macos")]
#[test]
fn an_owner_stop_ends_an_agent_whose_harness_has_not_drawn_its_screen_yet() {
    const STOP_BOUND: Duration = Duration::from_secs(5);
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexec /bin/sleep 300\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agents = Arc::new(Agents::open(&fixture.base).unwrap());
    let access = fixture.access.clone();
    let mut team = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_agents(agents.clone())
    .with_terminal(host.clone())
    .unwrap();
    let mut connection = Connection::open(fixture.base.join("engine.db")).unwrap();
    let tx = connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &crate::team::TeamAction::Delegate {
                project_id: fixture.setup.project_id.clone(),
                harness: Some("codex".into()),
                prompt: "Write page.md".into(),
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    fixture.until(&mut team, "the sub-agent works on the task", |f| {
        f.status()
            .agents
            .iter()
            .any(|agent| agent.status == AgentStatus::Working && agent.process_group.is_some())
    });
    let member = fixture.status().agents.remove(0);
    let group = i32::try_from(member.process_group.unwrap()).unwrap();
    let listed = agents
        .list(Some(&fixture.setup.project_id), 0, crate::agents::LIST_MAX)
        .unwrap();
    assert_eq!(listed.len(), 1, "{listed:#?}");
    let register = listed[0].id.clone();

    let asked = Instant::now();
    let stopper = {
        let agents = agents.clone();
        std::thread::spawn(move || agents.stop(&register))
    };
    while fixture.agent_status(member.id).0 != AgentStatus::Stopped {
        assert!(
            asked.elapsed() < STOP_BOUND,
            "the agent still runs {:?} after the owner's stop: {:#?}",
            asked.elapsed(),
            fixture.status()
        );
        team.tick().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    let stopped = stopper.join().unwrap().unwrap();
    assert_eq!(stopped.state, RunState::Stopped);
    assert!(!stopped.can_stop);
    assert!(
        !host
            .alive()
            .unwrap()
            .contains(&AgentId::team(member.id).unwrap())
    );
    assert!(!crate::agents::tests::alive(group));
    assert!(team.shutdown().unwrap().is_empty());
}

#[test]
fn a_conversation_note_names_the_task_its_agent_how_it_ended_and_where_its_result_is() {
    let ended =
        |status, result: Option<&str>, problem: Option<&str>, agent: Option<i64>| EndedTask {
            id: 7,
            harness: "codex".into(),
            status,
            prompt: "Write   the\nREADME".into(),
            result: result.map(Into::into),
            problem: problem.map(Into::into),
            agent,
            worktree: agent.map(|id| format!(".crazyagent/worktrees/agent-{id}")),
        };
    assert_eq!(
        conversation_note(&ended(
            TaskStatus::Done,
            Some("README written.\n\nIt covers setup."),
            None,
            Some(3)
        )),
        "Task 7 \"Write the README\" (codex, sub-agent 3) is done. Full result in the Team view, files in .crazyagent/worktrees/agent-3. It said: README written. It covers setup."
    );
    assert_eq!(
        conversation_note(&ended(TaskStatus::Done, None, None, None)),
        "Task 7 \"Write the README\" (codex) is done. Full result in the Team view."
    );
    assert_eq!(
        conversation_note(&ended(
            TaskStatus::Failed,
            None,
            Some("The sub-agent ran out of time"),
            Some(3)
        )),
        "Task 7 \"Write the README\" (codex, sub-agent 3) failed: The sub-agent ran out of time. Details in the Team view."
    );
    assert_eq!(
        conversation_note(&ended(
            TaskStatus::Cancelled,
            None,
            Some("Cancelled by you"),
            None
        )),
        "Task 7 \"Write the README\" (codex) was stopped: Cancelled by you. Details in the Team view."
    );
    let long = "word ".repeat(400);
    let mut huge = ended(TaskStatus::Done, Some(&long), Some(&long), Some(i64::MAX));
    huge.prompt = long.clone();
    huge.harness = "h".repeat(64);
    huge.worktree = Some("w".repeat(400));
    let note = conversation_note(&huge);
    assert!(note.len() <= NOTE_BYTES_MAX, "{} bytes", note.len());
    assert!(note.ends_with("..."), "a cut result says so: {note}");
}

/// Writes a small owner-only executable at `path`, replacing any file there.
fn engine_file(path: &Path) {
    std::fs::write(path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_team_master_s_delegate_server_is_the_engine_the_runtime_started_with() {
    let fixture = Fixture::new(1);
    let folder = fixture.base.join("engine-bin");
    std::fs::create_dir(&folder).unwrap();
    let engine = folder.join("crazyagent");
    engine_file(&engine);
    let runtime = fixture.runtime().with_delegation_tool(&engine).unwrap();
    let project = ProjectId::new(PROJECT).unwrap();
    let master = runtime
        .context(&project, 1, AgentRole::Master, true)
        .unwrap();
    let delegate: Vec<&crate::plugins::LaunchServer> = master
        .plugins()
        .servers
        .iter()
        .filter(|server| server.name == tool::SERVER_NAME)
        .collect();
    assert_eq!(delegate.len(), 1);
    let crate::plugins::ServerSpec::Stdio { command, args } = &delegate[0].spec else {
        panic!("the delegate server is a stdio server");
    };
    assert_eq!(Some(command.as_str()), engine.to_str());
    assert_eq!(
        args,
        &[
            tool::SERVER_COMMAND.to_owned(),
            runtime.tool_record(1).to_str().unwrap().to_owned()
        ]
    );
    let sub = runtime.context(&project, 2, AgentRole::Sub, true).unwrap();
    assert!(
        sub.plugins()
            .servers
            .iter()
            .all(|server| server.name != tool::SERVER_NAME)
    );

    let replacement = folder.join("replacement");
    engine_file(&replacement);
    std::fs::rename(&replacement, &engine).unwrap();
    let Err(swapped) = runtime.context(&project, 1, AgentRole::Master, true) else {
        panic!("a master context was built on a replaced engine");
    };
    assert!(
        swapped.contains("native_auth_host_executable_changed"),
        "{swapped}"
    );
    assert!(runtime.context(&project, 2, AgentRole::Sub, true).is_ok());
}

#[cfg(target_os = "macos")]
#[test]
fn a_team_master_never_gets_a_delegate_engine_inside_a_folder_it_can_write() {
    let fixture = Fixture::new(1);
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let account = TeamAccount {
        connection: reference(ENDPOINT),
        reach: TeamReach::Endpoint {
            base_url: "http://127.0.0.1:11434/v1".into(),
            model: "qwen2.5:0.5b".into(),
        },
    };
    let open = || {
        let access = fixture.access.clone();
        TeamRuntime::open(
            &fixture.base.join("engine.db"),
            &fixture.base.join("team"),
            BTreeMap::from([("codex", codex.clone())]),
            Box::new(move || Some(access.clone())),
            TeamLimits::default(),
        )
        .unwrap()
    };
    let runtime = open();
    assert!(
        runtime
            .prepare(
                &fixture.setup,
                1,
                AgentRole::Sub,
                "codex",
                true,
                &account,
                false
            )
            .is_ok()
    );
    drop(runtime);
    let objects = fixture.base.join("project").join(".git").join("objects");
    let engine = objects.join("crazyagent");
    engine_file(&engine);
    let runtime = open().with_delegation_tool(&engine).unwrap();
    let Err(refused) = runtime.prepare(
        &fixture.setup,
        2,
        AgentRole::Master,
        "codex",
        true,
        &account,
        false,
    ) else {
        panic!("a master was prepared with a delegate engine it can write");
    };
    assert!(refused.contains("team_delegate_writable"), "{refused}");
    assert!(
        runtime
            .prepare(
                &fixture.setup,
                3,
                AgentRole::Sub,
                "codex",
                true,
                &account,
                false
            )
            .is_ok()
    );
}

/// On Linux, which runs no agent confined to its own worktree in V1 (DEC-094), a team agent
/// stops before any sandbox or repository step, and its row carries the owner's plain line.
#[cfg(target_os = "linux")]
#[test]
fn a_team_agent_on_linux_stops_with_the_plain_unavailable_line() {
    let fixture = Fixture::new(1);
    let mut team = fixture.runtime();
    let Accounts::Usable(accounts) =
        eligible_accounts(&fixture.access, &fixture.setup.project_id, "codex")
    else {
        panic!("the endpoint fits codex");
    };
    let line = "Team agents and research are not available on Linux in this version.";
    let refused = team
        .spawn_run(
            &fixture.setup,
            AgentRole::Sub,
            "codex",
            false,
            &accounts[0],
            None,
        )
        .unwrap();
    assert_eq!(refused, Err(line.into()));
    let agents = fixture.status().agents;
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].status, AgentStatus::Failed);
    assert_eq!(agents[0].problem.as_deref(), Some(line));
    assert!(!fixture.base.join("project").join(".git").exists());
    assert!(team.shutdown().unwrap().is_empty());
}

/// A sub-agent edits a file in its worktree and the owner stops it before it finishes:
/// the engine commits the edit into that worktree's change set at the task's end, the
/// owner sees one pending change set for that sub-agent, and the team chat says so.
#[cfg(target_os = "macos")]
#[test]
fn a_sub_agent_task_that_edits_a_file_ends_with_a_pending_change_set_and_a_team_note() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(
        &codex,
        "#!/bin/sh\necho 'Written by the sub-agent.' > page.md\nexec /bin/sleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agents = Arc::new(Agents::open(&fixture.base).unwrap());
    let access = fixture.access.clone();
    let mut team = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_agents(agents.clone())
    .with_terminal(host.clone())
    .unwrap();
    let mut connection = Connection::open(fixture.base.join("engine.db")).unwrap();
    let tx = connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &crate::team::TeamAction::Delegate {
                project_id: fixture.setup.project_id.clone(),
                harness: Some("codex".into()),
                prompt: "Write page.md".into(),
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    fixture.until(&mut team, "the sub-agent works on the task", |f| {
        f.status()
            .agents
            .iter()
            .any(|agent| agent.status == AgentStatus::Working && agent.process_group.is_some())
    });
    let member = fixture.status().agents.remove(0);
    let worktree: String = fixture
        .connection
        .query_row(
            "SELECT worktree FROM team_agents WHERE id=?1",
            [member.id],
            |row| row.get(0),
        )
        .unwrap();
    let page = fixture.base.join("project").join(&worktree).join("page.md");
    let deadline = Instant::now() + WAIT;
    while !page.is_file() {
        assert!(
            Instant::now() < deadline,
            "the sub-agent never wrote its page"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let listed = agents
        .list(Some(&fixture.setup.project_id), 0, crate::agents::LIST_MAX)
        .unwrap();
    agents.stop(&listed[0].id).unwrap();
    fixture.until(&mut team, "the stopped sub-agent's task ends", |f| {
        f.agent_status(member.id).0 == AgentStatus::Stopped
    });
    let task: i64 = fixture
        .connection
        .query_row(
            "SELECT id FROM team_tasks WHERE prompt='Write page.md'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let waiting = format!(
        "The edits from task {task} (codex, sub-agent {}) are waiting for you in Changes.",
        member.id
    );
    assert!(fixture.note(&waiting), "{:#?}", fixture.status().messages);
    let landing = crate::landing::open(&fixture.base).unwrap();
    let sets = crate::landing::list(&landing, &fixture.setup.project_id).unwrap();
    assert_eq!(sets.len(), 1, "{sets:#?}");
    assert_eq!(sets[0].state, crate::landing::ChangeState::Pending);
    assert_eq!(sets[0].agent.kind, crate::landing::AgentKind::Sub);
    assert_eq!(sets[0].agent.id, member.id.to_string());
    assert_eq!(sets[0].files, 1);
    assert_ne!(sets[0].last_outcome, crate::landing::TurnOutcome::Succeeded);
    assert!(team.shutdown().unwrap().is_empty());
}

/// The owner asks a sub-agent that has stopped to update its change set: the resolve task
/// is pinned to that agent's worktree, so the same agent comes back in the same worktree
/// and works on it, and no other sub-agent or worktree is started for it.
#[cfg(target_os = "macos")]
#[test]
fn asking_a_sub_agent_to_update_runs_the_resolve_task_in_its_own_worktree() {
    let fixture = Fixture::new(2);
    let host = fixture.host();
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(
        &codex,
        "#!/bin/sh\necho run >> runs.txt\nexec /bin/sleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let agents = Arc::new(Agents::open(&fixture.base).unwrap());
    let access = fixture.access.clone();
    let mut team = TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_agents(agents.clone())
    .with_terminal(host.clone())
    .unwrap();
    let mut connection = Connection::open(fixture.base.join("engine.db")).unwrap();
    let tx = connection.transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &fixture.access,
            &crate::team::TeamAction::Delegate {
                project_id: fixture.setup.project_id.clone(),
                harness: Some("codex".into()),
                prompt: "Write page.md".into(),
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    fixture.until(&mut team, "the sub-agent works on the task", |f| {
        f.status()
            .agents
            .iter()
            .any(|agent| agent.status == AgentStatus::Working && agent.process_group.is_some())
    });
    let member = fixture.status().agents.remove(0);
    let worktree: String = fixture
        .connection
        .query_row(
            "SELECT worktree FROM team_agents WHERE id=?1",
            [member.id],
            |row| row.get(0),
        )
        .unwrap();
    let runs = fixture
        .base
        .join("project")
        .join(&worktree)
        .join("runs.txt");
    let ran = |times: usize| {
        let deadline = Instant::now() + WAIT;
        while std::fs::read_to_string(&runs)
            .unwrap_or_default()
            .lines()
            .count()
            < times
        {
            assert!(
                Instant::now() < deadline,
                "the sub-agent never ran {times} times"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    ran(1);
    let listed = agents
        .list(Some(&fixture.setup.project_id), 0, crate::agents::LIST_MAX)
        .unwrap();
    agents.stop(&listed[0].id).unwrap();
    fixture.until(&mut team, "the first task ends", |f| {
        f.agent_status(member.id).0 == AgentStatus::Stopped
    });

    let project = fixture.base.join("project");
    std::fs::write(project.join("OWNER.md"), "Written by the owner.\n").unwrap();
    for arguments in [&["add", "OWNER.md"][..], &["commit", "-qm", "Owner notes"]] {
        let status = crate::spawn_gate::spawn(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &fixture.base)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args([
                    "-c",
                    "user.name=Owner",
                    "-c",
                    "user.email=owner@crazyagent.invalid",
                ])
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(arguments)
                .current_dir(&project)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .wait()
        .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }
    let db = crate::landing::open(&fixture.base).unwrap();
    let sets = crate::landing::list(&db, &fixture.setup.project_id).unwrap();
    assert_eq!(sets.len(), 1, "{sets:?}");
    assert_eq!(sets[0].agent.id, member.id.to_string());
    crate::landing::update(&db, &sets[0].id).unwrap();
    crate::landing_turns::start_resolve(&db, &sets[0].id).unwrap();
    let prompt = crate::landing::resolve_task_prompt(&db, &sets[0].id).unwrap();
    let resolve: i64 = fixture
        .connection
        .query_row(
            "SELECT task FROM team_task_pins WHERE agent=?1",
            [member.id],
            |row| row.get(0),
        )
        .unwrap();
    fixture.until(
        &mut team,
        "the same sub-agent works on the resolve task in its recorded pane",
        |f| f.agent_status(member.id).0 == AgentStatus::Working && f.recorded(member.id),
    );
    ran(2);
    let (agent, status, sent): (Option<i64>, String, String) = fixture
        .connection
        .query_row(
            "SELECT agent,status,prompt FROM team_tasks WHERE id=?1",
            [resolve],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((agent, status.as_str()), (Some(member.id), "running"));
    assert_eq!(sent, prompt);
    assert!(
        prompt.contains("CrazyAgent merged it into your worktree"),
        "{prompt}"
    );
    assert!(
        project.join(&worktree).join("OWNER.md").is_file(),
        "the update merged the owner's commit into the sub-agent's worktree"
    );
    assert_eq!(
        count(
            &fixture.connection,
            "SELECT COUNT(*) FROM team_agents WHERE project=?1"
        ),
        1,
        "no other sub-agent was started for the resolve task"
    );
    let worktrees = fixture
        .base
        .join("project")
        .join(&worktree)
        .parent()
        .unwrap()
        .to_path_buf();
    assert_eq!(std::fs::read_dir(worktrees).unwrap().count(), 1);
    assert_eq!(
        team.shutdown().unwrap(),
        names(&[member.id]),
        "the working sub-agent's pane outlives the engine"
    );
}

const LIVE_CODEX: &str = r#"#!/bin/sh
d="$CODEX_HOME/sessions/2026/10/27"
/bin/mkdir -p "$d" || exit 1
f="$d/rollout-2026-10-27T00-00-00-$$.jsonl"
printf '{"type":"session_meta","payload":{"cwd":"%s"}}\n' "$(/bin/pwd -P)" > "$f"
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0 ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && continue ;;
  esac
  [ "$line" = "/exit" ] && exit 0
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  case "$line" in *"write page"*) echo 'Written by the live master.' > page.md ;; esac
  /bin/sleep 0.3
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
"#;

fn live_runtime(fixture: &Fixture, host: &Arc<dyn Host>, script: &str) -> TeamRuntime {
    let bin = fixture.base.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, script).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let access = fixture.access.clone();
    TeamRuntime::open(
        &fixture.base.join("engine.db"),
        &fixture.base.join("team"),
        BTreeMap::from([("codex", codex)]),
        Box::new(move || Some(access.clone())),
        TeamLimits::default(),
    )
    .unwrap()
    .with_terminal(host.clone())
    .unwrap()
}

fn masters(connection: &Connection) -> Vec<i64> {
    connection
        .prepare("SELECT id FROM team_agents WHERE project=?1 AND role='master' ORDER BY id")
        .unwrap()
        .query_map([PROJECT], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn turned_off(fixture: &Fixture, why: &str) -> bool {
    let said = |text: &str| {
        text.starts_with(&format!("The live master was turned off: {why}"))
            && text.ends_with(". Start it again from the master view.")
    };
    let status = fixture.status();
    !super::super::master::live(&fixture.connection, &fixture.setup.project_id).unwrap()
        && status.live_master == Some(false)
        && status.live_problem.as_deref().is_some_and(said)
        && status
            .messages
            .iter()
            .any(|message| message.state == MessageState::Note && said(&message.text))
}

fn idle_ticks(team: &mut TeamRuntime) {
    for _ in 0..20 {
        team.tick().unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A live master that ends on its own, as when the owner types /exit, turns its switch off
/// with one note instead of starting again every tick; turned on again, it starts under the
/// same agent row in the same worktree and home it left.
#[cfg(target_os = "macos")]
#[test]
fn a_live_master_that_ends_by_itself_turns_off_and_starts_again_in_its_own_worktree() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = live_runtime(&fixture, &host, LIVE_CODEX);
    let project = fixture.setup.project_id.clone();
    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "the live master's briefing turn ends", |f| {
        live_turns(&f.connection) == (1, false)
    });
    let master = masters(&fixture.connection)[0];
    let worktree = |f: &Fixture| -> Vec<String> {
        f.connection
            .prepare("SELECT worktree FROM team_worktrees WHERE project=?1")
            .unwrap()
            .query_map([PROJECT], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let first = worktree(&fixture);
    assert_eq!(first.len(), 1);

    host.input(&AgentId::team(master).unwrap(), b"/exit\r")
        .unwrap();
    fixture.until(&mut team, "the ended master turns its switch off", |f| {
        turned_off(f, "its terminal ended")
    });
    idle_ticks(&mut team);
    assert_eq!(
        masters(&fixture.connection),
        [master],
        "no master starts while off"
    );

    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "the master starts again and is briefed", |f| {
        live_turns(&f.connection) == (2, false)
    });
    assert_eq!(
        masters(&fixture.connection),
        [master],
        "the same master row starts again"
    );
    assert_eq!(worktree(&fixture), first, "in the same worktree");
    assert_eq!(host.alive().unwrap(), names(&[master]));
    assert_eq!(fixture.agent_status(master).0, AgentStatus::Idle);
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
    assert!(host.alive().unwrap().is_empty());
}

/// A live master that cannot start, first because its sandbox profile cannot be written and
/// then because its harness exits at once, turns its switch off after one try each time and
/// adds no agent rows while it is off.
#[cfg(target_os = "macos")]
#[test]
fn a_live_master_that_cannot_start_turns_off_after_one_try() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = live_runtime(&fixture, &host, "#!/bin/sh\nexit 3\n");
    let project = fixture.setup.project_id.clone();
    let blocked = fixture
        .base
        .join("team")
        .join("profiles")
        .join("agent-1.sb");
    std::fs::create_dir(&blocked).unwrap();

    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "a prepare failure turns the switch off", |f| {
        turned_off(f, "it could not start")
    });
    idle_ticks(&mut team);
    assert_eq!(
        masters(&fixture.connection),
        [1],
        "one try while it could not prepare"
    );
    assert_eq!(fixture.agent_status(1).0, AgentStatus::Failed);

    std::fs::remove_dir(&blocked).unwrap();
    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(
        &mut team,
        "a harness that exits at once turns the switch off",
        |f| turned_off(f, "its terminal ended"),
    );
    idle_ticks(&mut team);
    assert_eq!(
        masters(&fixture.connection).len(),
        2,
        "one more try, then none while off"
    );
    let notes = fixture
        .status()
        .messages
        .iter()
        .filter(|message| message.text.starts_with("The live master was turned off"))
        .count();
    assert_eq!(notes, 2, "one note for each time it turned off");
    team.shutdown().unwrap();
}

const RESUMING_CODEX: &str = r#"#!/bin/sh
d="$CODEX_HOME/sessions/2026/10/27"
/bin/mkdir -p "$d" || exit 1
printf '%s\n' "$*" >> "$CODEX_HOME/launches"
id=""
prev=""
for a in "$@"; do
  [ "$prev" = "resume" ] && id="$a"
  prev="$a"
done
if [ -n "$id" ]; then
  f=""
  for c in "$d"/rollout-*-"$id".jsonl; do [ -f "$c" ] && f="$c"; done
  [ -n "$f" ] || { echo "no conversation $id"; exit 4; }
else
  id=$(printf '%08x-0000-4000-8000-%012x' $$ $$)
  f="$d/rollout-2026-10-27T00-00-00-$id.jsonl"
  printf '{"type":"session_meta","payload":{"id":"%s","cwd":"%s"}}\n' "$id" "$(/bin/pwd -P)" > "$f"
fi
printf '\033[?2004hCodex stand-in ready\n'
e=$(printf '\033')
open=0
while IFS= read -r line; do
  case "$line" in
    *"$e[201~"*) open=0; printf '%s\n' "$line" >> "$CODEX_HOME/typed" ;;
    *"$e[200~"*) open=1; continue ;;
    *) [ "$open" = 1 ] && { printf '%s\n' "$line" >> "$CODEX_HOME/typed"; continue; } ;;
  esac
  printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >> "$f"
  /bin/sleep 0.3
  printf '{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done."}]}}\n' >> "$f"
  printf '{"type":"event_msg","payload":{"type":"task_complete"}}\n' >> "$f"
done
"#;

/// Makes `to` the project's live master session, parking the live one first, as the
/// owner's New session and Resume actions do. Takes the database and the session id.
fn switch_session(connection: &Connection, to: &str) {
    connection
        .execute(
            "UPDATE master_sessions SET phase='parked' WHERE project=?1 AND phase='configured'",
            [PROJECT],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO master_sessions(id,project,created,version,phase,body)
             VALUES (?1,?2,(SELECT COALESCE(MAX(created),0)+1 FROM master_sessions),
                     (SELECT COALESCE(MAX(created),0)+1 FROM master_sessions),'configured','{}')
             ON CONFLICT(id) DO UPDATE SET phase='configured'",
            params![to, PROJECT],
        )
        .unwrap();
}

fn kept_conversation(connection: &Connection, session: &str) -> Option<(String, PathBuf)> {
    connection
        .query_row(
            "SELECT conversation,record FROM team_master_conversations WHERE session=?1",
            [session],
            |row| Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?))),
        )
        .optional()
        .unwrap()
}

fn launches(fixture: &Fixture, master: i64) -> Vec<String> {
    let log = fixture
        .base
        .join("team/agents")
        .join(format!("agent-{master}"))
        .join("home/launches");
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn noted(fixture: &Fixture, text: &str) -> usize {
    fixture
        .status()
        .messages
        .iter()
        .filter(|message| message.state == MessageState::Note && message.text == text)
        .count()
}

/// Each master session keeps its own harness conversation: switching sessions restarts the
/// project's one live master in its worktree, a session it ran before continues by the
/// stored conversation id and reads only the turns added after the resume, and a session
/// whose conversation cannot continue, kept on another account or with its record gone,
/// starts a fresh harness conversation seeded with what could be read, with a note saying so.
#[cfg(target_os = "macos")]
#[test]
fn a_resumed_session_continues_its_own_harness_conversation_or_says_why_not() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = live_runtime(&fixture, &host, RESUMING_CODEX);
    let project = fixture.setup.project_id.clone();
    switch_session(&fixture.connection, "s-one");
    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(
        &mut team,
        "the first session's master is briefed and kept",
        |f| {
            live_turns(&f.connection) == (1, false)
                && kept_conversation(&f.connection, "s-one").is_some()
        },
    );
    let master = masters(&fixture.connection)[0];
    let (first, first_record) = kept_conversation(&fixture.connection, "s-one").unwrap();
    assert!(crate::harnesses::interactive::resumable(&first), "{first}");
    host.input(&AgentId::team(master).unwrap(), b"hello\r")
        .unwrap();
    fixture.until(
        &mut team,
        "the owner's turn in the first session ends",
        |f| live_turns(&f.connection) == (2, false),
    );

    switch_session(&fixture.connection, "s-two");
    fixture.until(
        &mut team,
        "a new session starts its own conversation",
        |f| {
            live_turns(&f.connection) == (3, false)
                && kept_conversation(&f.connection, "s-two").is_some()
        },
    );
    let (second, _) = kept_conversation(&fixture.connection, "s-two").unwrap();
    assert_ne!(second, first);
    assert_eq!(launches(&fixture, master).len(), 2);
    assert!(
        launches(&fixture, master)
            .iter()
            .all(|line| !line.contains("resume"))
    );

    switch_session(&fixture.connection, "s-one");
    fixture.until(&mut team, "the first session's master starts again", |f| {
        launches(f, master).len() == 3 && host.alive().unwrap() == names(&[master])
    });
    assert!(
        launches(&fixture, master)[2].ends_with(&format!(" resume {first}")),
        "{:?}",
        launches(&fixture, master)
    );
    idle_ticks(&mut team);
    assert_eq!(
        live_turns(&fixture.connection),
        (3, false),
        "no briefing turn on resume"
    );
    host.input(&AgentId::team(master).unwrap(), b"again\r")
        .unwrap();
    fixture.until(&mut team, "the resumed conversation's new turn ends", |f| {
        live_turns(&f.connection) == (4, false)
    });
    idle_ticks(&mut team);
    assert_eq!(
        live_turns(&fixture.connection).0,
        4,
        "earlier turns are not read again"
    );
    assert_eq!(
        kept_conversation(&fixture.connection, "s-one"),
        Some((first.clone(), first_record.clone()))
    );
    assert_eq!(
        masters(&fixture.connection),
        [master],
        "one master serves every session"
    );

    fixture
        .connection
        .execute(
            "UPDATE team_master_conversations SET connection='another-account' WHERE session='s-two'",
            [],
        )
        .unwrap();
    switch_session(&fixture.connection, "s-two");
    fixture.until(
        &mut team,
        "a session kept on another account starts fresh",
        |f| {
            noted(f, crate::master_sessions::FRESH_NOTE) == 1
                && kept_conversation(&f.connection, "s-two").is_some_and(|(id, _)| id != second)
                && live_turns(&f.connection) == (5, false)
        },
    );
    assert!(!launches(&fixture, master)[3].contains("resume"));
    let typed = std::fs::read_to_string(
        fixture
            .base
            .join("team/agents")
            .join(format!("agent-{master}"))
            .join("home/typed"),
    )
    .unwrap();
    assert!(
        typed.contains("Your earlier harness conversation in this session"),
        "{typed}"
    );
    assert!(typed.contains("You: Done."), "{typed}");

    std::fs::remove_file(&first_record).unwrap();
    switch_session(&fixture.connection, "s-one");
    fixture.until(
        &mut team,
        "a session whose record is gone starts fresh",
        |f| {
            noted(f, crate::master_sessions::LOST_NOTE) == 1
                && live_turns(&f.connection) == (6, false)
        },
    );
    assert!(!launches(&fixture, master)[4].contains("resume"));
    assert_eq!(masters(&fixture.connection), [master]);
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

/// A session whose kept rollout the harness reader refuses at a resume, here because a
/// tools-on master rewrote its first line to name another working folder, is not resumed
/// again: that start ends without turning the live switch off, and the next start begins a
/// fresh conversation with the note that the earlier one could not be read.
#[cfg(target_os = "macos")]
#[test]
fn a_resume_the_reader_refuses_starts_fresh_with_the_lost_note() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let mut team = live_runtime(&fixture, &host, RESUMING_CODEX);
    let project = fixture.setup.project_id.clone();
    switch_session(&fixture.connection, "s-one");
    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(
        &mut team,
        "the first session's master is briefed and kept",
        |f| {
            live_turns(&f.connection) == (1, false)
                && kept_conversation(&f.connection, "s-one").is_some()
        },
    );
    let master = masters(&fixture.connection)[0];
    let (first, record) = kept_conversation(&fixture.connection, "s-one").unwrap();
    switch_session(&fixture.connection, "s-two");
    fixture.until(&mut team, "the second session's master is briefed", |f| {
        live_turns(&f.connection) == (2, false)
            && kept_conversation(&f.connection, "s-two").is_some()
    });

    let text = std::fs::read_to_string(&record).unwrap();
    let (_, rest) = text.split_once('\n').unwrap();
    std::fs::write(
        &record,
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{first}\",\"cwd\":\"/elsewhere\"}}}}\n{rest}"
        ),
    )
    .unwrap();
    switch_session(&fixture.connection, "s-one");
    fixture.until(
        &mut team,
        "the refused resume starts fresh with the lost note",
        |f| {
            noted(f, crate::master_sessions::LOST_NOTE) == 1
                && live_turns(&f.connection) == (3, false)
                && kept_conversation(&f.connection, "s-one").is_some_and(|(id, _)| {
                    id != first && crate::harnesses::interactive::resumable(&id)
                })
        },
    );
    assert!(super::super::master::live(&fixture.connection, &project).unwrap());
    assert!(
        !fixture
            .status()
            .messages
            .iter()
            .any(|message| message.text.starts_with("The live master was turned off")),
        "a refused resume does not turn the live master off"
    );
    let launched = launches(&fixture, master);
    assert_eq!(
        launched.len(),
        3,
        "the refused resume never started the harness"
    );
    assert!(
        launched.iter().all(|line| !line.contains("resume")),
        "{launched:?}"
    );
    assert_eq!(masters(&fixture.connection), [master]);
    team.shutdown().unwrap();
    host.close(&[]).unwrap();
}

fn live_turns(connection: &Connection) -> (i64, bool) {
    connection
        .query_row(
            "SELECT turns,turn_open FROM team_live_masters WHERE project=?1",
            [PROJECT],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
}

/// The live master runs as its harness's own app in a kept pane: the engine types the
/// briefing, every turn counts and lands, the owner's typed turns included, engine input
/// waits for an idle master, and the next engine takes the same pane over.
#[cfg(target_os = "macos")]
#[test]
fn a_live_master_counts_and_lands_every_turn_and_a_restarted_engine_keeps_its_pane() {
    let fixture = Fixture::new(1);
    let host = fixture.host();
    let bin = fixture.base.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(&codex, LIVE_CODEX).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
    let open = || {
        let access = fixture.access.clone();
        TeamRuntime::open(
            &fixture.base.join("engine.db"),
            &fixture.base.join("team"),
            BTreeMap::from([("codex", codex.clone())]),
            Box::new(move || Some(access.clone())),
            TeamLimits::default(),
        )
        .unwrap()
        .with_terminal(host.clone())
        .unwrap()
    };
    let project = fixture.setup.project_id.clone();
    let mut team = open();
    team.tick().unwrap();
    assert_eq!(
        count(
            &fixture.connection,
            "SELECT COUNT(*) FROM team_agents WHERE project=?1"
        ),
        0,
        "no live master starts before the owner turns it on"
    );
    super::super::master::set_live(&fixture.connection, &project, true).unwrap();
    fixture.until(&mut team, "the live master's briefing turn ends", |f| {
        live_turns(&f.connection) == (1, false)
    });
    let master: i64 = fixture
        .connection
        .query_row(
            "SELECT id FROM team_agents WHERE project=?1 AND role='master'",
            [PROJECT],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fixture.agent_status(master).0, AgentStatus::Idle);
    assert_eq!(host.alive().unwrap(), names(&[master]));

    host.input(&AgentId::team(master).unwrap(), b"write page\r")
        .unwrap();
    fixture.until(&mut team, "the owner's own turn is counted", |f| {
        live_turns(&f.connection) == (2, false)
    });
    let landing = crate::landing::open(&fixture.base).unwrap();
    let sets = crate::landing::list(&landing, &project).unwrap();
    assert_eq!(sets.len(), 1, "{sets:#?}");
    assert_eq!(sets[0].agent.kind, crate::landing::AgentKind::Sub);
    assert_eq!(sets[0].agent.id, master.to_string());
    assert_eq!(sets[0].agent.label, "Team master (codex)");
    assert_eq!(sets[0].state, crate::landing::ChangeState::Pending);
    assert_eq!(sets[0].files, 1);

    let input = Connection::open(fixture.base.join("engine.db")).unwrap();
    super::super::master::master_input(
        &input,
        host.as_ref(),
        None,
        &project,
        "[owner] status please",
    )
    .unwrap();
    assert_eq!(
        super::super::master::master_input(&input, host.as_ref(), None, &project, "[owner] again"),
        Err(super::super::master::InputRefused::Busy),
        "engine input waits while its last input settles"
    );
    fixture.until(&mut team, "the engine's input turn is counted", |f| {
        live_turns(&f.connection) == (3, false)
    });

    assert_eq!(team.shutdown().unwrap(), names(&[master]));
    assert_eq!(host.alive().unwrap(), names(&[master]));
    let mut again = open();
    fixture.until(
        &mut again,
        "the next engine takes the live master over",
        |f| f.agent_status(master) == (AgentStatus::Idle, None) && f.recorded(master),
    );
    host.input(&AgentId::team(master).unwrap(), b"after the restart\r")
        .unwrap();
    fixture.until(&mut again, "a turn after the restart is counted", |f| {
        live_turns(&f.connection) == (4, false)
    });
    let turns: Vec<(i64, String)> = fixture
        .connection
        .prepare("SELECT turn,outcome FROM team_master_turns WHERE agent=?1 ORDER BY turn")
        .unwrap()
        .query_map([master], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        turns.iter().map(|turn| turn.0).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert_eq!(
        count(
            &fixture.connection,
            "SELECT COUNT(*) FROM team_agents WHERE project=?1"
        ),
        1,
        "the restart started no second master"
    );
    assert_eq!(again.shutdown().unwrap(), names(&[master]));
    host.close(&[]).unwrap();
    assert!(host.alive().unwrap().is_empty());
}
