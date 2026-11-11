use super::platform::{Stamp, copy_link, permission_bits, private_folder, stamp, writable_folder};
use super::{AddMode, holds_owner_home, owner_home, path_text, real_folder, refused};
use crate::engine::EngineError;
use std::collections::HashMap;
use std::fs;
use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How many entries and folder levels a copy, its check or a size read goes through
/// before it stops with `home_copy_too_large`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyBounds {
    pub entries: u64,
    pub depth: usize,
}

pub const COPY_BOUNDS: CopyBounds = CopyBounds {
    entries: 4_000_000,
    depth: 256,
};

pub const FORCED_COPY_BOUNDS: CopyBounds = CopyBounds {
    entries: 16_000_000,
    depth: 512,
};

const ENTRIES_MAX: u64 = COPY_BOUNDS.entries;
const DEPTH_MAX: usize = COPY_BOUNDS.depth;
const CHUNK_BYTES: usize = 64 * 1024;
/// How long before a move's check a source entry may have been modified for its stamp to
/// count as unproven: longer than a coarse clock tick (up to 10 ms on Linux) and than the
/// 2 second modify times of FAT volumes, within which a later same-size write keeps the
/// stamp (DEC-137 G2).
const STAMP_SETTLE: Duration = Duration::from_secs(4);
const DEADLINE_EVERY: u64 = 1024;
const GITDIR_FILE_BYTES_MAX: u64 = 4096;

/// A checked plan to bring one folder into the projects home.
/// Built only by `plan_add`, which refuses unsafe sources and taken names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddPlan {
    source: PathBuf,
    destination: PathBuf,
    mode: AddMode,
    bounds: CopyBounds,
}

impl AddPlan {
    pub(super) fn new(
        source: PathBuf,
        destination: PathBuf,
        mode: AddMode,
        bounds: CopyBounds,
    ) -> Self {
        assert!(source.is_absolute() && destination.is_absolute());
        assert_ne!(source, destination);
        assert!(bounds == COPY_BOUNDS || bounds == FORCED_COPY_BOUNDS);
        Self {
            source,
            destination,
            mode,
            bounds,
        }
    }

    /// The real folder being added.
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Where the folder lands in the projects home.
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    /// Moves or copies the folder into the home. A move is a rename; across
    /// disks, and out of iCloud Drive, whose folders a launchd agent cannot
    /// rename out (EDEADLK), it copies, checks the copy and only then deletes
    /// the source. Output: Ok once the folder is complete at its destination,
    /// or a code (`home_icloud_not_downloaded` for a file still only in
    /// iCloud, `home_permission_denied` when macOS or permissions refuse);
    /// on a failure before the folder is complete the source is left as it was and
    /// no partial folder shows; `home_move_source_left` means the folder landed but
    /// part of the source could not be deleted.
    pub fn run(self) -> Result<(), EngineError> {
        match self.place()? {
            Placed::Whole => Ok(()),
            Placed::Copied(old) => old.clear(),
        }
    }

    /// Puts the folder at its destination and deletes nothing: a rename for a move on
    /// one disk, else a checked copy that leaves the whole source in place for the
    /// caller to clear. Output: whether a source is left to clear, or the refusal code
    /// with the source as it was and no partial folder showing.
    pub(super) fn place(self) -> Result<Placed, EngineError> {
        if fs::symlink_metadata(&self.destination).is_ok() {
            return Err(refused("home_destination_exists"));
        }
        match self.mode {
            AddMode::Move => match fs::rename(&self.source, &self.destination) {
                Ok(()) => Ok(Placed::Whole),
                Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => self.place_by_copy(),
                Err(e) if e.kind() == std::io::ErrorKind::Deadlock => {
                    eprintln!("home_move_by_copy cause=edeadlk");
                    self.place_by_copy()
                }
                Err(e) => Err(failed_step("rename", &e, "home_move_failed")),
            },
            AddMode::Copy => copy_into(
                &self.source,
                &self.destination,
                Copying::Standalone,
                self.bounds,
            )
            .map(|_| Placed::Whole),
        }
    }

