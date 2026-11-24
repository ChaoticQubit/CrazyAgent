use super::{
    MANIFEST_FILE, Problem, REMOVING, RETIRED, STAGED, TREE_DEPTH_MAX, TREE_ENTRIES_MAX,
    USER_PLUGINS_MAX, UserFile, left_over, removing, removing_name, retired, retired_name,
};

use crate::Refused;
use crate::embedding::valid_name;
use crate::manifest::MANIFEST_BYTES_MAX;
use crate::store::refused;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(unix)]
#[path = "registry_folder_unix.rs"]
mod os;

#[cfg(windows)]
#[path = "registry_folder_windows.rs"]
mod os;

use os::{Dir, Kind};

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

fn open(root: &Path, create: bool) -> Result<Option<Dir>, Refused> {
    os::open_plugins(root, create)
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
/// waiting on a FIFO, at most `MANIFEST_BYTES_MAX` + 1 bytes. Inputs: the opened folder
/// and whether its plugin.toml must be owned by this user. Output: the bytes, or the
/// refusal.
fn read_dir(directory: Dir, owned: bool) -> Result<Vec<u8>, Problem> {
    use std::io::Read;
    let mut seen = 0;
    let mut extra = false;
    os::each(&directory, |name| {
        seen += 1;
        extra = name != MANIFEST_FILE || seen >= ENTRIES_MAX;
        !extra
    })
    .map_err(|()| unsafe_entry())?;
    if extra {
        return Err(Problem::new("plugin_folder_extra_entry", None, None));
    }
    assert!(seen < ENTRIES_MAX);
    let file = os::open_manifest(&directory, owned)
        .map_err(|()| unsafe_entry())?
        .ok_or_else(|| Problem::new("plugin_manifest_invalid", Some(1), None))?;
    drop(directory);
    let mut bytes = Vec::new();
    file.take(MANIFEST_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unsafe_entry())?;
    assert!(bytes.len() <= MANIFEST_BYTES_MAX + 1);
    Ok(bytes)
}

fn read_in(directory: &Dir, name: &str) -> Result<Vec<u8>, Problem> {
    let folder = os::open_folder(directory, name.as_ref(), true).map_err(|()| unsafe_entry())?;
    read_dir(folder, true)
}

/// Reads a folder the owner picked to add: not a link itself, opened without following
/// one, holding its plugin.toml only. Input: the folder. Output: the
/// plugin.toml bytes, or the refusal.
pub(super) fn read_source(source: &Path) -> Result<Vec<u8>, Problem> {
    let folder = os::open_picked(source).map_err(|()| unsafe_entry())?;
    read_dir(folder, false)
}

fn names(directory: &Dir) -> Result<(BTreeMap<String, Entry>, bool), Refused> {
    let mut names = BTreeMap::new();
    let mut more = false;
    os::each(directory, |file| {
        let Some((name, kind)) = entry_of(file.as_encoded_bytes()) else {
            return true;
        };
        let kept = names.entry(name.to_owned()).or_insert(kind);
        if kind == Entry::Folder {
            *kept = Entry::Folder;
        }
        if names.len() > USER_PLUGINS_MAX {
            names.pop_last();
            more = true;
        }
        true
    })
    .map_err(|()| unavailable())?;
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

/// Whether a plugin of this name, once written, is among the first `USER_PLUGINS_MAX`
/// by name without dropping another from them: it is listed already, or the folder
/// holds fewer entries than the cap, old plugin files included. Inputs: the engine root
/// and a valid plugin name. Output: the answer, or the folder's refusal.
pub(super) fn fits(root: &Path, name: &str) -> Result<bool, Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(true);
    };
    let (names, _) = names(&directory)?;
    Ok(names.contains_key(name) || names.len() < USER_PLUGINS_MAX)
}

fn exists(directory: &Dir, name: &str) -> Result<bool, Refused> {
    os::kind(directory, name.as_ref())
        .map(|kind| kind.is_some())
        .map_err(|()| unavailable())
}

