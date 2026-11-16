use super::{
    MACH_SERVICES, POWER_HELPER_SOCKET, ProjectSandbox, SandboxError, SandboxNetwork, policy,
    power_helper_denies, power_helper_denies_for, priming_profile,
};
use crate::keep_awake::{Want, platform_hold};
use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const CAFFEINATE: &str = "/usr/bin/caffeinate";
const POWER_SERVICE: &str = "com.apple.PowerManagement.control";
const ROOT_DOMAIN: &str = "RootDomainUserClient";
const REFUSED: &str = "Failed to create";
const HOLD_SECONDS: &str = "20";
const WAIT: Duration = Duration::from_secs(5);
const STEP: Duration = Duration::from_millis(50);

/// One private temp root with a project, the agent's home and temp folders and a profile
/// file, and the sandbox a confined agent of that project runs under.
struct Case {
    _temp: tempfile::TempDir,
    profile: PathBuf,
    sandbox: ProjectSandbox,
}

/// Makes the folders and the sandbox with the given network. Input: the network policy.
/// Output: the case.
fn case(network: SandboxNetwork) -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for folder in ["project", "home", "tmp", "profiles"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    let sandbox = ProjectSandbox::new(
        &root.join("project"),
        &root.join("home"),
        &root.join("tmp"),
        network,
    )
    .unwrap();
    Case {
        profile: root.join("profiles").join("agent.sb"),
        sandbox,
        _temp: temp,
    }
}

/// Runs one program to its end under the case's installed profile, in the project folder.
/// Inputs: the case, the program and its arguments. Output: the finished process.
fn confined(case: &Case, program: &str, arguments: &[&str]) -> Output {
    case.sandbox.install(&case.profile).unwrap();
    let mut command = case.sandbox.command(&case.profile, Path::new(program));
    command
        .args(arguments)
        .current_dir(case.sandbox.project_root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::spawn_gate::output(&mut command).unwrap()
}

/// Counts the running caffeinate processes whose command line ends with the given timeout,
/// the mark of this test's own children. Input: the timeout argument. Output: how many
/// `pgrep` finds.
fn timed_holders(seconds: &str) -> usize {
    let line = format!("^{CAFFEINATE} -i -t {seconds}$");
    let output =
        crate::spawn_gate::output(Command::new("/usr/bin/pgrep").args(["-f", &line])).unwrap();
    String::from_utf8(output.stdout).unwrap().lines().count()
}

/// Counts the caffeinate children that hold for one engine pid. Input: the pid. Output:
/// how many `pgrep` finds by their exact command line.
fn engine_holders(engine: u32) -> usize {
    let line = format!("^{CAFFEINATE} (-d )?-i -w {engine}$");
    let output =
        crate::spawn_gate::output(Command::new("/usr/bin/pgrep").args(["-f", &line])).unwrap();
    String::from_utf8(output.stdout).unwrap().lines().count()
}

fn until(mut condition: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + WAIT;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(STEP);
    }
}

/// rv-sec2 KA F2: no profile the engine writes for a confined process names the power
/// assertion service or the power root domain client, with the network open or closed, so
/// only the owner's Keep awake control decides whether the computer stays awake.
#[test]
fn no_confined_profile_can_reach_the_power_assertion_service() {
    assert!(!MACH_SERVICES.contains(&POWER_SERVICE));
    assert!(
        MACH_SERVICES
            .iter()
            .all(|service| !service.to_lowercase().contains("powermanagement"))
    );
    for network in [SandboxNetwork::Open, SandboxNetwork::Only(Vec::new())] {
        let case = case(network);
        let text = case.sandbox.profile();
        assert!(text.starts_with("(version 1)\n(deny default)\n"));
        assert!(!text.contains("PowerManagement"), "{text}");
        assert!(!text.contains(ROOT_DOMAIN), "{text}");
        assert!(!text.contains("(allow iokit-open"), "{text}");
    }
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().canonicalize().unwrap();
    let priming = priming_profile(&folder, Path::new("/usr/bin/true"));
    assert!(priming.starts_with("(version 1)\n(deny default)\n"));
    assert!(!priming.contains("PowerManagement"), "{priming}");
    assert!(!priming.contains(ROOT_DOMAIN), "{priming}");
}

/// rv-sec2 KA F2, on the real system: a confined agent that starts `caffeinate` gets no
/// power assertion, and says so, instead of holding the computer awake for 20 seconds.
/// `/usr/bin/true` runs under the same profile, so the refusal is the assertion's and not
/// the profile failing to start a program.
#[test]
fn a_confined_agent_cannot_hold_the_computer_awake() {
    let case = case(SandboxNetwork::Only(Vec::new()));
    let ran = confined(&case, "/usr/bin/true", &[]);
    assert!(ran.status.success(), "{ran:?}");
    let started = Instant::now();
    let held = confined(&case, CAFFEINATE, &["-i", "-t", HOLD_SECONDS]);
    let said = String::from_utf8_lossy(&held.stderr);
    assert!(!held.status.success(), "caffeinate held: {held:?}");
    assert!(said.contains(REFUSED), "refused for another reason: {said}");
    assert!(said.contains("assertion"), "{said}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "caffeinate waited out its timeout, so it held"
    );
    assert_eq!(timed_holders(HOLD_SECONDS), 0);
}

