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
    assert_eq!(
        command.get_current_dir(),
        Some(directories.project.as_path())
    );
    assert!(args.contains(&OsStr::new("--print")));
    assert!(args.contains(&OsStr::new("--verbose")));
    assert!(
        args.windows(2)
            .any(|pair| { pair == [OsStr::new("--input-format"), OsStr::new("stream-json")] })
    );
    assert!(
        args.windows(2)
            .any(|pair| { pair == [OsStr::new("--output-format"), OsStr::new("stream-json")] })
    );
    assert!(
        args.windows(2)
            .any(|pair| { pair == [OsStr::new("--model"), OsStr::new("claude-sonnet-4-6")] })
    );
    assert!(!args.contains(&OsStr::new("--dangerously-skip-permissions")));
    assert!(!args.contains(&OsStr::new("--allow-dangerously-skip-permissions")));
    assert!(!args.contains(&OsStr::new("bypassPermissions")));
}

#[test]
fn claude_conversation_disables_native_tools_and_ambient_mcp_and_settings() {
    let directories = Directories::new();
    let command = directories.command("claude-sonnet-4-6").unwrap();
    let args: Vec<_> = command.get_args().collect();
    assert!(
        args.windows(2)
            .any(|pair| { pair == [OsStr::new("--tools"), OsStr::new("")] })
    );
    assert!(args.contains(&OsStr::new("--strict-mcp-config")));
    assert!(args.windows(2).any(|pair| {
        pair == [
            OsStr::new("--mcp-config"),
            OsStr::new("{\"mcpServers\":{}}"),
        ]
    }));
    assert!(
        args.windows(2)
            .any(|pair| { pair == [OsStr::new("--setting-sources"), OsStr::new("")] })
    );
    assert!(args.contains(&OsStr::new("--disable-slash-commands")));
    assert!(args.contains(&OsStr::new("--no-session-persistence")));
}

