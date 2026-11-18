#![cfg(windows)]

use crazyagent_win_sandbox::dacl::{self, ADMINISTRATORS, SYSTEM};
use crazyagent_win_sandbox::private;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, LABEL_SECURITY_INFORMATION, RevertToSelf, SID_AND_ATTRIBUTES,
    SecurityImpersonation, SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_DUPLICATE,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenImpersonation, TokenIntegrityLevel,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_EXECUTE, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_READ, FILE_WRITE_DATA,
    READ_CONTROL, WRITE_OWNER,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_INTEGRITY;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken};

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

/// W1c D5: a private file opens for reading with the identity its writer pinned, a file
/// with a second hard link is refused for reading too, and a held read stops a rename.
#[test]
fn a_read_only_open_keeps_the_identity_and_the_private_check() {
    let root = scratch("read");
    let held = private::dir(&root).unwrap();
    let path = root.join("global.toml");
    let written = private::file(&path, true).unwrap();
    let pinned = private::identity(&written).unwrap();
    drop(written);
    let read = private::read_only(&path).unwrap();
    assert_eq!(private::identity(&read).unwrap(), pinned);
    assert!(std::fs::rename(&path, root.join("moved.toml")).is_err());
    drop(read);
    std::fs::hard_link(&path, root.join("again.toml")).unwrap();
    assert!(private::read_only(&path).is_err());
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}

