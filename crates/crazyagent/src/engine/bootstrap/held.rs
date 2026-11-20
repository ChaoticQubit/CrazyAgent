use crate::engine::EngineError;
use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

const UNSAFE: &str = "native_profile_custody_unsafe";
const INCOMPLETE: &str = "native_profile_incomplete";
const IN_USE: &str = "native_profile_in_use";
const LOCK_UNAVAILABLE: &str = "native_profile_lock_unavailable";
const NAME_BYTES_MAX: usize = 255;
const DEVICE_NAMES: [&str; 4] = ["AUX", "CON", "NUL", "PRN"];
const NUMBERED_DEVICES: [&str; 2] = ["COM", "LPT"];

/// One file's identity, compared to tell a swapped name from the held entry: device and
/// inode on Unix, volume serial number and file id on Windows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Pin {
    pub(super) device: u64,
    pub(super) inode: u64,
}

/// A folder native profile custody holds open and opens its children through, so a name
/// is always looked up in the held folder, never through a link (DESIGN-0515 D1). On
/// Windows it also keeps its own plain place, read from the handle, never a caller's path,
/// and binds every child to it (rv-sec3 #716 C1).
pub(super) struct Folder {
    file: File,
    pin: Pin,
    #[cfg(windows)]
    path: PathBuf,
}

fn failure(code: &str) -> EngineError {
    EngineError(code.to_owned())
}

fn unsafe_custody(_: impl std::fmt::Display) -> EngineError {
    failure(UNSAFE)
}

/// Asserts that a custody name is one plain component on every system (rv-sec3 #716 C3):
/// ASCII letters, digits, `.`, `_` and `-` only, so no separator, colon or stream, not `.`
/// or `..`, no trailing dot, and no Windows device name. Every custody name is a constant or
/// a lowercase hex key, so a failure is a programming error. Input: the name.
fn assert_component(name: &OsStr) {
    let text = name.to_str().expect("a custody name is UTF-8");
    assert!(!text.is_empty());
    assert!(text.len() <= NAME_BYTES_MAX, "{text:?}");
    assert!(
        text.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')),
        "{text:?}"
    );
    assert!(!text.ends_with('.'), "{text:?}");
    let stem = text
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    assert!(!DEVICE_NAMES.contains(&stem.as_str()), "{text:?}");
    let numbered = NUMBERED_DEVICES.iter().any(|device| {
        stem.len() == 4 && stem.starts_with(device) && stem.as_bytes()[3].is_ascii_digit()
    });
    assert!(!numbered, "{text:?}");
}

impl Folder {
    /// The held folder's identity. Output: the pin.
    pub(super) fn pin(&self) -> Pin {
        self.pin
    }

    /// The held folder's handle. Output: the file.
    pub(super) fn file(&self) -> &File {
        &self.file
    }
}

#[cfg(unix)]
impl Folder {
    /// Holds the first folder of a chain, opened without following a link and owned by the
    /// engine's user with mode 0700. Input: its absolute path. Output: the held folder,
    /// `native_profile_incomplete` when it is missing, or `native_profile_custody_unsafe`.
    pub(super) fn root(path: &Path) -> Result<Self, EngineError> {
        use rustix::fs::{Mode, OFlags};
        assert!(path.is_absolute(), "{}", path.display());
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(rustix::fs::open(path, flags, Mode::empty()).map_err(opened)?);
        if !super::safe_directory(&file.metadata().map_err(unsafe_custody)?) {
            return Err(failure(UNSAFE));
        }
        let pin = pin_of(&file)?;
        Ok(Self { file, pin })
    }

    /// Opens and holds one child folder through this folder, never following a link.
    /// Input: its name. Output: the held child, `native_profile_incomplete` when it is
    /// missing, or `native_profile_custody_unsafe`.
    pub(super) fn open_dir(&self, name: &OsStr) -> Result<Self, EngineError> {
        use rustix::fs::{Mode, OFlags};
        assert_component(name);
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file =
            File::from(rustix::fs::openat(&self.file, name, flags, Mode::empty()).map_err(opened)?);
        let pin = pin_of(&file)?;
        self.verify(name, &file, pin, true)?;
        Ok(Self { file, pin })
    }

