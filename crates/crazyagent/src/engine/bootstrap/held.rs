use crazyagent_win_sandbox::name;
#[cfg(windows)]
use crazyagent_win_sandbox::private;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

const NAME_BYTES_MAX: usize = 255;
const DEVICE_NAMES: [&str; 4] = ["AUX", "CON", "NUL", "PRN"];

/// Why a held open, make, listing, lock or remove failed, by cause (rv-sec3 #725 U5): the
/// entry is missing, it is not the entry the layer may use, another handle holds it, the
/// system refused for another reason, or a listing passed its bound. Each caller maps these
/// kinds to its own codes, so no caller's code lives in the layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeldError {
    Missing,
    Unsafe,
    InUse,
    Unavailable,
    Full,
}

/// How the root of a held chain was judged, inherited by every folder opened through it
/// (rv-sec3 #725 U1): `Private` by W1's private judge on Windows and the 0700 owner check
/// on Unix, `Caller` by a judge the caller named. Unjudged calls run only under `Caller`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Judged {
    Private,
    Caller,
}

/// One file's identity, compared to tell a swapped name from the held entry: device and
/// inode on Unix, volume serial number and file id on Windows.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pin {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

/// One plain component name every held call takes (DESIGN-0553 rule 3): made by
/// `Name::constant` for the layer's callers' own names, which asserts, or by `Name::checked`
/// for a name read off disk or from another program, which refuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Name(OsString);

/// What one entry of a held folder is, read without following it: a file, a folder, or a
/// link, which is any reparse point on Windows and a symbolic link on Unix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    File,
    Folder,
    Link,
}

/// One entry a listing gives: its name as the folder holds it, not yet checked, and its kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Child {
    pub(crate) name: OsString,
    pub(crate) kind: Kind,
}

/// A folder the layer holds open and opens its children through, so a name is always looked
/// up in the held folder, never through a link (DESIGN-0515 D1). On Windows it also keeps
/// its own plain place, read from the handle, never a caller's path, and binds every child
/// to it (rv-sec3 #716 C1). It carries how its root was judged (rv-sec3 #725 U1).
#[derive(Debug)]
pub(crate) struct Folder {
    file: File,
    #[cfg(test)]
    pin: Pin,
    judged: Judged,
    #[cfg(windows)]
    path: PathBuf,
}

impl Name {
    /// Makes a name from a constant the caller wrote, asserting it is one plain component
    /// on every system (rv-sec3 #716 C3), so a bad constant is a programming error. Input:
    /// the text. Output: the name.
    pub(crate) fn constant(text: &'static str) -> Self {
        assert_component(text);
        assert!(!name::refused(text), "{text:?}");
        Self(OsString::from(text))
    }

    /// Checks a name the layer's caller did not write, the same on every system (DESIGN-0553
    /// rule 3, rv-sec3 #725 U6). Input: the name. Output: the name, or `Unsafe` for one that
    /// is not UTF-8 or that [`name::refused`] turns away.
    pub(crate) fn checked(text: &OsStr) -> Result<Self, HeldError> {
        let plain = text.to_str().ok_or(HeldError::Unsafe)?;
        if name::refused(plain) {
            return Err(HeldError::Unsafe);
        }
        Ok(Self(text.to_owned()))
    }

    /// The name as the system takes it. Output: the text.
    pub(crate) fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}

/// Asserts that a constant name is one plain component on every system (rv-sec3 #716 C3):
/// ASCII letters, digits, `.`, `_` and `-` only, so no separator, colon or stream, not `.`
/// or `..`, no trailing dot, and no Windows device name. Input: the name.
fn assert_component(text: &str) {
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
    let numbered = name::NUMBERED_DEVICES.iter().any(|device| {
        stem.len() == 4 && stem.starts_with(device) && stem.as_bytes()[3].is_ascii_digit()
    });
    assert!(!numbered, "{text:?}");
}

impl Folder {
    /// The held folder's identity. Output: the pin.
    #[cfg(test)]
    pub(crate) fn pin(&self) -> Pin {
        self.pin
    }

    /// The held folder's handle. Output: the file.
    pub(crate) fn file(&self) -> &File {
        &self.file
    }

    /// Asserts that this folder's root was judged by its caller (rv-sec3 #725 U1), the
    /// first line of every unjudged call, so none ever runs on a tree held through
    /// `Folder::root`'s private judge.
    pub(crate) fn assert_caller_judged(&self) {
        assert_ne!(
            self.judged,
            Judged::Private,
            "an unjudged call on a private tree"
        );
        assert_eq!(self.judged, Judged::Caller);
    }
}

