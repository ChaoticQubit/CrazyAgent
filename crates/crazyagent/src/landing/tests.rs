use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const NOW_MS: u64 = 1_800_000_000_000;

/// One owner repository with one agent worktree added from it, an owner home holding
/// no global git config, and an engine root holding the change set store.
struct Case {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    db: Connection,
    workspace: Workspace,
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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

impl Case {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home, engine) = (root.join("repo"), root.join("home"), root.join("engine"));
        for folder in [&repo, &home, &engine] {
            std::fs::create_dir(folder).unwrap();
        }
        git_in(&home, &repo, &["init", "-q", "--template=", "-b", "main"]);
        git_in(&home, &repo, &["config", "user.name", "Repo Owner"]);
        git_in(
            &home,
            &repo,
            &["config", "user.email", "owner@example.invalid"],
        );
        write(&repo.join("README"), "one\ntwo\nthree\n");
        git_in(&home, &repo, &["add", "README"]);
        git_in(&home, &repo, &["commit", "-q", "-m", "Start"]);
        let worktree = root.join("agent-1");
        git_in(
            &home,
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        let db = open(&engine).unwrap();
        let workspace = Workspace {
            project: ProjectId::new("project-1").unwrap(),
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: "agent-1".into(),
                label: "Agent 1".into(),
            },
            repository: repo.clone(),
            worktree,
            git_dir: repo.join(".git/worktrees/agent-1"),
        };
        Self {
            _tmp: tmp,
            repo,
            home,
            db,
            workspace,
        }
    }

    fn owner(&self, args: &[&str]) -> String {
        git_in(&self.home, &self.repo, args)
    }

    fn agent(&self, args: &[&str]) -> String {
        git_in(&self.home, &self.workspace.worktree, args)
    }

    fn edit(&self, path: &str, text: &str) {
        write(&self.workspace.worktree.join(path), text);
    }

    fn owner_commit(&self, path: &str, text: &str, message: &str) -> String {
        write(&self.repo.join(path), text);
        self.owner(&["add", path]);
        self.owner(&["commit", "-q", "-m", message]);
        self.tip()
    }

    fn tip(&self) -> String {
        self.owner(&["rev-parse", "main"]).trim().to_owned()
    }

    fn start(&self) {
        record_worktree_start(
            &self.db,
            &self.workspace.repository,
            &self.workspace.worktree,
            &self.workspace.git_dir,
        )
        .unwrap();
    }

    fn agent_commit(&self, message: &str) -> String {
        self.agent(&["add", "-A"]);
        self.agent(&[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ]);
        self.agent_head()
    }

    /// Commits one file the way an agent can with a private index, leaving the
    /// worktree's own index and files at the commit before, so only the committed
    /// history holds the file. Inputs: the path bytes and the text. Output: the new HEAD.
    fn agent_plumbing_commit(&self, path: &[u8], text: &str) -> String {
        let scratch = self.home.join("plumbing");
        std::fs::create_dir_all(&scratch).unwrap();
        let (content, index) = (scratch.join("blob"), scratch.join("index"));
        std::fs::write(&content, text).unwrap();
        let _ = std::fs::remove_file(&index);
        let git = |args: &[&OsStr]| -> String {
            let output = crate::spawn_gate::output(
                Command::new("/usr/bin/git")
                    .env_clear()
                    .env("PATH", "/usr/bin:/bin")
                    .env("HOME", &self.home)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_INDEX_FILE", &index)
                    .env("LC_ALL", "C")
                    .arg("-C")
                    .arg(&self.workspace.worktree)
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
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let os = OsStr::new;
        let blob = git(&[os("hash-object"), os("-w"), content.as_os_str()]);
        git(&[os("read-tree"), os("HEAD")]);
        git(&[
            os("update-index"),
            os("--add"),
            os("--cacheinfo"),
            os("100644"),
            os(&blob),
            OsStr::from_bytes(path),
        ]);
        let tree = git(&[os("write-tree")]);
        let commit = git(&[
            os("-c"),
            os("user.name=Agent"),
            os("-c"),
            os("user.email=agent@example.invalid"),
            os("commit-tree"),
            os(&tree),
            os("-p"),
            os("HEAD"),
            os("-m"),
            os("Add through plumbing"),
        ]);
        git(&[os("update-ref"), os("HEAD"), os(&commit)]);
        self.agent_head()
    }

    fn agent_head(&self) -> String {
        self.agent(&["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn turn(&self, outcome: TurnOutcome) -> Option<ChangeSetId> {
        self.turn_with(outcome, "Add the fourth line\nand more")
    }

    fn turn_with(&self, outcome: TurnOutcome, prompt: &str) -> Option<ChangeSetId> {
        let turn = TurnEnd {
            outcome,
            turn: "turn-1".into(),
            prompt_line: prompt.into(),
        };
        record_turn_end(&self.db, &self.workspace, &turn).unwrap()
    }

    fn view(&self, id: &ChangeSetId) -> ChangeSetView {
        list(&self.db, &self.workspace.project)
            .unwrap()
            .into_iter()
            .find(|view| &view.id == id)
            .unwrap()
    }

    fn accept(&self, id: &ChangeSetId) -> Result<AcceptOutcome, EngineError> {
        self.accept_with(id, None)
    }

    fn accept_with(
        &self,
        id: &ChangeSetId,
        message: Option<&str>,
    ) -> Result<AcceptOutcome, EngineError> {
        accept(&self.db, &self.home, id, &self.view(id).head, message)
    }

    fn landed_message(&self, commit: &str) -> String {
        self.owner(&["log", "-1", "--format=%B", commit])
            .trim_end()
            .to_owned()
    }

    /// Hashes everything an owner sees in the checkout: its files, HEAD, index, refs
    /// and git state files. Objects and agent worktree metadata are left out, since a
    /// refused Accept may write unreachable objects. Output: one digest per path.
    fn checkout(&self) -> BTreeMap<String, String> {
        let mut seen = BTreeMap::new();
        let mut pending = vec![self.repo.clone()];
        while let Some(path) = pending.pop() {
            let relative = path.strip_prefix(&self.repo).unwrap().to_string_lossy();
            if [".git/objects", ".git/worktrees"].contains(&relative.as_ref()) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let digest = if metadata.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    pending.push(entry.unwrap().path());
                }
                format!("dir {:o}", metadata.mode())
            } else if metadata.file_type().is_symlink() {
                format!("link {:?}", std::fs::read_link(&path).unwrap())
            } else {
                let bytes = std::fs::read(&path).unwrap();
                format!("{:o} {:x}", metadata.mode(), Sha256::digest(bytes))
            };
            seen.insert(relative.into_owned(), digest);
        }
        seen
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, EngineError>) -> String {
    result.unwrap_err().0
}

#[test]
fn a_turn_that_edits_the_worktree_becomes_one_pending_change_set_the_engine_committed() {
    let case = Case::new();
    let base = case.tip();
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&id);
    assert_eq!(
        (
            view.state,
            view.base.as_str(),
            view.files,
            view.added,
            view.removed
        ),
        (ChangeState::Pending, base.as_str(), 2, 2, 0)
    );
    assert_eq!(
        (view.last_outcome, view.checked),
        (TurnOutcome::Succeeded, false)
    );
    assert_eq!(view.agent, case.workspace.agent);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%an <%ae>|%cn <%ce>|%s", &view.head])
            .trim(),
        "CrazyAgent <engine@crazyagent.invalid>|CrazyAgent <engine@crazyagent.invalid>|Add the fourth line"
    );
    assert_eq!(case.tip(), base, "the owner's branch moved");
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\n"
    );
    case.edit("README", "zero\none\ntwo\nthree\nfour\n");
    assert_eq!(case.turn(TurnOutcome::Failed), Some(id.clone()));
    let later = case.view(&id);
    assert_ne!(later.head, view.head);
    assert_eq!(
        (later.files, later.added, later.last_outcome),
        (2, 3, TurnOutcome::Failed)
    );
    assert_eq!(case.turn(TurnOutcome::Cancelled), Some(id.clone()));
    assert_eq!(
        case.view(&id).head,
        later.head,
        "a turn without changes made a commit"
    );
    let diff = diff(&case.db, &id, 0).unwrap();
    let files: Vec<_> = diff
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status))
        .collect();
    assert_eq!(
        files,
        [
            ("README", FileStatus::Modified),
            ("src/new.rs", FileStatus::Added)
        ]
    );
    assert!(
        diff.text.contains("+four\n") && diff.text.contains("+fn main() {}\n"),
        "{}",
        diff.text
    );
    assert_eq!((diff.next, diff.truncated), (None, false));
}

