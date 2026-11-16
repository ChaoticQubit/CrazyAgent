use super::super::Root;
use super::*;
use crate::config::{ModelAuthentication, ModelReference};
use crate::credentials::test_keychain::TestKeychain;
use crate::engine::bootstrap::{NativeProfileCustody, NativeProfileUsePurpose};
use crate::engine::owner::{HostStamp, NativeProfileBinding};
use crate::protocol::{ConfigId, CredentialRef, Cursor};
use crate::sandbox::toolchains::{self, ToolHomes};
use base64::Engine;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Command;

const FIXTURES: &str = "tests/fixtures/harness_layer";
const CAPTURE: &str = "CRAZYAGENT_PARITY_CAPTURE";
const EXPIRES_SECONDS: u64 = 4_102_444_800;
const FILE_BYTES_MAX: u64 = 64 * 1024;
const FILES_MAX: usize = 256;
const OWNER_HOME: &str = "<OWNER_HOME>";
const DEVELOPER_TOOLS: &str = "/Library/Developer/CommandLineTools";
const RUSTUP_BINS: [&str; 2] = ["/opt/homebrew/opt/rustup/bin", "/usr/local/opt/rustup/bin"];

/// The two native masters whose turns the snapshot holds.
#[derive(Clone, Copy)]
enum Master {
    Codex,
    Claude,
}

fn reference(value: &str) -> CredentialRef {
    CredentialRef::new(value).unwrap()
}

fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// Writes a managed Codex account's `auth.json` whose tokens lapse at a fixed time far
/// ahead, so two builds write the same copy. Input: the managed home. Output: none.
fn codex_account(managed: &Path) {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&json!({"exp": EXPIRES_SECONDS})).unwrap());
    let record = json!({
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": format!("parity-header.{claims}.parity-identity-signature"),
            "access_token": format!("parity-header.{claims}.parity-signature"),
            "refresh_token": "parity-refresh-secret",
            "account_id": "parity-account"
        },
        "last_refresh": "2026-10-26T00:00:00Z"
    });
    let path = managed.join("auth.json");
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

/// Stores a Claude Code sign-in record, lapsing at a fixed time far ahead, in the managed
/// home's own temporary login keychain. Input: the managed home. Output: the keychain,
/// deleted on drop.
fn claude_keychain(managed: &Path) -> TestKeychain {
    let record = json!({
        "claudeAiOauth": {
            "accessToken": "sk-ant-oat01-parity",
            "refreshToken": "sk-ant-ort01-parity-secret",
            "expiresAt": EXPIRES_SECONDS * 1000,
            "scopes": ["user:inference"],
            "subscriptionType": "max"
        }
    });
    let keychain = TestKeychain::create(managed);
    let service = format!(
        "Claude Code-credentials-{}",
        &format!("{:x}", Sha256::digest(managed.to_str().unwrap().as_bytes()))[..8]
    );
    keychain.add("parity", &service, &record.to_string());
    keychain
}

/// The owner's home and tool folders a tools-on master names, each with the fixed name
/// the snapshot holds instead, as the launch rows name them. Takes nothing. Output: the
/// pairs of path and placeholder.
fn tool_placeholders() -> Vec<(String, String)> {
    let homes = ToolHomes::owner().unwrap();
    let mut folders: Vec<(PathBuf, &str)> = vec![
        (homes.rustup().to_path_buf(), "<OWNER_RUSTUP>"),
        (homes.cargo().to_path_buf(), "<OWNER_CARGO>"),
        (PathBuf::from(DEVELOPER_TOOLS), "<DEVELOPER_TOOLS>"),
    ];
    folders.extend(RUSTUP_BINS.map(|bin| (PathBuf::from(bin), "<RUSTUP_BIN>")));
    folders.extend(
        homes
            .xcode()
            .map(|xcode| (xcode.to_path_buf(), "<XCODE_DEVELOPER>")),
    );
    folders.extend(
        toolchains::graphify(&homes)
            .into_iter()
            .zip(["<GRAPHIFY_TOOL>", "<GRAPHIFY_PYTHON>"]),
    );
    folders.push((homes.home().to_path_buf(), OWNER_HOME));
    let pairs: Vec<(String, String)> = folders
        .into_iter()
        .map(|(path, name)| (path.to_str().unwrap().to_owned(), name.to_owned()))
        .collect();
    assert!(
        pairs
            .iter()
            .all(|(path, _)| path.len() > 1 && path.starts_with('/'))
    );
    pairs
}

/// The files under a folder: each relative path with its mode and, for a small text file,
/// its text; a folder ends in `/`. Input: the folder. Output: the listing.
fn files(root: &Path) -> BTreeMap<String, Value> {
    let mut listed = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let mode = format!("{:o}", metadata.mode() & 0o7777);
            if metadata.is_dir() {
                listed.insert(format!("{relative}/"), json!({ "mode": mode }));
                pending.push(path);
            } else if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                listed.insert(relative, json!({ "link": target.to_str().unwrap() }));
            } else {
                let text = (metadata.len() <= FILE_BYTES_MAX)
                    .then(|| std::fs::read_to_string(&path).ok())
                    .flatten();
                listed.insert(relative, json!({ "mode": mode, "text": text }));
            }
            assert!(
                listed.len() <= FILES_MAX,
                "{} holds too many files",
                root.display()
            );
        }
    }
    listed
}

