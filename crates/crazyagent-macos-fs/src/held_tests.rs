use super::{HELD_REPLY_FIXED, HELD_REQUIRED, held_entry, held_entry_at, held_reply, open_search};
use std::os::fd::AsFd;
use std::os::unix::fs::MetadataExt;

/// A `kauth_filesec` holding the entries. Input: the entries. Output: the bytes.
fn filesec(entries: &[([u8; 16], u32, u32)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0x012c_c16du32.to_ne_bytes());
    bytes.extend_from_slice(&[0u8; 32]);
    bytes.extend_from_slice(&u32::try_from(entries.len()).unwrap().to_ne_bytes());
    bytes.extend_from_slice(&0u32.to_ne_bytes());
    for (principal, flags, rights) in entries {
        bytes.extend_from_slice(principal);
        bytes.extend_from_slice(&flags.to_ne_bytes());
        bytes.extend_from_slice(&rights.to_ne_bytes());
    }
    bytes
}

/// A held entry reply packed as `getattrlist` packs it: the length, the returned set, the
/// device 5, folder type, change time (100, 7), owner 501, group 20, mode 0o40755, the
/// filesec reference and file id 9, then the filesec. Inputs: the length field, the returned
/// set and the filesec, if any. Output: the bytes.
fn reply(length: Option<u32>, returned: u32, list: Option<&[u8]>) -> Vec<u8> {
    let mut bytes = vec![0u8; HELD_REPLY_FIXED];
    bytes[4..8].copy_from_slice(&returned.to_ne_bytes());
    bytes[24..28].copy_from_slice(&5i32.to_ne_bytes());
    bytes[28..32].copy_from_slice(&2u32.to_ne_bytes());
    bytes[32..40].copy_from_slice(&100i64.to_ne_bytes());
    bytes[40..48].copy_from_slice(&7i64.to_ne_bytes());
    bytes[48..52].copy_from_slice(&501u32.to_ne_bytes());
    bytes[52..56].copy_from_slice(&20u32.to_ne_bytes());
    bytes[56..60].copy_from_slice(&0o40755u32.to_ne_bytes());
    bytes[68..76].copy_from_slice(&9u64.to_ne_bytes());
    if let Some(list) = list {
        let offset = i32::try_from(HELD_REPLY_FIXED - 60).unwrap();
        bytes[60..64].copy_from_slice(&offset.to_ne_bytes());
        bytes[64..68].copy_from_slice(&u32::try_from(list.len()).unwrap().to_ne_bytes());
        bytes.extend_from_slice(list);
    }
    let own = u32::try_from(bytes.len()).unwrap();
    bytes[0..4].copy_from_slice(&length.unwrap_or(own).to_ne_bytes());
    bytes
}

/// A held reply reads every field at its packed offset, and only when whole: a length past
/// the reply or short of the fixed part, a missing required attribute, a reference past the
/// length, a negative one, or one pointing into the fixed part is an error.
#[test]
fn a_held_reply_reads_each_field_only_when_whole() {
    let full = libc::ATTR_CMN_RETURNED_ATTRS | HELD_REQUIRED | libc::ATTR_CMN_EXTENDED_SECURITY;
    let list = filesec(&[([3u8; 16], 1, 1 << 4)]);
    let whole = reply(None, full, Some(&list));
    let read = held_reply(&whole).unwrap();
    assert_eq!((read.device, read.inode, read.changed), (5, 9, (100, 7)));
    assert_eq!(
        (read.kind, read.owner, read.group, read.mode),
        (2, 501, 20, 0o40755)
    );
    assert!(read.folder() && !read.link());
    assert_eq!(read.entries.len(), 1);
    let none = held_reply(&reply(None, full & !libc::ATTR_CMN_EXTENDED_SECURITY, None));
    assert!(none.unwrap().entries.is_empty());
    let cut = u32::try_from(whole.len() + 1).unwrap();
    assert!(held_reply(&reply(Some(cut), full, Some(&list))).is_err());
    assert!(held_reply(&whole[..whole.len() - 1]).is_err());
    assert!(held_reply(&reply(Some(60), full, None)).is_err());
    for missing in [
        libc::ATTR_CMN_DEVID,
        libc::ATTR_CMN_OBJTYPE,
        libc::ATTR_CMN_CHGTIME,
        libc::ATTR_CMN_OWNERID,
        libc::ATTR_CMN_GRPID,
        libc::ATTR_CMN_ACCESSMASK,
        libc::ATTR_CMN_FILEID,
    ] {
        assert!(
            held_reply(&reply(None, full & !missing, Some(&list))).is_err(),
            "{missing:#x}"
        );
    }
    let mut past = whole.clone();
    past[64..68].copy_from_slice(&u32::try_from(list.len() + 24).unwrap().to_ne_bytes());
    assert!(held_reply(&past).is_err());
    let mut negative = whole.clone();
    negative[60..64].copy_from_slice(&(-4i32).to_ne_bytes());
    assert!(held_reply(&negative).is_err());
    let mut inside = whole;
    inside[60..64].copy_from_slice(&4i32.to_ne_bytes());
    assert!(held_reply(&inside).is_err());
}

/// A real folder, file and link read through an open folder name the same device, file id,
/// change time, owner, group and permission bits `lstat` reads, the folder read through its
/// own search handle too, and a link is read itself and refused by a no-follow open.
#[test]
fn held_reads_match_lstat_for_a_folder_a_file_and_a_link() {
    let home = std::env::temp_dir().join(format!("held-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("file"), b"x").unwrap();
    std::os::unix::fs::symlink("file", home.join("link")).unwrap();
    let path = std::ffi::CString::new(home.as_os_str().as_encoded_bytes()).unwrap();
    let folder = open_search(None, &path, false).unwrap();
    let same = |read: &super::HeldEntry, entry: &std::path::Path| {
        let seen = std::fs::symlink_metadata(entry).unwrap();
        assert_eq!(
            (
                read.device,
                read.inode,
                read.changed,
                read.owner,
                read.group
            ),
            (
                seen.dev(),
                seen.ino(),
                (seen.ctime(), seen.ctime_nsec()),
                seen.uid(),
                seen.gid()
            ),
            "{}",
            entry.display()
        );
        assert_eq!(
            read.mode & 0o7777,
            seen.mode() & 0o7777,
            "{}",
            entry.display()
        );
    };
    let held = held_entry(folder.as_fd()).unwrap();
    same(&held, &home);
    assert!(held.folder() && !held.link());
    let file = held_entry_at(folder.as_fd(), c"file").unwrap();
    same(&file, &home.join("file"));
    assert!(!file.folder() && !file.link());
    let link = held_entry_at(folder.as_fd(), c"link").unwrap();
    same(&link, &home.join("link"));
    assert!(link.link());
    assert!(open_search(Some(folder.as_fd()), c"link", false).is_err());
    std::fs::remove_dir_all(&home).unwrap();
}
