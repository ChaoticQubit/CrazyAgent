use std::path::PathBuf;
use std::process::Command;

const VARIABLE: &str = "CRAZYAGENT_MANAGED_CODEX_HOME";

/// The signed-in managed Codex profile home named by `CRAZYAGENT_MANAGED_CODEX_HOME`.
/// Input: the environment. Output: the canonical folder holding the profile's `auth.json`,
/// or a panic with the plain BLOCKED message and the command that unblocks the run.
fn managed_codex_home() -> PathBuf {
    let Some(home) = std::env::var_os(VARIABLE) else {
        panic!(
            "BLOCKED: no signed-in managed Codex account. Sign in with ChatGPT in CrazyAgent, then run: {VARIABLE}=<its managed profile home> cargo test --locked --offline -p crazyagent --test tui_v1_e2e"
        );
    };
    let home = PathBuf::from(home)
        .canonicalize()
        .unwrap_or_else(|_| panic!("BLOCKED: {VARIABLE} does not name an existing folder"));
    assert!(home.is_dir(), "BLOCKED: {VARIABLE} must name a folder");
    assert!(
        home.join("auth.json").is_file(),
        "BLOCKED: {VARIABLE} holds no auth.json, so it is not a signed-in managed Codex profile"
    );
    home
}

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
fn v1_tui_runs_from_first_run_to_research_and_a_master_reply_on_a_managed_codex_account() {
    let home = managed_codex_home();
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
            .arg(&home)
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
