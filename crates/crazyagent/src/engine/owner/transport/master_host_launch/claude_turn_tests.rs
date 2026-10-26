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
    drop(profile);
}
