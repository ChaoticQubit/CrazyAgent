use super::super::Root;
use super::*;
use crate::config::{ModelAuthentication, ModelReference};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor};
use base64::Engine;
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Writes a managed Codex account's `auth.json` whose access token lapses in eight hours.
/// Input: the managed home. Output: the access token the record holds.
fn codex_account(managed: &Path) -> String {
    let expires = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 8 * 3600;
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&json!({"exp": expires})).unwrap());
    let access = format!("unit-header.{claims}.unit-signature");
    let record = json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": "unit-identity",
            "access_token": access,
            "refresh_token": "unit-refresh-secret",
            "account_id": "unit-account"
        },
        "last_refresh": "2026-10-26T00:00:00Z"
    });
    let path = managed.join("auth.json");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    access
}

/// Copies a command with its arguments changed. Input: the command and the change.
/// Output: the new command with the same program, directory and environment.
fn changed(command: &Command, change: impl Fn(&mut Vec<String>)) -> Command {
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

fn settings(command: &Command) -> Vec<String> {
    let args: Vec<&str> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    args.windows(2)
        .filter(|pair| pair[0] == "-c")
        .map(|pair| pair[1].to_owned())
        .collect()
}

#[test]
fn a_codex_master_with_tools_on_runs_in_its_own_home_with_only_the_engine_delegate_server() {
    let temporary = tempfile::Builder::new()
        .prefix("ca-codex-turn-")
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
        id: reference("unit-codex-connection"),
        provider: reference("openai"),
        harness: reference("codex"),
        protocol: reference("native"),
        account_ref: reference("unit-codex-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "unit-model".into(),
            author: reference("openai"),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("unit-codex-profile"),
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
    let turn = RequestId::new("unit-codex-turn").unwrap();
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
    let access = codex_account(&managed);
    let runtime = base.join("runtime");
    private_directory(&runtime);
    let root = Root::open(&runtime).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let install = base.join("install");
    private_directory(&install);
    let codex = install.join("codex");
    let engine = install.join("crazyagent");
    for executable in [&codex, &engine] {
        std::fs::write(executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let (off_ref, _, _) = allocate_master_capture(&root).unwrap();
    let off_folder = MasterRuntimeFolder::new(&root, &off_ref);
    let place = MasterTurnPlace {
        root: &root,
        runtime_ref: &off_ref,
        repository: &repository,
        worktree: &worktree,
    };
    let MasterTurnLaunch {
        command,
        sandbox,
        home,
        credential,
        tool_record,
    } = codex_master_turn(
        &place,
        &profile,
        &codex,
        &connection,
        Duration::from_secs(3600),
        None,
    )
    .unwrap();
    let off_turn = runtime.join(format!("master-runtime-{}", off_ref.as_str()));
    assert_eq!(home, off_turn.join("home"));
    let written: Value =
        serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(written["tokens"]["access_token"], access);
    assert_eq!(written["tokens"]["refresh_token"], "");
    assert!(tool_record.is_none());
    let today = master_command(&codex, &home, &worktree, "unit-model", None).unwrap();
    assert!(command.get_args().eq(today.get_args()));
    assert!(command.get_envs().eq(today.get_envs()));
    assert!(
        sandbox
            .sandbox()
            .writable_roots()
            .chain(sandbox.sandbox().read_roots())
            .all(|root| !managed.starts_with(root) && !root.starts_with(&managed))
    );
    assert!(settings(&command).contains(&"sandbox_mode=\"read-only\"".to_owned()));
    assert!(settings(&command).contains(&"mcp_servers={}".to_owned()));
    assert_eq!(require_master_tool_policy(&command, None), Ok(()));
    let unused = crate::engine::claude_host::MasterTools {
        engine: &engine,
        drop: &engine,
    };
    assert!(require_master_tool_policy(&command, Some(unused)).is_err());
    assert!(
        sandbox
            .sandbox()
            .writable_roots()
            .any(|writable| *writable == repository)
    );
    drop(credential);
    drop(off_folder);

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
    } = codex_master_turn(
        &place,
        &profile,
        &codex,
        &connection,
        Duration::from_secs(3600),
        Some(&engine),
    )
    .unwrap();
    let turn = runtime.join(format!("master-runtime-{}", tools_ref.as_str()));
    assert_eq!(home, turn.join("home"));
    assert_eq!(std::fs::metadata(&home).unwrap().mode() & 0o7777, 0o700);
    let copy = home.join("auth.json");
    assert_eq!(std::fs::metadata(&copy).unwrap().mode() & 0o7777, 0o600);
    let written: Value = serde_json::from_slice(&std::fs::read(&copy).unwrap()).unwrap();
    assert_eq!(written["tokens"]["access_token"], access);
    assert_eq!(written["tokens"]["refresh_token"], "");
    let record = turn.join("tmp").join(crate::team::tool::DROP_FILE);
    assert_eq!(tool_record.as_deref(), Some(record.as_path()));
    let tools = crate::engine::claude_host::MasterTools {
        engine: &engine,
        drop: &record,
    };
    let envs: Vec<(String, String)> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_str().unwrap().to_owned(),
                value.unwrap().to_str().unwrap().to_owned(),
            )
        })
        .collect();
    for key in ["HOME", "CODEX_HOME"] {
        assert!(
            envs.contains(&(key.to_owned(), home.to_str().unwrap().to_owned())),
            "{envs:?}"
        );
    }
    let settings = settings(&command);
    for required in [
        "sandbox_mode=\"danger-full-access\"",
        "approval_policy=\"never\"",
        "cli_auth_credentials_store=\"file\"",
        "features.apps=false",
        "features.plugins=false",
        "features.goals=false",
        "features.multi_agent=false",
        "features.multi_agent_v2=false",
    ] {
        assert!(settings.contains(&required.to_owned()), "{required}");
    }
    for refused in [
        "sandbox_mode=\"read-only\"",
        "features.shell_tool=false",
        "features.apply_patch_freeform=false",
        "mcp_servers={}",
    ] {
        assert!(!settings.contains(&refused.to_owned()), "{refused}");
    }
    let servers: Vec<&String> = settings
        .iter()
        .filter(|setting| setting.starts_with("mcp_servers="))
        .collect();
    assert_eq!(servers.len(), 1, "{settings:?}");
    let parsed: toml::Table = toml::from_str(servers[0]).unwrap();
    let expected: toml::Table = toml::from_str(&format!(
        "[mcp_servers.crazyagent-team]\ncommand = {:?}\nargs = [\"team-mcp\", {:?}]\n",
        engine.to_str().unwrap(),
        record.to_str().unwrap()
    ))
    .unwrap();
    assert_eq!(parsed, expected);
    assert_eq!(
        sandbox.sandbox().writable_roots().collect::<Vec<_>>(),
        [&worktree, &home, &turn.join("tmp")]
    );
    assert!(sandbox.sandbox().read_roots().contains(&install));
    assert!(sandbox.sandbox().read_roots().contains(&repository));
    assert!(
        sandbox
            .sandbox()
            .writable_roots()
            .chain(sandbox.sandbox().read_roots())
            .all(|root| !managed.starts_with(root) && !root.starts_with(&managed))
    );
    assert_eq!(require_master_tool_policy(&command, Some(tools)), Ok(()));
    assert!(require_master_tool_policy(&command, None).is_err());

    let widened = changed(&command, |args| {
        let server = args
            .iter()
            .position(|arg| arg.starts_with("mcp_servers="))
            .unwrap();
        args[server] = format!(
            "mcp_servers={{crazyagent-team={{command={:?},args=[\"team-mcp\",{:?}]}},owner={{command=\"/usr/bin/true\"}}}}",
            engine.to_str().unwrap(),
            record.to_str().unwrap()
        );
    });
    assert!(require_master_tool_policy(&widened, Some(tools)).is_err());
    let relative = changed(&command, |args| {
        let server = args
            .iter()
            .position(|arg| arg.starts_with("mcp_servers="))
            .unwrap();
        args[server] =
            "mcp_servers={crazyagent-team={command=\"crazyagent\",args=[\"team-mcp\",\"/tmp/x\"]}}"
                .into();
    });
    assert!(require_master_tool_policy(&relative, Some(tools)).is_err());
    let shell_off = changed(&command, |args| {
        args.insert(0, "features.shell_tool=false".into());
        args.insert(0, "-c".into());
    });
    assert!(require_master_tool_policy(&shell_off, Some(tools)).is_err());
    let read_only = changed(&command, |args| {
        let sandbox = args
            .iter()
            .position(|arg| arg == "sandbox_mode=\"danger-full-access\"")
            .unwrap();
        args[sandbox] = "sandbox_mode=\"read-only\"".into();
    });
    assert!(require_master_tool_policy(&read_only, Some(tools)).is_err());
    for setting in [
        "features.multi_agent=false",
        "features.multi_agent_v2=false",
    ] {
        let own_agents = changed(&command, |args| {
            let at = args.iter().position(|arg| arg == setting).unwrap();
            args.drain(at - 1..=at);
        });
        assert!(
            require_master_tool_policy(&own_agents, Some(tools)).is_err(),
            "{setting}"
        );
    }
    let no_server = changed(&command, |args| {
        let server = args
            .iter()
            .position(|arg| arg.starts_with("mcp_servers="))
            .unwrap();
        args.drain(server - 1..=server);
    });
    assert!(require_master_tool_policy(&no_server, Some(tools)).is_err());
    let off_with_server = changed(&today, |args| {
        let server = args.iter().position(|arg| arg == "mcp_servers={}").unwrap();
        args[server] = servers[0].clone();
    });
    assert!(require_master_tool_policy(&off_with_server, None).is_err());

    let link = install.join("crazyagent-link");
    std::os::unix::fs::symlink(&engine, &link).unwrap();
    let hard = install.join("crazyagent-hard");
    std::fs::hard_link(&engine, &hard).unwrap();
    let other_drop = runtime
        .join("master-runtime-other")
        .join("tmp")
        .join(crate::team::tool::DROP_FILE);
    let served = |server: &Path, rest: &[&Path]| {
        let render = |text: &str| toml_edit::Value::from(text).to_string();
        let setting = format!(
            "mcp_servers={{crazyagent-team={{command={},args=[{}]}}}}",
            render(server.to_str().unwrap()),
            std::iter::once(render("team-mcp"))
                .chain(rest.iter().map(|arg| render(arg.to_str().unwrap())))
                .collect::<Vec<_>>()
                .join(",")
        );
        changed(&command, |args| {
            let index = args
                .iter()
                .position(|arg| arg.starts_with("mcp_servers="))
                .unwrap();
            args[index] = setting.clone();
        })
    };
    assert_eq!(
        require_master_tool_policy(&served(&engine, &[&record]), Some(tools)),
        Ok(())
    );
    for (name, refused) in [
        (
            "another executable",
            served(Path::new("/usr/bin/true"), &[&record]),
        ),
        ("a link to the engine", served(&link, &[&record])),
        ("a hard link to the engine", served(&hard, &[&record])),
        ("another turn's drop file", served(&engine, &[&other_drop])),
        (
            "an extra argument",
            served(&engine, &[&record, Path::new("--verbose")]),
        ),
        (
            "a second setting in the same flag",
            changed(&command, |args| {
                let index = args
                    .iter()
                    .position(|arg| arg.starts_with("mcp_servers="))
                    .unwrap();
                args[index] = format!("{}\nweb_search=\"live\"", servers[0]);
            }),
        ),
    ] {
        assert_eq!(
            require_master_tool_policy(&refused, Some(tools)).map_err(|error| error.0),
            Err("master_delegate_unverified".to_owned()),
            "{name}"
        );
    }

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
    assert_eq!(checked(MasterBackend::Codex, &turn, Some(&engine)), Ok(()));
    assert_eq!(
        checked(MasterBackend::Codex, &turn, Some(&hard)),
        unverified
    );
    assert_eq!(
        checked(MasterBackend::Claude, &turn, Some(&engine)),
        unverified
    );
    assert!(checked(MasterBackend::Codex, &turn, None).is_err());
    assert_eq!(
        checked(MasterBackend::Rcli, &turn, Some(&engine)),
        Err("master_runtime_backend_unsupported".to_owned())
    );
    let mut turn = turn;
    let record = turn.tool_record.replace(other_drop.clone());
    assert_eq!(
        checked(MasterBackend::Codex, &turn, Some(&engine)),
        unverified
    );
    turn.tool_record = record;
    assert_eq!(checked(MasterBackend::Codex, &turn, Some(&engine)), Ok(()));
    let inside = worktree.join("crazyagent");
    std::fs::write(&inside, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        checked(MasterBackend::Codex, &turn, Some(&inside)),
        Err("master_delegate_writable".to_owned())
    );
    std::fs::remove_file(&inside).unwrap();
    let quoted = |path: &Path| format!("'{}'", path.display());
    let script = format!(
        "(echo x > {engine}) 2>/dev/null && echo engine=written || echo engine=refused\n\
         (mv {engine} {moved}) 2>/dev/null && echo engine=moved || echo engine=kept\n\
         (echo x > {probe}) 2>/dev/null && echo worktree=written || echo worktree=refused\n",
        engine = quoted(&engine),
        moved = quoted(&engine.with_extension("moved")),
        probe = quoted(&worktree.join("probe.txt")),
    );
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(turn.sandbox.profile())
            .args(["/bin/sh", "-c", &script])
            .current_dir(&worktree)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &turn.home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "engine=refused\nengine=kept\nworktree=written\n",
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let MasterTurnLaunch { credential, .. } = turn;
    drop(credential);
    assert!(!copy.exists());
    drop(tools_folder);
    assert!(!home.exists());
    drop(profile);
}