    /// Makes one child folder with mode 0700 through this folder, create-only, so an entry
    /// already at the name is unsafe (rv-sec3 #716 C2), then holds it. Input: its name.
    /// Output: the held child, or `native_profile_custody_unsafe`.
    pub(super) fn make_dir(&self, name: &OsStr) -> Result<Self, EngineError> {
        use rustix::fs::Mode;
        assert_component(name);
        rustix::fs::mkdirat(&self.file, name, Mode::from_raw_mode(0o700))
            .map_err(unsafe_custody)?;
        self.file.sync_all().map_err(unsafe_custody)?;
        self.open_dir(name)
    }

    /// Opens one child file for reading through this folder, never following a link.
    /// Input: its name. Output: the file and its identity, `native_profile_incomplete` when
    /// it is missing, or `native_profile_custody_unsafe`.
    pub(super) fn open_read(&self, name: &OsStr) -> Result<(File, Pin), EngineError> {
        use rustix::fs::{Mode, OFlags};
        assert_component(name);
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file =
            File::from(rustix::fs::openat(&self.file, name, flags, Mode::empty()).map_err(opened)?);
        let pin = pin_of(&file)?;
        self.verify(name, &file, pin, false)?;
        Ok((file, pin))
    }

    /// Checks that a name in this folder still names the held entry: the entry is owned by
    /// the engine's user with a private mode, a file has one link, its identity is the pin,
    /// and a no-follow look at the name gives the same inode, device, owner, mode and links.
    /// Inputs: the name, the held entry, its pin and whether it is a folder. Output: Ok, or
    /// `native_profile_custody_unsafe`.
    pub(super) fn verify(
        &self,
        name: &OsStr,
        held: &File,
        pin: Pin,
        directory: bool,
    ) -> Result<(), EngineError> {
        use rustix::fs::AtFlags;
        use std::os::unix::fs::MetadataExt;
        assert_component(name);
        let metadata = held.metadata().map_err(unsafe_custody)?;
        let named = rustix::fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(unsafe_custody)?;
        let safe = if directory {
            super::safe_directory(&metadata)
        } else {
            super::safe_file(&metadata)
        };
        let held_pin = Pin {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if !safe
            || pin.inode == 0
            || held_pin != pin
            || named.st_ino != metadata.ino()
            || named.st_dev as u64 != metadata.dev()
            || named.st_uid != metadata.uid()
            || named.st_mode as u32 != metadata.mode()
            || (!directory && named.st_nlink as u64 != metadata.nlink())
        {
            return Err(failure(UNSAFE));
        }
        Ok(())
    }
}

/// Maps a failed open: a missing entry is `native_profile_incomplete`, anything else, a
/// link or a file where a folder belongs among them, is `native_profile_custody_unsafe`.
#[cfg(unix)]
fn opened(error: rustix::io::Errno) -> EngineError {
    if error == rustix::io::Errno::NOENT {
        failure(INCOMPLETE)
    } else {
        failure(UNSAFE)
    }
}

/// Reads an open entry's device and inode. Input: the handle. Output: the pin, or
/// `native_profile_custody_unsafe`.
#[cfg(unix)]
fn pin_of(file: &File) -> Result<Pin, EngineError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(unsafe_custody)?;
    Ok(Pin {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
impl Folder {
    /// Holds the first folder of a chain with W1, never creating it, never following a link
    /// at its name, and never sharing delete, so it cannot be renamed while held. The path
    /// must be the plain spelling (leader 05:2x, LIMITS line 15). The folder keeps the place
    /// read from its handle, not this path (rv-sec3 #716 C1). Input: its absolute plain
    /// path. Output: the held folder, `native_profile_incomplete` when it is missing, or
    /// `native_profile_custody_unsafe`.
    pub(super) fn root(path: &Path) -> Result<Self, EngineError> {
        assert!(plain_drive(path), "{}", path.display());
        let file = crazyagent_win_sandbox::private::open_dir(path).map_err(opened)?;
        let place = place_of(&file)?;
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            path: place,
        })
    }

    /// Opens and holds one child folder at this folder's place with W1, never creating it,
    /// then binds it to this folder. Input: its name. Output: the held child,
    /// `native_profile_incomplete` when it is missing, or `native_profile_custody_unsafe`.
    pub(super) fn open_dir(&self, name: &OsStr) -> Result<Self, EngineError> {
        assert_component(name);
        let path = self.path.join(name);
        let file = crazyagent_win_sandbox::private::open_dir(&path).map_err(opened)?;
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok(Self { file, pin, path })
    }

    /// Makes one private child folder with W1, create-only, so an entry already at the name
    /// is unsafe (rv-sec3 #716 C2), writes this folder's entries to disk, then binds the
    /// child to this folder. Input: its name. Output: the held child, or
    /// `native_profile_custody_unsafe`.
    pub(super) fn make_dir(&self, name: &OsStr) -> Result<Self, EngineError> {
        use crazyagent_win_sandbox::private;
        assert_component(name);
        let path = self.path.join(name);
        let file = private::new_dir(&path).map_err(unsafe_custody)?;
        private::sync_folder(&self.path).map_err(unsafe_custody)?;
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok(Self { file, pin, path })
    }

    /// Opens one child file for reading with W1, which refuses a link, a second name and any
    /// list other than the private one, then binds it to this folder. Input: its name.
    /// Output: the file and its identity, `native_profile_incomplete` when it is missing, or
    /// `native_profile_custody_unsafe`.
    pub(super) fn open_read(&self, name: &OsStr) -> Result<(File, Pin), EngineError> {
        assert_component(name);
        let path = self.path.join(name);
        let file = crazyagent_win_sandbox::private::read_only(&path).map_err(opened)?;
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok((file, pin))
    }

    /// Checks that a name in this folder still names the held entry (DESIGN-0515 D2): the
    /// name is opened again with W1 and bound to this folder, it is the same file as the held
    /// entry, the held entry passes W1's check, links included, and its identity is the pin.
    /// Inputs: the name, the held entry, its pin and whether it is a folder. Output: Ok, or
    /// `native_profile_custody_unsafe`.
    pub(super) fn verify(
        &self,
        name: &OsStr,
        held: &File,
        pin: Pin,
        directory: bool,
    ) -> Result<(), EngineError> {
        use crazyagent_win_sandbox::private;
        assert_component(name);
        let path = self.path.join(name);
        let reopened = if directory {
            private::open_dir(&path)
        } else {
            private::read_only(&path)
        }
        .map_err(unsafe_custody)?;
        self.bind(name, &reopened)?;
        let same = private::same(held, &reopened).map_err(unsafe_custody)?;
        private::check(held).map_err(unsafe_custody)?;
        let held_pin = pin_of(held)?;
        if pin.inode == 0 || held_pin != pin || !same {
            return Err(failure(UNSAFE));
        }
        Ok(())
    }

    /// Binds a child opened at this folder's place to this folder (rv-sec3 #716 C1): this
    /// folder's place, read from its handle again now, must still be the one it was held
    /// at, and the child's place, read from its handle, must be that place joined with the
    /// name. Inputs: the name and the child's handle. Output: Ok, or
    /// `native_profile_custody_unsafe`.
    fn bind(&self, name: &OsStr, child: &File) -> Result<(), EngineError> {
        assert!(plain_drive(&self.path), "{}", self.path.display());
        let parent = place_of(&self.file)?;
        let named = place_of(child)?;
        if parent != self.path || named != self.path.join(name) {
            return Err(failure(UNSAFE));
        }
        Ok(())
    }
}

/// Says whether a path is the plain drive spelling, such as `C:\work`: absolute, with a
/// drive prefix, never `\\?\` or a share (leader 05:2x, LIMITS line 15). Input: the path.
/// Output: the answer.
#[cfg(windows)]
fn plain_drive(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    let drive = matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
    );
    drive && path.is_absolute()
}

