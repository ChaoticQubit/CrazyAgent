use super::super::Root;
use super::*;
use crate::config::{ModelAuthentication, ModelReference};
use crate::connectors::gateway::LAUNCHES_FOLDER;
use crate::connectors::grants::{self, Who};
use crate::connectors::mcp_client::tests::{BOARD, KEY};
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::claude_host::MasterTools;
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor, ProjectId};
use base64::Engine;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// A new owner-only folder under the real `/tmp`, named as the kernel names it. Output: the
/// folder, which goes when dropped, and its canonical path.
fn temporary() -> (tempfile::TempDir, PathBuf) {
    let temporary = tempfile::Builder::new()
        .prefix("ca-master-connector-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let base = temporary.path().canonicalize().unwrap();
    (temporary, base)
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

/// Copies a command with its one `mcp_servers=` setting replaced. Inputs: the command and the
/// new setting. Output: the copy, with the same program, directory and environment.
fn with_servers(command: &Command, servers: &str) -> Command {
    let mut args: Vec<String> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap().to_owned())
        .collect();
    let at = args
        .iter()
        .position(|arg| arg.starts_with("mcp_servers="))
        .unwrap();
    args[at] = servers.to_owned();
    let mut copy = Command::new(command.get_program());
    copy.args(args)
        .current_dir(command.get_current_dir().unwrap())
        .env_clear();
    for (key, value) in command.get_envs() {
        copy.env(key, value.unwrap());
    }
    copy
}

fn render(text: &str) -> String {
    toml_edit::Value::from(text).to_string()
}

/// A Codex master's account lease, owner folder, project and install, as a tools-on turn
/// is built from: the profile holds a managed `auth.json` whose access token lapses in eight
/// hours, and the engine root has its connector launches folder.
struct Bench {
    profile: NativeProfileUse,
    _custody: NativeProfileCustody,
    root: Root,
    repository: PathBuf,
    worktree: PathBuf,
    codex: PathBuf,
    engine: PathBuf,
    launches: PathBuf,
    connection: ModelConnection,
    _temporary: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        let (temporary, base) = temporary();
        let state = base.join("state");
        private_directory(&state);
        let database = state.join("engine.db");
        let sqlite = rusqlite::Connection::open(&database).unwrap();
        sqlite.execute_batch("PRAGMA user_version=1").unwrap();
        sqlite.close().map_err(|(_, error)| error).unwrap();
        std::fs::set_permissions(&database, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut custody =
            NativeProfileCustody::capture(&database, &std::fs::metadata(&database).unwrap())
                .unwrap();
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
        let profile = custody
            .acquire_use(
                &host,
                &binding,
                &RequestId::new("unit-codex-connector-turn").unwrap(),
                1,
                NativeProfileUsePurpose::Execution,
            )
            .unwrap();
        let expires = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 8 * 3600;
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"exp": expires})).unwrap());
        let auth = profile.profile_path().unwrap().join("auth.json");
        let record = json!({
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": format!("unit-header.{claims}.unit-identity-signature"),
                "access_token": format!("unit-header.{claims}.unit-signature"),
                "refresh_token": "unit-refresh-secret",
                "account_id": "unit-account"
            },
            "last_refresh": "2026-10-26T00:00:00Z"
        });
        std::fs::write(&auth, serde_json::to_vec(&record).unwrap()).unwrap();
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600)).unwrap();
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
        let launches = root.engine_root().unwrap().join(LAUNCHES_FOLDER);
        assert_eq!(launches, base.join(LAUNCHES_FOLDER));
        private_directory(&launches);
        Self {
            profile,
            _custody: custody,
            root,
            repository,
            worktree,
            codex,
            engine,
            launches,
            connection,
            _temporary: temporary,
        }
    }

    /// Builds one Codex master turn on the bench. Inputs: the turn reference, the engine
    /// executable for tools on and the connector launch folder. Output: the launch.
    fn turn(
        &self,
        runtime_ref: &RequestId,
        engine: Option<&Path>,
        connector: Option<&Path>,
    ) -> Result<MasterTurnLaunch, EngineError> {
        let place = MasterTurnPlace {
            root: &self.root,
            runtime_ref,
            repository: &self.repository,
            worktree: &self.worktree,
            policy: &AgentPolicy::default(),
            trust: crate::trust::Trust::Reviewed,
            hidden: &[],
        };
        codex_master_turn(
            &place,
            &self.profile,
            &self.codex,
            &self.connection,
            Some(Duration::from_secs(3600)),
            engine,
            &[],
            crate::provider_retry::ProviderRetry::RetryBriefly,
            connector,
        )
    }
}

