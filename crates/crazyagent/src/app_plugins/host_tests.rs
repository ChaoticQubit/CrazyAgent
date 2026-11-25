use std::os::unix::fs::PermissionsExt;

use kernel_plugin_host::store::{KeyFolder, KeyRef};

use super::{KEYS, initialize, sweep_at_start, sweep_staged, yaml};

/// The harness reader keeps its own sentences once the plugin store left the engine: a
/// YAML refusal reads the same words as before, with its line or capitalised without one.
#[test]
fn a_harness_yaml_error_keeps_its_sentence() {
    let tab = yaml::parse_bytes(b"a: 1\n\tb: 2\n").expect_err("a tab is refused");
    assert_eq!(tab.message(), "Line 2: use spaces, not tabs, to indent.");
    let large = yaml::parse_bytes(&vec![b'a'; yaml::BYTES_MAX + 1]).expect_err("too large");
    assert_eq!(large.message(), "The file is larger than 16 KiB.");
}

/// The adapter's key store is the engine's own: a plugin key goes into `plugin-keys` as a
/// private 0600 file, reads back, is swept of staged copies and deleted, and an unknown
/// account reads as none.
#[test]
fn the_engine_key_store_backs_the_plugin_host() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let reference = KeyRef::new(&format!("plugin-key-{}", (KEYS.new_ref)().unwrap())).unwrap();
    assert!(!(KEYS.saved)(&root, KeyFolder::Plugin, &reference).unwrap());
    (KEYS.replace)(
        &root,
        KeyFolder::Plugin,
        &reference,
        b"sk-adapter-key-01".to_vec(),
    )
    .unwrap();
    assert!((KEYS.saved)(&root, KeyFolder::Plugin, &reference).unwrap());
    let file = root
        .join(crate::storage::accounts::PLUGIN_KEYS)
        .join(reference.as_str());
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    let value = (KEYS.read)(&root, KeyFolder::Plugin, &reference)
        .unwrap()
        .unwrap();
    assert_eq!(value.expose(), "sk-adapter-key-01");
    sweep_staged(&root);
    assert!((KEYS.saved)(&root, KeyFolder::Plugin, &reference).unwrap());
    (KEYS.remove)(&root, KeyFolder::Plugin, &reference).unwrap();
    assert!(!(KEYS.saved)(&root, KeyFolder::Plugin, &reference).unwrap());
    let db = rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    crate::storage::accounts::initialize(&db).unwrap();
    let unknown = KeyRef::new("acct-gone").unwrap();
    assert_eq!((KEYS.account)(&db, &unknown), Ok(None));
}

/// The start sweep deletes only plugin key files no binding names: a named plugin key, a
/// file that is no plugin key, and every file in the account key folder stay, and with no
/// readable binding table nothing is deleted.
#[test]
fn the_start_sweep_removes_only_unreferenced_plugin_key_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let key = |name: &str| KeyRef::new(name).unwrap();
    let save = |folder: KeyFolder, name: &str| {
        (KEYS.replace)(&root, folder, &key(name), b"sk-sweep-key-01".to_vec()).unwrap();
    };
    let saved = |folder: KeyFolder, name: &str| (KEYS.saved)(&root, folder, &key(name)).unwrap();
    save(KeyFolder::Plugin, "plugin-key-named");
    save(KeyFolder::Plugin, "plugin-key-orphan");
    save(KeyFolder::Plugin, "acct-other");
    save(KeyFolder::Account, "plugin-key-account");
    let bare = rusqlite::Connection::open_in_memory().unwrap();
    sweep_at_start(&bare, &root);
    assert!(saved(KeyFolder::Plugin, "plugin-key-orphan"));
    let db = rusqlite::Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    db.execute(
        "INSERT INTO app_plugin_secrets (plugin, secret, source, reference)
         VALUES ('acme', 'api_key', 'key', 'plugin-key-named')",
        [],
    )
    .unwrap();
    sweep_at_start(&db, &root);
    assert!(saved(KeyFolder::Plugin, "plugin-key-named"));
    assert!(!saved(KeyFolder::Plugin, "plugin-key-orphan"));
    assert!(saved(KeyFolder::Plugin, "acct-other"));
    assert!(saved(KeyFolder::Account, "plugin-key-account"));
}

/// On Linux the adapter hands the plugin host the range this Linux hands out by
/// itself, read where the sandbox reads it, so a plugin on such a port is refused.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_the_adapter_passes_the_handed_out_range() {
    let temp = tempfile::tempdir().unwrap();
    let engine = super::engine(temp.path());
    let range =
        kernel_sandbox::linux::helper::read_picked_ports().expect("this Linux names its range");
    assert_eq!(engine.handed_out, Some(range));
}

