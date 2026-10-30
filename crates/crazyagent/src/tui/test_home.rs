use crate::client_controller::OwnerController;
use crate::engine::owner::{OwnerResponse, OwnerResult};
use std::os::unix::fs::PermissionsExt;

/// Sets a fresh projects home beside the engine root, never inside it.
/// Input: an attached owner controller and the mutation generation.
/// Output: the home folder, kept alive by the caller; panics unless the
/// engine applied it.
pub(in crate::tui) async fn projects_home(
    controller: &mut OwnerController,
    generation: u64,
) -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("ca-tui-home-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::fs::canonicalize(home.path()).unwrap();
    let response = controller
        .set_projects_home(path.to_str().unwrap().into(), generation)
        .await
        .unwrap();
    assert!(
        matches!(
            &response,
            OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied
        ),
        "{response:?}"
    );
    home
}

/// Tells a test pump that its client is still attaching to the engine, so a pause in the
/// traffic is not yet the engine settling: under load the attach can outlast the pump's
/// quiet rounds, and a pump that stopped then left every first read unsent.
/// Input: the shell's link. Output: true while it connects or reconnects.
pub(in crate::tui) fn connecting(link: &crate::tui::ctx::Link) -> bool {
    matches!(
        link,
        crate::tui::ctx::Link::Connecting | crate::tui::ctx::Link::Reconnecting
    )
}

/// How long a TUI engine test lets the engine and its client wait for one answer.
/// Parallel test engines share one disk, so their flushed writes queue behind
/// each other and one write can take seconds; the 1 s default then answers
/// OutcomeUnknown although the engine is healthy.
pub(in crate::tui) const ENGINE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Engine settings for TUI engine tests: the defaults with the owner wait
/// raised to `ENGINE_WAIT`.
/// Input: none. Output: the effective configuration to start the engine with.
pub(in crate::tui) fn engine_config() -> crate::config::EffectiveConfig {
    let config = crate::config::resolve_config_bytes(
        &format!("timeout_ms = {}\n", ENGINE_WAIT.as_millis()),
        "",
    )
    .unwrap();
    assert_eq!(
        u128::from(config.settings.timeout_ms),
        ENGINE_WAIT.as_millis()
    );
    config
}

/// Runs real git in a folder with no system, global or hook config, for tests that build
/// a project checkout. Inputs: the folder and the git arguments. Output: git's standard
/// output; panics when git fails.
pub(in crate::tui) fn git(folder: &std::path::Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(folder)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

/// Makes a folder a git checkout on main with its own identity and a committed README,
/// then writes each instruction file as "<name> rule": the tracked ones committed, the
/// untracked ones left out of git. Inputs: the folder and the two name lists. Output: the
/// tip commit id.
pub(in crate::tui) fn instruction_repo(
    folder: &std::path::Path,
    tracked: &[&str],
    untracked: &[&str],
) -> String {
    git(folder, &["init", "-q", "-b", "main"]);
    git(folder, &["config", "user.name", "Owner"]);
    git(folder, &["config", "user.email", "owner@example.invalid"]);
    std::fs::write(folder.join("README.md"), "notes\n").unwrap();
    git(folder, &["add", "README.md"]);
    for name in tracked {
        std::fs::write(folder.join(name), format!("{name} rule\n")).unwrap();
        git(folder, &["add", name]);
    }
    git(folder, &["commit", "-q", "-m", "start"]);
    for name in untracked {
        std::fs::write(folder.join(name), format!("{name} rule\n")).unwrap();
    }
    git(folder, &["rev-parse", "HEAD"]).trim().to_owned()
}
