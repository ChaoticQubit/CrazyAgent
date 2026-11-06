use super::*;
use crate::accounts::{AccountDetails, Provider};
use crate::config::{ModelAuthentication, ModelConnection, ModelEndpoint, ModelReference};
use crate::plugins::{Scope, ServerSpec};
use crate::protocol::CredentialRef;
use crate::storage::accounts::ACCOUNT_KEYS;
use crate::storage::plugins::{
    CHECK_FOLDER, Command, ResultValue, SECRETS, STORE, execute, move_check_folder_aside,
    new_secret_reference, store_secret,
};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let connection = Connection::open(root.path().join("engine.db")).unwrap();
    super::super::initialize_schema(&connection).unwrap();
    super::super::plugins::initialize(&connection).unwrap();
    super::super::accounts::initialize(&connection).unwrap();
    super::super::setup::initialize(&connection).unwrap();
    initialize(&connection).unwrap();
    (root, connection)
}

fn server(connection: &mut Connection, root: &Path, name: &str) -> String {
    let ResultValue::Server(view) = execute(
        connection,
        root,
        Command::AddServer {
            scope: Scope::Global,
            name: name.into(),
            spec: ServerSpec::Stdio {
                command: "/usr/bin/true".into(),
                args: vec!["--stdio".into()],
            },
            secrets: Vec::new(),
        },
    )
    .unwrap() else {
        panic!("server view required");
    };
    view.id
}

fn attach(connection: &mut Connection, root: &Path, id: &str, name: &str) -> String {
    let reference = new_secret_reference().unwrap();
    store_secret(root, &reference, b"dummy-trash-value".to_vec()).unwrap();
    execute(
        connection,
        root,
        Command::AttachSecret {
            id: id.into(),
            name: name.into(),
            reference: reference.clone(),
        },
    )
    .unwrap();
    reference
}

fn secret_file(root: &Path, reference: &str) -> PathBuf {
    root.join(STORE).join(SECRETS).join(reference)
}

fn check_folder(root: &Path, id: &str) -> PathBuf {
    let folder = root.join(CHECK_FOLDER).join(id).join("project");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("written-by-check"), b"output").unwrap();
    root.join(CHECK_FOLDER).join(id)
}

fn notes(connection: &Connection) -> Vec<(String, String)> {
    connection
        .prepare("SELECT kind,name FROM file_trash ORDER BY kind,name")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn key_folder(root: &Path, mode: u32) -> PathBuf {
    let folder = root.join(ACCOUNT_KEYS);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(mode)).unwrap();
    folder
}

fn key_file(folder: &Path, name: &str) -> PathBuf {
    let file = folder.join(name);
    std::fs::write(&file, b"dummy-account-key").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    file
}

fn saved_account(connection: &Connection, id: &str, key: &str) {
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let account = ModelConnection {
        id: reference(id),
        provider: reference("openai-compatible"),
        harness: reference("rcli"),
        protocol: reference("openai-chat"),
        account_ref: reference("local-account"),
        endpoint: ModelEndpoint::Http {
            url: "http://127.0.0.1:8000/v1".into(),
        },
        model: ModelReference {
            id: "m".into(),
            author: reference("openai-compatible"),
        },
        auth: ModelAuthentication::ApiKey {
            credential_ref: reference(key),
        },
    };
    let details = AccountDetails {
        name: "Lab".into(),
        provider: Provider::OpenaiCompatible,
    };
    let body = serde_json::to_vec(&serde_json::json!({"connection": account, "details": details}))
        .unwrap();
    connection
        .execute(
            "INSERT INTO accounts(id, body) VALUES (?1, ?2)",
            params![id, body],
        )
        .unwrap();
}

#[test]
fn a_server_removed_with_no_settle_leaves_notes_the_sweep_deletes() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let id = server(&mut connection, root, "tools");
    let first = attach(&mut connection, root, &id, "API_TOKEN");
    let second = attach(&mut connection, root, &id, "OTHER_TOKEN");
    let checked = check_folder(root, &id);

    let version = crate::storage::plugins::plugin_version(&connection, &id).unwrap();
    let ResultValue::Removed(references) = execute(
        &mut connection,
        root,
        Command::RemoveServer {
            id: id.clone(),
            version,
        },
    )
    .unwrap() else {
        panic!("removed references required");
    };
    assert_eq!(references.len(), 2);
    assert!(secret_file(root, &first).exists());
    assert!(secret_file(root, &second).exists());
    assert!(checked.exists());
    let mut expected = vec![
        (MCP_CHECKS.to_string(), id.clone()),
        (MCP_SECRET.to_string(), first.clone()),
        (MCP_SECRET.to_string(), second.clone()),
    ];
    expected.sort();
    assert_eq!(notes(&connection), expected);

    assert_eq!(sweep(&connection, root).unwrap(), 0);
    assert!(!secret_file(root, &first).exists());
    assert!(!secret_file(root, &second).exists());
    assert!(!checked.exists());
    assert!(notes(&connection).is_empty());
}