#[test]
fn claude_launch_sets_only_selected_profile_environment() {
    let directories = Directories::new();
    let command = directories.command("claude-sonnet-4-6").unwrap();
    let environment: Vec<_> = command.get_envs().collect();
    assert!(environment.contains(&(OsStr::new("HOME"), Some(directories.home.as_os_str()))));
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
    assert!(
        args.windows(2)
            .any(|pair| { pair == [OsStr::new("--tools"), OsStr::new("")] })
    );
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
    assert!(
        launch_command(
            Path::new("claude"),
            &directories.project,
            &directories.home,
            "claude-sonnet-4-6",
            None
        )
        .is_err()
    );
    assert!(
        launch_command(
            Path::new("/approved/claude"),
            Path::new("project"),
            &directories.home,
            "claude-sonnet-4-6",
            None
        )
        .is_err()
    );
    assert!(
        launch_command(
            Path::new("/approved/claude"),
            &directories.project,
            Path::new("home"),
            "claude-sonnet-4-6",
            None
        )
        .is_err()
    );
    fs::set_permissions(&directories.home, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(directories.command("claude-sonnet-4-6").is_err());
}

#[test]
fn claude_launch_rejects_symlinked_profile_and_missing_project() {
    let directories = Directories::new();
    let alias = directories.root.join("profile-alias");
    std::os::unix::fs::symlink(&directories.home, &alias).unwrap();
    assert!(
        launch_command(
            Path::new("/approved/claude"),
            &directories.project,
            &alias,
            "claude-sonnet-4-6",
            None
        )
        .is_err()
    );
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
    let limit = usize::try_from(INSTRUCTIONS_BYTES_MAX).unwrap();
    let largest = directories.root.join("largest.md");
    fs::write(&largest, "x".repeat(limit)).unwrap();
    fs::set_permissions(&largest, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(launch(largest.as_path()).is_ok());
    let oversized = directories.root.join("oversized.md");
    fs::write(&oversized, "x".repeat(limit + 1)).unwrap();
    fs::set_permissions(&oversized, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(launch(oversized.as_path()).is_err());
}

#[test]
fn claude_process_bounds_reject_zero_and_excessive_resource_limits() {
    let valid = ClaudeHostLimits {
        run_time: Some(Duration::from_secs(120)),
        cleanup_time: Duration::from_secs(2),
        stdout_bytes: 262_144,
        stderr_bytes: 65_536,
    };
    assert!(valid.validate().is_ok());
    for limits in [
        ClaudeHostLimits {
            run_time: Some(Duration::ZERO),
            ..valid
        },
        ClaudeHostLimits {
            run_time: Some(Duration::from_secs(3601)),
            ..valid
        },
        ClaudeHostLimits {
            cleanup_time: Duration::ZERO,
            ..valid
        },
        ClaudeHostLimits {
            cleanup_time: Duration::from_secs(6),
            ..valid
        },
        ClaudeHostLimits {
            stdout_bytes: 0,
            ..valid
        },
        ClaudeHostLimits {
            stdout_bytes: STDOUT_BYTES_MAX + 1,
            ..valid
        },
        ClaudeHostLimits {
            stderr_bytes: 0,
            ..valid
        },
        ClaudeHostLimits {
            stderr_bytes: 262_145,
            ..valid
        },
    ] {
        assert!(limits.validate().is_err());
    }
    let unclocked = ClaudeHostLimits {
        run_time: None,
        ..valid
    };
    assert!(unclocked.validate().is_ok(), "no master time limit");
    for limit in crate::master_time_limit::TimeLimit::ALL {
        let limits = ClaudeHostLimits::from_budget(&ResourceBudget::unit(), limit.run_time());
        let limits = limits.unwrap_or_else(|_| panic!("{limit:?} is a host clock"));
        assert_eq!(limits.run_time, limit.run_time());
        assert_eq!(limits.run_time.is_none(), limit.minutes().is_none());
    }
    let long = ResourceBudget {
        wall_time_ms: 86_400_000,
        ..ResourceBudget::unit()
    };
    for (budget, clock) in [
        (ResourceBudget::unit(), Some(Duration::from_secs(3600))),
        (long, Some(Duration::from_secs(600))),
    ] {
        let limits = ClaudeHostLimits::from_budget(&budget, clock).unwrap();
        assert_eq!(
            limits.run_time, clock,
            "the clock is the setting, not the budget"
        );
    }
    for refused in [Duration::ZERO, Duration::from_secs(3601)] {
        assert!(ClaudeHostLimits::from_budget(&ResourceBudget::unit(), Some(refused)).is_err());
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

#[test]
fn a_real_tools_on_claude_master_keeps_its_default_tools_but_no_built_in_sub_agent() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let directories = Directories::new();
    let drop = directories
        .root
        .join("tmp")
        .join(crate::team::tool::DROP_FILE);
    let mut command = tools_launch_command(
        &installed_claude(),
        &directories.project,
        &directories.home,
        "native-default",
        None,
        MasterTools {
            engine: Path::new("/usr/bin/true"),
            drop: &drop,
            mailbox: &drop.with_file_name(crate::team::tool::MAILBOX_FOLDER),
            off: &[],
        },
    )
    .unwrap();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::spawn_gate::spawn(&mut command).unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n")
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (send, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let tools: Vec<String> = loop {
        let line = lines
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("Claude Code names its tools when its session starts");
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if event["type"] == "system" && event["subtype"] == "init" {
            break serde_json::from_value(event["tools"].clone()).unwrap();
        }
    };
    let _ = child.kill();
    child.wait().unwrap();
    for kept in ["Bash", "Edit", "Read", "Write"] {
        assert!(tools.iter().any(|tool| tool == kept), "{kept}: {tools:?}");
    }
    for built_in in ["Agent", "Task", "Workflow", "TeamCreate"] {
        assert!(
            !tools.iter().any(|tool| tool == built_in),
            "{built_in}: {tools:?}"
        );
    }
}

#[tokio::test]
async fn real_claude_master_host_without_sign_in_reports_it_is_not_signed_in() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
    use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
    use crate::engine::owner::{HostStamp, NativeProfileBinding};
    use crate::protocol::{ConfigId, Cursor, SessionId};
    use crate::sandbox::{InstalledSandbox, ProjectSandbox, SandboxNetwork, SandboxRemote};

    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let root = tempfile::Builder::new()
        .prefix("ca-claude-master-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
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
        .acquire_use(
            &host,
            &binding,
            &turn,
            1,
            NativeProfileUsePurpose::Execution,
        )
        .unwrap();
    let home = profile.profile_path().unwrap();
    let command =
        launch_command(&installed_claude(), &project, &home, "native-default", None).unwrap();
    let tmp = root.path().join("agent-tmp");
    fs::create_dir(&tmp).unwrap();
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
    let sandbox = ProjectSandbox::new(
        &fs::canonicalize(&project).unwrap(),
        &fs::canonicalize(&home).unwrap(),
        &fs::canonicalize(&tmp).unwrap(),
        SandboxNetwork::Only(vec![SandboxRemote::AnyHost(443)]),
    )
    .unwrap()
    .allow_read(installed_claude().parent().unwrap())
    .unwrap();
    let sandbox = InstalledSandbox::install(sandbox, &root.path().join("master.sb")).unwrap();
    assert!(
        !sandbox
            .sandbox()
            .writable_roots()
            .any(|writable| sandbox.profile().starts_with(writable))
    );
    let mut claude = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home.clone(),
        NativeAction::Launch {
            session_id: SessionId::new("claude-master-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: reference("claude-master-profile"),
            worktree_lease: RequestId::new("claude-master-lease").unwrap(),
        },
        connection.account_ref.clone(),
        crate::protocol::ProjectId::new("claude-master-project").unwrap(),
        Some(Duration::from_secs(60)),
        ResourceBudget::unit(),
        crate::master_frame_limit::FrameSize::default(),
    )
    .await
    .unwrap();
    claude
        .start_turn(turn.clone(), "Reply with one word.")
        .await
        .unwrap();
    let mut errors = Vec::new();
    let mut texts = Vec::new();
    let outcome = loop {
        let event = claude.next_event().await.unwrap();
        match event.kind {
            EventKind::AssistantError { code } => errors.push(code),
            EventKind::AssistantText { text, .. } => texts.push(text),
            EventKind::TurnFinished { outcome, .. } => {
                assert_eq!(event.request_id.as_ref(), Some(&turn));
                break outcome;
            }
            _ => {}
        }
    };
    assert!(
        texts.iter().any(|text| text.starts_with("Not logged in")),
        "{texts:?}"
    );
    assert_eq!(errors, ["not_signed_in"]);
    assert_ne!(outcome, TurnOutcome::Success);
    claude.close().await.unwrap();
    assert!(!home.join(".credentials.json").exists());
}

async fn stand_in_master(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    body: &str,
) -> ClaudeHost {
    stand_in_master_at(fixture, body, false).await
}

/// Starts a stand-in Claude Code master host running `body`, at Full access when
/// `unconfined`. Takes the fixture, the script body and the access. Returns the host.
async fn stand_in_master_at(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    body: &str,
    unconfined: bool,
) -> ClaudeHost {
    let clock = Some(Duration::from_secs(60));
    stand_in_master_under(fixture, body, unconfined, clock).await
}

/// Starts a stand-in Claude Code master host running `body` under a wall clock, none for
/// no clock. Takes the fixture, the script body, the access and the clock. Returns the
/// host.
async fn stand_in_master_under(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    body: &str,
    unconfined: bool,
    clock: Option<Duration>,
) -> ClaudeHost {
    let frames = crate::master_frame_limit::FrameSize::default();
    stand_in_master_framed(fixture, body, unconfined, clock, frames).await
}

/// Starts a stand-in Claude Code master host running `body` under a wall clock and a
/// master frame size limit. Takes the fixture, the script body, the access, the clock and
/// the frame size. Returns the host.
async fn stand_in_master_framed(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    body: &str,
    unconfined: bool,
    clock: Option<Duration>,
    frames: crate::master_frame_limit::FrameSize,
) -> ClaudeHost {
    let profile = fixture.lease("claude-group-turn", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("claude", body);
    let command =
        launch_command(&harness, &fixture.project, &home, "native-default", None).unwrap();
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let sandbox = if unconfined {
        sandbox.unconfined()
    } else {
        sandbox
    };
    ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: crate::protocol::SessionId::new("claude-group-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: CredentialRef::new("group-master-profile").unwrap(),
            worktree_lease: RequestId::new("claude-group-lease").unwrap(),
        },
        fixture.connection.account_ref.clone(),
        fixture.project_id.clone(),
        clock,
        ResourceBudget::unit(),
        frames,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn cancelling_a_claude_master_kills_its_whole_group_and_releases_the_lease() {
    use crate::engine::master_host_fixture::{
        HarnessProcesses, MasterFixture, PIPE_HOLDING_HARNESS,
    };

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master(&mut fixture, PIPE_HOLDING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.leader)
    );
    assert_eq!(
        fixture
            .sign_in("claude-group-busy", 2)
            .err()
            .map(|error| error.0)
            .as_deref(),
        Some("native_profile_in_use")
    );
    let started = std::time::Instant::now();
    let cancelled = claude.cancel().await;
    let elapsed = started.elapsed();
    assert!(cancelled.is_ok(), "{cancelled:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_gone();
    assert!(fixture.sign_in("claude-group-next", 3).is_ok());
}

#[cfg(not(target_os = "macos"))]
#[tokio::test]
async fn a_claude_master_process_that_left_its_group_cannot_hold_cleanup_or_the_lease() {
    use crate::engine::master_host_fixture::{
        GROUP_LEAVING_HARNESS, HarnessProcesses, MasterFixture,
    };

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master(&mut fixture, GROUP_LEAVING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.descendant)
    );
    let started = std::time::Instant::now();
    let cancelled = claude.cancel().await;
    let elapsed = started.elapsed();
    assert_eq!(
        cancelled.map_err(|error| error.0),
        Err("claude_host_cleanup_uncertain".to_owned()),
        "after {elapsed:?}"
    );
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_harness_gone();
    assert_eq!(
        rustix::process::test_kill_process(processes.descendant),
        Ok(())
    );
    assert!(fixture.sign_in("claude-group-next", 3).is_ok());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_claude_master_process_that_left_its_group_is_stopped_with_the_master() {
    use crate::engine::master_host_fixture::{
        GROUP_LEAVING_HARNESS, HarnessProcesses, MasterFixture,
    };

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master(&mut fixture, GROUP_LEAVING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.descendant)
    );
    let started = std::time::Instant::now();
    let cancelled = claude.cancel().await;
    let elapsed = started.elapsed();
    assert!(cancelled.is_ok(), "{cancelled:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_gone();
    assert!(fixture.sign_in("claude-group-next", 3).is_ok());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_claude_master_whose_group_keeps_an_unreaped_process_still_ends_and_releases_the_lease() {
    use crate::engine::master_host_fixture::{
        HarnessProcesses, MasterFixture, ZOMBIE_HOLDING_HARNESS,
    };

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master(&mut fixture, ZOMBIE_HOLDING_HARNESS).await;
    let processes = HarnessProcesses::wait(&fixture.project);
    assert_eq!(
        rustix::process::getpgid(Some(processes.descendant)),
        Ok(processes.descendant)
    );
    let cancelled = claude.cancel().await;
    assert_eq!(
        cancelled.map_err(|error| error.0),
        Err("claude_host_cleanup_uncertain".to_owned())
    );
    let started = std::time::Instant::now();
    while fixture.sign_in("claude-group-next", 3).is_err() {
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

#[tokio::test]
async fn a_claude_master_whose_harness_it_can_never_reap_still_ends_and_releases_the_lease() {
    use crate::engine::master_host_fixture::{MasterFixture, reap_before_the_host};

    for _ in 0..5 {
        let mut fixture = MasterFixture::new("claude");
        let mut claude = stand_in_master(
            &mut fixture,
            "#!/bin/sh\necho $$ > run/leader.pid\nexec /bin/sleep 30\n",
        )
        .await;
        if !reap_before_the_host(&fixture.project) {
            continue;
        }
        let started = std::time::Instant::now();
        while fixture.sign_in("claude-unreaped-next", 3).is_err() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the profile lease was never released"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(claude.cancel().await.is_err());
        return;
    }
    panic!("the host reaped its harness first on every attempt");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn cancelling_a_claude_master_stops_what_its_shells_detached() {
    use crate::engine::master_host_fixture::{DETACHING_HARNESS, DetachedProcesses, MasterFixture};

    let mut fixture = MasterFixture::new("claude");
    let body = format!("{DETACHING_HARNESS}exec /bin/sleep 30\n");
    let mut claude = stand_in_master(&mut fixture, &body).await;
    let processes = DetachedProcesses::wait(&fixture.project);
    let started = std::time::Instant::now();
    let cancelled = claude.cancel().await;
    let elapsed = started.elapsed();
    assert!(cancelled.is_ok(), "{cancelled:?} after {elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    processes.assert_gone();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn cancelling_a_full_access_claude_master_names_what_it_left_running_and_keeps_the_lease_until_it_ends()
 {
    use crate::engine::master_host_fixture::{DETACHING_HARNESS, DetachedProcesses, MasterFixture};
    use crate::harnesses::leftovers;

    let mut fixture = MasterFixture::new("claude");
    let body = format!("{DETACHING_HARNESS}exec /bin/sleep 30\n");
    let mut claude = stand_in_master_at(&mut fixture, &body, true).await;
    let processes = DetachedProcesses::wait(&fixture.project);
    let cancelled = claude.cancel().await;
    assert_eq!(
        cancelled.map_err(|error| error.0),
        Err(leftovers::LEFT.to_owned()),
        "a Full access master turn ended clean while what it started outside its terminal ran"
    );
    let named = format!("{} perl", processes.session.as_raw_nonzero());
    let reports = leftovers::held_by(&fixture.connection.id);
    assert!(
        reports.iter().any(|report| report.contains(&named)),
        "{reports:?}"
    );
    assert_eq!(
        fixture
            .sign_in("claude-full-busy", 2)
            .err()
            .map(|error| error.0)
            .as_deref(),
        Some("native_profile_in_use"),
        "the account lease was released while the master's leftover ran"
    );
    let _ = rustix::process::kill_process(processes.session, rustix::process::Signal::KILL);
    let started = std::time::Instant::now();
    while fixture.sign_in("claude-full-next", 3).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the account lease stayed held after the leftover ended"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// SETSID-L2: a Full access master's process that left its session and lost its parent
/// before the cancel, running a program whose environment macOS hides, is still named: the
/// follow from the start kept it.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn cancelling_a_full_access_claude_master_names_what_left_its_session_and_parent() {
    use crate::engine::master_host_fixture::{MasterFixture, ORPHANING_HARNESS, Orphan};
    use crate::harnesses::leftovers;

    let mut fixture = MasterFixture::new("claude");
    let body = format!("{ORPHANING_HARNESS}exec /bin/sleep 30\n");
    let mut claude = stand_in_master_at(&mut fixture, &body, true).await;
    let orphan = Orphan::wait(&fixture.project);
    let cancelled = claude.cancel().await;
    assert_eq!(
        cancelled.map_err(|error| error.0),
        Err(leftovers::LEFT.to_owned()),
        "a Full access master turn ended clean while the orphan it started ran"
    );
    let named = format!("{} perl", orphan.0.as_raw_nonzero());
    let reports = leftovers::held_by(&fixture.connection.id);
    assert!(
        reports.iter().any(|report| report.contains(&named)),
        "{reports:?}"
    );
    drop(orphan);
    let started = std::time::Instant::now();
    while fixture.sign_in("claude-orphan-next", 3).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the account lease stayed held after the orphan ended"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// L1 r1 security L1: a Claude host turn's leftovers follow its project's setting and show
/// in that project's owner view. Under Ask the hold waits with the account kept and nothing
/// signalled; the owner's Keep lets the account go at once while the orphan still runs.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_claude_host_hold_follows_its_projects_ask_then_keep_and_is_listed_for_the_owner() {
    use crate::engine::master_host_fixture::{MasterFixture, ORPHANING_HARNESS, Orphan};
    use crate::harnesses::leftovers::{self, HoldState, LeftoverMode};

    let mut fixture = MasterFixture::new("claude");
    let project = fixture.project_id.as_str().to_owned();
    leftovers::set_mode(&project, LeftoverMode::Ask);
    let body = format!("{ORPHANING_HARNESS}exec /bin/sleep 30\n");
    let mut claude = stand_in_master_at(&mut fixture, &body, true).await;
    let orphan = Orphan::wait(&fixture.project);
    let cancelled = claude.cancel().await;
    assert_eq!(
        cancelled.map_err(|error| error.0),
        Err(leftovers::LEFT.to_owned())
    );
    let listed = leftovers::held_in(&project);
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].who, "a Claude host turn");
    assert_eq!(listed[0].state, HoldState::Held);
    assert!(listed[0].holds_account);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        rustix::process::test_kill_process(orphan.0),
        Ok(()),
        "Ask signalled the orphan"
    );
    assert_eq!(
        fixture
            .sign_in("claude-ask-busy", 2)
            .err()
            .map(|error| error.0)
            .as_deref(),
        Some("native_profile_in_use"),
        "Ask let the account go before the owner chose"
    );
    assert!(leftovers::keep_held(listed[0].key, &project));
    assert!(
        fixture.sign_in("claude-kept-next", 3).is_ok(),
        "Keep held the account"
    );
    let kept = leftovers::held_in(&project);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].state, HoldState::Kept);
    assert!(!kept[0].holds_account);
    assert_eq!(rustix::process::test_kill_process(orphan.0), Ok(()));
    drop(orphan);
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_claude_master_that_ends_on_its_own_leaves_nothing_its_shells_detached() {
    use crate::engine::master_host_fixture::{DETACHING_HARNESS, DetachedProcesses, MasterFixture};

    let mut fixture = MasterFixture::new("claude");
    let body = format!("{DETACHING_HARNESS}/bin/cat > /dev/null\nexit 0\n");
    let mut claude = stand_in_master(&mut fixture, &body).await;
    let processes = DetachedProcesses::wait(&fixture.project);
    let closed = claude.close().await;
    assert!(closed.is_ok(), "{closed:?}");
    processes.assert_gone();
}

#[tokio::test]
async fn closing_a_claude_master_is_not_failed_by_the_exit_its_closed_input_causes() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master(
        &mut fixture,
        "#!/bin/sh\necho $$ > run/leader.pid\n/bin/cat > /dev/null\nexit 1\n",
    )
    .await;
    let ready = fixture.project.join("run/leader.pid");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "stand-in never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let closed = claude.close().await;
    assert!(closed.is_ok(), "{closed:?}");
    assert!(fixture.sign_in("claude-group-next", 3).is_ok());
}

#[test]
fn claude_host_faults_become_plain_codes_a_log_can_name() {
    assert_eq!(
        Fault::Exit(Some(64)).error().0,
        "claude_host_native_exit_64"
    );
    assert_eq!(
        Fault::Exit(None).error().0,
        "claude_host_native_exit_signal"
    );
    assert_eq!(
        Fault::Protocol(ProtocolError::StaleMessage).error().0,
        "claude_host_protocol_stale_message"
    );
    assert_eq!(
        Fault::Protocol(ProtocolError::Disconnected).error().0,
        "claude_host_protocol_disconnected"
    );
}

fn arguments(command: &std::process::Command) -> Vec<String> {
    command
        .get_args()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_claude_master_with_tools_on_gets_the_team_tool_flags_and_only_the_engine_delegate_server() {
    let directories = Directories::new();
    let engine = Path::new("/approved/crazyagent");
    let drop = directories
        .root
        .join("tmp")
        .join(crate::team::tool::DROP_FILE);
    let launch = |engine: &Path, drop: &Path| {
        tools_launch_command(
            Path::new("/approved/claude"),
            &directories.project,
            &directories.home,
            "native-default",
            None,
            MasterTools {
                engine,
                drop,
                mailbox: &drop.with_file_name(crate::team::tool::MAILBOX_FOLDER),
                off: &[],
            },
        )
    };
    let command = launch(engine, &drop).unwrap();
    let args = arguments(&command);
    let config: serde_json::Value = serde_json::from_str(&args[16]).unwrap();
    assert_eq!(
        config,
        serde_json::json!({"mcpServers": {"crazyagent-team": {
            "type": "stdio",
            "command": "/approved/crazyagent",
            "args": [
                "team-mcp",
                drop.to_str().unwrap(),
                drop.with_file_name(crate::team::tool::MAILBOX_FOLDER)
                    .to_str()
                    .unwrap(),
                "wake"
            ],
        }}})
    );
    assert_eq!(
        args,
        [
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--tools",
            "default",
            "--permission-mode",
            "bypassPermissions",
            "--disallowedTools",
            "Agent",
            "Workflow",
            "TeamCreate",
            "--strict-mcp-config",
            "--mcp-config",
            args[16].as_str(),
            "--setting-sources",
            "",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--disable-slash-commands",
            "--no-session-persistence",
        ]
    );
    assert!(
        !args.iter().any(|arg| arg == "--max-turns"),
        "a tools-on master names no step count, so a working turn is not ended at one (OB-5)"
    );
    let environment: Vec<(&str, &str)> = command
        .get_envs()
        .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
        .collect();
    let home = directories.home.to_str().unwrap();
    assert_eq!(
        environment,
        [
            ("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"),
            ("CLAUDE_CONFIG_DIR", home),
            ("CLAUDE_SECURESTORAGE_CONFIG_DIR", home),
            ("HOME", home),
            ("SHELL", "/bin/zsh"),
            ("TERM", "dumb"),
        ]
    );
    assert_eq!(
        arguments(&directories.command("native-default").unwrap()),
        [
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--tools",
            "",
            "--system-prompt",
            crate::harnesses::launch::CLAUDE_NO_TOOLS_PROMPT,
            "--strict-mcp-config",
            "--mcp-config",
            "{\"mcpServers\":{}}",
            "--setting-sources",
            "",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--disable-slash-commands",
            "--no-session-persistence",
            "--max-turns",
            "1",
        ]
    );
    let control = directories.root.join("drop\nfile.jsonl");
    for (engine, drop) in [
        (Path::new("crazyagent"), drop.as_path()),
        (engine, Path::new("crazyagent-delegations.jsonl")),
        (engine, control.as_path()),
    ] {
        assert_eq!(
            launch(engine, drop).err().map(|error| error.0).as_deref(),
            Some("claude_host_invalid_launch"),
            "{engine:?} {drop:?}"
        );
    }
}

#[test]
fn the_host_runs_a_tools_on_master_only_as_the_engine_builds_it() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let profile = fixture.lease("claude-tools-validate", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let engine = fixture.root.path().join("crazyagent");
    let drop = home.join(crate::team::tool::DROP_FILE);
    let tools = MasterTools {
        engine: &engine,
        drop: &drop,
        mailbox: &drop.with_file_name(crate::team::tool::MAILBOX_FOLDER),
        off: &[],
    };
    let on = tools_launch_command(
        Path::new("/approved/claude"),
        &fixture.project,
        &home,
        "native-default",
        None,
        tools,
    )
    .unwrap();
    let off = launch_command(
        Path::new("/approved/claude"),
        &fixture.project,
        &home,
        "native-default",
        None,
    )
    .unwrap();
    assert_eq!(validate_command(&on, &profile, &home).ok(), Some(true));
    assert_eq!(validate_command(&off, &profile, &home).ok(), Some(false));
    let changed = |command: &std::process::Command, at: usize, value: &str| {
        let mut args = arguments(command);
        args[at] = value.to_owned();
        let mut changed = std::process::Command::new(command.get_program());
        changed
            .args(args)
            .current_dir(command.get_current_dir().unwrap());
        changed.env_clear();
        for (key, value) in command.get_envs() {
            changed.env(key, value.unwrap());
        }
        changed
    };
    let widened = serde_json::json!({"mcpServers": {
        "crazyagent-team": {"type": "stdio", "command": engine, "args": ["team-mcp", drop, drop.with_file_name(crate::team::tool::MAILBOX_FOLDER), "wake"]},
        "extra": {"type": "stdio", "command": "/bin/sh", "args": []},
    }})
    .to_string();
    let mut capped = changed(&on, 0, "--print");
    capped.args(["--max-turns", "24"]);
    for refused in [
        changed(&on, 12, &widened),
        capped,
        changed(&on, 9, "default"),
        changed(&off, 7, "default"),
    ] {
        assert!(
            matches!(
                validate_command(&refused, &profile, &home),
                Err(Fault::Host(
                    "claude_host_unapproved_command" | "claude_host_invalid_command"
                ))
            ),
            "{:?}",
            arguments(&refused)
        );
    }
}

/// Each master step limit the owner can pick ends the engine's tools-on command with
/// `--max-turns` and its steps, and the host runs it; No limit adds nothing. A step count
/// the owner cannot pick, the flag twice, the flag with no count and a limit on a
/// tools-off command are refused (OB-5b).
#[test]
fn the_host_runs_a_tools_on_master_under_each_step_limit_the_owner_can_pick() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let profile = fixture.lease("claude-step-limit", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let engine = fixture.root.path().join("crazyagent");
    let drop = home.join(crate::team::tool::DROP_FILE);
    let mailbox = drop.with_file_name(crate::team::tool::MAILBOX_FOLDER);
    let built = || {
        let tools = MasterTools {
            engine: &engine,
            drop: &drop,
            mailbox: &mailbox,
            off: &[],
        };
        tools_launch_command(
            Path::new("/approved/claude"),
            &fixture.project,
            &home,
            "native-default",
            None,
            tools,
        )
        .unwrap()
    };
    let refused = |command: &std::process::Command| {
        matches!(
            validate_command(command, &profile, &home),
            Err(Fault::Host(
                "claude_host_unapproved_command" | "claude_host_invalid_command"
            ))
        )
    };
    let plain = arguments(&built());
    assert!(!plain.iter().any(|arg| arg == "--max-turns"));
    for limit in StepLimit::ALL {
        let mut command = built();
        limit_steps(&mut command, limit);
        let args = arguments(&command);
        match limit.steps() {
            Some(steps) => {
                assert_eq!(args.len(), plain.len() + 2, "{limit:?}");
                assert_eq!(
                    args[plain.len()..],
                    ["--max-turns".to_owned(), steps.to_string()]
                );
            }
            None => assert_eq!(args, plain),
        }
        assert_eq!(args[..plain.len()], plain[..], "{limit:?}");
        let ran = validate_command(&command, &profile, &home).ok();
        assert_eq!(ran, Some(true), "{limit:?}");
    }
    for steps in ["", "0", "1", "24", "51", "050", "201", "no_limit"] {
        let mut command = built();
        command.args(["--max-turns", steps]);
        assert!(refused(&command), "{steps:?}");
    }
    let mut twice = built();
    limit_steps(&mut twice, StepLimit::Steps50);
    limit_steps(&mut twice, StepLimit::Steps100);
    assert!(refused(&twice));
    let mut uncounted = built();
    uncounted.arg("--max-turns");
    assert!(refused(&uncounted));
    let mut off = launch_command(
        Path::new("/approved/claude"),
        &fixture.project,
        &home,
        "native-default",
        None,
    )
    .unwrap();
    limit_steps(&mut off, StepLimit::Steps50);
    assert!(refused(&off));
}

#[tokio::test]
async fn a_tools_on_claude_master_starts_with_a_shell_for_its_tools() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let profile = fixture.lease("claude-tools-shell", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in(
        "claude",
        "#!/bin/sh\nprintf 'shell=%s\\n' \"$(/usr/bin/printenv SHELL)\" > run/shell.txt\n/bin/cat > /dev/null\nexit 1\n",
    );
    let engine = fixture.root.path().join("crazyagent");
    let drop = home.join(crate::team::tool::DROP_FILE);
    let command = tools_launch_command(
        &harness,
        &fixture.project,
        &home,
        "native-default",
        None,
        MasterTools {
            engine: &engine,
            drop: &drop,
            mailbox: &drop.with_file_name(crate::team::tool::MAILBOX_FOLDER),
            off: &[],
        },
    )
    .unwrap();
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut claude = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: crate::protocol::SessionId::new("claude-tools-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: CredentialRef::new("tools-master-profile").unwrap(),
            worktree_lease: RequestId::new("claude-tools-lease").unwrap(),
        },
        fixture.connection.account_ref.clone(),
        fixture.project_id.clone(),
        Some(Duration::from_secs(60)),
        ResourceBudget::unit(),
        crate::master_frame_limit::FrameSize::default(),
    )
    .await
    .unwrap();
    let seen = fixture.project.join("run/shell.txt");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while fs::read_to_string(&seen).map_or(true, |text| !text.ends_with('\n')) {
        assert!(
            std::time::Instant::now() < deadline,
            "stand-in never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(fs::read_to_string(&seen).unwrap(), "shell=/bin/zsh\n");
    claude.close().await.unwrap();
}

const TOOL_STEP_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"tool_use","id":"tool-1","name":"Write","input":{"file_path":"note.txt"}}]}}'
printf '%s\n' '{"type":"user","uuid":"step-2","session_id":"native-session","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"written"}]}}'
printf '%s\n' '{"type":"assistant","uuid":"step-3","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-2","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// Runs one turn of a tool-step stand-in through a Claude master host built with tools on
/// or off. Input: the fixture, the lease operation and sequence, the switch and the stand-in's
/// script. Output: the event kinds the host delivered, then its error code, or none when the
/// turn finished.
async fn tool_step_turn(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    operation: &str,
    sequence: u64,
    tools: bool,
    body: &str,
) -> (Vec<crate::adapters::events::EventKind>, Option<String>) {
    held_turn(fixture, (operation, sequence), tools, body, Duration::ZERO).await
}

/// Runs one turn as `tool_step_turn` does, but reads no event for the first `held` of it, so
/// the host's event queue fills while the stand-in writes. Input: as `tool_step_turn`, with
/// the lease operation and sequence as a pair, and the time to hold off. Output: the same.
async fn held_turn(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    (operation, sequence): (&str, u64),
    tools: bool,
    body: &str,
    held: Duration,
) -> (Vec<crate::adapters::events::EventKind>, Option<String>) {
    use crate::adapters::events::EventKind;

    let profile = fixture.lease(operation, sequence).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("claude", body);
    let engine = fixture.root.path().join("crazyagent");
    let drop = home.join(crate::team::tool::DROP_FILE);
    let command = if tools {
        tools_launch_command(
            &harness,
            &fixture.project,
            &home,
            "native-default",
            None,
            MasterTools {
                engine: &engine,
                drop: &drop,
                mailbox: &drop.with_file_name(crate::team::tool::MAILBOX_FOLDER),
                off: &[],
            },
        )
    } else {
        launch_command(&harness, &fixture.project, &home, "native-default", None)
    }
    .unwrap();
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut claude = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: crate::protocol::SessionId::new("claude-steps-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: CredentialRef::new("steps-master-profile").unwrap(),
            worktree_lease: RequestId::new("claude-steps-lease").unwrap(),
        },
        fixture.connection.account_ref.clone(),
        fixture.project_id.clone(),
        Some(Duration::from_secs(60)),
        ResourceBudget::unit(),
        crate::master_frame_limit::FrameSize::default(),
    )
    .await
    .unwrap();
    claude
        .start_turn(RequestId::new("claude-steps-turn").unwrap(), "Write a note")
        .await
        .unwrap();
    tokio::time::sleep(held).await;
    let mut kinds = Vec::new();
    let error = loop {
        match claude.next_event().await {
            Ok(event) => {
                let finished = matches!(event.kind, EventKind::TurnFinished { .. });
                kinds.push(event.kind);
                if finished {
                    break None;
                }
            }
            Err(error) => break Some(error.0),
        }
    };
    let _ = claude.close().await;
    (kinds, error)
}

/// A stand-in Claude Code that writes 300 reply frames at once, more than the 128 events the
/// host's queue to the turn's reader holds, then ends the turn.
const BURST_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
i=0
while [ "$i" -lt 300 ]; do
  i=$((i+1))
  printf '{"type":"assistant","uuid":"said-%s","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-%s","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"line"}]}}\n' "$i" "$i"
done
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"line"}'
/bin/cat > /dev/null
exit 1
"#;

/// A burst of more frames than the host's event queue holds, written while the turn's reader
/// is busy, waits for the reader and is delivered whole (OB-5b): it used to fail the turn
/// with claude_host_event_delivery_failed at the 129th undelivered event.
#[tokio::test]
async fn a_burst_past_the_event_queue_waits_for_the_reader_and_is_not_a_failed_turn() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let (kinds, error) = held_turn(
        &mut fixture,
        ("claude-burst", 1),
        true,
        BURST_HARNESS,
        Duration::from_millis(500),
    )
    .await;
    assert_eq!(error, None, "{} events", kinds.len());
    let said = kinds
        .iter()
        .filter(|kind| matches!(kind, EventKind::AssistantText { text, .. } if text == "line"))
        .count();
    assert_eq!(said, 300, "every frame of the burst reached the reader");
    assert!(
        matches!(
            kinds.last(),
            Some(EventKind::TurnFinished {
                outcome: TurnOutcome::Success,
                ..
            })
        ),
        "{} events",
        kinds.len()
    );
}

/// A stand-in Claude Code whose turn writes 10,000 small frames, each with its own id, 9,500
/// tool answers and 500 reply lines, then ends the turn with DONE.
const MANY_EVENTS_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
i=0
while [ "$i" -lt 10000 ]; do
  i=$((i+1))
  if [ "$((i % 20))" -eq 0 ]; then
    printf '{"type":"assistant","uuid":"said-%s","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-%s","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"line"}]}}\n' "$i" "$i"
  else
    printf '{"type":"user","uuid":"answer-%s","session_id":"native-session"}\n' "$i"
  fi
done
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// A turn of 10,000 small events ends with its reply (OB-5d): no count of events ends a
/// turn. It used to fail with claude_host_event_limit at the 4,097th event, and the protocol
/// refused the 4,097th message id as over capacity. Its frames stay under the stand-in
/// host's 1 MiB of stdout, the bound that remains.
#[tokio::test]
async fn a_turn_of_ten_thousand_small_events_ends_with_its_reply() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let (kinds, error) =
        tool_step_turn(&mut fixture, "claude-many", 1, true, MANY_EVENTS_HARNESS).await;
    assert_eq!(error, None, "{} events", kinds.len());
    let said = kinds
        .iter()
        .filter(|kind| matches!(kind, EventKind::AssistantText { text, .. } if text == "line"))
        .count();
    let answers = kinds
        .iter()
        .filter(|kind| {
            matches!(kind, EventKind::Unsupported { message_type, .. } if message_type == "user")
        })
        .count();
    assert_eq!(said, 500, "every reply line reached the reader");
    assert_eq!(answers, 9_500, "every tool answer reached the reader");
    assert!(kinds.len() > 10_000, "{} events", kinds.len());
    let outcome = match kinds.last() {
        Some(EventKind::TurnFinished { outcome, .. }) => Some(outcome),
        _ => None,
    };
    let reply = match kinds.last() {
        Some(EventKind::TurnFinished { result, .. }) => result.as_deref(),
        _ => None,
    };
    assert_eq!(
        outcome,
        Some(&TurnOutcome::Success),
        "{} events",
        kinds.len()
    );
    assert_eq!(reply, Some("DONE"), "the turn ended with its reply");
}

#[tokio::test]
async fn a_tools_on_claude_master_passes_its_tool_steps_through_and_a_tools_off_one_refuses_them() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let (kinds, error) =
        tool_step_turn(&mut fixture, "claude-steps-on", 1, true, TOOL_STEP_HARNESS).await;
    assert_eq!(error, None, "{kinds:?}");
    assert!(
        kinds.iter().any(|kind| matches!(kind,
            EventKind::ToolUse { name, parent_tool_use_id: None, .. } if name == "Write")),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| matches!(kind,
            EventKind::AssistantText { text, parent_tool_use_id: None, .. } if text == "DONE")),
        "{kinds:?}"
    );
    assert!(
        matches!(
            kinds.last(),
            Some(EventKind::TurnFinished {
                outcome: TurnOutcome::Success,
                ..
            })
        ),
        "{kinds:?}"
    );
    let (kinds, error) = tool_step_turn(
        &mut fixture,
        "claude-steps-off",
        2,
        false,
        TOOL_STEP_HARNESS,
    )
    .await;
    assert_eq!(
        error.as_deref(),
        Some("claude_host_native_tools_unsupported"),
        "{kinds:?}"
    );
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, EventKind::TurnFinished { .. })),
        "{kinds:?}"
    );
}

