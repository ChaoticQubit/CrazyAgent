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
    team.finish_message(&project, waiting, "Noted.", TurnEnd::Success, vec![])
        .unwrap();
    team.finish_message(
        &project,
        latest,
        "Here is the plan.\nDELEGATE: Write the landing page",
        TurnEnd::Success,
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
    team.finish_task(task, "page written", TurnEnd::Success)
        .unwrap();
    team.finish_task(task, "", TurnEnd::Closed).unwrap();
    team.finish_message(&project, message, "", TurnEnd::Interrupted, vec![])
        .unwrap();
    team.finish_message(&project, message, "Done", TurnEnd::Success, vec![])
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
            .prepare(&fixture.setup, id, AgentRole::Sub, "codex", true, &account)
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
