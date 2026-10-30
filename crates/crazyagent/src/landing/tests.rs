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
/// no global git config that engine git in the test reads in place of HOME, and an
/// engine root holding the change set store.
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
        git::OWNER_HOME.with(|owner| *owner.borrow_mut() = Some(home.clone()));
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

/// One file of a layout: its name, its text and whether it is executable.
type LayoutFile = (&'static str, &'static str, bool);

/// The bare-repository layouts an agent can leave in a worktree (SM-R1): the folder git
/// makes with `init --bare`, the same names in mixed case, `HEAD` beside a `commondir`, and
/// executable files standing in for `objects` and `refs`. Output: each layout's folder and
/// its files with their text and whether each is executable.
fn bare_layouts() -> Vec<(&'static str, Vec<LayoutFile>)> {
    let head = "ref: refs/heads/main\n";
    vec![
        (
            "notes",
            vec![
                ("HEAD", head, false),
                ("config", "[core]\n\tbare = true\n", false),
                ("objects/info/packs", "\n", false),
                ("refs/heads/main", "\n", false),
            ],
        ),
        (
            "docs/Mixed",
            vec![
                ("Head", head, false),
                ("OBJECTS/x", "x\n", false),
                ("Refs/y", "y\n", false),
            ],
        ),
        (
            "linked",
            vec![("HEAD", head, false), ("commondir", "../..\n", false)],
        ),
        (
            "tools",
            vec![
                ("HEAD", head, false),
                ("objects", "#!/bin/sh\n", true),
                ("refs", "#!/bin/sh\n", true),
            ],
        ),
    ]
}

/// Writes one layout into a folder. Inputs: the folder and the layout's files.
fn plant_layout(folder: &Path, files: &[(&str, &str, bool)]) {
    for (name, text, executable) in files {
        let path = folder.join(name);
        write(&path, text);
        if *executable {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

#[test]
fn a_bare_repository_layout_an_agent_leaves_is_refused_when_its_turn_is_committed() {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Write notes".into(),
    };
    for (folder, files) in bare_layouts() {
        let case = Case::new();
        let layout = case.workspace.worktree.join(folder);
        plant_layout(&layout, &files);
        if folder == "notes" {
            assert_eq!(
                git_in(&case.home, &layout, &["rev-parse", "--git-dir"]).trim(),
                ".",
                "the owner's own git takes the layout as a repository"
            );
        }
        assert_eq!(
            refused(record_turn_end(&case.db, &case.workspace, &turn)),
            "changes_bare_repository",
            "{folder}"
        );
    }

    let case = Case::own_branch();
    let (_, files) = &bare_layouts()[0];
    plant_layout(&case.workspace.worktree.join("notes"), files);
    case.agent_commit("Notes");
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_bare_repository",
        "an Own branch agent's commit"
    );

    let case = Case::new();
    let worktree = case.workspace.worktree.clone();
    write(
        &worktree.join("src/objects/model.rs"),
        "pub struct Model;\n",
    );
    write(&worktree.join("src/refs/link.rs"), "pub struct Link;\n");
    write(&worktree.join("docs/HEAD"), "A file named HEAD.\n");
    write(&worktree.join("head/index.html"), "<p>head</p>\n");
    assert!(
        record_turn_end(&case.db, &case.workspace, &turn)
            .unwrap()
            .is_some(),
        "folders named objects and refs without a HEAD file land"
    );
}

/// Adds a second agent's linked worktree to a case's repository, from the owner's
/// current branch tip, as the engine adds one for a sibling sub-agent. Input: the case.
/// Output: the second agent's workspace.
fn second_agent(case: &Case) -> Workspace {
    let worktree = case.repo.parent().unwrap().join("agent-2");
    case.owner(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        worktree.to_str().unwrap(),
        "HEAD",
    ]);
    Workspace {
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-2".into(),
            label: "Agent 2".into(),
        },
        worktree,
        git_dir: case.repo.join(".git/worktrees/agent-2"),
        ..case.workspace.clone()
    }
}

#[test]
fn accepting_two_change_sets_that_each_hold_half_a_bare_layout_refuses_the_second() {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Write notes".into(),
    };
    let store: Vec<(&str, &str, bool)> = vec![
        ("objects/info/packs", "\n", false),
        ("refs/heads/main", "\n", false),
    ];
    let head: Vec<(&str, &str, bool)> = vec![
        ("HEAD", "ref: refs/heads/main\n", false),
        ("config", "[core]\n\tbare = true\n", false),
    ];
    for (first, second) in [(&store, &head), (&head, &store)] {
        let case = Case::new();
        let sibling = second_agent(&case);
        plant_layout(&case.workspace.worktree.join("notes"), first);
        plant_layout(&sibling.worktree.join("notes"), second);
        let one = record_turn_end(&case.db, &case.workspace, &turn)
            .unwrap()
            .expect("half a layout lands as a change set");
        let two = record_turn_end(&case.db, &sibling, &turn)
            .unwrap()
            .expect("the other half lands as a change set");
        assert!(matches!(
            case.accept(&one).unwrap(),
            AcceptOutcome::Landed { .. }
        ));
        let tip = case.tip();
        assert_eq!(
            refused(case.accept(&two)),
            "changes_bare_repository",
            "the merge would complete the layout in the owner's project"
        );
        assert_eq!(case.tip(), tip, "the branch did not move");
        let landed = case.repo.join("notes").join(second[0].0);
        assert!(!landed.exists(), "{}", landed.display());
    }
}

#[test]
fn a_link_an_agent_leaves_pointing_outside_the_project_or_into_the_agents_folder_is_refused() {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Link the notes".into(),
    };
    for target in [
        "/private/tmp",
        "../../outside",
        "../docs/../../outside",
        "../.crazyagent/worktrees/agent-3/target/bare",
        "../.CrazyAgent/Worktrees/agent-3",
        "./../.crazyagent/worktrees",
    ] {
        let case = Case::new();
        std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
        let link = case.workspace.worktree.join("docs/link");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, &link).unwrap();
        assert_eq!(
            refused(record_turn_end(&case.db, &case.workspace, &turn)),
            "changes_link_outside",
            "{target}"
        );
    }

    let case = Case::own_branch();
    std::os::unix::fs::symlink("/private/tmp", case.workspace.worktree.join("tmp-link")).unwrap();
    case.agent_commit("Link tmp");
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_link_outside",
        "an Own branch agent's commit"
    );

    let case = Case::new();
    std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
    let worktree = case.workspace.worktree.clone();
    std::fs::create_dir_all(worktree.join("docs")).unwrap();
    std::fs::create_dir_all(worktree.join("a/b")).unwrap();
    for (link, target) in [
        ("docs/readme", "../README"),
        ("docs/here", "."),
        ("a/b/docs", "../../docs"),
        ("docs/context", "../.crazyagent/context"),
        ("docs/worktrees", "../worktrees"),
    ] {
        std::os::unix::fs::symlink(target, worktree.join(link)).unwrap();
    }
    assert!(
        record_turn_end(&case.db, &case.workspace, &turn)
            .unwrap()
            .is_some(),
        "links that stay in the project and out of the agents' folder land"
    );
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
fn restore_refuses_while_the_worktree_holds_commits_made_after_its_turn_and_keeps_them() {
    for before_discard in [false, true] {
        let case = Case::new();
        case.edit("src/new.rs", "fn main() {}\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        if before_discard {
            case.edit("later.txt", "made before the discard\n");
            case.agent_commit("Work before the discard");
        }
        discard(&case.db, &id, &head, NOW_MS).unwrap();
        if !before_discard {
            assert_eq!(case.agent_head(), case.tip());
            case.edit("later.txt", "made after the discard\n");
            case.agent_commit("Work after the discard");
        }
        let kept = case.agent_head();
        let text = std::fs::read_to_string(case.workspace.worktree.join("later.txt")).unwrap();
        assert_eq!(
            refused(restore(&case.db, &id, NOW_MS + DAY_MS)),
            "changes_agent_busy",
            "before the discard: {before_discard}"
        );
        assert_eq!(case.agent_head(), kept);
        assert_eq!(
            std::fs::read_to_string(case.workspace.worktree.join("later.txt")).unwrap(),
            text
        );
        assert_eq!(case.view(&id).state, ChangeState::Discarded);
        let reference = format!("refs/crazyagent/discarded/{}", id.as_str());
        assert_eq!(case.owner(&["rev-parse", &reference]).trim(), head);
        assert_eq!(
            case.owner(&["for-each-ref", "refs/crazyagent/pending/"]),
            ""
        );
    }
}

