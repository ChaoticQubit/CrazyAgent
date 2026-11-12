#![cfg(target_os = "macos")]

use crazyagent::sandbox::shared_folders::{SharedAccess, SharedFolders, SharedScope, apply};
use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

/// One project folder the way a master or confined agent sees it: a committed repository
/// with a tracked `src/main.rs`, a wholly untracked `program/` and ignored `build/`, an
/// untracked `notes.txt`, secret files of every kind, and the agent's own writable folder
/// inside `.crazyagent/worktrees`, which reads the project folder.
struct Case {
    _temp: tempfile::TempDir,
    repository: PathBuf,
    own: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    profile: PathBuf,
}

const SECRETS: &[&str] = &[
    ".env",
    "config/.Env.Local",
    ".git-credentials",
    "keys/server.pem",
    "keys/release.keystore",
    "keys/upload.jks",
    "infra/prod.tfvars",
    "infra/terraform.tfstate",
    "service-account-prod.json",
    ".docker/config.json",
    ".kube/config",
    "keys/backup.age",
    "secrets.yaml",
    "program/credentials.json",
];

fn git(folder: &Path, arguments: &[&str]) {
    let output = crazyagent::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.email=owner@local", "-c", "user.name=owner"])
            .args(arguments)
            .current_dir(folder)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {output:?}");
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let repository = root.join("project");
    write(&repository.join("src").join("main.rs"), "fn main() {}\n");
    write(&repository.join(".gitignore"), "build/\n");
    git(&repository, &["init", "-q"]);
    git(&repository, &["add", "src/main.rs", ".gitignore"]);
    git(&repository, &["commit", "-q", "-m", "start"]);
    write(&repository.join("program").join("data.csv"), "a,b\n");
    write(&repository.join("build").join("out.txt"), "built\n");
    write(&repository.join("notes.txt"), "notes\n");
    for secret in SECRETS {
        write(&repository.join(secret), "SECRET_VALUE_4471\n");
    }
    let own = repository
        .join(".crazyagent")
        .join("worktrees")
        .join("master-1");
    write(&own.join(".env"), "OWN_VALUE_1\n");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&tmp).unwrap();
    let profile = root.join("profile.sb");
    Case {
        _temp: temp,
        repository,
        own,
        home,
        tmp,
        profile,
    }
}