#[test]
fn the_installed_codex_reads_a_tools_on_master_with_its_own_sub_agents_off() {
    let codex = crate::harnesses::catalog(&crate::harnesses::Search::from_environment())
        .into_iter()
        .find(|entry| entry.id == "codex")
        .and_then(|entry| entry.executable)
        .expect("installed Codex");
    let codex = std::fs::canonicalize(codex).unwrap();
    let temporary = tempfile::Builder::new()
        .prefix("ca-codex-features-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = temporary.path().canonicalize().unwrap();
    let home = base.join("home");
    private_directory(&home);
    let drop = base.join("tmp").join(crate::team::tool::DROP_FILE);
    let tools = crate::engine::claude_host::MasterTools {
        engine: &codex,
        drop: &drop,
    };
    let command = master_command(&codex, &home, &base, ACCOUNT_DEFAULT_MODEL, Some(tools)).unwrap();
    let mut features = Command::new(&codex);
    features
        .args(
            settings(&command)
                .iter()
                .flat_map(|setting| ["-c", setting]),
        )
        .args(["features", "list"])
        .env_clear()
        .env("HOME", &home)
        .env("CODEX_HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&base)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = crate::spawn_gate::output(&mut features).unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let state = |name: &str| {
        text.lines().find_map(|line| {
            let columns: Vec<&str> = line.split_whitespace().collect();
            (columns.first() == Some(&name))
                .then(|| columns.last().copied())
                .flatten()
        })
    };
    assert_eq!(state("apps"), Some("false"), "{text}");
    assert_eq!(state("multi_agent"), Some("false"), "{text}");
    assert_eq!(state("multi_agent_v2"), Some("false"), "{text}");
    assert_eq!(state("shell_tool"), Some("true"), "{text}");
}

#[test]
fn the_delegate_engine_is_the_registered_executable_at_its_canonical_path() {
    let temporary = tempfile::tempdir().unwrap();
    let base = temporary.path();
    let real = base.join("real");
    std::fs::create_dir(&real).unwrap();
    let engine = real.join("crazyagent");
    std::fs::write(&engine, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    let canonical = std::fs::canonicalize(&engine).unwrap();
    let registered =
        crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(&canonical).unwrap();
    assert_eq!(delegate_engine(&registered), Ok(canonical.as_path()));

    let linked = base.join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let through_link = crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(
        &linked.join("crazyagent"),
    )
    .unwrap();
    assert_eq!(
        delegate_engine(&through_link).map_err(|error| error.0),
        Err("master_delegate_unverified".to_owned())
    );

    std::fs::rename(&real, base.join("moved")).unwrap();
    std::fs::create_dir(&real).unwrap();
    std::fs::write(&engine, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        delegate_engine(&registered).map_err(|error| error.0),
        Err("native_auth_host_executable_changed".to_owned())
    );
}