    /// The copy a move falls back to across disks and out of iCloud Drive: copies every
    /// entry and checks the copy against the source, which stays whole. Output: the
    /// source to clear, with what the check matched, or the refusal code.
    pub(super) fn place_by_copy(self) -> Result<Placed, EngineError> {
        assert_eq!(self.mode, AddMode::Move);
        if fs::symlink_metadata(&self.destination).is_ok() {
            return Err(refused("home_destination_exists"));
        }
        let verified = copy_into(&self.source, &self.destination, Copying::Exact, self.bounds)?;
        Ok(Placed::Copied(OldSource {
            source: self.source,
            landed: self.destination,
            verified,
            bounds: self.bounds,
        }))
    }
}

/// What placing a folder left behind: nothing (a rename, or a Copy whose source stays on
/// purpose), or the whole source of a move that went by copy, still to be cleared.
#[derive(Debug)]
pub enum Placed {
    Whole,
    Copied(OldSource),
}

/// The source of a move that went by copy: the folder, the copy it landed as, what the
/// check of the copy matched and the bounds of the walk. Clearing it deletes only what the
/// check matched and what still equals its landed copy.
#[derive(Debug)]
pub struct OldSource {
    source: PathBuf,
    landed: PathBuf,
    verified: Verified,
    bounds: CopyBounds,
}

impl OldSource {
    /// The folder the move left behind.
    pub fn folder(&self) -> &Path {
        &self.source
    }

    /// Deletes the source entries the check matched and that are still as it saw them,
    /// then each folder left empty. Output: Ok once the source is gone, else
    /// `home_move_source_left` with every other entry kept in place.
    pub fn clear(self) -> Result<(), EngineError> {
        assert!(self.source.is_absolute());
        assert!(self.landed.is_absolute());
        make_writable(&self.source).map_err(|_| source_left(1))?;
        remove_verified(&self.source, &self.landed, &self.verified, self.bounds)
    }
}

/// A checked plan to move every top-level folder of the projects home to a
/// new home. Built only by `plan_move`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovePlan {
    from: PathBuf,
    to: PathBuf,
    folders: Vec<String>,
}

impl MovePlan {
    pub(super) fn new(from: PathBuf, to: PathBuf, folders: Vec<String>) -> Self {
        assert!(from.is_absolute() && to.is_absolute());
        assert_ne!(from, to);
        Self { from, to, folders }
    }

    /// The new home every folder moves to.
    pub fn destination(&self) -> &Path {
        &self.to
    }

    /// The names of the folders this plan moves.
    pub fn folders(&self) -> &[String] {
        &self.folders
    }

    /// Moves each folder in turn, as AddPlan's Move does. When one fails,
    /// the folders already moved are moved back; a folder whose checked copy
    /// landed but whose source could not be fully deleted is first made whole
    /// from that copy. Output: Ok when all moved, the first failure's code when
    /// everything went back (`home_move_failed` for a folder made whole), or
    /// `home_move_partial` when a folder could not be moved back.
    pub fn run(self) -> Result<(), EngineError> {
        let mut moved: Vec<&String> = Vec::with_capacity(self.folders.len());
        for name in &self.folders {
            let step = AddPlan::new(
                self.from.join(name),
                self.to.join(name),
                AddMode::Move,
                COPY_BOUNDS,
            );
            if let Err(mut failure) = step.run() {
                if failure.0 == "home_move_source_left" {
                    failure = match restore(&self.to.join(name), &self.from.join(name)) {
                        Ok(()) => refused("home_move_failed"),
                        Err(_) => refused("home_move_partial"),
                    };
                }
                for back in moved.into_iter().rev() {
                    let undo = AddPlan::new(
                        self.to.join(back),
                        self.from.join(back),
                        AddMode::Move,
                        COPY_BOUNDS,
                    );
                    if undo.run().is_err() {
                        return Err(refused("home_move_partial"));
                    }
                }
                return Err(failure);
            }
            moved.push(name);
        }
        assert_eq!(moved.len(), self.folders.len());
        Ok(())
    }
}

