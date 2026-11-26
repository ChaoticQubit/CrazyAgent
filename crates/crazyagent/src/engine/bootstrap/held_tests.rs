use super::{Child, Folder, HeldError, Judged, Kind, Name, lock, unlock};
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};

/// A constant name, as custody's own names are. Input: the text. Output: the name.
fn name(text: &'static str) -> Name {
    Name::constant(text)
}

/// A private temp folder, as an engine root is: 0700 and owned on Unix, a W1 private folder
/// on Windows. Output: the temp guard and the folder's canonical path.
fn private_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = kernel_sandbox::paths::canonical(temp.path())
        .unwrap()
        .join("root");
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
        let status = kernel_processes::spawn_gate::status(
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
        let status = kernel_processes::spawn_gate::status(
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
/// `Missing`. On Windows the held folder cannot be renamed meanwhile.
#[test]
fn a_held_folder_makes_opens_and_pins_its_children() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let made = held.make_dir(&name("a")).unwrap();
    let opened = held.open_dir(&name("a")).unwrap();
    assert_eq!(made.pin(), opened.pin());
    assert_ne!(held.pin(), opened.pin());
    held.verify(&name("a"), opened.file(), opened.pin(), true)
        .unwrap();
    private_file(&root.join("a").join("f"), b"bytes");
    let (file, pin) = opened.open_read(&name("f")).unwrap();
    opened.verify(&name("f"), &file, pin, false).unwrap();
    assert_ne!(pin, opened.pin());
    assert_eq!(
        held.open_dir(&name("missing")).err().unwrap(),
        HeldError::Missing
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
    drop(held.make_dir(&name("target")).unwrap());
    link_folder(&root.join("link"), &root.join("target"));
    assert_eq!(
        held.open_dir(&name("link")).err().unwrap(),
        HeldError::Unsafe
    );
    let first = held.make_dir(&name("swapped")).unwrap();
    let pin = first.pin();
    drop(first);
    let kept = hold_sharing_delete(&root.join("swapped"));
    std::fs::rename(root.join("swapped"), root.join("old")).unwrap();
    drop(held.make_dir(&name("swapped")).unwrap());
    assert_eq!(
        held.verify(&name("swapped"), &kept, pin, true).unwrap_err(),
        HeldError::Unsafe
    );
    drop(held.make_dir(&name("open")).unwrap());
    open_to_everyone(&root.join("open"));
    assert_eq!(
        held.open_dir(&name("open")).err().unwrap(),
        HeldError::Unsafe
    );
    private_file(&root.join("f"), b"bytes");
    std::fs::hard_link(root.join("f"), root.join("g")).unwrap();
    assert_eq!(held.open_read(&name("f")).err().unwrap(), HeldError::Unsafe);
    held.open_dir(&name("target")).unwrap();
}

/// N2a row 3: a constant name that is not one plain component, or holds a colon, which
/// Windows reads as a stream, trips `Name::constant`'s assertion before any open, and nothing
/// is made.
#[test]
fn a_name_that_is_not_one_plain_component_trips_the_layer() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let mut names = vec![
        "",
        ".",
        "..",
        "a/b",
        "a:b",
        "ab:c",
        "name::$DATA",
        "a.",
        "a ",
        "NUL",
        "con.txt",
    ];
    if cfg!(windows) {
        names.push("a\\b");
    }
    for text in names {
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            held.open_dir(&name(text)).is_ok()
        }));
        assert!(opened.is_err(), "{text:?} reached an open");
        let made = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            held.make_dir(&name(text)).is_ok()
        }));
        assert!(made.is_err(), "{text:?} reached a make");
    }
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
}

/// N2a row 4: shared locks meet, an exclusive one is `InUse` beside them, a
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
    assert_eq!(lock(&two, false).unwrap_err(), HeldError::InUse);
    unlock(&one);
    lock(&two, false).unwrap();
    assert_eq!(lock(&one, true).unwrap_err(), HeldError::InUse);
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

