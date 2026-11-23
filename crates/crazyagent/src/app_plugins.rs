use std::path::Path;

use kernel_plugin_host::Refused;
use kernel_plugin_host::registry::Engine;
use kernel_plugin_host::store::{AccountKey, KeyFolder, KeyRef, Keys};
use rusqlite::Connection;

use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use crate::storage::accounts::{self, ACCOUNT_KEYS, PLUGIN_KEYS};

pub mod yaml;

fn folder(folder: KeyFolder) -> &'static str {
    match folder {
        KeyFolder::Account => ACCOUNT_KEYS,
        KeyFolder::Plugin => PLUGIN_KEYS,
    }
}

/// The engine's own reference for a plugin host key reference, byte-equal: both run the
/// same identity rule, so it cannot fail. Input: the reference. Output: the wire reference.
fn credential(reference: &KeyRef) -> CredentialRef {
    let wired = CredentialRef::new(reference.as_str())
        .expect("a plugin key reference is an engine identity");
    assert_eq!(wired.as_str(), reference.as_str());
    wired
}

fn saved(root: &Path, key_folder: KeyFolder, reference: &KeyRef) -> Result<bool, ()> {
    accounts::key_saved_in(root, folder(key_folder), &credential(reference)).map_err(|_| ())
}

fn account(db: &Connection, id: &KeyRef) -> Result<Option<AccountKey>, ()> {
    let found = accounts::api_key_account(db, &credential(id)).map_err(|_| ())?;
    Ok(found.map(|(name, key)| (name, key.and_then(|key| KeyRef::new(key.as_str())))))
}

fn replace(root: &Path, key_folder: KeyFolder, reference: &KeyRef, key: Vec<u8>) -> Result<(), ()> {
    accounts::replace_key_in(root, folder(key_folder), &credential(reference), key).map_err(|_| ())
}

fn remove(root: &Path, key_folder: KeyFolder, reference: &KeyRef) -> Result<(), ()> {
    accounts::remove_key_in(root, folder(key_folder), reference.as_str()).map_err(|_| ())
}

fn read(
    root: &Path,
    key_folder: KeyFolder,
    reference: &KeyRef,
) -> Result<Option<kernel_security::secret::SecretString>, ()> {
    accounts::read_key_in(root, folder(key_folder), &credential(reference)).map_err(|_| ())
}

#[cfg(unix)]
fn sweep(root: &Path, key_folder: KeyFolder) -> Result<(), ()> {
    accounts::remove_staged_in(root, folder(key_folder)).map_err(|_| ())
}

#[cfg(not(unix))]
fn sweep(_root: &Path, _key_folder: KeyFolder) -> Result<(), ()> {
    Ok(())
}

fn new_ref() -> Result<String, &'static str> {
    crate::engine::owner::random_id().map_err(|_| "owner_entropy_unavailable")
}

/// The engine's key store as the plugin host takes it: `storage::accounts` key files in
/// `account-keys` and `plugin-keys`, account lookups in the engine database, and the
/// engine's random ids.
pub(crate) const KEYS: Keys = Keys {
    saved,
    account,
    replace,
    remove,
    read,
    sweep,
    new_ref,
};

/// The ports this machine hands out by itself: Linux reads
/// net.ipv4.ip_local_port_range; macOS keeps every plugin port free of agents in its
/// profile, so none there. Output: the range, or none.
#[cfg(target_os = "linux")]
fn handed_out() -> Option<std::ops::RangeInclusive<u16>> {
    crate::sandbox::linux::read_picked_ports()
}

#[cfg(not(target_os = "linux"))]
fn handed_out() -> Option<std::ops::RangeInclusive<u16>> {
    None
}

/// The engine facts one plugin host call takes. Input: the engine root. Output: them.
pub(crate) fn engine(root: &Path) -> Engine<'_> {
    Engine {
        root,
        keys: &KEYS,
        handed_out: handed_out(),
    }
}

fn error(refused: Refused) -> EngineError {
    EngineError(refused.code.into())
}

/// Creates the plugin store's tables when missing, as every engine open and migration step
/// 12 need. Input: the engine connection. Output: Ok, or `app_plugins_unavailable`.
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    kernel_plugin_host::store::initialize(db).map_err(error)
}

/// Deletes what a crash left staged by the plugin store at the engine root. Input: the
/// engine root. Output: none.
pub(crate) fn sweep_staged(root: &Path) {
    kernel_plugin_host::store::sweep_staged(&engine(root));
}

/// Runs the plugin store's start sweeps: what a crash left staged, then on Unix every
/// entered plugin key file in `plugin-keys` that no binding row names, as a committed
/// removal or a crash between a binding change and its file delete leaves it. Off Unix no
/// plugin key file is ever saved. Inputs: the engine connection and root. Output: none.
pub(crate) fn sweep_at_start(db: &Connection, root: &Path) {
    sweep_staged(root);
    #[cfg(unix)]
    sweep_unnamed_keys(db, root);
    #[cfg(not(unix))]
    let _ = db;
}

/// Deletes every entered plugin key file in `plugin-keys` that no binding row names. A name
/// that is no entered plugin key and the account key folder are never touched, and when the
/// binding rows do not read, no key is deleted. Inputs: the engine connection and root.
/// Output: none.
#[cfg(unix)]
fn sweep_unnamed_keys(db: &Connection, root: &Path) {
    let Ok(named) = kernel_plugin_host::store::named_keys(db) else {
        return;
    };
    let kept =
        |name: &str| Ok(!kernel_plugin_host::store::is_entered_key(name) || named.contains(name));
    let _ = accounts::settle_unkept_in(root, PLUGIN_KEYS, root, kept, |_| Ok(false));
}

#[cfg(all(test, unix))]
#[path = "app_plugins/host_tests.rs"]
mod host_tests;