/// The engine's capability reply keeps exactly its six handshake flags: the plugin grant
/// listing is a read of its own, and naming every field without a rest pattern fails to
/// compile if one is added or removed.
#[test]
fn the_handshake_flags_are_unchanged() {
    use crate::engine::owner::{OwnerData, OwnerRead};
    let reply = OwnerData::Capabilities {
        authenticated_owner_transport: true,
        native_worker_admission: false,
        human_consent_proven: true,
        native_input: false,
        merge_execution: true,
        settings_persistence: false,
    };
    let OwnerData::Capabilities {
        authenticated_owner_transport,
        native_worker_admission,
        human_consent_proven,
        native_input,
        merge_execution,
        settings_persistence,
    } = reply
    else {
        panic!("capabilities");
    };
    assert_eq!(
        [
            authenticated_owner_transport,
            native_worker_admission,
            human_consent_proven,
            native_input,
            merge_execution,
            settings_persistence,
        ],
        [true, false, true, false, true, false]
    );
    assert_ne!(OwnerRead::PluginGrants, OwnerRead::Capabilities);
    assert!(OwnerRead::Capabilities.command().unwrap().is_none());
    assert!(OwnerRead::PluginGrants.command().unwrap().is_none());
}

/// The owner read of plugin grants lists the plugins of the engine root its database sits
/// in: the bundled code plugin with its granted capability, the bundled data plugins off.
#[test]
fn the_owner_read_lists_plugin_grants() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    initialize(&db).unwrap();
    let tx = db.transaction().unwrap();
    let crate::engine::owner::OwnerData::PluginGrants { plugins } = super::grants(&tx).unwrap()
    else {
        panic!("plugin grants");
    };
    let ids: Vec<&str> = plugins
        .iter()
        .map(|grants| grants.plugin.as_str())
        .collect();
    assert_eq!(ids, ["agent-quit", "ollama", "openai", "voyage"]);
    assert_eq!(plugins[0].capabilities[0].name, "settings.save_quit");
}

/// A private engine root for one sweep row: the scratch guard and its canonical 0700
/// path. Output: both; a failure stops the row naming what failed.
fn sweep_root() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("scratch folder: {error}"));
    let root = temp
        .path()
        .canonicalize()
        .unwrap_or_else(|error| panic!("{}: {error}", temp.path().display()));
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|error| panic!("{}: {error}", root.display()));
    (temp, root)
}

/// Saves one plugin key file under the engine root. Inputs: the root and the key name.
fn save_plugin_key(root: &std::path::Path, name: &str) {
    let reference = KeyRef::new(name).unwrap_or_else(|error| panic!("{name}: {error:?}"));
    (KEYS.replace)(root, KeyFolder::Plugin, &reference, b"sk-sweep-key-02".to_vec())
        .unwrap_or_else(|error| panic!("save {name}: {error:?}"));
}

/// Whether one plugin key file is still saved. Inputs: the root and the key name.
/// Output: the answer.
fn plugin_key_saved(root: &std::path::Path, name: &str) -> bool {
    let reference = KeyRef::new(name).unwrap_or_else(|error| panic!("{name}: {error:?}"));
    (KEYS.saved)(root, KeyFolder::Plugin, &reference)
        .unwrap_or_else(|error| panic!("read {name}: {error:?}"))
}

