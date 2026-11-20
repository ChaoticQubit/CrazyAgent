use super::git::{Repo, Writes};
use super::*;
use std::process::{Command, Stdio};

/// The landing functions Windows refuses in 2.4.4, in `landing.rs` order (PLAN-1336 D3).
const REFUSED_NAMES: [&str; 50] = [
    "add_agent_clone_on",
    "repoint_agent_clone",
    "repoint_linked_worktree",
    "claim_task_branch",
    "agent_branch",
    "master_branch",
    "land_on",
    "main_tip",
    "agent_tip",
    "unmerged_sub_agents",
    "claim_sub_agent_branch",
    "merge_sub_agent",
    "ready_agent_clone",
    "name_agent_branch",
    "record_worktree_start",
    "tracked_files",
    "committed_files",
    "commit_deletions",
    "sweep_claims",
    "resolve_task_prompt",
    "owner_list",
    "settle_sub_agents",
    "waiting",
    "list",
    "open_teammate_sets",
    "accept_into",
    "accept_into_as",
    "can_merge_again",
    "land_master_set",
    "undo_landing",
    "owner_dirty",
    "commit_owner_files",
    "discard_as",
    "restore",
    "conflicts_with_main",
    "move_after_turn",
    "tell_main_moved",
    "sweep_regenerations",
    "update",
    "resolve_handed_over",
    "reconcile",
    "observe",
    "retire_blocker",
    "unlanded_teammates",
    "teammate_worktrees",
    "linked_teammate_place",
    "teammate_unsaved",
    "add_teammate_worktree",
    "retire_worktree",
    "prune",
];

/// A scratch place for one Windows landing row: an engine runtime folder with its store,
/// and a project made by the runner's own git with one commit and a linked worktree that
/// holds an uncommitted edit. Kept alive by the row through its temp folder.
struct Place {
    _tmp: tempfile::TempDir,
    db: Connection,
    home: PathBuf,
    workspace: Workspace,
}

/// Runs the runner's git in `folder` with `args` and requires it to succeed. Inputs: the
/// folder and the arguments. Output: none; a failed git fails the row.
fn git(folder: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=row",
            "-c",
            "user.email=row@example.invalid",
        ])
        .args(args)
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// Makes a [`Place`]: a store under `runtime`, a project with one commit and a linked
/// worktree `agent` with one edited file. Output: the place.
fn place() -> Place {
    let tmp = tempfile::tempdir().unwrap();
    let root = crate::sandbox::canonical(tmp.path()).unwrap();
    let runtime = root.join("runtime");
    let repository = root.join("project");
    let worktree = root.join("agent");
    let home = root.join("home");
    for folder in [&runtime, &repository, &home] {
        std::fs::create_dir(folder).unwrap();
    }
    git(&repository, &["init", "-q", "-b", "main"]);
    std::fs::write(repository.join("a.txt"), "one\n").unwrap();
    git(&repository, &["add", "a.txt"]);
    git(&repository, &["commit", "-q", "-m", "one"]);
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent",
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::write(worktree.join("a.txt"), "two\n").unwrap();
    let git_dir = repository.join(".git").join("worktrees").join("agent");
    assert!(git_dir.is_dir());
    let db = open(&runtime).unwrap();
    let workspace = Workspace {
        project: ProjectId::new("project-1").unwrap(),
        agent: ChangeAgent {
            kind: AgentKind::Master,
            id: "master-1".into(),
            label: "Master".into(),
        },
        repository,
        worktree,
        git_dir,
    };
    Place {
        _tmp: tmp,
        db,
        home,
        workspace,
    }
}

/// The code a landing answer refused with. Input: the answer. Output: its code.
fn code<T>(answer: Result<T, EngineError>) -> String {
    match answer {
        Ok(_) => "ok".into(),
        Err(problem) => refusal_code(&problem).to_owned(),
    }
}

