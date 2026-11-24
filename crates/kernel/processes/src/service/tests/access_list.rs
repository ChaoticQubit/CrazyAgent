#![cfg(unix)]
use super::*;

/// RV-DS241-18: a part an access list lets someone other than this user or root change
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

/// RV-DS241-18: a macOS `ls -lde` listing names an entry changeable by others when an
/// allow entry gives write, append, add or delete rights, or rights over the entry's own
/// attributes, security or owner, to anyone but this user, root, wheel, daemon or admin; deny
/// entries and read rights never do, and a listing it cannot read fails closed.
#[cfg(unix)]
#[test]
fn an_access_listing_that_lets_others_change_an_entry_is_shared() {
    let listing = |entries: &[&str]| {
        let mut text = String::from("drwxr-xr-x+ 2 me  staff  64 Nov 14 03:33 /Users/me/bin\n");
        for (index, entry) in entries.iter().enumerate() {
            text.push_str(&format!(" {index}: {entry}\n"));
        }
        text
    };
    let me = Some("me");
    assert_eq!(acl::listed_shared(&listing(&[]), me), Some(false));
    for entry in [
        "group:staff allow add_file,delete_child",
        "group:everyone allow write",
        "user:other allow append",
        "user:other allow add_subdirectory",
        "user:other allow delete",
        "user:other allow writeattr",
        "user:other allow writeextattr",
        "user:other allow writesecurity",
        "user:other allow chown",
        "group:staff inherited allow add_file",
        "ABCDEF01-2345-6789-ABCD-EF0123456789 allow write",
    ] {
        assert_eq!(
            acl::listed_shared(&listing(&["group:everyone deny delete", entry]), me),
            Some(true),
            "{entry}"
        );
        assert_eq!(
            acl::listed_shared(&listing(&[entry]), None),
            Some(true),
            "{entry}"
        );
    }
    for entry in [
        "group:everyone deny delete",
        "user:me allow write,append,delete",
        "user:root allow write",
        "group:wheel allow add_file",
        "group:daemon allow add_file",
        "group:admin allow add_file",
        "user:other allow read,list,search,readattr,readextattr,readsecurity,execute",
        "group:staff inherited deny add_file,delete_child",
    ] {
        assert_eq!(
            acl::listed_shared(&listing(&[entry]), me),
            Some(false),
            "{entry}"
        );
    }
    assert_eq!(
        acl::listed_shared(&listing(&["user:me allow write"]), None),
        Some(true),
        "without this user's name every user entry counts as someone else"
    );
    for broken in [
        format!("{}x: group:staff allow write\n", listing(&[])),
        format!("{} 0: group:staff\n", listing(&[])),
        format!("{} 0: group:staff allow\n", listing(&[])),
        String::new(),
    ] {
        assert_eq!(acl::listed_shared(&broken, me), None, "{broken:?}");
    }
}

/// RV-DS241-18 r2: one `ls -lde` listing of several entries is read block by block, each
/// block going to the one entry its first line ends with, the longest when one name ends
/// another. An entry with no block or two gets none, and so does one whose block does not
/// read, alone; a listing with a block for no entry or an access-list line before any entry
/// gives none for every entry.
#[cfg(unix)]
#[test]
fn a_listing_of_several_entries_is_read_block_by_block() {
    let me = Some("me");
    let line = |name: &str| format!("      7 drwxr-xr-x+ 2 me  staff  64 Nov 14 03:33 {name}\n");
    let open = " 0: group:staff allow add_file,delete_child\n";
    let closed = " 0: group:everyone deny delete\n";
    let entries = ["/Volumes/My /c", "/c", "/Users/me/bin", "/Users/me"];
    let text = format!(
        "{}{}{open}{}{closed}{}",
        line("/Users/me"),
        line("/Users/me/bin"),
        line("/Volumes/My /c"),
        line("/c")
    );
    assert_eq!(
        acl::listed_shared_all(&text, &entries, &[7; 4], me, &|_| Some(false)),
        vec![Some(false), Some(false), Some(true), Some(false)]
    );
    assert_eq!(
        acl::listed_shared_all(
            &line("/Users/me"),
            &["/Users/me", "/gone"],
            &[7; 2],
            me,
            &|_| Some(false)
        ),
        vec![Some(false), None],
        "an entry the listing left out"
    );
    assert_eq!(
        acl::listed_shared_all(
            &format!("{0}{0}", line("/a")),
            &["/a"],
            &[7; 1],
            me,
            &|_| Some(false)
        ),
        vec![None],
        "an entry listed twice"
    );
    assert_eq!(
        acl::listed_shared_all(
            &format!("{} 0: group:staff\n{}{open}", line("/a"), line("/b")),
            &["/a", "/b"],
            &[7; 2],
            me,
            &|_| Some(false)
        ),
        vec![None, Some(true)],
        "a block that does not read fails alone"
    );
    for whole in [
        format!("{open}{}", line("/a")),
        format!("{}{}{open}", line("/a"), line("/zzz")),
        String::new(),
    ] {
        assert_eq!(
            acl::listed_shared_all(&whole, &["/a", "/b"], &[7; 2], me, &|_| Some(false)),
            vec![None, None],
            "{whole:?}"
        );
    }
}