/// W1c D5: a replace inside a held private folder works, keeps the new file's identity under
/// the old name, and the folder flush after it succeeds; a replace over a target held open
/// by a private handle fails and leaves both files as they were.
#[test]
fn a_replace_in_a_held_folder_works_and_fails_cleanly_over_a_held_target() {
    let root = scratch("replace");
    let held = private::dir(&root).unwrap();
    let target = root.join("global.toml");
    drop(private::file(&target, true).unwrap());
    let staged = root.join(".publication");
    let file = private::file(&staged, true).unwrap();
    let pinned = private::identity(&file).unwrap();
    drop(file);
    std::fs::rename(&staged, &target).unwrap();
    private::sync_folder(&root).unwrap();
    let replaced = private::read_only(&target).unwrap();
    assert_eq!(private::identity(&replaced).unwrap(), pinned);
    drop(private::file(&staged, true).unwrap());
    assert!(std::fs::rename(&staged, &target).is_err());
    assert_eq!(private::identity(&replaced).unwrap(), pinned);
    assert!(staged.is_file());
    drop((replaced, held));
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

/// W1c D5 (F36): an existing empty folder of this user that other users may change is made
/// private again by repair, and a junction in place of such a folder is refused and its
/// target left as it was.
#[test]
fn repair_makes_an_open_folder_of_this_user_private_and_refuses_a_junction() {
    let folder = scratch("repair");
    std::fs::create_dir(&folder).unwrap();
    grant(&folder, "S-1-5-11", "M");
    assert!(private::dir(&folder).is_err());
    let user = private::user_sid().unwrap();
    let held = private::repair(&folder).unwrap();
    let facts = private::facts(&held).unwrap();
    only_trusted(facts.list.as_deref().unwrap(), &user);
    assert_eq!(private::judge_folder(&facts, &user), Ok(()));
    drop(held);
    drop(private::dir(&folder).unwrap());
    let target = scratch("repair-target");
    std::fs::create_dir(&target).unwrap();
    grant(&target, "S-1-5-11", "M");
    let junction = scratch("repair-junction");
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
    assert!(private::repair(&junction).is_err());
    assert!(private::dir(&target).is_err());
    std::fs::remove_dir(&junction).unwrap();
    std::fs::remove_dir_all(&target).unwrap();
    std::fs::remove_dir_all(&folder).unwrap();
}

/// #512 r2 (rv-sec2 F1, rv-code4 F2): a private folder whose inherit-only entry hands
/// authenticated users read on what is made in it is refused by `dir`, so SQLite's side
/// files never start open to them.
#[test]
fn a_folder_handing_other_users_access_to_what_is_made_in_it_is_refused() {
    let folder = scratch("handed");
    drop(private::dir(&folder).unwrap());
    let entry = "*S-1-5-11:(OI)(IO)(R)";
    run("icacls", &[folder.to_str().unwrap(), "/grant", entry]);
    assert!(private::dir(&folder).is_err());
    std::fs::remove_dir_all(&folder).unwrap();
}

/// #512 r2: repair refuses a folder that already holds a file, which keeps its own explicit
/// entries whatever the folder's new list says, and changes nothing, so `dir` still refuses
/// the folder and a second repair refuses it again.
#[test]
fn repair_refuses_a_folder_that_already_holds_a_file_and_changes_nothing() {
    let folder = scratch("repair-held");
    std::fs::create_dir(&folder).unwrap();
    grant(&folder, "S-1-5-11", "M");
    let kept = folder.join("kept");
    std::fs::write(&kept, "x").unwrap();
    run(
        "icacls",
        &[kept.to_str().unwrap(), "/grant", "*S-1-5-11:(R)"],
    );
    assert!(private::repair(&folder).is_err());
    assert!(private::dir(&folder).is_err());
    assert!(private::repair(&folder).is_err());
    assert_eq!(std::fs::read_to_string(&kept).unwrap(), "x");
    std::fs::remove_dir_all(&folder).unwrap();
}

/// The medium label with no-write-up, no-read-up and no-execute-up every store carries
/// (V244-WIN-STORE-LABEL-NX).
const MEDIUM_FULL: Option<(u32, u32)> = Some((0x2000, 0x7));
/// The medium label with no-write-up and no-read-up alone that 2.4.3 gave the stores.
const EARLIER: Option<(u32, u32)> = Some((0x2000, 0x3));

/// The mandatory label that applies to an open folder or file itself. Input: the object.
/// Output: the level and policy, None when it has none.
fn own_label(object: &std::fs::File) -> Option<(u32, u32)> {
    let facts = private::facts(object).unwrap();
    facts.label.and_then(|sacl| dacl::label(&sacl).unwrap())
}

/// #524 r3 (rv-code4 F2, leader 05:45), V244-WIN-STORE-LABEL-NX: the database folder, the
/// data root, hands the full medium label to the files made directly in it, engine.db and
/// SQLite's side files. It carries none itself and hands none to a folder made in it, which
/// may be a root granted to an AppContainer, nor to what is made there.
#[test]
fn the_database_folder_labels_only_the_files_made_directly_in_it() {
    let root = scratch("database-label");
    let held = private::store_dir(&root, private::Store::Database).unwrap();
    assert_eq!(own_label(&held), None);
    drop(private::file(&root.join("engine.db"), true).unwrap());
    std::fs::write(root.join("engine.db-wal"), "x").unwrap();
    for name in ["engine.db", "engine.db-wal"] {
        let file = std::fs::File::open(root.join(name)).unwrap();
        assert_eq!(own_label(&file), MEDIUM_FULL, "{name}");
    }
    let made = root.join("setup");
    let plain = root.join("check");
    std::fs::create_dir(&plain).unwrap();
    for folder in [private::dir(&made).unwrap(), private::dir(&plain).unwrap()] {
        assert_eq!(own_label(&folder), None);
    }
    std::fs::write(made.join("inside.txt"), "x").unwrap();
    let inside = std::fs::File::open(made.join("inside.txt")).unwrap();
    assert_eq!(own_label(&inside), None);
    drop((held, inside));
    std::fs::remove_dir_all(&root).unwrap();
}

/// #524 r3 (rv-code4 F2, leader 05:45), V244-WIN-STORE-LABEL-NX: a secret store folder
/// (account keys, MCP secrets) carries the full medium label itself and hands it to every
/// file and folder made in it.
#[test]
fn a_secret_folder_labels_itself_and_all_made_in_it() {
    let root = scratch("secret-label");
    let held = private::store_dir(&root, private::Store::Secret).unwrap();
    assert_eq!(own_label(&held), MEDIUM_FULL);
    let file = private::file(&root.join("secret-1"), true).unwrap();
    assert_eq!(own_label(&file), MEDIUM_FULL);
    let below = private::dir(&root.join("below")).unwrap();
    assert_eq!(own_label(&below), MEDIUM_FULL);
    drop((held, file, below));
    std::fs::remove_dir_all(&root).unwrap();
}

/// #524 r3 (rv-code4 F2, leader 05:45): a private folder or file outside the secret stores
/// carries no label of its own, as before #512 r2's N1.
#[test]
fn a_private_folder_and_file_carry_no_label_of_their_own() {
    let root = scratch("no-label");
    let held = private::dir(&root).unwrap();
    assert_eq!(own_label(&held), None);
    let file = private::file(&root.join("plain"), true).unwrap();
    assert_eq!(own_label(&file), None);
    drop((held, file));
    std::fs::remove_dir_all(&root).unwrap();
}

/// V244-WIN-SAVE-CHANNEL-ROOT-LABEL (leader 06:32, #524 r3 sec LOW): a plain private folder
/// already there takes its store's label once it is judged private, and so does a file
/// already in it, so a store gets its label whatever made it first; a folder another account
/// owns is refused as before and keeps no label.
#[test]
fn an_existing_plain_folder_takes_its_store_label_only_once_judged_private() {
    let root = scratch("relabel");
    drop(private::dir(&root).unwrap());
    let inside = root.join("made-before");
    drop(private::file(&inside, true).unwrap());
    let held = private::store_dir(&root, private::Store::Secret).unwrap();
    assert_eq!(own_label(&held), MEDIUM_FULL);
    assert_eq!(
        own_label(&private::file(&inside, false).unwrap()),
        MEDIUM_FULL
    );
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
    let foreign = scratch("relabel-foreign");
    drop(private::dir(&foreign).unwrap());
    run(
        "icacls",
        &[foreign.to_str().unwrap(), "/setowner", "*S-1-5-32-545"],
    );
    assert!(private::store_dir(&foreign, private::Store::Secret).is_err());
    let seen = std::fs::OpenOptions::new()
        .access_mode(windows_sys::Win32::Storage::FileSystem::READ_CONTROL)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
        .open(&foreign)
        .unwrap();
    assert_eq!(own_label(&seen), None);
    drop(seen);
    std::fs::remove_dir_all(&foreign).unwrap();
}

/// The low mandatory level SID, S-1-16-4096, in words so Windows reads it aligned.
const LOW_LEVEL: [u32; 3] = [0x0000_0101, 0x1000_0000, 0x0000_1000];

/// A copy of this process's token at impersonation level lowered to the low mandatory level,
/// the level a confined agent runs at. Output: the token.
fn low_token() -> OwnedHandle {
    let mut process = null_mut();
    let opened = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut process,
        )
    };
    assert_ne!(opened, 0, "{}", io::Error::last_os_error());
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut copy = null_mut();
    let made = unsafe {
        DuplicateTokenEx(
            process.as_raw_handle(),
            TOKEN_ALL_ACCESS,
            null(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut copy,
        )
    };
    assert_ne!(made, 0, "{}", io::Error::last_os_error());
    let token = unsafe { OwnedHandle::from_raw_handle(copy) };
    let mut level = LOW_LEVEL;
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: level.as_mut_ptr().cast(),
            Attributes: SE_GROUP_INTEGRITY as u32,
        },
    };
    let size = size_of::<TOKEN_MANDATORY_LABEL>() + size_of_val(&level);
    let set = unsafe {
        SetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            (&raw const label).cast(),
            size as u32,
        )
    };
    assert_ne!(set, 0, "{}", io::Error::last_os_error());
    token
}