/// N2a row 3c (rv-sec3 #716 C2): making a folder is create-only, the twin of `mkdirat`, so a
/// private folder already at the name is refused as unsafe and never adopted, and it stays
/// the same folder.
#[test]
fn making_a_folder_over_an_existing_one_is_refused_and_keeps_it() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let first = held.make_dir(&name("home")).unwrap().pin();
    assert_eq!(
        held.make_dir(&name("home")).err().unwrap(),
        HeldError::Unsafe
    );
    assert_eq!(held.open_dir(&name("home")).unwrap().pin(), first);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
}

/// N2a row 3d (rv-sec3 #716 C1): while a chain is held, the folder above its root cannot be
/// renamed either, so the path the held root was read at keeps naming it.
#[cfg(windows)]
#[test]
fn the_folder_above_a_held_chain_cannot_be_renamed() {
    let (temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let child = held.make_dir(&name("a")).unwrap();
    let above = root.parent().unwrap().to_path_buf();
    let moved = temp.path().with_extension("moved");
    assert!(std::fs::rename(&above, &moved).is_err());
    assert!(root.is_dir());
    assert!(!moved.exists());
    drop(child);
    drop(held);
}

/// N2a row 3e (rv-sec3 #716 C1): a root reached through a junction is held at its real
/// place, so retargeting that junction later leaves every open and make in the held tree.
#[cfg(windows)]
#[test]
fn retargeting_a_junction_above_the_root_leaves_opens_in_the_held_tree() {
    let (_temp, root) = private_root();
    let real = root.join("real");
    let other = root.join("other");
    for folder in [&real, &other, &real.join("anchor"), &other.join("anchor")] {
        drop(crazyagent_win_sandbox::private::dir(folder).unwrap());
    }
    let link = root.join("link");
    link_folder(&link, &real);
    let held = Folder::root(&link.join("anchor")).unwrap();
    std::fs::remove_dir(&link).unwrap();
    link_folder(&link, &other);
    let made = held.make_dir(&name("b")).unwrap();
    assert!(real.join("anchor").join("b").is_dir());
    assert!(!other.join("anchor").join("b").exists());
    assert_eq!(held.open_dir(&name("b")).unwrap().pin(), made.pin());
}

/// Lets every account read one file: mode 0644 on Unix, an Everyone read entry on Windows.
/// Input: the file.
fn file_to_everyone(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    #[cfg(windows)]
    {
        let status = kernel_processes::spawn_gate::status(
            std::process::Command::new("icacls")
                .arg(path)
                .args(["/grant", "*S-1-1-0:(R)"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .unwrap();
        assert!(status.success());
    }
}

/// Sorts a listing by name, so a row compares it whatever order the folder gives. Input: the
/// listing. Output: each child's name and kind, sorted.
fn sorted(children: Vec<Child>) -> Vec<(String, Kind)> {
    let mut named = children
        .into_iter()
        .map(|child| (child.name.into_string().unwrap(), child.kind))
        .collect::<Vec<_>>();
    named.sort_by(|one, other| one.0.cmp(&other.0));
    named
}

/// N2a r2 row 5 (DESIGN-0553 rule 3, rv-sec3 #725 U6): `Name::checked` refuses, the same on
/// every system, an empty name, `.`, `..`, a separator, a colon, NUL and other controls,
/// Windows' other invalid characters, a trailing dot or space, every device name with or
/// without an extension in any case, and a name over 255 UTF-16 units, each as `Unsafe`,
/// while a space inside a name, a non-ASCII letter and a 255-unit name pass.
#[test]
fn a_checked_name_refuses_every_unsafe_spelling_and_keeps_plain_ones() {
    let long = "a".repeat(256);
    let refused = [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a:b",
        "x::$DATA",
        "a\0b",
        "a\u{1}b",
        "a\u{7f}b",
        "a<b",
        "a>b",
        "a\"b",
        "a|b",
        "a?b",
        "a*b",
        "a.",
        "a ",
        "CON",
        "con.txt",
        "Prn",
        "AUX.json",
        "nul",
        "COM1",
        "com0.log",
        "LPT9",
        "COM\u{b9}",
        "lpt\u{b2}.txt",
        "CONIN$",
        "conout$",
        "CON .txt",
        long.as_str(),
    ];
    for text in refused {
        assert_eq!(
            Name::checked(OsStr::new(text)).err(),
            Some(HeldError::Unsafe),
            "{text:?}"
        );
    }
    let widest = "a".repeat(255);
    for text in [
        "a b",
        "skill-\u{fc}.md",
        "COM10",
        "console",
        "nul-free",
        widest.as_str(),
    ] {
        let checked = Name::checked(OsStr::new(text)).unwrap();
        assert_eq!(checked.as_os_str(), OsStr::new(text));
    }
}

/// N2a r2 row 6 (DESIGN-0553, rv-sec3 #725 closures): making a file is create-only and
/// private, the twin of `O_CREAT | O_EXCL` with 0600, so a second make of the name is
/// `Unsafe` and leaves the first file's bytes; `open_write` opens the made file pinned, and
/// refuses a hard-linked file and a file every account may read as `Unsafe`, and a missing
/// name as `Missing`.
#[test]
fn making_a_file_is_create_only_and_private_and_open_write_judges_it() {
    use std::io::{Read, Seek, Write};
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let (mut made, pin) = held.make_file(&name("state.json")).unwrap();
    made.write_all(b"first").unwrap();
    made.sync_all().unwrap();
    drop(made);
    assert_eq!(
        held.make_file(&name("state.json")).err(),
        Some(HeldError::Unsafe)
    );
    let (mut opened, opened_pin) = held.open_write(&name("state.json")).unwrap();
    assert_eq!(opened_pin, pin);
    let mut bytes = Vec::new();
    opened.rewind().unwrap();
    opened.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"first");
    held.verify(&name("state.json"), &opened, pin, false)
        .unwrap();
    drop(opened);
    private_file(&root.join("linked"), b"bytes");
    std::fs::hard_link(root.join("linked"), root.join("second")).unwrap();
    assert_eq!(
        held.open_write(&name("linked")).err(),
        Some(HeldError::Unsafe)
    );
    private_file(&root.join("open"), b"bytes");
    file_to_everyone(&root.join("open"));
    assert_eq!(
        held.open_write(&name("open")).err(),
        Some(HeldError::Unsafe)
    );
    assert_eq!(
        held.open_write(&name("missing")).err(),
        Some(HeldError::Missing)
    );
}

/// N2a r2 row 7 (DESIGN-0553, rv-sec3 #725 U4): a listing names a file, a folder and a link
/// (a symbolic link on Unix, a junction on Windows) as `File`, `Folder` and `Link`, never
/// lists what the link leads to, and more entries than the bound is `Full`.
#[test]
fn a_listing_names_files_folders_and_links_without_following_and_is_bounded() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let folder = held.make_dir(&name("folder")).unwrap();
    private_file(&root.join("folder").join("inner"), b"bytes");
    private_file(&root.join("file"), b"bytes");
    link_folder(&root.join("link"), &root.join("folder"));
    let listed = sorted(held.list(8).unwrap());
    assert_eq!(
        listed,
        vec![
            ("file".to_owned(), Kind::File),
            ("folder".to_owned(), Kind::Folder),
            ("link".to_owned(), Kind::Link),
        ]
    );
    assert_eq!(held.list(2).err(), Some(HeldError::Full));
    let inside = sorted(folder.list(1).unwrap());
    assert_eq!(inside, vec![("inner".to_owned(), Kind::File)]);
}

/// N2a r2 row 8 (DESIGN-0553, rv-sec3 #725 U5): a judged remove takes a pinned file and an
/// empty pinned folder, refuses a wrong pin as `Unsafe` and keeps the entry, and on Windows
/// an entry another W1 handle holds is `InUse` and stays.
#[test]
fn removing_an_entry_needs_its_pin_and_leaves_a_held_one() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let (file, pin) = held.make_file(&name("gone")).unwrap();
    drop(file);
    held.remove(&name("gone"), pin).unwrap();
    assert!(!root.join("gone").exists());
    let (file, pin) = held.make_file(&name("kept")).unwrap();
    drop(file);
    let wrong = super::Pin {
        device: pin.device,
        inode: pin.inode.wrapping_add(1),
    };
    assert_eq!(
        held.remove(&name("kept"), wrong).err(),
        Some(HeldError::Unsafe)
    );
    assert!(root.join("kept").is_file());
    let folder = held.make_dir(&name("empty")).unwrap();
    let folder_pin = folder.pin();
    drop(folder);
    held.remove(&name("empty"), folder_pin).unwrap();
    assert!(!root.join("empty").exists());
    #[cfg(windows)]
    {
        let (other, other_pin) = held.open_write(&name("kept")).unwrap();
        assert_eq!(
            held.remove(&name("kept"), other_pin).err(),
            Some(HeldError::InUse)
        );
        assert!(root.join("kept").is_file());
        drop(other);
    }
    held.remove(&name("kept"), pin).unwrap();
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
}

/// N2a r2 row 9 (rv-sec3 #725 U5): errors map by cause, so a busy entry is never read as
/// tampering: a missing name is `Missing`, a pin that is not the held entry's is `Unsafe`,
/// and on Windows a file another handle holds without sharing is `InUse`.
#[test]
fn errors_map_by_cause_busy_missing_and_swapped() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    assert_eq!(
        held.open_read(&name("none")).err(),
        Some(HeldError::Missing)
    );
    assert_eq!(held.open_dir(&name("none")).err(), Some(HeldError::Missing));
    let (file, pin) = held.make_file(&name("f")).unwrap();
    let wrong = super::Pin {
        device: pin.device,
        inode: pin.inode.wrapping_add(1),
    };
    assert_eq!(
        held.verify(&name("f"), &file, wrong, false).err(),
        Some(HeldError::Unsafe)
    );
    drop(file);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let busy = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(root.join("f"))
            .unwrap();
        assert_eq!(held.open_read(&name("f")).err(), Some(HeldError::InUse));
        drop(busy);
    }
    held.open_read(&name("f")).unwrap();
}