#[test]
fn an_agent_head_pointed_at_the_owners_branch_is_never_read_through_or_moved() {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-2".into(),
        prompt_line: "Move the owner's branch".into(),
    };
    for linked in [false, true] {
        let case = Case::new();
        let head_file = case.workspace.git_dir.join("HEAD");
        let detached = std::fs::read_to_string(&head_file).unwrap();
        let tip = case.tip();
        let point_at_main = || {
            std::fs::remove_file(&head_file).unwrap();
            if linked {
                std::os::unix::fs::symlink(case.repo.join(".git/refs/heads/main"), &head_file)
                    .unwrap();
            } else {
                std::fs::write(&head_file, "ref: refs/heads/main\n").unwrap();
            }
        };
        point_at_main();
        case.edit("README", "agent work\n");
        assert_eq!(
            refused(record_turn_end(&case.db, &case.workspace, &turn)),
            "changes_workspace_invalid",
            "linked: {linked}"
        );
        assert_eq!(case.tip(), tip, "linked: {linked}");
        assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
        std::fs::remove_file(&head_file).unwrap();
        std::fs::write(&head_file, &detached).unwrap();
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        point_at_main();
        discard(&case.db, &id, &head, NOW_MS).unwrap();
        assert_eq!(case.tip(), tip, "linked: {linked}");
        assert_eq!(
            refused(restore(&case.db, &id, NOW_MS + DAY_MS)),
            "changes_workspace_invalid",
            "linked: {linked}"
        );
        assert_eq!(case.tip(), tip, "linked: {linked}");
        let opened = flow::open_agent(
            &case.db,
            AgentKind::Sub,
            &case.workspace.repository,
            &case.workspace.worktree,
            &case.workspace.git_dir,
        );
        match opened {
            Ok(agent) => {
                assert!(!linked);
                assert_eq!(refused(agent.head()), "changes_workspace_invalid");
                assert!(flow::reset_agent(&agent, &head).is_err());
            }
            Err(refused) => {
                assert!(linked);
                assert_eq!(refused.0, "changes_workspace_invalid");
            }
        }
        assert_eq!(case.tip(), tip, "linked: {linked}");
        assert!(
            std::fs::symlink_metadata(case.workspace.git_dir.join("HEAD.lock")).is_err(),
            "linked: {linked}"
        );
        if !linked {
            assert_eq!(
                std::fs::read_to_string(&head_file).unwrap(),
                "ref: refs/heads/main\n"
            );
        }
    }
}

#[test]
fn engine_head_moves_never_follow_a_reflog_link_in_the_agent_admin_folder() {
    let case = Case::new();
    let config = case.repo.join(".git/config");
    let shallow = case.repo.join(".git/shallow");
    let before = std::fs::read(&config).unwrap();
    let logs = case.workspace.git_dir.join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let _ = std::fs::remove_file(logs.join("HEAD"));
    std::os::unix::fs::symlink(&config, logs.join("HEAD")).unwrap();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    assert_eq!(case.agent_head(), head);
    assert_eq!(std::fs::read(&config).unwrap(), before);
    std::fs::remove_file(logs.join("HEAD")).unwrap();
    std::os::unix::fs::symlink(&shallow, logs.join("HEAD")).unwrap();
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(case.agent_head(), case.tip());
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    assert_eq!(case.agent_head(), head);
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert!(std::fs::symlink_metadata(&shallow).is_err());
    assert!(
        std::fs::symlink_metadata(logs.join("HEAD"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        case.owner(&["config", "--get", "user.name"]).trim(),
        "Repo Owner"
    );
}

#[test]
fn a_fifo_planted_in_the_agent_git_folder_is_refused_without_waiting_on_it() {
    let (done, finished) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        use std::os::unix::fs::FileTypeExt;
        let case = Case::new();
        let fifo = |path: &Path| {
            let _ = std::fs::remove_file(path);
            let output = crate::spawn_gate::output(
                Command::new("/usr/bin/mkfifo")
                    .arg(path)
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .unwrap();
            assert!(
                output.status.success(),
                "mkfifo failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                std::fs::symlink_metadata(path)
                    .unwrap()
                    .file_type()
                    .is_fifo()
            );
        };
        let git_dir = case.workspace.git_dir.clone();
        let head_file = git_dir.join("HEAD");
        let tip = case.tip();
        let logs = git_dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        fifo(&logs.join("HEAD"));
        case.edit("src/new.rs", "fn main() {}\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        assert_eq!(case.agent_head(), head);
        discard(&case.db, &id, &head, NOW_MS).unwrap();
        assert_eq!(case.agent_head(), tip);
        restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
        assert_eq!(case.agent_head(), head);
        assert!(
            std::fs::symlink_metadata(logs.join("HEAD"))
                .unwrap()
                .file_type()
                .is_fifo()
        );
        fifo(&head_file);
        case.edit("later.txt", "more work\n");
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-2".into(),
            prompt_line: "Read a FIFO".into(),
        };
        assert_eq!(
            refused(record_turn_end(&case.db, &case.workspace, &turn)),
            "changes_workspace_invalid"
        );
        discard(&case.db, &id, &head, NOW_MS).unwrap();
        assert_eq!(case.view(&id).state, ChangeState::Discarded);
        assert_eq!(
            refused(restore(&case.db, &id, NOW_MS + DAY_MS)),
            "changes_workspace_invalid"
        );
        let late = NOW_MS + 8 * DAY_MS;
        let blocker = |now| {
            retire_blocker(
                &case.db,
                &case.workspace.repository,
                &case.workspace.worktree,
                &git_dir,
                now,
            )
            .unwrap()
        };
        assert_eq!(blocker(late), Some("cleanup_worktree_invalid"));
        assert_eq!(case.tip(), tip);
        std::fs::remove_file(&head_file).unwrap();
        std::fs::write(&head_file, format!("{head}\n")).unwrap();
        let agent = flow::open_agent(
            &case.db,
            AgentKind::Sub,
            &case.workspace.repository,
            &case.workspace.worktree,
            &git_dir,
        )
        .unwrap();
        assert_eq!(agent.head().unwrap(), head);
        fifo(&head_file);
        assert_eq!(refused(agent.head()), "changes_workspace_invalid");
        assert!(agent.move_head(None, &tip, "CrazyAgent: test").is_err());
        assert!(std::fs::symlink_metadata(git_dir.join("HEAD.lock")).is_err());
        assert!(
            std::fs::symlink_metadata(&head_file)
                .unwrap()
                .file_type()
                .is_fifo()
        );
        std::fs::remove_file(&head_file).unwrap();
        std::fs::write(&head_file, format!("{head}\n")).unwrap();
        fifo(&git_dir.join("gitdir"));
        assert_eq!(
            refused(flow::check_places(
                &case.workspace.repository,
                &case.workspace.worktree,
                &git_dir,
            )),
            "changes_workspace_invalid"
        );
        assert_eq!(blocker(late), Some("cleanup_worktree_invalid"));
        assert_eq!(case.tip(), tip);
        done.send(()).unwrap();
    });
    let waited = finished.recv_timeout(std::time::Duration::from_secs(120));
    assert!(
        !matches!(waited, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "a FIFO in the agent git folder made the engine wait"
    );
    worker.join().unwrap();
}

#[test]
fn engine_refs_are_moved_and_deleted_without_following_a_symbolic_ref_or_reflog_link() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let tip = case.tip();
    let config = case.repo.join(".git/config");
    let before = std::fs::read(&config).unwrap();
    let pending = format!("refs/crazyagent/pending/{}", id.as_str());
    let discarded = format!("refs/crazyagent/discarded/{}", id.as_str());
    let log = case.repo.join(".git/logs").join(&discarded);
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&config, &log).unwrap();
    assert_eq!(
        refused(discard(&case.db, &id, &head, NOW_MS)),
        "changes_workspace_invalid"
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    std::fs::remove_file(&log).unwrap();
    case.owner(&["symbolic-ref", &pending, "refs/heads/main"]);
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(case.tip(), tip);
    assert_eq!(case.owner(&["rev-parse", &discarded]).trim(), head);
    case.owner(&["symbolic-ref", &discarded, "refs/heads/main"]);
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    assert_eq!(case.tip(), tip);
    assert_eq!(case.owner(&["rev-parse", &pending]).trim(), head);
    assert_eq!(
        case.owner(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/crazyagent/discarded/"
        ]),
        ""
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
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

/// A dirty submodule in the agent's worktree does not make the worktree look unfinished, and
/// engine git never enters it to run its configured clean filter. The filter is configured
/// only after the setup's own git steps, so a racily clean file that the setup's git would
/// re-hash through it cannot leave the marker before engine git runs.
#[test]
fn a_dirty_submodule_is_ignored_and_its_configured_command_never_runs() {
    let case = Case::new();
    let worktree = case.workspace.worktree.clone();
    let sub = worktree.join("sub");
    std::fs::create_dir(&sub).unwrap();
    git_in(
        &case.home,
        &sub,
        &["init", "-q", "--template=", "-b", "main"],
    );
    git_in(&case.home, &sub, &["config", "user.name", "Sub Owner"]);
    git_in(
        &case.home,
        &sub,
        &["config", "user.email", "sub@example.invalid"],
    );
    write(&sub.join("inner.txt"), "inner\n");
    git_in(&case.home, &sub, &["add", "inner.txt"]);
    git_in(&case.home, &sub, &["commit", "-q", "-m", "Sub start"]);
    write(&sub.join(".gitattributes"), "inner.txt filter=guard\n");
    git_in(&case.home, &sub, &["add", ".gitattributes"]);
    git_in(&case.home, &sub, &["commit", "-q", "-m", "Sub filter"]);
    let sub_head = git_in(&case.home, &sub, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    git_in(
        &case.home,
        &worktree,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sub_head},sub"),
        ],
    );
    write(
        &worktree.join(".gitmodules"),
        "[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
    );
    git_in(&case.home, &worktree, &["add", ".gitmodules"]);
    git_in(
        &case.home,
        &worktree,
        &["commit", "-q", "-m", "Add submodule"],
    );
    let head = case.agent_head();

    let marker = case.home.join("submodule-clean-filter-ran");
    let filter = case.home.join("clean-filter.sh");
    marker_script(&filter, &marker);
    git_in(
        &case.home,
        &sub,
        &["config", "filter.guard.clean", filter.to_str().unwrap()],
    );
    write(&sub.join("inner.txt"), "changed\n");
    write(&sub.join("untracked.txt"), "new\n");

    let agent = git::Repo::open(&case.repo, &case.workspace.git_dir, Some(&worktree)).unwrap();
    assert!(
        flow::holds_only(&agent, &head).unwrap(),
        "a dirty submodule made the worktree look unfinished"
    );
    assert!(
        !marker.exists(),
        "engine git entered the submodule and ran its clean filter"
    );
}

#[test]
fn accept_lands_with_a_dirty_submodule_in_the_owner_checkout() {
    let case = Case::new();
    let sub = case.repo.join("vendor");
    std::fs::create_dir(&sub).unwrap();
    git_in(
        &case.home,
        &sub,
        &["init", "-q", "--template=", "-b", "main"],
    );
    git_in(&case.home, &sub, &["config", "user.name", "Vendor Owner"]);
    git_in(
        &case.home,
        &sub,
        &["config", "user.email", "vendor@example.invalid"],
    );
    write(&sub.join("inner.txt"), "inner\n");
    git_in(&case.home, &sub, &["add", "inner.txt"]);
    git_in(&case.home, &sub, &["commit", "-q", "-m", "Vendor start"]);
    let sub_head = git_in(&case.home, &sub, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();

    git_in(
        &case.home,
        &case.repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{sub_head},vendor"),
        ],
    );
    write(
        &case.repo.join(".gitmodules"),
        "[submodule \"vendor\"]\n\tpath = vendor\n\turl = ./vendor\n",
    );
    git_in(&case.home, &case.repo, &["add", ".gitmodules"]);
    git_in(
        &case.home,
        &case.repo,
        &["commit", "-q", "-m", "Add submodule"],
    );
    let with_submodule = case.tip();
    case.agent(&["reset", "-q", "--hard", &with_submodule]);

    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();

    write(&sub.join("inner.txt"), "changed\n");
    write(&sub.join("untracked.txt"), "new\n");

    let AcceptOutcome::Landed { commit, branch } = case.accept(&id).unwrap() else {
        panic!("a dirty owner submodule blocked the accept");
    };
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(
        case.owner(&["ls-tree", "--name-only", &commit])
            .lines()
            .filter(|name| *name == "vendor")
            .count(),
        1,
        "the landed commit dropped the submodule gitlink"
    );
    assert_eq!(
        std::fs::read_to_string(sub.join("inner.txt")).unwrap(),
        "changed\n",
        "the accept reset the owner's dirty submodule work"
    );
}

#[test]
fn an_embedded_repository_left_in_the_worktree_is_refused_when_the_turn_is_committed() {
    let case = Case::new();
    let worktree = case.workspace.worktree.clone();
    let embedded = worktree.join("vendored");
    std::fs::create_dir(&embedded).unwrap();
    git_in(
        &case.home,
        &embedded,
        &["init", "-q", "--template=", "-b", "main"],
    );
    git_in(&case.home, &embedded, &["config", "user.name", "Vendor"]);
    git_in(
        &case.home,
        &embedded,
        &["config", "user.email", "vendor@example.invalid"],
    );
    write(&embedded.join("code.rs"), "fn main() {}\n");
    git_in(&case.home, &embedded, &["add", "code.rs"]);
    git_in(
        &case.home,
        &embedded,
        &["commit", "-q", "-m", "Vendor start"],
    );

    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Vendor a dependency".into(),
    };
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_embedded_repository"
    );
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

