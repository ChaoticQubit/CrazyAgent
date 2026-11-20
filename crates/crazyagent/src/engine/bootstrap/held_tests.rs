use super::{Folder, lock, unlock};
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};

const UNSAFE: &str = "native_profile_custody_unsafe";
const IN_USE: &str = "native_profile_in_use";

fn name(text: &str) -> &OsStr {
    OsStr::new(text)
}

/// A private temp folder, as an engine root is: 0700 and owned on Unix, a W1 private folder
/// on Windows. Output: the temp guard and the folder's canonical path.
fn private_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = crate::sandbox::canonical(temp.path()).unwrap().join("root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
    }
    #[cfg(windows)]
    drop(crazyagent_win_sandbox::private::dir(&root).unwrap());
    (temp, root)
}

/// Writes a new private file, 0600 on Unix and a W1 private file on Windows. Inputs: the
/// path and the bytes.
fn private_file(path: &Path, bytes: &[u8]) {
    use std::io::Write;
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
    };
    #[cfg(windows)]
    let mut file = crazyagent_win_sandbox::private::file(path, true).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

/// Opens a folder by a handle that lets it be renamed, so a row can swap the name while a
/// handle still holds the old folder. Input: the folder. Output: the handle.
fn hold_sharing_delete(path: &Path) -> File {
    #[cfg(unix)]
    let file = File::open(path).unwrap();
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2 | 0x4)
            .custom_flags(crate::platform::FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap()
    };
    file
}

/// Makes `link` name the folder `target`: a symbolic link on Unix, a junction on Windows.
fn link_folder(link: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let status = crate::spawn_gate::status(
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(status.success());
    }
}

/// Lets every account read a folder: mode 0755 on Unix, an Everyone entry on Windows.
fn open_to_everyone(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    {
        let status = crate::spawn_gate::status(
            std::process::Command::new("icacls")
                .arg(path)
                .args(["/grant", "*S-1-1-0:(OI)(CI)(R)"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(status.success());
    }
}

/// N2a row 1: a held folder makes, opens and pins its children, a second open of one name
/// gives the same identity, a file opens pinned and passes the check, and a missing child is
/// `native_profile_incomplete`. On Windows the held folder cannot be renamed meanwhile.
#[test]
fn a_held_folder_makes_opens_and_pins_its_children() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let made = held.make_dir(name("a")).unwrap();
    let opened = held.open_dir(name("a")).unwrap();
    assert_eq!(made.pin(), opened.pin());
    assert_ne!(held.pin(), opened.pin());
    held.verify(name("a"), opened.file(), opened.pin(), true)
        .unwrap();
    private_file(&root.join("a").join("f"), b"bytes");
    let (file, pin) = opened.open_read(name("f")).unwrap();
    opened.verify(name("f"), &file, pin, false).unwrap();
    assert_ne!(pin, opened.pin());
    assert_eq!(
        held.open_dir(name("missing")).err().unwrap().0,
        "native_profile_incomplete"
    );
    #[cfg(windows)]
    {
        assert!(std::fs::rename(root.join("a"), root.join("moved")).is_err());
        assert!(root.join("a").is_dir());
        assert!(!root.join("moved").exists());
    }
}

/// N2a row 2: a child that is a link or junction, a name swapped to a new folder after its
/// pin, a child every account may read and a hard-linked file are each refused as unsafe,
/// while the plain folder beside them still opens.
#[test]
fn a_linked_swapped_open_or_hard_linked_child_is_refused() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    drop(held.make_dir(name("target")).unwrap());
    link_folder(&root.join("link"), &root.join("target"));
    assert_eq!(held.open_dir(name("link")).err().unwrap().0, UNSAFE);
    let first = held.make_dir(name("swapped")).unwrap();
    let pin = first.pin();
    drop(first);
    let kept = hold_sharing_delete(&root.join("swapped"));
    std::fs::rename(root.join("swapped"), root.join("old")).unwrap();
    drop(held.make_dir(name("swapped")).unwrap());
    assert_eq!(
        held.verify(name("swapped"), &kept, pin, true)
            .unwrap_err()
            .0,
        UNSAFE
    );
    drop(held.make_dir(name("open")).unwrap());
    open_to_everyone(&root.join("open"));
    assert_eq!(held.open_dir(name("open")).err().unwrap().0, UNSAFE);
    private_file(&root.join("f"), b"bytes");
    std::fs::hard_link(root.join("f"), root.join("g")).unwrap();
    assert_eq!(held.open_read(name("f")).err().unwrap().0, UNSAFE);
    held.open_dir(name("target")).unwrap();
}

/// N2a row 3: a name that is not one plain component, or holds a colon, which Windows reads
/// as a stream, trips the layer's assertion before any open, and nothing is made.
#[test]
fn a_name_that_is_not_one_plain_component_trips_the_layer() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let mut names = vec!["", ".", "..", "a/b", "a:b", "a.", "a ", "NUL", "con.txt"];
    if cfg!(windows) {
        names.push("a\\b");
    }
    for text in names {
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            held.open_dir(name(text)).is_ok()
        }));
        assert!(opened.is_err(), "{text:?} reached an open");
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            held.make_dir(name(text)).is_ok()
        }));
        assert!(made.is_err(), "{text:?} reached a make");
    }
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
}

/// N2a row 4: shared locks meet, an exclusive one is `native_profile_in_use` beside them, a
/// second handle reads the file during shared locks, and an unlock frees the file at once.
/// On Windows an exclusive lock also refuses another handle's read (DESIGN-0515 D4).
#[test]
fn shared_locks_meet_an_exclusive_one_waits_and_unlock_frees_it() {
    let (_temp, root) = private_root();
    let path = root.join("lock");
    private_file(&path, b"lock");
    let one = File::open(&path).unwrap();
    let two = File::open(&path).unwrap();
    lock(&one, true).unwrap();
    lock(&two, true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"lock");
    unlock(&two);
    assert_eq!(lock(&two, false).unwrap_err().0, IN_USE);
    unlock(&one);
    lock(&two, false).unwrap();
    assert_eq!(lock(&one, true).unwrap_err().0, IN_USE);
    #[cfg(windows)]
    assert!(
        std::fs::read(&path).is_err(),
        "an exclusive Windows lock refuses another handle's read"
    );
    unlock(&two);
    lock(&one, false).unwrap();
    unlock(&one);
}

/// N2a row 3b (leader 05:2x, rv-sec2 #717 C1): the layer holds only the plain Windows
/// spelling. A verbatim `\\?\` root trips its assertion before any open, while the same
/// folder in its plain spelling holds, so every held path the layer joins is plain.
#[cfg(windows)]
#[test]
fn a_verbatim_root_trips_the_layer_and_the_plain_spelling_holds() {
    let (_temp, root) = private_root();
    let plain = root.to_str().unwrap();
    assert!(plain.as_bytes().get(1) == Some(&b':'), "{plain}");
    let verbatim = PathBuf::from(format!(r"\\?\{plain}"));
    let held = std::panic::catch_unwind(|| Folder::root(&verbatim).is_ok());
    assert!(held.is_err(), "a verbatim root reached an open");
    Folder::root(&root).unwrap();
}
