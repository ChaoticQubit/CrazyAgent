use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use kernel_plugin_host::Refused;
use kernel_plugin_host::registry::{self, Engine, Switch};
use kernel_plugin_host::store::{self, AccountKey, KeyFolder, KeyRef, Keys};
use kernel_plugin_host::usage::{ChangeNotice, Holder, TurnOff, Usage};
use kernel_plugin_host::views::{Listing, Preview};
use rusqlite::Connection;

use crate::config::PluginsPolicy;
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use crate::storage::accounts::{self, ACCOUNT_KEYS, PLUGIN_KEYS};

pub mod words;
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
/// 12 need, then loads the engine's off set from them. Input: the engine connection.
/// Output: Ok, or `app_plugins_unavailable`.
pub(crate) fn initialize(db: &Connection) -> Result<(), EngineError> {
    kernel_plugin_host::store::initialize(db).map_err(error)?;
    switches().load(db);
    Ok(())
}

/// The bundled code plugins the owner turned off, held in memory so the gateway reads no
/// database per call. Until a load succeeds every plugin reads as off.
pub(crate) struct OffSet {
    off: RwLock<Option<BTreeSet<String>>>,
}

impl OffSet {
    /// An off set not yet loaded, which reads every plugin as off. Output: the set.
    pub(crate) const fn new() -> Self {
        Self {
            off: RwLock::new(None),
        }
    }

    /// Replaces the set with the switch rows the database holds; a read that fails leaves
    /// it unloaded. Input: the engine connection. Output: none.
    pub(crate) fn load(&self, db: &Connection) {
        let loaded = kernel_plugin_host::registry::off_code_plugins(db).ok();
        *self.off.write().unwrap_or_else(PoisonError::into_inner) = loaded;
    }

    /// Switches a bundled code plugin and, once the switch has committed, loads the set
    /// again. A connection inside an open transaction is refused before anything is
    /// written, so the set never shows a switch that could still roll back. Inputs: the
    /// engine connection, the engine facts, the plugin id and the switch. Output: Ok, the
    /// plugin host's refusal code, or `app_plugins_unavailable`.
    pub(crate) fn switch(
        &self,
        db: &Connection,
        engine: &Engine<'_>,
        plugin: &str,
        switch: Switch<'_>,
    ) -> Result<(), EngineError> {
        if !db.is_autocommit() {
            return Err(EngineError("app_plugins_unavailable".into()));
        }
        kernel_plugin_host::registry::set_enabled(db, engine, plugin, switch).map_err(error)?;
        assert!(db.is_autocommit());
        self.load(db);
        Ok(())
    }
}

impl bus_gateway::Switches for OffSet {
    fn is_on(&self, plugin: &str) -> bool {
        match &*self.off.read().unwrap_or_else(PoisonError::into_inner) {
            Some(off) => !off.contains(plugin),
            None => false,
        }
    }
}

/// The engine's one off set, loaded by [`initialize`] when the engine opens its database.
/// Output: the set.
pub(crate) fn switches() -> &'static OffSet {
    static OFF: OffSet = OffSet::new();
    &OFF
}

/// One App plugins owner request, run on the engine connection in the storage worker. It
/// carries an entered key's bytes, so it has no Debug form.
pub(crate) enum Command {
    List,
    Preview {
        folder: PathBuf,
    },
    Review {
        name: String,
    },
    Add {
        folder: PathBuf,
        sha256: String,
        replace: bool,
    },
    Trust {
        name: String,
        sha256: String,
    },
    SetEnabled {
        name: String,
        enabled: bool,
        when_off: Option<TurnOff>,
    },
    Users {
        name: String,
    },
    Remove {
        name: String,
    },
    BindAccount {
        name: String,
        secret: String,
        account: String,
    },
    SetKey {
        name: String,
        secret: String,
        key: Vec<u8>,
    },
    Unbind {
        name: String,
        secret: String,
    },
}

