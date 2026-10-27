use super::*;
use crate::landing::{AgentKind, ChangeAgent, TurnEnd, TurnOutcome, Workspace};
use std::process::{Command, Stdio};

const NOW: i64 = 1_800_000_000_000;

/// One project repository with a state folder, an owner home with no global git config,
/// and an engine root whose database holds the team and change set stores.
struct Case {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
    team: PathBuf,
    db: Connection,
    project: ProjectId,
}

/// Runs git as the owner would in a test: an isolated home, no system config, in `dir`.
/// Inputs: the owner home, the folder and the arguments. Output: standard output.
fn git_in(home: &Path, dir: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
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

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        std::fs::create_dir(repo.join(crate::projects_home::CRAZYAGENT_DIRECTORY)).unwrap();
        git_in(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git_in(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git_in(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        write(&repo.join("README"), b"one\n");
        write(&repo.join(".gitignore"), b"/.crazyagent/\ntarget/\n");
        git_in(&home, &repo, &["add", "README", ".gitignore"]);
        git_in(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let db = crate::landing::open(&engine).unwrap();
        super::super::initialize(&db).unwrap();
        Self {
            _tmp: tmp,
            home,
            repo,
            team: engine.join("team"),
            db,
            project: ProjectId::new("project-1").unwrap(),
        }
    }

    fn worktree(&self, id: i64) -> PathBuf {
        crate::projects_home::state_folder(&self.repo)
            .join("worktrees")
            .join(format!("agent-{id}"))
    }

    fn git_dir(&self, id: i64) -> PathBuf {
        self.repo.join(".git/worktrees").join(format!("agent-{id}"))
    }

    fn folder(&self, id: i64) -> PathBuf {
        self.team.join("agents").join(format!("agent-{id}"))
    }

    fn profile(&self, id: i64) -> PathBuf {
        self.team.join("profiles").join(format!("agent-{id}.sb"))
    }

    /// Adds one team agent as the runtime does: its worktree under the project's state
    /// folder with its start recorded, its agent folder and sandbox profile, and its rows.
    /// Inputs: the id, its role and its status.
    fn agent(&self, id: i64, role: &str, status: &str) {
        let worktree = self.worktree(id);
        git_in(
            &self.home,
            &self.repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        crate::landing::record_worktree_start(&self.db, &self.repo, &worktree, &self.git_dir(id))
            .unwrap();
        write(
            &self.folder(id).join("home/projects/x/session.jsonl"),
            b"{}\n",
        );
        std::fs::create_dir_all(self.folder(id).join("tmp")).unwrap();
        write(&self.profile(id), b"(version 1)\n");
        let stopped = matches!(status, "stopped" | "failed").then_some(NOW - 1000);
        self.db
            .execute(
                "INSERT INTO team_agents(id,project,role,harness,tools,connection,status,created_ms,heartbeat_ms,stopped_ms)
                 VALUES (?1,?2,?3,'claude-code',0,'claude-main',?4,?5,?5,?6)",
                params![id, self.project.as_str(), role, status, NOW - 5000, stopped],
            )
            .unwrap();
        self.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                params![
                    id,
                    self.project.as_str(),
                    self.repo.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    self.git_dir(id).to_str().unwrap()
                ],
            )
            .unwrap();
    }

    fn in_agent(&self, id: i64, args: &[&str]) -> String {
        git_in(&self.home, &self.worktree(id), args)
    }

    /// Ends a turn in an agent's worktree the way the runtime does, opening or adding to
    /// its change set. Input: the agent. Output: the change set.
    fn turn_end(&self, id: i64) -> crate::landing::ChangeSetId {
        let workspace = Workspace {
            project: self.project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: id.to_string(),
                label: format!("Agent {id}"),
            },
            repository: self.repo.clone(),
            worktree: self.worktree(id),
            git_dir: self.git_dir(id),
        };
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("turn-{id}"),
            prompt_line: "Edit the readme".into(),
        };
        crate::landing::record_turn_end(&self.db, &workspace, &turn)
            .unwrap()
            .unwrap()
    }

    fn head_of(&self, id: &crate::landing::ChangeSetId) -> String {
        crate::landing::list(&self.db, &self.project)
            .unwrap()
            .into_iter()
            .find(|view| &view.id == id)
            .unwrap()
            .head
    }

    fn listed(&self) -> Vec<i64> {
        finished(&self.db, &self.team, &self.project, NOW)
            .unwrap()
            .iter()
            .map(|agent| agent.id)
            .collect()
    }

    fn known(&self, id: i64) -> bool {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM team_worktrees WHERE agent=?1",
                [id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    }

    fn kept(&self, id: i64) -> bool {
        self.known(id) && self.worktree(id).is_dir() && self.folder(id).is_dir()
    }
}

#[test]
fn a_clean_finished_agent_is_listed_with_its_size_and_removed_with_its_folders() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("target/build.bin"), &[7; 64 * 1024]);
    case.agent(2, "master", "failed");
    write(&case.worktree(2).join("README"), b"one\ntwo\n");
    let landed = case.turn_end(2);
    let head = case.head_of(&landed);
    let outcome = crate::landing::accept(&case.db, &case.home, &landed, &head, None).unwrap();
    assert!(matches!(
        outcome,
        crate::landing::AcceptOutcome::Landed { .. }
    ));
    let listed = finished(&case.db, &case.team, &case.project, NOW).unwrap();
    assert_eq!(
        listed.iter().map(|agent| agent.id).collect::<Vec<_>>(),
        [2, 1]
    );
    assert!(listed[1].bytes >= 64 * 1024 && listed[1].counted_all);
    assert_eq!(listed[0].role, AgentRole::Master);
    assert!(!listed[1].label.is_empty());
    assert_eq!(
        remove(&case.db, &case.team, &case.project, &[1, 2], NOW).unwrap(),
        Ok(vec![1, 2])
    );
    let listing = git_in(&case.home, &case.repo, &["worktree", "list", "--porcelain"]);
    for id in [1, 2] {
        assert!(!case.worktree(id).exists());
        assert!(!case.git_dir(id).exists());
        assert!(!listing.contains(&format!("agent-{id}")));
        assert!(!case.folder(id).exists() && !case.profile(id).exists());
        assert!(!case.known(id));
    }
    let rows: i64 = case
        .db
        .query_row("SELECT COUNT(*) FROM team_agents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
    assert_eq!(
        git_in(&case.home, &case.repo, &["status", "--porcelain"]),
        ""
    );
    assert!(case.listed().is_empty());
}

#[test]
fn unlanded_or_live_work_keeps_the_agent_and_says_why() {
    let case = Case::new();
    case.agent(1, "sub", "stopped");
    write(&case.worktree(1).join("README"), b"edited\n");
    case.agent(2, "sub", "stopped");
    write(&case.worktree(2).join("notes.txt"), b"new\n");
    case.agent(3, "sub", "stopped");
    write(&case.worktree(3).join("README"), b"committed\n");
    case.in_agent(3, &["commit", "-q", "-am", "Agent commit"]);
    case.agent(4, "sub", "stopped");
    write(&case.worktree(4).join("README"), b"pending\n");
    case.turn_end(4);
    case.agent(5, "sub", "failed");
    write(&case.worktree(5).join("README"), b"discarded\n");
    let discarded = case.turn_end(5);
    let head = case.head_of(&discarded);
    crate::landing::discard(&case.db, &discarded, &head, u64::try_from(NOW).unwrap()).unwrap();
    case.agent(6, "master", "idle");
    case.agent(7, "sub", "stopped");
    case.db
        .execute(
            "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,created_ms)
             VALUES (?1,'master','claude-code',0,'Resolve','queued',7,?2)",
            params![case.project.as_str(), NOW],
        )
        .unwrap();
    case.agent(8, "sub", "working");
    assert!(case.listed().is_empty());
    let refusals = [
        (1, "cleanup_worktree_dirty"),
        (2, "cleanup_worktree_dirty"),
        (3, "cleanup_commits_unlanded"),
        (4, "cleanup_changes_open"),
        (5, "cleanup_changes_restorable"),
        (6, "cleanup_agent_running"),
        (7, "cleanup_agent_has_task"),
        (8, "cleanup_agent_running"),
    ];
    for (id, code) in refusals {
        assert_eq!(
            remove(&case.db, &case.team, &case.project, &[id], NOW).unwrap(),
            Err(code),
            "agent {id}"
        );
        assert!(case.kept(id), "agent {id} keeps its worktree and folder");
    }
    assert_eq!(case.in_agent(1, &["status", "--porcelain"]), " M README\n");
}

#[test]
fn a_change_after_the_read_is_caught_inside_the_removal_and_nothing_is_removed() {
    let case = Case::new();
    for id in [1, 2, 3] {
        case.agent(id, "sub", "stopped");
    }
    assert_eq!(case.listed(), [3, 2, 1]);
    write(
        &case.worktree(2).join("late.txt"),
        b"written after the read\n",
    );
    assert_eq!(
        remove(&case.db, &case.team, &case.project, &[1, 2], NOW).unwrap(),
        Err("cleanup_worktree_dirty")
    );
    assert!(case.kept(1) && case.kept(2));
    case.db
        .execute("UPDATE team_agents SET status='starting' WHERE id=3", [])
        .unwrap();
    assert_eq!(
        remove(&case.db, &case.team, &case.project, &[1, 3], NOW).unwrap(),
        Err("cleanup_agent_running")
    );
    assert!(case.kept(1) && case.kept(3));
    let other = ProjectId::new("project-2").unwrap();
    assert_eq!(
        remove(&case.db, &case.team, &other, &[1], NOW).unwrap(),
        Err("cleanup_agent_not_found")
    );
    let over = i64::try_from(RECLAIMABLE_MAX).unwrap() + 1;
    for agents in [vec![], vec![1, 1], (1..=over).collect::<Vec<i64>>()] {
        assert_eq!(
            remove(&case.db, &case.team, &case.project, &agents, NOW).unwrap(),
            Err("cleanup_request_invalid")
        );
    }
    assert!(case.kept(1));
    assert_eq!(
        remove(&case.db, &case.team, &case.project, &[1], NOW).unwrap(),
        Ok(vec![1])
    );
    assert_eq!(case.listed(), Vec::<i64>::new());
}