/// One user plugin. Inputs: the engine root and a valid plugin name. Output: none when
/// it is missing, else its plugin.toml bytes or the refusal (a left-over `.yaml` file
/// is `app_plugin_format_old`); or the folder's refusal.
pub(super) fn read(root: &Path, name: &str) -> Result<Option<Result<Vec<u8>, Problem>>, Refused> {
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

/// Deletes one plugin folder and what it holds, never following a link: a link in it is
/// unlinked itself, so nothing outside the plugin is touched. Inputs: the plugins folder
/// and the entry name. Output: Ok once it is gone, or `app_plugin_folder_unavailable`
/// past `TREE_DEPTH_MAX` levels or `TREE_ENTRIES_MAX` entries in one folder.
fn remove_tree(directory: &Dir, name: &str) -> Result<(), Refused> {
    remove_level(directory, name.as_ref(), TREE_DEPTH_MAX)
}

/// Deletes one folder level by level, each folder's own handle closed before the folder
/// itself is deleted. Inputs: the folder holding it, its name and the levels left below
/// it. Output: Ok once it is gone, or `app_plugin_folder_unavailable`.
fn remove_level(directory: &Dir, name: &OsStr, depth: usize) -> Result<(), Refused> {
    assert!(depth <= TREE_DEPTH_MAX);
    let folder = os::open_folder(directory, name, false).map_err(|()| unavailable())?;
    let mut inside: Vec<OsString> = Vec::new();
    os::each(&folder, |file| {
        inside.push(file.to_os_string());
        inside.len() <= TREE_ENTRIES_MAX
    })
    .map_err(|()| unavailable())?;
    if inside.len() > TREE_ENTRIES_MAX {
        return Err(unavailable());
    }
    for file in &inside {
        match os::kind(&folder, file).map_err(|()| unavailable())? {
            Some(Kind::Folder) if depth == 0 => return Err(unavailable()),
            Some(Kind::Folder) => remove_level(&folder, file, depth - 1)?,
            Some(Kind::Other) => {
                os::unlink(&folder, file).map_err(|()| unavailable())?;
            }
            None => {}
        }
    }
    drop(folder);
    os::remove_dir(directory, name).map_err(|()| unavailable())
}

/// Writes a user plugin as a staged private folder holding a private plugin.toml, renamed
/// into place, creating the private plugins folder when missing. An older folder of that
/// name is moved aside to a retired name first and moved back when the new one cannot take
/// its place; once the new one is in place the older one stays aside, handed back as a
/// [`FolderStep`] that deletes it after the commit or puts it back after a rollback. Inputs:
/// the engine root, a valid name, the bytes and the random id source. Output: the step for
/// an older folder, none when there was none, with the folder synced, or the folder's
/// refusal with nothing staged left.
pub(super) fn write(
    root: &Path,
    name: &str,
    bytes: &[u8],
    new_ref: fn() -> Result<String, &'static str>,
) -> Result<Option<FolderStep>, Refused> {
    let directory = open(root, true)?.ok_or_else(unavailable)?;
    let id = new_ref().map_err(refused)?;
    let staged = format!("{STAGED}{id}");
    os::make_folder(&directory, staged.as_ref()).map_err(|()| unavailable())?;
    let written = (|| {
        os::write_manifest(&directory, staged.as_ref(), bytes).map_err(|()| unavailable())?;
        let retired = retired(name, &id);
        let replaced = exists(&directory, name)?;
        if replaced {
            os::rename(&directory, name.as_ref(), retired.as_ref()).map_err(|()| unavailable())?;
        }
        if os::rename(&directory, staged.as_ref(), name.as_ref()).is_err() {
            if replaced {
                let _ = os::rename(&directory, retired.as_ref(), name.as_ref());
            }
            return Err(unavailable());
        }
        Ok(replaced.then(|| FolderStep {
            root: root.to_path_buf(),
            name: name.to_owned(),
            aside: Some(retired),
            replaced: true,
            new_ref,
        }))
    })();
    if written.is_err() {
        let _ = remove_tree(&directory, &staged);
    }
    let step = written?;
    os::sync(&directory).map_err(|()| unavailable())?;
    Ok(step)
}

/// Sweeps the staged and retired entries a crash between a write and its rename left, at
/// most `USER_PLUGINS_MAX` a start, in name order. Input: the engine root. Output: Ok
/// once each is restored or deleted or the folder is missing, else the first refusal
/// after every entry was tried.
pub(super) fn remove_staged(root: &Path) -> Result<(), Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(());
    };
    let mut left: Vec<OsString> = Vec::new();
    os::each(&directory, |file| {
        if left_over(file.as_encoded_bytes()) {
            left.push(file.to_os_string());
        }
        left.len() < USER_PLUGINS_MAX
    })
    .map_err(|()| unavailable())?;
    assert!(left.len() <= USER_PLUGINS_MAX);
    left.sort();
    let mut first = None;
    for file in &left {
        if let Err(refusal) = sweep(&directory, file) {
            first.get_or_insert(refusal);
        }
    }
    os::sync(&directory).map_err(|()| unavailable())?;
    first.map_or(Ok(()), Err)
}

