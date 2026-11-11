use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
#[cfg(not(unix))]
use std::path::PathBuf;
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

/// A folder a copy is reading: on Unix held open, so a name swapped for a link after the
/// folder was opened cannot send the walk somewhere else.
#[cfg(unix)]
type Folder = std::os::fd::OwnedFd;
#[cfg(not(unix))]
type Folder = PathBuf;

/// One entry of a folder a copy is reading. On Unix it is opened without following a link.
enum Opened {
    Folder(Folder),
    File(fs::File),
    Other,
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
/// name comes only from `plugin.json`, never the folder's own name, and is checked only once
/// hooks were found, so a folder with none reports `hooks_none_found` whatever it holds.
/// Input: the absolute plugin folder. Output: what was found, or `hooks_none_found`,
/// `hooks_invalid`, `hooks_too_many`, `hooks_plugin_manifest_missing`,
/// `hook_plugin_name_invalid` or `skill_folder_not_found`.
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
    let manifest = manifest.ok_or("hooks_plugin_manifest_missing")?;
    found.plugin = manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| valid_plugin_name(name))
        .ok_or("hook_plugin_name_invalid")?
        .to_owned();
    assert!(found.hooks.len() <= super::HOOKS_PER_PLUGIN_MAX);
    Ok(found)
}

