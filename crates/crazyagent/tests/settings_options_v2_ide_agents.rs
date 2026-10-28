#![cfg(unix)]

mod e2e_support;

use crazyagent::engine::owner::OwnerResult;
use crazyagent::ide::workspace::{self, AGENTS_SHOWN_MAX};
use crazyagent::protocol::ProjectId;
use e2e_support::{Engine, Owner, git, repository, scratch};
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PROJECT: &str = "v2-ide-agents";
const AGENTS: usize = AGENTS_SHOWN_MAX + 2;

/// The running engine with one project set up through the owner protocol and a real
/// repository in the folder the engine recorded for it.
struct Team {
    engine: Engine,
    project: ProjectId,
    folder: PathBuf,
    created_ms: i64,
    _disk: tempfile::TempDir,
}

impl Team {
    /// Starts the engine, picks a projects home and creates the project, then makes its
    /// folder a git repository. Input: a scratch prefix. Output: the case.
    async fn start(prefix: &str) -> Self {
        let engine = Engine::start(prefix);
        let mut owner = Owner::attach(&engine).await;
        let disk = scratch("ca-v2opt-agents-home-");
        let home = disk.path().join("Projects");
        fs::create_dir(&home).unwrap();
        assert_eq!(owner.set_home(&home).await, OwnerResult::Applied);
        assert_eq!(
            owner.create_project(PROJECT, "Buoy agents").await,
            OwnerResult::Applied
        );
        let project = ProjectId::new(PROJECT).unwrap();
        let folder = workspace::folder(engine.path(), &project).unwrap();
        if folder.join(".git").exists() {
            fs::write(folder.join("README.md"), "buoys\n").unwrap();
            git(&folder, &["add", "README.md"]);
            git(&folder, &["commit", "--quiet", "-m", "first"]);
        } else {
            repository(&folder, "README.md", "buoys\n");
        }
        Self {
            engine,
            project,
            folder,
            created_ms: 1_000,
            _disk: disk,
        }
    }

    /// Adds one working sub-agent with a real git worktree in the project's agents folder,
    /// recorded in the running engine's database as the team runtime records a start.
    /// Output: the worktree.
    fn agent(&mut self) -> PathBuf {
        self.created_ms += 1;
        let db = Connection::open(self.engine.path().join("engine.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (?1,'sub','codex',0,'working',?2,?2)",
            params![PROJECT, self.created_ms],
        )
        .unwrap();
        let id = db.last_insert_rowid();
        let relative = format!("worktrees/agent-{id}");
        let worktree = crazyagent::projects_home::state_folder(&self.folder).join(&relative);
        git(
            &self.folder,
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let git_dir = self
            .folder
            .join(".git")
            .join("worktrees")
            .join(format!("agent-{id}"));
        assert!(git_dir.is_dir());
        db.execute(
            "UPDATE team_agents SET worktree=?2 WHERE id=?1",
            params![id, relative],
        )
        .unwrap();
        db.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                id,
                PROJECT,
                self.folder.to_str().unwrap(),
                worktree.to_str().unwrap(),
                git_dir.to_str().unwrap()
            ],
        )
        .unwrap();
        let landing = crazyagent::landing::open(self.engine.path()).unwrap();
        crazyagent::landing::record_worktree_start(&landing, &self.folder, &worktree, &git_dir)
            .unwrap();
        worktree
    }
}

/// The folders the IDE opens, in order, from the workspace file the engine wrote.
/// Input: the file. Output: each folder's path.
fn opened(file: &Path) -> Vec<PathBuf> {
    let text = fs::read_to_string(file).unwrap();
    let workspace: serde_json::Value = serde_json::from_str(&text).unwrap();
    workspace["folders"]
        .as_array()
        .expect("the workspace lists its folders")
        .iter()
        .map(|folder| PathBuf::from(folder["path"].as_str().unwrap()))
        .collect()
}

/// The folders a workspace must open: the project, then the given agents newest first.
fn expected(folder: &Path, agents: &[PathBuf]) -> Vec<PathBuf> {
    std::iter::once(folder.to_path_buf())
        .chain(agents.iter().rev().cloned())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_agents_newest_12_opens_the_project_and_the_newest_twelve_agents() {
    let mut team = Team::start("ca-v2opt-agents-12-").await;
    let agents: Vec<PathBuf> = (0..AGENTS).map(|_| team.agent()).collect();

    let written = workspace::write(team.engine.path(), &team.project).unwrap();
    assert_eq!((written.hidden, written.show_all), (2, false));
    assert_eq!(
        opened(&written.path),
        expected(&team.folder, &agents[2..]),
        "the default opens the newest twelve agents and hides the two oldest"
    );

    let newest = team.agent();
    let written = workspace::write(team.engine.path(), &team.project).unwrap();
    assert_eq!(written.hidden, 3);
    let mut shown = agents[3..].to_vec();
    shown.push(newest);
    assert_eq!(opened(&written.path), expected(&team.folder, &shown));

    team.engine.restart();
    let written = workspace::write(team.engine.path(), &team.project).unwrap();
    assert_eq!(
        (written.hidden, written.show_all),
        (3, false),
        "the cap stays after a restart"
    );
    assert_eq!(
        opened(&written.path).len(),
        1 + AGENTS_SHOWN_MAX,
        "the project and twelve agents"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ide_agents_show_all_opens_every_agent_and_survives_a_restart() {
    let mut team = Team::start("ca-v2opt-agents-all-").await;
    let agents: Vec<PathBuf> = (0..AGENTS).map(|_| team.agent()).collect();

    let written = workspace::show_all(team.engine.path(), &team.project, true).unwrap();
    assert_eq!((written.hidden, written.show_all), (0, true));
    assert_eq!(
        opened(&written.path),
        expected(&team.folder, &agents),
        "Show all opens every working agent"
    );

    team.engine.restart();
    let written = workspace::write(team.engine.path(), &team.project).unwrap();
    assert_eq!(
        (written.hidden, written.show_all),
        (0, true),
        "Show all is kept after a restart"
    );
    assert_eq!(opened(&written.path), expected(&team.folder, &agents));

    let written = workspace::show_all(team.engine.path(), &team.project, false).unwrap();
    assert_eq!((written.hidden, written.show_all), (2, false));
    assert_eq!(opened(&written.path), expected(&team.folder, &agents[2..]));
}