/// Runs `work` on this thread while it impersonates `token`, then reverts; a failed revert
/// aborts the process, so no later code runs lowered. Inputs: the impersonation token and
/// the work. Output: what the work returned.
fn as_principal<T>(token: &OwnedHandle, work: impl FnOnce() -> T) -> T {
    let set = unsafe { SetThreadToken(null(), token.as_raw_handle()) };
    assert_ne!(set, 0, "{}", io::Error::last_os_error());
    let result = work();
    if unsafe { RevertToSelf() } == 0 {
        std::process::abort();
    }
    result
}

/// Gives an object a medium label with `policy` and inheritance `flags`, as an earlier
/// version or another tool may have left it. Inputs: the path, whether it is a folder, the
/// policy and the flags.
fn set_label(path: &Path, folder: bool, policy: u32, flags: u8) {
    let held = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_OWNER)
        .custom_flags(if folder {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        })
        .open(path)
        .unwrap();
    let bytes = dacl::labelled(0x2000, policy, flags);
    let mut words = vec![0_u32; bytes.len().div_ceil(4)];
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr().cast(), bytes.len())
    };
    let status = unsafe {
        SetSecurityInfo(
            held.as_raw_handle(),
            SE_FILE_OBJECT,
            LABEL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            null(),
            words.as_ptr().cast(),
        )
    };
    assert_eq!(status, 0, "{}", io::Error::from_raw_os_error(status as i32));
}

