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
    case.edit(".vscode/extensions.json", "{\"recommendations\": []}\n");
    case.agent_commit("Recommend nothing");
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
        let stopped = owner::accept(&case.db, &case.home, &id, &head, None, &|at| at == step);
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
    let stopped = owner::accept(&case.db, &case.home, &id, &head, None, &|at| {
        at == owner::Step::Recorded
    });
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
            &case.db, &case.home, &id, &tip, None, "release"
        )),
        "changes_stale"
    );
    let AcceptOutcome::Landed { commit, branch } =
        accept_into(&case.db, &case.home, &id, &head, None, "release").unwrap()
    else {
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
            refused(accept_into(&case.db, &case.home, &id, &head, None, target)),
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
            refused(record_turn_end(&case.db, &master, &merge_turn())),
            "changes_outside_merge",
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
            refused(record_turn_end(&case.db, agent, &merge_turn())),
            "changes_outside_merge",
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
    /// never restorable, its pending ref gone and no action left. Inputs: the change set
    /// and its head.
    fn removed_outside(&self, id: &ChangeSetId, head: &str) {
        let view = self.view(id);
        assert_eq!(
            (view.state, view.outside, view.restorable_until_ms),
            (ChangeState::Discarded, true, None),
            "{view:?}"
        );
        assert_eq!(self.owner_refs("refs/crazyagent"), "");
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
        refused(accept_into(&case.db, &case.home, &id, &head, None, "main")),
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
    let AcceptOutcome::Landed { commit, branch } =
        accept_into(&case.db, &case.home, &id, &head, None, "main").unwrap()
    else {
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
    use crate::sandbox::policy::AgentAccess;
    case.start();
    case.edit("README", "one\ntwo\nthree\nfour from the agent\n");
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
    let master = master_at_work(case);
    master_may_land(case, AgentAccess::Full, crate::team::MasterLands::Merges);
    let sub_head = case.view(&sub).head;
    let resolved = "one\ntwo\nthree\nfour from the owner\nfour from the agent\n";
    let in_master = |args: &[&str]| git_in(&case.home, &master.worktree, args);
    in_master(&["merge", "--no-commit", "-s", "ours", &sub_head]);
    write(&master.worktree.join("README"), resolved);
    in_master(&["add", "README"]);
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
    let fix = turn_of(case, &master, "Resolve the README conflict");
    (sub, fix, resolved)
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
