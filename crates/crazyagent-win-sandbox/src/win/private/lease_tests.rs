use super::super::{Words, acl};
use super::{Create, create_under, dir, facts};
use crate::plan::{Access, Ace};
use std::ffi::OsStr;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const CONTAINER: [u8; 40] = [
    1, 8, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 11, 0, 0, 0, 22, 0, 0, 0, 33, 0, 0, 0, 44, 0, 0, 0, 55, 0,
    0, 0, 66, 0, 0, 0, 77, 0, 0, 0,
];
const INHERITED: u8 = 0x10;

/// A fresh plain long-name scratch path in the temp folder, with nothing at it. Input: the
/// test's name. Output: the path.
fn scratch(name: &str) -> PathBuf {
    let temp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let plain = PathBuf::from(temp.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let path = plain.join(format!("crazy-w3-5-lease-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    assert!(!path.exists());
    path
}

/// The flags of each entry an object's list holds for the test container. Input: the path.
/// Output: one flags byte per entry for the container.
fn container_entries(path: &Path) -> Vec<u8> {
    let object = std::fs::OpenOptions::new()
        .access_mode(0x0002_0000 | 0x80)
        .custom_flags(0x0020_0000 | 0x0200_0000)
        .open(path)
        .unwrap();
    let list = facts(&object).unwrap().list.unwrap();
    let count = usize::from(u16::from_le_bytes([list[4], list[5]]));
    let mut at = 8;
    let mut found = Vec::new();
    for _ in 0..count {
        let size = usize::from(u16::from_le_bytes([list[at + 2], list[at + 3]]));
        if list[at + 8..at + size] == CONTAINER[..] {
            found.push(list[at + 1]);
        }
        at += size;
    }
    found
}

/// rv-sec2 RULING-16:21 C1, row 2: the sandbox's own writer (`acl::role` then `acl::write`,
/// SetNamedSecurityInfoW) grants Modify on the lease root, and the grant reaches a folder and
/// a file `create_under` made before it, as inherited entries. Withdrawing through the same
/// writer takes the container off them and off a folder made while the grant held.
#[test]
fn the_sandbox_grant_on_a_lease_reaches_entries_made_before_it_and_its_withdrawal_leaves_none() {
    let root = scratch("grant");
    let held = dir(&root).unwrap();
    let folder = create_under(&held, OsStr::new("src"), Create::Folder).unwrap();
    let file = create_under(&folder, OsStr::new("main.rs"), Create::File).unwrap();
    drop((file, folder));
    let container = Words::from_bytes(&CONTAINER);
    let path = root.to_str().unwrap().to_owned();
    let ace = Ace {
        path: path.clone(),
        access: Access::Modify,
    };
    let step = acl::role(&container, &ace, None).unwrap();
    assert!(step.snapshot.is_none());
    acl::write(
        step.change
            .as_ref()
            .expect("a grant writes the lease root's list"),
    )
    .unwrap();
    let before = [root.join("src"), root.join("src").join("main.rs")];
    for entry in &before {
        let found = container_entries(entry);
        assert!(
            !found.is_empty() && found.iter().all(|flags| flags & INHERITED != 0),
            "{}: {found:?}",
            entry.display()
        );
    }
    let after = root.join("after");
    std::fs::create_dir(&after).unwrap();
    assert!(!container_entries(&after).is_empty());
    let withdrawal = acl::withdrawal(&container, &path, None, false).unwrap();
    assert!(withdrawal.snapshot.is_none());
    acl::write(
        withdrawal
            .change
            .as_ref()
            .expect("a withdrawal writes the lease root's list"),
    )
    .unwrap();
    for entry in before.iter().chain([&after]) {
        assert!(container_entries(entry).is_empty(), "{}", entry.display());
    }
    assert!(container_entries(&root).is_empty());
    drop(held);
    std::fs::remove_dir_all(&root).unwrap();
}