/// An entry whose block printed no access-list lines takes the direct read's answer, as
/// `ls` prints a list it could not read exactly as no list; an entry whose block printed
/// lines is read from them and never read again.
#[cfg(unix)]
#[test]
fn an_entry_listed_with_no_access_lines_takes_the_direct_read() {
    let me = Some("me");
    let line = |name: &str| format!("      7 drwxr-xr-x  2 me  staff  64 Nov 14 03:33 {name}\n");
    let text = format!("{}{} 0: user:me allow write\n", line("/a"), line("/b"));
    let unreadable = |index: usize| {
        assert_eq!(index, 0, "an entry with access lines was read again");
        None
    };
    assert_eq!(
        acl::listed_shared_all(&text, &["/a", "/b"], &[7; 2], me, &unreadable),
        vec![None, Some(false)]
    );
}

/// A direct read that finds no list confirms none; one that finds a list `ls` did not print,
/// or cannot read, is not known, so the entry counts as changeable.
#[cfg(unix)]
#[test]
fn a_direct_read_confirms_only_a_list_that_is_not_there() {
    assert_eq!(acl::confirmed(Ok(false)), Some(false));
    assert_eq!(acl::confirmed(Ok(true)), None);
    assert_eq!(
        acl::confirmed(Err(std::io::Error::from_raw_os_error(13))),
        None
    );
}

/// Each block's first field is the inode `ls -i` read, padded to one width; a block whose
/// inode is not the one the entry was stamped with, or does not parse, is about another
/// file, so its entry reads none and is never read again.
#[cfg(unix)]
#[test]
fn a_block_whose_inode_is_not_the_stamped_one_reads_none() {
    let me = Some("me");
    let text = concat!(
        "        2 drwxr-xr-x  2 me  staff  64 Nov 14 03:33 /a\n",
        "609424587 -rw-r--r--  1 me  staff   0 Nov 14 03:33 /b\n",
        " 0: group:everyone allow write\n",
    );
    let entries = ["/a", "/b"];
    let never = |index: usize| -> Option<bool> { panic!("entry {index} was read again") };
    assert_eq!(
        acl::listed_shared_all(text, &entries, &[2, 609424587], me, &|_| Some(false)),
        vec![Some(false), Some(true)]
    );
    assert_eq!(
        acl::listed_shared_all(text, &entries, &[3, 609424587], me, &never),
        vec![None, Some(true)]
    );
    assert_eq!(
        acl::listed_shared_all(text, &entries, &[2, 609424588], me, &|_| Some(false)),
        vec![Some(false), None]
    );
    let bare = "drwxr-xr-x  2 me  staff  64 Nov 14 03:33 /a\n";
    assert_eq!(
        acl::listed_shared_all(bare, &["/a"], &[2], me, &never),
        vec![None]
    );
}

/// RV-DS241-18: a Linux `system.posix_acl_access` value names an entry changeable by others
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

/// RV-DS241-18: on macOS a folder whose mode bits are private but whose access list lets
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
/// changed, so an access answer read now is kept. Input: the entries.
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

