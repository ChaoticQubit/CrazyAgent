#![cfg(target_os = "linux")]
use crazyagent::sandbox::linux::{
    BWRAP_PATHS, HIDDEN_MOUNTED_AGAIN, LinuxBackend, LinuxSandbox, NEST_ARGS,
};

use crazyagent::sandbox::{ProjectSandbox, SandboxError, SandboxNetwork};

use std::fs;

use std::path::{Path, PathBuf};

use std::process::{Command, Stdio};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-landlock");

/// One case: a temporary root holding the project, the agent's home and temp folders, and
/// the hidden folder two levels below the project with one secret file. No input. Output:
/// the temporary root's guard, the root, the project and the hidden folder.
struct Case {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    private: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let private = project.join("notes").join("inner").join("private");
    for directory in [
        &private,
        &root.join("agent").join("home"),
        &root.join("agent").join("tmp"),
        &project.join("context"),
        &project.join("worktrees"),
        &project.join(".git").join("hooks"),
        &project.join(".git").join("objects"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(project.join(".git").join("config"), "[core]\n").unwrap();
    fs::write(private.join("secret.txt"), "hidden secret words\n").unwrap();
    Case {
        _temp: temp,
        root,
        project,
        private,
    }
}

/// The two backends that hide a folder.
#[derive(Clone, Copy, Debug)]
enum Hiding {
    Nested,
    Bubblewrap,
}

/// Builds the sandbox that hides the case's folder on one backend, as an install does.
/// Inputs: the backend and the case. Output: what the build returned.
fn built(backend: Hiding, case: &Case) -> Result<LinuxSandbox, SandboxError> {
    let policy = ProjectSandbox::new(
        &case.project,
        &case.root.join("agent").join("home"),
        &case.root.join("agent").join("tmp"),
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .hide(&case.private)
    .unwrap();
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    match backend {
        Hiding::Nested => {
            let nests = crazyagent::spawn_gate::status(
                Command::new(bwrap)
                    .args(NEST_ARGS)
                    .args(["--unshare-net", "--", "/bin/true"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .unwrap();
            assert!(nests.success(), "this runner's bubblewrap must nest");
            let built = LinuxSandbox::detect(policy, Path::new(HELPER));
            if let Ok(sandbox) = &built {
                assert!(matches!(sandbox.backend(), LinuxBackend::Nested { .. }));
            }
            built
        }
        Hiding::Bubblewrap => LinuxSandbox::new(
            policy,
            LinuxBackend::Bubblewrap {
                bwrap: bwrap.to_path_buf(),
                helper: PathBuf::from(HELPER),
            },
        ),
    }
}

/// A bind mount made with `sudo -n mount --bind`, taken down with a lazy unmount when
/// dropped. Inputs: the folder shown and the existing folder it is shown at. Output: the
/// guard.
struct Bound(PathBuf);

impl Bound {
    fn new(source: &Path, at: &Path) -> Self {
        let status = crazyagent::spawn_gate::status(
            Command::new("sudo")
                .args(["-n", "mount", "--bind"])
                .arg(source)
                .arg(at)
                .stdin(Stdio::null()),
        )
        .unwrap();
        assert!(status.success(), "sudo -n mount --bind {source:?} {at:?}");
        Self(at.to_path_buf())
    }
}

impl Drop for Bound {
    fn drop(&mut self) {
        let _ = crazyagent::spawn_gate::status(
            Command::new("sudo")
                .args(["-n", "umount", "-l"])
                .arg(&self.0)
                .stdin(Stdio::null()),
        );
    }
}

/// Asserts that a build was refused because a second mount shows the hidden folder.
/// Inputs: what the build returned and the row's words. Output: none.
fn refused_again(built: Result<LinuxSandbox, SandboxError>, what: &str) {
    match built {
        Err(SandboxError::Unavailable(reason)) if reason == HIDDEN_MOUNTED_AGAIN => {}
        Err(other) => panic!("{what}: refused for another reason: {other:?}"),
        Ok(sandbox) => panic!("{what}: built on {:?}", sandbox.backend()),
    }
}

/// rv-code4 F1 (#622), run 44410206144: a second mount of the case root outside every
/// grant let the Nested agent read the hidden folder's files past its cover, so the build
/// refuses it with the named line; without the mount the same build holds.
#[test]
#[ignore = "needs passwordless sudo for bind mounts; the farm runs it with --ignored"]
fn a_second_mount_outside_every_grant_refuses_a_nested_launch() {
    let case = case();
    assert!(built(Hiding::Nested, &case).is_ok(), "the control builds");
    let alias = tempfile::tempdir().unwrap();
    let _bound = Bound::new(&case.root, &alias.path().canonicalize().unwrap());
    refused_again(
        built(Hiding::Nested, &case),
        "Nested, alias outside the grants",
    );
}

/// rv-code4 F1 (#622), run 44410206144: a second mount of the case root inside the project
/// let both backends read, write and remove the hidden folder's files, so both builds refuse
/// it with the named line; a second mount of a folder beside the hidden one is no reason.
#[test]
#[ignore = "needs passwordless sudo for bind mounts; the farm runs it with --ignored"]
fn a_second_mount_inside_the_project_refuses_the_launch_on_both_bubblewrap_backends() {
    for backend in [Hiding::Nested, Hiding::Bubblewrap] {
        let case = case();
        let beside = case.project.join("worktrees").join("beside");
        fs::create_dir(&beside).unwrap();
        let context = Bound::new(&case.project.join("context"), &beside);
        assert!(
            built(backend, &case).is_ok(),
            "{backend:?}: a mount beside builds"
        );
        drop(context);
        let alias = case.project.join("worktrees").join("alias");
        fs::create_dir(&alias).unwrap();
        let _bound = Bound::new(&case.root, &alias);
        refused_again(
            built(backend, &case),
            &format!("{backend:?}, alias in the project"),
        );
    }
}
