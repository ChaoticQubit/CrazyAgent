use std::ops::RangeInclusive;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Refused;
use crate::embedding::{self, Manifest};
use crate::generated::permissions::BUNDLED_GRANTS;
use crate::manifest::MANIFEST_BYTES_MAX;
use crate::problem::{PORT_HANDED_OUT, Problem};
use crate::store::{Keys, Loaded, binding_rows, forget_key_file, refused, resolve, sqlite};
use crate::store_log;
use crate::trust;
use crate::usage::{ChangeNotice, Holder, Lease, TurnOff, Usage};
use crate::views::{
    Binding, Listing, Origin, PluginKind, PluginStatus, PluginView, Preview, SecretView,
};

/// The folder under the engine root that holds the owner's plugins, one folder each.
pub const FOLDER: &str = "app-plugins";
/// The most plugin folders read from [`FOLDER`].
pub const USER_PLUGINS_MAX: usize = 64;
/// The one file a plugin folder holds.
pub const MANIFEST_FILE: &str = "plugin.toml";

const BUNDLED: [(&str, &str); 3] = [
    ("openai", include_str!("../bundled/openai/plugin.toml")),
    ("voyage", include_str!("../bundled/voyage/plugin.toml")),
    ("ollama", include_str!("../bundled/ollama/plugin.toml")),
];

/// The most plugins that exist at once: the bundled ones and the listed folders of the
/// owner's plugins folder.
pub const PLUGINS_MAX: usize = BUNDLED.len() + USER_PLUGINS_MAX;
/// The most loopback ports [`loopback_ports`] lists: one per bundled and listed user plugin.
pub const LOOPBACK_PORTS_MAX: usize = BUNDLED.len() + USER_PLUGINS_MAX;

/// What the engine hands the plugin host for one call: its root folder, its key store, and
/// the ports this machine hands out by itself, where an agent could listen on a loopback
/// plugin port before the plugin's server does: Linux reads
/// net.ipv4.ip_local_port_range, other systems none.
#[derive(Debug, Clone)]
pub struct Engine<'a> {
    pub root: &'a Path,
    pub keys: &'a Keys,
    pub handed_out: Option<RangeInclusive<u16>>,
}

/// The plugins that ship with the app, in screen order, as (name, plugin.toml text) pairs.
/// They are trusted by shipping, start off, and turn on and off but are never removed.
pub fn bundled() -> &'static [(&'static str, &'static str)] {
    &BUNDLED
}

/// One plugin as read: its name, where it is from, the manifest or the problem refusing
/// it, the SHA-256 of its bytes, and the kind its text declares, which a refused plugin
/// keeps when its `kind` still reads, so it is listed on that kind's page.
pub(crate) struct Found {
    pub(crate) name: String,
    pub(crate) origin: Origin,
    pub(crate) parsed: Result<Manifest, Problem>,
    pub(crate) sha256: String,
    pub(crate) declared: Option<PluginKind>,
}

pub(crate) fn is_bundled(name: &str) -> bool {
    BUNDLED.iter().any(|(bundled, _)| *bundled == name)
}

/// Whether a name is a bundled code plugin's id from the generated grant table. Input:
/// the name. Output: the answer.
pub(crate) fn is_code_plugin(name: &str) -> bool {
    BUNDLED_GRANTS.iter().any(|grant| grant.plugin == name)
}

/// Whether a user plugin may not take a name: a bundled plugin or a bundled code plugin
/// already holds it, and with it that plugin's on and off state. Input: the name. Output:
/// the answer.
fn name_taken(name: &str) -> bool {
    is_bundled(name) || is_code_plugin(name)
}

fn bundled_found(name: &str, text: &str) -> Found {
    let manifest = embedding::parse(text).expect("a bundled plugin parses");
    assert_eq!(
        manifest.name, name,
        "a bundled plugin is listed under its own name"
    );
    Found {
        name: name.to_owned(),
        origin: Origin::Bundled,
        sha256: manifest.sha256.clone(),
        declared: Some(manifest.kind),
        parsed: Ok(manifest),
    }
}

fn parse_file(bytes: &[u8]) -> Result<Manifest, Problem> {
    match std::str::from_utf8(bytes) {
        Ok(text) if bytes.len() <= MANIFEST_BYTES_MAX => embedding::parse(text),
        _ => Err(Problem::new("plugin_manifest_invalid", Some(1), None)),
    }
}

