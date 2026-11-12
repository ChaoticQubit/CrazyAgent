#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

/// The real Codex executable the engine runs, from `CRAZYAGENT_CODEX` or Homebrew.
/// Input: the environment. Output: its canonical path, or a BLOCKED panic when missing.
fn codex_executable() -> PathBuf {
    let path = std::env::var_os("CRAZYAGENT_CODEX")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/codex"))
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: the real codex executable is not installed"));
    assert!(
        path.is_file(),
        "BLOCKED: the real codex executable is not a file"
    );
    path
}

#[test]
#[ignore = "live qualification: the owner finishes a ChatGPT sign-in in the browser"]
fn v1_tui_runs_from_first_run_to_research_and_a_master_reply_on_a_managed_codex_account() {
    let codex = codex_executable();
    let output = crazyagent::spawn_gate::output(
        Command::new("python3")
            .env_remove("PYTHONOPTIMIZE")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/qualification/tui_v1_flow.py"
            ))
            .arg(env!("CARGO_BIN_EXE_crazyagent"))
            .arg(&codex)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("V1 TUI flow passed"), "{stdout}");
}
