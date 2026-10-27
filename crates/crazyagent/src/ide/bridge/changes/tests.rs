use super::*;
use crate::landing::{ChangeAgent, FileStatus, RESTORE_WINDOW_MS, TurnEnd, TurnOutcome, Workspace};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const NOW: u64 = 1_800_000_000_000;

/// One owner repository on `main` with its own git identity, an agent worktree added
/// from it holding a modified, an added and a deleted file, an owner home with no global
/// git config, and an engine store with the change set that turn made.
struct Case {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    worktree: PathBuf,
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

impl Case {
    fn new() -> Self {
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
        std::fs::write(repo.join("OLD"), "gone soon\n").unwrap();
        owner(&["add", "README", "OLD"]);
        owner(&["commit", "-q", "-m", "Start"]);
        let worktree = root.join("agent-7");
        owner(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "HEAD",
        ]);
        let project = ProjectId::new("project-1").unwrap();
        let db = landing::open(&engine).unwrap();
        let workspace = Workspace {
            project: project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "7".into(),
                label: "Ada (@builder) \u{b7} Codex".into(),
            },
            repository: repo.clone(),
            worktree: worktree.clone(),
            git_dir: repo.join(".git/worktrees/agent-7"),
        };
        std::fs::write(worktree.join("README"), "uno\ntwo\nthree\n").unwrap();
        std::fs::write(worktree.join("NOTES"), "added by the agent\n").unwrap();
        std::fs::remove_file(worktree.join("OLD")).unwrap();
        let end = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Say one in Italian".into(),
        };
        landing::record_turn_end(&db, &workspace, &end)
            .unwrap()
            .unwrap();
        Self {
            _tmp: tmp,
            repo,
            home,
            worktree,
            db,
            project,
        }
    }

    fn only(&self, now: u64) -> IdeChangeSet {
        let mut sets = change_sets(&self.db, &self.project, now).unwrap();
        assert_eq!(sets.len(), 1, "{sets:?}");
        sets.remove(0)
    }

    fn owner_head(&self) -> String {
        git_in(&self.home, &self.repo, &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    }
}

#[test]
fn a_pending_set_is_one_group_named_for_its_agent_with_every_file() {
    let case = Case::new();
    let set = case.only(NOW);
    assert_eq!(set.agent, "Ada (@builder)");
    assert_eq!(set.role, AgentKind::Sub);
    assert_eq!(set.title, "Say one in Italian");
    assert_eq!(set.state, ChangeState::Pending);
    assert!(!set.waiting);
    assert_eq!(set.worktree, case.worktree);
    assert_eq!(set.restorable_until, None);
    assert_eq!(set.problem, None);
    let files: Vec<(&str, FileStatus)> = set
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status))
        .collect();
    assert_eq!(
        files,
        [
            ("NOTES", FileStatus::Added),
            ("OLD", FileStatus::Deleted),
            ("README", FileStatus::Modified)
        ]
    );
    assert_eq!(
        serde_json::to_value(&set).unwrap()["role"],
        serde_json::json!("sub")
    );
}

#[test]
fn ide_accept_lands_through_the_engine_and_the_set_leaves_pending() {
    let case = Case::new();
    let set = case.only(NOW);
    let start = case.owner_head();
    let landed = accept(
        &case.db,
        &case.home,
        &case.project,
        &set.id,
        &set.head,
        Some("Land the agent's edits"),
    )
    .unwrap();
    let AcceptOutcome::Landed { commit, branch } = landed else {
        panic!("{landed:?}");
    };
    assert_eq!(branch, "main");
    assert_eq!(case.owner_head(), commit);
    assert_eq!(
        git_in(&case.home, &case.repo, &["rev-parse", "HEAD^"]).trim(),
        start
    );
    assert_eq!(
        git_in(&case.home, &case.repo, &["log", "-1", "--format=%s"]).trim(),
        "Land the agent's edits"
    );
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "uno\ntwo\nthree\n"
    );
    assert!(
        change_sets(&case.db, &case.project, NOW)
            .unwrap()
            .is_empty(),
        "an accepted set has no group"
    );
}

#[test]
fn ide_accept_of_a_head_the_owner_did_not_review_changes_nothing() {
    let case = Case::new();
    let set = case.only(NOW);
    let start = case.owner_head();
    let refused = accept(
        &case.db,
        &case.home,
        &case.project,
        &set.id,
        &"0".repeat(40),
        None,
    )
    .unwrap_err();
    assert_eq!(refused.0, "changes_stale");
    assert_eq!(case.owner_head(), start);
    assert_eq!(case.only(NOW).state, ChangeState::Pending);
}

#[test]
fn ide_discard_then_restore_within_the_window_brings_the_set_back() {
    let case = Case::new();
    let set = case.only(NOW);
    let until = discard(&case.db, &case.project, &set.id, &set.head, NOW).unwrap();
    assert_eq!(until, NOW + RESTORE_WINDOW_MS);
    let discarded = case.only(NOW + 1);
    assert_eq!(discarded.state, ChangeState::Discarded);
    assert_eq!(discarded.restorable_until, Some(until));
    assert!(!case.worktree.join("NOTES").exists());
    restore(&case.db, &case.project, &set.id, NOW + 2).unwrap();
    let back = case.only(NOW + 3);
    assert_eq!(back.state, ChangeState::Pending);
    assert_eq!(back.head, set.head);
    assert_eq!(
        std::fs::read_to_string(case.worktree.join("NOTES")).unwrap(),
        "added by the agent\n"
    );
    discard(&case.db, &case.project, &set.id, &set.head, NOW + 4).unwrap();
    assert!(
        change_sets(&case.db, &case.project, NOW + 4 + RESTORE_WINDOW_MS)
            .unwrap()
            .is_empty(),
        "a discarded set past its window has no group"
    );
}

#[test]
fn the_base_side_of_a_diff_is_the_file_before_the_agent_touched_it() {
    let case = Case::new();
    let set = case.only(NOW);
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, "README").unwrap(),
        "one\ntwo\nthree\n"
    );
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, "OLD").unwrap(),
        "gone soon\n"
    );
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, "NOTES").unwrap(),
        ""
    );
    for outside in ["Cargo.toml", "../repo/README", ".git/config", ""] {
        assert_eq!(
            base_blob(&case.db, &case.project, &set.id, outside)
                .unwrap_err()
                .0,
            "changes_not_found",
            "{outside}"
        );
    }
}

#[test]
fn another_projects_ide_never_reads_or_changes_this_projects_change_sets() {
    let case = Case::new();
    let set = case.only(NOW);
    let start = case.owner_head();
    let other = ProjectId::new("project-2").unwrap();
    assert!(change_sets(&case.db, &other, NOW).unwrap().is_empty());
    let refusals = [
        change_diff(&case.db, &other, &set.id, 0).map(|_| ()),
        base_blob(&case.db, &other, &set.id, "README").map(|_| ()),
        accept(&case.db, &case.home, &other, &set.id, &set.head, None).map(|_| ()),
        discard(&case.db, &other, &set.id, &set.head, NOW).map(|_| ()),
        restore(&case.db, &other, &set.id, NOW),
    ];
    for refused in refusals {
        assert_eq!(refused.unwrap_err().0, "changes_not_found");
    }
    assert_eq!(case.owner_head(), start);
    assert_eq!(case.only(NOW), set);
    let page = change_diff(&case.db, &case.project, &set.id, 0).unwrap();
    assert!(page.text.contains("+uno"), "{}", page.text);
}