/// A tools-on master turn whose one tool step runs a child the way a harness runs a tool: its
/// output and errors captured, its input closed. The child writes Claude Code's own
/// rate-limit frame to its output, its errors, the terminal and transcript-like files in the
/// profile home and the project, then the turn ends well.
const FORGING_TOOL_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
frame='{"type":"assistant","uuid":"forged-1","session_id":"native-session","parent_tool_use_id":null,"error":"rate_limit","message":{"id":"forged-message","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"API Error: Rate limit reached"}]}}'
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"forge"}}]}}'
kept="${CLAUDE_CONFIG_DIR:-$HOME}/projects/forged"
ran=$( {
  printf '%s\n' "$frame"
  printf '%s\n' "$frame" >&2
  printf '%s\n' "$frame" 2>/dev/null >/dev/tty
  /bin/mkdir -p "$kept" 2>/dev/null && printf '%s\n' "$frame" >> "$kept/native-session.jsonl"
  printf '%s\n' "$frame" >> run/native-session.jsonl
} </dev/null 2>&1 )
printf '%s\n' '{"type":"user","uuid":"step-2","session_id":"native-session","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ran"}]}}'
printf '%s\n' '{"type":"assistant","uuid":"step-3","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-2","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// The control: the harness itself prints Claude Code's rate-limit frame on its own output,
/// as it does when the provider answers 429, then ends the turn.
const LIMITED_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"limited-1","session_id":"native-session","parent_tool_use_id":null,"error":"rate_limit","message":{"id":"limited-message","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"API Error: Rate limit reached"}]}}'
printf '%s\n' '{"type":"result","subtype":"error_during_execution","uuid":"result-1","session_id":"native-session","is_error":true,"result":"API Error: Rate limit reached"}'
/bin/cat > /dev/null
exit 1
"#;

/// Probe for SECAUDIT-RATELIMIT-FORGE: a configured master turn takes a rate limit only from
/// the frames its harness prints on the pipe the engine holds. A tool child writing the same
/// frame to its output, its errors, the terminal or transcript-like files never ends the turn
/// rate-limited, while the harness printing it does. A master turn's limit therefore keeps
/// pausing its account everywhere (`gateway_rate_limited`).
#[tokio::test]
async fn a_rate_limit_frame_a_master_tool_writes_never_reaches_the_turn_but_the_harness_frame_does()
{
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let (kinds, error) =
        tool_step_turn(&mut fixture, "claude-forged", 1, true, FORGING_TOOL_HARNESS).await;
    let written = fs::read_to_string(fixture.project.join("run/native-session.jsonl")).unwrap();
    assert!(written.contains("\"error\":\"rate_limit\""), "the tool ran");
    assert_eq!(error, None, "{kinds:?}");
    assert!(
        !kinds
            .iter()
            .any(|kind| matches!(kind, EventKind::AssistantError { .. })),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| matches!(kind,
            EventKind::AssistantText { text, parent_tool_use_id: None, .. } if text == "DONE")),
        "{kinds:?}"
    );
    assert!(
        matches!(
            kinds.last(),
            Some(EventKind::TurnFinished {
                outcome: TurnOutcome::Success,
                ..
            })
        ),
        "{kinds:?}"
    );
    let (kinds, error) =
        tool_step_turn(&mut fixture, "claude-limited", 2, true, LIMITED_HARNESS).await;
    assert_eq!(error, None, "{kinds:?}");
    assert!(
        kinds.iter().any(|kind| matches!(kind,
            EventKind::AssistantError { code } if code == "rate_limit")),
        "{kinds:?}"
    );
}