/// N2a r2 row 10 (rv-sec3 #725 U1): how a root was judged travels with every folder opened
/// through it, and the assertion an unjudged call starts with trips on a tree held through
/// `Folder::root`, W1's private judge, while it holds under a caller-judged root.
#[test]
fn an_unjudged_call_on_a_private_tree_trips_the_layer() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let child = held.make_dir(&name("child")).unwrap();
    for folder in [&held, &child] {
        let tripped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            folder.assert_caller_judged();
        }));
        assert!(tripped.is_err(), "an unjudged call ran on a private tree");
    }
    drop(child);
    let caller = Folder {
        judged: Judged::Caller,
        ..held
    };
    caller.assert_caller_judged();
    caller
        .open_dir(&name("child"))
        .unwrap()
        .assert_caller_judged();
}

/// N2a r2 row 11 (rv-sec3 #731 F1): a FIFO at a file name is refused as `Unsafe` by
/// `open_read` and `open_write` without blocking, as every custody open it replaces opens
/// with `O_NONBLOCK`; each open runs on its own thread and must answer within 10 seconds.
#[cfg(unix)]
#[test]
fn a_fifo_at_a_file_name_is_refused_without_blocking() {
    let (_temp, root) = private_root();
    let status = kernel_processes::spawn_gate::status(
        std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(root.join("fifo"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(status.success());
    for write in [false, true] {
        let folder = root.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let held = Folder::root(&folder).unwrap();
            let opened = if write {
                held.open_write(&name("fifo")).err()
            } else {
                held.open_read(&name("fifo")).err()
            };
            sender.send(opened).unwrap();
        });
        let answer = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("open (write {write}) blocked on a FIFO"));
        assert_eq!(answer, Some(HeldError::Unsafe), "write {write}");
    }
}