/// Reads where an open entry really is from its handle with W1 `place`, in the plain
/// spelling: the one `\\?\` drive prefix W1 gives is dropped, and any other form, a share
/// among them, is unsafe. The local strip goes once W3-4a's `one_spelling` arm lands
/// (DESIGN-0515 D6). Input: the handle. Output: the plain place, or
/// `native_profile_custody_unsafe`.
#[cfg(windows)]
fn place_of(file: &File) -> Result<PathBuf, EngineError> {
    use std::os::windows::ffi::OsStringExt;
    let units = crazyagent_win_sandbox::private::place(file).map_err(unsafe_custody)?;
    let verbatim = std::ffi::OsString::from_wide(&units);
    let plain = verbatim
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .map(PathBuf::from)
        .ok_or_else(|| failure(UNSAFE))?;
    if !plain_drive(&plain) {
        return Err(failure(UNSAFE));
    }
    Ok(plain)
}

/// Maps a failed W1 open: a missing entry is `native_profile_incomplete`, anything else, a
/// link or an entry W1 does not judge private among them, is `native_profile_custody_unsafe`.
#[cfg(windows)]
fn opened(error: std::io::Error) -> EngineError {
    if error.kind() == std::io::ErrorKind::NotFound {
        failure(INCOMPLETE)
    } else {
        failure(UNSAFE)
    }
}