const LATE_RESULT_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '%s\n' '{"type":"assistant","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"sleep 30","run_in_background":true}}]}}'
printf '%s\n' '{"type":"user","uuid":"step-2","session_id":"native-session","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"Command running in background with ID: b1"}]}}'
printf '%s\n' '{"type":"system","subtype":"task_started","uuid":"task-1","session_id":"native-session","task_id":"b1","run_id":"r1","tool_use_id":"tool-1","description":"sleep 30","task_type":"local_bash","is_backgrounded":true}'
printf '%s\n' "{\"type\":\"assistant\",\"uuid\":\"step-3\",\"session_id\":\"native-session\",\"parent_tool_use_id\":null,\"message\":{\"id\":\"message-2\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"observed-model\",\"content\":[{\"type\":\"text\",\"text\":\"STARTED $CLAUDE_CODE_DISABLE_BACKGROUND_TASKS\"}]}}"
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"STARTED"}'
printf '%s\n' '{"type":"system","subtype":"task_notification","uuid":"task-2","session_id":"native-session","task_id":"b1","run_id":"r1","tool_use_id":"tool-1","status":"completed","output_file":"","summary":"sleep 30 finished"}'
printf '%s\n' '{"type":"assistant","uuid":"step-4","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-3","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"The background task finished."}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-2","session_id":"native-session","is_error":false,"result":"The background task finished."}'
long=aaaaaaaaaa
for step in 1 2 3 4 5 6 7 8 9 10 11 12 13; do long=$long$long; done
printf '%s\n' "$long"
printf '%s' '{"type":"system","subtype":"task_'
/bin/cat > /dev/null
printf 'closed' > run/input-closed
exit 0
"#;