/// The PowerShell script that turns an empty folder into a junction in place, with
/// FSCTL_SET_REPARSE_POINT on a handle opened with write access and every share mode, so
/// it runs beside a W1 hold that shares read and write. Its exit code is the Windows
/// error, 0 once the folder is a junction.
#[cfg(windows)]
const JUNCTION_SCRIPT: &str = r##"param([string]$Folder, [string]$Target)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
public static class HeldJunction {
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(
        string name, uint access, uint share, IntPtr security,
        uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool DeviceIoControl(
        SafeFileHandle handle, uint code, byte[] input, int inputSize,
        IntPtr output, int outputSize, out int returned, IntPtr overlapped);
    public static int Set(string folder, string target) {
        SafeFileHandle handle = CreateFileW(
            folder, 0x40000000, 7, IntPtr.Zero, 3, 0x02200000, IntPtr.Zero);
        if (handle.IsInvalid) { return Marshal.GetLastWin32Error(); }
        using (handle) {
            byte[] substitute = Encoding.Unicode.GetBytes("\\??\\" + target);
            byte[] print = Encoding.Unicode.GetBytes(target);
            int data = 8 + substitute.Length + 2 + print.Length + 2;
            byte[] buffer = new byte[8 + data];
            BitConverter.GetBytes(0xA0000003u).CopyTo(buffer, 0);
            BitConverter.GetBytes((ushort)data).CopyTo(buffer, 4);
            BitConverter.GetBytes((ushort)0).CopyTo(buffer, 8);
            BitConverter.GetBytes((ushort)substitute.Length).CopyTo(buffer, 10);
            BitConverter.GetBytes((ushort)(substitute.Length + 2)).CopyTo(buffer, 12);
            BitConverter.GetBytes((ushort)print.Length).CopyTo(buffer, 14);
            substitute.CopyTo(buffer, 16);
            print.CopyTo(buffer, 16 + substitute.Length + 2);
            int returned;
            bool set = DeviceIoControl(
                handle, 0x000900A4, buffer, buffer.Length,
                IntPtr.Zero, 0, out returned, IntPtr.Zero);
            if (!set) { return Marshal.GetLastWin32Error(); }
            return 0;
        }
    }
}
'@
exit [HeldJunction]::Set($Folder, $Target)
"##;

/// Turns an empty folder into a junction to `target` in place, from another process, as an
/// agent able to write the folder could while custody holds it. The script goes to a file
/// and runs with `-File`. Inputs: the folder, the target and a scratch folder for the
/// script. Output: the script's exit code, 0 once the folder is a junction.
#[cfg(windows)]
fn turn_into_junction(folder: &Path, target: &Path, scratch: &Path) -> i32 {
    let script = scratch.join("held-junction.ps1");
    std::fs::write(&script, JUNCTION_SCRIPT).unwrap();
    let status = kernel_processes::spawn_gate::status(
        std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script)
            .arg("-Folder")
            .arg(folder)
            .arg("-Target")
            .arg(target)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
    .unwrap();
    status.code().unwrap()
}

/// N2a r2 row 12 (rv-sec3 #725 U4): a held, empty folder that another process turns into a
/// junction to a folder with entries lists as `Unsafe`, never as that folder's names, since
/// a listing by place would follow the junction. The row first asserts that the turn
/// succeeded, so a refusal by Windows fails here with its code.
#[cfg(windows)]
#[test]
fn a_held_folder_turned_into_a_junction_lists_as_unsafe() {
    let (temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    let child = held.make_dir(&name("child")).unwrap();
    drop(held.make_dir(&name("target")).unwrap());
    private_file(&root.join("target").join("foreign"), b"bytes");
    assert_eq!(child.list(4).unwrap(), Vec::new());
    let code = turn_into_junction(&root.join("child"), &root.join("target"), temp.path());
    assert_eq!(code, 0, "turning the held folder into a junction failed");
    assert!(root.join("child").join("foreign").is_file());
    assert_eq!(child.list(4).err(), Some(HeldError::Unsafe));
}

/// How many times a row's judge ran, so a row can tell a root refused before its judge
/// from one its judge refused.
static JUDGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A judge that passes every folder and counts its runs. Input: the held handle. Output:
/// true.
fn judge_passes(_held: &File) -> bool {
    JUDGED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    true
}

/// A judge that refuses every folder and counts its runs. Input: the held handle. Output:
/// false.
fn judge_refuses(_held: &File) -> bool {
    JUDGED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    false
}

/// A judge that passes every folder without counting, for rows that run beside the
/// counting row. Input: the held handle. Output: true.
fn judge_any(_held: &File) -> bool {
    true
}

/// A plain folder a caller judges itself, as an agent home is: made by std, no private list
/// set on it. Output: the temp guard and the folder's canonical path.
fn caller_root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = kernel_sandbox::paths::canonical(temp.path())
        .unwrap()
        .join("home");
    std::fs::create_dir(&root).unwrap();
    (temp, root)
}

/// A root held through a caller's judge runs that judge once on the held handle and is
/// refused as `Unsafe` when the judge says no. Every folder opened through it carries the
/// caller's judgement. A link at the root's name is `Unsafe` before the judge runs, and the
/// folder it names is never held.
#[test]
fn a_caller_judged_root_runs_its_judge_on_the_held_handle() {
    use std::sync::atomic::Ordering;
    let (_temp, root) = caller_root();
    let before = JUDGED.load(Ordering::SeqCst);
    let held = Folder::root_judged_by(&root, judge_passes).unwrap();
    assert_eq!(JUDGED.load(Ordering::SeqCst), before + 1);
    held.assert_caller_judged();
    held.make_dir_inherited(&name("child"))
        .unwrap()
        .assert_caller_judged();
    held.open_dir_unjudged(&name("child"))
        .unwrap()
        .assert_caller_judged();
    drop(held);
    assert_eq!(
        Folder::root_judged_by(&root, judge_refuses).err(),
        Some(HeldError::Unsafe)
    );
    assert_eq!(JUDGED.load(Ordering::SeqCst), before + 2);
    let link = root.with_file_name("linked");
    link_folder(&link, &root);
    assert_eq!(
        Folder::root_judged_by(&link, judge_passes).err(),
        Some(HeldError::Unsafe)
    );
    assert_eq!(JUDGED.load(Ordering::SeqCst), before + 2);
}

/// One call a row runs on a held folder, with the name it reports.
type Call<'a> = (&'a str, &'a dyn Fn(&Folder));

/// Every call that skips the layer's own judge trips its assertion on a tree held through
/// `Folder::root`, the private judge, and makes, opens or removes nothing there.
#[test]
fn the_unjudged_and_inherited_calls_trip_on_a_private_tree() {
    let (_temp, root) = private_root();
    let held = Folder::root(&root).unwrap();
    drop(held.make_dir(&name("kept")).unwrap());
    let calls: [Call; 4] = [
        ("make_dir_inherited", &|folder| {
            let _ = folder.make_dir_inherited(&name("made"));
        }),
        ("make_file_inherited", &|folder| {
            let _ = folder.make_file_inherited(&name("made"));
        }),
        ("open_dir_unjudged", &|folder| {
            let _ = folder.open_dir_unjudged(&name("kept"));
        }),
        ("remove_tree_unjudged", &|folder| {
            let _ = folder.remove_tree_unjudged(&name("kept"), 1);
        }),
    ];
    for (call, run) in calls {
        let tripped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&held)));
        assert!(tripped.is_err(), "{call} ran on a private tree");
    }
    assert!(!root.join("made").exists());
    assert!(root.join("kept").is_dir());
}