#[test]
fn accept_lands_one_squash_commit_authored_by_the_repository_identity_on_the_checked_out_branch() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.edit("src/new.rs", "fn main() {}\n");
    case.turn(TurnOutcome::Succeeded).unwrap();
    let before = case.owner_commit("other.txt", "owner work\n", "Owner work");
    let AcceptOutcome::Landed { commit, branch } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%an <%ae>|%cn <%ce>|%s", &commit])
            .trim(),
        format!(
            "{before}|Repo Owner <owner@example.invalid>|Repo Owner <owner@example.invalid>|Add the fourth line"
        )
    );
    assert!(
        !case
            .owner(&["log", "-1", "--format=%B", &commit])
            .contains("CrazyAgent")
    );
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &commit]),
        "README\nother.txt\nsrc/new.rs\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(case.view(&id).state, ChangeState::Accepted);
    assert_eq!(case.agent(&["rev-parse", "HEAD"]).trim(), commit);
    assert_eq!(case.agent(&["status", "--porcelain"]), "");
    assert_eq!(refused(case.accept(&id)), "changes_stale");
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
}

#[test]
fn a_dirty_detached_busy_or_conflicting_checkout_refuses_accept_and_changes_nothing() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let check = |change: &dyn Fn(), undo: &dyn Fn(), code: &str| {
        change();
        let before = case.checkout();
        assert_eq!(refused(case.accept(&id)), code);
        assert_eq!(case.checkout(), before, "{code} changed the checkout");
        undo();
    };
    check(
        &|| write(&case.repo.join("README"), "owner edit\n"),
        &|| {
            case.owner(&["checkout", "-q", "--", "README"]);
        },
        "changes_owner_dirty",
    );
    check(
        &|| {
            write(&case.repo.join("README"), "owner staged\n");
            case.owner(&["add", "README"]);
        },
        &|| {
            case.owner(&["reset", "-q", "--hard"]);
        },
        "changes_owner_dirty",
    );
    check(
        &|| write(&case.repo.join("src/new.rs"), "owner's untracked file\n"),
        &|| std::fs::remove_dir_all(case.repo.join("src")).unwrap(),
        "changes_owner_dirty",
    );
    check(
        &|| {
            case.owner(&["checkout", "-q", "--detach"]);
        },
        &|| {
            case.owner(&["checkout", "-q", "main"]);
        },
        "changes_detached_head",
    );
    let busy = [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "BISECT_LOG",
        "index.lock",
    ];
    for name in busy {
        let path = case.repo.join(".git").join(name);
        check(
            &|| write(&path, &format!("{}\n", case.tip())),
            &|| std::fs::remove_file(&path).unwrap(),
            "changes_git_busy",
        );
    }
    for name in ["rebase-merge", "rebase-apply", "sequencer"] {
        let path = case.repo.join(".git").join(name);
        check(
            &|| std::fs::create_dir(&path).unwrap(),
            &|| std::fs::remove_dir(&path).unwrap(),
            "changes_git_busy",
        );
    }
    assert_eq!(
        refused(accept(&case.db, &case.home, &id, &case.tip(), None)),
        "changes_stale"
    );
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    let before = case.checkout();
    assert_eq!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict {
            files: vec!["README".into()]
        }
    );
    assert_eq!(case.checkout(), before, "the conflict changed the checkout");
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.head, view.conflicts),
        (ChangeState::Conflict, head, vec!["README".to_owned()])
    );
}