#[cfg(unix)]
impl Folder {
    /// Holds the first folder of a chain, opened without following a link and owned by the
    /// engine's user with mode 0700. Input: its absolute path. Output: the held folder,
    /// `Missing`, or `Unsafe`.
    pub(crate) fn root(path: &Path) -> Result<Self, HeldError> {
        use rustix::fs::{Mode, OFlags};
        assert!(path.is_absolute(), "{}", path.display());
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(rustix::fs::open(path, flags, Mode::empty()).map_err(errno_cause)?);
        if !super::safe_directory(&file.metadata().map_err(cause)?) {
            return Err(HeldError::Unsafe);
        }
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            judged: Judged::Private,
        })
    }

    /// Opens and holds one child folder through this folder, never following a link.
    /// Input: its name. Output: the held child, `Missing`, or `Unsafe`.
    pub(crate) fn open_dir(&self, name: &Name) -> Result<Self, HeldError> {
        use rustix::fs::{Mode, OFlags};
        let file = self.open_at(name, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
        let pin = pin_of(&file)?;
        self.verify(name, &file, pin, true)?;
        Ok(Self {
            file,
            pin,
            judged: self.judged,
        })
    }

    /// Makes one child folder with mode 0700 through this folder, create-only, so an entry
    /// already at the name is `Unsafe` (rv-sec3 #716 C2), then holds it. Input: its name.
    /// Output: the held child, or the error.
    pub(crate) fn make_dir(&self, name: &Name) -> Result<Self, HeldError> {
        use rustix::fs::Mode;
        rustix::fs::mkdirat(&self.file, name.as_os_str(), Mode::from_raw_mode(0o700))
            .map_err(errno_cause)?;
        self.file.sync_all().map_err(cause)?;
        self.open_dir(name)
    }

    /// Opens one child file for reading through this folder, never following a link and
    /// never waiting, so a FIFO at the name is refused rather than blocking (rv-sec3 #731
    /// F1). Input: its name. Output: the file and its identity, `Missing`, or `Unsafe`.
    pub(crate) fn open_read(&self, name: &Name) -> Result<(File, Pin), HeldError> {
        use rustix::fs::{Mode, OFlags};
        self.open_file(name, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())
    }

    /// Makes one child file with mode 0600 through this folder, create-only, the twin of
    /// `O_CREAT | O_EXCL`, so an entry already at the name is `Unsafe`, then writes this
    /// folder's entries to disk. Input: its name. Output: the file and its identity.
    pub(crate) fn make_file(&self, name: &Name) -> Result<(File, Pin), HeldError> {
        use rustix::fs::{Mode, OFlags};
        let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL;
        let made = self.open_file(name, flags, Mode::from_raw_mode(0o600))?;
        self.file.sync_all().map_err(cause)?;
        Ok(made)
    }

    /// Opens one child file for reading and writing through this folder, never following a
    /// link and never waiting (rv-sec3 #731 F1). Input: its name. Output: the file and its
    /// identity, `Missing`, or `Unsafe`.
    pub(crate) fn open_write(&self, name: &Name) -> Result<(File, Pin), HeldError> {
        use rustix::fs::{Mode, OFlags};
        self.open_file(name, OFlags::RDWR | OFlags::NONBLOCK, Mode::empty())
    }

    /// Lists this folder from its held descriptor, never by path, skipping `.` and `..` and
    /// an entry gone meanwhile; the kind comes from a no-follow look at each name. Input:
    /// the most entries allowed. Output: the entries, `Full` past the bound, or `Unsafe`
    /// for an entry that is none of a file, a folder or a link.
    pub(crate) fn list(&self, limit_count: usize) -> Result<Vec<Child>, HeldError> {
        use rustix::fs::{AtFlags, Dir, FileType};
        use std::os::unix::ffi::OsStrExt;
        assert!(limit_count > 0);
        let mut children = Vec::new();
        for entry in Dir::read_from(&self.file).map_err(errno_cause)? {
            let entry = entry.map_err(errno_cause)?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = OsStr::from_bytes(bytes);
            let stat = match rustix::fs::statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                Err(error) if error == rustix::io::Errno::NOENT => continue,
                Err(error) => return Err(errno_cause(error)),
            };
            let kind = match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => Kind::File,
                FileType::Directory => Kind::Folder,
                FileType::Symlink => Kind::Link,
                _ => return Err(HeldError::Unsafe),
            };
            if children.len() == limit_count {
                return Err(HeldError::Full);
            }
            children.push(Child {
                name: name.to_owned(),
                kind,
            });
        }
        assert!(children.len() <= limit_count);
        Ok(children)
    }

    /// Removes one entry through this folder while it is still the pinned, private entry:
    /// opened without following a link, judged and compared with the pin by `verify`, then
    /// unlinked by name, as the custody removes it replaces are. Inputs: its name and pin.
    /// Output: Ok, `Missing`, or `Unsafe` for a link or a swapped or unsafe entry.
    pub(crate) fn remove(&self, name: &Name, pin: Pin) -> Result<(), HeldError> {
        use rustix::fs::{AtFlags, Mode, OFlags};
        let entry = self.open_at(name, OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())?;
        let directory = entry.metadata().map_err(cause)?.is_dir();
        self.verify(name, &entry, pin, directory)?;
        let flags = if directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        rustix::fs::unlinkat(&self.file, name.as_os_str(), flags).map_err(errno_cause)
    }

    /// Checks that a name in this folder still names the held entry: the entry is owned by
    /// the engine's user with a private mode, a file has one link, its identity is the pin,
    /// and a no-follow look at the name gives the same inode, device, owner, mode and links.
    /// Inputs: the name, the held entry, its pin and whether it is a folder. Output: Ok, or
    /// the error, `Unsafe` for any mismatch.
    pub(crate) fn verify(
        &self,
        name: &Name,
        held: &File,
        pin: Pin,
        directory: bool,
    ) -> Result<(), HeldError> {
        use rustix::fs::AtFlags;
        use std::os::unix::fs::MetadataExt;
        let metadata = held.metadata().map_err(cause)?;
        let named = rustix::fs::statat(&self.file, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(errno_cause)?;
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
            return Err(HeldError::Unsafe);
        }
        Ok(())
    }

    /// Opens one child file through this folder with the given access and creation flags,
    /// then pins and verifies it. Inputs: its name, the flags and the creation mode. Output:
    /// the file and its identity, or the error.
    fn open_file(
        &self,
        name: &Name,
        flags: rustix::fs::OFlags,
        mode: rustix::fs::Mode,
    ) -> Result<(File, Pin), HeldError> {
        let file = self.open_at(name, flags, mode)?;
        let pin = pin_of(&file)?;
        self.verify(name, &file, pin, false)?;
        Ok((file, pin))
    }

    /// Opens one name through this folder's descriptor, never following a link at it and
    /// never inherited by a child process. Inputs: its name, the flags and the creation
    /// mode. Output: the open entry, or the error by cause.
    fn open_at(
        &self,
        name: &Name,
        flags: rustix::fs::OFlags,
        mode: rustix::fs::Mode,
    ) -> Result<File, HeldError> {
        use rustix::fs::OFlags;
        let flags = flags | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        rustix::fs::openat(&self.file, name.as_os_str(), flags, mode)
            .map(File::from)
            .map_err(errno_cause)
    }
}