/// Restores or deletes one staged or retired entry: a retired folder goes back under its
/// plugin name when that name is missing; one beside a live plugin of its name first loses
/// that name, so a folder it cannot delete is never restored later; any other folder is
/// deleted, and a file or link is unlinked. Inputs: the plugins folder and the entry.
/// Output: Ok, or the refusal.
fn sweep(directory: &Dir, file: &OsStr) -> Result<(), Refused> {
    match os::kind(directory, file).map_err(|()| unavailable())? {
        None => Ok(()),
        Some(Kind::Folder) => {
            if let Some(text) = file.to_str()
                && let Some(name) = retired_name(text)
            {
                if !exists(directory, name)? {
                    return os::rename(directory, file, name.as_ref()).map_err(|()| unavailable());
                }
                let (_, id) = text
                    .rsplit_once('.')
                    .expect("a retired name ends in its id");
                let nameless = format!("{RETIRED}{id}");
                assert!(retired_name(&nameless).is_none());
                if os::rename(directory, file, nameless.as_ref()).is_ok() {
                    return remove_level(directory, nameless.as_ref(), TREE_DEPTH_MAX);
                }
            }
            remove_level(directory, file, TREE_DEPTH_MAX)
        }
        Some(Kind::Other) => os::unlink(directory, file)
            .map(|_| ())
            .map_err(|()| unavailable()),
    }
}

/// Settles each folder a removal moved aside, as the rows decide: one whose plugin still has
/// rows goes back under the plugin name when that name is missing, since its removal never
/// committed; any other first loses the plugin name, then is deleted, and one it cannot
/// delete stays nameless for the staged sweep. Inputs: the engine root and the rows check.
/// Output: Ok, or the first refusal after every entry was tried.
pub(super) fn sweep_removals(
    root: &Path,
    known: impl Fn(&str) -> Result<bool, Refused>,
) -> Result<(), Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(());
    };
    let mut left: Vec<OsString> = Vec::new();
    os::each(&directory, |file| {
        if file.as_encoded_bytes().starts_with(REMOVING.as_bytes()) {
            left.push(file.to_os_string());
        }
        left.len() < USER_PLUGINS_MAX
    })
    .map_err(|()| unavailable())?;
    assert!(left.len() <= USER_PLUGINS_MAX);
    left.sort();
    let mut first = None;
    for file in &left {
        if let Err(refusal) = settle_removal(&directory, file, &known) {
            first.get_or_insert(refusal);
        }
    }
    os::sync(&directory).map_err(|()| unavailable())?;
    first.map_or(Ok(()), Err)
}

