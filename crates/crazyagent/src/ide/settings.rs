use crate::engine::EngineError;
use crate::engine::owner::failure;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// The file in a project's IDE folder that lists every locked key with its value, for the
/// built-in extension to re-assert in the owner's User settings.
pub const LOCKED_FILE: &str = "locked-settings.json";
/// The environment variable that names [`LOCKED_FILE`] to the server's extension host.
pub const LOCKED_VARIABLE: &str = "CRAZYAGENT_IDE_LOCKED_FILE";
/// The theme keys no lock layer sets, so any theme stays selectable; CrazyAgent's own
/// themes are only [`defaults`].
pub const THEME_KEYS: [&str; 3] = [
    "window.autoDetectColorScheme",
    "workbench.preferredDarkColorTheme",
    "workbench.preferredLightColorTheme",
];

const USER_DATA: &str = "data";
const MACHINE: &str = "Machine";
#[cfg(unix)]
const SETTINGS: &str = "settings.json";
#[cfg(unix)]
const UNREADABLE: &str = "settings.json.unreadable";
#[cfg(unix)]
const PREVIOUS: &str = "settings.json.previous";
#[cfg(unix)]
const MACHINE_TEMPORARY: &str = "machine-settings.json.tmp";
#[cfg(unix)]
const LOCKED_TEMPORARY: &str = "locked-settings.json.tmp";
#[cfg(unix)]
const SHIM_TEMPORARY: &str = "git-shim.tmp";
#[cfg(unix)]
const SETTINGS_BYTES_MAX: u64 = 256 * 1024;

/// The lock files written for one server start: its Remote settings file and the locked-keys
/// file the extension reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub machine: PathBuf,
    pub locked: PathBuf,
}

/// Writes the lock before each server start: the Remote settings file
/// `<server data>/data/Machine/settings.json`, which VSCodium reh-web reads above the owner's
/// User settings, holding every [`locked`] key, the `extensions.allowed` pin list of the
/// owner's approved extensions ([`super::extensions::allowed_setting`]), the IDE terminal
/// profiles ([`super::terminal::machine_profiles`]) and `git.path` naming the git shim
/// [`super::git::SHIM`] written next to it first ([`super::git::shim_text`],
/// [`super::git::machine_settings`]), and no other key; and `<folder>/locked-settings.json`
/// for the extension. Each file is written whole in the engine's private IDE folder and
/// renamed into place, so the server never reads a partial file and a link to the old file
/// changes nothing. The server can never write that folder, so a Remote settings file
/// holding any other key, which only an earlier server could have left, is set aside as
/// `settings.json.previous` and none of it is carried forward; one that is not a JSON object
/// is kept aside as `settings.json.unreadable`; a link there is replaced, never followed. The
/// server's profile locks the whole Remote settings folder
/// ([`machine_folder`]), so the server can never write, add, remove or move anything there.
/// Inputs: the project's IDE folder, the server data folder inside it, the engine's data
/// folder above both and, when the owner lets the IDE run the project's hooks, the project
/// folder. Output: the written files, or a plain refusal.
#[cfg(unix)]
pub fn write(
    folder: &Path,
    server_data: &Path,
    data: &Path,
    project_hooks: Option<&Path>,
) -> Result<Written, EngineError> {
    if !folder.is_absolute()
        || !folder.starts_with(data)
        || folder == data
        || !server_data.starts_with(folder)
        || server_data == folder
        || server_data
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(failure("ide_settings_invalid"));
    }
    let user_data = user_data_folder(server_data);
    let machine = machine_folder(server_data);
    for path in [folder, server_data, user_data.as_path(), machine.as_path()] {
        super::bridge::private_folder(path)?;
    }
    let shim = machine.join(super::git::SHIM);
    replace(
        folder,
        SHIM_TEMPORARY,
        &shim,
        super::git::shim_text(project_hooks, crate::research::runner::sandboxed_git())?.as_bytes(),
        0o700,
    )?;
    let target = machine.join(SETTINGS);
    let locked = locked();
    let mut settings = locked.clone();
    settings.extend(super::terminal::machine_profiles()?);
    settings.extend(super::extensions::allowed_setting(data));
    settings.extend(super::git::machine_settings(&shim)?);
    let aside = match owner_settings(&target)? {
        Owner::Held(held) if held.keys().all(|key| settings.contains_key(key)) => None,
        Owner::Held(_) => Some(PREVIOUS),
        Owner::Unreadable => Some(UNREADABLE),
        Owner::Absent => None,
    };
    if let Some(name) = aside {
        std::fs::rename(&target, machine.join(name)).map_err(unwritable)?;
    }
    assert!(
        locked
            .iter()
            .all(|(key, value)| settings.get(key) == Some(value))
    );
    replace(
        folder,
        MACHINE_TEMPORARY,
        &target,
        &pretty(&Value::Object(settings))?,
        0o600,
    )?;
    let locked_path = folder.join(LOCKED_FILE);
    replace(
        folder,
        LOCKED_TEMPORARY,
        &locked_path,
        &pretty(&Value::Object(locked))?,
        0o600,
    )?;
    Ok(Written {
        machine: target,
        locked: locked_path,
    })
}

