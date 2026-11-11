use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
        accept(
            &self.db,
            &self.home,
            id,
            &self.view(id).head,
            message,
            crate::activity::AcceptPlace::Changes,
        )
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

#[track_caller]
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
    numbered_agent(case, 2)
}

/// Adds sub-agent `number` with its own linked detached worktree of the owner repository.
/// Inputs: the case and the agent number. Output: the agent's workspace.
fn numbered_agent(case: &Case, number: usize) -> Workspace {
    let name = format!("agent-{number}");
    let worktree = case.repo.parent().unwrap().join(&name);
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
            id: name.clone(),
            label: format!("Agent {number}"),
        },
        worktree,
        git_dir: case.repo.join(".git/worktrees").join(&name),
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

/// Starts team agent 3 above an owner submodule, then makes one owner change on main the
/// change checks refuse in an agent's own work, and has the agent take main in by a
/// fast-forward or, above an edit of its own, a merge. Inputs: the case, the owner change
/// ("gitlink bump" or "link out of the tree") and how main is taken in ("fast-forward" or
/// "merge"). Output: the agent's workspace.
fn agent_above_owner_change(case: &Case, owner_change: &str, taken_in: &str) -> Workspace {
    let first = case.tip();
    case.owner(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{first},vendor"),
    ]);
    case.owner(&["commit", "-q", "-m", "Owner adds a submodule"]);
    case.start();
    let agent = numbered_agent(case, 3);
    record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir).unwrap();
    if taken_in == "merge" {
        write(&agent.worktree.join("early.txt"), "early\n");
        git_in(&case.home, &agent.worktree, &["add", "early.txt"]);
        git_in(
            &case.home,
            &agent.worktree,
            &["commit", "-q", "-m", "Agent starts"],
        );
    }
    if owner_change == "gitlink bump" {
        let bumped = case.owner_commit("BUMP", "bump\n", "Owner moves on");
        case.owner(&[
            "update-index",
            "--cacheinfo",
            &format!("160000,{bumped},vendor"),
        ]);
    } else {
        std::os::unix::fs::symlink("/etc", case.repo.join("outside")).unwrap();
        case.owner(&["add", "outside"]);
    }
    case.owner(&["commit", "-q", "-m", "Owner changes one"]);
    let take = if taken_in == "merge" {
        ["merge", "-q", "--no-edit", "main"]
    } else {
        ["merge", "-q", "--ff-only", "main"]
    };
    git_in(&case.home, &agent.worktree, &take);
    agent
}

/// LAND-17-L1: an owner gitlink bump or an owner link out of the tree that an agent took in
/// from main, by a fast-forward or a merge, is the landing tip's and is not checked as the
/// agent's own, so the agent's later edit opens a set, committed or left in the worktree.
#[test]
fn owner_work_taken_in_from_main_is_not_checked_as_the_agents_own() {
    for owner_change in ["gitlink bump", "link out of the tree"] {
        for taken_in in ["fast-forward", "merge"] {
            for committed in [true, false] {
                let case = Case::new();
                let agent = agent_above_owner_change(&case, owner_change, taken_in);
                write(&agent.worktree.join("own.txt"), "own\n");
                if committed {
                    git_in(&case.home, &agent.worktree, &["add", "own.txt"]);
                    git_in(
                        &case.home,
                        &agent.worktree,
                        &["commit", "-q", "-m", "Own work"],
                    );
                }
                let opened =
                    record_turn_end(&case.db, &agent, &merge_turn()).map_err(|problem| problem.0);
                assert!(
                    matches!(opened, Ok(Some(_))),
                    "{owner_change}, {taken_in}, committed {committed}: {opened:?}"
                );
            }
        }
    }
}

/// LAND-17-L1: after an agent took owner work in from main, a link out of the tree or a
/// gitlink of its own is still refused, committed or left in the worktree.
#[test]
fn the_agents_own_link_or_gitlink_after_taking_main_in_is_still_refused() {
    for taken_in in ["fast-forward", "merge"] {
        for (own_change, refusal) in [
            ("committed link", "changes_link_outside"),
            ("uncommitted link", "changes_link_outside"),
            ("gitlink", "changes_embedded_repository"),
        ] {
            let case = Case::new();
            let agent = agent_above_owner_change(&case, "gitlink bump", taken_in);
            if own_change == "gitlink" {
                let tip = case.tip();
                git_in(
                    &case.home,
                    &agent.worktree,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{tip},mine"),
                    ],
                );
                git_in(
                    &case.home,
                    &agent.worktree,
                    &["commit", "-q", "-m", "Own submodule"],
                );
            } else {
                std::os::unix::fs::symlink("/etc", agent.worktree.join("mine")).unwrap();
                if own_change == "committed link" {
                    git_in(&case.home, &agent.worktree, &["add", "mine"]);
                    git_in(
                        &case.home,
                        &agent.worktree,
                        &["commit", "-q", "-m", "Own link"],
                    );
                }
            }
            assert_eq!(
                refused(record_turn_end(&case.db, &agent, &merge_turn())),
                refusal,
                "{taken_in}, {own_change}"
            );
        }
    }
}

/// LAND-17-L1: the bounds still count the whole range above the set's start, owner files
/// taken in from main included.
#[test]
fn the_bounds_still_count_owner_files_taken_in_from_main() {
    let case = Case::new();
    case.start();
    let agent = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir).unwrap();
    for name in ["one", "two", "three"] {
        case.owner_commit(name, "x\n", "Owner adds a file");
    }
    git_in(
        &case.home,
        &agent.worktree,
        &["merge", "-q", "--ff-only", "main"],
    );
    write(&agent.worktree.join("own.txt"), "own\n");
    git_in(&case.home, &agent.worktree, &["add", "own.txt"]);
    git_in(
        &case.home,
        &agent.worktree,
        &["commit", "-q", "-m", "Own work"],
    );
    let bounds = crate::worktrees::TreeBounds {
        files: 3,
        bytes: 1024,
        components: 3,
    };
    assert_eq!(
        refused(flow::record(
            &case.db,
            &agent,
            &merge_turn(),
            &bounds,
            NOW_MS
        )),
        "changes_too_large"
    );
}

/// Writes the repository's commit-graph with generation numbers version 1 for every commit
/// `head` and the branches reach, then rewrites it so `child` names `parent` as its only
/// parent, as a process able to write that file forges ancestry (land-19 r1 M1). Inputs:
/// the case, the commit the graph must reach, the child and the claimed parent.
fn forge_commit_graph(case: &Case, head: &str, child: &str, parent: &str) {
    case.owner(&["update-ref", "refs/heads/forged", head]);
    case.owner(&[
        "-c",
        "commitGraph.generationVersion=1",
        "commit-graph",
        "write",
        "--reachable",
    ]);
    case.owner(&["update-ref", "-d", "refs/heads/forged"]);
    let path = case.repo.join(".git/objects/info/commit-graph");
    let mut data = std::fs::read(&path).unwrap();
    assert!(data.starts_with(b"CGPH") && data[5] == 1);
    let mut chunks = BTreeMap::new();
    for at in (0..=usize::from(data[6])).map(|index| 8 + 12 * index) {
        let offset = u64::from_be_bytes(data[at + 4..at + 12].try_into().unwrap());
        chunks.insert(data[at..at + 4].to_vec(), usize::try_from(offset).unwrap());
    }
    assert!(!chunks.contains_key(b"GDA2".as_slice()));
    let fanout = chunks[b"OIDF".as_slice()];
    let count = u32::from_be_bytes(data[fanout + 1020..fanout + 1024].try_into().unwrap());
    let ids = chunks[b"OIDL".as_slice()];
    let position = |hex: &str| {
        (0..usize::try_from(count).unwrap())
            .find(|at| {
                data[ids + 20 * at..ids + 20 * at + 20]
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    == hex
            })
            .unwrap()
    };
    let (child, parent) = (position(child), position(parent));
    let rows = chunks[b"CDAT".as_slice()];
    let (row, parent_row) = (rows + 36 * child, rows + 36 * parent);
    let word =
        |data: &[u8], row: usize| u64::from_be_bytes(data[row + 28..row + 36].try_into().unwrap());
    let generation = (word(&data, parent_row) >> 34) + 1;
    let date = word(&data, row) & ((1 << 34) - 1);
    data[row + 20..row + 24].copy_from_slice(&u32::try_from(parent).unwrap().to_be_bytes());
    data[row + 24..row + 28].copy_from_slice(&0x7000_0000u32.to_be_bytes());
    data[row + 28..row + 36].copy_from_slice(&((generation << 34) | date).to_be_bytes());
    let forged = path.with_extension("forged");
    std::fs::write(&forged, data).unwrap();
    std::fs::rename(&forged, &path).unwrap();
}

/// Commits a gitlink named `mine` to `commit` in an agent's worktree as the agent, with the
/// empty folder an unfilled submodule leaves, so `add -A` keeps it. Inputs: the case, the
/// agent's worktree and the commit. Output: the new commit.
fn commit_own_gitlink(case: &Case, worktree: &Path, commit: &str) -> String {
    std::fs::create_dir(worktree.join("mine")).unwrap();
    git_in(
        &case.home,
        worktree,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},mine"),
        ],
    );
    git_in(
        &case.home,
        worktree,
        &[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "commit",
            "-q",
            "-m",
            "Own submodule",
        ],
    );
    git_in(&case.home, worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

/// Commits a gitlink named `vendor` to `commit` on the owner's branch, with the empty folder
/// an unfilled submodule leaves, so the owner's checkout stays clean. Inputs: the case and
/// the commit. Output: the new tip.
fn owner_commits_gitlink(case: &Case, commit: &str) -> String {
    std::fs::create_dir_all(case.repo.join("vendor")).unwrap();
    case.owner(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{commit},vendor"),
    ]);
    case.owner(&["commit", "-q", "-m", "Owner adds a submodule"]);
    case.tip()
}

/// land-19 r1 M1 (a): a commit-graph claiming the agent's gitlink commit is on the landing
/// tip changes nothing for the engine, whose git reads no commit-graph, so the gitlink is
/// still the agent's own and refused at its turn end. The owner's own git does take the
/// forged answer, which shows the graph was in place.
#[test]
fn a_forged_commit_graph_does_not_hide_an_agent_gitlink_at_the_turn_end() {
    let case = Case::new();
    case.start();
    let agent = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir).unwrap();
    let gitlinked = commit_own_gitlink(&case, &agent.worktree, &case.tip());
    write(&agent.worktree.join("own.txt"), "own\n");
    git_in(&case.home, &agent.worktree, &["add", "own.txt"]);
    git_in(
        &case.home,
        &agent.worktree,
        &["commit", "-q", "-m", "Own work"],
    );
    let head = git_in(&case.home, &agent.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let tip = case.owner_commit("LATER", "later\n", "Owner adds a file");
    forge_commit_graph(&case, &head, &tip, &gitlinked);
    case.owner(&["merge-base", "--is-ancestor", &gitlinked, &tip]);
    assert_eq!(
        refused(record_turn_end(&case.db, &agent, &merge_turn())),
        "changes_embedded_repository"
    );
}

/// land-19 r1 M1 (a) and (b): Accept reads no commit-graph either, and refuses a gitlink in
/// what it would land, here an agent gitlink a forged graph claims is on the tip, in a set
/// whose head moved past its turn-end checks.
#[test]
fn a_forged_commit_graph_does_not_hide_an_agent_gitlink_at_accept() {
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let start = case.tip();
    let gitlinked = commit_own_gitlink(&case, &case.workspace.worktree, &start);
    case.edit("OWN", "own\n");
    case.agent_commit("Own work");
    let head = case.agent(&["rev-parse", "HEAD"]).trim().to_owned();
    case.db
        .execute(
            "UPDATE landing_change_sets SET head=?1 WHERE id=?2",
            [head.as_str(), id.as_str()],
        )
        .unwrap();
    let tip = case.owner_commit("LATER", "later\n", "Owner adds a file");
    forge_commit_graph(&case, &head, &tip, &gitlinked);
    case.owner(&["merge-base", "--is-ancestor", &gitlinked, &tip]);
    assert_eq!(refused(case.accept(&id)), "changes_embedded_repository");
    assert_eq!(case.tip(), tip);
}

/// land-19 r1 L1, closed by M1 (b): an owner gitlink an agent took in is the tip's at the
/// turn end, but once the owner resets main below it, Accept would land it back, so Accept
/// refuses it.
#[test]
fn accept_refuses_an_owner_gitlink_the_owner_dropped_from_main_after_the_turn_end() {
    let case = Case::new();
    case.start();
    let agent = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir).unwrap();
    let start = case.tip();
    owner_commits_gitlink(&case, &start);
    git_in(
        &case.home,
        &agent.worktree,
        &["merge", "-q", "--ff-only", "main"],
    );
    write(&agent.worktree.join("own.txt"), "own\n");
    let id = record_turn_end(&case.db, &agent, &merge_turn())
        .unwrap()
        .unwrap();
    case.owner(&["reset", "-q", "--hard", &start]);
    assert_eq!(refused(case.accept(&id)), "changes_embedded_repository");
    assert_eq!(case.tip(), start);
}

/// land-19 r1 M1 (b): after an earlier set landed as a squash while the agent kept working,
/// an owner gitlink the agent then took in from main is the tip's, at the turn end and at
/// Accept, so the later set opens and lands.
#[test]
fn a_set_above_an_owner_gitlink_taken_in_after_a_squash_landing_opens_and_lands() {
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    case.edit("LATER", "later\n");
    case.agent_commit("Agent keeps working");
    let AcceptOutcome::Landed { .. } = case.accept(&first).unwrap() else {
        panic!("the first accept conflicted");
    };
    owner_commits_gitlink(&case, &case.tip());
    case.agent(&[
        "-c",
        "user.name=Agent",
        "-c",
        "user.email=agent@example.invalid",
        "merge",
        "-q",
        "--no-edit",
        "main",
    ]);
    case.edit("OWN", "own\n");
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    assert!(matches!(
        case.accept(&second).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
}

/// land-19 r1 L2: with no landing tip, as for a worker whose lead's worktree is gone, the
/// own checks read from the set's start, so the agent's own gitlink is refused.
#[test]
fn with_no_landing_tip_an_agent_gitlink_is_refused() {
    let mut case = Case::new();
    let lead_tip = worker_of_lead(&mut case, 1);
    let lead = case.repo.parent().unwrap().join("agent-35");
    case.owner(&["worktree", "remove", "--force", lead.to_str().unwrap()]);
    commit_own_gitlink(&case, &case.workspace.worktree, &lead_tip);
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &merge_turn())),
        "changes_embedded_repository"
    );
}

/// land-19 r1 L2: the own checks fall back to the set's start for a tip with no history in
/// common, and read from where the tip meets the head once the owner reset main below the
/// start, so in both the agent's own gitlink is refused.
#[test]
fn an_agent_gitlink_is_refused_with_an_unrelated_tip_or_a_tip_reset_below_the_start() {
    for owner_move in ["unrelated history", "reset below the start"] {
        let case = Case::new();
        case.start();
        let below = case.tip();
        case.owner_commit("LATER", "later\n", "Owner adds a file");
        let agent = numbered_agent(&case, 3);
        record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir)
            .unwrap();
        if owner_move == "unrelated history" {
            case.owner(&["checkout", "-q", "--orphan", "fresh"]);
            case.owner(&["commit", "-q", "-m", "Owner starts over"]);
            case.owner(&["branch", "-M", "fresh", "main"]);
        } else {
            case.owner(&["reset", "-q", "--hard", &below]);
        }
        commit_own_gitlink(&case, &agent.worktree, &below);
        assert_eq!(
            refused(record_turn_end(&case.db, &agent, &merge_turn())),
            "changes_embedded_repository",
            "{owner_move}"
        );
    }
}

/// land-19 r1 L2: a worker under Lands Lead takes in its lead's gitlink with its lead's
/// branch, which is its landing tip, so its own edit opens a set measured as the edit
/// alone, and its own gitlink is still refused.
#[test]
fn a_lead_worker_takes_in_its_lead_s_gitlink_and_its_own_is_still_refused() {
    let mut case = Case::new();
    let lead_tip = worker_of_lead(&mut case, 1);
    let lead = case.repo.parent().unwrap().join("agent-35");
    std::fs::create_dir(lead.join("vendor")).unwrap();
    git_in(
        &case.home,
        &lead,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{lead_tip},vendor"),
        ],
    );
    git_in(
        &case.home,
        &lead,
        &[
            "-c",
            "user.name=Lead",
            "-c",
            "user.email=lead@example.invalid",
            "commit",
            "-q",
            "-m",
            "Lead adds a submodule",
        ],
    );
    case.agent(&["merge", "-q", "--ff-only", "crazyagent/agent-35"]);
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).files, 1);
    commit_own_gitlink(&case, &case.workspace.worktree, &lead_tip);
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &merge_turn())),
        "changes_embedded_repository"
    );
}

/// Leaves the owner repository with no `.git/info`, as a clone or `init --template=` leaves
/// a project and as the case's repository starts, and writes a grafts file into a case
/// variant of it that gives `child` the only parent `parent`, as a project-root agent could
/// (land-19 r2 M1). A file system that keeps case, as on Linux, never reads the variant as
/// `info`, so there the grafts file also goes into `info` itself. The owner's own git then
/// answers ancestry from it. Inputs: the case, the grafted commit and its claimed parent.
fn forge_grafts(case: &Case, child: &str, parent: &str) {
    let git = case.repo.join(".git");
    match std::fs::remove_dir_all(git.join("info")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        removed => removed.unwrap(),
    }
    let grafts = format!("{child} {parent}\n");
    write(&git.join("INFO").join("grafts"), &grafts);
    if !git.join("info").join("grafts").is_file() {
        write(&git.join("info").join("grafts"), &grafts);
    }
    case.owner(&["merge-base", "--is-ancestor", parent, child]);
}

/// land-19 r2 M1: a grafts file claiming the agent's gitlink commit is on the landing tip
/// changes nothing for the engine, whose git reads no grafts, so the gitlink is refused at
/// the turn end and, in a set whose head moved past its turn-end checks, at Accept.
#[test]
fn a_grafts_file_does_not_hide_an_agent_gitlink_at_the_turn_end_or_at_accept() {
    {
        let case = Case::new();
        case.start();
        let agent = numbered_agent(&case, 3);
        record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir)
            .unwrap();
        let gitlinked = commit_own_gitlink(&case, &agent.worktree, &case.tip());
        write(&agent.worktree.join("own.txt"), "own\n");
        git_in(&case.home, &agent.worktree, &["add", "own.txt"]);
        git_in(
            &case.home,
            &agent.worktree,
            &["commit", "-q", "-m", "Own work"],
        );
        let tip = case.owner_commit("LATER", "later\n", "Owner adds a file");
        forge_grafts(&case, &tip, &gitlinked);
        assert_eq!(
            refused(record_turn_end(&case.db, &agent, &merge_turn())),
            "changes_embedded_repository"
        );
    }
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let gitlinked = commit_own_gitlink(&case, &case.workspace.worktree, &case.tip());
    case.edit("OWN", "own\n");
    case.agent_commit("Own work");
    let head = case.agent(&["rev-parse", "HEAD"]).trim().to_owned();
    case.db
        .execute(
            "UPDATE landing_change_sets SET head=?1 WHERE id=?2",
            [head.as_str(), id.as_str()],
        )
        .unwrap();
    let tip = case.owner_commit("LATER", "later\n", "Owner adds a file");
    forge_grafts(&case, &tip, &gitlinked);
    assert_eq!(refused(case.accept(&id)), "changes_embedded_repository");
    assert_eq!(case.tip(), tip);
}

/// T8 R1: a uv cache bucket an agent moved into its worktree, with its engine-made `.git`
/// link to `/dev/null`, opens a set of plain files with no `.git` path and no gitlink.
#[test]
fn a_moved_uv_bucket_opens_a_set_of_plain_files() {
    let case = Case::new();
    case.start();
    let bucket = case.workspace.worktree.join("moved");
    write(&bucket.join("a.txt"), "a\n");
    std::os::unix::fs::symlink("/dev/null", bucket.join(".git")).unwrap();
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let listed = case.owner(&["ls-tree", "-r", &case.view(&id).head]);
    assert!(listed.contains("moved/a.txt"), "{listed}");
    assert!(
        !listed.contains(".git") && !listed.contains("160000"),
        "{listed}"
    );
}

/// Makes links in a folder, each a path and its target. Inputs: the folder and the links.
fn plant_links(folder: &Path, links: &[(&str, &str)]) {
    for (link, target) in links {
        let path = folder.join(link);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, &path).unwrap();
    }
}

#[test]
fn a_link_reaching_the_agents_folder_through_another_link_dots_or_a_folded_name_is_refused() {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Link the notes".into(),
    };
    let rows: [(&str, &[(&str, &str)]); 4] = [
        (
            "a link through an ancestor link",
            &[
                ("p", ".crazyagent"),
                ("q", "p/worktrees/agent-3/target/bare"),
            ],
        ),
        (
            "dots through a folder link",
            &[("x/d", ".."), ("y", "x/d/../..")],
        ),
        (
            "transit through a link the agent keeps",
            &[(
                "t",
                ".crazyagent/worktrees/agent-3/lnk/../../../../target/bare",
            )],
        ),
        (
            "a name the volume folds to worktrees",
            &[("k", ".crazyagent/wor\u{212A}trees/agent-3/target/bare")],
        ),
    ];
    let outcomes: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, links)| {
            let case = Case::new();
            std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
            plant_links(&case.workspace.worktree, links);
            let outcome = match record_turn_end(&case.db, &case.workspace, &turn) {
                Err(error) => error.0,
                Ok(landed) => format!("landed {landed:?}"),
            };
            (*name, outcome)
        })
        .collect();

    let case = Case::new();
    std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
    plant_links(
        &case.workspace.worktree,
        &[("docs/readme", "../README"), ("a/b/docs", "../../docs")],
    );
    let id = record_turn_end(&case.db, &case.workspace, &turn)
        .unwrap()
        .expect("plain in-tree links land as a change set");
    assert!(
        matches!(case.accept(&id).unwrap(), AcceptOutcome::Landed { .. }),
        "plain in-tree links pass Accept"
    );

    let refusals: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, _)| (*name, "changes_link_outside".to_owned()))
        .collect();
    assert_eq!(outcomes, refusals);
}

#[test]
fn accepting_two_change_sets_that_each_hold_half_a_link_chain_refuses_the_second() {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Link the notes".into(),
    };
    let ancestor: &[(&str, &str)] = &[("p", ".crazyagent")];
    let through: &[(&str, &str)] = &[("q", "p/worktrees/agent-3/target/bare")];
    let outcomes: Vec<String> = [(through, ancestor), (ancestor, through)]
        .iter()
        .map(|(first, second)| {
            let case = Case::new();
            std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
            let sibling = second_agent(&case);
            plant_links(&case.workspace.worktree, first);
            plant_links(&sibling.worktree, second);
            let one = record_turn_end(&case.db, &case.workspace, &turn)
                .unwrap()
                .expect("one link lands as a change set");
            let two = record_turn_end(&case.db, &sibling, &turn)
                .unwrap()
                .expect("the other link lands as a change set");
            assert!(matches!(
                case.accept(&one).unwrap(),
                AcceptOutcome::Landed { .. }
            ));
            let tip = case.tip();
            let outcome = match case.accept(&two) {
                Err(error) => error.0,
                Ok(landed) => format!("landed {landed:?}"),
            };
            let landed = case.repo.join(second[0].0);
            if landed.symlink_metadata().is_err() && case.tip() == tip {
                outcome
            } else {
                format!("{outcome}, branch moved or {} written", landed.display())
            }
        })
        .collect();
    assert_eq!(
        outcomes,
        ["changes_link_outside", "changes_link_outside"],
        "the merge would chain q through p, in both orders"
    );
}

/// Records one turn of an agent's links and says what the engine did with it.
/// Inputs: the case and the workspace. Output: the refusal code, else what landed.
fn turn_outcome(case: &Case, workspace: &Workspace) -> String {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Link the notes".into(),
    };
    match record_turn_end(&case.db, workspace, &turn) {
        Err(error) => error.0,
        Ok(landed) => format!("landed {landed:?}"),
    }
}

#[test]
fn a_link_into_the_owners_git_folder_where_a_linked_agent_writes_is_refused() {
    let rows: [(&str, &[(&str, &str)]); 3] = [
        (
            "a worktree admin folder from a subfolder",
            &[("docs/n", "../.git/worktrees/agent-3/x")],
        ),
        (
            "the git folder in another case",
            &[("n", ".GIT/worktrees/agent-3/x")],
        ),
        ("the object store", &[("n", ".git/objects/evil")]),
    ];
    let outcomes: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, links)| {
            let case = Case::new();
            plant_links(&case.workspace.worktree, links);
            (*name, turn_outcome(&case, &case.workspace))
        })
        .collect();

    let case = Case::new();
    plant_links(
        &case.workspace.worktree,
        &[
            ("ci", ".github"),
            ("ignore", ".gitignore"),
            ("docs/hooks", "../.githooks"),
        ],
    );
    let id = record_turn_end(
        &case.db,
        &case.workspace,
        &TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Link the notes".into(),
        },
    )
    .unwrap()
    .expect("links to names that only start with .git land as a change set");
    assert!(
        matches!(case.accept(&id).unwrap(), AcceptOutcome::Landed { .. }),
        "links to names that only start with .git pass Accept"
    );

    let refusals: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, _)| (*name, "changes_link_outside".to_owned()))
        .collect();
    assert_eq!(outcomes, refusals);
}

#[test]
fn a_link_name_the_volume_folds_onto_a_step_of_another_link_is_refused() {
    let rows: [(&str, &[(&str, &str)]); 2] = [
        (
            "U+212A KELVIN SIGN folds onto k",
            &[
                ("\u{212A}", ".crazyagent"),
                ("o", "k/worktrees/agent-3/target/bare"),
            ],
        ),
        (
            "U+017F LONG S folds onto s",
            &[
                ("\u{17F}", ".crazyagent"),
                ("o2", "s/worktrees/agent-3/target/bare"),
            ],
        ),
    ];
    let outcomes: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, links)| {
            let case = Case::new();
            std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
            plant_links(&case.workspace.worktree, links);
            (*name, turn_outcome(&case, &case.workspace))
        })
        .collect();
    let refusals: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, _)| (*name, "changes_link_outside".to_owned()))
        .collect();
    assert_eq!(outcomes, refusals);
}

/// Commits the owner's links on the owner's branch, records the agent's links and, from a
/// second agent, the sibling's links, accepts the agent's change set and reports what
/// happens to the sibling's: its refusal at turn end or at Accept, or what landed. The
/// sibling's worktree starts before the owner's commit. Inputs: the owner's, the agent's
/// and the sibling's links; empty skips that step. Output: the refusal code, else what
/// landed and whether the branch moved.
fn sibling_link_outcome(
    owner: &[(&str, &str)],
    agent: &[(&str, &str)],
    sibling: &[(&str, &str)],
) -> String {
    let case = Case::new();
    std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
    let second = second_agent(&case);
    if !owner.is_empty() {
        plant_links(&case.repo, owner);
        for (link, _) in owner {
            case.owner(&["add", link]);
        }
        case.owner(&["commit", "-q", "-m", "Owner links"]);
    }
    if !agent.is_empty() {
        plant_links(&case.workspace.worktree, agent);
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: "turn-1".into(),
            prompt_line: "Link the notes".into(),
        };
        let one = record_turn_end(&case.db, &case.workspace, &turn)
            .unwrap()
            .expect("the agent's link lands as a change set");
        assert!(matches!(
            case.accept(&one).unwrap(),
            AcceptOutcome::Landed { .. }
        ));
    }
    plant_links(&second.worktree, sibling);
    let tip = case.tip();
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Link the notes".into(),
    };
    let two = match record_turn_end(&case.db, &second, &turn) {
        Err(error) => return error.0,
        Ok(None) => return "no change set".to_owned(),
        Ok(Some(two)) => two,
    };
    let outcome = match case.accept(&two) {
        Err(error) => error.0,
        Ok(landed) => format!("landed {landed:?}"),
    };
    let landed = case.repo.join(sibling[0].0);
    if landed.symlink_metadata().is_err() && case.tip() == tip {
        outcome
    } else {
        format!("{outcome}, branch moved or {} written", landed.display())
    }
}

#[test]
fn accepting_a_link_whose_step_the_volume_folds_onto_another_link_is_refused() {
    let kelvin: &[(&str, &str)] = &[("\u{212A}", ".crazyagent")];
    let through: &[(&str, &str)] = &[("o", "k/worktrees/agent-3/target/bare")];
    let rows: [(&str, String); 3] = [
        (
            "the Kelvin link lands second",
            sibling_link_outcome(&[], through, kelvin),
        ),
        (
            "the owner's branch holds the Kelvin link",
            sibling_link_outcome(kelvin, &[], through),
        ),
        (
            "the owner's branch holds a link through a Kelvin step",
            sibling_link_outcome(
                &[("o", "\u{212A}/worktrees/agent-3/target/bare")],
                &[],
                &[("k", ".crazyagent")],
            ),
        ),
    ];
    let refusals: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, _)| (*name, "changes_link_outside".to_owned()))
        .collect();
    assert_eq!(rows.to_vec(), refusals);
}

/// Commits the owner's links on the owner's branch, starts an agent from it, records the
/// agent's links and accepts their change set. Inputs: the owner's and the agent's links.
/// Output: the refusal code at turn end or Accept, else "landed" once the agent's first
/// link is on the owner's branch.
fn owner_then_agent_outcome(owner: &[(&str, &str)], agent: &[(&str, &str)]) -> String {
    let case = Case::new();
    std::fs::create_dir(case.repo.join(".crazyagent")).unwrap();
    plant_links(&case.repo, owner);
    for (link, _) in owner {
        case.owner(&["add", link]);
    }
    case.owner(&["commit", "-q", "-m", "Owner links"]);
    let workspace = second_agent(&case);
    plant_links(&workspace.worktree, agent);
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Link the notes".into(),
    };
    let id = match record_turn_end(&case.db, &workspace, &turn) {
        Err(error) => return error.0,
        Ok(None) => return "no change set".to_owned(),
        Ok(Some(id)) => id,
    };
    match case.accept(&id) {
        Err(error) => error.0,
        Ok(AcceptOutcome::Landed { .. }) if case.repo.join(agent[0].0).is_symlink() => {
            "landed".to_owned()
        }
        Ok(other) => format!("accepted {other:?} without the link"),
    }
}

#[test]
fn a_non_ascii_owner_link_leaves_an_agent_link_it_cannot_fold_onto_to_land() {
    let cafe: &[(&str, &str)] = &[("caf\u{E9}", "README")];
    let rows: [(&str, String); 3] = [
        (
            "an unrelated link beside the owner's caf\u{E9}",
            owner_then_agent_outcome(cafe, &[("docs/n", "../README")]),
        ),
        (
            "a link to cafe, which the volume keeps apart from caf\u{E9}",
            owner_then_agent_outcome(cafe, &[("docs/n", "../cafe")]),
        ),
        (
            "an agent link beside an owner link through a caf\u{E9} step",
            owner_then_agent_outcome(&[("o", "caf\u{E9}/x")], &[("n", "README")]),
        ),
    ];
    let landed: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, _)| (*name, "landed".to_owned()))
        .collect();
    assert_eq!(rows.to_vec(), landed);
}

#[test]
fn accepting_a_link_through_a_name_the_volume_folds_onto_several_ascii_letters_is_refused() {
    let rows: [(&str, String); 4] = [
        (
            "the owner's branch holds U+00DF SHARP S, which folds onto ss",
            sibling_link_outcome(
                &[("\u{DF}", ".crazyagent")],
                &[],
                &[("o", "ss/worktrees/agent-3/target/bare")],
            ),
        ),
        (
            "the owner's branch holds U+FB03 LIGATURE FFI, which folds onto ffi",
            sibling_link_outcome(
                &[("\u{FB03}", ".crazyagent")],
                &[],
                &[("o", "FFI/worktrees/agent-3/target/bare")],
            ),
        ),
        (
            "the owner's branch holds a link through a U+1E9E CAPITAL SHARP S step",
            sibling_link_outcome(
                &[("o", "\u{1E9E}/worktrees/agent-3/target/bare")],
                &[],
                &[("ss", ".crazyagent")],
            ),
        ),
        (
            "the owner's branch holds a link through a U+037E GREEK QUESTION MARK step",
            sibling_link_outcome(
                &[("o", "a\u{37E}/worktrees/agent-3/target/bare")],
                &[],
                &[("a;", ".crazyagent")],
            ),
        ),
    ];
    let refusals: Vec<(&str, String)> = rows
        .iter()
        .map(|(name, _)| (*name, "changes_link_outside".to_owned()))
        .collect();
    assert_eq!(rows.to_vec(), refusals);
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
    let diff = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
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
        refused(accept(
            &case.db,
            &case.home,
            &id,
            &case.tip(),
            None,
            crate::activity::AcceptPlace::Changes
        )),
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
fn recording_a_turn_end_alone_never_merges_main_into_the_agent_s_work() {
    let case = Case::new();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    case.edit("README", "one\ntwo\nthree, the agent's\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(
        case.view(&id).state,
        ChangeState::Pending,
        "the turn-end caller decides whether a set goes back to its agent"
    );
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        "one\ntwo\nthree, the agent's\n"
    );

    let case = Case::own_branch();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    case.edit("README", "one\ntwo\nthree, the agent's\n");
    let head = case.agent_commit("Change the third line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(
        (case.view(&id).state, case.view(&id).head),
        (ChangeState::Pending, head.clone())
    );
    assert_eq!(
        (case.agent_head(), case.agent(&["status", "--porcelain"])),
        (head, String::new()),
        "the engine never runs git in the agent's own clone (CC-N1)"
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
                assert!(flow::reset_agent(&agent, &head, &head).is_err());
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
    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
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
    diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    let before = case.checkout();
    assert_eq!(refused(case.accept(&id)), "changes_git_unsupported");
    assert_eq!(case.checkout(), before);
    let ran: Vec<_> = std::fs::read_dir(&markers)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(ran.is_empty(), "configured commands ran: {ran:?}");
}

/// Main renamed the file the agent edited and changed it too, so git merges the agent's
/// edit at main's new path; a merge driver the owner set for that path in the repository's
/// own attributes never runs, and Accept refuses as for any configured command.
#[test]
fn accept_refuses_a_merge_driver_on_the_path_main_renamed_the_agent_s_file_to() {
    let case = Case::new();
    case.edit("README", "uno\ntwo\nthree\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner(&["mv", "README", "LIESMICH"]);
    write(&case.repo.join("LIESMICH"), "one\ntwo\nTHREE\n");
    case.owner(&["commit", "-q", "-am", "Rename README and shout three"]);
    let marker = case.home.join("driver-ran");
    case.owner(&[
        "config",
        "merge.mark.driver",
        &format!("/usr/bin/touch {}", marker.display()),
    ]);
    write(
        &case.repo.join(".git/info/attributes"),
        "LIESMICH merge=mark\n",
    );
    let before = case.checkout();
    assert_eq!(refused(case.accept(&id)), "changes_git_unsupported");
    assert!(
        !marker.exists(),
        "the owner's merge driver ran at the path main renamed the agent's file to"
    );
    assert_eq!(case.checkout(), before);
    assert_eq!(case.view(&id).state, ChangeState::Pending);
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

/// OP-9b: a linked lead's native teammates work in linked worktrees of the owner
/// repository inside the lead's worktree, each in `.worktrees/<name>`. The lead's turn
/// commits its own edits and leaves that folder out, so no teammate worktree is taken in
/// as an embedded repository and the lead's turn is still kept.
#[test]
fn a_linked_lead_s_turn_leaves_its_teammates_worktrees_out() {
    let case = Case::new();
    case.start();
    let teammate = case
        .workspace
        .worktree
        .join(crate::agent_git::TEAMMATES_FOLDER)
        .join("greet");
    case.owner(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "agent-1--greet",
        teammate.to_str().unwrap(),
        "HEAD",
    ]);
    write(&teammate.join("greet.txt"), "hello\n");
    case.edit("README", "one\ntwo\nthree\nfour\n");

    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Add the fourth line".into(),
    };
    let recorded = record_turn_end(&case.db, &case.workspace, &turn);
    let Ok(Some(id)) = recorded else {
        panic!("the lead's turn is kept with its teammate's worktree inside: {recorded:?}");
    };
    let head = case.view(&id).head;
    assert_eq!(
        case.owner(&["show", "--name-only", "--format=", &head])
            .trim(),
        "README",
        "only the lead's own edit is in its change set"
    );
    assert!(teammate.join("greet.txt").is_file());
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
        let stopped = owner::accept(
            &case.db,
            &case.home,
            &id,
            &head,
            None,
            crate::activity::AcceptPlace::Changes,
            &|at| at == step,
        );
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
        let stopped = owner::accept(
            &case.db,
            &case.home,
            &id,
            &head,
            None,
            crate::activity::AcceptPlace::Changes,
            &|at| at == step,
        );
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
    let stopped = owner::accept(
        &case.db,
        &case.home,
        &id,
        &head,
        None,
        crate::activity::AcceptPlace::Changes,
        &|at| at == owner::Step::Recorded,
    );
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
        let diff = diff(&case.db, &id, &case.view(&id).head, page).unwrap();
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
    let past = diff(&case.db, &id, &case.view(&id).head, page + 5).unwrap();
    assert!(past.files.is_empty() && past.text.is_empty() && past.next.is_none());
}

#[test]
fn agent_change_to_vscode_folder_or_workspace_file_is_flagged() {
    let case = Case::new();
    let recommended = case.owner_commit(
        ".vscode/extensions.json",
        "{\"recommendations\": []}\n",
        "Recommend nothing",
    );
    case.agent(&["checkout", "-q", "--detach", &recommended]);
    case.start();
    case.agent(&["mv", ".vscode/extensions.json", "extensions.json"]);
    case.edit(
        ".vscode/settings.json",
        "{\"chat.disableAIFeatures\": false}\n",
    );
    case.edit("web/.VSCODE/launch.json", "{}\n");
    case.edit("shop.code-workspace", "{\"folders\": []}\n");
    case.edit("docs/Team.CODE-WORKSPACE", "{\"folders\": []}\n");
    std::fs::create_dir(case.workspace.worktree.join("app")).unwrap();
    std::os::unix::fs::symlink("../README", case.workspace.worktree.join("app/.vscode")).unwrap();
    case.edit("vscode/notes.md", "plain\n");
    case.edit("src/.vscode-old/a.txt", "plain\n");
    case.edit("notes.code-workspace.md", "plain\n");
    case.edit(".code-workspaces/a.txt", "plain\n");
    case.edit("api/.v\u{17f}code/settings.json", "{}\n");
    case.edit("ui/.VSCode/tasks.json", "{}\n");
    case.edit("ops/team.code-wor\u{212a}space", "{\"folders\": []}\n");
    case.edit("lib/.gitattribute\u{17f}", "* text\n");
    case.edit("a/.vscod\u{e9}/x.json", "{}\n");
    case.edit("b/.vscode\u{301}/y.json", "{}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    assert!(page.next.is_none());
    let mut flagged: Vec<(&str, Option<&str>)> = page
        .files
        .iter()
        .filter(|file| file.marks.contains(&FileMark::IdeConfig))
        .map(|file| (file.path.as_str(), file.old_path.as_deref()))
        .collect();
    flagged.sort_unstable();
    assert_eq!(
        flagged,
        [
            (".vscode/settings.json", None),
            ("api/.v\u{17f}code/settings.json", None),
            ("app/.vscode", None),
            ("docs/Team.CODE-WORKSPACE", None),
            ("extensions.json", Some(".vscode/extensions.json")),
            ("ops/team.code-wor\u{212a}space", None),
            ("shop.code-workspace", None),
            ("ui/.VSCode/tasks.json", None),
            ("web/.VSCODE/launch.json", None),
        ]
    );
    let attributes = page
        .files
        .iter()
        .find(|file| file.path == "lib/.gitattribute\u{17f}")
        .unwrap();
    assert_eq!(attributes.marks, [FileMark::Attributes]);
    let accented: Vec<&str> = page
        .files
        .iter()
        .filter(|file| file.path.starts_with("a/") || file.path.starts_with("b/"))
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(accented.len(), 2, "{accented:?}");
    let link = page
        .files
        .iter()
        .find(|file| file.path == "app/.vscode")
        .unwrap();
    assert_eq!(link.marks, [FileMark::Symlink, FileMark::IdeConfig]);
    assert_eq!(page.files.len(), 16, "{:?}", page.files);
}

#[test]
fn an_instruction_file_change_is_marked_and_its_hidden_characters_show_as_escapes() {
    let case = Case::new();
    case.edit("AGENTS.md", "Follow the plan.\u{200b}\u{e0041}\u{e0042}\n");
    case.edit(".claude/CLAUDE.md", "Keep tests real.\n");
    case.edit("web/CLAUDE.local.md", "Local notes.\n");
    case.edit("notes.md", "Plain\u{2028}notes.\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();

    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    assert_eq!(page.next, None);
    let marks: Vec<(String, String)> = page
        .files
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                serde_json::to_string(&file.marks).unwrap(),
            )
        })
        .collect();
    let instructions = r#"["instructions"]"#.to_owned();
    assert_eq!(
        marks,
        [
            (".claude/CLAUDE.md".to_owned(), instructions.clone()),
            ("AGENTS.md".to_owned(), instructions.clone()),
            ("notes.md".to_owned(), "[]".to_owned()),
            ("web/CLAUDE.local.md".to_owned(), instructions),
        ]
    );
    assert!(
        page.text
            .contains("+Follow the plan.\\u200b\\u{e0041}\\u{e0042}\n"),
        "{:?}",
        page.text
    );
    assert!(
        page.text.contains("+Plain\\u2028notes.\n"),
        "{:?}",
        page.text
    );
    assert!(!crate::memory::hidden_text(&page.text), "{:?}", page.text);
}

#[test]
fn instruction_marks_ignore_case_and_cover_codex_overrides_and_harness_folders() {
    let case = Case::new();
    for path in [
        "claude.md",
        "web/Agents.md",
        "AGENTS.override.md",
        ".claude/commands/x.md",
        ".codex/config.toml",
        "docs/.Claude/notes.md",
        "notes.md",
    ] {
        case.edit(path, "Text.\n");
    }
    let id = case.turn(TurnOutcome::Succeeded).unwrap();

    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    assert_eq!(page.next, None);
    let marked: Vec<(&str, bool)> = page
        .files
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.marks.contains(&FileMark::Instructions),
            )
        })
        .collect();
    assert_eq!(
        marked,
        [
            (".claude/commands/x.md", true),
            (".codex/config.toml", true),
            ("AGENTS.override.md", true),
            ("claude.md", true),
            ("docs/.Claude/notes.md", true),
            ("notes.md", false),
            ("web/Agents.md", true),
        ]
    );
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

/// A set that starts where an accepted set's head was is measured from that head only
/// while the owner's branch still holds the commit the engine landed. Once the owner
/// resets the branch below it, the earlier work counts again, and once gc prunes the
/// landed commit, a turn end still records the set and counts it from the joined range,
/// while the set keeps its stored start.
#[test]
fn a_landed_commit_the_owner_reset_away_counts_the_work_again_and_its_prune_breaks_no_turn() {
    let case = Case::new();
    case.start();
    let start = case.tip();
    case.edit("B", "b\n");
    let reviewed = case.agent_commit("Add B");
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    case.edit("C", "c\n");
    let newer = case.agent_commit("Add C");
    let AcceptOutcome::Landed { commit: squash, .. } = case.accept(&second).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        case.owner(&["rev-parse", &format!("{squash}^")]).trim(),
        start
    );
    case.owner(&["reset", "-q", "--hard", &start]);
    let third = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&third);
    assert_eq!(
        (view.base.as_str(), view.head.as_str(), view.files),
        (reviewed.as_str(), newer.as_str(), 2)
    );
    case.owner(&["reflog", "expire", "--expire=now", "--all"]);
    case.owner(&["gc", "-q", "--prune=now"]);
    let objects = case.owner(&[
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectname)",
    ]);
    assert!(!objects.lines().any(|id| id == squash));
    assert!(objects.lines().any(|id| id == reviewed));
    case.edit("D", "d\n");
    let latest = case.agent_commit("Add D");
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-2".into(),
        prompt_line: "Add D".into(),
    };
    let recorded = record_turn_end(&case.db, &case.workspace, &turn);
    assert_eq!(
        recorded.as_ref().ok(),
        Some(&Some(third.clone())),
        "{recorded:?}"
    );
    let view = case.view(&third);
    assert_eq!(
        (view.base.as_str(), view.head.as_str(), view.files),
        (reviewed.as_str(), latest.as_str(), 3)
    );
}

/// Puts the owner's checkout on `feat/x` at the first commit while `stage` holds 400 more
/// files and the agent's worktree starts at `stage`, with Land on naming `stage`, as when
/// the owner's checkout was left on an older branch (DEC-111 Q1). Input: the case. Output:
/// the first commit and the tip of `stage`.
fn land_on_stage(case: &Case) -> (String, String) {
    let start = case.tip();
    case.owner(&["checkout", "-q", "-b", "stage"]);
    for at in 0..400 {
        write(&case.repo.join(format!("stage/file-{at}.txt")), "stage\n");
    }
    case.owner(&["add", "stage"]);
    case.owner(&["commit", "-q", "-m", "Stage work"]);
    let stage = case.owner(&["rev-parse", "stage"]).trim().to_owned();
    case.owner(&["checkout", "-q", "-b", "feat/x", &start]);
    case.agent(&["reset", "-q", "--hard", &stage]);
    case.start();
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute(
            "INSERT INTO team_land_on(project,branch) VALUES (?1,'stage')",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    (start, stage)
}

/// A change set is measured from the Land on branch the owner chose, not from the owner's
/// checked-out branch, which lacks the 400 files the agent's work started from: the set
/// lists the agent's 2 files at its turn end and after a poll, the Accept branch list
/// names `stage` as where an Accept with no branch named lands, and that Accept lands there
/// as one commit with the owner's checkout untouched (DEC-111 Q1).
#[test]
fn a_change_set_is_measured_from_the_land_on_branch_not_the_owner_s_checkout() {
    let case = Case::new();
    let (start, stage) = land_on_stage(&case);
    case.edit("A", "a\n");
    case.edit("B", "b\n");
    case.agent_commit("Add A and B");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).files, 2);
    observe(&case.db, Some(&case.workspace.project)).unwrap();
    assert_eq!(case.view(&id).files, 2);
    let listed = target_branches(&case.db, &id).unwrap();
    assert_eq!(
        (listed.current.as_deref(), listed.land_on.as_deref()),
        (Some("feat/x"), Some("stage"))
    );
    let AcceptOutcome::Landed { commit, branch } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        (branch.as_str(), case.owner(&["rev-parse", "stage"]).trim()),
        ("stage", commit.as_str())
    );
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &commit]).trim(),
        stage
    );
    assert_eq!(case.owner(&["rev-parse", "feat/x"]).trim(), start);
    assert_eq!(
        case.owner(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feat/x"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    assert!(!case.repo.join("A").exists());
}

/// Update merges the Land on branch into an agent's work, not the owner's checked-out
/// branch, and the set is measured from that branch after (DEC-111 Q1).
#[test]
fn update_merges_the_land_on_branch_into_the_agent_s_work() {
    let case = Case::new();
    land_on_stage(&case);
    case.edit("A", "a\n");
    let head = case.agent_commit("Add A");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner(&["checkout", "-q", "stage"]);
    write(&case.repo.join("later.txt"), "later\n");
    case.owner(&["add", "later.txt"]);
    case.owner(&["commit", "-q", "-m", "Later stage work"]);
    let later = case.owner(&["rev-parse", "HEAD"]).trim().to_owned();
    case.owner(&["checkout", "-q", "feat/x"]);
    update(&case.db, &id).unwrap();
    let view = case.view(&id);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &view.head]).trim(),
        format!("{head} {later}")
    );
    assert_eq!((view.base.as_str(), view.files), (later.as_str(), 1));
}

/// A worktree with no recorded start opens its set above the head it had, and the set is
/// measured against the owner's branch (DEC-107 rule 1), so the agent's earlier commit the
/// owner's branch lacks is counted with the new file.
#[test]
fn a_worktree_with_no_recorded_start_keeps_opening_above_its_head() {
    let case = Case::new();
    case.edit("A", "a\n");
    let first = case.agent_commit("Add A");
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
    case.edit("B", "b\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&id);
    assert_eq!((view.base.as_str(), view.files), (first.as_str(), 2));
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
    assert_eq!(super::commit_subject(&long, &roots), "x".repeat(72));
    assert_eq!(
        super::commit_subject(&format!("Rename {long}"), &roots),
        "Rename"
    );
    assert_eq!(
        super::commit_subject("Fix\u{202E}\ttabs  and /api/users routes", &roots),
        "Fix tabs and /api/users routes"
    );
}

/// A local path after a key or a flag in the same word is cut to its last component with
/// the key kept, while a URL's own folders and a route stay as written.
#[test]
fn a_local_path_after_a_key_or_flag_in_the_same_word_is_cut_to_its_last_component() {
    assert_eq!(
        super::scrubbed(
            "Wrote key=/Users/ada/secret.txt --out=/home/ada/build/out.json path:~/notes/plan.md (src=/private/var/ada/run.log).",
            &[]
        ),
        "Wrote key=secret.txt --out=out.json path:plan.md (src=run.log)."
    );
    assert_eq!(
        super::scrubbed("See https://example.com/var/lib and /api/users", &[]),
        "See https://example.com/var/lib and /api/users"
    );
}

/// A `file://` link to a local file, in any case and under any folder, is cut to the
/// file's name with the marks around it kept.
#[test]
fn a_file_link_to_a_local_file_is_cut_to_its_last_component() {
    assert_eq!(
        super::scrubbed(
            "Open file:///Users/ada/notes/plan.md and <file://localhost/srv/ada/report.pdf> then see:FILE:///C:/Users/ada/run.sh",
            &[]
        ),
        "Open plan.md and <report.pdf> then see:run.sh"
    );
}

/// A Windows drive path, with backslashes or slashes, alone or after a flag, is cut to its
/// last component, while a URL scheme is not read as a drive.
#[test]
fn a_windows_drive_path_is_cut_to_its_last_component() {
    assert_eq!(
        super::scrubbed(
            r"Read C:\Users\ada\notes\plan.md and d:/work/ada/report.pdf then --log=E:\logs\ada\run.txt",
            &[]
        ),
        "Read plan.md and report.pdf then --log=run.txt"
    );
    assert_eq!(
        super::scrubbed("Fetch https://example.com/a and http://x.org/b", &[]),
        "Fetch https://example.com/a and http://x.org/b"
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
        super::commit_subject("Fix\u{2060}the\u{e0001} parser", &roots),
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
             ALTER TABLE landing_change_sets DROP COLUMN waiting;
             ALTER TABLE landing_change_sets DROP COLUMN accepted_in;",
        )
        .unwrap();
    store::initialize(&case.db).unwrap();
    assert_eq!(store::load(&case.db, &id).unwrap().accepted_in, None);
    assert_eq!(case.view(&id).message, "Add the fourth line");
    assert!(!case.view(&id).waiting);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(case.landed_message(&commit), "Add the fourth line");
}

#[test]
fn a_set_accepted_in_a_place_a_later_build_named_still_lists_and_keeps_that_place() {
    let case = Case::new();
    closing_tables(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    case.db
        .execute(
            "UPDATE landing_change_sets SET accepted_in='terminal' WHERE id=?1",
            [id.as_str()],
        )
        .unwrap();
    let listed = list(&case.db, &case.workspace.project)
        .expect("a set accepted in a later build's place still lists");
    assert!(
        listed
            .iter()
            .any(|view| view.id == id && view.state == ChangeState::Accepted)
    );
    let mut record =
        store::load(&case.db, &id).expect("a set accepted in a later build's place still reads");
    assert_eq!(record.accepted_in, None);
    let rows = crate::activity::list(&case.db, &case.workspace.project, crate::activity::LIST_MAX)
        .unwrap();
    let ours = rows
        .iter()
        .filter(|item| item.change_set.as_deref() == Some(id.as_str()))
        .collect::<Vec<_>>();
    assert!(!ours.is_empty());
    assert!(ours.iter().all(|item| item.place.is_none()), "{ours:?}");
    record.updated_ms += 1;
    store::save(&case.db, &record).unwrap();
    let kept: Option<String> = case
        .db
        .query_row(
            "SELECT accepted_in FROM landing_change_sets WHERE id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        kept.as_deref(),
        Some("terminal"),
        "a save by this build keeps a place it cannot read"
    );
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
    let head = case.view(id).head;
    let mut pages = vec![diff(&case.db, id, &head, 0).unwrap()];
    while let Some(next) = pages.last().and_then(|page| page.next) {
        pages.push(diff(&case.db, id, &head, next).unwrap());
    }
    pages
}

/// How many owner edits the store keeps. Inputs: the store and the change set they settled
/// on, `""` for those still waiting, or None for every one. Output: the count.
fn owner_edit_rows(db: &Connection, change_set: Option<&str>) -> i64 {
    match change_set {
        Some(set) => db.query_row(
            "SELECT COUNT(*) FROM landing_owner_edits WHERE change_set=?1",
            [set],
            |row| row.get(0),
        ),
        None => db.query_row("SELECT COUNT(*) FROM landing_owner_edits", [], |row| {
            row.get(0)
        }),
    }
    .unwrap()
}

#[test]
fn a_turn_end_that_cannot_settle_owner_edits_still_reports_its_change_set() {
    let case = Case::new();
    case.edit("notes.txt", "a note\n");
    owner_edit::record(&case.db, &case.workspace.worktree, "notes.txt", NOW_MS).unwrap();
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER refuse_settle BEFORE UPDATE ON landing_owner_edits
             BEGIN SELECT RAISE(ABORT, 'store busy'); END;",
        )
        .unwrap();
    let id = case
        .turn(TurnOutcome::Succeeded)
        .expect("the recorded change set is reported");
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(owner_edit_rows(&case.db, Some("")), 1);
    case.db
        .execute_batch("DROP TRIGGER refuse_settle;")
        .unwrap();
    case.edit("notes.txt", "a longer note\n");
    let again = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(owner_edit_rows(&case.db, Some("")), 0);
    assert_eq!(owner_edit_rows(&case.db, Some(again.as_str())), 1);
}

#[test]
fn owner_edits_are_forgotten_once_their_set_is_accepted_or_pruned_or_the_worktree_retires() {
    let case = Case::new();
    case.edit("notes.txt", "a note\n");
    owner_edit::record(&case.db, &case.workspace.worktree, "notes.txt", NOW_MS).unwrap();
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(owner_edit_rows(&case.db, Some(first.as_str())), 1);
    let landed = case.accept(&first).unwrap();
    assert!(matches!(landed, AcceptOutcome::Landed { .. }), "{landed:?}");
    assert_eq!(owner_edit_rows(&case.db, None), 0);

    case.edit("more.txt", "more\n");
    owner_edit::record(&case.db, &case.workspace.worktree, "more.txt", NOW_MS).unwrap();
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    let until = discard(&case.db, &second, &case.view(&second).head, NOW_MS).unwrap();
    assert_eq!(
        owner_edit_rows(&case.db, Some(second.as_str())),
        1,
        "a discarded set keeps its marks while it can be restored"
    );
    assert_eq!(prune(&case.db, until + 1).unwrap(), 1);
    assert_eq!(owner_edit_rows(&case.db, None), 0);

    owner_edit::record(&case.db, &case.workspace.worktree, "late.txt", NOW_MS).unwrap();
    assert_eq!(owner_edit_rows(&case.db, Some("")), 1);
    let workspace = &case.workspace;
    let retired = retire_worktree(
        &case.db,
        &workspace.repository,
        &workspace.worktree,
        &workspace.git_dir,
        until + 2,
    )
    .unwrap();
    assert_eq!(retired, Ok(()));
    assert_eq!(owner_edit_rows(&case.db, None), 0);
}

#[test]
fn a_change_sets_file_list_stays_when_other_diffs_push_its_diff_out() {
    let case = Case::new();
    case.edit("notes.txt", "a note\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &id).unwrap();
    let first = diff::listed(&record.repository, &record.base, &record.head).unwrap();
    let paths: Vec<&str> = first.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["notes.txt"]);
    let mut from = case.tip();
    for index in 0..=diff::RECENT_MAX {
        let to = case.owner_commit("flood.txt", &format!("{index}\n"), "Flood the recent diffs");
        diff::page(&record.repository, &from, &to, 0).unwrap();
        from = to;
    }
    let (git, moved) = (case.repo.join(".git"), case.repo.join("git-moved"));
    std::fs::rename(&git, &moved).unwrap();
    let paged = diff::page(&record.repository, &record.base, &record.head, 0);
    let again = diff::listed(&record.repository, &record.base, &record.head);
    std::fs::rename(&moved, &git).unwrap();
    assert!(
        paged.is_err(),
        "the flood left this diff among the recent diffs"
    );
    assert!(std::sync::Arc::ptr_eq(&first, &again.unwrap()));
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
    let past = diff(&case.db, &id, &case.view(&id).head, 900).unwrap();
    std::fs::rename(&moved, &git).unwrap();
    assert_eq!(again, first);
    assert!(past.files.is_empty() && past.text.is_empty() && past.next.is_none());
}

fn ledger_rows(case: &Case) -> Vec<(String, String, Option<String>)> {
    let engine = Path::new(case.db.path().unwrap()).parent().unwrap();
    crate::ledger::open(engine)
        .unwrap()
        .prepare("SELECT kind,action_key,outcome FROM ledger_actions ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// An Accept an engine stop interrupted is listed once in the action ledger after the
/// engine settles it, for the recovery banner: a pending notice saying whether it landed,
/// kept across later runs until the owner dismisses it with the outcome it showed.
#[test]
fn an_interrupted_accept_is_listed_in_the_ledger_once_settled() {
    for (step, landed, outcome) in [
        (owner::Step::Recorded, false, crate::ledger::Outcome::Failed),
        (
            owner::Step::FilesWritten,
            true,
            crate::ledger::Outcome::Done,
        ),
        (owner::Step::BranchMoved, true, crate::ledger::Outcome::Done),
    ] {
        let case = Case::new();
        case.edit("README", "one\ntwo\nthree\nfour\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        let stopped = owner::accept(
            &case.db,
            &case.home,
            &id,
            &head,
            None,
            crate::activity::AcceptPlace::Changes,
            &|at| at == step,
        );
        assert_eq!(refused(stopped), "changes_git_failed", "{step:?}");
        reconcile(&case.db).unwrap();
        reconcile(&case.db).unwrap();
        let listed: (String, String, Option<String>) =
            ("landing_accept".to_owned(), id.as_str().to_owned(), None);
        assert_eq!(ledger_rows(&case), vec![listed], "{step:?}");
        let engine = Path::new(case.db.path().unwrap()).parent().unwrap();
        let ledger = crate::ledger::open(engine).unwrap();
        let shown =
            crate::ledger::pending(&ledger, crate::ledger::ActionKind::LandingAccept).unwrap();
        assert_eq!(shown.len(), 1, "{step:?}");
        let notice: serde_json::Value = serde_json::from_str(&shown[0].payload).unwrap();
        assert_eq!(notice["change_set"], id.as_str(), "{step:?}");
        assert_eq!(notice["landed"], landed, "{step:?}");
        let dismissed = crate::ledger::dismiss(
            &ledger,
            crate::ledger::ActionKind::LandingAccept,
            shown[0].id.get(),
        );
        assert_eq!(dismissed.unwrap(), outcome, "{step:?}");
        reconcile(&case.db).unwrap();
        let name = if landed { "done" } else { "failed" };
        let dismissed = (
            "landing_accept".to_owned(),
            id.as_str().to_owned(),
            Some(name.to_owned()),
        );
        assert_eq!(ledger_rows(&case), vec![dismissed], "{step:?}");
    }
}

/// An Accept that finishes while upkeep's reconcile waits for the repository is a normal
/// Accept: reconcile decides from the record it loads under the hold, so it lists nothing.
/// The test holds the repository, lets reconcile take its list of in-flight change sets
/// and wait, then settles the change set as the live Accept does before letting go.
#[test]
fn an_accept_settled_while_reconcile_waits_is_not_listed_as_interrupted() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let stopped = owner::accept(
        &case.db,
        &case.home,
        &id,
        &head,
        None,
        crate::activity::AcceptPlace::Changes,
        &|at| at == owner::Step::Recorded,
    );
    assert_eq!(refused(stopped), "changes_git_failed");
    let repository = store::load(&case.db, &id).unwrap().repository;
    let held = flow::hold(&repository);
    let path = PathBuf::from(case.db.path().unwrap());
    let upkeep = std::thread::spawn(move || {
        let db = Connection::open(&path).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        reconcile(&db)
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(owner::settled(&case.db, &id).unwrap().intent.is_none());
    drop(held);
    upkeep.join().unwrap().unwrap();
    assert!(ledger_rows(&case).is_empty());
    assert!(store::in_flight(&case.db).unwrap().is_empty());
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
fn an_agent_head_naming_another_branch_is_refused_and_its_own_branch_takes_the_turn() {
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
    case.agent(&["checkout", "-q", "-b", "crazyagent/agent-2"]);
    case.edit("other.txt", "other\n");
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &turn)),
        "changes_workspace_invalid",
        "a sub-agent worktree never commits on another agent's branch"
    );
    let other = case.owner(&["rev-parse", "refs/heads/crazyagent/agent-2"]);
    assert_eq!(other.trim(), head, "another agent's branch moved");
    assert_eq!(case.view(&id).head, head);
    assert_eq!(case.tip(), tip);
    assert_eq!(
        std::fs::read_to_string(&head_file).unwrap(),
        "ref: refs/heads/crazyagent/agent-2\n"
    );
    std::fs::remove_file(case.workspace.worktree.join("other.txt")).unwrap();
    case.agent(&["checkout", "-q", "-b", "crazyagent/agent-1"]);
    case.edit("lease.txt", "lease\n");
    assert_eq!(
        record_turn_end(&case.db, &case.workspace, &turn).unwrap(),
        Some(id.clone()),
        "a sub-agent may commit on its own branch (B1)"
    );
    let named = case.owner(&["rev-parse", "refs/heads/crazyagent/agent-1"]);
    assert_ne!(named.trim(), head, "its own branch carries the turn");
    assert_eq!(named.trim(), case.agent_head());
    assert_eq!(
        case.owner(&["rev-parse", "refs/heads/crazyagent/agent-2"])
            .trim(),
        head
    );
    assert_eq!(
        case.tip(),
        tip,
        "an own-branch turn end moved the owner's branch"
    );
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

    /// The owner's engine refs when the clone's base ref is the only one, as `owner_refs`
    /// lists them (GA-R5). Output: the listing.
    fn base_ref_only(&self) -> String {
        let base = store::base_of(&self.db, self.workspace.worktree.to_str().unwrap())
            .unwrap()
            .unwrap();
        format!("refs/crazyagent/base/agent-own {base}\n")
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
        format!(
            "{}refs/crazyagent/pending/{} {first}\n",
            case.base_ref_only(),
            id.as_str()
        )
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

/// After an Accept, every agent clone the team records holds `refs/crazyagent/main` at the
/// landed commit, read by the agent's own git, so a second agent merges main into its
/// branch with plain `git merge crazyagent/main` and its next set lands with no
/// `changes_outside_merge` (OS-8).
#[test]
fn every_agent_clone_holds_the_landed_main_after_an_accept() {
    let case = Case::own_branch();
    let second = case.repo.parent().unwrap().join("agent-two");
    add_agent_clone(&case.repo, &second).unwrap();
    let other = Workspace {
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-two".into(),
            label: "Agent 2".into(),
        },
        git_dir: crate::agent_git::git_dir(&second),
        worktree: second.clone(),
        ..case.workspace.clone()
    };
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    crate::team::initialize(&case.db).unwrap();
    for (agent, workspace) in [(1, &case.workspace), (2, &other)] {
        case.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    agent,
                    workspace.project.as_str(),
                    workspace.repository.to_str().unwrap(),
                    workspace.worktree.to_str().unwrap(),
                    workspace.git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
    }
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    let main = git_in(
        &case.home,
        &second,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/crazyagent/main",
        ],
    );
    assert_eq!(main.trim(), commit, "the second clone holds no landed main");
    let agent = |args: &[&str]| {
        let named = [
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
        ];
        git_in(&case.home, &second, &[&named[..], args].concat())
    };
    write(&second.join("two.txt"), "second agent\n");
    agent(&["add", "-A"]);
    agent(&["commit", "-q", "-m", "Add two"]);
    agent(&["merge", "-q", "--no-edit", "crazyagent/main"]);
    assert_eq!(agent(&["rev-parse", "HEAD^2"]).trim(), commit);
    let next = record_turn_end(&case.db, &other, &own_branch_turn())
        .unwrap()
        .unwrap();
    let AcceptOutcome::Landed { commit: landed, .. } = case.accept(&next).unwrap() else {
        panic!("the second accept conflicted");
    };
    assert_eq!(
        case.owner(&["show", &format!("{landed}:README")]),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(
        case.owner(&["show", &format!("{landed}:two.txt")]),
        "second agent\n"
    );
}

const CLONE_AGENT: i64 = 7;

/// Records the case's agent as team sub-agent `CLONE_AGENT` with its worktree, own clone or
/// linked, and the coordination tables, as the engine does for a sub-agent, so an Accept
/// gives its worktree main and can send it notes. Input: the case. Output: none.
fn seat_team_agent(case: &mut Case) {
    case.workspace.agent.id = CLONE_AGENT.to_string();
    crate::team::initialize(&case.db).unwrap();
    crate::coord::ensure(&case.db).unwrap();
    let project = case.workspace.project.as_str();
    case.db
        .execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
             VALUES (?1,?2,'sub','claude',0,'idle',1,1,NULL)",
            rusqlite::params![CLONE_AGENT, project],
        )
        .unwrap();
    case.db
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                CLONE_AGENT,
                project,
                case.workspace.repository.to_str().unwrap(),
                case.workspace.worktree.to_str().unwrap(),
                case.workspace.git_dir.to_str().unwrap()
            ],
        )
        .unwrap();
}

/// The bodies of the notes the engine sent the clone agent, oldest first. Input: the case.
/// Output: the bodies.
fn engine_notes(case: &Case) -> Vec<String> {
    crate::coord::unread(&case.db, CLONE_AGENT, 10)
        .unwrap()
        .into_iter()
        .map(|note| {
            assert_eq!(
                (note.from, note.kind),
                (crate::coord::Party::Engine, crate::coord::MessageKind::Note)
            );
            note.body
        })
        .collect()
}

/// How many landed Notes the engine sent anyone in the case's store. Input: the case.
/// Output: the count.
fn landed_notes(case: &Case) -> i64 {
    case.db
        .query_row(
            "SELECT COUNT(*) FROM coord_messages WHERE from_kind='engine' AND idem_key LIKE 'landed-%'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// The Note the engine sends a clone agent once its set lands, as the owner worded it
/// (DEC-117 addendum, T6 r1 F1). Inputs: the landed commit and the set's head. Output: the
/// body.
fn landed_note(landed: &str, head: &str) -> String {
    format!(
        "Your change set landed on main as {}; its last commit was {}. If git log {}..HEAD shows no newer commits, start your next work from main: git reset --keep crazyagent/main.",
        &landed[..12],
        &head[..12],
        &head[..12]
    )
}

/// After the owner accepts a clone's set, the engine tells its agent in one Note that the
/// set landed and what its last commit was, so `git log <head>..HEAD` shows any newer work,
/// and to start from main with `git reset --keep`, naming no merge even with "Catch up with
/// main" on, since the agent's own git keeps the setting it launched with; that reset opens
/// its next set at the landed commit, which lands its one new line (OS-8).
#[test]
fn after_an_accept_a_clone_agent_is_told_to_restart_from_main_and_its_next_set_starts_there() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        engine_notes(&case),
        [landed_note(&commit, &head)],
        "the agent hears once that its set landed and how to start from main"
    );
    assert_eq!(
        case.agent(&["log", "--oneline", &format!("{}..HEAD", &head[..12])]),
        "",
        "the Note's own check shows no newer commits"
    );
    case.agent(&["reset", "-q", "--keep", "crazyagent/main"]);
    assert_eq!(case.agent_head(), commit);
    case.edit("README", "ONE\ntwo\nthree\nfour\n");
    case.agent_commit("Shout the first line");
    let next = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &next).unwrap();
    let measured = owner::measured_from(
        &case.db,
        &flow::shared(&case.repo).unwrap(),
        &record,
        &case.tip(),
    )
    .unwrap();
    assert_eq!(
        (record.base.as_str(), measured.as_str()),
        (commit.as_str(), commit.as_str()),
        "the next set opens and is measured at the landed commit"
    );
    let AcceptOutcome::Landed { commit: second, .. } = case.accept(&next).unwrap() else {
        panic!("the second accept conflicted");
    };
    assert_eq!(
        case.owner(&["diff", "--numstat", &commit, &second]),
        "1\t1\tREADME\n"
    );
    assert_eq!(
        engine_notes(&case).len(),
        2,
        "each accept tells the agent once"
    );
}

/// An owner commit on main after the Accept stays out of the agent's next set once the agent
/// resets to the newer main the engine gave its clone: the set opens at the merge base of its
/// head and the main tip, never at the older landed commit; with "Catch up with main" off the
/// Note reads the same (OS-8).
#[test]
fn an_owner_commit_after_the_accept_stays_out_of_the_set_of_an_agent_that_restarts_from_newer_main()
{
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    crate::team::set_catch_up_main(
        &case.db,
        &case.workspace.project,
        crate::team::CatchUpMain::Off,
    )
    .unwrap();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(engine_notes(&case), [landed_note(&commit, &head)]);
    let owner_work = case.owner_commit("other.txt", "owner work\n", "Owner work");
    refresh_main(&case.db, &case.workspace.project).unwrap();
    case.agent(&["reset", "-q", "--keep", "crazyagent/main"]);
    assert_eq!(case.agent_head(), owner_work);
    case.edit("README", "ONE\ntwo\nthree\nfour\n");
    case.agent_commit("Shout the first line");
    let next = case.turn(TurnOutcome::Succeeded).unwrap();
    let view = case.view(&next);
    assert_eq!(
        (view.base.as_str(), view.files, view.added, view.removed),
        (owner_work.as_str(), 1, 1, 1),
        "the owner's later commit is not the agent's change"
    );
    let AcceptOutcome::Landed { commit: second, .. } = case.accept(&next).unwrap() else {
        panic!("the second accept conflicted");
    };
    assert_eq!(
        case.owner(&["diff", "--numstat", &owner_work, &second]),
        "1\t1\tREADME\n"
    );
}

/// A clone whose branch holds neither its recorded base nor the commit landed for it is still
/// refused at its turn end, as before an agent could restart from main (GA-R3, OS-8).
#[test]
fn a_clone_branch_without_the_landed_work_is_still_refused() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    let start = case.tip();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    case.agent(&["reset", "-q", "--hard", &start]);
    case.edit("README", "ONE\ntwo\nthree\n");
    case.agent_commit("Shout on the old start");
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_branch_not_above_base"
    );
}

/// A landed Note the engine cannot send, here to a clone agent whose worktree the team
/// records and was given main but whose agent row is gone, is logged and leaves the Accept
/// landed (OS-8).
#[test]
fn a_landed_note_that_cannot_be_sent_never_fails_the_accept() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    case.db
        .execute("DELETE FROM team_agents WHERE id=?1", [CLONE_AGENT])
        .unwrap();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(
        (
            case.view(&id).state,
            case.tip(),
            clone_main(&case, &case.workspace.worktree)
        ),
        (ChangeState::Accepted, commit.clone(), commit)
    );
    assert_eq!(landed_notes(&case), 0);
}

/// A clone the Accept could not give main, here because a folder stands where its
/// `refs/crazyagent/main` goes, is sent no landed Note: a reset would take it to an older
/// main or fail (T6 r1 L1, F2).
#[test]
fn a_clone_not_given_main_by_the_accept_hears_no_landed_note() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let main = case.workspace.worktree.join(".git/refs/crazyagent/main");
    if main.is_file() {
        std::fs::remove_file(&main).unwrap();
    }
    std::fs::create_dir_all(&main).unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(clone_main(&case, &case.workspace.worktree), "");
    assert_eq!(engine_notes(&case), Vec::<String>::new());
}

/// A clone with an open set keeps that set's recorded base when it resets to main: the base
/// is never moved for an open set, so its turn end is refused as before (GA-R3, T6 r1 F3).
#[test]
fn a_clone_with_an_open_set_that_resets_to_main_is_still_refused() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    case.edit("two.txt", "second\n");
    case.agent_commit("Add a second file");
    let open = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&open).state, ChangeState::Pending);
    case.agent(&["reset", "-q", "--keep", "crazyagent/main"]);
    case.edit("README", "ONE\ntwo\nthree\nfour\n");
    case.agent_commit("Shout the first line");
    assert_eq!(
        refused(record_turn_end(
            &case.db,
            &case.workspace,
            &own_branch_turn()
        )),
        "changes_branch_not_above_base"
    );
}

/// The landed Note for one set is sent once however often the Accept's end runs for it,
/// since the engine keys it to the set (T6 r1 F3).
#[test]
fn a_landed_note_told_again_for_the_same_set_is_sent_once() {
    let mut case = Case::own_branch();
    seat_team_agent(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    owner::tell_landed(&case.db, &store::load(&case.db, &id).unwrap(), &commit);
    assert_eq!(engine_notes(&case), [landed_note(&commit, &head)]);
}

/// A sub-agent in a linked worktree, which the engine resets itself, is sent no landed Note
/// when its set lands (T6 r1 F3, L2).
#[test]
fn a_linked_worktree_agent_hears_no_landed_note() {
    let mut case = Case::new();
    case.start();
    seat_team_agent(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(landed_notes(&case), 0);
}

/// The master, whose change sets land from its leased linked worktree, is sent no landed Note
/// when its set lands (T6 r1 F3, L2).
#[test]
fn the_master_hears_no_landed_note() {
    let case = Case::new();
    case.start();
    crate::coord::ensure(&case.db).unwrap();
    let master = master_agent(&case);
    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    write(&master.worktree.join("master.txt"), "from the master\n");
    let master_set = record_turn_end(&case.db, &master, &merge_turn())
        .unwrap()
        .expect("the master's edit makes its change set");
    assert!(matches!(
        case.accept(&master_set).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(landed_notes(&case), 0);
}

/// Adds a second agent's own clone beside the case's and records both agent worktrees in
/// the team tables, agent 1 the case's and agent 2 the new one, as the runtime records
/// them. Input: the case. Output: the second agent's workspace.
fn second_clone(case: &Case) -> Workspace {
    let second = case.repo.parent().unwrap().join("agent-two");
    add_agent_clone(&case.repo, &second).unwrap();
    let other = Workspace {
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-two".into(),
            label: "Agent 2".into(),
        },
        git_dir: crate::agent_git::git_dir(&second),
        worktree: second,
        ..case.workspace.clone()
    };
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    crate::team::initialize(&case.db).unwrap();
    for (agent, workspace) in [(1, &case.workspace), (2, &other)] {
        case.db
            .execute(
                "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    agent,
                    workspace.project.as_str(),
                    workspace.repository.to_str().unwrap(),
                    workspace.worktree.to_str().unwrap(),
                    workspace.git_dir.to_str().unwrap()
                ],
            )
            .unwrap();
    }
    other
}

/// The commit `refs/crazyagent/main` names in an agent's own clone, read by its own git,
/// empty when it names none. Inputs: the case and the clone. Output: the commit.
fn clone_main(case: &Case, worktree: &Path) -> String {
    git_in(
        &case.home,
        worktree,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/crazyagent/main",
        ],
    )
    .trim()
    .to_owned()
}

/// An agent that brings the landed main into its branch with `git merge crazyagent/main`
/// has its next set recorded and landed, both when Land on names a branch checked out
/// nowhere and when it is the owner's checked-out branch: the merged parent is the Land on
/// tip the engine gave it, which is landed work (OS-8).
#[test]
fn a_merge_of_crazyagent_main_lands_whether_or_not_land_on_is_checked_out() {
    for land_on in [Some("release"), None] {
        let case = Case::own_branch();
        let other = second_clone(&case);
        let checked_out = case.tip();
        if let Some(branch) = land_on {
            case.owner(&["branch", branch]);
            case.db
                .execute(
                    "INSERT INTO team_land_on(project,branch) VALUES (?1,?2)",
                    rusqlite::params![case.workspace.project.as_str(), branch],
                )
                .unwrap();
        }
        case.edit("README", "one\ntwo\nthree\nfour\n");
        case.agent_commit("Add the fourth line");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let AcceptOutcome::Landed { commit, branch } = case.accept(&id).unwrap() else {
            panic!("the accept conflicted");
        };
        assert_eq!(branch, land_on.unwrap_or("main"));
        assert_eq!(
            case.tip() == checked_out,
            land_on.is_some(),
            "with Land on {land_on:?} the owner's checkout moves only when it is the Land on branch"
        );
        assert_eq!(clone_main(&case, &other.worktree), commit);
        let agent = |args: &[&str]| {
            let named = [
                "-c",
                "user.name=Agent",
                "-c",
                "user.email=agent@example.invalid",
            ];
            git_in(&case.home, &other.worktree, &[&named[..], args].concat())
        };
        write(&other.worktree.join("two.txt"), "second agent\n");
        agent(&["add", "-A"]);
        agent(&["commit", "-q", "-m", "Add two"]);
        agent(&["merge", "-q", "--no-edit", "crazyagent/main"]);
        assert_eq!(agent(&["rev-parse", "HEAD^2"]).trim(), commit);
        let next = record_turn_end(&case.db, &other, &own_branch_turn()).map_err(|error| error.0);
        let Ok(Some(next)) = next else {
            panic!("with Land on {land_on:?} the merge of crazyagent/main is refused: {next:?}");
        };
        let AcceptOutcome::Landed { commit: landed, .. } = case.accept(&next).unwrap() else {
            panic!("the second accept conflicted");
        };
        assert_eq!(
            case.owner(&["show", &format!("{landed}:two.txt")]),
            "second agent\n"
        );
        assert_eq!(
            case.owner(&["show", &format!("{landed}:README")]),
            "one\ntwo\nthree\nfour\n"
        );
    }
}

/// A `main.lock` left in an agent clone's `refs/crazyagent`, as by an engine killed between
/// making the lock and renaming it, is cleared when main is next given, and the clone gets
/// the landed main instead of keeping an old one forever.
#[test]
fn a_stale_main_lock_in_a_clone_is_cleared_when_main_is_given() {
    let case = Case::own_branch();
    let other = second_clone(&case);
    let held = other.git_dir.join("refs/crazyagent");
    std::fs::create_dir_all(&held).unwrap();
    std::fs::write(held.join("main.lock"), "stale\n").unwrap();
    crate::landing::refresh_main(&case.db, &case.workspace.project).unwrap();
    assert_eq!(clone_main(&case, &other.worktree), case.tip());
    assert!(!held.join("main.lock").exists(), "the stale lock is gone");
}

/// Giving main at an agent start waits for the hold an Accept keeps on the owner
/// repository, so it never writes a tip read before that Accept after the Accept wrote the
/// newer one: while the hold is kept no clone gets main, and once it is let go every clone
/// does.
#[test]
fn giving_main_waits_for_the_repository_hold() {
    let case = Case::own_branch();
    let other = second_clone(&case);
    let engine = case.repo.parent().unwrap().join("engine");
    let held = flow::hold(&case.workspace.repository);
    let (given, done) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let (home, project) = (case.home.clone(), case.workspace.project.clone());
        scope.spawn(move || {
            git::OWNER_HOME.with(|owner| *owner.borrow_mut() = Some(home));
            let db = open(&engine).unwrap();
            given
                .send(crate::landing::refresh_main(&db, &project).map_err(|error| error.0))
                .unwrap();
        });
        assert!(
            done.recv_timeout(std::time::Duration::from_millis(1500))
                .is_err(),
            "main was given while the repository was held"
        );
        assert_eq!(clone_main(&case, &other.worktree), "");
        drop(held);
        assert_eq!(
            done.recv_timeout(std::time::Duration::from_secs(30))
                .unwrap(),
            Ok(())
        );
    });
    assert_eq!(clone_main(&case, &other.worktree), case.tip());
    assert_eq!(clone_main(&case, &case.workspace.worktree), case.tip());
}

/// The engine writes an agent clone's `refs/crazyagent/main` with no git run there and
/// never through a link the agent planted on the way (CC-N1): a `refs/crazyagent` that
/// links outside is refused and the folder it names stays empty; with the link gone the
/// clone's own git reads the ref.
#[test]
fn giving_main_to_a_clone_refuses_a_link_on_the_way_and_writes_nothing_outside() {
    let case = Case::own_branch();
    let tip = case.tip();
    let outside = case.repo.parent().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let planted = case.workspace.git_dir.join("refs/crazyagent");
    let _ = std::fs::remove_dir_all(&planted);
    std::os::unix::fs::symlink(&outside, &planted).unwrap();
    assert_eq!(
        clone::give_main(&case.repo, &case.workspace.worktree, &tip).map_err(|error| error.0),
        Err("changes_workspace_invalid".to_owned())
    );
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    std::fs::remove_file(&planted).unwrap();
    clone::give_main(&case.repo, &case.workspace.worktree, &tip).unwrap();
    assert_eq!(
        case.agent(&[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/crazyagent/main"
        ])
        .trim(),
        tip
    );
}

/// A clone that holds only its landed branch and the `refs/crazyagent/main` the engine
/// gives it, loose or packed by the agent's own git, still retires: that ref is the
/// engine's, rewritten at every landing, and holds no work of the agent's.
#[test]
fn a_clone_holding_the_engine_s_main_ref_still_retires() {
    let case = Case::own_branch();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    let blocker = || {
        retire_blocker(
            &case.db,
            &case.repo,
            &case.workspace.worktree,
            &case.workspace.git_dir,
            NOW_MS,
        )
        .unwrap()
    };
    assert_eq!(
        blocker(),
        None,
        "the clone cannot retire even without the main ref"
    );
    case.agent(&["update-ref", "refs/crazyagent/main", &commit]);
    assert_eq!(blocker(), None, "the loose main ref kept the clone");
    case.agent(&["pack-refs", "--all"]);
    assert_eq!(blocker(), None, "the packed main ref kept the clone");
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
    assert_eq!(case.owner_refs("refs/crazyagent"), case.base_ref_only());
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
    assert_eq!(case.owner_refs("refs/crazyagent"), case.base_ref_only());
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
            || case.owner_refs("refs/crazyagent") != case.base_ref_only();
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
    assert_eq!(case.owner_refs("refs/crazyagent"), case.base_ref_only());
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
    assert_eq!(case.owner_refs("refs/crazyagent"), case.base_ref_only());

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

/// A clone made before its exclude named the project graph gets that line once when it is
/// launched again (18.15.1), so the graph the engine then shows there never reaches the
/// agent's commits, and an exclude the agent replaced with a link is never written through.
#[test]
fn a_reused_clone_made_before_the_graph_exclude_gets_the_line_once() {
    let case = Case::own_branch();
    let exclude = case.workspace.git_dir.join("info").join("exclude");
    let read = || std::fs::read_to_string(&exclude).unwrap();
    std::fs::write(&exclude, "/.worktrees/").unwrap();
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert_eq!(read(), "/.worktrees/\n/graphify-out/\n");
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert_eq!(
        read(),
        "/.worktrees/\n/graphify-out/\n",
        "a second launch adds nothing"
    );

    std::fs::write(&exclude, "/.worktrees/\n!/graphify-out/\n").unwrap();
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert_eq!(
        read(),
        "/.worktrees/\n!/graphify-out/\n/graphify-out/\n",
        "a line taking the graph back into git is outweighed by the engine's own last line"
    );

    std::fs::write(&exclude, "/.worktrees/\ngraphify-out\n").unwrap();
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert_eq!(
        read(),
        "/.worktrees/\ngraphify-out\n",
        "any usual spelling already keeps the graph out"
    );

    let owner = case.repo.parent().unwrap().join("owner-file");
    std::fs::write(&owner, "kept\n").unwrap();
    std::fs::remove_file(&exclude).unwrap();
    std::os::unix::fs::symlink(&owner, &exclude).unwrap();
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert_eq!(
        std::fs::read_to_string(&owner).unwrap(),
        "kept\n",
        "a linked exclude is never written through"
    );

    let shared = case.repo.parent().unwrap().join("shared-file");
    std::fs::write(&shared, "/.worktrees/\n").unwrap();
    std::fs::remove_file(&exclude).unwrap();
    std::fs::hard_link(&shared, &exclude).unwrap();
    ready_agent_clone(&case.workspace.worktree).unwrap();
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "/.worktrees/\n",
        "a hard-linked exclude is never written through (F1)"
    );
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
    assert_eq!(case.owner_refs("refs/crazyagent"), case.base_ref_only());
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

fn head_file(git_dir: &Path) -> String {
    std::fs::read_to_string(git_dir.join("HEAD")).unwrap()
}

fn named(case: &Case, workspace: &Workspace) -> Result<String, EngineError> {
    name_agent_branch(
        &case.db,
        &case.repo,
        &workspace.worktree,
        &workspace.git_dir,
    )
}

#[test]
fn a_named_linked_agent_commits_on_its_own_branch() {
    let case = Case::new();
    let tip = case.tip();
    assert_eq!(head_file(&case.workspace.git_dir), format!("{tip}\n"));
    assert_eq!(named(&case, &case.workspace).unwrap(), "crazyagent/agent-1");
    assert_eq!(
        head_file(&case.workspace.git_dir),
        "ref: refs/heads/crazyagent/agent-1\n"
    );
    assert_eq!(
        case.owner(&["rev-parse", "refs/heads/crazyagent/agent-1"])
            .trim(),
        tip
    );
    assert_eq!(named(&case, &case.workspace).unwrap(), "crazyagent/agent-1");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    assert_ne!(head, tip);
    assert_eq!(
        case.owner(&["rev-parse", "refs/heads/crazyagent/agent-1"])
            .trim(),
        head
    );
    assert_eq!(
        head_file(&case.workspace.git_dir),
        "ref: refs/heads/crazyagent/agent-1\n"
    );
    assert_eq!(case.tip(), tip, "the owner's branch moved");
}

#[test]
fn a_leftover_agent_branch_refuses_the_launch() {
    let case = Case::new();
    let tip = case.tip();
    case.owner(&["branch", "crazyagent/agent-1"]);
    assert_eq!(
        refused(named(&case, &case.workspace)),
        "changes_agent_branch_exists"
    );
    assert_eq!(head_file(&case.workspace.git_dir), format!("{tip}\n"));
}

#[test]
fn retiring_a_named_worktree_deletes_its_branch_and_keeps_one_the_agent_left() {
    let case = Case::new();
    named(&case, &case.workspace).unwrap();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    discard(&case.db, &id, &case.view(&id).head, NOW_MS).unwrap();
    let late = NOW_MS + 8 * DAY_MS;
    assert_eq!(
        retire_worktree(
            &case.db,
            &case.repo,
            &case.workspace.worktree,
            &case.workspace.git_dir,
            late
        )
        .unwrap(),
        Ok(())
    );
    assert!(!case.workspace.worktree.exists());
    assert_eq!(
        case.owner(&[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads/crazyagent/"
        ]),
        ""
    );

    let left = second_agent(&case);
    named(&case, &left).unwrap();
    record_worktree_start(&case.db, &case.repo, &left.worktree, &left.git_dir).unwrap();
    git_in(&case.home, &left.worktree, &["checkout", "-q", "--detach"]);
    let kept = case.owner(&["rev-parse", "refs/heads/crazyagent/agent-2"]);
    assert_eq!(
        retire_worktree(&case.db, &case.repo, &left.worktree, &left.git_dir, late).unwrap(),
        Ok(())
    );
    assert_eq!(
        case.owner(&["rev-parse", "refs/heads/crazyagent/agent-2"]),
        kept,
        "a branch the worktree no longer had checked out is not the engine's to delete"
    );
}

#[test]
fn accept_into_an_unchecked_out_branch_moves_only_the_ref() {
    let case = Case::new();
    case.owner(&["branch", "release"]);
    let tip = case.tip();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    assert_eq!(
        target_branches(&case.db, &id).unwrap(),
        TargetBranches {
            branches: vec!["main".into(), "release".into()],
            current: Some("main".into()),
            land_on: Some("main".into()),
            land_on_problem: None,
        }
    );
    let before = case.checkout();
    assert_eq!(
        refused(accept_into(
            &case.db,
            &case.home,
            &id,
            &tip,
            None,
            "release",
            crate::activity::AcceptPlace::Changes
        )),
        "changes_stale"
    );
    let AcceptOutcome::Landed { commit, branch } = accept_into(
        &case.db,
        &case.home,
        &id,
        &head,
        None,
        "release",
        crate::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(branch, "release");
    assert_eq!(case.owner(&["rev-parse", "release"]).trim(), commit);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%an <%ae>|%s", &commit])
            .trim(),
        format!("{tip}|Repo Owner <owner@example.invalid>|Add the fourth line")
    );
    assert_eq!(
        case.owner(&["show", &format!("{commit}:README")]),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(case.tip(), tip);
    let after = case.checkout();
    let changed: Vec<&String> = after
        .iter()
        .filter(|(path, digest)| before.get(*path) != Some(*digest))
        .map(|(path, _)| path)
        .chain(before.keys().filter(|path| !after.contains_key(*path)))
        .collect();
    assert!(
        changed
            .iter()
            .any(|path| path.ends_with("refs/heads/release")),
        "{changed:?}"
    );
    assert!(
        changed.iter().all(|path| {
            (path.starts_with(".git/refs/") || path.starts_with(".git/logs/refs/"))
                && !path.ends_with("heads/main")
        }),
        "only refs other than the owner's branch moved: {changed:?}"
    );
    assert_eq!(case.view(&id).state, ChangeState::Accepted);
}

#[test]
fn accept_into_a_lead_agent_branch_updates_its_worktree() {
    let case = Case::new();
    let tip = case.tip();
    let agents = case
        .repo
        .join(crate::projects_home::CRAZYAGENT_DIRECTORY)
        .join("worktrees");
    std::fs::create_dir_all(&agents).unwrap();
    let worktree = agents.join("agent-2");
    case.owner(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        worktree.to_str().unwrap(),
        "HEAD",
    ]);
    let lead = Workspace {
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-2".into(),
            label: "Agent 2".into(),
        },
        worktree,
        git_dir: case.repo.join(".git/worktrees/agent-2"),
        ..case.workspace.clone()
    };
    assert_eq!(named(&case, &lead).unwrap(), "crazyagent/agent-2");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { commit, branch } = accept_into(
        &case.db,
        &case.home,
        &id,
        &case.view(&id).head,
        None,
        "crazyagent/agent-2",
        crate::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!(branch, "crazyagent/agent-2");
    assert_eq!(
        std::fs::read_to_string(lead.worktree.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
    assert_eq!(
        git_in(&case.home, &lead.worktree, &["rev-parse", "HEAD"]).trim(),
        commit
    );
    assert_eq!(
        head_file(&lead.git_dir),
        "ref: refs/heads/crazyagent/agent-2\n"
    );
    assert_eq!(
        git_in(&case.home, &lead.worktree, &["status", "--porcelain"]),
        ""
    );
    assert_eq!(case.tip(), tip);

    case.edit("NOTES", "notes\n");
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    write(&lead.worktree.join("README"), "lead edits\n");
    let lead_tip = git_in(&case.home, &lead.worktree, &["rev-parse", "HEAD"]);
    assert_eq!(
        refused(accept_into(
            &case.db,
            &case.home,
            &second,
            &case.view(&second).head,
            None,
            "crazyagent/agent-2",
            crate::activity::AcceptPlace::Changes,
        )),
        "changes_agent_busy"
    );
    assert_eq!(
        git_in(&case.home, &lead.worktree, &["rev-parse", "HEAD"]),
        lead_tip
    );
    assert_eq!(
        std::fs::read_to_string(lead.worktree.join("README")).unwrap(),
        "lead edits\n"
    );
}

#[test]
fn accept_into_refuses_invalid_and_externally_checked_out_targets() {
    let case = Case::new();
    case.owner(&["branch", "release"]);
    case.owner(&["symbolic-ref", "refs/heads/alias", "refs/heads/release"]);
    let outside = case.repo.parent().unwrap().join("plain");
    case.owner(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "elsewhere",
        outside.to_str().unwrap(),
        "HEAD",
    ]);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let before = case.checkout();
    let release = case.owner(&["rev-parse", "release"]);
    let long = "b".repeat(BRANCH_BYTES_MAX + 1);
    for (target, code) in [
        ("-x", "changes_target_invalid"),
        ("a..b", "changes_target_invalid"),
        ("", "changes_target_invalid"),
        ("missing", "changes_target_invalid"),
        ("alias", "changes_target_invalid"),
        (long.as_str(), "changes_target_invalid"),
        ("elsewhere", "changes_target_checked_out"),
    ] {
        assert_eq!(
            refused(accept_into(
                &case.db,
                &case.home,
                &id,
                &head,
                None,
                target,
                crate::activity::AcceptPlace::Changes
            )),
            code,
            "{target:?}"
        );
    }
    assert_eq!(case.checkout(), before);
    assert_eq!(case.owner(&["rev-parse", "release"]), release);
    assert_eq!(case.view(&id).state, ChangeState::Pending);
}

impl Case {
    /// The same owner repository with a harness in its own clone on the task branch the
    /// engine claimed for it (DEC-100), its start recorded. Input: the task name.
    /// Output: the case and the branch.
    fn harness(name: &str) -> (Self, String) {
        let mut case = Self::new();
        let worktree = case.repo.parent().unwrap().join("agent-own");
        let branch = claim_task_branch(&case.db, &case.repo, &worktree, name).unwrap();
        add_agent_clone_on(&case.repo, &worktree, None, &branch).unwrap();
        case.workspace.git_dir = crate::agent_git::git_dir(&worktree);
        case.workspace.worktree = worktree;
        case.start();
        (case, branch)
    }
}

#[test]
fn a_harness_clone_works_on_its_task_branch() {
    let (case, branch) = Case::harness("docs");
    assert_eq!(branch, "crazyagent/docs");
    assert_eq!(
        case.agent(&["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/crazyagent/docs"
    );
    let exclude = std::fs::read_to_string(case.workspace.git_dir.join("info/exclude")).unwrap();
    assert!(
        exclude.lines().any(|line| line == "/.worktrees/"),
        "{exclude}"
    );
    assert!(
        exclude.lines().any(|line| line == "/graphify-out/"),
        "the project graph shown in the clone never reaches the agent's commits: {exclude}"
    );
    assert_eq!(
        agent_branch(&case.db, &case.workspace.worktree, &case.workspace.git_dir).unwrap(),
        Some(branch.clone())
    );
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).files, 1);
    let AcceptOutcome::Landed {
        commit,
        branch: landed,
    } = case.accept(&id).unwrap()
    else {
        panic!("the accept conflicted");
    };
    assert_eq!((landed.as_str(), case.tip()), ("main", commit));
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
}

#[test]
fn claiming_a_taken_task_branch_takes_the_next_suffix() {
    let case = Case::new();
    let root = case.repo.parent().unwrap().to_path_buf();
    let (other, mine) = (root.join("agent-other"), root.join("agent-mine"));
    let claim =
        |worktree: &Path, name: &str| claim_task_branch(&case.db, &case.repo, worktree, name);
    assert_eq!(claim(&other, "x").unwrap(), "crazyagent/x");
    case.owner(&["branch", "crazyagent/x-2"]);
    case.owner(&["branch", "crazyagent/x-3/y"]);
    assert_eq!(claim(&mine, "x").unwrap(), "crazyagent/x-4");
    assert_eq!(
        claim(&mine, "x").unwrap(),
        "crazyagent/x-4",
        "its own stored row does not count as taken"
    );
    case.owner(&["branch", "crazyagent/z"]);
    for suffix in 2..=64 {
        case.owner(&["branch", &format!("crazyagent/z-{suffix}")]);
    }
    assert_eq!(refused(claim(&mine, "z")), "changes_agent_branch_exists");
    for invalid in ["Bad", "", "a--b", "-a"] {
        assert_eq!(
            refused(claim(&mine, invalid)),
            "changes_agent_branch_invalid",
            "{invalid:?}"
        );
    }
}

#[test]
fn the_masters_branch_resolves_choice_then_origin_head_then_checkout() {
    let case = Case::new();
    let tip = case.tip();
    case.owner(&["branch", "trunk"]);
    assert_eq!(
        master_branch(&case.repo, Some("trunk")).unwrap(),
        Some(("trunk".to_owned(), tip.clone()))
    );
    for missing in ["missing", "-x", "a..b", ""] {
        assert_eq!(
            refused(master_branch(&case.repo, Some(missing))),
            "changes_target_invalid",
            "{missing:?}"
        );
    }
    assert_eq!(
        master_branch(&case.repo, None).unwrap(),
        Some(("main".to_owned(), tip.clone()))
    );
    case.owner(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/trunk",
    ]);
    assert_eq!(
        master_branch(&case.repo, None).unwrap(),
        Some(("trunk".to_owned(), tip.clone()))
    );
    case.owner(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/gone",
    ]);
    assert_eq!(
        master_branch(&case.repo, None).unwrap(),
        Some(("main".to_owned(), tip.clone())),
        "an origin HEAD with no local branch falls back to the checkout"
    );
    case.owner(&["symbolic-ref", "-d", "refs/remotes/origin/HEAD"]);
    case.owner(&["checkout", "-q", "--detach"]);
    assert_eq!(master_branch(&case.repo, None).unwrap(), None);
}

impl Case {
    /// Adds a teammate worktree to a harness clone at `.worktrees/<name>` on
    /// `<task branch>--<name>` with one committed file, as a native teammate is told to.
    /// Inputs: the task branch and the teammate's name. Output: the teammate's folder and
    /// branch.
    fn teammate(&self, branch: &str, name: &str) -> (PathBuf, String) {
        let teammate_branch = format!("{branch}--{name}");
        let folder = format!(".worktrees/{name}");
        self.agent(&["worktree", "add", "-q", &folder, "-b", &teammate_branch]);
        let teammate = self.workspace.worktree.join(&folder);
        write(&teammate.join("NOTES"), "teammate\n");
        git_in(&self.home, &teammate, &["add", "NOTES"]);
        git_in(
            &self.home,
            &teammate,
            &[
                "-c",
                "user.name=Teammate",
                "-c",
                "user.email=teammate@example.invalid",
                "commit",
                "-q",
                "-m",
                "Teammate notes",
            ],
        );
        (teammate, teammate_branch)
    }

    fn merge_in_clone(&self, branch: &str) {
        self.agent(&[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "merge",
            "-q",
            "--no-edit",
            branch,
        ]);
    }

    fn land(&self) {
        let id = self.turn(TurnOutcome::Succeeded).unwrap();
        assert!(matches!(
            self.accept(&id).unwrap(),
            AcceptOutcome::Landed { .. }
        ));
    }

    fn blocker(&self) -> Option<&'static str> {
        retire_blocker(
            &self.db,
            &self.repo,
            &self.workspace.worktree,
            &self.workspace.git_dir,
            NOW_MS,
        )
        .unwrap()
    }
}

#[test]
fn teammate_worktrees_never_reach_the_change_set() {
    let (case, branch) = Case::harness("docs");
    let (_, teammate_branch) = case.teammate(&branch, "a");
    case.merge_in_clone(&teammate_branch);
    write(
        &case.workspace.worktree.join(".worktrees/stray.txt"),
        "left by an agent\n",
    );
    case.agent(&["add", "-f", ".worktrees/stray.txt"]);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let tip = |case: &Case| {
        agent_tip(
            &case.db,
            &case.repo,
            &case.workspace.worktree,
            &case.workspace.git_dir,
        )
        .unwrap()
    };
    let first = tip(&case);
    assert_eq!(
        first,
        tip(&case),
        "the same clone tip fetches to the same head"
    );
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let mut files: Vec<String> = diff(&case.db, &id, &case.view(&id).head, 0)
        .unwrap()
        .files
        .into_iter()
        .map(|file| file.path)
        .collect();
    files.sort();
    assert_eq!(files, vec!["NOTES".to_owned(), "README".to_owned()]);
    assert_eq!(case.view(&id).head, first);
    let AcceptOutcome::Landed { commit, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    let landed = case.owner(&["ls-tree", "-r", "--name-only", &commit]);
    assert!(!landed.contains(".worktrees"), "{landed}");
    assert!(landed.lines().any(|path| path == "NOTES"), "{landed}");
}

#[test]
fn cleanup_keeps_a_harness_with_unmerged_or_dirty_teammates() {
    let (mut case, branch) = Case::harness("docs");
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute(
            "INSERT INTO team_agents(project,role,harness,tools,status,created_ms,heartbeat_ms,mode) VALUES (?1,'sub','claude-code',1,'working',1,1,'native')",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    case.workspace.agent.id = case.db.last_insert_rowid().to_string();
    let (teammate, teammate_branch) = case.teammate(&branch, "a");
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    case.land();
    let unlanded = |case: &Case| {
        unlanded_teammates(
            &case.db,
            &case.repo,
            &case.workspace.worktree,
            &case.workspace.git_dir,
        )
        .unwrap()
        .unwrap()
    };
    assert_eq!(case.blocker(), Some("cleanup_refs_unlanded"));
    assert!(!unlanded(&case).is_empty());

    case.merge_in_clone(&teammate_branch);
    case.land();
    write(&teammate.join("SCRATCH"), "not committed\n");
    assert_eq!(case.blocker(), Some("cleanup_worktree_dirty"));

    std::fs::remove_file(teammate.join("SCRATCH")).unwrap();
    assert_eq!(case.blocker(), None);
    assert_eq!(unlanded(&case), Vec::<String>::new());
    assert_eq!(
        retire_worktree(
            &case.db,
            &case.repo,
            &case.workspace.worktree,
            &case.workspace.git_dir,
            NOW_MS
        )
        .unwrap(),
        Ok(())
    );
    assert!(!case.workspace.worktree.exists());
    let stored: i64 = case
        .db
        .query_row("SELECT COUNT(*) FROM landing_agent_branches", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(stored, 0, "the stored task branch goes with the worktree");
}

/// Commits every given file in a lead's worktree as the lead would. Inputs: the case, the
/// lead's worktree and the files. Output: none; the commit is on the lead's branch.
fn lead_commit(case: &Case, lead: &Path, files: &[String]) {
    for file in files {
        write(&lead.join(file), "lead work\n");
    }
    git_in(&case.home, lead, &["add", "-A"]);
    git_in(
        &case.home,
        lead,
        &[
            "-c",
            "user.name=Lead",
            "-c",
            "user.email=lead@example.invalid",
            "commit",
            "-q",
            "-m",
            "Lead work",
        ],
    );
}

/// Makes team agent 35 a lead of the case's project with `files` lead files committed
/// ahead of the owner's `main`: in a linked worktree on the branch the engine names for
/// it, or in its own clone, recorded in the team tables. Inputs: the case, how many files
/// the lead adds and whether it works in its own clone. Output: the lead's worktree and
/// its tip as the engine reads it into the owner repository.
fn lead_35(case: &Case, files: usize, own_clone: bool) -> (PathBuf, String) {
    let lead = case.repo.parent().unwrap().join("agent-35");
    let git_dir = if own_clone {
        add_agent_clone(&case.repo, &lead).unwrap();
        crate::agent_git::git_dir(&lead)
    } else {
        case.owner(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "crazyagent/agent-35",
            lead.to_str().unwrap(),
            "main",
        ]);
        case.repo.join(".git/worktrees/agent-35")
    };
    let made: Vec<String> = (0..files).map(|at| format!("lead/file-{at}.txt")).collect();
    lead_commit(case, &lead, &made);
    crate::team::initialize(&case.db).unwrap();
    let project = case.workspace.project.as_str();
    case.db
        .execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
             VALUES (35,?1,'sub','claude',0,'idle',1,1,NULL)",
            [project],
        )
        .unwrap();
    case.db
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (35,?1,?2,?3,?4)",
            [
                project,
                case.repo.to_str().unwrap(),
                lead.to_str().unwrap(),
                git_dir.to_str().unwrap(),
            ],
        )
        .unwrap();
    let tip = flow::tip(&case.db, &case.repo, &lead, &git_dir).unwrap();
    (lead, tip)
}

/// Records a worker of lead 35 in the team tables at its workspace, whose agent id is its
/// team id, moves its worktree to the lead's tip and records its start there, as the
/// engine cuts a worker from its lead (N1). Inputs: the case, the worker's workspace and
/// the lead's tip. Output: none; the worker is in place.
fn put_under_lead(case: &Case, workspace: &Workspace, lead_tip: &str) {
    let agent: i64 = workspace.agent.id.parse().unwrap();
    let project = workspace.project.as_str();
    case.db
        .execute(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent)
             VALUES (?1,?2,'sub','claude',0,'idle',1,1,35)",
            rusqlite::params![agent, project],
        )
        .unwrap();
    case.db
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                agent,
                project,
                workspace.repository.to_str().unwrap(),
                workspace.worktree.to_str().unwrap(),
                workspace.git_dir.to_str().unwrap()
            ],
        )
        .unwrap();
    git_in(
        &case.home,
        &workspace.worktree,
        &["checkout", "-q", "--detach", lead_tip],
    );
    record_worktree_start(
        &case.db,
        &workspace.repository,
        &workspace.worktree,
        &workspace.git_dir,
    )
    .unwrap();
}

/// Puts a test change set's agent, as team agent 52, under lead 35 whose linked worktree
/// is `files` lead files ahead of the owner's `main`. Inputs: the case and how many files
/// the lead adds. Output: the lead's tip.
fn worker_of_lead(case: &mut Case, files: usize) -> String {
    let (_, lead_tip) = lead_35(case, files, false);
    case.workspace.agent.id = "52".into();
    put_under_lead(case, &case.workspace, &lead_tip);
    lead_tip
}

/// Sets the project's Lands setting as the owner picks it in Team settings. Inputs: the
/// case and the setting's name. Output: none; the setting is stored.
fn set_lands(case: &Case, lands: &str) {
    case.db
        .execute(
            "INSERT INTO team_nesting(project,depth,lands) VALUES (?1,2,?2)",
            [case.workspace.project.as_str(), lands],
        )
        .unwrap();
}

/// A change set's files, added lines and removed lines, as its view counts them.
type Counts = (u32, u32, u32);

/// Reads the counts a change set holds after its turn end and after a poll, and every path
/// its diff pages show. Inputs: the case and the change set. Output: both counts and the
/// paths.
fn measured(case: &Case, id: &ChangeSetId) -> (Counts, Counts, Vec<String>) {
    let counts = |view: ChangeSetView| -> Counts { (view.files, view.added, view.removed) };
    let recorded = counts(case.view(id));
    observe(&case.db, None).unwrap();
    let view = case.view(id);
    assert_eq!(view.state, ChangeState::Pending);
    let mut paths: Vec<String> = all_pages(case, id)
        .iter()
        .flat_map(|page| page.files.iter().map(|file| file.path.clone()))
        .collect();
    paths.sort();
    paths.dedup();
    (recorded, counts(view), paths)
}

/// A lead's worker lands on its lead's branch, so its change set holds only what would
/// change there: with the owner's branch far behind the lead, the set counts, judges and
/// shows the worker's one file, never the lead's work the owner's branch lacks (owner
/// session 2026-10-29, sets a3edc313 and ef58a29b).
#[test]
fn a_lead_worker_s_set_is_measured_against_its_lead_s_branch() {
    let mut case = Case::new();
    let lead_tip = worker_of_lead(&mut case, 30);
    assert_ne!(case.tip(), lead_tip);
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let recorded = case.view(&id);
    assert_eq!(
        (recorded.files, recorded.added, recorded.removed),
        (1, 1, 0),
        "turn end counted the lead's work"
    );
    observe(&case.db, None).unwrap();
    let observed = case.view(&id);
    assert_eq!(observed.state, ChangeState::Pending);
    assert_eq!(
        (observed.files, observed.added, observed.removed),
        (1, 1, 0),
        "the owner view counted the lead's work"
    );
    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    let paths: Vec<&str> = page.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["worker.txt"], "the diff showed the lead's work");
}

/// While the owner's Lands setting is Owner, the owner accepts a lead's worker's set onto
/// the owner's branch, so it is measured against that branch: the lead's work the owner's
/// branch lacks is part of it (Q16, current rule).
#[test]
fn a_lead_worker_s_set_is_measured_against_the_owner_s_branch_while_lands_is_owner() {
    let mut case = Case::new();
    worker_of_lead(&mut case, 30);
    set_lands(&case, "owner");
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (recorded, observed, paths) = measured(&case, &id);
    assert_eq!(
        (recorded, observed),
        ((31, 31, 0), (31, 31, 0)),
        "a Lands Owner worker set was measured against its lead's branch"
    );
    assert_eq!(paths.len(), 31);
    assert!(paths.iter().any(|path| path == "worker.txt"));
    assert!(paths.iter().any(|path| path == "lead/file-0.txt"));
}

/// A worker of a lead in its own clone is measured against the lead's clone branch, read
/// through the engine's checked fetch.
#[test]
fn a_clone_lead_s_worker_set_is_measured_against_the_lead_s_clone_branch() {
    let mut case = Case::new();
    let (_, lead_tip) = lead_35(&case, 30, true);
    case.workspace.agent.id = "52".into();
    put_under_lead(&case, &case.workspace, &lead_tip);
    assert_ne!(case.tip(), lead_tip);
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (recorded, observed, paths) = measured(&case, &id);
    assert_eq!(
        (recorded, observed),
        ((1, 1, 0), (1, 1, 0)),
        "the clone lead's work was counted"
    );
    assert_eq!(paths, ["worker.txt"]);
}

/// Update merges a lead's worker's set with its lead's branch, the branch it lands on, and
/// the merge commit and the resolve prompt name that branch, never the owner's.
#[test]
fn update_merges_a_lead_worker_s_set_with_its_lead_s_branch_and_names_it() {
    let mut case = Case::new();
    worker_of_lead(&mut case, 3);
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let worker_head = case.view(&id).head;
    let lead = case.repo.parent().unwrap().join("agent-35");
    lead_commit(&case, &lead, &["lead/later.txt".to_owned()]);
    let later = git_in(&case.home, &lead, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    update(&case.db, &id).unwrap();
    let view = case.view(&id);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%s", &view.head])
            .trim(),
        format!(
            "{worker_head} {later}|Merge the lead's branch crazyagent/agent-35 into this agent's work"
        )
    );
    assert_eq!(
        (view.state, view.base.as_str()),
        (ChangeState::Updating, later.as_str())
    );
    let prompt = resolve_task_prompt(&case.db, &id).unwrap();
    assert!(
        prompt.starts_with(
            "Your lead's branch crazyagent/agent-35 changed while your work waited for your lead's merge, so CrazyAgent merged it into your worktree.\n"
        ),
        "{prompt}"
    );
    assert!(!prompt.contains("owner"), "{prompt}");
}

/// Update merges a clone lead's worker's set with the lead's clone branch as the engine
/// fetches it now: the merge's second parent is the lead's newest commit, never the tip
/// the worker started from, and the merge names the lead's clone branch.
#[test]
fn update_merges_a_clone_lead_s_newest_tip_into_its_worker_s_set() {
    let mut case = Case::new();
    let (lead, lead_tip) = lead_35(&case, 3, true);
    case.workspace.agent.id = "52".into();
    put_under_lead(&case, &case.workspace, &lead_tip);
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let worker_head = case.view(&id).head;
    lead_commit(&case, &lead, &["lead/later.txt".to_owned()]);
    let later = git_in(&case.home, &lead, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    assert_ne!(later, lead_tip);
    update(&case.db, &id).unwrap();
    let view = case.view(&id);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%s", &view.head])
            .trim(),
        format!(
            "{worker_head} {later}|Merge the lead's branch agent/agent-35 into this agent's work"
        )
    );
    assert_eq!(view.base, later);
}

/// A lead's worker whose lead's worktree is no longer recorded keeps its stored change and
/// is never measured against the owner's branch, and Update refuses it instead of merging
/// the owner's branch into a worker that lands on its lead's.
#[test]
fn a_lead_worker_whose_lead_s_worktree_is_gone_keeps_its_change_and_update_refuses() {
    let mut case = Case::new();
    worker_of_lead(&mut case, 30);
    case.db
        .execute("DELETE FROM team_worktrees WHERE agent=35", [])
        .unwrap();
    case.edit("worker.txt", "worker work\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (recorded, observed, paths) = measured(&case, &id);
    assert_eq!(
        (recorded, observed),
        ((1, 1, 0), (1, 1, 0)),
        "the set was measured against the owner's branch"
    );
    assert_eq!(paths, ["worker.txt"]);
    let before = case.view(&id);
    assert_eq!(refused(update(&case.db, &id)), "changes_workspace_invalid");
    assert_eq!(case.view(&id), before);
}

/// A Changes poll reads one lead's branch once for all of its workers' sets: six worker
/// sets of one lead cost the git runs two do.
#[test]
fn a_poll_reads_a_lead_s_tip_once_for_all_of_its_workers() {
    let mut case = Case::new();
    let lead_tip = worker_of_lead(&mut case, 3);
    case.edit("worker.txt", "worker work\n");
    let mut ids = vec![case.turn(TurnOutcome::Succeeded).unwrap()];
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Write notes".into(),
    };
    let open = |agent: usize| {
        let name = format!("agent-{agent}");
        let worktree = case.repo.parent().unwrap().join(&name);
        case.owner(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            "main",
        ]);
        let workspace = Workspace {
            agent: ChangeAgent {
                kind: AgentKind::Sub,
                id: agent.to_string(),
                label: format!("Worker {agent}"),
            },
            worktree,
            git_dir: case.repo.join(".git/worktrees").join(&name),
            ..case.workspace.clone()
        };
        put_under_lead(&case, &workspace, &lead_tip);
        write(&workspace.worktree.join(format!("{name}.txt")), "notes\n");
        record_turn_end(&case.db, &workspace, &turn)
            .unwrap()
            .expect("the worker's work opens a change set")
    };
    let runs = || {
        case.observe();
        let before = git::GIT_RUNS.with(std::cell::Cell::get);
        case.observe();
        git::GIT_RUNS.with(std::cell::Cell::get) - before
    };
    ids.push(open(60));
    let two = runs();
    ids.extend((61..=64).map(open));
    assert_eq!(
        runs(),
        two,
        "six worker sets of one lead cost more git runs than two"
    );
    for id in &ids {
        let view = case.view(id);
        assert_eq!(
            (view.state, view.files, view.added, view.removed),
            (ChangeState::Pending, 1, 1, 0)
        );
    }
}

impl Case {
    /// Merges a commit into the agent worktree the way an agent running git by its full
    /// path can, with a merge commit. Input: the commit. Output: the new HEAD.
    fn agent_merge(&self, commit: &str) -> String {
        self.agent(&[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "merge",
            "-q",
            "--no-ff",
            "--no-edit",
            commit,
        ]);
        self.agent_head()
    }

    /// Makes branch `side` in the owner repository with one commit the owner's branch
    /// does not hold, leaving the owner on its branch. Output: the side tip.
    fn side_branch(&self) -> String {
        self.owner(&["checkout", "-q", "-b", "side"]);
        self.owner_commit("side.txt", "side\n", "Side work");
        let side = self.owner(&["rev-parse", "side"]).trim().to_owned();
        self.owner(&["checkout", "-q", "main"]);
        side
    }
}

fn merge_turn() -> TurnEnd {
    TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Do the work".into(),
    }
}

#[test]
fn a_change_set_whose_agent_merged_another_branch_is_refused_and_nothing_is_committed() {
    let case = Case::new();
    case.start();
    let side = case.side_branch();
    case.edit("own.txt", "own\n");
    case.agent_commit("Own work");
    let merged = case.agent_merge(&side);
    assert_eq!(case.agent(&["rev-parse", "HEAD^2"]).trim(), side);
    case.edit("own.txt", "own\nuncommitted\n");
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &merge_turn())),
        "changes_outside_merge"
    );
    assert_eq!(case.agent_head(), merged);
    assert!(list(&case.db, &case.workspace.project).unwrap().is_empty());
}

#[test]
fn the_engines_own_update_merge_leaves_the_next_turn_to_add_to_its_change_set() {
    let case = Case::new();
    case.start();
    case.edit("own.txt", "own\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let owner_tip = case.owner_commit("other.txt", "owner\n", "Owner work");
    update(&case.db, &id).unwrap();
    assert_eq!(case.agent(&["rev-parse", "HEAD^2"]).trim(), owner_tip);
    case.edit("own.txt", "own\nmore\n");
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    assert_eq!(case.view(&id).state, ChangeState::Pending);
}

#[test]
fn a_merge_already_in_the_commit_an_agent_started_from_is_not_its_own() {
    let case = Case::new();
    let side = case.side_branch();
    let inherited = case.agent_merge(&side);
    case.start();
    case.edit("own.txt", "own\n");
    assert!(case.turn(TurnOutcome::Succeeded).is_some());
    assert_eq!(
        case.agent(&["rev-parse", "HEAD~1"]).trim(),
        inherited,
        "the turn commit sits on the inherited merge"
    );
}

#[test]
fn a_lead_may_merge_its_own_workers_tip_and_is_refused_for_any_other_branch() {
    let mut case = Case::new();
    case.workspace.agent.id = "1".into();
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute_batch(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent,mode)
             VALUES (1,'project-1','sub','claude',1,'running',1,1,NULL,'app_spawned'),
                    (2,'project-1','sub','claude',1,'running',1,1,1,'app_spawned');",
        )
        .unwrap();
    let worker = second_agent(&case);
    case.db
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (2,'project-1',?1,?2,?3)",
            rusqlite::params![
                worker.repository.to_str().unwrap(),
                worker.worktree.to_str().unwrap(),
                worker.git_dir.to_str().unwrap()
            ],
        )
        .unwrap();
    case.start();
    write(&worker.worktree.join("worker.txt"), "worker\n");
    git_in(&case.home, &worker.worktree, &["add", "-A"]);
    git_in(
        &case.home,
        &worker.worktree,
        &[
            "-c",
            "user.name=Worker",
            "-c",
            "user.email=worker@example.invalid",
            "commit",
            "-q",
            "-m",
            "Worker work",
        ],
    );
    let worker_tip = git_in(&case.home, &worker.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    case.agent_merge(&worker_tip);
    assert_eq!(case.agent(&["rev-parse", "HEAD^2"]).trim(), worker_tip);
    let id = record_turn_end(&case.db, &case.workspace, &merge_turn())
        .unwrap()
        .unwrap();
    let side = case.side_branch();
    let merged = case.agent_merge(&side);
    assert_eq!(
        refused(record_turn_end(&case.db, &case.workspace, &merge_turn())),
        "changes_outside_merge"
    );
    assert_eq!(case.agent_head(), merged);
    assert_eq!(case.view(&id).state, ChangeState::Pending);
}

/// The master's worktree as the engine leases it, in the case's store: the project and
/// its repository admitted, and a master bootstrap lease checked out on its own
/// `crazyagent/` branch, the only HEAD a master change set is recorded from. Input: the
/// case. Output: the master's workspace.
fn master_agent(case: &Case) -> Workspace {
    use crate::protocol::{Outcome, ProjectSpec, SessionId, TaskId};
    let engine = case.repo.parent().unwrap().join("engine");
    let project = &case.workspace.project;
    crate::storage::initialize_authority_schemas(&case.db).unwrap();
    let repository = case.repo.to_str().unwrap();
    let spec = ProjectSpec {
        name: "Project one".into(),
        repositories: vec![repository.into()],
    };
    let task = TaskId::new("master-bootstrap-1").unwrap();
    let session = SessionId::new("session-1").unwrap();
    let tx = case.db.unchecked_transaction().unwrap();
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
            &tx, &engine, project, &task, &session, repository
        )
        .unwrap(),
        Outcome::Applied
    );
    tx.commit().unwrap();
    crate::worktrees::provision(&case.db, &task, repository).unwrap();
    let Outcome::Worktrees { leases } = crate::worktrees::leases(&case.db, &task).unwrap() else {
        panic!("the bootstrap task holds its lease");
    };
    assert_eq!(leases.len(), 1);
    let git_dir = crate::worktrees::lease_git_dir(&case.db, &leases[0]).unwrap();
    Workspace {
        agent: ChangeAgent {
            kind: AgentKind::Master,
            id: "master-1".into(),
            label: "Master".into(),
        },
        worktree: PathBuf::from(&leases[0].path),
        git_dir,
        ..case.workspace.clone()
    }
}

/// The owner asks the master to resolve the conflicts and merge (DEC-117 Q2). At Full
/// access the master's merge of an open change set into its own work passes its turn,
/// and accepting the master's set lands the merged work; below Full access the same merge
/// is refused as outside work (DEC-107).
#[test]
fn a_master_turn_merging_an_open_change_set_passes_only_at_full_access() {
    use crate::sandbox::policy::{AgentAccess, AgentPolicy};
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let sub_set = case.turn(TurnOutcome::Succeeded).unwrap();
    let sub_head = case.view(&sub_set).head;
    let master = master_agent(&case);
    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    let in_master = |args: &[&str]| git_in(&case.home, &master.worktree, args);
    in_master(&[
        "-c",
        "user.name=Master",
        "-c",
        "user.email=master@example.invalid",
        "merge",
        "-q",
        "--no-ff",
        "--no-edit",
        &sub_head,
    ]);
    assert_eq!(in_master(&["rev-parse", "HEAD^2"]).trim(), sub_head);
    let save = |access| {
        crate::sandbox::policy::save(
            &case.db,
            "project-1",
            &AgentPolicy {
                access,
                ..AgentPolicy::default()
            },
        )
        .unwrap()
    };
    save(AgentAccess::OwnBranch);
    assert_eq!(
        refused(record_turn_end(&case.db, &master, &merge_turn())),
        "changes_outside_merge"
    );
    save(AgentAccess::Full);
    let master_set = record_turn_end(&case.db, &master, &merge_turn())
        .unwrap()
        .expect("the master's merge makes its change set");
    case.accept(&master_set).unwrap();
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n",
        "accepting the master's set lands the merged change set's work"
    );
}

/// A master's change set that conflicts with main waits for the owner at its turn end, as
/// before OS-8: the engine never merges main into the master's leased worktree.
#[test]
fn a_master_s_set_that_conflicts_with_main_waits_for_the_owner_at_its_turn_end() {
    let case = Case::new();
    let master = master_agent(&case);
    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    std::fs::write(
        master.worktree.join("README"),
        "one\ntwo\nthree, the master's\n",
    )
    .unwrap();
    let recorded = crate::landing_turns::record(&case.db, &master, &merge_turn(), None)
        .map_err(|problem| problem.0);
    let Ok(crate::landing_turns::Landed::Added(id)) = recorded else {
        panic!("the master's turn kept no change set: {recorded:?}");
    };
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(
        std::fs::read_to_string(master.worktree.join("README")).unwrap(),
        "one\ntwo\nthree, the master's\n",
        "the engine merged main into the master's work at its turn end"
    );
    assert!(
        matches!(case.accept(&id).unwrap(), AcceptOutcome::Conflict { .. }),
        "the master's set clashes with main"
    );
}

/// Saves the project's agent access in the case's store. Inputs: the case and the access.
fn save_access(case: &Case, access: crate::sandbox::policy::AgentAccess) {
    crate::sandbox::policy::save(
        &case.db,
        "project-1",
        &crate::sandbox::policy::AgentPolicy {
            access,
            ..crate::sandbox::policy::AgentPolicy::default()
        },
    )
    .unwrap();
}

/// Records a merge commit of `commit` in an agent's worktree, as the agent's own merge
/// would. Inputs: the case, the agent's workspace and the commit.
fn merge_in(case: &Case, agent: &Workspace, commit: &str) {
    git_in(
        &case.home,
        &agent.worktree,
        &[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "merge",
            "-q",
            "--no-ff",
            "--no-edit",
            commit,
        ],
    );
    assert_eq!(
        git_in(&case.home, &agent.worktree, &["rev-parse", "HEAD^2"]).trim(),
        commit
    );
}

/// Full access widens the master's merges to open change sets only (DEC-117 Q2): its merge
/// of a commit no change set holds, or of a discarded set's head, is still refused as
/// outside work (DEC-107), while its merge of an open set passes.
#[test]
fn at_full_access_the_master_s_merge_of_a_foreign_commit_or_a_closed_set_is_refused() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    case.start();
    case.edit("closed.txt", "closed\n");
    let closed = case.turn(TurnOutcome::Succeeded).unwrap();
    let closed_head = case.view(&closed).head;
    discard(&case.db, &closed, &closed_head, NOW_MS).unwrap();
    assert_eq!(case.view(&closed).state, ChangeState::Discarded);
    case.edit("open.txt", "open\n");
    let open = case.turn(TurnOutcome::Succeeded).unwrap();
    let open_head = case.view(&open).head;
    let foreign = case.side_branch();
    let master = master_agent(&case);
    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    save_access(&case, AgentAccess::Full);
    let start = git_in(&case.home, &master.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    for (commit, what) in [
        (&foreign, "a commit no change set holds"),
        (&closed_head, "a discarded set's head"),
    ] {
        merge_in(&case, &master, commit);
        assert_eq!(
            record_turn_end(&case.db, &master, &merge_turn())
                .err()
                .map(|refusal| refusal.0)
                .as_deref(),
            Some("changes_outside_merge"),
            "the master's merge of {what} at Full access"
        );
        git_in(
            &case.home,
            &master.worktree,
            &["reset", "-q", "--hard", &start],
        );
    }
    merge_in(&case, &master, &open_head);
    assert!(
        record_turn_end(&case.db, &master, &merge_turn())
            .unwrap()
            .is_some(),
        "the master's merge of an open set passes at Full access"
    );
}

/// Full access widens no sub-agent's merges (DEC-117 Q2): a sub-agent's and a lead's merge
/// of another agent's open change set are still refused as outside work (DEC-107), while
/// the master's same merge passes.
#[test]
fn at_full_access_a_sub_agent_s_or_a_lead_s_merge_of_an_open_set_is_refused() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    case.start();
    case.edit("open.txt", "open\n");
    let open = case.turn(TurnOutcome::Succeeded).unwrap();
    let open_head = case.view(&open).head;
    let master = master_agent(&case);
    save_access(&case, AgentAccess::Full);
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute_batch(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms,parent,mode)
             VALUES (3,'project-1','sub','claude',1,'running',1,1,NULL,'app_spawned'),
                    (4,'project-1','sub','claude',1,'running',1,1,3,'app_spawned');",
        )
        .unwrap();
    let sub = second_agent(&case);
    let mut lead = numbered_agent(&case, 3);
    lead.agent.id = "3".into();
    let worker = numbered_agent(&case, 4);
    case.db
        .execute(
            "INSERT INTO team_worktrees(agent,project,repository,worktree,git_dir) VALUES (4,'project-1',?1,?2,?3)",
            rusqlite::params![
                worker.repository.to_str().unwrap(),
                worker.worktree.to_str().unwrap(),
                worker.git_dir.to_str().unwrap()
            ],
        )
        .unwrap();
    for (agent, what) in [(&sub, "a sub-agent"), (&lead, "a lead")] {
        record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir)
            .unwrap();
        merge_in(&case, agent, &open_head);
        assert_eq!(
            record_turn_end(&case.db, agent, &merge_turn())
                .err()
                .map(|refusal| refusal.0)
                .as_deref(),
            Some("changes_outside_merge"),
            "{what}'s merge of an open set at Full access"
        );
    }
    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    merge_in(&case, &master, &open_head);
    assert!(
        record_turn_end(&case.db, &master, &merge_turn())
            .unwrap()
            .is_some(),
        "the master's same merge passes at Full access"
    );
}

impl Case {
    fn observe(&self) {
        observe(&self.db, Some(&self.workspace.project)).unwrap();
    }

    /// Checks a change set closed as landed outside the app: accepted, marked outside, its
    /// pending ref gone, no action left, the quiet agent worktree on the owner's tip and
    /// no change set opened by the next turn. Inputs: the change set and its head.
    fn landed_outside(&self, id: &ChangeSetId, head: &str) {
        let view = self.view(id);
        assert_eq!(
            (view.state, view.outside),
            (ChangeState::Accepted, true),
            "{view:?}"
        );
        assert_eq!(self.owner_refs("refs/crazyagent"), "");
        assert_eq!(refused(self.accept(id)), "changes_stale");
        assert_eq!(
            refused(discard(&self.db, id, head, NOW_MS)),
            "changes_stale"
        );
        assert_eq!(self.agent_head(), self.tip());
        assert_eq!(self.turn(TurnOutcome::Succeeded), None);
    }

    /// Checks a change set closed as removed outside the app: discarded, marked outside,
    /// never restorable, its pending ref gone with every other engine ref but an own
    /// clone's base ref, which the clone keeps while it lives (GA-R5), and no action left.
    /// Inputs: the change set and its head.
    fn removed_outside(&self, id: &ChangeSetId, head: &str) {
        let view = self.view(id);
        assert_eq!(
            (view.state, view.outside, view.restorable_until_ms),
            (ChangeState::Discarded, true, None),
            "{view:?}"
        );
        let kept = if self.workspace.worktree.ends_with("agent-own") {
            self.base_ref_only()
        } else {
            String::new()
        };
        assert_eq!(self.owner_refs("refs/crazyagent"), kept);
        assert_eq!(
            refused(restore(&self.db, id, NOW_MS)),
            "changes_restore_expired"
        );
        assert_eq!(
            refused(discard(&self.db, id, head, NOW_MS)),
            "changes_stale"
        );
    }
}

#[test]
fn a_change_set_the_owner_merged_outside_the_app_closes_as_landed_outside() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.owner_commit("other.txt", "owner work\n", "Owner work");
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.outside, view.files, view.added),
        (ChangeState::Pending, false, 1, 1)
    );
    case.owner(&[
        "merge",
        "--no-ff",
        "-q",
        "-m",
        "Merge the agent work",
        &head,
    ]);
    case.observe();
    case.landed_outside(&id, &head);
}

#[test]
fn a_change_set_the_owner_cherry_picked_outside_the_app_closes_as_landed_outside() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.owner_commit("other.txt", "owner work\n", "Owner work");
    case.owner(&["cherry-pick", &head]);
    assert_eq!(
        case.owner(&["branch", "--contains", &head]),
        "",
        "the pick copied the change, not the commit"
    );
    case.observe();
    case.landed_outside(&id, &head);
}

#[test]
fn a_change_set_the_owner_squashed_outside_the_app_closes_as_landed_outside() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    assert_eq!(case.view(&id).files, 2);
    case.owner_commit("other.txt", "owner work\n", "Owner work");
    case.owner(&["merge", "--squash", "-q", &head]);
    case.owner(&["commit", "-q", "-m", "Squash the agent work"]);
    case.observe();
    case.landed_outside(&id, &head);
}

#[test]
fn a_change_set_the_owner_landed_in_part_by_hand_shows_only_the_rest_until_all_of_it_lands() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.owner_commit("README", "one\ntwo\nthree\nfour\n", "Owner adds the line");
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (
            view.state,
            view.outside,
            view.files,
            view.added,
            view.removed
        ),
        (ChangeState::Pending, false, 1, 1, 0)
    );
    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    let files: Vec<_> = page.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(files, ["src/new.rs"]);
    assert!(!page.text.contains("+four"), "{}", page.text);
    assert!(page.text.contains("+fn main() {}"), "{}", page.text);
    case.owner_commit("src/new.rs", "fn main() {}\n", "Owner adds main");
    case.observe();
    case.landed_outside(&id, &head);
}

/// Makes the activity tables the rows of a change set's closing need. Input: the case.
fn closing_tables(case: &Case) {
    case.db
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)",
        )
        .unwrap();
    crate::coord::ensure(&case.db).unwrap();
    crate::activity::initialize(&case.db).unwrap();
}

/// A case whose agent turn changed the README's second line, with the activity tables the
/// rows of its closing need. Output: the case, its change set and that set's head.
fn taken_case() -> (Case, ChangeSetId, String) {
    let case = Case::new();
    closing_tables(&case);
    case.edit("README", "one\nagent\nthree\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    (case, id, head)
}

/// Lands the agent's README line on the owner's branch in a commit of its own, then moves
/// that line on, so a merge of the set's head conflicts. Input: the case.
fn take_and_move_on(case: &Case) {
    case.owner_commit("README", "one\nagent\nthree\n", "Port the agent work");
    case.owner_commit("README", "one\nowner\nthree\n", "Build on the port");
}

/// The activity rows of a change set as actor, kind and text. Inputs: the case and the set.
fn set_rows(case: &Case, id: &ChangeSetId) -> Vec<(String, &'static str, String)> {
    let record = super::store::load(&case.db, id).unwrap();
    crate::activity::list(&case.db, &record.project, crate::activity::LIST_MAX)
        .unwrap()
        .into_iter()
        .filter(|item| item.change_set.as_deref() == Some(id.as_str()))
        .map(|item| (item.actor, item.kind.name(), item.text))
        .collect()
}

#[test]
fn a_change_set_main_took_and_moved_on_from_closes_restorably_by_the_engine() {
    let (case, id, head) = taken_case();
    take_and_move_on(&case);
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.outside),
        (ChangeState::Discarded, false),
        "{view:?}"
    );
    assert!(view.restorable_until_ms.is_some(), "{view:?}");
    assert_eq!(view.conflicts, Vec::<String>::new());
    assert_eq!(
        refused(discard(&case.db, &id, &head, NOW_MS)),
        "changes_stale"
    );
    assert_eq!(case.agent_head(), case.tip());
    let record = super::store::load(&case.db, &id).unwrap();
    assert_eq!(
        set_rows(&case, &id),
        vec![(
            crate::activity::ENGINE_ACTOR.to_owned(),
            "discarded",
            format!(
                "Closed the changes of {}: the branch they land on already took every file they change. {}",
                record.agent.label, record.prompt_line
            ),
        )]
    );
}

#[test]
fn a_change_set_the_owner_brought_back_stays_open_though_main_took_it() {
    let (case, id, _) = taken_case();
    take_and_move_on(&case);
    case.observe();
    let closed = case.view(&id);
    assert_eq!(closed.state, ChangeState::Discarded);
    let inside_window = closed.restorable_until_ms.unwrap() - 1;
    restore(&case.db, &id, inside_window).unwrap();
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.restorable_until_ms),
        (ChangeState::Pending, None),
        "{view:?}"
    );
}

#[test]
fn a_change_set_main_took_only_in_part_stays_open_with_its_whole_change() {
    let case = Case::new();
    case.edit("README", "one\nagent\nthree\n");
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    take_and_move_on(&case);
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.files),
        (ChangeState::Pending, 2),
        "{view:?}"
    );
}

#[test]
fn a_change_set_that_conflicts_with_work_main_never_took_stays_open() {
    let case = Case::new();
    case.edit("README", "one\nagent\nthree\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\nowner\nthree\n", "Owner work on the line");
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
}

#[test]
fn a_deletion_main_took_and_moved_on_from_closes_too() {
    let case = Case::new();
    std::fs::remove_file(case.workspace.worktree.join("README")).unwrap();
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner(&["rm", "-q", "README"]);
    case.owner(&["commit", "-q", "-m", "Port the removal"]);
    case.owner_commit("README", "written again\n", "Write it again");
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
}

#[test]
fn a_change_set_main_took_through_a_merge_and_moved_on_from_closes_too() {
    let (case, id, _) = taken_case();
    case.owner(&["switch", "-q", "-c", "port"]);
    case.owner_commit("README", "one\nagent\nthree\n", "Port on a branch");
    case.owner(&["switch", "-q", "main"]);
    case.owner(&["merge", "--no-ff", "-q", "-m", "Merge the port", "port"]);
    case.owner_commit("README", "one\nowner\nthree\n", "Build on the port");
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
}

#[test]
fn a_change_set_only_a_side_branch_held_stays_open() {
    let (case, id, _) = taken_case();
    case.owner(&["switch", "-q", "-c", "side"]);
    case.owner_commit(
        "README",
        "one\nagent\nthree\n",
        "Same content on a side branch",
    );
    case.owner(&["switch", "-q", "main"]);
    case.owner_commit("README", "one\nowner\nthree\n", "Main's own line");
    case.owner(&[
        "merge",
        "-q",
        "-s",
        "ours",
        "-m",
        "Keep main's line",
        "side",
    ]);
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
}

#[test]
fn a_change_set_main_took_and_reverted_stays_open() {
    let (case, id, _) = taken_case();
    case.owner_commit("README", "one\nagent\nthree\n", "Port the agent work");
    case.owner_commit("README", "one\ntwo\nthree\n", "Revert the port");
    case.owner_commit("README", "one\nowner\nthree\n", "Main's own line");
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
}

#[test]
fn restoring_a_set_main_took_gives_back_its_head_its_ref_and_its_claims() {
    let mut case = Case::new();
    closing_tables(&case);
    review_ready_claim(&mut case);
    case.edit("README", "one\nagent\nthree\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    take_and_move_on(&case);
    case.observe();
    let closed = case.view(&id);
    assert_eq!(closed.state, ChangeState::Discarded);
    assert_eq!(second_start(&case), 0, "the closed set holds no claim");
    restore(&case.db, &id, closed.restorable_until_ms.unwrap() - 1).unwrap();
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.head),
        (ChangeState::Pending, head.clone())
    );
    assert_eq!(case.agent_head(), head);
    assert!(case.owner_refs("refs/crazyagent").contains(&head));
    assert_eq!(second_start(&case), 1, "the restored set holds its claim");
}

#[test]
fn a_discard_by_the_master_is_recorded_as_the_master_s() {
    let (case, id, head) = taken_case();
    super::discard_as(&case.db, &id, &head, NOW_MS, crate::activity::MASTER_ACTOR).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
    let actors: Vec<String> = set_rows(&case, &id)
        .into_iter()
        .map(|(actor, _, _)| actor)
        .collect();
    assert_eq!(actors, vec![crate::activity::MASTER_ACTOR.to_owned()]);
}

#[test]
fn a_change_set_whose_worktree_was_deleted_outside_the_app_closes_as_removed_outside() {
    for removal in ["git worktree remove", "rm -rf"] {
        let case = Case::new();
        case.edit("src/new.rs", "fn main() {}\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        case.observe();
        assert_eq!(case.view(&id).state, ChangeState::Pending, "{removal}");
        let worktree = case.workspace.worktree.to_str().unwrap();
        match removal {
            "git worktree remove" => {
                case.owner(&["worktree", "remove", "--force", worktree]);
            }
            _ => std::fs::remove_dir_all(worktree).unwrap(),
        }
        case.observe();
        case.removed_outside(&id, &head);
    }
}

/// A discarded set the owner brings back, and a set closed because its worktree was
/// removed outside the app, each leave a row in the project's activity, named for what
/// happened (F5).
#[test]
fn restoring_a_discarded_set_and_closing_one_removed_outside_the_app_are_recorded() {
    let case = Case::new();
    case.db
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)",
        )
        .unwrap();
    crate::coord::ensure(&case.db).unwrap();
    crate::activity::initialize(&case.db).unwrap();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    std::fs::remove_dir_all(&case.workspace.worktree).unwrap();
    case.observe();
    case.removed_outside(&id, &head);
    let record = super::store::load(&case.db, &id).unwrap();
    let (label, line) = (&record.agent.label, &record.prompt_line);
    let mut rows: Vec<(&str, String)> =
        crate::activity::list(&case.db, &record.project, crate::activity::LIST_MAX)
            .unwrap()
            .into_iter()
            .filter(|item| item.change_set.as_deref() == Some(id.as_str()))
            .map(|item| (item.kind.name(), item.text))
            .collect();
    rows.sort();
    let mut expected = vec![
        (
            "changes_ready",
            format!("Brought back the changes of {label}: {line}"),
        ),
        (
            "discarded",
            format!("Discarded the changes of {label}: {line}"),
        ),
        (
            "discarded",
            format!("The changes of {label} were closed outside the app: {line}"),
        ),
    ];
    expected.sort();
    assert_eq!(rows, expected);
}

#[test]
fn an_own_branch_change_set_whose_branch_was_deleted_outside_the_app_closes_as_removed_outside() {
    let case = Case::own_branch();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    case.agent_commit("Add the fourth line");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    case.agent(&["checkout", "-q", "--detach"]);
    case.agent(&["branch", "-q", "-D", "agent/agent-own"]);
    case.observe();
    case.removed_outside(&id, &head);
}

#[test]
fn a_change_set_whose_commit_is_gone_closes_as_removed_outside() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.observe();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    let object = case
        .repo
        .join(".git/objects")
        .join(&head[..2])
        .join(&head[2..]);
    std::fs::remove_file(&object).unwrap();
    case.observe();
    case.removed_outside(&id, &head);
}

#[test]
fn a_poll_checks_every_open_head_with_one_git_run_for_the_repository() {
    let case = Case::new();
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-1".into(),
        prompt_line: "Write notes".into(),
    };
    case.edit("agent-1.txt", "one\n");
    let mut ids = vec![case.turn(TurnOutcome::Succeeded).unwrap()];
    let open = |number: usize| {
        let agent = numbered_agent(&case, number);
        write(
            &agent.worktree.join(format!("agent-{number}.txt")),
            &format!("{number}\n"),
        );
        record_turn_end(&case.db, &agent, &turn)
            .unwrap()
            .expect("the agent's work opens a change set")
    };
    let runs = || {
        case.observe();
        let before = git::GIT_RUNS.with(std::cell::Cell::get);
        case.observe();
        git::GIT_RUNS.with(std::cell::Cell::get) - before
    };
    ids.push(open(2));
    let two = runs();
    assert!(two >= 1, "the poll checks the heads");
    ids.extend((3..=6).map(open));
    assert_eq!(runs(), two, "six open sets cost the git runs two do");
    let heads: Vec<String> = ids.iter().map(|id| case.view(id).head).collect();
    assert_eq!(
        heads
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );
    assert!(
        ids.iter()
            .all(|id| case.view(id).state == ChangeState::Pending)
    );

    let gone = &heads[3];
    std::fs::remove_file(
        case.repo
            .join(".git/objects")
            .join(&gone[..2])
            .join(&gone[2..]),
    )
    .unwrap();
    case.observe();
    for (id, head) in ids.iter().zip(&heads) {
        let view = case.view(id);
        let expected = if head == gone {
            (ChangeState::Discarded, true)
        } else {
            (ChangeState::Pending, false)
        };
        assert_eq!((view.state, view.outside), expected, "{view:?}");
    }
}

#[test]
fn a_change_set_the_owner_has_not_landed_stays_open_with_its_whole_change() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let before = case.view(&id);
    case.owner_commit("other.txt", "owner work\n", "Owner work");
    case.observe();
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (
            view.state,
            view.outside,
            view.files,
            view.added,
            view.removed
        ),
        (
            ChangeState::Pending,
            false,
            before.files,
            before.added,
            before.removed
        )
    );
    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    assert!(page.text.contains("+four"), "{}", page.text);

    case.owner(&[
        "merge",
        "--no-ff",
        "-q",
        "-m",
        "Merge the agent work",
        &head,
    ]);
    case.owner(&["checkout", "-q", "--detach"]);
    case.observe();
    let view = case.view(&id);
    assert_eq!(
        (view.state, view.outside),
        (ChangeState::Pending, false),
        "a detached owner HEAD judges nothing"
    );
    case.owner(&["checkout", "-q", "main"]);
    case.observe();
    case.landed_outside(&id, &head);
}

#[test]
fn an_import_commit_that_loses_the_branch_race_leaves_only_the_staged_deletion_and_a_retry_lands() {
    let case = Case::new();
    let start = case.owner_commit("CLAUDE.md", "RULE\n", "Rules");
    std::fs::remove_file(case.repo.join("CLAUDE.md")).unwrap();
    let elsewhere = std::cell::RefCell::new(String::new());
    let lost = owner::commit_deletions(
        &case.repo,
        &case.home,
        &["CLAUDE.md"],
        "Move CLAUDE.md\n",
        &|| {
            let moved = case
                .owner(&[
                    "commit-tree",
                    "main^{tree}",
                    "-p",
                    "main",
                    "-m",
                    "Elsewhere",
                ])
                .trim()
                .to_owned();
            case.owner(&["update-ref", "refs/heads/main", &moved]);
            *elsewhere.borrow_mut() = moved;
        },
    );
    assert_eq!(refused(lost), "changes_branch_moved");
    let elsewhere = elsewhere.into_inner();
    assert_eq!(case.tip(), elsewhere);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &elsewhere]).trim(),
        start
    );
    assert_eq!(
        case.owner(&["status", "--porcelain"]),
        "D  CLAUDE.md\n",
        "only the deletion is staged; the owner's files are untouched"
    );
    let (commit, branch, files) = owner::commit_deletions(
        &case.repo,
        &case.home,
        &["CLAUDE.md"],
        "Move CLAUDE.md\n",
        &|| {},
    )
    .unwrap();
    assert_eq!(
        (branch.as_str(), files),
        ("main", vec!["CLAUDE.md".to_owned()])
    );
    assert_eq!(case.tip(), commit);
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%an <%ae>|%s", &commit])
            .trim(),
        format!("{elsewhere}|Repo Owner <owner@example.invalid>|Move CLAUDE.md")
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
}

#[test]
fn a_checkout_dirty_only_by_the_instruction_import_refuses_accept_until_the_move_is_committed() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("CLAUDE.md", "RULE\n", "Rules");
    let moved = crate::instruction_files::import(&case.repo).unwrap();
    assert_eq!(
        moved
            .iter()
            .map(|file| (file.path.as_str(), file.tracked, file.uncommitted))
            .collect::<Vec<_>>(),
        [("CLAUDE.md", true, true)]
    );
    let head = case.view(&id).head;
    let before = case.tip();
    assert_eq!(
        refused(accept_into(
            &case.db,
            &case.home,
            &id,
            &head,
            None,
            "main",
            crate::activity::AcceptPlace::Changes
        )),
        "changes_owner_dirty"
    );
    assert_eq!(case.tip(), before, "a refused accept moves no branch");
    assert_eq!(
        crate::instruction_files::uncommitted(&case.repo).unwrap(),
        ["CLAUDE.md"]
    );
    let made = crate::instruction_files::commit(&case.repo, &case.home).unwrap();
    assert_eq!(made.files, ["CLAUDE.md"]);
    assert_eq!(
        (made.branch.as_str(), case.tip()),
        ("main", made.commit.clone())
    );
    assert!(
        crate::instruction_files::uncommitted(&case.repo)
            .unwrap()
            .is_empty()
    );
    let AcceptOutcome::Landed { commit, branch } = accept_into(
        &case.db,
        &case.home,
        &id,
        &head,
        None,
        "main",
        crate::activity::AcceptPlace::Changes,
    )
    .unwrap() else {
        panic!("the accept conflicted");
    };
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P", &commit]).trim(),
        made.commit
    );
    assert_eq!(
        case.owner(&["ls-tree", "-r", "--name-only", &commit]),
        "README\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read_to_string(case.repo.join(".crazyagent/imported/CLAUDE.md")).unwrap(),
        "RULE\n"
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
/// An Own branch clone's discarded change set comes back only while the clone's branch
/// holds exactly the discarded head: commits made since, or a reset of the branch below
/// it, keep it discarded with `changes_agent_busy`, so a later turn end cannot quietly
/// replace the restored head with a tip that lacks it (GA-R4).
#[test]
fn an_own_branch_restore_needs_the_clone_branch_at_the_discarded_head() {
    let case = Case::own_branch();
    let base = case.tip();
    case.edit("src/new.rs", "fn main() {}\n");
    let head = case.agent_commit("Add main");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(case.view(&id).head, head);
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
    assert_eq!(
        case.agent_head(),
        head,
        "a discard leaves the clone's branch as it was"
    );

    case.edit("later.txt", "more work\n");
    case.agent_commit("More work");
    assert_eq!(
        refused(restore(&case.db, &id, NOW_MS + DAY_MS)),
        "changes_agent_busy"
    );
    assert_eq!(case.view(&id).state, ChangeState::Discarded);

    case.agent(&["reset", "-q", "--hard", &base]);
    assert_eq!(
        refused(restore(&case.db, &id, NOW_MS + DAY_MS)),
        "changes_agent_busy"
    );
    assert_eq!(case.view(&id).state, ChangeState::Discarded);

    case.agent(&["reset", "-q", "--hard", &head]);
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(
        case.owner(&[
            "rev-parse",
            &format!("refs/crazyagent/pending/{}", id.as_str())
        ])
        .trim(),
        head
    );
    assert_eq!(case.turn(TurnOutcome::Succeeded), Some(id.clone()));
    assert_eq!(
        case.view(&id).head,
        head,
        "the restored head stays the set's"
    );
}

/// The owner repository keeps the commit an Own branch clone borrows as its base under an
/// engine ref that follows the base, so the owner rewriting their branch and pruning
/// leaves the clone's history readable (GA-R5).
#[test]
fn the_owner_repository_keeps_an_own_branch_clone_s_base_through_a_rewrite_and_prune() {
    let case = Case::own_branch();
    let base = case.tip();
    let linked = case.repo.parent().unwrap().join("agent-1");
    case.owner(&["worktree", "remove", "--force", linked.to_str().unwrap()]);
    case.owner(&["commit", "-q", "--amend", "-m", "Start, reworded"]);
    assert_ne!(case.tip(), base);
    case.owner(&["reflog", "expire", "--expire=now", "--all"]);
    case.owner(&["gc", "-q", "--prune=now"]);
    assert!(
        case.owner_has(&base),
        "the clone's base survived the owner's prune"
    );
    assert_eq!(case.agent_head(), base);
    case.agent(&["log", "-q", "--format=%H"]);

    case.edit("src/new.rs", "fn main() {}\n");
    let head = case.agent_commit("Add main");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(
        case.owner(&["rev-parse", "refs/crazyagent/base/agent-own"])
            .trim(),
        head,
        "the base ref follows the clone's base"
    );
}

#[test]
fn files_an_instruction_file_imports_or_links_to_are_marked() {
    let case = Case::new();
    let fed = [
        "docs/conventions.md",
        "style/guide.md",
        "rules/extra.md",
        "docs/agents-target.md",
        "docs/other.md",
        "config/codex/config.toml",
    ];
    case.edit(
        "CLAUDE.md",
        "Read @docs/conventions.md and @./style/Guide.md, then the rest.\nNot these: @~/private.md @/etc/hosts @../outside.md\n",
    );
    case.edit(".claude/CLAUDE.md", "Also @../rules/extra.md\n");
    for path in fed {
        case.edit(path, "Old.\n");
    }
    std::os::unix::fs::symlink(
        "docs/agents-target.md",
        case.workspace.worktree.join("AGENTS.md"),
    )
    .unwrap();
    std::os::unix::fs::symlink("config/codex", case.workspace.worktree.join(".codex")).unwrap();
    case.agent_commit("Instructions that pull in other files");
    for path in fed {
        case.edit(path, "New.\n");
    }
    case.edit(
        "CLAUDE.md",
        "Read @docs/conventions.md and @./style/Guide.md, then the rest.\nNow also @docs/fresh.md\n",
    );
    case.edit("docs/fresh.md", "Fresh.\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();

    let page = diff(&case.db, &id, &case.view(&id).head, 0).unwrap();
    assert_eq!(page.next, None);
    let marked: Vec<(&str, bool)> = page
        .files
        .iter()
        .map(|file| {
            (
                file.path.as_str(),
                file.marks.contains(&FileMark::Instructions),
            )
        })
        .collect();
    assert_eq!(
        marked,
        [
            (".claude/CLAUDE.md", true),
            (".codex", true),
            ("AGENTS.md", true),
            ("CLAUDE.md", true),
            ("config/codex/config.toml", true),
            ("docs/agents-target.md", true),
            ("docs/conventions.md", true),
            ("docs/fresh.md", true),
            ("docs/other.md", false),
            ("rules/extra.md", true),
            ("style/guide.md", true),
        ]
    );
}

/// Seeds the coordination tables for OS-8: the case's sub-agent becomes engine agent 1 with
/// a claim on README for work it finished as review-ready, beside a second agent 2 and the
/// project's master, agent 3. Input: the case. Output: the review-ready work.
fn review_ready_claim(case: &mut Case) -> crate::coord::WorkId {
    case.workspace.agent.id = "1".into();
    crate::team::initialize(&case.db).unwrap();
    crate::coord::ensure(&case.db).unwrap();
    case.db
        .execute_batch(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (1,'project-1','sub','claude-code',1,'working',1,1),
                    (2,'project-1','sub','claude-code',1,'working',1,1),
                    (3,'project-1','master','claude-code',1,'working',1,1);",
        )
        .unwrap();
    let now = crate::team::now_ms();
    let (work, _) = crate::coord::start_work(
        &case.db,
        &case.workspace.project,
        1,
        "Readme".into(),
        vec!["README".into()],
        now,
    )
    .unwrap();
    crate::coord::finish(
        &case.db,
        work,
        1,
        crate::coord::Finish::ReviewReady,
        "Green.".into(),
        1,
        now,
    )
    .unwrap();
    work
}

/// Starts agent 2 on README as the next agent would. Input: the case. Output: how many
/// claims of other agents its start overlaps.
fn second_start(case: &Case) -> usize {
    start_on(case, 2, "README")
}

/// Starts one agent on one file. Inputs: the case, the agent and the file. Output: how many
/// claims of other agents its start overlaps.
fn start_on(case: &Case, agent: i64, file: &str) -> usize {
    crate::coord::start_work(
        &case.db,
        &case.workspace.project,
        agent,
        "Docs".into(),
        vec![file.into()],
        crate::team::now_ms(),
    )
    .unwrap()
    .1
    .len()
}

/// Has agent 1 start one unit of work on a file and finish it as review-ready, as a
/// sub-agent does during a turn. Inputs: the case and the file. Output: the work.
fn finished_on(case: &Case, file: &str) -> crate::coord::WorkId {
    let now = crate::team::now_ms();
    let (work, _) = crate::coord::start_work(
        &case.db,
        &case.workspace.project,
        1,
        "More".into(),
        vec![file.into()],
        now,
    )
    .unwrap();
    crate::coord::finish(
        &case.db,
        work,
        1,
        crate::coord::Finish::ReviewReady,
        "Green.".into(),
        1,
        now,
    )
    .unwrap();
    work
}

/// OS-8 M1: the master's review that completes, or closes, a sub-agent's review-ready work
/// keeps its claims while the sub-agent's change set waits; only the set's Accept, or its
/// discard, releases them.
#[test]
fn a_master_review_of_a_sub_s_work_keeps_its_claims_until_the_set_closes() {
    for outcome in [crate::coord::Finish::Complete, crate::coord::Finish::Closed] {
        let mut case = Case::new();
        let work = review_ready_claim(&mut case);
        case.edit("README", "one\ntwo\nthree\nfour\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        crate::coord::finish(
            &case.db,
            work,
            2,
            outcome,
            "Reviewed.".into(),
            3,
            crate::team::now_ms(),
        )
        .unwrap();
        assert_eq!(second_start(&case), 1, "{outcome:?}: the set still waits");
        match outcome {
            crate::coord::Finish::Complete => assert!(matches!(
                case.accept(&id).unwrap(),
                AcceptOutcome::Landed { .. }
            )),
            _ => {
                discard(&case.db, &id, &case.view(&id).head, NOW_MS).unwrap();
            }
        }
        assert_eq!(second_start(&case), 0, "{outcome:?}: the set closed");
    }
}

/// Registers agent 2 and the project's master, agent 3, in the team tables and returns the
/// master's workspace. Input: the case. Output: the workspace.
fn team_with_master(case: &Case) -> Workspace {
    let master = master_agent(case);
    crate::team::initialize(&case.db).unwrap();
    crate::coord::ensure(&case.db).unwrap();
    case.db
        .execute_batch(
            "INSERT INTO team_agents(id,project,role,harness,tools,status,created_ms,heartbeat_ms)
             VALUES (2,'project-1','sub','claude-code',1,'working',1,1),
                    (3,'project-1','master','claude-code',1,'working',1,1);",
        )
        .unwrap();
    master
}

/// Has the master start and finish work on README, so it holds that claim on ended work.
/// Input: the case. Output: none.
fn master_finishes_readme(case: &Case) {
    let now = crate::team::now_ms();
    let (work, _) = crate::coord::start_work(
        &case.db,
        &case.workspace.project,
        3,
        "Readme".into(),
        vec!["README".into()],
        now,
    )
    .unwrap();
    crate::coord::finish(
        &case.db,
        work,
        1,
        crate::coord::Finish::Complete,
        "Done.".into(),
        3,
        now,
    )
    .unwrap();
}

/// T5-R2-L1: the master's finished work holds its claims while the change set carrying it
/// waits, as a sub-agent's does, and that set's Accept releases them.
#[test]
fn a_master_s_finished_work_keeps_its_claims_until_its_set_lands() {
    let case = Case::new();
    let master = team_with_master(&case);
    master_finishes_readme(&case);
    write(&master.worktree.join("README"), "one\ntwo\nthree\nfour\n");
    let id = turn_of(&case, &master, "Add the line");
    assert_eq!(second_start(&case), 1, "the master's set still waits");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(second_start(&case), 0, "the master's set landed");
}

/// T5-R2-L1: a master's turn that leaves nothing to land releases its finished claims, so
/// they never hold forever.
#[test]
fn a_master_s_finished_work_is_released_by_a_turn_end_with_nothing_to_land() {
    let case = Case::new();
    let master = team_with_master(&case);
    master_finishes_readme(&case);
    assert_eq!(second_start(&case), 1, "the turn has not ended yet");
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-master".into(),
        prompt_line: "Nothing to change".into(),
    };
    assert_eq!(record_turn_end(&case.db, &master, &turn).unwrap(), None);
    assert_eq!(second_start(&case), 0, "nothing waits to land");
}

/// T5-R2-L1: a stopped master has no more turn ends, so the sweep holds the claims of work
/// it finished for its open set, and that set's Accept releases them.
#[test]
fn a_stopped_master_s_finished_work_is_held_for_its_open_set_by_the_sweep() {
    let case = Case::new();
    let master = team_with_master(&case);
    write(&master.worktree.join("README"), "one\ntwo\nthree\nfour\n");
    let id = turn_of(&case, &master, "Add the line");
    master_finishes_readme(&case);
    case.db
        .execute("UPDATE team_agents SET status='stopped' WHERE id=3", [])
        .unwrap();
    sweep_claims(&case.db, &case.workspace.project, NOW_MS).unwrap();
    assert_eq!(second_start(&case), 1, "the master's set still waits");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(second_start(&case), 0, "the master's set landed");
}

/// Land-7 r2: a team master's set is recorded as a sub-agent's with its row id, so the sweep
/// finds it by that id and holds the work the master finished before it stopped.
#[test]
fn a_stopped_team_master_s_finished_work_is_held_for_its_open_set_by_the_sweep() {
    let case = Case::new();
    let mut master = team_with_master(&case);
    master.agent.kind = AgentKind::Sub;
    master.agent.id = "3".into();
    write(&master.worktree.join("README"), "one\ntwo\nthree\nfour\n");
    let id = turn_of(&case, &master, "Add the line");
    master_finishes_readme(&case);
    case.db
        .execute("UPDATE team_agents SET status='stopped' WHERE id=3", [])
        .unwrap();
    sweep_claims(&case.db, &case.workspace.project, NOW_MS).unwrap();
    assert_eq!(second_start(&case), 1, "the master's set still waits");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(second_start(&case), 0, "the master's set landed");
}

/// Land-7 r2: with two open master sets the sweep holds a stopped master's finished work for
/// the newest, so closing the older one does not end the hold.
#[test]
fn a_stopped_master_s_finished_work_is_held_for_its_newest_open_set() {
    let case = Case::new();
    let master = team_with_master(&case);
    case.start();
    case.edit("OLDER", "older\n");
    let older = case.turn(TurnOutcome::Succeeded).unwrap();
    let mut record = store::load(&case.db, &older).unwrap();
    record.agent.kind = AgentKind::Master;
    store::save(&case.db, &record).unwrap();
    write(&master.worktree.join("README"), "one\ntwo\nthree\nfour\n");
    let newer = turn_of(&case, &master, "Add the line");
    master_finishes_readme(&case);
    case.db
        .execute("UPDATE team_agents SET status='stopped' WHERE id=3", [])
        .unwrap();
    sweep_claims(&case.db, &case.workspace.project, NOW_MS).unwrap();
    assert_eq!(
        crate::coord::held_sets(&case.db, 3).unwrap(),
        vec![newer.as_str().to_owned()]
    );
}

/// OS-8 L1: closing a change set releases only the claims of the work it holds. Work the
/// agent finished after the set's last turn end keeps its claims through that set's Accept
/// and is released by the Accept of the next set, which holds it.
#[test]
fn landing_a_set_releases_only_the_claims_of_work_it_holds() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    finished_on(&case, "src/two.rs");
    assert!(matches!(
        case.accept(&first).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        second_start(&case),
        0,
        "the landed set's own claim is released"
    );
    assert_eq!(
        start_on(&case, 2, "src/two.rs"),
        1,
        "work finished after the landed set's turn end still waits"
    );
    case.edit("src/two.rs", "two\n");
    let next = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_ne!(next, first);
    assert!(matches!(
        case.accept(&next).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        start_on(&case, 2, "src/two.rs"),
        0,
        "the next set landed it"
    );
}

/// OS-8 L2: review-ready work whose agent left nothing to land has no change set to wait
/// for, so its claims are released at the turn end that records nothing.
#[test]
fn review_ready_work_with_nothing_to_land_releases_its_claims_at_the_turn_end() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    assert_eq!(second_start(&case), 1, "the turn has not ended yet");
    assert_eq!(case.turn(TurnOutcome::Succeeded), None);
    assert_eq!(second_start(&case), 0, "nothing waits to land");
}

/// OS-8 L2: a claim release that fails when a set closes keeps the claim, never frees it
/// early, and is tried again at the agent's next turn end, so the claim is not held forever.
#[test]
fn a_failed_claim_release_is_tried_again_at_the_agent_s_next_turn_end() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER claims_stuck BEFORE UPDATE ON coord_claims
             BEGIN SELECT RAISE(ABORT, 'claims stuck'); END;",
        )
        .unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    case.db.execute_batch("DROP TRIGGER claims_stuck;").unwrap();
    assert_eq!(second_start(&case), 1, "the failed release kept the claim");
    case.edit("src/next.rs", "next\n");
    let next = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_ne!(next, id);
    assert_eq!(
        second_start(&case),
        0,
        "the next turn end released the landed set's claim"
    );
}

/// OS-8 L3: restoring a discarded sub-agent set brings its review-ready work's claims back,
/// so a start made after the discard on the same file now shows the overlap.
#[test]
fn restoring_a_discarded_sub_set_holds_its_claims_again() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    discard(&case.db, &id, &case.view(&id).head, NOW_MS).unwrap();
    assert_eq!(
        second_start(&case),
        0,
        "the discarded set released its claim"
    );
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(
        second_start(&case),
        1,
        "the restored set's claim overlaps the start made after the discard"
    );
}

/// Marks the case's sub-agent, engine agent 1, stopped, as the runtime does when it stops
/// or the engine starts. Input: the case.
fn stop_agent_1(case: &Case) {
    case.db
        .execute("UPDATE team_agents SET status='stopped' WHERE id=1", [])
        .unwrap();
}

/// Runs the engine's claims sweep for the case's project. Input: the case.
fn sweep(case: &Case) {
    sweep_claims(&case.db, &case.workspace.project, NOW_MS + 2 * DAY_MS).unwrap();
}

/// OS-8 r2 L2: a sub-agent that finished its work and stopped before any turn end never
/// runs again; with nothing to land, the engine's sweep releases its claims.
#[test]
fn a_stopped_sub_agent_s_finished_claims_are_released_by_the_sweep_when_nothing_waits() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    stop_agent_1(&case);
    assert_eq!(
        second_start(&case),
        1,
        "finished work still holds its claim"
    );
    sweep(&case);
    assert_eq!(
        second_start(&case),
        0,
        "nothing waits to land, so the sweep released it"
    );
}

/// OS-8 r2 L2: a sub-agent that finished its work after its set's last turn end and then
/// stopped keeps its claims for that open set: the sweep holds them for it, and the set's
/// Accept releases them.
#[test]
fn a_stopped_sub_agent_s_finished_claims_are_held_for_its_open_set_by_the_sweep() {
    let mut case = Case::new();
    let work = review_ready_claim(&mut case);
    crate::coord::transfer(
        &case.db,
        work,
        2,
        crate::coord::Transfer::Reopen,
        crate::coord::Party::Agent(1),
        crate::team::now_ms(),
    )
    .unwrap();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    crate::coord::finish(
        &case.db,
        work,
        3,
        crate::coord::Finish::ReviewReady,
        "Green again.".into(),
        1,
        crate::team::now_ms(),
    )
    .unwrap();
    stop_agent_1(&case);
    sweep(&case);
    assert_eq!(second_start(&case), 1, "the open set still waits");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        second_start(&case),
        0,
        "the set's Accept released the claim it held"
    );
}

/// OS-8 r2 L2: a claim release that failed when the set landed is retried by the engine's
/// sweep, so an agent that never has another turn end does not keep the claim.
#[test]
fn a_landed_set_s_failed_release_is_retried_by_the_sweep_after_its_agent_stopped() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER claims_stuck BEFORE UPDATE ON coord_claims
             BEGIN SELECT RAISE(ABORT, 'claims stuck'); END;",
        )
        .unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    case.db.execute_batch("DROP TRIGGER claims_stuck;").unwrap();
    stop_agent_1(&case);
    assert_eq!(second_start(&case), 1, "the failed release kept the claim");
    sweep(&case);
    assert_eq!(
        second_start(&case),
        0,
        "the sweep released the landed set's claim"
    );
}

/// OS-8 r2 L2: a restore whose claims could not be taken back is repaired by the engine's
/// sweep, so the restored set's claims overlap again.
#[test]
fn a_restore_whose_claims_failed_to_return_is_repaired_by_the_sweep() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    discard(&case.db, &id, &case.view(&id).head, NOW_MS).unwrap();
    case.db
        .execute_batch(
            "CREATE TEMP TRIGGER claims_stuck BEFORE UPDATE ON coord_claims
             BEGIN SELECT RAISE(ABORT, 'claims stuck'); END;",
        )
        .unwrap();
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    case.db.execute_batch("DROP TRIGGER claims_stuck;").unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(
        second_start(&case),
        0,
        "the failed restore left the claim released"
    );
    sweep(&case);
    assert_eq!(
        second_start(&case),
        1,
        "the sweep took the restored set's claim back"
    );
}

/// OS-8 L4: a sub-agent set whose worktree was deleted outside the app closes as removed
/// outside and releases its review-ready work's claims.
#[test]
fn a_sub_set_removed_outside_the_app_releases_its_claims() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    assert_eq!(second_start(&case), 1, "the set still waits");
    std::fs::remove_dir_all(&case.workspace.worktree).unwrap();
    case.observe();
    case.removed_outside(&id, &head);
    assert_eq!(
        second_start(&case),
        0,
        "the removed set's claim is released"
    );
}

/// OS-8: a sub-agent's claim on work it finished as review-ready holds while its change set
/// waits, so the next agent starting on the same file hears the overlap; accepting the set
/// lands the work and releases the claim, and the same start then overlaps nothing.
#[test]
fn accepting_a_sub_set_releases_its_agents_review_ready_claims() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(second_start(&case), 1, "the set has not landed yet");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(second_start(&case), 0, "the landed set's claim is released");
}

/// OS-8, DEC-117 Q7: rejecting a sub-agent's change set frees the files its review-ready
/// work claimed, so the next agent's start on them gets no overlap note.
#[test]
fn discarding_a_sub_set_releases_its_agents_review_ready_claims() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(second_start(&case), 1, "the set has not been rejected yet");
    discard(&case.db, &id, &case.view(&id).head, NOW_MS).unwrap();
    assert_eq!(
        second_start(&case),
        0,
        "the rejected set's claim is released"
    );
}

/// OS-8: the claim release runs only once a change set closed; called on a set still open,
/// it stops at its assertion, so no claim is freed while that set's edits wait for review.
#[test]
#[should_panic(expected = "!record.is_open()")]
fn releasing_claims_for_an_open_set_trips_the_assertion() {
    let mut case = Case::new();
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &id).unwrap();
    super::flow::release_agent_claims(&case.db, &record);
}

/// Gives the project the access level and master landing choice named, in a case whose
/// master is at work, so the engine's tables are there. Inputs: the case, the access and
/// the choice.
fn master_may_land(
    case: &Case,
    access: crate::sandbox::policy::AgentAccess,
    lands: crate::team::MasterLands,
) {
    let policy = crate::sandbox::policy::AgentPolicy {
        access,
        ..crate::sandbox::policy::AgentPolicy::default()
    };
    crate::sandbox::policy::save(&case.db, case.workspace.project.as_str(), &policy).unwrap();
    crate::team::set_master_lands(&case.db, &case.workspace.project, lands).unwrap();
}

/// Adds the project's master on the worktree the engine leases it, its turn start
/// recorded. Input: the case. Output: the master's workspace.
fn master_at_work(case: &Case) -> Workspace {
    let master = master_agent(case);
    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    master
}

/// Ends a turn of an agent other than the case's own. Inputs: the case, the agent and the
/// prompt line. Output: its change set.
fn turn_of(case: &Case, workspace: &Workspace, prompt: &str) -> ChangeSetId {
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: format!("turn-{}", workspace.agent.id),
        prompt_line: prompt.into(),
    };
    record_turn_end(&case.db, workspace, &turn)
        .unwrap()
        .unwrap()
}

/// The project's activity rows about a change set. Inputs: the case and the set. Output:
/// the rows, newest first.
fn activity_of(case: &Case, id: &ChangeSetId) -> Vec<crate::activity::ActivityItem> {
    crate::activity::list(&case.db, &case.workspace.project, 50)
        .unwrap()
        .into_iter()
        .filter(|item| item.change_set.as_deref() == Some(id.as_str()))
        .collect()
}

/// Makes the HackerBlog shape: the case's agent's set conflicts with the owner's fourth
/// line on main, then the master, at Full access with Merge into main itself, merges that
/// set's head and keeps both lines in its own turn. Input: the case. Output: the conflicted
/// set, the master's fix and the README the fix holds.
fn master_fixing_a_conflict(case: &Case) -> (ChangeSetId, ChangeSetId, &'static str) {
    let (sub, sub_head, master) = conflicted_set_and_master(case);
    master_resolves(case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let fix = turn_of(case, &master, "Resolve the README conflict");
    (sub, fix, BOTH_FOURTH_LINES)
}

/// The README a master's fix of the fourth-line conflict holds.
const BOTH_FOURTH_LINES: &str = "one\ntwo\nthree\nfour from the owner\nfour from the agent\n";

/// Makes the case's agent's set conflict with the owner's fourth line on main and brings
/// in the master, at Full access with Merge into main itself. Input: the case. Output: the
/// conflicted set, its head and the master's workspace.
fn conflicted_set_and_master(case: &Case) -> (ChangeSetId, String, Workspace) {
    conflicted_set_from(case, &[], |case| {
        case.owner_commit(
            "README",
            "one\ntwo\nthree\nfour from the owner\n",
            "Owner adds four",
        );
    })
}

/// Makes a set of the agent's fourth line and the given files conflict with what `on_main`
/// commits to main, then brings in the master at Full access with Merge into main itself.
/// Inputs: the case, the set's other files and the change on main. Output: the conflicted
/// set, its head and the master's workspace.
fn conflicted_set_from(
    case: &Case,
    files: &[(&str, &str)],
    on_main: impl FnOnce(&Case),
) -> (ChangeSetId, String, Workspace) {
    conflicted_set_changing(
        case,
        |case| {
            case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
            for (path, text) in files {
                case.edit(path, text);
            }
        },
        on_main,
    )
}

/// Makes a set of whatever `in_set` changes conflict with what `on_main` commits to main,
/// then brings in the master at Full access with Merge into main itself. Inputs: the case
/// and both changes. Output: the conflicted set, its head and the master's workspace.
fn conflicted_set_changing(
    case: &Case,
    in_set: impl FnOnce(&Case),
    on_main: impl FnOnce(&Case),
) -> (ChangeSetId, String, Workspace) {
    use crate::sandbox::policy::AgentAccess;
    case.start();
    in_set(case);
    let sub = case.turn(TurnOutcome::Succeeded).unwrap();
    on_main(case);
    assert!(matches!(
        case.accept(&sub).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    let master = master_at_work(case);
    master_may_land(case, AgentAccess::Full, crate::team::MasterLands::Merges);
    let sub_head = case.view(&sub).head;
    (sub, sub_head, master)
}

/// Ends a merge in the master's worktree the way a resolved merge ends: the merge is left
/// open, the files are written over it and it is committed as the master. Inputs: the
/// case, the master's workspace, the head it merges and the files it writes.
fn master_resolves(case: &Case, master: &Workspace, head: &str, files: &[(&str, &str)]) {
    let in_master = |args: &[&str]| git_in(&case.home, &master.worktree, args);
    in_master(&["merge", "--no-commit", "-s", "ours", head]);
    for (path, text) in files {
        write(&master.worktree.join(path), text);
        in_master(&["add", path]);
    }
    in_master(&[
        "-c",
        "user.name=Master",
        "-c",
        "user.email=master@example.invalid",
        "commit",
        "-q",
        "-m",
        "Take both fourth lines",
    ]);
}

/// Shows a master's set waits for the owner: nothing lands, it stays pending, the other
/// set keeps its state, main does not move and no merge row is written. Inputs: the case,
/// the master's set, the other set, its state and what the row is about.
fn master_set_waits(
    case: &Case,
    fix: &ChangeSetId,
    other: &ChangeSetId,
    state: ChangeState,
    about: &str,
) {
    let tip = case.tip();
    assert_eq!(
        land_master_set(&case.db, &case.home, fix).unwrap(),
        None,
        "{about}: the set waits"
    );
    assert_eq!(case.view(fix).state, ChangeState::Pending, "{about}");
    assert_eq!(case.view(other).state, state, "{about}");
    assert_eq!(case.tip(), tip, "{about}: main does not move");
    assert!(
        activity_of(case, fix)
            .iter()
            .all(|row| row.kind != crate::activity::ActivityKind::MergedIntoMain),
        "{about}: nothing merged, so no merge row"
    );
}

/// With Full access and Merges saved, a master fix never lands itself while the project's
/// agent access cannot be read: land time reads it again and refuses, and main and both
/// sets stay as they were (FBT3-S1, FB-T3 r1 L2).
#[test]
fn a_master_fix_never_lands_itself_while_agent_access_cannot_be_read() {
    let case = Case::new();
    let (sub, fix, _) = master_fixing_a_conflict(&case);
    let tip = case.tip();
    case.db
        .execute(
            "UPDATE project_agent_policy SET body='{' WHERE project=?1",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    assert!(land_master_set(&case.db, &case.home, &fix).is_err());
    assert_eq!(case.tip(), tip, "main did not move");
    assert_eq!(case.view(&fix).state, ChangeState::Pending);
    assert_eq!(case.view(&sub).state, ChangeState::Conflict);
}

#[test]
fn a_master_set_that_fixes_a_conflicted_set_lands_itself_and_closes_it_when_master_lands_is_merges()
{
    use crate::activity::ActivityKind;
    let case = Case::new();
    let (sub, fix, resolved) = master_fixing_a_conflict(&case);
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(&other.worktree.join("NOTES"), "unrelated\n");
    let unrelated = turn_of(&case, &other, "Write notes");

    assert_eq!(
        land_master_set(&case.db, &case.home, &unrelated).unwrap(),
        None,
        "only the master's own set lands itself"
    );
    assert_eq!(case.view(&unrelated).state, ChangeState::Pending);
    let outcome = land_master_set(&case.db, &case.home, &fix).unwrap();
    let Some(AcceptOutcome::Landed { commit, branch }) = outcome else {
        panic!("the master's fix lands: {outcome:?}");
    };
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(case.owner(&["show", "main:README"]), resolved);
    assert_eq!(case.view(&fix).state, ChangeState::Accepted);
    assert_eq!(
        case.view(&sub).state,
        ChangeState::Accepted,
        "the fixed set closes"
    );
    assert_eq!(
        store::load(&case.db, &sub).unwrap().landed.as_deref(),
        Some(commit.as_str()),
        "the fixed set landed with the master's commit"
    );
    assert_eq!(
        case.view(&unrelated).state,
        ChangeState::Pending,
        "a set the master's head does not hold stays open"
    );
    let rows = activity_of(&case, &fix);
    assert_eq!(rows.len(), 1, "one activity row for the land: {rows:?}");
    assert_eq!(rows[0].kind, ActivityKind::MergedIntoMain);
    assert_eq!(rows[0].actor, "Master");
    assert!(
        rows[0].text.contains("Agent 1") && rows[0].text.contains("main"),
        "the row names the set it closed and where it landed: {}",
        rows[0].text
    );
    let number = case.view(&fix).number.unwrap();
    assert!(
        rows[0]
            .text
            .starts_with(&format!("Master merged change set #{number} (Master: ")),
        "AL2: the row names the master's set by its number: {}",
        rows[0].text
    );
}

/// AL2 (rv-sec F1): the master's merge row gives the branch its set landed on as its own
/// field, the ref an Undo reverts on, so the desktop never reads it from the row's words.
#[test]
fn a_master_merge_row_gives_the_branch_its_set_landed_on() {
    let case = Case::new();
    let (_, fix, _) = master_fixing_a_conflict(&case);
    let outcome = land_master_set(&case.db, &case.home, &fix).unwrap();
    assert!(
        matches!(&outcome, Some(AcceptOutcome::Landed { branch, .. }) if branch == "main"),
        "the master's fix lands on main: {outcome:?}"
    );
    let merged = activity_of(&case, &fix);
    assert_eq!(merged.len(), 1, "one merge row: {merged:?}");
    assert_eq!(
        serde_json::to_value(&merged[0]).unwrap()["branch"],
        serde_json::json!("main"),
        "the merge row gives its branch as data"
    );
}

#[test]
fn an_ordinary_master_set_waits_for_the_owner_under_merges() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
    write(&master.worktree.join("README"), "one\ntwo\nthree\nfour\n");
    let edit = turn_of(&case, &master, "Add four");
    let tip = case.tip();

    assert_eq!(
        land_master_set(&case.db, &case.home, &edit).unwrap(),
        None,
        "a master set that closes no other set waits for the owner's Accept"
    );
    assert_eq!(case.view(&edit).state, ChangeState::Pending);
    assert_eq!(case.tip(), tip, "main does not move");
    assert!(
        activity_of(&case, &edit).is_empty(),
        "nothing merged, so no activity row"
    );
}

/// MASTER-LAND-SPLIT: an ordinary edit from an earlier turn never lands unaccepted, so a
/// later turn that merges in another open set leaves the whole master set waiting.
#[test]
fn a_later_turn_merging_in_another_set_leaves_the_earlier_turns_edit_waiting() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
    let sub = case.turn(TurnOutcome::Succeeded).unwrap();
    let sub_head = case.view(&sub).head;
    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
    write(&master.worktree.join("NOTES"), "from the earlier turn\n");
    let earlier = turn_of(&case, &master, "Write notes");
    let tip = case.tip();

    assert_eq!(
        land_master_set(&case.db, &case.home, &earlier).unwrap(),
        None,
        "the ordinary edit waits for the owner's Accept"
    );
    assert_eq!(case.tip(), tip, "main does not move for the ordinary edit");

    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    git_in(
        &case.home,
        &master.worktree,
        &[
            "-c",
            "user.name=Master",
            "-c",
            "user.email=master@example.invalid",
            "merge",
            "-q",
            "--no-edit",
            &sub_head,
        ],
    );
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-master-later".into(),
        prompt_line: "Merge the agent's work".into(),
    };
    let later = record_turn_end(&case.db, &master, &turn).unwrap().unwrap();
    assert_eq!(later, earlier, "the later turn joins the master's open set");

    master_set_waits(
        &case,
        &later,
        &sub,
        ChangeState::Pending,
        "an earlier turn's edit shares the set",
    );
}

/// MASTER-LAND-SPLIT: holding another open set's head is not enough. A clean merge, or a
/// fast-forward onto that head, resolves no conflict, so the set waits.
#[test]
fn a_master_set_bringing_in_another_set_with_no_conflict_waits_for_the_owner() {
    use crate::sandbox::policy::AgentAccess;
    for how in ["--no-ff", "--ff-only"] {
        let case = Case::new();
        case.start();
        case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
        let sub = case.turn(TurnOutcome::Succeeded).unwrap();
        let sub_head = case.view(&sub).head;
        let master = master_at_work(&case);
        master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
        git_in(
            &case.home,
            &master.worktree,
            &[
                "-c",
                "user.name=Master",
                "-c",
                "user.email=master@example.invalid",
                "merge",
                "-q",
                "--no-edit",
                how,
                &sub_head,
            ],
        );
        let fix = turn_of(&case, &master, "Merge the agent's work");
        master_set_waits(&case, &fix, &sub, ChangeState::Pending, how);
    }
}

/// MASTER-LAND-SPLIT: a real conflict fix in a later turn does not carry an ordinary edit
/// from an earlier turn of the same set onto main.
#[test]
fn a_conflict_fix_in_a_later_turn_leaves_the_earlier_turns_edit_waiting() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    write(&master.worktree.join("NOTES"), "from the earlier turn\n");
    let earlier = turn_of(&case, &master, "Write notes");
    master_set_waits(
        &case,
        &earlier,
        &sub,
        ChangeState::Conflict,
        "the ordinary edit alone",
    );

    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-master-later".into(),
        prompt_line: "Resolve the README conflict".into(),
    };
    let later = record_turn_end(&case.db, &master, &turn).unwrap().unwrap();
    assert_eq!(later, earlier, "the later turn joins the master's open set");

    master_set_waits(
        &case,
        &later,
        &sub,
        ChangeState::Conflict,
        "a two-turn set with a real conflict fix",
    );
    assert!(
        !case
            .owner(&["ls-tree", "--name-only", "main"])
            .contains("NOTES"),
        "the earlier turn's edit is not on main"
    );
}

/// MASTER-LAND-SPLIT: an edit hidden inside the merge commit, on a path the merge did not
/// conflict on, is an ordinary edit too.
#[test]
fn a_master_merge_with_an_extra_edit_inside_it_waits_for_the_owner() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", BOTH_FOURTH_LINES), ("NOTES", "an extra edit\n")],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "an extra edit in the merge",
    );
}

/// MASTER-LAND-SPLIT: a commit list with a follow-up commit after the fix is not only a
/// fix, so a master that keeps working after its merge waits.
#[test]
fn a_master_commit_after_its_conflict_fix_leaves_the_set_waiting() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    write(&master.worktree.join("NOTES"), "after the fix\n");
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "a commit after the fix",
    );
}

/// MASTER-LAND-SPLIT r2: every own merge is judged, so a clean merge of another set under
/// a real conflict fix keeps the master's set waiting (CODE L1).
#[test]
fn a_clean_merge_under_a_conflict_fix_leaves_the_master_set_waiting() {
    let case = Case::new();
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(&other.worktree.join("NOTES"), "clean\n");
    let clean = turn_of(&case, &other, "Write notes");
    let clean_head = case.view(&clean).head;
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    git_in(
        &case.home,
        &master.worktree,
        &[
            "-c",
            "user.name=Master",
            "-c",
            "user.email=master@example.invalid",
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Bring in the notes",
            &clean_head,
        ],
    );
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "a clean merge under the fix",
    );
    assert_eq!(case.view(&clean).state, ChangeState::Pending);
}

/// MASTER-LAND-SPLIT r2: a merge committed with a conflict marker still in the conflicted
/// file is no fix, whichever marker is left (CODE M2).
#[test]
fn a_master_merge_committed_with_a_conflict_marker_waits_for_the_owner() {
    for marker in ["<<<<<<< HEAD", ">>>>>>> theirs", "=======", "=======\r"] {
        let case = Case::new();
        let (sub, sub_head, master) = conflicted_set_and_master(&case);
        let text = format!("one\ntwo\nthree\nfour from the owner\n{marker}\nfour from the agent\n");
        master_resolves(&case, &master, &sub_head, &[("README", text.as_str())]);
        let fix = turn_of(&case, &master, "Resolve the README conflict");
        master_set_waits(&case, &fix, &sub, ChangeState::Conflict, marker);
    }
}

/// MASTER-LAND-SPLIT r2: only a whole marker counts, so a fix whose text merely looks like
/// one still lands by itself (CODE M2).
#[test]
fn a_conflict_fix_holding_text_like_a_marker_lands_itself() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    let text = "one\ntwo\nthree\nfour from the agent\nsee <<<<<<< here\n========\n =======\n>>>>>>>tight\n";
    master_resolves(&case, &master, &sub_head, &[("README", text)]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    let outcome = land_master_set(&case.db, &case.home, &fix).unwrap();
    assert!(matches!(outcome, Some(AcceptOutcome::Landed { .. })));
    assert_eq!(case.owner(&["show", "main:README"]), text);
    assert_eq!(case.view(&sub).state, ChangeState::Accepted);
}

/// MASTER-LAND-SPLIT r2: a fix that resolves the conflict by deleting the file has no
/// markers to find, so it lands by itself (CODE M2). The set whose edit it dropped stays
/// open (LAND-3 r2).
#[test]
fn a_conflict_fix_that_deletes_the_conflicted_file_lands_itself() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    let in_master = |args: &[&str]| git_in(&case.home, &master.worktree, args);
    in_master(&["merge", "--no-commit", "-s", "ours", &sub_head]);
    in_master(&["rm", "-q", "README"]);
    in_master(&[
        "-c",
        "user.name=Master",
        "-c",
        "user.email=master@example.invalid",
        "commit",
        "-q",
        "-m",
        "Drop the README",
    ]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    let outcome = land_master_set(&case.db, &case.home, &fix).unwrap();
    assert!(matches!(outcome, Some(AcceptOutcome::Landed { .. })));
    assert!(
        !case
            .owner(&["ls-tree", "--name-only", "main"])
            .contains("README")
    );
    set_stayed_open(&case, &sub, "the fix dropped the file the set edited");
}

/// MASTER-LAND-SPLIT r2: an edit the owner Discarded while the master's worktree was busy
/// stays below the next set's stored base, and a later conflict fix must not carry it to
/// main with no Accept: the own commits are listed from where the squash measures (SEC F1).
#[test]
fn a_conflict_fix_above_a_discarded_edit_the_worktree_kept_waits_for_the_owner() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    write(&master.worktree.join("NOTES"), "thrown away\n");
    let thrown = turn_of(&case, &master, "Write notes");
    let head = case.view(&thrown).head;
    let busy = master.worktree.join("busy.txt");
    write(&busy, "not committed yet\n");
    discard(&case.db, &thrown, &head, NOW_MS).unwrap();
    std::fs::remove_file(&busy).unwrap();
    assert_eq!(
        git_in(&case.home, &master.worktree, &["rev-parse", "HEAD"]).trim(),
        head,
        "the busy worktree keeps the discarded head"
    );

    record_worktree_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let turn = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: "turn-master-later".into(),
        prompt_line: "Resolve the README conflict".into(),
    };
    let fix = record_turn_end(&case.db, &master, &turn).unwrap().unwrap();
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "a fix above a discarded edit",
    );
    assert!(
        !case
            .owner(&["ls-tree", "--name-only", "main"])
            .contains("NOTES"),
        "the discarded edit is not on main"
    );
}

/// MASTER-LAND-SPLIT r2: a master working from a branch other than Land on holds commits
/// the Land on branch lacks, below its stored base, and a conflict fix must not carry them
/// there with no Accept (SEC F1).
#[test]
fn a_conflict_fix_on_a_base_the_land_on_branch_lacks_waits_for_the_owner() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
    let sub = case.turn(TurnOutcome::Succeeded).unwrap();
    let sub_head = case.view(&sub).head;
    case.owner(&["branch", "release"]);
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute(
            "INSERT INTO team_land_on(project,branch) VALUES (?1,'release')",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour from the owner\n",
        "Owner adds four",
    );
    case.owner_commit("NOTES", "only on main\n", "Main only");
    let release = case.owner(&["rev-parse", "release"]);
    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Pending,
        "a base Land on lacks",
    );
    assert_eq!(
        case.owner(&["rev-parse", "release"]),
        release,
        "the Land on branch does not move"
    );
}

/// MASTER-LAND-SPLIT r2: a merge that keeps none of a set's work holds the set's head yet
/// not its changes, so the owner's Accept of the master's set leaves that set open for its
/// own Accept or Discard (CODE M1).
#[test]
fn an_owner_accept_of_a_master_set_that_dropped_a_sets_work_leaves_that_set_open() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(&other.worktree.join("src/kept.rs"), "kept\n");
    let dropped = turn_of(&case, &other, "Add kept");
    let dropped_head = case.view(&dropped).head;
    case.start();
    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Waits);
    git_in(
        &case.home,
        &master.worktree,
        &[
            "-c",
            "user.name=Master",
            "-c",
            "user.email=master@example.invalid",
            "merge",
            "-q",
            "-s",
            "ours",
            "-m",
            "Keep none of it",
            &dropped_head,
        ],
    );
    write(&master.worktree.join("MASTER"), "own edit\n");
    let set = turn_of(&case, &master, "Own edit");
    let AcceptOutcome::Landed { .. } = case.accept(&set).unwrap() else {
        panic!("the owner's Accept of the master's set conflicted");
    };
    assert_eq!(
        case.view(&dropped).state,
        ChangeState::Pending,
        "a set whose work did not land stays open"
    );
    assert!(store::load(&case.db, &dropped).unwrap().landed.is_none());
    let AcceptOutcome::Landed { .. } = case.accept(&dropped).unwrap() else {
        panic!("the dropped set can no longer be accepted");
    };
    assert_eq!(case.owner(&["show", "main:src/kept.rs"]), "kept\n");
}

/// MASTER-LAND-SPLIT r2: the owner's Accepted row names the sets that Accept closed, as
/// the master's own merge row does, so no set leaves Changes unmentioned (SEC F2).
#[test]
fn an_owner_accept_of_a_master_set_names_the_sets_it_closed_on_its_row() {
    use crate::activity::ActivityKind;
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    let (sub, fix, _) = master_fixing_a_conflict(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Waits);
    assert!(matches!(
        case.accept(&fix).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(case.view(&sub).state, ChangeState::Accepted);
    let rows = activity_of(&case, &fix);
    let row = rows
        .iter()
        .find(|row| row.kind == ActivityKind::Accepted)
        .expect("the Accept wrote its row");
    assert!(
        row.text.contains("It closed the changes of Agent 1"),
        "the Accepted row names the set it closed: {}",
        row.text
    );
}

/// MASTER-LAND-SPLIT r2: a path carrying a configured merge driver stops the gate before
/// its re-merge reads it, so the set waits instead of failing at the land (SEC F3).
#[test]
fn a_conflict_fix_on_a_path_with_a_configured_merge_driver_waits_for_the_owner() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    write(
        &case.repo.join(".git/info/attributes"),
        "README merge=mark\n",
    );
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "a configured merge driver",
    );
}

/// Lands the master's fix and checks that the set it conflicted with stayed open while the
/// set's clean file reached main. Inputs: the case, the master's set and the conflicted set.
/// Output: none.
fn land_leaves_the_set_open(case: &Case, fix: &ChangeSetId, sub: &ChangeSetId) {
    let outcome = land_master_set(&case.db, &case.home, fix).unwrap();
    assert!(
        matches!(outcome, Some(AcceptOutcome::Landed { .. })),
        "the master's fix lands: {outcome:?}"
    );
    assert_eq!(case.owner(&["show", "main:NEW"]), "new\n");
    assert_eq!(
        case.view(sub).state,
        ChangeState::Conflict,
        "a set whose work on a conflicted path did not land stays open"
    );
    assert!(store::load(&case.db, sub).unwrap().landed.is_none());
}

/// LAND-3: one conflicted path left as main had it means the set's work there was dropped,
/// even when its other conflicted path and its clean file landed, so the set stays open
/// for the owner (rv-secreview land-1 r2 F1).
#[test]
fn a_master_land_that_kept_mains_side_of_one_conflict_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_from(
        &case,
        &[("NOTES", "notes of the agent\n"), ("NEW", "new\n")],
        |case| {
            case.owner_commit(
                "README",
                "one\ntwo\nthree\nfour from the owner\n",
                "Owner adds four",
            );
            case.owner_commit("NOTES", "notes of the owner\n", "Owner adds notes");
        },
    );
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[
            ("README", BOTH_FOURTH_LINES),
            ("NOTES", "notes of the owner\n"),
            ("NEW", "new\n"),
        ],
    );
    let fix = turn_of(&case, &master, "Keep the owner's notes");
    land_leaves_the_set_open(&case, &fix, &sub);
}

/// LAND-3: a conflicted path that neither the landed commit nor the commit under it holds
/// was left as main had it too, so the set whose edit of that file was dropped stays open.
#[test]
fn a_master_land_that_kept_mains_deletion_of_a_conflicted_file_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_from(&case, &[("NEW", "new\n")], |case| {
        case.owner(&["rm", "-q", "README"]);
        case.owner(&["commit", "-q", "-m", "Owner drops the README"]);
    });
    master_resolves(&case, &master, &sub_head, &[("NEW", "new\n")]);
    let fix = turn_of(&case, &master, "Keep the deletion");
    land_leaves_the_set_open(&case, &fix, &sub);
}

/// LAND-3: git writes longer markers on a path with this attribute, which the marker read
/// does not know, so a fix of such a path waits for the owner (land-1 r2 L1, F2).
#[test]
fn a_conflict_fix_on_a_path_with_a_marker_size_attribute_waits_for_the_owner() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    write(
        &case.repo.join(".git/info/attributes"),
        "README conflict-marker-size=10\n",
    );
    master_resolves(&case, &master, &sub_head, &[("README", BOTH_FOURTH_LINES)]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "a marker size attribute",
    );
}

/// LAND-3: the marker size attribute counts from the commits' own .gitattributes too, here
/// from the set's, which neither main nor the owner's checkout holds.
#[test]
fn a_conflict_fix_on_a_path_a_commit_gives_a_marker_size_waits_for_the_owner() {
    let case = Case::new();
    let attributes = (".gitattributes", "README conflict-marker-size=10\n");
    let (sub, sub_head, master) = conflicted_set_from(&case, &[attributes], |case| {
        case.owner_commit(
            "README",
            "one\ntwo\nthree\nfour from the owner\n",
            "Owner adds four",
        );
    });
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", BOTH_FOURTH_LINES), attributes],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Conflict,
        "a committed marker size attribute",
    );
}

/// Lands the master's fix. Inputs: the case and the master's set. Output: none.
fn master_fix_lands(case: &Case, fix: &ChangeSetId) {
    let outcome = land_master_set(&case.db, &case.home, fix).unwrap();
    assert!(
        matches!(outcome, Some(AcceptOutcome::Landed { .. })),
        "the master's fix lands: {outcome:?}"
    );
}

/// Asserts a conflicted set stayed open with nothing recorded as landed. Inputs: the case,
/// the set and what the row is about. Output: none.
fn set_stayed_open(case: &Case, set: &ChangeSetId, about: &str) {
    assert_eq!(case.view(set).state, ChangeState::Conflict, "{about}");
    assert!(
        store::load(&case.db, set).unwrap().landed.is_none(),
        "{about}"
    );
}

/// Asserts a conflicted set closed as landed. Inputs: the case, the set and what the row is
/// about. Output: none.
fn set_closed(case: &Case, set: &ChangeSetId, about: &str) {
    assert_eq!(case.view(set).state, ChangeState::Accepted, "{about}");
    assert!(
        store::load(&case.db, set).unwrap().landed.is_some(),
        "{about}"
    );
}

/// The README a master's fix holds when it keeps main's line and both sets' lines.
const ALL_FOURTH_LINES: &str =
    "one\ntwo\nthree\nfour from the owner\nfour from the agent\nfour from the second\n";

/// The README a master's fix holds when it keeps main's line and the second set's.
const OWNER_AND_SECOND: &str = "one\ntwo\nthree\nfour from the owner\nfour from the second\n";

/// Makes two agents' sets conflict with the owner's fourth line and brings in the master, at
/// Full access with Merge into main itself. Input: the case. Output: each set with its head,
/// and the master's workspace.
fn two_conflicted_sets_and_master(
    case: &Case,
) -> ((ChangeSetId, String), (ChangeSetId, String), Workspace) {
    use crate::sandbox::policy::AgentAccess;
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let other = numbered_agent(case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(
        &other.worktree.join("README"),
        "one\ntwo\nthree\nfour from the second\n",
    );
    let second = turn_of(case, &other, "Add the second line");
    case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour from the owner\n",
        "Owner adds four",
    );
    for set in [&first, &second] {
        assert!(matches!(
            case.accept(set).unwrap(),
            AcceptOutcome::Conflict { .. }
        ));
    }
    let master = master_at_work(case);
    master_may_land(case, AgentAccess::Full, crate::team::MasterLands::Merges);
    let (first_head, second_head) = (case.view(&first).head, case.view(&second).head);
    ((first, first_head), (second, second_head), master)
}

/// Commits a merge of a set in the master's worktree with `path` removed. Inputs: the case,
/// the master, the set's head and the path. Output: none.
fn master_merges_without(case: &Case, master: &Workspace, head: &str, path: &str) {
    let in_master = |args: &[&str]| git_in(&case.home, &master.worktree, args);
    in_master(&["merge", "--no-commit", "-s", "ours", head]);
    in_master(&["rm", "-q", path]);
    in_master(&[
        "-c",
        "user.name=Master",
        "-c",
        "user.email=master@example.invalid",
        "commit",
        "-q",
        "-m",
        "Drop the file",
    ]);
}

/// LAND-3 r2 (rv-code H1): the second fix kept the first set's line and dropped the second's.
#[test]
fn a_master_land_that_dropped_the_second_set_leaves_it_open_and_closes_the_first() {
    let case = Case::new();
    let ((first, first_head), (second, second_head), master) =
        two_conflicted_sets_and_master(&case);
    master_resolves(
        &case,
        &master,
        &first_head,
        &[("README", BOTH_FOURTH_LINES)],
    );
    master_resolves(
        &case,
        &master,
        &second_head,
        &[("README", BOTH_FOURTH_LINES)],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflicts");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &second, "its line is not in the landed file");
    set_closed(&case, &first, "its line is in the landed file");
}

/// LAND-3 r2 (rv-code H1): the second fix took the second set's line and dropped the first's.
#[test]
fn a_master_land_that_dropped_the_first_set_leaves_it_open_and_closes_the_second() {
    let case = Case::new();
    let ((first, first_head), (second, second_head), master) =
        two_conflicted_sets_and_master(&case);
    master_resolves(
        &case,
        &master,
        &first_head,
        &[("README", BOTH_FOURTH_LINES)],
    );
    master_resolves(
        &case,
        &master,
        &second_head,
        &[("README", OWNER_AND_SECOND)],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflicts");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &first, "its line is not in the landed file");
    set_closed(&case, &second, "its line is in the landed file");
}

/// LAND-3 r2: a fix that uses every set's line closes every set.
#[test]
fn a_master_land_that_kept_both_sets_lines_closes_both() {
    let case = Case::new();
    let ((first, first_head), (second, second_head), master) =
        two_conflicted_sets_and_master(&case);
    master_resolves(
        &case,
        &master,
        &first_head,
        &[("README", BOTH_FOURTH_LINES)],
    );
    master_resolves(
        &case,
        &master,
        &second_head,
        &[("README", ALL_FOURTH_LINES)],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflicts");
    master_fix_lands(&case, &fix);
    set_closed(&case, &first, "its line is in the landed file");
    set_closed(&case, &second, "its line is in the landed file");
}

/// LAND-3 r2: main's side plus any other line differs from main and still holds none of the
/// set's work (rv-code H1, sec land-3 F2).
#[test]
fn a_master_land_of_mains_side_and_an_unrelated_line_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[(
            "README",
            "one\ntwo\nthree\nfour from the owner\nunrelated\n",
        )],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "its line is not in the landed file");
}

/// LAND-3 r2 (sec land-3 F1): a land whose tree equals main makes no commit, and the commit
/// it records is the tip, so only the landed file can say the set's work is missing.
#[test]
fn a_master_land_that_made_no_commit_leaves_a_one_file_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_and_master(&case);
    let tip = case.tip();
    master_resolves(&case, &master, &sub_head, &[]);
    let fix = turn_of(&case, &master, "Keep main's README");
    master_fix_lands(&case, &fix);
    assert_eq!(case.tip(), tip, "the land made no commit");
    set_stayed_open(&case, &sub, "its line is not on main");
}

/// LAND-3 r2: a set that added one line twice needs both copies in the landed file.
#[test]
fn a_master_land_that_kept_one_copy_of_a_line_added_twice_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = conflicted_set_from(
        &case,
        &[("README", "one\ntwo\nthree\nsame\nsame\n")],
        |case| {
            case.owner_commit(
                "README",
                "one\ntwo\nthree\nfour from the owner\n",
                "Owner adds four",
            );
        },
    );
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", "one\ntwo\nthree\nfour from the owner\nsame\n")],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "one copy of its line is missing");
}

/// Conflicts a set that only removes README's third line with an edit of that line on main
/// and brings in the master. Input: the case. Output: the set, its head and the master.
fn removed_line_set_and_master(case: &Case) -> (ChangeSetId, String, Workspace) {
    conflicted_set_from(case, &[("README", "one\ntwo\n")], |case| {
        case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edits three");
    })
}

/// LAND-3 r2: a set that only removed a line, with the line back in the landed file.
#[test]
fn a_master_land_that_brought_a_removed_line_back_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = removed_line_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", "one\ntwo\nthree\nTHREE\n")],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "the line it removed is back");
}

/// LAND-3 r2: a set that only removed a line, with the line gone from the landed file.
#[test]
fn a_master_land_that_kept_a_removed_line_gone_closes_the_set() {
    let case = Case::new();
    let (sub, sub_head, master) = removed_line_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", "one\ntwo\nTHREE\nmore\n")],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_fix_lands(&case, &fix);
    set_closed(&case, &sub, "the line it removed is gone");
}

/// Conflicts a set that deletes README with an edit of README on main and brings in the
/// master. Input: the case. Output: the set, its head and the master.
fn deleted_file_set_and_master(case: &Case) -> (ChangeSetId, String, Workspace) {
    conflicted_set_changing(
        case,
        |case| std::fs::remove_file(case.workspace.worktree.join("README")).unwrap(),
        |case| {
            case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edits three");
        },
    )
}

/// LAND-3 r2: a set that deleted a file needs it absent from the landed tree.
#[test]
fn a_master_land_that_kept_a_file_the_set_deleted_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = deleted_file_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", "one\ntwo\nTHREE\nmore\n")],
    );
    let fix = turn_of(&case, &master, "Keep the README");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "the file it deleted is still there");
}

/// LAND-3 r2: a set that deleted a file closes when the landed tree lacks it.
#[test]
fn a_master_land_that_deleted_the_file_the_set_deleted_closes_the_set() {
    let case = Case::new();
    case.owner_commit("KEEP", "keep\n", "Owner adds a file");
    let (sub, sub_head, master) = deleted_file_set_and_master(&case);
    master_merges_without(&case, &master, &sub_head, "README");
    let fix = turn_of(&case, &master, "Delete the README");
    master_fix_lands(&case, &fix);
    set_closed(&case, &sub, "the file it deleted is gone");
}

/// The README of a set that only added its fourth line, with main not touching README.
const AGENT_FOURTH_LINE: &str = "one\ntwo\nthree\nfour from the agent\n";

/// Conflicts a set that changes a binary file with another change of it on main and brings
/// in the master. Input: the case. Output: the set, its head and the master.
fn binary_set_and_master(case: &Case) -> (ChangeSetId, String, Workspace) {
    case.owner_commit("data", "base\0\n", "Owner adds data");
    conflicted_set_from(case, &[("data", "agent\0\n")], |case| {
        case.owner_commit("data", "owner\0\n", "Owner edits data");
    })
}

/// LAND-3 r2: a binary path has no lines to read, so the landed blob must be the set's.
#[test]
fn a_master_land_that_took_mains_binary_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = binary_set_and_master(&case);
    master_resolves(&case, &master, &sub_head, &[("README", AGENT_FOURTH_LINE)]);
    let fix = turn_of(&case, &master, "Keep main's data");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "the landed blob is not the set's");
}

/// LAND-3 r2: a binary path closes when the landed blob is the set's.
#[test]
fn a_master_land_that_took_the_sets_binary_closes_the_set() {
    let case = Case::new();
    let (sub, sub_head, master) = binary_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", AGENT_FOURTH_LINE), ("data", "agent\0\n")],
    );
    let fix = turn_of(&case, &master, "Take the set's data");
    master_fix_lands(&case, &fix);
    set_closed(&case, &sub, "the landed blob is the set's");
}

/// LAND-3 r2: a binary file's bytes are not lines, so holding the set's bytes and more is
/// another blob.
#[test]
fn a_master_land_that_added_to_the_sets_binary_leaves_the_set_open() {
    let case = Case::new();
    let (sub, sub_head, master) = binary_set_and_master(&case);
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", AGENT_FOURTH_LINE), ("data", "agent\0\nextra\n")],
    );
    let fix = turn_of(&case, &master, "Change the set's data");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "the landed blob is not the set's");
}

/// LAND-3-R2: a conflicted path holding a line break cannot be read in the one-line-per-path
/// batch, and a read failure leaves the set open.
#[test]
fn a_master_land_whose_conflicted_path_cannot_be_read_leaves_the_set_open() {
    const NAME: &str = "line\nbreak";
    let case = Case::new();
    case.owner_commit(NAME, "one\n", "Owner adds a file");
    let (sub, sub_head, master) = conflicted_set_from(&case, &[(NAME, "agent\n")], |case| {
        case.owner_commit(NAME, "owner\n", "Owner edits the file");
    });
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[("README", AGENT_FOURTH_LINE), (NAME, "owner\nagent\n")],
    );
    let fix = turn_of(&case, &master, "Keep both sides");
    assert!(matches!(
        case.accept(&fix).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    set_stayed_open(&case, &sub, "its conflicted path could not be read");
}

/// LAND-3 r2: main renamed the file the set edited, so the conflict sits at a path the set
/// never had and no line of the set's can be read there. That waits for the owner, though
/// the fix may have used the set's work.
#[test]
fn a_master_land_at_a_path_the_set_never_had_leaves_the_set_open() {
    let case = Case::new();
    let long: String = (1..=12).map(|number| format!("line {number}\n")).collect();
    case.owner_commit("LONG", &long, "Owner adds a long file");
    case.agent(&["merge", "-q", "--ff-only", "main"]);
    let (sub, sub_head, master) = conflicted_set_changing(
        &case,
        |case| case.edit("LONG", &format!("{long}from the agent\n")),
        |case| {
            case.owner(&["mv", "LONG", "RENAMED"]);
            case.owner_commit(
                "RENAMED",
                &format!("{long}from the owner\n"),
                "Owner renames and edits",
            );
        },
    );
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[(
            "RENAMED",
            &format!("{long}from the owner\nfrom the agent\n"),
        )],
    );
    let fix = turn_of(&case, &master, "Resolve the renamed file");
    master_fix_lands(&case, &fix);
    set_stayed_open(&case, &sub, "the conflicted path was never the set's");
}

/// MASTER-LAND-SPLIT r2: a set already on the first parent of the fix came in by no fix,
/// even when the second parent holds it too, so the master's set waits (SEC F3).
#[test]
fn a_set_on_both_sides_of_a_conflict_fix_leaves_the_master_set_waiting() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    let held = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &held.repository, &held.worktree, &held.git_dir).unwrap();
    write(&held.worktree.join("NOTES"), "on both sides\n");
    let both = turn_of(&case, &held, "Write notes");
    let both_head = case.view(&both).head;
    let other = numbered_agent(&case, 4);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    git_in(
        &case.home,
        &other.worktree,
        &["merge", "-q", "--ff-only", &both_head],
    );
    write(
        &other.worktree.join("README"),
        "one\ntwo\nthree\nfour from the other\n",
    );
    let clashing = turn_of(&case, &other, "Add the other four");
    let clashing_head = case.view(&clashing).head;
    case.owner(&["merge", "-q", "--ff-only", &both_head]);
    case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour from the owner\n",
        "Owner adds four",
    );
    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
    master_resolves(
        &case,
        &master,
        &clashing_head,
        &[("README", "one\ntwo\nthree\nfour from both\n")],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    master_set_waits(
        &case,
        &fix,
        &both,
        ChangeState::Pending,
        "a set on both sides",
    );
    assert_eq!(case.view(&clashing).state, ChangeState::Pending);
}

/// MASTER-LAND-SPLIT r2: a master set already held in conflict, or with a step of the
/// engine half done, does not land by itself though its commits would pass (CODE L1).
#[test]
fn a_master_set_held_in_conflict_or_mid_step_does_not_land_itself() {
    for mid_step in [false, true] {
        let case = Case::new();
        let (_, fix, _) = master_fixing_a_conflict(&case);
        let mut held = store::load(&case.db, &fix).unwrap();
        if mid_step {
            held.intent = Some(
                serde_json::from_value(serde_json::json!({
                    "step": "reset",
                    "from": held.head,
                    "target": held.head,
                }))
                .unwrap(),
            );
        } else {
            held.state = ChangeState::Conflict;
        }
        store::save(&case.db, &held).unwrap();
        let tip = case.tip();
        assert_eq!(
            land_master_set(&case.db, &case.home, &fix).unwrap(),
            None,
            "mid_step {mid_step}"
        );
        assert_eq!(case.tip(), tip, "main does not move");
        assert_eq!(store::load(&case.db, &fix).unwrap(), held);
    }
}

/// MASTER-LAND-SPLIT: with criss-cross history a merge has more than one merge base, so
/// the engine cannot tell what the merge would have been and the set waits.
#[test]
fn a_master_merge_with_more_than_one_merge_base_waits_for_the_owner() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
    let sub = case.turn(TurnOutcome::Succeeded).unwrap();
    let first = case.view(&sub).head;
    let owner = case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour from the owner\n",
        "Owner adds four",
    );
    case.start();
    let in_agent = |args: &[&str]| git_in(&case.home, &case.workspace.worktree, args);
    in_agent(&["merge", "--no-commit", "-s", "ours", &owner]);
    case.edit("README", "one\ntwo\nthree\nfour from both, agent first\n");
    in_agent(&["add", "README"]);
    in_agent(&[
        "-c",
        "user.name=Agent",
        "-c",
        "user.email=agent@example.invalid",
        "commit",
        "-q",
        "-m",
        "Take main in",
    ]);
    assert_eq!(case.turn(TurnOutcome::Succeeded).unwrap(), sub);
    let second = case.view(&sub).head;
    assert_ne!(first, second, "the agent's second turn moved its set");

    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
    master_resolves(
        &case,
        &master,
        &first,
        &[("README", "one\ntwo\nthree\nfour from both, owner first\n")],
    );
    master_resolves(&case, &master, &second, &[("README", BOTH_FOURTH_LINES)]);
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    let parents = case.owner(&["log", "-1", "--format=%P", &case.view(&fix).head]);
    let parents: Vec<&str> = parents.split_whitespace().collect();
    assert_eq!(
        case.owner(&["merge-base", "--all", parents[0], parents[1]])
            .lines()
            .count(),
        2,
        "the fixture makes a criss-cross merge"
    );

    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Pending,
        "more than one merge base",
    );
}

/// MASTER-LAND-SPLIT: one set brought in by a real conflict does not excuse another set
/// the master fast-forwarded onto first; that one had no conflict.
#[test]
fn a_set_fast_forwarded_in_beside_a_conflict_fix_leaves_the_master_set_waiting() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(
        &other.worktree.join("README"),
        "one\ntwo\nthree\nfour from the other\n",
    );
    let clashing = turn_of(&case, &other, "Add the other four");
    let clashing_head = case.view(&clashing).head;
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
    let sub = case.turn(TurnOutcome::Succeeded).unwrap();
    let sub_head = case.view(&sub).head;

    let master = master_at_work(&case);
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Merges);
    git_in(
        &case.home,
        &master.worktree,
        &["merge", "-q", "--ff-only", &sub_head],
    );
    master_resolves(
        &case,
        &master,
        &clashing_head,
        &[("README", "one\ntwo\nthree\nfour from both\n")],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");

    master_set_waits(
        &case,
        &fix,
        &sub,
        ChangeState::Pending,
        "a set that came in by fast-forward",
    );
    assert_eq!(case.view(&clashing).state, ChangeState::Pending);
}

/// MASTER-LAND-SPLIT: the owner's "Ask me before merging" stops a master's conflict fix
/// the way it stops the merge tool, and with it off the same set lands, the other set's
/// clean files with it.
#[test]
fn a_pure_conflict_fix_waits_with_ask_on_and_lands_with_ask_off() {
    let case = Case::new();
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
    case.edit("src/new.rs", "fn main() {}\n");
    let sub = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour from the owner\n",
        "Owner adds four",
    );
    assert!(matches!(
        case.accept(&sub).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    let master = master_at_work(&case);
    master_may_land(
        &case,
        crate::sandbox::policy::AgentAccess::Full,
        crate::team::MasterLands::Merges,
    );
    let sub_head = case.view(&sub).head;
    master_resolves(
        &case,
        &master,
        &sub_head,
        &[
            ("README", BOTH_FOURTH_LINES),
            ("src/new.rs", "fn main() {}\n"),
        ],
    );
    let fix = turn_of(&case, &master, "Resolve the README conflict");
    let project = case.workspace.project.as_str();

    case.db
        .execute("INSERT INTO team_merge_ask VALUES (?1,1)", [project])
        .unwrap();
    master_set_waits(&case, &fix, &sub, ChangeState::Conflict, "Ask on");

    case.db
        .execute(
            "UPDATE team_merge_ask SET ask=0 WHERE project=?1",
            [project],
        )
        .unwrap();
    let outcome = land_master_set(&case.db, &case.home, &fix).unwrap();
    assert!(
        matches!(&outcome, Some(AcceptOutcome::Landed { branch, .. }) if branch == "main"),
        "with Ask off the pure fix lands: {outcome:?}"
    );
    assert_eq!(case.owner(&["show", "main:README"]), BOTH_FOURTH_LINES);
    assert_eq!(case.owner(&["show", "main:src/new.rs"]), "fn main() {}\n");
    assert_eq!(case.view(&sub).state, ChangeState::Accepted);
}

/// MASTER-LAND-SPLIT: when the fix waits and the owner Accepts it, the sets it holds close
/// with it. Left open, a second Accept conflicts again, because a squash on main is not an
/// ancestor of that set's head, and only Discard would clear it.
#[test]
fn an_owner_accept_of_a_master_set_closes_the_sets_it_holds_and_a_second_accept_is_not_stuck() {
    use crate::sandbox::policy::AgentAccess;
    let case = Case::new();
    let (sub, fix, resolved) = master_fixing_a_conflict(&case);
    let sub_head = case.view(&sub).head;
    master_may_land(&case, AgentAccess::Full, crate::team::MasterLands::Waits);
    master_set_waits(&case, &fix, &sub, ChangeState::Conflict, "Waits");

    let AcceptOutcome::Landed { commit, .. } = case.accept(&fix).unwrap() else {
        panic!("the owner's Accept of the fix conflicted");
    };
    assert_eq!(case.owner(&["show", "main:README"]), resolved);
    assert_eq!(
        case.view(&sub).state,
        ChangeState::Accepted,
        "the set the fix holds closes with the owner's Accept"
    );
    assert_eq!(
        store::load(&case.db, &sub).unwrap().landed.as_deref(),
        Some(commit.as_str()),
        "it landed with the owner's commit"
    );

    let tip = case.tip();
    assert_eq!(
        refused(accept(
            &case.db,
            &case.home,
            &sub,
            &sub_head,
            None,
            crate::activity::AcceptPlace::Changes
        )),
        "changes_stale",
        "a second Accept finds the set closed, not a new conflict"
    );
    assert_eq!(case.tip(), tip, "main does not move for the second Accept");
    assert!(
        store::open_of(&case.db, Some(&case.workspace.project))
            .unwrap()
            .is_empty(),
        "no set is left open to Discard"
    );
}

/// T3-R2-L1: the gate, the land and the close are one step on the repository. An owner
/// Discard of the covered set that races the master's land either comes first, so nothing
/// lands, or finds the set already closed. It must not leave the master's edit on main with
/// the set it was justified by thrown away.
#[test]
fn an_owner_discard_racing_a_master_land_cannot_slip_between_the_check_and_the_land() {
    let case = Case::new();
    let (sub, fix, resolved) = master_fixing_a_conflict(&case);
    let sub_head = case.view(&sub).head;
    let repository = case.workspace.repository.clone();
    let engine = case.repo.parent().unwrap().join("engine");
    let racer = std::cell::RefCell::new(None);
    let paused = |step: owner::Step| {
        if step == owner::Step::Gated {
            let (engine, home, set, head) = (
                engine.clone(),
                case.home.clone(),
                sub.clone(),
                sub_head.clone(),
            );
            let handle = std::thread::spawn(move || {
                git::OWNER_HOME.with(|owner| *owner.borrow_mut() = Some(home));
                let db = open(&engine).unwrap();
                db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                discard(&db, &set, &head, NOW_MS).map_err(|error| error.0)
            });
            while !handle.is_finished() && !flow::busy(&repository) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            *racer.borrow_mut() = Some(handle);
        }
        false
    };

    let outcome = owner::land_master(&case.db, &case.home, &fix, &paused).unwrap();

    let raced = racer.into_inner().unwrap().join().unwrap();
    assert!(
        matches!(&outcome, Some(AcceptOutcome::Landed { .. })),
        "the master's fix lands: {outcome:?}"
    );
    assert_eq!(
        raced,
        Err("changes_stale".to_owned()),
        "the Discard waits for the land and finds the set closed"
    );
    assert_eq!(case.owner(&["show", "main:README"]), resolved);
    assert_eq!(case.view(&sub).state, ChangeState::Accepted);
}

#[test]
fn a_master_fix_that_conflicts_with_main_lands_nothing() {
    let case = Case::new();
    let (sub, fix, _) = master_fixing_a_conflict(&case);
    let tip = case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour from the owner\nowner five\n",
        "Owner adds five",
    );
    let outcome = land_master_set(&case.db, &case.home, &fix).unwrap();
    assert_eq!(
        outcome,
        Some(AcceptOutcome::Conflict {
            files: vec!["README".to_owned()]
        })
    );
    assert_eq!(case.tip(), tip, "main does not move");
    assert_eq!(case.view(&fix).state, ChangeState::Pending);
    assert_eq!(case.view(&sub).state, ChangeState::Conflict);
    assert!(
        activity_of(&case, &fix)
            .iter()
            .all(|row| row.kind != crate::activity::ActivityKind::MergedIntoMain),
        "nothing merged, so no merge row"
    );
}

#[test]
fn a_master_set_waits_for_the_owner_under_waits_or_below_full_access() {
    use crate::sandbox::policy::AgentAccess;
    use crate::team::MasterLands;
    let case = Case::new();
    let (sub, fix, _) = master_fixing_a_conflict(&case);
    let tip = case.tip();
    for (access, lands) in [
        (AgentAccess::Full, MasterLands::Waits),
        (AgentAccess::OwnBranch, MasterLands::Merges),
        (AgentAccess::Sandboxed, MasterLands::Merges),
    ] {
        master_may_land(&case, access, lands);
        assert_eq!(
            land_master_set(&case.db, &case.home, &fix).unwrap(),
            None,
            "{access:?} with {lands:?} leaves the set to the owner"
        );
        assert_eq!(case.view(&fix).state, ChangeState::Pending);
        assert_eq!(case.view(&sub).state, ChangeState::Conflict);
        assert_eq!(case.tip(), tip, "main does not move");
    }
}

/// Makes the case's store keep the project's activity, as an engine's does. Input: the case.
fn keeps_activity(case: &Case) {
    case.db
        .execute_batch("CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY)")
        .unwrap();
    crate::activity::initialize(&case.db).unwrap();
}

#[test]
fn undo_reverts_a_landed_set_on_main_and_refuses_with_the_files_when_main_changed_them_since() {
    use crate::activity::ActivityKind;
    let case = Case::new();
    keeps_activity(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { commit: landed, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    let AcceptOutcome::Landed { commit, branch } = undo_landing(&case.db, &case.home, &id).unwrap()
    else {
        panic!("the undo conflicted");
    };
    assert_eq!((branch.as_str(), case.tip()), ("main", commit.clone()));
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P|%an <%ae>|%s", &commit])
            .trim(),
        format!("{landed}|Repo Owner <owner@example.invalid>|Undo: Add the fourth line")
    );
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    let rows = activity_of(&case, &id);
    assert!(
        rows.iter()
            .any(|row| row.kind == ActivityKind::Accepted && row.undone),
        "the Accept row shows Undone: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == ActivityKind::Undone && !row.undone)
            .count(),
        1,
        "one Undone row"
    );
    assert_eq!(
        refused(undo_landing(&case.db, &case.home, &id)),
        "changes_undo_unavailable",
        "a merge is undone once"
    );
    assert_eq!(case.tip(), commit);

    let case = Case::new();
    keeps_activity(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    let tip = case.owner_commit(
        "README",
        "one\ntwo\nthree\nfour, as the owner says\n",
        "Owner rewords four",
    );
    assert_eq!(
        undo_landing(&case.db, &case.home, &id).unwrap(),
        AcceptOutcome::Conflict {
            files: vec!["README".to_owned()]
        }
    );
    assert_eq!(case.tip(), tip, "main does not move");
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour, as the owner says\n"
    );
    assert_eq!(case.owner(&["status", "--porcelain"]), "");
    assert!(
        activity_of(&case, &id)
            .iter()
            .all(|row| !row.undone && row.kind != ActivityKind::Undone),
        "nothing was undone"
    );
}

#[test]
fn an_accept_that_made_no_commit_and_a_set_still_open_have_nothing_to_undo() {
    let case = Case::new();
    keeps_activity(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert_eq!(
        refused(undo_landing(&case.db, &case.home, &id)),
        "changes_undo_unavailable",
        "an open set has not landed"
    );
    let tip = case.owner_commit("README", "one\ntwo\nthree\nfour\n", "Owner adds four too");
    assert_eq!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed {
            commit: tip.clone(),
            branch: "main".to_owned()
        },
        "main already held the work, so the Accept made no commit"
    );
    assert_eq!(
        refused(undo_landing(&case.db, &case.home, &id)),
        "changes_undo_unavailable",
        "an undo never reverts a commit the Accept did not make"
    );
    assert_eq!(case.tip(), tip);
    assert_eq!(
        std::fs::read_to_string(case.repo.join("README")).unwrap(),
        "one\ntwo\nthree\nfour\n"
    );
}

#[test]
fn a_master_merge_whose_covered_set_cannot_close_is_still_listed_as_landed() {
    use crate::activity::ActivityKind;
    let case = Case::new();
    let (sub, fix, resolved) = master_fixing_a_conflict(&case);
    let tip = case.tip();
    let log = case.repo.join(".git/logs").join(flow::pending_ref(&sub));
    std::fs::create_dir_all(&log).unwrap();

    let outcome = land_master_set(&case.db, &case.home, &fix);

    assert!(
        matches!(outcome, Ok(Some(AcceptOutcome::Landed { .. }))),
        "main moved, so the master's note says it merged: {outcome:?}"
    );
    assert_ne!(case.tip(), tip, "main moved");
    assert_eq!(case.owner(&["show", "main:README"]), resolved);
    assert_eq!(case.view(&fix).state, ChangeState::Accepted);
    let rows = activity_of(&case, &fix);
    assert_eq!(
        rows.iter()
            .filter(|row| row.kind == ActivityKind::MergedIntoMain)
            .count(),
        1,
        "the landed merge is listed once: {rows:?}"
    );
}

#[test]
fn a_master_merge_an_engine_stop_cut_short_settles_as_the_masters_merge() {
    use crate::activity::ActivityKind;
    for step in [owner::Step::FilesWritten, owner::Step::BranchMoved] {
        let case = Case::new();
        let (sub, fix, resolved) = master_fixing_a_conflict(&case);
        let head = case.view(&fix).head;
        let stopped = owner::accept_to(
            &case.db,
            &case.home,
            &fix,
            &head,
            None,
            None,
            owner::By::Master,
            &|at| at == step,
        );
        assert_eq!(refused(stopped), "changes_git_failed", "{step:?}");

        assert_eq!(
            refused(case.accept(&fix)),
            "changes_stale",
            "the next owner action finishes the merge: {step:?}"
        );

        assert!(store::in_flight(&case.db).unwrap().is_empty(), "{step:?}");
        assert_eq!(case.owner(&["show", "main:README"]), resolved, "{step:?}");
        assert_eq!(case.view(&fix).state, ChangeState::Accepted, "{step:?}");
        assert_eq!(
            case.view(&sub).state,
            ChangeState::Accepted,
            "the covered set closes: {step:?}"
        );
        let rows: Vec<_> = activity_of(&case, &fix)
            .into_iter()
            .map(|row| (row.kind, row.actor))
            .collect();
        assert_eq!(
            rows,
            vec![(ActivityKind::MergedIntoMain, "Master".to_owned())],
            "listed as the master's merge, not an Accept: {step:?}"
        );
    }
}

#[test]
fn an_undo_an_engine_stop_cut_short_is_finished_or_dropped_by_the_next_owner_action() {
    use crate::activity::ActivityKind;
    for step in [
        owner::Step::Recorded,
        owner::Step::FilesWritten,
        owner::Step::BranchMoved,
    ] {
        let case = Case::new();
        keeps_activity(&case);
        case.edit("README", "one\ntwo\nthree\nfour\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        assert!(matches!(
            case.accept(&id).unwrap(),
            AcceptOutcome::Landed { .. }
        ));
        let stopped = owner::undo(&case.db, &case.home, &id, &|at| at == step);
        assert_eq!(refused(stopped), "changes_git_failed", "{step:?}");
        assert_eq!(
            store::in_flight(&case.db).unwrap().len(),
            1,
            "the undo saved its intent before it wrote anything: {step:?}"
        );

        let again = undo_landing(&case.db, &case.home, &id);

        assert!(store::in_flight(&case.db).unwrap().is_empty(), "{step:?}");
        if step == owner::Step::Recorded {
            assert!(
                matches!(again, Ok(AcceptOutcome::Landed { .. })),
                "nothing was written, so the intent is dropped and the undo runs: {again:?}"
            );
        } else {
            assert_eq!(
                refused(again),
                "changes_undo_unavailable",
                "the stopped undo was finished: {step:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(case.repo.join("README")).unwrap(),
            "one\ntwo\nthree\n",
            "{step:?}"
        );
        assert_eq!(case.owner(&["status", "--porcelain"]), "", "{step:?}");
        let rows = activity_of(&case, &id);
        assert_eq!(
            rows.iter()
                .filter(|row| row.kind == ActivityKind::Undone)
                .count(),
            1,
            "one Undone row: {step:?} {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.kind == ActivityKind::Accepted && row.undone),
            "the Accept row shows Undone: {step:?}"
        );
    }
}

/// LAND-16-L1: the Undone row names the set as its Accept row does, by its agent's title and
/// its message through change_set_message, so a message stored with hidden characters reads
/// as escapes, and never by the raw prompt.
#[test]
fn an_undone_row_names_the_set_as_its_accept_row_does() {
    use crate::activity::ActivityKind;
    let case = Case::new();
    keeps_activity(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.db
        .execute(
            "UPDATE landing_change_sets SET message = ?1",
            ["Stored before.\u{e0041}\u{feff}"],
        )
        .unwrap();
    let AcceptOutcome::Landed { .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    let AcceptOutcome::Landed { .. } = undo_landing(&case.db, &case.home, &id).unwrap() else {
        panic!("the undo conflicted");
    };
    let texts: Vec<String> = activity_of(&case, &id)
        .into_iter()
        .filter(|row| row.kind == ActivityKind::Undone)
        .map(|row| row.text)
        .collect();
    assert_eq!(
        texts,
        vec!["Undid the changes of Agent 1: Stored before.\\u{e0041}\\ufeff".to_owned()]
    );
}

#[test]
fn undoing_a_master_merge_names_and_marks_the_sets_it_closed() {
    use crate::activity::ActivityKind;
    let case = Case::new();
    let (sub, fix, _) = master_fixing_a_conflict(&case);
    let owner_readme = case.owner(&["show", "main:README"]);
    assert!(matches!(
        land_master_set(&case.db, &case.home, &fix).unwrap(),
        Some(AcceptOutcome::Landed { .. })
    ));

    let AcceptOutcome::Landed { commit, .. } = undo_landing(&case.db, &case.home, &fix).unwrap()
    else {
        panic!("the undo conflicted");
    };

    assert_eq!(case.owner(&["show", "main:README"]), owner_readme);
    assert_eq!(
        store::load(&case.db, &sub).unwrap().undone.as_deref(),
        Some(commit.as_str()),
        "the set the merge closed is marked undone with it"
    );
    let undone: Vec<_> = activity_of(&case, &fix)
        .into_iter()
        .filter(|row| row.kind == ActivityKind::Undone)
        .collect();
    assert_eq!(undone.len(), 1, "{undone:?}");
    assert!(
        undone[0].text.contains("Agent 1"),
        "the Undone row says the closed set's work was reverted too: {}",
        undone[0].text
    );
}

/// T4-R2-L1: an Accept that made no commit records the branch tip as where it landed. An
/// undo of the commit at that tip reverts nothing of the set when that commit changed none
/// of the set's files, so the set keeps its Accept and the Undone row does not name it.
#[test]
fn undoing_a_commit_leaves_a_set_accepted_above_it_whose_files_it_never_changed() {
    use crate::activity::ActivityKind;
    let case = Case::new();
    keeps_activity(&case);
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let already = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nthree\nfour\n", "Owner takes the line");
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(&other.worktree.join("src/kept.rs"), "kept\n");
    let later = turn_of(&case, &other, "Add kept");
    let AcceptOutcome::Landed { commit: tip, .. } = case.accept(&later).unwrap() else {
        panic!("the later set conflicted");
    };
    let AcceptOutcome::Landed { commit, .. } = case.accept(&already).unwrap() else {
        panic!("the set already on main conflicted");
    };
    assert_eq!(commit, tip, "the Accept made no commit");
    assert!(matches!(
        undo_landing(&case.db, &case.home, &later).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        store::load(&case.db, &already).unwrap().undone,
        None,
        "the undo reverted nothing of this set"
    );
    let label = store::load(&case.db, &already).unwrap().agent.label;
    let rows: Vec<_> = activity_of(&case, &later)
        .into_iter()
        .filter(|row| row.kind == ActivityKind::Undone)
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        !rows[0].text.contains(&label),
        "the Undone row claims a revert that did not happen: {}",
        rows[0].text
    );
}

/// Has a new agent write the README and lands its set. Inputs: the case, the agent's number
/// and the README's text. Output: the set.
fn readme_landed_by(case: &Case, number: usize, text: &str) -> ChangeSetId {
    let other = numbered_agent(case, number);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(&other.worktree.join("README"), text);
    let id = turn_of(case, &other, "Change the README");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    id
}

/// Lands one set's README line from a second agent, optionally has the owner commit one
/// unrelated file above it, then commits `files`, so the case's own set, holding the same line
/// and `files`, is accepted with no commit at the tip. Inputs: the case, the files and whether
/// a commit sits between. Output: the set accepted with no commit and the set that made it.
fn same_change_accepted(
    case: &Case,
    files: &[(&str, &str)],
    between: bool,
) -> (ChangeSetId, ChangeSetId) {
    keeps_activity(case);
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    for (path, text) in files {
        case.edit(path, text);
    }
    let same = case.turn(TurnOutcome::Succeeded).unwrap();
    let maker = readme_landed_by(case, 3, "one\ntwo\nthree\nfour\n");
    let mut tip = case.tip();
    if between {
        tip = case.owner_commit("BETWEEN", "between\n", "Owner adds a file");
    }
    for (path, text) in files {
        tip = case.owner_commit(path, text, "Owner takes the file");
    }
    let AcceptOutcome::Landed { commit, .. } = case.accept(&same).unwrap() else {
        panic!("the set already on main conflicted");
    };
    assert_eq!(commit, tip, "the Accept made no commit");
    (same, maker)
}

/// Undoes a set and asserts another set was marked undone by that commit and named on the
/// one Undone row. Inputs: the case, the set undone, the other set and what the row is
/// about. Output: none.
fn undo_marks_and_names(case: &Case, undone: &ChangeSetId, other: &ChangeSetId, about: &str) {
    use crate::activity::ActivityKind;
    let AcceptOutcome::Landed { commit, .. } = undo_landing(&case.db, &case.home, undone).unwrap()
    else {
        panic!("the undo conflicted");
    };
    let stored = store::load(&case.db, other).unwrap();
    assert_eq!(stored.undone.as_deref(), Some(commit.as_str()), "{about}");
    let rows: Vec<_> = activity_of(case, undone)
        .into_iter()
        .filter(|row| row.kind == ActivityKind::Undone)
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].text.contains(&stored.agent.label),
        "{about}: {}",
        rows[0].text
    );
}

/// LAND-4 L1: an Accept that made no commit records whatever commit was the tip, which can
/// sit above the commit that really made the change. Undoing that commit reverts the set
/// too, so the set is marked and named.
#[test]
fn undoing_a_commit_marks_a_set_accepted_above_it_with_the_same_change() {
    let case = Case::new();
    let (same, maker) = same_change_accepted(&case, &[], true);
    undo_marks_and_names(&case, &maker, &same, "the undo reverted this set's change");
}

/// LAND-4 L2: one reverted file is enough, since the set's work is then not all there.
#[test]
fn undoing_a_commit_marks_a_set_when_only_one_of_its_files_was_reverted() {
    let case = Case::new();
    let (same, maker) = same_change_accepted(&case, &[("OTHER", "other\n")], true);
    undo_marks_and_names(&case, &maker, &same, "one of its two files was reverted");
}

/// LAND-4 L2 and SEC-F1: a set recorded at the undone commit whose head git can no longer
/// read counts as reverted, since the branch has already moved and the undo must not fail.
#[test]
fn undoing_a_commit_marks_a_set_recorded_at_it_whose_head_git_cannot_read() {
    let case = Case::new();
    let (same, maker) = same_change_accepted(&case, &[], false);
    case.db
        .execute(
            "UPDATE landing_change_sets SET head=?1 WHERE id=?2",
            ["0123456789abcdef0123456789abcdef01234567", same.as_str()],
        )
        .unwrap();
    undo_marks_and_names(&case, &maker, &same, "an unreadable set counts as reverted");
}

/// Land-6 r2: an unreadable set above the undone commit proves nothing about that commit,
/// so the undo does not name it.
#[test]
fn undoing_a_commit_leaves_a_set_above_it_whose_head_git_cannot_read() {
    let case = Case::new();
    let (same, maker) = same_change_accepted(&case, &[], true);
    case.db
        .execute(
            "UPDATE landing_change_sets SET head=?1 WHERE id=?2",
            ["0123456789abcdef0123456789abcdef01234567", same.as_str()],
        )
        .unwrap();
    assert!(matches!(
        undo_landing(&case.db, &case.home, &maker).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(store::load(&case.db, &same).unwrap().undone, None);
}

/// Land-6 r2: sharing a file is not enough. A later set on the same file whose lines the
/// revert left in place is neither marked nor named, so the Undone row stays true.
#[test]
fn undoing_a_commit_leaves_a_later_set_on_the_same_file_whose_lines_survive() {
    use crate::activity::ActivityKind;
    let case = Case::new();
    keeps_activity(&case);
    case.start();
    case.edit("README", "zero\none\ntwo\nthree\n");
    let later = case.turn(TurnOutcome::Succeeded).unwrap();
    let older = readme_landed_by(&case, 3, "one\ntwo\nthree\nfour\n");
    readme_landed_by(&case, 4, "zero\none\ntwo\nthree\nfour\n");
    let tip = case.tip();
    let AcceptOutcome::Landed { commit, .. } = case.accept(&later).unwrap() else {
        panic!("the set already on main conflicted");
    };
    assert_eq!(commit, tip, "the Accept made no commit");
    assert!(matches!(
        undo_landing(&case.db, &case.home, &older).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    let stored = store::load(&case.db, &later).unwrap();
    assert_eq!(stored.undone, None, "its line is still on main");
    let rows: Vec<_> = activity_of(&case, &older)
        .into_iter()
        .filter(|row| row.kind == ActivityKind::Undone)
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        !rows[0].text.contains(&stored.agent.label),
        "the Undone row claims a revert that did not happen: {}",
        rows[0].text
    );
}

/// Land-6 r2: sets below the undone commit cost no git run, since one read of the range
/// decides for all of them.
#[test]
fn undoing_a_commit_costs_the_same_git_runs_for_any_number_of_sets_below_it() {
    let runs = |copies: usize| {
        let case = Case::new();
        keeps_activity(&case);
        case.start();
        case.edit("README", "one\ntwo\nthree\nfour\n");
        let already = case.turn(TurnOutcome::Succeeded).unwrap();
        case.owner_commit("README", "one\ntwo\nthree\nfour\n", "Owner takes the line");
        assert!(matches!(
            case.accept(&already).unwrap(),
            AcceptOutcome::Landed { .. }
        ));
        let record = store::load(&case.db, &already).unwrap();
        for copy in 0..copies {
            let mut below = record.clone();
            below.id = ChangeSetId::new(format!("{copy:032x}")).unwrap();
            store::save(&case.db, &below).unwrap();
        }
        let later = readme_landed_by(&case, 3, "zero\none\ntwo\nthree\nfour\n");
        let before = git::GIT_RUNS.with(std::cell::Cell::get);
        undo_landing(&case.db, &case.home, &later).unwrap();
        git::GIT_RUNS.with(std::cell::Cell::get) - before
    };
    assert_eq!(
        runs(5),
        runs(0),
        "sets below the undone commit cost git runs"
    );
}

/// Land-6 r2: a git failure reading what the undo reverted stops the step and leaves the
/// intent, so the next look at the set runs it again instead of marking on a guess.
#[test]
fn a_read_failure_in_a_finished_undo_is_left_for_settle_to_retry() {
    let case = Case::new();
    let (same, maker) = same_change_accepted(&case, &[], true);
    let landed = store::load(&case.db, &maker).unwrap().landed.unwrap();
    assert!(
        owner::undo(&case.db, &case.home, &maker, &|step| {
            step == owner::Step::BranchMoved
        })
        .is_err()
    );
    let set_landed = |value: &str| {
        case.db
            .execute(
                "UPDATE landing_change_sets SET landed=?1 WHERE id=?2",
                [value, maker.as_str()],
            )
            .unwrap();
    };
    set_landed("0123456789abcdef0123456789abcdef01234567");
    assert!(owner::settled(&case.db, &maker).is_err());
    assert!(store::load(&case.db, &maker).unwrap().intent.is_some());
    assert_eq!(store::load(&case.db, &same).unwrap().undone, None);
    set_landed(&landed);
    owner::settled(&case.db, &maker).unwrap();
    assert!(store::load(&case.db, &maker).unwrap().intent.is_none());
    assert!(store::load(&case.db, &same).unwrap().undone.is_some());
}

/// LAND-4 L1: only sets above the undone commit can hold its change. A set accepted with no
/// commit before it landed keeps its Accept, though the commit changed the same file.
#[test]
fn undoing_a_commit_leaves_a_set_accepted_below_it_on_the_same_file() {
    let case = Case::new();
    keeps_activity(&case);
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let already = case.turn(TurnOutcome::Succeeded).unwrap();
    let tip = case.owner_commit("README", "one\ntwo\nthree\nfour\n", "Owner takes the line");
    let AcceptOutcome::Landed { commit, .. } = case.accept(&already).unwrap() else {
        panic!("the set already on main conflicted");
    };
    assert_eq!(commit, tip, "the Accept made no commit");
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(
        &other.worktree.join("README"),
        "one\ntwo\nthree\nfour\nfive\n",
    );
    let later = turn_of(&case, &other, "Add five");
    assert!(matches!(
        case.accept(&later).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert!(matches!(
        undo_landing(&case.db, &case.home, &later).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        store::load(&case.db, &already).unwrap().undone,
        None,
        "the undo left this set's line in place"
    );
}

/// Land-6 r2: a set marked undone keeps the commit that really reverted it. A later undo of
/// another commit on the same file finds its work already gone and does not rewrite that.
#[test]
fn undoing_a_second_commit_keeps_the_first_undo_of_a_set() {
    let case = Case::new();
    keeps_activity(&case);
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let same = case.turn(TurnOutcome::Succeeded).unwrap();
    let older = readme_landed_by(&case, 3, "one\ntwo\nthree\nfour\n");
    let newer = readme_landed_by(&case, 4, "zero\none\ntwo\nthree\nfour\n");
    assert!(matches!(
        case.accept(&same).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    let AcceptOutcome::Landed { commit: first, .. } =
        undo_landing(&case.db, &case.home, &older).unwrap()
    else {
        panic!("the first undo conflicted");
    };
    assert_eq!(
        store::load(&case.db, &same).unwrap().undone.as_deref(),
        Some(first.as_str()),
        "that undo removed this set's line"
    );
    assert!(matches!(
        undo_landing(&case.db, &case.home, &newer).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        store::load(&case.db, &same).unwrap().undone.as_deref(),
        Some(first.as_str()),
        "the later undo rewrote the first one"
    );
}

/// LAND-4 L1: a set whose recorded commit the repository no longer holds sits above nothing,
/// so an undo leaves it Accepted, as a landed commit that is gone counts as not landed.
#[test]
fn undoing_a_commit_leaves_a_set_whose_recorded_commit_is_gone() {
    let case = Case::new();
    let (same, maker) = same_change_accepted(&case, &[], true);
    case.db
        .execute(
            "UPDATE landing_change_sets SET landed=?1 WHERE id=?2",
            ["0123456789abcdef0123456789abcdef01234567", same.as_str()],
        )
        .unwrap();
    assert!(matches!(
        undo_landing(&case.db, &case.home, &maker).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(store::load(&case.db, &same).unwrap().undone, None);
}

/// LAND-4 L1: an undo moves one branch. A set accepted with no commit at a commit only
/// another branch holds still has its change there, so it keeps its Accept.
#[test]
fn undoing_a_commit_leaves_a_set_accepted_on_another_branch() {
    let case = Case::new();
    keeps_activity(&case);
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let same = case.turn(TurnOutcome::Succeeded).unwrap();
    let other = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &other.repository, &other.worktree, &other.git_dir).unwrap();
    write(&other.worktree.join("README"), "one\ntwo\nthree\nfour\n");
    let maker = turn_of(&case, &other, "Add the line");
    assert!(matches!(
        case.accept(&maker).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    let tree = case.owner(&["rev-parse", "main^{tree}"]);
    let on_stage = case.owner(&["commit-tree", tree.trim(), "-p", "main", "-m", "On stage"]);
    let on_stage = on_stage.trim().to_owned();
    case.owner(&["branch", "stage", &on_stage]);
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute(
            "INSERT INTO team_land_on(project,branch) VALUES (?1,'stage')",
            [case.workspace.project.as_str()],
        )
        .unwrap();
    let AcceptOutcome::Landed { commit, branch } = case.accept(&same).unwrap() else {
        panic!("the set on stage conflicted");
    };
    assert_eq!(
        (commit, branch),
        (on_stage, "stage".to_owned()),
        "no commit, on stage"
    );
    assert!(matches!(
        undo_landing(&case.db, &case.home, &maker).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(
        store::load(&case.db, &same).unwrap().undone,
        None,
        "stage still holds the change"
    );
}

#[test]
fn an_undo_reverts_on_the_branch_the_set_landed_on_after_land_on_changes() {
    let case = Case::new();
    keeps_activity(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let AcceptOutcome::Landed { commit: landed, .. } = case.accept(&id).unwrap() else {
        panic!("the accept conflicted");
    };
    case.owner(&["branch", "stage"]);
    crate::team::initialize(&case.db).unwrap();
    case.db
        .execute(
            "INSERT INTO team_land_on(project,branch) VALUES (?1,'stage')",
            [case.workspace.project.as_str()],
        )
        .unwrap();

    let AcceptOutcome::Landed { commit, branch } = undo_landing(&case.db, &case.home, &id).unwrap()
    else {
        panic!("the undo conflicted");
    };

    assert_eq!(branch, "main", "the undo goes where the set landed");
    assert_eq!(case.owner(&["rev-parse", "main"]).trim(), commit);
    assert_eq!(
        case.owner(&["rev-parse", "stage"]).trim(),
        landed,
        "Land on's branch is left alone"
    );
}

/// A master's worktree start is recorded once: the call each master turn makes keeps the
/// stored start after the master committed, so its next turn end still holds that commit
/// in its change set (T3 r2).
#[test]
fn a_master_worktree_start_is_recorded_once_and_never_moves_past_its_commits() {
    let case = Case::new();
    case.start();
    let master = master_agent(&case);
    let start_of = || {
        record_master_start(
            &case.db,
            &master.repository,
            &master.worktree,
            &master.git_dir,
        )
        .unwrap()
    };
    start_of();
    let in_master = |args: &[&str]| git_in(&case.home, &master.worktree, args);
    let start = in_master(&["rev-parse", "HEAD"]).trim().to_owned();
    write(&master.worktree.join("NOTES"), "master notes\n");
    in_master(&["add", "NOTES"]);
    in_master(&[
        "-c",
        "user.name=Master",
        "-c",
        "user.email=master@example.invalid",
        "commit",
        "-q",
        "-m",
        "Add notes",
    ]);
    start_of();
    let id = record_turn_end(&case.db, &master, &merge_turn())
        .unwrap()
        .expect("the master's own commit makes its change set");
    let set = case.view(&id);
    assert_eq!(
        (set.base.as_str(), set.files),
        (start.as_str(), 1),
        "the start never moved past the master's commit"
    );
}

/// T3-R2-S1: a master turn reads its stored start without the repository hold, so a long
/// holder of that hold never delays the turn.
#[test]
fn a_master_turn_with_a_stored_start_does_not_wait_for_the_repository_hold() {
    let case = Case::new();
    case.start();
    let master = master_agent(&case);
    record_master_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    let held = flow::hold(&master.repository);
    let engine = case.repo.parent().unwrap().join("engine");
    let home = git::OWNER_HOME.with(|home| home.borrow().clone());
    let turn = master.clone();
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        git::OWNER_HOME.with(|owner| *owner.borrow_mut() = home);
        let db = open(&engine).unwrap();
        let _ = sent.send(record_master_start(
            &db,
            &turn.repository,
            &turn.worktree,
            &turn.git_dir,
        ));
    });
    let waited = got.recv_timeout(Duration::from_secs(30));
    drop(held);
    assert!(matches!(waited, Ok(Ok(()))), "the turn waited: {waited:?}");
}

/// T3-R2-S2: a master that only fast-forwarded its worktree to main has nothing to land, so
/// its turn end opens no empty set.
#[test]
fn a_master_that_only_fast_forwarded_to_main_opens_no_change_set() {
    let case = Case::new();
    case.start();
    let master = master_agent(&case);
    record_master_start(
        &case.db,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    case.owner_commit("LATER", "later\n", "Owner adds a file");
    git_in(
        &case.home,
        &master.worktree,
        &["merge", "-q", "--ff-only", "main"],
    );
    assert_eq!(
        record_turn_end(&case.db, &master, &merge_turn()).unwrap(),
        None
    );
}

/// T3-R2-S2: the same holds for a team agent, since the rule sits where every set opens.
#[test]
fn a_team_agent_that_only_fast_forwarded_to_main_opens_no_change_set() {
    let case = Case::new();
    case.start();
    let agent = numbered_agent(&case, 3);
    record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir).unwrap();
    case.owner_commit("LATER", "later\n", "Owner adds a file");
    git_in(
        &case.home,
        &agent.worktree,
        &["merge", "-q", "--ff-only", "main"],
    );
    assert_eq!(
        record_turn_end(&case.db, &agent, &merge_turn()).unwrap(),
        None
    );
}

/// LAND-8-L2: a team agent that only fast-forwarded to main across owner commits that the
/// change checks refuse in an agent's own work, a gitlink and a link out of the tree, opens
/// no set and its turn end does not fail: those commits are on main already, so nothing of
/// the agent's is there to check.
#[test]
fn a_fast_forward_across_owner_commits_the_change_checks_refuse_opens_no_set() {
    for owner_change in ["gitlink", "link out of the tree"] {
        let case = Case::new();
        case.start();
        let agent = numbered_agent(&case, 3);
        record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir)
            .unwrap();
        if owner_change == "gitlink" {
            let tip = case.tip();
            case.owner(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{tip},vendor"),
            ]);
        } else {
            std::os::unix::fs::symlink("/etc", case.repo.join("outside")).unwrap();
            case.owner(&["add", "outside"]);
        }
        case.owner(&["commit", "-q", "-m", "Owner adds one"]);
        git_in(
            &case.home,
            &agent.worktree,
            &["merge", "-q", "--ff-only", "main"],
        );
        assert_eq!(
            record_turn_end(&case.db, &agent, &merge_turn()).map_err(|problem| problem.0),
            Ok(None),
            "{owner_change}"
        );
    }
}

/// Starts team agent 3, has the owner commit a plain file on main and the agent fast-forward
/// onto it, as a turn that only took main in leaves the agent. Input: the case. Output: the
/// agent's workspace.
fn fast_forwarded_agent(case: &Case) -> Workspace {
    case.start();
    let agent = numbered_agent(case, 3);
    record_worktree_start(&case.db, &agent.repository, &agent.worktree, &agent.git_dir).unwrap();
    case.owner_commit("LATER", "later\n", "Owner adds a file");
    git_in(
        &case.home,
        &agent.worktree,
        &["merge", "-q", "--ff-only", "main"],
    );
    agent
}

/// Commits everything in an agent's worktree as the agent. Inputs: the case, the agent's
/// workspace and the message.
fn commit_as_agent(case: &Case, agent: &Workspace, message: &str) {
    git_in(&case.home, &agent.worktree, &["add", "-A"]);
    git_in(
        &case.home,
        &agent.worktree,
        &[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

/// rv-secreview land-17 L2: the fast-forward skip covers only a head already on the landing
/// tip, so a link out of the tree that the agent commits after a fast-forward is still
/// refused at its turn end.
#[test]
fn an_agent_link_out_of_the_tree_committed_after_a_fast_forward_is_refused() {
    let case = Case::new();
    let agent = fast_forwarded_agent(&case);
    std::os::unix::fs::symlink("/etc", agent.worktree.join("outside")).unwrap();
    commit_as_agent(&case, &agent, "Agent links out");
    assert_eq!(
        refused(record_turn_end(&case.db, &agent, &merge_turn())),
        "changes_link_outside"
    );
}

/// rv-secreview land-17 L2: the same link left uncommitted after a fast-forward is refused by
/// the commit that takes the worktree's files in.
#[test]
fn an_agent_link_out_of_the_tree_left_uncommitted_after_a_fast_forward_is_refused() {
    let case = Case::new();
    let agent = fast_forwarded_agent(&case);
    std::os::unix::fs::symlink("/etc", agent.worktree.join("outside")).unwrap();
    assert_eq!(
        refused(record_turn_end(&case.db, &agent, &merge_turn())),
        "changes_link_outside"
    );
}

/// rv-code land-17: a plain commit the agent makes after a fast-forward opens a set.
#[test]
fn a_plain_agent_commit_after_a_fast_forward_opens_a_set() {
    let case = Case::new();
    let agent = fast_forwarded_agent(&case);
    write(&agent.worktree.join("own.txt"), "own\n");
    commit_as_agent(&case, &agent, "Agent adds a file");
    assert!(
        record_turn_end(&case.db, &agent, &merge_turn())
            .unwrap()
            .is_some()
    );
}

/// T3-R2-S4: an ordinary Accept's stored intent keeps the shape an older engine reads, and a
/// master's still carries its mark.
#[test]
fn an_ordinary_land_intent_is_stored_in_the_shape_an_older_engine_reads() {
    let land = |master| store::Intent::Land {
        reference: "refs/heads/main".into(),
        old: "a".into(),
        new: "b".into(),
        master,
    };
    let stored = serde_json::to_value(land(false)).unwrap();
    assert!(stored.get("master").is_none(), "{stored}");
    let marked = serde_json::to_string(&land(true)).unwrap();
    assert_eq!(
        serde_json::from_str::<store::Intent>(&marked).unwrap(),
        land(true)
    );
}

/// G24 r1 L1: a reset's stored copy of a set that was never regenerated keeps the shape an
/// older engine reads after a rollback, and a regenerated set's copy still carries its fields.
#[test]
fn a_reset_copy_of_a_set_never_regenerated_is_stored_in_the_shape_an_older_engine_reads() {
    let case = Case::new();
    case.edit("README", "one\ntwo\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let mut record = store::load(&case.db, &id).unwrap();
    assert_eq!(record.regenerated_head, None);
    assert!(record.regenerated_files.is_empty());
    let stored = serde_json::to_value(store::Earlier::of(&record)).unwrap();
    assert!(stored.get("regenerated_head").is_none(), "{stored}");
    assert!(stored.get("regenerated_files").is_none(), "{stored}");
    record.regenerated_head = Some(record.head.clone());
    record.regenerated_files = vec!["README".into()];
    let regenerated = store::Earlier::of(&record);
    let text = serde_json::to_string(&regenerated).unwrap();
    assert_eq!(
        serde_json::from_str::<store::Earlier>(&text).unwrap(),
        regenerated
    );
}

/// A set's proposed message names only the agent's own commits: owner commits the agent
/// brought in by merging main are already on main, so they never become its subject or
/// its list (T4 r2: an undo named the owner's commit).
#[test]
fn a_set_message_leaves_out_main_commits_its_agent_merged_in() {
    let case = Case::new();
    case.start();
    let tip = case.owner_commit("owner.txt", "owner\n", "Owner work");
    case.agent(&["merge", "-q", "--ff-only", &tip]);
    assert_eq!(case.agent_head(), tip);
    case.edit("own.txt", "own\n");
    case.agent_commit("Agent work");
    let id = case
        .turn(TurnOutcome::Succeeded)
        .expect("the agent's own commit makes its change set");
    assert_eq!(case.view(&id).message, "Agent work");
}

/// Plants `edit` to run inside every engine reset of the case's agent worktree, on any
/// thread, after its caller checked the worktree held nothing new (RESET-KEEP). Inputs: the
/// case and the edit. Output: how many resets ran it so far.
fn plant_inside_resets(
    case: &Case,
    edit: impl Fn() + Send + Sync + 'static,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    plant(&flow::BEFORE_RESET, &case.workspace.git_dir, edit)
}

/// Plants `edit` in `planted` for an agent git folder, counting its runs. Inputs: the
/// seam, the git folder and the edit. Output: how many resets ran it so far.
fn plant(
    planted: &std::sync::Mutex<Vec<(PathBuf, flow::PlantedEdit)>>,
    git_dir: &Path,
    edit: impl Fn() + Send + Sync + 'static,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&runs);
    planted.lock().unwrap().push((
        git_dir.to_path_buf(),
        std::sync::Arc::new(move || {
            edit();
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    ));
    runs
}

/// Plants a write of "agent edit" to the case's README inside every engine reset of its
/// agent worktree. Input: the case. Output: how many resets ran it so far.
fn edit_inside_resets(case: &Case) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let file = case.workspace.worktree.join("README");
    plant_inside_resets(case, move || write(&file, "agent edit\n"))
}

/// Removes what [`plant_inside_resets`] planted for the case and returns how many resets
/// ran it. Inputs: the case and the count. Output: the runs.
fn unplant(case: &Case, runs: &std::sync::atomic::AtomicUsize) -> usize {
    unplant_from(&case.workspace.git_dir, runs)
}

/// Removes every edit planted for an agent git folder, in both seams, and returns how many
/// resets ran the one counted by `runs`. Inputs: the git folder and the count. Output: the
/// runs.
fn unplant_from(git_dir: &Path, runs: &std::sync::atomic::AtomicUsize) -> usize {
    for planted in [
        &flow::BEFORE_RESET,
        &flow::AFTER_READ_TREE,
        &flow::AFTER_HEAD_MOVE,
    ] {
        planted
            .lock()
            .unwrap()
            .retain(|(planted, _)| planted != git_dir);
    }
    runs.load(std::sync::atomic::Ordering::SeqCst)
}

/// Asserts an engine reset ran the planted README edit, the edit is still in the agent's
/// worktree, its HEAD is still `head` and the set holds no half-done step. Inputs: the
/// case, the set, the head kept and the run count.
fn assert_edit_kept(
    case: &Case,
    id: &ChangeSetId,
    head: &str,
    runs: &std::sync::atomic::AtomicUsize,
) {
    assert!(unplant(case, runs) >= 1, "no engine reset ran");
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        "agent edit\n",
        "the engine's reset overwrote the agent's edit"
    );
    assert_eq!(case.agent_head(), head);
    assert!(store::load(&case.db, id).unwrap().intent.is_none());
}

/// The start the store records for the case's agent worktree. Input: the case. Output: it.
fn recorded_start(case: &Case) -> Option<String> {
    store::base_of(&case.db, case.workspace.worktree.to_str().unwrap()).unwrap()
}

/// An agent edit written inside a discard's reset, after its clean check, is never
/// overwritten: the worktree keeps the set's head and its next set opens above it.
#[test]
fn an_agent_edit_inside_a_discard_reset_survives_and_the_worktree_keeps_its_head() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let runs = edit_inside_resets(&case);
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_edit_kept(&case, &id, &head, &runs);
    assert_eq!(recorded_start(&case), Some(head));
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
}

/// An agent edit written inside the reset after its set landed is never overwritten: the
/// set lands, the worktree keeps the set's head and its next set opens above it.
#[test]
fn an_agent_edit_inside_the_reset_after_accept_survives_and_the_worktree_keeps_its_head() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.owner_commit("README", "one\ntwo\nthree\nowner\n", "Owner edit");
    let runs = edit_inside_resets(&case);
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_edit_kept(&case, &id, &head, &runs);
    assert_eq!(recorded_start(&case), Some(head));
    assert_eq!(case.view(&id).state, ChangeState::Accepted);
}

/// An agent edit written inside a restore's reset is never overwritten: the restore is
/// refused and the set, its refs and the worktree stay as the discard left them.
#[test]
fn an_agent_edit_inside_a_restore_reset_survives_and_the_set_stays_discarded() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    let tip = case.tip();
    let before = store::load(&case.db, &id).unwrap();
    let runs = edit_inside_resets(&case);
    assert_eq!(
        refused(restore(&case.db, &id, NOW_MS + DAY_MS)),
        "changes_agent_busy"
    );
    assert_edit_kept(&case, &id, &tip, &runs);
    assert_eq!(recorded_start(&case), Some(tip));
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
    assert_eq!(
        case.owner(&["rev-parse", &flow::discarded_ref(&id)]).trim(),
        head
    );
    assert_eq!(
        case.owner(&["for-each-ref", "refs/crazyagent/pending/"]),
        ""
    );
}

/// An agent edit written inside an update's reset is never overwritten: the update is
/// refused and the set and its pending ref stay as they were.
#[test]
fn an_agent_edit_inside_an_update_reset_survives_and_the_update_is_refused() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    let before = store::load(&case.db, &id).unwrap();
    let runs = edit_inside_resets(&case);
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_edit_kept(&case, &id, &before.head, &runs);
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
    assert_eq!(
        case.owner(&["rev-parse", &flow::pending_ref(&id)]).trim(),
        before.head
    );
}

/// An agent edit written inside the reset an engine start finishes for a discard an
/// engine stop cut short is never overwritten: the worktree keeps the set's head.
#[test]
fn an_agent_edit_inside_the_reset_an_engine_start_finishes_survives() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let mut stopped = store::load(&case.db, &id).unwrap();
    stopped.state = ChangeState::Discarded;
    stopped.restorable_until_ms = Some(NOW_MS + 7 * DAY_MS);
    stopped.intent = Some(store::Intent::Reset {
        from: Some(head.clone()),
        target: case.tip(),
        earlier: None,
    });
    store::save(&case.db, &stopped).unwrap();
    let runs = edit_inside_resets(&case);
    owner::settled(&case.db, &id).unwrap();
    assert_edit_kept(&case, &id, &head, &runs);
    assert_eq!(recorded_start(&case), Some(head));
}

const APP_MANIFEST: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n\n[features]\ndefault = []\n\n[dev-dependencies]\n";

/// The Cargo.lock Cargo writes for the case's workspace of `app`, `a` and `b`, with `app`
/// depending on the names given. Input: the names. Output: the lockfile text.
fn cargo_lock(app_dependencies: &[&str]) -> String {
    let mut text = String::from(
        "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    if !app_dependencies.is_empty() {
        text.push_str("dependencies = [\n");
        for name in app_dependencies {
            text.push_str(&format!(" \"{name}\",\n"));
        }
        text.push_str("]\n");
    }
    text.push_str("\n[[package]]\nname = \"b\"\nversion = \"0.1.0\"\n");
    text
}

/// Makes the case's repository a Cargo workspace of an `app` crate and two path crates `a`
/// and `b` with its lockfile, seats the case's agent as team sub-agent `CLONE_AGENT` so it
/// can be sent notes, and starts its turn there. Input: the case. Output: none.
fn cargo_workspace(case: &mut Case) {
    seat_team_agent(case);
    let crate_manifest = |name: &str| {
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
    };
    for (path, text) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"app\", \"a\", \"b\"]\nresolver = \"2\"\n".to_owned(),
        ),
        ("app/Cargo.toml", APP_MANIFEST.to_owned()),
        ("a/Cargo.toml", crate_manifest("a")),
        ("b/Cargo.toml", crate_manifest("b")),
        ("app/src/lib.rs", String::new()),
        ("a/src/lib.rs", String::new()),
        ("b/src/lib.rs", String::new()),
        ("Cargo.lock", cargo_lock(&[])),
    ] {
        write(&case.repo.join(path), &text);
    }
    case.owner(&["add", "-A"]);
    case.owner(&["commit", "-q", "-m", "Start a Cargo workspace"]);
    case.agent(&["checkout", "-q", "--detach", "main"]);
    case.start();
}

/// The notes the engine left in the owner's team conversation, oldest first, read every
/// 50 ms until one is there or `within` passes. Inputs: the case and the time. Output: the
/// texts.
#[cfg(target_os = "macos")]
fn owner_notes_within(case: &Case, within: Duration) -> Vec<String> {
    let started = Instant::now();
    loop {
        let notes: Vec<String> = case
            .db
            .prepare("SELECT text FROM team_messages WHERE project=?1 AND state='note' ORDER BY id")
            .unwrap()
            .query_map([case.workspace.project.as_str()], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        if !notes.is_empty() || started.elapsed() >= within {
            return notes;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What the owner reads once a set they heard waits in Changes went back to its agent.
/// Input: the set. Output: the note.
#[cfg(target_os = "macos")]
fn went_back_note(set: &ChangeSetView) -> String {
    format!(
        "The edits from {} went back to it to take in newer work, so they are not waiting for you in Changes yet.",
        set.agent.label
    )
}

/// The agent adds a dev-dependency of `app` and the owner then adds a dependency of `app` on
/// crate `a` on main, each with its own Cargo.lock, so the lockfile alone clashes. Inputs:
/// the case, the agent's manifest line and the name it locks. Output: the agent's change
/// set.
fn lockfile_clash(case: &Case, line: &str, name: &str) -> ChangeSetId {
    case.edit(
        "app/Cargo.toml",
        &APP_MANIFEST.replace(
            "[dev-dependencies]\n",
            &format!("[dev-dependencies]\n{line}\n"),
        ),
    );
    case.edit("Cargo.lock", &cargo_lock(&[name]));
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    write(
        &case.repo.join("app/Cargo.toml"),
        &APP_MANIFEST.replace(
            "[dependencies]\n",
            "[dependencies]\na = { path = \"../a\" }\n",
        ),
    );
    write(&case.repo.join("Cargo.lock"), &cargo_lock(&["a"]));
    case.owner(&["add", "-A"]);
    case.owner(&["commit", "-q", "-m", "Depend on a"]);
    id
}

/// What a lockfile regeneration left behind: temporary worktrees the owner repository
/// lists or keeps an admin folder for, and the engine's scratch folders. Input: the case.
/// Output: the leftovers, none when it cleaned up.
fn regenerate_leftovers(case: &Case) -> Vec<String> {
    let named = |folder: &Path| -> Vec<String> {
        std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().display().to_string())
            .filter(|path| path.contains("regenerate"))
            .collect()
    };
    let mut left: Vec<String> = case
        .owner(&["worktree", "list", "--porcelain"])
        .lines()
        .filter(|line| line.contains("regenerate"))
        .map(str::to_owned)
        .collect();
    left.extend(named(&case.repo.join(".git/worktrees")));
    left.extend(named(
        &crate::projects_home::state_folder(&case.repo).join("worktrees"),
    ));
    left.extend(named(&case.repo.parent().unwrap().join("engine")));
    left
}

/// Two dependency changes that clash only in Cargo.lock: the engine merges main into the
/// set's head taking main's Cargo.lock, has real cargo rewrite it offline in a temporary
/// worktree confined like an agent, and commits it as its own commit on top of the set with
/// the set's head and main as parents; the set stays Pending on main, its agent is told to
/// reset to the commit, and nothing temporary is left (OS-8).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_lockfile_only_clash_is_rewritten_by_its_tool_as_an_engine_commit_on_top_of_the_set() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    let before = case.view(&id).head;
    let tip = case.tip();
    let mut record = store::load(&case.db, &id).unwrap();
    let regenerated =
        lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None).unwrap();
    let commit = regenerated.expect("a lockfile-only clash is regenerated");
    assert_eq!(
        (
            record.head.as_str(),
            record.base.as_str(),
            record.state,
            record.conflicts.len()
        ),
        (commit.as_str(), tip.as_str(), ChangeState::Pending, 0)
    );
    let saved = store::load(&case.db, &id).unwrap();
    assert_eq!(
        (saved.head.as_str(), saved.base.as_str(), saved.state),
        (commit.as_str(), tip.as_str(), ChangeState::Pending),
        "the set is saved on main with the engine's commit"
    );
    assert_eq!(
        case.owner(&["log", "-1", "--format=%P%n%an <%ae>%n%s", &commit]),
        format!(
            "{before} {tip}\nCrazyAgent <engine@crazyagent.invalid>\nRegenerate Cargo.lock on main\n"
        )
    );
    let lock = case.owner(&["show", &format!("{commit}:Cargo.lock")]);
    assert!(
        lock.contains(
            "name = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"a\",\n \"b\",\n]\n"
        ) && !lock.contains("<<<<<<<"),
        "cargo locked both dependencies:\n{lock}"
    );
    let manifest = case.owner(&["show", &format!("{commit}:app/Cargo.toml")]);
    assert!(
        manifest.contains("a = { path = \"../a\" }")
            && manifest.contains("b = { path = \"../b\" }"),
        "both manifest changes merged:\n{manifest}"
    );
    assert_eq!(
        case.owner(&["rev-parse", &flow::pending_ref(&id)]).trim(),
        commit
    );
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
    assert_eq!(
        case.agent_head(),
        commit,
        "the engine moved the agent's worktree to its commit"
    );
    assert_eq!(
        engine_notes(&case),
        [format!(
            "The engine regenerated Cargo.lock on top of your work as {commit}."
        )]
    );
}

/// When the lockfile's tool fails, here cargo offline on a crate it has never fetched, the
/// set, main and the store stay as they were, no note goes out and the temporary worktree
/// and scratch folders are gone, so Task 7's update can hand the clash to the agent.
#[test]
fn a_lockfile_its_tool_cannot_rewrite_changes_nothing_and_leaves_nothing_behind() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let id = lockfile_clash(&case, "zzz-never-fetched = \"1\"", "zzz-never-fetched");
    let tip = case.tip();
    let mut record = store::load(&case.db, &id).unwrap();
    let before = (record.head.clone(), record.base.clone(), record.state);
    assert_eq!(
        lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None).unwrap(),
        None
    );
    assert_eq!(
        (record.head.clone(), record.base.clone(), record.state),
        before
    );
    let saved = store::load(&case.db, &id).unwrap();
    assert_eq!((saved.head, saved.base, saved.state), before);
    assert_eq!(case.tip(), tip, "main does not move");
    assert!(engine_notes(&case).is_empty(), "no note goes out");
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// A clash in a file that is not one of the named lockfiles is not the engine's to rewrite:
/// nothing changes and nothing is left behind.
#[test]
fn a_clash_beyond_the_named_lockfiles_regenerates_nothing() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    case.edit("README", "one\ntwo\nthree from the agent\n");
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    case.owner_commit(
        "README",
        "one\ntwo\nthree from the owner\n",
        "Owner edits three",
    );
    let mut record = store::load(&case.db, &id).unwrap();
    let head = record.head.clone();
    assert_eq!(
        lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None).unwrap(),
        None
    );
    assert_eq!(record.head, head);
    assert_eq!(store::load(&case.db, &id).unwrap().head, head);
    assert!(engine_notes(&case).is_empty());
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// OWNER-PORTS-FAIL-OPEN r1 L1: a lockfile's tool runs on the project's open network but
/// never listens on the owner's configured local model port, while another free port stays
/// open to it, and owner settings that do not read refuse the regeneration instead of
/// running the tool with that port unknown: nothing changes and nothing is left behind.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_lockfile_tool_never_listens_on_a_configured_model_port_and_unread_settings_refuse_it() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let (kept, other) = {
        let first = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        let second = crate::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
        (
            first.local_addr().unwrap().port(),
            second.local_addr().unwrap().port(),
        )
    };
    let probe = format!(
        "probe() {{\nif /usr/bin/perl -MIO::Socket::INET -e 'exit(IO::Socket::INET->new(LocalAddr => \"127.0.0.1\", LocalPort => $ARGV[0], Listen => 1) ? 0 : 1)' \"$2\"; then echo \"# $1 listened\" >> Cargo.lock; else echo \"# $1 refused\" >> Cargo.lock; fi\n}}\nprobe kept {kept}\nprobe other {other}"
    );
    stand_in_cargo(&case, "", &probe, Duration::from_secs(60));
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    let folder = case.repo.parent().unwrap();
    let settings = (folder.join("global.toml"), folder.join("project.toml"));
    write(&settings.0, "[[[\n");
    let mut record = store::load(&case.db, &id).unwrap();
    let head = record.head.clone();
    assert_eq!(
        lockfiles::regenerate(
            &case.db,
            &mut record,
            &["Cargo.lock".to_owned()],
            Some(&settings)
        )
        .unwrap(),
        None,
        "settings that do not read refuse the regeneration"
    );
    assert_eq!(record.head, head);
    assert_eq!(store::load(&case.db, &id).unwrap().head, head);
    assert!(engine_notes(&case).is_empty());
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
    write(
        &settings.0,
        &format!(
            r#"version = 1
timeout_ms = 2000
allowed_destinations = []

[model_access]
accounts = ["local-account"]
connections = [{{ id = "local-connection", provider = "openai-compatible", harness = "rcli", protocol = "openai-chat", account_ref = "local-account", endpoint = {{ kind = "http", url = "http://127.0.0.1:{kept}/v1" }}, model = {{ id = "qwen2.5:0.5b", author = "openai-compatible" }}, auth = {{ method = "none" }} }}]
pools = [{{ id = "local-pool", members = ["local-connection"], projects = [], roles = [], fallback_allowed = false }}]
"#
        ),
    );
    std::fs::set_permissions(&settings.0, std::fs::Permissions::from_mode(0o600)).unwrap();
    let commit = lockfiles::regenerate(
        &case.db,
        &mut record,
        &["Cargo.lock".to_owned()],
        Some(&settings),
    )
    .unwrap()
    .expect("readable settings let the regeneration run");
    let lock = case.owner(&["show", &format!("{commit}:Cargo.lock")]);
    assert!(
        lock.contains("# kept refused\n") && lock.contains("# other listened\n"),
        "the tool listens only where the owner's model server does not:\n{lock}"
    );
}

const TURN_END_WITHIN: Duration = Duration::from_secs(20);
const REGENERATED_WITHIN: Duration = Duration::from_secs(120);
#[cfg(target_os = "macos")]
const OWNER_NOTE_WITHIN: Duration = Duration::from_secs(5);

/// Ends the agent's turn the way a team turn does, on its own thread and store connection,
/// and waits at most `within` for it to return. Inputs: the case, the turn id and the time.
/// Output: what the turn end reported, or None when it had not returned in time.
fn turn_end_within(
    case: &Case,
    turn: &str,
    within: Duration,
) -> Option<crate::landing_turns::Landed> {
    let engine = case.repo.parent().unwrap().join("engine");
    let workspace = case.workspace.clone();
    let home = git::OWNER_HOME.with(|home| home.borrow().clone());
    let end = TurnEnd {
        outcome: TurnOutcome::Succeeded,
        turn: turn.into(),
        prompt_line: "Depend on b".into(),
    };
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        git::OWNER_HOME.with(|owner| *owner.borrow_mut() = home);
        let db = open(&engine).unwrap();
        let _ = sent.send(crate::landing_turns::record(&db, &workspace, &end, None).unwrap());
    });
    got.recv_timeout(within).ok()
}

/// The owner's dependency of `app` on crate `a` lands on main first, with any stand-in tool
/// written beside it; the agent then adds a dev-dependency with its own Cargo.lock and its
/// turn ends the way a team turn does. Inputs: the case, the agent's manifest line and the
/// name it locks. Output: the agent's change set and what its turn end reported, None when
/// it had not returned within `TURN_END_WITHIN`.
fn lockfile_clash_at_turn_end(
    case: &Case,
    line: &str,
    name: &str,
) -> (ChangeSetId, Option<crate::landing_turns::Landed>) {
    write(
        &case.repo.join("app/Cargo.toml"),
        &APP_MANIFEST.replace(
            "[dependencies]\n",
            "[dependencies]\na = { path = \"../a\" }\n",
        ),
    );
    write(&case.repo.join("Cargo.lock"), &cargo_lock(&["a"]));
    case.owner(&["add", "-A"]);
    case.owner(&["commit", "-q", "-m", "Depend on a"]);
    case.edit(
        "app/Cargo.toml",
        &APP_MANIFEST.replace(
            "[dev-dependencies]\n",
            &format!("[dev-dependencies]\n{line}\n"),
        ),
    );
    case.edit("Cargo.lock", &cargo_lock(&[name]));
    let landed = turn_end_within(case, "turn-1", TURN_END_WITHIN);
    let id = list(&case.db, &case.workspace.project)
        .unwrap()
        .first()
        .expect("the agent's turn kept a change set")
        .id
        .clone();
    (id, landed)
}

/// Reads the case's set every 50 ms until `done` holds with no step of the engine's left
/// half done (its stored intent cleared, so an update or a regeneration has also moved the
/// agent's worktree) or `within` passes. Inputs: the case, the set, the time and the
/// condition. Output: the set as last read.
fn set_within(
    case: &Case,
    id: &ChangeSetId,
    within: Duration,
    done: impl Fn(&ChangeSetView) -> bool,
) -> ChangeSetView {
    let deadline = Instant::now() + within;
    loop {
        let set = case.view(id);
        let settled = store::load(&case.db, id).unwrap().intent.is_none();
        if (done(&set) && settled) || Instant::now() >= deadline {
            return set;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Tells whether a set's head is the engine's regeneration of its Cargo.lock. Inputs: the
/// case and the set. Output: the answer.
fn regenerated(case: &Case, set: &ChangeSetView) -> bool {
    case.owner(&["log", "-1", "--format=%s", &set.head]).trim() == "Regenerate Cargo.lock on main"
}

/// A word unique to the case, for finding the processes its stand-in tool started. Input:
/// the case. Output: the word.
fn token(case: &Case) -> String {
    let folder = case.repo.parent().unwrap().file_name().unwrap();
    format!("ca-t8-{}", folder.to_string_lossy().trim_start_matches('.'))
}

/// Writes a stand-in `cargo` at `tools/cargo` in the owner's checkout, to land with the
/// owner's next commit, and puts that folder first on this case's tool PATH with `time` for
/// the whole regeneration. It runs `on_version` and prints a version for `--version`, and
/// otherwise writes the Cargo.lock cargo writes for both dependencies, then runs
/// `on_update`. Inputs: the case, the two shell snippets and the time. Output: none.
fn stand_in_cargo(case: &Case, on_version: &str, on_update: &str, time: Duration) {
    let lock = cargo_lock(&["a", "b"]);
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n{on_version}\necho 'cargo 1.90.0 (stand-in)'\nexit 0\nfi\ncat > Cargo.lock <<'LOCK'\n{lock}LOCK\n{on_update}\nexit 0\n"
    );
    let tool = case.repo.join("tools/cargo");
    write(&tool, &script);
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    lockfiles::TEST_TOOLS
        .lock()
        .unwrap()
        .insert(case.repo.clone(), ("tools".to_owned(), time));
}

/// A file outside the repository with bytes unique to the case. Input: the case. Output:
/// its path and its bytes.
fn outside_secret(case: &Case) -> (PathBuf, String) {
    let outside = case.repo.parent().unwrap().join("outside-secret");
    let secret = format!("outside secret {}\n", token(case));
    write(&outside, &secret);
    (outside, secret)
}

/// Tells whether the owner repository holds an object with exactly these bytes. Inputs:
/// the case and the bytes. Output: the answer.
fn holds_blob(case: &Case, bytes: &str) -> bool {
    let probe = case.repo.parent().unwrap().join("probe-blob");
    write(&probe, bytes);
    let id = case.owner(&["hash-object", "--", probe.to_str().unwrap()]);
    crate::spawn_gate::status(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(&case.repo)
            .args(["cat-file", "-e", id.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap()
    .success()
}

/// Tells whether one commit of the owner repository is an ancestor of another. Inputs: the
/// case and the two commits. Output: the answer.
#[cfg(target_os = "macos")]
fn is_ancestor(case: &Case, older: &str, newer: &str) -> bool {
    crate::spawn_gate::status(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(&case.repo)
            .args(["merge-base", "--is-ancestor", older, newer]),
    )
    .unwrap()
    .success()
}

/// The processes still running whose command line holds `token`, after up to 3 s for them
/// to end. Input: the token. Output: their ids.
fn survivors(token: &str) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let found = crate::spawn_gate::output(
            Command::new("/usr/bin/pgrep")
                .args(["-f", token])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            found.status.code().is_some_and(|code| code <= 1),
            "{found:?}"
        );
        let ids: Vec<String> = String::from_utf8_lossy(&found.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        if ids.is_empty() || Instant::now() >= deadline {
            return ids;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Two agents that each add a dependency clash only in Cargo.lock: the agent's turn end
/// returns at once and the engine rewrites the lockfile with cargo on top of its set, moves
/// the agent's worktree there and tells it, so the set waits for the owner on main with
/// both dependencies locked, and its Accept lands with no conflict shown (OS-8).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_lockfile_only_clash_is_regenerated_on_top_of_the_set_and_the_set_lands() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let (id, landed) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    assert_eq!(
        landed,
        Some(crate::landing_turns::Landed::Added(id.clone()))
    );
    let tip = case.tip();
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        regenerated(&case, set)
    });
    assert_eq!(
        (
            set.state,
            set.waiting,
            set.conflicts.clone(),
            set.base.as_str()
        ),
        (
            ChangeState::Pending,
            false,
            Vec::<String>::new(),
            tip.as_str()
        ),
        "the set waits for the owner on main, not for its agent"
    );
    assert_eq!(
        case.owner(&["log", "-1", "--format=%an <%ae>%n%s", &set.head]),
        "CrazyAgent <engine@crazyagent.invalid>\nRegenerate Cargo.lock on main\n"
    );
    assert_eq!(
        set.regenerated,
        vec!["Cargo.lock".to_owned()],
        "Changes can say the project's tools wrote this lockfile"
    );
    assert_eq!(
        case.owner(&["rev-parse", &format!("{}^2", set.head)])
            .trim(),
        tip
    );
    let lock = case.owner(&["show", &format!("{}:Cargo.lock", set.head)]);
    assert!(
        lock.contains(
            "name = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"a\",\n \"b\",\n]\n"
        ) && !lock.contains("<<<<<<<"),
        "cargo locked both dependencies:\n{lock}"
    );
    assert_eq!(
        case.agent_head(),
        set.head,
        "the engine moved the agent's worktree to its commit"
    );
    let agent_lock = std::fs::read_to_string(case.workspace.worktree.join("Cargo.lock")).unwrap();
    assert_eq!(
        agent_lock, lock,
        "the agent's worktree holds the regenerated lockfile"
    );
    assert_eq!(
        engine_notes(&case),
        [format!(
            "The engine regenerated Cargo.lock on top of your work as {}.",
            set.head
        )]
    );
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
    let outcome = case.accept(&id).unwrap();
    assert!(
        matches!(outcome, AcceptOutcome::Landed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(case.repo.join("Cargo.lock")).unwrap(),
        lock,
        "main holds the regenerated lockfile"
    );
    let manifest = std::fs::read_to_string(case.repo.join("app/Cargo.toml")).unwrap();
    assert!(
        manifest.contains("a = { path = \"../a\" }")
            && manifest.contains("b = { path = \"../b\" }"),
        "both dependencies land:\n{manifest}"
    );
}

/// Once a later turn moves a regenerated set's head, Changes no longer says the project's
/// tools wrote its lockfile, since the agent may have rewritten it (T8-R1-WORDING).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_later_turn_on_a_regenerated_set_drops_the_words_about_the_tools() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        regenerated(&case, set)
    });
    assert_eq!(set.regenerated, vec!["Cargo.lock".to_owned()]);
    case.edit("app/src/lib.rs", "pub fn later() {}\n");
    turn_end_within(&case, "turn-2", TURN_END_WITHIN);
    let later = set_within(&case, &id, TURN_END_WITHIN, |later| later.head != set.head);
    assert_ne!(later.head, set.head);
    assert_eq!(later.regenerated, Vec::<String>::new());
}

/// When cargo cannot rewrite the clashing lockfile, here offline on a crate it never
/// fetched, the turn end falls back to Task 7's update: the set goes back to its agent with
/// main merged into its worktree, no regeneration note goes out and nothing temporary is
/// left (OS-8).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_lockfile_clash_its_tool_cannot_rewrite_goes_back_to_its_agent_at_its_turn_end() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let (id, landed) =
        lockfile_clash_at_turn_end(&case, "zzz-never-fetched = \"1\"", "zzz-never-fetched");
    assert_eq!(
        landed,
        Some(crate::landing_turns::Landed::Added(id.clone()))
    );
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        set.state == ChangeState::Updating
    });
    assert_eq!(
        (set.state, set.conflicts.clone()),
        (ChangeState::Updating, vec!["Cargo.lock".to_owned()])
    );
    let agent_lock = std::fs::read_to_string(case.workspace.worktree.join("Cargo.lock")).unwrap();
    assert!(agent_lock.contains("<<<<<<<"), "{agent_lock}");
    assert!(
        engine_notes(&case)
            .iter()
            .all(|note| !note.contains("regenerated")),
        "no regeneration note goes out"
    );
    assert_eq!(
        owner_notes_within(&case, OWNER_NOTE_WITHIN),
        vec![went_back_note(&set)],
        "the owner, told the edits wait in Changes, hears they went back"
    );
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// Off macOS the engine runs no lockfile tool, since a regeneration is confined by Seatbelt
/// alone. `lockfiles::regenerate` lands nothing for a Cargo.lock-only clash that cargo could
/// rewrite (`Regenerated::Refused`), and at its turn end the set goes back to its agent at
/// once: reported Waiting, with main merged into its worktree and nothing temporary left.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_a_lockfile_only_clash_is_never_regenerated_and_goes_back_to_its_agent() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    let tip = case.tip();
    let mut record = store::load(&case.db, &id).unwrap();
    let before = (record.head.clone(), record.base.clone(), record.state);
    assert_eq!(
        lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None).unwrap(),
        None
    );
    assert_eq!(
        (record.head.clone(), record.base.clone(), record.state),
        before
    );
    assert_eq!(case.tip(), tip, "main does not move");
    assert!(engine_notes(&case).is_empty(), "no note goes out");
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());

    let mut case = Case::new();
    cargo_workspace(&mut case);
    let (id, landed) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    assert_eq!(
        landed,
        Some(crate::landing_turns::Landed::Waiting(id.clone()))
    );
    let set = set_within(&case, &id, TURN_END_WITHIN, |set| {
        set.state == ChangeState::Updating
    });
    assert_eq!(
        (set.state, set.conflicts.clone(), set.regenerated.clone()),
        (
            ChangeState::Updating,
            vec!["Cargo.lock".to_owned()],
            Vec::<String>::new()
        )
    );
    let agent_lock = std::fs::read_to_string(case.workspace.worktree.join("Cargo.lock")).unwrap();
    assert!(agent_lock.contains("<<<<<<<"), "{agent_lock}");
    assert!(
        engine_notes(&case)
            .iter()
            .all(|note| !note.contains("regenerated")),
        "no regeneration note goes out"
    );
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// A lockfile the tool leaves as a link to a file outside the repository, as a folder or as
/// a fifo is never read: the set goes back to its agent, no object holds the outside file's
/// bytes and nothing temporary is left (OS-8).
#[test]
fn a_lockfile_its_tool_leaves_as_a_link_a_folder_or_a_fifo_is_never_read() {
    for leave in [
        "ln -s \"$OUTSIDE\" Cargo.lock",
        "mkdir Cargo.lock",
        "/usr/bin/mkfifo Cargo.lock",
    ] {
        let mut case = Case::new();
        cargo_workspace(&mut case);
        let (outside, secret) = outside_secret(&case);
        stand_in_cargo(
            &case,
            "",
            &format!("OUTSIDE='{}'\nrm -f Cargo.lock\n{leave}", outside.display()),
            Duration::from_secs(60),
        );
        let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
        let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
            set.state == ChangeState::Updating
        });
        assert_eq!(
            set.state,
            ChangeState::Updating,
            "{leave}: the set goes back to its agent"
        );
        assert!(
            !holds_blob(&case, &secret),
            "{leave}: the outside file's bytes were stored"
        );
        assert_eq!(regenerate_leftovers(&case), Vec::<String>::new(), "{leave}");
    }
}

/// A process the tool leaves running in a session of its own, which swaps the lockfile for
/// a link to a file outside the repository, has ended before the lockfile is read: the set
/// goes back to its agent, no object holds the outside file's bytes and no process of the
/// tool's run is left (OS-8).
#[test]
fn a_process_the_tool_leaves_running_has_ended_before_the_lockfile_is_read() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let token = token(&case);
    let (outside, secret) = outside_secret(&case);
    let swap = format!(
        r#"/usr/bin/perl -MPOSIX -e 'my ($outside) = @ARGV; fork and exit; POSIX::setsid(); unlink "Cargo.lock"; symlink $outside, "Cargo.lock"; open my $done, ">", ".swapped"; close $done; my $end = time + 60; sleep 1 while time < $end' '{}' {token}
while [ ! -e .swapped ]; do sleep 0.05; done
rm -f .swapped"#,
        outside.display()
    );
    stand_in_cargo(&case, "", &swap, Duration::from_secs(60));
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        set.state == ChangeState::Updating
    });
    assert_eq!(set.state, ChangeState::Updating);
    assert!(
        !holds_blob(&case, &secret),
        "the outside file's bytes were stored"
    );
    assert_eq!(
        survivors(&token),
        Vec::<String>::new(),
        "every process of the tool's run ends before the lockfile is read"
    );
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// A child of `cargo --version` that keeps its output pipe open in a session of its own
/// never stalls the agent's turn end or keeps the repository held: the turn end returns at
/// once, the regeneration ends within its time and lands, the repository is free after and
/// no process of the run is left (OS-8).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_child_holding_the_version_pipe_never_stalls_the_turn_end_or_the_repository() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let token = token(&case);
    stand_in_cargo(
        &case,
        &format!("/usr/bin/perl -MPOSIX -e 'fork and exit; POSIX::setsid(); sleep 120' {token}"),
        "",
        Duration::from_secs(60),
    );
    let (id, landed) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    assert!(landed.is_some(), "the turn end returned at once");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        regenerated(&case, set) || set.state == ChangeState::Updating
    });
    assert!(regenerated(&case, &set), "{:?}", set.state);
    let repo = case.repo.clone();
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _held = flow::hold(&repo);
        let _ = sent.send(());
    });
    assert!(
        got.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the repository is free once the regeneration ended"
    );
    assert_eq!(survivors(&token), Vec::<String>::new());
}

/// A tool still running when the whole regeneration's time is up is stopped and the set
/// goes back to its agent through Task 7's update, well before the tool would end (OS-8).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_tool_past_the_regeneration_time_falls_back_to_the_agent_update() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    stand_in_cargo(&case, "", "sleep 30", Duration::from_secs(4));
    let started = Instant::now();
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(
        &case,
        &id,
        TURN_END_WITHIN.saturating_sub(started.elapsed()),
        |set| set.state == ChangeState::Updating,
    );
    assert_eq!(
        set.state,
        ChangeState::Updating,
        "past its time the tool is stopped and the agent fixes the clash"
    );
    assert_eq!(
        owner_notes_within(&case, OWNER_NOTE_WITHIN),
        vec![went_back_note(&set)]
    );
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// One lockfile tool a row runs for real on a workspace whose packages are all local, so no
/// registry is asked. Fields: the program, its lockfile, the manifest both sides edit, that
/// manifest and the lockfile at the start, the owner's and the agent's, the other files, what
/// the tool's lockfile must name and what the landed manifest must hold.
#[cfg(target_os = "macos")]
struct RealTool {
    program: &'static str,
    lockfile: &'static str,
    manifest: &'static str,
    start: (String, String),
    owner: (String, String),
    agent: (String, String),
    others: Vec<(&'static str, String)>,
    locks: [&'static str; 2],
    keeps: [&'static str; 2],
}

/// Whether a program runs on the PATH the engine gives its tools. Input: the program.
/// Output: the answer.
#[cfg(target_os = "macos")]
fn runs_for_the_engine(program: &str) -> bool {
    crate::spawn_gate::status(
        Command::new("/usr/bin/env")
            .env_clear()
            .env(
                "PATH",
                "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
            )
            .env("HOME", "/tmp")
            .args([program, "--version"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success())
}

/// Commits the tool's workspace on main, then the owner's and the agent's edits that clash in
/// its lockfile alone, and ends the agent's turn. Inputs: the case and the tool. Output: the
/// agent's change set and what the turn end reported.
#[cfg(target_os = "macos")]
fn tool_clash_at_turn_end(
    case: &mut Case,
    tool: &RealTool,
) -> (ChangeSetId, Option<crate::landing_turns::Landed>) {
    seat_team_agent(case);
    for (path, text) in &tool.others {
        write(&case.repo.join(path), text);
    }
    write(&case.repo.join(tool.manifest), &tool.start.0);
    write(&case.repo.join(tool.lockfile), &tool.start.1);
    case.owner(&["add", "-A"]);
    case.owner(&["commit", "-q", "-m", "Start a workspace"]);
    case.agent(&["checkout", "-q", "--detach", "main"]);
    case.start();
    write(&case.repo.join(tool.manifest), &tool.owner.0);
    write(&case.repo.join(tool.lockfile), &tool.owner.1);
    case.owner(&["add", "-A"]);
    case.owner(&["commit", "-q", "-m", "Depend on a"]);
    case.edit(tool.manifest, &tool.agent.0);
    case.edit(tool.lockfile, &tool.agent.1);
    let landed = turn_end_within(case, "turn-1", TURN_END_WITHIN);
    let id = list(&case.db, &case.workspace.project)
        .unwrap()
        .first()
        .expect("the agent's turn kept a change set")
        .id
        .clone();
    (id, landed)
}

/// A clash in one tool's lockfile alone is rewritten by the real tool in its sandbox, and
/// the set lands with both sides locked; only `cargo update` ran in a row before
/// (T8-R1-TOOLS). A tool this machine does not have fails the row, so a missing tool is
/// never counted as proof. pnpm has no row yet, since it does not regenerate in the tool
/// sandbox today. Input: the tool.
#[cfg(target_os = "macos")]
fn a_real_tool_rewrites_its_clashing_lockfile(tool: &RealTool) {
    let program = tool.program;
    assert!(
        runs_for_the_engine(program),
        "{program} is not installed for the engine, so this row cannot run"
    );
    let mut case = Case::new();
    let (id, landed) = tool_clash_at_turn_end(&mut case, tool);
    assert_eq!(
        landed,
        Some(crate::landing_turns::Landed::Added(id.clone())),
        "{program}"
    );
    let subject = format!("Regenerate {} on main", tool.lockfile);
    let head_subject = |set: &ChangeSetView| {
        case.owner(&["log", "-1", "--format=%s", &set.head])
            .trim()
            .to_owned()
    };
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        set.state == ChangeState::Updating || head_subject(set) == subject
    });
    assert_eq!(
        (head_subject(&set), set.state),
        (subject, ChangeState::Pending),
        "{program}: its lockfile did not land"
    );
    let lock = case.owner(&["show", &format!("{}:{}", set.head, tool.lockfile)]);
    for name in tool.locks {
        assert!(lock.contains(name), "{program}: {name} in\n{lock}");
    }
    assert_eq!(
        regenerate_leftovers(&case),
        Vec::<String>::new(),
        "{program}"
    );
    let outcome = case.accept(&id).unwrap();
    assert!(
        matches!(outcome, AcceptOutcome::Landed { .. }),
        "{program}: {outcome:?}"
    );
    let manifest = std::fs::read_to_string(case.repo.join(tool.manifest)).unwrap();
    for kept in tool.keeps {
        assert!(manifest.contains(kept), "{program}: {kept} in\n{manifest}");
    }
}

/// A package.json whose two dependency lists sit far enough apart for git to merge edits to
/// each. Inputs: the two lists' entries. Output: the text.
#[cfg(target_os = "macos")]
fn node_app(dependencies: &str, dev_dependencies: &str) -> String {
    format!(
        "{{\n  \"name\": \"app\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": {{{dependencies}}},\n  \"private\": true,\n  \"license\": \"MIT\",\n  \"devDependencies\": {{{dev_dependencies}}}\n}}\n"
    )
}

/// The packages a Node workspace links, beside its root manifest. Input: extra root fields.
/// Output: the files.
#[cfg(target_os = "macos")]
fn node_packages(root_fields: &str) -> Vec<(&'static str, String)> {
    vec![
        (
            "package.json",
            format!("{{\"name\":\"root\",\"version\":\"1.0.0\",\"private\":true{root_fields}}}\n"),
        ),
        (
            "packages/a/package.json",
            "{\"name\":\"a\",\"version\":\"1.0.0\"}\n".to_owned(),
        ),
        (
            "packages/b/package.json",
            "{\"name\":\"b\",\"version\":\"1.0.0\"}\n".to_owned(),
        ),
    ]
}

/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn real_npm_rewrites_a_clashing_package_lock() {
    let lock = |version: &str| {
        format!(
            "{{\"name\":\"root\",\"version\":\"{version}\",\"lockfileVersion\":3,\"requires\":true,\"packages\":{{}}}}\n"
        )
    };
    a_real_tool_rewrites_its_clashing_lockfile(&RealTool {
        program: "npm",
        lockfile: "package-lock.json",
        manifest: "packages/app/package.json",
        start: (node_app("", ""), lock("1.0.0")),
        owner: (node_app("\"a\": \"1.0.0\"", ""), lock("1.0.1")),
        agent: (node_app("", "\"b\": \"1.0.0\""), lock("1.0.2")),
        others: node_packages(",\"workspaces\":[\"packages/*\"]"),
        locks: [
            "\"resolved\": \"packages/a\"",
            "\"resolved\": \"packages/b\"",
        ],
        keeps: ["\"a\": \"1.0.0\"", "\"b\": \"1.0.0\""],
    });
}

/// A uv workspace of two local packages whose app the owner and the agent each give one
/// more dependency, so only uv.lock clashes. Output: the workspace.
#[cfg(target_os = "macos")]
fn uv_workspace() -> RealTool {
    let manifest = |dependencies: &str, dev: &str| {
        format!(
            "[project]\nname = \"app\"\nversion = \"1.0.0\"\nrequires-python = \">=3.9\"\ndependencies = [{dependencies}]\n\n[dependency-groups]\ndev = [{dev}]\n\n[tool.uv.sources]\na = {{ workspace = true }}\nb = {{ workspace = true }}\n\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\n"
        )
    };
    let lock = |python: &str| format!("version = 1\nrequires-python = \">={python}\"\n");
    let member = |name: &str| {
        format!(
            "[project]\nname = \"{name}\"\nversion = \"1.0.0\"\nrequires-python = \">=3.9\"\ndependencies = []\n"
        )
    };
    RealTool {
        program: "uv",
        lockfile: "uv.lock",
        manifest: "pyproject.toml",
        start: (manifest("", ""), lock("3.9")),
        owner: (manifest("\"a\"", ""), lock("3.10")),
        agent: (manifest("", "\"b\""), lock("3.11")),
        others: vec![
            ("packages/a/pyproject.toml", member("a")),
            ("packages/b/pyproject.toml", member("b")),
        ],
        locks: ["name = \"a\"", "name = \"b\""],
        keeps: ["dependencies = [\"a\"]", "dev = [\"b\"]"],
    }
}

/// T8 row 6: a clash in uv.lock alone is rewritten by the real uv in the tool sandbox, on a
/// cache the engine primes for that run under the SM1-R rule, and the run leaves nothing
/// behind; land-14 sent it back to its agent unregenerated. Fails without uv.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn real_uv_rewrites_a_clashing_uv_lock() {
    a_real_tool_rewrites_its_clashing_lockfile(&uv_workspace());
}

/// A tool that changes a file other than the clashing lockfiles, here the manifest, lands
/// nothing: the set goes back to its agent through Task 7's update (OS-8).
#[test]
fn a_tool_that_also_rewrites_a_manifest_falls_back_to_the_agent_update() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    stand_in_cargo(
        &case,
        "",
        "printf '# rewritten\\n' >> app/Cargo.toml",
        Duration::from_secs(60),
    );
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        set.state == ChangeState::Updating || regenerated(&case, set)
    });
    assert!(!regenerated(&case, &set), "the rewritten manifest landed");
    assert_eq!(set.state, ChangeState::Updating);
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// A Cargo.lock-only clash of an agent the owner or the master stopped, or of a project
/// whose team the owner paused, is not regenerated and its agent gets no note; the stopped
/// set keeps its head and waits for the owner (OS-8, HB-S5-B, T8-R2-S3).
#[test]
fn an_owner_stopped_agent_or_a_paused_team_gets_no_regeneration_and_no_note() {
    for stop in ["owner stop", "master stop", "paused team"] {
        let mut case = Case::new();
        cargo_workspace(&mut case);
        let project = case.workspace.project.as_str().to_owned();
        if stop != "paused team" {
            let maker = stop.trim_end_matches(" stop");
            case.db
                .execute(
                    "INSERT INTO team_owner_stops(agent,project,stopped_ms,stopped_by) VALUES (?1,?2,1,?3)",
                    rusqlite::params![CLONE_AGENT, project, maker],
                )
                .unwrap();
        } else {
            case.db
                .execute(
                    "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,1)",
                    [project.as_str()],
                )
                .unwrap();
        }
        let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
        let head = case.view(&id).head;
        let set = set_within(&case, &id, Duration::from_secs(10), |set| {
            regenerated(&case, set)
        });
        assert!(!regenerated(&case, &set), "{stop}: the set was regenerated");
        assert!(
            engine_notes(&case)
                .iter()
                .all(|note| !note.contains("regenerated")),
            "{stop}: the agent got a regeneration note"
        );
        assert_eq!(set.head, head, "{stop}: the set keeps its head");
        assert_eq!(
            set.state,
            ChangeState::Pending,
            "{stop}: the set waits for the owner"
        );
    }
}

/// Starts a Cargo.lock-only clash whose stand-in tool is still running when the turn end
/// returns, then runs `during`. Inputs: the case and what happens while the tool runs.
/// Output: the set, its head before the regeneration and what `during` returned.
#[cfg(target_os = "macos")]
fn clash_during_a_slow_tool<T>(
    case: &mut Case,
    during: impl FnOnce(&Case) -> T,
) -> (ChangeSetId, String, T) {
    cargo_workspace(case);
    stand_in_cargo(
        case,
        "",
        ": > .tool-started\nsleep 8\nrm -f .tool-started",
        Duration::from_secs(60),
    );
    let (id, _) = lockfile_clash_at_turn_end(case, "b = { path = \"../b\" }", "b");
    let head = case.view(&id).head;
    let started = crate::projects_home::state_folder(&case.repo)
        .join("worktrees")
        .join(format!("regenerate-{}", id.as_str()))
        .join(".tool-started");
    let deadline = Instant::now() + REGENERATED_WITHIN;
    while !started.exists() {
        assert!(Instant::now() < deadline, "the tool never started");
        std::thread::sleep(Duration::from_millis(20));
    }
    let done = during(case);
    (id, head, done)
}

/// Waits until a set's regeneration has ended. Input: the set. Output: none.
#[cfg(target_os = "macos")]
fn regeneration_ended(id: &ChangeSetId) {
    let deadline = Instant::now() + REGENERATED_WITHIN;
    while lockfiles::is_running(id) {
        assert!(Instant::now() < deadline, "the regeneration never ended");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// T8-R2-S2: the owner's stop while the tool runs lands nothing, keeps the set for the owner
/// and sends the agent no note.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn an_owner_stop_while_the_lockfile_tool_runs_lands_nothing() {
    let mut case = Case::new();
    let (id, head, ()) = clash_during_a_slow_tool(&mut case, |case| {
        case.db
            .execute(
                "INSERT INTO team_owner_stops(agent,project,stopped_ms,stopped_by) VALUES (?1,?2,1,'owner')",
                rusqlite::params![CLONE_AGENT, case.workspace.project.as_str()],
            )
            .unwrap();
    });
    regeneration_ended(&id);
    let set = case.view(&id);
    assert!(!regenerated(&case, &set), "the set was regenerated");
    assert_eq!(
        (set.head.as_str(), set.state),
        (head.as_str(), ChangeState::Pending)
    );
    assert!(
        engine_notes(&case)
            .iter()
            .all(|note| !note.contains("regenerated")),
        "the stopped agent got a regeneration note"
    );
}

/// T8-R2-S2 and S3: a pause while the tool runs lands nothing, and the engine's update waits
/// too, so the paused team's worktree is left alone.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_pause_while_the_lockfile_tool_runs_lands_nothing_and_starts_no_update() {
    let mut case = Case::new();
    let (id, head, ()) = clash_during_a_slow_tool(&mut case, |case| {
        case.db
            .execute(
                "INSERT INTO team_paused_projects(project,since_ms) VALUES (?1,1)",
                [case.workspace.project.as_str()],
            )
            .unwrap();
    });
    regeneration_ended(&id);
    let set = set_within(&case, &id, Duration::from_secs(5), |set| {
        set.state != ChangeState::Pending
    });
    assert!(!regenerated(&case, &set), "the set was regenerated");
    assert_eq!(
        (set.head.as_str(), set.state),
        (head.as_str(), ChangeState::Pending)
    );
}

/// T8-R2-S2: main moving while the tool runs throws that run away and regenerates on the new
/// main.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn main_moving_while_the_lockfile_tool_runs_regenerates_on_the_new_main() {
    let mut case = Case::new();
    let (id, _, moved) = clash_during_a_slow_tool(&mut case, |case| {
        case.owner_commit("LATER", "later\n", "Owner moves main")
    });
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        regenerated(&case, set) && is_ancestor(&case, &moved, &set.head)
    });
    assert!(regenerated(&case, &set), "the set was never regenerated");
    assert!(
        is_ancestor(&case, &moved, &set.head),
        "it ran on the old main"
    );
    assert_eq!(set.state, ChangeState::Pending);
}

/// T8-R2-S2: an edit the agent makes while the tool runs stops the regeneration from landing
/// and survives, on a file the regeneration would not change.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_edit_while_the_lockfile_tool_runs_stays_and_lands_nothing() {
    let mut case = Case::new();
    let (id, _, ()) =
        clash_during_a_slow_tool(&mut case, |case| case.edit("README", "edited mid-run\n"));
    regeneration_ended(&id);
    let set = case.view(&id);
    assert!(!regenerated(&case, &set), "the set was regenerated");
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        "edited mid-run\n"
    );
}

/// rv-code T8 r1 N2: only a uv.lock clash primes uv, so a Cargo.lock clash never runs or
/// waits on uv priming and its tool gets no uv cache, whatever uv the owner has. Red only
/// where the owner's tool PATH finds a uv the engine may run.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_cargo_lock_regeneration_primes_no_uv_cache() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    stand_in_cargo(
        &case,
        "",
        "echo \"# uv cache ${UV_CACHE_DIR:-none}\" >> Cargo.lock",
        Duration::from_secs(60),
    );
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    let mut record = store::load(&case.db, &id).unwrap();
    let commit = lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None)
        .unwrap()
        .expect("the Cargo.lock clash was regenerated");
    let lock = case.owner(&["show", &format!("{commit}:Cargo.lock")]);
    assert!(lock.contains("# uv cache none\n"), "{lock}");
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// T8-R2-S4: past the most regenerations one repository runs at once, a clash goes straight
/// to Task 7's update.
#[test]
fn a_third_lockfile_regeneration_in_one_repository_goes_to_the_agent_update() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    stand_in_cargo(&case, "", "", Duration::from_secs(60));
    let word = token(&case);
    let _busy = [
        lockfiles::occupy(&case.repo, &format!("{word}-1")),
        lockfiles::occupy(&case.repo, &format!("{word}-2")),
    ];
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        set.state == ChangeState::Updating || regenerated(&case, set)
    });
    assert!(!regenerated(&case, &set), "a third regeneration ran");
    assert_eq!(set.state, ChangeState::Updating);
}

/// T8-R2-S4: under that limit a regeneration still runs.
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_second_lockfile_regeneration_in_one_repository_still_runs() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    stand_in_cargo(&case, "", "", Duration::from_secs(60));
    let _busy = lockfiles::occupy(&case.repo, &format!("{}-1", token(&case)));
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        regenerated(&case, set)
    });
    assert!(
        regenerated(&case, &set),
        "the second regeneration never ran"
    );
}

/// The engine's start removes the temporary worktree, its admin folder and the engine's
/// scratch folders a regeneration left when the engine stopped mid-run (OS-8).
#[test]
fn the_engine_start_sweeps_the_regenerations_a_stopped_engine_left() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    crate::storage::initialize_authority_schemas(&case.db).unwrap();
    let set = "0123456789abcdef0123456789abcdef";
    let worktree = crate::projects_home::state_folder(&case.repo)
        .join("worktrees")
        .join(format!("regenerate-{set}"));
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    case.owner(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        worktree.to_str().unwrap(),
        "HEAD",
    ]);
    std::fs::create_dir_all(
        case.repo
            .parent()
            .unwrap()
            .join("engine/regenerate")
            .join(set)
            .join("home"),
    )
    .unwrap();
    assert!(!regenerate_leftovers(&case).is_empty());
    assert_eq!(crate::landing::sweep_regenerations(&case.db).unwrap(), 1);
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
    assert!(
        !case
            .owner(&["worktree", "list", "--porcelain"])
            .contains("regenerate")
    );
}

/// The engine's start sweep removes a link at `.git/worktrees`, or one in it, by itself and
/// before its own `git worktree prune`, which would otherwise empty the folder the link names
/// outside the project; an older build let a Sandboxed process or a Full access agent plant
/// one (SECAUDIT-WORKTREE-ADMIN-PLANT r1 L1). The real admin folders stay.
#[test]
fn the_engine_start_sweep_removes_a_planted_worktrees_link_before_its_prune_follows_it() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    crate::storage::initialize_authority_schemas(&case.db).unwrap();
    let owner = case.repo.parent().unwrap().join("owner-files");
    let files = [owner.join("keep.txt"), owner.join("sub").join("keep.txt")];
    std::fs::create_dir_all(owner.join("sub")).unwrap();
    for file in &files {
        std::fs::write(file, "owner file\n").unwrap();
    }
    let kept = || files.iter().filter(|file| file.is_file()).count();
    let admin = case.repo.join(".git").join("worktrees");
    let planted = admin.join("x");
    std::os::unix::fs::symlink(&owner, &planted).unwrap();
    assert_eq!(crate::landing::sweep_regenerations(&case.db).unwrap(), 0);
    assert!(std::fs::symlink_metadata(&planted).is_err());
    assert_eq!(kept(), 2);
    assert!(
        case.owner(&["worktree", "list", "--porcelain"])
            .contains("agent-1")
    );
    let held = case.repo.join(".git").join("worktrees-held");
    std::fs::rename(&admin, &held).unwrap();
    std::os::unix::fs::symlink(&owner, &admin).unwrap();
    assert_eq!(crate::landing::sweep_regenerations(&case.db).unwrap(), 0);
    assert!(std::fs::symlink_metadata(&admin).is_err());
    assert_eq!(kept(), 2);
    std::fs::rename(&held, &admin).unwrap();
}

/// A regeneration's own merge gate reads the paths main changed too: an owner merge driver
/// on the path main renamed the agent's file to never runs, and nothing is regenerated
/// (OS-8, as Task 7's turn-end check).
#[test]
fn a_merge_driver_on_the_path_main_renamed_the_agent_s_file_to_never_runs_in_a_regeneration() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    case.edit("README", "uno\ntwo\nthree\n");
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    case.owner_commit("README", "eins\ntwo\nthree\n", "Say one in German");
    case.owner(&["mv", "README", "LIESMICH"]);
    case.owner(&["commit", "-q", "-m", "Rename README"]);
    let marker = case.repo.parent().unwrap().join("driver-ran");
    case.owner(&[
        "config",
        "merge.mark.driver",
        &format!("/usr/bin/touch {}", marker.display()),
    ]);
    write(
        &case.repo.join(".git/info/attributes"),
        "LIESMICH merge=mark\n",
    );
    let mut record = store::load(&case.db, &id).unwrap();
    let regenerated =
        lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None).unwrap();
    assert!(
        !marker.exists(),
        "the owner's merge driver ran at the path main renamed the agent's file to"
    );
    assert_eq!(regenerated, None);
    assert_eq!(regenerate_leftovers(&case), Vec::<String>::new());
}

/// An agent that ignores the engine's note and edits on keeps the regenerated lockfile: its
/// worktree already holds the engine's commit, so its next turn end builds on it (OS-8).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn an_agent_that_ignores_the_note_keeps_the_regenerated_lockfile_at_its_next_turn_end() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let set = set_within(&case, &id, REGENERATED_WITHIN, |set| {
        regenerated(&case, set)
    });
    assert!(regenerated(&case, &set));
    case.edit("README", "one\ntwo\nthree\nfour\n");
    assert!(turn_end_within(&case, "turn-2", TURN_END_WITHIN).is_some());
    let next = case.view(&id);
    assert_ne!(next.head, set.head, "the next turn end added the edit");
    assert!(
        is_ancestor(&case, &set.head, &next.head),
        "the agent's next turn end dropped the engine's regenerated lockfile"
    );
}

/// An untracked agent file written inside the reset after its set landed, at a path the
/// landed commit adds, is never overwritten: the worktree keeps the set's head (RESET-KEEP).
#[test]
fn an_untracked_agent_file_where_the_reset_adds_one_survives() {
    let case = Case::new();
    case.edit("src/new.rs", "fn main() {}\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    case.owner_commit("owner.txt", "owner\n", "Owner file");
    let file = case.workspace.worktree.join("owner.txt");
    let runs = plant_inside_resets(&case, move || write(&file, "agent file\n"));
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Landed { .. }
    ));
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("owner.txt")).unwrap(),
        "agent file\n"
    );
    assert_eq!(case.agent_head(), head);
    assert_eq!(recorded_start(&case), Some(head));
}

/// An agent commit made inside a discard's reset is never dropped from its branch: the
/// reset finds HEAD moved and changes nothing, and the next set opens above the set's head,
/// so it holds the commit (RESET-KEEP).
#[test]
fn an_agent_commit_inside_a_discard_reset_stays_on_its_branch() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant_inside_resets(&case, move || {
        write(&worktree.join("later.txt"), "later\n");
        git_in(&home, &worktree, &["add", "later.txt"]);
        git_in(
            &home,
            &worktree,
            &[
                "-c",
                "user.name=Agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-q",
                "-m",
                "Later work",
            ],
        );
    });
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(
        case.agent(&["log", "-1", "--format=%s"]).trim(),
        "Later work"
    );
    assert_eq!(case.agent(&["rev-parse", "HEAD^"]).trim(), head);
    assert_eq!(recorded_start(&case), Some(head));
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
}

/// An agent edit written inside the reset a lockfile regeneration applies, after its clean
/// check, is never overwritten: the regeneration lands nothing, Task 7's update then finds
/// the edit at its own clean check and refuses before any reset, and the set waits for the
/// owner on the agent's own head (RESET-KEEP).
#[test]
fn an_agent_edit_inside_a_regeneration_reset_survives_and_the_set_keeps_its_head() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    stand_in_cargo(&case, "", "", Duration::from_secs(60));
    let file = case.workspace.worktree.join("Cargo.lock");
    let runs = plant_inside_resets(&case, move || write(&file, "agent edit\n"));
    let (id, _) = lockfile_clash_at_turn_end(&case, "b = { path = \"../b\" }", "b");
    let head = case.agent_head();
    let pending = flow::pending_ref(&id);
    let deadline = Instant::now() + REGENERATED_WITHIN;
    while Instant::now() < deadline
        && !(runs.load(std::sync::atomic::Ordering::SeqCst) >= 1
            && store::load(&case.db, &id).unwrap().intent.is_none()
            && case.owner(&["rev-parse", &pending]).trim() == head)
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        unplant(&case, &runs),
        1,
        "only the regeneration reached a reset; Task 7's update refused at its clean check"
    );
    let set = case.view(&id);
    assert_eq!(
        (set.state, set.head.as_str()),
        (ChangeState::Pending, head.as_str())
    );
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("Cargo.lock")).unwrap(),
        "agent edit\n",
        "the engine's reset overwrote the agent's edit"
    );
    assert_eq!(case.agent_head(), head);
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
    assert!(
        engine_notes(&case)
            .iter()
            .all(|note| !note.contains("regenerated")),
        "no regeneration note goes out"
    );
}

/// An index lock that stops an update's reset at its index refresh (a tracked file's stat
/// changed, so git must write the index) leaves no half-done reset behind: the agent's
/// commit made afterwards stays on its branch through the owner's next action, and the set
/// is as it was (RESET-KEEP r2 M1).
#[test]
fn an_agent_commit_after_a_reset_an_index_lock_stopped_stays_through_the_next_owner_action() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    let before = store::load(&case.db, &id).unwrap();
    let lock = case.workspace.git_dir.join("index.lock");
    let held = lock.clone();
    let readme = case.workspace.worktree.join("README");
    let runs = plant_inside_resets(&case, move || {
        write(&held, "");
        std::fs::File::options()
            .write(true)
            .open(&readme)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(946_684_800))
            .unwrap();
    });
    assert!(update(&case.db, &id).is_err());
    assert_eq!(unplant(&case, &runs), 1);
    std::fs::remove_file(&lock).unwrap();
    case.edit("later.txt", "later\n");
    let later = case.agent_commit("Later work");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(
        case.agent_head(),
        later,
        "the agent's commit left its branch"
    );
    assert_eq!(case.agent(&["rev-parse", "HEAD^"]).trim(), before.head);
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
}

/// A reset intent stored before resets recorded where they start never moves the agent's
/// HEAD off a commit the agent made later: the engine drops it, the commit stays and the
/// next set opens above the set's head (RESET-KEEP r2 M1).
#[test]
fn a_reset_intent_stored_without_its_start_never_moves_a_later_agent_commit() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    let mut stopped = store::load(&case.db, &id).unwrap();
    stopped.state = ChangeState::Discarded;
    stopped.restorable_until_ms = Some(NOW_MS + 7 * DAY_MS);
    stopped.intent = Some(
        serde_json::from_value(serde_json::json!({"step": "reset", "target": case.tip()})).unwrap(),
    );
    store::save(&case.db, &stopped).unwrap();
    case.edit("later.txt", "later\n");
    let later = case.agent_commit("Later work");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(
        case.agent_head(),
        later,
        "the agent's commit left its branch"
    );
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
    assert_eq!(recorded_start(&case), Some(head));
}

/// An agent that commits and edits a path inside an update's reset, after the files moved
/// and before HEAD moves, keeps both: the HEAD move fails, putting the files back refuses
/// on the edit, and the update is refused with the set as it was (RESET-KEEP r2).
#[test]
fn an_agent_commit_and_edit_after_a_reset_moved_the_files_are_kept() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    let before = store::load(&case.db, &id).unwrap();
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(&flow::AFTER_READ_TREE, &case.workspace.git_dir, move || {
        git_in(
            &home,
            &worktree,
            &[
                "-c",
                "user.name=Agent",
                "-c",
                "user.email=agent@example.invalid",
                "commit",
                "-q",
                "-m",
                "Later work",
            ],
        );
        write(&worktree.join("README"), "agent edit\n");
    });
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(
        case.agent(&["log", "-1", "--format=%s"]).trim(),
        "Later work"
    );
    assert_eq!(case.agent(&["rev-parse", "HEAD^"]).trim(), before.head);
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        "agent edit\n"
    );
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
}

/// An agent that switches its branch inside a reset, after the files moved and before HEAD
/// moves, is never reported as reset: the engine reads HEAD again through the branch it
/// opened, refuses, and puts that branch and the files back (RESET-KEEP r2).
#[test]
fn a_branch_switch_inside_a_reset_is_never_reported_as_reset() {
    let case = Case::new();
    let master = master_agent(&case);
    let agent = flow::open_agent(
        &case.db,
        AgentKind::Master,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    let from = agent.head().unwrap();
    let lease = git_in(&case.home, &master.worktree, &["symbolic-ref", "HEAD"])
        .trim()
        .to_owned();
    let target = case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    let (home, worktree) = (case.home.clone(), master.worktree.clone());
    let runs = plant(&flow::AFTER_READ_TREE, &master.git_dir, move || {
        git_in(&home, &worktree, &["switch", "-q", "-c", "side"]);
    });
    let reset = flow::reset_agent(&agent, &from, &target).unwrap();
    assert_eq!(unplant_from(&master.git_dir, &runs), 1);
    assert!(
        !reset,
        "a reset whose agent switched branches was reported as done"
    );
    assert_eq!(
        git_in(&case.home, &master.worktree, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/side"
    );
    assert_eq!(
        git_in(&case.home, &master.worktree, &["rev-parse", &lease]).trim(),
        from
    );
    assert_eq!(
        git_in(&case.home, &master.worktree, &["status", "--porcelain"]),
        ""
    );
}

/// The case's master agent opened for a reset onto a new owner commit. Input: the case.
/// Output: its workspace, the opened agent, the commit it holds, its lease branch's full
/// name and the owner commit.
fn master_to_reset(case: &Case) -> (Workspace, flow::Agent, String, String, String) {
    let master = master_agent(case);
    let agent = flow::open_agent(
        &case.db,
        AgentKind::Master,
        &master.repository,
        &master.worktree,
        &master.git_dir,
    )
    .unwrap();
    let from = agent.head().unwrap();
    let lease = git_in(&case.home, &master.worktree, &["symbolic-ref", "HEAD"])
        .trim()
        .to_owned();
    let target = case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    (master, agent, from, lease, target)
}

/// What the agent does at the last step of a reset in the rows below: commits a new file
/// where it stands, then moves to a new branch. Inputs: the owner home and the worktree.
/// Output: none.
fn commit_then_switch(home: &Path, worktree: &Path) {
    write(&worktree.join("later.txt"), "later\n");
    git_in(home, worktree, &["add", "later.txt"]);
    git_in(
        home,
        worktree,
        &[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "commit",
            "-q",
            "-m",
            "Later work",
        ],
    );
    git_in(home, worktree, &["switch", "-q", "-c", "side"]);
}

/// Commits what the agent's index holds, the way an agent can inside a reset. Inputs: the
/// home and the worktree. Output: none.
fn commit_the_index(home: &Path, worktree: &Path) {
    git_in(
        home,
        worktree,
        &[
            "-c",
            "user.name=Agent",
            "-c",
            "user.email=agent@example.invalid",
            "commit",
            "-q",
            "-m",
            "Later work",
        ],
    );
}

/// Land-2 r1 M1: a commit made after the files moved took them with it, so putting them
/// back would stage a reversal of the agent's own commit for its next turn to post.
#[test]
fn an_agent_commit_after_a_reset_moved_the_files_gets_no_reversal() {
    let case = Case::new();
    let id = conflicted(&case);
    let before = store::load(&case.db, &id).unwrap();
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(&flow::AFTER_READ_TREE, &case.workspace.git_dir, move || {
        commit_the_index(&home, &worktree);
    });
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(
        case.agent(&["status", "--porcelain"]),
        "",
        "the engine staged a reversal of the agent's commit"
    );
    assert_eq!(case.agent(&["rev-parse", "HEAD^"]).trim(), before.head);
    let committed = case.agent(&["show", "HEAD:README"]);
    assert_ne!(
        committed,
        case.agent(&["show", &format!("{}:README", before.head)]),
        "the commit holds what the reset moved"
    );
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        committed,
        "the file stays at what the reset moved it to"
    );
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
}

/// Land-2 r1 M1: an unstaged edit of another file does not make the agent's commit any less
/// its own. The index says whether the moved files were committed, the worktree cannot.
#[test]
fn an_agent_commit_and_an_edit_elsewhere_after_a_reset_moved_the_files_gets_no_reversal() {
    let case = Case::new();
    let id = conflicted(&case);
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(&flow::AFTER_READ_TREE, &case.workspace.git_dir, move || {
        write(&worktree.join("later.txt"), "later\n");
        git_in(&home, &worktree, &["add", "later.txt"]);
        commit_the_index(&home, &worktree);
        write(&worktree.join("later.txt"), "edited\n");
    });
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(
        case.agent(&["status", "--porcelain"]),
        " M later.txt\n",
        "the engine staged a reversal of the agent's commit"
    );
}

/// LAND-2-R2-L1: a file staged after the agent's commit took what the reset moved does not
/// make that commit any less its own. Each path goes back only where HEAD does not hold it,
/// so the staged file stays and no reversal of the commit is staged.
#[test]
fn an_agent_commit_and_a_file_staged_after_a_reset_moved_the_files_gets_no_reversal() {
    let case = Case::new();
    let id = conflicted(&case);
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(&flow::AFTER_READ_TREE, &case.workspace.git_dir, move || {
        commit_the_index(&home, &worktree);
        write(&worktree.join("later.txt"), "later\n");
        git_in(&home, &worktree, &["add", "later.txt"]);
    });
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(
        case.agent(&["status", "--porcelain"]),
        "A  later.txt\n",
        "the engine staged a reversal of the agent's commit"
    );
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        case.agent(&["show", "HEAD:README"]),
        "the file stays at what the agent committed"
    );
}

/// Land-2 r1 F1: a commit made before the files moved holds none of them, so they still go
/// back under it, or the next turn would post the reset as the agent's change.
#[test]
fn an_agent_commit_before_a_reset_moved_the_files_still_gets_them_back() {
    let case = Case::new();
    let id = conflicted(&case);
    let before = store::load(&case.db, &id).unwrap();
    let original = case.agent(&["show", &format!("{}:README", before.head)]);
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(
        &flow::BEFORE_READ_TREE,
        &case.workspace.git_dir,
        move || {
            write(&worktree.join("later.txt"), "later\n");
            git_in(&home, &worktree, &["add", "later.txt"]);
            commit_the_index(&home, &worktree);
        },
    );
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(case.agent(&["status", "--porcelain"]), "");
    assert_eq!(case.agent(&["rev-parse", "HEAD^"]).trim(), before.head);
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        original,
        "the files the reset moved did not go back"
    );
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
}

/// Land-2 r1 L1: an update an engine stopped after the files moved, with the agent then
/// committing, drops the reset and puts the set back. The files stay in the agent's commit.
#[test]
fn an_update_stopped_after_its_files_moved_is_put_back_once_the_agent_committed() {
    let case = Case::new();
    let id = conflicted(&case);
    let before = store::load(&case.db, &id).unwrap();
    let original = case.agent(&["show", &format!("{}:README", before.head)]);
    stopped_inside_reset(&case, &flow::AFTER_READ_TREE, || update(&case.db, &id));
    assert!(store::load(&case.db, &id).unwrap().intent.is_some());
    assert_put_back(&case, &id, &before, Some(&before.head));
    assert_ne!(
        case.agent(&["show", "HEAD:README"]),
        original,
        "the agent's commit took what the reset moved"
    );
}

/// Land-2 r1 L1: an update an engine stopped after the files moved, with the agent then
/// editing, moves nothing and puts the set back, the edit and HEAD as the agent left them.
#[test]
fn an_update_stopped_after_its_files_moved_is_put_back_once_the_agent_edited() {
    let case = Case::new();
    let id = conflicted(&case);
    let before = store::load(&case.db, &id).unwrap();
    stopped_inside_reset(&case, &flow::AFTER_READ_TREE, || update(&case.db, &id));
    case.edit("README", "agent edit\n");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(
        case.agent_head(),
        before.head,
        "HEAD moved over the agent's edit"
    );
    assert_eq!(
        std::fs::read_to_string(case.workspace.worktree.join("README")).unwrap(),
        "agent edit\n"
    );
    assert_eq!(store::load(&case.db, &id).unwrap(), before);
}

/// A set of the case's agent that conflicts with an owner commit, held by a refused
/// Accept. Input: the case. Output: its id.
fn conflicted(case: &Case) -> ChangeSetId {
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.owner_commit("README", "one\ntwo\nTHREE\n", "Owner edit");
    assert!(matches!(
        case.accept(&id).unwrap(),
        AcceptOutcome::Conflict { .. }
    ));
    id
}

/// Stores the case's set as discarded with a reset left to finish, the way an engine stop
/// leaves it. Inputs: the case, the set, where the reset starts and its target. Output:
/// none.
fn discarded_with_reset(case: &Case, id: &ChangeSetId, from: Option<&str>, target: &str) {
    let mut stopped = store::load(&case.db, id).unwrap();
    stopped.state = ChangeState::Discarded;
    stopped.restorable_until_ms = Some(NOW_MS + 7 * DAY_MS);
    stopped.intent = Some(reset_intent(from, target));
    store::save(&case.db, &stopped).unwrap();
}

/// A stored reset step, as an engine wrote it. Inputs: where it starts, None for an engine
/// from before resets recorded that, and its target. Output: the step.
fn reset_intent(from: Option<&str>, target: &str) -> store::Intent {
    serde_json::from_value(serde_json::json!({"step": "reset", "from": from, "target": target}))
        .unwrap()
}

/// Stops a step of the engine inside a reset the way an engine stop does, after the step
/// stored its intent and moved its pending ref. Inputs: the case, the seam it stops at and
/// the step. Output: none.
fn stopped_inside_reset<T>(
    case: &Case,
    seam: &std::sync::Mutex<Vec<(PathBuf, flow::PlantedEdit)>>,
    step: impl FnOnce() -> T,
) {
    let runs = plant(seam, &case.workspace.git_dir, || {
        panic!("the engine stopped inside a reset")
    });
    let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(step));
    unplant(case, &runs);
    assert!(stopped.is_err(), "the step never reached its reset");
}

/// Has the agent commit after a stopped reset, runs the engine's next look at the set and
/// asserts the commit stayed, the set is as it was and its pending ref names `pending`.
/// Inputs: the case, the set, the record before the step and the commit the ref names, or
/// None when no ref should exist.
fn assert_put_back(case: &Case, id: &ChangeSetId, before: &store::Record, pending: Option<&str>) {
    case.edit("later.txt", "later\n");
    let later = case.agent_commit("Later work");
    owner::settled(&case.db, id).unwrap();
    assert_eq!(
        case.agent_head(),
        later,
        "the agent's commit left its branch"
    );
    assert_eq!(&store::load(&case.db, id).unwrap(), before);
    let reference = flow::pending_ref(id);
    let found = case.owner(&["for-each-ref", "--format=%(objectname)", &reference]);
    assert_eq!(found.trim(), pending.unwrap_or(""));
}

/// An agent that switches branch right after a reset moved HEAD holds the reset's commit
/// on the new branch, so the engine writes no reversal there: it reports no reset, puts
/// the lease branch back and leaves the new branch's files alone (RK-R2-S1).
#[test]
fn a_branch_switch_after_a_reset_moved_head_writes_no_reversal_on_the_new_branch() {
    let case = Case::new();
    let (master, agent, from, lease, target) = master_to_reset(&case);
    let (home, worktree) = (case.home.clone(), master.worktree.clone());
    let runs = plant(&flow::AFTER_HEAD_MOVE, &master.git_dir, move || {
        git_in(&home, &worktree, &["switch", "-q", "-c", "side"]);
    });
    let reset = flow::reset_agent(&agent, &from, &target).unwrap();
    assert_eq!(unplant_from(&master.git_dir, &runs), 1);
    assert!(
        !reset,
        "a reset whose agent switched branches was reported as done"
    );
    let git = |args: &[&str]| git_in(&case.home, &master.worktree, args).trim().to_owned();
    assert_eq!(git(&["rev-parse", "side"]), target);
    assert_eq!(git(&["rev-parse", &lease]), from);
    assert_eq!(
        git(&["status", "--porcelain"]),
        "",
        "the engine wrote a reversal onto the branch holding the reset's commit"
    );
}

/// An agent that commits on its branch and leaves it inside a reset keeps everything: the
/// lease branch cannot go back over the commit, so the engine writes no files and answers
/// busy instead of reporting a plain refusal its caller would undo (RK-R2-S1).
#[test]
fn a_reset_whose_branch_moved_on_before_it_could_go_back_writes_no_files() {
    let case = Case::new();
    let (master, agent, from, lease, target) = master_to_reset(&case);
    let (home, worktree) = (case.home.clone(), master.worktree.clone());
    let runs = plant(&flow::AFTER_HEAD_MOVE, &master.git_dir, move || {
        commit_then_switch(&home, &worktree);
    });
    let outcome = flow::reset_agent(&agent, &from, &target).map_err(|refused| refused.0);
    assert_eq!(unplant_from(&master.git_dir, &runs), 1);
    let git = |args: &[&str]| git_in(&case.home, &master.worktree, args).trim().to_owned();
    assert_eq!(
        git(&["status", "--porcelain"]),
        "",
        "the engine wrote files after its branch moved on"
    );
    assert_eq!(
        std::fs::read_to_string(master.worktree.join("README")).unwrap(),
        "one\ntwo\nTHREE\n"
    );
    assert_eq!(git(&["rev-parse", "side^"]), target);
    assert_eq!(git(&["rev-parse", &lease]), git(&["rev-parse", "side"]));
    assert_eq!(outcome, Err("changes_agent_busy".to_owned()));
}

/// An update whose agent builds on the merge and leaves its HEAD inside the reset stays
/// the update it was: the engine answers busy with the step still stored, and its next
/// look finds the work on the merge and keeps the set there (RK-R2-S1).
#[test]
fn an_update_whose_agent_built_on_the_merge_and_left_keeps_the_merge() {
    let case = Case::new();
    let id = conflicted(&case);
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(&flow::AFTER_HEAD_MOVE, &case.workspace.git_dir, move || {
        commit_then_switch(&home, &worktree);
    });
    assert_eq!(refused(update(&case.db, &id)), "changes_agent_busy");
    assert_eq!(unplant(&case, &runs), 1);
    let stopped = store::load(&case.db, &id).unwrap();
    assert!(
        stopped.intent.is_some(),
        "a reset that reached its branch was undone as if it never ran"
    );
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(case.agent(&["rev-parse", "side^"]).trim(), stopped.head);
    assert_eq!(case.agent(&["status", "--porcelain"]), "");
    let pending = case.owner(&["rev-parse", &flow::pending_ref(&id)]);
    assert_eq!(pending.trim(), stopped.head);
    assert_eq!(
        store::load(&case.db, &id).unwrap(),
        store::Record {
            intent: None,
            ..stopped
        }
    );
}

/// A discard whose agent builds on the reset and leaves its HEAD inside it still ends as a
/// discard with no step left, and the worktree's next set opens above the commit the reset
/// reached (RK-R2-S1).
#[test]
fn a_discard_whose_agent_built_on_the_reset_and_left_starts_from_the_reset() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (head, tip) = (case.view(&id).head, case.tip());
    let (home, worktree) = (case.home.clone(), case.workspace.worktree.clone());
    let runs = plant(&flow::AFTER_HEAD_MOVE, &case.workspace.git_dir, move || {
        commit_then_switch(&home, &worktree);
    });
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(unplant(&case, &runs), 1);
    assert_eq!(case.agent(&["status", "--porcelain"]), "");
    assert_eq!(case.agent(&["rev-parse", "side^"]).trim(), tip);
    assert_eq!(case.view(&id).state, ChangeState::Discarded);
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
    assert_eq!(recorded_start(&case), Some(tip));
}

/// LAND-2-L2: a discard an engine stopped after HEAD moved, with an agent commit on top,
/// reached its reset, so the next set opens above it and does not show the discarded work
/// as removed by the agent.
#[test]
fn a_discard_stopped_after_head_moved_starts_the_next_set_from_its_reset_once_the_agent_committed()
{
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (head, tip) = (case.view(&id).head, case.tip());
    stopped_inside_reset(&case, &flow::AFTER_HEAD_MOVE, || {
        discard(&case.db, &id, &head, NOW_MS)
    });
    case.edit("later.txt", "later\n");
    case.agent_commit("Later work");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(recorded_start(&case), Some(tip));
}

/// LAND-2-L2: a discard's target is an ancestor of its head, so being built on the target
/// proves nothing alone. A commit on the set's head never saw the reset and the next set
/// keeps opening above that head.
#[test]
fn a_discard_stopped_before_its_reset_starts_the_next_set_from_its_head_once_the_agent_committed() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    stopped_inside_reset(&case, &flow::BEFORE_RESET, || {
        discard(&case.db, &id, &head, NOW_MS)
    });
    case.edit("later.txt", "later\n");
    case.agent_commit("Later work");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(recorded_start(&case), Some(head));
}

/// Stops a restore of a discarded set holding a claim after HEAD moved, runs `then`, settles
/// the set and asserts the restore finished: pending, its discarded ref gone and its claim
/// held again. Inputs: the case and what happens before the settle. Output: none.
fn restore_stopped_after_head_moved_finishes(mut case: Case, then: impl FnOnce(&Case)) {
    review_ready_claim(&mut case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    assert_eq!(second_start(&case), 0, "the discarded set holds no claim");
    stopped_inside_reset(&case, &flow::AFTER_HEAD_MOVE, || {
        restore(&case.db, &id, NOW_MS + DAY_MS)
    });
    then(&case);
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(case.view(&id).state, ChangeState::Pending);
    assert_eq!(
        case.owner(&["for-each-ref", "refs/crazyagent/discarded/"]),
        ""
    );
    assert_eq!(second_start(&case), 1, "the restored set holds its claim");
}

/// LAND-2-L3: a restore finished by the next look at the set does what the live restore
/// does last, so its claims hold again and its discarded ref does not leak.
#[test]
fn a_restore_stopped_after_head_moved_finishes_its_claims_and_ref_when_settled() {
    restore_stopped_after_head_moved_finishes(Case::new(), |_| {});
}

/// LAND-2-L3: the same, reached with an agent commit on top of the restored head.
#[test]
fn a_restore_stopped_after_head_moved_finishes_its_claims_and_ref_once_the_agent_committed() {
    restore_stopped_after_head_moved_finishes(Case::new(), |case| {
        case.edit("later.txt", "later\n");
        case.agent_commit("Later work");
    });
}

/// A reset an engine stopped after the files moved and before HEAD moved, stored before
/// resets recorded where they start, finishes when the worktree holds exactly its target:
/// the files never reach the agent's next turn as its own change (RK-R2-S2).
#[test]
fn a_reset_stored_without_its_start_finishes_when_the_files_are_already_at_its_target() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (head, tip) = (case.view(&id).head, case.tip());
    discarded_with_reset(&case, &id, None, &tip);
    case.agent(&["read-tree", "-u", "-m", &head, &tip]);
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(case.agent_head(), tip, "the stopped reset was not finished");
    assert_eq!(case.agent(&["status", "--porcelain"]), "");
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
    assert_eq!(recorded_start(&case), Some(tip));
}

/// An update stopped the same way, stored without its start, also finishes: HEAD holds the
/// set's old head, an ancestor of the merge the worktree already holds (RK-R2-S2).
#[test]
fn an_update_stored_without_its_start_finishes_onto_the_merge_the_files_hold() {
    let case = Case::new();
    let id = conflicted(&case);
    let before = store::load(&case.db, &id).unwrap();
    update(&case.db, &id).unwrap();
    let mut stopped = store::load(&case.db, &id).unwrap();
    let merged = stopped.head.clone();
    case.agent(&["update-ref", "--no-deref", "HEAD", &before.head]);
    stopped.intent = Some(reset_intent(None, &merged));
    store::save(&case.db, &stopped).unwrap();
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(
        case.agent_head(),
        merged,
        "the stopped update was not finished"
    );
    assert_eq!(case.agent(&["status", "--porcelain"]), "");
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
}

/// A commit the agent made whose files equal the reset's target is still the agent's: a
/// reset stored without its start never moves HEAD off it, because that commit is neither
/// the set's head nor behind the target (RK-R2-S2).
#[test]
fn a_reset_stored_without_its_start_never_moves_a_commit_matching_its_target() {
    let case = Case::new();
    let original = case.owner(&["show", "HEAD:README"]);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let tip = case.tip();
    case.edit("README", &original);
    let later = case.agent_commit("Put it back");
    discarded_with_reset(&case, &id, None, &tip);
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(
        case.agent_head(),
        later,
        "the agent's commit left its branch"
    );
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
}

/// A stored reset moves HEAD only from the commit it recorded: a later agent commit stays
/// on its branch though the step recorded a start (RK-R2-C1).
#[test]
fn a_stored_reset_never_moves_a_commit_other_than_its_start() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let (head, tip) = (case.view(&id).head, case.tip());
    discarded_with_reset(&case, &id, Some(&head), &tip);
    case.edit("later.txt", "later\n");
    let later = case.agent_commit("Later work");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(
        case.agent_head(),
        later,
        "the agent's commit left its branch"
    );
    assert!(store::load(&case.db, &id).unwrap().intent.is_none());
    assert_eq!(recorded_start(&case), Some(head));
}

/// An update an engine stopped before its reset moved anything, with the agent committing
/// meanwhile, drops the reset and puts the set and its pending ref back as they were, as
/// the live refusal does (RK-R1-S2).
#[test]
fn an_update_stopped_inside_its_reset_is_put_back_once_the_agent_committed() {
    let case = Case::new();
    let id = conflicted(&case);
    let before = store::load(&case.db, &id).unwrap();
    stopped_inside_reset(&case, &flow::BEFORE_RESET, || update(&case.db, &id));
    assert!(store::load(&case.db, &id).unwrap().intent.is_some());
    assert_put_back(&case, &id, &before, Some(&before.head));
}

/// A restore an engine stopped the same way leaves the set discarded with no pending ref,
/// as the live refusal does (RK-R1-S2).
#[test]
fn a_restore_stopped_inside_its_reset_stays_discarded_once_the_agent_committed() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    let before = store::load(&case.db, &id).unwrap();
    stopped_inside_reset(&case, &flow::BEFORE_RESET, || {
        restore(&case.db, &id, NOW_MS + DAY_MS)
    });
    assert_eq!(
        store::load(&case.db, &id).unwrap().state,
        ChangeState::Pending
    );
    assert_put_back(&case, &id, &before, None);
}

/// LAND-4 SEC-F1: a set restored once keeps that mark when a later restore is put back, so
/// the put-back restores what the set held and not a constant.
#[test]
fn a_restore_stopped_inside_its_reset_keeps_the_mark_of_an_earlier_restore() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let head = case.view(&id).head;
    discard(&case.db, &id, &head, NOW_MS).unwrap();
    restore(&case.db, &id, NOW_MS + DAY_MS).unwrap();
    discard(&case.db, &id, &head, NOW_MS + DAY_MS + 1).unwrap();
    let before = store::load(&case.db, &id).unwrap();
    assert!(before.restored, "the set was restored once");
    stopped_inside_reset(&case, &flow::BEFORE_RESET, || {
        restore(&case.db, &id, NOW_MS + 2 * DAY_MS)
    });
    assert_put_back(&case, &id, &before, None);
}

/// A lockfile regeneration an engine stopped the same way puts the set and its pending ref
/// back on the agent's own head (RK-R1-S2).
/// macOS only: a lockfile tool runs confined by Seatbelt, so elsewhere the engine regenerates
/// nothing (`Regenerated::Refused`) and the clash goes back to its agent, as the Linux row asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_regeneration_stopped_inside_its_reset_is_put_back_once_the_agent_committed() {
    let mut case = Case::new();
    cargo_workspace(&mut case);
    let id = lockfile_clash(&case, "b = { path = \"../b\" }", "b");
    let before = store::load(&case.db, &id).unwrap();
    let mut record = before.clone();
    stopped_inside_reset(&case, &flow::BEFORE_RESET, || {
        lockfiles::regenerate(&case.db, &mut record, &["Cargo.lock".to_owned()], None)
    });
    assert!(store::load(&case.db, &id).unwrap().intent.is_some());
    assert_put_back(&case, &id, &before, Some(&before.head));
}

/// A reset that reached its commit before an engine stop stays done when the agent commits
/// on that commit afterwards: the set keeps the update, never the state before it
/// (RK-R1-S2).
#[test]
fn an_update_stopped_after_head_moved_keeps_the_merge_once_the_agent_commits_on_it() {
    let case = Case::new();
    let id = conflicted(&case);
    stopped_inside_reset(&case, &flow::AFTER_HEAD_MOVE, || update(&case.db, &id));
    let stopped = store::load(&case.db, &id).unwrap();
    assert!(stopped.intent.is_some());
    case.edit("later.txt", "later\n");
    let later = case.agent_commit("Later work");
    owner::settled(&case.db, &id).unwrap();
    assert_eq!(case.agent_head(), later);
    assert_eq!(
        store::load(&case.db, &id).unwrap(),
        store::Record {
            intent: None,
            ..stopped
        }
    );
}

/// Saves a closed copy of `first` under a new id, made at `created_ms`, in `project`.
/// Inputs: the case, the record copied, the id's hex digit, the time and the project.
/// Output: the new id.
fn closed_copy(
    case: &Case,
    first: &store::Record,
    digit: char,
    created_ms: u64,
    project: &str,
) -> ChangeSetId {
    let copy = unsaved_copy(first, digit, created_ms, project);
    store::save(&case.db, &copy).unwrap();
    copy.id
}

/// Copies a change set as a discarded set with another id, not stored yet. Inputs: the
/// record, the digit its id repeats, when it was made and its project. Output: the copy.
fn unsaved_copy(
    first: &store::Record,
    digit: char,
    created_ms: u64,
    project: &str,
) -> store::Record {
    let mut copy = first.clone();
    copy.id = ChangeSetId::new(digit.to_string().repeat(32)).unwrap();
    copy.project = ProjectId::new(project).unwrap();
    copy.state = ChangeState::Discarded;
    copy.restorable_until_ms = Some(NOW_MS + 7 * DAY_MS);
    copy.created_ms = created_ms;
    copy
}

/// The number each listed change set of a project carries, by id. Inputs: the case and
/// the project. Output: the numbers.
fn numbers(case: &Case, project: &str) -> BTreeMap<String, Option<u64>> {
    list(&case.db, &ProjectId::new(project).unwrap())
        .unwrap()
        .into_iter()
        .map(|view| (view.id.as_str().to_owned(), view.number))
        .collect()
}

/// Adds the engine's projects table holding these projects, the ones a start numbers sets
/// of. Inputs: the case and the projects. Output: none.
fn with_projects(case: &Case, projects: &[&str]) {
    case.db
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)",
        )
        .unwrap();
    for project in projects {
        case.db
            .execute(
                "INSERT INTO projects(id, body) VALUES (?1, '{}')",
                [project],
            )
            .unwrap();
    }
}

/// Drops the numbers of these sets, as an engine without numbers leaves the sets it
/// writes. Inputs: the case and the sets. Output: none.
fn drop_numbers(case: &Case, ids: &[&ChangeSetId]) {
    for id in ids {
        case.db
            .execute(
                "DELETE FROM landing_change_set_numbers WHERE change_set=?1",
                [id.as_str()],
            )
            .unwrap();
    }
}

/// AL17 (1): each change set a project adds gets the next number of that project, in the
/// order the sets are added, and keeps it when the set is saved again.
#[test]
fn change_sets_are_numbered_in_the_order_a_project_adds_them() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &first).unwrap();
    let second = closed_copy(&case, &record, 'b', record.created_ms - 10, "project-1");
    let third = closed_copy(&case, &record, 'a', record.created_ms - 20, "project-1");
    assert_eq!(case.view(&first).number, Some(1));
    assert_eq!(case.view(&second).number, Some(2));
    assert_eq!(case.view(&third).number, Some(3));
    let mut again = store::load(&case.db, &first).unwrap();
    again.prompt_line = "Saved again".into();
    store::save(&case.db, &again).unwrap();
    assert_eq!(case.view(&first).number, Some(1));
}

/// Two connections saving at once each take the next number of the project: every save is
/// applied and no two sets share a number (MOCK-FIX-C r1, rv-sec F4).
#[test]
fn two_connections_saving_at_once_each_take_the_next_number() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &first).unwrap();
    let path = PathBuf::from(case.db.path().unwrap());
    let start = std::sync::Arc::new(std::sync::Barrier::new(2));
    let saving: Vec<_> = [['a', 'b', 'c'], ['d', 'e', 'f']]
        .into_iter()
        .map(|digits| {
            let (record, path, start) = (record.clone(), path.clone(), start.clone());
            std::thread::spawn(move || {
                let db = Connection::open(&path).unwrap();
                db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
                start.wait();
                for digit in digits {
                    let copy = unsaved_copy(&record, digit, record.created_ms - 10, "project-1");
                    store::save(&db, &copy).unwrap();
                }
            })
        })
        .collect();
    for thread in saving {
        thread.join().unwrap();
    }
    let mut given: Vec<u64> = numbers(&case, "project-1")
        .into_values()
        .map(Option::unwrap)
        .collect();
    given.sort_unstable();
    assert_eq!(given, (1..=7).collect::<Vec<u64>>());
}

/// A save inside a caller's transaction nests in it: a refused save leaves the caller's
/// earlier work to commit, and rolling the caller back undoes the set and its number
/// (MOCK-FIX-C r1, rv-sec F4).
#[test]
fn a_save_inside_a_callers_transaction_commits_and_rolls_back_with_it() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &first).unwrap();
    let copy = |digit| unsaved_copy(&record, digit, record.created_ms - 10, "project-1");
    let tx = case.db.unchecked_transaction().unwrap();
    let kept = copy('b');
    store::save(&tx, &kept).unwrap();
    let mut second_open = copy('c');
    second_open.state = ChangeState::Pending;
    second_open.restorable_until_ms = None;
    assert_eq!(
        refused(store::save(&tx, &second_open)),
        "changes_agent_has_pending"
    );
    tx.commit().unwrap();
    assert_eq!(case.view(&kept.id).number, Some(2));
    assert_eq!(
        refused(store::load(&case.db, &second_open.id)),
        "changes_not_found"
    );
    let tx = case.db.unchecked_transaction().unwrap();
    let dropped = copy('d');
    store::save(&tx, &dropped).unwrap();
    tx.rollback().unwrap();
    assert_eq!(
        refused(store::load(&case.db, &dropped.id)),
        "changes_not_found"
    );
    let next = copy('e');
    store::save(&case.db, &next).unwrap();
    assert_eq!(case.view(&next.id).number, Some(3));
}

/// AL17 (2): change sets stored before numbers existed get numbers at the next engine
/// start in the order they were made, the id breaking a tie, and a second start changes
/// nothing.
#[test]
fn change_sets_stored_before_numbers_are_numbered_by_when_they_were_made() {
    let case = Case::new();
    with_projects(&case, &["project-1"]);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &first).unwrap();
    let made = record.created_ms;
    let tie_late = closed_copy(&case, &record, 'c', made - 100, "project-1");
    let tie_early = closed_copy(&case, &record, 'b', made - 100, "project-1");
    let oldest = closed_copy(&case, &record, 'd', made - 200, "project-1");
    case.db
        .execute_batch("DELETE FROM landing_change_set_numbers")
        .unwrap();
    assert!(numbers(&case, "project-1").values().all(Option::is_none));
    assert_eq!(number_change_sets(&case.db).unwrap(), 4);
    let expected = BTreeMap::from([
        (oldest.as_str().to_owned(), Some(1)),
        (tie_early.as_str().to_owned(), Some(2)),
        (tie_late.as_str().to_owned(), Some(3)),
        (first.as_str().to_owned(), Some(4)),
    ]);
    assert_eq!(numbers(&case, "project-1"), expected);
    assert_eq!(number_change_sets(&case.db).unwrap(), 0);
    assert_eq!(numbers(&case, "project-1"), expected);
}

/// AL17 (3): sets an engine without numbers wrote after numbered ones, as after a rollback
/// to b19 and back, get numbers after the project's highest at the next start, and the
/// numbered sets keep theirs.
#[test]
fn sets_written_without_a_number_are_numbered_after_the_highest_at_the_next_start() {
    let case = Case::new();
    with_projects(&case, &["project-1"]);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &first).unwrap();
    let made = record.created_ms;
    let second = closed_copy(&case, &record, 'b', made + 10, "project-1");
    let late = closed_copy(&case, &record, 'c', made + 30, "project-1");
    let early = closed_copy(&case, &record, 'd', made + 20, "project-1");
    drop_numbers(&case, &[&late, &early]);
    assert_eq!(number_change_sets(&case.db).unwrap(), 2);
    let expected = BTreeMap::from([
        (first.as_str().to_owned(), Some(1)),
        (second.as_str().to_owned(), Some(2)),
        (early.as_str().to_owned(), Some(3)),
        (late.as_str().to_owned(), Some(4)),
    ]);
    assert_eq!(numbers(&case, "project-1"), expected);
}

/// AL17 (4): two projects number their change sets independently, each from 1, at insert
/// and at the start that numbers sets stored without one; a removed project's sets are
/// left unnumbered.
#[test]
fn two_projects_number_their_change_sets_independently() {
    let case = Case::new();
    with_projects(&case, &["project-1", "project-2"]);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let first = case.turn(TurnOutcome::Succeeded).unwrap();
    let record = store::load(&case.db, &first).unwrap();
    let made = record.created_ms;
    let other = closed_copy(&case, &record, 'b', made + 10, "project-2");
    let second = closed_copy(&case, &record, 'c', made + 20, "project-1");
    let other_second = closed_copy(&case, &record, 'd', made + 30, "project-2");
    let removed = closed_copy(&case, &record, 'e', made + 40, "project-3");
    assert_eq!(
        numbers(&case, "project-1"),
        BTreeMap::from([
            (first.as_str().to_owned(), Some(1)),
            (second.as_str().to_owned(), Some(2))
        ])
    );
    assert_eq!(
        numbers(&case, "project-2"),
        BTreeMap::from([
            (other.as_str().to_owned(), Some(1)),
            (other_second.as_str().to_owned(), Some(2))
        ])
    );
    case.db
        .execute_batch("DELETE FROM landing_change_set_numbers")
        .unwrap();
    assert_eq!(number_change_sets(&case.db).unwrap(), 4);
    assert_eq!(numbers(&case, "project-2")[other_second.as_str()], Some(2));
    assert_eq!(numbers(&case, "project-3")[removed.as_str()], None);
}

/// AL17: a numbers table holding a number below 1, which the store's own table refuses,
/// trips the assertion that a project's numbers start at 1 when the next set is numbered.
#[test]
#[should_panic(expected = "a change set number starts at 1")]
fn a_number_below_one_in_the_table_trips_the_numbering_assertion() {
    let case = Case::new();
    case.db
        .execute_batch(
            "DROP TABLE landing_change_set_numbers;
             CREATE TABLE landing_change_set_numbers (change_set TEXT PRIMARY KEY, project TEXT NOT NULL, number INTEGER NOT NULL);
             INSERT INTO landing_change_set_numbers VALUES ('x', 'project-1', -1);",
        )
        .unwrap();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let _ = case.turn(TurnOutcome::Succeeded);
}

/// AL17: a change set is named by its number, its agent, the first line of its message cut
/// to 48 characters and its file count, as the desktop names it; one the engine has not
/// numbered yet by its agent.
#[test]
fn a_change_set_name_carries_its_number_agent_subject_and_files() {
    let case = Case::new();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let mut view = case.view(&id);
    view.number = Some(16);
    view.agent.label = "Wren".into();
    view.message = "checkout UI\n\nThe form and its summary.".into();
    view.files = 7;
    assert_eq!(
        change_set_name(&view),
        "Change set #16 (Wren: checkout UI, 7 files)"
    );
    view.agent.kind = AgentKind::Master;
    view.message = String::new();
    view.files = 1;
    assert_eq!(change_set_name(&view), "Change set #16 (Master, 1 file)");
    view.agent.kind = AgentKind::Sub;
    view.agent.label = "agent-1".into();
    view.message = "x".repeat(80);
    view.files = 3;
    assert_eq!(
        change_set_name(&view),
        format!("Change set #16 (agent-1: {}..., 3 files)", "x".repeat(45))
    );
    view.number = None;
    assert_eq!(change_set_name(&view), "agent-1");
}

/// AL2: the owner's Accept is listed as "You accepted change set #1 (Agent 1: ...)", the
/// set named by its number as the mockup names it.
#[test]
fn the_owners_accept_names_the_set_by_its_number_in_the_activity() {
    let case = Case::new();
    case.db
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, body TEXT NOT NULL)",
        )
        .unwrap();
    crate::coord::ensure(&case.db).unwrap();
    crate::activity::initialize(&case.db).unwrap();
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    case.accept(&id).unwrap();
    let texts: Vec<String> = crate::activity::list(&case.db, &case.workspace.project, 50)
        .unwrap()
        .into_iter()
        .filter(|item| item.kind == crate::activity::ActivityKind::Accepted)
        .map(|item| item.text)
        .collect();
    assert_eq!(
        texts,
        vec!["You accepted change set #1 (Agent 1: Add the fourth line, 1 file)".to_owned()]
    );
}

/// The places the project's Accepted rows name, newest first. Input: the case. Output: the
/// places.
fn accepted_places(case: &Case) -> Vec<Option<crate::activity::AcceptPlace>> {
    crate::activity::list(&case.db, &case.workspace.project, 50)
        .unwrap()
        .into_iter()
        .filter(|item| item.kind == crate::activity::ActivityKind::Accepted)
        .map(|item| item.place)
        .collect()
}

/// AL2-PLACE: the owner's Accepted row says where Accept was pressed, Changes or the IDE,
/// through either landing call, while an Accept the engine makes for someone else names no
/// place.
#[test]
fn an_owner_accept_row_says_where_accept_was_pressed() {
    use crate::activity::AcceptPlace;
    let pending = || {
        let case = Case::new();
        closing_tables(&case);
        case.edit("README", "one\ntwo\nthree\nfour\n");
        let id = case.turn(TurnOutcome::Succeeded).unwrap();
        let head = case.view(&id).head;
        (case, id, head)
    };
    for (place, onto) in [
        (AcceptPlace::Ide, None),
        (AcceptPlace::Changes, None),
        (AcceptPlace::Changes, Some("main")),
        (AcceptPlace::Ide, Some("main")),
    ] {
        let (case, id, head) = pending();
        let landed = match onto {
            None => accept(&case.db, &case.home, &id, &head, None, place),
            Some(branch) => accept_into(&case.db, &case.home, &id, &head, None, branch, place),
        };
        assert!(
            matches!(landed, Ok(AcceptOutcome::Landed { .. })),
            "{landed:?}"
        );
        assert_eq!(
            accepted_places(&case),
            vec![Some(place)],
            "{place:?} onto {onto:?}"
        );
    }
    let (case, id, head) = pending();
    let landed = super::accept_into_as(
        &case.db,
        &case.home,
        &id,
        &head,
        None,
        "main",
        crate::activity::ENGINE_ACTOR,
    );
    assert!(
        matches!(landed, Ok(AcceptOutcome::Landed { .. })),
        "{landed:?}"
    );
    assert_eq!(accepted_places(&case), vec![None]);
}

/// LAND-15-L1: the activity list reads a landing store made before Accept kept its place,
/// with no `accepted_in` column and before any start adds it, without an error, and its
/// Accepted row names no place, as one stored before places were kept.
#[test]
fn an_activity_list_on_a_landing_store_without_the_place_column_names_no_place() {
    use crate::activity::AcceptPlace;
    let case = Case::new();
    closing_tables(&case);
    case.edit("README", "one\ntwo\nthree\nfour\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    assert!(matches!(case.accept(&id), Ok(AcceptOutcome::Landed { .. })));
    assert_eq!(accepted_places(&case), vec![Some(AcceptPlace::Changes)]);
    case.db
        .execute_batch("ALTER TABLE landing_change_sets DROP COLUMN accepted_in;")
        .unwrap();
    assert_eq!(accepted_places(&case), vec![None]);
}

/// AL2-PLACE: the owner's Accept never goes through accept_into_as, which knows no place.
#[test]
#[should_panic(expected = "the owner's Accept goes through accept or accept_into")]
fn an_owner_accept_through_accept_into_as_is_refused_by_assertion() {
    let case = Case::new();
    let id = ChangeSetId::new("0".repeat(32)).unwrap();
    let _ = super::accept_into_as(
        &case.db,
        &case.home,
        &id,
        "0",
        None,
        "main",
        crate::activity::OWNER_ACTOR,
    );
}

/// How long a landing race row runs: `CRAZYAGENT_LANDING_RACE_SECONDS` for a longer run at a
/// red, else a few seconds. Output: the seconds.
#[cfg(target_os = "macos")]
fn race_seconds() -> u64 {
    std::env::var("CRAZYAGENT_LANDING_RACE_SECONDS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(5)
}

/// The engine's git on the case's agent worktree, confined for one step. Inputs: the case and
/// what the step may write. Output: the repository.
#[cfg(target_os = "macos")]
fn confined_agent(case: &Case, writes: git::Writes) -> git::Repo {
    git::Repo::open(
        &case.repo,
        &case.workspace.git_dir,
        Some(&case.workspace.worktree),
    )
    .unwrap()
    .confined(writes)
    .unwrap()
}

/// SECAUDIT unconfined-git-add-reads-through-raced-symlink, with no timing: a step on the work
/// tree cannot read a file behind a link, while a plain file in the work tree still reads.
#[cfg(target_os = "macos")]
#[test]
fn a_confined_git_step_cannot_read_a_file_behind_a_link() {
    let case = Case::new();
    let outside = case.home.join("marker");
    write(&outside, "secret\n");
    let worktree = &case.workspace.worktree;
    std::os::unix::fs::symlink(&outside, worktree.join("link")).unwrap();
    write(&worktree.join("plain"), "plain\n");
    let repo = confined_agent(&case, git::Writes::Objects);
    let hash = |path: &str| {
        repo.exec(&["hash-object", "--", path], None, &[], 4096)
            .unwrap()
            .code
    };
    assert_eq!(hash("plain"), 0);
    assert_ne!(hash("link"), 0, "a step read the file behind the link");
}

/// SECAUDIT unconfined-read-tree-u-in-agent-writable-worktree, with no timing: a step writes
/// only what it names. Neither step reaches the owner's hooks or anywhere outside, a step
/// filling the work tree writes no git metadata, and a step adding the work tree writes
/// nothing in it. What each may write still works.
#[cfg(target_os = "macos")]
#[test]
fn a_confined_git_step_writes_only_what_it_names_and_never_the_git_folder() {
    use super::git::Writes::{Objects, WorkTree};
    let case = Case::new();
    let worktree = case.workspace.worktree.clone();
    let folder = case.repo.join(".git");
    let private = case.workspace.git_dir.clone();
    let outside = case.home.join("outside");
    let checkout = |writes: git::Writes, into: &Path| {
        let prefix = format!("--prefix={}/", into.display());
        confined_agent(&case, writes)
            .exec(&["checkout-index", "-a", "-f", &prefix], None, &[], 4096)
            .unwrap()
    };
    for (writes, into) in [
        (WorkTree, outside.clone()),
        (WorkTree, folder.join("hooks")),
        (WorkTree, folder.join("objects")),
        (WorkTree, private),
        (Objects, worktree.join("out")),
        (Objects, outside),
        (Objects, folder.join("hooks")),
    ] {
        checkout(writes, &into);
        assert!(
            std::fs::symlink_metadata(into.join("README")).is_err(),
            "a step wrote in {}",
            into.display()
        );
    }
    assert_eq!(checkout(WorkTree, &worktree.join("ok")).code, 0);
    assert!(worktree.join("ok/README").is_file());
    assert_eq!(checkout(Objects, &folder.join("objects/ok")).code, 0);
    assert!(folder.join("objects/ok/README").is_file());
}

/// SECAUDIT unconfined-git-add-reads-through-raced-symlink: a file an agent keeps swapping for
/// a link while its turn ends leaves no blob of what the link points at.
#[cfg(target_os = "macos")]
#[test]
fn a_file_swapped_for_a_link_during_the_turn_end_leaves_no_blob_of_what_it_points_at() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let case = Case::new();
    let marker = format!("{}\n", "S".repeat(63));
    let secret = case.home.join("marker");
    std::fs::write(&secret, &marker).unwrap();
    let decoy = case.workspace.worktree.join("decoy.txt");
    std::fs::write(&decoy, format!("{}\n", "D".repeat(63))).unwrap();
    let side = case.home.join("swap");
    std::fs::create_dir_all(&side).unwrap();
    let deadline = Instant::now() + Duration::from_secs(race_seconds());
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = std::thread::spawn({
        let (stop, decoy, secret) = (stop.clone(), decoy.clone(), secret.clone());
        move || {
            let (plain, link) = (side.join("plain"), side.join("link"));
            let mut round = 0u64;
            while !stop.load(Ordering::Relaxed) && Instant::now() < deadline {
                std::fs::write(&plain, format!("{round:063}\n")).unwrap();
                let _ = std::fs::rename(&plain, &decoy);
                let _ = std::fs::remove_file(&link);
                std::os::unix::fs::symlink(&secret, &link).unwrap();
                let _ = std::fs::rename(&link, &decoy);
                round += 1;
            }
        }
    });
    let mut turns = 0u32;
    while turns < 200 && Instant::now() < deadline {
        let turn = TurnEnd {
            outcome: TurnOutcome::Succeeded,
            turn: format!("turn-{turns}"),
            prompt_line: "Probe".into(),
        };
        let _ = record_turn_end(&case.db, &case.workspace, &turn);
        turns += 1;
    }
    stop.store(true, Ordering::Relaxed);
    swapper.join().unwrap();
    assert!(turns > 0);
    let listing = git_in(
        &case.home,
        &case.repo,
        &["cat-file", "--batch-all-objects", "--batch-check"],
    );
    let found = listing
        .lines()
        .filter(|line| line.ends_with(" blob 64"))
        .filter_map(|line| line.split(' ').next())
        .any(|sha| git_in(&case.home, &case.repo, &["cat-file", "-p", sha]) == marker);
    assert!(!found, "a blob holds the bytes behind the link");
}

/// One attempt of the read-tree race: a lead's worktree on its branch holding `x/keep.txt`
/// from one landed set, and a waiting second set adding `x/planted.bin` with `known`.
/// Input: the file's text. Output: the case, the lead and the waiting set.
#[cfg(target_os = "macos")]
fn read_tree_attempt(known: &str) -> (Case, Workspace, ChangeSetId) {
    let case = Case::new();
    let agents = case
        .repo
        .join(crate::projects_home::CRAZYAGENT_DIRECTORY)
        .join("worktrees");
    std::fs::create_dir_all(&agents).unwrap();
    let worktree = agents.join("agent-2");
    case.owner(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        worktree.to_str().unwrap(),
        "HEAD",
    ]);
    let lead = Workspace {
        agent: ChangeAgent {
            kind: AgentKind::Sub,
            id: "agent-2".into(),
            label: "Agent 2".into(),
        },
        worktree,
        git_dir: case.repo.join(".git/worktrees/agent-2"),
        ..case.workspace.clone()
    };
    assert_eq!(named(&case, &lead).unwrap(), "crazyagent/agent-2");
    case.edit("x/keep.txt", "keep\n");
    let id = case.turn(TurnOutcome::Succeeded).unwrap();
    let landed = accept_into(
        &case.db,
        &case.home,
        &id,
        &case.view(&id).head,
        None,
        "crazyagent/agent-2",
        crate::activity::AcceptPlace::Changes,
    )
    .unwrap();
    assert!(matches!(landed, AcceptOutcome::Landed { .. }));
    case.edit("x/planted.bin", known);
    let second = case.turn(TurnOutcome::Succeeded).unwrap();
    (case, lead, second)
}

/// SECAUDIT unconfined-read-tree-u-in-agent-writable-worktree, with no timing: the step an
/// Accept fills an agent's worktree through writes nothing outside that worktree, where the
/// same step on the owner's checkout still can.
#[cfg(target_os = "macos")]
#[test]
fn an_accept_fills_an_agents_worktree_through_a_step_confined_to_it() {
    let (case, lead, _) = read_tree_attempt("known\n");
    let repository =
        git::Repo::open(&case.repo, &case.repo.join(".git"), Some(&case.repo)).unwrap();
    let agent = flow::open_agent(
        &case.db,
        AgentKind::Sub,
        &case.repo,
        &lead.worktree,
        &lead.git_dir,
    )
    .unwrap();
    let outside = case.home.join("outside");
    let prefix = format!("--prefix={}/", outside.display());
    let args = ["checkout-index", "-a", "-f", prefix.as_str()];

    let target = owner::Target::Agent(Box::new(agent));
    let (confined, refused) = owner::filled_checkout(&target, &repository)
        .unwrap()
        .unwrap();
    assert_eq!(refused, "changes_agent_busy");
    confined.exec(&args, None, &[], 4096).unwrap();
    assert!(
        std::fs::symlink_metadata(outside.join("README")).is_err(),
        "the step wrote outside the agent's worktree"
    );

    let (plain, refused) = owner::filled_checkout(&owner::Target::Checkout, &repository)
        .unwrap()
        .unwrap();
    assert_eq!(refused, "changes_owner_dirty");
    assert_eq!(plain.exec(&args, None, &[], 4096).unwrap().code, 0);
    assert!(
        outside.join("README").is_file(),
        "the owner's checkout was confined"
    );
}

/// SECAUDIT unconfined-read-tree-u-in-agent-writable-worktree: a folder of a lead's worktree
/// swapped for a link while its worker's set lands writes nothing outside the worktree, and
/// the same landing with no swap still fills the worktree under the confined step.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_swapped_for_a_link_during_a_landing_writes_nothing_outside_the_worktree() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let known: String = (0..)
        .map(|n: u32| format!("{n:08}\n"))
        .take(466_034)
        .collect();
    let (case, lead, second) = read_tree_attempt(&known);
    let landed = accept_into(
        &case.db,
        &case.home,
        &second,
        &case.view(&second).head,
        None,
        "crazyagent/agent-2",
        crate::activity::AcceptPlace::Changes,
    )
    .unwrap();
    assert!(
        matches!(landed, AcceptOutcome::Landed { .. })
            && lead.worktree.join("x/planted.bin").is_file(),
        "the landing with no swap did not fill the worktree"
    );
    let deadline = Instant::now() + Duration::from_secs(race_seconds());
    let (mut tried, mut hits) = (0u32, 0u32);
    while tried < 100 && Instant::now() < deadline {
        let (case, lead, second) = read_tree_attempt(&known);
        let outside = tempfile::tempdir().unwrap();
        let go = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let x = lead.worktree.join("x");
        let aside = lead.worktree.join("x.aside");
        let swapper = std::thread::spawn({
            let (go, done, x, aside) = (go.clone(), done.clone(), x.clone(), aside.clone());
            let target = outside.path().to_path_buf();
            move || {
                while !go.load(Ordering::Relaxed) {
                    if done.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::yield_now();
                }
                while !done.load(Ordering::Relaxed) {
                    let _ = std::fs::rename(&x, &aside);
                    let _ = std::os::unix::fs::symlink(&target, &x);
                    let _ = std::fs::remove_file(&x);
                    let _ = std::fs::rename(&aside, &x);
                }
            }
        });
        let head = case.view(&second).head;
        let _ = owner::accept_to(
            &case.db,
            &case.home,
            &second,
            &head,
            None,
            Some("crazyagent/agent-2"),
            owner::By::Named(crate::activity::ENGINE_ACTOR),
            &|step| {
                if step == owner::Step::Recorded {
                    go.store(true, Ordering::Relaxed);
                }
                false
            },
        );
        done.store(true, Ordering::Relaxed);
        swapper.join().unwrap();
        tried += 1;
        if std::fs::read(outside.path().join("planted.bin"))
            .is_ok_and(|bytes| bytes == known.as_bytes())
        {
            hits += 1;
        }
    }
    assert!(tried > 0);
    assert_eq!(hits, 0, "planted.bin was written outside the worktree");
}