#[tokio::test]
async fn a_claude_master_turn_ends_at_its_result_when_a_background_task_continues_the_session() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let profile = fixture.lease("claude-late-turn", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("claude", LATE_RESULT_HARNESS);
    let engine = fixture.root.path().join("crazyagent");
    let drop = home.join(crate::team::tool::DROP_FILE);
    let command = tools_launch_command(
        &harness,
        &fixture.project,
        &home,
        "native-default",
        None,
        MasterTools {
            engine: &engine,
            drop: &drop,
            mailbox: &drop.with_file_name(crate::team::tool::MAILBOX_FOLDER),
            off: &[],
        },
    )
    .unwrap();
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut claude = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: crate::protocol::SessionId::new("claude-late-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: CredentialRef::new("late-master-profile").unwrap(),
            worktree_lease: RequestId::new("claude-late-lease").unwrap(),
        },
        fixture.connection.account_ref.clone(),
        fixture.project_id.clone(),
        Some(Duration::from_secs(60)),
        ResourceBudget::unit(),
        crate::master_frame_limit::FrameSize::default(),
    )
    .await
    .unwrap();
    claude
        .start_turn(RequestId::new("claude-late-turn").unwrap(), "Start a task")
        .await
        .unwrap();
    let mut kinds = Vec::new();
    loop {
        let event = claude.next_event().await.unwrap();
        let finished = matches!(event.kind, EventKind::TurnFinished { .. });
        kinds.push(event.kind);
        if finished {
            break;
        }
    }
    let waited = std::time::Instant::now();
    while claude.diagnostic().is_none() && waited.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        fixture.project.join("run/input-closed").exists() && claude.diagnostic().is_some(),
        "the host closes the harness input at the turn's result and the harness ends by itself"
    );
    let closed = claude.close().await.map_err(|error| error.0);
    assert_eq!(closed, Ok(()), "{kinds:?}");
    let diagnostic = claude.diagnostic().unwrap();
    assert_eq!(diagnostic.fault, None);
    assert_eq!(diagnostic.message, None);
    assert_eq!(diagnostic.late_lines, 5);
    assert_eq!(
        diagnostic.late,
        [
            "system/task_notification",
            "assistant",
            "result/success",
            "unreadable",
            "unreadable"
        ]
    );
    assert_eq!(diagnostic.frames(), ["cut 8158", "unfinished 33"]);
    assert!(
        matches!(
            kinds.last(),
            Some(EventKind::TurnFinished {
                outcome: TurnOutcome::Success,
                ..
            })
        ),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|kind| matches!(kind,
            EventKind::AssistantText { text, .. } if text == "STARTED 1")),
        "the master runs with background tasks off: {kinds:?}"
    );
    assert!(
        !kinds.iter().any(|kind| matches!(kind,
            EventKind::AssistantText { text, .. } if text.contains("background task finished"))),
        "{kinds:?}"
    );
}

