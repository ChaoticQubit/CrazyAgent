use super::*;
use crate::harnesses::interactive::{InteractiveCommand, Transcript};
use crate::project_setup::{HarnessSlot, TeamPlan};
use crate::terminal::tmux::{SOCKET, Tmux, installed};
use crate::terminal::{AgentId, FOLDER};
use rusqlite::params;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PROJECT: &str = "ide-agents";
const OTHER: &str = "ide-other";
const READ_ONE: &str = "IFS= read -r line\nprintf 'got:%s\\n' \"$line\" > \"$1\"\nexec sleep 60\n";

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    tmux: PathBuf,
    connection: Connection,
    setup: StoredSetup,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-ide-agents-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = root.path().canonicalize().unwrap();
        let tmux = installed(&base).expect("tmux must be installed on this machine");
        let connection = Connection::open(base.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&connection).unwrap();
        let folder = base.join("project");
        std::fs::create_dir(&folder).unwrap();
        let setup = StoredSetup {
            project_id: ProjectId::new(PROJECT).unwrap(),
            name: "IDE agents".into(),
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
                    agents: 3,
                    tools: false,
                }],
                min_agents: 1,
                max_agents: 4,
            },
            documents: vec![],
            folder: folder.to_str().unwrap().into(),
            created_ms: 1,
            revision: 1,
        };
        Self {
            _root: root,
            base,
            tmux,
            connection,
            setup,
        }
    }

    fn agent(&self, project: &str, role: &str, status: &str, worktree: Option<&str>) -> i64 {
        self.connection
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,connection,worktree,status,created_ms,heartbeat_ms)
                 VALUES (?1,?2,'codex',0,'local-a',?3,?4,1,1)",
                params![project, role, worktree, status],
            )
            .unwrap();
        self.connection.last_insert_rowid()
    }

    fn host(&self) -> Arc<dyn Host> {
        Arc::new(Tmux::open(Some(self.tmux.clone()), &self.base.join(FOLDER)))
    }

    fn pane(&self, host: &Arc<dyn Host>, agent: i64, out: &Path) {
        let script = self.base.join("read-one.sh");
        std::fs::write(&script, READ_ONE).unwrap();
        let command = InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                script.to_str().unwrap().into(),
                out.to_str().unwrap().into(),
            ],
            cwd: self.base.clone(),
            env: Vec::new(),
            transcript: Transcript::Unavailable,
        };
        host.spawn(&AgentId::team(agent).unwrap(), &command)
            .unwrap();
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

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The sidebar's agents are the team runtime's own list for the project, field for field,
/// each worktree resolved inside the project folder; a recorded worktree that would leave
/// the folder is not offered, and only a running sub-agent can be messaged.
#[test]
fn ide_agents_list_matches_team_runtime() {
    let fixture = Fixture::new();
    let master = fixture.agent(
        PROJECT,
        "master",
        "idle",
        Some(".crazyagent/worktrees/agent-1"),
    );
    let working = fixture.agent(
        PROJECT,
        "sub",
        "working",
        Some(".crazyagent/worktrees/agent-2"),
    );
    let stopped = fixture.agent(PROJECT, "sub", "stopped", None);
    let escaping = fixture.agent(PROJECT, "sub", "idle", Some("../outside"));
    fixture.agent(
        OTHER,
        "sub",
        "working",
        Some(".crazyagent/worktrees/agent-5"),
    );
    fixture
        .connection
        .execute(
            "INSERT INTO team_names(agent,project,persona,handle) VALUES (?1,?2,'Ada','page-fix')",
            params![working, PROJECT],
        )
        .unwrap();

    let listed = agents(&fixture.connection, &fixture.setup).unwrap();
    let team = crate::team::status(&fixture.connection, &fixture.setup)
        .unwrap()
        .agents;
    assert_eq!(listed.len(), team.len());
    assert_eq!(
        listed.iter().map(|agent| agent.id).collect::<Vec<_>>(),
        [master, working, stopped, escaping]
    );
    for (shown, runtime) in listed.iter().zip(&team) {
        assert_eq!(shown.id, runtime.id);
        assert_eq!(shown.label, runtime.label);
        assert_eq!(shown.role, runtime.role);
        assert_eq!(shown.harness, runtime.harness);
        assert_eq!(shown.status, runtime.status);
        assert_eq!(shown.status_label, runtime.status_label);
    }
    let folder = fixture.base.join("project");
    let worktree = |id: i64| {
        listed
            .iter()
            .find(|agent| agent.id == id)
            .unwrap()
            .worktree
            .clone()
    };
    assert_eq!(
        worktree(master),
        Some(
            folder
                .join(".crazyagent/worktrees/agent-1")
                .to_str()
                .unwrap()
                .to_owned()
        )
    );
    assert_eq!(
        worktree(working),
        Some(
            folder
                .join(".crazyagent/worktrees/agent-2")
                .to_str()
                .unwrap()
                .to_owned()
        )
    );
    assert_eq!(worktree(stopped), None);
    assert_eq!(
        worktree(escaping),
        None,
        "a worktree outside the project is not offered"
    );
    let messageable: Vec<i64> = listed
        .iter()
        .filter(|agent| agent.message)
        .map(|agent| agent.id)
        .collect();
    assert_eq!(messageable, [working, escaping]);
    assert!(listed[1].label.contains("Ada"), "{}", listed[1].label);
}

/// The sidebar's message action types the owner's text into the project's running
/// sub-agent and notes it for the master's next turn; a master, a stopped agent, another
/// project's agent and blank text are refused with the team's codes and nothing is noted.
#[test]
fn ide_agent_message_types_into_a_running_sub_agent_and_notes_the_master() {
    let fixture = Fixture::new();
    let host = fixture.host();
    let master = fixture.agent(PROJECT, "master", "idle", None);
    let sub = fixture.agent(PROJECT, "sub", "working", None);
    let stopped = fixture.agent(PROJECT, "sub", "stopped", None);
    let foreign = fixture.agent(OTHER, "sub", "working", None);
    let got = fixture.base.join("got.txt");
    fixture.pane(&host, sub, &got);
    let project = &fixture.setup.project_id;
    let send = |agent: i64, text: &str| {
        message(
            &fixture.connection,
            host.as_ref(),
            project,
            agent,
            text,
            1_000,
        )
        .unwrap()
    };
    let noted = || -> Vec<(i64, String)> {
        fixture
            .connection
            .prepare("SELECT agent,text FROM team_owner_prompts ORDER BY id")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };

    assert_eq!(send(master, "Check the page"), Err("team_agent_not_found"));
    assert_eq!(
        send(stopped, "Check the page"),
        Err("team_agent_not_running")
    );
    assert_eq!(send(foreign, "Check the page"), Err("team_agent_not_found"));
    assert_eq!(send(sub, "   "), Err("team_message_invalid"));
    assert!(noted().is_empty(), "a refused message is not noted");

    assert_eq!(send(sub, "Check the page"), Ok(sub));
    until("the typed message", || {
        std::fs::read_to_string(&got).is_ok_and(|text| text == "got:Check the page\n")
    });
    assert_eq!(noted(), [(sub, "Check the page".to_owned())]);
}
