use super::*;
use crate::landing::{AcceptOutcome, ChangeAgent, ChangeSetView, TurnOutcome};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// One owner repository, an agent worktree added from it, an owner home with no global
/// git config, and an engine store holding the team tables, with a change set that
/// conflicts with the owner's branch.
struct Case {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    worktree: PathBuf,
    db: Connection,
    project: ProjectId,
    id: ChangeSetId,
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

impl Case {
    /// Builds the case. Input: whether the team still has the agent that made the set.
    fn new(on_team: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        let owner = |args: &[&str]| git_in(&home, &repo, args);
        owner(&["init", "-q", "--template=", "-b", "main"]);
        owner(&["config", "user.name", "Repo Owner"]);
        owner(&["config", "user.email", "owner@example.invalid"]);
        std::fs::write(repo.join("README"), "one\ntwo\nthree\n").unwrap();
        owner(&["add", "README"]);
        owner(&["commit", "-q", "-m", "Start"]);
        let worktree = root.join("agent-1");
        owner(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ]);
        let git_dir = repo.join(".git/worktrees/agent-1");
        let project = ProjectId::new("project-1").unwrap();
        let db = landing::open(&engine).unwrap();
        crate::team::initialize(&db).unwrap();
        let mut member = 7;
        if on_team {
            db.execute(
                "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
                 VALUES (?1,'sub','codex',1,'stopped',1,1)",
                [project.as_str()],
            )
            .unwrap();
            member = db.last_insert_rowid();
            db.execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir)
                 VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    member,
                    project.as_str(),
                    repo.to_str().unwrap(),
                    worktree.to_str().unwrap(),
                    git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
        }
        let workspace = Workspace {
            project: project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: member.to_string(),
                label: "Agent 1".into(),
            },
            repository: repo.clone(),
            worktree: worktree.clone(),
            git_dir,
        };
        std::fs::write(worktree.join("README"), "uno\ntwo\nthree\n").unwrap();
        let end = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Say one in Italian".into(),
        };
        let id = landing::record_turn_end(&db, &workspace, &end)
            .unwrap()
            .unwrap();
        std::fs::write(repo.join("README"), "eins\ntwo\nthree\n").unwrap();
        owner(&["commit", "-q", "-am", "Say one in German"]);
        let head = view(&db, &id).unwrap().unwrap().head;
        let landed = landing::accept(&db, &home, &id, &head, None).unwrap();
        assert!(
            matches!(landed, AcceptOutcome::Conflict { .. }),
            "{landed:?}"
        );
        Self {
            _tmp: tmp,
            repo,
            worktree,
            db,
            project,
            id,
        }
    }

    fn set(&self) -> ChangeSetView {
        view(&self.db, &self.id).unwrap().unwrap()
    }

    fn agent_head(&self) -> String {
        let home = self.repo.parent().unwrap().join("home");
        git_in(&home, &self.worktree, &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    }

    fn readme(&self) -> String {
        std::fs::read_to_string(self.worktree.join("README")).unwrap()
    }

    fn resolve_tasks(&self) -> Vec<String> {
        let mut statement = self
            .db
            .prepare(
                "SELECT t.prompt FROM team_tasks t JOIN team_task_pins p ON p.task=t.id
                 WHERE t.project=?1 ORDER BY t.id",
            )
            .unwrap();
        statement
            .query_map([self.project.as_str()], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

#[test]
fn an_update_for_a_sub_agent_no_longer_on_the_team_is_refused_and_changes_nothing() {
    let case = Case::new(false);
    let (before, head, readme) = (case.set(), case.agent_head(), case.readme());
    assert_eq!(before.state, ChangeState::Conflict);
    assert_eq!(head, before.head);
    let refused = update(&case.db, &case.id).unwrap_err();
    assert_eq!(refused.0, "team_agent_not_found");
    assert!(
        crate::engine::owner::CHANGES_CODES.contains(&refused.0.as_str()),
        "the desktop shows this refusal as itself, not as an unknown outcome"
    );
    assert_eq!(case.set(), before);
    assert_eq!((case.agent_head(), case.readme()), (head, readme));
    assert!(!case.readme().contains("<<<<<<<"));
    assert!(case.resolve_tasks().is_empty());
}

#[test]
fn an_update_for_a_team_agent_merges_and_hands_it_the_resolve_work() {
    let case = Case::new(true);
    assert!(update(&case.db, &case.id).unwrap());
    let set = case.set();
    assert_eq!((set.state, set.waiting), (ChangeState::Updating, false));
    assert_eq!(case.agent_head(), set.head);
    assert!(case.readme().contains("<<<<<<<"));
    let tasks = case.resolve_tasks();
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert!(tasks[0].contains("- README\n"), "{}", tasks[0]);
}

#[test]
fn a_merged_update_whose_agent_was_not_handed_the_work_waits_until_asked_again() {
    let case = Case::new(true);
    landing::update(&case.db, &case.id).unwrap();
    let merged = case.set();
    assert_eq!(
        (merged.state, merged.waiting),
        (ChangeState::Updating, true)
    );
    assert!(case.readme().contains("<<<<<<<"));
    assert!(case.resolve_tasks().is_empty());

    assert!(!update(&case.db, &case.id).unwrap());
    let set = case.set();
    assert_eq!(
        (set.state, set.waiting, set.head.as_str()),
        (ChangeState::Updating, false, merged.head.as_str())
    );
    assert_eq!(case.agent_head(), merged.head);
    assert_eq!(case.resolve_tasks().len(), 1);
    assert_eq!(
        update(&case.db, &case.id).unwrap_err().0,
        "changes_stale",
        "a set its agent is already resolving is not merged again"
    );

    let waiting = Case::new(true);
    landing::update(&waiting.db, &waiting.id).unwrap();
    let head = waiting.set().head;
    landing::discard(
        &waiting.db,
        &waiting.id,
        &head,
        u64::try_from(crate::team::now_ms()).unwrap(),
    )
    .unwrap();
    assert_eq!(waiting.set().state, ChangeState::Discarded);
    assert!(!waiting.readme().contains("<<<<<<<"));
}