/// The program, arguments, variables and folder of a master command. Input: the command.
/// Output: the record.
fn command_record(command: &Command) -> Value {
    let args: Vec<&str> = command
        .get_args()
        .map(|argument| argument.to_str().unwrap())
        .collect();
    let env: BTreeMap<&str, Option<&str>> = command
        .get_envs()
        .map(|(name, value)| {
            (
                name.to_str().unwrap(),
                value.map(|value| value.to_str().unwrap()),
            )
        })
        .collect();
    json!({
        "program": command.get_program().to_str().unwrap(),
        "args": args,
        "env": env,
        "cwd": command.get_current_dir().map(|folder| folder.to_str().unwrap()),
    })
}

/// One master turn as the snapshot holds it: the command, the files of its home, the
/// folders its boundary lets it write and read, the toolchain folders it grants, which
/// `tool_placeholders` names, and its tool record. Input: the launch. Output: the record.
fn turn_record(launch: &MasterTurnLaunch) -> Value {
    let text = |path: &PathBuf| path.to_str().unwrap().to_owned();
    let rules = launch.sandbox.sandbox();
    json!({
        "command": command_record(&launch.command),
        "home": files(&launch.home),
        "writable": rules.writable_roots().map(text).collect::<Vec<_>>(),
        "read": rules.read_roots().iter().map(text).collect::<Vec<_>>(),
        "tools": rules.tool_roots().iter().map(text).collect::<Vec<_>>(),
        "tool_record": launch.tool_record.as_ref().map(text),
    })
}

/// Replaces each placeholder's text in every string and key of a record. Inputs: the
/// record and the pairs, longest text first. Output: the record.
fn normalized(value: Value, pairs: &[(String, String)]) -> Value {
    let replaced = |mut text: String| {
        for (from, to) in pairs {
            text = text.replace(from.as_str(), to);
        }
        text
    };
    match value {
        Value::String(text) => Value::String(replaced(text)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| normalized(item, pairs))
                .collect(),
        ),
        Value::Object(fields) => {
            let mut renamed = Map::new();
            for (key, item) in fields {
                renamed.insert(replaced(key), normalized(item, pairs));
            }
            Value::Object(renamed)
        }
        other => other,
    }
}

