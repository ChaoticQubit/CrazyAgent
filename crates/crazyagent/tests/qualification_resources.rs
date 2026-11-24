#![cfg(unix)]

use crazyagent::isolation::run_resource_probe;
use std::fs;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[test]
fn inherited_resource_limits_have_real_observations() {
    let scratch =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local-state/resource-tests");
    fs::create_dir_all(&scratch).unwrap();
    let root = tempfile::tempdir_in(scratch).unwrap().keep();
    eprintln!("retained resource artifacts: {}", root.display());
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_crazyagent-boundary-child"));
    let evidence = run_resource_probe(&executable, &root).unwrap();
    assert_eq!(evidence.len(), 7);
    let cpu = &evidence["limit-cpu"];
    assert!(matches!(cpu.signal, Some(24 | 9)), "{cpu:?}");
    assert!(
        cpu.stdout
            .contains("inherited Cpu soft=Some(2) hard=Some(2)"),
        "{cpu:?}"
    );
    assert!(cpu.elapsed_ms < 5000, "{cpu:?}");
    assert!(
        evidence["memory-baseline"]
            .stdout
            .contains("allocated-and-touched=25165824")
    );
    assert!(
        evidence["process-baseline"]
            .stdout
            .contains("children-started=6")
    );
    for name in ["limit-as", "limit-rss", "limit-nproc"] {
        assert!(
            evidence[name].stdout.contains("set-limit"),
            "{:?}",
            evidence[name]
        );
    }
}

/// Runs the boundary child with `args` from a cleared environment and collects its exit
/// status, standard output and standard error.
fn child(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_crazyagent-boundary-child"));
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    kernel_processes::spawn_gate::collected(&mut command).unwrap()
}

/// Under the small address-space limit the child states the 16 MiB limit it asked for and
/// never allocates and touches its 24 MiB block: the limit is refused, the limited child
/// cannot start, or its reservation is denied, and each ends without the allocation line.
#[test]
fn limit_as_small_refuses_the_small_allocation() {
    let output = child(&["limit-as-small"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("set-limit As requested=16777216 result="),
        "{output:?}"
    );
    assert!(!stdout.contains("allocated-and-touched"), "{output:?}");
    assert!(
        !output.status.success() || stdout.contains("allocation-denied size=25165824"),
        "{output:?}"
    );
}

/// The inherited role first prints the name of the limit it read, soft and hard values
/// beside it, for each of the four names it takes; on macOS the resident-set name reads
/// the address-space limit, which is the same limit there. Any other name is refused
/// with exit code 2 and nothing on standard output.
#[test]
fn inherited_names_each_resource_it_reads() {
    let rss = if cfg!(target_os = "macos") {
        "As"
    } else {
        "Rss"
    };
    for (asked, read) in [
        ("Cpu", "Cpu"),
        ("As", "As"),
        ("Rss", rss),
        ("Nproc", "Nproc"),
    ] {
        let output = child(&["inherited", asked]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let first = stdout.lines().next().unwrap_or_default();
        assert!(
            first.starts_with(&format!("inherited {read} soft=")) && first.contains(" hard="),
            "{asked}: {output:?}"
        );
        assert!(output.status.success(), "{asked}: {output:?}");
    }
    let refused = child(&["inherited", "Disk"]);
    assert_eq!(refused.status.code(), Some(2), "{refused:?}");
    assert!(refused.stdout.is_empty(), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("unknown inherited resource"),
        "{refused:?}"
    );
}

/// A sleep child left alone ends by itself with exit code 0 after its 100 ms sleep, and
/// one whose process group is killed first ends by that signal before its sleep is over,
/// so no sleep child outlives the group it was started in.
#[test]
fn sleep_child_outlives_its_parent_only_until_the_group_ends() {
    let started = Instant::now();
    let alone = child(&["sleep-child"]);
    assert!(alone.status.success(), "{alone:?}");
    assert!(started.elapsed() >= Duration::from_millis(100), "{alone:?}");

    let mut command = Command::new(env!("CARGO_BIN_EXE_crazyagent-boundary-child"));
    command
        .arg("sleep-child")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut sleeper = kernel_processes::spawn_gate::spawn(&mut command).unwrap();
    let group = rustix::process::Pid::from_child(&sleeper);
    rustix::process::kill_process_group(group, rustix::process::Signal::KILL).unwrap();
    let status = sleeper.wait().unwrap();
    assert_eq!(status.signal(), Some(9), "{status:?}");
}
