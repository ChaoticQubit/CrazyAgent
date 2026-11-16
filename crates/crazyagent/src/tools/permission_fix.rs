use super::{Places, Refusal, Refused};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const TOOL_FIX_REFUSED: &str = "tool_fix_refused";
pub const TOOL_FIX_TOO_LARGE: &str = "tool_fix_too_large";
pub const TOOL_FIX_FAILED: &str = "tool_fix_failed";
pub const TOOL_FIX_NOT_NEEDED: &str = "tool_fix_not_needed";
pub const TOOL_FIX_UNAVAILABLE: &str = "tool_fix_unavailable";
pub const TOOL_FIX_CHANGED: &str = "tool_fix_changed";
pub const TOOL_FIX_OFF: &str = "tool_fix_off";
/// The most entries one fix looks at before it stops.
pub const ENTRIES_MAX: u64 = 200_000;
/// The deepest folder below the target one fix walks into.
pub const DEPTH_MAX: usize = 64;
/// The fewest folders between HOME and a prefix walked as a whole tree, so a tool in
/// `~/.local/bin` or `~/.cargo/bin` never walks all of `~/.local` or `~/.cargo`.
const PREFIX_DEPTH_MIN: usize = 3;
/// Package manager prefixes a fix never changes, even when this user owns them: their
/// manager owns how they are laid out.
const MANAGED_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];

/// What one permission fix changes (DEC-162): a prefix walked as a whole tree, or one folder
/// or file alone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub path: PathBuf,
    pub recursive: bool,
}

impl Target {
    /// The command the owner can run for the same change, the path in single quotes with
    /// any quote in it closed, escaped and reopened, so the shell reads it as one word.
    /// Output: the command.
    pub fn command(&self) -> String {
        let quoted = format!("'{}'", self.path.to_string_lossy().replace('\'', "'\\''"));
        if self.recursive {
            format!("chmod -R go-w {quoted}")
        } else {
            format!("chmod go-w {quoted}")
        }
    }
}

/// What a fix did: entries whose group or other write was removed, entries passed over
/// because another user owns them, links left as they are, other entries passed over, and
/// changed files that also have a name outside the target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub changed: u64,
    pub skipped_foreign: u64,
    pub skipped_links: u64,
    pub skipped_other: u64,
    pub shared: u64,
}

/// How far one walk may go: entries looked at and folders below the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    entries: u64,
    depth: usize,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            entries: ENTRIES_MAX,
            depth: DEPTH_MAX,
        }
    }
}

/// The target a permission fix would change for a file the resolver passed over because
/// another user or group can change a part of it: on the path the part lies on (as written
/// or with links resolved), the prefix above the file's `bin` folder as a whole tree when it
/// lies at least [`PREFIX_DEPTH_MIN`] folders below HOME and holds the part, else the part
/// alone. Inputs: the refusal and the places it was found with. Output: the target, or none
/// for another refusal or a place a fix may not change ([`allowed`]).
pub fn target(refused: &Refused, places: &Places) -> Option<Target> {
    let Refusal::OthersCanChange(part) = &refused.why else {
        return None;
    };
    let home = places.home()?;
    let real = refused.file.canonicalize().ok();
    let chain = std::iter::once(refused.file.as_path())
        .chain(real.as_deref())
        .find(|chain| chain.ancestors().any(|folder| folder == part.as_path()))?;
    let prefix = chain
        .parent()
        .filter(|bin| bin.file_name() == Some(OsStr::new("bin")))
        .and_then(Path::parent)
        .filter(|prefix| below_home(prefix, home).is_some_and(|depth| depth >= PREFIX_DEPTH_MIN));
    let target = match prefix {
        Some(prefix) if part.starts_with(prefix) => Target {
            path: prefix.to_path_buf(),
            recursive: true,
        },
        _ => Target {
            path: part.clone(),
            recursive: false,
        },
    };
    allowed(&target.path, places).then_some(target)
}

/// The form of HOME a path is written under, HOME as given or with its links resolved.
/// Inputs: the path and HOME. Output: that HOME, or none outside it.
fn anchor(path: &Path, home: &Path) -> Option<PathBuf> {
    let real = home.canonicalize().ok();
    std::iter::once(home.to_path_buf())
        .chain(real)
        .find(|home| path.starts_with(home))
}