/// Maps a failed Unix call by cause (rv-sec3 #725 U5): a missing entry is `Missing`; an
/// entry already there, a link at a no-follow name and a file or folder where the other
/// belongs are `Unsafe`; every other error is `Unavailable`. Input: the errno. Output: the
/// kind.
#[cfg(unix)]
fn errno_cause(error: rustix::io::Errno) -> HeldError {
    use rustix::io::Errno;
    if error == Errno::NOENT {
        return HeldError::Missing;
    }
    let tampered = [Errno::EXIST, Errno::LOOP, Errno::NOTDIR, Errno::ISDIR];
    if tampered.contains(&error) {
        HeldError::Unsafe
    } else {
        HeldError::Unavailable
    }
}

/// Maps a failed std call on Unix the way `errno_cause` maps a rustix one. Input: the error.
/// Output: the kind, `Unavailable` for an error with no errno.
#[cfg(unix)]
fn cause(error: std::io::Error) -> HeldError {
    error.raw_os_error().map_or(HeldError::Unavailable, |raw| {
        errno_cause(rustix::io::Errno::from_raw_os_error(raw))
    })
}

/// Reads an open entry's device and inode. Input: the handle. Output: the pin, or the
/// error.
#[cfg(all(unix, test))]
fn pin_of(file: &File) -> Result<Pin, HeldError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(cause)?;
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
    /// path. Output: the held folder, `Missing`, or `Unsafe`.
    #[cfg(test)]
    pub(crate) fn root(path: &Path) -> Result<Self, HeldError> {
        assert!(plain_drive(path), "{}", path.display());
        let file = private::open_dir(path).map_err(cause)?;
        let place = place_of(&file)?;
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            judged: Judged::Private,
            path: place,
        })
    }

    /// Opens and holds one child folder at this folder's place with W1, never creating it,
    /// then binds it to this folder. Input: its name. Output: the held child, `Missing`, or
    /// `Unsafe`.
    #[cfg(test)]
    pub(crate) fn open_dir(&self, name: &Name) -> Result<Self, HeldError> {
        let path = self.path.join(name.as_os_str());
        let file = private::open_dir(&path).map_err(cause)?;
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            judged: self.judged,
            path,
        })
    }

    /// Makes one private child folder with W1, create-only, so an entry already at the name
    /// is `Unsafe` (rv-sec3 #716 C2), writes this folder's entries to disk, then binds the
    /// child to this folder. Input: its name. Output: the held child, or the error.
    #[cfg(test)]
    pub(crate) fn make_dir(&self, name: &Name) -> Result<Self, HeldError> {
        let path = self.path.join(name.as_os_str());
        let file = private::new_dir(&path).map_err(|error| self.made_cause(name, error))?;
        private::sync_folder(&self.path).map_err(cause)?;
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            judged: self.judged,
            path,
        })
    }

    /// Opens one child file for reading with W1, which refuses a link, a second name and any
    /// list other than the private one, then binds it to this folder. Input: its name.
    /// Output: the file and its identity, `Missing`, `InUse`, or `Unsafe`.
    #[cfg(test)]
    pub(crate) fn open_read(&self, name: &Name) -> Result<(File, Pin), HeldError> {
        let path = self.path.join(name.as_os_str());
        let file = private::read_only(&path).map_err(cause)?;
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok((file, pin))
    }

    /// Makes one private child file with W1, create-only, so an entry already at the name is
    /// `Unsafe`, writes this folder's entries to disk, then binds the file to this folder.
    /// Input: its name. Output: the file, open for reading and writing, and its identity.
    #[cfg(test)]
    pub(crate) fn make_file(&self, name: &Name) -> Result<(File, Pin), HeldError> {
        self.private_file(name, true)
    }

    /// Opens one private child file for reading and writing with W1, which refuses a link,
    /// a second name and any list other than the private one, then binds it to this folder.
    /// Input: its name. Output: the file and its identity, `Missing`, `InUse`, or `Unsafe`.
    #[cfg(test)]
    pub(crate) fn open_write(&self, name: &Name) -> Result<(File, Pin), HeldError> {
        self.private_file(name, false)
    }

    /// Lists this folder by its bound place (rv-sec3 #725 Q2). Before and after the pass
    /// the held handle must show no reparse point and the same place, so a folder turned
    /// into a junction meanwhile is `Unsafe` (U4). The kind comes from the find data,
    /// never followed. Input: the most entries allowed. Output: the entries, or `Full`.
    pub(crate) fn list(&self, limit_count: usize) -> Result<Vec<Child>, HeldError> {
        use std::os::windows::fs::MetadataExt;
        assert!(limit_count > 0);
        self.unmoved()?;
        let mut children = Vec::new();
        for entry in std::fs::read_dir(&self.path).map_err(cause)? {
            let entry = entry.map_err(cause)?;
            let attributes = entry.metadata().map_err(cause)?.file_attributes();
            if children.len() == limit_count {
                return Err(HeldError::Full);
            }
            children.push(Child {
                name: entry.file_name(),
                kind: kind_of(attributes),
            });
        }
        self.unmoved()?;
        assert!(children.len() <= limit_count);
        Ok(children)
    }

    /// Removes one entry while it is still the pinned, private entry: opened as itself with
    /// delete access, never sharing delete, bound to this folder, compared with the pin and
    /// judged by W1, then deleted through that handle with POSIX semantics. Inputs: its name
    /// and pin. Output: Ok, `Missing`, `InUse` while another handle holds it, or `Unsafe`.
    #[cfg(test)]
    pub(crate) fn remove(&self, name: &Name, pin: Pin) -> Result<(), HeldError> {
        use crate::platform::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
        use std::os::windows::fs::OpenOptionsExt;
        let entry = std::fs::OpenOptions::new()
            .access_mode(DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(name.as_os_str()))
            .map_err(cause)?;
        self.bind(name, &entry)?;
        if pin.inode == 0 || pin_of(&entry)? != pin {
            return Err(HeldError::Unsafe);
        }
        private::check(&entry).map_err(cause)?;
        private::delete(&entry).map_err(cause)
    }

    /// Checks that a name in this folder still names the held entry (DESIGN-0515 D2): the
    /// name is opened again with W1 and bound to this folder, it is the same file as the held
    /// entry, the held entry passes W1's check, links included, and its identity is the pin.
    /// Inputs: the name, the held entry, its pin and whether it is a folder. Output: Ok, or
    /// the error, `Unsafe` for any mismatch.
    #[cfg(test)]
    pub(crate) fn verify(
        &self,
        name: &Name,
        held: &File,
        pin: Pin,
        directory: bool,
    ) -> Result<(), HeldError> {
        let path = self.path.join(name.as_os_str());
        let reopened = if directory {
            private::open_dir(&path)
        } else {
            private::read_only(&path)
        }
        .map_err(cause)?;
        self.bind(name, &reopened)?;
        let same = private::same(held, &reopened).map_err(cause)?;
        private::check(held).map_err(cause)?;
        let held_pin = pin_of(held)?;
        if pin.inode == 0 || held_pin != pin || !same {
            return Err(HeldError::Unsafe);
        }
        Ok(())
    }

    /// Makes or opens one private child file with W1 for reading and writing, then binds
    /// it to this folder; a make writes this folder's entries to disk first. Inputs: its
    /// name and whether to create it. Output: the file and its identity, or the error.
    #[cfg(test)]
    fn private_file(&self, name: &Name, create: bool) -> Result<(File, Pin), HeldError> {
        let path = self.path.join(name.as_os_str());
        let file = private::file(&path, create).map_err(|error| {
            if create {
                self.made_cause(name, error)
            } else {
                cause(error)
            }
        })?;
        if create {
            private::sync_folder(&self.path).map_err(cause)?;
        }
        self.bind(name, &file)?;
        let pin = pin_of(&file)?;
        Ok((file, pin))
    }

    /// Binds a child opened at this folder's place to this folder (rv-sec3 #716 C1): this
    /// folder's place, read from its handle again now, must still be the one it was held
    /// at, and the child's place, read from its handle, must be that place joined with the
    /// name. Inputs: the name and the child's handle. Output: Ok, or `Unsafe`.
    fn bind(&self, name: &Name, child: &File) -> Result<(), HeldError> {
        assert!(plain_drive(&self.path), "{}", self.path.display());
        let parent = place_of(&self.file)?;
        let named = place_of(child)?;
        if parent != self.path || named != self.path.join(name.as_os_str()) {
            return Err(HeldError::Unsafe);
        }
        Ok(())
    }

    /// Checks that the held folder is still a plain folder at its place (rv-sec3 #725 U4):
    /// no reparse point on the held handle, and the place read from it unchanged. Output:
    /// Ok, or `Unsafe`.
    fn unmoved(&self) -> Result<(), HeldError> {
        let facts = private::facts(&self.file).map_err(cause)?;
        if facts.reparse || place_of(&self.file)? != self.path {
            return Err(HeldError::Unsafe);
        }
        Ok(())
    }

    /// Classifies a failed create-only make: Windows reports a name already taken by a folder
    /// or a link with a code that is no collision, so an `Unavailable` failure is `Unsafe` when
    /// the name's kind reads through this folder, as itself and only read. Nothing is written,
    /// and a lookup that fails keeps `Unavailable`. Inputs: the name and the error. Output: the kind.
    fn made_cause(&self, name: &Name, error: std::io::Error) -> HeldError {
        match cause(error) {
            HeldError::Unavailable if self.kind_unjudged(name).is_ok() => HeldError::Unsafe,
            kind => kind,
        }
    }

    /// Reads what one entry of this folder is, opened as itself, only read and bound to this
    /// folder: any reparse point is a link. Input: its name. Output: its kind, or the error.
    fn kind_unjudged(&self, name: &Name) -> Result<Kind, HeldError> {
        let entry = open_as_itself(&self.path.join(name.as_os_str()), None)?;
        self.bind(name, &entry)?;
        let facts = private::facts(&entry).map_err(cause)?;
        let folder = entry.metadata().map_err(cause)?.is_dir();
        Ok(if facts.reparse {
            Kind::Link
        } else if folder {
            Kind::Folder
        } else {
            Kind::File
        })
    }
}

