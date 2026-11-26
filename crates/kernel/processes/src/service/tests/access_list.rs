#![cfg(unix)]
use super::*;

/// A part an access list lets someone other than this user or root change
/// counts as changeable at its depth, for the engine service and for the tools the resolver
/// runs alike, even inside a store.
#[cfg(unix)]
#[test]
fn a_part_an_access_list_opens_to_others_is_changeable() {
    const ME: u32 = 501;
    let own = Part::Found {
        owner: ME,
        group: 20,
        mode: 0o755,
        folder: true,
        link: false,
    };
    let system = Part::Found {
        owner: 0,
        group: 0,
        mode: 0o755,
        folder: true,
        link: false,
    };
    let tool = Part::Found {
        owner: ME,
        group: 20,
        mode: 0o755,
        folder: false,
        link: false,
    };
    for open_stores in [false, true] {
        assert_eq!(
            first_changeable_part(&[tool, Part::Shared, own, system], ME, open_stores),
            Some(1)
        );
        assert_eq!(
            first_changeable_part(&[Part::Shared, own, system], ME, open_stores),
            Some(0)
        );
        assert_eq!(
            first_changeable_part(&[tool, own, system], ME, open_stores),
            None
        );
    }
}

/// A macOS access list names an entry changeable by others when an allow entry gives a
/// change right (write or add, delete, append or add a folder, delete a child, write
/// attributes or extended attributes, write the list, take ownership, and the generic all
/// and write rights) to anyone but this user, root, wheel, daemon or admin, inherited or
/// only for inheritance alike; deny entries and read rights never do, and an entry of
/// another kind, a list not read for the stamped object or trusted principals that could
/// not be read all fail closed.
#[cfg(unix)]
#[test]
fn an_access_list_entry_that_lets_others_change_an_entry_is_shared() {
    const ME: [u8; 16] = [1; 16];
    const ROOT: [u8; 16] = [2; 16];
    const OTHER: [u8; 16] = [9; 16];
    let trusted = [ME, ROOT, [3; 16], [4; 16], [5; 16]];
    let shared = |entries: &[acl::Entry]| acl::entries_shared(entries, &trusted);
    assert_eq!(shared(&[]), Some(false));
    for bit in [2, 4, 5, 6, 8, 10, 12, 13, 21, 23] {
        assert_eq!(
            shared(&[(OTHER, 1, 1 << bit)]),
            Some(true),
            "change bit {bit}"
        );
        assert_eq!(
            shared(&[(OTHER, 2, 1 << bit)]),
            Some(false),
            "denied bit {bit}"
        );
        for principal in trusted {
            assert_eq!(
                shared(&[(principal, 1, 1 << bit)]),
                Some(false),
                "trusted bit {bit}"
            );
        }
    }
    for bit in [1, 3, 7, 9, 11, 20, 22, 24] {
        assert_eq!(
            shared(&[(OTHER, 1, 1 << bit)]),
            Some(false),
            "read bit {bit}"
        );
    }
    for flags in [1 | (1 << 4), 1 | (1 << 5) | (1 << 6), 1 | (1 << 8)] {
        assert_eq!(
            shared(&[(OTHER, flags, 1 << 2)]),
            Some(true),
            "flags {flags:#x}"
        );
    }
    assert_eq!(
        shared(&[(OTHER, 2, 1 << 2), (ME, 1, 1 << 2), (OTHER, 1, 1 << 23)]),
        Some(true)
    );
    for kind in [0, 3, 4, 15] {
        assert_eq!(shared(&[(ME, kind, 1 << 1)]), None, "kind {kind}");
    }
    let entries = [(OTHER, 1, 1 << 2)];
    assert_eq!(acl::read_answer(Some(&entries), Some(&trusted)), Some(true));
    assert_eq!(acl::read_answer(Some(&[]), Some(&trusted)), Some(false));
    assert_eq!(acl::read_answer(None, Some(&trusted)), None, "no read");
    assert_eq!(acl::read_answer(Some(&[]), None), None, "no trusted GUIDs");
}

