use super::super::Root;
use super::*;
use crate::config::{ModelAuthentication, ModelReference};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};

/// Copies a command with its MCP configuration replaced and arguments appended. Input: the
/// command, the configuration text and the extra arguments. Output: the new command.
fn configured(command: &Command, config: &str, extra: &[&str]) -> Command {
    rewritten(command, |args| {
        let index = args.iter().position(|arg| arg == "--mcp-config").unwrap();
        args[index + 1] = config.to_owned();
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    })
}

fn rewritten(command: &Command, change: impl Fn(&mut Vec<String>)) -> Command {
    let mut args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect();
    change(&mut args);
    let mut copy = Command::new(command.get_program());
    copy.args(args)
        .current_dir(command.get_current_dir().unwrap())
        .env_clear();
    for (key, value) in command.get_envs() {
        copy.env(key, value.unwrap());
    }
    copy
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Runs the system keychain tool against one home only, with secrets passed on stdin.
/// Inputs: the home, the arguments and the optional stdin text. Output: a panic unless
/// the tool succeeds.
fn security(home: &Path, args: &[&str], input: Option<&str>) {
    let mut child = crate::spawn_gate::spawn(
        Command::new("/usr/bin/security")
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    if let Some(input) = input {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "security {args:?} failed");
}

/// Stores a Claude Code sign-in record where Claude Code keeps it: the managed home's
/// own login keychain, under the service named after that home.
/// Inputs: the managed home and the record. Output: the keychain item exists.
fn claude_keychain(home: &Path, record: &Value) {
    let folder = home.join("Library/Keychains");
    std::fs::create_dir_all(&folder).unwrap();
    for private in [home.join("Library"), folder.clone()] {
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let keychain = folder.join("login.keychain-db");
    let path = keychain.to_str().unwrap();
    security(home, &["create-keychain", "-p", "", path], None);
    let service = format!(
        "Claude Code-credentials-{}",
        &format!("{:x}", Sha256::digest(home.to_str().unwrap().as_bytes()))[..8]
    );
    let hex: String = record
        .to_string()
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    security(
        home,
        &["-i"],
        Some(&format!(
            "add-generic-password -a unit -s \"{service}\" -X {hex} \"{path}\"\n"
        )),
    );
}

#[test]
fn a_claude_master_turn_runs_in_its_own_home_with_a_copy_that_cannot_refresh() {
    let temporary = tempfile::Builder::new()
        .prefix("ca-master-turn-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let state = base.join("state");
    private_directory(&state);
    let database = state.join("engine.db");
    let sqlite = rusqlite::Connection::open(&database).unwrap();
    sqlite.execute_batch("PRAGMA user_version=1").unwrap();
    sqlite.close().map_err(|(_, error)| error).unwrap();
    std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut custody =
        NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap()).unwrap();
    let host = HostStamp {
        db_instance: "a".repeat(64),
        engine_epoch: "b".repeat(64),
    };
    let connection = ModelConnection {
        id: reference("unit-master-connection"),
        provider: reference("anthropic"),
        harness: reference("claude"),
        protocol: reference("native"),
        account_ref: reference("unit-master-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "unit-model".into(),
            author: reference("anthropic"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("unit-master-profile"),
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
    let turn = RequestId::new("unit-master-turn").unwrap();
    let profile = custody
        .acquire_use(
            &host,
            &binding,
            &turn,
            1,
            NativeProfileUsePurpose::Execution,
        )
        .unwrap();
    let managed = profile.profile_path().unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let expires = u64::try_from(now_ms).unwrap() + 8 * 3600 * 1000;
    let record = json!({
        "claudeAiOauth": {
            "accessToken": "sk-ant-oat01-unit",
            "refreshToken": "sk-ant-ort01-unit-secret",
            "expiresAt": expires,
            "scopes": ["user:inference"],
            "subscriptionType": "max"
        },
        "mcpOAuth": {"unit-server": {"accessToken": "mcp-unit-secret"}}
    });
    claude_keychain(&managed, &record);
    let runtime = base.join("runtime");
    private_directory(&runtime);
    let root = Root::open(&runtime).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let install = base.join("install");
    private_directory(&install);
    let claude = install.join("claude");
    std::fs::write(&claude, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (runtime_ref, _, _) = allocate_master_capture(&root).unwrap();
    let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &runtime_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let MasterTurnLaunch {
        command: off_command,
        sandbox,
        home,
        credential,
        ..
    } = claude_master_turn(
        &place,
        &profile,
        &claude,
        &connection,
        "Unit master instructions.",
        Duration::from_secs(3600),
        None,
    )
    .unwrap();
    assert_eq!(
        home,
        runtime
            .join(format!("master-runtime-{}", runtime_ref.as_str()))
            .join("home")
    );
    assert_eq!(std::fs::metadata(&home).unwrap().mode() & 0o7777, 0o700);
    let copy = home.join(".credentials.json");
    assert_eq!(std::fs::metadata(&copy).unwrap().mode() & 0o7777, 0o600);
    let written: Value = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
    assert_eq!(
        written,
        json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-unit",
                "refreshToken": null,
                "expiresAt": expires,
                "scopes": ["user:inference"],
                "subscriptionType": "max"
            }
        })
    );
    let rules = sandbox.sandbox();
    let writable: Vec<PathBuf> = rules.writable_roots().cloned().collect();
    assert!(
        writable.iter().any(|root| home.starts_with(root)),
        "{writable:?}"
    );
    assert!(
        writable
            .iter()
            .chain(rules.read_roots())
            .all(|root| !managed.starts_with(root) && !root.starts_with(&managed)),
        "{writable:?} {:?}",
        rules.read_roots()
    );
    drop(credential);
    assert!(!copy.exists());
    assert!(home.is_dir());
    drop(folder);
    assert!(!home.exists());

    let engine = install.join("crazyagent");
    std::fs::write(&engine, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (tools_ref, _, _) = allocate_master_capture(&root).unwrap();
    let tools_folder = MasterRuntimeFolder::new(&root, &tools_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &tools_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record,
    } = claude_master_turn(
        &place,
        &profile,
        &claude,
        &connection,
        "Unit master instructions.",
        Duration::from_secs(3600),
        Some(&engine),
    )
    .unwrap();
    let turn = runtime.join(format!("master-runtime-{}", tools_ref.as_str()));
    let record = turn.join("tmp").join(crate::team::tool::DROP_FILE);
    assert_eq!(tool_record.as_deref(), Some(record.as_path()));
    let args: Vec<&str> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--permission-mode", "bypassPermissions"])
    );
    let config = args[args.iter().position(|arg| *arg == "--mcp-config").unwrap() + 1];
    let config: Value = serde_json::from_str(config).unwrap();
    assert_eq!(
        config,
        json!({"mcpServers": {"crazyagent-team": {
            "type": "stdio",
            "command": engine.to_str().unwrap(),
            "args": ["team-mcp", record.to_str().unwrap()],
        }}})
    );
    let tools = crate::engine::claude_host::MasterTools {
        engine: &engine,
        drop: &record,
    };
    let delegate = |command: &Command, tools| {
        crate::engine::claude_host::require_delegate(command, tools).map_err(|error| error.0)
    };
    let unverified: Result<(), String> = Err("master_delegate_unverified".to_owned());
    assert_eq!(delegate(&command, Some(tools)), Ok(()));
    assert_eq!(delegate(&command, None), unverified);
    assert_eq!(delegate(&off_command, None), Ok(()));
    assert_eq!(delegate(&off_command, Some(tools)), unverified);
    let denied = args
        .iter()
        .position(|arg| *arg == "--disallowedTools")
        .expect("a tools-on master denies Claude Code's own sub-agent tools");
    assert_eq!(
        args[denied + 1..denied + 5],
        ["Agent", "Workflow", "TeamCreate", "--strict-mcp-config"]
    );
    let without = |names: usize| {
        move |args: &mut Vec<String>| {
            args.drain(denied + names..denied + 4);
        }
    };
    for (name, changed) in [
        ("no deny list", rewritten(&command, without(0))),
        ("a shorter deny list", rewritten(&command, without(3))),
        (
            "a longer deny list",
            rewritten(&command, |args| args.insert(denied + 4, "Bash".into())),
        ),
        (
            "a second deny list",
            rewritten(&command, |args| {
                args.extend(["--disallowedTools", "Agent"].map(String::from));
            }),
        ),
        (
            "another spelling",
            rewritten(&command, |args| {
                args.extend(["--disallowed-tools", "Bash"].map(String::from));
            }),
        ),
        (
            "an inline deny list",
            rewritten(&command, |args| args.push("--disallowedTools=Bash".into())),
        ),
    ] {
        assert_eq!(delegate(&changed, Some(tools)), unverified, "{name}");
    }
    let off_denied = rewritten(&off_command, |args| {
        args.extend(["--disallowedTools", "Agent", "Workflow", "TeamCreate"].map(String::from));
    });
    assert_eq!(delegate(&off_denied, None), unverified);
    let link = install.join("crazyagent-link");
    std::os::unix::fs::symlink(&engine, &link).unwrap();
    let hard = install.join("crazyagent-hard");
    std::fs::hard_link(&engine, &hard).unwrap();
    let other_drop = runtime
        .join("master-runtime-other")
        .join("tmp")
        .join(crate::team::tool::DROP_FILE);
    let served = |server: &Path, args: Value| {
        json!({"mcpServers": {"crazyagent-team": {
            "type": "stdio",
            "command": server.to_str().unwrap(),
            "args": args,
        }}})
        .to_string()
    };
    let legit = served(&engine, json!(["team-mcp", record.to_str().unwrap()]));
    assert_eq!(
        delegate(&configured(&command, &legit, &[]), Some(tools)),
        Ok(())
    );
    let mut second = serde_json::from_str::<Value>(&legit).unwrap();
    second["mcpServers"]["owner"] = json!({"type": "stdio", "command": "/usr/bin/true"});
    for (name, config, extra) in [
        (
            "another executable",
            served(
                Path::new("/usr/bin/true"),
                json!(["team-mcp", record.to_str().unwrap()]),
            ),
            vec![],
        ),
        (
            "a link to the engine",
            served(&link, json!(["team-mcp", record.to_str().unwrap()])),
            vec![],
        ),
        (
            "a hard link to the engine",
            served(&hard, json!(["team-mcp", record.to_str().unwrap()])),
            vec![],
        ),
        (
            "another turn's drop file",
            served(&engine, json!(["team-mcp", other_drop.to_str().unwrap()])),
            vec![],
        ),
        (
            "an extra argument",
            served(
                &engine,
                json!(["team-mcp", record.to_str().unwrap(), "--verbose"]),
            ),
            vec![],
        ),
        ("a second server", second.to_string(), vec![]),
        (
            "a second configuration",
            legit.clone(),
            vec!["--mcp-config".to_owned(), legit.clone()],
        ),
        (
            "an inline configuration",
            legit.clone(),
            vec![format!("--mcp-config={legit}")],
        ),
    ] {
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        assert_eq!(
            delegate(&configured(&command, &config, &extra), Some(tools)),
            unverified,
            "{name}"
        );
    }
    assert_eq!(
        sandbox.sandbox().writable_roots().collect::<Vec<_>>(),
        [&worktree, &home, &turn.join("tmp")]
    );
    assert!(sandbox.sandbox().read_roots().contains(&install));
    assert!(sandbox.sandbox().read_roots().contains(&repository));
    assert!(home.join(".credentials.json").is_file());
    let registered = |path: &Path| {
        crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(path).unwrap()
    };
    let turn = MasterTurnLaunch {
        command,
        sandbox,
        home: home.clone(),
        credential,
        tool_record,
    };
    let checked = |backend, turn: &MasterTurnLaunch, engine: Option<&Path>| {
        let engine = engine.map(registered);
        require_master_delegate(backend, turn, engine.as_ref()).map_err(|error| error.0)
    };
    let unverified: Result<(), String> = Err("master_delegate_unverified".to_owned());
    assert_eq!(checked(MasterBackend::Claude, &turn, Some(&engine)), Ok(()));
    assert_eq!(
        checked(MasterBackend::Claude, &turn, Some(&hard)),
        unverified
    );
    assert_eq!(
        checked(MasterBackend::Codex, &turn, Some(&engine)),
        Err("master_runtime_backend_unsupported".to_owned())
    );
    assert!(checked(MasterBackend::Claude, &turn, None).is_err());
    assert_eq!(
        checked(MasterBackend::Rcli, &turn, Some(&engine)),
        Err("master_runtime_backend_unsupported".to_owned())
    );
    let mut turn = turn;
    let record = turn.tool_record.replace(other_drop.clone());
    assert_eq!(
        checked(MasterBackend::Claude, &turn, Some(&engine)),
        unverified
    );
    turn.tool_record = record;
    assert_eq!(checked(MasterBackend::Claude, &turn, Some(&engine)), Ok(()));
    let MasterTurnLaunch { credential, .. } = turn;
    drop(credential);
    drop(tools_folder);
    assert!(!home.exists());
    drop(profile);
}
