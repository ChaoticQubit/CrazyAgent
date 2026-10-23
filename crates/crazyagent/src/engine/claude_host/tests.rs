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
            None,
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
    assert!(environment.contains(&(
        OsStr::new("CLAUDE_CONFIG_DIR"),
        Some(directories.home.as_os_str())
    )));
    assert!(environment.contains(&(
        OsStr::new("CLAUDE_SECURESTORAGE_CONFIG_DIR"),
        Some(directories.home.as_os_str())
    )));
    assert!(environment.iter().all(|(key, _)| {
        matches!(
            key.to_str(),
            Some("HOME" | "CLAUDE_CONFIG_DIR" | "CLAUDE_SECURESTORAGE_CONFIG_DIR" | "TERM")
        )
    }));
}

#[test]
fn claude_launch_lets_the_signed_in_account_choose_its_default_model() {
    let directories = Directories::new();
    let command = directories.command("native-default").unwrap();
    let args: Vec<_> = command.get_args().collect();
    assert!(!args.contains(&OsStr::new("--model")));
    assert!(!args.contains(&OsStr::new("native-default")));
    assert!(args.windows(2).any(|pair| {
        pair == [OsStr::new("--tools"), OsStr::new("")]
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
        "claude-sonnet-4-6",
        None
    )
    .is_err());
    assert!(launch_command(
        Path::new("/approved/claude"),
        Path::new("project"),
        &directories.home,
        "claude-sonnet-4-6",
        None
    )
    .is_err());
    assert!(launch_command(
        Path::new("/approved/claude"),
        &directories.project,
        Path::new("home"),
        "claude-sonnet-4-6",
        None
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
        "claude-sonnet-4-6",
        None
    )
    .is_err());
    fs::remove_dir(&directories.project).unwrap();
    assert!(directories.command("claude-sonnet-4-6").is_err());
}

#[test]
fn claude_launch_appends_only_a_private_engine_instruction_file() {
    let directories = Directories::new();
    let plain_command = directories.command("native-default").unwrap();
    let plain: Vec<_> = plain_command.get_args().collect();
    assert!(!plain.contains(&OsStr::new("--append-system-prompt-file")));
    let file = directories.root.join("instructions.md");
    fs::write(&file, "# Project context from CrazyAgent\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let launch = |path: &Path| {
        launch_command(
            Path::new("/approved/claude"),
            &directories.project,
            &directories.home,
            "native-default",
            Some(path),
        )
    };

    let command = launch(file.as_path()).unwrap();
    let args: Vec<_> = command.get_args().collect();
    assert_eq!(args.len(), plain.len() + 2);
    assert_eq!(&args[..plain.len()], &plain[..]);
    assert_eq!(
        &args[plain.len()..],
        &[OsStr::new("--append-system-prompt-file"), file.as_os_str()]
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == [OsStr::new("--tools"), OsStr::new("")])
    );
    assert!(
        args.windows(2)
            .any(|pair| pair == [OsStr::new("--max-turns"), OsStr::new("1")])
    );

    assert!(launch(Path::new("instructions.md")).is_err());
    assert!(launch(directories.root.join("missing.md").as_path()).is_err());
    let alias = directories.root.join("alias.md");
    std::os::unix::fs::symlink(&file, &alias).unwrap();
    assert!(launch(alias.as_path()).is_err());
    let linked = directories.root.join("linked.md");
    fs::hard_link(&file, &linked).unwrap();
    assert!(launch(file.as_path()).is_err());
    fs::remove_file(&linked).unwrap();
    assert!(launch(file.as_path()).is_ok());
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(launch(file.as_path()).is_err());
    let oversized = directories.root.join("oversized.md");
    fs::write(&oversized, "x".repeat(64 * 1024 + 1)).unwrap();
    fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(launch(oversized.as_path()).is_err());
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

fn installed_claude() -> PathBuf {
    let entries = crate::harnesses::catalog(&crate::harnesses::Search::from_environment());
    let entry = entries
        .into_iter()
        .find(|entry| entry.id == "claude-code")
        .unwrap();
    fs::canonicalize(entry.executable.expect("installed Claude Code")).unwrap()
}

#[tokio::test]
async fn real_claude_master_host_without_sign_in_reports_authentication_failure() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
    use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
    use crate::engine::owner::{HostStamp, NativeProfileBinding};
    use crate::protocol::{ConfigId, Cursor, SessionId};

    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let root = tempfile::Builder::new()
        .prefix("ca-claude-master-")
        .tempdir_in("/private/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let database = root.path().join("engine.db");
    let sqlite = rusqlite::Connection::open(&database).unwrap();
    sqlite.execute_batch("PRAGMA user_version=1").unwrap();
    sqlite.close().map_err(|(_, error)| error).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let mut custody =
        NativeProfileCustody::capture(&database, &fs::metadata(&database).unwrap()).unwrap();
    let host = HostStamp {
        db_instance: "a".repeat(64),
        engine_epoch: "b".repeat(64),
    };
    let connection = ModelConnection {
        id: reference("claude-master-connection"),
        provider: reference("anthropic"),
        harness: reference("claude"),
        protocol: reference("native"),
        account_ref: reference("claude-master-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "native-default".into(),
            author: reference("anthropic"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("claude-master-profile"),
        },
    };
    let (custody_id, _) = custody.prepare(&host, &connection).unwrap();
    let binding = NativeProfileBinding {
        database_instance: host.db_instance.clone(),
        engine_epoch: host.engine_epoch.clone(),
        config_id: ConfigId::new("c".repeat(64)).unwrap(),
        source_hash: "d".repeat(64),
        at_revision: Cursor::new(1),
        connection: connection.clone(),
        custody_id,
    };
    let turn = RequestId::new("claude-master-turn").unwrap();
    let profile = custody
        .acquire_use(&host, &binding, &turn, 1, NativeProfileUsePurpose::Execution)
        .unwrap();
    let home = profile.profile_path().unwrap();
    let command = launch_command(&installed_claude(), &project, &home, "native-default", None).unwrap();
    let mut claude = ClaudeHost::spawn(
        command,
        profile,
        NativeAction::Launch {
            session_id: SessionId::new("claude-master-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: reference("claude-master-profile"),
            worktree_lease: RequestId::new("claude-master-lease").unwrap(),
        },
        connection.account_ref.clone(),
        ResourceBudget::unit(),
    )
    .await
    .unwrap();
    claude.start_turn(turn.clone(), "Reply with one word.").await.unwrap();
    let mut errors = Vec::new();
    let outcome = loop {
        let event = claude.next_event().await.unwrap();
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::TurnFinished { outcome, .. } => {
                assert_eq!(event.request_id.as_ref(), Some(&turn));
                break outcome;
            }
            _ => {}
        }
    };
    assert_eq!(errors, ["authentication_failed"]);
    assert_ne!(outcome, TurnOutcome::Success);
    claude.close().await.unwrap();
    assert!(!home.join(".credentials.json").exists());
}