fn declared_file_kind(bytes: &[u8]) -> Option<PluginKind> {
    embedding::declared_kind(std::str::from_utf8(bytes).ok()?)
}

pub(crate) fn user_found(stem: String, read: Result<Vec<u8>, Problem>) -> Found {
    let (parsed, sha256, declared) = match read {
        Err(problem) => (Err(problem), String::new(), None),
        Ok(bytes) => {
            let sha256 = kernel_common::hash(&bytes);
            let parsed = parse_file(&bytes).and_then(|manifest| {
                if manifest.name != stem {
                    Err(Problem::new(
                        "app_plugin_file_name_mismatch",
                        None,
                        Some("plugin"),
                    ))
                } else if name_taken(&manifest.name) {
                    Err(Problem::new("app_plugin_name_taken", None, Some("plugin")))
                } else {
                    Ok(manifest)
                }
            });
            let declared = match &parsed {
                Ok(manifest) => Some(manifest.kind),
                Err(_) => declared_file_kind(&bytes),
            };
            (parsed, sha256, declared)
        }
    };
    Found {
        name: stem,
        origin: Origin::User,
        parsed,
        sha256,
        declared,
    }
}

/// Refuses a plugin whose loopback port lies in the ports this machine hands out, that
/// plugin alone with `app_plugin_port_handed_out`, so it never runs and no launch keeps its
/// port, since plugin files never stop launches. Inputs: the plugin read
/// and the range, none when nothing is handed out. Output: the same plugin, refused or not.
pub(crate) fn outside_handed_out(found: Found, handed: Option<&RangeInclusive<u16>>) -> Found {
    let Found {
        name,
        origin,
        parsed,
        sha256,
        declared,
    } = found;
    let parsed = parsed.and_then(|manifest| match handed_problem(&manifest, handed) {
        Some(problem) => Err(problem),
        None => Ok(manifest),
    });
    Found {
        name,
        origin,
        parsed,
        sha256,
        declared,
    }
}

fn handed_problem(manifest: &Manifest, handed: Option<&RangeInclusive<u16>>) -> Option<Problem> {
    let port = loopback_port(manifest)?;
    let range = handed.filter(|range| range.contains(&port))?;
    Some(Problem::port_handed_out(port, range))
}

fn find(engine: &Engine<'_>, name: &str) -> Result<Option<Found>, Refused> {
    let handed = engine.handed_out.as_ref();
    if let Some((bundled, text)) = BUNDLED.iter().find(|(bundled, _)| *bundled == name) {
        return Ok(Some(outside_handed_out(
            bundled_found(bundled, text),
            handed,
        )));
    }
    if !embedding::valid_name(name) {
        return Ok(None);
    }
    let Some(read) = folder::read(engine.root, name)? else {
        return Ok(None);
    };
    let read = if folder::listed(engine.root, name)? {
        read
    } else {
        Err(Problem::new("app_plugin_past_cap", None, None))
    };
    Ok(Some(outside_handed_out(
        user_found(name.to_owned(), read),
        handed,
    )))
}

pub(crate) fn found(engine: &Engine<'_>, name: &str) -> Result<Found, Refused> {
    find(engine, name)?.ok_or_else(|| refused("app_plugin_not_found"))
}

/// Forgets what a reserved name holds that a bundled plugin or a bundled code plugin does
/// not own: a trust record, which neither kind ever gets, marks an older plugin of the
/// owner's of that name, and a switch row under a bare code plugin id is such a leftover
/// too, since a code plugin's own switch sits under `code:<id>`. Each leftover goes as one
/// [`store_log::Event::Forgotten`], then the keys entered for its bindings go, as
/// `store::drop_binding` does. Inputs: database and engine facts. Output: Ok, or
/// `app_plugins_unavailable`.
pub(crate) fn prepare(db: &Connection, engine: &Engine<'_>) -> Result<(), Refused> {
    let reserved = BUNDLED
        .iter()
        .map(|(name, _)| *name)
        .chain(BUNDLED_GRANTS.iter().map(|grant| grant.plugin));
    for name in reserved {
        let leftover: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM app_plugin_trust WHERE name=?1)
                 OR (?2 AND EXISTS(SELECT 1 FROM app_plugin_state WHERE name=?1))",
                params![name, is_code_plugin(name)],
                |row| row.get(0),
            )
            .map_err(sqlite)?;
        if !leftover {
            continue;
        }
        let bindings = binding_rows(db, name)?;
        store_log::record(
            db,
            store_log::Event::Forgotten {
                plugin: name.to_owned(),
            },
        )?;
        for (_, source, reference) in &bindings {
            forget_key_file(db, engine, source, reference);
        }
    }
    Ok(())
}

