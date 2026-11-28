use std::path::{Path, PathBuf};

use kernel_security::secret::SecretString;
use rusqlite::{Connection, OptionalExtension, params};

use crate::Refused;
use crate::embedding::Manifest;
use crate::registry::{self, Engine};
use crate::views::{Binding, Origin};

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS app_plugin_trust (
    name TEXT PRIMARY KEY CHECK(length(name) BETWEEN 1 AND 64),
    sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
    hosts TEXT NOT NULL CHECK(length(hosts) <= 4096),
    trusted_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS app_plugin_state (
    name TEXT PRIMARY KEY CHECK(length(name) BETWEEN 1 AND 64),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1))
);
CREATE TABLE IF NOT EXISTS app_plugin_secrets (
    plugin TEXT NOT NULL CHECK(length(plugin) BETWEEN 1 AND 64),
    secret TEXT NOT NULL CHECK(length(secret) BETWEEN 1 AND 64),
    source TEXT NOT NULL CHECK(source IN ('account', 'key')),
    reference TEXT NOT NULL CHECK(length(reference) BETWEEN 1 AND 128),
    PRIMARY KEY (plugin, secret)
);";

const KEY_PREFIX: &str = "plugin-key-";
const BINDINGS_MAX: i64 = 64;
const ACCOUNT: &str = "account";
const KEY: &str = "key";

/// Creates the store's three tables when missing: trust records, on and off switches and
/// secret bindings. The engine runs it at every open and migration step 12 runs it inside
/// the upgrade. Input: the engine connection. Output: Ok, or `app_plugins_unavailable`.
pub fn initialize(db: &Connection) -> Result<(), Refused> {
    db.execute_batch(SCHEMA).map_err(sqlite)
}

/// Builds a refusal that names no line. Input: the code. Output: the refusal.
pub(crate) fn refused(code: &'static str) -> Refused {
    Refused { code, line: 1 }
}

pub(crate) fn sqlite(_: rusqlite::Error) -> Refused {
    refused("app_plugins_unavailable")
}

/// The engine folder a saved key lives in: an API-key account's keys, or the keys the owner
/// entered for a plugin. The engine maps each to its own folder name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFolder {
    Account,
    Plugin,
}

impl KeyFolder {
    /// Both key folders, in the order the start sweep clears them.
    pub const ALL: [KeyFolder; 2] = [KeyFolder::Account, KeyFolder::Plugin];
}

/// The name of one saved key file or one account, as the bindings table stores it: an
/// engine identity (`kernel_common::names::validate_identity`). It only ever names a key,
/// never holds one. Made only by [`KeyRef::new`], which runs that check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRef(String);

impl KeyRef {
    /// Checks a stored or entered reference. Input: the text. Output: the reference, or
    /// none for text the identity rule refuses.
    pub fn new(text: &str) -> Option<Self> {
        kernel_common::names::validate_identity(text).ok()?;
        Some(Self(text.to_owned()))
    }

    /// Lends the reference text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// What the key store knows of one saved account: its display name and the reference of its
/// API key, none for an account that does not sign in with one.
pub type AccountKey = (String, Option<KeyRef>);

/// The engine's key store, handed in by the engine as plain functions: whether a key is
/// saved, an account's display name and key reference, replace, remove and read one key,
/// sweep a key folder's staged copies, and a fresh random id for a new key or staged name.
/// Each error is the store's own, which the plugin host names with its own code.
#[derive(Debug, Clone, Copy)]
pub struct Keys {
    pub saved: fn(&Path, KeyFolder, &KeyRef) -> Result<bool, ()>,
    pub account: fn(&Connection, &KeyRef) -> Result<Option<AccountKey>, ()>,
    pub replace: fn(&Path, KeyFolder, &KeyRef, Vec<u8>) -> Result<(), ()>,
    pub remove: fn(&Path, KeyFolder, &KeyRef) -> Result<(), ()>,
    pub read: fn(&Path, KeyFolder, &KeyRef) -> Result<Option<SecretString>, ()>,
    pub sweep: fn(&Path, KeyFolder) -> Result<(), ()>,
    pub new_ref: fn() -> Result<String, &'static str>,
}

/// A plugin ready to run: its manifest, where it came from and where the key behind each
/// declared secret is saved, in declaration order. It holds no key.
#[derive(Debug)]
pub struct Loaded {
    pub manifest: Manifest,
    pub origin: Origin,
    pub bindings: Vec<Bound>,
}

/// Where one bound secret's key is saved: an API-key account's saved key, or a key the
/// owner entered for the plugin. It holds the reference only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    Account(KeyRef),
    Key(KeyRef),
}

impl Bound {
    /// The folder the key is saved in.
    pub fn folder(&self) -> KeyFolder {
        match self {
            Bound::Account(_) => KeyFolder::Account,
            Bound::Key(_) => KeyFolder::Plugin,
        }
    }

