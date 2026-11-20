#![cfg(windows)]

use crazyagent_win_sandbox::private::{self, Create};
use std::ffi::OsStr;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

/// A fresh scratch path in the temp folder, unique to this test and run, with nothing at
/// it. Input: the test's name. Output: the path.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("crazy-w3-5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    assert!(!path.exists());
    path
}

/// The names in a folder, sorted. Input: the folder path. Output: the names.
fn listed(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// W3-5 D6: a folder made under a held W1 folder is itself the root of the next create, and
/// both the folder and the file below it pass W1's private judge, at the path their names
/// give. This holds for either descriptor QUESTION-1512 Q2 weighs.
#[test]
fn a_folder_and_a_file_are_made_private_relative_to_held_folders() {
    let root = scratch("made");
    let held = private::dir(&root).unwrap();
    let folder = private::create_under(&held, OsStr::new("src"), Create::Folder).unwrap();
    let mut file = private::create_under(&folder, OsStr::new("main.rs"), Create::File).unwrap();
    file.write_all(b"fn main() {}\n").unwrap();
    file.sync_all().unwrap();
    file.rewind().unwrap();
    let mut back = String::new();
    file.read_to_string(&mut back).unwrap();
    assert_eq!(back, "fn main() {}\n");
    assert_eq!(
        std::fs::read(root.join("src").join("main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    private::check(&folder).unwrap();
    private::check(&file).unwrap();
    assert!(std::fs::metadata(root.join("src")).unwrap().is_dir());
    assert_eq!(listed(&root), ["src"]);
    drop((file, folder, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// rv-sec2 #742 D6-3: a name already there is refused with ERROR_ALREADY_EXISTS whether it
/// matches exactly or only in letter case, and whatever kind it is; nothing is opened in its
/// place, and the entry already there keeps its bytes.
#[test]
fn a_taken_or_case_only_name_is_refused_and_the_entry_stays() {
    let root = scratch("taken");
    let held = private::dir(&root).unwrap();
    let mut first = private::create_under(&held, OsStr::new("Readme"), Create::File).unwrap();
    first.write_all(b"old").unwrap();
    drop(first);
    let folder = private::create_under(&held, OsStr::new("Src"), Create::Folder).unwrap();
    for (name, kind) in [
        ("Readme", Create::File),
        ("README", Create::File),
        ("readme", Create::Folder),
        ("src", Create::Folder),
        ("SRC", Create::File),
    ] {
        let refused = private::create_under(&held, OsStr::new(name), kind).unwrap_err();
        assert_eq!(
            refused.raw_os_error(),
            Some(183),
            "{name} {kind:?}: {refused}"
        );
    }
    assert_eq!(std::fs::read(root.join("Readme")).unwrap(), b"old");
    assert_eq!(listed(&root), ["Readme", "Src"]);
    drop((folder, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// fix10 NOTE-1326 item 2 (N6 C6): a file made by `create_under` carries delete access from
/// its own open, so after a write and a sync it renames over an existing name with
/// `Mode::Replace`, and the name then holds the new bytes and that file's identity.
#[test]
fn a_made_file_is_written_synced_and_renamed_over_an_existing_name() {
    let root = scratch("replace");
    let held = private::dir(&root).unwrap();
    let mut old =
        private::create_under(&held, OsStr::new("config.worktree"), Create::File).unwrap();
    old.write_all(b"old").unwrap();
    drop(old);
    let mut staged =
        private::create_under(&held, OsStr::new(".config.worktree.partial"), Create::File).unwrap();
    staged.write_all(b"new").unwrap();
    staged.sync_all().unwrap();
    let pinned = private::identity(&staged).unwrap();
    private::rename(
        &staged,
        &held,
        OsStr::new("config.worktree"),
        private::Mode::Replace,
    )
    .unwrap();
    drop(staged);
    let landed = root.join("config.worktree");
    assert_eq!(std::fs::read(&landed).unwrap(), b"new");
    let reread = private::read_only_replaceable(&landed).unwrap();
    assert_eq!(private::identity(&reread).unwrap(), pinned);
    assert_eq!(listed(&root), ["config.worktree"]);
    drop((reread, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// rv-sec2 #742 D6-3: a junction already at the name is a collision, never followed, so
/// nothing is made in the folder it points to.
#[test]
fn a_junction_at_the_name_is_refused_and_not_followed() {
    let root = scratch("junction");
    let held = private::dir(&root).unwrap();
    let elsewhere = scratch("junction-target");
    std::fs::create_dir(&elsewhere).unwrap();
    let link = root.join("link");
    let mut command = std::process::Command::new("cmd");
    command
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&elsewhere)
        .stdout(std::process::Stdio::null());
    let status = command.status().unwrap();
    assert!(status.success());
    for kind in [Create::Folder, Create::File] {
        let refused = private::create_under(&held, OsStr::new("link"), kind).unwrap_err();
        assert_eq!(refused.raw_os_error(), Some(183), "{kind:?}: {refused}");
    }
    assert!(listed(&elsewhere).is_empty());
    drop(held);
    std::fs::remove_dir(&link).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&elsewhere).unwrap();
}

/// rv-sec2 #742 D6-1 and #751 L1: a name the one name rule `name::refused` refuses, a
/// device name, a character Windows refuses or a trailing dot or space among them, trips
/// `create_under`'s assertion before any call, as it does `rename`'s; callers refuse such
/// names first.
#[test]
fn a_name_that_is_not_one_component_trips_the_assertion() {
    let root = scratch("name");
    let held = private::dir(&root).unwrap();
    for name in [
        "", ".", "..", "a.", "a ", "a\\b", "a/b", "a:b", "x::$DATA", "a\0b", "aux", "NUL.txt",
        "com1", "a*b", "a?b", "a\u{1}b",
    ] {
        let tripped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            private::create_under(&held, OsStr::new(name), Create::File)
        }));
        assert!(tripped.is_err(), "{name:?} reached the create");
    }
    assert!(listed(&root).is_empty());
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}

/// The control bits of an open entry's security descriptor. Input: the handle, opened with
/// read-control. Output: the bits.
fn control(entry: &std::fs::File) -> u16 {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl};
    let mut descriptor = std::ptr::null_mut();
    let status = unsafe {
        GetSecurityInfo(
            entry.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(status, 0);
    let mut bits = 0u16;
    let mut revision = 0u32;
    let read = unsafe { GetSecurityDescriptorControl(descriptor, &mut bits, &mut revision) };
    unsafe { LocalFree(descriptor) };
    assert_ne!(read, 0);
    bits
}

/// The entries of an access list as their flags and SIDs, for the allow and deny kinds.
/// Input: the list's bytes. Output: one pair per entry.
fn entries(list: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let count = usize::from(u16::from_le_bytes([list[4], list[5]]));
    let mut at = 8;
    let mut found = Vec::new();
    for _ in 0..count {
        let kind = list[at];
        let flags = list[at + 1];
        let size = usize::from(u16::from_le_bytes([list[at + 2], list[at + 3]]));
        assert!(matches!(kind, 0 | 1), "entry kind {kind}");
        found.push((flags, list[at + 8..at + size].to_vec()));
        at += size;
    }
    assert_eq!(found.len(), count);
    found
}

const PROTECTED: u16 = 0x1000;
const INHERITED: u8 = 0x10;
const ALL_APP_PACKAGES: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 1, 0, 0, 0];

/// rv-sec2 RULING-15:12 row 1: a folder and a file `create_under` makes for a lease have the
/// engine's user as owner, even on an elevated token, and a list that is not protected and
/// holds inherited entries only, so a later grant on the lease reaches them.
#[test]
fn a_lease_entry_has_the_user_as_owner_and_an_unprotected_inherited_list() {
    let root = scratch("inherited");
    let held = private::dir(&root).unwrap();
    let folder = private::create_under(&held, OsStr::new("src"), Create::Folder).unwrap();
    let file = private::create_under(&folder, OsStr::new("main.rs"), Create::File).unwrap();
    let user = private::user_sid().unwrap();
    for (name, entry) in [("src", &folder), ("src/main.rs", &file)] {
        let facts = private::facts(entry).unwrap();
        assert_eq!(facts.owner, user, "{name}");
        assert_eq!(
            control(entry) & PROTECTED,
            0,
            "{name}: the list is protected"
        );
        let found = entries(&facts.list.unwrap());
        assert!(!found.is_empty(), "{name}");
        assert!(
            found.iter().all(|(flags, _)| flags & INHERITED != 0),
            "{name}: an entry is not inherited: {found:?}"
        );
    }
    drop((file, folder, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// rv-sec2 RULING-15:12 row 2: after an inheritable grant on the lease root, as the sandbox
/// grants an agent its worktree, a file made before the grant carries the grantee's entry
/// as an inherited one.
#[test]
fn a_grant_on_the_lease_root_reaches_an_entry_made_before_it() {
    let root = scratch("grant");
    let held = private::dir(&root).unwrap();
    let folder = private::create_under(&held, OsStr::new("src"), Create::Folder).unwrap();
    let file = private::create_under(&folder, OsStr::new("main.rs"), Create::File).unwrap();
    let status = std::process::Command::new("icacls")
        .arg(&root)
        .args(["/grant", "*S-1-15-2-1:(OI)(CI)(M)"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let found = entries(&private::facts(&file).unwrap().list.unwrap());
    assert!(
        found
            .iter()
            .any(|(flags, sid)| flags & INHERITED != 0 && sid[..] == ALL_APP_PACKAGES[..]),
        "{found:?}"
    );
    drop((file, folder, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// rv-sec2 RULING-15:12 row 3: a file made as `Create::PrivateFile`, as
/// `.config.worktree.partial` is in the repository's admin folder, has the user as owner and
/// W1's protected private list, which W1's judge passes.
#[test]
fn a_private_file_has_a_protected_private_list() {
    let root = scratch("private");
    let held = private::dir(&root).unwrap();
    let file = private::create_under(
        &held,
        OsStr::new(".config.worktree.partial"),
        Create::PrivateFile,
    )
    .unwrap();
    assert_ne!(control(&file) & PROTECTED, 0);
    assert_eq!(
        private::facts(&file).unwrap().owner,
        private::user_sid().unwrap()
    );
    private::check(&file).unwrap();
    drop((file, held));
    std::fs::remove_dir_all(&root).unwrap();
}