#[test]
fn update_merges_the_owner_branch_into_the_agent_worktree_with_conflict_markers() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let owner_tip = case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    update(&case.db, &id).unwrap();
    let merged = std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap();
    assert!(
        merged.contains("<<<<<<<") && merged.contains("=======") && merged.contains(">>>>>>>"),
        "{merged}"
    );
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.base.as_str()),
        (ChangeState::Updating, owner_tip.as_str())
    );
    let prompt = resolve_task_prompt(&case.db, &id).unwrap();
    assert!(
        prompt.contains("README") && prompt.contains("conflict"),
        "{prompt}"
    );
    assert_eq!(refused(case.accept(&id)), "changes_stale");
    case.edit("README", "one\ntwo\nTHREE\nfour\n");
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the resolved change set conflicted again");
    };
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &commit]).trim(),
        owner_tip
    );
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nTHREE\nfour\n"
    );
}

#[test]
fn update_refuses_while_the_agent_has_unrecorded_changes() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    case.edit("notes.txt", "still working\n");
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(case.view(&id).state, ChangeState::Conflict);
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
}

#[test]
fn discard_keeps_the_work_restorable_for_seven_days_and_restore_brings_it_back() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let tip = case.owner_commit("other.txt", "owner work\n", "Owner work");
    assert_eq!(
        refused(discard(&case.db, &id, &tip, NOW_MS)),
        "changes_stale"
    );
    assert_eq!(
        discard(&case.db, &id, &head, NOW_MS).unwrap(),
        NOW_MS + 7 * DAY_MS
    );
    let reference = format!("refs/crazyagent/discarded/{}", id.as_str());
    assert_eq!(case.owner(&["rev-parse", &reference]).trim(), head);
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.restorable_until_ms),
        (ChangeState::Discarded, Some(NOW_MS + 7 * DAY_MS))
    );
    assert_eq!(case.agent(&["rev-parse", "HEAD"]).trim(), tip);
    assert!(!case.workspace.worktree.join("src/new.rs").exists());
    assert!(case.workspace.worktree.join("other.txt").exists());
    assert_eq!(case.tip(), tip, "discard moved the owner's branch");
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(case.agent(&["rev-parse", "HEAD"]).trim(), head);
    assert!(case.workspace.worktree.join("src/new.rs").exists());
    assert_eq!(
        case.owner(&["for-each-ref", "refs/crazyagent/discarded/"]),
        ""
    );
    assert_eq!(
        case.owner(&[
            "rev-parse",
            &format!("refs/crazyagent/pending/{}", id.as_str())
        ])
        .trim(),
        head
    );
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    case.edit("later.txt", "new work\n");
    let later = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_ne!(later, id);
    assert_eq!(
        refused(restore(&case.db, &id, NOW_MS)),
        "changes_agent_has_pending"
    );
}

