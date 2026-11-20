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

/// rv-sec2 #742 D6-1 and #751 L1: a name that is not one plain component, or that ends in a
/// dot or a space, trips `create_under`'s assertion before any call, the same shape
/// `rename` asserts; callers refuse such names first.
#[test]
fn a_name_that_is_not_one_component_trips_the_assertion() {
    let root = scratch("name");
    let held = private::dir(&root).unwrap();
    for name in [
        "", ".", "..", "a.", "a ", "a\\b", "a/b", "a:b", "x::$DATA", "a\0b",
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

/// QUESTION-1512 Q2, the one line rv-sec2 rules on: an entry `create_under` makes is owned by
/// the engine's user, the owner W1 gives every private entry, even on an elevated token whose
/// default owner is the administrators group. A null descriptor (#742 D6-3 as written) fails
/// this row on such a token; W1's own descriptor passes it.
#[test]
fn a_made_entry_is_owned_by_the_engine_user() {
    let root = scratch("owner");
    let held = private::dir(&root).unwrap();
    let folder = private::create_under(&held, OsStr::new("src"), Create::Folder).unwrap();
    let file = private::create_under(&folder, OsStr::new("main.rs"), Create::File).unwrap();
    let user = private::user_sid().unwrap();
    assert_eq!(private::facts(&folder).unwrap().owner, user);
    assert_eq!(private::facts(&file).unwrap().owner, user);
    drop((file, folder, held));
    std::fs::remove_dir_all(&root).unwrap();
}