/// A Linux `system.posix_acl_access` value names an entry changeable by others
/// when a named user other than this user or root, or a named group the trusted-group rule
/// does not trust, has write; the owner, group and other entries stay with the mode bits,
/// and a value it cannot read fails closed.
#[cfg(unix)]
#[test]
fn a_posix_access_list_that_lets_others_write_is_shared() {
    const ME: u32 = 1000;
    const OTHER: u32 = 1001;
    const OWN_GROUP: u32 = 1000;
    const SHARED_GROUP: u32 = 100;
    let trusted = |group: u32| group == 0 || group == OWN_GROUP;
    let value = |entries: &[(u16, u16, u32)]| {
        let mut bytes = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in entries {
            bytes.extend(tag.to_le_bytes());
            bytes.extend(perm.to_le_bytes());
            bytes.extend(id.to_le_bytes());
        }
        bytes
    };
    let base = [
        (0x01, 7, u32::MAX),
        (0x04, 5, u32::MAX),
        (0x10, 7, u32::MAX),
        (0x20, 5, u32::MAX),
    ];
    let with = |extra: (u16, u16, u32)| {
        let mut entries = base.to_vec();
        entries.insert(1, extra);
        value(&entries)
    };
    assert_eq!(acl::posix_shared(&value(&base), ME, trusted), Some(false));
    for open in [(0x02, 7, OTHER), (0x02, 2, OTHER), (0x08, 6, SHARED_GROUP)] {
        assert_eq!(
            acl::posix_shared(&with(open), ME, trusted),
            Some(true),
            "{open:?}"
        );
    }
    for closed in [
        (0x02, 7, ME),
        (0x02, 7, 0),
        (0x02, 5, OTHER),
        (0x08, 7, OWN_GROUP),
        (0x08, 5, SHARED_GROUP),
    ] {
        assert_eq!(
            acl::posix_shared(&with(closed), ME, trusted),
            Some(false),
            "{closed:?}"
        );
    }
    let mut old = value(&base);
    old[0] = 1;
    assert_eq!(acl::posix_shared(&old, ME, trusted), None);
    let mut cut = value(&base);
    cut.pop();
    assert_eq!(acl::posix_shared(&cut, ME, trusted), None);
    assert_eq!(acl::posix_shared(&[], ME, trusted), None);
}

/// On macOS a folder whose mode bits are private but whose access list lets
/// staff add and delete files is changeable by others, for the engine service and for the
/// resolver's tools, while a deny-only list leaves it private.
#[cfg(target_os = "macos")]
#[test]
fn a_tool_folder_staff_may_change_through_its_access_list_is_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    assert_eq!(changeable_by_others(&tool), None);
    let chmod = |rule: &str| {
        let status = crate::spawn_gate::status(
            std::process::Command::new("/bin/chmod")
                .arg("+a")
                .arg(rule)
                .arg(&folder),
        )
        .unwrap();
        assert!(status.success(), "{rule}");
    };
    chmod("user:nobody deny add_file");
    assert_eq!(
        changeable_by_others(&tool),
        None,
        "a deny entry changes nothing"
    );
    assert_eq!(mode_of(&folder), 0o755);
    chmod("group:staff allow add_file,delete_child");
    assert_eq!(mode_of(&folder), 0o755, "the mode bits still read private");
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(changeable_past_sticky(&tool), Some(folder.clone()));
}

/// The wall clock's whole second, as change times count them. Output: the second.
#[cfg(target_os = "macos")]
fn now_secs() -> i64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    i64::try_from(since.as_secs()).unwrap()
}

/// The whole second an entry last changed. Input: the entry. Output: the second.
#[cfg(target_os = "macos")]
fn changed_secs(entry: &Path) -> i64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(entry).unwrap().ctime()
}