/// The engine's own hold does not run under a profile: with a confined agent refused, the
/// Keep awake hold for an engine pid is still taken and released.
#[test]
fn the_engines_own_hold_still_works_beside_a_confined_agent() {
    let case = case(SandboxNetwork::Only(Vec::new()));
    let mut engine = crate::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("120")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let pid = engine.id();
    assert!(pid > 1);
    let mut hold = platform_hold(pid);
    let held = hold.apply(&Want::Idle { display: false }).unwrap();
    assert!(held.idle);
    assert!(until(|| engine_holders(pid) == 1));
    let refused = confined(&case, CAFFEINATE, &["-i", "-t", HOLD_SECONDS]);
    assert!(!refused.status.success(), "{refused:?}");
    assert_eq!(engine_holders(pid), 1, "the engine's hold stays");
    hold.release();
    assert!(until(|| engine_holders(pid) == 0));
    engine.kill().unwrap();
    engine.wait().unwrap();
}

const NC: &str = "/usr/bin/nc";
const SH: &str = "/bin/sh";
const HELPER_DENY: &str = concat!(
    "(deny network-outbound\n",
    "  (remote unix-socket (path-literal \"/var/run/dev.crazyagent.power.sock\"))\n",
    "  (remote unix-socket (path-literal \"/private/var/run/dev.crazyagent.power.sock\")))\n",
    "(deny mach-lookup (global-name \"dev.crazyagent.power\"))\n"
);

/// Connects to a Unix socket under a profile written as given, with `nc -U`. Inputs: the case's
/// sandbox, the profile path and text, and the socket. Output: the finished `nc`.
fn connect_under(sandbox: &ProjectSandbox, profile: &Path, text: &str, socket: &Path) -> Output {
    fs::write(profile, text).unwrap();
    let mut command = sandbox.command(profile, Path::new(NC));
    command
        .args(["-U", socket.to_str().unwrap()])
        .current_dir(sandbox.project_root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::spawn_gate::output(&mut command).unwrap()
}

#[test]
fn an_agent_sandbox_cannot_connect_to_the_power_helper_socket() {
    let Case {
        _temp: temp,
        profile,
        sandbox,
    } = case(SandboxNetwork::Only(Vec::new()));
    let socket = temp.path().canonicalize().unwrap().join("helper.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let grant = policy::AccessGrant::Socket {
        path: socket.clone(),
    };
    let sandbox = sandbox.grant_access(&[grant], None).unwrap();
    let granted = sandbox.profile();
    assert_eq!(power_helper_denies(), HELPER_DENY);
    assert!(granted.ends_with(HELPER_DENY), "the real profile ends with the helper deny");
    let served = std::thread::spawn(move || listener.accept().map(drop));
    let open = connect_under(&sandbox, &profile, &granted, &socket);
    assert!(open.status.success(), "{}", String::from_utf8_lossy(&open.stderr));
    served.join().unwrap().unwrap();
    let path = socket.to_str().unwrap();
    let denied = format!("{granted}{}", power_helper_denies_for(&[path]));
    assert!(denied.ends_with(&format!(
        concat!(
            "(deny network-outbound\n  (remote unix-socket (path-literal \"{}\")))\n",
            "(deny mach-lookup (global-name \"dev.crazyagent.power\"))\n"
        ),
        path
    )));
    let shut = connect_under(&sandbox, &profile, &denied, &socket);
    assert!(!shut.status.success());
    let said = String::from_utf8_lossy(&shut.stderr);
    assert!(said.contains("Operation not permitted"), "a deny after the grant wins: {said}");
}

#[test]
fn a_socket_grant_naming_the_power_helper_is_refused() {
    for spelling in [
        "/var/run/dev.crazyagent.power.sock",
        "/private/var/run/dev.crazyagent.power.sock",
    ] {
        let grant = policy::AccessGrant::Socket {
            path: PathBuf::from(spelling),
        };
        let refused = case(SandboxNetwork::Only(Vec::new()))
            .sandbox
            .grant_access(&[grant], None)
            .map(drop);
        let named = matches!(
            &refused,
            Err(SandboxError::Invalid(reason)) if reason.contains("lid helper")
        );
        assert!(named, "{spelling}: {refused:?}");
    }
}

#[test]
fn the_power_helper_deny_comes_after_every_allow_in_every_profile() {
    let open = case(SandboxNetwork::Open);
    let limited = case(SandboxNetwork::Only(Vec::new()));
    let folder = limited._temp.path().canonicalize().unwrap();
    let priming = priming_profile(&folder, Path::new("/usr/bin/true"));
    for text in [open.sandbox.profile(), limited.sandbox.profile(), priming] {
        assert!(text.ends_with(HELPER_DENY), "{text}");
        let deny = text.len() - HELPER_DENY.len();
        assert!(text.rfind("(allow ").is_some_and(|allow| allow < deny));
        assert_eq!(text.matches(POWER_HELPER_SOCKET).count(), 1);
    }
}

#[test]
fn an_agent_sandbox_cannot_write_the_app_bundle() {
    let case = case(SandboxNetwork::Only(Vec::new()));
    let root = case._temp.path().canonicalize().unwrap();
    let contents = root.join("Applications/CrazyAgent.app/Contents");
    let helper = contents.join("MacOS/crazyagent-power-helper");
    let plist = contents.join("Library/LaunchDaemons/dev.crazyagent.power.plist");
    for file in [&helper, &plist] {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "signed\n").unwrap();
    }
    for file in [&helper, &plist] {
        let script = format!("echo changed > '{}'", file.display());
        let wrote = confined(&case, SH, &["-c", &script]);
        assert!(!wrote.status.success(), "{}", file.display());
        assert_eq!(fs::read_to_string(file).unwrap(), "signed\n");
        let moved = confined(&case, "/bin/mv", &[file.to_str().unwrap(), "/tmp/out"]);
        assert!(!moved.status.success(), "{}", file.display());
        assert!(file.exists());
    }
}
