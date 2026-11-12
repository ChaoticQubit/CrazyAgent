use super::*;
use crate::landing::{
    ChangeAgent, FileKind, FileMark, FileStatus, RESTORE_WINDOW_MS, TurnEnd, TurnOutcome, Workspace,
};
use crate::protocol::FRAME_BYTES_MAX;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(in crate::ide::bridge) const NOW: u64 = 1_800_000_000_000;

/// One owner repository on `main` with its own git identity, an agent worktree added
/// from it holding a modified, an added and a deleted file, an owner home with no global
/// git config, and an engine store with the change set that turn made.
pub(in crate::ide::bridge) struct Case {
    _tmp: tempfile::TempDir,
    pub(in crate::ide::bridge) repo: PathBuf,
    pub(in crate::ide::bridge) home: PathBuf,
    pub(in crate::ide::bridge) worktree: PathBuf,
    pub(in crate::ide::bridge) db: Connection,
    pub(in crate::ide::bridge) project: ProjectId,
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

/// The master's worktree as the engine leases it: the project created and the repository
/// admitted for it through the engine's own paths, a master bootstrap lease reserved and
/// checked out on its own `crazyagent/` branch, the only HEAD a master change set may be
/// recorded from. Inputs: the store, the engine root, the project and the repository.
/// Output: the worktree and its git folder.
fn master_lease(
    db: &Connection,
    engine: &Path,
    project: &ProjectId,
    repo: &Path,
) -> (PathBuf, PathBuf) {
    use crate::protocol::{Outcome, ProjectSpec, SessionId, TaskId};
    crate::storage::initialize_authority_schemas(db).unwrap();
    let repository = repo.to_str().unwrap();
    let spec = ProjectSpec {
        name: "Project one".into(),
        repositories: vec![repository.into()],
    };
    let task = TaskId::new("master-bootstrap-1").unwrap();
    let session = SessionId::new("session-1").unwrap();
    let tx = db.unchecked_transaction().unwrap();
    assert_eq!(
        crate::storage::create_project(&tx, project, &spec).unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        crate::worktrees::admit(&tx, project, repository).unwrap(),
        Outcome::Applied
    );
    assert_eq!(
        crate::worktrees::reserve_master_bootstrap(
            &tx, engine, project, &task, &session, repository
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    crate::worktrees::provision(db, &task, repository).unwrap();
    let Outcome::Worktrees { leases } = crate::worktrees::leases(db, &task).unwrap() else {
        panic!("the bootstrap task holds its lease");
    };
    assert_eq!(leases.len(), 1);
    let git_dir = crate::worktrees::lease_git_dir(db, &leases[0]).unwrap();
    (PathBuf::from(&leases[0].path), git_dir)
}

impl Case {
    pub(in crate::ide::bridge) fn new() -> Self {
        Self::in_store(None)
    }

    /// The case with its change set in the store of an engine root, such as a running
    /// owner server's, or of a new root in the case's own folder. Input: the engine root,
    /// or None. Output: the case.
    pub(in crate::ide::bridge) fn in_store(engine: Option<&Path>) -> Self {
        Self::made(
            engine,
            ChangeAgent {
                kind: AgentKind::Sub,
                id: "7".into(),
                label: "Ada (@builder) \u{b7} Codex".into(),
            },
            &[],
            &[],
        )
    }

    /// The case with its change set made by `agent`.
    pub(in crate::ide::bridge) fn with(agent: ChangeAgent) -> Self {
        Self::with_files(agent, &[], &[])
    }

    /// The case with extra files in the owner's first commit and extra agent edits in its
    /// turn, `None` deleting a file. Inputs: the agent, the base files and the edits.
    pub(in crate::ide::bridge) fn with_files(
        agent: ChangeAgent,
        base: &[(&str, &str)],
        edits: &[(&str, Option<&str>)],
    ) -> Self {
        let base: Vec<(&str, &[u8])> = base
            .iter()
            .map(|(path, text)| (*path, text.as_bytes()))
            .collect();
        let edits: Vec<(&str, Option<&[u8]>)> = edits
            .iter()
            .map(|(path, text)| (*path, text.map(str::as_bytes)))
            .collect();
        Self::made(None, agent, &base, &edits)
    }

    /// The case with more files the agent changed: each path with its bytes at the base
    /// and the bytes the agent wrote. Input: the files. Output: the case.
    fn with_bytes(files: &[(&str, &[u8], &[u8])]) -> Self {
        let base: Vec<(&str, &[u8])> = files.iter().map(|(path, base, _)| (*path, *base)).collect();
        let edits: Vec<(&str, Option<&[u8]>)> = files
            .iter()
            .map(|(path, _, agent)| (*path, Some(*agent)))
            .collect();
        Self::made(
            None,
            ChangeAgent {
                kind: AgentKind::Sub,
                id: "7".into(),
                label: "Ada (@builder) \u{b7} Codex".into(),
            },
            &base,
            &edits,
        )
    }

    /// The case with its change set made by `agent` in the store of an engine root, or
    /// of a new root in the case's own folder, with extra base files and agent edits.
    /// Inputs: the engine root or None, the agent, the base files and the edits. Output:
    /// the case.
    fn made(
        engine: Option<&Path>,
        agent: ChangeAgent,
        base: &[(&str, &[u8])],
        edits: &[(&str, Option<&[u8]>)],
    ) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let (repo, home) = (root.join("repo"), root.join("home"));
        for folder in [&repo, &home] {
            std::fs::create_dir(folder).unwrap();
        }
        let engine = match engine {
            Some(engine) => engine.to_path_buf(),
            None => {
                let engine = root.join("engine");
                std::fs::create_dir(&engine).unwrap();
                engine
            }
        };
        let owner = |args: &[&str]| git_in(&home, &repo, args);
        owner(&["init", "-q", "--template=", "-b", "main"]);
        owner(&["config", "user.name", "Repo Owner"]);
        owner(&["config", "user.email", "owner@example.invalid"]);
        std::fs::write(repo.join("README"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join("OLD"), "gone soon\n").unwrap();
        for (path, text) in base {
            let file = repo.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        owner(&["add", "-A"]);
        owner(&["commit", "-q", "-m", "Start"]);
        let project = ProjectId::new("project-1").unwrap();
        let db = landing::open(&engine).unwrap();
        let (worktree, git_dir) = match agent.kind {
            AgentKind::Sub => {
                let worktree = root.join("agent-7");
                owner(&[
                    "worktree",
                    "add",
                    "-q",
                    "--detach",
                    worktree.to_str().unwrap(),
                    "HEAD",
                ]);
                (worktree, repo.join(".git/worktrees/agent-7"))
            }
            AgentKind::Master => master_lease(&db, &engine, &project, &repo),
        };
        let workspace = Workspace {
            project: project.clone(),
            agent,
            repository: repo.clone(),
            worktree: worktree.clone(),
            git_dir,
        };
        std::fs::write(worktree.join("README"), "uno\ntwo\nthree\n").unwrap();
        std::fs::write(worktree.join("NOTES"), "added by the agent\n").unwrap();
        std::fs::remove_file(worktree.join("OLD")).unwrap();
        for (path, text) in edits {
            let file = worktree.join(path);
            match text {
                Some(text) => {
                    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                    std::fs::write(file, text).unwrap();
                }
                None => std::fs::remove_file(file).unwrap(),
            }
        }
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

    pub(in crate::ide::bridge) fn only(&self, now: u64) -> IdeChangeSet {
        let mut sets = change_sets(&self.db, &self.project, now).unwrap();
        assert_eq!(sets.len(), 1, "{sets:?}");
        sets.remove(0)
    }

    pub(in crate::ide::bridge) fn owner_head(&self) -> String {
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

/// AL2-PLACE: an Accept pressed in the IDE's Source Control is listed in the project's
/// activity as made in the IDE.
#[test]
fn an_ide_accept_is_listed_as_made_in_the_ide() {
    let case = Case::new();
    case.db
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)",
        )
        .unwrap();
    crate::coord::ensure(&case.db).unwrap();
    crate::activity::initialize(&case.db).unwrap();
    let set = case.only(NOW);
    let landed = accept(
        &case.db,
        &case.home,
        &case.project,
        &set.id,
        &set.head,
        "Land the agent's edits",
    )
    .unwrap();
    assert!(matches!(landed, AcceptOutcome::Landed { .. }), "{landed:?}");
    let places: Vec<_> = crate::activity::list(&case.db, &case.project, 50)
        .unwrap()
        .into_iter()
        .filter(|item| item.kind == crate::activity::ActivityKind::Accepted)
        .map(|item| item.place)
        .collect();
    assert_eq!(places, vec![Some(crate::activity::AcceptPlace::Ide)]);
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
        "Land the agent's edits",
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
    for head in [
        "0".repeat(40),
        "HEAD".into(),
        set.head.to_uppercase(),
        String::new(),
    ] {
        let refused = accept(
            &case.db,
            &case.home,
            &case.project,
            &set.id,
            &head,
            "Land it",
        )
        .unwrap_err();
        assert_eq!(refused.0, "changes_stale", "{head}");
        assert_eq!(
            discard(&case.db, &case.project, &set.id, &head, NOW)
                .unwrap_err()
                .0,
            "changes_stale",
            "{head}"
        );
    }
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
    let whole = |text: &str| BasePage {
        text: text.into(),
        next: None,
    };
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, &set.head, "README", 0).unwrap(),
        whole("one\ntwo\nthree\n")
    );
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, &set.head, "OLD", 0).unwrap(),
        whole("gone soon\n")
    );
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, &set.head, "NOTES", 0).unwrap(),
        whole("")
    );
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, &set.head, "README", 1)
            .unwrap_err()
            .0,
        "changes_not_found"
    );
    for outside in ["Cargo.toml", "../repo/README", ".git/config", ""] {
        assert_eq!(
            base_blob(&case.db, &case.project, &set.id, &set.head, outside, 0)
                .unwrap_err()
                .0,
            "changes_not_found",
            "{outside}"
        );
    }
}

