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