const EARLY_ASSISTANT_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"assistant","uuid":"early-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-1","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"sk-ant-oat01-notarealtoken"}]}}'
/bin/cat > /dev/null
exit 0
"#;

#[tokio::test]
async fn a_claude_master_protocol_failure_keeps_the_failing_line_kind_and_frames_without_content() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master(&mut fixture, EARLY_ASSISTANT_HARNESS).await;
    claude
        .start_turn(RequestId::new("claude-early-turn").unwrap(), "Reply")
        .await
        .unwrap();
    let failed = loop {
        match claude.next_event().await {
            Ok(_) => continue,
            Err(error) => break error.0,
        }
    };
    assert_eq!(failed, "claude_host_protocol_invalid_state");
    assert_eq!(
        claude.close().await.map_err(|error| error.0),
        Err("claude_host_protocol_invalid_state".to_owned())
    );
    let diagnostic = claude.diagnostic().unwrap();
    assert_eq!(
        diagnostic.fault.as_deref(),
        Some("claude_host_protocol_invalid_state")
    );
    assert_eq!(diagnostic.message.as_deref(), Some("assistant"));
    assert_eq!(diagnostic.late_lines, 0);
    assert_eq!(diagnostic.frames(), ["assistant 247"]);
}

#[test]
fn a_stream_diagnostic_names_its_last_frames_by_kind_and_size_after_a_cut() {
    let mut diagnostic = diagnostic::StreamDiagnostic::default();
    let stream = b"{\"type\":\"assistant\"}\n".repeat(400);
    for chunk in stream.chunks(4096) {
        diagnostic.keep(chunk);
    }
    let frames = diagnostic.frames();
    assert_eq!(frames.len(), 391);
    assert_eq!(frames[0], "cut 1");
    assert!(frames[1..].iter().all(|frame| frame == "assistant 20"));
    let mut whole = diagnostic::StreamDiagnostic::default();
    whole.keep(b"{\"text\":\"sk-ant-ort01-SECRET_PART-2\"}\n");
    assert_eq!(whole.frames(), ["unreadable 37"]);
    whole.late(b"{\"type\":\"sk-ant-oat01-SECRET\"}");
    whole.late(b"{\"type\":\"system\",\"subtype\":\"Task Notification\"}");
    for _ in 0..20 {
        whole.late(b"{\"type\":\"assistant\"}");
    }
    assert_eq!(whole.late_lines, 22);
    assert_eq!(whole.late.len(), 16);
    assert_eq!(
        whole.late[..3],
        ["unreadable", "system/unreadable", "assistant"]
    );
}