/// Makes a folder as this user's 0700 folder. Input: the path.
fn private_folder(path: &std::path::Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Writes a database whose binding rows name the given keys, as a kept copy of replaced
/// 2.5 data holds them. Inputs: the copy's folder, made here, and the key names. Output:
/// the database path.
fn copy_naming(folder: &std::path::Path, keys: &[&str]) -> std::path::PathBuf {
    private_folder(folder);
    let path = folder.join("engine.db");
    let db = rusqlite::Connection::open(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    kernel_plugin_host::store::initialize(&db)
        .unwrap_or_else(|error| panic!("{}: {error:?}", path.display()));
    for (index, key) in keys.iter().enumerate() {
        db.execute(
            "INSERT INTO app_plugin_secrets (plugin, secret, source, reference)
             VALUES (?1, 'api_key', 'key', ?2)",
            [format!("acme{index}"), (*key).to_owned()],
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    path
}

/// A live engine database with the plugin store's tables and no binding row, as 2.4 rows
/// restored and upgraded hold it. Output: the connection.
fn restored_engine() -> rusqlite::Connection {
    let db = rusqlite::Connection::open_in_memory()
        .unwrap_or_else(|error| panic!("in-memory database: {error}"));
    initialize(&db).unwrap_or_else(|error| panic!("plugin store tables: {error:?}"));
    db
}

/// The names in a folder, sorted. Input: the folder. Output: the names.
fn names_in(folder: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap_or_else(|error| panic!("{}: {error}", folder.display()))
        .map(|entry| {
            let entry = entry.unwrap_or_else(|error| panic!("{}: {error}", folder.display()));
            entry.file_name().to_string_lossy().into_owned()
        })
        .collect();
    names.sort();
    names
}

/// A plugin key a kept copy of replaced 2.5 data names stays through the start sweep, as a
/// restore whose key settle stopped early leaves it in `plugin-keys`: an older copy naming
/// it keeps it though the newest copy names nothing, a key no copy and no row names is
/// still deleted, and the copies are read only, their database bytes and names unchanged.
#[test]
fn a_key_a_kept_replaced_copy_names_survives_the_start_sweep() {
    let (_temp, root) = sweep_root();
    let backups = root.join("backups");
    private_folder(&backups);
    let older = copy_naming(&backups.join("replaced-20261101T000000Z"), &["plugin-key-kept"]);
    let newer = copy_naming(&backups.join("replaced-20261120T000000Z"), &[]);
    let (older_bytes, newer_bytes) = (fixture_bytes(&older), fixture_bytes(&newer));
    save_plugin_key(&root, "plugin-key-kept");
    save_plugin_key(&root, "plugin-key-orphan");

    sweep_at_start(&restored_engine(), &root);

    assert!(plugin_key_saved(&root, "plugin-key-kept"));
    assert!(!plugin_key_saved(&root, "plugin-key-orphan"));
    assert_eq!(fixture_bytes(&older), older_bytes);
    assert_eq!(fixture_bytes(&newer), newer_bytes);
    for copy in [&older, &newer] {
        let folder = copy.parent().unwrap_or_else(|| panic!("{} has no folder", copy.display()));
        assert_eq!(names_in(folder), ["engine.db"], "{}", folder.display());
    }
}

/// A kept copy of replaced data the sweep cannot read keeps every plugin key, as one a
/// failed settle could not open: its own `replaced-` folder whose database is not one, or
/// more copies than the sweep reads. A link named as a copy, or a folder with another
/// name, is not read and keeps nothing.
#[test]
fn an_unreadable_replaced_copy_keeps_every_plugin_key() {
    let (_temp, root) = sweep_root();
    let backups = root.join("backups");
    private_folder(&backups);
    let outside = root.join("outside");
    copy_naming(&outside, &["plugin-key-orphan"]);
    std::os::unix::fs::symlink(&outside, backups.join("replaced-20261102T000000Z"))
        .unwrap_or_else(|error| panic!("{}: {error}", backups.display()));
    copy_naming(&backups.join("11-to-12-20261103T000000Z"), &["plugin-key-orphan"]);
    save_plugin_key(&root, "plugin-key-orphan");
    sweep_at_start(&restored_engine(), &root);
    assert!(!plugin_key_saved(&root, "plugin-key-orphan"));

    let unreadable = backups.join("replaced-20261104T000000Z");
    private_folder(&unreadable);
    let database = unreadable.join("engine.db");
    std::fs::write(&database, b"not a database, as a cut copy reads")
        .unwrap_or_else(|error| panic!("{}: {error}", database.display()));
    save_plugin_key(&root, "plugin-key-orphan");
    sweep_at_start(&restored_engine(), &root);
    assert!(plugin_key_saved(&root, "plugin-key-orphan"));

    std::fs::remove_dir_all(&unreadable)
        .unwrap_or_else(|error| panic!("{}: {error}", unreadable.display()));
    let template = copy_naming(&root.join("template"), &[]);
    for index in 0..=crate::storage::migrations::REPLACED_SCAN_MAX {
        let copy = backups.join(format!("replaced-20261105T{index:06}Z"));
        private_folder(&copy);
        std::fs::copy(&template, copy.join("engine.db"))
            .unwrap_or_else(|error| panic!("{}: {error}", copy.display()));
    }
    sweep_at_start(&restored_engine(), &root);
    assert!(plugin_key_saved(&root, "plugin-key-orphan"));
}

/// Reads a fixture's bytes. Input: the path. Output: the bytes; a failed read stops the
/// row naming the path and the I/O error.
fn fixture_bytes(path: &std::path::Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}