/// An Accept that fails after saving its intent is repaired by the owner's next click, not
/// left stale until the next upkeep: one that wrote nothing is retried and lands, one that
/// wrote the files or moved the branch is finished, and a Discard goes through.
#[test]
fn an_accept_that_failed_after_saving_its_intent_is_repaired_by_the_next_owner_action() {
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
        let stopped = owner::accept(&case.db, &case.home, &id, &head, None, &|at| at == step);
        assert_eq!(refused(stopped), "changes_git_failed", "{step:?}");
        assert_eq!(store::in_flight(&case.db).unwrap().len(), 1, "{step:?}");

        let again = case.accept(&id);

        assert!(store::in_flight(&case.db).unwrap().is_empty(), "{step:?}");
        if step == owner::Step::Recorded {
            assert!(
                matches!(again, Ok(AcceptOutcome::Landed { .. })),
                "{again:?}"
            );
        } else {
            assert_eq!(refused(again), "changes_stale", "{step:?}");
        }
        assert_eq!(case.view(&id).state, ChangeState::Accepted, "{step:?}");
        assert_ne!(case.tip(), tip, "{step:?}");
        assert_eq!(case.owner(&["status", "--porcelain"]), "", "{step:?}");
    }

    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let tip = case.tip();
    let stopped = owner::accept(&case.db, &case.home, &id, &head, None, &|at| {
        at == owner::Step::Recorded
    });
    assert_eq!(refused(stopped), "changes_git_failed");

    discard(&case.db, &id, &head, NOW_MS).unwrap();

    assert!(store::in_flight(&case.db).unwrap().is_empty());
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
    assert_eq!(case.tip(), tip);
}

