use super::*;
#[cfg(unix)]
use rustix::fs::RenameFlags;

#[cfg(unix)]
pub(super) const STAGING: &str = ".staging-";
pub(super) const ROOT_STAGING: &str = ".native-profiles-staging-";
pub(super) const STAGING_MAX: usize = 16;
const TREE_ENTRIES_MAX: usize = 8;

#[cfg(test)]
thread_local! {
    pub(super) static INTERRUPT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static AT_STEP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Stops a staged change at this point when a test chose it, the way a crash would, or runs
/// the action the test put in `AT_STEP` there and goes on. No input. Output:
/// `native_profile_interrupted` on the step a test selected through `INTERRUPT` (counted
/// from 1) and set no action for; Ok on every other step and always outside tests.
pub(super) fn step() -> Result<(), EngineError> {
    #[cfg(test)]
    {
        let left = INTERRUPT.get();
        if left == 1 {
            INTERRUPT.set(0);
            if let Some(action) = AT_STEP.take() {
                action();
                return Ok(());
            }
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
    #[cfg(unix)]
    _held: HeldLock,
    #[cfg(windows)]
    _claim: File,
}

/// Where [`Staging::replace_into`] failed: at its check that the staging directory is still
/// the one the engine made, a tamper check that a directory put in its place fails
/// (`Changed`), or at any other step, an I/O failure or a test's stop (`Stopped`). Each holds
/// the error the caller returns.
#[cfg(unix)]
pub(super) enum Unreplaced {
    Stopped(EngineError),
    Changed(EngineError),
}

/// Gives the error an [`Unreplaced`] holds, for a caller that treats every failure alike.
/// Input: the failure. Output: its error.
#[cfg(unix)]
impl From<Unreplaced> for EngineError {
    fn from(unreplaced: Unreplaced) -> Self {
        match unreplaced {
            Unreplaced::Stopped(error) | Unreplaced::Changed(error) => error,
        }
    }
}

impl Staging {
    /// Makes a staging directory in `parent` under a fresh random name with the prefix and
    /// claims it: an exclusive `flock` on Unix, the lock on its lock stream on Windows
    /// (DESIGN-0601 R1), so a sweep leaves it alone while its creator lives. Inputs: the parent
    /// and the prefix. Output: the claimed staging directory, or the error.
    pub(super) fn create(parent: &Folder, prefix: &str) -> Result<Self, EngineError> {
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
        let folder = parent
            .make_dir(&held_name(OsStr::new(&name))?)
            .map_err(|_| failure("native_profile_custody_unsafe"))?;
        let directory = Directory::of(folder);
        step()?;
        #[cfg(unix)]
        let held = lock(directory.file())?;
        #[cfg(windows)]
        let claim = directory
            .folder
            .claim()
            .map_err(|_| failure("native_profile_custody_unsafe"))?;
        Ok(Self {
            name,
            directory,
            #[cfg(unix)]
            _held: held,
            #[cfg(windows)]
            _claim: claim,
        })
    }

    /// Renames the finished staging directory to its destination name through the held
    /// layer, never over an existing entry (R4), then opens the destination again and
    /// requires it to be the staged directory. Every handle below it must be closed first, as
    /// Windows refuses to rename a folder with an open child. Inputs: the shared parent and
    /// the destination name. Output: the same directory under its new name, unclaimed;
    /// `native_profile_in_use` if the name appeared meanwhile, leaving this staging directory
    /// for a later sweep.
    pub(super) fn commit(
        self,
        parent: &Folder,
        destination: &str,
    ) -> Result<Directory, EngineError> {
        let unsafe_commit = |_| failure("native_profile_custody_unsafe");
        self.directory.folder.sync().map_err(unsafe_commit)?;
        step()?;
        self.verify_in(parent)?;
        let Self {
            name,
            directory,
            #[cfg(unix)]
                _held: held,
            #[cfg(windows)]
                _claim: claim,
        } = self;
        let staged = directory.identity;
        let from = held_name(OsStr::new(&name))?;
        let to = held_name(OsStr::new(destination))?;
        parent
            .rename(directory.folder, &from, &to)
            .map_err(|error| match error {
                HeldError::Unsafe => failure("native_profile_in_use"),
                _ => failure("native_profile_custody_unsafe"),
            })?;
        parent.sync().map_err(unsafe_commit)?;
        step()?;
        #[cfg(unix)]
        crate::engine::bootstrap::held::unlock(&held.0);
        #[cfg(unix)]
        drop(held);
        #[cfg(windows)]
        drop(claim);
        let directory = Directory::open(parent, OsStr::new(destination))?;
        if directory.identity != staged {
            return Err(failure("native_profile_custody_unsafe"));
        }
        Ok(directory)
    }

    /// Moves one finished file out of this staging directory over a file of the same
    /// name in another directory, then removes the emptied staging directory.
    /// Inputs: the staging parent, the file name and its destination directory.
    /// Output: Ok once the destination names the staged file and the parent is synced,
    /// or the failure, told apart by [`Unreplaced`].
    #[cfg(unix)]
    pub(super) fn replace_into(
        self,
        parent: &Folder,
        name: &str,
        destination: &File,
    ) -> Result<(), Unreplaced> {
        self.move_into(name, destination)
            .map_err(Unreplaced::Stopped)?;
        self.verify_in(parent).map_err(Unreplaced::Changed)?;
        rustix::fs::unlinkat(parent.file(), self.name.as_str(), AtFlags::REMOVEDIR)
            .map_err(|error| Unreplaced::Stopped(unsafe_custody(error)))?;
        parent
            .file()
            .sync_all()
            .map_err(|error| Unreplaced::Stopped(unsafe_custody(error)))
    }

    /// Moves one finished file out of this staging directory over a file of the same name
    /// in another directory and syncs that directory. Inputs: the file name and the
    /// destination directory. Output: Ok once the destination names the staged file.
    #[cfg(unix)]
    fn move_into(&self, name: &str, destination: &File) -> Result<(), EngineError> {
        self.directory.file().sync_all().map_err(unsafe_custody)?;
        step()?;
        rustix::fs::renameat(self.directory.file(), name, destination, name)
            .map_err(unsafe_custody)?;
        destination.sync_all().map_err(unsafe_custody)?;
        step()
    }

    /// Swaps one finished file in this staging directory with the file of the same name in
    /// another directory in a single rename, or moves it in when that name is free there,
    /// so this staging directory then holds the file it replaced (RV-DS241-50). Inputs: the
    /// name and the other directory. Output: whether a file was replaced; nothing after
    /// the rename can fail here, so the caller owns every check that follows it.
    #[cfg(unix)]
    pub(super) fn exchange(&self, name: &str, destination: &File) -> Result<bool, EngineError> {
        self.directory.file().sync_all().map_err(unsafe_custody)?;
        step()?;
        match rustix::fs::renameat_with(
            self.directory.file(),
            name,
            destination,
            name,
            RenameFlags::EXCHANGE,
        ) {
            Ok(()) => Ok(true),
            Err(rustix::io::Errno::NOENT) => {
                rustix::fs::renameat_with(
                    self.directory.file(),
                    name,
                    destination,
                    name,
                    RenameFlags::NOREPLACE,
                )
                .map_err(unsafe_custody)?;
                Ok(false)
            }
            Err(error) => Err(unsafe_custody(error)),
        }
    }

    /// Checks that this staging directory is still the one named in its parent. Input: the
    /// parent. Output: Ok, or the identity failure.
    pub(super) fn verify_in(&self, parent: &Folder) -> Result<(), EngineError> {
        self.directory.verify(parent, OsStr::new(&self.name))
    }

    /// Removes this staging directory and the regular files it holds. Input: the parent.
    /// Output: Ok once the parent no longer names it; on a failure it stays for the next
    /// sweep.
    pub(super) fn discard(self, parent: &Folder) -> Result<(), EngineError> {
        self.verify_in(parent)?;
        remove_tree(&self.directory.folder, 0)?;
        let name = held_name(OsStr::new(&self.name))?;
        let pin = self.directory.folder.pin();
        drop(self);
        remove_entry(parent, &name, pin)?;
        parent
            .sync()
            .map_err(|_| failure("native_profile_custody_unsafe"))
    }
}

/// Removes every abandoned staging directory with the given prefix from a parent.
/// Inputs: the parent and the prefix. Output: Ok after removing each one whose creator
/// no longer claims it (an exclusive `flock` on Unix, a held handle on Windows); a live one
/// is left alone, and one that is not a private directory of this user, or holds anything a
/// staged change never writes, is refused.
pub(super) fn sweep(parent: &Folder, prefix: &str) -> Result<(), EngineError> {
    let names = parent
        .names(
            |name| is_staging(name.as_encoded_bytes(), prefix),
            STAGING_MAX,
        )
        .map_err(|error| match error {
            HeldError::Full => failure("native_profile_capacity"),
            _ => failure("native_profile_custody_unsafe"),
        })?;
    for name in names {
        let name = held_name(&name)?;
        #[cfg(windows)]
        let claimed = match parent.claim_entry(&name) {
            Ok(claimed) => claimed,
            Err(HeldError::InUse | HeldError::Missing) => continue,
            Err(_) => return Err(failure("native_profile_custody_unsafe")),
        };
        let staged = Directory::open(parent, name.as_os_str())?;
        #[cfg(unix)]
        let _claimed = {
            let held = staged.file().try_clone().map_err(unsafe_custody)?;
            match HeldLock::take(held, FlockOperation::NonBlockingLockExclusive) {
                Ok(held) => held,
                Err(rustix::io::Errno::WOULDBLOCK) => continue,
                Err(error) => return Err(unsafe_custody(error)),
            }
        };
        #[cfg(windows)]
        if crazyagent_win_sandbox::private::identity(&claimed).map_err(unsafe_custody)?
            != crazyagent_win_sandbox::private::identity(staged.file()).map_err(unsafe_custody)?
        {
            return Err(failure("native_profile_custody_unsafe"));
        }
        staged.verify(parent, name.as_os_str())?;
        remove_tree(&staged.folder, 1)?;
        let pin = staged.folder.pin();
        drop(staged);
        #[cfg(windows)]
        drop(claimed);
        remove_entry(parent, &name, pin)?;
    }
    parent
        .sync()
        .map_err(|_| failure("native_profile_custody_unsafe"))
}

/// Removes one pinned entry of a held folder through the layer. Inputs: the folder, the
/// name and the pin. Output: Ok, or `native_profile_custody_unsafe`.
fn remove_entry(parent: &Folder, name: &Name, pin: Pin) -> Result<(), EngineError> {
    assert_ne!(pin.inode, 0);
    parent
        .remove(name, pin)
        .map_err(|_| failure("native_profile_custody_unsafe"))
}

/// Removes the contents of an engine-made directory: private files of this user and,
/// while depth remains, private subdirectories holding the same, each opened, pinned and
/// removed through the held layer. Inputs: the held directory and the depth left. Output:
/// Ok once it is empty; anything else is refused with `native_profile_custody_unsafe` and
/// left in place.
pub(super) fn remove_tree(directory: &Folder, depth: usize) -> Result<(), EngineError> {
    let unsafe_tree = |_| failure("native_profile_custody_unsafe");
    let children = directory.list(TREE_ENTRIES_MAX).map_err(unsafe_tree)?;
    assert!(children.len() <= TREE_ENTRIES_MAX);
    for child in children {
        let name = held_name(&child.name)?;
        let pin = match child.kind {
            Kind::File => directory.open_read(&name).map_err(unsafe_tree)?.1,
            Kind::Folder if depth > 0 => {
                let folder = directory.open_dir(&name).map_err(unsafe_tree)?;
                remove_tree(&folder, depth - 1)?;
                folder.pin()
            }
            Kind::Folder | Kind::Link => return Err(failure("native_profile_custody_unsafe")),
        };
        remove_entry(directory, &name, pin)?;
    }
    directory.sync().map_err(unsafe_tree)
}
