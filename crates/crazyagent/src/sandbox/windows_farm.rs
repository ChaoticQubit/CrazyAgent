use kernel_sandbox::windows::owner_git::{OWNER_GIT_FOLDER, OwnerGit};
use kernel_sandbox::windows::{GRANTS_FOLDER, HELPER_NAME};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The lock every share row holds from its session reset to its last session read.
static SESSIONS: Mutex<()> = Mutex::new(());

/// Proof a row holds the runner's SMB sessions: the server's session table is machine-wide,
/// so two share rows in one test process would erase or add each other's sessions
/// (CodeRabbit CR-0443 unc9 r2). Dropping it lets the next row in.
pub(crate) struct Sessions(
    #[allow(dead_code, reason = "held only for its drop")] MutexGuard<'static, ()>,
);

/// Waits until no other row holds the sessions, then holds them. A row that failed while
/// holding them leaves nothing to undo, so a poisoned lock is taken as it is. Output: the
/// hold, kept for the whole row.
pub(crate) fn hold() -> Sessions {
    Sessions(SESSIONS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Each planted shape with the share it names, as the setup script opens them.
pub(crate) const SHAPES: [(&str, &str); 5] = [
    ("alternates", "unc-alternates"),
    ("nested", "unc-nested"),
    ("include", "unc-include"),
    ("commondir", "unc-commondir"),
    ("exclude-link", "unc-exclude"),
];

/// A value the setup script exported for the rows. Input: the variable name. Output:
/// its value; a missing one fails the row, which runs only after that script.
fn exported(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is set by unc-share-setup.ps1 on the test farm"))
}

/// The path `rest` inside the share `name` at the runner's own IPv4 address. Inputs: the
/// share and the path inside it. Output: the UNC path, with backslashes.
fn share(name: &str, rest: &str) -> String {
    format!(
        r"\\{}\{name}\{rest}",
        exported("CRAZYAGENT_FARM_SHARE_HOST")
    )
}

/// Runs `net` with `args` and returns what it printed. Input: the arguments. Output:
/// standard output and error, as one text.
fn net(args: &[&str]) -> String {
    let output = kernel_processes::spawn_gate::collected(
        Command::new("net").args(args).stdin(Stdio::null()),
    )
    .unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Ends every server session, so the next read sees only what a row's git opened.
/// Input: the row's hold on the sessions.
pub(crate) fn reset_sessions(held: &Sessions) {
    net(&["session", "/delete", "/y"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!server_session(held), "a server session outlived the reset");
}

/// True when the runner's SMB server holds a session, the effect a share open leaves
/// (P1: no container open made one, every owner open did). Input: the row's hold on the
/// sessions. Output: the answer.
pub(crate) fn server_session(_held: &Sessions) -> bool {
    !net(&["session"]).contains("There are no entries")
}

/// Makes the runner user own `folder` and everything in it, as an owner's own project
/// is: the elevated runner's files are owned by Administrators, which contained git
/// refuses as dubious ownership (P2e). Input: the folder.
pub(crate) fn own(folder: &Path) {
    let sid = exported("CRAZYAGENT_FARM_USER_SID");
    let output = kernel_processes::spawn_gate::collected(
        Command::new("icacls")
            .arg(folder)
            .args(["/setowner", &format!("*{sid}"), "/T", "/C", "/L", "/Q"])
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "icacls /setowner: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// Installs owner git for `project` with the helper beside the test binary's folder and
/// owner git's and the grants' folders in `place`, making the grants folder as the engine's
/// data root holds it, then runs one wrapped `git --version` to exit 0, as a prepare would
/// before any timed step. Inputs: the temp folder, the project and git's program. Output:
/// owner git; a refusal, or a contained git that fails, fails the row with its stderr.
pub(crate) fn installed(place: &Path, project: &Path, git: &Path) -> OwnerGit {
    let tests = std::env::current_exe().unwrap();
    let helper: PathBuf = tests.parent().unwrap().with_file_name(HELPER_NAME);
    assert!(
        helper.is_file(),
        "build crazyagent-win-boundary first: {}",
        helper.display()
    );
    let grants = place.join(GRANTS_FOLDER);
    std::fs::create_dir_all(&grants).unwrap();
    let owner = OwnerGit::install(
        &crate::sandbox::PROFILE_HOOKS,
        &place.join(OWNER_GIT_FOLDER),
        project,
        git,
        &helper,
        &grants,
    )
    .unwrap_or_else(|refused| {
        panic!(
            "owner git install for {} with {}: {refused:?}",
            project.display(),
            git.display()
        )
    });
    let mut version = Command::new(owner.git());
    version
        .arg("--version")
        .current_dir(kernel_sandbox::paths::canonical(project).unwrap())
        .env("GIT_CONFIG_GLOBAL", kernel_common::names::NULL_DEVICE);
    let mut wrapped = owner.wrap(&version).unwrap_or_else(|refused| {
        panic!(
            "owner git wrap for {} with {}: {refused:?}",
            project.display(),
            owner.git().display()
        )
    });
    let ran = kernel_processes::spawn_gate::collected(&mut wrapped).unwrap();
    assert!(
        ran.status.success(),
        "owner git --version in its container: {:?}, stderr: {}",
        ran.status,
        String::from_utf8_lossy(&ran.stderr)
    );
    owner
}

/// Plants `shape` in the repository whose work tree is `project`, pointing git at its
/// share: an alternates file, a nested gitfile, a config include, a `commondir` or an
/// `info\exclude` link. Inputs: the project and the shape, one of [`SHAPES`].
pub(crate) fn plant(project: &Path, shape: &str) {
    let git_dir = project.join(".git");
    let (_, name) = SHAPES
        .iter()
        .find(|(known, _)| *known == shape)
        .unwrap_or_else(|| panic!("unknown shape {shape}"));
    match shape {
        "alternates" => std::fs::write(
            git_dir.join("objects").join("info").join("alternates"),
            format!("{}\n", share(name, "objects")),
        )
        .unwrap(),
        "nested" => {
            let sub = project.join("sub");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("x.txt"), "x").unwrap();
            std::fs::write(
                sub.join(".git"),
                format!("gitdir: {}\n", share(name, "nested")),
            )
            .unwrap();
        }
        "include" => {
            let config = git_dir.join("config");
            let kept = std::fs::read_to_string(&config).unwrap();
            let path = share(name, "cfg").replace('\\', "/");
            std::fs::write(&config, format!("{kept}[include]\n\tpath = {path}\n")).unwrap();
        }
        "commondir" => std::fs::write(
            git_dir.join("commondir"),
            format!("{}\n", share(name, "common")),
        )
        .unwrap(),
        "exclude-link" => {
            let exclude = git_dir.join("info").join("exclude");
            std::fs::create_dir_all(exclude.parent().unwrap()).unwrap();
            let _ = std::fs::remove_file(&exclude);
            std::os::windows::fs::symlink_file(share(name, "exclude"), &exclude).unwrap();
        }
        _ => unreachable!(),
    }
}
