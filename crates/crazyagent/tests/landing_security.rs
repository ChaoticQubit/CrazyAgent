#![cfg(unix)]

//! Independent security acceptance tests for the landing engine (DEC-095, LAND-PLAN
//! security constraints). Written by a different worker than LAND-E so the author does not
//! write the check that decides its own completion. Real git, real engine functions, no
//! mocks: every test drives the public `crazyagent::landing` API the owner `changes`
//! protocol calls, against a real repository and a real linked worktree.

use crazyagent::engine::EngineError;
use crazyagent::landing::{
    self, AgentKind, ChangeAgent, ChangeSetId, FileKind, FileMark, TurnEnd, TurnOutcome, Workspace,
};
use crazyagent::protocol::ProjectId;
use rusqlite::Connection;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs git with no ambient configuration and with hooks and fsmonitor off, as the test's
/// own setup, never as the engine: the fixture never runs the hooks, filters or fsmonitor
/// it plants, so a marker can only come from the engine under test. Takes the working
/// directory and arguments. Returns stdout, panicking on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = crazyagent::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A private 0700 temporary directory under `/tmp`. Takes a prefix. Returns it, removed on
/// drop.
fn tmp(prefix: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

/// A real owner repository and the surrounding engine state a landing test needs.
struct Fixture {
    _root: tempfile::TempDir,
    engine: PathBuf,
    repo: PathBuf,
    owner_home: PathBuf,
    marker: PathBuf,
    project: ProjectId,
}

impl Fixture {
    /// Builds an owner repository on branch `main` with one base commit, an engine state
    /// folder holding `engine.db`'s directory, an owner home carrying the owner git
    /// identity, and a marker path OUTSIDE the repository that an attack would create.
    fn new() -> Self {
        let root = tmp("landing-security-");
        let engine = root.path().join("engine");
        std::fs::create_dir(&engine).unwrap();
        let owner_home = root.path().join("owner-home");
        std::fs::create_dir(&owner_home).unwrap();
        std::fs::write(
            owner_home.join(".gitconfig"),
            "[user]\n\tname = Owner Example\n\temail = owner@example.invalid\n",
        )
        .unwrap();
        let repo = root.path().join("project");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "Owner Example"]);
        git(&repo, &["config", "user.email", "owner@example.invalid"]);
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        git(&repo, &["add", "tracked.txt"]);
        git(&repo, &["commit", "-m", "base"]);
        Self {
            engine,
            repo,
            owner_home,
            marker: root.path().join("ATTACK_RAN"),
            project: ProjectId::new("landing-security").unwrap(),
            _root: root,
        }
    }

    /// Opens the landing change set store. Returns the connection; `changes_unavailable`
    /// (the stub) makes every test red here until LAND-E implements the store.
    fn store(&self) -> Connection {
        landing::open(&self.engine).expect("landing store opens")
    }

    /// Adds a linked worktree of the owner repository, as the engine does for an agent.
    /// Takes the worktree name. Returns its folder and its real git metadata dir under the
    /// repository's `.git/worktrees`, read from git itself rather than assumed.
    fn worktree(&self, name: &str) -> (PathBuf, PathBuf) {
        let worktree = self._root.path().join("worktrees").join(name);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let git_dir = PathBuf::from(
            git(&worktree, &["rev-parse", "--absolute-git-dir"])
                .trim()
                .to_owned(),
        );
        (worktree, git_dir)
    }

    /// The workspace for a sub-agent turn that ran in `worktree`. Takes the worktree and
    /// its git dir. Returns the workspace.
    fn workspace(&self, worktree: &Path, git_dir: &Path) -> Workspace {
        Workspace {
            project: self.project.clone(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "agent-1".into(),
                label: "worker".into(),
            },
            repository: self.repo.clone(),
            worktree: worktree.to_path_buf(),
            git_dir: git_dir.to_path_buf(),
        }
    }

    /// The current tip of the owner's `main` branch.
    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    /// Installs a repository `pre-commit` hook that creates the marker, in the shared
    /// common hooks dir every linked worktree uses.
    fn install_hook(&self) {
        let hooks = self.repo.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in [
            "pre-commit",
            "post-commit",
            "post-checkout",
            "post-index-change",
        ] {
            let path = hooks.join(hook);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\n/usr/bin/touch {}\nexit 0\n",
                    self.marker.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Configures a clean/smudge filter and a diff textconv, all of which create the marker
    /// if git ever runs them, plus the `.gitattributes` that binds every path to them.
    fn install_filter(&self, worktree: &Path) {
        let touch = format!("/bin/sh -c '/usr/bin/touch {}; cat'", self.marker.display());
        git(&self.repo, &["config", "filter.evil.clean", &touch]);
        git(&self.repo, &["config", "filter.evil.smudge", &touch]);
        git(&self.repo, &["config", "diff.evil.textconv", &touch]);
        std::fs::write(worktree.join(".gitattributes"), "* filter=evil diff=evil\n").unwrap();
    }

    /// Configures a core.fsmonitor hook that creates the marker if git runs it.
    fn install_fsmonitor(&self) {
        let script = self._root.path().join("fsmonitor.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n/usr/bin/touch {}\nexit 0\n",
                self.marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(
            &self.repo,
            &["config", "core.fsmonitor", script.to_str().unwrap()],
        );
    }

    /// Asserts the attack marker was never created, naming the step that should have blocked
    /// it.
    fn assert_marker_absent(&self, during: &str) {
        assert!(
            !self.marker.exists(),
            "an attacker's command ran during {during}: marker present"
        );
    }
}

/// A turn that ended well, for recording a change set.
fn turn() -> TurnEnd {
    TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "do the work".into(),
    }
}

/// Every file and its bytes under `dir`, excluding any `.git`, sorted by path, so two
/// snapshots compare byte for byte. Takes the directory. Returns the entries.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(base).unwrap().to_path_buf();
            if meta.file_type().is_dir() {
                walk(base, &path, out);
            } else if meta.file_type().is_symlink() {
                out.push((
                    relative,
                    std::fs::read_link(&path)
                        .unwrap()
                        .into_os_string()
                        .into_vec(),
                ));
            } else {
                out.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The owner checkout, as a refusal must leave it: its branch tip, its porcelain status and
/// every working-tree byte outside `.git`.
fn checkout_state(repo: &Path) -> (String, String, Vec<(PathBuf, Vec<u8>)>) {
    (
        git(repo, &["rev-parse", "HEAD"]).trim().to_owned(),
        git(repo, &["status", "--porcelain=v1"]),
        snapshot(repo),
    )
}

/// Records a change set from a real worktree edit, asserting one is pending. Takes the
/// fixture, the store, the workspace. Returns the change set id.
fn record_one(db: &Connection, workspace: &Workspace) -> ChangeSetId {
    landing::record_turn_end(db, workspace, &turn())
        .expect("record_turn_end")
        .expect("a change set is pending after an edit")
}

/// The head commit the owner reviews for a change set: the value Accept and Discard
/// require as their `expected_head`, read from the engine's own listing rather than
/// assumed to be the owner branch tip. Inputs: the store, the project and the change set.
/// Returns the head.
fn reviewed_head(db: &Connection, project: &ProjectId, id: &ChangeSetId) -> String {
    landing::list(db, project)
        .expect("list")
        .into_iter()
        .find(|view| &view.id == id)
        .expect("the change set is listed")
        .head
}

#[test]
fn no_repository_hook_runs_during_commit_diff_or_accept() {
    let fx = Fixture::new();
    fx.install_hook();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "agent wrote this\n").unwrap();
    let workspace = fx.workspace(&worktree, &git_dir);
    let id = record_one(&db, &workspace);
    fx.assert_marker_absent("record_turn_end");
    let _ = landing::diff(&db, &id, 0).expect("diff");
    fx.assert_marker_absent("diff");
    let head = reviewed_head(&db, &fx.project, &id);
    let _ = landing::accept(&db, &fx.owner_home, &id, &head, None).expect("accept");
    fx.assert_marker_absent("accept");
}

#[test]
fn no_filter_driver_runs_during_commit_diff_or_accept() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    fx.install_filter(&worktree);
    std::fs::write(worktree.join("filtered.txt"), "secret content\n").unwrap();
    let workspace = fx.workspace(&worktree, &git_dir);
    let id = record_one(&db, &workspace);
    fx.assert_marker_absent("record_turn_end");
    let _ = landing::diff(&db, &id, 0).expect("diff");
    fx.assert_marker_absent("diff");
    let head = reviewed_head(&db, &fx.project, &id);
    match landing::accept(&db, &fx.owner_home, &id, &head, None) {
        Ok(_) => {}
        Err(EngineError(code)) => assert_eq!(
            code, "changes_git_unsupported",
            "accept must refuse rather than run a configured filter driver"
        ),
    }
    fx.assert_marker_absent("accept");
}

