#![cfg(target_os = "linux")]
use crazyagent::sandbox::linux::{
    BWRAP_PATHS, HIDDEN_MOUNTED_AGAIN, HIDDEN_MOUNTS_NOT_PRIVATE, LinuxBackend, LinuxSandbox,
    NEST_ARGS,
};

use crazyagent::sandbox::{ProjectSandbox, Sandbox};
use kernel_sandbox::error::SandboxError;
use kernel_sandbox::net::SandboxNetwork;

use std::fs;

use std::io::{BufRead, BufReader, Read, Write};

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

/// The closed policy that hides the case's folder. Input: the case. Output: the policy.
fn hiding_policy(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &crazyagent::sandbox::PROFILE_HOOKS,
        &case.project,
        &case.root.join("agent").join("home"),
        &case.root.join("agent").join("tmp"),
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
    .hide(&case.private)
    .unwrap()
}

/// This machine's bubblewrap at its first fixed place. No input. Output: the path.
fn bwrap() -> &'static Path {
    BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed")
}

/// Builds the sandbox that hides the case's folder on one backend, as an install does.
/// Inputs: the backend and the case. Output: what the build returned.
fn built(backend: Hiding, case: &Case) -> Result<LinuxSandbox, SandboxError> {
    let policy = hiding_policy(case);
    let bwrap = bwrap();
    match backend {
        Hiding::Nested => {
            let nests = kernel_processes::spawn_gate::status(
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
        let status = kernel_processes::spawn_gate::status(
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
        let _ = kernel_processes::spawn_gate::status(
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

/// Starts the agent on one backend, with no second mount yet, as a wrapped `/bin/sh -c`
/// that prints `ready`, waits for a line, then tries to read, list, write and remove the
/// hidden folder through an alias. Once it is running, the host binds the case root at the
/// alias, then lets it go on. Inputs: the backend, the case and the alias folder, which
/// exists and is empty. Output: what the agent printed after `ready`.
fn through_a_later_mount(backend: Hiding, case: &Case, alias: &Path) -> String {
    let sandbox = built(backend, case).expect("no second mount before the start");
    let shown = alias.join(case.private.strip_prefix(&case.root).unwrap());
    let shown = format!("'{}'", shown.display());
    let script = format!(
        "echo ready; read go; cat {shown}/secret.txt 2>/dev/null; ls -A {shown} 2>/dev/null; \
         (echo agent > {shown}/new.txt) 2>/dev/null; rm -f {shown}/secret.txt 2>/dev/null; \
         echo done"
    );
    let mut harness = Command::new("/bin/sh");
    harness
        .args(["-c", &script])
        .current_dir(case.project.join("worktrees"));
    let mut child = kernel_processes::spawn_gate::spawn(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n", "{backend:?}");
    let bound = Bound::new(&case.root, alias);
    child.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut rest = String::new();
    output.read_to_string(&mut rest).unwrap();
    assert!(child.wait().unwrap().success(), "{backend:?}");
    drop(bound);
    rest
}

/// What a later mount let through on one backend: the agent's output past `done` and the
/// host's hidden folder after the run. Inputs: the backend, the case and the agent's
/// output. Output: none when nothing came through, else the leg's account.
fn came_through(backend: Hiding, case: &Case, rest: &str) -> Option<String> {
    let secret = fs::read_to_string(case.private.join("secret.txt")).ok();
    let planted = case.private.join("new.txt").exists();
    let kept = secret.as_deref() == Some("hidden secret words\n") && !planted;
    (rest != "done\n" || !kept).then(|| {
        format!("{backend:?}: printed {rest:?}, host secret {secret:?}, new.txt {planted}")
    })
}

/// V24x-LINUX-NEST-MOUNT-PROPAGATION R1: a second mount of the hidden folder's disk that
/// the host makes inside the project after the agent started never reaches it on either
/// hiding backend, so nothing of the folder is read, listed, written or removed through it.
/// bubblewrap makes the agent's tree a slave of the host's (bubblewrap.c:2959), so at the
/// base every later mount came in. Each backend runs, and the row reports every leg.
#[test]
#[ignore = "needs passwordless sudo for bind mounts; the farm runs it with --ignored"]
fn a_mount_made_after_the_start_inside_the_project_stays_out_of_both_bubblewrap_backends() {
    let mut through = Vec::new();
    for backend in [Hiding::Bubblewrap, Hiding::Nested] {
        let case = case();
        let alias = case.project.join("worktrees").join("alias");
        fs::create_dir(&alias).unwrap();
        let rest = through_a_later_mount(backend, &case, &alias);
        through.extend(came_through(backend, &case, &rest));
    }
    assert!(through.is_empty(), "{through:#?}");
}

/// V24x-LINUX-NEST-MOUNT-PROPAGATION R2: a second mount of the case root that the host
/// makes outside every grant after a Nested agent started never reaches it; run 44410206144
/// read the hidden folder's files this way. Bubblewrap binds no such folder, so only the
/// Nested backend runs.
#[test]
#[ignore = "needs passwordless sudo for bind mounts; the farm runs it with --ignored"]
fn a_mount_made_after_the_start_outside_every_grant_stays_out_of_a_nested_agent() {
    let case = case();
    let folder = tempfile::tempdir().unwrap();
    let alias = folder.path().canonicalize().unwrap();
    let rest = through_a_later_mount(Hiding::Nested, &case, &alias);
    let through = came_through(Hiding::Nested, &case, &rest);
    assert!(through.is_none(), "{through:#?}");
}

/// Sets this computer's `user.max_user_namespaces` with `sudo -n sysctl` and puts back the
/// value it read first when dropped. Input: the new limit. Output: the guard.
struct Limit(String);

impl Limit {
    fn new(limit: u32) -> Self {
        let was = fs::read_to_string("/proc/sys/user/max_user_namespaces").unwrap();
        let was = was.trim().to_owned();
        assert!(was.parse::<u32>().is_ok(), "{was:?}");
        assert!(set_limit(&limit.to_string()), "sudo -n sysctl {limit}");
        Self(was)
    }
}

impl Drop for Limit {
    fn drop(&mut self) {
        let _ = set_limit(&self.0);
    }
}

/// Writes `user.max_user_namespaces`. Input: the value. Output: whether sysctl took it.
fn set_limit(value: &str) -> bool {
    kernel_processes::spawn_gate::status(
        Command::new("sudo")
            .args(["-n", "sysctl", "-q", "-w"])
            .arg(format!("user.max_user_namespaces={value}"))
            .stdin(Stdio::null()),
    )
    .is_ok_and(|status| status.success())
}

/// V24x-LINUX-NEST-MOUNT-PROPAGATION Q5 (leader 19:09): with `user.max_user_namespaces` at 0 the
/// helper asked for a private tree, started with a clear environment as the engine starts it,
/// exits 3 with the named line, and a build with a hidden folder refuses with it on both backends;
/// at this machine's own limit, before each, both hold. Limit 0, not 1: under 1 the row's own
/// bubblewrap check failed (run 44442220795).
#[test]
#[ignore = "needs passwordless sudo for sysctl; the farm runs it with --ignored"]
fn the_helper_and_both_builds_fail_closed_where_no_user_namespace_can_be_made() {
    let mut wrong = Vec::new();
    let helper = |what: &str| {
        let output =
            kernel_processes::spawn_gate::collected(Command::new(HELPER).env_clear().args([
                "--filter-sockets",
                "--private-mounts",
                "--",
                "/bin/true",
            ]))
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        (what.to_owned(), output.status.code(), stderr)
    };
    let build = |backend: Hiding, case: &Case| {
        let bwrap = bwrap().to_path_buf();
        let helper = PathBuf::from(HELPER);
        let backend = match backend {
            Hiding::Nested => LinuxBackend::Nested { bwrap, helper },
            Hiding::Bubblewrap => LinuxBackend::Bubblewrap { bwrap, helper },
        };
        LinuxSandbox::new(hiding_policy(case), backend)
    };
    let (what, code, stderr) = helper("the helper at this machine's limit");
    if code != Some(0) {
        wrong.push(format!("{what}: exit {code:?}, {stderr:?}"));
    }
    let limit = Limit::new(0);
    let (what, code, stderr) = helper("the helper with no user namespace");
    drop(limit);
    let named = stderr.contains(HIDDEN_MOUNTS_NOT_PRIVATE)
        && stderr.contains("a user and mount namespace of its own");
    if code != Some(3) || !named {
        wrong.push(format!("{what}: exit {code:?}, {stderr:?}"));
    }
    for backend in [Hiding::Nested, Hiding::Bubblewrap] {
        let case = case();
        if let Err(error) = build(backend, &case) {
            wrong.push(format!("{backend:?} at this machine's limit: {error:?}"));
        }
        let limit = Limit::new(0);
        let refused = build(backend, &case);
        drop(limit);
        match refused {
            Err(SandboxError::Unavailable(reason)) if reason == HIDDEN_MOUNTS_NOT_PRIVATE => {}
            Err(other) => wrong.push(format!("{backend:?}: refused for {other:?}")),
            Ok(sandbox) => wrong.push(format!("{backend:?}: built on {:?}", sandbox.backend())),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
