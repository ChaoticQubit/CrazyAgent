use super::{CodexHostOutput, CodexHostProcess};
use crate::routing::ResourceBudget;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn budget() -> ResourceBudget {
    ResourceBudget {
        output_bytes: 1024,
        wall_time_ms: 2000,
        ..ResourceBudget::unit()
    }
}

fn selected_command(program: &str, root: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    command
}

#[tokio::test]
async fn codex_host_process_real_pipe_input_output_eof_and_reap_are_retained() {
    let root = tempfile::tempdir().unwrap();
    let command = selected_command("/bin/cat", root.path());
    let mut process = CodexHostProcess::spawn(command, &budget()).await.unwrap();
    let actual_pid = process.pid();
    assert!(actual_pid > 1);
    process.send(b"unit inert pipe bytes\n").await.unwrap();
    process.close_stdin();
    let mut stdout = Vec::new();
    let status = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match process.next_output().await.unwrap() {
                CodexHostOutput::Stdout(bytes) => stdout.extend(bytes),
                CodexHostOutput::Exited => break process.exit_status().expect("reaped status"),
            }
        }
    })
    .await
    .unwrap();
    assert!(status.success());
    assert_eq!(stdout, b"unit inert pipe bytes\n");
    assert_eq!(process.observed_bytes(), (stdout.len() as u64, 0));
    assert_eq!(process.pid(), actual_pid);
    assert_eq!(process.close().await.unwrap(), status);
    assert_eq!(process.close().await.unwrap(), status);
}

/// A shell script that writes 5,600 numbered lines of 101 bytes, 565,600 bytes: more than
/// the 128 chunks of 4,096 bytes the output queue holds.
const FLOOD: &str =
    "i=0; while [ \"$i\" -lt 5600 ]; do i=$((i+1)); printf '%0100d\\n' \"$i\"; done";

/// A reader that stalls while the harness writes more than the output queue holds no longer
/// ends the run (OB-5e): it used to fail with codex_host_output_backpressure at the 129th
/// unread chunk. What the queue cannot hold stays in the pipe, every byte arrives in order,
/// and a harness that exits while its last output still waits there is read to its end, not
/// given up.
#[tokio::test]
async fn a_stalled_reader_holds_the_harness_output_back_and_loses_none_of_it() {
    let root = tempfile::tempdir().unwrap();
    let mut command = selected_command("/bin/sh", root.path());
    command.args(["-c", FLOOD]);
    let limits = ResourceBudget {
        output_bytes: 1_048_576,
        wall_time_ms: 20_000,
        ..ResourceBudget::unit()
    };
    let mut process = CodexHostProcess::spawn(command, &limits).await.unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut stdout = Vec::new();
    let status = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match process.next_output().await.unwrap() {
                CodexHostOutput::Stdout(bytes) => stdout.extend(bytes),
                CodexHostOutput::Exited => break process.exit_status().expect("reaped status"),
            }
        }
    })
    .await
    .unwrap();
    assert!(status.success());
    assert_eq!(stdout.len(), 5_600 * 101);
    assert_eq!(process.observed_bytes(), (stdout.len() as u64, 0));
    let text = String::from_utf8(stdout).unwrap();
    assert_eq!(text.lines().count(), 5_600);
    for (index, line) in text.lines().enumerate() {
        assert_eq!(line, format!("{:0100}", index + 1));
    }
    assert_eq!(process.close().await.unwrap(), status);
}

