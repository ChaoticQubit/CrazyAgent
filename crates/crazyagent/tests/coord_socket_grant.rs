#![cfg(target_os = "macos")]

use crazyagent::sandbox::{ProjectSandbox, SandboxNetwork};
use crazyagent::spawn_gate;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

const NC_LIMIT: Duration = Duration::from_secs(20);

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    daemon: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let daemon = root.join("daemon");
    for directory in [&project, &home, &tmp, &daemon] {
        std::fs::create_dir_all(directory).unwrap();
    }
    Case {
        _temp: temp,
        project,
        home,
        tmp,
        daemon,
    }
}

fn sandbox(case: &Case) -> ProjectSandbox {
    ProjectSandbox::new(
        &case.project,
        &case.home,
        &case.tmp,
        SandboxNetwork::Only(Vec::new()),
    )
    .unwrap()
}

/// Connects to a Unix socket from inside the agent sandbox with the system `nc`. Takes
/// the sandbox, the profile path and the socket. Returns whether the connection worked.
/// A refused connect ends nc at once, so nc still running after `NC_LIMIT` reached a
/// listener that never answers: it is killed and the test fails with that.
fn connects(sandbox: &ProjectSandbox, profile: &Path, socket: &Path) -> bool {
    sandbox.install(profile).unwrap();
    let mut command = sandbox.command(profile, Path::new("/usr/bin/nc"));
    command
        .arg("-U")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = spawn_gate::spawn(&mut command).unwrap();
    let deadline = Instant::now() + NC_LIMIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status.success();
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "nc was still connected to {} after {NC_LIMIT:?}: the sandbox let it reach a socket that never answers",
                socket.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// K2b: a socket grant takes only a plain absolute path of ASCII letters, digits, `/`, `.`,
/// `-` and `_` within the 103 bytes macOS binds, in a folder that exists, and at most two.
#[test]
fn a_socket_grant_takes_only_plain_bounded_paths_and_two_sockets() {
    let case = case();
    let fits = 103 - case.daemon.to_str().unwrap().len() - 1;
    let longest = case.daemon.join("s".repeat(fits));
    assert_eq!(longest.to_str().unwrap().len(), 103);
    assert!(sandbox(&case).allow_unix_socket(&longest).is_ok());
    let over = case.daemon.join("s".repeat(fits + 1));
    for refused in [
        PathBuf::from("app.sock"),
        case.daemon.join("..").join("daemon").join("app.sock"),
        case.daemon.join(".").join("app.sock"),
        PathBuf::from(format!("{}//app.sock", case.daemon.display())),
        PathBuf::from(format!("{}/app.sock/", case.daemon.display())),
        case.daemon.join("app sock"),
        case.daemon.join("äpp.sock"),
        case.daemon.join("missing").join("app.sock"),
        over,
    ] {
        assert!(
            sandbox(&case).allow_unix_socket(&refused).is_err(),
            "{}",
            refused.display()
        );
    }
    let two = sandbox(&case)
        .allow_unix_socket(&case.daemon.join("one.sock"))
        .unwrap()
        .allow_unix_socket(&case.daemon.join("two.sock"))
        .unwrap();
    assert!(
        two.allow_unix_socket(&case.daemon.join("three.sock"))
            .is_err()
    );
}

/// K2b: the profile grants each socket exactly: binding and connecting its path literal,
/// reading and writing it and its lock, and reading its folder's metadata.
#[test]
fn the_profile_holds_exact_rules_for_each_granted_socket() {
    let case = case();
    let one = case.daemon.join("one.sock");
    let two = case.daemon.join("two.sock");
    let profile = sandbox(&case)
        .allow_unix_socket(&one)
        .unwrap()
        .allow_unix_socket(&two)
        .unwrap()
        .profile();
    assert_eq!(profile.matches("unix-socket").count(), 4);
    for socket in [&one, &two] {
        let path = socket.display();
        for rule in [
            format!("(allow network-bind (local unix-socket (path-literal \"{path}\")))"),
            format!("(allow network-outbound (remote unix-socket (path-literal \"{path}\")))"),
            format!(
                "(allow file-read* file-write*\n  (literal \"{path}\")\n  (literal \"{path}.lock\"))"
            ),
            format!(
                "(allow file-read-metadata (literal \"{}\"))",
                case.daemon.display()
            ),
        ] {
            assert!(profile.contains(&rule), "{rule}\n{profile}");
        }
    }
    assert!(!sandbox(&case).profile().contains("unix-socket"));
}

/// K2b: inside the real Seatbelt sandbox an agent connects to its granted socket and is
/// refused the socket next to it in the same folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sandboxed_agent_reaches_only_its_granted_socket() {
    let case = case();
    let granted = case.daemon.join("granted.sock");
    let other = case.daemon.join("other.sock");
    let granted_listener = spawn_gate::bind_unix(&granted).unwrap();
    let _other_listener = spawn_gate::bind_unix(&other).unwrap();
    let accepting = tokio::spawn(async move {
        let (stream, _) = spawn_gate::accept_unix(&granted_listener).await.unwrap();
        drop(stream);
    });
    let profiles = case.tmp.parent().unwrap().join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();

    let plain = sandbox(&case);
    assert!(!connects(&plain, &profiles.join("plain.sb"), &granted));
    let widened = sandbox(&case).allow_unix_socket(&granted).unwrap();
    assert!(!connects(
        &widened,
        &profiles.join("widened-other.sb"),
        &other
    ));
    assert!(connects(&widened, &profiles.join("widened.sb"), &granted));
    accepting.await.unwrap();
}