/// The access right to delete an object, `DELETE`.
#[cfg(windows)]
const DELETE: u32 = 0x0001_0000;

/// The access right to read an object's owner and lists, `READ_CONTROL`.
#[cfg(windows)]
const READ_CONTROL: u32 = 0x0002_0000;

/// The access right to read an object's attributes, `FILE_READ_ATTRIBUTES`.
#[cfg(windows)]
const FILE_READ_ATTRIBUTES: u32 = 0x0080;

/// The share mode that lets other handles read, `FILE_SHARE_READ`.
#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x1;

/// The share mode that lets other handles write, `FILE_SHARE_WRITE`.
#[cfg(windows)]
const FILE_SHARE_WRITE: u32 = 0x2;

/// The file attribute of a reparse point, `FILE_ATTRIBUTE_REPARSE_POINT`.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

/// The file attribute of a folder, `FILE_ATTRIBUTE_DIRECTORY`.
#[cfg(windows)]
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0010;

/// The Windows error for a missing file, `ERROR_FILE_NOT_FOUND`.
#[cfg(windows)]
const FILE_NOT_FOUND: i32 = 2;

/// The Windows error for a missing folder on the way, `ERROR_PATH_NOT_FOUND`.
#[cfg(windows)]
const PATH_NOT_FOUND: i32 = 3;

