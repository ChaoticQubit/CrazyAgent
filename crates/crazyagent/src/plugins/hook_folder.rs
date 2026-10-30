use std::ffi::OsStr;
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path};

pub const FILES_MAX: usize = 2048;
pub const BYTES_MAX: u64 = 16 * 1024 * 1024;
pub const LAUNCH_FILES_MAX: usize = 4096;
pub const LAUNCH_BYTES_MAX: u64 = 64 * 1024 * 1024;
pub const DEPTH_MAX: usize = 8;
pub const PLUGIN_NAME_BYTES_MAX: usize = 32;
pub const MATCHER_BYTES_MAX: usize = 256;
pub const TIMEOUT_MAX: u32 = 600;
const ENTRIES_MAX: usize = 4096;
const MANIFEST_BYTES_MAX: u64 = 64 * 1024;
const HOOKS_FILE_BYTES_MAX: u64 = 256 * 1024;
const SKIPPED: [&str; 2] = [".git", ".DS_Store"];
const ALWAYS: [&str; 2] = ["hooks", "skills"];

/// The Claude Code hook events a plugin hook may run on. A hook on any other event is left
/// out when its plugin is read.
pub const EVENTS: [&str; 12] = [
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "UserPromptSubmit",
    "SessionStart",
    "SessionEnd",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PermissionRequest",
];

/// One command hook a plugin defines: the event and optional matcher it runs on, its shell
/// command as the plugin wrote it and its timeout in seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSpec {
    pub event: String,
    pub matcher: Option<String>,
    pub command: String,
    pub timeout: Option<u32>,
}

/// What reading a plugin folder found: the plugin's name, its command hooks in file order
/// and how many hooks were left out for an unknown event, a non-command type or a bad value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub plugin: String,
    pub hooks: Vec<HookSpec>,
    pub skipped: usize,
}

/// Files and bytes one copy has made so far.
#[derive(Debug, Default)]
pub struct Tally {
    pub files: usize,
    pub bytes: u64,
}

/// Checks a plugin name used in hook names and folder names.
/// Input: the name. Output: true for 1 to 32 ASCII letters, digits, `.`, `-` or `_` not
/// starting with `.`.
pub fn valid_plugin_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= PLUGIN_NAME_BYTES_MAX
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
}

/// Reads the command hooks a Claude Code plugin folder defines, running nothing: the
/// `hooks/hooks.json` file and the `hooks` entry of `.claude-plugin/plugin.json`, an object
/// or the relative path of a hooks file inside the folder, or a list of those. The plugin's
/// name comes from `plugin.json`, else the folder's own name, and is checked only once hooks
/// were found, so a folder with none reports `hooks_none_found` whatever it is called.
/// Input: the absolute plugin folder. Output: what was found, or `hooks_none_found`,
/// `hooks_invalid`, `hooks_too_many`, `hook_plugin_name_invalid` or `skill_folder_not_found`.
pub fn read(folder: &Path) -> Result<Found, &'static str> {
    if !folder.is_absolute()
        || folder.as_os_str().len() > super::skill_folder::SOURCE_BYTES_MAX
        || !fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err("skill_folder_not_found");
    }
    let manifest = read_small(
        &folder.join(".claude-plugin").join("plugin.json"),
        MANIFEST_BYTES_MAX,
    )?
    .map(|text| serde_json::from_str::<serde_json::Value>(&text).map_err(|_| "hooks_invalid"))
    .transpose()?;
    let mut configs = Vec::new();
    if let Some(text) = read_small(
        &folder.join("hooks").join("hooks.json"),
        HOOKS_FILE_BYTES_MAX,
    )? {
        configs
            .push(serde_json::from_str::<serde_json::Value>(&text).map_err(|_| "hooks_invalid")?);
    }
    let listed = manifest.as_ref().and_then(|manifest| manifest.get("hooks"));
    let entries: Vec<&serde_json::Value> = match listed {
        Some(serde_json::Value::Array(items)) => items.iter().collect(),
        Some(item) => vec![item],
        None => Vec::new(),
    };
    for entry in entries {
        match entry {
            serde_json::Value::Object(_) => configs.push(entry.clone()),
            serde_json::Value::String(path) => {
                let relative = Path::new(path.strip_prefix("./").unwrap_or(path));
                if relative == Path::new("hooks/hooks.json") {
                    continue;
                }
                if !inside(relative) {
                    return Err("hooks_invalid");
                }
                let text = read_small(&folder.join(relative), HOOKS_FILE_BYTES_MAX)?
                    .ok_or("hooks_invalid")?;
                configs.push(
                    serde_json::from_str::<serde_json::Value>(&text)
                        .map_err(|_| "hooks_invalid")?,
                );
            }
            _ => return Err("hooks_invalid"),
        }
    }
    let mut found = Found {
        plugin: String::new(),
        hooks: Vec::new(),
        skipped: 0,
    };
    for config in &configs {
        collect(config, &mut found)?;
    }
    if found.hooks.is_empty() {
        return Err("hooks_none_found");
    }
    found.plugin = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_owned))
        .or_else(|| folder.file_name()?.to_str().map(str::to_owned))
        .filter(|name| valid_plugin_name(name))
        .ok_or("hook_plugin_name_invalid")?;
    assert!(found.hooks.len() <= super::HOOKS_PER_PLUGIN_MAX);
    Ok(found)
}