#[tokio::test]
async fn codex_host_process_quiet_deadline_preserves_child_until_explicit_bounded_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let mut command = selected_command("/bin/sleep", root.path());
    command.arg("30");
    let limits = ResourceBudget {
        wall_time_ms: 250,
        ..budget()
    };
    let started = Instant::now();
    let mut process = CodexHostProcess::spawn(command, &limits).await.unwrap();
    let actual_pid = process.pid();
    let error = tokio::time::timeout(Duration::from_secs(2), process.next_output())
        .await
        .unwrap()
        .err()
        .expect("quiet child must hit its observed deadline");
    assert_eq!(error.0, "codex_host_deadline");
    assert_eq!(process.pid(), actual_pid);
    assert_eq!(process.observed_bytes(), (0, 0));
    let status = tokio::time::timeout(Duration::from_secs(3), process.close())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    assert_eq!(process.close().await.unwrap(), status);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn codex_host_process_real_stderr_overflow_is_an_error_with_observed_bytes_and_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let mut command = selected_command("/bin/sh", root.path());
    command.args(["-c", "printf 12345678901234567 >&2", "codex-host-inert"]);
    let limits = ResourceBudget {
        output_bytes: 16,
        ..budget()
    };
    let mut process = CodexHostProcess::spawn(command, &limits).await.unwrap();
    process.close_stdin();
    let error = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match process.next_output().await {
                Err(error) => break error,
                Ok(CodexHostOutput::Stdout(bytes)) => assert!(bytes.is_empty()),
                Ok(CodexHostOutput::Exited) => {
                    panic!(
                        "overflow was reported as ordinary exit: {:?}",
                        process.exit_status()
                    )
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(error.0, "codex_host_output_limit");
    assert_eq!(process.observed_bytes(), (0, 17));
    let status = tokio::time::timeout(Duration::from_secs(3), process.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(process.close().await.unwrap(), status);
}

#[cfg(unix)]
async fn stand_in_host(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    body: &str,
) -> CodexHostProcess {
    stand_in_host_at(fixture, body, false).await
}

/// Starts a stand-in Codex host process running `body` under custody with a real lease,
/// at Full access when `unconfined`. Takes the fixture, the script body and the access.
/// Returns the process.
#[cfg(unix)]
async fn stand_in_host_at(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    body: &str,
    unconfined: bool,
) -> CodexHostProcess {
    let profile = fixture.lease("codex-group-turn", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("codex", body);
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let sandbox = if unconfined {
        sandbox.unconfined()
    } else {
        sandbox
    };
    let mut command = Command::new(&harness);
    command.current_dir(std::fs::canonicalize(&fixture.project).unwrap());
    let command = sandbox.wrap(&command).unwrap();
    let limits = ResourceBudget {
        wall_time_ms: 20_000,
        ..budget()
    };
    let clock = Some(Duration::from_millis(limits.wall_time_ms));
    let lease = Some((profile, fixture.project_id.clone()));
    CodexHostProcess::spawn_guarded(command, &limits, clock, lease, unconfined)
        .await
        .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn closing_a_codex_host_kills_its_whole_group_and_releases_the_lease() {
    use crate::engine::master_host_fixture::{
        HarnessProcesses, MasterFixture, PIPE_HOLDING_HARNESS,
    };

    let mut fixture = MasterFixture::new("codex");
    let mut process = stand_in_host(&mut fixture, PIPE_HOLDING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        u32::try_from(processes.leader.as_raw_nonzero().get()),
        Ok(process.pid())
    );
    let group = rustix::process::getpgid(Some(processes.descendant));
    assert_eq!(
        fixture
            .sign_in("codex-group-busy", 2)
            .err()
            .map(|error| error.0)
            .as_deref(),
        Some("native_profile_in_use")
    );
    let started = Instant::now();
    let closed = process.close().await;
    let elapsed = started.elapsed();
    assert!(closed.is_ok(), "{closed:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(group, Ok(processes.leader));
    processes.assert_gone();
    assert!(fixture.sign_in("codex-group-next", 3).is_ok());
}

#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn a_codex_host_process_that_left_its_group_cannot_hold_custody_or_the_lease() {
    use crate::engine::master_host_fixture::{
        GROUP_LEAVING_HARNESS, HarnessProcesses, MasterFixture,
    };

    let mut fixture = MasterFixture::new("codex");
    let mut process = stand_in_host(&mut fixture, GROUP_LEAVING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.descendant)
    );
    let started = Instant::now();
    let closed = process.close().await;
    let elapsed = started.elapsed();
    assert_eq!(
        closed.map_err(|error| error.0),
        Err("codex_host_cleanup_uncertain".to_owned()),
        "after {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_harness_gone();
    assert_eq!(
        rustix::process::test_kill_process(processes.descendant),
        Ok(())
    );
    assert!(fixture.sign_in("codex-group-next", 3).is_ok());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_codex_host_process_that_left_its_group_is_stopped_with_the_host() {
    use crate::engine::master_host_fixture::{
        GROUP_LEAVING_HARNESS, HarnessProcesses, MasterFixture,
    };

    let mut fixture = MasterFixture::new("codex");
    let mut process = stand_in_host(&mut fixture, GROUP_LEAVING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.descendant)
    );
    let started = Instant::now();
    let closed = process.close().await;
    let elapsed = started.elapsed();
    assert!(closed.is_ok(), "{closed:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_gone();
    assert!(fixture.sign_in("codex-group-next", 3).is_ok());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_codex_host_whose_group_keeps_an_unreaped_process_still_ends_and_releases_the_lease() {
    use crate::engine::master_host_fixture::{
        HarnessProcesses, MasterFixture, ZOMBIE_HOLDING_HARNESS,
    };

    let mut fixture = MasterFixture::new("codex");
    let mut process = stand_in_host(&mut fixture, ZOMBIE_HOLDING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.descendant)
    );
    let closed = process.close().await;
    assert_eq!(
        closed.map_err(|error| error.0),
        Err("codex_host_cleanup_uncertain".to_owned())
    );
    let started = Instant::now();
    while fixture.sign_in("codex-group-next", 3).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the profile lease was never released"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    while rustix::process::test_kill_process_group(processes.leader) != Err(rustix::io::Errno::SRCH)
    {
        assert!(
            started.elapsed() < Duration::from_secs(12),
            "the harness group was never emptied"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    processes.assert_gone();
}

#[cfg(unix)]
#[tokio::test]
async fn a_codex_host_whose_harness_it_can_never_reap_still_ends_and_releases_the_lease() {
    use crate::engine::master_host_fixture::{MasterFixture, reap_before_the_host};

    for _ in 0..5 {
        let mut fixture = MasterFixture::new("codex");
        let mut process = stand_in_host(
            &mut fixture,
            "#!/bin/sh\necho $$ > run/leader.pid\nexec /bin/sleep 30\n",
        )
        .await;
        if !reap_before_the_host(&fixture.project) {
            continue;
        }
        let started = Instant::now();
        while fixture.sign_in("codex-unreaped-next", 3).is_err() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the profile lease was never released"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(process.close().await.is_err());
        return;
    }
    panic!("the host reaped its harness first on every attempt");
}

#[cfg(unix)]
#[tokio::test]
async fn a_codex_host_whose_output_accounting_fails_still_ends_and_releases_the_lease() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("codex");
    let mut process = stand_in_host(
        &mut fixture,
        "#!/bin/sh\ni=0\nwhile [ ! -f go ] && [ $i -lt 400 ]; do /bin/sleep 0.05; i=$((i+1)); done\necho counted\nexec /bin/sleep 30\n",
    )
    .await;
    process.poison_shared_state();
    std::fs::write(fixture.project.join("go"), b"").unwrap();
    let started = Instant::now();
    while fixture.sign_in("codex-accounting-next", 3).is_err() {
        assert!(
            started.elapsed() < super::process::CLOSE_TIME + Duration::from_secs(5),
            "the profile lease was never released"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        process.close().await.map_err(|error| error.0),
        Err("codex_host_custody_poisoned".to_owned())
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn closing_a_codex_host_stops_what_its_commands_detached() {
    use crate::engine::master_host_fixture::{DETACHING_HARNESS, DetachedProcesses, MasterFixture};

    let mut fixture = MasterFixture::new("codex");
    let body = format!("{DETACHING_HARNESS}exec /bin/sleep 30\n");
    let mut process = stand_in_host(&mut fixture, &body).await;
    let processes = DetachedProcesses::wait(&fixture.project);
    let started = Instant::now();
    let closed = process.close().await;
    let elapsed = started.elapsed();
    assert!(closed.is_ok(), "{closed:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_gone();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn closing_a_full_access_codex_host_names_what_it_left_running_and_keeps_the_lease_until_it_ends()
 {
    use crate::engine::master_host_fixture::{DETACHING_HARNESS, DetachedProcesses, MasterFixture};
    use crate::harnesses::leftovers;

    let mut fixture = MasterFixture::new("codex");
    let body = format!("{DETACHING_HARNESS}exec /bin/sleep 30\n");
    let mut process = stand_in_host_at(&mut fixture, &body, true).await;
    let processes = DetachedProcesses::wait(&fixture.project);
    let closed = process.close().await;
    assert_eq!(
        closed.map_err(|error| error.0),
        Err(leftovers::LEFT.to_owned()),
        "a Full access Codex host ended clean while what it started outside its terminal ran"
    );
    let named = format!("{} perl", processes.session.as_raw_nonzero());
    let reports = leftovers::held_by(&fixture.connection.id);
    assert!(
        reports.iter().any(|report| report.contains(&named)),
        "{reports:?}"
    );
    assert_eq!(
        fixture
            .sign_in("codex-full-busy", 2)
            .err()
            .map(|error| error.0)
            .as_deref(),
        Some("native_profile_in_use"),
        "the account lease was released while the host's leftover ran"
    );
    let _ = rustix::process::kill_process(processes.session, rustix::process::Signal::KILL);
    let started = Instant::now();
    while fixture.sign_in("codex-full-next", 3).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the account lease stayed held after the leftover ended"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// L1 r1 security L1: a Codex host turn's leftovers follow its project's Keep: the hold is
/// listed in that project's owner view as kept, the account goes at once and the orphan is
/// never signalled.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_codex_host_hold_follows_its_projects_keep_and_is_listed_for_the_owner() {
    use crate::engine::master_host_fixture::{MasterFixture, ORPHANING_HARNESS, Orphan};
    use crate::harnesses::leftovers::{self, HoldState, LeftoverMode};

    let mut fixture = MasterFixture::new("codex");
    let project = fixture.project_id.as_str().to_owned();
    leftovers::set_mode(&project, LeftoverMode::Keep);
    let body = format!("{ORPHANING_HARNESS}exec /bin/sleep 30\n");
    let mut process = stand_in_host_at(&mut fixture, &body, true).await;
    let orphan = Orphan::wait(&fixture.project);
    let closed = process.close().await;
    assert_eq!(
        closed.map_err(|error| error.0),
        Err(leftovers::LEFT.to_owned())
    );
    let listed = leftovers::held_in(&project);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].who, "a Codex host turn");
    assert_eq!(listed[0].state, HoldState::Kept);
    assert!(!listed[0].holds_account);
    assert!(
        fixture.sign_in("codex-kept-next", 2).is_ok(),
        "Keep held the account"
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        rustix::process::test_kill_process(orphan.0),
        Ok(()),
        "Keep signalled the orphan"
    );
    drop(orphan);
}

/// SETSID-L2: a Full access Codex host's process that left its session and lost its parent
/// before the close, running a program whose environment macOS hides, is still named: the
/// follow from the start kept it.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn closing_a_full_access_codex_host_names_what_left_its_session_and_parent() {
    use crate::engine::master_host_fixture::{MasterFixture, ORPHANING_HARNESS, Orphan};
    use crate::harnesses::leftovers;

    let mut fixture = MasterFixture::new("codex");
    let body = format!("{ORPHANING_HARNESS}exec /bin/sleep 30\n");
    let mut process = stand_in_host_at(&mut fixture, &body, true).await;
    let orphan = Orphan::wait(&fixture.project);
    let closed = process.close().await;
    assert_eq!(
        closed.map_err(|error| error.0),
        Err(leftovers::LEFT.to_owned()),
        "a Full access Codex host ended clean while the orphan it started ran"
    );
    let named = format!("{} perl", orphan.0.as_raw_nonzero());
    let reports = leftovers::held_by(&fixture.connection.id);
    assert!(
        reports.iter().any(|report| report.contains(&named)),
        "{reports:?}"
    );
    drop(orphan);
    let started = Instant::now();
    while fixture.sign_in("codex-orphan-next", 3).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the account lease stayed held after the orphan ended"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(unix)]
#[test]
fn a_codex_host_runs_in_the_home_its_command_names_and_never_in_another() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("codex");
    let profile = fixture.lease("codex-home-turn", 1).unwrap();
    let managed = profile.profile_path().unwrap();
    let private = fixture.root.path().join("turn-home");
    std::fs::create_dir(&private).unwrap();
    let private = std::fs::canonicalize(&private).unwrap();
    let project = std::fs::canonicalize(&fixture.project).unwrap();
    let command = |home: &Path| {
        let mut command = Command::new("/registered/codex");
        command
            .args(["-c", "cli_auth_credentials_store=\"file\"", "app-server"])
            .current_dir(&project)
            .env("HOME", home)
            .env("CODEX_HOME", home);
        command
    };
    let home_of = |command: &Command| -> Vec<(String, String)> {
        command
            .get_envs()
            .filter(|(key, _)| *key == "HOME" || *key == "CODEX_HOME")
            .map(|(key, value)| {
                (
                    key.to_str().unwrap().to_owned(),
                    value.unwrap().to_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    let (selected, _, tools) =
        super::prepare_command(command(&private), &profile, &private).unwrap();
    assert!(
        !tools,
        "a command without the tools-on settings starts a read-only thread"
    );
    let private_text = private.to_str().unwrap().to_owned();
    assert_eq!(
        home_of(&selected),
        [
            ("CODEX_HOME".to_owned(), private_text.clone()),
            ("HOME".to_owned(), private_text)
        ]
    );
    let (selected, _, _) = super::prepare_command(command(&managed), &profile, &managed).unwrap();
    let managed_text = managed.to_str().unwrap().to_owned();
    assert_eq!(
        home_of(&selected),
        [
            ("CODEX_HOME".to_owned(), managed_text.clone()),
            ("HOME".to_owned(), managed_text)
        ]
    );
    for (named, given) in [(&managed, &private), (&private, &managed)] {
        assert_eq!(
            super::prepare_command(command(named), &profile, given)
                .map(|_| ())
                .map_err(|error| error.0),
            Err("codex_host_home_unsafe".to_owned())
        );
    }
    let mut tools_on = command(&private);
    tools_on.args(["-c", "sandbox_mode=\"danger-full-access\""]);
    let (_, _, tools) = super::prepare_command(tools_on, &profile, &private).unwrap();
    assert!(tools, "the tools-on command starts a tools thread");
    let inside = managed.join("nested");
    assert_eq!(
        super::prepare_command(command(&inside), &profile, &inside)
            .map(|_| ())
            .map_err(|error| error.0),
        Err("codex_host_home_unsafe".to_owned())
    );
}

/// A stand-in Codex app-server that answers initialize, the master thread and the turn in
/// Codex 0.164's shapes, then reports a turn on a thread the master never started.
#[cfg(unix)]
const FOREIGN_THREAD_APP_SERVER: &str = r#"#!/bin/sh
read -r line
printf '%s\n' "{\"id\":1,\"result\":{\"userAgent\":\"stand-in\",\"codexHome\":\"$CODEX_HOME\",\"platformFamily\":\"unix\",\"platformOs\":\"macos\"}}"
read -r line
read -r line
cwd=$(pwd)
printf '%s\n' "{\"id\":2,\"result\":{\"thread\":{\"id\":\"stand-in-thread\",\"cwd\":\"$cwd\",\"turns\":[],\"status\":{\"type\":\"idle\"}},\"cwd\":\"$cwd\",\"model\":\"stand-in-model\",\"modelProvider\":\"openai\",\"approvalPolicy\":\"on-request\",\"approvalsReviewer\":\"user\",\"sandbox\":{\"type\":\"readOnly\",\"networkAccess\":false}}}"
read -r line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"stand-in-turn","items":[],"status":"inProgress"}}}'
printf '%s\n' '{"method":"turn/started","params":{"threadId":"stand-in-child","note":"sk-proj-NOTAREALKEY","turn":{"id":"stand-in-child-turn","items":[],"status":"inProgress"}}}'
exec /bin/sleep 30
"#;

#[cfg(unix)]
#[tokio::test]
async fn a_codex_master_protocol_failure_keeps_the_refused_frame_and_frames_without_content() {
    use crate::adapters::{NativeAction, NativeHarness};
    use crate::engine::master_host_fixture::MasterFixture;
    use crate::protocol::{CredentialRef, RequestId, SessionId};

    let mut fixture = MasterFixture::new("codex");
    let profile = fixture.lease("codex-diagnostic-turn", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("codex", FOREIGN_THREAD_APP_SERVER);
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut command = Command::new(&harness);
    command
        .args(["-c", "cli_auth_credentials_store=\"file\""])
        .env("CODEX_HOME", &home)
        .current_dir(std::fs::canonicalize(&fixture.project).unwrap());
    let action = NativeAction::Launch {
        session_id: SessionId::new("diagnostic-session").unwrap(),
        harness: NativeHarness::Codex,
        profile_ref: CredentialRef::new("group-master-profile").unwrap(),
        worktree_lease: RequestId::new("diagnostic-lease").unwrap(),
    };
    let limits = ResourceBudget {
        output_bytes: 65_536,
        wall_time_ms: 20_000,
        ..budget()
    };
    let mut host = super::CodexHost::spawn(
        command,
        sandbox,
        profile,
        home,
        action,
        CredentialRef::new("group-master-account").unwrap(),
        fixture.project_id.clone(),
        Some(Duration::from_millis(limits.wall_time_ms)),
        limits,
        "stand-in instructions",
    )
    .await
    .unwrap();
    assert!(host.diagnostic().fault.is_none());
    host.start_turn(RequestId::new("diagnostic-turn").unwrap(), "hello")
        .await
        .unwrap();
    let failed = loop {
        if let Err(error) = host.next_event().await {
            break error;
        }
    };
    assert_eq!(failed.0, "codex_host_protocol_stale_message");
    let diagnostic = host.diagnostic();
    assert_eq!(diagnostic.fault.as_deref(), Some(failed.0.as_str()));
    assert_eq!(
        diagnostic.message.as_deref(),
        Some("turn/started id=- thread=stand-in-child parent=- turn=stand-in-child-turn")
    );
    let frames = diagnostic.frames();
    assert!(
        frames.ends_with(&["response 82".to_owned(), "turn/started 162".to_owned()]),
        "{frames:?}"
    );
    let _ = host.close().await;
}

/// Starts a Codex master host on a stand-in app-server under the master's own sandbox, as
/// the engine does. Inputs: a name for the turn's lease and ids, the stand-in's script and
/// the host's budget, whose wall time is its clock. Output: the fixture, which owns the
/// folders, and the running host.
#[cfg(unix)]
async fn stand_in_master(
    name: &str,
    body: &str,
    limits: ResourceBudget,
) -> (
    crate::engine::master_host_fixture::MasterFixture,
    super::CodexHost,
) {
    use crate::adapters::{NativeAction, NativeHarness};
    use crate::engine::master_host_fixture::MasterFixture;
    use crate::protocol::{CredentialRef, RequestId, SessionId};

    let mut fixture = MasterFixture::new("codex");
    let profile = fixture.lease(&format!("codex-{name}"), 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("codex", body);
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut command = Command::new(&harness);
    command
        .args(["-c", "cli_auth_credentials_store=\"file\""])
        .env("CODEX_HOME", &home)
        .current_dir(std::fs::canonicalize(&fixture.project).unwrap());
    let action = NativeAction::Launch {
        session_id: SessionId::new(format!("{name}-session")).unwrap(),
        harness: NativeHarness::Codex,
        profile_ref: CredentialRef::new("group-master-profile").unwrap(),
        worktree_lease: RequestId::new(format!("{name}-lease")).unwrap(),
    };
    let host = super::CodexHost::spawn(
        command,
        sandbox,
        profile,
        home,
        action,
        CredentialRef::new("group-master-account").unwrap(),
        fixture.project_id.clone(),
        Some(Duration::from_millis(limits.wall_time_ms)),
        limits,
        "stand-in instructions",
    )
    .await
    .unwrap();
    assert!(host.diagnostic().fault.is_none());
    (fixture, host)
}

/// A stand-in Codex app-server that answers initialize, the master thread and the turn in
/// Codex 0.164's shapes, then writes a long turn: 10,000 reply deltas and a turn end whose
/// one line lists 10,000 items.
#[cfg(unix)]
const LONG_TURN_APP_SERVER: &str = r#"#!/bin/sh
read -r line
printf '%s\n' "{\"id\":1,\"result\":{\"userAgent\":\"stand-in\",\"codexHome\":\"$CODEX_HOME\",\"platformFamily\":\"unix\",\"platformOs\":\"macos\"}}"
read -r line
read -r line
cwd=$(pwd)
printf '%s\n' "{\"id\":2,\"result\":{\"thread\":{\"id\":\"stand-in-thread\",\"cwd\":\"$cwd\",\"turns\":[],\"status\":{\"type\":\"idle\"}},\"cwd\":\"$cwd\",\"model\":\"stand-in-model\",\"modelProvider\":\"openai\",\"approvalPolicy\":\"on-request\",\"approvalsReviewer\":\"user\",\"sandbox\":{\"type\":\"readOnly\",\"networkAccess\":false}}}"
read -r line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"stand-in-turn","items":[],"status":"inProgress"}}}'
i=0
while [ "$i" -lt 10000 ]; do
  i=$((i+1))
  printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"stand-in-thread","turnId":"stand-in-turn","itemId":"item-1","delta":"x"}}'
done
printf '%s' '{"method":"turn/completed","params":{"threadId":"stand-in-thread","turn":{"id":"stand-in-turn","status":"completed","items":[{"id":"item-0"}'
i=0
while [ "$i" -lt 9999 ]; do
  i=$((i+1))
  printf ',{"id":"item-%s"}' "$i"
done
printf '%s\n' ']}}}'
exec /bin/sleep 30
"#;

/// A long Codex master turn ends with its reply whatever it counts (OB-5e): 10,000 reply
/// deltas written while the turn's reader is busy, far more than the 128 chunks the output
/// queue holds, and a turn end listing 10,000 items, where 129 used to be refused as over
/// capacity. Every delta reaches the reader and the turn ends as a success.
#[cfg(unix)]
#[tokio::test]
async fn a_long_codex_master_turn_of_ten_thousand_items_ends_with_its_reply() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::protocol::RequestId;

    let limits = ResourceBudget {
        output_bytes: 4 * 1_048_576,
        wall_time_ms: 30_000,
        ..budget()
    };
    let (_fixture, mut host) = stand_in_master("long-turn", LONG_TURN_APP_SERVER, limits).await;
    host.start_turn(RequestId::new("long-turn").unwrap(), "hello")
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut deltas = 0;
    let ended = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match host.next_event().await.unwrap().kind {
                EventKind::AssistantDelta { text, .. } => {
                    assert_eq!(text, "x");
                    deltas += 1;
                }
                EventKind::TurnFinished { outcome, .. } => break outcome,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(deltas, 10_000, "every delta reached the reader");
    assert_eq!(ended, TurnOutcome::Success);
    assert!(host.diagnostic().fault.is_none());
    let _ = host.close().await;
}

/// A stand-in Codex app-server whose turn starts twenty native sub-agent threads at once,
/// answers the interrupt the engine sends for each by its id, then ends the master's turn
/// with a reply.
#[cfg(unix)]
const SUB_AGENTS_APP_SERVER: &str = r#"#!/bin/sh
read -r line
printf '%s\n' "{\"id\":1,\"result\":{\"userAgent\":\"stand-in\",\"codexHome\":\"$CODEX_HOME\",\"platformFamily\":\"unix\",\"platformOs\":\"macos\"}}"
read -r line
read -r line
cwd=$(pwd)
printf '%s\n' "{\"id\":2,\"result\":{\"thread\":{\"id\":\"stand-in-thread\",\"cwd\":\"$cwd\",\"turns\":[],\"status\":{\"type\":\"idle\"}},\"cwd\":\"$cwd\",\"model\":\"stand-in-model\",\"modelProvider\":\"openai\",\"approvalPolicy\":\"on-request\",\"approvalsReviewer\":\"user\",\"sandbox\":{\"type\":\"readOnly\",\"networkAccess\":false}}}"
read -r line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"stand-in-turn","items":[],"status":"inProgress"}}}'
i=0
while [ "$i" -lt 20 ]; do
  i=$((i+1))
  printf '%s\n' "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"child-$i\",\"turn\":{\"id\":\"child-turn-$i\",\"rootTurnId\":\"stand-in-turn\",\"items\":[],\"status\":\"inProgress\"}}}"
done
i=0
while [ "$i" -lt 20 ]; do
  i=$((i+1))
  read -r line
  rest=${line#*\"id\":}
  printf '%s\n' "{\"id\":${rest%%,*},\"result\":{}}"
done
printf '%s\n' '{"method":"item/agentMessage/delta","params":{"threadId":"stand-in-thread","turnId":"stand-in-turn","itemId":"item-1","delta":"done"}}'
printf '%s\n' '{"method":"turn/completed","params":{"threadId":"stand-in-thread","turn":{"id":"stand-in-turn","items":[],"status":"completed"}}}'
exec /bin/sleep 30
"#;

/// A Codex master turn that starts twenty native sub-agents ends with its reply (OB-5 r2):
/// the 17th thread used to end the turn as over capacity, and so did the 17th interrupt
/// waiting for its answer. The engine interrupts every one and the master's reply arrives.
#[cfg(unix)]
#[tokio::test]
async fn a_codex_master_turn_that_starts_twenty_native_sub_agents_ends_with_its_reply() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::protocol::RequestId;

    let limits = ResourceBudget {
        output_bytes: 65_536,
        wall_time_ms: 20_000,
        ..budget()
    };
    let (_fixture, mut host) = stand_in_master("sub-agents", SUB_AGENTS_APP_SERVER, limits).await;
    host.start_turn(RequestId::new("sub-agents-turn").unwrap(), "hello")
        .await
        .unwrap();
    let mut reply = String::new();
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match host.next_event().await.unwrap().kind {
                EventKind::AssistantDelta { text, .. } => reply.push_str(&text),
                EventKind::TurnFinished { outcome, .. } => break outcome,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(reply, "done");
    assert_eq!(ended, TurnOutcome::Success);
    assert!(host.diagnostic().fault.is_none());
    let _ = host.close().await;
}

/// A stand-in Codex app-server whose turn takes `rounds` model rounds that each start a
/// command and end with their token count, as Codex 0.164 writes them. With `interrupt` it
/// then reads one more request and, when it is the turn's interrupt, acknowledges it and
/// ends the turn interrupted; any other line, or none asked for, ends the turn completed.
/// Inputs: the rounds and the interrupt switch. Output: the script.
#[cfg(unix)]
fn steps_app_server(rounds: u16, interrupt: bool) -> String {
    let start = r#"#!/bin/sh
read -r line
printf '%s\n' "{\"id\":1,\"result\":{\"userAgent\":\"stand-in\",\"codexHome\":\"$CODEX_HOME\",\"platformFamily\":\"unix\",\"platformOs\":\"macos\"}}"
read -r line
read -r line
cwd=$(pwd)
printf '%s\n' "{\"id\":2,\"result\":{\"thread\":{\"id\":\"stand-in-thread\",\"cwd\":\"$cwd\",\"turns\":[],\"status\":{\"type\":\"idle\"}},\"cwd\":\"$cwd\",\"model\":\"stand-in-model\",\"modelProvider\":\"openai\",\"approvalPolicy\":\"on-request\",\"approvalsReviewer\":\"user\",\"sandbox\":{\"type\":\"readOnly\",\"networkAccess\":false}}}"
read -r line
printf '%s\n' '{"id":3,"result":{"turn":{"id":"stand-in-turn","items":[],"status":"inProgress"}}}'
"#;
    let rounds = format!(
        r#"i=0
while [ "$i" -lt {rounds} ]; do
  i=$((i+1))
  printf '%s\n' "{{\"method\":\"item/started\",\"params\":{{\"threadId\":\"stand-in-thread\",\"turnId\":\"stand-in-turn\",\"startedAtMs\":1,\"item\":{{\"type\":\"commandExecution\",\"id\":\"cmd-$i\"}}}}}}"
  printf '%s\n' '{{"method":"thread/tokenUsage/updated","params":{{"threadId":"stand-in-thread","turnId":"stand-in-turn","tokenUsage":{{}}}}}}'
done
"#
    );
    let end = if interrupt {
        r#"read -r line
case "$line" in
  *'"turn/interrupt"'*)
    printf '%s\n' '{"id":4,"result":{}}'
    printf '%s\n' '{"method":"turn/completed","params":{"threadId":"stand-in-thread","turn":{"id":"stand-in-turn","items":[],"status":"interrupted"}}}'
    ;;
  *)
    printf '%s\n' '{"method":"turn/completed","params":{"threadId":"stand-in-thread","turn":{"id":"stand-in-turn","items":[],"status":"completed"}}}'
    ;;
esac
exec /bin/sleep 30
"#
    } else {
        r#"printf '%s\n' '{"method":"turn/completed","params":{"threadId":"stand-in-thread","turn":{"id":"stand-in-turn","items":[],"status":"completed"}}}'
exec /bin/sleep 30
"#
    };
    format!("{start}{rounds}{end}")
}

/// Runs one stand-in Codex master turn under a step limit to its end. Inputs: a name, the
/// stand-in script and the limit, none for a host never given one. Output: the turn's
/// outcome, or the error that ended it.
#[cfg(unix)]
async fn steps_turn(
    name: &str,
    script: &str,
    limit: Option<crate::master_step_limit::StepLimit>,
) -> Result<crate::adapters::events::TurnOutcome, crate::engine::EngineError> {
    use crate::adapters::events::EventKind;
    use crate::protocol::RequestId;

    let limits = ResourceBudget {
        output_bytes: 1_048_576,
        wall_time_ms: 20_000,
        ..budget()
    };
    let (_fixture, mut host) = stand_in_master(name, script, limits).await;
    if let Some(limit) = limit {
        host.limit_steps(limit);
    }
    host.start_turn(RequestId::new(format!("{name}-turn")).unwrap(), "hello")
        .await
        .unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match host.next_event().await {
                Ok(event) => {
                    if let EventKind::TurnFinished { outcome, .. } = event.kind {
                        break Ok(outcome);
                    }
                }
                Err(error) => break Err(error),
            }
        }
    })
    .await
    .unwrap();
    let _ = host.close().await;
    ended
}