/// A discarded change set whose ref cannot be deleted, here behind a stale ref lock a crash
/// left, never stops the others from being pruned, and is pruned itself once it can be.
#[test]
fn a_discarded_set_whose_ref_cannot_be_deleted_never_stops_the_others_from_pruning() {
    let case = Case::new();
    case.edit("src/first.rs", "fn first() {}\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    discard(&case.db, &first, &case.view(&first).head, NOW_MS).unwrap();
    case.edit("src/second.rs", "fn second() {}\n");
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_ne!(first, second);
    discard(&case.db, &second, &case.view(&second).head, NOW_MS).unwrap();
    let reference = |id: &ChangeSetId| format!("refs/crazyagent/discarded/{}", id.as_str());
    let lock = case
        .repo
        .join(".git")
        .join(format!("{}.lock", reference(&first)));
    assert!(lock.parent().unwrap().is_dir());
    write(&lock, "");

    assert_eq!(prune(&case.db, NOW_MS + 7 * DAY_MS).unwrap(), 1);

    assert_eq!(
        case.owner(&["for-each-ref", "--format=%(refname)", "refs/crazyagent/"])
            .trim(),
        reference(&first)
    );
    assert!(!store::load(&case.db, &first).unwrap().pruned);
    assert!(store::load(&case.db, &second).unwrap().pruned);
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(prune(&case.db, NOW_MS + 7 * DAY_MS).unwrap(), 1);
    assert_eq!(case.owner(&["for-each-ref", "refs/crazyagent/"]), "");
    assert!(store::load(&case.db, &first).unwrap().pruned);
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
fn the_owners_ignore_file_is_the_global_setting_else_the_default_and_only_a_real_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let (home, xdg) = (root.join("home"), root.join("xdg"));
    std::fs::create_dir(&home).unwrap();
    assert_eq!(git::owner_excludes(&home, None), None);
    write(&home.join(".config/git/ignore"), "secret.env\n");
    assert_eq!(
        git::owner_excludes(&home, None),
        Some(home.join(".config/git/ignore"))
    );
    assert_eq!(git::owner_excludes(&home, Some(xdg.as_path())), None);
    write(&xdg.join("git/ignore"), "secret.env\n");
    assert_eq!(
        git::owner_excludes(&home, Some(xdg.as_path())),
        Some(xdg.join("git/ignore"))
    );
    assert_eq!(
        git::owner_excludes(&home, Some(Path::new("xdg"))),
        Some(home.join(".config/git/ignore"))
    );
    assert_eq!(git::owner_excludes(Path::new("home"), None), None);
    write(
        &home.join(".gitconfig"),
        "[core]\n\texcludesFile = ~/ignores/global\n",
    );
    assert_eq!(git::owner_excludes(&home, None), None);
    write(&home.join("ignores/real"), "secret.env\n");
    std::os::unix::fs::symlink(home.join("ignores/real"), home.join("ignores/global")).unwrap();
    assert_eq!(
        git::owner_excludes(&home, None),
        Some(home.join("ignores/real"))
    );
    std::fs::remove_file(home.join("ignores/global")).unwrap();
    std::fs::create_dir(home.join("ignores/global")).unwrap();
    assert_eq!(git::owner_excludes(&home, None), None);
    write(
        &home.join(".gitconfig"),
        "[core]\n\texcludesFile = relative/ignore\n",
    );
    write(&home.join("relative/ignore"), "secret.env\n");
    assert_eq!(git::owner_excludes(&home, None), None);
}

#[test]
fn files_the_owners_global_ignore_file_excludes_stay_out_of_change_sets() {
    let configured: [(&str, Option<&str>); 2] = [
        (".config/git/ignore", None),
        (
            "ignores/global",
            Some("[core]\n\texcludesFile = ~/ignores/global\n"),
        ),
    ];
    for (file, config) in configured {
        let case = Case::new();
        write(&case.home.join(file), "secret.env\n");
        if let Some(config) = config {
            write(&case.home.join(".gitconfig"), config);
        }
        case.edit("README", "one\ntwo\nthree\nfour\n");
        case.edit("secret.env", "TOKEN=x\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let view = case.view(&id);
        assert_eq!(view.files, 1, "{file}");
        assert_eq!(
            case.owner(&["ls-tree", "-r", "--name-only", &view.head]),
            "README\n",
            "{file}"
        );
        assert_eq!(case.agent(&["ls-files", "--", "secret.env"]), "", "{file}");
        discard(&case.db, &id, &view.head, NOW_MS).unwrap();
        assert_eq!(case.agent_head(), case.tip(), "{file}");
        assert_eq!(
            std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(
            std::fs::read_to_string(case.workspace.worktree.join("secret.env")).unwrap(),
            "TOKEN=x\n"
        );
        case.agent(&["add", "-f", "secret.env"]);
        let head = case.agent_head();
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-2".into(),
            prompt_line: "Add the env file".into(),
        };
        assert_eq!(
            refused(record_turn_end(&case.db, &case.workspace, &turn)),
            "changes_ignored_files",
            "{file}"
        );
        assert_eq!(case.agent_head(), head, "{file}");
        assert_eq!(case.view(&id).state, ChangeState::Discarded, "{file}");
        assert_eq!(
            list(&case.db, &case.workspace.project).unwrap().len(),
            1,
            "{file}"
        );
    }
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
        "Owner title\u{e0041}\u{e0042}",
        "Zero\u{200b}width",
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
fn hidden_characters_in_an_agent_subject_reach_the_owner_and_the_commit_only_as_escapes() {
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Formats changed files.\u{e0041}\u{e0042}\u{200b}");
    let id = case
        .turn_with(TurnOutcome::Succeeded, "Ignored prompt line")
        .unwrap();
    assert_eq!(
        case.view(&id).message,
        "Formats changed files.\\u{e0041}\\u{e0042}\\u200b"
    );
    let roots = [case.repo.as_path()];
    assert_eq!(
        flow::subject("Fix\u{2060}the\u{e0001} parser", &roots),
        "Fix\\u2060the\\u{e0001} parser"
    );
    case.db
        .execute(
            "UPDATE landing_change_sets SET message = ?1",
            ["Stored before.\u{e0041}\u{feff}"],
        )
        .unwrap();
    let stored = "Stored before.\\u{e0041}\\ufeff";
    assert_eq!(case.view(&id).message, stored);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), stored);
    assert!(!crate::memory::hidden_text(&case.landed_message(&commit)));
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

fn all_pages(case: &Case, id: &ChangeSetId) -> Vec<ChangeDiff> {
    let mut pages = vec![diff(&case.db, id, 0).unwrap()];
    while let Some(next) = pages.last().and_then(|page| page.next) {
        pages.push(diff(&case.db, id, next).unwrap());
    }
    pages
}

#[test]
fn later_diff_pages_are_read_from_the_first_computation_without_running_git() {
    let case = Case::new();
    let big: String = (0..3000).map(|line| format!("line {line}\n")).collect();
    case.edit("big.txt", &big);
    case.edit("notes.txt", "a note\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let first = all_pages(&case, &id);
    assert!(first.len() > 1, "a 3000-line file fit one page");
    assert!(first.iter().all(|page| !page.truncated));
    let text: String = first.iter().map(|page| page.text.as_str()).collect();
    assert!(text.contains("+line 0\n") && text.contains("+line 2999\n"));
    assert!(text.contains("+a note\n"));
    let record = store::load(&case.db, &id).unwrap();
    let (git, moved) = (case.repo.join(".git"), case.repo.join("git-moved"));
    std::fs::rename(&git, &moved).unwrap();
    assert!(diff::page(&record.repository, &record.head, &record.base, 0).is_err());
    let again = all_pages(&case, &id);
    let past = diff(&case.db, &id, 900).unwrap();
    std::fs::rename(&moved, &git).unwrap();
    assert_eq!(again, first);
    assert!(past.files.is_empty() && past.text.is_empty() && past.next.is_none());
}

impl Case {
    /// Writes a loose object file under `id` holding the bytes of another object, as a
    /// confined agent could once plant one: git keeps an existing object instead of writing
    /// its own and reads a loose object without checking its bytes. Inputs: the id to plant
    /// and the text whose object bytes it gets. Output: none; the file is in place.
    fn plant(&self, id: &str, text: &str) {
        let source = self._tmp.path().join("planted.txt");
        std::fs::write(&source, text).unwrap();
        let other = self.owner(&["hash-object", "-w", source.to_str().unwrap()]);
        let objects = self.repo.join(".git").join("objects");
        let other = other.trim();
        let bytes = std::fs::read(objects.join(&other[..2]).join(&other[2..])).unwrap();
        let target = objects.join(&id[..2]).join(&id[2..]);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        if target.exists() {
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        std::fs::write(&target, bytes).unwrap();
        assert_ne!(id, other);
    }

    /// The id git gives a text as a blob, without storing it. Input: the text. Output: the id.
    fn blob_id(&self, text: &str) -> String {
        let source = self._tmp.path().join("predicted.txt");
        std::fs::write(&source, text).unwrap();
        self.owner(&["hash-object", source.to_str().unwrap()])
            .trim()
            .to_owned()
    }
}

#[test]
fn an_agent_head_naming_another_branch_is_refused_and_moves_no_branch() {
    let case = Case::new();
    case.start();
    let tip = case.tip();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let head_file = case.workspace.git_dir.join("HEAD");
    let detached = std::fs::read_to_string(&head_file).unwrap();
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-2".into(),
        prompt_line: "More".into(),
    };
    std::fs::write(&head_file, "ref: refs/heads/main\n").unwrap();
    case.edit("evil.txt", "evil\n");
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_workspace_invalid"
    );
    assert_eq!(refused(update(&case.db, &id)), "changes_workspace_invalid");
    assert_eq!(case.tip(), tip, "the owner's branch moved without Accept");
    std::fs::remove_file(&head_file).unwrap();
    std::os::unix::fs::symlink(case.repo.join(".git").join("HEAD"), &head_file).unwrap();
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_workspace_invalid"
    );
    assert_eq!(case.tip(), tip);
    std::fs::remove_file(&head_file).unwrap();
    std::fs::write(&head_file, &detached).unwrap();
    std::fs::remove_file(case.workspace.worktree.join("evil.txt")).unwrap();
    assert_eq!(case.agent_head(), head);
    case.agent(&["checkout", "-q", "-b", "crazyagent/agent-1"]);
    case.edit("lease.txt", "lease\n");
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_workspace_invalid",
        "a sub-agent worktree must stay detached"
    );
    let named = case.owner(&["rev-parse", "refs/heads/crazyagent/agent-1"]);
    assert_eq!(named.trim(), head);
    assert_eq!(case.view(&id).head, head);
    assert_eq!(case.tip(), tip, "a lease turn end moved the owner's branch");
    assert_eq!(
        std::fs::read_to_string(&head_file).unwrap(),
        "ref: refs/heads/crazyagent/agent-1\n"
    );
}

#[test]
fn accept_and_update_refuse_an_object_whose_bytes_do_not_match_its_id() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let blob = case.blob_id("one\ntwo\nthree\nfour\n");
    let before = case.checkout();
    case.plant(&blob, "one\ntwo\nthree\nfour\nunreviewed\n");
    assert_eq!(refused(case.accept(&id)), "changes_workspace_invalid");
    assert_eq!(case.checkout(), before, "a refused Accept wrote");
    assert_eq!(refused(update(&case.db, &id)), "changes_workspace_invalid");
    assert_eq!(case.checkout(), before);
}

#[test]
fn accept_refuses_a_merge_whose_predicted_object_was_planted_first() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let tip = case.owner_commit("README", "ONE\ntwo\nthree\n", "Owner edit");
    let merged = case.blob_id("ONE\ntwo\nthree\nfour\n");
    case.plant(&merged, "ONE\ntwo\nthree\nfour\nunreviewed\n");
    let before = case.checkout();
    assert_eq!(refused(case.accept(&id)), "changes_workspace_invalid");
    assert_eq!(case.checkout(), before, "a refused Accept wrote");
    assert_eq!(case.tip(), tip);
    let planted = case
        .repo
        .join(".git/objects")
        .join(&merged[..2])
        .join(&merged[2..]);
    std::fs::remove_file(planted).unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "ONE\ntwo\nthree\nfour\n"
    );
}

impl Case {
    /// The same owner repository with the agent in its own clone (DEC-102 Own branch)
    /// instead of a linked worktree, its start recorded. Output: the case.
    fn own_branch() -> Self {
        let mut case = Self::new();
        let worktree = case.repo.parent().unwrap().join("agent-own");
        add_agent_clone(&case.repo, &worktree).unwrap();
        case.workspace.git_dir = crate::agent_git::git_dir(&worktree);
        case.workspace.worktree = worktree;
        case.start();
        case
    }

