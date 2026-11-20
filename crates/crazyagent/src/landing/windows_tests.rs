use super::git::{GIT_RUNS, Repo, Writes};
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
    let mut command = Command::new("git");
    command
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
        .stderr(Stdio::null());
    let status = crate::spawn_gate::status(&mut command).unwrap();
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
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL); \
         INSERT INTO projects(id, body) VALUES ('project-1', '{}');",
    )
    .unwrap();
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
/// match; the owner's identity read stays refused, and the checked-out branch is a read of
/// the git folder alone (rv-sec3 #746 Q3b).
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
        checked_out_branch(&ws.repository).unwrap().as_deref(),
        Some("main")
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

/// The value a built engine git command sets for `name`, if any. Inputs: the command and the
/// variable. Output: the value.
fn set_in(command: &Command, name: &str) -> Option<std::ffi::OsString> {
    command
        .get_envs()
        .find(|(known, _)| known.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned))
}

/// W5b G1, rv-sec3 #746 Q3b: an engine git read on Windows names no work tree and starts in
/// the git folder it reads, never in a folder an agent writes, since Windows looks for a
/// program's libraries in its starting folder.
#[test]
fn a_windows_engine_read_names_no_work_tree_and_starts_in_the_git_folder() {
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::open(&ws.repository, &ws.git_dir, None).unwrap();
    let built = repo.built(&["rev-parse", "HEAD"]).unwrap();
    assert!(!built.get_args().any(|arg| arg == "--work-tree"));
    assert_eq!(built.get_current_dir(), Some(ws.git_dir.as_path()));
    assert_eq!(set_in(&built, "GIT_WORK_TREE"), None);
    let head = repo.capture(&["rev-parse", "HEAD"], 64).unwrap();
    assert_eq!(head.len(), 41);
}

/// W5b G1: a repository with a work tree and no confinement starts no git on Windows; a read
/// of a work tree is a step, which stays refused until its container exists.
#[test]
fn a_windows_engine_call_on_a_work_tree_without_a_step_is_refused_and_starts_nothing() {
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::open(&ws.repository, &ws.git_dir, Some(&ws.worktree)).unwrap();
    let before = GIT_RUNS.with(std::cell::Cell::get);
    assert_eq!(
        code(repo.exec(&["rev-parse", "HEAD"], None, &[], 64)),
        "changes_git_unsupported"
    );
    assert_eq!(GIT_RUNS.with(std::cell::Cell::get), before);
}

/// W5b G3: an engine git read on Windows clears its environment to the engine set, names the
/// null device as Windows spells it for every file it must not read, and reads no system,
/// global or other config outside the repository's own git folder.
#[test]
fn a_windows_engine_read_keeps_the_engine_settings_spelled_for_windows_and_reads_no_outside_config()
{
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::open(&ws.repository, &ws.git_dir, None).unwrap();
    let built = repo.built(&["version"]).unwrap();
    let allowed = [
        "PATH",
        "SystemRoot",
        "LC_ALL",
        "GIT_CONFIG_GLOBAL",
        "GIT_GRAFT_FILE",
        "GIT_NO_REPLACE_OBJECTS",
    ];
    for (name, _) in built.get_envs() {
        let name = name.to_str().unwrap();
        let safe = crate::research::runner::GIT_SAFE_ENV
            .iter()
            .any(|(known, _)| known.eq_ignore_ascii_case(name));
        assert!(
            safe || allowed.iter().any(|known| known.eq_ignore_ascii_case(name)),
            "engine git sets {name}"
        );
    }
    let null = Some(std::ffi::OsString::from(crate::github::NULL_DEVICE));
    assert_eq!(set_in(&built, "GIT_CONFIG_GLOBAL"), null);
    assert_eq!(set_in(&built, "GIT_GRAFT_FILE"), null);
    assert!(built.get_args().any(|arg| arg == "core.attributesFile=NUL"));
    assert!(
        !built
            .get_args()
            .any(|arg| arg.to_string_lossy().contains("/dev/null"))
    );
    let listing = repo
        .capture(&["config", "--show-origin", "--list"], 65536)
        .unwrap();
    let listing = String::from_utf8(listing).unwrap();
    for line in listing.lines() {
        let origin = line
            .split('\t')
            .next()
            .unwrap()
            .replace('\\', "/")
            .to_lowercase();
        assert!(
            origin.starts_with("command line:")
                || (origin.starts_with("file:") && origin.contains("/.git/")),
            "engine git read config from {origin}"
        );
    }
}

/// W5b G4: steps and reads run one git on Windows, the judged program the engine's own git
/// resolver finds (W3-5a), never a name a process search could match elsewhere.
#[test]
fn a_windows_engine_read_runs_the_one_judged_git() {
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::open(&ws.repository, &ws.git_dir, None).unwrap();
    let built = repo.built(&["version"]).unwrap();
    let judged = crate::worktrees::git_program().unwrap();
    assert_eq!(Path::new(built.get_program()), judged.as_path());
}

