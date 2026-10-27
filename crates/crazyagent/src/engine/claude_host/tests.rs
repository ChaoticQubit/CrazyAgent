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
        ClaudeHostLimits {
            run_time: Duration::ZERO,
            ..valid
        },
        ClaudeHostLimits {
            run_time: Duration::from_secs(601),
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
        ClaudeHostLimits { events: 0, ..valid },
        ClaudeHostLimits {
            events: 4097,
            ..valid
        },
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
        ResourceBudget::unit(),
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
    let profile = fixture.lease("claude-group-turn", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("claude", body);
    let command =
        launch_command(&harness, &fixture.project, &home, "native-default", None).unwrap();
    let sandbox = fixture.sandbox(&home, harness.parent().unwrap());
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
        ResourceBudget::unit(),
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
        "#!/bin/sh\necho $$ > leader.pid\n/bin/cat > /dev/null\nexit 1\n",
    )
    .await;
    let ready = fixture.project.join("leader.pid");
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
            MasterTools { engine, drop },
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
            "args": ["team-mcp", drop.to_str().unwrap()],
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
            "--max-turns",
            "24",
        ]
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
        "crazyagent-team": {"type": "stdio", "command": engine, "args": ["team-mcp", drop]},
        "extra": {"type": "stdio", "command": "/bin/sh", "args": []},
    }})
    .to_string();
    let steps = arguments(&on).len() - 1;
    for refused in [
        changed(&on, 12, &widened),
        changed(&on, steps, "99"),
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

#[tokio::test]
async fn a_tools_on_claude_master_starts_with_a_shell_for_its_tools() {
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let profile = fixture.lease("claude-tools-shell", 1).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in(
        "claude",
        "#!/bin/sh\nprintf 'shell=%s\\n' \"$(/usr/bin/printenv SHELL)\" > shell.txt\n/bin/cat > /dev/null\nexit 1\n",
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
        ResourceBudget::unit(),
    )
    .await
    .unwrap();
    let seen = fixture.project.join("shell.txt");
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

/// Runs one turn of the tool-step stand-in through a Claude master host built with tools on
/// or off. Input: the fixture, the lease operation and sequence, and the switch. Output: the
/// event kinds the host delivered, then its error code, or none when the turn finished.
async fn tool_step_turn(
    fixture: &mut crate::engine::master_host_fixture::MasterFixture,
    operation: &str,
    sequence: u64,
    tools: bool,
) -> (Vec<crate::adapters::events::EventKind>, Option<String>) {
    use crate::adapters::events::EventKind;

    let profile = fixture.lease(operation, sequence).unwrap();
    let home = profile.profile_path().unwrap();
    let harness = fixture.stand_in("claude", TOOL_STEP_HARNESS);
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
        ResourceBudget::unit(),
    )
    .await
    .unwrap();
    claude
        .start_turn(RequestId::new("claude-steps-turn").unwrap(), "Write a note")
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
    (kinds, error)
}

#[tokio::test]
async fn a_tools_on_claude_master_passes_its_tool_steps_through_and_a_tools_off_one_refuses_them() {
    use crate::adapters::events::{EventKind, TurnOutcome};
    use crate::engine::master_host_fixture::MasterFixture;

    let mut fixture = MasterFixture::new("claude");
    let (kinds, error) = tool_step_turn(&mut fixture, "claude-steps-on", 1, true).await;
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
    let (kinds, error) = tool_step_turn(&mut fixture, "claude-steps-off", 2, false).await;
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
printf 'closed' > input-closed
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
        ResourceBudget::unit(),
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
        fixture.project.join("input-closed").exists() && claude.diagnostic().is_some(),
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
        ResourceBudget {
            output_bytes: 64 * 1024 * 1024,
            ..ResourceBudget::unit()
        },
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