/// Asserts a lower integrity process of the user can open `path` with no sharing neither to
/// read, for execute only, for delete only nor to write, any of which would block the
/// engine's later opens and renames, while the engine opens it each way (#562 F1, #568 N1).
/// Input: the file.
fn closed_to_low_holders(path: &Path) {
    let open = |access: u32| {
        std::fs::OpenOptions::new()
            .access_mode(access)
            .share_mode(0)
            .open(path)
    };
    let low = low_token();
    for (hold, access) in [
        ("read", FILE_GENERIC_READ),
        ("execute only", FILE_EXECUTE),
        ("delete only", DELETE),
        ("write only", FILE_WRITE_DATA),
    ] {
        drop(open(access).unwrap_or_else(|error| panic!("the engine's {hold} open: {error}")));
        let lowered = as_principal(&low, || open(access));
        println!(
            "a low-il {hold} open of {}: {:?}",
            path.display(),
            lowered
                .as_ref()
                .map(|_| ())
                .map_err(io::Error::raw_os_error)
        );
        assert!(
            lowered.is_err(),
            "a low-il process holds {path:?} for {hold}"
        );
    }
}

/// V244-WIN-STORE-LABEL-NX (leader 09:13): engine.db in the database folder and a file in a
/// secret store are closed to a lower integrity process of the user, to read and for execute
/// only with no sharing, so a confined agent cannot hold either and block the engine.
#[test]
fn a_low_integrity_holder_cannot_open_engine_db_or_a_secret_store_file() {
    let database = scratch("nx-database");
    let held = private::store_dir(&database, private::Store::Database).unwrap();
    let engine_db = database.join("engine.db");
    drop(private::file(&engine_db, true).unwrap());
    assert_eq!(
        own_label(&private::file(&engine_db, false).unwrap()),
        MEDIUM_FULL
    );
    closed_to_low_holders(&engine_db);
    drop(held);
    let secret = scratch("nx-secret");
    let held_secret = private::store_dir(&secret, private::Store::Secret).unwrap();
    let key = secret.join("secret-1");
    drop(private::file(&key, true).unwrap());
    assert_eq!(own_label(&private::file(&key, false).unwrap()), MEDIUM_FULL);
    closed_to_low_holders(&key);
    drop(held_secret);
    std::fs::remove_dir_all(&database).unwrap();
    std::fs::remove_dir_all(&secret).unwrap();
}

/// V244-WIN-STORE-LABEL-NX (leader 09:13, earlier installs): a store folder an earlier
/// version labelled 0x3, holding engine.db or a key made under that label, takes the full
/// label when it is next opened, and so do the files already in it.
#[test]
fn an_earlier_store_label_is_raised_at_open_with_the_files_it_holds() {
    for (store, flags, name) in [
        (private::Store::Database, 0x0D, "engine.db"),
        (private::Store::Secret, 0x03, "secret-1"),
    ] {
        let root = scratch(&format!("nx-earlier-{name}"));
        drop(private::store_dir(&root, store).unwrap());
        set_label(&root, true, 0x3, flags);
        let inside = root.join(name);
        std::fs::write(&inside, "x").unwrap();
        let before = std::fs::File::open(&inside).unwrap();
        assert_eq!(own_label(&before), EARLIER, "{name} before");
        drop(before);
        let held = private::store_dir(&root, store).unwrap();
        let after = private::file(&inside, false).unwrap();
        assert_eq!(own_label(&after), MEDIUM_FULL, "{name} after");
        drop(after);
        closed_to_low_holders(&inside);
        drop(held);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// V244-WIN-STORE-LABEL-NX (leader 09:14): a private file carrying a medium label weaker
/// than the full policy, no-write-up alone or with no-read-up, is refused on open, since a
/// lower integrity process may read it or hold it; with the full policy it opens.
#[test]
fn a_private_file_with_a_weaker_label_is_refused() {
    let root = scratch("weaker-label");
    let held = private::dir(&root).unwrap();
    let path = root.join("plain");
    drop(private::file(&path, true).unwrap());
    for policy in [0x1, 0x3] {
        set_label(&path, false, policy, 0);
        assert!(private::file(&path, false).is_err(), "{policy}");
        assert!(private::read_only(&path).is_err(), "{policy}");
    }
    set_label(&path, false, 0x7, 0);
    drop(private::file(&path, false).expect("the full label opens"));
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}