/// Waits, at most 1.5 s, until the clock is in the first half of a second, so a change and
/// the listing made right after it share one second.
#[cfg(target_os = "macos")]
fn early_in_a_second() {
    for _ in 0..30 {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        if since.subsec_millis() < 500 {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the clock never read the first half of a second");
}

/// An answer is kept only when its entry settled before the listing, a second `lstat` after
/// the listing finds the same device, inode and change time, and the answer came from the
/// listing's own lines, never from the direct read.
#[cfg(target_os = "macos")]
#[test]
fn an_access_answer_is_kept_only_for_the_file_its_stamp_names() {
    let stamp = (1, 2, 100, 0);
    assert!(acl::keep(&stamp, Some(&stamp), false, 101));
    assert!(!acl::keep(&stamp, Some(&stamp), false, 100), "not settled");
    assert!(!acl::keep(&stamp, None, false, 101), "no second lstat");
    for moved in [
        (9, 2, 100, 0),
        (1, 9, 100, 0),
        (1, 2, 101, 0),
        (1, 2, 100, 1),
    ] {
        assert!(!acl::keep(&stamp, Some(&moved), false, 101), "{moved:?}");
    }
    assert!(
        !acl::keep(&stamp, Some(&stamp), true, 101),
        "the direct read"
    );
}

/// HFS-CTIME: an access answer is kept only when its listing started in a later second than
/// the entry last changed, so a later change moves the stamp even where change times keep
/// whole seconds (HFS+), and an answer read in the second of the change is never kept.
#[cfg(target_os = "macos")]
#[test]
fn an_access_answer_is_kept_only_once_its_entry_changed_in_an_earlier_second() {
    assert!(acl::settled(&(1, 2, 100, 0), 101));
    assert!(acl::settled(&(1, 2, 100, 999_999_999), 101));
    assert!(!acl::settled(&(1, 2, 100, 0), 100), "the same whole second");
    assert!(
        !acl::settled(&(1, 2, 100, 999_999_999), 100),
        "the same second, to the nanosecond"
    );
    assert!(
        !acl::settled(&(1, 2, 105, 0), 100),
        "a change time ahead of the clock"
    );
}

/// Changes a tool's folder and reads the tool's access answer within one second, at most
/// three tries, since a slow spawn can carry the listing into the next second: the first try
/// gives staff `add_file` on the folder's access list, a later one flips its mode, which
/// keeps that entry. Each read lists the folder alone. Inputs: the tool, its folder, the root.
#[cfg(target_os = "macos")]
fn changed_and_read_in_one_second(tool: &Path, folder: &Path, root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    for attempt in 0..3u32 {
        early_in_a_second();
        if attempt == 0 {
            let status = crate::spawn_gate::status(
                std::process::Command::new("/bin/chmod")
                    .arg("+a")
                    .arg("group:staff allow add_file,delete_child")
                    .arg(folder),
            )
            .unwrap();
            assert!(status.success());
        } else {
            let mode = if attempt == 1 { 0o750 } else { 0o755 };
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        assert_eq!(changeable_by_others(tool), Some(folder.to_path_buf()));
        let changed = acl::take_listings();
        assert_eq!(changed.len(), 1, "{changed:?}");
        let ours: Vec<PathBuf> = changed[0]
            .iter()
            .filter(|listed| listed.starts_with(root))
            .cloned()
            .collect();
        assert_eq!(ours, vec![folder.to_path_buf()], "the changed part alone");
        if now_secs() == changed_secs(folder) {
            return;
        }
    }
    panic!("no change and its listing shared a second in three tries");
}

/// RV-DS241-18 r2: one `ls -lde` lists every part of a path the first time, names with a
/// space or an accent included; a repeat lists none of the parts it read, and an access-list
/// change moves only the changed part's change time, so that part alone is listed again and
/// the change counts. HFS-CTIME: a part read in the second it changed is read again once
/// that second has passed, and only then kept.
#[cfg(target_os = "macos")]
#[test]
fn one_access_listing_reads_a_whole_path_and_a_repeat_lists_only_changed_parts() {
    let temp = durable_folder();
    let kit = temp.path().join("tool kit \u{e9}");
    let folder = kit.join("bin");
    let tool = folder.join("tmux");
    write_mode(&tool, "#!/bin/sh\n", 0o755);
    settle(&[&tool, &folder, &kit, temp.path()]);
    let ours = |listings: &[Vec<PathBuf>]| -> Vec<PathBuf> {
        listings
            .iter()
            .flatten()
            .filter(|listed| listed.starts_with(temp.path()))
            .cloned()
            .collect()
    };
    acl::take_listings();
    assert_eq!(changeable_by_others(&tool), None);
    let first = acl::take_listings();
    assert_eq!(first.len(), 1, "one listing for every part: {first:?}");
    for part in [&tool, &folder, &kit] {
        assert!(first[0].contains(part), "{part:?} in {first:?}");
    }

    assert_eq!(changeable_by_others(&tool), None);
    let repeat = acl::take_listings();
    assert!(repeat.len() <= 1, "{repeat:?}");
    assert_eq!(
        ours(&repeat),
        Vec::<PathBuf>::new(),
        "nothing read is listed again"
    );

    changed_and_read_in_one_second(&tool, &folder, temp.path());
    settle(&[&folder]);
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(
        ours(&acl::take_listings()),
        vec![folder.clone()],
        "a part read in the second it changed is read again"
    );
    assert_eq!(changeable_by_others(&tool), Some(folder.clone()));
    assert_eq!(
        ours(&acl::take_listings()),
        Vec::<PathBuf>::new(),
        "read in a later second, it is kept"
    );
}