/// An inherited file and folder are create-only, so an entry or a link already at the name
/// is `Unsafe` and the folder a link names gets nothing. They take the folder's own list on
/// Windows, so an entry the folder passes on reaches them, where a private file has only the
/// private list; on Unix they are the user's own, 0600 and 0700.
#[test]
fn an_inherited_file_and_folder_are_create_only_and_take_the_folder_s_list() {
    use std::io::Write;
    let (_temp, root) = caller_root();
    let outside = root.with_file_name("outside");
    std::fs::create_dir(&outside).unwrap();
    link_folder(&root.join("link"), &outside);
    open_to_everyone(&root);
    let held = Folder::root_judged_by(&root, judge_any).unwrap();
    let (mut file, pin) = held.make_file_inherited(&name("settings.json")).unwrap();
    file.write_all(b"{}").unwrap();
    assert_ne!(pin.inode, 0);
    let folder = held.make_dir_inherited(&name("plugin")).unwrap();
    for taken in ["settings.json", "plugin", "link"] {
        assert_eq!(
            held.make_file_inherited(&name(taken)).err(),
            Some(HeldError::Unsafe),
            "{taken}"
        );
        assert_eq!(
            held.make_dir_inherited(&name(taken)).err(),
            Some(HeldError::Unsafe),
            "{taken}"
        );
    }
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
    assert_eq!(std::fs::read(root.join("settings.json")).unwrap(), b"{}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root.join("settings.json")), 0o600);
        assert_eq!(mode(&root.join("plugin")), 0o700);
    }
    #[cfg(windows)]
    {
        use crazyagent_win_sandbox::private;
        assert!(
            private::check(&file).is_err(),
            "the file kept a private list"
        );
        assert!(
            private::check(folder.file()).is_err(),
            "the folder kept a private list"
        );
        let (_private_temp, private_path) = private_root();
        let private_held = Folder::root(&private_path).unwrap();
        let (own, _) = private_held.make_file(&name("own")).unwrap();
        private::check(&own).unwrap();
    }
    drop(folder);
}

