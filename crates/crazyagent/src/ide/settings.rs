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
const SETTINGS: &str = "settings.json";
#[cfg(unix)]
const UNREADABLE: &str = "settings.json.unreadable";
#[cfg(unix)]
const MACHINE_TEMPORARY: &str = "machine-settings.json.tmp";
#[cfg(unix)]
const LOCKED_TEMPORARY: &str = "locked-settings.json.tmp";
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
/// User settings, holding every [`locked`] key over the owner's own keys there, which are
/// kept; and `<folder>/locked-settings.json` for the extension. Each file is written whole in
/// the engine's private IDE folder and renamed into place, so the server never reads a
/// partial file and a link to the old file changes nothing. A Remote settings file that is
/// not a JSON object is kept aside as `settings.json.unreadable`; a link there is replaced,
/// never followed. The server's profile locks the whole Remote settings folder
/// ([`machine_folder`]), so the server can never write, add, remove or move anything there.
/// Inputs: the project's IDE folder and the server data folder inside it. Output: the
/// written files, or a plain refusal.
#[cfg(unix)]
pub fn write(folder: &Path, server_data: &Path) -> Result<Written, EngineError> {
    if !folder.is_absolute()
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
    let target = machine.join(SETTINGS);
    let mut settings = match owner_settings(&target)? {
        Owner::Kept(settings) => settings,
        Owner::Unreadable => {
            std::fs::rename(&target, machine.join(UNREADABLE)).map_err(unwritable)?;
            Map::new()
        }
        Owner::Absent => Map::new(),
    };
    let locked = locked();
    settings.extend(locked.clone());
    replace(folder, MACHINE_TEMPORARY, &target, &Value::Object(settings))?;
    let locked_path = folder.join(LOCKED_FILE);
    replace(
        folder,
        LOCKED_TEMPORARY,
        &locked_path,
        &Value::Object(locked),
    )?;
    Ok(Written {
        machine: target,
        locked: locked_path,
    })
}

/// Refuses the lock where the engine has no Unix file modes, so no IDE server starts.
/// Takes the folders, unused. Returns `ide_unavailable`.
#[cfg(not(unix))]
pub fn write(_folder: &Path, _server_data: &Path) -> Result<Written, EngineError> {
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
/// render before Remote settings load: every locked key, and CrazyAgent's themes following
/// the system appearance, which the owner may change. Output: the defaults.
pub fn defaults() -> Map<String, Value> {
    let mut all = locked();
    all.extend(table([
        (THEME_KEYS[0], json!(true)),
        (THEME_KEYS[1], json!("CrazyAgent Dark")),
        (THEME_KEYS[2], json!("CrazyAgent Light")),
    ]));
    all
}

/// The keys Remote settings apply: AI features, agents and the command center chat off, the
/// MCP gallery, terminal hint, tips, start editor, Accounts, announcements, walkthroughs,
/// remote recommendations, semantic search and telemetry. Output: the keys.
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
        ("workbench.activity.showAccounts", json!(false)),
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
        ("extensions.autoUpdate", json!(false)),
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
    Kept(Map<String, Value>),
    Unreadable,
}

/// Reads the owner's keys from the Remote settings file without following a link or waiting
/// on a special file. Input: its path. Output: absent when missing or not a regular file, the
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
        Ok(Value::Object(settings)) => Owner::Kept(settings),
        _ => Owner::Unreadable,
    })
}

/// Replaces a file whole: writes a new 0600 file in the engine's private IDE folder, syncs it
/// and renames it over the target. Inputs: the folder, the temporary name, the target and
/// the JSON. Output: Ok once renamed, or `ide_settings_unwritable`.
#[cfg(unix)]
fn replace(folder: &Path, temporary: &str, target: &Path, body: &Value) -> Result<(), EngineError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let text = serde_json::to_string_pretty(body).map_err(unwritable)?;
    let temporary = folder.join(temporary);
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(unwritable(error)),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(unwritable)?;
    file.write_all(text.as_bytes()).map_err(unwritable)?;
    file.sync_all().map_err(unwritable)?;
    std::fs::rename(&temporary, target).map_err(unwritable)
}

#[cfg(unix)]
fn unwritable<E>(_: E) -> EngineError {
    failure("ide_settings_unwritable")
}

#[cfg(all(test, unix))]
mod tests;
