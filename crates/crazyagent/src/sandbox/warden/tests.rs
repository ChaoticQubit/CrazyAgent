use super::fixture::{DetachingRun, reaped_within, survivors};
use super::*;
use rustix::process::Pid;
use std::os::unix::process::CommandExt;
use std::process::Stdio;

#[test]
fn the_profile_every_agent_gets_confines_its_signals_and_a_looser_one_does_not() {
    let run = DetachingRun::new();
    let generated = std::fs::read_to_string(&run.profile).unwrap();
    assert!(confines_signals(&generated));
    for looser in [
        generated.replace("(deny default)", "(allow default)"),
        generated.replace("(deny default)\n", ""),
        format!("{generated}(allow signal)\n"),
        format!("{generated}(allow signal (target others))\n"),
        format!("{generated}(allow file-read* signal)\n"),
    ] {
        assert!(!confines_signals(&looser), "{looser}");
    }
}

#[test]
fn only_the_session_shim_under_a_confining_profile_gets_a_warden() {
    let run = DetachingRun::new();
    assert!(wardable(&run.command(&run.profile)).unwrap());
    let loose = run.project.join("loose.sb");
    std::fs::write(&loose, "(version 1)\n(allow default)\n").unwrap();
    assert!(!wardable(&run.command(&loose)).unwrap());
    let mut bare = Command::new(super::super::SANDBOX_EXEC);
    bare.arg("-f").arg(&run.profile).arg("/bin/sh");
    assert!(!wardable(&bare).unwrap());
    assert!(!wardable(&Command::new("/bin/sh")).unwrap());
}

#[test]
fn a_fired_warden_stops_every_process_its_run_detached_and_reports_it() {
    let run = DetachingRun::new();
    let (mut child, warden) = spawn(&mut run.command(&run.profile)).unwrap();
    let processes = run.processes();
    let warden = warden.unwrap();
    assert_eq!(warden.fire(FIRE_TIME), Ok(()));
    assert!(reaped_within(&mut child, Duration::from_secs(2)));
    assert_eq!(survivors(processes, Duration::from_secs(1)), []);
}

/// Starts a run whose session shim is forked by a process inside the run's own sandbox, so
/// the warden can signal the process that started it. Takes the run. Returns the child and
/// the engine's end of its warden.
fn unarmed_run(run: &DetachingRun) -> (std::process::Child, std::os::unix::net::UnixStream) {
    let mut command = Command::new(super::super::SANDBOX_EXEC);
    command
        .arg("-f")
        .arg(&run.profile)
        .args([
            "/usr/bin/perl",
            "-mPOSIX",
            "-e",
            "defined(my $pid = fork()) or exit 1; if (!$pid) { exec {$ARGV[0]} @ARGV; exit 1 } POSIX::close($ENV{CRAZYAGENT_WARDEN_FD}); waitpid($pid, 0)",
        ])
        .args(super::super::SESSION_SHIM)
        .args(["/bin/sh", "-c", "echo $$ > leader.pid; exec /bin/sleep 30"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&run.project)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let (engine, warden) = crate::spawn_gate::unix_pair_std().unwrap();
    let handed = std::os::fd::OwnedFd::from(warden);
    command.env(
        WARDEN_FD,
        std::os::fd::AsRawFd::as_raw_fd(&handed).to_string(),
    );
    let child = crate::spawn_gate::spawn_handing(&mut command, handed).unwrap();
    (child, engine)
}

#[test]
fn a_warden_that_can_signal_the_process_that_started_it_does_not_arm() {
    let run = DetachingRun::new();
    let (mut child, engine) = unarmed_run(&run);
    assert_eq!(
        Warden::reported(engine, ARM_TIME).fire(FIRE_TIME),
        Err("warden_unarmed")
    );
    let harness = run.written("leader.pid");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rustix::process::test_kill_process(harness), Ok(()));
    assert!(child.try_wait().unwrap().is_none());
    rustix::process::kill_process(harness, rustix::process::Signal::KILL).unwrap();
    assert!(reaped_within(&mut child, Duration::from_secs(2)));
}

#[test]
fn an_unarmed_warden_that_already_exited_is_still_reported_unarmed() {
    let run = DetachingRun::new();
    let (mut child, engine) = unarmed_run(&run);
    let warden = Warden::reported(engine, ARM_TIME);
    assert_eq!(warden.report, b"U");
    assert_eq!(
        hear(
            &warden.engine,
            &mut [0_u8; 1],
            Instant::now() + Duration::from_secs(2)
        ),
        Heard::Closed
    );
    assert_eq!(warden.fire(FIRE_TIME), Err("warden_unarmed"));
    rustix::process::kill_process(run.written("leader.pid"), rustix::process::Signal::KILL)
        .unwrap();
    assert!(reaped_within(&mut child, Duration::from_secs(2)));
}

