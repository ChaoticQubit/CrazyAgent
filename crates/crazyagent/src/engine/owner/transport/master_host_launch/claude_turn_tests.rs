use super::super::Root;
use super::*;
use crate::config::{ModelAuthentication, ModelReference};
use crate::credentials::test_keychain::TestKeychain;
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Command;

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

/// Lists the tools the engine's `team-mcp` server offers when it runs, in this process, on
/// the arguments a master turn's MCP configuration gives it. Input: the server's arguments
/// after its executable. Output: the tool names in the order the server lists them.
pub(super) fn listed_by(args: &[String]) -> Vec<String> {
    assert_eq!(args[0], crate::team::tool::SERVER_COMMAND);
    let request = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})
    );
    let mut output = Vec::new();
    crate::team::tool::serve(
        std::io::Cursor::new(request),
        &mut output,
        Path::new(&args[1]),
        args.get(2).map(Path::new),
        args.get(3..).unwrap_or_default(),
    )
    .unwrap();
    let answer: Value = serde_json::from_slice(&output).unwrap();
    answer["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Stores a Claude Code sign-in record where Claude Code keeps it: the managed home's
/// own login keychain, a temporary one, under the service named after that home.
/// Inputs: the managed home and the record. Output: the keychain, deleted on drop.
fn claude_keychain(home: &Path, record: &Value) -> TestKeychain {
    let keychain = TestKeychain::create(home);
    let service = format!(
        "Claude Code-credentials-{}",
        &format!("{:x}", Sha256::digest(home.to_str().unwrap().as_bytes()))[..8]
    );
    keychain.add("unit", &service, &record.to_string());
    keychain
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
    let _keychain = claude_keychain(&managed, &record);
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
        policy: &AgentPolicy::default(),
        hidden: &[],
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
        Some(Duration::from_secs(3600)),
        None,
        &[],
        StepLimit::Steps50,
    )
    .unwrap();
    let single: Vec<&str> = off_command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(
        single.iter().filter(|arg| **arg == "--max-turns").count(),
        1,
        "a tools-off turn carries no master step limit"
    );
    assert!(single.windows(2).any(|pair| pair == ["--max-turns", "1"]));
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

    let (refused_ref, _, _) = allocate_master_capture(&root).unwrap();
    let refused_place = MasterTurnPlace {
        root: &root,
        runtime_ref: &refused_ref,
        repository: &repository,
        worktree: &worktree,
        policy: &AgentPolicy::default(),
        hidden: &[],
    };
    let host_limit = usize::try_from(crate::engine::claude_host::INSTRUCTIONS_BYTES_MAX).unwrap();
    let refused = claude_master_turn(
        &refused_place,
        &profile,
        &claude,
        &connection,
        &"x".repeat(host_limit + 1),
        Some(Duration::from_secs(3600)),
        None,
        &[],
        StepLimit::NoLimit,
    );
    assert_eq!(
        refused.err().map(|error| error.0),
        Some("master_instructions_too_large".to_owned())
    );
    let refused_turn = runtime.join(format!("master-runtime-{}", refused_ref.as_str()));
    assert!(
        !refused_turn.join("home").exists(),
        "no credential copy is made for a prompt Claude Code would refuse"
    );
    assert!(!refused_turn.join("instructions.md").exists());

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
        policy: &AgentPolicy::default(),
        hidden: &[],
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
        Some(Duration::from_secs(3600)),
        Some(&engine),
        &[crate::team::tool::RECALL_TOOL.to_owned()],
        StepLimit::Steps100,
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
    assert!(
        args.ends_with(&["--max-turns", "100"]),
        "a tools-on turn ends with the owner's master step limit (OB-5b): {args:?}"
    );
    let config = args[args.iter().position(|arg| *arg == "--mcp-config").unwrap() + 1];
    let config: Value = serde_json::from_str(config).unwrap();
    let mailbox = turn.join("tmp").join(crate::team::tool::MAILBOX_FOLDER);
    let served: Vec<String> = config["mcpServers"]["crazyagent-team"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        listed_by(&served),
        [
            crate::team::tool::TOOL_NAME,
            crate::team::tool::ASK_TOOL,
            crate::team::tool::RECORD_TOOL,
            crate::team::tool::WITHDRAW_TOOL,
            crate::team::tool::REMEMBER_TOOL
        ],
        "a console master with tools asks the owner and remembers through the engine, less the tools switched off"
    );
    assert_eq!(std::fs::metadata(&mailbox).unwrap().mode() & 0o7777, 0o700);
    assert_eq!(
        config,
        json!({"mcpServers": {"crazyagent-team": {
            "type": "stdio",
            "command": engine.to_str().unwrap(),
            "args": [
                "team-mcp",
                record.to_str().unwrap(),
                mailbox.to_str().unwrap(),
                "recall",
                "wake"
            ],
        }}})
    );
    let off = [crate::team::tool::RECALL_TOOL.to_owned()];
    let tools = crate::engine::claude_host::MasterTools {
        engine: &engine,
        drop: &record,
        mailbox: &mailbox,
        off: &off,
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
    let legit = served(
        &engine,
        json!([
            "team-mcp",
            record.to_str().unwrap(),
            mailbox.to_str().unwrap(),
            "recall",
            "wake"
        ]),
    );
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
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "recall",
                    "wake"
                ]),
            ),
            vec![],
        ),
        (
            "a link to the engine",
            served(
                &link,
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "recall",
                    "wake"
                ]),
            ),
            vec![],
        ),
        (
            "a hard link to the engine",
            served(
                &hard,
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "recall",
                    "wake"
                ]),
            ),
            vec![],
        ),
        (
            "another turn's drop file",
            served(
                &engine,
                json!([
                    "team-mcp",
                    other_drop.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "recall",
                    "wake"
                ]),
            ),
            vec![],
        ),
        (
            "an extra argument",
            served(
                &engine,
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "recall",
                    "wake",
                    "--verbose"
                ]),
            ),
            vec![],
        ),
        (
            "another turn's mailbox",
            served(
                &engine,
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    other_drop
                        .with_file_name(crate::team::tool::MAILBOX_FOLDER)
                        .to_str()
                        .unwrap(),
                    "recall",
                    "wake"
                ]),
            ),
            vec![],
        ),
        (
            "a switched-off tool listed again",
            served(
                &engine,
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "wake"
                ]),
            ),
            vec![],
        ),
        (
            "wake listed for a conversation master",
            served(
                &engine,
                json!([
                    "team-mcp",
                    record.to_str().unwrap(),
                    mailbox.to_str().unwrap(),
                    "recall"
                ]),
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
    let caches = default_master_tool_caches(&root.path, &repository);
    let turn_tmp = turn.join("tmp");
    assert_eq!(
        sandbox.sandbox().writable_roots().collect::<Vec<_>>(),
        [&worktree, &home, &turn_tmp]
            .into_iter()
            .chain(&caches)
            .collect::<Vec<_>>()
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
    let checked_off = |backend, turn: &MasterTurnLaunch, engine: Option<&Path>, off: &[String]| {
        let engine = engine.map(registered);
        require_master_delegate(backend, turn, engine.as_ref(), off).map_err(|error| error.0)
    };
    let checked = |backend, turn: &MasterTurnLaunch, engine: Option<&Path>| {
        checked_off(backend, turn, engine, &off)
    };
    let unverified: Result<(), String> = Err("master_delegate_unverified".to_owned());
    assert_eq!(
        checked_off(MasterBackend::Claude, &turn, Some(&engine), &[]),
        unverified,
        "a turn built with recall switched off is not the turn with every tool on"
    );
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
