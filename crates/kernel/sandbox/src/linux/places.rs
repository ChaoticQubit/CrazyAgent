use crate::linux::helper::GRANT_BECAME_LINK;
use crate::linux::launch::{LANDLOCK_DEVICES, SYSTEM_READ_ROOTS};
use rustix::fs::{FileType, Mode, OFlags};
use rustix::io::Errno;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The helper word that names one grant root bubblewrap binds, then its device and inode.
pub(crate) const BOUND: &str = "--bound";

/// How the engine refuses a bubblewrap grant root it cannot open to record, since
/// bubblewrap would then bind a place nobody checked.
pub(crate) const GRANT_NOT_OPENED: &str =
    "a granted folder cannot be opened to record what bubblewrap binds";

/// How the helper refuses a bubblewrap launch one of whose bound grant roots is not the
/// place the engine opened at wrap, as when it became a link before bubblewrap bound it.
pub(crate) const BOUND_ROOT_CHANGED: &str =
    "a granted folder is not the one the engine checked when bubblewrap bound it";

/// One grant root bubblewrap binds, as the engine opened it at wrap: the path, and the
/// device and inode of the place itself, never of a link's target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bound {
    pub(crate) root: PathBuf,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

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

/// Opens each grant root bubblewrap binds through [`granted_place`] and records the device
/// and inode of the opened place. Input: the roots, in bind order. Output: one identity per
/// root, or the refusal of a root that is a link, sits under one, or does not open.
pub(crate) fn bound_roots<'a>(
    roots: impl IntoIterator<Item = &'a PathBuf>,
) -> Result<Vec<Bound>, String> {
    let mut bound = Vec::new();
    for root in roots {
        let unopened = |detail: String| format!("{GRANT_NOT_OPENED}: {}{detail}", root.display());
        let place = granted_place(root)?.ok_or_else(|| unopened(String::new()))?;
        let meta = File::from(place)
            .metadata()
            .map_err(|error| unopened(format!(": {error}")))?;
        bound.push(Bound {
            root: root.clone(),
            dev: meta.dev(),
            ino: meta.ino(),
        });
    }
    Ok(bound)
}

/// The helper words for the bound roots: [`BOUND`], the root, its device and its inode, per
/// root. Input: the identities. Output: the words.
pub(crate) fn bound_args(bound: &[Bound]) -> Vec<OsString> {
    let mut args = Vec::with_capacity(4 * bound.len());
    for one in bound {
        args.push(BOUND.into());
        args.push(one.root.clone().into_os_string());
        args.push(one.dev.to_string().into());
        args.push(one.ino.to_string().into());
    }
    assert_eq!(args.len(), 4 * bound.len());
    args
}

/// Reads one bound root's device and inode after [`BOUND`] and its path, each a decimal
/// number. Inputs: the absolute root and the remaining words. Output: the identity, or the
/// reason it is refused.
pub(crate) fn bound_entry(
    root: PathBuf,
    rest: &mut impl Iterator<Item = OsString>,
) -> Result<Bound, String> {
    let mut number = || {
        rest.next()
            .as_deref()
            .and_then(OsStr::to_str)
            .and_then(|text| text.parse::<u64>().ok())
            .ok_or_else(|| format!("{BOUND} needs a device and an inode"))
    };
    let dev = number()?;
    let ino = number()?;
    Ok(Bound { root, dev, ino })
}

/// Checks, inside bubblewrap after every mount, that each bound root is still the place the
/// engine opened at wrap, reading the root itself and never a link's target. A bind mount
/// keeps its source's device and inode, so a root swapped before bubblewrap bound it differs.
/// Input: the identities. Output: the refusal naming the first root that differs or cannot
/// be read.
pub(crate) fn held_roots(bound: &[Bound]) -> Result<(), String> {
    for one in bound {
        let changed =
            |detail: String| format!("{BOUND_ROOT_CHANGED}: {}{detail}", one.root.display());
        let meta =
            std::fs::symlink_metadata(&one.root).map_err(|error| changed(format!(": {error}")))?;
        if meta.dev() != one.dev || meta.ino() != one.ino {
            return Err(changed(String::new()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bound_root_records_the_place_itself_and_is_held_until_it_is_swapped() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let home = base.join("home");
        let secret = base.join("secret");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        let file = base.join("note.txt");
        std::fs::write(&file, "note\n").unwrap();
        let bound = bound_roots([&home, &file]).unwrap();
        let meta = std::fs::symlink_metadata(&home).unwrap();
        assert_eq!((bound[0].dev, bound[0].ino), (meta.dev(), meta.ino()));
        assert_eq!(bound[1].root, file);
        assert_eq!(held_roots(&bound), Ok(()));
        std::fs::rename(&home, base.join("home-moved")).unwrap();
        std::os::unix::fs::symlink(&secret, &home).unwrap();
        let refused = held_roots(&bound).unwrap_err();
        assert!(refused.starts_with(BOUND_ROOT_CHANGED), "{refused}");
        assert!(
            bound_roots([&home])
                .unwrap_err()
                .starts_with(GRANT_BECAME_LINK)
        );
        std::fs::remove_file(&home).unwrap();
        assert!(
            held_roots(&bound)
                .unwrap_err()
                .starts_with(BOUND_ROOT_CHANGED)
        );
        assert!(
            bound_roots([&home])
                .unwrap_err()
                .starts_with(GRANT_NOT_OPENED)
        );
    }

    #[test]
    fn bound_words_read_back_as_the_same_identities() {
        let bound = vec![Bound {
            root: PathBuf::from("/w/home"),
            dev: 66306,
            ino: 4_294_967_301,
        }];
        let mut words = bound_args(&bound).into_iter();
        assert_eq!(words.next().as_deref(), Some(OsStr::new(BOUND)));
        let root = PathBuf::from(words.next().unwrap());
        assert_eq!(bound_entry(root, &mut words), Ok(bound[0].clone()));
        assert!(words.next().is_none());
        let mut short = vec![OsString::from("1")].into_iter();
        assert!(bound_entry(PathBuf::from("/w"), &mut short).is_err());
        let mut signed = vec![OsString::from("1"), OsString::from("-2")].into_iter();
        assert!(bound_entry(PathBuf::from("/w"), &mut signed).is_err());
    }
}