#[test]
fn agent_ide_settings_are_read_from_the_change_set_head_by_the_engine_only() {
    let old_tasks = "{\"tasks\": []}\n";
    let tasks = "{\"tasks\": [{\"label\": \"x\", \"runOptions\": {\"runOn\": \"folderOpen\"}}]}\n";
    let settings = "{\"chat.disableAIFeatures\": false, \"files.readonlyInclude\": {}}\n";
    let workspace = "{\"folders\": [{\"path\": \"/\"}]}\n";
    let case = Case::with_files(
        ChangeAgent {
            kind: AgentKind::Sub,
            id: "7".into(),
            label: "Ada (@builder) \u{b7} Codex".into(),
        },
        &[
            (".vscode/tasks.json", old_tasks),
            (".vscode/extensions.json", "{}\n"),
        ],
        &[
            (".vscode/settings.json", Some(settings)),
            (".vscode/tasks.json", Some(tasks)),
            (".vscode/extensions.json", None),
            ("shop.code-workspace", Some(workspace)),
        ],
    );
    let set = case.only(NOW);
    let whole = |text: &str| BasePage {
        text: text.into(),
        next: None,
    };
    let marked: Vec<&str> = set
        .files
        .iter()
        .filter(|file| file.marks.contains(&FileMark::IdeConfig))
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        marked,
        [
            ".vscode/extensions.json",
            ".vscode/settings.json",
            ".vscode/tasks.json",
            "shop.code-workspace"
        ]
    );
    let config = |path: &str, page: u16| {
        config_blob(&case.db, &case.project, &set.id, &set.head, path, page)
    };
    assert_eq!(config(".vscode/settings.json", 0).unwrap(), whole(settings));
    assert_eq!(config(".vscode/tasks.json", 0).unwrap(), whole(tasks));
    assert_eq!(config("shop.code-workspace", 0).unwrap(), whole(workspace));
    assert_eq!(
        base_blob(
            &case.db,
            &case.project,
            &set.id,
            &set.head,
            ".vscode/tasks.json",
            0
        )
        .unwrap(),
        whole(old_tasks)
    );
    std::fs::write(
        case.worktree.join(".vscode/settings.json"),
        "{\"later\": true}\n",
    )
    .unwrap();
    assert_eq!(config(".vscode/settings.json", 0).unwrap(), whole(settings));
    assert_eq!(
        config(".vscode/settings.json", 1).unwrap_err().0,
        "changes_not_found"
    );
    for refused in [
        ".vscode/extensions.json",
        "README",
        "NOTES",
        "OLD",
        "../agent-7/.vscode/settings.json",
        ".git/config",
        "",
    ] {
        assert_eq!(
            config(refused, 0).unwrap_err().0,
            "changes_not_found",
            "{refused}"
        );
    }
    let other = ProjectId::new("project-2").unwrap();
    assert_eq!(
        config_blob(
            &case.db,
            &other,
            &set.id,
            &set.head,
            ".vscode/settings.json",
            0
        )
        .unwrap_err()
        .0,
        "changes_not_found"
    );
}

