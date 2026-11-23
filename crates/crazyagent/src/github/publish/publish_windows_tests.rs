use super::*;
use std::process::{Command, Stdio};

const HOST: &str = "github.com";
const TOKEN: &str = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
const SHAPES: [&str; 5] = [
    "commondir",
    "include",
    "alternates",
    "http-alternates",
    "refs-junction",
];

/// Runs the runner's git as a test owner would, with no system or global config, and
/// returns what it printed, its output collected ([`kernel_processes::spawn_gate::collected`]).
/// Inputs: the folder and the arguments. Output: its standard output.
fn owner_git(folder: &Path, args: &[&str]) -> String {
    let output = kernel_processes::spawn_gate::collected(
        Command::new(git().unwrap().program())
            .args(args)
            .current_dir(folder)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", kernel_common::names::NULL_DEVICE),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// A project on `main` with one commit by the author its own config names, as an owner's
/// project is, beside the engine's empty git home and an owner home with no git config.
/// Each folder is removed when the project is dropped.
struct Project {
    place: tempfile::TempDir,
    home: tempfile::TempDir,
    owner_home: tempfile::TempDir,
}

/// Makes the project with git from the runner's PATH: a repository on `main` whose own
/// config names the author, with `README.md` and no commit yet, so a publish commit builds
/// its whole tree. Output: the project.
fn unborn() -> Project {
    let made = Project {
        place: tempfile::tempdir().unwrap(),
        home: tempfile::tempdir().unwrap(),
        owner_home: tempfile::tempdir().unwrap(),
    };
    let folder = made.path();
    std::fs::create_dir(&folder).unwrap();
    owner_git(&folder, &["init", "-q", "--template=", "-b", "main"]);
    owner_git(&folder, &["config", "user.name", "Owner"]);
    owner_git(&folder, &["config", "user.email", "owner@example.invalid"]);
    std::fs::write(folder.join("README.md"), "readme").unwrap();
    made
}

/// Makes the project as [`unborn`] does, with one commit of `README.md`.
/// Output: the project.
fn project() -> Project {
    let made = unborn();
    let folder = made.path();
    owner_git(&folder, &["add", "README.md"]);
    owner_git(&folder, &["commit", "-q", "--no-gpg-sign", "-m", "start"]);
    made
}

impl Project {
    fn path(&self) -> PathBuf {
        self.place.path().join("project")
    }

    fn git_dir(&self) -> PathBuf {
        self.path().join(".git")
    }

    fn head(&self) -> String {
        let head = owner_git(&self.path(), &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        assert!(
            matches!(head.len(), 40 | 64) && head.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "head {head:?}"
        );
        head
    }

    /// Plants one shape that points owner git at another place, toward a share where
    /// the shape names one. Input: the shape, one of `SHAPES`.
    fn plant(&self, shape: &str) {
        let git_dir = self.git_dir();
        let info = git_dir.join("objects").join("info");
        let planted = match shape {
            "commondir" => std::fs::write(git_dir.join("commondir"), "\\\\127.0.0.1\\x\\c\n"),
            "include" => {
                let config = git_dir.join("config");
                let kept = std::fs::read_to_string(&config).unwrap();
                std::fs::write(
                    &config,
                    format!("{kept}[include]\n\tpath = //127.0.0.1/x/c\n"),
                )
            }
            "alternates" => std::fs::write(info.join("alternates"), "\\\\127.0.0.1\\x\\o\n"),
            "http-alternates" => {
                std::fs::write(info.join("http-alternates"), "http://127.0.0.1/\n")
            }
            "refs-junction" => {
                let target = self.place.path().join("outside-refs");
                std::fs::create_dir(&target).unwrap();
                std::fs::remove_dir_all(git_dir.join("refs")).unwrap();
                junction(&git_dir.join("refs"), &target);
                Ok(())
            }
            _ => panic!("unknown shape {shape}"),
        };
        planted.unwrap();
    }
}

/// Makes `link` a junction to the folder `target`, as an agent could. Inputs: the link,
/// which must not exist, and the target. Output: none; a failed `mklink` fails the row.
fn junction(link: &Path, target: &Path) {
    let made = kernel_processes::spawn_gate::output(
        Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
}

/// Every file below `folder` with its bytes, sorted, so a row can tell that no step wrote
/// there. Input: the folder. Output: each file's path below the folder and its contents.
fn snapshot(folder: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(next) = folders.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                files.push((path.strip_prefix(folder).unwrap().to_path_buf(), bytes));
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "{} holds no file", folder.display());
    files
}

/// Tells how `answer` differs from the refusal `code`, whatever the step returns on success.
/// Inputs: the answer, the code and what was tried, which names the difference. Output:
/// none for the refusal, else what was tried and what came back.
fn mismatch<T: std::fmt::Debug>(
    answer: Result<T, EngineError>,
    code: &str,
    tried: &str,
) -> Option<String> {
    (!matches!(&answer, Err(EngineError(got)) if got == code))
        .then(|| format!("{tried}: {answer:?}"))
}

/// Asserts `answer` is the refusal `code` ([`mismatch`]). Inputs: the answer, the code and
/// what was tried, which names the failure. Output: none; a mismatch fails the test.
fn refused<T: std::fmt::Debug>(answer: Result<T, EngineError>, code: &str, tried: &str) {
    if let Some(wrong) = mismatch(answer, code, tried) {
        panic!("{wrong}");
    }
}

#[test]
fn every_windows_publish_step_refuses_a_git_folder_that_points_elsewhere() {
    let mut wrong = Vec::new();
    for shape in SHAPES {
        let made = project();
        let (git, home, folder) = (git().unwrap(), made.home.path(), made.path());
        let local = inspect(&git, home, &folder, HOST).unwrap();
        let head = made.head();
        assert!(configured_author(&git, made.owner_home.path(), home, &folder).is_some());
        made.plant(shape);
        let refusal = "github_publish_not_plain_repo";
        wrong.extend(mismatch(
            inspect(&git, home, &folder, HOST),
            refusal,
            &format!("{shape} inspect"),
        ));
        let choices = GitHubPrepare {
            private: true,
            docs: false,
            approved: Vec::new(),
            rules: GitHubRule::defaults(),
        };
        wrong.extend(mismatch(
            prepare(&git, home, &folder, &local, &choices),
            refusal,
            &format!("{shape} prepare"),
        ));
        for history in [GitHubHistory::Empty, GitHubHistory::Owner] {
            wrong.extend(mismatch(
                secret_findings(&git, home, &folder, history),
                refusal,
                &format!("{shape} secrets {history:?}"),
            ));
        }
        wrong.extend(mismatch(
            commit(
                &git,
                home,
                &folder,
                &[],
                &Author {
                    name: "Ada".into(),
                    email: "ada@example.invalid".into(),
                },
            ),
            refusal,
            &format!("{shape} commit"),
        ));
        wrong.extend(mismatch(
            has_workflows(&git, home, &folder, &head),
            refusal,
            &format!("{shape} workflows"),
        ));
        if configured_author(&git, made.owner_home.path(), home, &folder).is_some() {
            wrong.push(format!("{shape} author: still read"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_windows_publish_refuses_a_nested_git_entry_naming_its_folder_before_git_walks_the_tree() {
    let ada = Author {
        name: "Ada".into(),
        email: "ada@example.invalid".into(),
    };
    for (folder_name, real) in [("sub", false), ("deeper/a/b", true)] {
        let made = project();
        let (git, home, folder) = (git().unwrap(), made.home.path(), made.path());
        let nested = folder.join(folder_name);
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("x.txt"), "untracked").unwrap();
        if real {
            owner_git(&nested, &["init", "-q", "--template="]);
        } else {
            std::fs::write(nested.join(".git"), "gitdir: \\\\127.0.0.1\\x\\n\n").unwrap();
        }
        let refusal = "github_publish_nested_repository";
        refused(
            secret_findings(&git, home, &folder, GitHubHistory::Empty),
            refusal,
            &format!("{folder_name} secrets"),
        );
        refused(
            commit(&git, home, &folder, &[], &ada),
            refusal,
            &format!("{folder_name} commit"),
        );
    }
}

#[test]
fn a_windows_publish_of_a_project_with_no_nested_git_entry_still_commits() {
    let made = project();
    let (git, home, folder) = (git().unwrap(), made.home.path(), made.path());
    std::fs::create_dir_all(folder.join(".github").join("workflows")).unwrap();
    std::fs::write(
        folder.join(".github").join("workflows").join("ci.yml"),
        "on: push\n",
    )
    .unwrap();
    std::fs::write(folder.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(folder.join("notes.txt"), "untracked").unwrap();
    let ada = Author {
        name: "Ada".into(),
        email: "ada@example.invalid".into(),
    };
    let scan = secret_findings(&git, home, &folder, GitHubHistory::Empty);
    assert!(scan.is_ok(), "secrets: {scan:?}");
    let made_commit = commit(&git, home, &folder, &[], &ada);
    assert!(made_commit.is_ok(), "commit: {made_commit:?}");
}

#[test]
fn a_windows_publish_opens_no_file_the_repository_config_names_outside_the_tree() {
    let made = project();
    let (git, home, folder) = (git().unwrap(), made.home.path(), made.path());
    let outside = made.place.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("excludes"), "notes.txt\n").unwrap();
    std::fs::write(outside.join("attributes"), "").unwrap();
    std::fs::write(outside.join("mailmap"), "").unwrap();
    for (key, file) in [
        ("core.excludesFile", "excludes"),
        ("core.attributesFile", "attributes"),
        ("mailmap.file", "mailmap"),
    ] {
        owner_git(
            &folder,
            &["config", key, outside.join(file).to_str().unwrap()],
        );
    }
    std::fs::write(folder.join("notes.txt"), format!("token = {TOKEN}\n")).unwrap();
    let scan = secret_findings(&git, home, &folder, GitHubHistory::Empty).unwrap();
    assert!(
        scan.findings
            .iter()
            .any(|finding| finding.path == "notes.txt"),
        "the outside excludes file hid notes.txt: {scan:?}"
    );
    let args: Vec<String> = project_git(&git, home, &folder)
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    for set in [
        "core.excludesFile=/dev/null",
        "core.attributesFile=/dev/null",
        "log.mailmap=false",
        "mailmap.file=/dev/null",
    ] {
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == set),
            "{set}: {args:?}"
        );
    }
}

const CONTAINED_STEPS: [&str; 6] = [
    "inspect",
    "prepare",
    "secrets",
    "commit",
    "workflows",
    "author",
];

/// The project's folder in the form owner git's boundary holds it, the canonical one, which
/// a contained command's folder must start with. Input: the project. Output: the folder.
fn contained_folder(made: &Project) -> PathBuf {
    kernel_sandbox::paths::canonical(&made.path()).unwrap()
}

/// Installs owner git for `made`, owned by the runner user, and gives the git that runs
/// every project command in its container. Inputs: the project and its canonical folder.
/// Output: the routed git.
fn routed(made: &Project, folder: &Path) -> PublishGit {
    use crate::sandbox::windows_farm;
    farm::own(folder);
    let owner = farm::installed(made.place.path(), folder, git().unwrap().program());
    PublishGit::contained(Arc::new(owner))
}

/// Runs one publish step for `made` through `git`, its answer dropped: the share-stop row
/// reads the server session, not what the step returned. Inputs: the step, one of
/// [`CONTAINED_STEPS`], the routed git, the project, its folder, its state and its head.
fn ran_step(
    step: &str,
    git: &PublishGit,
    made: &Project,
    folder: &Path,
    local: &GitHubLocal,
    head: &str,
) {
    let home = made.home.path();
    let ada = Author {
        name: "Ada".into(),
        email: "ada@example.invalid".into(),
    };
    let choices = GitHubPrepare {
        private: true,
        docs: false,
        approved: Vec::new(),
        rules: GitHubRule::defaults(),
    };
    match step {
        "inspect" => drop(inspect(git, home, folder, HOST)),
        "prepare" => drop(prepare(git, home, folder, local, &choices)),
        "secrets" => drop(secret_findings(git, home, folder, GitHubHistory::Empty)),
        "commit" => drop(commit(git, home, folder, &[], &ada)),
        "workflows" => drop(has_workflows(git, home, folder, head)),
        "author" => drop(configured_author(git, made.owner_home.path(), home, folder)),
        _ => panic!("unknown step {step}"),
    }
}

#[test]
#[ignore = "needs unc-share-setup.ps1's shares and device entries on an elevated runner (test farm)"]
fn a_contained_windows_publish_step_opens_no_share_a_git_folder_names_after_the_check() {
    use crate::sandbox::windows_farm;
    let sessions = farm::hold();
    let mut reached = Vec::new();
    for (shape, _) in farm::SHAPES {
        for step in CONTAINED_STEPS {
            let made = if step == "commit" {
                unborn()
            } else {
                project()
            };
            let folder = contained_folder(&made);
            let git = routed(&made, &folder);
            let local = inspect(&git, made.home.path(), &folder, HOST).unwrap();
            let head = if step == "workflows" {
                made.head()
            } else {
                String::new()
            };
            farm::reset_sessions(&sessions);
            if shape == "nested" {
                farm::plant(&folder, shape);
            } else {
                let planted = folder.clone();
                BEFORE_SPAWN.with(|next| {
                    *next.borrow_mut() = Some(Box::new(move || farm::plant(&planted, shape)));
                });
            }
            ran_step(step, &git, &made, &folder, &local, &head);
            BEFORE_SPAWN.with(|next| *next.borrow_mut() = None);
            if farm::server_session(&sessions) {
                reached.push(format!("{shape} {step}"));
            }
        }
    }
    assert!(
        reached.is_empty(),
        "a server session after contained publish steps: {}",
        reached.join(", ")
    );
}

#[test]
#[ignore = "needs unc-share-setup.ps1's device entries on an elevated runner (test farm)"]
fn a_contained_windows_publish_commits_a_new_file_with_the_project_author_first() {
    let made = unborn();
    let folder = contained_folder(&made);
    let home = made.home.path();
    std::fs::write(folder.join("notes.txt"), "new").unwrap();
    std::fs::write(
        made.owner_home.path().join(".gitconfig"),
        "[include]\n\tpath = identity.gitconfig\n",
    )
    .unwrap();
    std::fs::write(
        made.owner_home.path().join("identity.gitconfig"),
        "[user]\n\tname = Global\n\temail = global@example.invalid\n",
    )
    .unwrap();
    let git = routed(&made, &folder);
    inspect(&git, home, &folder, HOST).unwrap();
    let author = configured_author(&git, made.owner_home.path(), home, &folder).unwrap();
    assert_eq!(
        author,
        Author {
            name: "Owner".into(),
            email: "owner@example.invalid".into(),
        },
        "the project's own identity wins over the owner's global one"
    );
    let said = made.place.path().join("contained-stderr.txt");
    CONTAINED_STDERR.with(|file| *file.borrow_mut() = Some(std::fs::File::create(&said).unwrap()));
    let committed = commit(&git, home, &folder, &[], &author).unwrap_or_else(|failed| {
        panic!(
            "{failed:?}, contained git said: {}",
            std::fs::read_to_string(&said).unwrap_or_default()
        )
    });
    CONTAINED_STDERR.with(|file| *file.borrow_mut() = None);
    assert_eq!(
        made.head(),
        committed.commit,
        "the contained commit is the branch's first"
    );
    let tree = owner_git(&folder, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert_eq!(
        tree.lines().collect::<Vec<_>>(),
        ["README.md", "notes.txt"],
        "the contained add staged every file on the private index"
    );
    owner_git(&folder, &["config", "--unset", "user.name"]);
    owner_git(&folder, &["config", "--unset", "user.email"]);
    assert_eq!(
        configured_author(&git, made.owner_home.path(), home, &folder)
            .unwrap()
            .name,
        "Global",
        "with no project identity the owner's global config gives it, through its include"
    );
}

/// Judges a commit of an unborn project whose `sub\.git` gitfile names the share's
/// repository, as the planted row does: right only when it is `github_publish_failed` for
/// git's "'sub/' does not have a commit checked out", with no branch made. Inputs: the
/// commit's answer, the project and what contained git said. Output: what is wrong, or none.
fn gitfile_commit_wrong(
    answer: &Result<GitHubCommitted, EngineError>,
    folder: &Path,
    said: &str,
) -> Option<String> {
    let refused = matches!(answer, Err(EngineError(code)) if code == "github_publish_failed");
    let unborn = !folder
        .join(".git")
        .join("refs")
        .join("heads")
        .join("main")
        .exists();
    let named = said.contains("'sub/' does not have a commit checked out");
    (!(refused && unborn && named))
        .then(|| format!("{answer:?}, refused {refused}, unborn {unborn}, sub named {named}"))
}

#[test]
#[ignore = "needs unc-share-setup.ps1's shares and device entries on an elevated runner (test farm)"]
fn a_contained_windows_publish_reads_no_git_folder_or_gitfile_planted_below_the_project() {
    use crate::sandbox::windows_farm;
    let sessions = farm::hold();
    let ada = Author {
        name: "Ada".into(),
        email: "ada@example.invalid".into(),
    };
    let mut wrong = Vec::new();
    for shape in ["folder", "gitfile"] {
        for step in CONTAINED_STEPS {
            let made = if step == "commit" {
                unborn()
            } else {
                project()
            };
            let folder = contained_folder(&made);
            let home = made.home.path();
            let inner = folder.join("inner");
            if shape == "folder" {
                std::fs::create_dir(&inner).unwrap();
                owner_git(&inner, &["init", "-q", "--template=", "-b", "other"]);
                owner_git(&inner, &["config", "user.name", "Inner"]);
                owner_git(&inner, &["config", "user.email", "inner@example.invalid"]);
                std::fs::write(inner.join("y.txt"), "y").unwrap();
                owner_git(&inner, &["add", "y.txt"]);
                owner_git(&inner, &["commit", "-q", "--no-gpg-sign", "-m", "inner"]);
                let started_inside = kernel_processes::spawn_gate::collected(
                    Command::new(git().unwrap().program())
                        .args(["rev-parse", "--abbrev-ref", "HEAD"])
                        .current_dir(&inner)
                        .env("GIT_DIR", ".git")
                        .env("GIT_CONFIG_NOSYSTEM", "1")
                        .env("GIT_CONFIG_GLOBAL", kernel_common::names::NULL_DEVICE),
                )
                .unwrap();
                assert_eq!(
                    String::from_utf8_lossy(&started_inside.stdout).trim(),
                    "other",
                    "a relative GIT_DIR started in inner reads the planted repository"
                );
            } else {
                farm::plant(&folder, "nested");
            }
            let git = routed(&made, &folder);
            let head = if step == "workflows" {
                made.head()
            } else {
                String::new()
            };
            let kept = (shape == "folder").then(|| snapshot(&inner.join(".git")));
            let said = made.place.path().join("contained-stderr.txt");
            CONTAINED_STDERR
                .with(|file| *file.borrow_mut() = Some(std::fs::File::create(&said).unwrap()));
            farm::reset_sessions(&sessions);
            let answer = match step {
                "inspect" => match inspect(&git, home, &folder, HOST) {
                    Ok(local) if local.branch.as_deref() == Some("main") => None,
                    other => Some(format!("{other:?}")),
                },
                "commit" => {
                    let answer = commit(&git, home, &folder, &[], &ada);
                    match (shape, answer) {
                        ("folder", Ok(made_commit))
                            if made_commit.branch == "main"
                                && made_commit.commit == made.head() =>
                        {
                            None
                        }
                        ("folder", other) => Some(format!("{other:?}")),
                        (_, answer) => gitfile_commit_wrong(
                            &answer,
                            &folder,
                            &std::fs::read_to_string(&said).unwrap_or_default(),
                        ),
                    }
                }
                "author" => match configured_author(&git, made.owner_home.path(), home, &folder) {
                    Some(author) if author.name == "Owner" => None,
                    other => Some(format!("{other:?}")),
                },
                _ => {
                    let local = inspect(&git, home, &folder, HOST).unwrap();
                    ran_step(step, &git, &made, &folder, &local, &head);
                    None
                }
            };
            CONTAINED_STDERR.with(|file| *file.borrow_mut() = None);
            if let Some(got) = answer {
                wrong.push(format!(
                    "{shape} {step}: {got}, contained git said: {}",
                    std::fs::read_to_string(&said).unwrap_or_default()
                ));
            }
            if farm::server_session(&sessions) {
                wrong.push(format!("{shape} {step}: a server session"));
            }
            if let Some(kept) = kept
                && snapshot(&inner.join(".git")) != kept
            {
                wrong.push(format!("{shape} {step}: the planted inner .git changed"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
#[ignore = "needs unc-share-setup.ps1's device entries on an elevated runner (test farm)"]
fn a_contained_windows_publish_refuses_a_project_git_replaced_by_a_gitfile_or_link_as_owner_git_does()
 {
    let refusal = "github_publish_not_plain_repo";
    let ada = Author {
        name: "Ada".into(),
        email: "ada@example.invalid".into(),
    };
    let choices = GitHubPrepare {
        private: true,
        docs: false,
        approved: Vec::new(),
        rules: GitHubRule::defaults(),
    };
    let mut wrong = Vec::new();
    for shape in ["gitfile", "junction"] {
        let made = project();
        let folder = contained_folder(&made);
        let home = made.home.path();
        let outside = made.place.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        owner_git(&outside, &["init", "-q", "--template=", "-b", "main"]);
        let outside_git = outside.join(".git");
        let contained = routed(&made, &folder);
        let local = inspect(&contained, home, &folder, HOST).unwrap();
        let head = made.head();
        let kept = snapshot(&outside_git);
        let project_git_dir = folder.join(".git");
        std::fs::remove_dir_all(&project_git_dir).unwrap();
        match shape {
            "gitfile" => std::fs::write(
                &project_git_dir,
                format!("gitdir: {}\n", outside_git.display()),
            )
            .unwrap(),
            _ => junction(&project_git_dir, &outside_git),
        }
        for (side, git) in [("owner", git().unwrap()), ("contained", contained.clone())] {
            let tried = |what: &str| format!("{shape} {side} {what}");
            wrong.extend(mismatch(
                inspect(&git, home, &folder, HOST),
                refusal,
                &tried("inspect"),
            ));
            wrong.extend(mismatch(
                prepare(&git, home, &folder, &local, &choices),
                refusal,
                &tried("prepare"),
            ));
            for history in [GitHubHistory::Empty, GitHubHistory::Owner] {
                wrong.extend(mismatch(
                    secret_findings(&git, home, &folder, history),
                    refusal,
                    &tried(&format!("secrets {history:?}")),
                ));
            }
            wrong.extend(mismatch(
                commit(&git, home, &folder, &[], &ada),
                refusal,
                &tried("commit"),
            ));
            wrong.extend(mismatch(
                has_workflows(&git, home, &folder, &head),
                refusal,
                &tried("workflows"),
            ));
            if configured_author(&git, made.owner_home.path(), home, &folder).is_some() {
                wrong.push(tried("author: still read"));
            }
        }
        if snapshot(&outside_git) != kept {
            wrong.push(format!("{shape}: the outside repository changed"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn a_relative_git_dir_holds_only_where_its_work_tree_names_the_start() {
    let project = Path::new("C:\\p");
    let held = |dir: &str, start: Option<&Path>| {
        let mut command = Command::new("git");
        command.env("GIT_DIR", dir).env("GIT_WORK_TREE", project);
        if let Some(start) = start {
            command.current_dir(start);
        }
        starts_at_its_work_tree(&command)
    };
    assert!(held(".git", Some(project)));
    assert!(!held(".git", Some(&project.join("sub"))));
    assert!(!held(".git", None));
    assert!(held("C:\\p\\.git", Some(&project.join("sub"))));
    assert!(starts_at_its_work_tree(&Command::new("git")));
}

#[test]
#[ignore = "needs unc-share-setup.ps1's device entries on an elevated runner (test farm)"]
#[should_panic(expected = "a contained git with a relative GIT_DIR starts in its work tree")]
fn a_contained_windows_git_started_below_the_project_trips_the_start_assertion() {
    let made = project();
    let folder = contained_folder(&made);
    std::fs::create_dir(folder.join("inner")).unwrap();
    let git = routed(&made, &folder);
    let mut command = project_git(&git, made.home.path(), &folder);
    assert_eq!(
        command.get_envs().find(|(name, _)| *name == "GIT_DIR"),
        Some((
            std::ffi::OsStr::new("GIT_DIR"),
            Some(std::ffi::OsStr::new(".git"))
        )),
        "a contained command names its git folder from its start"
    );
    let _ = run(
        git.container(),
        command
            .current_dir(folder.join("inner"))
            .args(["rev-parse", "HEAD"]),
    );
}

#[test]
fn the_unwrapped_scope_holds_only_on_its_own_thread_until_dropped() {
    let on = || UNWRAPPED.with(std::cell::Cell::get);
    assert!(!on(), "a row starts wrapped");
    {
        let _unwrapped = Unwrapped::start();
        assert!(on(), "the scope unwraps its own thread");
        assert!(
            !std::thread::spawn(|| UNWRAPPED.with(std::cell::Cell::get))
                .join()
                .unwrap(),
            "another row's thread stays wrapped"
        );
    }
    assert!(!on(), "the scope ends wrapped");
    let failed = std::panic::catch_unwind(|| {
        let _unwrapped = Unwrapped::start();
        assert!(!UNWRAPPED.with(std::cell::Cell::get), "a failing row");
    });
    assert!(failed.is_err());
    assert!(!on(), "a failing row's unwind leaves its thread wrapped");
}

#[test]
#[ignore = "needs unc-share-setup.ps1's shares and device entries on an elevated runner (test farm)"]
fn a_planted_share_gitfile_fails_the_planted_check_when_the_commit_runs_unwrapped() {
    use crate::sandbox::windows_farm;
    let sessions = farm::hold();
    let nested_head = std::env::var("CRAZYAGENT_FARM_NESTED_HEAD")
        .expect("CRAZYAGENT_FARM_NESTED_HEAD is set by unc-share-setup.ps1 on the test farm");
    let made = unborn();
    let folder = contained_folder(&made);
    farm::plant(&folder, "nested");
    let git = routed(&made, &folder);
    let ada = Author {
        name: "Ada".into(),
        email: "ada@example.invalid".into(),
    };
    farm::reset_sessions(&sessions);
    let answer = {
        let _unwrapped = Unwrapped::start();
        commit(&git, made.home.path(), &folder, &[], &ada)
    };
    let reached = farm::server_session(&sessions);
    assert!(
        !UNWRAPPED.with(std::cell::Cell::get),
        "the row ends wrapped"
    );
    assert!(
        gitfile_commit_wrong(
            &answer,
            &folder,
            "fatal: 'sub/' does not have a commit checked out"
        )
        .is_some(),
        "the planted check passed an unwrapped commit even granted git's refusal: {answer:?}"
    );
    assert!(
        reached,
        "the unwrapped commit opened no server session: {answer:?}"
    );
    assert!(
        answer.is_ok(),
        "the unwrapped commit did not reach the share's repository: {answer:?}"
    );
    assert_eq!(
        owner_git(&folder, &["ls-tree", "HEAD", "sub"]).trim_end(),
        format!("160000 commit {nested_head}\tsub"),
        "the unwrapped commit records sub as a gitlink at the share's commit"
    );
}