/// W5a a5-1: every landing function Windows does not run in 2.4.4, the Changes list among
/// them so the Changes screen stays not ready, answers what it answered before W5a, `changes_unavailable`, or nothing done for the four the engine calls on every
/// turn or sweep, and records its own name, so none of them starts work that Windows has no
/// landing design for (PLAN-1336 D3).
#[test]
fn each_refused_landing_function_refuses_by_name_on_windows() {
    let place = place();
    let db = &place.db;
    let ws = &place.workspace;
    let (repo, tree, dir) = (&ws.repository, &ws.worktree, &ws.git_dir);
    let project = &ws.project;
    let home = place.home.as_path();
    let id = ChangeSetId("cs-1".into());
    let tmp = home.join("tmp");
    std::fs::create_dir(&tmp).unwrap();
    let sandbox = crate::sandbox::ProjectSandbox::new(
        repo,
        home,
        &tmp,
        crate::sandbox::SandboxNetwork::Only(Vec::new()),
    )
    .unwrap();
    let lead = LeadGit {
        sandbox: &sandbox,
        profile: home,
    };
    REFUSED.with(|names| names.borrow_mut().clear());
    let answers = [
        code(add_agent_clone_on(repo, tree, None, "b")),
        code(repoint_agent_clone(dir, repo, tree)),
        code(repoint_linked_worktree(dir, tree, dir, tree)),
        code(claim_task_branch(db, repo, tree, "b")),
        code(agent_branch(db, tree, dir)),
        code(master_branch(repo, None)),
        code(land_on(db, repo, project)),
        code(main_tip(db, repo, project)),
        code(agent_tip(db, repo, tree, dir)),
        code(unmerged_sub_agents(db, project, 1)),
        code(claim_sub_agent_branch(db, repo, tree, "b", None, "n")),
        code(merge_sub_agent(db, project, 1, 2, &lead)),
        code(ready_agent_clone(tree)),
        code(name_agent_branch(db, repo, tree, dir)),
        code(record_worktree_start(db, repo, tree, dir)),
        code(tracked_files(repo, &["a.txt"])),
        code(committed_files(repo, &["a.txt"])),
        code(commit_deletions(repo, home, &["a.txt"], "m")),
        code(sweep_claims(db, project, 1)),
        code(resolve_task_prompt(db, &id)),
        code(owner_list(db, project)),
        code(settle_sub_agents(db, project, 1)),
        code(waiting(db, Some(project))),
        code(list(db, project)),
        code(open_teammate_sets(db, tree, 4)),
        code(accept_into(
            db,
            home,
            &id,
            "h",
            None,
            "main",
            crate::activity::AcceptPlace::Changes,
        )),
        code(accept_into_as(db, home, &id, "h", None, "main", "owner")),
        code(can_merge_again(repo)),
        code(land_master_set(db, home, &id)),
        code(undo_landing(db, home, &id)),
        code(owner_dirty(db, &id, None)),
        code(commit_owner_files(db, home, &id, &[], "m", None)),
        code(discard_as(db, &id, "h", 1, "owner")),
        code(restore(db, &id, 1)),
        code(conflicts_with_main(db, &id, None, None)),
        move_after_turn(db, ws).to_string(),
        tell_main_moved(db, project, 1).to_string(),
        code(sweep_regenerations(db)),
        code(update(db, &id)),
        code(resolve_handed_over(db, &id)),
        code(reconcile(db)),
        code(observe(db, Some(project))),
        code(retire_blocker(db, repo, tree, dir, 1)),
        code(unlanded_teammates(db, repo, tree, dir)),
        code(teammate_worktrees(db, tree, dir)),
        code(linked_teammate_place(db, tree, dir, "n")),
        code(teammate_unsaved(db, tree, dir, "n")),
        code(add_teammate_worktree(db, tree, dir, "n", &lead)),
        code(retire_worktree(db, repo, tree, dir, 1)),
        code(prune(db, 1)),
    ];
    assert_eq!(answers.len(), REFUSED_NAMES.len());
    for (answer, name) in answers.iter().zip(REFUSED_NAMES) {
        let expected = match name {
            "land_master_set" | "sweep_regenerations" => "ok",
            "move_after_turn" | "tell_main_moved" => "false",
            _ => "changes_unavailable",
        };
        assert_eq!(answer, expected, "{name}");
    }
    let names = REFUSED.with(|names| names.borrow().clone());
    assert_eq!(names, REFUSED_NAMES.to_vec());
}

