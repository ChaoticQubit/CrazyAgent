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
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 1);
    assert!(stuck.exists());
    assert_eq!(
        notes(&connection),
        vec![(ACCOUNT_KEY.to_string(), "lab-key-stuck".to_string())]
    );

    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
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

    assert_eq!(sweep(&connection, root, None).unwrap(), 2);
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

const DUMMY: &[u8] = b"dummy";

fn private_folder(path: &Path) {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
}

fn private_file(path: &Path) {
    std::fs::write(path, DUMMY).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn digest_of(name: &str) -> String {
    format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(name.as_bytes())
    )
}

struct Slot {
    folder: PathBuf,
    home: PathBuf,
    files: [PathBuf; 3],
}

/// A synthetic managed profile slot under `root` holding dummy sign-in files, a manifest and
/// one other file of its home. Inputs: the root and the profile reference. Output: its paths.
fn slot(root: &Path, name: &str) -> Slot {
    let folder = root.join("native-profiles").join(digest_of(name));
    let home = folder.join("home");
    private_folder(&home.join("Library/Keychains"));
    private_file(&folder.join("identity.json"));
    private_file(&home.join("keep.txt"));
    let files = [
        home.join("auth.json"),
        home.join(".credentials.json"),
        home.join("Library/Keychains/login.keychain-db"),
    ];
    for file in &files {
        private_file(file);
    }
    Slot {
        folder,
        home,
        files,
    }
}

/// A synthetic generated SDK home under `root` holding a dummy key.
/// Inputs: the root and the credential reference. Output: the home's path.
fn generated(root: &Path, name: &str) -> PathBuf {
    let home = root.join("configured-sdk-homes").join(digest_of(name));
    private_folder(&home.join(".backboard"));
    private_file(&home.join(".backboard/config.json"));
    private_file(&home.join(".backboard/keys.json"));
    home
}

/// SECAUDIT account-removal-leaves-native-managed-profile-credentials: a removed native
/// account loses each sign-in file, and the rest of its slot stays.
#[test]
fn a_removed_native_account_loses_its_sign_in_files_and_keeps_the_rest_of_its_slot() {
    let (dir, connection) = database();
    let root = dir.path();
    let slot = slot(root, "lab-profile");
    put(&connection, NATIVE_SIGN_IN, "lab-profile").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    for file in &slot.files {
        assert!(!file.exists(), "{}", file.display());
    }
    assert!(slot.folder.join("identity.json").exists());
    assert!(slot.home.join("keep.txt").exists());
    assert!(notes(&connection).is_empty());
}

/// SECAUDIT configured-rcli-home-keeps-provider-key: a removed API key account's generated
/// home goes whole, and a sibling home stays.
#[test]
fn a_removed_api_key_accounts_generated_home_goes_whole_and_a_sibling_stays() {
    let (dir, connection) = database();
    let root = dir.path();
    let removed = generated(root, "lab-key");
    let sibling = generated(root, "lab-other");
    put(&connection, GENERATED_HOME, "lab-key").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    assert!(!removed.exists());
    assert!(sibling.join(".backboard/keys.json").exists());
    assert!(notes(&connection).is_empty());
}

/// Condition 1: a sign-in name that is a link goes, and what it points at is untouched.
#[test]
fn a_sign_in_file_that_is_a_link_goes_and_its_target_stays() {
    let (dir, connection) = database();
    let root = dir.path();
    let slot = slot(root, "lab-profile");
    let outside = root.join("outside-target");
    private_file(&outside);
    std::fs::remove_file(&slot.files[0]).unwrap();
    std::os::unix::fs::symlink(&outside, &slot.files[0]).unwrap();
    put(&connection, NATIVE_SIGN_IN, "lab-profile").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    assert!(std::fs::symlink_metadata(&slot.files[0]).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), DUMMY);
}