/// Copies the parts of a plugin folder its hooks run from into a new folder of the plugin
/// store: its `hooks` and `skills` folders and each other top-level entry a hook command
/// names, never following a link and leaving out `.git` and `.DS_Store`. Files keep their
/// owner execute bit. Inputs: the plugin folder, a destination that must not exist yet and
/// the hooks read from the folder. Output: unit, or `hooks_too_large` or
/// `plugin_store_unavailable` after removing any partial copy.
pub fn copy(source: &Path, destination: &Path, hooks: &[HookSpec]) -> Result<(), &'static str> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err("plugin_store_unavailable");
    }
    folder(destination)?;
    let mut tally = Tally::default();
    let result = (|| {
        let mut entries = 0;
        for entry in fs::read_dir(source).map_err(|_| "skill_folder_not_found")? {
            entries += 1;
            if entries > ENTRIES_MAX {
                return Err("hooks_too_large");
            }
            let entry = entry.map_err(|_| "skill_folder_not_found")?;
            let name = entry.file_name();
            if !wanted(&name, hooks) {
                continue;
            }
            copy_entry(
                &entry.path(),
                &destination.join(&name),
                1,
                &mut tally,
                (FILES_MAX, BYTES_MAX),
            )?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    assert!(tally.files <= FILES_MAX && tally.bytes <= BYTES_MAX);
    result
}

/// Copies one stored plugin folder whole into an agent launch, under the launch's shared
/// bounds. Inputs: the stored folder, the destination, which must not exist yet, and the
/// launch's tally. Output: unit, or `hooks_too_large` or `plugin_store_unavailable`.
pub fn copy_stored(
    source: &Path,
    destination: &Path,
    tally: &mut Tally,
) -> Result<(), &'static str> {
    if !fs::symlink_metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        return Err("plugin_store_unavailable");
    }
    copy_entry(
        source,
        destination,
        0,
        tally,
        (LAUNCH_FILES_MAX, LAUNCH_BYTES_MAX),
    )?;
    assert!(tally.files <= LAUNCH_FILES_MAX && tally.bytes <= LAUNCH_BYTES_MAX);
    Ok(())
}

fn collect(config: &serde_json::Value, found: &mut Found) -> Result<(), &'static str> {
    let events = match config.get("hooks") {
        Some(inner) if inner.is_object() => inner,
        _ => config,
    };
    let events = events.as_object().ok_or("hooks_invalid")?;
    for (event, groups) in events {
        let groups = match groups.as_array() {
            Some(groups) => groups,
            None if event == "description" => continue,
            None => return Err("hooks_invalid"),
        };
        for group in groups {
            let matcher = match group.get("matcher") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(text)) if text.is_empty() => None,
                Some(serde_json::Value::String(text)) => Some(text.clone()),
                Some(_) => return Err("hooks_invalid"),
            };
            let commands = group
                .get("hooks")
                .and_then(serde_json::Value::as_array)
                .ok_or("hooks_invalid")?;
            for hook in commands {
                let command = hook
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .filter(|_| {
                        hook.get("type").and_then(serde_json::Value::as_str) == Some("command")
                    });
                let timeout = match hook.get("timeout") {
                    None => Some(None),
                    Some(value) => value
                        .as_u64()
                        .filter(|seconds| *seconds >= 1)
                        .map(|seconds| Some(seconds.min(u64::from(TIMEOUT_MAX)) as u32)),
                };
                let usable = EVENTS.contains(&event.as_str())
                    && matcher.as_deref().is_none_or(|matcher| {
                        matcher.len() <= MATCHER_BYTES_MAX && !matcher.chars().any(char::is_control)
                    });
                match (command, timeout) {
                    (Some(command), Some(timeout))
                        if usable
                            && !command.trim().is_empty()
                            && command.len() <= super::COMMAND_LINE_BYTES_MAX
                            && !command.chars().any(char::is_control) =>
                    {
                        if found.hooks.len() == super::HOOKS_PER_PLUGIN_MAX {
                            return Err("hooks_too_many");
                        }
                        found.hooks.push(HookSpec {
                            event: event.clone(),
                            matcher: matcher.clone(),
                            command: command.to_owned(),
                            timeout,
                        });
                    }
                    _ => found.skipped += 1,
                }
            }
        }
    }
    Ok(())
}