#[test]
fn a_codex_master_with_a_connector_names_exactly_that_second_server() {
    let (_temporary, base) = temporary();
    let home = base.join("home");
    let worktree = base.join("worktree");
    private_directory(&home);
    private_directory(&worktree);
    let codex = base.join("install").join("codex");
    let engine = base.join("install").join("crazyagent");
    let drop = base.join("tmp").join(crate::team::tool::DROP_FILE);
    let mailbox = base.join("tmp").join(crate::team::tool::MAILBOX_FOLDER);
    let folder = base.join(LAUNCHES_FOLDER).join("a".repeat(32));
    let other = base.join(LAUNCHES_FOLDER).join("b".repeat(32));
    let plain = MasterTools {
        engine: &engine,
        drop: &drop,
        mailbox: &mailbox,
        off: &[],
        connector: None,
    };
    let with = MasterTools {
        connector: Some(&folder),
        ..plain
    };
    let elsewhere = MasterTools {
        connector: Some(&other),
        ..plain
    };
    let build = |tools: MasterTools<'_>| {
        master_command(
            &codex,
            &home,
            &worktree,
            "unit-model",
            Some(tools),
            crate::provider_retry::ProviderRetry::RetryBriefly,
        )
        .unwrap()
    };
    let command = build(with);
    let servers: Vec<String> = settings(&command)
        .into_iter()
        .filter(|setting| setting.starts_with("mcp_servers="))
        .collect();
    assert_eq!(servers.len(), 1, "{servers:?}");
    let parsed: toml::Table = toml::from_str(&servers[0]).unwrap();
    let expected: toml::Table = toml::from_str(&format!(
        "[mcp_servers.crazyagent-team]\ncommand = {:?}\nargs = [\"team-mcp\", {:?}, {:?}, \"wake\"]\n\
         [mcp_servers.ca-connectors]\ncommand = {:?}\nargs = [\"connector-mcp\", {:?}]\n",
        engine.to_str().unwrap(),
        drop.to_str().unwrap(),
        mailbox.to_str().unwrap(),
        engine.to_str().unwrap(),
        folder.to_str().unwrap(),
    ))
    .unwrap();
    assert_eq!(parsed, expected);
    assert_eq!(require_master_tool_policy(&command, Some(with)), Ok(()));
    for expected in [None, Some(plain), Some(elsewhere)] {
        assert!(
            require_master_tool_policy(&command, expected).is_err(),
            "{:?}",
            expected.map(|tools| tools.connector)
        );
    }
    let without = build(plain);
    assert_eq!(require_master_tool_policy(&without, Some(plain)), Ok(()));
    assert!(require_master_tool_policy(&without, Some(with)).is_err());

    let team = format!(
        "crazyagent-team={{command={},args=[{}]}}",
        render(engine.to_str().unwrap()),
        [
            "team-mcp",
            drop.to_str().unwrap(),
            mailbox.to_str().unwrap(),
            "wake"
        ]
        .map(render)
        .join(",")
    );
    let entry = |command: &str, args: &[&str], rest: &str| {
        format!(
            "mcp_servers={{{team},ca-connectors={{command={},args=[{}]{rest}}}}}",
            render(command),
            args.iter()
                .map(|arg| render(arg))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let engine_text = engine.to_str().unwrap();
    let folder_text = folder.to_str().unwrap();
    assert_eq!(
        require_master_tool_policy(
            &with_servers(
                &command,
                &entry(engine_text, &["connector-mcp", folder_text], "")
            ),
            Some(with)
        ),
        Ok(()),
        "the setting spelled by hand is the one the engine builds"
    );
    for (name, servers) in [
        (
            "another executable",
            entry("/usr/bin/true", &["connector-mcp", folder_text], ""),
        ),
        (
            "another launch folder",
            entry(engine_text, &["connector-mcp", other.to_str().unwrap()], ""),
        ),
        (
            "an extra argument",
            entry(
                engine_text,
                &["connector-mcp", folder_text, "--verbose"],
                "",
            ),
        ),
        (
            "another server command",
            entry(engine_text, &["team-mcp", folder_text], ""),
        ),
        (
            "an environment",
            entry(
                engine_text,
                &["connector-mcp", folder_text],
                ",env={A=\"b\"}",
            ),
        ),
        (
            "a third server",
            format!(
                "{},owner={{command=\"/usr/bin/true\"}}}}",
                entry(engine_text, &["connector-mcp", folder_text], "")
                    .strip_suffix('}')
                    .unwrap()
            ),
        ),
    ] {
        assert_eq!(
            require_master_tool_policy(&with_servers(&command, &servers), Some(with))
                .map_err(|error| error.0),
            Err("master_delegate_unverified".to_owned()),
            "{name}: {servers}"
        );
    }
}

#[test]
fn a_tools_on_codex_turn_with_its_connector_runs_that_server_and_uses_only_its_folder() {
    let bench = Bench::new();
    let folder = bench.launches.join("a".repeat(32));
    let other = bench.launches.join("b".repeat(32));
    private_directory(&folder);
    let (runtime_ref, _, _) = allocate_master_capture(&bench.root).unwrap();
    let turn_folder = MasterRuntimeFolder::new(&bench.root, &runtime_ref);
    let turn = bench
        .turn(&runtime_ref, Some(&bench.engine), Some(&folder))
        .unwrap();
    assert!(
        turn.sandbox
            .sandbox()
            .writable_roots()
            .any(|root| root == &folder),
        "the turn uses its own launch folder"
    );
    assert!(
        turn.sandbox
            .sandbox()
            .writable_roots()
            .chain(turn.sandbox.sandbox().read_roots())
            .all(|root| root != &bench.launches && root != &other),
        "the turn never reaches the launches folder or another launch"
    );
    let servers: Vec<String> = settings(&turn.command)
        .into_iter()
        .filter(|setting| setting.starts_with("mcp_servers="))
        .collect();
    assert_eq!(servers.len(), 1, "{servers:?}");
    let parsed: toml::Table = toml::from_str(&servers[0]).unwrap();
    let server = &parsed["mcp_servers"][crate::connectors::launch::SERVER_NAME];
    assert_eq!(server["command"].as_str(), bench.engine.to_str());
    assert_eq!(
        server["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            crate::connectors::shim::SERVER_COMMAND,
            folder.to_str().unwrap()
        ]
    );
    let registered =
        crate::engine::native_auth_host::RegisteredNativeAuthHost::capture(&bench.engine).unwrap();
    let checked = |connector: Option<&Path>| {
        require_master_delegate(
            MasterBackend::Codex,
            &turn,
            Some(&registered),
            &[],
            connector,
        )
        .map_err(|error| error.0)
    };
    assert_eq!(checked(Some(&folder)), Ok(()));
    for refused in [None, Some(other.as_path())] {
        assert_eq!(
            checked(refused),
            Err("master_delegate_unverified".to_owned()),
            "{refused:?}"
        );
    }
    drop(turn);
    drop(turn_folder);
}

#[test]
#[should_panic(expected = "a connector server runs only beside the tools it serves")]
fn a_tools_off_master_turn_is_never_handed_a_connector() {
    let bench = Bench::new();
    let folder = bench.launches.join("a".repeat(32));
    private_directory(&folder);
    let (runtime_ref, _, _) = allocate_master_capture(&bench.root).unwrap();
    let _turn_folder = MasterRuntimeFolder::new(&bench.root, &runtime_ref);
    let _ = bench.turn(&runtime_ref, None, Some(&folder));
}

#[test]
fn a_master_connector_launch_lasts_as_long_as_its_turn_holds_it() {
    let (_temporary, base) = temporary();
    let root = base.join("engine");
    private_directory(&root);
    let db = crate::questions::open(&root).unwrap();
    crate::storage::initialize_authority_schemas(&db).unwrap();
    for project in ["alpha", "beta"] {
        let folder = base.join(project);
        std::fs::create_dir_all(&folder).unwrap();
        crate::ide::workspace::tests::add_project(&db, project, &folder);
    }
    crate::app_plugins::store::add(
        &db,
        &root,
        BOARD,
        &crate::semantic::sha256_hex(BOARD.as_bytes()),
        false,
        1,
    )
    .unwrap();
    crate::app_plugins::store::set_key(&db, &root, "board", "api_key", KEY.as_bytes().to_vec())
        .unwrap();
    grants::set(&db, &root, "alpha", "board", true, Who::Both, 2).unwrap();
    let launches = |db: &rusqlite::Connection| -> i64 {
        db.query_row("SELECT COUNT(*) FROM connector_launches", [], |row| {
            row.get(0)
        })
        .unwrap()
    };

    let held = MasterConnector::open(&root, &ProjectId::new("alpha").unwrap()).unwrap();
    assert!(
        held.is_some(),
        "a project's master granted a connector gets a launch"
    );
    let held = held.unwrap();
    let folder = held.folder().to_path_buf();
    assert!(folder.is_dir());
    assert_eq!(folder.parent(), Some(root.join(LAUNCHES_FOLDER).as_path()));
    assert_eq!(launches(&db), 1);
    drop(held);
    assert!(
        !folder.exists(),
        "dropping the turn's hold ends its launch folder"
    );
    assert_eq!(launches(&db), 0, "and its record");

    assert!(
        MasterConnector::open(&root, &ProjectId::new("beta").unwrap())
            .unwrap()
            .is_none(),
        "a project with no connector granted gets no launch"
    );
    assert_eq!(launches(&db), 0);
}