    /// The owner repository's refs under one prefix, name and commit, one per line.
    /// Input: the prefix. Output: the listing.
    fn owner_refs(&self, prefix: &str) -> String {
        self.owner(&["for-each-ref", "--format=%(refname) %(objectname)", prefix])
    }

    /// Plants in the agent's own clone, under a blob id its commit stored there, the
    /// stored bytes of another text, as an agent with full git in its clone can.
    /// Inputs: the id and the text.
    fn plant_in_clone(&self, id: &str, text: &str) {
        let source = self._tmp.path().join("clone-planted.txt");
        std::fs::write(&source, text).unwrap();
        let other = self.agent(&["hash-object", "-w", source.to_str().unwrap()]);
        let objects = self.workspace.git_dir.join("objects");
        let other = other.trim();
        let bytes = std::fs::read(objects.join(&other[..2]).join(&other[2..])).unwrap();
        let target = objects.join(&id[..2]).join(&id[2..]);
        assert!(
            target.exists(),
            "the agent's commit stored no blob in its clone"
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&target, bytes).unwrap();
        assert_ne!(id, other);
    }

    /// Tells whether the owner repository's object store holds an object, read as the
    /// owner's git reads it. Input: the id. Output: the answer.
    fn owner_has(&self, id: &str) -> bool {
        crate::spawn_gate::output(
            Command::new("/usr/bin/git")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .arg("-C")
                .arg(&self.repo)
                .args(["cat-file", "-e", id])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .unwrap()
        .status
        .success()
    }

    /// Makes a repository beside the owner's that the agent's sandbox would not let it
    /// read, with one commit holding one file. Inputs: the folder name and the file's text.
    /// Output: the repository folder and its commit.
    fn foreign(&self, name: &str, text: &str) -> (PathBuf, String) {
        let foreign = self.repo.parent().unwrap().join(name);
        std::fs::create_dir(&foreign).unwrap();
        git_in(
            &self.home,
            &foreign,
            &["init", "-q", "--template=", "-b", "main"],
        );
        write(&foreign.join("secret.txt"), text);
        git_in(&self.home, &foreign, &["add", "secret.txt"]);
        git_in(
            &self.home,
            &foreign,
            &[
                "-c",
                "user.name=Other",
                "-c",
                "user.email=other@example.invalid",
                "commit",
                "-q",
                "-m",
                "Other",
            ],
        );
        let commit = git_in(&self.home, &foreign, &["rev-parse", "HEAD"]);
        (foreign, commit.trim().to_owned())
    }
}

fn own_branch_turn() -> TurnEnd {
    TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Add the fourth line".into(),
    }
}

#[test]
fn an_own_branch_agent_commits_on_its_branch_and_the_project_refs_stay_as_they_were() {
    let case = Case::own_branch();
    let base = case.tip();
    let clone = case.workspace.git_dir.clone();
    assert!(std::fs::symlink_metadata(&clone).unwrap().is_dir());
    assert_eq!(
        std::fs::read_to_string(clone.join("HEAD")).unwrap(),
        "ref: refs/heads/agent/agent-own\n"
    );
    assert_eq!(
        std::fs::read_to_string(clone.join("objects/info/alternates")).unwrap(),
        format!("{}\n", case.repo.join(".git/objects").display())
    );
    assert_eq!(
        (case.agent_head(), case.agent(&["status", "--porcelain"])),
        (base.clone(), String::new())
    );
    let heads = case.owner_refs("refs/heads");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.agent_commit("Add the fourth line");
    case.agent(&["branch", "side"]);
    case.agent(&["tag", "v1"]);
    assert_eq!(case.owner_refs("refs/heads"), heads);
    assert_eq!(case.owner_refs("refs/tags"), "");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&id);
    assert_eq!(
        (view.base.as_str(), view.head.as_str(), view.files),
        (base.as_str(), first.as_str(), 1)
    );
    assert_eq!(case.owner(&["cat-file", "-t", &first]), "commit\n");
    assert_eq!(case.owner_refs("refs/heads"), heads);
    assert_eq!(case.owner_refs("refs/tags"), "");
    assert_eq!(
        case.owner_refs("refs/crazyagent"),
        format!("refs/crazyagent/pending/{} {first}\n", id.as_str())
    );
    case.edit("README", "uncommitted\n");
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    assert_eq!(
        case.view(&id).head,
        first,
        "uncommitted work entered the change set"
    );
    assert_eq!(case.tip(), base);
}

#[test]
fn accept_lands_the_commits_an_own_branch_agent_made_and_leaves_its_branch_alone() {
    let case = Case::own_branch();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    case.edit("src/new.rs", "fn main() {}\n");
    let head = case.agent_commit("Add main");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let before = case.owner_commit("other.txt", "owner work\n", "Owner work");
    let AcceptOutcome::Landed { commit, branch } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%an <%ae>", &commit])
            .trim(),
        format!("{before}|Repo Owner <owner@example.invalid>")
    );
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &commit]),
        "README\nother.txt\nsrc/new.rs\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    assert_eq!(case.view(&id).state, ChangeState::Accepted);
    assert_eq!(case.agent_head(), head, "Accept moved the agent's branch");
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
    case.edit("README", "one\ntwo\nthree\nfour\nfive\n");
    case.agent_commit("Add the fifth line");
    let next = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&next).base, head);
    let AcceptOutcome::Landed { commit: second, .. } = case.accept(&next).unwrap() else {
        panic!("the second accept conflicted");
    };
    assert_eq!(
        case.owner(&["show", &format!("{second}:README")]),
        "one\ntwo\nthree\nfour\nfive\n"
    );
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &second]),
        "README\nother.txt\nsrc/new.rs\n"
    );
}

#[test]
fn an_own_branch_agent_resolves_an_update_on_its_branch_and_accept_lands_it() {
    let case = Case::own_branch();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let head = case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let tip = case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    update(&case.db, &id).unwrap();
    let merged = case.view(&id).head;
    assert_eq!(case.agent_head(), head, "update moved the agent's branch");
    let prompt = resolve_task_prompt(&case.db, &id).unwrap();
    assert!(
        prompt.contains(&format!("git merge {merged}")) && !prompt.contains("--ff-only"),
        "{prompt}"
    );
    case.agent(&["merge", "-q", "--ff-only", &merged]);
    case.edit("README", "one\ntwo\nTHREE\nfour\n");
    case.agent_commit("Resolve the owner's edit");
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.base.as_str()),
        (ChangeState::Pending, tip.as_str())
    );
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the resolved accept conflicted");
    };
    assert_eq!(
        case.owner(&["show", &format!("{commit}:README")]),
        "one\ntwo\nTHREE\nfour\n"
    );
}

#[test]
fn an_own_branch_agent_cannot_move_the_owner_branch_or_change_what_accept_lands() {
    let case = Case::own_branch();
    let tip = case.tip();
    let heads = case.owner_refs("refs/heads");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let reviewed = case.agent_commit("Add the fourth line");
    case.agent(&["update-ref", "refs/heads/main", &reviewed]);
    case.agent(&["symbolic-ref", "HEAD", "refs/heads/main"]);
    case.agent(&[
        "symbolic-ref",
        "refs/heads/agent/agent-own",
        "refs/heads/main",
    ]);
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).head, reviewed);
    assert_eq!(
        case.owner_refs("refs/heads"),
        heads,
        "a clone ref moved an owner ref"
    );
    case.edit("README", "one\ntwo\nthree\nfour\nunreviewed\n");
    let swapped = case.agent_commit("Swap after review");
    assert_ne!(swapped, reviewed);
    assert_eq!(case.tip(), tip);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        case.owner(&["show", &format!("{commit}:README")]),
        "one\ntwo\nthree\nfour\n",
        "Accept landed what the agent swapped in after review"
    );
    let git_dir = case.workspace.git_dir.clone();
    std::fs::rename(&git_dir, case.repo.parent().unwrap().join("moved-git")).unwrap();
    std::os::unix::fs::symlink(case.repo.join(".git"), &git_dir).unwrap();
    let heads = case.owner_refs("refs/heads");
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_workspace_invalid"
    );
    assert_eq!(case.owner_refs("refs/heads"), heads);
}

#[test]
fn objects_planted_in_an_own_branch_clone_never_reach_a_change_set() {
    let case = Case::own_branch();
    let tip = case.tip();
    case.edit("README", "one\ntwo\nthree\nreviewed four\n");
    case.agent_commit("Add the fourth line");
    let blob = case.blob_id("one\ntwo\nthree\nreviewed four\n");
    case.plant_in_clone(&blob, "one\ntwo\nthree\nunreviewed four\n");
    let before = case.checkout();
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_agent_branch_invalid"
    );
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
    assert_eq!(case.owner_refs("refs/crazyagent"), "");
    assert_eq!(case.tip(), tip);
    assert_eq!(
        case.checkout(),
        before,
        "a refused fetch changed the checkout"
    );
}