/// The Windows error while another handle's share mode refuses this open,
/// `ERROR_SHARING_VIOLATION`.
#[cfg(windows)]
const SHARING_VIOLATION: i32 = 32;

/// The Windows error while another handle holds a lock, `ERROR_LOCK_VIOLATION`.
#[cfg(windows)]
const LOCK_VIOLATION: i32 = 33;

/// The Windows error for a create-only open of a name already there, `ERROR_FILE_EXISTS`.
#[cfg(windows)]
const FILE_EXISTS: i32 = 80;

/// The Windows error for a create-only folder already there, `ERROR_ALREADY_EXISTS`.
#[cfg(windows)]
const ALREADY_EXISTS: i32 = 183;

/// Reads one listed entry's kind from its find-data attributes, never following it: any
/// reparse point is a link. Input: the attributes. Output: the kind.
#[cfg(windows)]
fn kind_of(attributes: u32) -> Kind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        Kind::Link
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        Kind::Folder
    } else {
        Kind::File
    }
}

/// Maps a failed Windows call by cause (rv-sec3 #725 U5): sharing and lock violations are
/// `InUse`; not found and path not found are `Missing`; an entry already there and W1's
/// own refusals, a failed judge with no system code, are `Unsafe`; every other error,
/// access denied on a busy hold among them, is `Unavailable`. Input: the error. Output: the
/// kind.
#[cfg(windows)]
fn cause(error: std::io::Error) -> HeldError {
    match error.raw_os_error() {
        Some(SHARING_VIOLATION | LOCK_VIOLATION) => HeldError::InUse,
        Some(FILE_NOT_FOUND | PATH_NOT_FOUND) => HeldError::Missing,
        Some(FILE_EXISTS | ALREADY_EXISTS) => HeldError::Unsafe,
        Some(_) => HeldError::Unavailable,
        None if error.kind() == std::io::ErrorKind::PermissionDenied => HeldError::Unsafe,
        None => HeldError::Unavailable,
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
/// among them, is `Unsafe`. The local strip goes once W3-4a's `one_spelling` arm lands
/// (DESIGN-0515 D6). Input: the handle. Output: the plain place, or the error.
#[cfg(windows)]
fn place_of(file: &File) -> Result<PathBuf, HeldError> {
    use std::os::windows::ffi::OsStringExt;
    let units = private::place(file).map_err(cause)?;
    let verbatim = OsString::from_wide(&units);
    let plain = verbatim
        .to_str()
        .and_then(|text| text.strip_prefix(r"\\?\"))
        .map(PathBuf::from)
        .ok_or(HeldError::Unsafe)?;
    if !plain_drive(&plain) {
        return Err(HeldError::Unsafe);
    }
    Ok(plain)
}

/// Reads an open entry's volume serial number and 64-bit file id with W1 `identity`.
/// Input: the handle. Output: the pin, or the error.
#[cfg(all(windows, test))]
fn pin_of(file: &File) -> Result<Pin, HeldError> {
    let (device, inode) = private::identity(file).map_err(cause)?;
    Ok(Pin { device, inode })
}

/// Takes one lock on a held file without waiting (DESIGN-0515 D4): `flock` on Unix; on
/// Windows a shared lock is std's whole-file shared lock and an exclusive one W1's, both
/// mandatory, so an exclusive lock also refuses other handles' reads. Inputs: the file and
/// whether the lock is shared. Output: Ok, `InUse` while a conflicting lock is held, or
/// `Unavailable`.
#[cfg(test)]
pub(crate) fn lock(file: &File, shared: bool) -> Result<(), HeldError> {
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
        private::lock(file).map_err(|error| error.raw_os_error() == Some(LOCK_VIOLATION))
    };
    taken.map_err(|busy| {
        if busy {
            HeldError::InUse
        } else {
            HeldError::Unavailable
        }
    })
}

/// Releases a lock this handle took, now and not at the handle's close. A failed release
/// is not reported: the lock still ends when the handle closes, as the custody guards'
/// own unlocks already do on Unix. Input: the file.
#[cfg(test)]
pub(crate) fn unlock(file: &File) {
    #[cfg(unix)]
    let _ = rustix::fs::flock(file, rustix::fs::FlockOperation::Unlock);
    #[cfg(windows)]
    let _ = private::unlock(file);
}

/// The most entries one folder of a tree removal may list, past which the removal is `Full`.
const TREE_ENTRIES_MAX: usize = 4096;

/// The deepest a tree removal may be asked to go below the folder it removes.
pub(crate) const TREE_DEPTH_MAX: usize = 16;

/// How many times a tree removal empties a folder that fills again before it gives up with
/// `InUse`.
const REFILL_TRIES: usize = 3;

impl Folder {
    /// Removes one entry of a caller-judged tree, a folder with all it holds, bottom up
    /// through held folders, never following a link: a link anywhere goes as itself, and a
    /// folder that fills again is emptied again a fixed number of times. Inputs: the name and
    /// the folders deep below it the removal may go. Output: Ok, `Missing`, `Full` past the
    /// depth or an entry bound, `InUse`, or the error.
    pub(crate) fn remove_tree_unjudged(&self, name: &Name, depth: usize) -> Result<(), HeldError> {
        self.assert_caller_judged();
        assert!(depth <= TREE_DEPTH_MAX, "{depth}");
        let kind = self.kind_unjudged(name)?;
        self.remove_child_unjudged(name, kind, depth)
    }

    /// Removes one listed entry of a caller-judged tree: a file or a link at once, a folder
    /// once emptied, again while it fills meanwhile. Inputs: the name, its kind and the depth
    /// left below it. Output: Ok, or the error.
    fn remove_child_unjudged(
        &self,
        name: &Name,
        kind: Kind,
        depth: usize,
    ) -> Result<(), HeldError> {
        if kind != Kind::Folder {
            return self.remove_entry_unjudged(name, kind);
        }
        for _ in 0..REFILL_TRIES {
            let child = self.open_dir_unjudged(name)?;
            child.empty_unjudged(depth)?;
            drop(child);
            match self.remove_entry_unjudged(name, Kind::Folder) {
                Err(HeldError::Unavailable) if self.refilled_unjudged(name) => {}
                removed => return removed,
            }
        }
        Err(HeldError::InUse)
    }

    /// Removes everything in this caller-judged folder, each entry as its listed kind, a
    /// folder only while depth is left. An entry gone meanwhile counts as removed. Input: the
    /// depth left below this folder. Output: Ok, `Full`, or the error.
    fn empty_unjudged(&self, depth: usize) -> Result<(), HeldError> {
        for child in self.list(TREE_ENTRIES_MAX)? {
            let name = Name::checked(&child.name)?;
            if child.kind == Kind::Folder && depth == 0 {
                return Err(HeldError::Full);
            }
            match self.remove_child_unjudged(&name, child.kind, depth.saturating_sub(1)) {
                Ok(()) | Err(HeldError::Missing) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Whether a folder whose removal failed holds entries again. Input: its name. Output:
    /// true when it lists one or more entries.
    fn refilled_unjudged(&self, name: &Name) -> bool {
        match self.open_dir_unjudged(name).and_then(|child| child.list(1)) {
            Ok(children) => !children.is_empty(),
            Err(error) => error == HeldError::Full,
        }
    }
}

#[cfg(all(unix, test))]
impl Folder {
    /// Holds the first folder of a chain its caller judges, opened without following a link
    /// and judged by `judge` on the held handle only, never by path. A link at the name is
    /// `Unsafe` before the judge runs. Inputs: the absolute path and the judge. Output: the
    /// held folder, carrying the caller's judgement, `Missing`, or `Unsafe`.
    pub(crate) fn root_judged_by(path: &Path, judge: fn(&File) -> bool) -> Result<Self, HeldError> {
        use rustix::fs::{Mode, OFlags};
        assert!(path.is_absolute(), "{}", path.display());
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let file = File::from(rustix::fs::open(path, flags, Mode::empty()).map_err(errno_cause)?);
        if !judge(&file) {
            return Err(HeldError::Unsafe);
        }
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            judged: Judged::Caller,
        })
    }

    /// Opens and holds one child folder of a caller-judged tree through this folder, never
    /// following a link, with no owner or mode judged. Input: its name. Output: the held
    /// child, `Missing`, or `Unsafe` for a link or a file at the name.
    pub(crate) fn open_dir_unjudged(&self, name: &Name) -> Result<Self, HeldError> {
        use rustix::fs::{Mode, OFlags};
        self.assert_caller_judged();
        let file = self.open_at(name, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            pin,
            judged: self.judged,
        })
    }

    /// Makes one child folder of a caller-judged tree with mode 0700, create-only, so an
    /// entry or a link already at the name is `Unsafe`, then holds it. Input: its name.
    /// Output: the held child, or the error.
    pub(crate) fn make_dir_inherited(&self, name: &Name) -> Result<Self, HeldError> {
        use rustix::fs::Mode;
        self.assert_caller_judged();
        rustix::fs::mkdirat(&self.file, name.as_os_str(), Mode::from_raw_mode(0o700))
            .map_err(errno_cause)?;
        self.file.sync_all().map_err(cause)?;
        self.open_dir_unjudged(name)
    }

    /// Makes one child file of a caller-judged tree with mode 0600, create-only and never
    /// through a link, then writes this folder's entries to disk. Input: its name. Output:
    /// the file, open for reading and writing, or the error.
    pub(crate) fn make_file_inherited(&self, name: &Name) -> Result<File, HeldError> {
        use rustix::fs::{Mode, OFlags};
        self.assert_caller_judged();
        let flags = OFlags::RDWR | OFlags::CREATE | OFlags::EXCL;
        let file = self.open_at(name, flags, Mode::from_raw_mode(0o600))?;
        self.file.sync_all().map_err(cause)?;
        Ok(file)
    }

    /// Reads what one entry of a caller-judged tree is without following it. Input: its
    /// name. Output: its kind, `Missing`, or `Unsafe` for an entry that is none of a file, a
    /// folder or a link.
    fn kind_unjudged(&self, name: &Name) -> Result<Kind, HeldError> {
        use rustix::fs::{AtFlags, FileType};
        let stat = rustix::fs::statat(&self.file, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(errno_cause)?;
        match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Ok(Kind::File),
            FileType::Directory => Ok(Kind::Folder),
            FileType::Symlink => Ok(Kind::Link),
            _ => Err(HeldError::Unsafe),
        }
    }

    /// Unlinks one entry of a caller-judged tree by name through this folder, a folder only
    /// when empty; a link goes as itself. Inputs: its name and its kind. Output: Ok, or the
    /// error, `Unsafe` when the entry is no longer of that kind.
    fn remove_entry_unjudged(&self, name: &Name, kind: Kind) -> Result<(), HeldError> {
        use rustix::fs::AtFlags;
        let flags = if kind == Kind::Folder {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        rustix::fs::unlinkat(&self.file, name.as_os_str(), flags).map_err(errno_cause)
    }
}

#[cfg(windows)]
impl Folder {
    /// Holds the first folder of a chain its caller judges: only the plain drive spelling,
    /// opened as itself, never sharing delete, refused as `Unsafe` when it is a reparse point
    /// or not a folder before `judge` runs on the held handle. The folder keeps the place
    /// read from its handle. Inputs: the absolute path and the judge. Output: the held
    /// folder, carrying the caller's judgement, `Missing`, or `Unsafe`.
    pub(crate) fn root_judged_by(path: &Path, judge: fn(&File) -> bool) -> Result<Self, HeldError> {
        if !plain_drive(path) {
            return Err(HeldError::Unsafe);
        }
        let file = open_as_itself(path, None)?;
        plain_folder(&file)?;
        let place = place_of(&file)?;
        if !judge(&file) {
            return Err(HeldError::Unsafe);
        }
        #[cfg(test)]
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            #[cfg(test)]
            pin,
            judged: Judged::Caller,
            path: place,
        })
    }

    /// Opens and holds one child folder of a caller-judged tree at this folder's place, as
    /// itself and never sharing delete, refused as `Unsafe` when it is a reparse point or not
    /// a folder, then binds it to this folder. Input: its name. Output: the held child,
    /// `Missing`, `InUse`, or `Unsafe`.
    pub(crate) fn open_dir_unjudged(&self, name: &Name) -> Result<Self, HeldError> {
        self.assert_caller_judged();
        let path = self.path.join(name.as_os_str());
        let file = open_as_itself(&path, None)?;
        plain_folder(&file)?;
        self.bind(name, &file)?;
        #[cfg(test)]
        let pin = pin_of(&file)?;
        Ok(Self {
            file,
            #[cfg(test)]
            pin,
            judged: self.judged,
            path,
        })
    }

    /// Makes one child folder of a caller-judged tree, create-only, so an entry or a link
    /// already at the name is `Unsafe`, taking this folder's inheritable list, writes this
    /// folder's entries to disk, then holds and binds it. Input: its name. Output: the held
    /// child, or the error.
    pub(crate) fn make_dir_inherited(&self, name: &Name) -> Result<Self, HeldError> {
        self.assert_caller_judged();
        self.unmoved()?;
        std::fs::create_dir(self.path.join(name.as_os_str()))
            .map_err(|error| self.made_cause(name, error))?;
        private::sync_folder(&self.path).map_err(cause)?;
        self.open_dir_unjudged(name)
    }

    /// Makes one child file of a caller-judged tree, create-only and never through a link,
    /// taking this folder's inheritable list, so an agent the folder lets read gets it; then
    /// writes this folder's entries to disk and binds the file. Input: its name. Output: the
    /// file, open for reading and writing, or the error.
    pub(crate) fn make_file_inherited(&self, name: &Name) -> Result<File, HeldError> {
        use crate::platform::FILE_FLAG_OPEN_REPARSE_POINT;
        use std::os::windows::fs::OpenOptionsExt;
        self.assert_caller_judged();
        self.unmoved()?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(self.path.join(name.as_os_str()))
            .map_err(|error| self.made_cause(name, error))?;
        private::sync_folder(&self.path).map_err(cause)?;
        self.bind(name, &file)?;
        Ok(file)
    }

    /// Deletes one entry of a caller-judged tree through a handle opened as itself with
    /// delete access and bound to this folder, its read-only mark cleared first, with POSIX
    /// semantics; a link goes as itself and a folder only when empty. Inputs: its name and
    /// its kind, which the handle already tells. Output: Ok, `Missing`, `InUse`, or the
    /// error.
    fn remove_entry_unjudged(&self, name: &Name, _kind: Kind) -> Result<(), HeldError> {
        let access = DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES;
        let entry = open_as_itself(&self.path.join(name.as_os_str()), Some(access))?;
        self.bind(name, &entry)?;
        private::clear_read_only(&entry).map_err(cause)?;
        private::delete(&entry).map_err(cause)
    }
}

/// The access right to write an object's attributes, `FILE_WRITE_ATTRIBUTES`.
#[cfg(windows)]
const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;

/// Opens one entry as itself, never through a reparse point, a folder as well as a file,
/// sharing read and write but never delete, so it cannot be renamed or deleted by another
/// handle while held. Inputs: the path and the access, read when none. Output: the open
/// entry, or the error by cause.
#[cfg(windows)]
fn open_as_itself(path: &Path, access: Option<u32>) -> Result<File, HeldError> {
    use crate::platform::{FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT};
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    match access {
        Some(access) => options.access_mode(access),
        None => options.read(true),
    };
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(cause)
}

/// Refuses an entry opened as itself that is a reparse point or not a folder. Input: the
/// open entry. Output: Ok, or `Unsafe`.
#[cfg(windows)]
fn plain_folder(entry: &File) -> Result<(), HeldError> {
    let facts = private::facts(entry).map_err(cause)?;
    let folder = entry.metadata().map_err(cause)?.is_dir();
    if facts.reparse || !folder {
        return Err(HeldError::Unsafe);
    }
    Ok(())
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
