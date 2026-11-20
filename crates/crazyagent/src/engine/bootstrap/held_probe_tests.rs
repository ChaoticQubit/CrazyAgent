use crazyagent_win_sandbox::private;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const STREAM: &str = ":crazyagent.lock";

/// Appends one observed outcome to the probe record the farm prints, at the path in
/// `CRAZYAGENT_PROBE_OUT`, or in the temp folder without it. Input: the line.
fn note(line: &str) {
    let path = std::env::var_os("CRAZYAGENT_PROBE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("held-probes.txt"));
    let mut out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(out, "{line}").unwrap();
}

/// Describes an I/O result as its kind and Windows error number. Input: the result.
/// Output: the text.
fn told<T>(result: &std::io::Result<T>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => format!("{:?} os={:?}", error.kind(), error.raw_os_error()),
    }
}

/// Describes a lock attempt as ok, would-block or the error. Input: the result. Output: the
/// text.
fn locked(result: Result<(), std::fs::TryLockError>) -> String {
    match result {
        Ok(()) => "ok".to_owned(),
        Err(std::fs::TryLockError::WouldBlock) => "would-block".to_owned(),
        Err(std::fs::TryLockError::Error(error)) => {
            format!("{:?} os={:?}", error.kind(), error.raw_os_error())
        }
    }
}

/// A private scratch root made with W1, as the held rows use. Output: the temp guard and the
/// root's plain path.
fn private_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = crate::sandbox::canonical(temp.path()).unwrap().join("root");
    drop(private::dir(&root).unwrap());
    (temp, root)
}

