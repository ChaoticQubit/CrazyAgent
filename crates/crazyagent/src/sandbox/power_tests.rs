use super::{MACH_SERVICES, ProjectSandbox, SandboxNetwork, priming_profile};
use crate::keep_awake::{Want, platform_hold};
use std::fs;
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