/// Refuses the lock where the engine has no Unix file modes, so no IDE server starts.
/// Takes the folders and the hooks switch, unused. Returns `ide_unavailable`.
#[cfg(not(unix))]
pub fn write(
    _folder: &Path,
    _server_data: &Path,
    _data: &Path,
    _project_hooks: Option<&Path>,
) -> Result<Written, EngineError> {
    Err(failure("ide_unavailable"))
}

/// The Remote settings folder `<server data>/data/Machine` that [`write`] fills before each
/// server start. The server's profile locks it as a whole, matched in any letter case, and
/// keeps the folders up to the server data folder from being moved, since a literal deny
/// leaves the granted parents movable. A read-only file mode alone is not enough: the
/// workbench offers to overwrite it. Input: the server data folder. Output: the folder.
pub fn machine_folder(server_data: &Path) -> PathBuf {
    user_data_folder(server_data).join(MACHINE)
}

/// The user data folder `<server data>/data` the server is started with as its
/// `--user-data-dir`, the server's own default place, so its Remote settings folder is the
/// one [`write`] fills and the profile locks. Input: the server data folder. Output: the
/// folder.
pub fn user_data_folder(server_data: &Path) -> PathBuf {
    server_data.join(USER_DATA)
}

/// Every locked key with its value: the Remote settings, workspace and application keys.
/// It never holds a [`THEME_KEYS`] key. Output: the keys.
pub fn locked() -> Map<String, Value> {
    let mut all = machine();
    all.extend(workspace());
    all.extend(application());
    all
}

/// The keys the generated workspace file carries, the layer above Remote settings that the
/// workbench applies for window keys: AI features off again, since a window key there would
/// override Remote settings, MCP access, extension tools, the settings AI search toggle,
/// extension recommendations and the start editor. Output: the keys.
pub fn workspace() -> Map<String, Value> {
    table([
        ("chat.disableAIFeatures", json!(true)),
        ("chat.mcp.access", json!("none")),
        ("chat.extensionTools.enabled", json!(false)),
        ("workbench.settings.showAISearchToggle", json!(false)),
        ("extensions.ignoreRecommendations", json!(true)),
        ("workbench.startupEditor", json!("none")),
    ])
}

/// The embedder defaults the IDE proxy gives the workbench page, which hold from its first
/// render before Remote settings load: every locked key, plus CrazyAgent's themes following
/// the system appearance and a secondary side bar that starts hidden, since its default
/// container is the disabled Chat. The owner may change these. Output: the defaults.
pub fn defaults() -> Map<String, Value> {
    let mut all = locked();
    all.extend(table([
        (THEME_KEYS[0], json!(true)),
        (THEME_KEYS[1], json!("CrazyAgent Dark")),
        (THEME_KEYS[2], json!("CrazyAgent Light")),
        (
            "workbench.secondarySideBar.defaultVisibility",
            json!("hidden"),
        ),
    ]));
    all
}

