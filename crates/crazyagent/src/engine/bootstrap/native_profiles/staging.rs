use super::*;
use rustix::fs::RenameFlags;

pub(super) const STAGING: &str = ".staging-";
pub(super) const ROOT_STAGING: &str = ".native-profiles-staging-";
pub(super) const STAGING_MAX: usize = 16;
const TREE_ENTRIES_MAX: usize = 8;

#[cfg(test)]
thread_local! {
    pub(super) static INTERRUPT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Stops a staged change at this point when a test chose it, the way a crash would.
/// No input. Output: `native_profile_interrupted` on the step a test selected through
/// `INTERRUPT` (counted from 1); Ok on every other step and always outside tests.
pub(super) fn step() -> Result<(), EngineError> {
    #[cfg(test)]
    {
        let left = INTERRUPT.get();
        if left == 1 {
            INTERRUPT.set(0);
            return Err(failure("native_profile_interrupted"));
        }
        INTERRUPT.set(left.saturating_sub(1));
    }
    Ok(())
}

/// Tells whether a directory entry name is a staging name with the given prefix.
/// Inputs: the raw entry name and the prefix. Output: true for the prefix followed by
/// exactly 32 lowercase hex digits, the only names `Staging::create` makes.
pub(super) fn is_staging(name: &[u8], prefix: &str) -> bool {
    name.strip_prefix(prefix.as_bytes())
        .and_then(|rest| std::str::from_utf8(rest).ok())
        .is_some_and(|rest| hex(rest, 32))
}

/// A private directory beside its destination, built while its creator holds an
/// exclusive lock on it and renamed into place only when complete, so the destination
/// name only ever appears whole and an abandoned one is provably free to remove.
pub(super) struct Staging {
    name: String,
    pub(super) directory: Directory,
}

impl Staging {
    pub(super) fn create(parent: &File, prefix: &str) -> Result<Self, EngineError> {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|_| failure("native_profile_entropy_unavailable"))?;
        let name = format!(
            "{prefix}{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        assert!(is_staging(name.as_bytes(), prefix));
        rustix::fs::mkdirat(parent, name.as_str(), Mode::from_raw_mode(0o700))
            .map_err(unsafe_custody)?;
        step()?;
        let directory = Directory::open(parent, OsStr::new(&name))?;
        lock(&directory.file)?;
        Ok(Self { name, directory })
    }

    /// Renames the finished staging directory to its destination name, never over an
    /// existing entry. Inputs: the shared parent and the destination name. Output: the
    /// same directory under its new name, unlocked; `native_profile_in_use` if the name
    /// appeared meanwhile, leaving this staging directory for a later sweep.
    pub(super) fn commit(self, parent: &File, destination: &str) -> Result<Directory, EngineError> {
        self.directory.file.sync_all().map_err(unsafe_custody)?;
        step()?;
        rustix::fs::renameat_with(
            parent,
            self.name.as_str(),
            parent,
            destination,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                failure("native_profile_in_use")
            } else {
                unsafe_custody(error)
            }
        })?;
        parent.sync_all().map_err(unsafe_custody)?;
        step()?;
        self.directory.verify(parent, OsStr::new(destination))?;
        flock(&self.directory.file, FlockOperation::Unlock).map_err(unsafe_custody)?;
        Ok(self.directory)
    }

    /// Moves one finished file out of this staging directory over a file of the same
    /// name in another directory, then removes the emptied staging directory.
    /// Inputs: the staging parent, the file name and its destination directory.
    /// Output: Ok once the destination names the staged file and the parent is synced.
    pub(super) fn replace_into(
        self,
        parent: &File,
        name: &str,
        destination: &File,
    ) -> Result<(), EngineError> {
        self.directory.file.sync_all().map_err(unsafe_custody)?;
        step()?;
        rustix::fs::renameat(&self.directory.file, name, destination, name)
            .map_err(unsafe_custody)?;
        destination.sync_all().map_err(unsafe_custody)?;
        step()?;
        self.directory.verify(parent, OsStr::new(&self.name))?;
        rustix::fs::unlinkat(parent, self.name.as_str(), AtFlags::REMOVEDIR)
            .map_err(unsafe_custody)?;
        parent.sync_all().map_err(unsafe_custody)
    }
}

/// Removes every abandoned staging directory with the given prefix from a parent.
/// Inputs: the parent and the prefix. Output: Ok after removing each one whose creator
/// no longer holds its lock; a live one is left alone, and one that is not a private
/// directory of this user, or holds anything a staged change never writes, is refused.
pub(super) fn sweep(parent: &File, prefix: &str) -> Result<(), EngineError> {
    let mut names = Vec::new();
    for entry in Dir::read_from(parent).map_err(unsafe_custody)? {
        let entry = entry.map_err(unsafe_custody)?;
        if is_staging(entry.file_name().to_bytes(), prefix) {
            if names.len() == STAGING_MAX {
                return Err(failure("native_profile_capacity"));
            }
            names.push(entry.file_name().to_owned());
        }
    }
    for name in names {
        let staged = Directory::open(parent, OsStr::from_bytes(name.to_bytes()))?;
        match flock(&staged.file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(rustix::io::Errno::WOULDBLOCK) => continue,
            Err(error) => return Err(unsafe_custody(error)),
        }
        staged.verify(parent, OsStr::from_bytes(name.to_bytes()))?;
        remove_tree(&staged.file, 1)?;
        rustix::fs::unlinkat(parent, name.as_c_str(), AtFlags::REMOVEDIR)
            .map_err(unsafe_custody)?;
    }
    parent.sync_all().map_err(unsafe_custody)
}

/// Removes the contents of an engine-made directory: regular files of this user and,
/// while depth remains, private subdirectories holding the same. Inputs: the open
/// directory and the depth left. Output: Ok once it is empty; anything else is refused
/// with `native_profile_custody_unsafe` and left in place.
pub(super) fn remove_tree(directory: &File, depth: usize) -> Result<(), EngineError> {
    let mut names = Vec::new();
    for entry in Dir::read_from(directory).map_err(unsafe_custody)? {
        let entry = entry.map_err(unsafe_custody)?;
        if matches!(entry.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        if names.len() == TREE_ENTRIES_MAX {
            return Err(failure("native_profile_custody_unsafe"));
        }
        names.push(entry.file_name().to_owned());
    }
    for name in names {
        let metadata = rustix::fs::statat(directory, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(unsafe_custody)?;
        if metadata.st_uid != rustix::process::getuid().as_raw() {
            return Err(failure("native_profile_custody_unsafe"));
        }
        match FileType::from_raw_mode(metadata.st_mode) {
            FileType::RegularFile => {
                rustix::fs::unlinkat(directory, name.as_c_str(), AtFlags::empty())
                    .map_err(unsafe_custody)?;
            }
            FileType::Directory if depth > 0 => {
                let child = Directory::open(directory, OsStr::from_bytes(name.to_bytes()))?;
                remove_tree(&child.file, depth - 1)?;
                child.verify(directory, OsStr::from_bytes(name.to_bytes()))?;
                rustix::fs::unlinkat(directory, name.as_c_str(), AtFlags::REMOVEDIR)
                    .map_err(unsafe_custody)?;
            }
            _ => return Err(failure("native_profile_custody_unsafe")),
        }
    }
    directory.sync_all().map_err(unsafe_custody)
}