#[test]
fn a_stream_frame_is_named_by_its_protocol_kind_and_never_by_free_text() {
    let mut diagnostic = diagnostic::StreamDiagnostic::default();
    diagnostic.keep(
        b"{\"method\":\"item/agentMessage/delta\",\"params\":{}}\n{\"id\":3,\"result\":{}}\n\
{\"id\":3,\"error\":{\"code\":1}}\n{\"method\":\"ghp_0123456789abcdef\",\"params\":{}}\n\
{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"sk-ant-oat01-SECRET\"}\n",
    );
    assert_eq!(
        diagnostic.frames(),
        [
            "item/agentMessage/delta 48",
            "response 20",
            "error 27",
            "unreadable 45",
            "system/init 34",
            "unreadable 30"
        ]
    );
}

#[test]
fn a_stream_diagnostic_is_appended_private_rotated_and_never_through_a_link() {
    let directories = Directories::new();
    let logs = directories.root.join("logs");
    let entry = serde_json::json!({"fault": "claude_host_protocol_invalid_state", "tail": "x".repeat(10_000)});
    diagnostic::append(&logs, &entry).unwrap();
    let log = logs.join(diagnostic::LOG_FILE);
    assert_eq!(
        fs::metadata(&logs).unwrap().permissions().mode() & 0o7777,
        0o700
    );
    assert_eq!(
        fs::metadata(&log).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    let text = fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 1);
    let written: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    assert_eq!(written, entry);
    for _ in 0..40 {
        diagnostic::append(&logs, &entry).unwrap();
    }
    let rotated = logs.join(format!("{}.1", diagnostic::LOG_FILE));
    assert_eq!(
        fs::metadata(&rotated).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert!(fs::metadata(&log).unwrap().len() <= diagnostic::ROTATE_BYTES);
    assert!(fs::metadata(&rotated).unwrap().len() <= diagnostic::ROTATE_BYTES);
    fs::remove_file(&log).unwrap();
    let elsewhere = directories.root.join("elsewhere");
    fs::write(&elsewhere, "kept").unwrap();
    fs::set_permissions(&elsewhere, fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &log).unwrap();
    assert_eq!(
        diagnostic::append(&logs, &entry).map_err(|error| error.0),
        Err("master_stream_log_unsafe".to_owned())
    );
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "kept");
}