#[test]
fn no_fsmonitor_runs_during_commit_or_diff() {
    let fx = Fixture::new();
    fx.install_fsmonitor();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "agent wrote this\n").unwrap();
    let workspace = fx.workspace(&worktree, &git_dir);
    let id = record_one(&db, &workspace);
    fx.assert_marker_absent("record_turn_end");
    let _ = landing::diff(&db, &id, 0).expect("diff");
    fx.assert_marker_absent("diff");
}

#[test]
fn a_crafted_worktree_dot_git_file_does_not_redirect_the_engine() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "real agent edit\n").unwrap();
    let attacker = fx._root.path().join("attacker-gitdir");
    let attacker_hooks = attacker.join("hooks");
    std::fs::create_dir_all(&attacker_hooks).unwrap();
    let hook = attacker_hooks.join("pre-commit");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\n/usr/bin/touch {}\nexit 0\n",
            fx.marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        attacker.join("config"),
        format!(
            "[core]\n\thooksPath = {}\n\tfsmonitor = {}\n",
            attacker_hooks.display(),
            hook.display()
        ),
    )
    .unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", attacker.display()),
    )
    .unwrap();
    let workspace = fx.workspace(&worktree, &git_dir);
    let id = record_one(&db, &workspace);
    fx.assert_marker_absent("record_turn_end with a crafted worktree .git file");
    let _ = landing::diff(&db, &id, 0).expect("diff");
    fx.assert_marker_absent("diff with a crafted worktree .git file");
}