/// How many folders below HOME a path lies ([`anchor`]). Inputs: the path and HOME. Output:
/// the count, 0 for HOME itself, or none outside it.
fn below_home(path: &Path, home: &Path) -> Option<usize> {
    let anchor = anchor(path, home)?;
    path.strip_prefix(&anchor)
        .ok()
        .map(|rest| rest.components().count())
}

/// Whether a permission fix may change a path, by the same places the resolver used: only
/// strictly below HOME, never a package manager prefix, a place agents can write (a project,
/// a `.crazyagent` folder, an agent temp folder), the engine's data folder with its setup
/// folder, or, when the places refuse temp folders, a path that leads into one.
/// Inputs: the path and the places. Output: the answer.
fn allowed(path: &Path, places: &Places) -> bool {
    let Some(home) = places.home() else {
        return false;
    };
    let real = path.canonicalize().ok();
    let forms = || std::iter::once(path).chain(real.as_deref());
    below_home(path, home).is_some_and(|depth| depth > 0)
        && !forms().any(|form| {
            MANAGED_PREFIXES
                .iter()
                .any(|prefix| form.starts_with(prefix))
                || places
                    .engine_roots
                    .iter()
                    .any(|root| form.starts_with(root))
        })
        && !places.agent_writable(path)
        && !(places.temp_refused
            && real
                .as_deref()
                .is_some_and(|real| super::leads_into_temp(path, real, &places.temps)))
}

/// Removes group and other write from a target the places allow ([`allowed`]), walking it
/// from HOME one folder at a time ([`walk`]) as this user, the only owner it changes.
/// Inputs: the target and the places. Output: the report, `tool_fix_refused` for a target
/// or a folder on the way that may not change, `tool_fix_too_large` past the bounds, or
/// `tool_fix_failed`.
pub fn fix(target: &Target, places: &Places) -> Result<Report, &'static str> {
    if !allowed(&target.path, places) {
        return Err(TOOL_FIX_REFUSED);
    }
    let home = places.home().ok_or(TOOL_FIX_REFUSED)?;
    let anchor = anchor(&target.path, home).ok_or(TOOL_FIX_REFUSED)?;
    let me = current_uid();
    walk(
        &anchor,
        &target.path,
        target.recursive,
        Bounds::default(),
        &|uid, _| uid == me,
    )
}

/// Removes group and other write from a tree inside the engine's own setup folder after
/// setup installs into it, since uv writes files under the process umask (DEC-162 R2).
/// Inputs: the setup folder and the tree. Output: the report, or the code as for [`fix`].
#[cfg(unix)]
pub(crate) fn tighten_setup(setup: &Path, tree: &Path) -> Result<Report, &'static str> {
    if tree == setup || !tree.starts_with(setup) {
        return Err(TOOL_FIX_REFUSED);
    }
    let me = current_uid();
    walk(setup, tree, true, Bounds::default(), &|uid, _| uid == me)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

#[cfg(not(unix))]
fn walk(
    _anchor: &Path,
    _target: &Path,
    _recursive: bool,
    _bounds: Bounds,
    _owned: &dyn Fn(u32, u64) -> bool,
) -> Result<Report, &'static str> {
    Err(TOOL_FIX_REFUSED)
}

