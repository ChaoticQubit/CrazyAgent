use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use rustix::fd::AsFd;
use rustix::fs::{AtFlags, Mode, OFlags};

/// The files this process holds open for good, by device and inode, and any second
/// descriptor of one of them opened in a race. None is ever closed: closing any
/// descriptor of engine.db drops every POSIX lock this process holds on it, those of its
/// live SQLite connections included.
struct Held {
    files: BTreeMap<(rustix::fs::Dev, u64), Arc<File>>,
    spares: Vec<File>,
}

static HELD: Mutex<Held> = Mutex::new(Held {
    files: BTreeMap::new(),
    spares: Vec::new(),
});

/// Gives a file this process keeps open for its whole life: the one already held for the
/// inode the name now has, or a new read-only, close-on-exec open that follows no link,
/// kept under its own device and inode before any check, so no caller ever closes a
/// descriptor of it. Inputs: the folder (or the working folder) and the name. Output: the
/// shared file, or the open's error.
pub fn held_file(parent: impl AsFd, name: &Path) -> Result<Arc<File>, std::io::Error> {
    let parent = parent.as_fd();
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    if let Ok(named) = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        && let Some(file) = held.files.get(&(named.st_dev, named.st_ino))
    {
        return Ok(Arc::clone(file));
    }
    let file = File::from(rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )?);
    let opened = match rustix::fs::fstat(&file) {
        Ok(opened) => opened,
        Err(cause) => {
            held.spares.push(file);
            return Err(cause.into());
        }
    };
    let key = (opened.st_dev, opened.st_ino);
    assert!(key.1 != 0);
    if let Some(kept) = held.files.get(&key) {
        let kept = Arc::clone(kept);
        held.spares.push(file);
        return Ok(kept);
    }
    let file = Arc::new(file);
    held.files.insert(key, Arc::clone(&file));
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_file_is_shared_by_every_name_of_its_inode_and_closed_on_exec() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("engine.db");
        let second = root.path().join("linked.db");
        std::fs::write(&first, b"held").unwrap();
        std::fs::hard_link(&first, &second).unwrap();

        let once = held_file(rustix::fs::CWD, &first).unwrap();
        let again = held_file(rustix::fs::CWD, &first).unwrap();
        let linked = held_file(rustix::fs::CWD, &second).unwrap();

        assert!(
            Arc::ptr_eq(&once, &again),
            "a second call shares the held file"
        );
        assert!(
            Arc::ptr_eq(&once, &linked),
            "a hard link shares the held file"
        );
        let flags = rustix::io::fcntl_getfd(&*once).unwrap();
        assert!(
            flags.contains(rustix::io::FdFlags::CLOEXEC),
            "the held file passes into a child"
        );
    }
}