/// Settles one folder a removal moved aside: back under its plugin name when the rows still
/// name the plugin and the name is missing, else renamed without the name and deleted. When
/// the rows do not read it stays for the next start. Inputs: the plugins folder, the entry
/// and the rows check. Output: Ok, or the refusal.
fn settle_removal(
    directory: &Dir,
    file: &OsStr,
    known: &impl Fn(&str) -> Result<bool, Refused>,
) -> Result<(), Refused> {
    match os::kind(directory, file).map_err(|()| unavailable())? {
        None => Ok(()),
        Some(Kind::Folder) => {
            if let Some(text) = file.to_str()
                && let Some(name) = removing_name(text)
            {
                if known(name)? && !exists(directory, name)? {
                    return os::rename(directory, file, name.as_ref()).map_err(|()| unavailable());
                }
                let (_, id) = text
                    .rsplit_once('.')
                    .expect("a removal name ends in its id");
                let nameless = format!("{RETIRED}{id}");
                assert!(retired_name(&nameless).is_none());
                if os::rename(directory, file, nameless.as_ref()).is_ok() {
                    return remove_level(directory, nameless.as_ref(), TREE_DEPTH_MAX);
                }
            }
            remove_level(directory, file, TREE_DEPTH_MAX)
        }
        Some(Kind::Other) => os::unlink(directory, file)
            .map(|_| ())
            .map_err(|()| unavailable()),
    }
}

/// A user plugin's folder moved aside under a removal name that still names it. Dropped
/// before it is finished, it goes back under the plugin name, as the start sweep puts it
/// back after a crash while the plugin's rows remain.
pub(super) struct Retired<'a> {
    root: &'a Path,
    name: &'a str,
    aside: Option<String>,
}

/// Moves a user plugin's folder aside under a removal name that still names it, so a
/// removal whose rows do not commit puts it back. Inputs: the engine root, a valid name and
/// the random id source. Output: the retired folder, holding none when no folder has that
/// name, or the folder's refusal with the folder left in place.
pub(super) fn retire<'a>(
    root: &'a Path,
    name: &'a str,
    new_ref: fn() -> Result<String, &'static str>,
) -> Result<Retired<'a>, Refused> {
    let mut held = Retired {
        root,
        name,
        aside: None,
    };
    let Some(directory) = open(root, false)? else {
        return Ok(held);
    };
    if os::kind(&directory, name.as_ref()).map_err(|()| unavailable())? != Some(Kind::Folder) {
        return Ok(held);
    }
    let aside = removing(name, &new_ref().map_err(refused)?);
    os::rename(&directory, name.as_ref(), aside.as_ref()).map_err(|()| unavailable())?;
    held.aside = Some(aside);
    os::sync(&directory).map_err(|()| unavailable())?;
    Ok(held)
}

impl Retired<'_> {
    /// Hands the removal's folder steps over once its rows are written: the folder moved
    /// aside, if any, and the plugin name's own entries. Input: the random id source.
    /// Output: the step, which no longer puts the folder back when dropped.
    pub(super) fn into_step(mut self, new_ref: fn() -> Result<String, &'static str>) -> FolderStep {
        FolderStep {
            root: self.root.to_path_buf(),
            name: self.name.to_owned(),
            aside: self.aside.take(),
            replaced: false,
            new_ref,
        }
    }
}

/// The folder steps of one removal of a plugin with no rows: nothing moved aside, and the
/// plugin name's own entries deleted once the change commits. Inputs: the engine root, a
/// valid name and the random id source. Output: the step.
pub(super) fn removal(
    root: &Path,
    name: &str,
    new_ref: fn() -> Result<String, &'static str>,
) -> FolderStep {
    FolderStep {
        root: root.to_path_buf(),
        name: name.to_owned(),
        aside: None,
        replaced: false,
        new_ref,
    }
}

/// A plugin folder change that waits for the end of the transaction its rows are written
/// in: a removal's folder moved aside under its removal name, or a replace's older folder
/// moved aside under its retired name. After the commit [`FolderStep::commit`] deletes
/// what went; after a rollback [`FolderStep::roll_back`] puts the folder back under its
/// name. A step never ended leaves the folder aside for the start sweeps.
#[derive(Debug)]
pub struct FolderStep {
    root: PathBuf,
    name: String,
    aside: Option<String>,
    replaced: bool,
    new_ref: fn() -> Result<String, &'static str>,
}

