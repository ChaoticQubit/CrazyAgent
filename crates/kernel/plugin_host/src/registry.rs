use std::ops::RangeInclusive;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Refused;
use crate::embedding::{self, Manifest};
use crate::manifest::MANIFEST_BYTES_MAX;
use crate::problem::Problem;
use crate::store::{Keys, Loaded, binding_rows, drop_binding, refused, resolve, sqlite};
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
                } else if is_bundled(&manifest.name) {
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

/// Drops what an older plugin of the owner's left under a name a bundled plugin now has:
/// its secret bindings with the keys entered for it, its switch and its trust record. A
/// bundled plugin never gets a trust record, so one is the mark of such a leftover, and it
/// goes last so an interrupted pass runs again. Each key is deleted before its binding
/// row, so a key that cannot be deleted stops the pass with its row still there. Inputs:
/// database and engine facts. Output: Ok, `app_plugins_unavailable`, or
/// `app_plugin_key_store_unavailable`.
pub(crate) fn prepare(db: &Connection, engine: &Engine<'_>) -> Result<(), Refused> {
    for (name, _) in BUNDLED {
        let leftover: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM app_plugin_trust WHERE name=?1)",
                [name],
                |row| row.get(0),
            )
            .map_err(sqlite)?;
        if !leftover {
            continue;
        }
        for (secret, source, reference) in binding_rows(db, name)? {
            drop_binding(db, engine, name, &secret, &source, &reference)?;
        }
        db.execute("DELETE FROM app_plugin_state WHERE name=?1", [name])
            .map_err(sqlite)?;
        db.execute("DELETE FROM app_plugin_trust WHERE name=?1", [name])
            .map_err(sqlite)?;
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

pub(crate) fn set_flag(db: &Connection, name: &str, enabled: bool) -> Result<(), Refused> {
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
        PluginStatus::NeedsReview
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
    if is_bundled(&manifest.name) {
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
/// or `app_plugin_changed`, `_refused`, `_name_taken`, or the folder's refusal.
pub fn add(
    db: &Connection,
    engine: &Engine<'_>,
    source: &Path,
    sha256: &str,
    replace: bool,
    now_ms: i64,
) -> Result<(), Refused> {
    prepare(db, engine)?;
    let bytes = folder::read_source(source).map_err(|_| refused("app_plugin_refused"))?;
    if kernel_common::hash(&bytes) != sha256 {
        return Err(refused("app_plugin_changed"));
    }
    let manifest = parse_file(&bytes).map_err(|_| refused("app_plugin_refused"))?;
    assert_eq!(manifest.sha256, sha256);
    if is_bundled(&manifest.name) {
        return Err(refused("app_plugin_name_taken"));
    }
    if !replace && folder::read(engine.root, &manifest.name)?.is_some() {
        return Err(refused("app_plugin_name_taken"));
    }
    folder::write(engine.root, &manifest.name, &bytes, engine.keys.new_ref)?;
    trust::settle(db, engine, &manifest, now_ms)
}

/// Turning a plugin on, or off with what the off does to runs holding it.
#[derive(Debug, Clone, Copy)]
pub enum Switch<'u> {
    On,
    Off { usage: &'u Usage, when_off: TurnOff },
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
    let found = found(engine, name)?;
    match switch {
        Switch::On => {
            let manifest = found.parsed.map_err(|_| refused("app_plugin_refused"))?;
            trust::check(db, found.origin, &manifest)?;
            set_flag(db, name, true)
        }
        Switch::Off { usage, when_off } => {
            set_flag(db, name, false)?;
            if when_off == TurnOff::StopRuns {
                usage.stop_runs(name);
            }
            assert!(!enabled_flag(db, name)?);
            Ok(())
        }
    }
}

/// Removes a user plugin: its folder (or the .yaml file an older version left), trust
/// record, state, bindings and the keys entered for it. Account keys stay. Under a bundled
/// plugin's name it removes only what the owner's folder still holds, which the bundled
/// plugin shadows. Inputs: database, engine facts and name. Output: Ok, or
/// `app_plugin_bundled`, `app_plugin_not_found`, or the folder's refusal.
pub fn remove(db: &Connection, engine: &Engine<'_>, name: &str) -> Result<(), Refused> {
    prepare(db, engine)?;
    if is_bundled(name) {
        return if folder::remove(engine.root, name)? {
            Ok(())
        } else {
            Err(refused("app_plugin_bundled"))
        };
    }
    if !embedding::valid_name(name) {
        return Err(refused("app_plugin_not_found"));
    }
    let removed = folder::remove(engine.root, name)?;
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
        return Err(refused("app_plugin_not_found"));
    }
    for (secret, source, reference) in binding_rows(db, name)? {
        drop_binding(db, engine, name, &secret, &source, &reference)?;
    }
    db.execute("DELETE FROM app_plugin_trust WHERE name=?1", [name])
        .map_err(sqlite)?;
    db.execute("DELETE FROM app_plugin_state WHERE name=?1", [name])
        .map_err(sqlite)?;
    Ok(())
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

/// Deletes the plugin folders a crash between a staged copy and its rename left. Input:
/// the engine root. Output: Ok, or the folder's refusal.
pub(crate) fn remove_staged(root: &Path) -> Result<(), Refused> {
    folder::remove_staged(root)
}

/// One user plugin the folder listing returns: its name and its plugin.toml bytes, or the
/// refusal that kept it unread.
type UserFile = (String, Result<Vec<u8>, Problem>);

#[cfg(unix)]
mod folder {
    use super::{FOLDER, MANIFEST_FILE, Problem, USER_PLUGINS_MAX, UserFile};

    use crate::Refused;
    use crate::embedding::valid_name;
    use crate::manifest::MANIFEST_BYTES_MAX;
    use crate::store::refused;
    use rustix::fs::{AtFlags, Mode, OFlags};
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    const STAGED: &str = ".add-";
    const RETIRED: &str = ".old-";
    const LEGACY_SUFFIX: &str = ".yaml";

    /// The most entries read from one plugin folder before it is refused for holding more
    /// than its plugin.toml.
    const ENTRIES_MAX: usize = 4;

    /// What one name in the plugins folder is: a plugin folder, or the `.yaml` file a
    /// version before plugin folders wrote.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Entry {
        Folder,
        Legacy,
    }

    fn unavailable() -> Refused {
        refused("app_plugin_folder_unavailable")
    }

    fn unsafe_entry() -> Problem {
        Problem::new("app_plugin_file_unsafe", None, None)
    }

    fn legacy() -> Problem {
        Problem::new("app_plugin_format_old", None, None)
    }

    fn open(root: &Path, create: bool) -> Result<Option<std::fs::File>, Refused> {
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
                return Err(refused("app_plugin_folder_unsafe"));
            }
            Err(_) => return Err(unavailable()),
        };
        let metadata = directory.metadata().map_err(|_| unavailable())?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::getuid().as_raw()
            || metadata.mode() & 0o7777 != 0o700
        {
            return Err(refused("app_plugin_folder_unsafe"));
        }
        Ok(Some(directory))
    }

    fn entry_of(file: &[u8]) -> Option<(&str, Entry)> {
        let name = std::str::from_utf8(file).ok()?;
        if let Some(stem) = name.strip_suffix(LEGACY_SUFFIX) {
            return valid_name(stem).then_some((stem, Entry::Legacy));
        }
        valid_name(name).then_some((name, Entry::Folder))
    }

    /// Reads a plugin folder's plugin.toml: the folder opened without following a link,
    /// holding that one entry, the file a plain file read without following a link or
    /// waiting on a FIFO, at most `MANIFEST_BYTES_MAX` + 1 bytes. Inputs: the folder and
    /// whether it must be owned by this user. Output: the bytes, or the refusal.
    fn read_dir(directory: std::fs::File, owned: bool) -> Result<Vec<u8>, Problem> {
        use std::io::Read;
        let uid = rustix::process::getuid().as_raw();
        let metadata = directory.metadata().map_err(|_| unsafe_entry())?;
        if !metadata.is_dir() || (owned && metadata.uid() != uid) {
            return Err(unsafe_entry());
        }
        let mut seen = 0;
        for entry in rustix::fs::Dir::read_from(&directory).map_err(|_| unsafe_entry())? {
            let entry = entry.map_err(|_| unsafe_entry())?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            seen += 1;
            if name != MANIFEST_FILE.as_bytes() || seen >= ENTRIES_MAX {
                return Err(Problem::new("plugin_folder_extra_entry", None, None));
            }
        }
        assert!(seen < ENTRIES_MAX);
        let file = std::fs::File::from(
            rustix::fs::openat(
                &directory,
                MANIFEST_FILE,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|error| match error {
                rustix::io::Errno::NOENT => Problem::new("plugin_manifest_invalid", Some(1), None),
                _ => unsafe_entry(),
            })?,
        );
        let metadata = file.metadata().map_err(|_| unsafe_entry())?;
        if !metadata.is_file() || (owned && metadata.uid() != uid) {
            return Err(unsafe_entry());
        }
        let mut bytes = Vec::new();
        file.take(MANIFEST_BYTES_MAX as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unsafe_entry())?;
        assert!(bytes.len() <= MANIFEST_BYTES_MAX + 1);
        Ok(bytes)
    }

    fn read_in(directory: &std::fs::File, name: &str) -> Result<Vec<u8>, Problem> {
        let folder = rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unsafe_entry())?;
        read_dir(std::fs::File::from(folder), true)
    }

    /// Reads a folder the owner picked to add: not a link itself, opened without following
    /// one, holding its plugin.toml only. Input: the folder. Output: the
    /// plugin.toml bytes, or the refusal.
    pub(super) fn read_source(source: &Path) -> Result<Vec<u8>, Problem> {
        let metadata = std::fs::symlink_metadata(source).map_err(|_| unsafe_entry())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(unsafe_entry());
        }
        let folder = rustix::fs::open(
            source,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unsafe_entry())?;
        read_dir(std::fs::File::from(folder), false)
    }

    fn names(directory: &std::fs::File) -> Result<(BTreeMap<String, Entry>, bool), Refused> {
        let mut names = BTreeMap::new();
        let mut more = false;
        for entry in rustix::fs::Dir::read_from(directory).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let Some((name, kind)) = entry_of(entry.file_name().to_bytes()) else {
                continue;
            };
            let kept = names.entry(name.to_owned()).or_insert(kind);
            if kind == Entry::Folder {
                *kept = Entry::Folder;
            }
            if names.len() > USER_PLUGINS_MAX {
                names.pop_last();
                more = true;
            }
        }
        assert!(names.len() <= USER_PLUGINS_MAX);
        Ok((names, more))
    }

    /// The owner's plugins by name, at most `USER_PLUGINS_MAX`, each read without following
    /// a link; a `.yaml` file an older version wrote is listed refused and kept. Input: the
    /// engine root. Output: (name, bytes or refusal) pairs, whether more exist, or the
    /// folder's refusal.
    pub(super) fn entries(root: &Path) -> Result<(Vec<UserFile>, bool), Refused> {
        let Some(directory) = open(root, false)? else {
            return Ok((Vec::new(), false));
        };
        let (names, more) = names(&directory)?;
        Ok((
            names
                .into_iter()
                .map(|(name, kind)| {
                    let read = match kind {
                        Entry::Folder => read_in(&directory, &name),
                        Entry::Legacy => Err(legacy()),
                    };
                    (name, read)
                })
                .collect(),
            more,
        ))
    }

    /// Whether a plugin is among the first `USER_PLUGINS_MAX` by name, the ones `entries`
    /// lists; one past them is refused so it never runs with an unkept port. Inputs: the
    /// engine root and a valid plugin name. Output: the answer, or the folder's refusal.
    pub(super) fn listed(root: &Path, name: &str) -> Result<bool, Refused> {
        let Some(directory) = open(root, false)? else {
            return Ok(false);
        };
        let (names, _) = names(&directory)?;
        Ok(names.contains_key(name))
    }

    fn exists(directory: &std::fs::File, name: &str) -> Result<bool, Refused> {
        match rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(_) => Err(unavailable()),
        }
    }

    /// One user plugin. Inputs: the engine root and a valid plugin name. Output: none when
    /// it is missing, else its plugin.toml bytes or the refusal (a left-over `.yaml` file
    /// is `app_plugin_format_old`); or the folder's refusal.
    pub(super) fn read(
        root: &Path,
        name: &str,
    ) -> Result<Option<Result<Vec<u8>, Problem>>, Refused> {
        let Some(directory) = open(root, false)? else {
            return Ok(None);
        };
        if exists(&directory, name)? {
            return Ok(Some(read_in(&directory, name)));
        }
        if exists(&directory, &format!("{name}{LEGACY_SUFFIX}"))? {
            return Ok(Some(Err(legacy())));
        }
        Ok(None)
    }

    /// Deletes one plugin folder: each plain entry in it, at most `ENTRIES_MAX`, then the
    /// folder. A folder in it is refused, so nothing outside the plugin is touched.
    fn remove_tree(directory: &std::fs::File, name: &str) -> Result<(), Refused> {
        let folder = std::fs::File::from(
            rustix::fs::openat(
                directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| unavailable())?,
        );
        let mut inside = Vec::new();
        for entry in rustix::fs::Dir::read_from(&folder).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let file = entry.file_name();
            if file.to_bytes() == b"." || file.to_bytes() == b".." {
                continue;
            }
            if entry.file_type() == rustix::fs::FileType::Directory {
                return Err(unavailable());
            }
            inside.push(file.to_owned());
            if inside.len() > ENTRIES_MAX {
                return Err(unavailable());
            }
        }
        for file in &inside {
            match rustix::fs::unlinkat(&folder, file.as_c_str(), AtFlags::empty()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(_) => return Err(unavailable()),
            }
        }
        rustix::fs::unlinkat(directory, name, AtFlags::REMOVEDIR).map_err(|_| unavailable())
    }

    /// Writes a user plugin as a staged 0700 folder holding a 0600 plugin.toml, renamed into
    /// place; an older folder of that name is moved aside first and deleted after, creating
    /// the 0700 plugins folder when missing. Inputs: the engine root, a valid name, the
    /// bytes and the random id source. Output: Ok with the folder synced, or the folder's
    /// refusal with nothing staged left.
    pub(super) fn write(
        root: &Path,
        name: &str,
        bytes: &[u8],
        new_ref: fn() -> Result<String, &'static str>,
    ) -> Result<(), Refused> {
        let directory = open(root, true)?.ok_or_else(unavailable)?;
        let id = new_ref().map_err(refused)?;
        let staged = format!("{STAGED}{id}");
        rustix::fs::mkdirat(&directory, staged.as_str(), Mode::from_raw_mode(0o700))
            .map_err(|_| unavailable())?;
        let written = (|| {
            let folder = std::fs::File::from(
                rustix::fs::openat(
                    &directory,
                    staged.as_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| unavailable())?,
            );
            let mut file = std::fs::File::from(
                rustix::fs::openat(
                    &folder,
                    MANIFEST_FILE,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(|_| unavailable())?,
            );
            std::io::Write::write_all(&mut file, bytes)
                .and_then(|()| file.sync_all())
                .and_then(|()| folder.sync_all())
                .map_err(|_| unavailable())?;
            let retired = format!("{RETIRED}{id}");
            let replaced = exists(&directory, name)?;
            if replaced {
                rustix::fs::renameat(&directory, name, &directory, retired.as_str())
                    .map_err(|_| unavailable())?;
            }
            rustix::fs::renameat(&directory, staged.as_str(), &directory, name)
                .map_err(|_| unavailable())?;
            if replaced {
                remove_tree(&directory, &retired)?;
            }
            Ok(())
        })();
        if written.is_err() {
            let _ = remove_tree(&directory, &staged);
        }
        written?;
        directory.sync_all().map_err(|_| unavailable())
    }

    /// Deletes the staged and retired plugin folders a crash between a write and its rename
    /// left, at most `USER_PLUGINS_MAX` a start. Input: the engine root. Output: Ok once
    /// those found are gone or the folder is missing, or the folder's refusal.
    pub(super) fn remove_staged(root: &Path) -> Result<(), Refused> {
        let Some(directory) = open(root, false)? else {
            return Ok(());
        };
        let mut staged = Vec::new();
        for entry in rustix::fs::Dir::read_from(&directory).map_err(|_| unavailable())? {
            let entry = entry.map_err(|_| unavailable())?;
            let name = entry.file_name().to_bytes();
            if name.starts_with(STAGED.as_bytes()) || name.starts_with(RETIRED.as_bytes()) {
                staged.push(String::from_utf8_lossy(name).into_owned());
                if staged.len() == USER_PLUGINS_MAX {
                    break;
                }
            }
        }
        assert!(staged.len() <= USER_PLUGINS_MAX);
        for name in &staged {
            remove_tree(&directory, name)?;
        }
        directory.sync_all().map_err(|_| unavailable())
    }

    /// Deletes a user plugin's folder and any `.yaml` file an older version left under its
    /// name; an entry of that name that is not a folder, such as a link, is unlinked itself
    /// without following it. Inputs: the engine root and a valid name. Output: whether
    /// anything was deleted, or the folder's refusal.
    pub(super) fn remove(root: &Path, name: &str) -> Result<bool, Refused> {
        let Some(directory) = open(root, false)? else {
            return Ok(false);
        };
        let kind = match rustix::fs::statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Some(rustix::fs::FileType::from_raw_mode(stat.st_mode)),
            Err(rustix::io::Errno::NOENT) => None,
            Err(_) => return Err(unavailable()),
        };
        match kind {
            Some(rustix::fs::FileType::Directory) => remove_tree(&directory, name)?,
            Some(_) => match rustix::fs::unlinkat(&directory, name, AtFlags::empty()) {
                Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                Err(_) => return Err(unavailable()),
            },
            None => {}
        }
        let folder = kind.is_some();
        let legacy = match rustix::fs::unlinkat(
            &directory,
            format!("{name}{LEGACY_SUFFIX}").as_str(),
            AtFlags::empty(),
        ) {
            Ok(()) => true,
            Err(rustix::io::Errno::NOENT) => false,
            Err(_) => return Err(unavailable()),
        };
        Ok(folder || legacy)
    }
}