/// W5b G5, rv-sec2 N1: opening a repository on Windows refuses a config hook and any include
/// before any read the landing path makes, so no program the repository's config names runs.
#[test]
fn a_windows_repository_with_a_config_hook_or_an_include_is_refused_at_open() {
    let place = place();
    let ws = &place.workspace;
    git(
        &ws.repository,
        &["config", "hook.planted.command", "planted.exe"],
    );
    git(
        &ws.repository,
        &["config", "hook.planted.event", "pre-commit"],
    );
    assert_eq!(
        code(Repo::open(&ws.repository, &ws.git_dir, None)),
        "changes_git_hooks"
    );
    git(
        &ws.repository,
        &["config", "--remove-section", "hook.planted"],
    );
    assert!(Repo::open(&ws.repository, &ws.git_dir, None).is_ok());
    git(
        &ws.repository,
        &["config", "include.path", "planted.config"],
    );
    assert_eq!(
        code(Repo::open(&ws.repository, &ws.git_dir, None)),
        "changes_git_include"
    );
}

/// W5b G6: on Windows a read that looks at a work tree or an index an agent can write, such
/// as a work-tree compare or an untracked listing, is a step, as on Unix, and is refused
/// until the Windows step exists.
#[test]
fn a_windows_work_tree_compare_or_untracked_listing_is_a_step_and_stays_refused() {
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::open(&ws.repository, &ws.git_dir, Some(&ws.worktree)).unwrap();
    assert_eq!(code(repo.reading()), "changes_git_unsupported");
    for args in [
        &["diff-files", "--name-only"][..],
        &["ls-files", "--others", "--exclude-standard", "-z"][..],
    ] {
        assert_eq!(
            code(repo.exec(args, None, &[], 4096)),
            "changes_git_unsupported"
        );
    }
}

/// Whether a boundary profile may write the owner's git folder: a write root that is it,
/// lies in it or holds it. Inputs: the profile and the repository. Output: the answer.
fn writes_git(
    profile: &crazyagent_win_sandbox::profile::BoundaryProfile,
    repository: &Path,
) -> bool {
    let git = repository.join(".git").to_string_lossy().to_lowercase();
    profile.write_roots.iter().any(|root| {
        let root = root.to_lowercase();
        git == root
            || git.starts_with(&format!("{root}\\"))
            || root.starts_with(&format!("{git}\\"))
    })
}

/// W5b G2, rv-sec3 #746 Q3b: engine git reads the owner's git folder as the engine only while
/// no agent may write it. A profile built as the master's turn builds it, its worktree with the
/// repository read only, writes nothing under `<repo>\.git`; the check trips on a profile
/// that holds the repository; and a worktree checkout, which writes the git folder's
/// `worktrees` entry and objects, is refused on Windows.
#[test]
fn no_windows_agent_profile_writes_the_owner_git_folder_and_a_worktree_checkout_is_refused() {
    use crate::sandbox::windows::{GRANTS_FOLDER, boundary_profile};
    use crate::sandbox::{ProjectSandbox, SandboxNetwork};
    let place = place();
    let ws = &place.workspace;
    let root = ws.repository.parent().unwrap();
    let (tmp, grants) = (root.join("tmp"), root.join(GRANTS_FOLDER));
    for folder in [&tmp, &grants] {
        std::fs::create_dir(folder).unwrap();
    }
    let network = || SandboxNetwork::Only(Vec::new());
    let master = ProjectSandbox::new(&ws.worktree, &place.home, &tmp, network())
        .and_then(|sandbox| sandbox.allow_read(&ws.repository))
        .unwrap();
    let profile = boundary_profile(&master, &grants).unwrap();
    assert!(!profile.write_roots.is_empty());
    assert!(!writes_git(&profile, &ws.repository));
    let whole = ProjectSandbox::new(&ws.repository, &place.home, &tmp, network())
        .unwrap()
        .as_job_folder();
    assert!(writes_git(
        &boundary_profile(&whole, &grants).unwrap(),
        &ws.repository
    ));
    let worktrees = crate::projects_home::state_folder(&ws.repository).join("worktrees");
    std::fs::create_dir_all(&worktrees).unwrap();
    let checkout = worktrees.join("w1");
    git(
        &ws.repository,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "w1",
            checkout.to_str().unwrap(),
        ],
    );
    let checkout = crate::sandbox::canonical(&checkout).unwrap();
    let team = ProjectSandbox::new(&ws.repository, &place.home, &tmp, network()).unwrap();
    assert!(team.confine_to_worktree(&checkout).is_err());
}