/// A tree removal under a caller-judged root takes files, folders and links bottom-up and
/// never follows a link: a link inside the tree, or at the tree's own name, goes as itself
/// and every file in the folder it names stays. A missing name is `Missing`.
#[test]
fn a_tree_removal_never_follows_a_link_and_keeps_what_it_names() {
    let (_temp, root) = caller_root();
    let outside = root.with_file_name("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("kept.txt"), "kept").unwrap();
    let tree = root.join("skills");
    std::fs::create_dir_all(tree.join("one").join("two")).unwrap();
    std::fs::write(tree.join("a.md"), "a").unwrap();
    std::fs::write(tree.join("one").join("b.md"), "b").unwrap();
    std::fs::write(tree.join("one").join("two").join("c.md"), "c").unwrap();
    link_folder(&tree.join("out"), &outside);
    link_folder(&tree.join("one").join("out"), &outside);
    link_folder(&root.join("linked"), &outside);
    let held = Folder::root_judged_by(&root, judge_any).unwrap();
    held.remove_tree_unjudged(&name("skills"), 3).unwrap();
    assert!(std::fs::symlink_metadata(&tree).is_err());
    held.remove_tree_unjudged(&name("linked"), 3).unwrap();
    assert!(std::fs::symlink_metadata(root.join("linked")).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("kept.txt")).unwrap(),
        "kept"
    );
    assert_eq!(
        held.remove_tree_unjudged(&name("skills"), 3).err(),
        Some(HeldError::Missing)
    );
    assert!(std::fs::read_dir(&root).unwrap().next().is_none());
}