#[test]
fn the_head_side_of_a_diff_is_the_recorded_head_while_the_agent_works_on() {
    let case = Case::new();
    let set = case.only(NOW);
    std::fs::write(case.worktree.join("README"), "dos\n").unwrap();
    std::fs::remove_file(case.worktree.join("NOTES")).unwrap();
    std::fs::write(case.worktree.join("OLD"), "back again\n").unwrap();
    let whole = |text: &str| BasePage {
        text: text.into(),
        next: None,
    };
    assert_eq!(
        head_blob(&case.db, &case.project, &set.id, &set.head, "README", 0).unwrap(),
        whole("uno\ntwo\nthree\n")
    );
    assert_eq!(
        head_blob(&case.db, &case.project, &set.id, &set.head, "NOTES", 0).unwrap(),
        whole("added by the agent\n")
    );
    assert_eq!(
        head_blob(&case.db, &case.project, &set.id, &set.head, "README", 1)
            .unwrap_err()
            .0,
        "changes_not_found"
    );
    for outside in ["OLD", "Cargo.toml", "../repo/README", ".git/config", ""] {
        assert_eq!(
            head_blob(&case.db, &case.project, &set.id, &set.head, outside, 0)
                .unwrap_err()
                .0,
            "changes_not_found",
            "{outside}"
        );
    }
    let other = ProjectId::new("project-2").unwrap();
    assert_eq!(
        head_blob(&case.db, &other, &set.id, &set.head, "README", 0)
            .unwrap_err()
            .0,
        "changes_not_found"
    );
}