#[test]
fn a_discarded_change_set_ref_is_pruned_after_seven_days() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    let reference = format!("refs/crazyagent/discarded/{}", id.as_str());
    assert_eq!(prune(&case.db, NOW_MS + 7 * DAY_MS - 1).unwrap(), 0);
    assert_eq!(case.owner(&["rev-parse", &reference]).trim(), head);
    assert_eq!(prune(&case.db, NOW_MS + 7 * DAY_MS).unwrap(), 1);
    assert_eq!(case.owner(&["for-each-ref", "refs/crazyagent/"]), "");
    assert_eq!(prune(&case.db, NOW_MS + 8 * DAY_MS).unwrap(), 0);
    assert_eq!(
        refused(restore(&case.db, &id, NOW_MS + 7 * DAY_MS)),
        "changes_restore_expired"
    );
}

/// Writes an executable hook or helper script that leaves a marker file when it runs.
fn marker_script(path: &Path, marker: &Path) {
    write(
        path,
        &format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    );
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn repository_hooks_filters_merge_drivers_textconv_and_fsmonitor_never_run() {
    let case = Case::new();
    let markers = case.home.join("markers");
    std::fs::create_dir(&markers).unwrap();
    let hooks = [
        "pre-commit",
        "post-commit",
        "post-checkout",
        "post-index-change",
        "reference-transaction",
        "post-merge",
        "pre-auto-gc",
        "post-rewrite",
    ];
    for hook in hooks {
        marker_script(
            &case.repo.join(".git/hooks").join(hook),
            &markers.join(hook),
        );
    }
    let helper = |name: &str| {
        let script = case.home.join(format!("{name}.sh"));
        marker_script(&script, &markers.join(name));
        script.display().to_string()
    };
    for (key, value) in [
        ("filter.evil.clean", helper("clean")),
        ("filter.evil.smudge", helper("smudge")),
        ("filter.evil.process", helper("process")),
        ("merge.evil.driver", helper("merge")),
        ("diff.evil.textconv", helper("textconv")),
        ("diff.evil.command", helper("diff")),
        ("core.fsmonitor", helper("fsmonitor")),
    ] {
        case.owner(&["config", key, &value]);
    }
    case.edit(".gitattributes", "*.txt diff=evil\n");
    case.edit("a.txt", "plain text\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let page = diff(&case.db, &id, 0).unwrap();
    assert!(page.files.iter().any(|file| file.path == ".gitattributes"
        && file.marks.contains(&FileMark::Attributes)));
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    case.edit(
        ".gitattributes",
        "*.txt diff=evil\n*.dat filter=evil merge=evil\n",
    );
    case.edit("b.dat", "filtered\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(
        case.owner(&["cat-file", "-p", &format!("{}:b.dat", case.view(&id).head)]),
        "filtered\n",
        "the clean filter changed the committed content"
    );
    diff(&case.db, &id, 0).unwrap();
    let before = case.checkout();
    assert_eq!(refused(case.accept(&id)), "changes_git_unsupported");
    assert_eq!(case.checkout(), before);
    let ran: Vec<_> = std::fs::read_dir(&markers)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(ran.is_empty(), "configured commands ran: {ran:?}");
}

#[test]
fn an_accept_stopped_at_any_step_is_finished_or_undone_when_the_engine_starts() {
    for step in [
        owner::Step::Recorded,
        owner::Step::FilesWritten,
        owner::Step::BranchMoved,
    ] {
        let case = Case::new();
        case.edit("README", "one\ntwo\nthree\nfour\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        let tip = case.tip();
        let before = case.checkout();
        let stopped = owner::accept(&case.db, &case.home, &id, &head, None, &|at| at == step);
        assert_eq!(refused(stopped), "changes_git_failed", "{step:?}");
        assert_eq!(store::in_flight(&case.db).unwrap().len(), 1, "{step:?}");
        reconcile(&case.db).unwrap();
        assert!(store::in_flight(&case.db).unwrap().is_empty(), "{step:?}");
        if step == owner::Step::Recorded {
            assert_eq!(case.view(&id).state, ChangeState::Pending);
            assert_eq!((case.tip(), case.checkout()), (tip, before));
            assert!(matches!(
                case.accept(&id).unwrap(),
                AcceptOutcome::Landed { .. }
            ));
        } else {
            assert_eq!(case.view(&id).state, ChangeState::Accepted, "{step:?}");
            assert_ne!(case.tip(), tip);
            assert_eq!(
                case.owner(&["log", "-1", "--format=%P", "main"]).trim(),
                tip
            );
            assert_eq!(case.owner(&["status", "--porcelain"]), "", "{step:?}");
            assert_eq!(case.agent(&["rev-parse", "HEAD"]).trim(), case.tip());
        }
        assert_eq!(
            std::fs::read_to_string(case.repo.join("README")).unwrap(),
            "one\ntwo\nthree\nfour\n"
        );
    }
}

#[test]
fn a_turn_past_the_tree_bounds_is_refused_and_leaves_nothing_committed_or_staged() {
    let case = Case::new();
    let bounds = crate::worktrees::TreeBounds {
        files: 3,
        bytes: 64,
        components: 3,
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Too much".into(),
    };
    let head = case.agent(&["rev-parse", "HEAD"]);
    let cases: [&[(&str, usize)]; 3] = [
        &[("a", 1), ("b", 1), ("c", 1), ("d", 1)],
        &[("large.txt", 65)],
        &[("one/two/three/deep.txt", 1)],
    ];
    for files in cases {
        for (path, size) in files {
            case.edit(path, &"x".repeat(*size));
        }
        assert_eq!(
            refused(flow::record(
                &case.db,
                &case.workspace,
                &turn,
                &bounds,
                NOW_MS
            )),
            "changes_too_large",
            "{files:?}"
        );
        assert_eq!(case.agent(&["rev-parse", "HEAD"]), head);
        assert_eq!(case.agent(&["diff", "--cached", "--name-only"]), "");
        assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
        for (path, _) in files {
            let top = path.split('/').next().unwrap();
            let target = case.workspace.worktree.join(top);
            if target.is_dir() {
                std::fs::remove_dir_all(target).unwrap();
            } else {
                std::fs::remove_file(target).unwrap();
            }
        }
    }
    case.edit("a", "x");
    assert!(
        flow::record(&case.db, &case.workspace, &turn, &bounds, NOW_MS)
            .unwrap()
            .is_some()
    );
}

#[test]
fn diff_pages_are_plain_text_and_mark_what_the_owner_should_check() {
    let case = Case::new();
    let worktree = &case.workspace.worktree;
    let big: String = (0..3000).map(|line| format!("line {line}\n")).collect();
    case.edit("big.txt", &big);
    case.edit("escape.txt", "\u{1b}[31mred\u{202E}txt\r\n");
    case.edit("script.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        worktree.join("script.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::set_permissions(
        worktree.join("README"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("README", worktree.join("link")).unwrap();
    case.edit(".gitmodules", "[submodule \"x\"]\n");
    std::fs::write(worktree.join("bin.dat"), [0_u8, 1, 2, 255]).unwrap();
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (mut files, mut text, mut page) = (Vec::new(), String::new(), 0);
    loop {
        let diff = diff(&case.db, &id, page).unwrap();
        assert!(!diff.truncated);
        assert!(serde_json::to_vec(&diff).unwrap().len() < crate::protocol::FRAME_BYTES_MAX);
        files.extend(diff.files);
        text.push_str(&diff.text);
        match diff.next {
            Some(next) => page = next,
            None => break,
        }
    }
    assert!(page >= 1, "a 3000-line file fit one page");
    let shown: Vec<_> = files
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.status,
                file.kind,
                file.marks.clone(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                ".gitmodules",
                FileStatus::Added,
                FileKind::Text,
                vec![FileMark::Modules]
            ),
            (
                "README",
                FileStatus::Modified,
                FileKind::Text,
                vec![FileMark::Executable, FileMark::Mode]
            ),
            ("big.txt", FileStatus::Added, FileKind::Text, vec![]),
            ("bin.dat", FileStatus::Added, FileKind::Binary, vec![]),
            ("escape.txt", FileStatus::Added, FileKind::Text, vec![]),
            (
                "link",
                FileStatus::Added,
                FileKind::Symlink,
                vec![FileMark::Symlink]
            ),
            (
                "script.sh",
                FileStatus::Added,
                FileKind::Text,
                vec![FileMark::Executable]
            ),
        ]
    );
    assert!(text.contains("+line 0\n") && text.contains("+line 2999\n"));
    assert!(
        text.contains("+\u{FFFD}[31mred\u{FFFD}txt\u{FFFD}\n"),
        "{text:?}"
    );
    assert!(
        !text
            .chars()
            .any(|c| c == '\u{1b}' || c == '\u{202E}' || c == '\r')
    );
    let past = diff(&case.db, &id, page + 5).unwrap();
    assert!(past.files.is_empty() && past.text.is_empty() && past.next.is_none());
}

#[test]
fn commits_the_agent_made_in_a_clean_tree_open_a_change_set_above_the_worktree_start() {
    let case = Case::new();
    let start = case.tip();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.agent_commit("Add the fourth line");
    case.edit("src/new.rs", "fn main() {}\n");
    let second = case.agent_commit("Add a main");
    assert_ne!(first, second);
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&id);
    assert_eq!(
        (
            view.state,
            view.base.as_str(),
            view.head.as_str(),
            view.files,
            view.added,
            view.removed
        ),
        (
            ChangeState::Pending,
            start.as_str(),
            second.as_str(),
            2,
            2,
            0
        )
    );
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    assert_eq!(case.view(&id).head, second);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the agent's commits did not land");
    };
    assert_eq!(
        case.owner(&["show", "main:README"]),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(case.owner(&["show", "main:src/new.rs"]), "fn main() {}\n");
    assert_eq!(case.agent_head(), commit);
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
}

#[test]
fn commits_plus_uncommitted_work_land_together_and_later_commits_join_the_open_set() {
    let case = Case::new();
    let start = case.tip();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let committed = case.agent_commit("Add the fourth line");
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&id);
    assert_eq!(
        (view.base.as_str(), view.files, view.added, view.removed),
        (start.as_str(), 2, 2, 0)
    );
    assert_eq!(case.agent(&["rev-parse", "HEAD^"]).trim(), committed);
    assert_eq!(view.head, case.agent_head());
    case.edit("LATER", "later\n");
    let later = case.agent_commit("Add a later file");
    assert_eq!(case.turn(TurnOutcome::Failed), Some(id.clone()));
    let view = case.view(&id);
    assert_eq!(
        (
            view.state,
            view.head.as_str(),
            view.files,
            view.last_outcome
        ),
        (ChangeState::Pending, later.as_str(), 3, TurnOutcome::Failed)
    );
}

#[test]
fn the_worktree_start_moves_to_where_accept_or_discard_leaves_the_worktree() {
    let case = Case::new();
    case.start();
    case.edit("A", "a\n");
    case.agent_commit("Add A");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let owner_tip = case.owner_commit("OWNER", "owner\n", "Owner work");
    discard(&case.db, &first, &case.view(&first).head, NOW_MS).unwrap();
    assert_eq!(case.agent_head(), owner_tip);
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
    case.edit("B", "b\n");
    let reviewed = case.agent_commit("Add B");
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&second);
    assert_eq!(
        (view.base.as_str(), view.head.as_str(), view.files),
        (owner_tip.as_str(), reviewed.as_str(), 1)
    );
    case.edit("C", "c\n");
    let newer = case.agent_commit("Add C");
    assert!(matches!(
        case.accept(&second).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(case.agent_head(), newer);
    let third = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&third);
    assert_eq!(
        (view.base.as_str(), view.head.as_str(), view.files),
        (reviewed.as_str(), newer.as_str(), 1)
    );
    assert!(matches!(
        case.accept(&third).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    for (path, landed) in [("A", false), ("B", true), ("C", true)] {
        let shown = case.owner(&["ls-tree", "--name-only", "main", path]);
        assert_eq!(!shown.is_empty(), landed, "{path}");
    }
}

#[test]
fn a_worktree_with_no_recorded_start_keeps_opening_above_its_head() {
    let case = Case::new();
    case.edit("A", "a\n");
    case.agent_commit("Add A");
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
    case.edit("B", "b\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).files, 1);
}

#[test]
fn commits_the_agent_made_past_the_file_bound_are_refused_and_change_nothing() {
    let case = Case::new();
    case.start();
    let bounds = crate::worktrees::TreeBounds {
        files: 3,
        bytes: 64,
        components: 3,
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Too much".into(),
    };
    for path in ["a", "b", "c", "d"] {
        case.edit(path, "x");
    }
    let head = case.agent_commit("Add four files");
    assert_eq!(
        refused(flow::record(
            &case.db,
            &case.workspace,
            &turn,
            &bounds,
            NOW_MS
        )),
        "changes_too_large"
    );
    assert_eq!(case.agent_head(), head);
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
}

#[test]
fn agent_history_past_the_byte_path_depth_or_ignore_bounds_is_refused_and_changes_nothing() {
    let bounds = crate::worktrees::TreeBounds {
        files: 3,
        bytes: 64,
        components: 3,
    };
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Past the bounds".into(),
    };
    let long = "x".repeat(1025);
    let skip_worktree_env = |case: &Case| {
        case.agent(&["update-index", "--skip-worktree", ".gitignore"]);
        case.edit(".gitignore", "");
        case.edit("secret.env", "TOKEN=x\n");
    };
    type Setup<'a> = Box<dyn Fn(&Case) + 'a>;
    let cases: Vec<(&str, &str, Setup<'_>, &str)> = vec![
        (
            "a committed blob past the byte bound",
            "",
            Box::new(|case| {
                case.edit("large.txt", &"x".repeat(65));
                case.agent_commit("Add a large file");
            }),
            "changes_too_large",
        ),
        (
            "a committed path past the depth bound",
            "",
            Box::new(|case| {
                case.edit("one/two/three/deep.txt", "x");
                case.agent_commit("Add a deep file");
            }),
            "changes_too_large",
        ),
        (
            "a committed path past the length bound",
            "",
            Box::new(|case| {
                case.agent_plumbing_commit(long.as_bytes(), "x");
            }),
            "changes_too_large",
        ),
        (
            "a committed path that is not UTF-8",
            "",
            Box::new(|case| {
                case.agent_plumbing_commit(b"bad-\xff.txt", "x");
            }),
            "changes_git_unsupported",
        ),
        (
            "a committed file the owner's ignore rules exclude",
            "secret.env\n",
            Box::new(|case| {
                case.edit("secret.env", "TOKEN=x\n");
                case.agent(&["add", "-f", "secret.env"]);
                case.agent_commit("Add the env file");
            }),
            "changes_ignored_files",
        ),
        (
            "a committed file named like pathspec magic the owner's ignore rules exclude",
            ":*\n",
            Box::new(|case| {
                case.edit(":(top)keep.txt", "x");
                case.agent(&["add", "-f", ":(literal):(top)keep.txt"]);
                case.agent_commit("Add a file named like magic");
            }),
            "changes_ignored_files",
        ),
        (
            "a staged file the owner's ignore rules exclude",
            "secret.env\n",
            Box::new(|case| {
                case.edit("secret.env", "TOKEN=x\n");
                case.agent(&["add", "-f", "secret.env"]);
            }),
            "changes_ignored_files",
        ),
        (
            "a file the agent un-ignored behind a skip-worktree .gitignore",
            "secret.env\n",
            Box::new(skip_worktree_env),
            "changes_ignored_files",
        ),
        (
            "a committed file the agent un-ignored behind a skip-worktree .gitignore",
            "secret.env\n",
            Box::new(|case| {
                skip_worktree_env(case);
                case.agent_commit("Add the env file");
            }),
            "changes_ignored_files",
        ),
    ];
    for (what, rules, make, code) in cases {
        let case = Case::new();
        if !rules.is_empty() {
            case.owner_commit(".gitignore", rules, "Ignore files");
            case.agent(&["checkout", "-q", "--detach", "main"]);
        }
        case.start();
        make(&case);
        let head = case.agent_head();
        assert_eq!(
            refused(flow::record(
                &case.db,
                &case.workspace,
                &turn,
                &bounds,
                NOW_MS
            )),
            code,
            "{what}"
        );
        assert_eq!(case.agent_head(), head, "{what}");
        assert!(
            list(&case.db, &case.workspace.project).unwrap().is_empty(),
            "{what}"
        );
    }
}

#[test]
fn agent_history_within_the_bounds_lands_with_an_ignored_file_the_owner_tracks() {
    let case = Case::new();
    write(&case.repo.join(".gitignore"), "*.log\n");
    write(&case.repo.join("keep.log"), "kept\n");
    case.owner(&["add", ".gitignore"]);
    case.owner(&["add", "-f", "keep.log"]);
    case.owner(&["commit", "-q", "-m", "Track one log"]);
    case.agent(&["checkout", "-q", "--detach", "main"]);
    case.start();
    case.edit("keep.log", "kept\nand more\n");
    case.edit("notes.txt", "notes\n");
    case.agent_commit("Edit the log and add notes");
    case.edit("later.txt", "later\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).files, 3);
    let AcceptOutcome::Landed { .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        std::fs::read_to_string(case.repo.join("keep.log")).unwrap(),
        "kept\nand more\n"
    );
}

#[test]
fn the_proposed_message_reuses_the_agent_commit_subjects_newest_first_and_lands() {
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    case.edit("src/new.rs", "fn main() {}\n");
    let tree = case.workspace.worktree.display().to_string();
    case.agent_commit(&format!("Add a main in {tree}/src/new.rs\n\nBody text"));
    case.edit("LATER", "later\n");
    let id = case
        .turn_with(TurnOutcome::Succeeded, "Ignored prompt line")
        .unwrap();
    let message = "Add a main in src/new.rs\n\n- Add the fourth line";
    assert_eq!(case.view(&id).message, message);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), message);
}

#[test]
fn without_agent_commits_the_message_is_the_prompt_line_with_local_paths_cut_at_a_word() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let prompt = format!(
        "Fix the parser in {}/src/parse.rs per (/Users/someone/Downloads/spec.pdf), \
         ~/notes/todo.md and keep going with words\nsecond line",
        case.repo.display()
    );
    let id = case.turn_with(TurnOutcome::Succeeded, &prompt).unwrap();
    let message = "Fix the parser in src/parse.rs per (spec.pdf), todo.md and keep going";
    assert_eq!(case.view(&id).message, message);
    assert_eq!(
        case.agent(&["log", "-1", "--format=%s", "HEAD"]).trim(),
        message
    );
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), message);
    let roots = [case.repo.as_path()];
    let long = "x".repeat(80);
    assert_eq!(flow::subject(&long, &roots), "x".repeat(72));
    assert_eq!(flow::subject(&format!("Rename {long}"), &roots), "Rename");
    assert_eq!(
        flow::subject("Fix\u{202E}\ttabs  and /api/users routes", &roots),
        "Fix tabs and /api/users routes"
    );
}