/// A tree removal is bounded by its depth: folders nested past it are `Full`, and the
/// deepest file stays, so a tree an agent nests without end never runs the walk on.
#[test]
fn a_tree_removal_stops_at_its_depth_bound() {
    let (_temp, root) = caller_root();
    let deep = root.join("tree").join("1").join("2").join("3");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("deep.md"), "deep").unwrap();
    let held = Folder::root_judged_by(&root, judge_any).unwrap();
    assert_eq!(
        held.remove_tree_unjudged(&name("tree"), 1).err(),
        Some(HeldError::Full)
    );
    assert_eq!(
        std::fs::read_to_string(deep.join("deep.md")).unwrap(),
        "deep"
    );
    held.remove_tree_unjudged(&name("tree"), 3).unwrap();
    assert!(!root.join("tree").exists());
}

/// On Windows a file in the tree that another handle holds without delete sharing makes
/// the removal `InUse`, and the file stays until that handle closes, when the same removal
/// takes the whole tree.
#[cfg(windows)]
#[test]
fn a_tree_removal_with_a_held_file_is_in_use_and_keeps_it() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_temp, root) = caller_root();
    std::fs::create_dir_all(root.join("tree").join("sub")).unwrap();
    std::fs::write(root.join("tree").join("sub").join("busy.md"), "busy").unwrap();
    let busy = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(root.join("tree").join("sub").join("busy.md"))
        .unwrap();
    let held = Folder::root_judged_by(&root, judge_any).unwrap();
    assert_eq!(
        held.remove_tree_unjudged(&name("tree"), 2).err(),
        Some(HeldError::InUse)
    );
    assert!(root.join("tree").join("sub").join("busy.md").is_file());
    drop(busy);
    held.remove_tree_unjudged(&name("tree"), 2).unwrap();
    assert!(!root.join("tree").exists());
}