/// The sandbox of a launch in `own` that reads the project folder, with `shared` applied.
fn sandbox(case: &Case, shared: &SharedFolders) -> ProjectSandbox {
    let sandbox = ProjectSandbox::new(
        &case.own,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .allow_read(&case.repository)
    .unwrap();
    apply(sandbox, &case.repository, shared).unwrap()
}

fn run(case: &Case, sandbox: &ProjectSandbox, script: &str) -> Output {
    sandbox.install(&case.profile).unwrap();
    let mut command = sandbox.command(&case.profile, Path::new("/bin/sh"));
    command
        .arg("-c")
        .arg(script)
        .current_dir(sandbox.project_root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crazyagent::spawn_gate::output(&mut command).unwrap()
}

fn quoted(path: &Path) -> String {
    let text = path.to_str().unwrap();
    assert!(!text.contains('\''));
    format!("'{text}'")
}

fn reads(case: &Case, sandbox: &ProjectSandbox, path: &Path) -> bool {
    let output = run(case, sandbox, &format!("cat {}", quoted(path)));
    output.status.success()
}

fn writes(case: &Case, sandbox: &ProjectSandbox, path: &Path) -> bool {
    let output = run(case, sandbox, &format!("printf x >> {}", quoted(path)));
    output.status.success()
}

fn with(scope: SharedScope, access: SharedAccess) -> SharedFolders {
    SharedFolders {
        scope,
        access,
        chosen: vec!["program".into()],
        secrets: Vec::new(),
    }
}

/// DEC-147.9 All with Read-only: every local folder reads, gitignored ones included, none is
/// written, every secret kind stays closed in any letter case while its name stays listed,
/// and the agent's own folder keeps its own `.env`.
#[test]
fn all_read_only_reads_every_local_folder_and_keeps_every_secret_closed() {
    let case = case();
    let sandbox = sandbox(&case, &SharedFolders::default());
    for open in [
        "program/data.csv",
        "build/out.txt",
        "notes.txt",
        "src/main.rs",
    ] {
        assert!(
            reads(&case, &sandbox, &case.repository.join(open)),
            "{open}"
        );
    }
    for secret in SECRETS {
        let output = run(
            &case,
            &sandbox,
            &format!("cat {}", quoted(&case.repository.join(secret))),
        );
        assert!(!output.status.success(), "{secret} read");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("SECRET_VALUE_4471"),
            "{secret}"
        );
    }
    let listed = run(
        &case,
        &sandbox,
        &format!("ls -a {}", quoted(&case.repository)),
    );
    let names = String::from_utf8_lossy(&listed.stdout);
    assert!(
        names.contains(".env") && names.contains("program"),
        "{names}"
    );
    for path in [
        "program/data.csv",
        "notes.txt",
        "src/main.rs",
        "program/new.txt",
    ] {
        assert!(
            !writes(&case, &sandbox, &case.repository.join(path)),
            "{path} written"
        );
    }
    assert!(
        reads(&case, &sandbox, &case.own.join(".env")),
        "the own folder's .env"
    );
}

/// Leader row: a link with a plain name that points at a secret is still closed, in the
/// project folder and from the agent's own folder.
#[test]
fn a_link_with_a_plain_name_to_a_secret_is_still_closed() {
    let case = case();
    std::os::unix::fs::symlink(
        case.repository.join(".env"),
        case.repository.join("notes-link"),
    )
    .unwrap();
    std::os::unix::fs::symlink(case.repository.join(".env"), case.own.join("peek")).unwrap();
    std::os::unix::fs::symlink(
        case.repository.join("keys"),
        case.repository.join("plain-folder"),
    )
    .unwrap();
    let sandbox = sandbox(&case, &SharedFolders::default());
    for link in [
        case.repository.join("notes-link"),
        case.own.join("peek"),
        case.repository.join("plain-folder").join("server.pem"),
    ] {
        let output = run(&case, &sandbox, &format!("cat {}", quoted(&link)));
        assert!(!output.status.success(), "{} read", link.display());
        assert!(!String::from_utf8_lossy(&output.stdout).contains("SECRET_VALUE_4471"));
    }
}

/// A secret the owner shared back reads, never writes, and only that one opens.
#[test]
fn a_shared_back_secret_reads_but_never_writes() {
    let case = case();
    let shared = SharedFolders {
        secrets: vec![".env".into()],
        ..SharedFolders::default()
    };
    let sandbox = sandbox(&case, &shared);
    assert!(reads(&case, &sandbox, &case.repository.join(".env")));
    assert!(!writes(&case, &sandbox, &case.repository.join(".env")));
    assert!(!reads(
        &case,
        &sandbox,
        &case.repository.join("keys").join("server.pem")
    ));
}

/// None: no untracked local folder or file reads, names stay listed, tracked files read.
#[test]
fn none_keeps_every_untracked_local_path_closed() {
    let case = case();
    let sandbox = sandbox(&case, &with(SharedScope::None, SharedAccess::ReadOnly));
    for closed in ["program/data.csv", "build/out.txt", "notes.txt"] {
        assert!(
            !reads(&case, &sandbox, &case.repository.join(closed)),
            "{closed}"
        );
    }
    assert!(reads(
        &case,
        &sandbox,
        &case.repository.join("src").join("main.rs")
    ));
    let listed = run(&case, &sandbox, &format!("ls {}", quoted(&case.repository)));
    assert!(String::from_utf8_lossy(&listed.stdout).contains("program"));
}

/// Chosen folders: only the picked folder reads.
#[test]
fn chosen_folders_reads_only_the_picked_folders() {
    let case = case();
    let sandbox = sandbox(&case, &with(SharedScope::Chosen, SharedAccess::ReadOnly));
    assert!(reads(
        &case,
        &sandbox,
        &case.repository.join("program").join("data.csv")
    ));
    assert!(!reads(
        &case,
        &sandbox,
        &case.repository.join("build").join("out.txt")
    ));
    assert!(!reads(&case, &sandbox, &case.repository.join("notes.txt")));
}

/// Read-write: a shared untracked top-level folder takes new files; a secret inside it,
/// a tracked file and an untracked file at the root stay unwritable.
#[test]
fn read_write_changes_shared_untracked_folders_but_never_secrets_or_tracked_files() {
    let case = case();
    let sandbox = sandbox(&case, &with(SharedScope::All, SharedAccess::ReadWrite));
    assert!(writes(
        &case,
        &sandbox,
        &case.repository.join("program").join("new.txt")
    ));
    assert!(writes(
        &case,
        &sandbox,
        &case.repository.join("build").join("out.txt")
    ));
    for closed in [
        "program/credentials.json",
        "program/.env",
        "src/main.rs",
        "notes.txt",
    ] {
        assert!(
            !writes(&case, &sandbox, &case.repository.join(closed)),
            "{closed}"
        );
    }
    let picked = sandbox_for_chosen(&case);
    assert!(writes(
        &case,
        &picked,
        &case.repository.join("program").join("more.txt")
    ));
    assert!(!writes(
        &case,
        &picked,
        &case.repository.join("build").join("out.txt")
    ));
}

fn sandbox_for_chosen(case: &Case) -> ProjectSandbox {
    sandbox(case, &with(SharedScope::Chosen, SharedAccess::ReadWrite))
}

/// The secret rule comes after the profile's root read allows and before every older deny,
/// with the shared-back allow between them, so every deny that existed before still wins.
#[test]
fn the_secret_rule_sits_before_every_older_deny() {
    let case = case();
    let shared = SharedFolders {
        secrets: vec![".env".into()],
        ..SharedFolders::default()
    };
    let profile = sandbox(&case, &shared).profile();
    let root_allow = profile
        .find(&format!("(subpath \"{}\")", case.repository.display()))
        .unwrap();
    let secret_rule = profile
        .find(&format!(
            "(require-not (subpath \"{}/.git\"))",
            case.repository.display()
        ))
        .expect("the secret rule exempts .git and the own folder");
    let shared_back = profile
        .find(&format!(
            "(allow file-read-data file-read-xattr\n  (subpath \"{}\")",
            case.repository.join(".env").display()
        ))
        .unwrap();
    let older = profile.find("(deny file-read* file-write*").unwrap();
    assert!(root_allow < secret_rule, "{profile}");
    assert!(secret_rule < shared_back, "{profile}");
    assert!(shared_back < older, "{profile}");
    assert!(profile.contains(&format!(
        "(require-not (subpath \"{}/.git\"))",
        case.repository.display()
    )));
    assert!(profile.contains(&format!(
        "(require-not (subpath \"{}\"))",
        case.own.display()
    )));
}