#[test]
fn an_edited_message_lands_and_an_invalid_one_changes_nothing() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let before = case.checkout();
    let long = "x".repeat(MESSAGE_BYTES_MAX + 1);
    for message in [
        " \r\n\t ",
        "Bad \u{1b}[31m red",
        "Bad\rreturn",
        long.as_str(),
    ] {
        assert_eq!(
            refused(case.accept_with(&id, Some(message))),
            "changes_message_invalid"
        );
        assert_eq!(case.checkout(), before);
        let view = case.view(&id);
        assert_eq!(
            (view.state, view.head.as_str()),
            (ChangeState::Pending, head.as_str())
        );
    }
    let edited = "  Owner title\r\n\r\nWhy it changed\twith a tab\n";
    let AcceptOutcome::Landed { commit, .. } = case.accept_with(&id, Some(edited)).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        case.landed_message(&commit),
        "Owner title\n\nWhy it changed\twith a tab"
    );
}

#[test]
fn a_store_from_before_the_added_columns_gains_them_and_falls_back_to_the_prompt() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.db
        .execute_batch(
            "ALTER TABLE landing_change_sets DROP COLUMN message;
             ALTER TABLE landing_change_sets DROP COLUMN waiting;",
        )
        .unwrap();
    store::initialize(&case.db).unwrap();
    assert_eq!(case.view(&id).message, "Add the fourth line");
    assert!(!case.view(&id).waiting);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), "Add the fourth line");
}