/// The kind of the plugin a name stands for: a bundled plugin, or a folder of the owner's
/// that reads as a plugin. Inputs: the engine facts and the name. Output: the kind, none
/// when no plugin has the name or it is refused, or the folder's refusal.
pub fn kind_of(engine: &Engine<'_>, name: &str) -> Result<Option<PluginKind>, Refused> {
    Ok(find(engine, name)?
        .and_then(|found| found.parsed.ok())
        .map(|manifest| manifest.kind))
}

fn enabled_flag(db: &Connection, name: &str) -> Result<bool, Refused> {
    let stored: Option<bool> = db
        .query_row(
            "SELECT enabled FROM app_plugin_state WHERE name=?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    Ok(stored.unwrap_or(false))
}

/// Switches a plugin by its switch table key, a plugin name or a code plugin's
/// `code:<id>`, recording the change; a switch to the state the row already holds records
/// nothing. Inputs: database, key and the state. Output: Ok, or the store's refusal.
pub(crate) fn set_flag(db: &Connection, name: &str, enabled: bool) -> Result<(), Refused> {
    let stored: Option<bool> = db
        .query_row(
            "SELECT enabled FROM app_plugin_state WHERE name=?1",
            [name],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    if stored == Some(enabled) {
        return Ok(());
    }
    let event = match name.strip_prefix(CODE_KEY_PREFIX) {
        Some(plugin) => store_log::Event::CodeSwitched {
            plugin: plugin.to_owned(),
            enabled,
        },
        None => store_log::Event::Switched {
            plugin: name.to_owned(),
            enabled,
        },
    };
    store_log::record(db, event)
}

/// Writes one switch row as the store keeps it. Inputs: database, key and the state.
/// Output: Ok, or `app_plugins_unavailable`.
pub(crate) fn set_flag_row(db: &Connection, name: &str, enabled: bool) -> Result<(), Refused> {
    db.execute(
        "INSERT INTO app_plugin_state (name, enabled) VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET enabled=excluded.enabled",
        params![name, enabled],
    )
    .map_err(sqlite)?;
    Ok(())
}

fn view(db: &Connection, engine: &Engine<'_>, found: Found) -> Result<PluginView, Refused> {
    let manifest = match found.parsed {
        Err(problem) => {
            return Ok(PluginView {
                title: found.name.clone(),
                name: found.name,
                version: String::new(),
                description: String::new(),
                kind: found.declared.unwrap_or(PluginKind::Embedding),
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
            binding: resolve(db, engine, &manifest.name, secret)?.0,
        });
    }
    let reviewed = trust::reviewed(db, found.origin, &manifest)?;
    let status = if !reviewed {
        match trust::trusted_sha256(db, &manifest.name)? {
            Some(trusted_sha256) => PluginStatus::Changed { trusted_sha256 },
            None => PluginStatus::NeedsReview,
        }
    } else if secrets.iter().any(|secret| secret.binding == Binding::None) {
        PluginStatus::NeedsKey
    } else {
        PluginStatus::Ready
    };
    Ok(PluginView {
        enabled: reviewed && enabled_flag(db, &manifest.name)?,
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

fn loopback_port(manifest: &Manifest) -> Option<u16> {
    let url = &manifest.http.url;
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    loopback.then(|| url.port_or_known_default()).flatten()
}

/// The loopback ports every plugin that can run would send to, trusted or not, on or off,
/// for a tools-on sandbox to keep free of agent listeners.
/// Folders past the listed cap and plugins on a port this machine hands out are skipped,
/// since `ready` refuses them. Input: the engine facts. Output: the nonzero ports sorted
/// once each, at most `LOOPBACK_PORTS_MAX`, none inside the handed-out range, or the
/// folder's refusal, so a launch fails closed instead of missing a port.
pub fn loopback_ports(engine: &Engine<'_>) -> Result<Vec<u16>, Refused> {
    let (files, _) = folder::entries(engine.root)?;
    let handed = engine.handed_out.as_ref();
    let mut ports: Vec<u16> = BUNDLED
        .iter()
        .map(|(name, text)| bundled_found(name, text))
        .chain(files.into_iter().map(|(stem, read)| user_found(stem, read)))
        .map(|found| outside_handed_out(found, handed))
        .filter_map(|found| found.parsed.ok())
        .filter_map(|manifest| loopback_port(&manifest))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    assert!(ports.len() <= LOOPBACK_PORTS_MAX);
    assert!(!ports.contains(&0), "a manifest refuses port 0");
    assert!(
        handed.is_none_or(|range| ports.iter().all(|port| !range.contains(port))),
        "a plugin on a handed-out port is refused"
    );
    Ok(ports)
}

/// Lists every plugin for the owner's screen: the bundled ones first, then the owner's
/// plugin folders by name, at most `USER_PLUGINS_MAX`. Inputs: the engine database and
/// facts. Output: the listing with each plugin's status and secret bindings, or the
/// folder's refusal (`app_plugin_folder_unsafe` for a folder that is not this user's 0700).
pub fn list(db: &Connection, engine: &Engine<'_>) -> Result<Listing, Refused> {
    prepare(db, engine)?;
    let handed = engine.handed_out.as_ref();
    let mut plugins = Vec::with_capacity(PLUGINS_MAX);
    for (name, text) in BUNDLED {
        let found = outside_handed_out(bundled_found(name, text), handed);
        plugins.push(view(db, engine, found)?);
    }
    let (files, more) = folder::entries(engine.root)?;
    for (stem, read) in files {
        let found = outside_handed_out(user_found(stem, read), handed);
        plugins.push(view(db, engine, found)?);
    }
    assert!(plugins.len() <= PLUGINS_MAX);
    Ok(Listing { plugins, more })
}

/// The trust screen for a plugin folder the owner picked, before anything is copied: the
/// folder must hold its plugin.toml only, read without following a link. Inputs: the
/// database, the engine facts and the picked folder. Output: the trust screen with the
/// SHA-256 the add must name, or the refusal with its line or field; a bundled plugin's
/// name is `app_plugin_name_taken`.
pub fn preview(db: &Connection, engine: &Engine<'_>, source: &Path) -> Result<Preview, Refused> {
    let manifest = match folder::read_source(source).and_then(|bytes| parse_file(&bytes)) {
        Ok(manifest) => manifest,
        Err(problem) => return Ok(Preview::Refused { problem }),
    };
    if name_taken(&manifest.name) {
        return Ok(Preview::Refused {
            problem: Problem::new("app_plugin_name_taken", None, Some("plugin")),
        });
    }
    if let Some(problem) = handed_problem(&manifest, engine.handed_out.as_ref()) {
        return Ok(Preview::Refused { problem });
    }
    let replaces = folder::read(engine.root, &manifest.name)?.is_some();
    Ok(Preview::Ready {
        trust: trust::trust_view(db, &manifest, replaces)?,
    })
}

/// The trust screen for a plugin already listed, such as a folder dropped into the plugins
/// folder, with what changed and grew since the owner trusted it. Inputs: the
/// database, the engine facts and the plugin name. Output: the trust screen, its refusal,
/// or `app_plugin_not_found`.
pub fn review(db: &Connection, engine: &Engine<'_>, name: &str) -> Result<Preview, Refused> {
    Ok(match found(engine, name)?.parsed {
        Ok(manifest) => Preview::Ready {
            trust: trust::trust_view(db, &manifest, false)?,
        },
        Err(problem) => Preview::Refused { problem },
    })
}

/// Copies a plugin folder the owner confirmed on its trust screen into the plugins folder,
/// trusts it and turns it on. The plugin.toml is read once, its SHA-256 taken
/// over the bytes read, compared with the reviewed one, and only those bytes are written,
/// so what runs is what was reviewed. Keys bound to an older version stay only while its
/// hosts are unchanged. Inputs: database, engine facts, the picked folder, the SHA-256 the
/// trust screen showed, whether to replace a plugin of that name, and the time. Output: Ok,
/// or `app_plugin_changed`, `_refused`, `_name_taken`, `_past_cap` (the listing would not
/// show it, or would drop a listed plugin for it), `_port_handed_out` (its loopback port
/// is in the range the engine reads now), or the folder's refusal; outside the capture of
/// its own transaction, `app_plugins_unavailable` before anything is written. A replaced
/// folder is deleted only after the capture's commit and put back after its rollback.
pub fn add(
    db: &Connection,
    engine: &Engine<'_>,
    source: &Path,
    sha256: &str,
    replace: bool,
    now_ms: i64,
) -> Result<(), Refused> {
    store_log::in_capture(db)?;
    prepare(db, engine)?;
    let bytes = folder::read_source(source).map_err(|_| refused("app_plugin_refused"))?;
    if kernel_common::hash(&bytes) != sha256 {
        return Err(refused("app_plugin_changed"));
    }
    let manifest = parse_file(&bytes).map_err(|_| refused("app_plugin_refused"))?;
    assert_eq!(manifest.sha256, sha256);
    if name_taken(&manifest.name) {
        return Err(refused("app_plugin_name_taken"));
    }
    if !replace && folder::read(engine.root, &manifest.name)?.is_some() {
        return Err(refused("app_plugin_name_taken"));
    }
    if !folder::fits(engine.root, &manifest.name)? {
        return Err(refused("app_plugin_past_cap"));
    }
    if handed_problem(&manifest, engine.handed_out.as_ref()).is_some() {
        return Err(refused(PORT_HANDED_OUT));
    }
    let replaced = folder::write(
        engine.root,
        &manifest.name,
        &bytes,
        replace,
        engine.keys.new_ref,
    )?;
    if let Some(step) = replaced.and_then(store_log::after_commit) {
        step.commit()?;
    }
    trust::settle(db, engine, &manifest, now_ms)
}

/// Turning a plugin on, or off with what the off does to runs holding it and whether the
/// owner lifted the lock on essential bundled code plugins.
#[derive(Debug, Clone, Copy)]
pub enum Switch<'u> {
    On,
    Off {
        usage: &'u Usage,
        when_off: TurnOff,
        essential_unlocked: bool,
    },
}

/// Turns a plugin on or off. A user plugin turns on only while its trusted text is
/// unchanged. Off writes the off state first, so the next call is refused while a call
/// already running finishes; with [`TurnOff::StopRuns`] each run holding the plugin is then
/// stopped once. Inputs: database, engine facts, name and the switch. Output: Ok, or
/// `app_plugin_not_found`, `_refused`, `_needs_review`, `_changed`.
pub fn set_enabled(
    db: &Connection,
    engine: &Engine<'_>,
    name: &str,
    switch: Switch<'_>,
) -> Result<(), Refused> {
    prepare(db, engine)?;
    if is_code_plugin(name) {
        return switch_code_plugin(db, name, switch);
    }
    let found = found(engine, name)?;
    match switch {
        Switch::On => {
            let manifest = found.parsed.map_err(|_| refused("app_plugin_refused"))?;
            trust::check(db, found.origin, &manifest)?;
            set_flag(db, name, true)
        }
        Switch::Off {
            usage, when_off, ..
        } => {
            set_flag(db, name, false)?;
            if when_off == TurnOff::StopRuns {
                usage.stop_runs(name);
            }
            assert!(!enabled_flag(db, name)?);
            Ok(())
        }
    }
}

/// The switch table key of a bundled code plugin: `code:<id>`, which no user plugin name can
/// carry. Input: the plugin id. Output: the key.
pub(crate) fn code_key(name: &str) -> String {
    format!("{CODE_KEY_PREFIX}{name}")
}

const CODE_KEY_PREFIX: &str = "code:";

/// Turns a bundled code plugin on or off under its `code:<id>` key in the switch table,
/// where no row reads as on. An essential one is refused with `app_plugin_essential` while the
/// `plugins.essential_unlocked` setting is false; turning off with `stop_runs` stops each
/// run holding it once the off state is written. Inputs: database, plugin id and switch.
/// Output: Ok, `app_plugin_essential`, or `app_plugins_unavailable`.
fn switch_code_plugin(db: &Connection, name: &str, switch: Switch<'_>) -> Result<(), Refused> {
    let grant = BUNDLED_GRANTS
        .iter()
        .find(|grant| grant.plugin == name)
        .expect("only a bundled code plugin id switches here");
    let key = code_key(name);
    assert!(!embedding::valid_name(&key));
    match switch {
        Switch::On => set_flag(db, &key, true),
        Switch::Off {
            usage,
            when_off,
            essential_unlocked,
        } => {
            if grant.essential && !essential_unlocked {
                return Err(refused("app_plugin_essential"));
            }
            set_flag(db, &key, false)?;
            if when_off == TurnOff::StopRuns {
                usage.stop_runs(name);
            }
            assert!(off_code_plugins(db)?.contains(name));
            Ok(())
        }
    }
}

/// The bundled code plugins turned off, by plugin id, for the engine's in-memory off set.
/// Only `code:<id>` switch rows count, so a row an older plugin of the owner's left under a
/// bare code plugin id never turns that plugin off; an id no bundled code plugin has is
/// left out. Input: the database. Output: the ids, or `app_plugins_unavailable`.
pub fn off_code_plugins(db: &Connection) -> Result<std::collections::BTreeSet<String>, Refused> {
    let mut statement = db
        .prepare("SELECT name FROM app_plugin_state WHERE enabled=0 AND name LIKE 'code:%'")
        .map_err(sqlite)?;
    let names: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .map_err(sqlite)?
        .collect::<Result<_, _>>()
        .map_err(sqlite)?;
    Ok(names
        .iter()
        .filter_map(|key| key.strip_prefix(CODE_KEY_PREFIX))
        .filter(|name| is_code_plugin(name))
        .map(str::to_owned)
        .collect())
}

/// Removes a user plugin: its folder moved aside under its removal name, then its bindings,
/// trust record and state as one Forgotten event in a savepoint, then the keys entered for
/// it; the folder moved aside and any .yaml file an older version left are deleted only
/// after the capture's commit, and the folder goes back after its rollback. A failed row
/// write, or a savepoint release that cannot commit outside a capture, changes nothing. Account keys stay. Under a bundled plugin's name it removes only
/// what the owner's folder still holds, which the bundled plugin shadows. Outside the
/// capture of its own transaction it is refused before anything is written. Inputs:
/// database, engine facts and name. Output: Ok, or `app_plugin_bundled`,
/// `app_plugin_not_found`, `app_plugins_unavailable`, the database's or the folder's refusal.
pub fn remove(db: &Connection, engine: &Engine<'_>, name: &str) -> Result<(), Refused> {
    let captured = store_log::in_capture(db)?;
    prepare(db, engine)?;
    if is_bundled(name) {
        return if folder::remove(engine.root, name, engine.keys.new_ref)? {
            Ok(())
        } else {
            Err(refused("app_plugin_bundled"))
        };
    }
    if !embedding::valid_name(name) {
        return Err(refused("app_plugin_not_found"));
    }
    let present = folder::read(engine.root, name)?.is_some();
    let known = known(db, name)?;
    if !present && !known {
        return Err(refused("app_plugin_not_found"));
    }
    let retired = if known {
        Some(folder::retire(engine.root, name, engine.keys.new_ref)?)
    } else {
        None
    };
    let bindings = binding_rows(db, name)?;
    if known {
        assert_eq!(db.is_autocommit(), !captured);
        db.execute_batch("SAVEPOINT app_plugin_remove")
            .map_err(sqlite)?;
        let forgotten = store_log::Event::Forgotten {
            plugin: name.to_owned(),
        };
        let written = store_log::record(db, forgotten).and_then(|()| {
            db.execute_batch("RELEASE app_plugin_remove")
                .map_err(sqlite)
        });
        if let Err(refusal) = written {
            let undo = if captured {
                "ROLLBACK TO app_plugin_remove; RELEASE app_plugin_remove"
            } else {
                "ROLLBACK"
            };
            let _ = db.execute_batch(undo);
            return Err(refusal);
        }
    }
    for (_, source, reference) in &bindings {
        forget_key_file(db, engine, source, reference);
    }
    let step = match retired {
        Some(retired) => retired.into_step(engine.keys.new_ref),
        None => folder::removal(engine.root, name, engine.keys.new_ref),
    };
    match store_log::after_commit(step) {
        Some(step) => step.commit(),
        None => Ok(()),
    }
}

/// Loads a plugin to run: valid, trusted as it is now, turned on, and every declared secret
/// bound to a saved key. A plugin whose plugin.toml changed since it was trusted is refused
/// until it is approved again. Inputs: database, engine facts and name. Output:
/// the plugin, or `app_plugin_not_found`, `_refused`, `_needs_review`, `_changed`, `_off`,
/// `_needs_key`.
pub fn ready(db: &Connection, engine: &Engine<'_>, name: &str) -> Result<Loaded, Refused> {
    prepare(db, engine)?;
    let found = found(engine, name)?;
    let manifest = found.parsed.map_err(|_| refused("app_plugin_refused"))?;
    trust::check(db, found.origin, &manifest)?;
    if !enabled_flag(db, &manifest.name)? {
        return Err(refused("app_plugin_off"));
    }
    let mut bindings = Vec::with_capacity(manifest.secrets().len());
    for secret in manifest.secrets() {
        let (_, bound) = resolve(db, engine, &manifest.name, secret)?;
        bindings.push(bound.ok_or_else(|| refused("app_plugin_needs_key"))?);
    }
    assert_eq!(bindings.len(), manifest.secrets().len());
    Ok(Loaded {
        manifest,
        origin: found.origin,
        bindings,
    })
}

/// Loads a plugin for one run and records that the run holds it until the lease drops. A
/// trusted plugin whose plugin.toml changed is refused and keeps one notice of that change
/// in `usage`. Inputs: database, engine facts, the engine's usage, name, the run and its
/// stop. Output: the plugin and its lease, or the refusal of [`ready`], or
/// `plugin_holders_capacity`.
pub fn hold(
    db: &Connection,
    engine: &Engine<'_>,
    usage: &Usage,
    name: &str,
    holder: Holder,
    stop: Box<dyn FnOnce() + Send>,
) -> Result<(Loaded, Lease), Refused> {
    match ready(db, engine, name) {
        Ok(loaded) => {
            usage.settled(name);
            let lease = usage.hold(&loaded.manifest.name, holder, stop)?;
            Ok((loaded, lease))
        }
        Err(changed) if changed.code == "app_plugin_changed" => {
            let current = found(engine, name)?
                .parsed
                .map_err(|_| refused("app_plugin_refused"))?;
            let trusted = trust::trusted_sha256(db, name)?.ok_or(changed)?;
            assert_ne!(trusted, current.sha256);
            usage.note_change(ChangeNotice {
                plugin: current.name,
                trusted_sha256: trusted,
                current_sha256: current.sha256,
            });
            Err(changed)
        }
        Err(other) => Err(other),
    }
}

/// Sweeps what a crash between a staged copy and its rename left in the plugins folder: a
/// retired folder whose plugin name is missing is moved back under that name, one beside
/// the live plugin of its name is left for [`sweep_replaced`], and every other staged or
/// retired entry is deleted. Input: the engine root. Output: Ok, or the first refusal
/// after every entry was tried.
pub(crate) fn remove_staged(root: &Path) -> Result<(), Refused> {
    folder::sweep_within(root, USER_PLUGINS_MAX)
}

/// The start sweep acting on at most `cap` entries, so a row can put an entry outside one
/// sweep's batch. Inputs: the engine root and the cap, 1 to `USER_PLUGINS_MAX`. Output: as
/// [`remove_staged`].
#[cfg(all(test, unix))]
pub(crate) fn remove_staged_within(root: &Path, cap: usize) -> Result<(), Refused> {
    folder::sweep_within(root, cap)
}

/// Whether any row a removal deletes names the plugin: its trust record, its switch or a
/// secret binding. Inputs: the database and the name. Output: the answer, or the
/// database's refusal.
fn known(db: &Connection, name: &str) -> Result<bool, Refused> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_plugin_trust WHERE name=?1)
         OR EXISTS(SELECT 1 FROM app_plugin_state WHERE name=?1)
         OR EXISTS(SELECT 1 FROM app_plugin_secrets WHERE plugin=?1)",
        [name],
        |row| row.get(0),
    )
    .map_err(sqlite)
}

/// Settles at start each folder a removal moved aside, by the rows that removal deletes:
/// while they still name the plugin, the removal never committed and the folder goes back
/// under the name when that name is missing; otherwise it loses the name and is deleted.
/// Inputs: database and engine facts. Output: Ok, or the first refusal after every entry
/// was tried.
pub fn sweep_removals(db: &Connection, engine: &Engine<'_>) -> Result<(), Refused> {
    folder::sweep_removals(engine.root, |name| known(db, name))
}

/// Settles at start each retired folder a replace left beside the live plugin of its name,
/// by the SHA-256 the trust row holds: deleted while the live text is trusted, put back in
/// place of the live folder while only its own text is trusted, since that replace never
/// committed, and otherwise kept. Inputs: the database and the engine root. Output: Ok, or
/// the first refusal after every name was tried.
pub fn sweep_replaced(db: &Connection, root: &Path) -> Result<(), Refused> {
    folder::sweep_replaced(root, |name| trust::trusted_sha256(db, name))
}

const STAGED: &str = ".add-";
const RETIRED: &str = ".old-";
const REMOVING: &str = ".rm-";
const OWED: &str = ".back-";

/// The most folder levels below a plugin folder that deleting it goes down.
const TREE_DEPTH_MAX: usize = 4;

/// The most entries deleting a plugin folder reads from any one folder in it.
const TREE_ENTRIES_MAX: usize = 64;

/// Whether a plugins folder entry is a staged copy, a retired folder or a folder owed back
/// to its plugin name. Input: the entry name. Output: the answer.
fn left_over(file: &[u8]) -> bool {
    file.starts_with(STAGED.as_bytes())
        || file.starts_with(RETIRED.as_bytes())
        || file.starts_with(OWED.as_bytes())
}

/// The retired folder name a replace moves a plugin's old folder to: the plugin name and
/// the write id. Inputs: a valid name and the id. Output: the folder name.
fn retired(name: &str, id: &str) -> String {
    assert!(embedding::valid_name(name));
    format!("{RETIRED}{name}.{id}")
}

/// The plugin name a retired folder was moved aside from. Input: the entry name. Output:
/// the name, or none when the entry is no retired folder or carries no valid name.
fn retired_name(file: &str) -> Option<&str> {
    let (name, _id) = file.strip_prefix(RETIRED)?.rsplit_once('.')?;
    embedding::valid_name(name).then_some(name)
}

/// The folder name a rolled-back replace gives the original folder it could not put back
/// under the plugin name, so the start sweep puts it back: the plugin name and the write id.
/// Inputs: a valid name and the id. Output: the folder name.
fn owed(name: &str, id: &str) -> String {
    assert!(embedding::valid_name(name));
    format!("{OWED}{name}.{id}")
}

/// The plugin name a folder is owed back to. Input: the entry name. Output: the name, or
/// none when the entry is no owed folder or carries no valid name.
fn owed_name(file: &str) -> Option<&str> {
    let (name, _id) = file.strip_prefix(OWED)?.rsplit_once('.')?;
    embedding::valid_name(name).then_some(name)
}

/// The folder name a removal moves a plugin's folder to until its rows are gone: the plugin
/// name and an id. Inputs: a valid name and the id. Output: the folder name.
fn removing(name: &str, id: &str) -> String {
    assert!(embedding::valid_name(name));
    format!("{REMOVING}{name}.{id}")
}

/// The plugin name a removal moved a folder aside from. Input: the entry name. Output: the
/// name, or none when the entry is no removal folder or carries no valid name.
fn removing_name(file: &str) -> Option<&str> {
    let (name, _id) = file.strip_prefix(REMOVING)?.rsplit_once('.')?;
    embedding::valid_name(name).then_some(name)
}

/// One user plugin the folder listing returns: its name and its plugin.toml bytes, or the
/// refusal that kept it unread.
type UserFile = (String, Result<Vec<u8>, Problem>);

#[cfg(not(any(unix, windows)))]
compile_error!("the plugins folder is built for Unix and Windows only");

#[path = "registry_folder.rs"]
mod folder;

pub use folder::FolderStep;
