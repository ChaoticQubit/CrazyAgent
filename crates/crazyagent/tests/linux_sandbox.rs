#![cfg(target_os = "linux")]
use crazyagent::sandbox::linux::{BWRAP_PATHS, HelperRequest, LinuxBackend, LinuxSandbox, NEST_ARGS};

use crazyagent::sandbox::{
    InstalledSandbox, LinuxSockets, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork,
    SandboxRemote,
};

use landlock::{CompatLevel, Compatible, Ruleset, RulesetAttr, Scope};

use std::ffi::{OsStr, OsString};

use std::fs;

use std::io::{BufRead, BufReader, Read, Write};

use std::path::{Path, PathBuf};

use std::process::{Child, ChildStdout, Command, ExitStatus, Output, Stdio};

use std::time::{Duration, Instant};

const HELPER: &str = env!("CARGO_BIN_EXE_crazyagent-landlock");

struct Case {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    outside: PathBuf,
}

fn case() -> Case {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    let outside = root.join("outside");
    let git = project.join(".git");
    for directory in [
        &project,
        &home,
        &tmp,
        &outside,
        &project.join("context"),
        &project.join("worktrees"),
        &git.join("hooks"),
        &git.join("objects"),
        &git.join("modules"),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(project.join("context").join("brief.md"), "brief\n").unwrap();
    fs::write(git.join("config"), "[core]\n").unwrap();
    fs::write(outside.join("fixture.txt"), "outside fixture\n").unwrap();
    std::os::unix::fs::symlink(&outside, project.join("link")).unwrap();
    Case {
        _temp: temp,
        project,
        home,
        tmp,
        outside,
    }
}

fn policy(case: &Case, network: SandboxNetwork) -> ProjectSandbox {
    ProjectSandbox::new(&case.project, &case.home, &case.tmp, network).unwrap()
}

/// The plain Landlock backend for a policy: detection with no bubblewrap to look for, so a
/// row about that backend holds on a machine that would otherwise nest the agent. Input:
/// the policy. Output: the sandbox, which each row then asserts is Landlock.
fn pinned(sandbox: ProjectSandbox) -> LinuxSandbox {
    LinuxSandbox::detect_among(sandbox, Path::new(HELPER), &[]).unwrap()
}

fn quoted(path: &Path) -> String {
    format!("'{}'", path.display())
}

fn run(sandbox: &dyn Sandbox, case: &Case, script: &str) -> Output {
    let mut harness = Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(script)
        .current_dir(case.project.join("worktrees"));
    crazyagent::spawn_gate::output(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap()
}

fn boundary_script(case: &Case) -> String {
    let worktrees = case.project.join("worktrees");
    let git = case.project.join(".git");
    format!(
        "echo inside > {inside} && echo home > {home} && \
         (echo x > {escape} || true) && (echo x >> {brief} || true) && \
         (echo x > {hook} || true) && (echo x > {top} || true) && \
         (echo x > {module} || true) && \
         (echo x > {via_link} || true) && (cat {fixture} > {leak} || true)",
        inside = quoted(&worktrees.join("inside.txt")),
        home = quoted(&case.home.join("home.txt")),
        escape = quoted(&case.outside.join("escape.txt")),
        brief = quoted(&case.project.join("context").join("brief.md")),
        hook = quoted(&git.join("hooks").join("pre-commit")),
        top = quoted(&case.project.join("top.txt")),
        module = quoted(&git.join("modules").join("config")),
        via_link = quoted(&case.project.join("link").join("via-link.txt")),
        fixture = quoted(&case.outside.join("fixture.txt")),
        leak = quoted(&worktrees.join("leak.txt")),
    )
}

fn assert_boundary(case: &Case, output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let worktrees = case.project.join("worktrees");
    assert_eq!(
        fs::read_to_string(worktrees.join("inside.txt")).unwrap(),
        "inside\n"
    );
    assert_eq!(
        fs::read_to_string(case.home.join("home.txt")).unwrap(),
        "home\n"
    );
    assert!(!case.outside.join("escape.txt").exists());
    assert_eq!(
        fs::read_to_string(case.project.join("context").join("brief.md")).unwrap(),
        "brief\n"
    );
    assert!(
        !case
            .project
            .join(".git")
            .join("hooks")
            .join("pre-commit")
            .exists()
    );
    assert!(!case.project.join("top.txt").exists());
    assert!(
        !case
            .project
            .join(".git")
            .join("modules")
            .join("config")
            .exists()
    );
    assert!(!case.outside.join("via-link.txt").exists());
    assert!(
        !fs::read_to_string(worktrees.join("leak.txt"))
            .unwrap_or_default()
            .contains("outside fixture")
    );
}

/// The state field of `/proc/<pid>/stat`, or None once the process is gone. Input: the
/// process id. Output: the state letter, `Z` for one that exited and waits to be reaped.
fn process_state(pid: u32) -> Option<char> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(") ")?.1.chars().next()
}

/// RV-DS241-142: a helper run told to die with its parent (`--die-with <pid>`, as every engine
/// git step is) starts its program, which the helper became, and that program is killed once
/// its parent exits, so a git step never outlives the engine that took its lock. A parent id
/// that is not the helper's parent is refused (exit 3) before the program starts, and
/// process 1 is never named.
#[test]
fn a_helper_told_to_die_with_its_parent_is_killed_when_that_parent_exits() {
    let case = case();
    let started = case.tmp.join("started");
    let parent = Command::new("/bin/sh")
        .arg("-c")
        .arg(
            "\"$0\" --die-with $$ --read / --write \"$1\" -- \
             /bin/sh -c 'echo $$ > \"$0\"; exec /bin/sleep 60' \"$2\" & /bin/sleep 2",
        )
        .arg(HELPER)
        .arg(&case.tmp)
        .arg(&started)
        .stdin(Stdio::null())
        .status()
        .unwrap();
    assert!(parent.success());
    let pid: u32 = fs::read_to_string(&started)
        .expect("the helper started its program")
        .trim()
        .parse()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while process_state(pid).is_some_and(|state| state != 'Z')
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        process_state(pid).is_none_or(|state| state == 'Z'),
        "the program outlived its parent"
    );

    let refused = case.tmp.join("refused");
    let status = Command::new(HELPER)
        .args(["--die-with", &i32::MAX.to_string(), "--read", "/", "--write"])
        .arg(&case.tmp)
        .args(["--", "/bin/sh", "-c", "echo ran > \"$0\""])
        .arg(&refused)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3), "a parent id that is not the parent");
    assert!(!refused.exists(), "nothing started for a wrong parent id");
    let first = Command::new(HELPER)
        .args(["--die-with", "1", "--read", "/", "--", "/bin/true"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(first.code(), Some(2), "process 1 is refused as an argument");
}

#[test]
fn landlock_holds_the_project_boundary_on_this_kernel() {
    let case = case();
    let sandbox = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(sandbox.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        sandbox.backend()
    );
    assert_boundary(&case, &run(&sandbox, &case, &boundary_script(&case)));
}

#[test]
fn bubblewrap_holds_the_same_project_boundary() {
    let case = case();
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let sandbox = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_boundary(&case, &run(&sandbox, &case, &boundary_script(&case)));
}

const TRUNCATE: &str = "import os, sys\n\
for path in sys.argv[1:]:\n    \
try:\n        os.truncate(path, 0)\n        print('truncated')\n    \
except OSError:\n        print('refused')\n";

#[test]
fn neither_backend_lets_an_agent_truncate_a_file_it_may_only_read() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let own = case.project.join("worktrees").join("own.txt");
    let brief = case.project.join("context").join("brief.md");
    let fixture = case.outside.join("fixture.txt");
    let landlock = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    for sandbox in [&landlock as &dyn Sandbox, &bubblewrap] {
        fs::write(&own, "own\n").unwrap();
        let mut harness = Command::new("/usr/bin/python3");
        harness
            .args(["-I", "-c", TRUNCATE])
            .arg(&own)
            .arg(&brief)
            .arg(&fixture)
            .current_dir(case.project.join("worktrees"));
        let output = crazyagent::spawn_gate::output(
            sandbox
                .wrap(&harness)
                .unwrap()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "truncated\nrefused\nrefused\n"
        );
        assert_eq!(fs::read_to_string(&own).unwrap(), "");
        assert_eq!(fs::read_to_string(&brief).unwrap(), "brief\n");
        assert_eq!(fs::read_to_string(&fixture).unwrap(), "outside fixture\n");
    }
}

const SESSION: &str = "read -r stat < /proc/$$/stat; set -- ${stat##*\") \"}; echo \"$$ $3 $4 $5\"";

/// Runs a wrapped [`SESSION`] command to its end. Takes the command. Returns the four
/// numbers it printed, as Linux shows them to the agent: its process id, its process group,
/// its session and its controlling terminal.
fn session_fields(command: &mut Command) -> Vec<String> {
    let output = crazyagent::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
    assert_eq!(fields.len(), 4, "{text}");
    fields
}

fn assert_own_session(command: &mut Command) {
    let fields = session_fields(command);
    assert_eq!(
        fields[1], fields[0],
        "the agent leads its process group: {fields:?}"
    );
    assert_eq!(
        fields[2], fields[0],
        "the agent leads its own session: {fields:?}"
    );
    assert_eq!(
        fields[3], "0",
        "the agent has no controlling terminal: {fields:?}"
    );
}

/// Checks an agent under bubblewrap, which runs it in a process namespace of its own: the
/// group and the session are led by that namespace's first process, bubblewrap's own, in a
/// session made for this agent, with no controlling terminal. A session that began outside
/// the namespace, such as the engine's, would read 0 there, never 1. Takes the wrapped
/// [`SESSION`] command.
fn assert_boundary_session(command: &mut Command) {
    let fields = session_fields(command);
    assert_eq!(
        fields[1], "1",
        "the boundary's first process leads the agent's group: {fields:?}"
    );
    assert_eq!(
        fields[2], "1",
        "the boundary's first process leads a session of its own: {fields:?}"
    );
    assert_eq!(
        fields[3], "0",
        "the agent has no controlling terminal: {fields:?}"
    );
}

const SIGNAL: &str = "kill -0 \"$1\" 2>/dev/null && echo delivered || echo refused";

fn signal(sandbox: Option<&dyn Sandbox>, case: &Case, target: u32) -> String {
    let mut harness = Command::new("/bin/sh");
    harness
        .args(["-c", SIGNAL, "sh"])
        .arg(target.to_string())
        .current_dir(case.project.join("worktrees"));
    agent_stdout(sandbox, harness)
}