/// Copies the parts of a plugin folder its hooks run from into a new folder of the plugin
/// store: its `hooks` and `skills` folders and each other top-level entry a hook command
/// names, leaving out `.git` and `.DS_Store`. On Unix no link is followed, at any part of
/// the folder's path or inside it. Files keep their owner execute bit. Inputs: the plugin
/// folder's canonical path, a destination that must not exist yet and the hooks read from
/// the folder. Output: unit, or `skill_folder_not_found`, `hooks_too_large` or
/// `plugin_store_unavailable` after removing any partial copy.
pub fn copy(source: &Path, destination: &Path, hooks: &[HookSpec]) -> Result<(), &'static str> {
    if fs::symlink_metadata(destination).is_ok() {
        return Err("plugin_store_unavailable");
    }
    folder(destination)?;
    let mut tally = Tally::default();
    let result = (|| {
        let root = open_root(source).map_err(|_| "skill_folder_not_found")?;
        for name in names(&root, "skill_folder_not_found")? {
            if wanted(&name, hooks) {
                copy_child(
                    &root,
                    &name,
                    &destination.join(&name),
                    1,
                    &mut tally,
                    (FILES_MAX, BYTES_MAX),
                )?;
            }
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
    let root = open_folder(source).map_err(|_| "plugin_store_unavailable")?;
    folder(destination)?;
    copy_children(
        &root,
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

/// Copies each entry of an open folder but `.git` and `.DS_Store`. Inputs: the folder, its
/// copy, its depth, the tally and the bounds. Output: unit, or a plain rejection code.
fn copy_children(
    from: &Folder,
    target: &Path,
    depth: usize,
    tally: &mut Tally,
    bounds: (usize, u64),
) -> Result<(), &'static str> {
    for name in names(from, "plugin_store_unavailable")? {
        if name.to_str().is_some_and(|name| SKIPPED.contains(&name)) {
            continue;
        }
        copy_child(from, &name, &target.join(&name), depth + 1, tally, bounds)?;
    }
    Ok(())
}

/// Copies one entry of an open folder, leaving out a link or anything else that is neither
/// a file nor a folder. Inputs: the folder, the entry's name, its copy, its depth, the
/// tally and the bounds. Output: unit, or a plain rejection code.
fn copy_child(
    parent: &Folder,
    name: &OsStr,
    target: &Path,
    depth: usize,
    tally: &mut Tally,
    (files_max, bytes_max): (usize, u64),
) -> Result<(), &'static str> {
    if depth > DEPTH_MAX {
        return Err("hooks_too_large");
    }
    match open_entry(parent, name).map_err(|_| "plugin_store_unavailable")? {
        Opened::Folder(inner) => {
            folder(target)?;
            copy_children(&inner, target, depth, tally, (files_max, bytes_max))
        }
        Opened::File(from) => {
            let length = from
                .metadata()
                .map_err(|_| "plugin_store_unavailable")?
                .len();
            let bytes = tally.bytes.saturating_add(length);
            if tally.files >= files_max || bytes > bytes_max {
                return Err("hooks_too_large");
            }
            tally.files += 1;
            tally.bytes = bytes;
            copy_file(from, target, length)
        }
        Opened::Other => Ok(()),
    }
}

/// Opens a plugin folder one part at a time from the filesystem root, through no link at any
/// part, so a parent swapped for a link after the folder was read cannot redirect the copy.
/// Input: the canonical path. Output: the folder.
#[cfg(unix)]
fn open_root(path: &Path) -> std::io::Result<Folder> {
    Ok(super::skill_folder::open_real(
        path,
        super::skill_folder::FOLDER,
    )?)
}

#[cfg(not(unix))]
fn open_root(path: &Path) -> std::io::Result<Folder> {
    open_folder(path)
}

/// Opens a folder a copy starts from, refusing a link at its last part. Input: the path.
/// Output: the folder.
#[cfg(unix)]
fn open_folder(path: &Path) -> std::io::Result<Folder> {
    Ok(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )?)
}

#[cfg(not(unix))]
fn open_folder(path: &Path) -> std::io::Result<Folder> {
    if fs::symlink_metadata(path)?.is_dir() {
        Ok(path.to_path_buf())
    } else {
        Err(std::io::ErrorKind::InvalidInput.into())
    }
}

/// Lists an open folder, at most `ENTRIES_MAX` names. Inputs: the folder and the code for a
/// failed read. Output: its names, or that code or `hooks_too_large`.
#[cfg(unix)]
fn names(folder: &Folder, unavailable: &'static str) -> Result<Vec<OsString>, &'static str> {
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(folder).map_err(|_| unavailable)? {
        let entry = entry.map_err(|_| unavailable)?;
        if matches!(entry.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        if names.len() == ENTRIES_MAX {
            return Err("hooks_too_large");
        }
        names.push(OsStr::from_bytes(entry.file_name().to_bytes()).to_owned());
    }
    Ok(names)
}

#[cfg(not(unix))]
fn names(folder: &Folder, unavailable: &'static str) -> Result<Vec<OsString>, &'static str> {
    let mut names = Vec::new();
    for entry in fs::read_dir(folder).map_err(|_| unavailable)? {
        let entry = entry.map_err(|_| unavailable)?;
        if names.len() == ENTRIES_MAX {
            return Err("hooks_too_large");
        }
        names.push(entry.file_name());
    }
    Ok(names)
}

/// Opens one entry through its open parent without following a link, so an entry swapped
/// after its check is refused. Inputs: the parent and the name. Output: what it is.
#[cfg(unix)]
fn open_entry(parent: &Folder, name: &OsStr) -> std::io::Result<Opened> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match FileType::from_raw_mode(statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode) {
        FileType::Directory => Ok(Opened::Folder(openat(
            parent,
            name,
            flags | OFlags::DIRECTORY,
            Mode::empty(),
        )?)),
        FileType::RegularFile => {
            let file = fs::File::from(openat(
                parent,
                name,
                flags | OFlags::NONBLOCK,
                Mode::empty(),
            )?);
            if file.metadata()?.is_file() {
                Ok(Opened::File(file))
            } else {
                Err(std::io::ErrorKind::InvalidInput.into())
            }
        }
        _ => Ok(Opened::Other),
    }
}

#[cfg(not(unix))]
fn open_entry(parent: &Folder, name: &OsStr) -> std::io::Result<Opened> {
    let path = parent.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.is_dir() {
        Ok(Opened::Folder(path))
    } else if metadata.is_file() {
        Ok(Opened::File(open_plain(&path)?))
    } else {
        Ok(Opened::Other)
    }
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

/// Copies an open file's first `bytes` bytes to a new file, on Unix keeping its owner
/// execute bit. Inputs: the file, the new path and the length. Output: unit or
/// `plugin_store_unavailable`.
fn copy_file(from: fs::File, target: &Path, bytes: u64) -> Result<(), &'static str> {
    let unavailable = |_| "plugin_store_unavailable";
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        let runs = from.metadata().map_err(unavailable)?.permissions().mode() & 0o111 != 0;
        options
            .mode(if runs { 0o700 } else { 0o600 })
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let mut to = options.open(target).map_err(unavailable)?;
    let copied = std::io::copy(&mut from.take(bytes), &mut to).map_err(unavailable)?;
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

#[cfg(all(test, unix))]
mod tests;