/// Waits, at most 1.5 s, until the clock is in a later second than every entry last
/// changed, so a read made now never falls in the second its entry changed. Input: the
/// entries.
#[cfg(target_os = "macos")]
fn settle(entries: &[&Path]) {
    let changed = entries.iter().copied().map(changed_secs).max().unwrap();
    for _ in 0..30 {
        if now_secs() > changed {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the clock did not pass second {changed}");
}

/// One access listing of the `lstat` reader reads every part of a path, names with a space
/// or an accent included, and an access-list grant on a part reads as changeable on the next
/// call, and as not changeable again once the grant is gone.
#[cfg(target_os = "macos")]
#[test]
fn one_access_listing_reads_a_whole_path_and_a_grant_reads_changeable_on_the_next_call() {
    let temp = durable_folder();
    let kit = temp.path().join("tool kit \u{e9}");
    let folder = kit.join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&tool, &folder, &kit, temp.path()]);
    let touching = |listings: Vec<Vec<PathBuf>>| -> Vec<Vec<PathBuf>> {
        listings
            .into_iter()
            .filter(|listing| listing.iter().any(|listed| listed.starts_with(temp.path())))
            .collect()
    };
    acl::take_listings();
    assert_eq!(changeable_by_lstat(&tool), None);
    let first = touching(acl::take_listings());
    assert_eq!(first.len(), 1, "one listing for every part: {first:?}");
    for part in [&tool, &folder, &kit] {
        assert!(first[0].contains(part), "{part:?} in {first:?}");
    }

    chmod_entry(&["+a", "group:staff allow add_file,delete_child"], &folder);
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    chmod_entry(&["-a", "group:staff allow add_file,delete_child"], &folder);
    assert_eq!(changeable_by_others(&tool), None);
}

/// An entry whose mode changes while its access list is read gets no answer, which counts as
/// changeable; an entry with the same read-only access list, left alone, reads as not
/// shared. A move of the change time alone is read again instead (the relisting rows).
#[cfg(target_os = "macos")]
#[test]
fn an_entry_changed_while_its_access_list_is_read_gets_no_answer() {
    use std::os::unix::fs::PermissionsExt;
    let temp = durable_folder();
    let still = temp.path().join("still");
    let swapped = temp.path().join("swapped");
    for entry in [&still, &swapped] {
        write_mode(entry, "", 0o644);
        let status = crate::spawn_gate::status(
            std::process::Command::new("/bin/chmod")
                .arg("+a")
                .arg("group:staff allow read")
                .arg(entry),
        )
        .unwrap();
        assert!(status.success());
    }
    settle(&[&still, &swapped]);
    let read = |entry: &Path| {
        let metadata = std::fs::symlink_metadata(entry).unwrap();
        acl::shared_all(&[(entry, &metadata)])
    };
    assert_eq!(read(&still), vec![Some(false)]);
    let moved = swapped.clone();
    acl::after_next_listing(move || {
        std::fs::set_permissions(&moved, std::fs::Permissions::from_mode(0o600)).unwrap();
    });
    assert_eq!(read(&swapped), vec![None]);
}

/// The first part of a path the `lstat` reader names changeable by others: each part read
/// with `lstat` and its access list listed ([`changeable::read_parts`]), judged as
/// [`changeable_by_others`] judges its reads. Input: the absolute path. Output: the entry.
#[cfg(target_os = "macos")]
fn changeable_by_lstat(path: &Path) -> Option<PathBuf> {
    let entries: Vec<&Path> = path.ancestors().collect();
    first_changeable_part(&changeable::read_parts(&entries), current_uid(), false)
        .and_then(|depth| path.ancestors().nth(depth))
        .map(Path::to_path_buf)
}

/// Makes one new file in the folder now and again after every later access listing on this
/// thread, as parallel work keeps adding files to a shared temp or home folder. Inputs: the
/// folder and how many files it made so far.
#[cfg(target_os = "macos")]
fn churn(folder: PathBuf, made: u32) {
    assert!(
        made < 64,
        "the churn ran past every listing a read may make"
    );
    let made_file = folder.join(format!("churn-{made}"));
    std::fs::write(&made_file, "").unwrap_or_else(|error| panic!("write {made_file:?}: {error}"));
    acl::after_next_listing(move || churn(folder, made + 1));
}

/// A folder that gains files while its access list is read, on every listing, never reads
/// as changeable by others from that churn alone: its access list never changed.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_gaining_files_during_every_listing_does_not_read_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("busy");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&folder, &tool]);
    let busy = folder.clone();
    acl::after_next_listing(move || churn(busy, 0));
    assert_eq!(changeable_by_lstat(&tool), None);
}