fn agent_stdout(sandbox: Option<&dyn Sandbox>, harness: Command) -> String {
    let mut command = match sandbox {
        Some(sandbox) => sandbox.wrap(&harness).unwrap(),
        None => harness,
    };
    let output = crazyagent::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn no_agent_signals_an_outside_process_where_the_kernel_can_scope_signals() {
    let case = case();
    let mut outside = crazyagent::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let target = outside.id();
    let scoped = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .scope(Scope::Signal)
        .is_ok();
    assert_eq!(
        signal(None, &case, target),
        "delivered\n",
        "the unsandboxed control must reach the outside process"
    );
    let landlock = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    assert_eq!(
        signal(Some(&landlock), &case, target),
        if scoped { "refused\n" } else { "delivered\n" },
        "Landlock scopes signals from Linux 6.12; scoped here: {scoped}"
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(signal(Some(&bubblewrap), &case, target), "refused\n");
    outside.kill().unwrap();
    outside.wait().unwrap();
}

const HOLD_TERMINAL: &str = "import os, sys
primary, secondary = os.openpty()
print(os.ttyname(secondary), flush=True)
sys.stdin.read()
";

const OPEN_TERMINAL: &str = "import os, sys
try:
    os.close(os.open(sys.argv[1], os.O_RDWR | os.O_NOCTTY))
    print('opened')
except OSError:
    print('refused')
";

fn open_terminal(sandbox: Option<&dyn Sandbox>, case: &Case, terminal: &Path) -> String {
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", OPEN_TERMINAL])
        .arg(terminal)
        .current_dir(case.project.join("worktrees"));
    agent_stdout(sandbox, harness)
}

#[test]
fn no_agent_opens_a_terminal_the_owner_holds_outside_its_boundary() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let mut holder = crazyagent::spawn_gate::spawn(
        Command::new("/usr/bin/python3")
            .args(["-I", "-c", HOLD_TERMINAL])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let terminal = PathBuf::from(line.trim_end());
    assert!(terminal.starts_with("/dev/pts"), "{line}");
    assert_eq!(
        open_terminal(None, &case, &terminal),
        "opened\n",
        "the unsandboxed control must open the held terminal"
    );
    let landlock = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    assert_eq!(
        open_terminal(Some(&landlock), &case, &terminal),
        "refused\n"
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(
        open_terminal(Some(&bubblewrap), &case, &terminal),
        "refused\n"
    );
    drop(holder.stdin.take());
    holder.wait().unwrap();
}

/// No agent shares the engine's session or terminal. Under Landlock the agent itself leads
/// its group and session, however it is started. Under bubblewrap the first process of the
/// agent's own process namespace leads them, see [`assert_boundary_session`].
#[test]
fn every_agent_leads_a_session_of_its_own_without_the_engine_terminal() {
    let case = case();
    let mut harness = Command::new("/bin/sh");
    harness
        .args(["-c", SESSION])
        .current_dir(case.project.join("worktrees"));
    let landlock = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    let wrapped = landlock.wrap(&harness).unwrap();
    assert_own_session(&mut landlock.wrap(&harness).unwrap());
    let mut ungrouped = Command::new(wrapped.get_program());
    ungrouped
        .args(wrapped.get_args())
        .env_clear()
        .envs(
            wrapped
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .current_dir(wrapped.get_current_dir().unwrap());
    assert_own_session(&mut ungrouped);
    let setsid = Path::new("/usr/bin/setsid");
    assert!(
        setsid.is_file(),
        "Linux test machines need util-linux setsid"
    );
    let mut leader = Command::new(setsid);
    leader
        .arg(wrapped.get_program())
        .args(wrapped.get_args())
        .env_clear()
        .envs(
            wrapped
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .current_dir(wrapped.get_current_dir().unwrap());
    assert_own_session(&mut leader);
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_boundary_session(&mut bubblewrap.wrap(&harness).unwrap());
}

#[test]
fn an_agent_adds_top_level_entries_to_its_own_worktree_but_not_to_the_project_folder() {
    let case = case();
    let worktree = case
        .outside
        .parent()
        .unwrap()
        .join("lease")
        .join("worktree");
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("README.md"), "lease\n").unwrap();
    let leased =
        |network: SandboxNetwork| policy(&case, network).allow_worktree(&worktree).unwrap();
    let script = format!(
        "mkdir {folder} && echo made > {file} && \
         (echo x > {top} || true) && (mkdir {top_folder} || true)",
        folder = quoted(&worktree.join("src2")),
        file = quoted(&worktree.join("Makefile")),
        top = quoted(&case.project.join("new.txt")),
        top_folder = quoted(&case.project.join("src3")),
    );
    let landlock = pinned(leased(SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        leased(SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    for sandbox in [&landlock as &dyn Sandbox, &bubblewrap] {
        let mut harness = Command::new("/bin/sh");
        harness.arg("-c").arg(&script).current_dir(&worktree);
        let output = crazyagent::spawn_gate::output(
            sandbox
                .wrap(&harness)
                .unwrap()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped()),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(worktree.join("src2").is_dir());
        assert_eq!(
            fs::read_to_string(worktree.join("Makefile")).unwrap(),
            "made\n"
        );
        assert!(!case.project.join("new.txt").exists());
        assert!(!case.project.join("src3").exists());
        fs::remove_dir(worktree.join("src2")).unwrap();
        fs::remove_file(worktree.join("Makefile")).unwrap();
    }
}

#[test]
fn landlock_limits_a_tools_off_agent_to_its_model_endpoint_port() {
    let case = case();
    let allowed = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let denied = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let allowed_port = allowed.local_addr().unwrap().port();
    let denied_port = denied.local_addr().unwrap().port();
    let network = SandboxNetwork::Only(vec![SandboxRemote::Loopback(allowed_port)]);
    let sandbox = pinned(policy(&case, network.clone()));
    assert!(
        matches!(sandbox.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock network rules (Linux 6.7 or newer): {:?}",
        sandbox.backend()
    );
    let mut harness = Command::new("/bin/bash");
    harness
        .arg("-c")
        .arg(format!(
            "(exec 3<>/dev/tcp/127.0.0.1/{allowed_port}) && echo allowed; \
             (exec 3<>/dev/tcp/127.0.0.1/{denied_port}) 2>/dev/null && echo denied; true"
        ))
        .current_dir(&case.project);
    let output = crazyagent::spawn_gate::output(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout), "allowed\n");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, network),
        LinuxBackend::Bubblewrap {
            bwrap: PathBuf::from("/bin/sh"),
            helper: PathBuf::from(HELPER),
        },
    );
    assert!(matches!(bubblewrap, Err(SandboxError::Unavailable(_))));
}

#[test]
fn the_helper_reads_back_the_policy_the_engine_wrote_without_protected_paths_or_links() {
    let case = case();
    let network = SandboxNetwork::Only(vec![
        SandboxRemote::Loopback(8080),
        SandboxRemote::AnyHost(443),
        SandboxRemote::AnyHost(8080),
    ]);
    let sandbox = LinuxSandbox::new(
        policy(&case, network),
        LinuxBackend::Landlock {
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    let mut harness = Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg("true")
        .env("HOME", "/root")
        .env("HARNESS_MODE", "interactive")
        .current_dir(case.project.join("worktrees"));
    let wrapped = sandbox.wrap(&harness).unwrap();
    assert_eq!(wrapped.get_program(), HELPER);
    let request = HelperRequest::parse(wrapped.get_args().map(OsString::from).collect()).unwrap();
    let HelperRequest::Run {
        policy,
        program,
        args,
    } = request
    else {
        panic!("expected a run request");
    };
    assert_eq!(program, Path::new("/bin/sh"));
    assert_eq!(args, ["-c", "true"].map(OsString::from));
    assert!(policy.limit_network);
    assert_eq!(policy.connect, [53, 443, 8080]);
    assert!(
        wrapped
            .get_envs()
            .any(|(name, value)| name == "RES_OPTIONS" && value == Some(OsStr::new("use-vc")))
    );
    assert!(policy.read.contains(&case.project));
    let git = case.project.join(".git");
    for granted in [
        case.project.join("worktrees"),
        case.home.clone(),
        case.tmp.clone(),
    ] {
        assert!(policy.write.contains(&granted), "{}", granted.display());
    }
    for refused in [
        case.project.clone(),
        case.project.join("context"),
        case.project.join("link"),
        git.clone(),
        git.join("hooks"),
        git.join("config"),
        git.join("objects"),
        git.join("modules"),
    ] {
        assert!(!policy.write.contains(&refused), "{}", refused.display());
    }
    let envs: Vec<_> = wrapped.get_envs().collect();
    assert!(envs.contains(&(OsStr::new("HOME"), Some(case.home.as_os_str()))));
    assert!(envs.contains(&(OsStr::new("HARNESS_MODE"), Some(OsStr::new("interactive")))));
    assert_eq!(
        wrapped.get_current_dir(),
        Some(case.project.join("worktrees").as_path())
    );
    let mut outside = Command::new("/bin/sh");
    outside.current_dir(&case.outside);
    assert!(matches!(
        sandbox.wrap(&outside),
        Err(SandboxError::Invalid(_))
    ));
    let mut relative = Command::new("sh");
    relative.current_dir(&case.project);
    assert!(matches!(
        sandbox.wrap(&relative),
        Err(SandboxError::Invalid(_))
    ));
    let parse = |args: &[&str]| HelperRequest::parse(args.iter().map(OsString::from).collect());
    assert!(parse(&["--write", "relative", "--", "/bin/sh"]).is_err());
    assert!(parse(&["--connect", "0", "--", "/bin/sh"]).is_err());
    assert!(parse(&["--probe", "--", "/bin/sh"]).is_err());
    assert_eq!(
        parse(&["--probe", "--limit-network"]),
        Ok(HelperRequest::Probe {
            limit_network: true,
            open_sockets: false,
            keep_free: false,
        })
    );
    assert_eq!(
        parse(&["--probe", "--limit-network", "--open-sockets"]),
        Ok(HelperRequest::Probe {
            limit_network: true,
            open_sockets: true,
            keep_free: false,
        })
    );
    assert!(parse(&["--probe", "--open-sockets"]).is_err());
    assert!(parse(&["--open-sockets", "--", "/bin/sh"]).is_err());
}

#[test]
fn an_installed_sandbox_on_linux_launches_through_the_linux_boundary() {
    let case = case();
    let profiles = case.outside.join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    let installed = InstalledSandbox::install(
        policy(&case, SandboxNetwork::Open),
        &profiles.join("agent.sb"),
    )
    .unwrap();
    let launcher = installed.launcher().to_path_buf();
    assert!(
        launcher.ends_with("crazyagent-landlock")
            || BWRAP_PATHS.iter().any(|path| launcher == Path::new(path)),
        "{}",
        launcher.display()
    );
    let output = run(&installed, &case, &boundary_script(&case));
    assert_boundary(&case, &output);
}

#[test]
fn only_agent_worktrees_in_the_crazyagent_folder_are_writable() {
    let case = case();
    let state = case.project.join(".crazyagent");
    for directory in ["context", "worktrees", "setup"] {
        fs::create_dir_all(state.join(directory)).unwrap();
    }
    fs::write(state.join("context").join("brief.md"), "brief\n").unwrap();
    let sandbox = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Landlock {
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    let mut harness = Command::new("/bin/sh");
    harness.current_dir(state.join("worktrees"));
    let wrapped = sandbox.wrap(&harness).unwrap();
    let HelperRequest::Run { policy, .. } =
        HelperRequest::parse(wrapped.get_args().map(OsString::from).collect()).unwrap()
    else {
        panic!("expected a run request");
    };
    for granted in [state.join("worktrees"), state.join("setup")] {
        assert!(policy.write.contains(&granted), "{}", granted.display());
    }
    for refused in [state.clone(), state.join("context")] {
        assert!(!policy.write.contains(&refused), "{}", refused.display());
    }
}

const REACH: &str = "import os, socket, sys
try:
    socket.socketpair()
    print('pair=ok')
except OSError:
    print('pair=refused')
try:
    socket.socket(socket.AF_UNIX).connect(sys.argv[1])
    print('unix=ok')
except OSError:
    print('unix=refused')
try:
    socket.create_connection(('127.0.0.1', int(sys.argv[2])), 5).close()
    print('tcp=ok')
except OSError:
    print('tcp=refused')
print('runtime=present' if os.path.exists(sys.argv[3]) else 'runtime=absent')
";

fn reach(
    sandbox: Option<&dyn Sandbox>,
    case: &Case,
    socket: &Path,
    port: u16,
    runtime: &Path,
) -> String {
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", REACH])
        .arg(socket)
        .arg(port.to_string())
        .arg(runtime)
        .current_dir(case.project.join("worktrees"));
    let mut command = match sandbox {
        Some(sandbox) => sandbox.wrap(&harness).unwrap(),
        None => harness,
    };
    let output = crazyagent::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn a_linux_agent_reaches_ip_endpoints_but_no_unix_socket_in_the_engine_runtime() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let runtime = case.outside.parent().unwrap().join("runtime");
    let terminal = runtime.join("terminal");
    fs::create_dir_all(&terminal).unwrap();
    let socket = terminal.join("tmux.sock");
    let _listener = crazyagent::spawn_gate::bind_unix_std(&socket).unwrap();
    let endpoint = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let guarded = |network: SandboxNetwork| policy(&case, network).guard_engine(&runtime).unwrap();
    assert_eq!(
        reach(None, &case, &socket, port, &runtime),
        "pair=ok\nunix=ok\ntcp=ok\nruntime=present\n",
        "the unsandboxed control must reach the socket"
    );
    let landlock = pinned(guarded(SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock and seccomp: {:?}",
        landlock.backend()
    );
    assert_eq!(
        reach(Some(&landlock), &case, &socket, port, &runtime),
        "pair=ok\nunix=refused\ntcp=ok\nruntime=present\n"
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        guarded(SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(
        reach(Some(&bubblewrap), &case, &socket, port, &runtime),
        "pair=ok\nunix=refused\ntcp=refused\nruntime=absent\n",
        "bubblewrap starts an agent only without a network"
    );
}

/// A closed bubblewrap agent reaches no Unix socket even in a folder bound for it, such as an
/// owner's socket in the project folder: the helper's socket filter refuses every Unix-domain
/// socket there as it does under Landlock (RV-DS241-84).
#[test]
fn a_bubblewrap_agent_reaches_no_unix_socket_in_a_folder_bound_for_it() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let runtime = case.outside.parent().unwrap().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    let socket = case.project.join("owner.sock");
    let _listener = crazyagent::spawn_gate::bind_unix_std(&socket).unwrap();
    let endpoint = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = endpoint.local_addr().unwrap().port();
    assert_eq!(
        reach(None, &case, &socket, port, &runtime),
        "pair=ok\nunix=ok\ntcp=ok\nruntime=present\n",
        "the unsandboxed control must reach the socket"
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(
        reach(Some(&bubblewrap), &case, &socket, port, &runtime),
        "pair=ok\nunix=refused\ntcp=refused\nruntime=absent\n"
    );
}

const FAMILIES: &str = "import errno, socket
for name, family, kind, protocol in (('unix', socket.AF_UNIX, socket.SOCK_STREAM, 0),
                                     ('vsock', 40, socket.SOCK_STREAM, 0),
                                     ('bluetooth', 31, socket.SOCK_STREAM, 0),
                                     ('packet', 17, socket.SOCK_DGRAM, 0),
                                     ('alg', 38, socket.SOCK_SEQPACKET, 0),
                                     ('udp', socket.AF_INET, socket.SOCK_DGRAM, 0),
                                     ('audit', 16, socket.SOCK_RAW, 9),
                                     ('tcp', socket.AF_INET, socket.SOCK_STREAM, 0),
                                     ('route', 16, socket.SOCK_RAW, 0)):
    try:
        socket.socket(family, kind, protocol).close()
        print(name + '=ok')
    except OSError as error:
        print(name + ('=refused' if error.errno == errno.EACCES else '=other'))
";

/// Runs the socket family probe through `command`, which starts `/usr/bin/python3` with
/// the arguments it is given. Input: the command. Output: one line per socket kind, `ok`,
/// `refused` for the socket filter's `EACCES`, or `other` for any other failure.
fn families(mut command: Command) -> String {
    let output = crazyagent::spawn_gate::output(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A closed bubblewrap agent opens no socket its network namespace does not hold: the
/// helper's socket filter keeps only TCP over IPv4 and routing netlink, as for a Landlock
/// agent with a network limit, so an AF_VSOCK socket, which `--unshare-net` does not
/// isolate and which reaches a VM host, is refused as a Unix-domain one is, and so is every
/// other family, datagram and netlink kind (RV-DS241-84 r2, rv-sec3 F1).
#[test]
fn a_closed_bubblewrap_agent_opens_no_socket_its_network_namespace_does_not_hold() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let expected = "unix=refused\nvsock=refused\nbluetooth=refused\npacket=refused\n\
                    alg=refused\nudp=refused\naudit=refused\ntcp=ok\nroute=ok\n";
    let mut filtered = Command::new(HELPER);
    filtered
        .args(["--filter-sockets", "--", "/usr/bin/python3", "-I", "-c", FAMILIES])
        .current_dir(&case.project);
    assert_eq!(families(filtered), expected, "the helper's socket filter alone");
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", FAMILIES])
        .current_dir(case.project.join("worktrees"));
    assert_eq!(
        families(bubblewrap.wrap(&harness).unwrap()),
        expected,
        "a closed bubblewrap agent"
    );
}

const LISTEN: &str = "import socket, sys
for port in sys.argv[1:]:
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        listener.bind(('127.0.0.1', int(port)))
        listener.listen(1)
        print(port + '=bound')
    except PermissionError:
        print(port + '=refused')
    except OSError:
        print(port + '=taken')
    listener.close()
for name, family, protocol in (('mptcp', socket.AF_INET, 262), ('mptcp6', socket.AF_INET6, 262),
                               ('smc', 43, 0)):
    try:
        socket.socket(family, socket.SOCK_STREAM, protocol).close()
        print(name + '=ok')
    except PermissionError:
        print(name + '=refused')
    except OSError:
        print(name + '=absent')
";

/// Runs the listen probe inside a sandbox. Inputs: the sandbox, the case and the TCP ports
/// to listen on, port 0 asking Linux for a free one. Output: one line per port, then one
/// per socket kind that holds a TCP port without being plain TCP.
fn listens(sandbox: &dyn Sandbox, case: &Case, ports: &[u16]) -> String {
    port_probe(sandbox, case, LISTEN, ports)
}

const PICKS: &str = "import socket, sys
kept = set(int(port) for port in sys.argv[1:])
def held(bind):
    count = 0
    for _ in range(3000):
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        if bind:
            listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        if listener.getsockname()[1] in kept:
            count += 1
        listener.close()
    return count
print('bind0=' + str(held(True)))
print('listen=' + str(held(False)))
";

/// Runs a Python port probe inside a sandbox. Inputs: the sandbox, the case, the script
/// and the TCP ports it is given. Output: what the script printed.
fn port_probe(sandbox: &dyn Sandbox, case: &Case, script: &str, ports: &[u16]) -> String {
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", script])
        .args(ports.iter().map(u16::to_string))
        .current_dir(case.project.join("worktrees"));
    let output = crazyagent::spawn_gate::output(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// An agent with an open network listens on any TCP port but the owner's fixed ports, a
/// kept local model port and the model proxy ports, and opens no stream socket that would
/// hold such a port without being plain TCP. Bubblewrap, which keeps no port free, starts
/// no open-network agent at all (DEC-137 addendum 2).
#[test]
fn an_open_network_agent_listens_on_no_owner_model_or_proxy_port() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let kept: u16 = 18_080;
    let open = || {
        policy(&case, SandboxNetwork::Open)
            .keep_ports(&[kept])
            .unwrap()
    };
    let landlock = pinned(open());
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock network rules (Linux 6.7 or newer): {:?}",
        landlock.backend()
    );
    assert_eq!(
        listens(&landlock, &case, &[1455, 11434, 29_300, 29_427, kept, 0]),
        "1455=refused\n11434=refused\n29300=refused\n29427=refused\n18080=refused\n0=bound\n\
         mptcp=refused\nmptcp6=refused\nsmc=refused\n"
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    match LinuxSandbox::new(
        open(),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    ) {
        Err(SandboxError::Unavailable(reason)) => assert_eq!(
            reason,
            crazyagent::sandbox::LINUX_OPEN_NETWORK_NEEDS_LANDLOCK
        ),
        other => panic!("bubblewrap must refuse an open network: {other:?}"),
    }
}

/// LPF1-M1: Landlock checks only the port a bind names, so a port Linux picks itself, for
/// a bind to port 0 or a listen with no bind, must never be one the owner's side keeps.
/// The kept ports lie below the range Linux picks from, so thousands of picks hold none.
#[test]
fn a_port_linux_picks_for_an_open_network_agent_is_never_a_kept_port() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let kept: u16 = 18_080;
    let open = policy(&case, SandboxNetwork::Open)
        .keep_ports(&[kept])
        .unwrap();
    let landlock = pinned(open);
    let mut ports: Vec<u16> = vec![1455, 11434, kept];
    ports.extend(29_300..=29_427);
    assert_eq!(
        port_probe(&landlock, &case, PICKS, &ports),
        "bind0=0\nlisten=0\n"
    );
}

const LISTENERS: &str = "import os, socket
def listen(name, family, address):
    try:
        listener = socket.socket(family, socket.SOCK_STREAM)
    except PermissionError:
        print(name + '=refused')
        return
    try:
        if address is not None:
            listener.bind(address)
        listener.listen(1)
        print(name + '=listened')
    except PermissionError:
        print(name + '=refused')
    finally:
        listener.close()
listen('bind0', socket.AF_INET, ('127.0.0.1', 0))
listen('listen', socket.AF_INET, None)
listen('unix', socket.AF_UNIX, os.path.join(os.environ['TMPDIR'], 'agent.sock'))
";

/// Reads the range Linux hands out by itself on this machine. No input. Output: its first
/// port.
fn lowest_picked_port() -> u16 {
    let range = fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range").unwrap();
    range
        .split_ascii_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// REL220-F2: a listen on a socket that was never bound takes a port Linux picks with no
/// bind check, on a limited network as on an open one. So a kept port inside the range
/// Linux picks from refuses a limited launch with the open network's sentence, under both
/// limited-network sockets settings, by the engine and by a helper started without it.
#[test]
fn a_limited_network_with_a_kept_port_linux_could_pick_is_refused_at_launch() {
    let case = case();
    let low = lowest_picked_port();
    for sockets in [LinuxSockets::TcpOnly, LinuxSockets::TcpPorts] {
        let limited = policy(&case, SandboxNetwork::Only(Vec::new()))
            .limit_sockets(sockets)
            .keep_ports(&[low])
            .unwrap();
        match LinuxSandbox::detect(limited, Path::new(HELPER)) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(reason.contains(&format!("port {low} ")), "{reason}");
                assert!(reason.contains("by itself"), "{reason}");
            }
            other => panic!("a kept port Linux could pick must refuse the launch: {other:?}"),
        }
    }
    let direct = crazyagent::spawn_gate::output(
        Command::new(HELPER)
            .args([
                "--limit-network",
                "--keep-free",
                &low.to_string(),
                "--",
                "/bin/true",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert_eq!(
        direct.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains(&format!("port {low} ")),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

/// REL220-F2 follow-up: bubblewrap starts a closed agent in a network of its own, where a
/// port it listens on is never a host port, so a kept port inside the range Linux picks
/// from refuses no closed bubblewrap launch. Landlock leaves the agent in the host's
/// network, where a listen with no bind takes a picked port, so the same port still refuses
/// an open, a limited and a closed Landlock launch.
#[test]
fn a_kept_port_linux_could_pick_refuses_every_landlock_launch_but_no_closed_bubblewrap_one() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let low = lowest_picked_port();
    let kept = |network: SandboxNetwork| policy(&case, network).keep_ports(&[low]).unwrap();
    for network in [
        SandboxNetwork::Open,
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
        SandboxNetwork::Only(Vec::new()),
    ] {
        match LinuxSandbox::detect(kept(network.clone()), Path::new(HELPER)) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(
                    reason.contains(&format!("port {low} ")),
                    "{network:?}: {reason}"
                );
            }
            other => panic!(
                "a kept port Linux could pick must refuse a Landlock launch on {network:?}: \
                 {other:?}"
            ),
        }
    }
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        kept(SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(
        port_probe(&bubblewrap, &case, LISTENERS, &[]),
        "bind0=listened\nlisten=listened\nunix=refused\n"
    );
}

/// REL220-F2: a limited launch whose kept ports lie outside the range Linux picks from
/// starts, and its agent keeps every TCP listen: a bare listen on a port Linux picks, and,
/// on bubblewrap, a bound one. A bind stays refused on Landlock, which allows a limited
/// agent no bind port, and the socket filter opens no Unix socket for any agent, on
/// Landlock as `a_linux_agent_reaches_ip_endpoints_but_no_unix_socket_in_the_engine_runtime`
/// shows and on bubblewrap, which starts every agent through the filter.
#[test]
fn a_limited_network_agent_with_kept_ports_outside_the_picked_ones_keeps_its_listeners() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let kept: u16 = 18_080;
    assert!(kept < lowest_picked_port());
    let limited = |sockets: LinuxSockets| {
        policy(&case, SandboxNetwork::Only(Vec::new()))
            .limit_sockets(sockets)
            .keep_ports(&[kept])
            .unwrap()
    };
    for sockets in [LinuxSockets::TcpOnly, LinuxSockets::TcpPorts] {
        let landlock = pinned(limited(sockets));
        assert!(
            matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
            "Linux test machines need Landlock network rules (Linux 6.7 or newer): {:?}",
            landlock.backend()
        );
        assert_eq!(
            port_probe(&landlock, &case, LISTENERS, &[]),
            "bind0=refused\nlisten=listened\nunix=refused\n",
            "{sockets:?}"
        );
    }
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        limited(LinuxSockets::TcpOnly),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(
        port_probe(&bubblewrap, &case, LISTENERS, &[]),
        "bind0=listened\nlisten=listened\nunix=refused\n"
    );
}

/// LPF1-M1: a kept port inside the range Linux picks from could be handed to the agent
/// with no check, so the launch is refused with a plain sentence, by the engine and by a
/// helper started without it.
#[test]
fn an_open_network_with_a_kept_port_linux_could_pick_is_refused_at_launch() {
    let case = case();
    let low = lowest_picked_port();
    let open = policy(&case, SandboxNetwork::Open)
        .keep_ports(&[low])
        .unwrap();
    match LinuxSandbox::detect(open, Path::new(HELPER)) {
        Err(SandboxError::Unavailable(reason)) => {
            assert!(reason.contains(&format!("port {low} ")), "{reason}");
            assert!(reason.contains("by itself"), "{reason}");
        }
        other => panic!("a kept port Linux could pick must refuse the launch: {other:?}"),
    }
    let direct = crazyagent::spawn_gate::output(
        Command::new(HELPER)
            .args(["--keep-free", &low.to_string(), "--", "/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert_eq!(direct.status.code(), Some(3));
    assert!(
        String::from_utf8_lossy(&direct.stderr).contains(&format!("port {low} ")),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
}

struct Checkouts {
    _temp: tempfile::TempDir,
    project: PathBuf,
    home: PathBuf,
    tmp: PathBuf,
    own: PathBuf,
}

fn git(cwd: &Path, arguments: &[&str]) {
    let output = crazyagent::spawn_gate::output(
        Command::new("git")
            .args([
                "-c",
                "user.name=Owner",
                "-c",
                "user.email=owner@crazyagent.invalid",
            ])
            .args(arguments)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkouts() -> Checkouts {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    let home = root.join("agent").join("home");
    let tmp = root.join("agent").join("tmp");
    for directory in [&project, &home, &tmp, &project.join("worktrees")] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(project.join("owner.txt"), "owner\n").unwrap();
    git(&project, &["init", "-q"]);
    git(&project, &["add", "owner.txt"]);
    git(&project, &["commit", "-q", "-m", "start"]);
    let own = project.join("worktrees").join("agent-1");
    git(
        &project,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            own.to_str().unwrap(),
            "HEAD",
        ],
    );
    Checkouts {
        _temp: temp,
        project,
        home,
        tmp,
        own,
    }
}

fn confined(checkouts: &Checkouts) -> ProjectSandbox {
    ProjectSandbox::new(
        &checkouts.project,
        &checkouts.home,
        &checkouts.tmp,
        SandboxNetwork::Open,
    )
    .unwrap()
    .confine_to_worktree(&checkouts.own)
    .unwrap()
}

/// Neither backend holds an agent confined to its own worktree: inside the worktree's git
/// admin folder and the object store it must write, Landlock cannot keep `commondir`,
/// `gitdir` or the existing objects read-only, and bubblewrap cannot bind every object.
#[test]
fn an_agent_confined_to_its_worktree_is_refused_on_both_backends() {
    let checkouts = checkouts();
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    for backend in [
        LinuxBackend::Landlock {
            helper: PathBuf::from(HELPER),
        },
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    ] {
        assert!(matches!(
            LinuxSandbox::new(confined(&checkouts), backend),
            Err(SandboxError::Unavailable(_))
        ));
    }
    assert!(matches!(
        LinuxSandbox::detect(confined(&checkouts), Path::new(HELPER)),
        Err(SandboxError::Unavailable(_))
    ));
}

/// An agent writing the project folder may add objects and refs as git does, but may not
/// plant or rewrite the metadata git follows to other repository data: alternates, grafts,
/// a shallow list, a common dir, a worktree config or another worktree's links.
#[test]
fn no_agent_plants_the_git_metadata_git_follows_on_either_backend() {
    let case = case();
    let git = case.project.join(".git");
    for folder in ["objects/info", "objects/ab", "info", "worktrees/agent-1"] {
        fs::create_dir_all(git.join(folder)).unwrap();
    }
    fs::write(git.join("worktrees/agent-1/commondir"), "../..\n").unwrap();
    let planted = [
        "objects/info/alternates",
        "info/grafts",
        "shallow",
        "commondir",
        "config.worktree",
        "worktrees/agent-1/commondir",
    ];
    let mut script = String::new();
    for target in planted {
        script.push_str(&format!(
            "(echo /elsewhere > {path}) 2>/dev/null && echo {target}=written || echo {target}=refused\n",
            path = quoted(&git.join(target)),
        ));
    }
    script.push_str(&format!(
        "(echo object > {path}) 2>/dev/null && echo object=written || echo object=refused\n",
        path = quoted(&git.join("objects/ab/0123")),
    ));
    let expected: String = planted
        .iter()
        .map(|target| format!("{target}=refused\n"))
        .chain(["object=written\n".to_owned()])
        .collect();
    let landlock = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    for sandbox in [&landlock as &dyn Sandbox, &bubblewrap] {
        let output = run(sandbox, &case, &script);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::remove_file(git.join("objects/ab/0123")).unwrap();
    }
    for target in &planted[..5] {
        assert!(!git.join(target).exists(), "{target}");
    }
    assert_eq!(
        fs::read_to_string(git.join("worktrees/agent-1/commondir")).unwrap(),
        "../..\n"
    );
}

const DATAGRAM: &str = "import os, socket, sys
port = int(sys.argv[1])
def attempt(name, family, kind, proto, address):
    try:
        s = socket.socket(family, kind, proto)
        if address is not None:
            s.sendto(b'x', address)
        s.close()
        print(name + '=ok')
    except PermissionError:
        print(name + '=refused')
    except OSError as error:
        print(name + '=' + type(error).__name__)
attempt('udp', socket.AF_INET, socket.SOCK_DGRAM, 0, ('127.0.0.1', port))
attempt('udp6', socket.AF_INET6, socket.SOCK_DGRAM, 0, None)
attempt('udp_nonblock', socket.AF_INET, socket.SOCK_DGRAM | socket.SOCK_NONBLOCK, 0, None)
attempt('ping', socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_ICMP, None)
attempt('sctp', socket.AF_INET, socket.SOCK_STREAM, 132, None)
attempt('vsock', 40, socket.SOCK_STREAM, 0, None)
attempt('netlink_route', socket.AF_NETLINK, socket.SOCK_RAW, 0, None)
attempt('netlink_generic', socket.AF_NETLINK, socket.SOCK_RAW, 16, None)
try:
    socket.create_connection(('127.0.0.1', port), 5).close()
    print('tcp=ok')
except OSError:
    print('tcp=refused')
try:
    socket.create_connection(('127.0.0.1', 53), 5).close()
    print('dns_tcp=reachable')
except ConnectionRefusedError:
    print('dns_tcp=reachable')
except OSError as error:
    print('dns_tcp=' + type(error).__name__)
print('resolver=' + os.environ.get('RES_OPTIONS', ''))
";

fn datagrams(sandbox: Option<&dyn Sandbox>, case: &Case, port: u16) -> Vec<String> {
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", DATAGRAM])
        .arg(port.to_string())
        .current_dir(case.project.join("worktrees"));
    agent_stdout(sandbox, harness)
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_tcp_only_agent_sends_no_datagram_and_may_ask_names_on_tcp_port_53() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let endpoint = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let open = datagrams(None, &case, port);
    assert!(
        open.contains(&"udp=ok".to_owned())
            && open.contains(&"tcp=ok".to_owned())
            && open.contains(&"netlink_generic=ok".to_owned()),
        "the unsandboxed control must send a datagram and open generic netlink: {open:?}"
    );
    let named = SandboxNetwork::Only(vec![
        SandboxRemote::Loopback(port),
        SandboxRemote::AnyHost(443),
    ]);
    let landlock = pinned(policy(&case, named));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock network rules (Linux 6.7 or newer): {:?}",
        landlock.backend()
    );
    assert_eq!(
        datagrams(Some(&landlock), &case, port),
        [
            "udp=refused",
            "udp6=refused",
            "udp_nonblock=refused",
            "ping=refused",
            "sctp=refused",
            "vsock=refused",
            "netlink_route=ok",
            "netlink_generic=refused",
            "tcp=ok",
            "dns_tcp=reachable",
            "resolver=use-vc",
        ]
    );
    let loopback = SandboxNetwork::Only(vec![SandboxRemote::Loopback(port)]);
    let landlock = pinned(policy(&case, loopback));
    assert_eq!(
        datagrams(Some(&landlock), &case, port),
        [
            "udp=refused",
            "udp6=refused",
            "udp_nonblock=refused",
            "ping=refused",
            "sctp=refused",
            "vsock=refused",
            "netlink_route=ok",
            "netlink_generic=refused",
            "tcp=ok",
            "dns_tcp=PermissionError",
            "resolver=",
        ],
        "an agent with no remote on another host gets no resolver port"
    );
    let unlimited = pinned(policy(&case, SandboxNetwork::Open));
    let reached = datagrams(Some(&unlimited), &case, port);
    assert!(
        reached.contains(&"udp=ok".to_owned()) && reached.contains(&"resolver=".to_owned()),
        "an open network keeps datagrams: {reached:?}"
    );
}

#[test]
fn a_tcp_ports_agent_keeps_its_port_limit_but_opens_datagrams_and_asks_names_its_own_way() {
    let case = case();
    let endpoint = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let named = SandboxNetwork::Only(vec![
        SandboxRemote::Loopback(port),
        SandboxRemote::AnyHost(443),
    ]);
    let landlock = pinned(policy(&case, named).limit_sockets(LinuxSockets::TcpPorts));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock network rules (Linux 6.7 or newer): {:?}",
        landlock.backend()
    );
    let mut harness = Command::new("/usr/bin/python3");
    harness.current_dir(case.project.join("worktrees"));
    let wrapped = landlock.wrap(&harness).unwrap();
    let HelperRequest::Run { policy, .. } =
        HelperRequest::parse(wrapped.get_args().map(OsString::from).collect()).unwrap()
    else {
        panic!("expected a run request");
    };
    assert!(policy.limit_network && policy.open_sockets);
    assert_eq!(policy.connect, [443, port]);
    assert!(!wrapped.get_envs().any(|(name, _)| name == "RES_OPTIONS"));
    let reached = datagrams(Some(&landlock), &case, port);
    for expected in [
        "udp=ok",
        "udp6=ok",
        "udp_nonblock=ok",
        "netlink_route=ok",
        "netlink_generic=ok",
        "tcp=ok",
        "dns_tcp=PermissionError",
        "resolver=",
    ] {
        assert!(
            reached.contains(&expected.to_owned()),
            "{expected}: {reached:?}"
        );
    }
}

#[test]
fn under_refuse_no_limited_network_agent_starts_on_either_backend() {
    let case = case();
    let named = SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]);
    for network in [named, SandboxNetwork::Only(Vec::new())] {
        let refused = policy(&case, network).limit_sockets(LinuxSockets::Refuse);
        for result in [
            LinuxSandbox::detect(refused.clone(), Path::new(HELPER)),
            LinuxSandbox::new(
                refused.clone(),
                LinuxBackend::Landlock {
                    helper: PathBuf::from(HELPER),
                },
            ),
        ] {
            match result {
                Err(SandboxError::Unavailable(reason)) => {
                    assert!(reason.contains("limited-network sockets"), "{reason}")
                }
                other => panic!("a limited network must be refused: {other:?}"),
            }
        }
    }
    let open = policy(&case, SandboxNetwork::Open).limit_sockets(LinuxSockets::Refuse);
    assert!(LinuxSandbox::detect(open, Path::new(HELPER)).is_ok());
}

/// TCP only takes a glibc program and a script whose interpreter is one (`/bin/sh` on a
/// glibc machine), and refuses a program with a musl loader and a script whose interpreter
/// is that program, each before it starts. A script is judged by its interpreter, one level.
#[test]
fn tcp_only_refuses_a_harness_whose_loader_is_not_glibc_before_it_starts() {
    let case = case();
    let original = fs::read("/bin/true").unwrap();
    let at = original
        .windows(8)
        .position(|window| window == b"ld-linux")
        .expect("Linux test machines run glibc: /bin/true names ld-linux as its loader");
    let mut musl = original.clone();
    musl[at..at + 8].copy_from_slice(b"ld-musl-");
    let programs = case.outside.join("programs");
    fs::create_dir(&programs).unwrap();
    for (name, bytes) in [("glibc", &original[..]), ("musl", &musl[..])] {
        fs::write(programs.join(name), bytes).unwrap();
    }
    fs::write(
        programs.join("script"),
        format!("#!{}\ntrue\n", programs.join("musl").display()),
    )
    .unwrap();
    fs::write(programs.join("shell"), "#!/bin/sh\ntrue\n").unwrap();
    let named = SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]);
    let wrap = |sockets: LinuxSockets, network: SandboxNetwork, name: &str| {
        let sandbox = LinuxSandbox::new(
            policy(&case, network).limit_sockets(sockets),
            LinuxBackend::Landlock {
                helper: PathBuf::from(HELPER),
            },
        )
        .unwrap();
        let mut harness = Command::new(programs.join(name));
        harness.current_dir(&case.project);
        sandbox.wrap(&harness)
    };
    for name in ["glibc", "shell"] {
        assert!(
            wrap(LinuxSockets::TcpOnly, named.clone(), name).is_ok(),
            "{name} looks names up over TCP, so TCP only takes it"
        );
    }
    for name in ["musl", "script"] {
        match wrap(LinuxSockets::TcpOnly, named.clone(), name) {
            Err(SandboxError::Unavailable(reason)) => {
                assert!(reason.contains("cannot look up names over TCP"), "{reason}");
                assert!(reason.contains("TCP ports only"), "{reason}");
            }
            other => panic!("{name} must be refused under TCP only: {other:?}"),
        }
        assert!(wrap(LinuxSockets::TcpPorts, named.clone(), name).is_ok());
        let local = SandboxNetwork::Only(vec![SandboxRemote::Loopback(8080)]);
        assert!(wrap(LinuxSockets::TcpOnly, local, name).is_ok());
    }
}

const ENVIRON: &str = "import sys
def read(path):
    try:
        with open(path, 'rb') as file:
            return 'marker' if b'CRAZYAGENT_TEST_MARKER=outside' in file.read() else 'read'
    except PermissionError:
        return 'refused'
    except FileNotFoundError:
        return 'absent'
print('outside=' + read('/proc/' + sys.argv[1] + '/environ'))
print('own=' + read('/proc/self/environ'))
";

fn environ(sandbox: Option<&dyn Sandbox>, case: &Case, target: u32) -> String {
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", ENVIRON])
        .arg(target.to_string())
        .current_dir(case.project.join("worktrees"));
    agent_stdout(sandbox, harness)
}

#[test]
fn no_agent_reads_the_environment_of_a_process_outside_its_boundary() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let mut outside = crazyagent::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("60")
            .env("CRAZYAGENT_TEST_MARKER", "outside")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let target = outside.id();
    assert_eq!(
        environ(None, &case, target),
        "outside=marker\nown=read\n",
        "the unsandboxed control must read the outside environment"
    );
    let landlock = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(landlock.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock file rules (Linux 6.2 or newer): {:?}",
        landlock.backend()
    );
    assert_eq!(
        environ(Some(&landlock), &case, target),
        "outside=refused\nown=read\n",
        "Landlock refuses ptrace-mode reads of a process outside the agent's domain"
    );
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let bubblewrap = LinuxSandbox::new(
        policy(&case, SandboxNetwork::Only(Vec::new())),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
            helper: PathBuf::from(HELPER),
        },
    )
    .unwrap();
    assert_eq!(
        environ(Some(&bubblewrap), &case, target),
        "outside=absent\nown=read\n"
    );
    outside.kill().unwrap();
    outside.wait().unwrap();
}

/// The bubblewrap this machine nests an agent in, by a control that owes the engine
/// nothing: bubblewrap alone making the namespaces of [`NEST_ARGS`]. Output: none, with the
/// reason printed, on a machine that refuses them (Ubuntu 24.04 without a profile, most
/// containers), where every nested row is skipped.
fn nesting_bwrap() -> Option<PathBuf> {
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let made = crazyagent::spawn_gate::status(
        Command::new(bwrap)
            .args(NEST_ARGS)
            .args(["--unshare-net", "--", "/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .is_ok_and(|status| status.success());
    if !made {
        eprintln!("skipped: this machine makes no user and process namespaces for bubblewrap");
    }
    made.then(|| bwrap.to_path_buf())
}

/// The nested backend for a policy, on a machine whose control says it can nest, where
/// detection must pick it. Input: the policy. Output: the sandbox, or none where the
/// machine cannot nest and the row is skipped.
fn nested(sandbox: ProjectSandbox) -> Option<LinuxSandbox> {
    nesting_bwrap()?;
    let sandbox = LinuxSandbox::detect(sandbox, Path::new(HELPER)).unwrap();
    assert!(
        matches!(sandbox.backend(), LinuxBackend::Nested { .. }),
        "this machine makes the namespaces, so detection nests the agent: {:?}",
        sandbox.backend()
    );
    Some(sandbox)
}

/// Starts a wrapped `/bin/sh -c script` with its input and output piped and waits for its
/// first line. Inputs: the sandbox, the case and the script, which prints `ready` first.
/// Output: the running first process and the reader of the rest of its output.
fn started(sandbox: &dyn Sandbox, case: &Case, script: &str) -> (Child, BufReader<ChildStdout>) {
    let mut harness = Command::new("/bin/sh");
    harness
        .args(["-c", script])
        .current_dir(case.project.join("worktrees"));
    let mut child = crazyagent::spawn_gate::spawn(
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
    assert_eq!(line, "ready\n");
    (child, output)
}

/// Sends a signal with `/bin/kill`. Inputs: the signal's name and the target as kill takes
/// it, a process id, or a process group as its id after a minus sign. Output: none.
fn send(signal: &str, target: &str) {
    let status = crazyagent::spawn_gate::status(
        Command::new("/bin/kill")
            .arg(format!("-{signal}"))
            .args(["--", target])
            .stdin(Stdio::null()),
    )
    .unwrap();
    assert!(status.success(), "kill -{signal} {target}");
}

/// Waits for a child to exit. Inputs: the child and the longest wait. Output: its exit
/// status, or none when it still runs at the deadline.
fn exit_within(child: &mut Child, wait: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + wait;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return Some(status),
            None if Instant::now() >= deadline => return None,
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Tells whether a process whose command line holds `token` runs on this machine, in any
/// process namespace. Input: the token. Output: true while `/proc` lists one.
fn running(token: &str) -> bool {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            fs::read(entry.path().join("cmdline"))
                .is_ok_and(|line| String::from_utf8_lossy(&line).contains(token))
        })
}

/// Waits up to five seconds for every process whose command line holds `token` to end.
/// Input: the token. Output: whether none is left.
fn ended(token: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while running(token) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    !running(token)
}

/// The fields of `/proc/<pid>/stat` after the name, as this test process sees them. Input:
/// the process id. Output: state, parent, process group, session and controlling terminal.
fn host_stat(pid: u32) -> Vec<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields = stat.rsplit_once(") ").unwrap().1;
    fields.split(' ').take(5).map(str::to_owned).collect()
}

/// DEC-179: where this machine makes user and process namespaces, detection nests the agent
/// in bubblewrap under the same Landlock policy, on an open network and on a limited one,
/// and the nested agent is held to the project boundary as a plain Landlock agent is.
#[test]
fn a_nested_agent_holds_the_project_boundary() {
    for network in [
        SandboxNetwork::Open,
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
    ] {
        let case = case();
        let Some(sandbox) = nested(policy(&case, network)) else {
            return;
        };
        assert_boundary(&case, &run(&sandbox, &case, &boundary_script(&case)));
    }
}

/// With no bubblewrap to look for, detection keeps the plain Landlock backend, as on a
/// machine that has none installed or refuses its namespaces.
#[test]
fn without_a_bubblewrap_to_look_for_detection_keeps_the_plain_landlock_backend() {
    let case = case();
    let sandbox = pinned(policy(&case, SandboxNetwork::Open));
    assert!(
        matches!(sandbox.backend(), LinuxBackend::Landlock { .. }),
        "{:?}",
        sandbox.backend()
    );
}

const UNSEEN: &str = "kill -0 \"$1\" 2>/dev/null && echo delivered || echo refused; \
     test -e \"/proc/$1\" && echo listed || echo unlisted; echo $$";

/// DEC-089, RV-DS241-86: a nested agent runs in a process namespace of its own, so on every
/// kernel, also one before Linux 6.12 with no Landlock signal scope, it neither signals a
/// process outside nor finds it in `/proc`, and it is among the first processes there.
#[test]
fn a_nested_agent_neither_signals_nor_lists_a_process_outside_its_own() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let mut outside = crazyagent::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let target = outside.id().to_string();
    let seen = |boundary: Option<&dyn Sandbox>| {
        let mut harness = Command::new("/bin/sh");
        harness
            .args(["-c", UNSEEN, "sh", target.as_str()])
            .current_dir(case.project.join("worktrees"));
        agent_stdout(boundary, harness)
    };
    let control = seen(None);
    assert!(
        control.starts_with("delivered\nlisted\n"),
        "the unsandboxed control must reach and list the outside process: {control}"
    );
    let boundary: &dyn Sandbox = &sandbox;
    let inside = seen(Some(boundary));
    let lines: Vec<&str> = inside.lines().collect();
    assert_eq!(lines.len(), 3, "{inside}");
    assert_eq!(lines[..2], ["refused", "unlisted"], "{inside}");
    let own: u32 = lines[2].parse().unwrap();
    assert!(
        own < 16,
        "the harness is not among the first processes: {own}"
    );
    outside.kill().unwrap();
    outside.wait().unwrap();
}

/// A nested agent started with pipes shares no session or terminal with the engine: the
/// first process, bubblewrap's outer one, leads a group and a session of its own with no
/// controlling terminal, and the harness, inside the namespace, reads both as 0.
#[test]
fn a_nested_agent_shares_no_session_or_terminal_with_the_engine() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let script = format!("echo ready; {SESSION}; read -r line");
    let (mut child, mut output) = started(&sandbox, &case, &script);
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let inside: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(inside.len(), 4, "{line}");
    assert_eq!(
        inside[1..],
        ["0", "0", "0"],
        "group and session are led from outside the namespace, with no terminal: {line}"
    );
    let first = child.id().to_string();
    let outside = host_stat(child.id());
    assert_eq!(
        outside[2..],
        [first.as_str(), first.as_str(), "0"],
        "the first process leads its group and session, with no terminal: {outside:?}"
    );
    drop(child.stdin.take());
    let status = exit_within(&mut child, Duration::from_secs(5));
    assert_eq!(status.and_then(|status| status.code()), Some(0));
}

const PANE: &str = "import os, pty, sys
pid, primary = pty.fork()
if pid == 0:
    os.execv(sys.argv[1], sys.argv[1:])
shown = b''
while True:
    try:
        chunk = os.read(primary, 1024)
    except OSError:
        break
    if not chunk:
        break
    shown += chunk
os.waitpid(pid, 0)
sys.stdout.write(shown.decode(errors='replace'))
";

/// A nested agent started as the first process of a terminal pane keeps that pane as its
/// controlling terminal, as a plain Landlock agent does: the session is taken outside the
/// namespace, where the helper can see that it already leads one.
#[test]
fn a_nested_pane_agent_keeps_its_pane_as_its_terminal() {
    let case = case();
    assert!(
        Path::new("/usr/bin/python3").is_file(),
        "Linux test machines need /usr/bin/python3"
    );
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let mut harness = Command::new("/usr/bin/python3");
    harness
        .args(["-I", "-c", OPEN_TERMINAL, "/dev/tty"])
        .current_dir(case.project.join("worktrees"));
    let wrapped = sandbox.wrap(&harness).unwrap();
    let mut pane = Command::new("/usr/bin/python3");
    pane.args(["-I", "-c", PANE])
        .arg(wrapped.get_program())
        .args(wrapped.get_args())
        .env_clear()
        .envs(
            wrapped
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .current_dir(wrapped.get_current_dir().unwrap())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = crazyagent::spawn_gate::output(&mut pane).unwrap();
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(
        shown.contains("opened"),
        "the pane is not the agent's terminal: {shown} {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const HANDLES_TERM: &str =
    "trap 'echo stopped; exit 7' TERM; echo ready; while :; do sleep 0.1; done";

/// The engine stops an agent by a TERM to its group, a grace wait on the first process, then
/// a KILL to the group. The first nested process is bubblewrap's outer one, which the TERM
/// must not end: the harness handles it inside its grace and its own exit code comes back.
#[test]
fn a_nested_harness_that_handles_the_stop_signal_gets_its_grace_and_its_own_exit_code() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let (mut child, mut output) = started(&sandbox, &case, HANDLES_TERM);
    send("TERM", &format!("-{}", child.id()));
    let mut rest = String::new();
    output.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "stopped\n", "the harness handled the stop signal");
    let status = exit_within(&mut child, Duration::from_secs(5));
    assert_eq!(
        status.and_then(|status| status.code()),
        Some(7),
        "the first process ends with the harness's own exit code: {status:?}"
    );
}

/// A harness that ignores TERM runs on through the grace with its first process, and the
/// group KILL then ends everything the agent started, also a process that left its group
/// and session, since the KILL reaches the first process of its process namespace.
#[test]
fn a_nested_harness_that_ignores_the_stop_signal_ends_at_the_group_kill_with_all_it_started() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let token = format!("601.{}", std::process::id());
    let script =
        format!("trap '' TERM; setsid sleep {token} & echo ready; while :; do sleep 1; done");
    let (mut child, _output) = started(&sandbox, &case, &script);
    let group = format!("-{}", child.id());
    send("TERM", &group);
    assert_eq!(
        exit_within(&mut child, Duration::from_millis(300)),
        None,
        "the stop signal ended the first process of a harness that ignores it"
    );
    assert!(running(&token), "the control: the agent's processes run");
    send("KILL", &group);
    assert!(
        exit_within(&mut child, Duration::from_secs(5)).is_some(),
        "the group kill did not end the first process"
    );
    assert!(
        ended(&token),
        "a process that left the agent's group and session outlived the group kill"
    );
}

/// A KILL of the first process alone, with no signal to the group, ends the whole nested
/// agent: its namespace is tied to that process, so the engine never reads an agent as
/// ended while a process of it runs on.
#[test]
fn a_kill_of_the_first_process_alone_ends_the_whole_nested_agent() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let token = format!("602.{}", std::process::id());
    let script = format!("setsid sleep {token} & echo ready; while :; do sleep 1; done");
    let (mut child, _output) = started(&sandbox, &case, &script);
    assert!(running(&token), "the control: the agent's processes run");
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        ended(&token),
        "a process of the agent outlived its first process"
    );
}

/// The first nested process ends only with the harness or by KILL: a signal sent to it
/// alone, as a terminal or another program of the owner's may send, leaves the agent running.
#[test]
fn the_first_nested_process_outlives_every_signal_but_kill() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let (mut child, mut output) = started(&sandbox, &case, "echo ready; read -r line; echo done");
    let first = child.id().to_string();
    for signal in ["TERM", "HUP", "INT", "QUIT", "USR1", "USR2"] {
        send(signal, &first);
    }
    assert_eq!(
        exit_within(&mut child, Duration::from_millis(300)),
        None,
        "a signal other than KILL ended the first process"
    );
    drop(child.stdin.take());
    let mut rest = String::new();
    output.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "done\n");
    let status = exit_within(&mut child, Duration::from_secs(5));
    assert_eq!(status.and_then(|status| status.code()), Some(0));
}

/// SA vid 4: the lines both clients show about this Linux follow what it holds. The
/// helper's scope question agrees with the kernel, and the host report agrees with it and
/// with the control that says whether this machine nests an agent.
#[test]
fn the_host_report_agrees_with_the_kernel_and_the_nesting_control() {
    let scoped = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .scope(Scope::Signal)
        .is_ok();
    let asked = crazyagent::spawn_gate::status(
        Command::new(HELPER)
            .arg("--scoped")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    assert_eq!(asked.code(), Some(if scoped { 0 } else { 1 }));
    let host = crazyagent::sandbox::linux::read_host(Path::new(HELPER));
    assert_eq!(host.scoped, scoped);
    assert_eq!(host.nested, nesting_bwrap().is_some());
    let absent = crazyagent::sandbox::linux::read_host(Path::new("/nonexistent/helper"));
    assert!(!absent.scoped && !absent.nested, "{absent:?}");
}

#[test]
fn a_host_whose_landlock_does_not_hold_reports_the_process_namespace_bubblewrap_gives() {
    let refusing = Path::new("/bin/false");
    assert!(refusing.is_file(), "the row needs /bin/false as a helper that holds nothing");
    let bwrap = BWRAP_PATHS.iter().any(|path| Path::new(path).is_file());
    assert!(bwrap, "Linux test machines need bubblewrap installed");
    let host = crazyagent::sandbox::linux::read_host(refusing);
    assert!(!host.scoped, "{host:?}");
    assert!(host.nested, "{host:?}");
}

/// This computer's address on its default route, which is never loopback. No input.
/// Output: the address; it panics on a machine with no route, where the rows that ask for
/// another host prove nothing.
fn outward_address() -> std::net::IpAddr {
    let socket = crazyagent::spawn_gate::bind_udp_std("0.0.0.0:0").unwrap();
    socket
        .connect("192.0.2.1:9")
        .expect("Linux test machines need a default route");
    let address = socket.local_addr().unwrap().ip();
    assert!(!address.is_loopback() && !address.is_unspecified(), "{address}");
    address
}

#[test]
fn a_loopback_only_nested_agent_reaches_its_port_on_this_computer_and_on_no_other_host() {
    let case = case();
    let served = crazyagent::spawn_gate::bind_tcp_std("0.0.0.0:0").unwrap();
    let port = served.local_addr().unwrap().port();
    let other = outward_address();
    std::thread::spawn(move || {
        for mut asked in served.incoming().flatten().take(16) {
            let _ = asked.write_all(b"pong\n");
        }
    });
    let script = format!(
        "read -r line </dev/tcp/127.0.0.1/{port}; echo \"loopback=$line\"; \
         (exec 4<>/dev/tcp/{other}/{port}) 2>/dev/null && echo other=reached || echo other=refused"
    );
    let reach = |sandbox: &dyn Sandbox| {
        let mut harness = Command::new("/bin/bash");
        harness
            .args(["-c", script.as_str()])
            .current_dir(case.project.join("worktrees"));
        agent_stdout(Some(sandbox), harness)
    };
    let network = SandboxNetwork::Only(vec![SandboxRemote::Loopback(port)]);
    let shared = pinned(policy(&case, network.clone()));
    assert_eq!(
        reach(&shared),
        "loopback=pong\nother=reached\n",
        "on the shared network the port is open on every host (RV-DS241-83)"
    );
    let Some(bridged) = nested(policy(&case, network)) else {
        return;
    };
    assert_eq!(reach(&bridged), "loopback=pong\nother=refused\n");
}

#[test]
fn the_bridge_leaves_no_descriptor_in_the_harness() {
    let case = case();
    let network = SandboxNetwork::Only(vec![SandboxRemote::Loopback(18_080)]);
    let Some(bridged) = nested(policy(&case, network.clone())) else {
        return;
    };
    let shared = pinned(policy(&case, network));
    let open = |sandbox: &dyn Sandbox| {
        String::from_utf8(run(sandbox, &case, "ls /proc/self/fd").stdout).unwrap()
    };
    assert_eq!(open(&bridged), "0\n1\n2\n3\n");
    assert_eq!(open(&bridged), open(&shared));
}

/// Tells whether a broker that carries a port runs on this machine, by its arguments.
/// Input: the port. Output: true when one does.
fn broker_runs(port: u16) -> bool {
    let wanted = port.to_string();
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            fs::read(entry.path().join("cmdline")).is_ok_and(|line| {
                let words: Vec<&[u8]> = line.split(|byte| *byte == 0).collect();
                words.contains(&b"--broker".as_slice()) && words.contains(&wanted.as_bytes())
            })
        })
}

#[test]
fn the_broker_runs_while_the_agent_does_and_ends_with_it() {
    let case = case();
    let held = crazyagent::spawn_gate::bind_tcp_std("127.0.0.1:0").unwrap();
    let port = held.local_addr().unwrap().port();
    let network = SandboxNetwork::Only(vec![SandboxRemote::Loopback(port)]);
    let Some(bridged) = nested(policy(&case, network)) else {
        return;
    };
    assert!(!broker_runs(port));
    let (mut child, _output) = started(&bridged, &case, "echo ready; read -r line");
    assert!(broker_runs(port), "a bridged agent runs with its broker");
    drop(child.stdin.take());
    assert!(exit_within(&mut child, Duration::from_secs(5)).is_some());
    let deadline = Instant::now() + Duration::from_secs(5);
    while broker_runs(port) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!broker_runs(port), "the broker ends with the agent");
}

/// A guard: bubblewrap's outer process, the first process of a nested agent, runs with no
/// new privileges, so a set-user-ID bubblewrap gains nothing from the engine (rv-sec3 F1).
/// Bubblewrap sets the flag itself once it runs, so only a set-user-ID one tells the two
/// apart.
#[test]
fn bubblewrap_starts_for_a_nested_agent_with_no_new_privileges() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let (mut child, _output) = started(&sandbox, &case, "echo ready; read -r line");
    let status = fs::read_to_string(format!("/proc/{}/status", child.id())).unwrap();
    drop(child.stdin.take());
    assert!(exit_within(&mut child, Duration::from_secs(5)).is_some());
    let held = status
        .lines()
        .find_map(|line| line.strip_prefix("NoNewPrivs:"))
        .map(str::trim);
    assert_eq!(held, Some("1"), "{status}");
}

/// A gate row for a bubblewrap fact taken from memory: `--die-with-parent` must tie the
/// namespace to bubblewrap's outer process only. Were that outer process tied to the thread
/// that started it, an agent would end when an engine thread does.
#[test]
fn a_nested_agent_outlives_the_thread_that_started_it() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let script = "echo ready; read -r line; echo \"got $line\"";
    let (mut child, mut output) = std::thread::scope(|scope| {
        scope
            .spawn(|| started(&sandbox, &case, script))
            .join()
            .unwrap()
    });
    assert!(
        exit_within(&mut child, Duration::from_millis(500)).is_none(),
        "the agent ended with the thread that started it"
    );
    child.stdin.take().unwrap().write_all(b"on\n").unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "got on\n");
    let status = exit_within(&mut child, Duration::from_secs(5));
    assert_eq!(status.and_then(|status| status.code()), Some(0));
}

/// A gate row for a bubblewrap fact taken from memory: its outer process exits with the
/// harness, at once and with its code, and the namespace then ends with everything the
/// harness left behind, a process in a session of its own too.
#[test]
fn a_nested_agent_ends_with_its_harness_and_leaves_nothing_it_started() {
    let case = case();
    let Some(sandbox) = nested(policy(&case, SandboxNetwork::Open)) else {
        return;
    };
    let token = format!("603.{}", std::process::id());
    let mut harness = Command::new("/bin/sh");
    harness
        .arg("-c")
        .arg(format!("setsid sleep {token} & exit 5"))
        .current_dir(case.project.join("worktrees"));
    let mut child = crazyagent::spawn_gate::spawn(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let status = exit_within(&mut child, Duration::from_secs(5));
    assert_eq!(
        status.and_then(|status| status.code()),
        Some(5),
        "the first process waits for more than the harness"
    );
    assert!(ended(&token), "a process the harness left outlived it");
}

/// A launch behind the helper with a hand-written policy and the call supervisor's flags,
/// as the engine writes them for a host with no namespaces to nest an agent in: it reads
/// every file and writes the devices and the case's folders. Inputs: the case, the policy
/// and guard flags, the program and its arguments. Output: the launch, not yet started.
fn supervised(case: &Case, flags: &[&str], program: &str, args: &[&str]) -> Command {
    let root = case.project.parent().unwrap();
    let mut command = Command::new(HELPER);
    command
        .args(["--read", "/", "--write", "/dev", "--write"])
        .arg(root)
        .args(flags)
        .arg("--")
        .arg(program)
        .args(args)
        .current_dir(&case.project)
        .stdin(Stdio::null());
    command
}

/// Runs a launch to its end. Input: the launch. Output: what it wrote and how it ended.
fn finished(mut command: Command) -> Output {
    crazyagent::spawn_gate::output(command.stdout(Stdio::piped()).stderr(Stdio::piped())).unwrap()
}

/// What a finished launch wrote to its output, after it ended well. Input: the output.
fn told(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Rows about the call supervisor need a kernel that runs it: seccomp user notification
/// whose wait only a fatal signal cuts (Linux 5.19) and a ptrace scope that lets a parent
/// read the programs it started. The check itself fails where nothing supervises it.
#[test]
fn the_call_supervisor_probe_holds_on_this_machine_and_fails_with_nothing_supervising() {
    let quiet = |command: &mut Command| {
        let silenced = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crazyagent::spawn_gate::status(silenced).unwrap()
    };
    let probe = quiet(Command::new(HELPER).arg("--probe-supervisor"));
    assert!(
        probe.success(),
        "Linux test machines need Linux 5.19 or newer and a ptrace scope below 2: {probe:?}"
    );
    let case = case();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let mut check = Command::new(HELPER);
    check.arg("--probe-supervised").arg(&port).arg(&case.tmp);
    assert_eq!(
        quiet(&mut check).code(),
        Some(1),
        "the check passes with no supervisor, so it checks nothing"
    );
}

const GUARDED_REACH: &str = "import ctypes, socket, sys
port = int(sys.argv[1])
def said(call):
    try:
        call()
        return 'ok'
    except OSError as error:
        return str(error.errno)
def reach(family, host, wait):
    def call():
        link = socket.socket(family, socket.SOCK_STREAM)
        link.settimeout(wait)
        link.connect((host, port))
    return said(call)
print('here=' + reach(socket.AF_INET, '127.0.0.1', 2))
print('there=' + reach(socket.AF_INET, '192.0.2.1', 2))
print('mapped-here=' + reach(socket.AF_INET6, '::ffff:127.0.0.1', 2))
print('mapped-there=' + reach(socket.AF_INET6, '::ffff:192.0.2.1', 2))
print('blocking-here=' + reach(socket.AF_INET, '127.0.0.1', None))
fast = socket.socket()
print('fast-open=' + said(lambda: fast.sendto(b'x', 0x20000000, ('192.0.2.1', port))))
libc = ctypes.CDLL(None, use_errno=True)
wide = socket.socket()
refused = libc.connect(wide.fileno(), ctypes.create_string_buffer(200), 200)
print('wide=' + (str(ctypes.get_errno()) if refused else 'ok'))
";

/// Landlock limits an agent's ports, not its hosts. The call supervisor connects for the
/// agent after it read the address itself, so a guarded port is reached on this machine
/// only, in every address form, a send that would open a connection is refused whole, and
/// an address longer than any the kernel takes is refused as the kernel refuses it.
#[test]
fn a_supervised_agent_reaches_a_guarded_port_on_this_machine_and_on_no_other_host() {
    let case = case();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let limited = ["--limit-network", "--connect", &port];
    let guarded = [&limited[..], &["--guard-loopback", &port][..]].concat();
    let script = ["-I", "-u", "-c", GUARDED_REACH, &port];
    let held = finished(supervised(&case, &guarded, "/usr/bin/python3", &script));
    assert_eq!(
        told(&held),
        "here=ok\nthere=13\nmapped-here=ok\nmapped-there=13\nblocking-here=ok\n\
         fast-open=95\nwide=22\n"
    );
    let open = finished(supervised(&case, &limited, "/usr/bin/python3", &script));
    let open = told(&open);
    assert!(open.contains("here=ok\n"), "{open}");
    assert!(!open.contains("there=13\n"), "the port limit alone names no host: {open}");
}

const GIT_ENTRIES: &str = r#"said() {
    name=$1
    shift
    if "$@" 2>/dev/null; then echo "$name=made"; else echo "$name=refused"; fi
}
mkdir sub sub/plain
: > sub/kept
ln -s .git sub/dangling
said folder mkdir sub/.git
said file sh -c ': > sub/.git'
said link ln -s elsewhere sub/.git
said hard-link ln sub/kept sub/.git
said rename mv sub/plain sub/.git
said upper-case mkdir sub/.GIT
said dotted mkdir 'sub/.git.'
said through-link sh -c ': > sub/dangling'
said below mkdir -p sub/.git/objects
said init git init -q sub/repo
said own-file sh -c ': > .git/objects/made'
said own-rename mv .git/objects/made .git/objects/moved
said own-folder mkdir .git/refs
said own-nested mkdir .git/refs/.git
said plain-file sh -c ': > sub/file'
said plain-rename mv sub/file sub/plain/file
said own-away mv "$PWD" "$PWD.away"
"#;

/// With no namespace to hide a git folder behind, the call supervisor makes each new
/// entry for the agent after it resolved where the entry lands: a `.git` in any spelling
/// and by any call is refused, also through a link, while the working tree's own `.git`
/// and every other name work, and the folder that holds the own `.git` stays where it is.
#[test]
fn a_supervised_agent_makes_no_git_entry_but_inside_its_own_git_folder() {
    let case = case();
    let project = case.project.to_str().unwrap();
    let guard = ["--guard-git", "--own-tree", project];
    let held = finished(supervised(&case, &guard, "/bin/sh", &["-c", GIT_ENTRIES]));
    assert_eq!(
        told(&held),
        "folder=refused\nfile=refused\nlink=refused\nhard-link=refused\nrename=refused\n\
         upper-case=refused\ndotted=refused\nthrough-link=refused\nbelow=refused\n\
         init=refused\nown-file=made\nown-rename=made\nown-folder=made\nown-nested=refused\n\
         plain-file=made\nplain-rename=made\nown-away=refused\n"
    );
    let sub = case.project.join("sub");
    for absent in [".git", ".GIT", ".git.", "repo/.git"] {
        assert!(fs::symlink_metadata(sub.join(absent)).is_err(), "{absent}");
    }
    assert!(case.project.join(".git/objects/moved").is_file());
    assert!(sub.join("plain/file").is_file());
    let plain = finished(supervised(&case, &[], "/bin/sh", &["-c", "mkdir sub/.git"]));
    assert!(plain.status.success(), "the control makes the folder with no guard");
}

const OPENS: &str = "import ctypes, os
libc = ctypes.CDLL(None, use_errno=True)
refused = libc.syscall(437, -100, b'never', ctypes.create_string_buffer(24), 24)
print('openat2=' + str(ctypes.get_errno() if refused < 0 else 0))
os.umask(0o027)
os.close(os.open('made', os.O_CREAT | os.O_WRONLY, 0o666))
os.mkdir('folder', 0o777)
modes = [os.stat(name).st_mode & 0o777 for name in ('made', 'folder')]
print('modes=%o %o' % tuple(modes))
with open('/proc/self/fd/1', 'w') as out:
    out.write('proc=written\\n')
with open('/dev/stderr', 'w') as err:
    err.write('piped\\n')
";

/// A call made for the agent behaves as the agent's own: `openat2`, whose flags lie in
/// memory, is answered as missing so tools fall back to `openat`; a new file and folder
/// take the agent's file mode mask; `/proc/self` names the agent, not the supervisor, so
/// its own output and a piped `/dev/stderr` open as before.
#[test]
fn a_call_made_for_a_supervised_agent_keeps_its_mask_its_proc_names_and_its_pipes() {
    let case = case();
    let project = case.project.to_str().unwrap();
    let guard = ["--guard-git", "--own-tree", project];
    let script = ["-I", "-u", "-c", OPENS];
    let held = finished(supervised(&case, &guard, "/usr/bin/python3", &script));
    assert_eq!(told(&held), "openat2=38\nmodes=640 750\nproc=written\n");
    assert_eq!(String::from_utf8_lossy(&held.stderr), "piped\n");
    assert!(!case.project.join("never").exists());
}

const GUARDED_SIGNALS: &str = "import fcntl, os, signal, socket, subprocess, sys
outside = int(sys.argv[1])
def said(call):
    try:
        call()
        return 'ok'
    except OSError as error:
        return str(error.errno)
child = subprocess.Popen(['/bin/sleep', '30'])
link = socket.socket()
print('outside=' + said(lambda: os.kill(outside, 0)))
print('everyone=' + said(lambda: os.kill(-1, 0)))
print('own-group=' + said(lambda: os.kill(0, 0)))
print('self=' + said(lambda: os.kill(os.getpid(), 0)))
print('child=' + said(lambda: os.kill(child.pid, signal.SIGTERM)))
print('owner-outside=' + said(lambda: fcntl.fcntl(link, fcntl.F_SETOWN, outside)))
print('owner-self=' + said(lambda: fcntl.fcntl(link, fcntl.F_SETOWN, os.getpid())))
print('ended=' + str(child.wait()))
";

/// Before Linux 6.12 Landlock scopes no signal. The call supervisor lets a signal through
/// only to the programs the agent started, its own group among them, on every kernel, and
/// a descriptor's signals may be owned by those programs alone.
#[test]
fn a_supervised_agent_signals_only_the_programs_it_started() {
    let case = case();
    let mut outside = crazyagent::spawn_gate::spawn(
        Command::new("/bin/sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let target = outside.id().to_string();
    let script = ["-I", "-u", "-c", GUARDED_SIGNALS, &target];
    let guard = ["--guard-signals"];
    let held = finished(supervised(&case, &guard, "/usr/bin/python3", &script));
    assert_eq!(
        told(&held),
        "outside=1\neveryone=1\nown-group=ok\nself=ok\nchild=ok\nowner-outside=1\n\
         owner-self=ok\nended=-15\n"
    );
    assert_eq!(outside.try_wait().unwrap(), None, "the outside process still runs");
    outside.kill().unwrap();
    outside.wait().unwrap();
}

/// The helper stays as the harness's parent under the call supervisor, so the engine sees
/// the harness through it: its exit code, 128 plus the signal that ended it, and a stop
/// sent to the launch's group, which the harness handles while the helper waits for it.
#[test]
fn a_supervised_agent_ends_with_its_harness_code_and_stops_with_its_group() {
    let case = case();
    let guard = ["--guard-signals"];
    let code = |script: &str| {
        let done = finished(supervised(&case, &guard, "/bin/sh", &["-c", script]));
        done.status.code()
    };
    assert_eq!(code("exit 7"), Some(7));
    assert_eq!(code("kill -KILL $$"), Some(137));
    let token = format!("611.{}", std::process::id());
    let script = format!("trap 'echo stopped; exit 9' TERM; echo ready; sleep {token} & wait");
    let mut child = crazyagent::spawn_gate::spawn(
        supervised(&case, &guard, "/bin/sh", &["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )
    .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    send("TERM", &format!("-{}", child.id()));
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "stopped\n");
    let status = exit_within(&mut child, Duration::from_secs(5));
    assert_eq!(status.and_then(|status| status.code()), Some(9));
    assert!(ended(&token), "a process the harness started outlived the stop");
}

const STORM: &str = "import os, signal
signal.signal(signal.SIGALRM, lambda number, frame: None)
signal.setitimer(signal.ITIMER_REAL, 0.001, 0.001)
for number in range(300):
    os.close(os.open('storm-%d' % number, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
    os.mkdir('storm-folder-%d' % number)
signal.setitimer(signal.ITIMER_REAL, 0)
print('made=' + str(len([name for name in os.listdir('.') if name.startswith('storm-')])))
";

/// A signal that lands while the supervisor makes an entry must not run the call twice:
/// the second run would find the entry the first one made. The trapped call waits for its
/// answer through every signal but a fatal one.
#[test]
fn a_signal_storm_runs_no_supervised_call_twice() {
    let case = case();
    let project = case.project.to_str().unwrap();
    let guard = ["--guard-git", "--own-tree", project];
    let script = ["-I", "-u", "-c", STORM];
    let held = finished(supervised(&case, &guard, "/usr/bin/python3", &script));
    assert_eq!(told(&held), "made=600\n");
}