/// The keys Remote settings apply: AI features, agents and the command center chat off, the
/// MCP gallery, terminal hint, tips, start editor, announcements, walkthroughs, remote
/// recommendations, semantic search and telemetry. Output: the keys.
fn machine() -> Map<String, Value> {
    table([
        ("chat.disableAIFeatures", json!(true)),
        ("chat.agent.enabled", json!(false)),
        ("chat.commandCenter.enabled", json!(false)),
        ("chat.mcp.gallery.enabled", json!(false)),
        ("window.commandCenter", json!(false)),
        ("terminal.integrated.initialHint", json!(false)),
        ("workbench.tips.enabled", json!(false)),
        ("workbench.startupEditor", json!("none")),
        ("workbench.welcomePage.extraAnnouncements", json!(false)),
        (
            "workbench.welcomePage.walkthroughs.openOnInstall",
            json!(false),
        ),
        (
            "workbench.remoteIndicator.showExtensionRecommendations",
            json!(false),
        ),
        ("search.searchView.semanticSearchBehavior", json!("manual")),
        ("telemetry.telemetryLevel", json!("off")),
    ])
}

/// The application keys neither Remote settings nor the workspace file apply, held by the
/// embedder defaults and the extension's re-assert in User settings: no updates, extension
/// auto-updates or update checks, and no automatic tasks. Output: the keys.
fn application() -> Map<String, Value> {
    table([
        ("update.mode", json!("none")),
        ("extensions.autoUpdate", json!("off")),
        ("extensions.autoCheckUpdates", json!(false)),
        ("task.allowAutomaticTasks", json!("off")),
    ])
}

fn table<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// What the Remote settings file held before this write.
#[cfg(unix)]
enum Owner {
    Absent,
    Held(Map<String, Value>),
    Unreadable,
}

/// Reads the keys the Remote settings file holds without following a link or waiting on a
/// special file. Input: its path. Output: absent when missing or not a regular file, the
/// object, or unreadable when it holds no JSON object or is over `SETTINGS_BYTES_MAX` bytes.
#[cfg(unix)]
fn owner_settings(path: &Path) -> Result<Owner, EngineError> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Owner::Absent),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(Owner::Absent),
        Err(error) => return Err(unwritable(error)),
    };
    if !file.metadata().map_err(unwritable)?.is_file() {
        return Ok(Owner::Absent);
    }
    let mut bytes = Vec::new();
    file.take(SETTINGS_BYTES_MAX + 1)
        .read_to_end(&mut bytes)
        .map_err(unwritable)?;
    if bytes.len() as u64 > SETTINGS_BYTES_MAX {
        return Ok(Owner::Unreadable);
    }
    Ok(match serde_json::from_slice(&bytes) {
        Ok(Value::Object(settings)) => Owner::Held(settings),
        _ => Owner::Unreadable,
    })
}

/// A settings file's text: the JSON, pretty-printed. Input: the value. Output: its bytes, or
/// `ide_settings_unwritable`.
#[cfg(unix)]
fn pretty(body: &Value) -> Result<Vec<u8>, EngineError> {
    serde_json::to_vec_pretty(body).map_err(unwritable)
}

/// Replaces a file whole: writes a new file with `mode` in the engine's private IDE folder,
/// syncs it and renames it over the target. Inputs: the folder, the temporary name, the
/// target, the bytes and the mode. Output: Ok once renamed, or `ide_settings_unwritable`.
#[cfg(unix)]
pub(super) fn replace(
    folder: &Path,
    temporary: &str,
    target: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), EngineError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    assert!(mode & 0o077 == 0);
    let temporary = folder.join(temporary);
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unwritable(error)),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .map_err(unwritable)?;
    file.write_all(bytes).map_err(unwritable)?;
    file.sync_all().map_err(unwritable)?;
    std::fs::rename(&temporary, target).map_err(unwritable)
}

#[cfg(unix)]
fn unwritable<E>(_: E) -> EngineError {
    failure("ide_settings_unwritable")
}

#[cfg(all(test, unix))]
mod tests;