#[cfg(not(unix))]
mod folder {
    use super::{MANIFEST_FILE, Problem, UserFile};
    use crate::Refused;
    use crate::manifest::{Bounded, MANIFEST_BYTES_MAX, read_bounded};
    use crate::store::refused;
    use std::path::Path;

    /// No user plugins folder off Unix: nothing is listed. Output: an empty listing.
    pub(super) fn entries(_root: &Path) -> Result<(Vec<UserFile>, bool), Refused> {
        Ok((Vec::new(), false))
    }

    /// No user plugins folder off Unix, so no plugin is past its cap. Output: true.
    pub(super) fn listed(_root: &Path, _name: &str) -> Result<bool, Refused> {
        Ok(true)
    }

    /// No user plugins folder off Unix. Output: none.
    pub(super) fn read(
        _root: &Path,
        _name: &str,
    ) -> Result<Option<Result<Vec<u8>, Problem>>, Refused> {
        Ok(None)
    }

    /// Reads a folder the owner picked: not a reparse point, holding its plugin.toml only,
    /// read through the guarded open. Input: the folder. Output: the bytes, or the refusal.
    pub(super) fn read_source(source: &Path) -> Result<Vec<u8>, Problem> {
        let unsafe_entry = || Problem::new("app_plugin_file_unsafe", None, None);
        let metadata = std::fs::symlink_metadata(source).map_err(|_| unsafe_entry())?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(unsafe_entry());
        }
        let mut seen = 0;
        for entry in std::fs::read_dir(source).map_err(|_| unsafe_entry())? {
            let entry = entry.map_err(|_| unsafe_entry())?;
            seen += 1;
            if entry.file_name() != MANIFEST_FILE || seen > 1 {
                return Err(Problem::new("plugin_folder_extra_entry", None, None));
            }
        }
        match read_bounded(&source.join(MANIFEST_FILE), MANIFEST_BYTES_MAX) {
            Bounded::Text(text) => Ok(text.into_bytes()),
            Bounded::TooLarge | Bounded::Unreadable => {
                Err(Problem::new("plugin_manifest_invalid", Some(1), None))
            }
        }
    }

    /// No user plugins folder off Unix. Output: `app_plugin_folder_unavailable`.
    pub(super) fn write(
        _root: &Path,
        _name: &str,
        _bytes: &[u8],
        _new_ref: fn() -> Result<String, &'static str>,
    ) -> Result<(), Refused> {
        Err(refused("app_plugin_folder_unavailable"))
    }

    /// No user plugins folder off Unix. Output: Ok, nothing to delete.
    pub(super) fn remove_staged(_root: &Path) -> Result<(), Refused> {
        Ok(())
    }

    /// No user plugins folder off Unix. Output: false, nothing was deleted.
    pub(super) fn remove(_root: &Path, _name: &str) -> Result<bool, Refused> {
        Ok(false)
    }
}
