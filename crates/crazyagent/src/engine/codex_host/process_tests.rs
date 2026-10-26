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
    let profile = fixture.lease("codex-group-turn", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("codex", body);
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut command = Command::new(&harness);
    command.current_dir(std::fs::canonicalize(&fixture.project).unwrap());
    let command = sandbox.wrap(&command).unwrap();
    let limits = ResourceBudget {
        wall_time_ms: 20_000,
        ..budget()
    };
    CodexHostProcess::spawn_guarded(command, &limits, Some(profile))
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
