use std::os::unix::fs::PermissionsExt;

use kernel_plugin_host::store::{KeyFolder, KeyRef};

use super::{KEYS, initialize, sweep_staged, yaml};

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
    let db = rusqlite::Connection::open(root.join("engine.db")).unwrap();
    initialize(&db).unwrap();
    let crate::engine::owner::OwnerData::PluginGrants { plugins } = super::grants(&db).unwrap()
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