/// Condition 1: a folder of the walk that is a link is not followed, and the note stays.
#[test]
fn a_keychains_folder_that_is_a_link_is_not_followed_and_stays_noted() {
    let (dir, connection) = database();
    let root = dir.path();
    let slot = slot(root, "lab-profile");
    let outside = root.join("outside");
    private_folder(&outside);
    private_file(&outside.join("login.keychain-db"));
    let keychains = slot.home.join("Library/Keychains");
    std::fs::remove_dir_all(&keychains).unwrap();
    std::os::unix::fs::symlink(&outside, &keychains).unwrap();
    put(&connection, NATIVE_SIGN_IN, "lab-profile").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 1);
    assert!(outside.join("login.keychain-db").exists());
    assert_eq!(
        notes(&connection),
        vec![(NATIVE_SIGN_IN.to_string(), "lab-profile".to_string())]
    );
}

/// Condition 1: a generated home that is a link is not followed, and the note stays.
#[test]
fn a_generated_home_that_is_a_link_is_not_followed_and_stays_noted() {
    let (dir, connection) = database();
    let root = dir.path();
    let outside = root.join("outside");
    private_folder(&outside.join(".backboard"));
    private_file(&outside.join(".backboard/keys.json"));
    private_folder(&root.join("configured-sdk-homes"));
    std::os::unix::fs::symlink(
        &outside,
        root.join("configured-sdk-homes").join(digest_of("lab-key")),
    )
    .unwrap();
    put(&connection, GENERATED_HOME, "lab-key").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 1);
    assert!(outside.join(".backboard/keys.json").exists());
}

/// The removal's bound: a generated home at the file limit goes whole; one file more keeps
/// the folder and the note, and its key is gone all the same.
#[test]
fn a_generated_home_past_its_bound_loses_its_key_and_stays_noted() {
    let (dir, connection) = database();
    let root = dir.path();
    for (name, extra, kept) in [("lab-at", 7, false), ("lab-past", 8, true)] {
        let home = generated(root, name);
        for index in 0..extra {
            private_file(&home.join(format!(".backboard/extra-{index}")));
        }
        put(&connection, GENERATED_HOME, name).unwrap();
        sweep(&connection, root, None).unwrap();
        assert!(!home.join(".backboard/keys.json").exists(), "{name}");
        assert_eq!(home.exists(), kept, "{name}");
    }
    assert_eq!(notes(&connection).len(), 1);
}

/// Condition 2: an API key account added again under the same reference before the sweep
/// keeps its new home and key.
#[test]
fn an_api_key_account_added_again_before_the_sweep_keeps_its_generated_home() {
    let (dir, connection) = database();
    let root = dir.path();
    let home = generated(root, "lab-key");
    put(&connection, GENERATED_HOME, "lab-key").unwrap();
    saved_account(&connection, "lab-again", "lab-key");

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    assert!(home.join(".backboard/keys.json").exists());
    assert!(notes(&connection).is_empty());
}

/// Condition 3: a slot another account row still names is not touched.
#[test]
fn a_slot_another_account_still_names_is_not_touched() {
    let (dir, connection) = database();
    let root = dir.path();
    let slot = slot(root, "lab-profile");
    put(&connection, NATIVE_SIGN_IN, "lab-profile").unwrap();
    saved_account(&connection, "lab-other", "lab-profile");

    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    for file in &slot.files {
        assert!(file.exists(), "{}", file.display());
    }
    assert!(notes(&connection).is_empty());
}

/// A profile an agent holds keeps its sign-in and its note, and loses both once it is free.
#[test]
fn a_profile_in_use_stays_noted_and_loses_its_sign_in_once_free() {
    let (dir, connection) = database();
    let root = dir.path();
    let slot = slot(root, "lab-profile");
    let lease = std::fs::File::open(slot.folder.join("identity.json")).unwrap();
    rustix::fs::flock(&lease, rustix::fs::FlockOperation::NonBlockingLockShared).unwrap();
    put(&connection, NATIVE_SIGN_IN, "lab-profile").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 1);
    assert!(slot.files[0].exists());
    drop(lease);
    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    assert!(!slot.files[0].exists());
}

