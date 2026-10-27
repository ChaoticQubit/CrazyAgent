use super::*;
use crate::engine::bootstrap::EngineService;
use crate::landing::{
    AgentKind, ChangeAgent, ChangeState, FileStatus, TurnEnd, TurnOutcome, Workspace,
};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

fn private_root(prefix: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
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

fn id() -> ChangeSetId {
    ChangeSetId::new("0123456789abcdef0123456789abcdef").unwrap()
}

#[test]
fn a_request_without_a_trusted_connection_answers_for_its_own_query() {
    let query = ChangesQuery::Accept {
        id: id(),
        expected_head: "a".repeat(40),
        message: "Add a file".into(),
    };
    let event = ChangesEvent::unavailable(query.clone());
    assert_eq!(event.query, query);
    assert_eq!(event.result, Err("trusted_connection_unavailable".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tui_lists_diffs_discards_restores_accepts_and_updates_a_real_change_set() {
    let root = private_root("ca-tchanges-");
    let work = private_root("ca-tchanges-work-");
    let base = std::fs::canonicalize(work.path()).unwrap();
    let (repo, home) = (base.join("repo"), base.join("home"));
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&home).unwrap();
    let owner = |args: &[&str]| git_in(&home, &repo, args);
    owner(&["init", "-q", "--template=", "-b", "main"]);
    owner(&["config", "user.name", "Repo Owner"]);
    owner(&["config", "user.email", "owner@example.invalid"]);
    owner(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("README"), "one\ntwo\nthree\n").unwrap();
    owner(&["add", "README"]);
    owner(&["commit", "-q", "-m", "Start"]);
    let worktree = base.join("agent-1");
    owner(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        worktree.to_str().unwrap(),
        "HEAD",
    ]);
    let project = ProjectId::new("project-1").unwrap();
    let git_dir = repo.join(".git/worktrees/agent-1");
    let turn = |prompt: &str| TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: prompt.into(),
    };

    let (_stop, stopped) = tokio::sync::watch::channel(false);
    let config = crate::tui::test_home::engine_config();
    let mut service = EngineService::start(root.path(), "e.sock", &config, &stopped)
        .await
        .unwrap();
    let mut native = NativeOwner::new(
        root.path().join("e.sock"),
        crate::tui::test_home::ENGINE_WAIT,
    );
    let (_, access) = native.attach().await;
    assert!(matches!(access, OwnerEvent::Access { ready: true, .. }));
    let db = crate::landing::open(root.path()).unwrap();
    db.execute(
        "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms)
         VALUES (?1,'sub','codex',1,'stopped',1,1)",
        [project.as_str()],
    )
    .unwrap();
    let member = db.last_insert_rowid();
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

    std::fs::write(worktree.join("README"), "one\ntwo\nthree\nfour\n").unwrap();
    let first = crate::landing::record_turn_end(&db, &workspace, &turn("Add the fourth line"))
        .unwrap()
        .expect("an edited worktree makes a change set");

    let listed = |sets: Vec<ChangeSetView>, id: &ChangeSetId| {
        sets.into_iter()
            .find(|set| &set.id == id)
            .expect("the change set is listed")
    };
    let query = ChangesQuery::List {
        project: project.clone(),
    };
    let event = native.changes(query.clone()).await;
    assert_eq!(event.query, query);
    let Ok(ChangesView::List(sets)) = event.result else {
        panic!("change set list required: {:?}", event.result);
    };
    let set = listed(sets, &first);
    assert_eq!(set.state, ChangeState::Pending);
    assert!(!set.checked);
    assert_eq!((set.files, set.added, set.removed), (1, 1, 0));

    let event = native
        .changes(ChangesQuery::Diff {
            id: first.clone(),
            page: 0,
        })
        .await;
    let Ok(ChangesView::Diff(diff)) = event.result else {
        panic!("diff page required: {:?}", event.result);
    };
    assert_eq!(diff.files.len(), 1);
    assert_eq!(diff.files[0].path, "README");
    assert_eq!(diff.files[0].status, FileStatus::Modified);
    assert!(diff.text.contains("+four"), "{}", diff.text);
    assert!(!diff.text.contains('\u{1b}'), "{}", diff.text);

    let event = native
        .changes(ChangesQuery::Discard {
            id: first.clone(),
            expected_head: set.head.clone(),
        })
        .await;
    let Ok(ChangesView::Discarded {
        restorable_until_ms,
    }) = event.result
    else {
        panic!("discard required: {:?}", event.result);
    };
    assert!(restorable_until_ms > set.updated_ms);
    let event = native
        .changes(ChangesQuery::Restore { id: first.clone() })
        .await;
    assert_eq!(event.result, Ok(ChangesView::Restored));

    let event = native
        .changes(ChangesQuery::Accept {
            id: first.clone(),
            expected_head: set.head.clone(),
            message: "Land the fourth line\n\nEdited before accepting".into(),
        })
        .await;
    let Ok(ChangesView::Accepted(AcceptOutcome::Landed { branch, .. })) = event.result else {
        panic!("accept required: {:?}", event.result);
    };
    assert_eq!(branch, "main");
    assert_eq!(
        std::fs::read_to_string(repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(owner(&["log", "-1", "--format=%an"]).trim(), "Repo Owner");
    assert_eq!(
        owner(&["log", "-1", "--format=%B"]).trim_end(),
        "Land the fourth line\n\nEdited before accepting"
    );

    std::fs::write(worktree.join("README"), "uno\ntwo\nthree\nfour\n").unwrap();
    let second = crate::landing::record_turn_end(&db, &workspace, &turn("Say one in Italian"))
        .unwrap()
        .expect("a second edit makes a change set");
    std::fs::write(repo.join("README"), "eins\ntwo\nthree\nfour\n").unwrap();
    owner(&["commit", "-q", "-am", "Say one in German"]);
    let event = native
        .changes(ChangesQuery::List {
            project: project.clone(),
        })
        .await;
    let Ok(ChangesView::List(sets)) = event.result else {
        panic!("change set list required: {:?}", event.result);
    };
    let set = listed(sets, &second);
    let event = native
        .changes(ChangesQuery::Accept {
            id: second.clone(),
            expected_head: set.head,
            message: set.message,
        })
        .await;
    assert_eq!(
        event.result,
        Ok(ChangesView::Accepted(AcceptOutcome::Conflict {
            files: vec!["README".into()]
        }))
    );
    let event = native.changes(ChangesQuery::Update { id: second }).await;
    assert_eq!(event.result, Ok(ChangesView::Updating));
    assert!(
        std::fs::read_to_string(worktree.join("README"))
            .unwrap()
            .contains("<<<<<<<")
    );
    let (prompt, status): (String, String) = db
        .query_row(
            "SELECT t.prompt,t.status FROM team_tasks t JOIN team_task_pins p ON p.task=t.id
             WHERE p.project=?1 AND p.agent=?2",
            rusqlite::params![project.as_str(), member],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "queued");
    assert!(prompt.contains("- README\n"), "{prompt}");

    service.shutdown().await.unwrap();
}