/// Builds a master's turn with tools off and its turn with tools on, in fresh folders and
/// a fresh custody, as the turn rows of each master do. Input: the master. Output: the
/// records and the placeholders their folders read as.
fn master_turns(master: Master) -> (Value, Vec<(String, String)>) {
    let temporary = tempfile::Builder::new()
        .prefix("ca-parity-master-")
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
    let (provider, harness) = match master {
        Master::Codex => ("openai", "codex"),
        Master::Claude => ("anthropic", "claude"),
    };
    let connection = ModelConnection {
        id: reference("parity-master-connection"),
        provider: reference(provider),
        harness: reference(harness),
        protocol: reference("native"),
        account_ref: reference("parity-master-account"),
        endpoint: ModelEndpoint::Native {},
        model: ModelReference {
            id: "parity-model".into(),
            author: reference(provider),
        },
        auth: ModelAuthentication::NativeManaged {
            profile_ref: reference("parity-master-profile"),
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
    let turn = RequestId::new("parity-master-turn").unwrap();
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
    let _keychain = match master {
        Master::Codex => {
            codex_account(&managed);
            None
        }
        Master::Claude => Some(claude_keychain(&managed)),
    };
    let runtime = state.join("runtime");
    private_directory(&runtime);
    let root = Root::open(&runtime).unwrap();
    let repository = base.join("repository");
    let worktree = base.join("worktrees").join("lease");
    private_directory(&repository);
    private_directory(&worktree);
    let install = base.join("install");
    private_directory(&install);
    let program = install.join(harness);
    let engine = install.join("crazyagent");
    for executable in [&program, &engine] {
        std::fs::write(executable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut pairs = vec![
        (
            temporary.path().to_str().unwrap().to_owned(),
            "<BASE>".to_owned(),
        ),
        (base.to_str().unwrap().to_owned(), "<BASE>".to_owned()),
    ];
    let cache =
        crate::sandbox::tool_cache::project_folder(Path::new("/"), repository.to_str().unwrap());
    pairs.push((
        cache.file_name().unwrap().to_str().unwrap().to_owned(),
        "<REPOSITORY_TOOL_CACHE>".to_owned(),
    ));
    for (folder, name) in [
        (&worktree, "<WORKTREE_AS_CLAUDE_PROJECT>"),
        (&repository, "<REPOSITORY_AS_CLAUDE_PROJECT>"),
    ] {
        if let Some(encoded) = crate::harnesses::interactive::claude_project_folder(folder) {
            pairs.push((encoded, name.to_owned()));
        }
    }
    let mut record = Map::new();
    for (case, tools) in [("tools-off", None), ("tools-on", Some(engine.as_path()))] {
        let (runtime_ref, capture_ref, _) = allocate_master_capture(&root).unwrap();
        pairs.push((runtime_ref.as_str().to_owned(), format!("<{case}-turn>")));
        pairs.push((capture_ref.as_str().to_owned(), format!("<{case}-capture>")));
        let folder = MasterRuntimeFolder::new(&root, &runtime_ref);
        let place = MasterTurnPlace {
            root: &root,
            runtime_ref: &runtime_ref,
            repository: &repository,
            worktree: &worktree,
            policy: &AgentPolicy::default(),
            hidden: &[],
        };
        let launch = match master {
            Master::Codex => codex_master_turn(
                &place,
                &profile,
                &program,
                &connection,
                Some(Duration::from_secs(3600)),
                tools,
                &[],
                crate::provider_retry::ProviderRetry::RetryBriefly,
            ),
            Master::Claude => claude_master_turn(
                &place,
                &profile,
                &program,
                &connection,
                "Parity master instructions.",
                Some(Duration::from_secs(3600)),
                tools,
                &[],
                StepLimit::Steps50,
                crate::provider_retry::ProviderRetry::StopAtOnce,
            ),
        }
        .unwrap();
        record.insert(case.to_owned(), turn_record(&launch));
        drop(launch);
        drop(folder);
    }
    (Value::Object(record), pairs)
}

/// Builds a master's turns twice and returns the one record both builds give, failing when
/// they differ; while capturing, both builds are kept beside the snapshot first. Inputs:
/// the snapshot name and the master. Output: the record.
fn twice(name: &str, master: Master) -> Value {
    let owner = tool_placeholders();
    let records: Vec<Value> = (0..2)
        .map(|_| {
            let (record, mut pairs) = master_turns(master);
            pairs.extend(owner.iter().cloned());
            pairs
                .sort_by(|left, right| right.0.len().cmp(&left.0.len()).then(left.0.cmp(&right.0)));
            pairs.dedup();
            normalized(record, &pairs)
        })
        .collect();
    if records[0] != records[1]
        && let Some(folder) = std::env::var_os(CAPTURE)
    {
        std::fs::create_dir_all(&folder).unwrap();
        for (round, record) in records.iter().enumerate() {
            let path = PathBuf::from(&folder).join(format!("{name}.round{round}.json"));
            std::fs::write(path, serde_json::to_string_pretty(record).unwrap()).unwrap();
        }
    }
    assert_eq!(records[0], records[1], "{name}: two builds differ");
    let text = records[0].to_string();
    let mut leftovers = vec![
        "/private/tmp/".to_owned(),
        "/private/var/folders/".to_owned(),
        "/var/folders/".to_owned(),
        "/Users/".to_owned(),
        OWNER_HOME.to_owned(),
        "parity-refresh-secret".to_owned(),
        "sk-ant-ort01-parity-secret".to_owned(),
    ];
    leftovers.extend(owner.into_iter().map(|(path, _)| path));
    for leftover in leftovers {
        assert!(
            !text.contains(&leftover),
            "{name} still holds {leftover}: {text}"
        );
    }
    records.into_iter().next().unwrap()
}

/// Writes the record as a new snapshot in the capture folder when one is named, else
/// compares it with the kept snapshot, which must exist. Inputs: the name and the record.
/// Output: none; a difference fails.
fn check(name: &str, record: &Value) {
    let shown = format!("{}\n", serde_json::to_string_pretty(record).unwrap());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(folder) = std::env::var_os(CAPTURE) {
        let folder = PathBuf::from(folder);
        let checkout = manifest.ancestors().nth(2).unwrap();
        assert!(folder.is_absolute() && !folder.starts_with(checkout));
        std::fs::create_dir_all(&folder).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(folder.join(format!("{name}.json")))
            .unwrap();
        std::io::Write::write_all(&mut file, shown.as_bytes()).unwrap();
        return;
    }
    let path = manifest.join(FIXTURES).join(format!("{name}.json"));
    let kept = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("no snapshot at {}", path.display()));
    assert_eq!(shown, kept, "{name} differs from its snapshot");
}

/// A Codex master turn, tools off and on, equals the snapshot taken at the base commit:
/// command, home, boundary and tool record (Task 5, Task 8 and Task 20).
#[test]
fn codex_master_turns_match_the_base_snapshot() {
    check("master-codex", &twice("master-codex", Master::Codex));
}

/// A Claude Code master turn, tools off and on, equals the snapshot taken at the base
/// commit: command, home, boundary and tool record (Task 5, Task 8 and Task 20).
#[test]
fn claude_master_turns_match_the_base_snapshot() {
    check("master-claude", &twice("master-claude", Master::Claude));
}