#[test]
fn no_hook_or_filter_runs_during_update_or_discard() {
    let fx = Fixture::new();
    fx.install_hook();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    fx.install_filter(&worktree);
    std::fs::write(worktree.join("tracked.txt"), "agent changed base\n").unwrap();
    let workspace = fx.workspace(&worktree, &git_dir);
    let id = record_one(&db, &workspace);
    std::fs::write(fx.repo.join("tracked.txt"), "owner changed base\n").unwrap();
    git(&fx.repo, &["commit", "-am", "owner edit"]);
    let _ = landing::update(&db, &id);
    fx.assert_marker_absent("update");
    let _ = landing::discard(&db, &id, &fx.head(), 1_000);
    fx.assert_marker_absent("discard");
}

#[test]
fn crafted_tree_entries_are_marked_in_the_diff() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::os::unix::fs::symlink("/etc/passwd", worktree.join("link")).unwrap();
    let script = worktree.join("run.sh");
    std::fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(worktree.join(".gitattributes"), "*.txt text\n").unwrap();
    std::fs::write(
        worktree.join(".gitmodules"),
        "[submodule \"dep\"]\n\tpath = dep\n\turl = ./dep\n",
    )
    .unwrap();
    let dep = worktree.join("dep");
    std::fs::create_dir(&dep).unwrap();
    git(&dep, &["init", "-b", "main"]);
    git(&dep, &["config", "user.name", "Dep Example"]);
    git(&dep, &["config", "user.email", "dep@example.invalid"]);
    std::fs::write(dep.join("seed.txt"), "seed\n").unwrap();
    git(&dep, &["add", "seed.txt"]);
    git(&dep, &["commit", "-m", "dep base"]);
    let workspace = fx.workspace(&worktree, &git_dir);
    let id = record_one(&db, &workspace);
    let diff = landing::diff(&db, &id, 0).expect("diff");
    let kind = |path: &str| diff.files.iter().find(|f| f.path == path).map(|f| f.kind);
    let marks = |path: &str| {
        diff.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.marks.clone())
            .unwrap_or_default()
    };
    assert_eq!(kind("link"), Some(FileKind::Symlink), "{diff:?}");
    assert!(marks("link").contains(&FileMark::Symlink), "{diff:?}");
    assert!(marks("run.sh").contains(&FileMark::Executable), "{diff:?}");
    assert_eq!(kind("dep"), Some(FileKind::Gitlink), "{diff:?}");
    assert!(
        marks(".gitattributes").contains(&FileMark::Attributes),
        "{diff:?}"
    );
    assert!(
        marks(".gitmodules").contains(&FileMark::Modules),
        "{diff:?}"
    );
}