/// Walks a target below an anchor folder through folder handles, never reopening a path:
/// every folder from the anchor down to the target must be a real folder `owned` accepts,
/// then the target alone, or with `recursive` the target and everything below it, loses
/// group and other write ([`Walk`]). Inputs: the anchor, the target, whether it is walked
/// whole, the bounds and who counts as the owner (uid and inode). Output: the report, or
/// `tool_fix_refused`, `tool_fix_too_large` or `tool_fix_failed`.
#[cfg(unix)]
fn walk(
    anchor: &Path,
    target: &Path,
    recursive: bool,
    bounds: Bounds,
    owned: &dyn Fn(u32, u64) -> bool,
) -> Result<Report, &'static str> {
    use rustix::fs::{AtFlags, FileType};
    use std::path::Component;
    let rest = target.strip_prefix(anchor).map_err(|_| TOOL_FIX_REFUSED)?;
    let names: Vec<&OsStr> = rest
        .components()
        .map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<_>>()
        .ok_or(TOOL_FIX_REFUSED)?;
    let (last, parents) = names.split_last().ok_or(TOOL_FIX_REFUSED)?;
    let mut folder = rustix::fs::open(anchor, walker::FOLDER, rustix::fs::Mode::empty())
        .map_err(|_| TOOL_FIX_REFUSED)?;
    let (kind, uid, inode) =
        walker::described(&rustix::fs::fstat(&folder).map_err(|_| TOOL_FIX_FAILED)?);
    if kind != FileType::Directory || !owned(uid, inode) {
        return Err(TOOL_FIX_REFUSED);
    }
    for name in parents {
        folder = walker::child_folder(&folder, name, owned)?;
    }
    let stat = rustix::fs::statat(&folder, *last, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| TOOL_FIX_REFUSED)?;
    let (kind, uid, inode) = walker::described(&stat);
    if !owned(uid, inode) {
        return Err(TOOL_FIX_REFUSED);
    }
    let mut walk = walker::Walk {
        report: Report::default(),
        seen: 0,
        bounds,
        owned,
    };
    match kind {
        FileType::Directory => {
            let handle =
                rustix::fs::openat(&folder, *last, walker::FOLDER, rustix::fs::Mode::empty())
                    .map_err(|_| TOOL_FIX_REFUSED)?;
            if !walk.tighten(&handle, kind)? {
                return Err(TOOL_FIX_REFUSED);
            }
            if recursive {
                walk.folder(&handle, 1)?;
            }
        }
        FileType::RegularFile if !recursive => {
            let handle =
                rustix::fs::openat(&folder, *last, walker::FILE, rustix::fs::Mode::empty())
                    .map_err(|_| TOOL_FIX_REFUSED)?;
            if !walk.tighten(&handle, kind)? {
                return Err(TOOL_FIX_REFUSED);
            }
        }
        _ => return Err(TOOL_FIX_REFUSED),
    }
    let report = walk.report;
    assert!(report.changed + report.skipped_other <= walk.seen + 1);
    Ok(report)
}

#[cfg(unix)]
mod walker {
    use super::{Bounds, Report, TOOL_FIX_FAILED, TOOL_FIX_REFUSED, TOOL_FIX_TOO_LARGE};
    use rustix::fd::OwnedFd;
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
    use std::ffi::{CStr, OsStr};