/// The master step limit stops a Codex master (PA-9): at its 50th step the host interrupts
/// the turn, and the interrupted turn ends with the limit's cause, master_step_limit_50,
/// the cause a Claude master's error_max_turns ends with. A turn that ends within its limit
/// keeps its end, and a host with no limit never interrupts.
#[cfg(unix)]
#[tokio::test]
async fn a_codex_master_stops_at_its_step_limit_with_the_limit_cause() {
    use crate::adapters::events::TurnOutcome;
    use crate::master_step_limit::{StepLimit, steps_of_cause};

    let stopped = steps_turn(
        "steps-reached",
        &steps_app_server(50, true),
        Some(StepLimit::Steps50),
    )
    .await;
    let cause = stopped.expect_err("the turn ends at its step limit").0;
    assert_eq!(cause, "master_step_limit_50");
    assert_eq!(steps_of_cause(&cause), Some(50));
    let within = steps_turn(
        "steps-within",
        &steps_app_server(49, false),
        Some(StepLimit::Steps50),
    )
    .await;
    assert_eq!(within.unwrap(), TurnOutcome::Success);
    let unlimited = steps_turn("steps-none", &steps_app_server(60, false), None).await;
    assert_eq!(unlimited.unwrap(), TurnOutcome::Success);
    let off = steps_turn(
        "steps-off",
        &steps_app_server(60, false),
        Some(StepLimit::NoLimit),
    )
    .await;
    assert_eq!(off.unwrap(), TurnOutcome::Success);
}

