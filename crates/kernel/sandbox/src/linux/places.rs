use crate::linux::helper::GRANT_BECAME_LINK;
use crate::linux::launch::{LANDLOCK_DEVICES, SYSTEM_READ_ROOTS};
use rustix::fs::{FileType, Mode, OFlags};
use rustix::io::Errno;
use std::os::fd::OwnedFd;
use std::path::{Component, Path};

/// Whether `path` is one of the fixed system folders and devices every policy reads or
/// writes, some of which are links on this computer (`/bin` to `/usr/bin`), so their rules
/// follow links as before; every other rule path is a granted folder. Input: the path.
/// Output: the answer.
pub(crate) fn fixed_place(path: &Path) -> bool {
    SYSTEM_READ_ROOTS
        .iter()
        .chain(&["/proc"])
        .chain(LANDLOCK_DEVICES)
        .any(|fixed| path == Path::new(fixed))
}

/// Opens a granted folder or file one component at a time from `/`, each with `O_PATH` and
/// `O_NOFOLLOW` and each checked not to be a link, since such an open of a link returns the
/// link itself. Input: the absolute path. Output: the open place, none when it cannot be
/// opened as today, or the refusal when any component is a link or not a folder.
pub(crate) fn granted_place(path: &Path) -> Result<Option<OwnedFd>, String> {
    assert!(path.is_absolute());
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let link = || format!("{GRANT_BECAME_LINK}: {}", path.display());
    let Ok(mut place) = rustix::fs::open("/", flags, Mode::empty()) else {
        return Ok(None);
    };
    for component in path.components().skip(1) {
        let Component::Normal(name) = component else {
            return Err(link());
        };
        place = match rustix::fs::openat(&place, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::NOTDIR) => return Err(link()),
            Err(_) => return Ok(None),
        };
        let Ok(stat) = rustix::fs::fstat(&place) else {
            return Ok(None);
        };
        if FileType::from_raw_mode(stat.st_mode) == FileType::Symlink {
            return Err(link());
        }
    }
    Ok(Some(place))
}
