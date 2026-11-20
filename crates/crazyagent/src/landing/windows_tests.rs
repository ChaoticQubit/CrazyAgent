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

/// A git config file a row planted lines in, put back as it was when the row ends, the
/// file removed when there was none. Holds the path and its bytes before the row.
struct Planted(PathBuf, Option<Vec<u8>>);

impl Drop for Planted {
    fn drop(&mut self) {
        let _ = match &self.1 {
            Some(before) => std::fs::write(&self.0, before),
            None => std::fs::remove_file(&self.0),
        };
    }
}

/// Appends `lines` to the git config file at `path`, made when missing. Inputs: the file and
/// the lines. Output: the guard that puts the file back.
fn plant(path: PathBuf, lines: &str) -> Planted {
    let before = std::fs::read(&path).ok();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = before.clone().unwrap_or_default();
    bytes.extend_from_slice(lines.as_bytes());
    std::fs::write(&path, bytes).unwrap();
    Planted(path, before)
}

/// W5b G3: an engine git read on Windows clears its environment to the engine set, names the
/// null device as Windows spells it for every file it must not read, and reads no system,
/// global or other config outside the repository's own git folder. On the farm the row also
/// plants a marker in the machine's system and user git config, as W3-5a's row does, and the
/// landing read lists neither (rv-sec3 #746 Q3b row 3).
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
    let farm = std::env::var("GITHUB_ACTIONS").as_deref() == Ok("true");
    let _planted = farm.then(|| {
        let system = PathBuf::from(std::env::var_os("ProgramData").unwrap())
            .join("Git")
            .join("config");
        let global = PathBuf::from(std::env::var_os("USERPROFILE").unwrap()).join(".gitconfig");
        [
            plant(system, "\n[w5b]\n\tplanted = system\n"),
            plant(global, "\n[w5b]\n\tplanted = global\n"),
        ]
    });
    let listing = repo
        .capture(&["config", "--show-origin", "--list"], 65536)
        .unwrap();
    assert!(!String::from_utf8_lossy(&listing).contains("w5b"));
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

/// The places a W5b2 row's Windows step works in for the place's linked worktree, with the
/// engine data root in `data` and the runner's git. Inputs: the place and the data root.
/// Output: the step places.
fn step_places(place: &Place, data: &Path) -> super::step_windows::StepPlaces {
    let git = crate::worktrees::git_program().unwrap();
    super::step_windows::StepPlaces::for_lease(
        &place.workspace.git_dir,
        &place.workspace.worktree,
        data,
        &git,
    )
    .unwrap()
}

/// The write roots of one step kind's boundary profile, lowercased, sorted. Inputs: the step
/// places, the kind and the grants folder. Output: the roots.
fn step_writes(
    places: &super::step_windows::StepPlaces,
    writes: Writes,
    grants: &Path,
) -> Vec<String> {
    let policy = super::step_windows::policy(places, writes).unwrap();
    let mut roots: Vec<String> = crate::sandbox::windows::boundary_profile(&policy, grants)
        .unwrap()
        .write_roots
        .iter()
        .map(|root| root.to_lowercase())
        .collect();
    roots.sort();
    roots
}

/// Lowercased, sorted text of paths, to compare with [`step_writes`]. Input: the paths.
/// Output: the texts.
fn lowered(paths: &[PathBuf]) -> Vec<String> {
    let mut texts: Vec<String> = paths
        .iter()
        .map(|path| path.to_str().unwrap().to_lowercase())
        .collect();
    texts.sort();
    texts
}