/// Every master backend maps the master step limit to the same stop (PA-9, DEC-141): a
/// Claude master gets `--max-turns`, whose error_max_turns the runtime names with the
/// limit's cause; a Codex master's host counts its steps and ends the turn with that same
/// cause; an rcli master has no tools, so each turn is one step, which the setting says.
/// The match names every backend, so a new one fails to build until it maps the limit.
#[cfg(unix)]
#[tokio::test]
async fn every_master_backend_maps_the_master_step_limit_to_one_stop() {
    use crate::master_session::MasterBackend;
    use crate::master_step_limit::{StepLimit, steps_of_cause};

    let limit = StepLimit::Steps50;
    let cause = limit.cause().unwrap();
    for backend in [
        MasterBackend::Claude,
        MasterBackend::Codex,
        MasterBackend::Rcli,
    ] {
        match backend {
            MasterBackend::Claude => {
                let mut command = Command::new("claude");
                crate::engine::claude_host::limit_steps(&mut command, limit);
                let args: Vec<_> = command.get_args().collect();
                assert_eq!(args, ["--max-turns", "50"]);
            }
            MasterBackend::Codex => {
                let ended = steps_turn("steps-parity", &steps_app_server(50, true), Some(limit))
                    .await
                    .expect_err("a Codex master stops at its step limit");
                assert_eq!(ended.0, cause);
            }
            MasterBackend::Rcli => {}
        }
    }
    assert_eq!(steps_of_cause(&cause), Some(50));
}
