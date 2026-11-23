use std::time::{Duration, Instant};

const RUNS: usize = 200;

/// The 95th percentile of the timed runs, in whole microseconds, as the budget lines quote
/// it. Input: one duration per run. Output: the p95 in microseconds; the row fails when
/// the runs are not exactly [`RUNS`].
fn p95_us(mut runs: Vec<Duration>) -> u128 {
    assert_eq!(runs.len(), RUNS);
    runs.sort_unstable();
    let rank = (RUNS * 95).div_ceil(100) - 1;
    runs[rank].as_micros()
}

/// Times one step [`RUNS`] times and prints the budget line for its row. Inputs: the row
/// name and the step. Output: the p95 in microseconds, which must be above 0, so a row
/// that times nothing fails.
fn timed(row: &str, mut step: impl FnMut()) -> u128 {
    let runs: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let started = Instant::now();
            step();
            started.elapsed()
        })
        .collect();
    let p95 = p95_us(runs);
    assert!(p95 > 0, "{row} timed nothing");
    println!("budget {row} p95_us={p95} runs={RUNS}");
    p95
}

/// Building one agent's sandbox for a fixed project, as a macOS launch does: the project
/// sandbox, its Seatbelt profile written to disk and the confined agent command. Prints
/// `budget profile_build_p95 p95_us=<n> runs=200`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "budget row, run with --ignored --nocapture --test-threads=1"]
fn profile_build_p95() {
    use crazyagent::sandbox::{ProjectSandbox, agent_command};
    use kernel_sandbox::net::SandboxNetwork;
    use kernel_sandbox::profile::command::SANDBOX_EXEC;
    use std::ffi::OsStr;
    use std::path::Path;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (project, home, tmp) = (root.join("project"), root.join("home"), root.join("tmp"));
    let profiles = root.join("profiles");
    for folder in [&project, &home, &tmp, &profiles] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let profile = profiles.join("agent.sb");
    let base = [("HOME", home.as_os_str()), ("TMPDIR", tmp.as_os_str())];
    let program = Path::new("/usr/bin/true");
    timed("profile_build_p95", || {
        let sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Open).unwrap();
        sandbox.install(&profile).unwrap();
        let none = std::iter::empty::<(&OsStr, Option<&OsStr>)>();
        let command = agent_command(&profile, program, &base, none).unwrap();
        assert_eq!(command.get_program(), SANDBOX_EXEC);
    });
    assert!(profile.is_file());
}

/// Reading one project's saved agent policy from an engine database file, as every launch
/// does through `sandbox::policy_store::load`. The policy is saved once first, so each read finds
/// its row. Prints `budget policy_load_p95 p95_us=<n> runs=200`.
#[test]
#[ignore = "budget row, run with --ignored --nocapture --test-threads=1"]
fn policy_load_p95() {
    use crazyagent::sandbox::policy_store;
    use kernel_sandbox::policy::AgentPolicy;
    let temp = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(temp.path().join("engine.db")).unwrap();
    let saved = AgentPolicy::default();
    policy_store::save(&connection, "budget-project", &saved).unwrap();
    timed("policy_load_p95", || {
        let loaded = policy_store::load(&connection, "budget-project").unwrap();
        assert_eq!(loaded, saved);
    });
}

/// Building one agent's Linux boundary for a fixed project, as a Linux launch does: the
/// project sandbox, the backend [`LinuxSandbox::detect`] picks with the Landlock helper,
/// and the wrapped harness command, which carries the Landlock policy to the helper as
/// arguments; Linux writes no policy file. Prints
/// `budget linux_policy_write_p95 p95_us=<n> runs=200`.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "budget row, run with --ignored --nocapture --test-threads=1"]
fn linux_policy_write_p95() {
    use crazyagent::sandbox::ProjectSandbox;
    use crazyagent::sandbox::linux::LinuxSandbox;
    use kernel_sandbox::net::SandboxNetwork;
    use kernel_sandbox::profile::command::Sandbox;
    use std::path::Path;
    use std::process::Command;
    let helper = Path::new(env!("CARGO_BIN_EXE_crazyagent-landlock"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (project, home, tmp) = (root.join("project"), root.join("home"), root.join("tmp"));
    for folder in [&project, &home, &tmp] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let harness = Command::new("/bin/true");
    timed("linux_policy_write_p95", || {
        let sandbox = ProjectSandbox::new(&project, &home, &tmp, SandboxNetwork::Open).unwrap();
        let linux = LinuxSandbox::detect(sandbox, helper).unwrap();
        let command = linux.wrap(&harness).unwrap();
        assert_ne!(command.get_program(), harness.get_program());
    });
}