const LARGE_RESULT_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
long=aaaaaaaaaa
for step in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18; do long=$long$long; done
printf '%s' '{"type":"user","uuid":"step-1","session_id":"native-session","parent_tool_use_id":null,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"read"}]},"tool_use_result":{"stdout":"'
printf '%s' "$long"
printf '%s\n' '","stderr":"","interrupted":false,"isImage":false,"noOutputExpected":false}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

#[tokio::test]
async fn a_claude_master_turn_reads_a_tool_result_past_a_mebibyte_within_its_allowance() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let profile = fixture.lease("claude-large-result", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("claude", LARGE_RESULT_HARNESS);
    let command =
        launch_command(&harness, &fixture.project, &home, "native-default", None).unwrap();
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
    let mut claude = ClaudeHost::spawn(
        command,
        sandbox,
        profile,
        home,
        NativeAction::Launch {
            session_id: crate::protocol::SessionId::new("claude-large-session").unwrap(),
            harness: NativeHarness::Claude,
            profile_ref: CredentialRef::new("large-master-profile").unwrap(),
            worktree_lease: RequestId::new("claude-large-lease").unwrap(),
        },
        fixture.connection.account_ref.clone(),
        fixture.project_id.clone(),
        Some(Duration::from_secs(60)),
        ResourceBudget {
            output_bytes: 64 * 1024 * 1024,
            ..ResourceBudget::unit()
        },
        crate::master_frame_limit::FrameSize::default(),
    )
    .await
    .unwrap();
    claude
        .start_turn(RequestId::new("claude-large-turn").unwrap(), "Read the log")
        .await
        .unwrap();
    let mut kinds = Vec::new();
    let error = loop {
        match claude.next_event().await {
            Ok(event) => {
                let finished = matches!(event.kind, EventKind::TurnFinished { .. });
                kinds.push(event.kind);
                if finished {
                    break None;
                }
            }
            Err(error) => break Some(error.0),
        }
    };
    let _ = claude.close().await;
    assert_eq!(error, None, "{kinds:?}");
    assert!(
        matches!(
            kinds.last(),
            Some(EventKind::TurnFinished {
                outcome: TurnOutcome::Success,
                ..
            })
        ),
        "{kinds:?}"
    );
}

/// A stand-in Claude Code that reads a turn, works five seconds without a word and then
/// replies DONE.
const QUIET_THEN_DONE: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
/bin/sleep 5
printf '%s\n' '{"type":"assistant","uuid":"said-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-said","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// Runs one turn of `QUIET_THEN_DONE` on a host under a wall clock. Takes the clock, none
/// for no clock. Returns the error that ended the turn, none for a turn that finished, and
/// how long the turn took.
async fn quiet_turn_under(clock: Option<Duration>) -> (Option<String>, Duration) {
    use crate::adapters::events::EventKind;
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let mut claude = stand_in_master_under(&mut fixture, QUIET_THEN_DONE, false, clock).await;
    let started = std::time::Instant::now();
    let turn = RequestId::new("claude-quiet-turn").unwrap();
    let mut error = claude.start_turn(turn, "Work quietly").await.err();
    while error.is_none() {
        match claude.next_event().await {
            Ok(event) if matches!(event.kind, EventKind::TurnFinished { .. }) => break,
            Ok(_) => {}
            Err(failed) => error = Some(failed),
        }
    }
    let took = started.elapsed();
    let _ = claude.close().await;
    (error.map(|error| error.0), took)
}

/// A master host with no wall clock, the owner's choice of no master time limit, waits for
/// a quiet turn however long it works (OB-5c): the turn that a two second clock ends with
/// `claude_host_deadline` runs its five seconds to the reply under no clock.
#[tokio::test]
async fn a_claude_master_with_no_clock_outlasts_the_quiet_turn_a_clock_ends() {
    let (error, took) = quiet_turn_under(Some(Duration::from_secs(2))).await;
    assert_eq!(error.as_deref(), Some("claude_host_deadline"));
    assert!(
        took < Duration::from_secs(5),
        "the clock ended it: {took:?}"
    );
    let (error, took) = quiet_turn_under(None).await;
    assert_eq!(error, None, "no clock ends the turn");
    assert!(
        took >= Duration::from_secs(5),
        "the turn ran whole: {took:?}"
    );
}

/// A stand-in Claude Code that reads a turn and replies with the byte length of the turn
/// line it read.
const FRAME_SIZE_HARNESS: &str = r#"#!/bin/sh
IFS= read -r turn
printf '%s\n' '{"type":"system","subtype":"init","uuid":"init-1","session_id":"native-session","model":"observed-model","capabilities":[]}'
printf '{"type":"assistant","uuid":"said-1","session_id":"native-session","parent_tool_use_id":null,"message":{"id":"message-said","type":"message","role":"assistant","model":"observed-model","content":[{"type":"text","text":"%s"}]}}\n' "${#turn}"
printf '%s\n' '{"type":"result","subtype":"success","uuid":"result-1","session_id":"native-session","is_error":false,"result":"DONE"}'
/bin/cat > /dev/null
exit 1
"#;

/// A Claude master host takes a turn as large as the owner's master frame size limit
/// allows (F39): a prompt close to the chosen limit reaches the harness whole, past the
/// 64 KiB engine wire size under 256 KiB and 1 MiB, and a prompt past the limit is
/// refused with `claude_host_input_limit` before anything is written.
#[tokio::test]
async fn a_claude_master_takes_a_turn_up_to_its_frame_size_limit() {
    use crate::adapters::events::EventKind;
    use crate::engine::master_host_fixture::MasterFixture;
    use crate::master_frame_limit::FrameSize;

    for limit in FrameSize::ALL {
        let mut fixture = MasterFixture::new("claude");
        let clock = Some(Duration::from_secs(60));
        let mut claude =
            stand_in_master_framed(&mut fixture, FRAME_SIZE_HARNESS, false, clock, limit).await;
        let over = "x".repeat(limit.bytes() + 1);
        let refused = claude
            .start_turn(RequestId::new("claude-frame-over").unwrap(), &over)
            .await
            .unwrap_err();
        assert_eq!(refused.0, "claude_host_input_limit");
        let text = "x".repeat(limit.bytes() - 1024);
        claude
            .start_turn(RequestId::new("claude-frame-turn").unwrap(), &text)
            .await
            .unwrap();
        let mut said = Vec::new();
        loop {
            match claude.next_event().await.unwrap().kind {
                EventKind::AssistantText { text, .. } => said.push(text),
                EventKind::TurnFinished { .. } => break,
                _ => {}
            }
        }
        let line: usize = said.concat().parse().unwrap();
        assert!(
            line > limit.bytes() - 1024 && line < limit.bytes(),
            "{line}"
        );
        let _ = claude.close().await;
    }
}
