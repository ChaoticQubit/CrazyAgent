#![cfg(unix)]

use crazyagent::activity::{self, ActivityKind};
use crazyagent::landing::{
    self, AcceptOutcome, AgentKind, ChangeAgent, ChangeSetId, TurnEnd, TurnOutcome, Workspace,
};
use crazyagent::protocol::ProjectId;
use crazyagent::team::events::EventKind;
use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

const PROJECT: &str = "project-1";
const GOAL: &str = "Ship the login page";

/// Runs git as the owner would, with no system or global config, an isolated home and a
/// fixed identity, and requires it to succeed. Takes the folder, the home and the
/// arguments. Returns its trimmed standard output.
fn git(folder: &Path, home: &Path, arguments: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args(["-c", "user.email=agent@local", "-c", "user.name=agent"])
            .arg("-C")
            .arg(folder)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// One owner repository with the engine database beside it, holding every table the
/// activity reads, and one team sub-agent, agent 1, in its own clone, recorded as the
/// engine records a team agent's worktree.
struct Project {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    worktree: PathBuf,
    git_dir: PathBuf,
    db: Connection,
    id: ProjectId,
}

impl Project {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (project, home, engine) =
            (root.join("project"), root.join("home"), root.join("engine"));
        for folder in [&project, &home, &engine] {
            fs::create_dir(folder).unwrap();
        }
        git(&project, &home, &["init", "-q", "-b", "main"]);
        git(&project, &home, &["config", "user.name", "Repo Owner"]);
        git(
            &project,
            &home,
            &["config", "user.email", "owner@example.invalid"],
        );
        fs::write(project.join("README"), "one\ntwo\nthree\n").unwrap();
        git(&project, &home, &["add", "README"]);
        git(&project, &home, &["commit", "-q", "-m", "Start"]);
        fs::create_dir_all(project.join(".git/info")).unwrap();
        fs::write(project.join(".git/info/exclude"), ".crazyagent/\n").unwrap();
        fs::create_dir_all(project.join(".crazyagent/worktrees")).unwrap();
        let db = landing::open(&engine).unwrap();
        crazyagent::team::initialize(&db).unwrap();
        crazyagent::team::events::initialize(&db).unwrap();
        crazyagent::coord::ensure(&db).unwrap();
        db.execute_batch("CREATE TABLE projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)")
            .unwrap();
        activity::initialize(&db).unwrap();
        let worktree = project.join(".crazyagent/worktrees/agent-1");
        let branch = landing::claim_task_branch(&db, &project, &worktree, "login").unwrap();
        landing::add_agent_clone_on(&project, &worktree, None, &branch).unwrap();
        let git_dir = crazyagent::agent_git::git_dir(&worktree);
        db.execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (1,?1,'sub','claude-code',1,'working',0,0)",
            [PROJECT],
        )
        .unwrap();
        db.execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (1,?1,?2,?3,?4)",
            params![
                PROJECT,
                project.to_str().unwrap(),
                worktree.to_str().unwrap(),
                git_dir.to_str().unwrap()
            ],
        )
        .unwrap();
        landing::record_worktree_start(&db, &project, &worktree, &git_dir).unwrap();
        Self {
            _temp: temp,
            project,
            home,
            worktree,
            git_dir,
            db,
            id: ProjectId::new(PROJECT).unwrap(),
        }
    }

    /// Commits one file in a checkout. Takes the checkout, the path, the text and the
    /// message.
    fn commit(&self, checkout: &Path, path: &str, text: &str, message: &str) {
        fs::write(checkout.join(path), text).unwrap();
        git(checkout, &self.home, &["add", path]);
        git(checkout, &self.home, &["commit", "-q", "-m", message]);
    }

    /// Ends one successful turn of the sub-agent, which records its work as a change set.
    /// Returns the change set.
    fn turn_end(&self) -> ChangeSetId {
        let workspace = Workspace {
            project: self.id.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "1".into(),
                label: "Sub-agent 1".into(),
            },
            repository: self.project.clone(),
            worktree: self.worktree.clone(),
            git_dir: self.git_dir.clone(),
        };
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Build the login form".into(),
        };
        landing::record_turn_end(&self.db, &workspace, &turn)
            .unwrap()
            .expect("the turn left work for review")
    }

    fn head(&self, set: &ChangeSetId) -> String {
        landing::change_set(&self.db, set).unwrap().unwrap().1.head
    }

    fn accept(&self, set: &ChangeSetId) -> AcceptOutcome {
        landing::accept_into(&self.db, &self.home, set, &self.head(set), None, "main").unwrap()
    }

    fn event(&self, kind: EventKind, text: &str) {
        crazyagent::team::events::record(&self.db, &self.id, 1, None, kind, text, now_ms())
            .unwrap()
            .expect("the event was recorded");
    }
}