/// IDE Accept lands the head the set recorded, which its diff shows, while the agent's
/// worktree already holds its next turn, as the desktop's Accept does: the newer edits stay
/// out of the landed commit and stay in the worktree.
#[test]
fn ide_accept_lands_the_reviewed_head_while_the_agent_worktree_holds_more() {
    let case = Case::new();
    let set = case.only(NOW);
    let start = case.owner_head();
    std::fs::write(case.worktree.join("README"), "dos\n").unwrap();
    std::fs::write(case.worktree.join("SCRATCH"), "the next turn\n").unwrap();
    let landed = accept(
        &case.db,
        &case.home,
        &case.project,
        &set.id,
        &set.head,
        "Land it",
    )
    .unwrap();
    assert!(matches!(landed, AcceptOutcome::Landed { .. }), "{landed:?}");
    let head = case.owner_head();
    assert_ne!(head, start);
    let landed_text =
        |path: &str| git_in(&case.home, &case.repo, &["show", &format!("{head}:{path}")]);
    assert_eq!(landed_text("README"), "uno\ntwo\nthree\n");
    assert_eq!(landed_text("NOTES"), "added by the agent\n");
    let names = git_in(&case.home, &case.repo, &["ls-tree", "--name-only", &head]);
    assert!(!names.lines().any(|name| name == "SCRATCH"), "{names}");
    assert_eq!(
        std::fs::read_to_string(case.worktree.join("README")).unwrap(),
        "dos\n"
    );
    assert!(case.worktree.join("SCRATCH").exists());
}

#[test]
fn a_legacy_encoded_text_file_opens_on_both_sides_of_its_diff() {
    let case = Case::with_bytes(&[("LATIN", &b"caf\xe9\n"[..], &b"caf\xe9s cr\xe8me\n"[..])]);
    let set = case.only(NOW);
    let latin = set.files.iter().find(|file| file.path == "LATIN").unwrap();
    assert_eq!(
        (latin.status, latin.kind),
        (FileStatus::Modified, FileKind::Text)
    );
    assert_eq!(
        base_blob(&case.db, &case.project, &set.id, &set.head, "LATIN", 0)
            .unwrap()
            .text,
        "caf\u{fffd}\n"
    );
    assert_eq!(
        head_blob(&case.db, &case.project, &set.id, &set.head, "LATIN", 0)
            .unwrap()
            .text,
        "caf\u{fffd}s cr\u{fffd}me\n"
    );
}

#[test]
fn a_file_text_and_the_file_list_are_read_from_git_once_for_every_page() {
    let case = Case::new();
    let set = case.only(NOW);
    let base = base_blob(&case.db, &case.project, &set.id, &set.head, "README", 0).unwrap();
    let head = head_blob(&case.db, &case.project, &set.id, &set.head, "README", 0).unwrap();
    let (git, moved) = (case.repo.join(".git"), case.repo.join("git-moved"));
    std::fs::rename(&git, &moved).unwrap();
    let base_again = base_blob(&case.db, &case.project, &set.id, &set.head, "README", 0);
    let head_again = head_blob(&case.db, &case.project, &set.id, &set.head, "README", 0);
    let past = head_blob(&case.db, &case.project, &set.id, &set.head, "README", 1);
    std::fs::rename(&moved, &git).unwrap();
    assert_eq!(base_again.unwrap(), base);
    assert_eq!(head_again.unwrap(), head);
    assert_eq!(past.unwrap_err().0, "changes_not_found");
}