/// Makes one new file in the folder now and after every later access listing on this
/// thread, and opens the folder to another user's writes right after the fourth listing of
/// the folder alone, once three relistings have all seen it move. Inputs: the folder, the
/// files made so far and the listings of the folder alone so far.
#[cfg(target_os = "macos")]
fn churn_then_open(folder: PathBuf, made: u32, alone: u32) {
    assert!(
        made < 64,
        "the churn ran past every listing a read may make"
    );
    let made_file = folder.join(format!("churn-{made}"));
    std::fs::write(&made_file, "").unwrap_or_else(|error| panic!("write {made_file:?}: {error}"));
    let listed_alone = acl::last_listing() == [folder.clone()];
    let alone = alone + u32::from(listed_alone);
    if listed_alone && alone == 4 {
        let status = crate::spawn_gate::status(
            std::process::Command::new("/bin/chmod")
                .arg("+a")
                .arg("user:nobody allow add_file,write")
                .arg(&folder),
        )
        .unwrap_or_else(|error| panic!("chmod +a on {folder:?}: {error}"));
        assert!(status.success(), "chmod +a on {folder:?}");
    }
    acl::after_next_listing(move || churn_then_open(folder, made + 1, alone));
}

/// A folder whose access list lets another user add files, and which keeps gaining files
/// through every listing, so its read never holds still, reads as changeable by others from
/// its last two reads.
#[cfg(target_os = "macos")]
#[test]
fn a_shared_folder_gaining_files_through_every_listing_reads_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("busy");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    let status = crate::spawn_gate::status(
        std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("user:nobody allow add_file")
            .arg(&folder),
    )
    .unwrap_or_else(|error| panic!("chmod +a on {folder:?}: {error}"));
    assert!(status.success(), "chmod +a on {folder:?}");
    settle(&[&folder, &tool]);
    acl::take_listings();
    let busy = folder.clone();
    acl::after_next_listing(move || churn(busy, 0));
    assert_eq!(changeable_by_lstat(&tool), Some(folder.clone()));
    let alone = acl::take_listings()
        .iter()
        .filter(|listing| **listing == [folder.clone()])
        .count();
    assert!(
        alone >= 5,
        "the folder was read alone {alone} times, short of the last reads"
    );
}

/// A folder that keeps gaining files through every relisting, and gains an access list
/// that lets another user write just after the first listing that follows them, reads as
/// changeable by others: an answer read while the folder never held still needs a second
/// listing that reads the same access list.
#[cfg(target_os = "macos")]
#[test]
fn an_access_list_added_after_the_relistings_of_a_busy_folder_still_reads_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("busy");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&folder, &tool]);
    acl::take_listings();
    let busy = folder.clone();
    acl::after_next_listing(move || churn_then_open(busy, 0, 0));
    assert_eq!(changeable_by_lstat(&tool), Some(folder.clone()));
}

/// An access list that lets another user write, added while the folder's list is read,
/// still makes the folder read as changeable by others.
#[cfg(target_os = "macos")]
#[test]
fn an_access_list_added_during_the_listing_still_reads_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&folder, &tool]);
    let opened = folder.clone();
    acl::after_next_listing(move || {
        let status = crate::spawn_gate::status(
            std::process::Command::new("/bin/chmod")
                .arg("+a")
                .arg("user:nobody allow add_file,write")
                .arg(&opened),
        )
        .unwrap_or_else(|error| panic!("chmod +a on {opened:?}: {error}"));
        assert!(status.success(), "chmod +a on {opened:?}");
    });
    assert_eq!(changeable_by_lstat(&tool), Some(folder.clone()));
}

/// A deny-only access list, as a macOS home folder carries (`group:everyone deny delete`),
/// lets no one else change the folder, so it never reads as changeable by others.
#[cfg(target_os = "macos")]
#[test]
fn a_deny_only_access_list_as_a_home_folder_carries_is_not_changeable() {
    let temp = durable_folder();
    let folder = temp.path().join("home");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    let status = crate::spawn_gate::status(
        std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("group:everyone deny delete")
            .arg(&folder),
    )
    .unwrap_or_else(|error| panic!("chmod +a on {folder:?}: {error}"));
    assert!(status.success(), "chmod +a on {folder:?}");
    assert_eq!(changeable_by_others(&tool), None);
    temp.close().unwrap();
}

/// A world-writable folder above a tool reads as the changeable part of the tool's path,
/// though every part settled before the call.
#[cfg(target_os = "macos")]
#[test]
fn a_world_writable_folder_above_a_tool_reads_changeable() {
    let temp = durable_folder();
    let folder = shared_folder(&temp.path().join("open"), 0o777);
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&folder, &tool]);
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
}