/// Moves a folder by copying it, checking every entry of the copy against the
/// source, and deleting the source only after that check passes.
/// Input: the real source folder and a free destination path. Output: Ok, or
/// `home_copy_unverified` with the source untouched, or `home_move_source_left`
/// when the folder landed but part of the source could not be deleted.
pub fn move_by_copy(source: &Path, destination: &Path) -> Result<(), EngineError> {
    move_within(source, destination, COPY_BOUNDS)
}

/// `move_by_copy` within the given bounds: only source entries the check of the copy
/// matched, and still as it saw them, are removed. Inputs: the source, the destination and
/// the bounds. Output: as `move_by_copy`.
fn move_within(source: &Path, destination: &Path, bounds: CopyBounds) -> Result<(), EngineError> {
    let verified = copy_into(source, destination, Copying::Exact, bounds)?;
    make_writable(source).map_err(|_| source_left(1))?;
    remove_verified(source, destination, &verified, bounds)
}

/// The source files and links a move's check found equal in the copy, each under a hash of
/// its path with the stamp it had when checked and whether that stamp is unproven, its
/// modify time being within [`STAMP_SETTLE`] of the check. The move deletes only entries
/// still carrying that stamp, so a file iCloud syncs in or changes after the check stays;
/// an entry with an unproven stamp must also still equal its landed copy. Two paths
/// sharing a hash keep one stamp, so the other entry stays too.
#[derive(Debug, Default)]
struct Verified(HashMap<u64, (Stamp, bool)>);

impl Verified {
    fn key(path: &Path) -> u64 {
        BuildHasherDefault::<DefaultHasher>::default().hash_one(path)
    }

    fn record(&mut self, path: &Path, metadata: &fs::Metadata) {
        assert!(!metadata.is_dir() || metadata.file_type().is_symlink());
        if let Some(checked) = stamp(metadata) {
            let unproven = checked.modified_since(SystemTime::now(), STAMP_SETTLE);
            self.0.insert(Self::key(path), (checked, unproven));
        }
    }

    /// Tells whether a source entry may be deleted: it still carries the stamp the check
    /// saw, and when that stamp is unproven its bytes, or its target for a link, still
    /// equal the landed copy's, read now. Inputs: the entry, its own metadata with links
    /// not followed, the source folder and the landed folder. Output: the answer; false
    /// when anything cannot be read.
    fn removable(
        &self,
        path: &Path,
        metadata: &fs::Metadata,
        source: &Path,
        landed: &Path,
    ) -> bool {
        assert!(!metadata.is_dir() || metadata.file_type().is_symlink());
        assert!(source.is_absolute());
        let Some((checked, unproven)) = self.0.get(&Self::key(path)) else {
            return false;
        };
        if stamp(metadata) != Some(*checked) {
            return false;
        }
        if !unproven {
            return true;
        }
        let Ok(below) = path.strip_prefix(source) else {
            return false;
        };
        let copy = landed.join(below);
        if metadata.file_type().is_symlink() {
            return fs::read_link(path)
                .is_ok_and(|target| fs::read_link(&copy).is_ok_and(|kept| kept == target));
        }
        fs::symlink_metadata(&copy).is_ok_and(|kept| kept.is_file())
            && same_bytes(path, &copy).unwrap_or(false)
    }
}

