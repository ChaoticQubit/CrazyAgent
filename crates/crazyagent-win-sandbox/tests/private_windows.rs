#![cfg(windows)]

use crazyagent_win_sandbox::dacl::{self, ADMINISTRATORS, SYSTEM};
use crazyagent_win_sandbox::private;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A fresh scratch path in the temp folder, unique to this test and run, with nothing at
/// it. Input: the test's name. Output: the path.
fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("crazy-w1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    let _ = std::fs::remove_file(&path);
    assert!(!path.exists());
    path
}

/// Runs a Windows tool and fails the test when it fails. Inputs: the program and its
/// arguments.
fn run(program: &str, args: &[&str]) {
    let status = Command::new(program).args(args).status().unwrap();
    assert!(status.success(), "{program} {args:?} failed: {status}");
}

/// Grants a SID inheritable rights on a folder through icacls. Inputs: the folder, the SID
/// string and the icacls rights letter.
fn grant(folder: &Path, sid: &str, rights: &str) {
    let entry = format!("*{sid}:(OI)(CI)({rights})");
    run("icacls", &[folder.to_str().unwrap(), "/grant", &entry]);
}

/// Asserts every entry of an access list names the user, SYSTEM or the administrators.
/// Inputs: the list and the user's SID.
fn only_trusted(list: &[u8], user: &[u8]) {
    for entry in dacl::entries(list).unwrap() {
        assert!(
            entry.sid == user || entry.sid == SYSTEM || entry.sid == ADMINISTRATORS,
            "unexpected entry {entry:?}"
        );
    }
}

