use super::platform::{copy_link, permission_bits, private_folder, writable_folder};
use super::{AddMode, holds_owner_home, owner_home, path_text, real_folder, refused};
use crate::engine::EngineError;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

const ENTRIES_MAX: u64 = 4_000_000;
const DEPTH_MAX: usize = 256;
const CHUNK_BYTES: usize = 64 * 1024;
const DEADLINE_EVERY: u64 = 1024;

/// A checked plan to bring one folder into the projects home.
/// Built only by `plan_add`, which refuses unsafe sources and taken names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddPlan {
    source: PathBuf,
    destination: PathBuf,
    mode: AddMode,
}

impl AddPlan {
    pub(super) fn new(source: PathBuf, destination: PathBuf, mode: AddMode) -> Self {
        assert!(source.is_absolute() && destination.is_absolute());
        assert_ne!(source, destination);
        Self {
            source,
            destination,
            mode,
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
    /// disks it copies, checks the copy and only then deletes the source.
    /// Output: Ok once the folder is complete at its destination, or a code;
    /// on failure the source is left as it was and no partial folder shows.
    pub fn run(self) -> Result<(), EngineError> {
        if fs::symlink_metadata(&self.destination).is_ok() {
            return Err(refused("home_destination_exists"));
        }
        match self.mode {
            AddMode::Move => match fs::rename(&self.source, &self.destination) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                    move_by_copy(&self.source, &self.destination)
                }
                Err(_) => Err(refused("home_move_failed")),
            },
            AddMode::Copy => copy_into(&self.source, &self.destination, false),
        }
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
            let step = AddPlan::new(self.from.join(name), self.to.join(name), AddMode::Move);
            if let Err(mut failure) = step.run() {
                if failure.0 == "home_move_source_left" {
                    failure = match restore(&self.to.join(name), &self.from.join(name)) {
                        Ok(()) => refused("home_move_failed"),
                        Err(_) => refused("home_move_partial"),
                    };
                }
                for back in moved.into_iter().rev() {
                    let undo =
                        AddPlan::new(self.to.join(back), self.from.join(back), AddMode::Move);
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
    copy_into(source, destination, true)?;
    make_writable(source)
        .and_then(|()| fs::remove_dir_all(source))
        .map_err(|_| refused("home_move_source_left"))
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
    let failed = |_| refused("home_copy_failed");
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

fn copy_into(source: &Path, destination: &Path, verify: bool) -> Result<(), EngineError> {
    let parent = destination
        .parent()
        .ok_or_else(|| refused("home_copy_failed"))?;
    let mut token = [0u8; 8];
    getrandom::fill(&mut token).map_err(|_| refused("home_copy_failed"))?;
    let staging = parent.join(format!(
        ".crazyagent-adding-{}",
        token.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ));
    let result = copy_tree(source, &staging)
        .and_then(|()| {
            if verify {
                verify_tree(source, &staging)
            } else {
                Ok(())
            }
        })
        .and_then(|()| {
            if fs::symlink_metadata(destination).is_ok() {
                return Err(refused("home_destination_exists"));
            }
            fs::rename(&staging, destination).map_err(|_| refused("home_copy_failed"))
        });
    if result.is_err() && fs::symlink_metadata(&staging).is_ok() {
        let _ = make_writable(&staging);
        let _ = fs::remove_dir_all(&staging);
    }
    result
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
    let failed = |_| refused("home_copy_failed");
    let root = fs::symlink_metadata(source).map_err(failed)?;
    if !root.is_dir() || root.file_type().is_symlink() {
        return Err(refused("home_source_not_folder"));
    }
    fs::create_dir(target).map_err(failed)?;
    private_folder(target).map_err(failed)?;
    let mut folders = vec![(target.to_path_buf(), root.permissions())];
    let mut stack = vec![(source.to_path_buf(), target.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    while let Some((from, to, depth)) = stack.pop() {
        for entry in fs::read_dir(&from).map_err(failed)? {
            entries += 1;
            if entries > ENTRIES_MAX {
                return Err(refused("home_copy_too_large"));
            }
            let entry = entry.map_err(failed)?;
            let path = entry.path();
            let copy = to.join(entry.file_name());
            let metadata = fs::symlink_metadata(&path).map_err(failed)?;
            let kind = metadata.file_type();
            if kind.is_symlink() {
                let link = fs::read_link(&path).map_err(failed)?;
                copy_link(&path, link, &copy).map_err(failed)?;
            } else if kind.is_dir() {
                if depth + 1 > DEPTH_MAX {
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
    assert!(entries <= ENTRIES_MAX);
    Ok(())
}

/// Checks that a copy matches its source entry by entry.
/// Input: the source and the copy. Output: Ok when both hold the same names,
/// kinds, permission bits, symlink targets and file bytes; otherwise
/// `home_copy_unverified`.
pub fn verify_tree(source: &Path, copy: &Path) -> Result<(), EngineError> {
    let differs = || refused("home_copy_unverified");
    let mut stack = vec![(source.to_path_buf(), copy.to_path_buf(), 0usize)];
    let mut entries = 0u64;
    while let Some((left, right, depth)) = stack.pop() {
        let a = fs::symlink_metadata(&left).map_err(|_| differs())?;
        let b = fs::symlink_metadata(&right).map_err(|_| differs())?;
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
            continue;
        }
        if permission_bits(&a) != permission_bits(&b) {
            return Err(differs());
        }
        if a.is_file() {
            if a.len() != b.len() || !same_bytes(&left, &right)? {
                return Err(differs());
            }
            continue;
        }
        if depth > DEPTH_MAX {
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
            if entries > ENTRIES_MAX {
                return Err(differs());
            }
            stack.push((left.join(&name), right.join(&name), depth + 1));
        }
    }
    Ok(())
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

/// Adds up the bytes of the files in a folder without following links.
/// Input: the owner's path text and the time by which to give up. Output:
/// the total file bytes, `folder_size_unsafe` for `/`, the user's home folder
/// or any folder holding it, `folder_size_unreadable`, `folder_size_timeout`
/// or `folder_size_too_large` past the entry bound.
pub fn folder_size(path: &str, deadline: Instant) -> Result<u64, EngineError> {
    path_text(path, "folder_size_path_invalid")?;
    let root = real_folder(
        Path::new(path),
        "folder_size_unreadable",
        "folder_size_unsafe",
    )?;
    if holds_owner_home(&root, &owner_home()?) {
        return Err(refused("folder_size_unsafe"));
    }
    let mut bytes = 0u64;
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
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
            }
        }
    }
    assert!(entries <= ENTRIES_MAX);
    Ok(bytes)
}
