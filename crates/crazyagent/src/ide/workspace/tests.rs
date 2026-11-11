use super::*;
use crate::landing::{AgentKind, ChangeAgent, TurnEnd, TurnOutcome};
use crate::project_setup::{StoredSetup, TeamPlan};
use crate::protocol::{Outcome, ProjectSpec};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};

const PROJECT: &str = "alpha";

/// Records a project with a stored setup whose folder is `folder`, the rows a project
/// created through setup has, the project row through the engine's own create path. The
/// write takes the write lock at its start, so beside an engine still writing it waits its
/// turn; a lock taken only at the first write fails at once when another writer got in first.
/// Inputs: the engine database, the project id and its folder.
pub(crate) fn add_project(db: &Connection, project: &str, folder: &Path) {
    let setup = StoredSetup {
        project_id: ProjectId::new(project).unwrap(),
        name: project.into(),
        about: String::new(),
        tech_stack: vec![],
        platforms: vec![],
        research: vec![],
        pool: None,
        team: TeamPlan {
            master: "codex".into(),
            master_tools: false,
            sub_harnesses: vec![],
            min_agents: 1,
            max_agents: 16,
        },
        documents: vec![],
        folder: folder.to_str().unwrap().into(),
        created_ms: 1,
        revision: 1,
    };
    let spec = ProjectSpec {
        name: setup.name.clone(),
        repositories: vec![],
    };
    let tx =
        rusqlite::Transaction::new_unchecked(db, rusqlite::TransactionBehavior::Immediate).unwrap();
    assert_eq!(
        crate::storage::create_project(&tx, &setup.project_id, &spec).unwrap(),
        Outcome::Applied
    );
    tx.execute(
        "INSERT INTO project_setups VALUES (?1,?2)",
        params![project, serde_json::to_string(&setup).unwrap()],
    )
    .unwrap();
    tx.commit().unwrap();
}

/// Runs git in `dir` with no global or system configuration. Inputs: the folder and the
/// arguments. Output: standard output.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// One team agent the case added: its id, worktree and git metadata folder.
pub(crate) struct Agent {
    pub(crate) id: i64,
    pub(crate) worktree: PathBuf,
    pub(crate) git_dir: PathBuf,
}

/// A real engine database beside a real project repository, whose team agents each get a
/// real git worktree in the project's `.crazyagent/worktrees/` folder, as the team runtime
/// adds them.
pub(crate) struct Case {
    _tmp: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) folder: PathBuf,
    pub(crate) db: Connection,
    pub(crate) project: ProjectId,
    created_ms: i64,
}