/// Deletes the files and links of a moved source that still carry the stamp its check saw
/// ([`Verified::removable`]), then each folder left empty, deepest first. An entry the
/// check never saw or that changed since, such as a file iCloud synced in or one rewritten
/// within the same clock tick, stays with its folders. Inputs: the source, the landed copy,
/// what the check matched and the bounds. Output: Ok once the source is gone, else
/// `home_move_source_left`, logged with the count of entries kept.
fn remove_verified(
    source: &Path,
    landed: &Path,
    verified: &Verified,
    bounds: CopyBounds,
) -> Result<(), EngineError> {
    assert!(source.is_absolute());
    let mut folders = vec![source.to_path_buf()];
    let mut stack = vec![(source.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    let mut kept = 0u64;
    while let Some((folder, depth)) = stack.pop() {
        let Ok(listing) = fs::read_dir(&folder) else {
            kept += 1;
            continue;
        };
        for entry in listing {
            entries += 1;
            if entries > bounds.entries {
                return Err(source_left(kept + 1));
            }
            let Ok(path) = entry.map(|entry| entry.path()) else {
                kept += 1;
                continue;
            };
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                kept += 1;
                continue;
            };
            let inner = metadata.is_dir() && !metadata.file_type().is_symlink();
            if inner && !super::icloud::dataless(&metadata) && depth < bounds.depth {
                folders.push(path.clone());
                stack.push((path, depth + 1));
            } else if inner
                || !verified.removable(&path, &metadata, source, landed)
                || fs::remove_file(&path).is_err()
            {
                kept += 1;
            }
        }
    }
    for folder in folders.iter().rev() {
        if fs::remove_dir(folder).is_err() {
            kept += 1;
        }
    }
    assert!(entries <= bounds.entries);
    assert_eq!(folders.first().map(PathBuf::as_path), Some(source));
    if kept > 0 {
        return Err(source_left(kept));
    }
    Ok(())
}

fn source_left(kept: u64) -> EngineError {
    assert!(kept > 0);
    eprintln!("home_move_source_left kept={kept}");
    refused("home_move_source_left")
}

/// Makes a source whole again after `move_by_copy` landed its checked copy
/// but could not delete all of the source, then removes the copy.
/// Input: the complete copy and what is left of the source. Output: Ok once
/// the source matches the copy again and the copy is gone; otherwise the code
/// of the step that failed, with nothing taken from the source.
fn restore(copy: &Path, source: &Path) -> Result<(), EngineError> {
    fill_tree(copy, source)?;
    verify_tree(copy, source)?;
    make_writable(copy)
        .and_then(|()| fs::remove_dir_all(copy))
        .map_err(|_| refused("home_move_partial"))
}

/// Copies back every entry of `copy` missing from `target` without following
/// links, then gives each folder of `target` its counterpart's permissions.
/// Input: a complete copy and a partly deleted folder it was copied from.
/// Output: Ok, or the refusal codes of `copy_tree`; entries still in `target`
/// are kept for `verify_tree` to compare.
fn fill_tree(copy: &Path, target: &Path) -> Result<(), EngineError> {
    let failed = |e: std::io::Error| failed_step("refill", &e, "home_copy_failed");
    if fs::symlink_metadata(target).is_err() {
        return copy_tree(copy, target);
    }
    let root = fs::symlink_metadata(copy).map_err(failed)?;
    let mut folders = vec![(target.to_path_buf(), root)];
    let mut stack = vec![(copy.to_path_buf(), target.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    while let Some((from, to, depth)) = stack.pop() {
        for entry in fs::read_dir(&from).map_err(failed)? {
            entries += 1;
            if entries > ENTRIES_MAX {
                return Err(refused("home_copy_too_large"));
            }
            let entry = entry.map_err(failed)?;
            let path = entry.path();
            let kept = to.join(entry.file_name());
            let metadata = fs::symlink_metadata(&path).map_err(failed)?;
            let kind = metadata.file_type();
            match fs::symlink_metadata(&kept) {
                Err(_) if kind.is_symlink() => {
                    let link = fs::read_link(&path).map_err(failed)?;
                    copy_link(&path, link, &kept).map_err(failed)?;
                }
                Err(_) if kind.is_dir() => copy_tree(&path, &kept)?,
                Err(_) => {
                    fs::copy(&path, &kept).map_err(failed)?;
                    fs::set_permissions(&kept, metadata.permissions()).map_err(failed)?;
                }
                Ok(existing)
                    if kind.is_dir() && existing.is_dir() && !existing.file_type().is_symlink() =>
                {
                    if depth + 1 > DEPTH_MAX {
                        return Err(refused("home_copy_too_large"));
                    }
                    folders.push((kept.clone(), metadata));
                    stack.push((path, kept, depth + 1));
                }
                Ok(_) => {}
            }
        }
    }
    for (folder, wanted) in folders.into_iter().rev() {
        let now = fs::symlink_metadata(&folder).map_err(failed)?;
        if permission_bits(&now) != permission_bits(&wanted) {
            fs::set_permissions(&folder, wanted.permissions()).map_err(failed)?;
        }
    }
    assert!(entries <= ENTRIES_MAX);
    Ok(())
}

/// How `copy_into` copies: every entry, checked against the source afterwards, for a
/// move; or as a standalone repository for a Copy, leaving out the git worktree links
/// that would still tie the copy to the source (see `left_out`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Copying {
    Exact,
    Standalone,
}

fn copy_into(
    source: &Path,
    destination: &Path,
    how: Copying,
    bounds: CopyBounds,
) -> Result<Verified, EngineError> {
    let parent = destination
        .parent()
        .ok_or_else(|| refused("home_copy_failed"))?;
    let mut token = [0u8; 8];
    getrandom::fill(&mut token).map_err(|_| refused("home_copy_failed"))?;
    let staging = parent.join(format!(
        ".crazyagent-adding-{}",
        token.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    let result = match how {
        Copying::Exact => copy_entries(source, &staging, &|_, _| false, bounds)
            .and_then(|()| verify_within(source, &staging, bounds)),
        Copying::Standalone => copy_entries(
            source,
            &staging,
            &|path, kind| left_out(source, path, kind),
            bounds,
        )
        .map(|()| Verified::default()),
    }
    .and_then(|verified| {
        if fs::symlink_metadata(destination).is_ok() {
            return Err(refused("home_destination_exists"));
        }
        fs::rename(&staging, destination)
            .map(|()| verified)
            .map_err(|e| failed_step("place", &e, "home_copy_failed"))
    });
    if result.is_err() && fs::symlink_metadata(&staging).is_ok() {
        let _ = make_writable(&staging);
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// The refusal for one failed step of an add, logged by step and errno, never by path: a
/// file still only in iCloud (EDEADLK, what macOS answers a launchd agent that may not
/// download it), a folder macOS privacy or its permissions keep CrazyAgent from (EPERM,
/// EACCES), or else the step's own code. Inputs: the step, the error and that code.
/// Output: the refusal.
fn failed_step(step: &str, error: &std::io::Error, other: &'static str) -> EngineError {
    assert!(!step.is_empty() && other.starts_with("home_"));
    let code = match error.kind() {
        std::io::ErrorKind::Deadlock => "home_icloud_not_downloaded",
        std::io::ErrorKind::PermissionDenied => "home_permission_denied",
        _ => other,
    };
    let errno = error.raw_os_error().unwrap_or(0);
    eprintln!("home_add_step_failed step={step} code={code} errno={errno}");
    refused(code)
}

/// The refusal for a placeholder an add step met: its data lives only in iCloud, and
/// reading it would fail or fetch it, so nothing of it is copied, checked or deleted.
/// Input: the step. Output: `home_icloud_not_downloaded`, logged by step, never by path.
fn still_in_icloud(step: &str) -> EngineError {
    assert!(!step.is_empty());
    eprintln!("home_add_step_failed step={step} code=home_icloud_not_downloaded cause=dataless");
    refused("home_icloud_not_downloaded")
}

fn make_writable(root: &Path) -> std::io::Result<()> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(folder) = stack.pop() {
        writable_folder(&folder)?;
        for entry in fs::read_dir(&folder)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

/// Copies a folder tree without following links.
/// Input: a real source folder and a target path that does not exist yet.
/// Output: Ok with files, folders and their permissions copied and every
/// symlink recreated as the same symlink, never followed out of the source;
/// `home_copy_special_file` for devices, pipes or sockets, and
/// `home_copy_too_large` past the entry or depth bound.
pub fn copy_tree(source: &Path, target: &Path) -> Result<(), EngineError> {
    copy_entries(source, target, &|_, _| false, COPY_BOUNDS)
}

/// The skip rule of a standalone copy, asked for each entry below the source root.
/// Inputs: the source root, the entry and its type. Output: true for a git folder's
/// `worktrees` admin folder (the main `.git`, a nested repository's or a submodule's)
/// and for a linked worktree checkout: a folder whose `.git` is a link, or a file not
/// pointing relatively inside the source at a git folder other than a worktree admin.
fn left_out(root: &Path, path: &Path, kind: fs::FileType) -> bool {
    if !kind.is_dir() {
        return false;
    }
    if path.file_name() == Some(std::ffi::OsStr::new("worktrees"))
        && path.parent().is_some_and(git_folder)
    {
        return true;
    }
    let link = path.join(".git");
    match fs::symlink_metadata(&link) {
        Err(_) => false,
        Ok(metadata) if metadata.file_type().is_symlink() => true,
        Ok(metadata) if metadata.is_dir() => false,
        Ok(_) => !gitdir_inside(root, path, &link),
    }
}

/// True for a folder holding git's own data, as git itself tells one: a `HEAD` file
/// and `objects` and `refs` folders, none of them links.
fn git_folder(folder: &Path) -> bool {
    let plain = |name: &str, folder_wanted: bool| {
        fs::symlink_metadata(folder.join(name)).is_ok_and(|metadata| {
            !metadata.file_type().is_symlink() && metadata.is_dir() == folder_wanted
        })
    };
    plain("HEAD", false) && plain("objects", true) && plain("refs", true)
}

/// True when the `gitdir:` line of a `.git` file in `folder` is a relative path, its
/// `..` parts leading, that stays inside `root` and does not name a worktree admin
/// folder (`<git folder>/worktrees/<name>`), as a submodule's does. Inputs: the source
/// root, the folder and its `.git` file. Output: false otherwise, or unreadable.
fn gitdir_inside(root: &Path, folder: &Path, file: &Path) -> bool {
    let Some(text) = crate::platform::read_plain_file(file, GITDIR_FILE_BYTES_MAX) else {
        return false;
    };
    let Some(target) = text
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|target| !target.is_empty())
    else {
        return false;
    };
    let mut resolved = folder.to_path_buf();
    let mut named = false;
    for part in Path::new(target).components() {
        match part {
            std::path::Component::ParentDir if !named => {
                if !resolved.pop() {
                    return false;
                }
            }
            std::path::Component::Normal(name) => {
                named = true;
                resolved.push(name);
            }
            std::path::Component::CurDir => {}
            _ => return false,
        }
    }
    let admin = resolved.parent().is_some_and(|parent| {
        parent.file_name() == Some(std::ffi::OsStr::new("worktrees"))
            && parent.parent().is_some_and(git_folder)
    });
    named && resolved.starts_with(root) && resolved != root && !admin
}

/// Copies a folder tree without following links, leaving out each entry `skip` names.
/// Inputs: a real source folder, a target path that does not exist yet, the skip rule
/// and the bounds. Output: as `copy_tree`.
fn copy_entries(
    source: &Path,
    target: &Path,
    skip: &dyn Fn(&Path, fs::FileType) -> bool,
    bounds: CopyBounds,
) -> Result<(), EngineError> {
    let failed = |e: std::io::Error| failed_step("copy", &e, "home_copy_failed");
    let root = fs::symlink_metadata(source).map_err(failed)?;
    if !root.is_dir() || root.file_type().is_symlink() {
        return Err(refused("home_source_not_folder"));
    }
    if super::icloud::dataless(&root) {
        return Err(still_in_icloud("copy"));
    }
    fs::create_dir(target).map_err(failed)?;
    private_folder(target).map_err(failed)?;
    let mut folders = vec![(target.to_path_buf(), root.permissions())];
    let mut stack = vec![(source.to_path_buf(), target.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    while let Some((from, to, depth)) = stack.pop() {
        for entry in fs::read_dir(&from).map_err(failed)? {
            entries += 1;
            if entries > bounds.entries {
                return Err(refused("home_copy_too_large"));
            }
            let entry = entry.map_err(failed)?;
            let path = entry.path();
            let copy = to.join(entry.file_name());
            let metadata = fs::symlink_metadata(&path).map_err(failed)?;
            let kind = metadata.file_type();
            if skip(&path, kind) {
                continue;
            }
            if super::icloud::dataless(&metadata) {
                return Err(still_in_icloud("copy"));
            }
            if kind.is_symlink() {
                let link = fs::read_link(&path).map_err(failed)?;
                copy_link(&path, link, &copy).map_err(failed)?;
            } else if kind.is_dir() {
                if depth + 1 > bounds.depth {
                    return Err(refused("home_copy_too_large"));
                }
                fs::create_dir(&copy).map_err(failed)?;
                private_folder(&copy).map_err(failed)?;
                folders.push((copy.clone(), metadata.permissions()));
                stack.push((path, copy, depth + 1));
            } else if kind.is_file() {
                fs::copy(&path, &copy).map_err(failed)?;
                fs::set_permissions(&copy, metadata.permissions()).map_err(failed)?;
            } else {
                return Err(refused("home_copy_special_file"));
            }
        }
    }
    for (folder, permissions) in folders.into_iter().rev() {
        fs::set_permissions(&folder, permissions).map_err(failed)?;
    }
    assert!(entries <= bounds.entries);
    Ok(())
}

/// Checks that a copy matches its source entry by entry.
/// Input: the source and the copy. Output: Ok when both hold the same names,
/// kinds, permission bits, symlink targets and file bytes; otherwise
/// `home_copy_unverified`.
pub fn verify_tree(source: &Path, copy: &Path) -> Result<(), EngineError> {
    verify_within(source, copy, COPY_BOUNDS).map(drop)
}

/// `verify_tree` within the given bounds. Inputs: the source, the copy and the bounds.
/// Output: the source files and links found equal, or the code of `verify_tree`, or
/// `home_icloud_not_downloaded` for a source entry whose data lives only in iCloud.
fn verify_within(source: &Path, copy: &Path, bounds: CopyBounds) -> Result<Verified, EngineError> {
    let differs = || refused("home_copy_unverified");
    let mut verified = Verified::default();
    let mut stack = vec![(source.to_path_buf(), copy.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    while let Some((left, right, depth)) = stack.pop() {
        let a = fs::symlink_metadata(&left).map_err(|_| differs())?;
        let b = fs::symlink_metadata(&right).map_err(|_| differs())?;
        if super::icloud::dataless(&a) {
            return Err(still_in_icloud("verify"));
        }
        if a.file_type().is_symlink() != b.file_type().is_symlink()
            || a.is_dir() != b.is_dir()
            || a.is_file() != b.is_file()
        {
            return Err(differs());
        }
        if a.file_type().is_symlink() {
            if fs::read_link(&left).ok() != fs::read_link(&right).ok() {
                return Err(differs());
            }
            verified.record(&left, &a);
            continue;
        }
        if permission_bits(&a) != permission_bits(&b) {
            return Err(differs());
        }
        if a.is_file() {
            if a.len() != b.len() || !same_bytes(&left, &right)? {
                return Err(differs());
            }
            verified.record(&left, &a);
            continue;
        }
        if depth > bounds.depth {
            return Err(differs());
        }
        let names = |folder: &Path| -> Result<Vec<std::ffi::OsString>, EngineError> {
            let mut names = Vec::new();
            for entry in fs::read_dir(folder).map_err(|_| differs())? {
                names.push(entry.map_err(|_| differs())?.file_name());
            }
            names.sort();
            Ok(names)
        };
        let left_names = names(&left)?;
        if left_names != names(&right)? {
            return Err(differs());
        }
        for name in left_names {
            entries += 1;
            if entries > bounds.entries {
                return Err(differs());
            }
            stack.push((left.join(&name), right.join(&name), depth + 1));
        }
    }
    assert!(verified.0.len() as u64 <= entries + 1);
    Ok(verified)
}

fn same_bytes(left: &Path, right: &Path) -> Result<bool, EngineError> {
    let open = |path: &Path| fs::File::open(path).map_err(|_| refused("home_copy_unverified"));
    let (mut a, mut b) = (open(left)?, open(right)?);
    let mut first = vec![0u8; CHUNK_BYTES];
    let mut second = vec![0u8; CHUNK_BYTES];
    loop {
        let read = a
            .read(&mut first)
            .map_err(|_| refused("home_copy_unverified"))?;
        if read == 0 {
            let mut probe = [0u8; 1];
            return Ok(b
                .read(&mut probe)
                .map_err(|_| refused("home_copy_unverified"))?
                == 0);
        }
        b.read_exact(&mut second[..read])
            .map_err(|_| refused("home_copy_unverified"))?;
        if first[..read] != second[..read] {
            return Ok(false);
        }
    }
}

/// What `folder_size` found in a folder: the bytes of its files and how many of its
/// entries are placeholders whose data lives only in iCloud, counted without listing a
/// placeholder folder, so a folder inside one counts once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    pub bytes: u64,
    pub placeholders: u64,
}

/// Adds up the bytes of the files in a folder without following links, and counts its
/// iCloud placeholders; a folder that is a placeholder itself counts as one.
/// Input: the owner's path text and the time by which to give up. Output: the measure,
/// `folder_size_unsafe` for `/`, the user's home folder or any folder holding it,
/// `folder_size_unreadable`, `folder_size_timeout` or `folder_size_too_large` past the
/// entry bound.
pub fn folder_size(path: &str, deadline: Instant) -> Result<Measured, EngineError> {
    path_text(path, "folder_size_path_invalid")?;
    let root = real_folder(
        Path::new(path),
        "folder_size_unreadable",
        "folder_size_unsafe",
    )?;
    if holds_owner_home(&root, &owner_home()?) {
        return Err(refused("folder_size_unsafe"));
    }
    let top = fs::symlink_metadata(&root).map_err(|_| refused("folder_size_unreadable"))?;
    if super::icloud::dataless(&top) {
        return Ok(Measured {
            bytes: 0,
            placeholders: 1,
        });
    }
    let mut bytes = 0u64;
    let mut placeholders = 0u64;
    let mut entries = 0u64;
    let mut stack = vec![root];
    while let Some(folder) = stack.pop() {
        let listing = fs::read_dir(&folder).map_err(|_| refused("folder_size_unreadable"))?;
        for entry in listing {
            entries += 1;
            if entries > ENTRIES_MAX {
                return Err(refused("folder_size_too_large"));
            }
            if entries.is_multiple_of(DEADLINE_EVERY) && Instant::now() >= deadline {
                return Err(refused("folder_size_timeout"));
            }
            let entry = entry.map_err(|_| refused("folder_size_unreadable"))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| refused("folder_size_unreadable"))?;
            let placeholder = super::icloud::dataless(&metadata);
            if placeholder {
                placeholders += 1;
            }
            if metadata.is_dir() && !metadata.file_type().is_symlink() && !placeholder {
                stack.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    assert!(entries <= ENTRIES_MAX);
    assert!(placeholders <= entries);
    Ok(Measured {
        bytes,
        placeholders,
    })
}

#[cfg(all(test, unix))]
mod tests;