#[test]
fn a_second_open_change_set_for_one_worktree_is_refused_and_both_rows_stay() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let first = store::load(&case.db, &id).unwrap();
    assert!(first.is_open());
    let mut second = first.clone();
    second.id = ChangeSetId::new("f".repeat(32)).unwrap();
    assert_ne!(second.id, first.id);
    second.prompt_line = "A second turn".into();
    assert_eq!(
        refused(store::save(&case.db, &second)),
        "changes_agent_has_pending"
    );
    assert_eq!(store::load(&case.db, &id).unwrap(), first);
    assert_eq!(
        refused(store::load(&case.db, &second.id)),
        "changes_not_found"
    );
    second.state = ChangeState::Discarded;
    second.restorable_until_ms = Some(NOW_MS + 7 * DAY_MS);
    store::save(&case.db, &second).unwrap();
    assert_eq!(store::load(&case.db, &second.id).unwrap(), second);
    let mut reopened = second.clone();
    reopened.state = ChangeState::Pending;
    reopened.restorable_until_ms = None;
    assert_eq!(
        refused(store::save(&case.db, &reopened)),
        "changes_agent_has_pending"
    );
    assert_eq!(store::load(&case.db, &id).unwrap(), first);
    assert_eq!(store::load(&case.db, &second.id).unwrap(), second);
    let mut updated = first.clone();
    updated.prompt_line = "Updated in place".into();
    updated.updated_ms += 1;
    store::save(&case.db, &updated).unwrap();
    assert_eq!(store::load(&case.db, &id).unwrap(), updated);
    assert_eq!(
        store::open_for(&case.db, first.worktree.to_str().unwrap()).unwrap(),
        Some(updated)
    );
}