impl Case {
    pub(crate) fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("ca-ide-sections-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let root = base.join("engine");
        let folder = base.join("project");
        for path in [&root, &folder] {
            std::fs::create_dir(path).unwrap();
        }
        let db = Connection::open(root.join("engine.db")).unwrap();
        crate::storage::initialize_authority_schemas(&db).unwrap();
        add_project(&db, PROJECT, &folder);
        git(&folder, &["init", "-q", "--template=", "-b", "main"]);
        git(&folder, &["config", "user.name", "Repo Owner"]);
        git(&folder, &["config", "user.email", "owner@example.invalid"]);
        std::fs::write(folder.join("README"), "one\n").unwrap();
        git(&folder, &["add", "README"]);
        git(&folder, &["commit", "-q", "-m", "Start"]);
        std::fs::create_dir(folder.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        Self {
            _tmp: tmp,
            root,
            folder,
            db,
            project: ProjectId::new(PROJECT).unwrap(),
            created_ms: 1_000,
        }
    }

    /// Adds a team agent with a real worktree and records where it starts, as a launch
    /// does. Inputs: the role and status. Output: the agent.
    pub(crate) fn agent(&mut self, role: &str, status: &str) -> Agent {
        self.created_ms += 1;
        self.db
            .execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
                 VALUES (?1,?2,'codex',0,?3,?4,?4)",
                params![PROJECT, role, status, self.created_ms],
            )
            .unwrap();
        let id = self.db.last_insert_rowid();
        let relative = format!("worktrees/agent-{id}");
        let worktree = crate::projects_home::state_folder(&self.folder).join(&relative);
        git(
            &self.folder,
            &[
                "worktree",
                "add",
                "-q",
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
        self.db
            .execute(
                "UPDATE team_agents SET worktree=?2 WHERE id=?1",
                params![id, relative],
            )
            .unwrap();
        self.db
            .execute(
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
        let landing = crate::landing::open(&self.root).unwrap();
        crate::landing::record_worktree_start(&landing, &self.folder, &worktree, &git_dir).unwrap();
        Agent {
            id,
            worktree,
            git_dir,
        }
    }

    pub(crate) fn status(&self, agent: &Agent, status: &str) {
        let changed = self
            .db
            .execute(
                "UPDATE team_agents SET status=?2 WHERE id=?1",
                params![agent.id, status],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    /// Ends a turn in which the agent wrote a file, which opens its change set the way the
    /// team runtime records a finished turn. Input: the agent. Output: the change set.
    pub(crate) fn change(&self, agent: &Agent) -> crate::landing::ChangeSetId {
        std::fs::write(agent.worktree.join("notes.txt"), "made by the agent\n").unwrap();
        let landing = crate::landing::open(&self.root).unwrap();
        let workspace = crate::landing::Workspace {
            project: self.project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: agent.id.to_string(),
                label: format!("Sub-agent {}", agent.id),
            },
            repository: self.folder.clone(),
            worktree: agent.worktree.clone(),
            git_dir: agent.git_dir.clone(),
        };
        let end = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("task-{}", agent.id),
            prompt_line: "Write the notes".into(),
        };
        match crate::landing_turns::record(&landing, &workspace, &end, None).unwrap() {
            crate::landing_turns::Landed::Added(id) => id,
            _ => panic!("the turn opened no change set"),
        }
    }

    pub(crate) fn file(&self) -> PathBuf {
        self.root.join("ide").join(PROJECT).join(FILE)
    }

    /// The written workspace file's folders as (name, path) pairs, in file order.
    pub(crate) fn folders(&self) -> Vec<(String, PathBuf)> {
        let text = std::fs::read_to_string(self.file()).unwrap();
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        body["folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|folder| {
                (
                    folder["name"].as_str().unwrap().to_owned(),
                    PathBuf::from(folder["path"].as_str().unwrap()),
                )
            })
            .collect()
    }

    fn names(&self) -> Vec<String> {
        self.folders().into_iter().map(|(name, _)| name).collect()
    }
}

fn sub(agent: &Agent) -> String {
    format!("Sub-agent {}", agent.id)
}

#[test]
fn workspace_lists_project_master_and_active_agents_newest_first_capped_at_12() {
    let mut case = Case::new();
    let master = case.agent("master", "idle");
    let subs: Vec<Agent> = (0..14).map(|_| case.agent("sub", "working")).collect();
    let written = write(&case.root, &case.project).unwrap();
    assert_eq!(
        written,
        Written {
            path: case.file(),
            hidden: 2,
            show_all: false,
        }
    );
    let file = std::fs::symlink_metadata(case.file()).unwrap();
    assert!(file.is_file());
    assert_eq!(file.mode() & 0o7777, 0o600);
    let folder = std::fs::symlink_metadata(case.root.join("ide").join(PROJECT)).unwrap();
    assert_eq!(folder.mode() & 0o7777, 0o700);
    let mut expected = vec![
        ("Project".to_owned(), case.folder.clone()),
        ("master".to_owned(), master.worktree.clone()),
    ];
    expected.extend(
        subs.iter()
            .rev()
            .take(AGENTS_SHOWN_MAX)
            .map(|agent| (sub(agent), agent.worktree.clone())),
    );
    assert_eq!(case.folders(), expected);
    assert!(!case.names().contains(&sub(&subs[0])));
    assert!(!case.names().contains(&sub(&subs[1])));
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(case.file()).unwrap()).unwrap();
    let running: serde_json::Map<String, serde_json::Value> = subs
        .iter()
        .map(|agent| {
            (
                format!("{}/**", agent.worktree.display()),
                serde_json::Value::Bool(true),
            )
        })
        .collect();
    assert_eq!(
        body["settings"],
        serde_json::json!({
            "chat.disableAIFeatures": true,
            "chat.mcp.access": "none",
            "chat.extensionTools.enabled": false,
            "workbench.settings.showAISearchToggle": false,
            "extensions.ignoreRecommendations": true,
            "workbench.startupEditor": "none",
            "git.detectSubmodules": false,
            "git.autoRepositoryDetection": false,
            "files.readonlyInclude": running,
            "files.readonlyExclude": {},
        }),
        "the workspace file carries the lock's and git's workspace keys and the running agents' globs"
    );
}

#[test]
fn show_all_agents_adds_the_rest() {
    let mut case = Case::new();
    let subs: Vec<Agent> = (0..14).map(|_| case.agent("sub", "working")).collect();
    assert_eq!(write(&case.root, &case.project).unwrap().hidden, 2);
    assert_eq!(case.names().len(), 1 + AGENTS_SHOWN_MAX);
    let written = show_all(&case.root, &case.project, true).unwrap();
    assert_eq!(
        written,
        Written {
            path: case.file(),
            hidden: 0,
            show_all: true,
        }
    );
    let mut expected = vec!["Project".to_owned()];
    expected.extend(subs.iter().rev().map(sub));
    assert_eq!(case.names(), expected);
    let again = write(&case.root, &case.project).unwrap();
    assert!(again.show_all);
    assert_eq!(case.names(), expected);
    let fewer = show_all(&case.root, &case.project, false).unwrap();
    assert_eq!((fewer.hidden, fewer.show_all), (2, false));
    assert_eq!(case.names().len(), 1 + AGENTS_SHOWN_MAX);
    assert_eq!(case.names()[1], sub(&subs[13]));
}

#[test]
fn workspace_file_changes_when_an_agent_starts_and_stops() {
    let mut case = Case::new();
    let master = case.agent("master", "working");
    let first = case.agent("sub", "working");
    follow(&case.db, &case.root).unwrap();
    assert!(std::fs::symlink_metadata(case.file()).is_err());
    write(&case.root, &case.project).unwrap();
    assert_eq!(
        case.names(),
        vec!["Project".to_owned(), "master".to_owned(), sub(&first)]
    );
    let unchanged = std::fs::metadata(case.file()).unwrap().ino();
    follow(&case.db, &case.root).unwrap();
    assert_eq!(std::fs::metadata(case.file()).unwrap().ino(), unchanged);
    let second = case.agent("sub", "starting");
    follow(&case.db, &case.root).unwrap();
    assert_eq!(
        case.folders(),
        vec![
            ("Project".to_owned(), case.folder.clone()),
            ("master".to_owned(), master.worktree.clone()),
            (sub(&second), second.worktree.clone()),
            (sub(&first), first.worktree.clone()),
        ]
    );
    case.status(&first, "stopped");
    follow(&case.db, &case.root).unwrap();
    assert_eq!(
        case.names(),
        vec!["Project".to_owned(), "master".to_owned(), sub(&second)]
    );
    case.status(&master, "stopped");
    case.status(&second, "failed");
    follow(&case.db, &case.root).unwrap();
    assert_eq!(case.names(), vec!["Project".to_owned()]);
}

#[test]
fn folder_names_follow_agent_name_and_role() {
    let mut case = Case::new();
    case.agent("master", "idle");
    let unnamed = case.agent("sub", "working");
    let persona_only = case.agent("sub", "working");
    let with_role = case.agent("sub", "working");
    let plain = names::assign_persona(&case.db, &case.project, persona_only.id).unwrap();
    let named = names::assign_persona(&case.db, &case.project, with_role.id).unwrap();
    assert_ne!(plain, named);
    let handle = names::assign_role(&case.db, &case.project, with_role.id, "builder")
        .unwrap()
        .unwrap();
    assert_eq!(handle, "builder");
    write(&case.root, &case.project).unwrap();
    assert_eq!(
        case.names(),
        vec![
            "Project".to_owned(),
            "master".to_owned(),
            format!("{named} (@builder)"),
            plain,
            sub(&unnamed),
        ]
    );
}

#[test]
fn idle_agent_without_open_change_set_is_left_out() {
    let mut case = Case::new();
    let idle = case.agent("sub", "working");
    let idle_changed = case.agent("sub", "working");
    let stopped_changed = case.agent("sub", "working");
    let stopped = case.agent("sub", "working");
    let working = case.agent("sub", "working");
    case.change(&idle_changed);
    case.change(&stopped_changed);
    for agent in [&idle, &idle_changed] {
        case.status(agent, "idle");
    }
    for agent in [&stopped_changed, &stopped] {
        case.status(agent, "stopped");
    }
    write(&case.root, &case.project).unwrap();
    assert_eq!(
        case.names(),
        vec![
            "Project".to_owned(),
            sub(&working),
            sub(&stopped_changed),
            sub(&idle_changed),
        ]
    );
}

#[test]
fn ide_server_folder_is_the_stored_project_folder_with_links_resolved() {
    let case = Case::new();
    assert_eq!(folder(&case.root, &case.project).unwrap(), case.folder);
    let linked = case.root.join("linked-project");
    std::os::unix::fs::symlink(&case.folder, &linked).unwrap();
    add_project(&case.db, "beta", &linked);
    let beta = ProjectId::new("beta").unwrap();
    assert_eq!(
        folder(&case.root, &beta).unwrap(),
        case.folder,
        "the server and its sandbox get the folder the link points to"
    );
    std::fs::remove_file(&linked).unwrap();
    assert_eq!(
        folder(&case.root, &beta).unwrap_err().0,
        "ide_project_folder_invalid"
    );
    assert_eq!(
        folder(&case.root, &ProjectId::new("gamma").unwrap())
            .unwrap_err()
            .0,
        "ide_project_unknown"
    );
}