/// W5b2a, rv-sec3 #746 C1, C2 and RULING 17:17 C-a: each Windows step kind writes only the
/// engine's step folder in the worktree's git folder, its own home in the W1-private lease
/// folder of the data root and its temp folder in the step folder, plus its kind's target: the
/// object store for an Objects step, the work tree for a WorkTree step, nothing for an Index
/// step. No kind writes the worktree's git folder itself, the common git folder or another
/// lease's places, and every kind reads the work tree, the object store and git's marked install
/// folder but no other part of the common git folder, so a junction an agent toggles after the
/// engine's walk reaches no config, hook or ref (rv-sec3 RULING 17:42 R2).
#[test]
fn a_windows_step_writes_only_its_step_folder_its_own_home_and_its_kind_target() {
    let place = place();
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    let grants = data.join(crate::sandbox::windows::GRANTS_FOLDER);
    std::fs::create_dir(&grants).unwrap();
    let places = step_places(&place, &data);
    let ws = &place.workspace;
    let common = ws.repository.join(".git");
    let step = ws.git_dir.join("crazyagent-step");
    assert_eq!(places.step, step);
    assert!(places.lease.starts_with(data.join("step-profiles")));
    assert!(
        !places.git.to_str().unwrap().starts_with(r"\\?\"),
        "a step starts git's plain program, the form the boundary takes"
    );
    assert!(places.git.starts_with(&places.git_root));
    let tmp = step.join("tmp");
    let home = |kind: &str| places.lease.join(format!("{kind}-home"));
    assert_eq!(
        step_writes(&places, Writes::Index, &grants),
        lowered(&[step.clone(), home("index"), tmp.clone()])
    );
    assert_eq!(
        step_writes(&places, Writes::Objects, &grants),
        lowered(&[
            step.clone(),
            common.join("objects"),
            home("objects"),
            tmp.clone()
        ])
    );
    assert_eq!(
        step_writes(&places, Writes::WorkTree, &grants),
        lowered(&[ws.worktree.clone(), home("worktree"), tmp])
    );
    for writes in [Writes::Index, Writes::Objects, Writes::WorkTree] {
        let policy = super::step_windows::policy(&places, writes).unwrap();
        let profile = crate::sandbox::windows::boundary_profile(&policy, &grants).unwrap();
        let reads: Vec<String> = profile
            .read_roots
            .iter()
            .map(|root| root.to_lowercase())
            .collect();
        let secret = common.join("config").to_str().unwrap().to_lowercase();
        assert!(
            !reads
                .iter()
                .chain(&profile.write_roots)
                .any(|root| secret.starts_with(&root.to_lowercase())),
            "no step reaches the common git folder beyond its objects (RULING 17:42 R2)"
        );
        for read in [&ws.worktree, &common.join("objects"), &places.git_root] {
            let read = read.to_str().unwrap().to_lowercase();
            assert!(
                reads.contains(&read)
                    || profile
                        .write_roots
                        .iter()
                        .any(|root| root.to_lowercase() == read),
                "a step reads {read}"
            );
        }
        let private = ws.git_dir.to_str().unwrap().to_lowercase();
        assert!(
            !profile
                .write_roots
                .iter()
                .any(|root| root.to_lowercase() == private),
            "no step writes the worktree's git folder itself"
        );
    }
}

/// W5b2a, rv-sec3 #746 C2: the three step kinds of one lease run in three containers, none of
/// them owner git's for the project or an agent's for the worktree, and each a name the boundary
/// accepts.
#[test]
fn the_three_windows_step_containers_differ_from_each_other_owner_git_and_agents() {
    let place = place();
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    let grants = data.join(crate::sandbox::windows::GRANTS_FOLDER);
    std::fs::create_dir(&grants).unwrap();
    let places = step_places(&place, &data);
    let ws = &place.workspace;
    let names: Vec<String> = [Writes::Index, Writes::Objects, Writes::WorkTree]
        .into_iter()
        .map(|writes| {
            let policy = super::step_windows::policy(&places, writes).unwrap();
            crate::sandbox::windows::boundary_profile(&policy, &grants)
                .unwrap()
                .container
        })
        .collect();
    let owner = crate::sandbox::windows::owner_git::places(&data.join("owner-git"), &ws.repository);
    let others = [
        crate::sandbox::windows::container_for(&ws.repository, &owner.home),
        crate::sandbox::windows::container_for(&ws.worktree, &place.home),
    ];
    for (at, name) in names.iter().enumerate() {
        crazyagent_win_sandbox::profile::check_container(name).unwrap();
        assert!(
            !others.contains(name),
            "a step container is no other container"
        );
        assert!(!names[at + 1..].contains(name), "the kinds differ");
    }
}

/// W5b2a, rv-sec3 RULING 17:17 C-c: a container name is never reused. A worktree removed and
/// made again at the same path is another lease, with another key and other containers, so an
/// entry a missed withdrawal left names a SID no later step runs as.
#[test]
fn a_windows_worktree_made_again_at_the_same_path_gets_new_step_containers() {
    let place = place();
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    let grants = data.join(crate::sandbox::windows::GRANTS_FOLDER);
    std::fs::create_dir(&grants).unwrap();
    let ws = &place.workspace;
    let containers = |places: &super::step_windows::StepPlaces| -> Vec<String> {
        [Writes::Index, Writes::Objects, Writes::WorkTree]
            .into_iter()
            .map(|writes| {
                let policy = super::step_windows::policy(places, writes).unwrap();
                crate::sandbox::windows::boundary_profile(&policy, &grants)
                    .unwrap()
                    .container
            })
            .collect()
    };
    let first = step_places(&place, &data);
    let key = super::step_windows::lease_key(&ws.worktree).unwrap();
    assert_eq!(key, super::step_windows::lease_key(&ws.worktree).unwrap());
    let before = containers(&first);
    let worktree = ws.worktree.to_str().unwrap();
    git(&ws.repository, &["worktree", "remove", "--force", worktree]);
    git(
        &ws.repository,
        &["worktree", "add", "-q", "-B", "agent", worktree],
    );
    let again = step_places(&place, &data);
    assert_ne!(super::step_windows::lease_key(&ws.worktree).unwrap(), key);
    assert_ne!(again.lease, first.lease);
    for name in containers(&again) {
        assert!(!before.contains(&name), "a container name is never reused");
    }
}

/// W5b2a, rv-sec3 RULING 17:17 C-a: a step profile is read back through W1 before it is used
/// again, so a profile file replaced by a link, or one that carries another account's entry,
/// stops the step with `changes_git_unsupported`, while a profile the engine wrote installs
/// again.
#[test]
fn a_windows_step_profile_that_is_a_link_or_names_another_account_is_refused() {
    let place = place();
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    let grants = data.join(crate::sandbox::windows::GRANTS_FOLDER);
    std::fs::create_dir(&grants).unwrap();
    let places = step_places(&place, &data);
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .with_file_name(crate::sandbox::windows::HELPER_NAME);
    assert!(helper.is_file(), "build crazyagent-win-boundary first");
    let install = || super::step_windows::installed(&places, Writes::Index, &helper, &grants);
    install().unwrap();
    install().unwrap();
    let profile = places.lease.join("index.sb");
    assert!(crazyagent_win_sandbox::private::read_only(&profile).is_ok());
    let elsewhere = data.join("elsewhere.sb");
    std::fs::copy(&profile, &elsewhere).unwrap();
    std::fs::remove_file(&profile).unwrap();
    std::os::windows::fs::symlink_file(&elsewhere, &profile).unwrap();
    assert_eq!(code(install()), "changes_git_unsupported");
    std::fs::remove_file(&profile).unwrap();
    install().unwrap();
    let granted = crate::spawn_gate::collected(
        Command::new("icacls")
            .arg(&profile)
            .args(["/grant", "*S-1-1-0:(M)"])
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(granted.status.success());
    assert_eq!(code(install()), "changes_git_unsupported");
}

/// Makes a junction at `link` to `target` with the runner's `mklink /J`. Inputs: the link and
/// its target. Output: none; a failed `mklink` fails the row.
fn junction(link: &Path, target: &Path) {
    let made = crate::spawn_gate::collected(
        Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(made.status.success(), "mklink /J {link:?}");
}

/// W5b2b, rv-sec3 RULING 17:42 C3 condition A: before a step scans the work tree, the engine
/// walks it without following anything and refuses at the first reparse point, so a junction
/// an agent planted to the common git folder, one nested two folders deep, or a folder symlink
/// stops the step with `changes_workspace_invalid`, while the plain work tree with its own
/// `.git` file passes.
#[test]
fn a_windows_work_tree_holding_a_junction_or_a_folder_symlink_is_refused_before_a_scan() {
    let place = place();
    let ws = &place.workspace;
    let common = ws.repository.join(".git");
    assert!(ws.worktree.join(".git").is_file());
    assert_eq!(code(super::step_windows::reparse_free(&ws.worktree)), "ok");
    let planted = ws.worktree.join("jn-git");
    junction(&planted, &common);
    assert_eq!(
        code(super::step_windows::reparse_free(&ws.worktree)),
        "changes_workspace_invalid"
    );
    std::fs::remove_dir(&planted).unwrap();
    assert_eq!(code(super::step_windows::reparse_free(&ws.worktree)), "ok");
    let deep = ws.worktree.join("one").join("two");
    std::fs::create_dir_all(&deep).unwrap();
    junction(&deep.join("jn"), &common);
    assert_eq!(
        code(super::step_windows::reparse_free(&ws.worktree)),
        "changes_workspace_invalid"
    );
    std::fs::remove_dir(deep.join("jn")).unwrap();
    std::os::windows::fs::symlink_dir(&common, ws.worktree.join("ln-git")).unwrap();
    assert_eq!(
        code(super::step_windows::reparse_free(&ws.worktree)),
        "changes_workspace_invalid"
    );
    std::fs::remove_dir(ws.worktree.join("ln-git")).unwrap();
    assert_eq!(code(super::step_windows::reparse_free(&ws.worktree)), "ok");
}

/// W5b2b, rv-sec3 RULING 17:42 C3 condition A: the walk itself is bounded and held to a plain
/// root. A work tree that is a junction is refused with `changes_workspace_invalid`, and a work
/// tree deeper than the walk's depth bound is refused with `changes_workspace_capacity`.
#[test]
fn a_windows_work_tree_walk_refuses_a_junction_root_and_stops_at_its_bound() {
    let place = place();
    let ws = &place.workspace;
    let elsewhere = place.home.join("root-link");
    junction(&elsewhere, &ws.worktree);
    assert_eq!(
        code(super::step_windows::reparse_free(&elsewhere)),
        "changes_workspace_invalid"
    );
    let mut deep = ws.worktree.clone();
    for _ in 0..=super::step_windows::WALK_DEPTH_MAX {
        deep.push("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(
        code(super::step_windows::reparse_free(&ws.worktree)),
        "changes_workspace_capacity"
    );
}

/// W5b2b, rv-sec3 RULING 17:42 R2-b: the shadow git folder's config is an allowlist the engine
/// writes from the owner's listing. The ten keys that shape what `add`, refresh, `diff-files`
/// and `ls-files` produce are kept, the last value winning as git reads it; a remote URL with a
/// token, a credential helper, a hooks path, an fsmonitor, an include, a URL rewrite, an HTTP
/// header and an SSH command are all left out; a kept key whose value is not a plain word
/// stops the step.
#[test]
fn a_windows_shadow_config_keeps_only_the_keys_that_shape_a_scan() {
    let listing = b"core.autocrlf\nfalse\0core.autocrlf\ninput\0core.quotepath\nfalse\0core.checkroundtripencoding\nSHIFT-JIS\0core.symlinks\0remote.origin.url\nhttps://secret-token@example.invalid/r.git\0credential.helper\nstore\0core.hookspath\nhooks-elsewhere\0core.fsmonitor\ntrue\0include.path\nelsewhere.cfg\0url.https://x.invalid/.insteadof\ngit://y\0http.extraheader\nAuthorization: secret\0core.sshcommand\nssh -i key\0";
    let text = super::step_windows::shadow_config(listing).unwrap();
    assert_eq!(
        text,
        "[core]\n\tautocrlf = input\n\tquotepath = false\n\tcheckroundtripencoding = SHIFT-JIS\n\tsymlinks = true\n"
    );
    for left in [
        "secret",
        "store",
        "hooks",
        "fsmonitor",
        "include",
        "insteadof",
        "ssh",
    ] {
        assert!(!text.to_lowercase().contains(left), "{left} left out");
    }
    assert_eq!(
        code(super::step_windows::shadow_config(b"core.eol\nlf\n[x]\0")),
        "changes_git_unsupported"
    );
}

/// W5b2b, rv-sec3 RULING 17:42 R2-b and R2-c: the engine writes the shadow git folder in the
/// step folder through W1, holding only a detached `HEAD` at the step's commit, an empty
/// `refs` folder and the allowlisted config made from the owner's own config file, which a
/// planted remote URL with a token never reaches. Git reads it as a repository whose objects
/// are the owner's object store, named by `GIT_OBJECT_DIRECTORY`.
#[test]
fn a_windows_shadow_git_folder_holds_a_detached_head_and_the_allowlisted_config_only() {
    let place = place();
    let ws = &place.workspace;
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    git(
        &ws.repository,
        &[
            "config",
            "remote.origin.url",
            "https://secret-token@example.invalid/r.git",
        ],
    );
    git(&ws.repository, &["config", "core.autocrlf", "input"]);
    let repo = Repo::open(&ws.repository, &ws.git_dir, None).unwrap();
    let head = String::from_utf8(repo.capture(&["rev-parse", "HEAD"], 128).unwrap()).unwrap();
    let head = head.trim_end();
    let places = step_places(&place, &data);
    let shadow = super::step_windows::write_shadow(&places, &repo, head).unwrap();
    assert_eq!(shadow, places.step.join("git"));
    let mut names: Vec<String> = std::fs::read_dir(&shadow)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["HEAD", "config", "refs"]);
    assert_eq!(std::fs::read_dir(shadow.join("refs")).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_to_string(shadow.join("HEAD")).unwrap(),
        format!("{head}\n")
    );
    let config = std::fs::read_to_string(shadow.join("config")).unwrap();
    assert!(config.contains("autocrlf = input"));
    assert!(!config.contains("secret") && !config.contains("remote"));
    for name in ["HEAD", "config"] {
        assert!(crazyagent_win_sandbox::private::read_only(&shadow.join(name)).is_ok());
    }
    let read = crate::spawn_gate::collected(
        Command::new("git")
            .env("GIT_DIR", &shadow)
            .env(
                "GIT_OBJECT_DIRECTORY",
                ws.repository.join(".git").join("objects"),
            )
            .args(["rev-parse", "HEAD^{tree}"])
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(
        read.status.success(),
        "{:?}",
        String::from_utf8_lossy(&read.stderr)
    );
    let again = super::step_windows::write_shadow(&places, &repo, head).unwrap();
    assert_eq!(again, shadow);
    assert_eq!(
        code(super::step_windows::write_shadow(&places, &repo, "HEAD")),
        "changes_workspace_invalid"
    );
}

/// W5b2b, the WorkTree HIGH of probe r2 and r4: no step kind's profile guards a path inside the
/// work tree. A guarded path the boundary cannot find, such as `.git\hooks` under a linked
/// worktree whose `.git` is a file, makes it keep the root from taking new entries while
/// leaving its files open to delete, so a checkout unlinked `a.txt` and could not create it
/// again. Like the Linux step, the WorkTree kind writes the whole work tree as a job folder.
#[test]
fn no_windows_step_profile_guards_a_path_inside_the_work_tree() {
    let place = place();
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    let grants = data.join(crate::sandbox::windows::GRANTS_FOLDER);
    std::fs::create_dir(&grants).unwrap();
    let places = step_places(&place, &data);
    let work = place.workspace.worktree.to_str().unwrap().to_lowercase();
    for writes in [Writes::Index, Writes::Objects, Writes::WorkTree] {
        let policy = super::step_windows::policy(&places, writes).unwrap();
        let profile = crate::sandbox::windows::boundary_profile(&policy, &grants).unwrap();
        let inside: Vec<&str> = profile
            .guarded
            .iter()
            .map(|guarded| guarded.path.as_str())
            .filter(|path| path.to_lowercase().starts_with(&work))
            .collect();
        assert!(inside.is_empty(), "guards inside the work tree: {inside:?}");
    }
}

/// W5b2b, rv-sec3 RULING 18:04 E-1: the empty file a non-Index step names as its excludes file,
/// since git stops on `NUL` there (probe r4), is W1-private in the lease's folder of the data
/// root, outside every step kind's write roots, so no step git can plant a pattern that hides a
/// path from a later turn commit. A file the engine finds holding anything stops the step.
#[test]
fn the_windows_no_excludes_file_sits_where_no_step_container_writes() {
    let place = place();
    let data = place.home.parent().unwrap().join("data");
    std::fs::create_dir(&data).unwrap();
    let grants = data.join(crate::sandbox::windows::GRANTS_FOLDER);
    std::fs::create_dir(&grants).unwrap();
    let places = step_places(&place, &data);
    let empty = super::step_windows::no_excludes(&places).unwrap();
    assert_eq!(super::step_windows::no_excludes(&places).unwrap(), empty);
    assert!(empty.starts_with(&places.lease));
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);
    assert!(crazyagent_win_sandbox::private::read_only(&empty).is_ok());
    let named = empty.to_str().unwrap().to_lowercase();
    for writes in [Writes::Index, Writes::Objects, Writes::WorkTree] {
        let policy = super::step_windows::policy(&places, writes).unwrap();
        let profile = crate::sandbox::windows::boundary_profile(&policy, &grants).unwrap();
        assert!(
            !profile
                .write_roots
                .iter()
                .any(|root| named.starts_with(&root.to_lowercase())),
            "no step kind writes the no-excludes file"
        );
    }
    std::fs::write(&empty, "*\n").unwrap();
    assert_eq!(
        code(super::step_windows::no_excludes(&places)),
        "changes_git_unsupported"
    );
}

/// W5b2b, rv-sec3 RULING 18:04 E-2: an engine read that reads attributes or grafts answers on
/// Windows with the null device the engine names for `core.attributesFile` and
/// `GIT_GRAFT_FILE`, as the excludes file does not: `check-attr` on the commit, a commit count
/// and a commit's patch all succeed.
#[test]
fn a_windows_engine_read_of_attributes_or_grafts_answers_with_the_null_device() {
    let place = place();
    let ws = &place.workspace;
    let repo = Repo::open(&ws.repository, &ws.git_dir, None).unwrap();
    let attributes = repo
        .capture(&["check-attr", "--source", "HEAD", "-a", "a.txt"], 4096)
        .unwrap();
    assert!(
        attributes.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&attributes)
    );
    assert_eq!(
        repo.capture(&["rev-list", "--count", "HEAD"], 64).unwrap(),
        b"1\n"
    );
    let patch = repo
        .capture(
            &["log", "-1", "-p", "--format=%H", "HEAD", "--", "a.txt"],
            4096,
        )
        .unwrap();
    assert!(String::from_utf8_lossy(&patch).contains("+one"));
}

/// W5c1, the r2 finding W5c inherits: a plain Windows linked worktree, as the runner's git makes
/// it, passes the place check as `Linked`. Its paths are the engine's plain drive form, not the
/// verbatim `\\?\` form `canonicalize` returns, and git writes the admin folder's `gitdir` with
/// forward slashes. An admin folder whose `gitdir` names another folder's `.git` stays refused.
#[test]
fn a_windows_linked_worktree_passes_the_place_check_as_linked() {
    let place = place();
    let ws = &place.workspace;
    let gitdir = std::fs::read_to_string(ws.git_dir.join("gitdir")).unwrap();
    assert!(gitdir.contains('/'), "git writes {gitdir:?}");
    let layout = super::flow::check_places(&ws.repository, &ws.worktree, &ws.git_dir);
    assert!(
        matches!(layout, Ok(crate::agent_git::Layout::Linked)),
        "gitdir {gitdir:?}, worktree {:?}, answer {}",
        ws.worktree,
        layout.as_ref().map_or_else(
            |error| refusal_code(error).to_owned(),
            |found| format!("{found:?}")
        )
    );
    let elsewhere = place.home.join("other");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(
        ws.git_dir.join("gitdir"),
        format!(
            "{}\n",
            elsewhere.join(".git").to_str().unwrap().replace('\\', "/")
        ),
    )
    .unwrap();
    assert_eq!(
        code(super::flow::check_places(
            &ws.repository,
            &ws.worktree,
            &ws.git_dir
        )),
        "changes_workspace_invalid"
    );
}