#[test]
fn a_change_set_over_the_tree_bound_refuses_and_lands_nothing() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    let mut deep = worktree.clone();
    for _ in 0..70 {
        deep = deep.join("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("file.txt"), "x\n").unwrap();
    let workspace = fx.workspace(&worktree, &git_dir);
    let before = fx.head();
    let refused = landing::record_turn_end(&db, &workspace, &turn());
    match refused {
        Err(EngineError(code)) => assert_eq!(code, "changes_too_large", "over-bound refusal code"),
        Ok(outcome) => panic!("a tree over the bound was accepted: {outcome:?}"),
    }
    assert_eq!(fx.head(), before, "nothing landed on the owner branch");
    assert!(
        landing::list(&db, &fx.project)
            .expect("list")
            .iter()
            .all(|set| set.state != landing::ChangeState::Pending),
        "no partial change set is presented as pending"
    );
}

#[test]
fn accept_refuses_a_dirty_owner_checkout_and_changes_nothing() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "agent edit\n").unwrap();
    let id = record_one(&db, &fx.workspace(&worktree, &git_dir));
    let expected = reviewed_head(&db, &fx.project, &id);
    std::fs::write(fx.repo.join("tracked.txt"), "owner uncommitted\n").unwrap();
    let before = checkout_state(&fx.repo);
    let err = landing::accept(&db, &fx.owner_home, &id, &expected, None).unwrap_err();
    assert_eq!(err.0, "changes_owner_dirty");
    assert_eq!(
        checkout_state(&fx.repo),
        before,
        "a refused accept changed the checkout"
    );
}

#[test]
fn accept_refuses_a_detached_owner_head_and_changes_nothing() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "agent edit\n").unwrap();
    let id = record_one(&db, &fx.workspace(&worktree, &git_dir));
    let expected = reviewed_head(&db, &fx.project, &id);
    git(&fx.repo, &["checkout", "--detach"]);
    let before = checkout_state(&fx.repo);
    let err = landing::accept(&db, &fx.owner_home, &id, &expected, None).unwrap_err();
    assert_eq!(err.0, "changes_detached_head");
    assert_eq!(
        checkout_state(&fx.repo),
        before,
        "a refused accept changed the checkout"
    );
}

#[test]
fn accept_refuses_a_mid_operation_owner_checkout_and_changes_nothing() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "agent edit\n").unwrap();
    let id = record_one(&db, &fx.workspace(&worktree, &git_dir));
    let expected = reviewed_head(&db, &fx.project, &id);
    std::fs::write(
        fx.repo.join(".git").join("MERGE_HEAD"),
        format!("{}\n", fx.head()),
    )
    .unwrap();
    let before = checkout_state(&fx.repo);
    let err = landing::accept(&db, &fx.owner_home, &id, &expected, None).unwrap_err();
    assert_eq!(err.0, "changes_git_busy");
    assert_eq!(
        checkout_state(&fx.repo),
        before,
        "a refused accept changed the checkout"
    );
}

#[test]
fn accept_moves_the_branch_only_by_compare_and_swap() {
    let fx = Fixture::new();
    let db = fx.store();
    let (worktree, git_dir) = fx.worktree("agent-1");
    std::fs::write(worktree.join("edit.txt"), "agent edit\n").unwrap();
    let id = record_one(&db, &fx.workspace(&worktree, &git_dir));
    let expected = reviewed_head(&db, &fx.project, &id);
    let unreviewed = fx.head();
    std::fs::write(fx.repo.join("owner.txt"), "owner moved main\n").unwrap();
    git(&fx.repo, &["add", "owner.txt"]);
    git(&fx.repo, &["commit", "-m", "owner commit"]);
    let moved = fx.head();
    assert_ne!(expected, unreviewed);
    let err = landing::accept(&db, &fx.owner_home, &id, &unreviewed, None).unwrap_err();
    assert!(
        err.0 == "changes_stale" || err.0 == "changes_branch_moved",
        "an expected head the owner did not review must be refused, got {}",
        err.0
    );
    assert_eq!(
        fx.head(),
        moved,
        "a refused compare-and-swap moved the branch"
    );
    match landing::accept(&db, &fx.owner_home, &id, &expected, None).expect("accept at the real tip") {
        landing::AcceptOutcome::Landed { commit, branch } => {
            assert_eq!(branch, "main");
            assert_eq!(fx.head(), commit, "the branch moved to the squash commit");
            assert_eq!(
                git(&fx.repo, &["rev-parse", &format!("{commit}^")]).trim(),
                moved,
                "the squash commit sits on the reviewed tip"
            );
            let author = git(&fx.repo, &["show", "-s", "--format=%an <%ae>", &commit]);
            assert_eq!(author.trim(), "Owner Example <owner@example.invalid>");
        }
        landing::AcceptOutcome::Conflict { files } => panic!("unexpected conflict: {files:?}"),
    }
}
