#![cfg(target_os = "linux")]
use crazyagent::sandbox::linux::{BWRAP_PATHS, HelperRequest, LinuxBackend, LinuxSandbox};

use crazyagent::sandbox::{
    InstalledSandbox, ProjectSandbox, Sandbox, SandboxError, SandboxNetwork, SandboxRemote,
};

use std::ffi::{OsStr, OsString};

use std::fs;

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
         (echo x > {via_link} || true) && (cat {fixture} > {leak} || true)",
        inside = quoted(&worktrees.join("inside.txt")),
        home = quoted(&case.home.join("home.txt")),
        escape = quoted(&case.outside.join("escape.txt")),
        brief = quoted(&case.project.join("context").join("brief.md")),
        hook = quoted(&git.join("hooks").join("pre-commit")),
        top = quoted(&case.project.join("top.txt")),
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
        "Linux test machines need Landlock (Linux 5.13 or newer): {:?}",
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
        git.join("objects"),
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
    sibling: PathBuf,
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
    let sibling = project.join("worktrees").join("agent-2");
    for worktree in [&own, &sibling] {
        git(
            &project,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
    }
    Checkouts {
        _temp: temp,
        project,
        home,
        tmp,
        own,
        sibling,
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

fn assert_confined(sandbox: &dyn Sandbox, checkouts: &Checkouts) {
    let script = format!(
        "export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null\n\
         g='git -c user.name=W -c user.email=w@crazyagent.invalid -c core.hooksPath=/dev/null'\n\
         echo mine > mine.txt && $g add mine.txt && $g commit -q -m mine && echo committed\n\
         (echo x > {sibling}) 2>/dev/null && echo sibling=written || echo sibling=refused\n\
         (echo x >> {owner}) 2>/dev/null && echo owner=written || echo owner=refused\n\
         (echo x > {fresh}) 2>/dev/null && echo project=written || echo project=refused\n\
         (ln {owner} linked.txt) 2>/dev/null && echo link=made || echo link=refused\n",
        sibling = quoted(&checkouts.sibling.join("escaped.txt")),
        owner = quoted(&checkouts.project.join("owner.txt")),
        fresh = quoted(&checkouts.project.join("fresh.txt")),
    );
    let mut harness = Command::new("/bin/sh");
    harness.arg("-c").arg(script).current_dir(&checkouts.own);
    let output = crazyagent::spawn_gate::output(
        sandbox
            .wrap(&harness)
            .unwrap()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "committed\nsibling=refused\nowner=refused\nproject=refused\nlink=refused\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!checkouts.sibling.join("escaped.txt").exists());
    assert_eq!(
        fs::read_to_string(checkouts.project.join("owner.txt")).unwrap(),
        "owner\n"
    );
    assert!(!checkouts.project.join("fresh.txt").exists());
}

#[test]
fn a_confined_landlock_agent_writes_only_its_own_worktree() {
    let checkouts = checkouts();
    let sandbox = LinuxSandbox::detect(confined(&checkouts), Path::new(HELPER)).unwrap();
    assert!(
        matches!(sandbox.backend(), LinuxBackend::Landlock { .. }),
        "Linux test machines need Landlock (Linux 5.13 or newer): {:?}",
        sandbox.backend()
    );
    assert_confined(&sandbox, &checkouts);
}

#[test]
fn a_confined_bubblewrap_agent_writes_only_its_own_worktree() {
    let checkouts = checkouts();
    let bwrap = BWRAP_PATHS
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .expect("Linux test machines need bubblewrap installed");
    let sandbox = LinuxSandbox::new(
        confined(&checkouts),
        LinuxBackend::Bubblewrap {
            bwrap: bwrap.to_path_buf(),
        },
    )
    .unwrap();
    assert_confined(&sandbox, &checkouts);
}
