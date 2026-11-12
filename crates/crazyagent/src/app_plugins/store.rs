use super::manifest::{self, Manifest, Runs};
use super::{
    Binding, LOCAL_PLUGIN, Listing, Origin, PluginKind, PluginStatus, PluginView, Preview,
    Problem, SecretView, USER_PLUGINS_MAX, yaml,
};
use crate::engine::EngineError;
use crate::protocol::CredentialRef;
use crate::storage::accounts::{self, ACCOUNT_KEYS, PLUGIN_KEYS};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

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

const BUNDLED: [(&str, &str); 4] = [
    ("local", include_str!("../../assets/app-plugins/local.yaml")),
    ("openai", include_str!("../../assets/app-plugins/openai.yaml")),
    ("voyage", include_str!("../../assets/app-plugins/voyage.yaml")),
    ("ollama", include_str!("../../assets/app-plugins/ollama.yaml")),
];

const KEY_PREFIX: &str = "plugin-key-";
const BINDINGS_MAX: i64 = 64;
const ACCOUNT: &str = "account";
const KEY: &str = "key";

/// The plugins that ship with the app, in screen order, as (name, manifest text) pairs.
/// They are trusted by shipping and can only be turned off, never removed.
pub fn bundled() -> &'static [(&'static str, &'static str)] {
    &BUNDLED
}

/// A plugin ready to run: its manifest, where it came from, and where the key behind each
/// declared secret is saved, in declaration order. It holds references only, never a key.
#[derive(Debug)]
pub struct Loaded {
    pub manifest: Manifest,
    pub origin: Origin,
    pub bindings: Vec<Bound>,
}

/// Where one bound secret's key is saved: an API-key account's saved key, or a key the
/// owner entered for the plugin in `plugin-keys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    Account(CredentialRef),
    Key(CredentialRef),
}

struct Found {
    name: String,
    origin: Origin,
    parsed: Result<Manifest, Problem>,
    sha256: String,
}

fn error(code: &'static str) -> EngineError {
    EngineError(code.into())
}

fn sqlite(_: rusqlite::Error) -> EngineError {
    error("app_plugins_unavailable")
}

fn tables(db: &Connection) -> Result<(), EngineError> {
    db.execute_batch(SCHEMA).map_err(sqlite)
}

fn is_bundled(name: &str) -> bool {
    BUNDLED.iter().any(|(bundled, _)| *bundled == name)
}

fn bundled_found(name: &str, text: &str) -> Found {
    let manifest = manifest::parse(text).expect("a bundled plugin parses");
    assert_eq!(manifest.name, name, "a bundled plugin is listed under its own name");
    Found {
        name: name.to_owned(),
        origin: Origin::Bundled,
        sha256: manifest.sha256.clone(),
        parsed: Ok(manifest),
    }
}

fn parse_file(bytes: &[u8]) -> Result<Manifest, Problem> {
    match std::str::from_utf8(bytes) {
        Ok(text) if bytes.len() <= yaml::BYTES_MAX => manifest::parse(text),
        _ => Err(yaml::parse_bytes(bytes)
            .expect_err("bytes over the bound or not UTF-8 are refused")
            .problem()),
    }
}

fn user_found(stem: String, read: Result<Vec<u8>, Problem>) -> Found {
    let (parsed, sha256) = match read {
        Err(problem) => (Err(problem), String::new()),
        Ok(bytes) => {
            let sha256 = crate::semantic::sha256_hex(&bytes);
            let parsed = parse_file(&bytes).and_then(|manifest| {
                if manifest.name != stem {
                    Err(Problem::new("app_plugin_file_name_mismatch", None, Some("name")))
                } else if is_bundled(&manifest.name) {
                    Err(Problem::new("app_plugin_name_taken", None, Some("name")))
                } else {
                    Ok(manifest)
                }
            });
            (parsed, sha256)
        }
    };
    Found {
        name: stem,
        origin: Origin::User,
        parsed,
        sha256,
    }
}

fn find(root: &Path, name: &str) -> Result<Option<Found>, EngineError> {
    if let Some((bundled, text)) = BUNDLED.iter().find(|(bundled, _)| *bundled == name) {
        return Ok(Some(bundled_found(bundled, text)));
    }
    if !manifest::valid_name(name) {
        return Ok(None);
    }
    Ok(folder::read(root, name)?.map(|read| user_found(name.to_owned(), read)))
}

