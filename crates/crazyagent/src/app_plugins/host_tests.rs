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
    let range = crate::sandbox::linux::read_picked_ports().expect("this Linux names its range");
    assert_eq!(engine.handed_out, Some(range));
}