/// An agent in its own clone that commits on its old head instead of merging the update
/// is refused at its turn end, and the set shows as waiting for its agent again with a
/// prompt that merges without `--ff-only`, so one more resolve lands its work (GA-R3).
#[test]
fn an_own_branch_agent_that_skips_the_update_merge_is_asked_again_and_its_merge_lands() {
    let case = Case::own_branch();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let tip = case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    update(&case.db, &id).unwrap();
    resolve_handed_over(&case.db, &id).unwrap();
    let merged = case.view(&id).head;
    assert!(!case.view(&id).waiting);
    case.edit("src/new.rs", "fn main() {}\n");
    case.agent_commit("Add main on the old head");
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_branch_not_above_base"
    );
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.waiting, view.head.as_str()),
        (ChangeState::Updating, true, merged.as_str()),
        "the refused set does not wait for its agent"
    );
    let prompt = resolve_task_prompt(&case.db, &id).unwrap();
    assert!(
        prompt.contains(&format!("git merge {merged}")) && !prompt.contains("--ff-only"),
        "{prompt}"
    );
    case.agent(&[
        "-c",
        "user.name=Agent",
        "-c",
        "user.email=agent@example.invalid",
        "merge",
        "-q",
        "--no-edit",
        &merged,
    ]);
    case.edit("README", "one\ntwo\nTHREE\nfour\n");
    case.agent_commit("Resolve the owner's edit");
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.base.as_str()),
        (ChangeState::Pending, tip.as_str())
    );
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the resolved accept conflicted");
    };
    assert_eq!(
        case.owner(&["show", &format!("{commit}:README")]),
        "one\ntwo\nTHREE\nfour\n"
    );
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &commit]),
        "README\nsrc/new.rs\n"
    );
}

/// The engine's fetch never reads an object store the agent added to its clone: a second
/// alternates line, or a pack linked in from another repository, is refused before git
/// runs, and none of that repository's objects reach the project (DEC102-F1).
#[test]
fn a_clone_that_borrows_another_object_store_is_refused_and_imports_nothing() {
    let case = Case::own_branch();
    let (foreign, commit) = case.foreign("foreign", "not for the agent\n");
    let secret = case.blob_id("not for the agent\n");
    let alternates = case.workspace.git_dir.join("objects/info/alternates");
    let engine_line = std::fs::read_to_string(&alternates).unwrap();
    std::fs::write(
        &alternates,
        format!("{engine_line}{}\n", foreign.join(".git/objects").display()),
    )
    .unwrap();
    case.agent(&["update-ref", "refs/heads/agent/agent-own", &commit]);
    let heads = case.owner_refs("refs/heads");
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_workspace_invalid"
    );
    assert!(
        !case.owner_has(&secret),
        "a foreign object reached the project"
    );

    std::fs::write(&alternates, &engine_line).unwrap();
    git_in(&case.home, &foreign, &["repack", "-a", "-d", "-q"]);
    let packs = foreign.join(".git/objects/pack");
    for entry in std::fs::read_dir(&packs).unwrap() {
        let name = entry.unwrap().file_name();
        std::os::unix::fs::symlink(
            packs.join(&name),
            case.workspace.git_dir.join("objects/pack").join(&name),
        )
        .unwrap();
    }
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_workspace_invalid"
    );
    assert!(
        !case.owner_has(&secret),
        "a foreign object reached the project"
    );
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
    assert_eq!(case.owner_refs("refs/heads"), heads);
    assert_eq!(case.owner_refs("refs/crazyagent"), "");
}

/// One way an agent can make git serve another repository, history or object source
/// from its clone: its name with the rows of rv-audit's DEC102-F1 redirect list it covers,
/// and how it is planted given the case, the foreign repository and its commit.
struct Redirect {
    name: &'static str,
    plant: fn(&Case, &Path, &str),
}

/// Appends a section to the clone's config. Inputs: the case and the text.
fn configure(case: &Case, text: &str) {
    let path = case.workspace.git_dir.join("config");
    let mut config = std::fs::read_to_string(&path).unwrap();
    config.push_str(text);
    std::fs::write(path, config).unwrap();
}

/// Points the agent's branch in its clone at a commit by writing the ref file, as the
/// agent can without git checking that the commit is there. Inputs: the case and the
/// commit.
fn branch_at(case: &Case, commit: &str) {
    write(
        &case.workspace.git_dir.join("refs/heads/agent/agent-own"),
        &format!("{commit}\n"),
    );
}

/// The files in the foreign repository's loose object folders, or in its pack folder after
/// packing it when `packed`. Inputs: the case, the foreign repository and whether to pack.
/// Output: each file's folder and name.
fn foreign_files(case: &Case, foreign: &Path, packed: bool) -> Vec<(PathBuf, std::ffi::OsString)> {
    let objects = foreign.join(".git/objects");
    if packed {
        git_in(&case.home, foreign, &["repack", "-a", "-d", "-q"]);
    }
    let mut files = Vec::new();
    for folder in std::fs::read_dir(&objects).unwrap() {
        let folder = folder.unwrap().path();
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        if (packed && name == "pack") || (!packed && name.len() == 2) {
            for file in std::fs::read_dir(&folder).unwrap() {
                files.push((folder.clone(), file.unwrap().file_name()));
            }
        }
    }
    assert!(!files.is_empty());
    files
}