#[test]
fn a_warden_stops_its_run_when_the_engine_holding_its_end_is_killed() {
    let run = DetachingRun::new();
    let (mut child, warden) = spawn(&mut run.command(&run.profile)).unwrap();
    let processes = run.processes();
    let Warden { engine, .. } = warden.unwrap();
    let mut engine_process = crate::spawn_gate::spawn_handing(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        std::os::fd::OwnedFd::from(engine),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for process in processes {
        assert_eq!(rustix::process::test_kill_process(process), Ok(()));
    }
    engine_process.kill().unwrap();
    engine_process.wait().unwrap();
    assert!(reaped_within(&mut child, Duration::from_secs(2)));
    assert_eq!(survivors(processes, Duration::from_secs(1)), []);
}

#[test]
fn a_group_signal_right_after_spawn_cannot_reach_the_warden() {
    let run = DetachingRun::new();
    let (mut child, warden) = spawn(&mut run.command(&run.profile)).unwrap();
    rustix::process::kill_process_group(
        rustix::process::Pid::from_child(&child),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    assert!(reaped_within(&mut child, Duration::from_secs(2)));
    assert_eq!(warden.unwrap().fire(FIRE_TIME), Ok(()));
}

#[test]
fn a_warden_that_fired_and_closed_before_the_engine_reads_is_confirmed() {
    let run = DetachingRun::new();
    let (mut child, warden) = spawn(&mut run.command(&run.profile)).unwrap();
    let processes = run.processes();
    let warden = warden.unwrap();
    warden.engine.shutdown(std::net::Shutdown::Write).unwrap();
    assert!(reaped_within(&mut child, Duration::from_secs(2)));
    assert_eq!(survivors(processes, Duration::from_secs(1)), []);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(warden.fire(FIRE_TIME), Ok(()));
}

/// Starts runs at the same time, each through [`spawn`] from its own thread. Takes the runs.
/// Returns each run's child, warden and three processes, in the order given.
fn started_together(runs: &[&DetachingRun]) -> Vec<(std::process::Child, Warden, [Pid; 3])> {
    let barrier = std::sync::Barrier::new(runs.len());
    let barrier = &barrier;
    let started: Vec<(std::process::Child, Warden)> = std::thread::scope(|scope| {
        let threads: Vec<_> = runs
            .iter()
            .map(|run| {
                scope.spawn(move || {
                    barrier.wait();
                    let (child, warden) = spawn(&mut run.command(&run.profile)).unwrap();
                    (child, warden.unwrap())
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect()
    });
    assert_eq!(started.len(), runs.len());
    started
        .into_iter()
        .zip(runs)
        .map(|((child, warden), run)| (child, warden, run.processes()))
        .collect()
}

/// Checks that a run still stands: its harness is not reaped and its detached processes
/// still take signals. Takes the label, the child and its three processes.
fn standing(label: &str, child: &mut std::process::Child, processes: [Pid; 3]) {
    std::thread::sleep(Duration::from_millis(200));
    assert!(child.try_wait().unwrap().is_none(), "{label} harness ended");
    for process in processes {
        assert_eq!(
            rustix::process::test_kill_process(process),
            Ok(()),
            "{label} lost {process:?}"
        );
    }
}

/// Starts three runs at the same time, ends the first by firing its warden and the second by
/// killing a process that holds its warden's end, as when the engine dies, and checks after
/// each end that only the ended run is gone. The third run's warden then still reports its
/// own kill. Takes the three runs.
fn each_warden_ends_only_its_own_run(runs: [&DetachingRun; 3]) {
    let mut started = started_together(&runs).into_iter();
    let (mut fired, fired_warden, fired_processes) = started.next().unwrap();
    let (mut orphaned, orphaned_warden, orphaned_processes) = started.next().unwrap();
    let (mut spared, spared_warden, spared_processes) = started.next().unwrap();
    assert!(started.next().is_none());
    standing("orphaned", &mut orphaned, orphaned_processes);
    standing("spared", &mut spared, spared_processes);

    assert_eq!(fired_warden.fire(FIRE_TIME), Ok(()));
    assert!(reaped_within(&mut fired, Duration::from_secs(2)));
    assert_eq!(survivors(fired_processes, Duration::from_secs(1)), []);
    standing("orphaned after the fire", &mut orphaned, orphaned_processes);
    standing("spared after the fire", &mut spared, spared_processes);

    let Warden { engine, .. } = orphaned_warden;
    let mut engine_process = crate::spawn_gate::spawn_handing(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
        std::os::fd::OwnedFd::from(engine),
    )
    .unwrap();
    standing(
        "orphaned with its end handed",
        &mut orphaned,
        orphaned_processes,
    );
    engine_process.kill().unwrap();
    engine_process.wait().unwrap();
    assert!(reaped_within(&mut orphaned, Duration::from_secs(2)));
    assert_eq!(survivors(orphaned_processes, Duration::from_secs(1)), []);
    standing(
        "spared after the engine kill",
        &mut spared,
        spared_processes,
    );

    assert_eq!(spared_warden.fire(FIRE_TIME), Ok(()));
    assert!(reaped_within(&mut spared, Duration::from_secs(2)));
    assert_eq!(survivors(spared_processes, Duration::from_secs(1)), []);
}

#[test]
fn runs_started_together_from_one_profile_file_are_each_ended_only_by_their_own_warden() {
    let run = DetachingRun::new();
    let runs = [
        run.beside("fired"),
        run.beside("orphaned"),
        run.beside("spared"),
    ];
    assert!(runs.iter().all(|beside| beside.profile == run.profile));
    each_warden_ends_only_its_own_run([&runs[0], &runs[1], &runs[2]]);
}

#[test]
fn runs_started_together_from_their_own_profiles_are_each_ended_only_by_their_own_warden() {
    let runs = [
        DetachingRun::new(),
        DetachingRun::new(),
        DetachingRun::new(),
    ];
    assert_ne!(runs[0].profile, runs[1].profile);
    assert_ne!(runs[1].profile, runs[2].profile);
    each_warden_ends_only_its_own_run([&runs[0], &runs[1], &runs[2]]);
}