/// W5a a5-2, rv-sec3 #746 C6: a Windows confined step answers `changes_git_unsupported`
/// for every kind and for a reading step, and an engine git call starts no process and
/// answers the same, so no state of the branch runs git with the engine's own rights.
#[test]
fn a_windows_engine_git_step_or_call_is_refused_and_starts_nothing() {
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::fresh(&ws.git_dir, &ws.worktree);
    assert_eq!(code(repo.reading()), "changes_git_unsupported");
    for writes in [Writes::Objects, Writes::WorkTree, Writes::Index] {
        assert_eq!(code(repo.confined(writes)), "changes_git_unsupported");
    }
    let marker = ws.worktree.join("ran");
    let marker_text = marker.to_str().unwrap();
    assert_eq!(
        code(repo.exec(
            &["config", "--file", marker_text, "a.b", "c"],
            None,
            &[],
            4096
        )),
        "changes_git_unsupported"
    );
    assert!(!marker.exists());
}

/// W5a a5-3: the master path's landing functions on Windows answer an error and record
/// nothing: a turn end stops at the worktree check, whose Windows path shape W5c makes
/// match, and the owner reads stop at engine git's not-ready refusal.
#[test]
fn the_master_landing_path_on_windows_records_nothing() {
    let mut place = place();
    place.workspace.agent.kind = AgentKind::Sub;
    let db = &place.db;
    let ws = &place.workspace;
    let home = place.home.as_path();
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "edit a".into(),
    };
    assert_eq!(
        code(record_master_start(
            db,
            &ws.repository,
            &ws.worktree,
            &ws.git_dir
        )),
        "changes_workspace_invalid"
    );
    assert_eq!(
        code(record_turn_end(db, ws, &turn)),
        "changes_workspace_invalid"
    );
    assert_eq!(
        code(record_commits(db, ws, &turn)),
        "changes_workspace_invalid"
    );
    assert_eq!(
        code(owner_git_identity(&ws.repository, home)),
        "changes_unavailable"
    );
    assert_eq!(
        code(checked_out_branch(&ws.repository)),
        "changes_unavailable"
    );
    assert!(open_change_set(db, &ws.worktree).unwrap().is_none());
    assert_eq!(number_change_sets(db).unwrap(), 0);
    assert!(
        !ws.repository
            .join(".git")
            .join("refs")
            .join("crazyagent")
            .exists()
    );
}

/// W5a a5-4: the landing functions that read one change set from the engine's store
/// answer on Windows over an empty store, as they do on macOS and Linux.
#[test]
fn the_store_reads_of_one_change_set_answer_on_windows() {
    let place = place();
    let db = &place.db;
    let ws = &place.workspace;
    assert_eq!(number_change_sets(db).unwrap(), 0);
    assert!(open_change_set(db, &ws.worktree).unwrap().is_none());
    assert!(
        change_set(db, &ChangeSetId("cs-1".into()))
            .unwrap()
            .is_none()
    );
}

/// W5a a5-5, PLAN-1336 D1: a file an agent may have replaced is read on Windows only when
/// it is a plain file, never through a symbolic link it was swapped for, as the Unix twin
/// opens it without following a link.
#[test]
fn a_bounded_read_on_windows_refuses_a_link_and_reads_a_plain_file() {
    let place = place();
    let folder = place.home.join("reads");
    std::fs::create_dir(&folder).unwrap();
    let secret = folder.join("secret.txt");
    std::fs::write(&secret, "owner bytes\n").unwrap();
    let plain = folder.join("plain.txt");
    std::fs::write(&plain, "plain\n").unwrap();
    let link = folder.join("gitdir");
    std::os::windows::fs::symlink_file(&secret, &link).unwrap();
    assert_eq!(super::flow::bounded_read(&plain, 64).unwrap(), b"plain\n");
    assert_eq!(
        code(super::flow::bounded_read(&link, 64)),
        "changes_workspace_invalid"
    );
}