/// A profile being renewed keeps its sign-in and its note, and loses both once it is free.
#[test]
fn a_profile_being_renewed_stays_noted_and_loses_its_sign_in_once_free() {
    let (dir, connection) = database();
    let root = dir.path();
    let slot = slot(root, "lab-profile");
    let renewal = std::fs::File::open(&slot.folder).unwrap();
    rustix::fs::flock(
        &renewal,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
    put(&connection, NATIVE_SIGN_IN, "lab-profile").unwrap();

    assert_eq!(sweep(&connection, root, None).unwrap(), 1);
    assert!(slot.files[0].exists());
    drop(renewal);
    assert_eq!(sweep(&connection, root, None).unwrap(), 0);
    assert!(!slot.files[0].exists());
}

/// Condition 4: files a crash left between the note and the delete go at the next start.
#[tokio::test]
async fn the_next_engine_start_removes_the_account_files_a_crash_left_behind() {
    let case = Catalogued::new(&[], "");
    let home = generated(case.root.path(), "lab-key");
    let slot = slot(case.root.path(), "lab-profile");
    put(&case.db, GENERATED_HOME, "lab-key").unwrap();
    put(&case.db, NATIVE_SIGN_IN, "lab-profile").unwrap();
    let root = case.close();
    let _open = started(root.path()).await;
    assert!(notes(&Connection::open(root.path().join("engine.db")).unwrap()).is_empty());
    assert!(!home.exists());
    assert!(!slot.files[0].exists());
}

const LAB_PROFILE: &str = r#"provider = "openai"
harness = "codex"
protocol = "native"
account_ref = "account"
endpoint = { kind="native" }
auth = { method="native_managed", profile_ref="lab-profile" }
"#;

const LAB_KEY: &str = r#"provider = "openai-compatible"
harness = "rcli"
protocol = "openai-chat"
account_ref = "account"
endpoint = { kind="http", url="http://127.0.0.1:8000/v1" }
auth = { method="api_key", credential_ref="lab-key" }
"#;

/// A real database with a live owner grant, on real settings whose catalog holds one
/// connection per id, each with the given lines. It writes no account row.
struct Catalogued {
    root: tempfile::TempDir,
    db: Connection,
    _lock: super::super::EngineLock,
    sources: super::super::settings::SettingsSources,
    config: crate::config::EffectiveConfig,
    admission: crate::engine::owner::Admission,
}

impl Catalogued {
    fn new(ids: &[&str], shared: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ca-trash-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut text = String::from(
            "version = 1\ntimeout_ms = 5000\n[model_access]\naccounts = [\"account\"]\n",
        );
        for id in ids {
            text.push_str(&format!(
                "[[model_access.connections]]\nid = \"{id}\"\nmodel = {{ id=\"model-{id}\", author=\"openai\" }}\n{shared}"
            ));
        }
        let global = root.path().join("global.toml");
        std::fs::write(&global, text).unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let sources = super::super::settings::SettingsSources::open(
            &global,
            &root.path().join("project.toml"),
        )
        .unwrap();
        let config = sources.config().unwrap();
        let super::super::OpenedDatabase {
            mut connection,
            lock,
            ..
        } = super::super::open_actor_database(&root.path().join("engine.db"), false, None).unwrap();
        super::super::accounts::initialize(&connection).unwrap();
        let started = super::super::owner_start(&mut connection).unwrap();
        let admission = started.owners[0].admission.clone();
        Self {
            root,
            db: connection,
            _lock: lock,
            sources,
            config,
            admission,
        }
    }

    /// Saves the account row of one catalog connection. Input: its id. Output: none.
    fn save_row(&self, id: &str) {
        let connection = self
            .config
            .settings
            .model_access
            .as_ref()
            .unwrap()
            .connections
            .iter()
            .find(|c| c.id.as_str() == id)
            .unwrap();
        let details = AccountDetails {
            name: "Lab".into(),
            provider: Provider::OpenaiCompatible,
        };
        let body =
            serde_json::to_vec(&serde_json::json!({"connection": connection, "details": details}))
                .unwrap();
        self.db
            .execute(
                "INSERT INTO accounts(id, body) VALUES (?1, ?2)",
                params![id, body],
            )
            .unwrap();
    }

    /// Runs the real account removal. Input: the id. Output: none.
    fn remove(&mut self, id: &str) {
        let removed = super::super::accounts::execute(
            &mut self.db,
            &mut self.config,
            Some(&self.sources),
            &self.admission,
            super::super::accounts::Command::Remove(CredentialRef::new(id).unwrap()),
        )
        .unwrap();
        assert!(matches!(
            removed,
            super::super::accounts::ResultValue::Removed(_)
        ));
    }

    /// Closes the database and settings so an engine start can take them. Output: the root.
    fn close(self) -> tempfile::TempDir {
        let Self { root, .. } = self;
        root
    }
}

/// Starts real engine storage on a folder, the way the engine does. Input: the folder.
/// Output: the storage and the settings it recovered.
async fn started(root: &Path) -> (super::super::Storage, crate::config::EffectiveConfig) {
    super::super::Storage::open_with_settings_sources(
        &root.join("engine.db"),
        &root.join("global.toml"),
        &root.join("project.toml"),
    )
    .await
    .unwrap()
}

fn in_settings(config: &crate::config::EffectiveConfig, id: &str) -> bool {
    config
        .settings
        .model_access
        .as_ref()
        .is_some_and(|catalog| catalog.connections.iter().any(|c| c.id.as_str() == id))
}

/// Removes connection a beside connection b on one profile and checks that b can still
/// launch. Input: the connections that have an account row. Output: none.
fn removed_beside_another_keeps_the_sign_in(rows: &[&str]) {
    let mut case = Catalogued::new(&["a", "b"], LAB_PROFILE);
    let slot = slot(case.root.path(), "lab-profile");
    for id in rows {
        case.save_row(id);
    }
    case.remove("a");
    let other =
        super::super::native_profile_connection(&case.config, &CredentialRef::new("b").unwrap())
            .unwrap();
    assert_eq!(
        other.auth,
        ModelAuthentication::NativeManaged {
            profile_ref: CredentialRef::new("lab-profile").unwrap()
        }
    );
    assert!(!in_settings(&case.config, "a"));
    assert!(slot.folder.join("identity.json").exists());
    for file in &slot.files {
        assert!(file.exists(), "{}", file.display());
    }
    assert!(notes(&case.db).is_empty());
}

/// r2 sec F1: both connections have an account row.
#[test]
fn a_connection_removed_beside_another_keeps_the_sign_in_when_both_have_rows() {
    removed_beside_another_keeps_the_sign_in(&["a", "b"]);
}

/// r2 sec F1: only the removed connection has an account row, so only the catalog names the
/// profile afterwards.
#[test]
fn a_connection_removed_beside_another_keeps_the_sign_in_when_only_it_has_a_row() {
    removed_beside_another_keeps_the_sign_in(&["a"]);
}

/// r2 sec F1: only the other connection has an account row.
#[test]
fn a_connection_removed_beside_another_keeps_the_sign_in_when_only_the_other_has_a_row() {
    removed_beside_another_keeps_the_sign_in(&["b"]);
}

/// r2 sec F1: neither connection has an account row.
#[test]
fn a_connection_removed_beside_another_keeps_the_sign_in_when_neither_has_a_row() {
    removed_beside_another_keeps_the_sign_in(&[]);
}

/// r2 sec F1: the generated home of a key another connection still uses stays.
#[test]
fn an_api_key_connection_removed_beside_another_keeps_the_generated_home() {
    let mut case = Catalogued::new(&["a", "b"], LAB_KEY);
    let home = generated(case.root.path(), "lab-key");
    case.remove("a");
    assert!(in_settings(&case.config, "b"));
    assert!(home.join(".backboard/keys.json").exists());
    assert!(
        !notes(&case.db)
            .iter()
            .any(|(kind, _)| kind == GENERATED_HOME)
    );
}

/// r2 sec F1: the check holds the sign-in only while a connection is left to use it.
#[test]
fn a_profile_loses_its_sign_in_once_its_last_connection_is_removed() {
    let mut case = Catalogued::new(&["a", "b"], LAB_PROFILE);
    let slot = slot(case.root.path(), "lab-profile");
    case.remove("a");
    assert!(slot.files[0].exists());
    case.remove("b");
    for file in &slot.files {
        assert!(!file.exists(), "{}", file.display());
    }
    assert!(slot.folder.join("identity.json").exists());
    assert!(notes(&case.db).is_empty());
}

/// r2 sec F1: a note left from an earlier removal meets a connection added since, at start.
#[tokio::test]
async fn the_start_sweep_keeps_a_sign_in_a_catalog_connection_names() {
    let case = Catalogued::new(&["b"], LAB_PROFILE);
    let slot = slot(case.root.path(), "lab-profile");
    put(&case.db, NATIVE_SIGN_IN, "lab-profile").unwrap();
    let root = case.close();
    let _open = started(root.path()).await;
    for file in &slot.files {
        assert!(file.exists(), "{}", file.display());
    }
    assert!(notes(&Connection::open(root.path().join("engine.db")).unwrap()).is_empty());
}

/// r2 sec F1: the sweep reads the catalog a crash left journaled, not the older file.
#[tokio::test]
async fn the_start_sweep_reads_the_catalog_after_a_pending_settings_write_is_recovered() {
    use super::super::{Cursor, OwnerMutation, OwnerOperation, OwnerRequest, RequestId, settings};
    let reference = |value: &str| CredentialRef::new(value).unwrap();
    let mut case = Catalogued::new(&["a"], &LAB_PROFILE.replace("lab-profile", "other-profile"));
    let slot = slot(case.root.path(), "lab-profile");
    let mut second = case
        .config
        .settings
        .model_access
        .as_ref()
        .unwrap()
        .connections[0]
        .clone();
    second.id = reference("b");
    second.model.id = "model-b".into();
    second.auth = ModelAuthentication::NativeManaged {
        profile_ref: reference("lab-profile"),
    };
    let revision = {
        let tx = case.db.transaction().unwrap();
        super::super::owner_revision(&tx).unwrap()
    };
    let request = OwnerRequest {
        protocol_version: crate::protocol::PROTOCOL_VERSION,
        request_id: RequestId::new("pending-connection").unwrap(),
        expected_revision: Cursor::new(revision),
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ModelAccess {
                expected_config_id: case.config.id.clone(),
                expected_source_hash: settings::verified_source_hash(
                    &case.config,
                    Some(&case.sources),
                )
                .unwrap(),
                edit: crate::config::ModelAccessEdit::CreateConnection { connection: second },
            },
        },
    };
    case.db
        .execute_batch("CREATE TRIGGER publication_abort BEFORE UPDATE ON settings_publication BEGIN SELECT RAISE(ABORT, 'dummy_abort'); END;")
        .unwrap();
    assert!(
        settings::apply(
            &mut case.db,
            &mut case.config,
            Some(&case.sources),
            &case.admission,
            &request
        )
        .is_err()
    );
    assert!(settings::pending(&case.db).unwrap());
    case.db
        .execute_batch("DROP TRIGGER publication_abort")
        .unwrap();
    assert!(!in_settings(&case.sources.config().unwrap(), "b"));
    put(&case.db, NATIVE_SIGN_IN, "lab-profile").unwrap();
    let root = case.close();
    let (_open, recovered) = started(root.path()).await;
    assert!(in_settings(&recovered, "b"));
    for file in &slot.files {
        assert!(file.exists(), "{}", file.display());
    }
    assert!(notes(&Connection::open(root.path().join("engine.db")).unwrap()).is_empty());
}