impl FolderStep {
    /// Deletes what the committed change let go. A removal deletes its folder moved aside,
    /// first renamed without the plugin name so one it cannot delete is never put back, then
    /// any entry left under the plugin name; a replace deletes the older folder the same
    /// way, best effort, as the start sweep finishes one it cannot. Output: Ok, or the
    /// removal folder's refusal when it keeps the plugin name.
    pub fn commit(self) -> Result<(), Refused> {
        let Self {
            root,
            name,
            aside,
            replaced,
            new_ref,
        } = self;
        assert!(valid_name(&name));
        if let Some(aside) = aside {
            let directory = open(&root, false)?.ok_or_else(unavailable)?;
            let (_, id) = aside
                .rsplit_once('.')
                .expect("an aside name ends in its id");
            let nameless = format!("{RETIRED}{id}");
            assert!(retired_name(&nameless).is_none());
            let lost = os::rename(&directory, aside.as_ref(), nameless.as_ref()).is_ok();
            let gone = remove_tree(&directory, if lost { &nameless } else { &aside });
            os::sync(&directory).map_err(|()| unavailable())?;
            if replaced {
                return Ok(());
            }
            if !lost {
                gone?;
            }
        }
        assert!(!replaced);
        remove(&root, &name, new_ref).map(drop)
    }

    /// Puts the folder moved aside back under the plugin name after the change rolled back.
    /// A replace first moves the new folder off the name, renamed without it and deleted; if
    /// that move fails both stay and the start sweep keeps the new one. Output: none; a
    /// failed step is left to the start sweeps.
    pub fn roll_back(self) {
        let Some(aside) = self.aside else {
            return;
        };
        let Ok(Some(directory)) = open(&self.root, false) else {
            return;
        };
        if self.replaced {
            let Ok(id) = (self.new_ref)() else {
                return;
            };
            let nameless = format!("{RETIRED}{id}");
            assert!(retired_name(&nameless).is_none());
            if os::rename(&directory, self.name.as_ref(), nameless.as_ref()).is_err() {
                return;
            }
            let _ = remove_tree(&directory, &nameless);
        }
        let _ = os::rename(&directory, aside.as_ref(), self.name.as_ref());
        let _ = os::sync(&directory);
    }
}

impl Drop for Retired<'_> {
    fn drop(&mut self) {
        let Some(aside) = self.aside.take() else {
            return;
        };
        if let Ok(Some(directory)) = open(self.root, false) {
            let _ = os::rename(&directory, aside.as_ref(), self.name.as_ref());
            let _ = os::sync(&directory);
        }
    }
}

/// Deletes a user plugin's folder and any `.yaml` file an older version left under its
/// name. The folder first moves to a retired name with no plugin name in it, so one it
/// cannot delete is never listed or restored again and stays for the start sweep; an
/// entry that is not a folder, such as a link, is unlinked itself without following it.
/// Inputs: the engine root, a valid name and the random id source. Output: whether
/// anything was deleted, or the folder's refusal.
pub(super) fn remove(
    root: &Path,
    name: &str,
    new_ref: fn() -> Result<String, &'static str>,
) -> Result<bool, Refused> {
    let Some(directory) = open(root, false)? else {
        return Ok(false);
    };
    let kind = os::kind(&directory, name.as_ref()).map_err(|()| unavailable())?;
    match kind {
        Some(Kind::Folder) => {
            let aside = format!("{RETIRED}{}", new_ref().map_err(refused)?);
            os::rename(&directory, name.as_ref(), aside.as_ref()).map_err(|()| unavailable())?;
            let _ = remove_tree(&directory, &aside);
        }
        Some(Kind::Other) => {
            os::unlink(&directory, name.as_ref()).map_err(|()| unavailable())?;
        }
        None => {}
    }
    let folder = kind.is_some();
    let legacy = os::unlink(&directory, format!("{name}{LEGACY_SUFFIX}").as_ref())
        .map_err(|()| unavailable())?;
    Ok(folder || legacy)
}