    pub(super) const FOLDER: OFlags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);
    pub(super) const FILE: OFlags = OFlags::RDONLY
        .union(OFlags::NOFOLLOW)
        .union(OFlags::NONBLOCK)
        .union(OFlags::CLOEXEC);
    const OTHERS_WRITE: Mode = Mode::WGRP.union(Mode::WOTH);

    /// The kind, owner and inode of an entry. Input: its status. Output: the three.
    pub(super) fn described(stat: &Stat) -> (FileType, u32, u64) {
        (
            FileType::from_raw_mode(stat.st_mode),
            stat.st_uid,
            stat.st_ino,
        )
    }

    /// Opens one folder below another on the way to a target: it must be a real folder
    /// `owned` accepts, before it opens and again on the handle. Inputs: the parent handle,
    /// the name and who counts as the owner. Output: the handle, or `tool_fix_refused`.
    pub(super) fn child_folder(
        parent: &OwnedFd,
        name: &OsStr,
        owned: &dyn Fn(u32, u64) -> bool,
    ) -> Result<OwnedFd, &'static str> {
        let stat = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| TOOL_FIX_REFUSED)?;
        let (kind, uid, inode) = described(&stat);
        if kind != FileType::Directory || !owned(uid, inode) {
            return Err(TOOL_FIX_REFUSED);
        }
        let handle = rustix::fs::openat(parent, name, FOLDER, Mode::empty())
            .map_err(|_| TOOL_FIX_REFUSED)?;
        let again = rustix::fs::fstat(&handle).map_err(|_| TOOL_FIX_REFUSED)?;
        if described(&again) != (kind, uid, inode) || again.st_dev != stat.st_dev {
            return Err(TOOL_FIX_REFUSED);
        }
        Ok(handle)
    }

    /// One walk in progress: what it did so far, how many entries it looked at, its bounds
    /// and who counts as the owner.
    pub(super) struct Walk<'owned> {
        pub(super) report: Report,
        pub(super) seen: u64,
        pub(super) bounds: Bounds,
        pub(super) owned: &'owned dyn Fn(u32, u64) -> bool,
    }

    impl Walk<'_> {
        /// Tightens every entry of an open folder and walks into its folders, up to the
        /// bounds. Inputs: the folder handle and its depth below the target. Output: Ok, or
        /// `tool_fix_too_large` or `tool_fix_failed`.
        pub(super) fn folder(
            &mut self,
            folder: &OwnedFd,
            depth: usize,
        ) -> Result<(), &'static str> {
            if depth > self.bounds.depth {
                return Err(TOOL_FIX_TOO_LARGE);
            }
            let entries = rustix::fs::Dir::read_from(folder).map_err(|_| TOOL_FIX_FAILED)?;
            for entry in entries {
                let entry = entry.map_err(|_| TOOL_FIX_FAILED)?;
                let name = entry.file_name();
                if matches!(name.to_bytes(), b"." | b"..") {
                    continue;
                }
                self.seen += 1;
                if self.seen > self.bounds.entries {
                    return Err(TOOL_FIX_TOO_LARGE);
                }
                self.entry(folder, name, depth)?;
            }
            Ok(())
        }

        /// Tightens one entry of a folder as its status reads, never following a link: a link
        /// is left, an entry another user owns is left and never walked into, a folder is
        /// tightened and walked, a regular file others can write is tightened, anything else
        /// is left. Inputs: the folder handle, the entry name and the folder's depth. Output:
        /// Ok, or the walk's code.
        fn entry(
            &mut self,
            folder: &OwnedFd,
            name: &CStr,
            depth: usize,
        ) -> Result<(), &'static str> {
            let Ok(stat) = rustix::fs::statat(folder, name, AtFlags::SYMLINK_NOFOLLOW) else {
                self.report.skipped_other += 1;
                return Ok(());
            };
            let (kind, uid, inode) = described(&stat);
            if kind == FileType::Symlink {
                self.report.skipped_links += 1;
                return Ok(());
            }
            if !(self.owned)(uid, inode) {
                self.report.skipped_foreign += 1;
                return Ok(());
            }
            match kind {
                FileType::Directory => {
                    match rustix::fs::openat(folder, name, FOLDER, Mode::empty()) {
                        Ok(handle) => {
                            if self.tighten(&handle, kind)? {
                                self.folder(&handle, depth + 1)?;
                            }
                            Ok(())
                        }
                        Err(_) => {
                            self.report.skipped_other += 1;
                            Ok(())
                        }
                    }
                }
                FileType::RegularFile => {
                    if !Mode::from_raw_mode(stat.st_mode).intersects(OTHERS_WRITE) {
                        return Ok(());
                    }
                    match rustix::fs::openat(folder, name, FILE, Mode::empty()) {
                        Ok(handle) => self.tighten(&handle, kind).map(drop),
                        Err(_) => {
                            self.report.skipped_other += 1;
                            Ok(())
                        }
                    }
                }
                _ => {
                    self.report.skipped_other += 1;
                    Ok(())
                }
            }
        }

        /// Removes group and other write from an open entry after reading it again on the
        /// handle, so an entry swapped since its status was read is left. Inputs: the handle
        /// and the kind it was opened as. Output: whether the entry is still this kind and
        /// owned (so a folder may be walked), or `tool_fix_failed`.
        pub(super) fn tighten(
            &mut self,
            handle: &OwnedFd,
            kind: FileType,
        ) -> Result<bool, &'static str> {
            let stat = rustix::fs::fstat(handle).map_err(|_| TOOL_FIX_FAILED)?;
            let (now, uid, inode) = described(&stat);
            if now != kind || !(self.owned)(uid, inode) {
                self.report.skipped_other += 1;
                return Ok(false);
            }
            let mut mode = Mode::from_raw_mode(stat.st_mode);
            if !mode.intersects(OTHERS_WRITE) {
                return Ok(true);
            }
            mode.remove(OTHERS_WRITE);
            rustix::fs::fchmod(handle, mode).map_err(|_| TOOL_FIX_FAILED)?;
            self.report.changed += 1;
            if kind == FileType::RegularFile && stat.st_nlink > 1 {
                self.report.shared += 1;
            }
            Ok(true)
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