#[test]
fn a_check_folder_moved_aside_before_a_crash_is_swept_and_other_folders_stay() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let id = server(&mut connection, root, "tools");
    let kept = server(&mut connection, root, "kept");
    let checks = root.join(CHECK_FOLDER);
    check_folder(root, &id);
    let live = check_folder(root, &kept);
    let version = crate::storage::plugins::plugin_version(&connection, &id).unwrap();
    let ResultValue::Removed(_) = execute(
        &mut connection,
        root,
        Command::RemoveServer {
            id: id.clone(),
            version,
        },
    )
    .unwrap() else {
        panic!("removed references required");
    };
    let moved = move_check_folder_aside(&checks, &id)
        .unwrap()
        .expect("the removed server's check folder moves aside");
    assert!(!checks.join(&id).exists());
    assert!(moved.join("project").join("written-by-check").exists());
    let other = checks.join(format!(".removed-{id}-other-{}", "a".repeat(32)));
    std::fs::create_dir_all(&other).unwrap();
    assert_eq!(move_check_folder_aside(&checks, &id).unwrap(), None);
    assert_eq!(
        move_check_folder_aside(&checks, "../escape"),
        Err(EngineError("plugin_check_folder_invalid".into()))
    );

    assert_eq!(sweep(&connection, root).unwrap(), 0);
    assert!(!moved.exists(), "a moved folder a crash left is swept");
    assert!(live.exists(), "a live server's check folder stays");
    assert!(
        other.exists(),
        "another name's moved folder is not this note's"
    );
    assert!(notes(&connection).is_empty());
}

#[test]
fn replaced_and_detached_secrets_are_swept_and_the_attached_one_stays() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let id = server(&mut connection, root, "tools");
    let replaced = attach(&mut connection, root, &id, "API_TOKEN");
    let kept = attach(&mut connection, root, &id, "API_TOKEN");
    let detached = attach(&mut connection, root, &id, "OTHER_TOKEN");
    execute(
        &mut connection,
        root,
        Command::DetachSecret {
            id: id.clone(),
            name: "OTHER_TOKEN".into(),
        },
    )
    .unwrap();
    let mut expected = vec![
        (MCP_SECRET.to_string(), replaced.clone()),
        (MCP_SECRET.to_string(), detached.clone()),
    ];
    expected.sort();
    assert_eq!(notes(&connection), expected);

    assert_eq!(sweep(&connection, root).unwrap(), 0);
    assert!(!secret_file(root, &replaced).exists());
    assert!(!secret_file(root, &detached).exists());
    assert!(secret_file(root, &kept).exists());
    assert!(notes(&connection).is_empty());
}

#[test]
fn a_noted_name_a_live_record_still_holds_is_forgotten_and_kept_on_disk() {
    let (dir, mut connection) = database();
    let root = dir.path();
    let id = server(&mut connection, root, "tools");
    let live = attach(&mut connection, root, &id, "API_TOKEN");
    let checked = check_folder(root, &id);
    let folder = key_folder(root, 0o700);
    let held = key_file(&folder, "lab-key-live");
    saved_account(&connection, "lab-account", "lab-key-live");
    put(&connection, MCP_SECRET, &live).unwrap();
    put(&connection, MCP_CHECKS, &id).unwrap();
    put(&connection, ACCOUNT_KEY, "lab-key-live").unwrap();

    assert_eq!(sweep(&connection, root).unwrap(), 0);
    assert!(secret_file(root, &live).exists());
    assert!(checked.join("project").join("written-by-check").exists());
    assert!(held.exists());
    assert!(notes(&connection).is_empty());
}

#[test]
fn a_removed_account_key_is_swept_and_a_saved_account_keeps_its_key() {
    let (dir, connection) = database();
    let root = dir.path();
    let folder = key_folder(root, 0o700);
    let removed = key_file(&folder, "lab-key-removed");
    let held = key_file(&folder, "lab-key-held");
    saved_account(&connection, "lab-account", "lab-key-held");
    put(&connection, ACCOUNT_KEY, "lab-key-removed").unwrap();
    put(&connection, ACCOUNT_KEY, "lab-key-missing").unwrap();

    assert_eq!(sweep(&connection, root).unwrap(), 0);
    assert!(!removed.exists());
    assert!(held.exists());
    assert!(notes(&connection).is_empty());
}

#[test]
fn a_key_in_an_unsafe_folder_stays_noted_for_the_next_start() {
    let (dir, connection) = database();
    let root = dir.path();
    let folder = key_folder(root, 0o755);
    let stuck = key_file(&folder, "lab-key-stuck");
    put(&connection, ACCOUNT_KEY, "lab-key-stuck").unwrap();

    assert_eq!(sweep(&connection, root).unwrap(), 1);
    assert!(stuck.exists());
    assert_eq!(
        notes(&connection),
        vec![(ACCOUNT_KEY.to_string(), "lab-key-stuck".to_string())]
    );

    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(sweep(&connection, root).unwrap(), 0);
    assert!(!stuck.exists());
}

#[test]
fn a_note_naming_a_path_outside_its_folder_is_refused_and_kept() {
    let (dir, connection) = database();
    let root = dir.path();
    let outside = root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("kept"), b"kept").unwrap();
    put(&connection, MCP_CHECKS, "../outside").unwrap();
    put(&connection, ACCOUNT_KEY, ".hidden").unwrap();

    assert_eq!(sweep(&connection, root).unwrap(), 2);
    assert!(outside.join("kept").exists());
}

#[test]
fn the_next_engine_start_sweeps_what_a_crash_left_behind() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("engine.db");
    let (secret, checked) = {
        let super::super::OpenedDatabase { mut connection, .. } =
            super::super::open_actor_database(&path, false, None).unwrap();
        let id = server(&mut connection, dir.path(), "tools");
        let secret = attach(&mut connection, dir.path(), &id, "API_TOKEN");
        let checked = check_folder(dir.path(), &id);
        let version = crate::storage::plugins::plugin_version(&connection, &id).unwrap();
        execute(
            &mut connection,
            dir.path(),
            Command::RemoveServer { id, version },
        )
        .unwrap();
        assert!(secret_file(dir.path(), &secret).exists());
        (secret, checked)
    };

    let super::super::OpenedDatabase { connection, .. } =
        super::super::open_actor_database(&path, false, None).unwrap();
    assert!(!secret_file(dir.path(), &secret).exists());
    assert!(!checked.exists());
    assert!(notes(&connection).is_empty());
}