/// One sub-agent's work, from its start through a finished turn, an Accept, a Discard, a
/// conflicting Accept and an update to the work's end, is recorded in the project's
/// activity newest first, each event named by who acted, with the work's goal and the
/// change set it touched.
#[test]
fn every_project_event_the_engine_sees_is_recorded_newest_first() {
    let project = Project::new();
    let (work, _) = crazyagent::coord::start_work(
        &project.db,
        &project.id,
        1,
        GOAL.into(),
        vec!["login.txt".into()],
        now_ms(),
    )
    .unwrap();
    project.commit(
        &project.worktree,
        "login.txt",
        "login\n",
        "Add the login form",
    );
    let accepted = project.turn_end();
    project.event(EventKind::Finished, "Finished: Build the login form");
    project.event(EventKind::ChangeSetReady, "Changes are ready for review");
    assert!(matches!(
        project.accept(&accepted),
        AcceptOutcome::Landed { .. }
    ));
    project.commit(&project.worktree, "extra.txt", "extra\n", "Add more");
    let discarded = project.turn_end();
    landing::discard(
        &project.db,
        &discarded,
        &project.head(&discarded),
        u64::try_from(now_ms()).unwrap(),
    )
    .unwrap();
    project.commit(
        &project.project,
        "README",
        "one\nTWO\nthree\n",
        "Owner edit",
    );
    project.commit(&project.worktree, "README", "one\n2\nthree\n", "Agent edit");
    let conflicting = project.turn_end();
    assert!(matches!(
        project.accept(&conflicting),
        AcceptOutcome::Conflict { .. }
    ));
    landing::update(&project.db, &conflicting).unwrap();
    crazyagent::coord::finish(
        &project.db,
        work,
        1,
        crazyagent::coord::Finish::Complete,
        "Done".into(),
        1,
        now_ms(),
    )
    .unwrap();

    let listed = activity::list(&project.db, &project.id, activity::LIST_MAX).unwrap();

    let seen: Vec<(ActivityKind, &str, Option<&str>, Option<&str>)> = listed
        .iter()
        .map(|item| {
            (
                item.kind,
                item.actor.as_str(),
                item.goal.as_deref(),
                item.change_set.as_deref(),
            )
        })
        .collect();
    let agent = "Sub-agent 1";
    assert_eq!(
        seen,
        vec![
            (ActivityKind::WorkComplete, agent, Some(GOAL), None),
            (
                ActivityKind::SentBack,
                "Owner",
                Some(GOAL),
                Some(conflicting.as_str())
            ),
            (
                ActivityKind::Conflict,
                "Owner",
                Some(GOAL),
                Some(conflicting.as_str())
            ),
            (
                ActivityKind::Discarded,
                "Owner",
                Some(GOAL),
                Some(discarded.as_str())
            ),
            (
                ActivityKind::Accepted,
                "Owner",
                Some(GOAL),
                Some(accepted.as_str())
            ),
            (ActivityKind::ChangesReady, agent, Some(GOAL), None),
            (ActivityKind::TaskFinished, agent, Some(GOAL), None),
            (ActivityKind::WorkStarted, agent, Some(GOAL), None),
        ]
    );
    assert!(
        listed.windows(2).all(|pair| pair[0].at_ms >= pair[1].at_ms),
        "{listed:?}"
    );
    assert_eq!(
        listed[4].text,
        "You accepted change set #1 (Sub-agent 1: Add the login form, 1 file)"
    );
    assert_eq!(listed[6].text, "Sub-agent 1 finished a task");
}

/// A task's start and end name the task by its title (F1): the prompt's first line with an
/// absolute path outside the project cut to its last component. A finished task never
/// carries the agent's reply, so a path and a hidden character the reply holds are never
/// listed, and a failed task only says it failed.
#[test]
fn a_task_row_names_its_task_and_never_carries_the_reply() {
    let project = Project::new();
    let task = |status: &str, result: Option<&str>, error: Option<&str>| -> i64 {
        project
            .db
            .execute(
                "INSERT INTO team_tasks(project,source,harness,tools,prompt,status,agent,result,error,created_ms) VALUES (?1,'owner','claude-code',1,?2,?3,1,?4,?5,1)",
                params![
                    PROJECT,
                    "Build the login form from /Users/ada/notes/plan.md\nKeep it short.",
                    status,
                    result,
                    error
                ],
            )
            .unwrap();
        project.db.last_insert_rowid()
    };
    let done = task(
        "done",
        Some("Done. I read /Users/ada/.ssh/id_rsa\u{200b} to sign in."),
        None,
    );
    let failed = task(
        "failed",
        None,
        Some("harness exited reading /Users/ada/.aws/credentials"),
    );
    activity::note_task(&project.db, &project.id, 1, done, now_ms());
    for ended in [done, failed] {
        assert!(
            crazyagent::team::events::task_ended(&project.db, ended, now_ms())
                .unwrap()
                .is_some()
        );
    }

    let listed = activity::list(&project.db, &project.id, activity::LIST_MAX).unwrap();

    let tasks: Vec<(ActivityKind, &str)> = listed
        .iter()
        .filter(|item| {
            matches!(
                item.kind,
                ActivityKind::TaskStarted | ActivityKind::TaskFinished
            )
        })
        .map(|item| (item.kind, item.text.as_str()))
        .collect();
    let title = "Build the login form from plan.md";
    assert_eq!(
        tasks,
        vec![
            (
                ActivityKind::TaskFinished,
                format!("Sub-agent 1 could not finish \u{201c}{title}\u{201d}").as_str()
            ),
            (
                ActivityKind::TaskFinished,
                format!("Sub-agent 1 finished \u{201c}{title}\u{201d}").as_str()
            ),
            (
                ActivityKind::TaskStarted,
                format!("Sub-agent 1 started \u{201c}{title}\u{201d}").as_str()
            ),
        ]
    );
    for item in &listed {
        for shown in [
            item.actor.as_str(),
            item.goal.as_deref().unwrap_or_default(),
            item.text.as_str(),
        ] {
            assert!(
                !shown.contains("/Users/")
                    && !shown.contains('\u{200b}')
                    && !shown.contains("id_rsa")
                    && !shown.contains("credentials"),
                "{shown:?}"
            );
        }
    }
}