/// Reads an open entry's volume serial number and 64-bit file id with W1 `identity`.
/// Input: the handle. Output: the pin, or `native_profile_custody_unsafe`.
#[cfg(windows)]
fn pin_of(file: &File) -> Result<Pin, EngineError> {
    let (device, inode) =
        crazyagent_win_sandbox::private::identity(file).map_err(unsafe_custody)?;
    Ok(Pin { device, inode })
}

/// Takes one lock on a held file without waiting (DESIGN-0515 D4): `flock` on Unix; on
/// Windows a shared lock is std's whole-file shared lock and an exclusive one W1's, both
/// mandatory, so an exclusive lock also refuses other handles' reads. Inputs: the file and
/// whether the lock is shared. Output: Ok, `native_profile_in_use` while a conflicting lock
/// is held, or `native_profile_lock_unavailable`.
pub(super) fn lock(file: &File, shared: bool) -> Result<(), EngineError> {
    #[cfg(unix)]
    let taken = {
        use rustix::fs::FlockOperation;
        let operation = if shared {
            FlockOperation::NonBlockingLockShared
        } else {
            FlockOperation::NonBlockingLockExclusive
        };
        rustix::fs::flock(file, operation).map_err(|error| {
            error == rustix::io::Errno::WOULDBLOCK || error == rustix::io::Errno::AGAIN
        })
    };
    #[cfg(windows)]
    let taken = if shared {
        file.try_lock_shared()
            .map_err(|error| matches!(error, std::fs::TryLockError::WouldBlock))
    } else {
        crazyagent_win_sandbox::private::lock(file)
            .map_err(|error| error.raw_os_error() == Some(LOCK_VIOLATION))
    };
    taken.map_err(|busy| failure(if busy { IN_USE } else { LOCK_UNAVAILABLE }))
}

/// The Windows error a lock held by another handle gives, `ERROR_LOCK_VIOLATION`.
#[cfg(windows)]
const LOCK_VIOLATION: i32 = 33;

/// Releases a lock this handle took, now and not at the handle's close. A failed release
/// is not reported: the lock still ends when the handle closes, as the custody guards'
/// own unlocks already do on Unix. Input: the file.
pub(super) fn unlock(file: &File) {
    #[cfg(unix)]
    let _ = rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock);
    #[cfg(windows)]
    let _ = crazyagent_win_sandbox::private::unlock(file);
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "held_probe_tests.rs"]
mod probes;