#[test]
fn another_projects_ide_never_reads_or_changes_this_projects_change_sets() {
    let case = Case::new();
    let set = case.only(NOW);
    let start = case.owner_head();
    let other = ProjectId::new("project-2").unwrap();
    assert!(change_sets(&case.db, &other, NOW).unwrap().is_empty());
    let refusals = [
        change_diff(&case.db, &other, &set.id, &set.head, 0).map(|_| ()),
        base_blob(&case.db, &other, &set.id, &set.head, "README", 0).map(|_| ()),
        head_blob(&case.db, &other, &set.id, &set.head, "README", 0).map(|_| ()),
        accept(&case.db, &case.home, &other, &set.id, &set.head, "Land it").map(|_| ()),
        discard(&case.db, &other, &set.id, &set.head, NOW).map(|_| ()),
        restore(&case.db, &other, &set.id, NOW),
    ];
    for refused in refusals {
        assert_eq!(refused.unwrap_err().0, "changes_not_found");
    }
    assert_eq!(case.owner_head(), start);
    assert_eq!(case.only(NOW), set);
    let page = change_diff(&case.db, &case.project, &set.id, &set.head, 0).unwrap();
    assert!(page.text.contains("+uno"), "{}", page.text);
}

#[test]
fn a_base_text_larger_than_a_frame_reads_back_whole_across_pages() {
    let text = "line \"quoted\" \\ tab\t bell\u{7} snow \u{2603}\n".repeat(4000);
    assert!(text.len() > FRAME_BYTES_MAX);
    let (mut read, mut page) = (String::new(), 0);
    loop {
        let shown = page_of(&text, page).unwrap();
        let escaped = serde_json::to_vec(&shown.text).unwrap().len() - 2;
        assert!(escaped <= BASE_PAGE_BYTES, "page {page} is {escaped} bytes");
        let reply = crate::ide::bridge::IdeReply::Ok {
            data: serde_json::to_value(&shown).unwrap(),
        };
        assert!(crate::protocol::encode_frame(&reply).is_ok(), "page {page}");
        read.push_str(&shown.text);
        match shown.next {
            Some(next) => {
                assert_eq!(next, page + 1);
                page = next;
            }
            None => break,
        }
    }
    assert_eq!(read, text);
    assert!(page > 1, "{page}");
    assert_eq!(page_of(&text, page + 1), None);
    assert_eq!(
        page_of("", 0),
        Some(BasePage {
            text: String::new(),
            next: None
        })
    );
    assert_eq!(page_of("", 1), None);
}

#[test]
fn a_list_past_the_frame_budget_shows_the_sets_that_did_not_fit_as_too_large() {
    let case = Case::new();
    let listed = case.only(NOW);
    let files = (0..100)
        .map(|index| ChangedFile {
            path: format!("src/{index:05}/{}", "\u{1}".repeat(40)),
            old_path: None,
            status: FileStatus::Added,
            kind: FileKind::Text,
            marks: Vec::new(),
        })
        .collect();
    let big = IdeChangeSet {
        files,
        message: "Grow the shop\n\nEvery page names its garden.".into(),
        title: "Grow the shop".into(),
        ..listed.clone()
    };
    let huge = IdeChangeSet {
        worktree: PathBuf::from(format!("/{}", "w".repeat(LIST_BYTES_MAX))),
        ..listed.clone()
    };
    let kept = fit(vec![
        big.clone(),
        big.clone(),
        huge,
        big.clone(),
        listed.clone(),
    ])
    .unwrap();
    assert_eq!(kept.len(), 4, "{kept:?}");
    assert_eq!(kept[0], big);
    for summary in &kept[1..3] {
        assert!(summary.files.is_empty());
        assert_eq!(summary.message, "Grow the shop");
        assert_eq!(summary.problem.as_deref(), Some("changes_too_large"));
        assert_eq!(summary.head, big.head);
    }
    assert_eq!(kept[3], listed);
    let encoded = serde_json::to_vec(&kept).unwrap();
    assert!(encoded.len() <= LIST_BYTES_MAX, "{}", encoded.len());
    let reply = crate::ide::bridge::IdeReply::Ok {
        data: serde_json::to_value(&kept).unwrap(),
    };
    assert!(crate::protocol::encode_frame(&reply).is_ok());
}