/// W1 D1: a new private folder and file are owned by the user and list only the user,
/// SYSTEM and the administrators; a second create of the same file is refused.
#[test]
fn a_new_private_folder_and_file_list_only_the_user_system_and_administrators() {
    let root = scratch("new");
    let user = private::user_sid().unwrap();
    let held = private::dir(&root).unwrap();
    let folder = private::facts(&held).unwrap();
    assert_eq!(folder.owner, user);
    only_trusted(folder.list.as_deref().unwrap(), &user);
    assert_eq!(private::judge(&folder, &user), Ok(()));
    let path = root.join("engine.db");
    let file = private::file(&path, true).unwrap();
    let facts = private::facts(&file).unwrap();
    assert_eq!(facts.links, 1);
    only_trusted(facts.list.as_deref().unwrap(), &user);
    private::check(&file).unwrap();
    assert_eq!(
        private::file(&path, true).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    drop((file, held));
    let again = private::dir(&root).expect("an existing private folder opens again");
    let reopened = private::file(&path, false).expect("an existing private file opens again");
    drop((reopened, again));
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 D3 (A6 P4): a held private folder cannot be renamed away, so a path checked through
/// it keeps naming the same folder; once the hold ends the rename works.
#[test]
fn a_held_private_folder_cannot_be_renamed() {
    let root = scratch("held");
    let moved = scratch("held-moved");
    let held = private::dir(&root).unwrap();
    assert!(std::fs::rename(&root, &moved).is_err());
    assert!(root.is_dir() && !moved.exists());
    drop(held);
    std::fs::rename(&root, &moved).unwrap();
    std::fs::remove_dir_all(&moved).unwrap();
}

/// W1 D1: a private file that gained a second hard link is refused on open.
#[test]
fn a_private_file_with_a_second_hard_link_is_refused() {
    let root = scratch("link");
    let held = private::dir(&root).unwrap();
    let path = root.join("engine.db");
    drop(private::file(&path, true).unwrap());
    drop(private::file(&path, false).expect("one link opens"));
    std::fs::hard_link(&path, root.join("again.db")).unwrap();
    assert!(private::file(&path, false).is_err());
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 D3: a symbolic link in place of a private file is refused, never followed.
#[test]
fn a_symbolic_link_to_a_private_file_is_refused() {
    let root = scratch("symlink");
    let held = private::dir(&root).unwrap();
    let target = root.join("engine.db");
    drop(private::file(&target, true).unwrap());
    let link = root.join("link.db");
    std::os::windows::fs::symlink_file(&target, &link).expect("the hosted runner may make links");
    drop(private::file(&target, false).expect("the link's target opens by its own name"));
    assert!(private::file(&link, false).is_err());
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 D3: a junction in place of the data folder is refused, even when its target is a
/// private folder.
#[test]
fn a_junction_in_place_of_the_data_folder_is_refused() {
    let target = scratch("junction-target");
    drop(private::dir(&target).unwrap());
    let junction = scratch("junction");
    run(
        "cmd",
        &[
            "/C",
            "mklink",
            "/J",
            junction.to_str().unwrap(),
            target.to_str().unwrap(),
        ],
    );
    drop(private::dir(&target).expect("the junction's target opens by its own name"));
    assert!(private::dir(&junction).is_err());
    std::fs::remove_dir(&junction).unwrap();
    std::fs::remove_dir_all(&target).unwrap();
}

/// W1 A3: a data folder made under a parent that grants every app container stays
/// private: its protected list carries no container entry, so no agent reaches it.
#[test]
fn a_private_folder_under_a_parent_that_grants_every_container_stays_private() {
    let parent = scratch("granted");
    std::fs::create_dir(&parent).unwrap();
    grant(&parent, "S-1-15-2-1", "F");
    let user = private::user_sid().unwrap();
    let held = private::dir(&parent.join("engine")).unwrap();
    let facts = private::facts(&held).unwrap();
    for entry in dacl::entries(facts.list.as_deref().unwrap()).unwrap() {
        assert!(!dacl::package(entry.sid), "container entry {entry:?}");
    }
    assert_eq!(private::judge(&facts, &user), Ok(()));
    drop(held);
    std::fs::remove_dir_all(&parent).unwrap();
}

/// W1 D1: an existing folder that authenticated users may change is refused as a data
/// folder, the twin of a Unix folder with group or other bits.
#[test]
fn an_existing_folder_other_users_may_change_is_refused() {
    let folder = scratch("open");
    std::fs::create_dir(&folder).unwrap();
    grant(&folder, "S-1-5-11", "M");
    assert!(private::dir(&folder).is_err());
    std::fs::remove_dir_all(&folder).unwrap();
}

/// W1 D1 and F5: a data folder made at a drive root, whose default list lets authenticated
/// users add folders, is private all the same.
#[test]
fn a_private_folder_at_a_drive_root_is_private() {
    let here = std::env::current_dir().unwrap();
    let drive = here.ancestors().last().unwrap().to_path_buf();
    let root = drive.join(format!("crazy-w1-drive-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let user = private::user_sid().unwrap();
    let held = private::dir(&root).unwrap();
    let facts = private::facts(&held).unwrap();
    only_trusted(facts.list.as_deref().unwrap(), &user);
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 D4: a second exclusive lock on the engine lock file is refused while the first is
/// held, so two engines never share one database.
#[test]
fn a_second_lock_on_the_engine_lock_file_is_refused() {
    let root = scratch("lock");
    let held = private::dir(&root).unwrap();
    let path = root.join("engine.engine-lock");
    let first = private::file(&path, true).unwrap();
    private::lock(&first).unwrap();
    let second = private::file(&path, false).unwrap();
    assert!(private::lock(&second).is_err());
    drop((second, first, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 F2 (rv-code4): the twin of `engine_lock_released` says the lock file is free before
/// an engine locks it, held while one does, and free again once that hold is unlocked, and
/// its own probe leaves nothing locked behind.
#[test]
fn the_lock_probe_sees_a_held_engine_lock_and_leaves_none_of_its_own() {
    let root = scratch("probe");
    let held = private::dir(&root).unwrap();
    let path = root.join("engine.engine-lock");
    let engine = private::file(&path, true).unwrap();
    let probe = private::file(&path, false).unwrap();
    assert!(private::free(&probe).unwrap());
    private::lock(&engine).expect("the probe unlocked its own shared lock");
    assert!(!private::free(&probe).unwrap());
    private::unlock(&engine).unwrap();
    assert!(private::free(&probe).unwrap());
    drop((probe, engine, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 D3: two handles on one private file are the same file and two private files are not,
/// the twin of the `dev` and `ino` compare engine storage runs once SQLite has opened.
#[test]
fn same_tells_one_private_file_from_another() {
    let root = scratch("same");
    let held = private::dir(&root).unwrap();
    let one = private::file(&root.join("one.db"), true).unwrap();
    let again = private::file(&root.join("one.db"), false).unwrap();
    let other = private::file(&root.join("other.db"), true).unwrap();
    assert!(private::same(&one, &again).unwrap());
    assert!(!private::same(&one, &other).unwrap());
    drop((other, again, one, held));
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1 D1 (rv-sec3 N4): a private file given a low mandatory label, which lets a low
/// integrity process of the user write it, is refused on open.
#[test]
fn a_private_file_with_a_low_label_is_refused() {
    let root = scratch("label");
    let held = private::dir(&root).unwrap();
    let path = root.join("engine.db");
    drop(private::file(&path, true).unwrap());
    drop(private::file(&path, false).expect("a file with no label opens"));
    run(
        "icacls",
        &[path.to_str().unwrap(), "/setintegritylevel", "low"],
    );
    assert!(private::file(&path, false).is_err());
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}