/// What one App plugins request answers: the listing with the change notices drained after
/// it, a trust screen, or the runs holding a plugin.
#[derive(Debug)]
pub(crate) enum Done {
    Page {
        listing: Listing,
        notices: Vec<ChangeNotice>,
    },
    Preview(Preview),
    Users(Vec<Holder>),
}

/// The engine's one record of which runs hold which plugin and of the change notices not
/// yet shown. Output: the record.
pub(crate) fn usage() -> &'static Usage {
    static USAGE: std::sync::LazyLock<Usage> = std::sync::LazyLock::new(Usage::default);
    &USAGE
}

/// Runs one App plugins request on the engine connection outside a transaction. A change
/// answers with the listing after it. Turning a plugin off follows the owner's setting for
/// runs holding it unless the request picks a choice for this one call. Inputs: the engine
/// connection, the owner's plugin settings and the command. Output: the answer, or the
/// plugin store's refusal code.
pub(crate) fn execute(
    db: &Connection,
    policy: &PluginsPolicy,
    command: Command,
) -> Result<Done, EngineError> {
    let root = crate::storage::trash::engine_root(db)?;
    let engine = engine(root);
    let now = crate::team::now_ms();
    match command {
        Command::List => {}
        Command::Preview { folder } => {
            return registry::preview(db, &engine, &folder)
                .map(Done::Preview)
                .map_err(error);
        }
        Command::Review { name } => {
            return registry::review(db, &engine, &name)
                .map(Done::Preview)
                .map_err(error);
        }
        Command::Users { name } => return Ok(Done::Users(usage().users(&name))),
        Command::Add {
            folder,
            sha256,
            replace,
        } => registry::add(db, &engine, &folder, &sha256, replace, now).map_err(error)?,
        Command::Trust { name, sha256 } => {
            kernel_plugin_host::trust::trust_file(db, &engine, &name, &sha256, now)
                .map_err(error)?;
        }
        Command::SetEnabled {
            name,
            enabled,
            when_off,
        } => {
            let switch = if enabled {
                Switch::On
            } else {
                Switch::Off {
                    usage: usage(),
                    when_off: when_off.unwrap_or(policy.when_turned_off),
                    essential_unlocked: policy.essential_unlocked,
                }
            };
            switches().switch(db, &engine, &name, switch)?;
        }
        Command::Remove { name } => registry::remove(db, &engine, &name).map_err(error)?,
        Command::BindAccount {
            name,
            secret,
            account,
        } => store::bind_account(db, &engine, &name, &secret, &account).map_err(error)?,
        Command::SetKey { name, secret, key } => {
            store::set_key(db, &engine, &name, &secret, key).map_err(error)?;
        }
        Command::Unbind { name, secret } => {
            store::unbind(db, &engine, &name, &secret).map_err(error)?;
        }
    }
    let listing = registry::list(db, &engine).map_err(error)?;
    usage().note_listed(&listing);
    Ok(Done::Page {
        listing,
        notices: usage().take_notices(),
    })
}

/// Deletes what a crash left staged by the plugin store at the engine root. Input: the
/// engine root. Output: none.
pub(crate) fn sweep_staged(root: &Path) {
    kernel_plugin_host::store::sweep_staged(&engine(root));
}

/// The owner read of every plugin with the capabilities it is granted, for the engine root
/// the database sits in. Input: the engine connection. Output: the listing as owner data,
/// or the plugin host's refusal code, or `file_trash_root_unknown` for an in-memory database.
pub(crate) fn grants(db: &Connection) -> Result<crate::engine::owner::OwnerData, EngineError> {
    let root = crate::storage::trash::engine_root(db)?;
    let plugins = kernel_plugin_host::grants::list(db, &engine(root)).map_err(error)?;
    assert!(plugins.len() <= kernel_plugin_host::grants::GRANTS_MAX);
    Ok(crate::engine::owner::OwnerData::PluginGrants { plugins })
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

#[cfg(test)]
#[path = "app_plugins/switch_tests.rs"]
mod switch_tests;
#[cfg(test)]
#[path = "app_plugins/words_tests.rs"]
mod words_tests;