fn found(root: &Path, name: &str) -> Result<Found, EngineError> {
    find(root, name)?.ok_or_else(|| error("app_plugin_not_found"))
}

fn hosts_key(manifest: &Manifest) -> String {
    let mut hosts = match &manifest.runs {
        Runs::Local { .. } => Vec::new(),
        Runs::Http(http) => http.hosts.clone(),
    };
    hosts.sort();
    hosts.join("\n")
}

fn reviewed(db: &Connection, origin: Origin, manifest: &Manifest) -> Result<bool, EngineError> {
    if origin == Origin::Bundled {
        return Ok(true);
    }
    let trusted: Option<String> = db
        .query_row(
            "SELECT sha256 FROM app_plugin_trust WHERE name=?1",
            [manifest.name.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    Ok(trusted.as_deref() == Some(manifest.sha256.as_str()))
}

fn enabled_flag(db: &Connection, name: &str, origin: Origin) -> Result<bool, EngineError> {
    let stored: Option<bool> = db
        .query_row(
            "SELECT enabled FROM app_plugin_state WHERE name=?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    Ok(stored.unwrap_or(origin == Origin::Bundled && name == LOCAL_PLUGIN))
}

fn set_flag(db: &Connection, name: &str, enabled: bool) -> Result<(), EngineError> {
    db.execute(
        "INSERT INTO app_plugin_state (name, enabled) VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET enabled=excluded.enabled",
        params![name, enabled],
    )
    .map_err(sqlite)?;
    Ok(())
}

fn binding_row(
    db: &Connection,
    plugin: &str,
    secret: &str,
) -> Result<Option<(String, String)>, EngineError> {
    db.query_row(
        "SELECT source, reference FROM app_plugin_secrets WHERE plugin=?1 AND secret=?2",
        [plugin, secret],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(sqlite)
}

fn binding_rows(db: &Connection, plugin: &str) -> Result<Vec<(String, String, String)>, EngineError> {
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

fn key_saved(root: &Path, folder: &str, reference: &CredentialRef) -> bool {
    accounts::key_saved_in(root, folder, reference).unwrap_or(false)
}

fn plugin_key(reference: &str) -> Option<CredentialRef> {
    reference
        .starts_with(KEY_PREFIX)
        .then(|| CredentialRef::new(reference).ok())
        .flatten()
}

fn resolve(
    db: &Connection,
    root: &Path,
    plugin: &str,
    secret: &str,
) -> Result<(Binding, Option<Bound>), EngineError> {
    let unbound = (Binding::None, None);
    let Some((source, reference)) = binding_row(db, plugin, secret)? else {
        return Ok(unbound);
    };
    if source == KEY {
        return Ok(match plugin_key(&reference) {
            Some(key) if key_saved(root, PLUGIN_KEYS, &key) => (Binding::Key, Some(Bound::Key(key))),
            _ => unbound,
        });
    }
    let Ok(account) = CredentialRef::new(reference) else {
        return Ok(unbound);
    };
    Ok(match accounts::api_key_account(db, &account).ok().flatten() {
        Some((name, Some(key))) if key_saved(root, ACCOUNT_KEYS, &key) => (
            Binding::Account {
                account: account.as_str().to_owned(),
                name,
            },
            Some(Bound::Account(key)),
        ),
        _ => unbound,
    })
}

fn view(db: &Connection, root: &Path, found: Found) -> Result<PluginView, EngineError> {
    let manifest = match found.parsed {
        Err(problem) => {
            return Ok(PluginView {
                title: found.name.clone(),
                name: found.name,
                version: String::new(),
                description: String::new(),
                kind: PluginKind::Embedding,
                origin: found.origin,
                status: PluginStatus::Refused { problem },
                enabled: false,
                runs: None,
                secrets: Vec::new(),
                sha256: found.sha256,
            });
        }
        Ok(manifest) => manifest,
    };
    let mut secrets = Vec::with_capacity(manifest.secrets().len());
    for secret in manifest.secrets() {
        secrets.push(SecretView {
            name: secret.clone(),
            binding: resolve(db, root, &manifest.name, secret)?.0,
        });
    }
    let reviewed = reviewed(db, found.origin, &manifest)?;
    let status = if !reviewed {
        PluginStatus::NeedsReview
    } else if secrets.iter().any(|secret| secret.binding == Binding::None) {
        PluginStatus::NeedsKey
    } else {
        PluginStatus::Ready
    };
    Ok(PluginView {
        enabled: reviewed && enabled_flag(db, &manifest.name, found.origin)?,
        runs: Some(manifest.runs_view()),
        name: manifest.name,
        title: manifest.title,
        version: manifest.version,
        description: manifest.description,
        kind: manifest.kind,
        origin: found.origin,
        status,
        secrets,
        sha256: manifest.sha256,
    })
}

/// Lists every plugin for the owner's screen: the bundled ones first, then the user's
/// plugins folder by name, at most `USER_PLUGINS_MAX`. Inputs: the engine database and
/// root. Output: the listing with each plugin's status and secret bindings, or the
/// folder's refusal (`app_plugin_folder_unsafe` for a folder that is not this user's 0700).
pub fn list(db: &Connection, root: &Path) -> Result<Listing, EngineError> {
    tables(db)?;
    let mut plugins = Vec::with_capacity(BUNDLED.len() + USER_PLUGINS_MAX);
    for (name, text) in BUNDLED {
        plugins.push(view(db, root, bundled_found(name, text))?);
    }
    let (files, more) = folder::entries(root)?;
    for (stem, read) in files {
        plugins.push(view(db, root, user_found(stem, read))?);
    }
    Ok(Listing { plugins, more })
}

/// The trust screen for pasted or picked manifest text, before anything is saved.
/// Inputs: the engine root and the text. Output: the trust screen, or the refusal with its
/// line or field; a bundled plugin's name is `app_plugin_name_taken`.
pub fn preview(root: &Path, text: &str) -> Result<Preview, EngineError> {
    let manifest = match manifest::parse(text) {
        Ok(manifest) => manifest,
        Err(problem) => return Ok(Preview::Refused { problem }),
    };
    if is_bundled(&manifest.name) {
        return Ok(Preview::Refused {
            problem: Problem::new("app_plugin_name_taken", None, Some("name")),
        });
    }
    let replaces = folder::read(root, &manifest.name)?.is_some();
    Ok(Preview::Ready {
        trust: manifest.trust_view(replaces),
    })
}

/// The trust screen for a plugin already listed, such as a file dropped into the plugins
/// folder. Inputs: the engine root and the plugin name. Output: the trust screen, its
/// refusal, or `app_plugin_not_found`.
pub fn review(root: &Path, name: &str) -> Result<Preview, EngineError> {
    Ok(match found(root, name)?.parsed {
        Ok(manifest) => Preview::Ready {
            trust: manifest.trust_view(false),
        },
        Err(problem) => Preview::Refused { problem },
    })
}

fn settle(db: &Connection, root: &Path, manifest: &Manifest, now_ms: i64) -> Result<(), EngineError> {
    let hosts = hosts_key(manifest);
    let trusted_hosts: Option<String> = db
        .query_row(
            "SELECT hosts FROM app_plugin_trust WHERE name=?1",
            [manifest.name.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    for (secret, source, reference) in binding_rows(db, &manifest.name)? {
        if trusted_hosts.as_deref() != Some(hosts.as_str())
            || !manifest.secrets().contains(&secret)
        {
            drop_binding(db, root, &manifest.name, &secret, &source, &reference)?;
        }
    }
    db.execute(
        "INSERT INTO app_plugin_trust (name, sha256, hosts, trusted_ms) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(name) DO UPDATE SET sha256=excluded.sha256, hosts=excluded.hosts,
         trusted_ms=excluded.trusted_ms",
        params![manifest.name, manifest.sha256, hosts, now_ms],
    )
    .map_err(sqlite)?;
    set_flag(db, &manifest.name, true)
}

/// Saves a user plugin the owner confirmed on its trust screen, trusts it and turns it on.
/// Keys bound to an older version stay only when its hosts are unchanged. Inputs: database,
/// engine root, the exact text, the SHA-256 the trust screen showed, whether to replace a
/// plugin of that name, and the time. Output: Ok, or `app_plugin_changed`, `_refused`,
/// `_name_taken`, or the folder's refusal.
pub fn add(
    db: &Connection,
    root: &Path,
    text: &str,
    sha256: &str,
    replace: bool,
    now_ms: i64,
) -> Result<(), EngineError> {
    tables(db)?;
    if crate::semantic::sha256_hex(text.as_bytes()) != sha256 {
        return Err(error("app_plugin_changed"));
    }
    let manifest = manifest::parse(text).map_err(|_| error("app_plugin_refused"))?;
    if is_bundled(&manifest.name) {
        return Err(error("app_plugin_name_taken"));
    }
    if !replace && folder::read(root, &manifest.name)?.is_some() {
        return Err(error("app_plugin_name_taken"));
    }
    folder::write(root, &manifest.name, text.as_bytes())?;
    settle(db, root, &manifest, now_ms)
}

/// Trusts a user plugin file as the owner reviewed it and turns it on. Inputs: database,
/// engine root, plugin name, the SHA-256 its trust screen showed, and the time. Output: Ok,
/// or `app_plugin_changed` when the file changed since, `_refused`, `_bundled`, `_not_found`.
pub fn trust_file(
    db: &Connection,
    root: &Path,
    name: &str,
    sha256: &str,
    now_ms: i64,
) -> Result<(), EngineError> {
    tables(db)?;
    if is_bundled(name) {
        return Err(error("app_plugin_bundled"));
    }
    let manifest = found(root, name)?
        .parsed
        .map_err(|_| error("app_plugin_refused"))?;
    if manifest.sha256 != sha256 {
        return Err(error("app_plugin_changed"));
    }
    settle(db, root, &manifest, now_ms)
}

/// Turns a plugin on or off. A user plugin turns on only while its trusted text is
/// unchanged. Inputs: database, engine root, name and the new state. Output: Ok, or
/// `app_plugin_not_found`, `_refused`, `_needs_review`.
pub fn set_enabled(db: &Connection, root: &Path, name: &str, enabled: bool) -> Result<(), EngineError> {
    tables(db)?;
    let found = found(root, name)?;
    if enabled {
        let manifest = found.parsed.map_err(|_| error("app_plugin_refused"))?;
        if !reviewed(db, found.origin, &manifest)? {
            return Err(error("app_plugin_needs_review"));
        }
    }
    set_flag(db, name, enabled)
}

fn drop_binding(
    db: &Connection,
    root: &Path,
    plugin: &str,
    secret: &str,
    source: &str,
    reference: &str,
) -> Result<(), EngineError> {
    if source == KEY
        && let Some(key) = plugin_key(reference)
    {
        accounts::remove_key_in(root, PLUGIN_KEYS, key.as_str())
            .map_err(|_| error("app_plugin_key_store_unavailable"))?;
    }
    db.execute(
        "DELETE FROM app_plugin_secrets WHERE plugin=?1 AND secret=?2",
        [plugin, secret],
    )
    .map_err(sqlite)?;
    Ok(())
}

/// Removes a user plugin: its file, trust record, state, bindings and the keys entered for
/// it. Account keys stay. Inputs: database, engine root and name. Output: Ok, or
/// `app_plugin_bundled`, `app_plugin_not_found`, or the folder's refusal.
pub fn remove(db: &Connection, root: &Path, name: &str) -> Result<(), EngineError> {
    tables(db)?;
    if is_bundled(name) {
        return Err(error("app_plugin_bundled"));
    }
    if !manifest::valid_name(name) {
        return Err(error("app_plugin_not_found"));
    }
    let removed = folder::remove(root, name)?;
    let known: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM app_plugin_trust WHERE name=?1)
             OR EXISTS(SELECT 1 FROM app_plugin_state WHERE name=?1)
             OR EXISTS(SELECT 1 FROM app_plugin_secrets WHERE plugin=?1)",
            [name],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    if !removed && !known {
        return Err(error("app_plugin_not_found"));
    }
    for (secret, source, reference) in binding_rows(db, name)? {
        drop_binding(db, root, name, &secret, &source, &reference)?;
    }
    db.execute("DELETE FROM app_plugin_trust WHERE name=?1", [name])
        .map_err(sqlite)?;
    db.execute("DELETE FROM app_plugin_state WHERE name=?1", [name])
        .map_err(sqlite)?;
    Ok(())
}

fn bindable(db: &Connection, root: &Path, name: &str, secret: &str) -> Result<(), EngineError> {
    let found = found(root, name)?;
    let manifest = found.parsed.map_err(|_| error("app_plugin_refused"))?;
    if !manifest.secrets().iter().any(|declared| declared == secret) {
        return Err(error("app_plugin_secret_not_declared"));
    }
    if !reviewed(db, found.origin, &manifest)? {
        return Err(error("app_plugin_needs_review"));
    }
    Ok(())
}

fn save_binding(
    db: &Connection,
    root: &Path,
    plugin: &str,
    secret: &str,
    source: &str,
    reference: &str,
) -> Result<(), EngineError> {
    if let Some((old_source, old_reference)) = binding_row(db, plugin, secret)?
        && (old_source != source || old_reference != reference)
    {
        drop_binding(db, root, plugin, secret, &old_source, &old_reference)?;
    }
    db.execute(
        "INSERT INTO app_plugin_secrets (plugin, secret, source, reference) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(plugin, secret) DO UPDATE SET source=excluded.source,
         reference=excluded.reference",
        [plugin, secret, source, reference],
    )
    .map_err(sqlite)?;
    Ok(())
}

/// Binds a declared secret to a saved API-key account's key; the key itself is never
/// copied. A key entered for that secret before is deleted. Inputs: database, engine root,
/// plugin, secret and account id. Output: Ok, or `app_plugin_account_not_found`,
/// `_account_has_no_key`, `_secret_not_declared`, `_needs_review`, `_not_found`.
pub fn bind_account(
    db: &Connection,
    root: &Path,
    name: &str,
    secret: &str,
    account: &str,
) -> Result<(), EngineError> {
    tables(db)?;
    bindable(db, root, name, secret)?;
    let id = CredentialRef::new(account).map_err(|_| error("app_plugin_account_not_found"))?;
    let key = match accounts::api_key_account(db, &id).map_err(|_| error("app_plugins_unavailable"))? {
        None => return Err(error("app_plugin_account_not_found")),
        Some((_, None)) => return Err(error("app_plugin_account_has_no_key")),
        Some((_, Some(key))) => key,
    };
    if !accounts::key_saved_in(root, ACCOUNT_KEYS, &key)
        .map_err(|_| error("app_plugin_key_store_unavailable"))?
    {
        return Err(error("app_plugin_account_has_no_key"));
    }
    save_binding(db, root, name, secret, ACCOUNT, id.as_str())
}

/// Saves a key the owner entered for one declared secret as a private 0600 file in
/// `plugin-keys`, replacing the one entered before in place. The key is never logged,
/// shown or put in argv or env, and is zeroed. Inputs: database, engine root, plugin,
/// secret and key bytes. Output: Ok, or `app_plugin_key_invalid`, `_key_store_unavailable`,
/// `_secret_not_declared`, `_needs_review`, `_not_found`.
pub fn set_key(
    db: &Connection,
    root: &Path,
    name: &str,
    secret: &str,
    mut key: Vec<u8>,
) -> Result<(), EngineError> {
    let saved = (|| {
        tables(db)?;
        bindable(db, root, name, secret)?;
        if key.is_empty()
            || key.len() > crate::credentials::SECRET_BYTES_MAX
            || key.iter().any(|byte| !(0x21..=0x7e).contains(byte))
        {
            return Err(error("app_plugin_key_invalid"));
        }
        let kept = binding_row(db, name, secret)?
            .filter(|(source, _)| source == KEY)
            .and_then(|(_, reference)| plugin_key(&reference));
        let reference = match &kept {
            Some(reference) => reference.clone(),
            None => CredentialRef::new(format!(
                "{KEY_PREFIX}{}",
                crate::engine::owner::random_id()?
            ))
            .map_err(|_| error("app_plugin_key_store_unavailable"))?,
        };
        accounts::replace_key_in(root, PLUGIN_KEYS, &reference, std::mem::take(&mut key))
            .map_err(|_| error("app_plugin_key_store_unavailable"))?;
        let bound = save_binding(db, root, name, secret, KEY, reference.as_str());
        if bound.is_err() && kept.is_none() {
            let _ = accounts::remove_key_in(root, PLUGIN_KEYS, reference.as_str());
        }
        bound
    })();
    key.fill(0);
    saved
}

/// Unbinds one secret and deletes the key entered for it; an account's key stays.
/// Inputs: database, engine root, plugin and secret. Output: Ok, including when it was not
/// bound, or `app_plugin_secret_not_declared`, `app_plugin_not_found`.
pub fn unbind(db: &Connection, root: &Path, name: &str, secret: &str) -> Result<(), EngineError> {
    tables(db)?;
    if let Ok(manifest) = found(root, name)?.parsed
        && !manifest.secrets().iter().any(|declared| declared == secret)
    {
        return Err(error("app_plugin_secret_not_declared"));
    }
    if let Some((source, reference)) = binding_row(db, name, secret)? {
        drop_binding(db, root, name, secret, &source, &reference)?;
    }
    Ok(())
}

/// Loads a plugin to run: valid, trusted as it is now, turned on, and every declared secret
/// bound to a saved key. Inputs: database, engine root and name. Output: the plugin, or
/// `app_plugin_not_found`, `_refused`, `_needs_review`, `_off`, `_needs_key`.
pub fn ready(db: &Connection, root: &Path, name: &str) -> Result<Loaded, EngineError> {
    tables(db)?;
    let found = found(root, name)?;
    let manifest = found.parsed.map_err(|_| error("app_plugin_refused"))?;
    if !reviewed(db, found.origin, &manifest)? {
        return Err(error("app_plugin_needs_review"));
    }
    if !enabled_flag(db, &manifest.name, found.origin)? {
        return Err(error("app_plugin_off"));
    }
    let mut bindings = Vec::with_capacity(manifest.secrets().len());
    for secret in manifest.secrets() {
        let (_, bound) = resolve(db, root, &manifest.name, secret)?;
        bindings.push(bound.ok_or_else(|| error("app_plugin_needs_key"))?);
    }
    assert_eq!(bindings.len(), manifest.secrets().len());
    Ok(Loaded {
        manifest,
        origin: found.origin,
        bindings,
    })
}

/// Reads one bound secret's key into engine memory for a single request. Inputs: the
/// engine root and the binding from `ready`. Output: the key, `app_plugin_needs_key` when
/// it is gone, or `app_plugin_key_store_unavailable`.
pub fn secret_value(
    root: &Path,
    bound: &Bound,
) -> Result<crate::credentials::SecretString, EngineError> {
    let (folder, reference) = match bound {
        Bound::Account(reference) => (ACCOUNT_KEYS, reference),
        Bound::Key(reference) => (PLUGIN_KEYS, reference),
    };
    accounts::read_key_in(root, folder, reference)
        .map_err(|_| error("app_plugin_key_store_unavailable"))?
        .ok_or_else(|| error("app_plugin_needs_key"))
}

#[cfg(unix)]
mod folder {
    use super::{Problem, USER_PLUGINS_MAX, error, yaml};
    use crate::app_plugins::FOLDER;
    use crate::engine::EngineError;
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    const SUFFIX: &str = ".yaml";

    fn file_name(name: &str) -> String {
        format!("{name}{SUFFIX}")
    }

    fn unavailable() -> EngineError {
        error("app_plugin_folder_unavailable")
    }

    fn open(root: &Path, create: bool) -> Result<Option<std::fs::File>, EngineError> {
        if create {
            match rustix::fs::mkdir(root.join(FOLDER), Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(_) => return Err(unavailable()),
            }
        }
        let directory = match rustix::fs::open(
            root.join(FOLDER),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => std::fs::File::from(fd),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                return Err(error("app_plugin_folder_unsafe"));
            }
            Err(_) => return Err(unavailable()),
        };
        let metadata = directory.metadata().map_err(|_| unavailable())?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(error("app_plugin_folder_unsafe"));
        }
        Ok(Some(directory))
    }

    fn read_in(directory: &std::fs::File, name: &str) -> Result<Vec<u8>, Problem> {
        use std::io::Read;
        let refused = || Problem::new("app_plugin_file_unsafe", None, None);
        let file = std::fs::File::from(
            rustix::fs::openat(
                directory,
                file_name(name).as_str(),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|_| refused())?,
        );
        let metadata = file.metadata().map_err(|_| refused())?;
        if !metadata.is_file() || metadata.uid() != rustix::process::getuid().as_raw() {
            return Err(refused());
        }
        let mut bytes = Vec::new();
        file.take(yaml::BYTES_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| refused())?;
        assert!(bytes.len() <= yaml::BYTES_MAX + 1);
        Ok(bytes)
    }

    /// The user's plugin files by name, at most `USER_PLUGINS_MAX`, each read without
    /// following a link. Input: the engine root. Output: (name, bytes or file refusal)
    /// pairs, whether more files exist, or the folder's refusal.
    pub(super) fn entries(
        root: &Path,
    ) -> Result<(Vec<(String, Result<Vec<u8>, Problem>)>, bool), EngineError> {
        let Some(directory) = open(root, false)? else {
            return Ok((Vec::new(), false));
        };
        let mut stems = std::collections::BTreeSet::new();
        let mut more = false;
        for entry in rustix::fs::Dir::read_from(&directory).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let Ok(file) = std::str::from_utf8(entry.file_name().to_bytes()) else {
                continue;
            };
            let Some(stem) = file.strip_suffix(SUFFIX) else {
                continue;
            };
            if stem.is_empty() || stem.starts_with('.') {
                continue;
            }
            stems.insert(stem.to_owned());
            if stems.len() > USER_PLUGINS_MAX {
                stems.pop_last();
                more = true;
            }
        }
        assert!(stems.len() <= USER_PLUGINS_MAX);
        Ok((
            stems
                .into_iter()
                .map(|stem| {
                    let read = read_in(&directory, &stem);
                    (stem, read)
                })
                .collect(),
            more,
        ))
    }

    /// One user plugin file. Inputs: the engine root and a valid plugin name. Output: none
    /// when it is missing, else its bytes or the file refusal; or the folder's refusal.
    pub(super) fn read(
        root: &Path,
        name: &str,
    ) -> Result<Option<Result<Vec<u8>, Problem>>, EngineError> {
        let Some(directory) = open(root, false)? else {
            return Ok(None);
        };
        match rustix::fs::statat(&directory, file_name(name).as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Ok(_) => Ok(Some(read_in(&directory, name))),
            Err(_) => Err(unavailable()),
        }
    }

    /// Writes a user plugin file as a staged 0600 file renamed over the old one, creating
    /// the 0700 folder when missing. Inputs: the engine root, a valid name and the bytes.
    /// Output: Ok with the folder synced, or the folder's refusal with nothing staged.
    pub(super) fn write(root: &Path, name: &str, bytes: &[u8]) -> Result<(), EngineError> {
        let directory = open(root, true)?.ok_or_else(unavailable)?;
        let staged = format!(".add-{}", crate::engine::owner::random_id()?);
        let mut file = std::fs::File::from(
            rustix::fs::openat(
                &directory,
                staged.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| unavailable())?,
        );
        let written = std::io::Write::write_all(&mut file, bytes).and_then(|()| file.sync_all());
        if written.is_err()
            || rustix::fs::renameat(&directory, staged.as_str(), &directory, file_name(name).as_str())
                .is_err()
        {
            let _ = rustix::fs::unlinkat(&directory, staged.as_str(), AtFlags::empty());
            return Err(unavailable());
        }
        directory.sync_all().map_err(|_| unavailable())
    }

    /// Deletes a user plugin file. Inputs: the engine root and a valid name. Output: whether
    /// a file was deleted, or the folder's refusal.
    pub(super) fn remove(root: &Path, name: &str) -> Result<bool, EngineError> {
        let Some(directory) = open(root, false)? else {
            return Ok(false);
        };
        match rustix::fs::unlinkat(&directory, file_name(name).as_str(), AtFlags::empty()) {
            Ok(()) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(unavailable()),
        }
    }
}

#[cfg(not(unix))]
mod folder {
    use super::{Problem, error};
    use crate::engine::EngineError;
    use std::path::Path;

    /// No user plugins folder off Unix: nothing is listed. Output: an empty listing.
    pub(super) fn entries(
        _root: &Path,
    ) -> Result<(Vec<(String, Result<Vec<u8>, Problem>)>, bool), EngineError> {
        Ok((Vec::new(), false))
    }

    /// No user plugins folder off Unix. Output: none.
    pub(super) fn read(
        _root: &Path,
        _name: &str,
    ) -> Result<Option<Result<Vec<u8>, Problem>>, EngineError> {
        Ok(None)
    }

    /// No user plugins folder off Unix. Output: `app_plugin_folder_unavailable`.
    pub(super) fn write(_root: &Path, _name: &str, _bytes: &[u8]) -> Result<(), EngineError> {
        Err(error("app_plugin_folder_unavailable"))
    }

    /// No user plugins folder off Unix. Output: false, nothing was deleted.
    pub(super) fn remove(_root: &Path, _name: &str) -> Result<bool, EngineError> {
        Ok(false)
    }
}

#[cfg(all(test, unix))]
#[path = "store_tests.rs"]
mod tests;
