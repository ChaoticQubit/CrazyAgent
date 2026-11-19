use super::CodexHostProcess;
use crate::routing::ResourceBudget;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// V243-W5-LOWS LOW-2: std offers no non-blocking anonymous pipe on Windows, so a Codex host
/// start there is refused as `codex_host_unavailable` once its child runs, and custody kills
/// and reaps that child before the refusal returns. The child is a copy of the system
/// `cmd.exe` looping until killed; Windows refuses to delete an image while it runs, so the
/// copy going at once shows no child outlived the refusal.
#[tokio::test]
async fn a_windows_codex_host_start_is_refused_and_leaves_no_child_running() {
    let root = tempfile::tempdir().unwrap();
    let system_root = std::env::var_os("SystemRoot").unwrap();
    let shell = PathBuf::from(&system_root).join("System32").join("cmd.exe");
    let program = root.path().join("codex-host-refused.exe");
    std::fs::copy(&shell, &program).unwrap();
    let mut command = Command::new(&program);
    command
        .args(["/d", "/c", "for /l %i in (0,0,1) do @rem"])
        .current_dir(root.path())
        .env("SystemRoot", &system_root);
    let budget = ResourceBudget {
        output_bytes: 1024,
        wall_time_ms: 10_000,
        ..ResourceBudget::unit()
    };

    let started = Instant::now();
    let refused = match CodexHostProcess::spawn(command, &budget).await {
        Ok(_) => panic!("a Windows Codex host start was not refused"),
        Err(refused) => refused,
    };

    assert_eq!(refused.0, "codex_host_unavailable");
    assert!(started.elapsed() < Duration::from_secs(10));
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::remove_file(&program).is_err() {
        assert!(
            Instant::now() < deadline,
            "the refused Codex host child still runs"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!program.exists());
}