    /// The saved key's reference.
    pub fn reference(&self) -> &KeyRef {
        match self {
            Bound::Account(reference) | Bound::Key(reference) => reference,
        }
    }
}

pub(crate) fn binding_row(
    db: &Connection,
    plugin: &str,
    secret: &str,
) -> Result<Option<(String, String)>, Refused> {
    db.query_row(
        "SELECT source, reference FROM app_plugin_secrets WHERE plugin=?1 AND secret=?2",
        [plugin, secret],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(sqlite)
}

pub(crate) fn binding_rows(
    db: &Connection,
    plugin: &str,
) -> Result<Vec<(String, String, String)>, Refused> {
    let mut statement = db
        .prepare(
            "SELECT secret, source, reference FROM app_plugin_secrets WHERE plugin=?1
             ORDER BY secret LIMIT ?2",
        )
        .map_err(sqlite)?;
    let rows = statement
        .query_map(params![plugin, BINDINGS_MAX], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(sqlite)?;
    rows.collect::<Result<_, _>>().map_err(sqlite)
}

fn key_saved(engine: &Engine<'_>, folder: KeyFolder, reference: &KeyRef) -> bool {
    (engine.keys.saved)(engine.root, folder, reference).unwrap_or(false)
}

fn plugin_key(reference: &str) -> Option<KeyRef> {
    reference
        .starts_with(KEY_PREFIX)
        .then(|| KeyRef::new(reference))
        .flatten()
}

pub(crate) fn resolve(
    db: &Connection,
    engine: &Engine<'_>,
    plugin: &str,
    secret: &str,
) -> Result<(Binding, Option<Bound>), Refused> {
    let unbound = (Binding::None, None);
    let Some((source, reference)) = binding_row(db, plugin, secret)? else {
        return Ok(unbound);
    };
    if source == KEY {
        return Ok(match plugin_key(&reference) {
            Some(key) if key_saved(engine, KeyFolder::Plugin, &key) => {
                (Binding::Key, Some(Bound::Key(key)))
            }
            _ => unbound,
        });
    }
    let Some(account) = KeyRef::new(&reference) else {
        return Ok(unbound);
    };
    Ok(match (engine.keys.account)(db, &account).ok().flatten() {
        Some((name, Some(key))) if key_saved(engine, KeyFolder::Account, &key) => (
            Binding::Account {
                account: account.as_str().to_owned(),
                name,
            },
            Some(Bound::Account(key)),
        ),
        _ => unbound,
    })
}

/// Deletes one binding row, then the key file entered for it through [`forget_key_file`].
/// Inputs: database, engine facts, plugin, secret and the row's source and reference.
/// Output: Ok once the row is gone, or the database's refusal with the row and file kept.
pub(crate) fn drop_binding(
    db: &Connection,
    engine: &Engine<'_>,
    plugin: &str,
    secret: &str,
    source: &str,
    reference: &str,
) -> Result<(), Refused> {
    crate::store_log::record(
        db,
        crate::store_log::Event::Unbound {
            plugin: plugin.to_owned(),
            secret: secret.to_owned(),
        },
    )?;
    forget_key_file(db, engine, source, reference);
    Ok(())
}

/// Deletes the key file a removed or replaced binding row named, only once that change is
/// committed: at once when the connection holds no open transaction, after the commit when
/// a plugin store capture holds the transaction's changes. Under any other open
/// transaction, or when the delete fails, the file stays and the start sweep deletes it
/// once no row names it. Inputs: database, engine facts and the old row's source and
/// reference. Output: none.
pub(crate) fn forget_key_file(db: &Connection, engine: &Engine<'_>, source: &str, reference: &str) {
    if source != KEY {
        return;
    }
    let Some(key) = plugin_key(reference) else {
        return;
    };
    if db.is_autocommit() {
        let _ = (engine.keys.remove)(engine.root, KeyFolder::Plugin, &key);
    } else {
        let _ = crate::store_log::forget_after_commit(key);
    }
}

/// Every key name a binding row names, whatever its source, for the start sweep to keep.
/// Input: the database. Output: the names, or the database's refusal, on which the sweep
/// deletes nothing.
pub fn named_keys(db: &Connection) -> Result<std::collections::BTreeSet<String>, Refused> {
    let mut statement = db
        .prepare("SELECT reference FROM app_plugin_secrets")
        .map_err(sqlite)?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    Ok(names)
}

/// Whether a file name in the plugin key folder is a key entered for a plugin, the only
/// kind the start sweep may delete. Input: the name. Output: true when it is.
pub fn is_entered_key(name: &str) -> bool {
    plugin_key(name).is_some()
}

fn bindable(db: &Connection, engine: &Engine<'_>, name: &str, secret: &str) -> Result<(), Refused> {
    let found = registry::found(engine, name)?;
    let manifest = found.parsed.map_err(|_| refused("app_plugin_refused"))?;
    if !manifest.secrets().iter().any(|declared| declared == secret) {
        return Err(refused("app_plugin_secret_not_declared"));
    }
    if !crate::trust::reviewed(db, found.origin, &manifest)? {
        return Err(refused("app_plugin_needs_review"));
    }
    Ok(())
}

fn save_binding(
    db: &Connection,
    engine: &Engine<'_>,
    plugin: &str,
    secret: &str,
    source: &str,
    reference: &str,
) -> Result<(), Refused> {
    let old = binding_row(db, plugin, secret)?;
    if old.as_ref().is_some_and(|(old_source, old_reference)| {
        old_source == source && old_reference == reference
    }) {
        return Ok(());
    }
    let source_word = crate::store_log::Source::from_word(source)
        .expect("a binding is saved from an account or an entered key");
    crate::store_log::record(
        db,
        crate::store_log::Event::Bound {
            plugin: plugin.to_owned(),
            secret: secret.to_owned(),
            source: source_word,
            reference: reference.to_owned(),
        },
    )?;
    if let Some((old_source, old_reference)) = old
        && (old_source != source || old_reference != reference)
    {
        forget_key_file(db, engine, &old_source, &old_reference);
    }
    Ok(())
}

/// Binds a declared secret to a saved API-key account's key; the key itself is never
/// copied. A key entered for that secret before is deleted. Inputs: database, engine facts,
/// plugin, secret and account id. Output: Ok, or `app_plugin_account_not_found`,
/// `_account_has_no_key`, `_secret_not_declared`, `_needs_review`, `_not_found`.
pub fn bind_account(
    db: &Connection,
    engine: &Engine<'_>,
    name: &str,
    secret: &str,
    account: &str,
) -> Result<(), Refused> {
    registry::prepare(db, engine)?;
    bindable(db, engine, name, secret)?;
    let id = KeyRef::new(account).ok_or_else(|| refused("app_plugin_account_not_found"))?;
    let key =
        match (engine.keys.account)(db, &id).map_err(|()| refused("app_plugins_unavailable"))? {
            None => return Err(refused("app_plugin_account_not_found")),
            Some((_, None)) => return Err(refused("app_plugin_account_has_no_key")),
            Some((_, Some(key))) => key,
        };
    if !(engine.keys.saved)(engine.root, KeyFolder::Account, &key)
        .map_err(|()| refused("app_plugin_key_store_unavailable"))?
    {
        return Err(refused("app_plugin_account_has_no_key"));
    }
    save_binding(db, engine, name, secret, ACCOUNT, id.as_str())
}

/// Saves a key the owner entered for one declared secret through the engine's key store.
/// The key is first written to a fresh key file. One that replaces a key entered before
/// writes no binding: a [`KeyStep`] moves it into the old key's file after the capture's
/// commit and deletes it on a rollback. A first key or one that replaces an account binding
/// writes the binding to the fresh file, which is deleted again if that write fails. The key
/// is never logged, shown or put in argv or env, and is wiped. Outside the capture of its
/// own transaction it is refused before the key is saved. Inputs: database, engine facts,
/// plugin, secret and key bytes. Output: Ok, or `app_plugin_key_invalid`,
/// `_key_store_unavailable`, `_secret_not_declared`, `_needs_review`, `_not_found`, or
/// `app_plugins_unavailable`.
pub fn set_key(
    db: &Connection,
    engine: &Engine<'_>,
    name: &str,
    secret: &str,
    mut key: Vec<u8>,
) -> Result<(), Refused> {
    let saved = (|| {
        crate::store_log::in_capture(db)?;
        registry::prepare(db, engine)?;
        bindable(db, engine, name, secret)?;
        if key.is_empty()
            || key.len() > kernel_security::secret::SECRET_BYTES_MAX
            || key.iter().any(|byte| !(0x21..=0x7e).contains(byte))
        {
            return Err(refused("app_plugin_key_invalid"));
        }
        let kept = binding_row(db, name, secret)?
            .filter(|(source, _)| source == KEY)
            .and_then(|(_, reference)| plugin_key(&reference));
        let id = (engine.keys.new_ref)().map_err(refused)?;
        let reference = KeyRef::new(&format!("{KEY_PREFIX}{id}"))
            .filter(|fresh| kept.as_ref() != Some(fresh))
            .ok_or_else(|| refused("app_plugin_key_store_unavailable"))?;
        (engine.keys.replace)(
            engine.root,
            KeyFolder::Plugin,
            &reference,
            std::mem::take(&mut key),
        )
        .map_err(|()| refused("app_plugin_key_store_unavailable"))?;
        if let Some(kept) = kept {
            let step = KeyStep {
                root: engine.root.to_path_buf(),
                kept,
                fresh: Some(reference),
                keys: *engine.keys,
            };
            return match crate::store_log::key_after_commit(step) {
                Some(step) => step.commit(),
                None => Ok(()),
            };
        }
        let bound = save_binding(db, engine, name, secret, KEY, reference.as_str());
        if bound.is_err() {
            let _ = (engine.keys.remove)(engine.root, KeyFolder::Plugin, &reference);
        }
        bound
    })();
    kernel_security::secret::wipe(&mut key);
    saved
}

/// An entered key that replaces the one before it, waiting in a fresh key file for the end
/// of the transaction. After the commit [`KeyStep::commit`] moves it into the old key's
/// file; a step rolled back or dropped with neither deletes the fresh file and leaves the
/// old key as it was. It holds key file names only, never key bytes.
#[derive(Debug)]
pub struct KeyStep {
    root: PathBuf,
    kept: KeyRef,
    fresh: Option<KeyRef>,
    keys: Keys,
}

impl KeyStep {
    /// Moves the new key into the old key's file, then deletes the fresh file, which goes
    /// on every exit. Output: Ok, or `app_plugin_key_store_unavailable`; the old key's file
    /// then holds the old key, or the new key when the move got past its rename, and a
    /// retry writes it again.
    pub fn commit(mut self) -> Result<(), Refused> {
        let Some(fresh) = self.fresh.take() else {
            return Ok(());
        };
        assert_ne!(fresh, self.kept);
        let unavailable = || refused("app_plugin_key_store_unavailable");
        let moved = (self.keys.read)(&self.root, KeyFolder::Plugin, &fresh)
            .map_err(|()| unavailable())
            .and_then(|key| key.ok_or_else(unavailable))
            .and_then(|key| {
                let bytes = key.expose().as_bytes().to_vec();
                (self.keys.replace)(&self.root, KeyFolder::Plugin, &self.kept, bytes)
                    .map_err(|()| unavailable())
            });
        let _ = (self.keys.remove)(&self.root, KeyFolder::Plugin, &fresh);
        moved
    }

    /// Deletes the fresh key file after the change rolled back. Output: none; a file it
    /// cannot delete is left to the start sweep, which deletes a key no binding names.
    pub fn roll_back(mut self) {
        self.discard();
    }

    /// Deletes the fresh key file, once. Output: none.
    fn discard(&mut self) {
        if let Some(fresh) = self.fresh.take() {
            let _ = (self.keys.remove)(&self.root, KeyFolder::Plugin, &fresh);
        }
    }
}

impl Drop for KeyStep {
    fn drop(&mut self) {
        self.discard();
    }
}

/// Unbinds one secret and deletes the key entered for it; an account's key stays.
/// Inputs: database, engine facts, plugin and secret. Output: Ok, including when it was not
/// bound, or `app_plugin_secret_not_declared`, `app_plugin_not_found`.
pub fn unbind(
    db: &Connection,
    engine: &Engine<'_>,
    name: &str,
    secret: &str,
) -> Result<(), Refused> {
    registry::prepare(db, engine)?;
    if let Ok(manifest) = registry::found(engine, name)?.parsed
        && !manifest.secrets().iter().any(|declared| declared == secret)
    {
        return Err(refused("app_plugin_secret_not_declared"));
    }
    if let Some((source, reference)) = binding_row(db, name, secret)? {
        drop_binding(db, engine, name, secret, &source, &reference)?;
    }
    Ok(())
}

/// Reads one bound secret's key into engine memory for a single request. Inputs: the
/// engine facts and the binding from `ready`. Output: the key, `app_plugin_needs_key` when
/// it is gone, or `app_plugin_key_store_unavailable`.
pub fn secret_value(engine: &Engine<'_>, bound: &Bound) -> Result<SecretString, Refused> {
    (engine.keys.read)(engine.root, bound.folder(), bound.reference())
        .map_err(|()| refused("app_plugin_key_store_unavailable"))?
        .ok_or_else(|| refused("app_plugin_needs_key"))
}

/// Sweeps what a crash between a staged write and its rename left at the engine root: key
/// copies staged in the key folders are deleted, a plugin's old folder whose name is
/// missing is moved back under it, and other staged plugin entries are deleted. The engine
/// runs it at start, before any store call can stage a file. A leftover it cannot delete,
/// or a folder that refuses it, stays for the next start and stops nothing. Input: the
/// engine facts. Output: none.
pub fn sweep_staged(engine: &Engine<'_>) {
    for folder in KeyFolder::ALL {
        let _ = (engine.keys.sweep)(engine.root, folder);
    }
    let _ = registry::remove_staged(engine.root);
}