const REDIRECTS: [Redirect; 21] = [
    Redirect {
        name: "#1 commondir",
        plant: |case, foreign, _| {
            write(
                &case.workspace.git_dir.join("commondir"),
                &format!("{}\n", foreign.join(".git").display()),
            );
        },
    },
    Redirect {
        name: "#2 nested .git folder",
        plant: |case, foreign, _| {
            let nested = case.workspace.git_dir.join(".git");
            git_in(
                &case.home,
                foreign,
                &[
                    "clone",
                    "--bare",
                    "-q",
                    "--no-local",
                    ".",
                    nested.to_str().unwrap(),
                ],
            );
        },
    },
    Redirect {
        name: "#2 nested .git file",
        plant: |case, foreign, _| {
            write(
                &case.workspace.git_dir.join(".git"),
                &format!("gitdir: {}\n", foreign.join(".git").display()),
            );
        },
    },
    Redirect {
        name: "#2 nested .git link",
        plant: |case, foreign, _| {
            std::os::unix::fs::symlink(foreign.join(".git"), case.workspace.git_dir.join(".git"))
                .unwrap();
        },
    },
    Redirect {
        name: "#3 .git.git file beside an unreadable HEAD (RL-1)",
        plant: |case, foreign, _| {
            write(&case.workspace.git_dir.join("HEAD"), "not a head\n");
            write(
                &case.workspace.worktree.join(".git.git"),
                &format!("gitdir: {}\n", foreign.join(".git").display()),
            );
        },
    },
    Redirect {
        name: "#3 .git.git link beside moved refs (RL-1)",
        plant: |case, foreign, _| {
            let git_dir = &case.workspace.git_dir;
            std::fs::rename(git_dir.join("refs"), git_dir.join("refs-moved")).unwrap();
            std::os::unix::fs::symlink(
                foreign.join(".git"),
                case.workspace.worktree.join(".git.git"),
            )
            .unwrap();
        },
    },
    Redirect {
        name: "#4 clone .git as a gitdir file",
        plant: |case, foreign, _| {
            let git_dir = &case.workspace.git_dir;
            std::fs::rename(git_dir, git_dir.with_file_name("git-aside")).unwrap();
            write(
                git_dir,
                &format!("gitdir: {}\n", foreign.join(".git").display()),
            );
        },
    },
    Redirect {
        name: "#4 clone .git as a link",
        plant: |case, foreign, _| {
            let git_dir = &case.workspace.git_dir;
            std::fs::rename(git_dir, git_dir.with_file_name("git-aside")).unwrap();
            std::os::unix::fs::symlink(foreign.join(".git"), git_dir).unwrap();
        },
    },
    Redirect {
        name: "#5 alternates naming only a foreign store",
        plant: |case, foreign, commit| {
            write(
                &case.workspace.git_dir.join("objects/info/alternates"),
                &format!("{}\n", foreign.join(".git/objects").display()),
            );
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#7 loose objects as links",
        plant: |case, foreign, commit| {
            for (folder, name) in foreign_files(case, foreign, false) {
                let target = case
                    .workspace
                    .git_dir
                    .join("objects")
                    .join(folder.file_name().unwrap());
                std::fs::create_dir_all(&target).unwrap();
                std::os::unix::fs::symlink(folder.join(&name), target.join(&name)).unwrap();
            }
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#8 fanout folders as links",
        plant: |case, foreign, commit| {
            let mut linked = 0;
            for (folder, _) in foreign_files(case, foreign, false) {
                let target = case
                    .workspace
                    .git_dir
                    .join("objects")
                    .join(folder.file_name().unwrap());
                if std::fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_none()) {
                    std::fs::remove_dir(&target).unwrap();
                }
                if std::fs::symlink_metadata(&target).is_err() {
                    std::os::unix::fs::symlink(&folder, target).unwrap();
                    linked += 1;
                }
            }
            assert!(linked > 0, "no fan-out folder was linked");
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#9 packs as links",
        plant: |case, foreign, commit| {
            let packs = case.workspace.git_dir.join("objects/pack");
            for (folder, name) in foreign_files(case, foreign, true) {
                std::os::unix::fs::symlink(folder.join(&name), packs.join(&name)).unwrap();
            }
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#10 packs as hard links",
        plant: |case, foreign, commit| {
            let packs = case.workspace.git_dir.join("objects/pack");
            for (folder, name) in foreign_files(case, foreign, true) {
                std::fs::hard_link(folder.join(&name), packs.join(&name)).unwrap();
            }
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#11 objects folder as a link",
        plant: |case, foreign, commit| {
            let objects = case.workspace.git_dir.join("objects");
            std::fs::rename(&objects, objects.with_file_name("objects-aside")).unwrap();
            std::os::unix::fs::symlink(foreign.join(".git/objects"), &objects).unwrap();
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#12 multi-pack-index (RL-2)",
        plant: |case, foreign, commit| {
            foreign_files(case, foreign, true);
            git_in(&case.home, foreign, &["multi-pack-index", "write"]);
            std::fs::copy(
                foreign.join(".git/objects/pack/multi-pack-index"),
                case.workspace.git_dir.join("objects/pack/multi-pack-index"),
            )
            .unwrap();
            branch_at(case, commit);
        },
    },
    Redirect {
        name: "#22 shallow",
        plant: |case, _, _| {
            let head = case.agent(&["rev-parse", "HEAD"]);
            write(&case.workspace.git_dir.join("shallow"), &head);
        },
    },
    Redirect {
        name: "#21 info/grafts",
        plant: |case, _, _| {
            let head = case.agent(&["rev-parse", "HEAD"]);
            let info = case.workspace.git_dir.join("info");
            std::fs::create_dir_all(&info).unwrap();
            write(&info.join("grafts"), &head);
        },
    },
    Redirect {
        name: "#17 include.path",
        plant: |case, foreign, _| {
            configure(
                case,
                &format!(
                    "[include]\n\tpath = {}\n",
                    foreign.join(".git/config").display()
                ),
            );
        },
    },
    Redirect {
        name: "#16 #18 includeIf path and extensions.worktreeConfig",
        plant: |case, foreign, _| {
            configure(
                case,
                &format!(
                    "[includeIf \"gitdir:/\"]\n\tpath = {}\n[extensions]\n\tworktreeConfig = true\n",
                    foreign.join(".git/config").display()
                ),
            );
            write(
                &case.workspace.git_dir.join("config.worktree"),
                "[core]\n\tbare = false\n",
            );
        },
    },
    Redirect {
        name: "#15 extensions.refStorage",
        plant: |case, _, _| {
            configure(
                case,
                "[core]\n\trepositoryformatversion = 1\n[extensions]\n\trefStorage = files\n",
            );
        },
    },
    Redirect {
        name: "#19 promisor remote and extensions.partialClone",
        plant: |case, foreign, _| {
            configure(
                case,
                &format!(
                    "[remote \"origin\"]\n\turl = {}\n\tpromisor = true\n[Extensions]\n\tPartialClone = origin\n",
                    foreign.display()
                ),
            );
        },
    },
];

/// The engine's fetch never serves another repository, history or object source an agent
/// names from its clone (DEC102-F1, AL-1, DEC102-F2, RL-1, RL-2): every row of rv-audit's
/// redirect list that lands objects, and the config, history and lazy-fetch rows, is
/// planted in a clone of its own and refused as `changes_workspace_invalid`, with none of
/// the foreign repository's objects in the project, no change set and no ref left. Every
/// row is tried before the test reports.
#[test]
fn a_clone_that_redirects_its_repository_is_refused_and_imports_nothing() {
    let mut held = Vec::new();
    for redirect in &REDIRECTS {
        let case = Case::own_branch();
        let text = format!("not for the agent: {}\n", redirect.name);
        let (foreign, commit) = case.foreign("foreign", &text);
        git_in(&case.home, &foreign, &["branch", "agent/agent-own"]);
        let secret = case.blob_id(&text);
        let heads = case.owner_refs("refs/heads");
        (redirect.plant)(&case, &foreign, &commit);
        let code = record_turn_end(&case.db, &case.workspace, &own_branch_turn())
            .map(|_| "accepted".to_owned())
            .unwrap_or_else(|error| error.0);
        let leaked = case.owner_has(&secret);
        let sets = list(&case.db, &case.workspace.project).unwrap().len();
        let moved = case.owner_refs("refs/heads") != heads
            || !case.owner_refs("refs/crazyagent").is_empty();
        if code != "changes_workspace_invalid" || leaked || sets != 0 || moved {
            held.push(format!(
                "{}: {code}, foreign object imported {leaked}, change sets {sets}, refs moved {moved}",
                redirect.name
            ));
        }
    }
    assert!(held.is_empty(), "{held:#?}");
}

/// Git serves the clone's git folder itself even when the agent changes its clone after
/// the engine checked it: a `.git.git` made between the checks and the fetch, beside a
/// git folder git cannot read, is never served (upload-pack `--strict`, RL-1).
#[test]
fn a_redirect_made_after_the_checks_is_never_served() {
    let case = Case::own_branch();
    let text = "not for the agent: after the checks\n";
    let (foreign, _) = case.foreign("foreign", text);
    git_in(&case.home, &foreign, &["branch", "agent/agent-own"]);
    let secret = case.blob_id(text);
    let heads = case.owner_refs("refs/heads");
    super::clone::BETWEEN_CHECK_AND_FETCH.set(Some(|git_dir: &Path| {
        let worktree = git_dir.parent().unwrap();
        let foreign = worktree.parent().unwrap().join("foreign/.git");
        std::fs::write(git_dir.join("HEAD"), "not a head\n").unwrap();
        std::fs::write(
            worktree.join(".git.git"),
            format!("gitdir: {}\n", foreign.display()),
        )
        .unwrap();
    }));
    let code = refused(record_turn_end(
        &case.db,
        &case.workspace,
        &own_branch_turn(),
    ));
    super::clone::BETWEEN_CHECK_AND_FETCH.set(None);
    assert_eq!(code, "changes_agent_branch_invalid");
    assert!(
        !case.owner_has(&secret),
        "the fallback repository was served"
    );
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
    assert_eq!(case.owner_refs("refs/heads"), heads);
    assert_eq!(case.owner_refs("refs/crazyagent"), "");
}

/// Temp files an interrupted git command leaves in the clone's pack folder do not stop
/// the agent's turn end, since git never serves them; any other name there still does.
#[test]
fn git_temp_files_left_in_a_clone_s_pack_folder_let_the_turn_end() {
    let case = Case::own_branch();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let packs = case.workspace.git_dir.join("objects/pack");
    write(&packs.join("tmp_pack_x.idx"), "partial");
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_workspace_invalid"
    );
    std::fs::remove_file(packs.join("tmp_pack_x.idx")).unwrap();
    for name in ["tmp_pack_Ab3dE9", "tmp_idx_zZ09aq", "tmp_rev_000000"] {
        write(&packs.join(name), "partial");
    }
    let id = record_turn_end(&case.db, &case.workspace, &own_branch_turn())
        .unwrap()
        .expect("the turn's commit makes a change set");
    let listed = list(&case.db, &case.workspace.project).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].files, 1);
}

/// Files an interrupted `git repack` leaves in the clone's pack folder, named as git
/// names them (`.tmp-<pid>-pack-<id>.<extension>`), do not stop the agent's turn end and
/// are never served, while a temp pack git would read, its `.idx` beside its `.pack`, and
/// any other `.tmp-` name are still refused (RL3-N1).
#[test]
fn repack_temp_files_let_the_turn_end_but_a_temp_pack_git_would_read_is_refused() {
    let case = Case::own_branch();
    let text = "not for the agent: a repack temp pack\n";
    let (foreign, commit) = case.foreign("foreign", text);
    let secret = case.blob_id(text);
    let packs = case.workspace.git_dir.join("objects/pack");
    let mut pack_id = None;
    for (folder, name) in foreign_files(&case, &foreign, true) {
        let name = name.to_str().unwrap();
        let rest = name.strip_prefix("pack-").unwrap();
        std::fs::copy(
            folder.join(name),
            packs.join(format!(".tmp-4242-pack-{rest}")),
        )
        .unwrap();
        pack_id = rest.split_once('.').map(|(found, _)| found.to_owned());
    }
    let pack_id = pack_id.unwrap();
    let temp =
        |pid: &str, extension: &str| packs.join(format!(".tmp-{pid}-pack-{pack_id}.{extension}"));
    let own = case.agent_head();
    branch_at(&case, &commit);
    let turn_end = || {
        record_turn_end(&case.db, &case.workspace, &own_branch_turn())
            .map(|_| "accepted".to_owned())
            .unwrap_or_else(|error| error.0)
    };
    assert_eq!(turn_end(), "changes_workspace_invalid");

    std::fs::rename(temp("4242", "pack"), temp("31337", "pack")).unwrap();
    for extension in ["mtimes", "bitmap", "promisor"] {
        write(&temp("4242", extension), "partial");
    }
    for refused_name in [
        temp("4242", "keep"),
        temp("04242", "rev"),
        packs.join(format!(".tmp-4243-pack-{}.rev", pack_id.to_uppercase())),
        packs.join(format!(".tmp-4242-cruft-{pack_id}.pack")),
    ] {
        write(&refused_name, "partial");
        assert_eq!(turn_end(), "changes_workspace_invalid", "{refused_name:?}");
        std::fs::remove_file(&refused_name).unwrap();
    }
    assert_eq!(turn_end(), "changes_agent_branch_invalid");
    assert!(temp("4242", "idx").is_file() && temp("31337", "pack").is_file());
    assert!(!case.owner_has(&secret), "a temp pack was served");
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
    assert!(case.owner_refs("refs/crazyagent").is_empty());

    branch_at(&case, &own);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = record_turn_end(&case.db, &case.workspace, &own_branch_turn())
        .unwrap()
        .expect("the turn's commit makes a change set");
    let listed = list(&case.db, &case.workspace.project).unwrap();
    assert_eq!((listed.len(), &listed[0].id, listed[0].files), (1, &id, 1));
}

/// The store folders of a clone, `pack` and the 256 loose-object folders, that the Own
/// branch sandbox keeps the agent from making, moving or replacing (RL-3).
/// Input: the clone's git folder. Output: each folder.
fn store_folder_paths(git_dir: &Path) -> Vec<PathBuf> {
    let objects = git_dir.join("objects");
    std::iter::once(objects.join("pack"))
        .chain((0..=u8::MAX).map(|byte| objects.join(format!("{byte:02x}"))))
        .collect()
}

/// A new clone already holds its pack folder and all 256 loose-object folders, so the
/// Own branch sandbox can deny making, moving or replacing them, and the agent's git still
/// commits, repacks, collects garbage and prunes, and its turn end lands (RL-3).
#[test]
fn a_new_clone_holds_its_store_folders_and_its_git_still_commits_repacks_and_prunes() {
    let case = Case::own_branch();
    let missing: Vec<PathBuf> = store_folder_paths(&case.workspace.git_dir)
        .into_iter()
        .filter(|folder| !std::fs::symlink_metadata(folder).is_ok_and(|found| found.is_dir()))
        .collect();
    assert!(missing.is_empty(), "{} missing: {missing:?}", missing.len());
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    case.agent(&["repack", "-a", "-d", "-q"]);
    case.agent(&["gc", "-q"]);
    case.agent(&["prune", "--expire=now"]);
    case.edit("NEW", "new\n");
    case.agent_commit("Add a file");
    case.agent(&["fsck", "--no-progress"]);
    let id = record_turn_end(&case.db, &case.workspace, &own_branch_turn())
        .unwrap()
        .expect("the agent's commits make a change set");
    let listed = list(&case.db, &case.workspace.project).unwrap();
    assert_eq!((listed.len(), &listed[0].id, listed[0].files), (1, &id, 2));
}

/// A clone launched again gets back any store folder its git removed, and the engine
/// never follows a link the agent put in place of its object folder (RL-3).
#[test]
fn a_reused_clone_gets_its_store_folders_back_and_a_linked_object_folder_is_refused() {
    let case = Case::own_branch();
    let git_dir = case.workspace.git_dir.clone();
    for name in ["00", "ab", "ff"] {
        std::fs::remove_dir(git_dir.join("objects").join(name)).unwrap();
    }
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert!(
        store_folder_paths(&git_dir)
            .iter()
            .all(|folder| std::fs::symlink_metadata(folder).is_ok_and(|found| found.is_dir()))
    );

    let elsewhere = case.repo.parent().unwrap().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let objects = git_dir.join("objects");
    std::fs::rename(&objects, git_dir.join("objects-aside")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &objects).unwrap();
    assert_eq!(
        refused(ready_agent_clone(&case.workspace.worktree)),
        "changes_workspace_invalid"
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// A clone holding more object bytes than the change bounds allow is refused before the
/// engine fetches, so none of its objects reach the project's store (DEC102-F2).
#[test]
fn a_clone_past_the_byte_bound_is_refused_before_its_objects_reach_the_project() {
    let case = Case::own_branch();
    let bounds = crate::worktrees::TreeBounds {
        files: 64,
        bytes: 1024,
        components: 8,
    };
    let mut noise = String::new();
    let mut digest = Sha256::digest(b"noise");
    while noise.len() < 16 * 1024 {
        noise.push_str(&format!("{digest:x}\n"));
        digest = Sha256::digest(digest);
    }
    case.edit("big.txt", &noise);
    case.agent_commit("Add a big file");
    let blob = case.blob_id(&noise);
    assert_eq!(
        refused(flow::record(
            &case.db,
            &case.workspace,
            &own_branch_turn(),
            &bounds,
            NOW_MS
        )),
        "changes_too_large"
    );
    assert!(
        !case.owner_has(&blob),
        "a refused clone's object reached the project"
    );
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
    assert_eq!(case.owner_refs("refs/crazyagent"), "");
}

/// Engine git never opens an agent's own clone, whose config is the agent's: the engine
/// refuses it as an agent worktree, and `Repo::open` stops on any git folder that is neither
/// the owner checkout's nor a linked worktree's in its `.git/worktrees` (CC-N1).
#[test]
#[should_panic(expected = "engine git opens only the owner checkout's git folder")]
fn engine_git_never_opens_an_agent_s_own_clone() {
    let case = Case::own_branch();
    let opened = flow::open_agent(
        &case.db,
        AgentKind::Sub,
        &case.repo,
        &case.workspace.worktree,
        &case.workspace.git_dir,
    );
    assert!(matches!(&opened, Err(refused) if refused.0 == "changes_workspace_invalid"));
    let _ = git::Repo::open(
        &case.repo,
        &case.workspace.git_dir,
        Some(&case.workspace.worktree),
    );
}

/// The paths every diff page of a change set names, in page order. Inputs: the case and
/// the set. Output: the paths.
fn shown_paths(case: &Case, id: &ChangeSetId) -> Vec<String> {
    all_pages(case, id)
        .iter()
        .flat_map(|page| page.files.iter().map(|file| file.path.clone()))
        .collect()
}

/// A change set whose base is older than the owner's tip, part of it already on the
/// owner's branch, shows only the rest, measured from the merge base of the owner's tip
/// and its head; with the owner's checkout detached it falls back to its stored base; and
/// Accept lands only the rest (DEC-107 rule 1, fix/changes-vs-owner-branch).
#[test]
fn a_set_the_owner_holds_part_of_shows_and_lands_only_the_rest() {
    let case = Case::new();
    case.start();
    case.edit("a.txt", "a\n");
    let first = case.agent_commit("Add a");
    case.edit("b.txt", "b\n");
    case.agent_commit("Add b");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(shown_paths(&case, &id), ["a.txt", "b.txt"]);
    case.owner(&["merge", "-q", "--ff-only", &first]);
    assert_eq!(shown_paths(&case, &id), ["b.txt"]);
    case.owner(&["checkout", "-q", "--detach"]);
    assert_eq!(
        shown_paths(&case, &id),
        ["a.txt", "b.txt"],
        "a detached owner checkout falls back to the stored base"
    );
    case.owner(&["checkout", "-q", "main"]);
    assert_eq!(shown_paths(&case, &id), ["b.txt"]);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &commit]).trim(),
        first
    );
    assert_eq!(
        case.owner(&["diff", "--name-only", &first, &commit]),
        "b.txt\n"
    );
}

/// Writes `count` small files under one folder of the agent's worktree, "" for its root.
/// Inputs: the case, the folder and the count.
fn add_files(case: &Case, folder: &str, count: u32) {
    let at = case.workspace.worktree.join(folder);
    std::fs::create_dir_all(&at).unwrap();
    for file in 0..count {
        std::fs::write(at.join(format!("f{file}.txt")), format!("{file}\n")).unwrap();
    }
}

/// Ends one successful turn of the case's agent. Input: the case. Output: what the turn
/// end gave.
fn turn_result(case: &Case) -> Result<Option<ChangeSetId>, EngineError> {
    record_turn_end(&case.db, &case.workspace, &own_branch_turn())
}

/// A turn whose work adds more files than the limit to one new top-level folder is refused
/// with the folder, its count and the limit for the agent; at the limit it is kept, and a
/// folder the owner's branch has or files at the root never count (DEC-107 rule 3,
/// fix/changes-vs-owner-branch).
#[test]
fn a_new_folder_past_the_default_limit_is_refused_with_its_count() {
    let case = Case::new();
    add_files(&case, "scratch", 1001);
    let refused = turn_result(&case).unwrap_err();
    assert_eq!(refusal_code(&refused), NEW_FOLDER_REFUSED);
    let told = refusal_for_agent(&refused).unwrap();
    assert!(
        told.contains(
            "folder scratch/ adds 1001 files, more than this project's limit of 1000 files"
        ),
        "{told}"
    );

    let case = Case::new();
    add_files(&case, "scratch", 1000);
    assert!(turn_result(&case).unwrap().is_some());

    let case = Case::new();
    case.owner_commit("src/keep.txt", "kept\n", "Add src");
    add_files(&case, "src", 1001);
    assert!(turn_result(&case).unwrap().is_some());

    let case = Case::new();
    add_files(&case, "", 1001);
    assert!(turn_result(&case).unwrap().is_some());
}

/// The owner's New folder limit setting, stored for the project, replaces the default.
#[test]
fn a_new_folder_limit_the_owner_set_is_the_one_applied() {
    let case = Case::new();
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute(
            "INSERT INTO team_new_folder_limit(project,files) VALUES (?1,100)",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    add_files(&case, "scratch", 101);
    let refused = turn_result(&case).unwrap_err();
    assert!(
        refusal_for_agent(&refused)
            .unwrap()
            .contains("adds 101 files, more than this project's limit of 100 files"),
        "{refused:?}"
    );
}