fn inside(relative: &Path) -> bool {
    !relative.as_os_str().is_empty()
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn wanted(name: &OsStr, hooks: &[HookSpec]) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if SKIPPED.contains(&name) || name.starts_with('.') {
        return false;
    }
    if ALWAYS.contains(&name) {
        return true;
    }
    hooks.iter().any(|hook| named(&hook.command, name))
}

fn named(command: &str, name: &str) -> bool {
    let after = *b"/\"' ;)";
    command.match_indices(name).any(|(at, _)| {
        let before = at.checked_sub(1).map(|index| command.as_bytes()[index]);
        let next = command.as_bytes().get(at + name.len()).copied();
        matches!(before, Some(b'/' | b'\'' | b'"')) && next.is_none_or(|byte| after.contains(&byte))
    })
}

fn copy_entry(
    source: &Path,
    target: &Path,
    depth: usize,
    tally: &mut Tally,
    (files_max, bytes_max): (usize, u64),
) -> Result<(), &'static str> {
    if depth > DEPTH_MAX {
        return Err("hooks_too_large");
    }
    let metadata = fs::symlink_metadata(source).map_err(|_| "plugin_store_unavailable")?;
    if metadata.is_dir() {
        folder(target)?;
        let mut entries = 0;
        for entry in fs::read_dir(source).map_err(|_| "plugin_store_unavailable")? {
            entries += 1;
            if entries > ENTRIES_MAX {
                return Err("hooks_too_large");
            }
            let entry = entry.map_err(|_| "plugin_store_unavailable")?;
            let name = entry.file_name();
            if name.to_str().is_some_and(|name| SKIPPED.contains(&name)) {
                continue;
            }
            copy_entry(
                &entry.path(),
                &target.join(name),
                depth + 1,
                tally,
                (files_max, bytes_max),
            )?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Ok(());
    }
    tally.files += 1;
    tally.bytes = tally.bytes.saturating_add(metadata.len());
    if tally.files > files_max || tally.bytes > bytes_max {
        return Err("hooks_too_large");
    }
    copy_file(source, target, metadata.len())
}

#[cfg(unix)]
fn folder(path: &Path) -> Result<(), &'static str> {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
        .map_err(|_| "plugin_store_unavailable")
}

#[cfg(not(unix))]
fn folder(path: &Path) -> Result<(), &'static str> {
    fs::create_dir(path).map_err(|_| "plugin_store_unavailable")
}

#[cfg(unix)]
fn copy_file(source: &Path, target: &Path, bytes: u64) -> Result<(), &'static str> {
    let unavailable = |_| "plugin_store_unavailable";
    let from = open_plain(source).map_err(unavailable)?;
    let opened = from.metadata().map_err(unavailable)?;
    if !opened.is_file() || opened.len() != bytes {
        return Err("plugin_store_unavailable");
    }
    let mode = if opened.permissions().mode() & 0o111 != 0 {
        0o700
    } else {
        0o600
    };
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(target)
        .map_err(unavailable)?;
    let copied = std::io::copy(&mut from.take(bytes), &mut to).map_err(unavailable)?;
    if copied != bytes {
        return Err("plugin_store_unavailable");
    }
    Ok(())
}

#[cfg(not(unix))]
fn copy_file(source: &Path, target: &Path, bytes: u64) -> Result<(), &'static str> {
    let copied = fs::copy(source, target).map_err(|_| "plugin_store_unavailable")?;
    if copied != bytes {
        return Err("plugin_store_unavailable");
    }
    Ok(())
}

#[cfg(unix)]
fn open_plain(path: &Path) -> std::io::Result<fs::File> {
    rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map(fs::File::from)
    .map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn open_plain(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

fn read_small(path: &Path, max: u64) -> Result<Option<String>, &'static str> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(None);
    }
    let file = open_plain(path).map_err(|_| "hooks_invalid")?;
    let metadata = file.metadata().map_err(|_| "hooks_invalid")?;
    if !metadata.is_file() || metadata.len() > max {
        return Err("hooks_invalid");
    }
    let mut text = String::new();
    file.take(max + 1)
        .read_to_string(&mut text)
        .map_err(|_| "hooks_invalid")?;
    if text.len() as u64 > max {
        return Err("hooks_invalid");
    }
    Ok(Some(text))
}