/// Runs `/bin/chmod` with the arguments on the entry and requires it to succeed. Inputs: the
/// arguments and the entry.
#[cfg(target_os = "macos")]
fn chmod_entry(args: &[&str], entry: &Path) {
    let status = crate::spawn_gate::status(
        std::process::Command::new("/bin/chmod")
            .args(args)
            .arg(entry),
    )
    .unwrap();
    assert!(status.success(), "chmod {args:?} {entry:?}");
}

/// The access answer `shared_all` gives one entry, judged from what `lstat` reads of it now.
/// Input: the entry. Output: the answer.
#[cfg(target_os = "macos")]
fn read_alone(entry: &Path) -> Option<bool> {
    let metadata = std::fs::symlink_metadata(entry).unwrap();
    acl::shared_all(&[(entry, &metadata)])[0]
}

/// A file and a folder whose mode is 000 still have their access lists read: an allow entry
/// that lets staff change them reads as shared, and one with no list reads as not shared,
/// though neither can be opened.
#[cfg(target_os = "macos")]
#[test]
fn a_mode_000_entry_has_its_access_list_read() {
    use std::os::unix::fs::PermissionsExt;
    let temp = durable_folder();
    let file = temp.path().join("file");
    let plain = temp.path().join("plain");
    let folder = temp.path().join("folder");
    write_mode(&file, "", 0o644);
    write_mode(&plain, "", 0o644);
    std::fs::create_dir(&folder).unwrap();
    chmod_entry(&["+a", "group:staff allow write"], &file);
    chmod_entry(&["+a", "group:staff allow add_file"], &folder);
    for entry in [&file, &plain, &folder] {
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let read: Vec<Option<bool>> = [&file, &plain, &folder]
        .into_iter()
        .map(|entry| read_alone(entry))
        .collect();
    for entry in [&file, &plain, &folder] {
        std::fs::set_permissions(entry, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(read, vec![Some(true), Some(false), Some(true)]);
}

/// `/` itself, a tool folder on the sealed system volume, whose volume reply names another
/// device than its entry, and a folder on the Data volume are all root's or this user's
/// alone, so none reads as changeable by others.
#[cfg(target_os = "macos")]
#[test]
fn the_root_a_system_volume_folder_and_a_data_volume_folder_are_not_changeable() {
    assert_eq!(changeable_by_others(Path::new("/")), None);
    assert_eq!(changeable_by_others(Path::new("/usr/bin")), None);
    let beside = std::env::current_exe().unwrap();
    let temp = tempfile::tempdir_in(beside.parent().unwrap()).unwrap();
    assert_eq!(changeable_by_others(temp.path()), None);
}

/// A disk image `hdiutil` made and mounted for one row, detached by its exact mount path
/// when dropped.
#[cfg(target_os = "macos")]
struct Mounted(PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Mounted {
    fn drop(&mut self) {
        let detached = crate::spawn_gate::status(
            std::process::Command::new("/usr/bin/hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.0),
        );
        assert!(
            detached.is_ok_and(|status| status.success()),
            "detach {:?}",
            self.0
        );
    }
}

/// An entry on a volume that keeps no access lists, an MS-DOS disk image, reads as unknown,
/// which counts as changeable, never as an entry without a list. A runner that cannot make
/// or mount the image says SKIP and checks nothing.
#[cfg(target_os = "macos")]
#[test]
fn an_entry_on_a_volume_without_access_lists_reads_unknown() {
    let temp = durable_folder();
    let image = temp.path().join("fat.dmg");
    let mount = temp.path().join("mount");
    std::fs::create_dir(&mount).unwrap();
    let hdiutil = |args: &[&str], paths: &[&Path]| {
        crate::spawn_gate::status(
            std::process::Command::new("/usr/bin/hdiutil")
                .args(args)
                .args(paths),
        )
        .is_ok_and(|status| status.success())
    };
    let made = [
        "create", "-quiet", "-size", "8m", "-fs", "MS-DOS", "-volname", "NOACL", "-o",
    ];
    if !hdiutil(&made, &[&image]) {
        eprintln!("SKIP: hdiutil could not make an MS-DOS image");
        return;
    }
    let attach = ["attach", "-quiet", "-nobrowse", "-noverify", "-mountpoint"];
    if !hdiutil(&attach, &[&mount, &image]) {
        eprintln!("SKIP: hdiutil could not mount the MS-DOS image");
        return;
    }
    let mounted = Mounted(mount.clone());
    let file = mount.join("file");
    std::fs::write(&file, "").unwrap();
    let read = [read_alone(&file), read_alone(&mount)];
    drop(mounted);
    eprintln!("RAN: the MS-DOS image was mounted and read");
    assert_eq!(read, [None, None]);
}

/// An entry swapped for another file after `lstat` read it reads as none, since the list
/// read belongs to another object.
#[cfg(target_os = "macos")]
#[test]
fn an_entry_swapped_after_its_lstat_reads_none() {
    let temp = durable_folder();
    let entry = temp.path().join("entry");
    let other = temp.path().join("other");
    write_mode(&entry, "", 0o644);
    write_mode(&other, "", 0o644);
    settle(&[&entry, &other]);
    let judged = std::fs::symlink_metadata(&entry).unwrap();
    std::fs::rename(&other, &entry).unwrap();
    assert_eq!(acl::shared_all(&[(&entry, &judged)]), vec![None]);
}

/// Allow entries that name this user or the admin group by name give no one else a change
/// right, so the entry does not read as shared; one that names staff does.
#[cfg(target_os = "macos")]
#[test]
fn allow_entries_for_this_user_and_the_admin_group_do_not_read_shared() {
    let temp = durable_folder();
    let entry = temp.path().join("entry");
    write_mode(&entry, "", 0o644);
    let me = nix::unistd::User::from_uid(nix::unistd::getuid())
        .unwrap()
        .unwrap()
        .name;
    chmod_entry(&["+a", &format!("user:{me} allow write,delete")], &entry);
    chmod_entry(&["+a", "group:admin allow write,writesecurity"], &entry);
    assert_eq!(read_alone(&entry), Some(false));
    chmod_entry(&["+a", "group:staff allow append"], &entry);
    assert_eq!(read_alone(&entry), Some(true));
}

/// A folder named with a trailing slash reads as it does without one, and a link to a
/// folder named with one is read itself, which `lstat` with the slash did not find, so it
/// reads as none.
#[cfg(target_os = "macos")]
#[test]
fn a_trailing_slash_never_makes_the_read_follow_a_link() {
    let temp = durable_folder();
    let folder = temp.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    chmod_entry(&["+a", "group:staff allow add_file"], &folder);
    let slashed = PathBuf::from(format!("{}/", folder.display()));
    assert_eq!(read_alone(&slashed), Some(true));
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&folder, &link).unwrap();
    let slashed_link = PathBuf::from(format!("{}/", link.display()));
    assert_eq!(read_alone(&slashed_link), None);
}

/// An access-list grant made between two calls of the same check reads as changeable on the
/// second call, for the engine service and for the tools the resolver runs alike, though
/// every part of the path settled before the first call.
#[cfg(target_os = "macos")]
#[test]
fn an_access_list_grant_between_two_calls_reads_changeable_on_the_second() {
    let temp = durable_folder();
    let folder = temp.path().join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&tool, &folder, temp.path()]);
    assert_eq!(changeable_by_others(&tool), None);
    assert_eq!(changeable_past_sticky(&tool), None);
    chmod_entry(&["+a", "user:nobody allow add_file,write"], &folder);
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(changeable_past_sticky(&tool), Some(folder.clone()));
}

/// Neither the access-list reader nor the changeable walk keeps a read between calls: the only
/// process-wide or per-thread state their sources declare is the fixed GUID table and the two
/// test-only listing records, so every check reads each part again.
#[test]
fn the_changeable_readers_hold_no_state_between_calls() {
    let allowed = ["GUIDS", "LISTINGS", "AFTER_LISTING"];
    let mut held = Vec::new();
    let mut per_thread = 0;
    for (file, text) in [
        ("acl.rs", include_str!("../acl.rs")),
        ("changeable.rs", include_str!("../changeable.rs")),
    ] {
        for (index, line) in text.lines().enumerate() {
            if line.contains("thread_local!") {
                per_thread += 1;
                continue;
            }
            let declares = line.matches("static ").count() > line.matches("'static ").count();
            let state = declares || line.contains("LazyLock") || line.contains("OnceLock");
            if state && !allowed.iter().any(|name| line.contains(name)) {
                held.push(format!("{file}:{}: {}", index + 1, line.trim()));
            }
        }
    }
    assert_eq!(held, Vec::<String>::new());
    assert_eq!(per_thread, 2);
}
