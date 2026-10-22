use super::*;
use crate::engine::EngineError;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Directories {
    root: PathBuf,
    project: PathBuf,
    home: PathBuf,
}

impl Directories {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "crazyagent-claude-host-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let project = root.join("project");
        let home = root.join("profile-home");
        fs::create_dir(&project).unwrap();
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            project,
            home,
        }
    }

    fn command(&self, model: &str) -> Result<std::process::Command, EngineError> {
        launch_command(
            Path::new("/approved/claude"),
            &self.project,
            &self.home,
            model,
        )
    }
}

impl Drop for Directories {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn claude_launch_uses_official_stream_input_and_output_without_prompt_in_argv() {
    let directories = Directories::new();
    let command = directories.command("claude-sonnet-4-6").unwrap();
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(command.get_program(), OsStr::new("/approved/claude"));
    assert_eq!(command.get_current_dir(), Some(directories.project.as_path()));
    assert!(args.contains(&OsStr::new("--print")));
    assert!(args.contains(&OsStr::new("--verbose")));
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--input-format"), OsStr::new("stream-json")]
    }));
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--output-format"), OsStr::new("stream-json")]
    }));
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--model"), OsStr::new("claude-sonnet-4-6")]
    }));
    assert!(!args.contains(&OsStr::new("--dangerously-skip-permissions")));
    assert!(!args.contains(&OsStr::new("--allow-dangerously-skip-permissions")));
    assert!(!args.contains(&OsStr::new("bypassPermissions")));
}

#[test]
fn claude_conversation_disables_native_tools_and_ambient_mcp_and_settings() {
    let directories = Directories::new();
    let command = directories.command("claude-sonnet-4-6").unwrap();
    let args: Vec<_> = command.get_args().collect();
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--tools"), OsStr::new("")]
    }));
    assert!(args.contains(&OsStr::new("--strict-mcp-config")));
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--mcp-config"), OsStr::new("{\"mcpServers\":{}}")]
    }));
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--setting-sources"), OsStr::new("")]
    }));
    assert!(args.contains(&OsStr::new("--disable-slash-commands")));
    assert!(args.contains(&OsStr::new("--no-session-persistence")));
}

#[test]
fn claude_launch_sets_only_selected_profile_environment() {
    let directories = Directories::new();
    let command = directories.command("claude-sonnet-4-6").unwrap();
    let environment: Vec<_> = command.get_envs().collect();
    assert!(environment.contains(&(
        OsStr::new("HOME"),
        Some(directories.home.as_os_str())
    )));
    let configuration = directories.home.join(".claude");
    assert!(environment.contains(&(
        OsStr::new("CLAUDE_CONFIG_DIR"),
        Some(configuration.as_os_str())
    )));
    assert!(environment.iter().all(|(key, _)| {
        matches!(key.to_str(), Some("HOME" | "CLAUDE_CONFIG_DIR" | "TERM"))
    }));
}

#[test]
fn claude_launch_rejects_empty_option_like_and_unbounded_model_selection() {
    let directories = Directories::new();
    for model in ["", " ", "--model", "model\nother", "model\0other"] {
        assert!(directories.command(model).is_err(), "model {model:?}");
    }
    assert!(directories.command(&"x".repeat(257)).is_err());
}

#[test]
fn claude_launch_rejects_relative_paths_and_shared_profile_home() {
    let directories = Directories::new();
    assert!(launch_command(
        Path::new("claude"),
        &directories.project,
        &directories.home,
        "claude-sonnet-4-6"
    )
    .is_err());
    assert!(launch_command(
        Path::new("/approved/claude"),
        Path::new("project"),
        &directories.home,
        "claude-sonnet-4-6"
    )
    .is_err());
    assert!(launch_command(
        Path::new("/approved/claude"),
        &directories.project,
        Path::new("home"),
        "claude-sonnet-4-6"
    )
    .is_err());
    fs::set_permissions(&directories.home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(directories.command("claude-sonnet-4-6").is_err());
}

#[test]
fn claude_launch_rejects_symlinked_profile_and_missing_project() {
    let directories = Directories::new();
    let alias = directories.root.join("profile-alias");
    std::os::unix::fs::symlink(&directories.home, &alias).unwrap();
    assert!(launch_command(
        Path::new("/approved/claude"),
        &directories.project,
        &alias,
        "claude-sonnet-4-6"
    )
    .is_err());
    fs::remove_dir(&directories.project).unwrap();
    assert!(directories.command("claude-sonnet-4-6").is_err());
}

#[test]
fn claude_process_bounds_reject_zero_and_excessive_resource_limits() {
    let valid = ClaudeHostLimits {
        run_time: Duration::from_secs(120),
        cleanup_time: Duration::from_secs(2),
        stdout_bytes: 262_144,
        stderr_bytes: 65_536,
        events: 4096,
    };
    assert!(valid.validate().is_ok());
    for limits in [
        ClaudeHostLimits { run_time: Duration::ZERO, ..valid },
        ClaudeHostLimits { run_time: Duration::from_secs(601), ..valid },
        ClaudeHostLimits { cleanup_time: Duration::ZERO, ..valid },
        ClaudeHostLimits { cleanup_time: Duration::from_secs(6), ..valid },
        ClaudeHostLimits { stdout_bytes: 0, ..valid },
        ClaudeHostLimits { stdout_bytes: 1_048_577, ..valid },
        ClaudeHostLimits { stderr_bytes: 0, ..valid },
        ClaudeHostLimits { stderr_bytes: 262_145, ..valid },
        ClaudeHostLimits { events: 0, ..valid },
        ClaudeHostLimits { events: 4097, ..valid },
    ] {
        assert!(limits.validate().is_err());
    }
}