/// Writes a new W1 private file. Inputs: the path and the bytes.
fn private_file(path: &Path, bytes: &[u8]) {
    let mut file = private::file(path, true).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

/// Opens or makes the lock stream of an entry by its path, sharing read and write only.
/// Input: the entry's path. Output: the open result.
fn stream(path: &Path) -> std::io::Result<File> {
    let mut named = path.as_os_str().to_owned();
    named.push(STREAM);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(SHARE_READ_WRITE)
        .open(PathBuf::from(named))
}

/// Counts a folder's entries as a listing sees them. Input: the folder. Output: the count.
fn count(folder: &Path) -> usize {
    std::fs::read_dir(folder).unwrap().count()
}

/// D4r probe P1 (rv-sec3 #716 C5): records whether a held W1 folder handle takes a shared
/// and an exclusive byte-range lock.
#[test]
fn probe_p1_a_range_lock_on_a_held_folder_handle() {
    let (_temp, root) = private_root();
    let folder = private::open_dir(&root).unwrap();
    note(&format!(
        "P1 folder try_lock_shared: {}",
        locked(folder.try_lock_shared())
    ));
    let _ = folder.unlock();
    note(&format!(
        "P1 folder try_lock: {}",
        locked(folder.try_lock())
    ));
    note(&format!(
        "P1 folder W1 lock: {}",
        told(&private::lock(&folder))
    ));
}

/// D4r probe P2 (C5): records whether a named lock stream opens on a folder W1 holds, carries
/// the folder's file id, adds no listed entry, passes W1's check, and takes shared and
/// exclusive locks that a second stream handle meets.
#[test]
fn probe_p2_a_lock_stream_on_a_held_folder() {
    let (_temp, root) = private_root();
    let path = root.join("profile");
    let folder = private::dir(&path).unwrap();
    let before = count(&root);
    let first = stream(&path);
    note(&format!(
        "P2 stream open while the folder is held: {}",
        told(&first)
    ));
    let Ok(first) = first else { return };
    let same_id = private::identity(&first).ok() == private::identity(&folder).ok();
    note(&format!(
        "P2 stream identity equals the folder's: {same_id}"
    ));
    note(&format!(
        "P2 listing count before {before} after {}",
        count(&root)
    ));
    note(&format!("P2 listing inside the folder: {}", count(&path)));
    note(&format!(
        "P2 W1 check on the stream: {}",
        told(&private::check(&first))
    ));
    let second = stream(&path).unwrap();
    note(&format!(
        "P2 first try_lock_shared: {}",
        locked(first.try_lock_shared())
    ));
    note(&format!(
        "P2 second try_lock_shared: {}",
        locked(second.try_lock_shared())
    ));
    let _ = second.unlock();
    note(&format!(
        "P2 second try_lock beside a shared: {}",
        locked(second.try_lock())
    ));
    let _ = first.unlock();
    note(&format!(
        "P2 second try_lock alone: {}",
        locked(second.try_lock())
    ));
    note(&format!(
        "P2 first try_lock_shared beside it: {}",
        locked(first.try_lock_shared())
    ));
    let renamed = std::fs::rename(&path, root.join("moved"));
    note(&format!("P2 rename of the held folder: {}", told(&renamed)));
    drop(folder);
    let renamed = std::fs::rename(&path, root.join("moved"));
    note(&format!(
        "P2 rename with only stream handles open: {}",
        told(&renamed)
    ));
}

/// D4r probe P3 (C4): records whether an exclusive lock on a manifest's lock stream leaves a
/// fresh W1 read of the manifest's bytes alone while a second stream lock is refused.
#[test]
fn probe_p3_a_stream_lock_leaves_reads_of_the_file() {
    let (_temp, root) = private_root();
    let path = root.join("identity.json");
    private_file(&path, b"{}");
    let holder = stream(&path).unwrap();
    note(&format!(
        "P3 exclusive on the stream: {}",
        locked(holder.try_lock())
    ));
    let mut bytes = Vec::new();
    let read = private::read_only(&path).and_then(|mut file| file.read_to_end(&mut bytes));
    note(&format!(
        "P3 fresh W1 read during it: {} bytes {:?}",
        told(&read),
        bytes
    ));
    let other = stream(&path).unwrap();
    note(&format!(
        "P3 second stream try_lock_shared: {}",
        locked(other.try_lock_shared())
    ));
    note(&format!("P3 listing count: {}", count(&root)));
    let _ = holder.unlock();
}

/// Replace probe P4 (NOTE-0548): records whether std's rename replaces a file whose only open
/// handle is a locked lock stream, and whether the lock then follows the old file or the new.
#[test]
fn probe_p4_a_replace_over_a_file_whose_stream_is_locked() {
    let (_temp, root) = private_root();
    let target = root.join("identity.json");
    let staged = root.join("staged.json");
    private_file(&target, b"old");
    private_file(&staged, b"new");
    let holder = stream(&target).unwrap();
    note(&format!(
        "P4 exclusive on the target's stream: {}",
        locked(holder.try_lock())
    ));
    let replaced = std::fs::rename(&staged, &target);
    note(&format!("P4 replace by rename: {}", told(&replaced)));
    note(&format!(
        "P4 target bytes: {:?}",
        std::fs::read(&target).ok()
    ));
    let fresh = stream(&target);
    note(&format!("P4 new stream open: {}", told(&fresh)));
    if let Ok(fresh) = fresh {
        note(&format!(
            "P4 new stream try_lock: {}",
            locked(fresh.try_lock())
        ));
    }
    let _ = holder.unlock();
}

/// Replace probe P5 (NOTE-0548): records whether std's rename replaces a file W1 holds open
/// without sharing delete, then with that handle closed, and with a handle sharing delete.
#[test]
fn probe_p5_a_replace_over_a_held_file() {
    let (_temp, root) = private_root();
    let target = root.join("config.json");
    let staged = root.join("staged.json");
    private_file(&target, b"old");
    private_file(&staged, b"new");
    let held = private::read_only(&target).unwrap();
    note(&format!(
        "P5 replace over a W1 held target: {}",
        told(&std::fs::rename(&staged, &target))
    ));
    drop(held);
    let sharing = OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .open(&target)
        .unwrap();
    let replaced = std::fs::rename(&staged, &target);
    note(&format!(
        "P5 replace over a target held sharing delete: {}",
        told(&replaced)
    ));
    let mut kept = Vec::new();
    let read = (&sharing).read_to_end(&mut kept);
    note(&format!(
        "P5 the old handle still reads {} {:?}",
        told(&read),
        kept
    ));
    note(&format!(
        "P5 target bytes: {:?}",
        std::fs::read(&target).ok()
    ));
}

/// Rename probe P6 (#716 C7): records whether a folder renames while a child is held by W1,
/// first with no handle on the folder, then with one that shares delete.
#[test]
fn probe_p6_a_folder_rename_with_a_held_child() {
    let (_temp, root) = private_root();
    let folder = root.join("staging");
    drop(private::dir(&folder).unwrap());
    private_file(&folder.join("identity.json"), b"{}");
    let child = private::read_only(&folder.join("identity.json")).unwrap();
    let renamed = std::fs::rename(&folder, root.join("moved"));
    note(&format!(
        "P6 folder rename with a held file child: {}",
        told(&renamed)
    ));
    let open = OpenOptions::new()
        .read(true)
        .share_mode(SHARE_ALL)
        .custom_flags(BACKUP_SEMANTICS)
        .open(&folder)
        .unwrap();
    let renamed = std::fs::rename(&folder, root.join("moved"));
    note(&format!(
        "P6 again with a folder handle sharing delete: {}",
        told(&renamed)
    ));
    drop(child);
    let renamed = std::fs::rename(&folder, root.join("moved"));
    note(&format!("P6 after the child closes: {}", told(&renamed)));
    drop(open);
}

/// Makes `link` a junction to the folder `target` with `mklink /J`. Inputs: both paths.
fn junction(link: &Path, target: &Path) {
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

/// Opens an entry as itself, never following a link at its name, with delete access, the
/// open S9b's remove by handle needs (NOTE-0600 item 8). Input: the path. Output: the open
/// result.
fn unfollowed_deletable(path: &Path) -> std::io::Result<File> {
    const DELETE: u32 = 0x0001_0000;
    const READ_CONTROL: u32 = 0x0002_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .access_mode(DELETE | READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(SHARE_ALL)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
        .open(path)
}

/// Held API probe P7 (NOTE-0600 items 5 and 8): records the place W1 reads from a junction
/// opened as itself, which a bound remove compares, and whether a delete through that handle
/// removes the junction only and keeps its target.
#[test]
fn probe_p7_a_junction_opened_as_itself_is_placed_and_removed_alone() {
    let (_temp, root) = private_root();
    let target = root.join("target");
    drop(private::dir(&target).unwrap());
    private_file(&target.join("kept"), b"kept");
    let link = root.join("link");
    junction(&link, &target);
    let opened = unfollowed_deletable(&link);
    note(&format!(
        "P7 open of the junction as itself: {}",
        told(&opened)
    ));
    let Ok(opened) = opened else { return };
    let place = private::place(&opened).map(|units| String::from_utf16_lossy(&units));
    note(&format!("P7 place of the junction handle: {place:?}"));
    note(&format!("P7 link path: {}", link.display()));
    note(&format!(
        "P7 W1 delete through it: {}",
        told(&private::delete(&opened))
    ));
    drop(opened);
    note(&format!(
        "P7 link gone: {} target kept: {}",
        !link.exists(),
        target.join("kept").is_file()
    ));
}
