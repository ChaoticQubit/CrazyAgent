#![cfg(target_os = "linux")]
use crazyagent::sandbox::linux::{BWRAP_PATHS, HelperRequest, LinuxBackend, LinuxSandbox};

use crazyagent::sandbox::{
    InstalledSandbox, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, SandboxRemote,
};

use landlock::{CompatLevel, Compatible, Ruleset, RulesetAttr, Scope};

use std::ffi::{OsStr, OsString};

use std::fs;

use std::io::{BufRead, BufReader};

use std::path::{Path, PathBuf};

use std::process::{Command, Output, Stdio};

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

#[test]
fn landlock_holds_the_project_boundary_on_this_kernel() {
    let case = case();
    let sandbox =
        LinuxSandbox::detect(policy(&case, SandboxNetwork::Open), Path::new(HELPER)).unwrap();
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
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
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
    let landlock =
        LinuxSandbox::detect(policy(&case, SandboxNetwork::Open), Path::new(HELPER)).unwrap();
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
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
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

fn assert_own_session(command: &mut Command) {
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
    let fields: Vec<&str> = text.split_whitespace().collect();
    assert_eq!(fields.len(), 4, "{text}");
    assert_eq!(
        fields[1], fields[0],
        "the agent leads its process group: {text}"
    );
    assert_eq!(
        fields[2], fields[0],
        "the agent leads its own session: {text}"
    );
    assert_eq!(
        fields[3], "0",
        "the agent has no controlling terminal: {text}"
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
    let landlock =
        LinuxSandbox::detect(policy(&case, SandboxNetwork::Open), Path::new(HELPER)).unwrap();
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
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
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
    let landlock =
        LinuxSandbox::detect(policy(&case, SandboxNetwork::Open), Path::new(HELPER)).unwrap();
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
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
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

#[test]
fn every_agent_leads_a_session_of_its_own_without_the_engine_terminal() {
    let case = case();
    let mut harness = Command::new("/bin/sh");
    harness
        .args(["-c", SESSION])
        .current_dir(case.project.join("worktrees"));
    let landlock =
        LinuxSandbox::detect(policy(&case, SandboxNetwork::Open), Path::new(HELPER)).unwrap();
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
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
        },
    )
    .unwrap();
    assert_own_session(&mut bubblewrap.wrap(&harness).unwrap());
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
    let leased = || {
        policy(&case, SandboxNetwork::Open)
            .allow_worktree(&worktree)
            .unwrap()
    };
    let script = format!(
        "mkdir {folder} && echo made > {file} && \
         (echo x > {top} || true) && (mkdir {top_folder} || true)",
        folder = quoted(&worktree.join("src2")),
        file = quoted(&worktree.join("Makefile")),
        top = quoted(&case.project.join("new.txt")),
        top_folder = quoted(&case.project.join("src3")),
    );
    let landlock = LinuxSandbox::detect(leased(), Path::new(HELPER)).unwrap();
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
        leased(),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
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
    let sandbox = LinuxSandbox::detect(policy(&case, network.clone()), Path::new(HELPER)).unwrap();
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
    assert_eq!(policy.connect, [443, 8080]);
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
            limit_network: true
        })
    );
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
    let guarded = || {
        policy(&case, SandboxNetwork::Open)
            .guard_engine(&runtime)
            .unwrap()
    };
    assert_eq!(
        reach(None, &case, &socket, port, &runtime),
        "pair=ok\nunix=ok\ntcp=ok\nruntime=present\n",
        "the unsandboxed control must reach the socket"
    );
    let landlock = LinuxSandbox::detect(guarded(), Path::new(HELPER)).unwrap();
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
        guarded(),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
        },
    )
    .unwrap();
    assert_eq!(
        reach(Some(&bubblewrap), &case, &socket, port, &runtime),
        "pair=ok\nunix=refused\ntcp=ok\nruntime=absent\n"
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
    let landlock =
        LinuxSandbox::detect(policy(&case, SandboxNetwork::Open), Path::new(HELPER)).unwrap();
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
        policy(&case, SandboxNetwork::Open),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
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
