use crate::plan::{Access, READ_EXECUTE};
use std::ops::Range;

/// The SID `S-1-15-2-1` (ALL APPLICATION PACKAGES) as bytes. System folders such as
/// `C:\Windows` and `C:\Program Files` grant it read and run, so every container reads them.
pub const ALL_APPLICATION_PACKAGES: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 1, 0, 0, 0];

const ACL_HEADER_BYTES: usize = 8;
const ACE_HEADER_BYTES: usize = 8;
const ACCESS_ALLOWED: u8 = 0;
const ACCESS_DENIED: u8 = 1;
const OBJECT_INHERIT: u8 = 0x01;
const CONTAINER_INHERIT: u8 = 0x02;
const INHERIT_ONLY: u8 = 0x08;
const INHERITED: u8 = 0x10;
/// The most bytes a SID takes: its header and fifteen sub-authorities.
pub const SID_BYTES_MAX: usize = 68;

/// One allow or deny entry read from an access list: its type, flags, rights and SID bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub allows: bool,
    pub flags: u8,
    pub mask: u32,
    pub sid: &'a [u8],
}

/// Reads the allow and deny entries of an access list copied out of Windows, checking every
/// size against the bytes present; entries of other types are skipped. Input: the access list
/// bytes. Output: the entries, or the plain reason the bytes are not a well-formed list.
pub fn entries(acl: &[u8]) -> Result<Vec<Entry<'_>>, &'static str> {
    Ok(spans(acl)?
        .into_iter()
        .filter_map(|span| span.entry)
        .collect())
}

/// The bytes one entry takes in an access list, and the allow or deny entry they hold (None
/// for other types).
struct Span<'a> {
    range: Range<usize>,
    entry: Option<Entry<'a>>,
}

/// Splits an access list into its entries' spans, checking every size against the bytes
/// present.
fn spans(acl: &[u8]) -> Result<Vec<Span<'_>>, &'static str> {
    if acl.len() < ACL_HEADER_BYTES {
        return Err("the access list is shorter than its header");
    }
    let size = usize::from(u16::from_le_bytes([acl[2], acl[3]]));
    let count = usize::from(u16::from_le_bytes([acl[4], acl[5]]));
    if size < ACL_HEADER_BYTES || size > acl.len() {
        return Err("the access list size does not match its bytes");
    }
    let mut at = ACL_HEADER_BYTES;
    let mut found = Vec::with_capacity(count);
    for _ in 0..count {
        if at + ACE_HEADER_BYTES > size {
            return Err("an access entry runs past the list");
        }
        let kind = acl[at];
        let flags = acl[at + 1];
        let length = usize::from(u16::from_le_bytes([acl[at + 2], acl[at + 3]]));
        if length < ACE_HEADER_BYTES || at + length > size {
            return Err("an access entry has a bad size");
        }
        let entry = if matches!(kind, ACCESS_ALLOWED | ACCESS_DENIED) {
            let mask = u32::from_le_bytes([acl[at + 4], acl[at + 5], acl[at + 6], acl[at + 7]]);
            let sid = sid_at(&acl[at + ACE_HEADER_BYTES..at + length])?;
            Some(Entry {
                allows: kind == ACCESS_ALLOWED,
                flags,
                mask,
                sid,
            })
        } else {
            None
        };
        found.push(Span {
            range: at..at + length,
            entry,
        });
        at += length;
    }
    assert!(at <= size);
    Ok(found)
}

/// The access list without the entries the boundary puts there for `sid`: every allow or
/// deny entry of that SID that is not inherited goes, inherit-only ones included, and every
/// other entry is kept byte for byte and in order, so the list stays in Windows' canonical
/// order. Unused room at the end is dropped. Inputs: the access list bytes and the SID.
/// Output: the new list, None when it holds no such entry, or the reason it is malformed.
pub fn without(acl: &[u8], sid: &[u8]) -> Result<Option<Vec<u8>>, &'static str> {
    let spans = spans(acl)?;
    let total = spans.len();
    let kept: Vec<Range<usize>> = spans
        .into_iter()
        .filter(|span| {
            !span
                .entry
                .is_some_and(|entry| entry.sid == sid && entry.flags & INHERITED == 0)
        })
        .map(|span| span.range)
        .collect();
    if kept.len() == total {
        return Ok(None);
    }
    let body: Vec<u8> = kept
        .iter()
        .flat_map(|range| &acl[range.clone()])
        .copied()
        .collect();
    let size = u16::try_from(ACL_HEADER_BYTES + body.len()).expect("a list only shrinks");
    let count = u16::try_from(kept.len()).expect("a list only shrinks");
    let mut list = Vec::with_capacity(usize::from(size));
    list.extend_from_slice(&acl[..2]);
    list.extend(size.to_le_bytes());
    list.extend(count.to_le_bytes());
    list.extend_from_slice(&acl[6..ACL_HEADER_BYTES]);
    list.extend(body);
    assert_eq!(list.len(), usize::from(size));
    assert!(list.len() < acl.len());
    Ok(Some(list))
}

/// Whether the list gives `sid` more than `mask` through an allow entry of its own, explicit
/// or inherit-only, as when an earlier profile granted modify where this one grants read and
/// run. Inherited entries follow the folder above and are not counted. Inputs: the access
/// list bytes, the SID and the planned rights. Output: true when it does; true for a
/// malformed list, so it is never taken as settled.
pub fn exceeds(acl: &[u8], sid: &[u8], mask: u32) -> bool {
    entries(acl).map_or(true, |found| {
        found.iter().any(|entry| {
            entry.allows
                && entry.sid == sid
                && entry.flags & INHERITED == 0
                && entry.mask & !mask != 0
        })
    })
}

fn sid_at(bytes: &[u8]) -> Result<&[u8], &'static str> {
    if bytes.len() < 8 || bytes[0] != 1 {
        return Err("an access entry holds a bad SID");
    }
    let length = 8 + 4 * usize::from(bytes[1]);
    if length > bytes.len() || length > SID_BYTES_MAX {
        return Err("an access entry holds a bad SID");
    }
    Ok(&bytes[..length])
}

/// Whether the list already holds the explicit entry the boundary would add for `sid`: the same
/// allow or deny type, at least the access's rights, not inherited, applying to the path itself,
/// and inherited by files and folders below when the path is a folder and the access inherits.
/// Inputs: the access list bytes, the SID bytes, the access and whether the path is a folder.
/// Output: true when present; false for a malformed list, so the entry is written again.
pub fn holds(acl: &[u8], sid: &[u8], access: Access, folder: bool) -> bool {
    let inherit = OBJECT_INHERIT | CONTAINER_INHERIT;
    entries(acl).is_ok_and(|found| {
        found.iter().any(|entry| {
            entry.allows != access.denies()
                && entry.sid == sid
                && entry.mask & access.mask() == access.mask()
                && entry.flags & (INHERITED | INHERIT_ONLY) == 0
                && (!folder || !access.inherits() || entry.flags & inherit == inherit)
        })
    })
}

/// Whether the list lets `sid` read and run the path itself through any allow entry, explicit
/// or inherited. Inputs: the access list bytes and the SID bytes. Output: true when it does;
/// false for a malformed list.
pub fn reads(acl: &[u8], sid: &[u8]) -> bool {
    entries(acl).is_ok_and(|found| {
        found.iter().any(|entry| {
            entry.allows
                && entry.sid == sid
                && entry.flags & INHERIT_ONLY == 0
                && entry.mask & READ_EXECUTE == READ_EXECUTE
        })
    })
}

/// The SID `S-1-5-18` (SYSTEM) as bytes.
pub const SYSTEM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
/// The SID `S-1-5-32-544` (the Administrators group) as bytes.
pub const ADMINISTRATORS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0];
/// The SID of the TrustedInstaller service as bytes:
/// `S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464`.
pub const TRUSTED_INSTALLER: [u8; 32] = [
    1, 6, 0, 0, 0, 0, 0, 5, 80, 0, 0, 0, 181, 137, 251, 56, 25, 132, 194, 203, 92, 108, 35, 109,
    87, 0, 119, 110, 192, 2, 100, 135,
];
/// The SID `S-1-3-4` (OWNER RIGHTS) as bytes, which stands for whoever owns the object.
pub const OWNER_RIGHTS: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 3, 4, 0, 0, 0];

const WRITE_DATA: u32 = 0x0000_0002;
const APPEND_DATA: u32 = 0x0000_0004;
const WRITE_EA: u32 = 0x0000_0010;
const DELETE_CHILD: u32 = 0x0000_0040;
const WRITE_ATTRIBUTES: u32 = 0x0000_0100;
const DELETE: u32 = 0x0001_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Every right that changes a file: its data, its extended attributes and attributes, its
/// deletion, its access list or owner, and generic write or all, which map onto those.
pub const FILE_CHANGES: u32 = WRITE_DATA
    | APPEND_DATA
    | WRITE_EA
    | WRITE_ATTRIBUTES
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL;
/// Every right that changes a program's own folder: those of a file, where writing and
/// appending data are adding a file or a folder, plus deleting what is in it, so no one can
/// plant a library beside the program.
pub const OWN_FOLDER_CHANGES: u32 = FILE_CHANGES | DELETE_CHILD;
/// The rights on a folder further up that can swap what lies below it: deleting what is in
/// it or the folder itself, changing its access list or owner, and generic write or all.
/// Adding a new file or folder alone swaps nothing that is already there.
pub const ANCESTOR_CHANGES: u32 =
    DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL;

/// Whether an owner may hold a program the engine starts with its own rights, or a folder
/// above it: SYSTEM, the administrators, TrustedInstaller or the engine's own user, who can
/// change it anyway. Inputs: the owner's SID bytes and the user's. Output: the answer.
pub fn trusted_owner(owner: &[u8], user: &[u8]) -> bool {
    assert!(user.len() >= ACE_HEADER_BYTES);
    [
        &SYSTEM[..],
        &ADMINISTRATORS[..],
        &TRUSTED_INSTALLER[..],
        user,
    ]
    .contains(&owner)
}

/// The first SID other than SYSTEM, the administrators, TrustedInstaller, OWNER RIGHTS and
/// the engine's own user that an allow entry applying to the object itself gives any of
/// `rights`: inherited entries count, inherit-only ones do not, and deny entries are not
/// counted, as they only take rights away. Inputs: the access list bytes, the user's SID bytes
/// and the rights. Output: that SID, none when only those may change the object, or the
/// reason the list is malformed.
pub fn other_writer<'a>(
    acl: &'a [u8],
    user: &[u8],
    rights: u32,
) -> Result<Option<&'a [u8]>, &'static str> {
    assert_ne!(rights, 0);
    assert!(user.len() >= ACE_HEADER_BYTES);
    let allowed = [
        &SYSTEM[..],
        &ADMINISTRATORS[..],
        &TRUSTED_INSTALLER[..],
        &OWNER_RIGHTS[..],
        user,
    ];
    Ok(entries(acl)?
        .into_iter()
        .find(|entry| {
            entry.allows
                && entry.flags & INHERIT_ONLY == 0
                && entry.mask & rights != 0
                && !allowed.contains(&entry.sid)
        })
        .map(|entry| entry.sid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{DENY_WRITE, KEEP_ENTRIES, MODIFY};

    const CONTAINER: [u8; 20] = [1, 3, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 7, 0, 0, 0, 9, 0, 0, 0];

    fn ace(kind: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
        let length = (ACE_HEADER_BYTES + sid.len()) as u16;
        let mut bytes = vec![kind, flags];
        bytes.extend(length.to_le_bytes());
        bytes.extend(mask.to_le_bytes());
        bytes.extend(sid);
        bytes
    }

    fn acl(aces: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = aces.concat();
        let size = (ACL_HEADER_BYTES + body.len()) as u16;
        let mut bytes = vec![2, 0];
        bytes.extend(size.to_le_bytes());
        bytes.extend((aces.len() as u16).to_le_bytes());
        bytes.extend([0, 0]);
        bytes.extend(body);
        bytes
    }

    #[test]
    fn entries_read_allow_and_deny_and_skip_other_types() {
        let object_ace = {
            let mut bytes = vec![5, 0];
            bytes.extend(16u16.to_le_bytes());
            bytes.extend([0; 12]);
            bytes
        };
        let list = acl(&[
            ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER),
            object_ace,
            ace(ACCESS_DENIED, 0x13, DENY_WRITE, &ALL_APPLICATION_PACKAGES),
        ]);
        let found = entries(&list).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            Entry {
                allows: true,
                flags: 0x03,
                mask: MODIFY,
                sid: &CONTAINER
            }
        );
        assert!(!found[1].allows);
        assert_eq!(found[1].sid, &ALL_APPLICATION_PACKAGES[..]);
    }

    #[test]
    fn malformed_lists_are_refused_not_read_past() {
        let good = acl(&[ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER)]);
        assert!(entries(&good[..7]).is_err());
        let mut short = good.clone();
        short[2..4].copy_from_slice(&200u16.to_le_bytes());
        assert!(entries(&short).is_err());
        let mut count = good.clone();
        count[4..6].copy_from_slice(&2u16.to_le_bytes());
        assert!(entries(&count).is_err());
        let mut ace_size = good.clone();
        ace_size[10..12].copy_from_slice(&4u16.to_le_bytes());
        assert!(entries(&ace_size).is_err());
        let mut sid_count = good.clone();
        sid_count[17] = 40;
        assert!(entries(&sid_count).is_err());
        let mut revision = good.clone();
        revision[16] = 2;
        assert!(entries(&revision).is_err());
        assert!(!holds(&sid_count, &CONTAINER, Access::Modify, true));
        assert!(!reads(&revision, &CONTAINER));
        assert!(entries(&acl(&[])).unwrap().is_empty());
    }

    #[test]
    fn holds_finds_only_the_explicit_entry_the_boundary_writes() {
        let folder = acl(&[ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER)]);
        assert!(holds(&folder, &CONTAINER, Access::Modify, true));
        assert!(holds(&folder, &CONTAINER, Access::ReadExecute, true));
        assert!(!holds(&folder, &CONTAINER, Access::DenyWrite, true));
        assert!(!holds(
            &folder,
            &ALL_APPLICATION_PACKAGES,
            Access::Modify,
            true
        ));
        let inherited = acl(&[ace(ACCESS_ALLOWED, 0x13, MODIFY, &CONTAINER)]);
        assert!(!holds(&inherited, &CONTAINER, Access::Modify, true));
        let inherit_only = acl(&[ace(ACCESS_ALLOWED, 0x0b, MODIFY, &CONTAINER)]);
        assert!(!holds(&inherit_only, &CONTAINER, Access::Modify, true));
        let file = acl(&[ace(ACCESS_ALLOWED, 0x00, MODIFY, &CONTAINER)]);
        assert!(holds(&file, &CONTAINER, Access::Modify, false));
        assert!(!holds(&file, &CONTAINER, Access::Modify, true));
        let denied = acl(&[ace(ACCESS_DENIED, 0x03, DENY_WRITE, &CONTAINER)]);
        assert!(holds(&denied, &CONTAINER, Access::DenyWrite, true));
        assert!(!holds(&denied, &CONTAINER, Access::DenyAll, true));
        assert!(!holds(&denied, &CONTAINER, Access::Modify, true));
        assert!(holds(&denied, &CONTAINER, Access::KeepEntries, true));
        let kept = acl(&[ace(ACCESS_DENIED, 0x00, KEEP_ENTRIES, &CONTAINER)]);
        assert!(holds(&kept, &CONTAINER, Access::KeepEntries, true));
        assert!(!holds(&kept, &CONTAINER, Access::DenyWrite, true));
        assert!(!holds(
            &kept,
            &ALL_APPLICATION_PACKAGES,
            Access::KeepEntries,
            true
        ));
    }

    #[test]
    fn without_drops_only_the_containers_own_explicit_entries() {
        let object_ace = {
            let mut bytes = vec![5, 0];
            bytes.extend(16u16.to_le_bytes());
            bytes.extend([0; 12]);
            bytes
        };
        let kept = [
            object_ace.clone(),
            ace(ACCESS_ALLOWED, 0x13, READ_EXECUTE, &CONTAINER),
            ace(
                ACCESS_ALLOWED,
                0x03,
                READ_EXECUTE,
                &ALL_APPLICATION_PACKAGES,
            ),
        ];
        let list = acl(&[
            ace(ACCESS_DENIED, 0x00, KEEP_ENTRIES, &CONTAINER),
            ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER),
            kept[0].clone(),
            kept[1].clone(),
            kept[2].clone(),
            ace(ACCESS_ALLOWED, 0x0b, MODIFY, &CONTAINER),
        ]);
        let stripped = without(&list, &CONTAINER).unwrap().unwrap();
        assert_eq!(
            stripped,
            acl(&kept),
            "explicit and inherit-only entries of the container go; inherited ones and every \
             other SID's stay byte for byte"
        );
        assert_eq!(without(&stripped, &CONTAINER).unwrap(), None);
        assert_eq!(
            without(&list, &[1, 1, 0, 0, 0, 0, 0, 5, 7, 0, 0, 0]),
            Ok(None)
        );
        let mut slack = list.clone();
        slack.extend([0; 8]);
        let size = u16::try_from(slack.len()).unwrap();
        slack[2..4].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            without(&slack, &CONTAINER).unwrap().unwrap(),
            acl(&kept),
            "unused room at the end of a list is dropped"
        );
        assert!(without(&list[..7], &CONTAINER).is_err());
        let mut count = list.clone();
        count[4..6].copy_from_slice(&7u16.to_le_bytes());
        assert!(without(&count, &CONTAINER).is_err());
        let only = acl(&[ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER)]);
        assert_eq!(without(&only, &CONTAINER).unwrap(), Some(acl(&[])));
    }

    #[test]
    fn exceeds_finds_an_explicit_grant_beyond_the_planned_rights() {
        let modify = acl(&[ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER)]);
        assert!(exceeds(&modify, &CONTAINER, READ_EXECUTE));
        assert!(!exceeds(&modify, &CONTAINER, MODIFY));
        assert!(!exceeds(&modify, &ALL_APPLICATION_PACKAGES, READ_EXECUTE));
        let inherited = acl(&[ace(ACCESS_ALLOWED, 0x13, MODIFY, &CONTAINER)]);
        assert!(
            !exceeds(&inherited, &CONTAINER, READ_EXECUTE),
            "an inherited grant follows the folder above"
        );
        let inherit_only = acl(&[ace(ACCESS_ALLOWED, 0x0b, MODIFY, &CONTAINER)]);
        assert!(exceeds(&inherit_only, &CONTAINER, READ_EXECUTE));
        let denied = acl(&[ace(ACCESS_DENIED, 0x03, DENY_WRITE, &CONTAINER)]);
        assert!(!exceeds(&denied, &CONTAINER, READ_EXECUTE));
        assert!(
            exceeds(&modify[..7], &CONTAINER, MODIFY),
            "a list that cannot be read is never taken as settled"
        );
    }

    #[test]
    fn reads_accepts_inherited_read_grants_for_all_application_packages() {
        let program_files = acl(&[
            ace(ACCESS_ALLOWED, 0x0b, 0xa000_0000, &ALL_APPLICATION_PACKAGES),
            ace(
                ACCESS_ALLOWED,
                0x10,
                READ_EXECUTE,
                &ALL_APPLICATION_PACKAGES,
            ),
        ]);
        assert!(reads(&program_files, &ALL_APPLICATION_PACKAGES));
        let generic_only = acl(&[ace(
            ACCESS_ALLOWED,
            0x0b,
            0xa000_0000,
            &ALL_APPLICATION_PACKAGES,
        )]);
        assert!(!reads(&generic_only, &ALL_APPLICATION_PACKAGES));
        let user_folder = acl(&[ace(ACCESS_ALLOWED, 0x03, 0x001f_01ff, &CONTAINER)]);
        assert!(!reads(&user_folder, &ALL_APPLICATION_PACKAGES));
        let denied = acl(&[ace(
            ACCESS_DENIED,
            0x00,
            READ_EXECUTE,
            &ALL_APPLICATION_PACKAGES,
        )]);
        assert!(!reads(&denied, &ALL_APPLICATION_PACKAGES));
    }

    const USERS: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 33, 2, 0, 0];
    const AUTHENTICATED_USERS: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 11, 0, 0, 0];
    const CREATOR_OWNER: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0];
    const OWNER: [u8; 28] = [
        1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 233, 3, 0, 0,
    ];
    const OTHER_ACCOUNT: [u8; 28] = [
        1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 234, 3, 0, 0,
    ];
    const FULL: u32 = 0x001F_01FF;
    const ADD_SUBDIRECTORY: u32 = 0x0000_0004;

    /// #306 sec F1: a program under Program Files, where TrustedInstaller, SYSTEM and the
    /// administrators may change it, every user and every container may only read and run it,
    /// and CREATOR OWNER's entry only passes down, is changed by no one else at any level.
    #[test]
    fn a_program_files_list_lets_only_the_system_change_a_program() {
        let list = acl(&[
            ace(ACCESS_ALLOWED, INHERITED, FULL, &TRUSTED_INSTALLER),
            ace(ACCESS_ALLOWED, INHERITED, MODIFY, &SYSTEM),
            ace(ACCESS_ALLOWED, INHERITED, MODIFY, &ADMINISTRATORS),
            ace(ACCESS_ALLOWED, INHERITED, READ_EXECUTE, &USERS),
            ace(
                ACCESS_ALLOWED,
                INHERITED,
                READ_EXECUTE,
                &ALL_APPLICATION_PACKAGES,
            ),
            ace(
                ACCESS_ALLOWED,
                OBJECT_INHERIT | CONTAINER_INHERIT | INHERIT_ONLY | INHERITED,
                GENERIC_ALL,
                &CREATOR_OWNER,
            ),
            ace(ACCESS_ALLOWED, 0, FULL, &OWNER_RIGHTS),
        ]);
        for rights in [FILE_CHANGES, OWN_FOLDER_CHANGES, ANCESTOR_CHANGES] {
            assert_eq!(other_writer(&list, &OWNER, rights), Ok(None));
        }
        let own = acl(&[ace(ACCESS_ALLOWED, INHERITED, FULL, &OWNER)]);
        assert_eq!(other_writer(&own, &OWNER, FILE_CHANGES), Ok(None));
    }

    /// #306 sec F1: every user with modify, another account, or an agent's container that can
    /// change a program is named, and a group that may only add a folder, as Authenticated
    /// Users may at a drive's root, passes as a folder further up but not as the program's own
    /// folder, where it could plant a library.
    #[test]
    fn another_account_or_a_container_that_can_change_a_program_is_named() {
        let users = acl(&[ace(ACCESS_ALLOWED, INHERITED, MODIFY, &USERS)]);
        assert_eq!(
            other_writer(&users, &OWNER, FILE_CHANGES),
            Ok(Some(&USERS[..]))
        );
        let other = acl(&[ace(ACCESS_ALLOWED, 0, DELETE, &OTHER_ACCOUNT)]);
        assert_eq!(
            other_writer(&other, &OWNER, ANCESTOR_CHANGES),
            Ok(Some(&OTHER_ACCOUNT[..]))
        );
        let container = acl(&[ace(ACCESS_ALLOWED, 0x03, MODIFY, &CONTAINER)]);
        assert_eq!(
            other_writer(&container, &OWNER, FILE_CHANGES),
            Ok(Some(&CONTAINER[..]))
        );
        let root = acl(&[ace(
            ACCESS_ALLOWED,
            0,
            ADD_SUBDIRECTORY,
            &AUTHENTICATED_USERS,
        )]);
        assert_eq!(other_writer(&root, &OWNER, ANCESTOR_CHANGES), Ok(None));
        assert_eq!(
            other_writer(&root, &OWNER, OWN_FOLDER_CHANGES),
            Ok(Some(&AUTHENTICATED_USERS[..]))
        );
    }

    /// #306 sec F1: an inherit-only entry, which passes down without applying, and a deny
    /// entry, which only takes rights away, name no one, while a malformed list is an error
    /// the caller refuses on.
    #[test]
    fn inherit_only_and_deny_entries_name_no_one_and_a_malformed_list_is_an_error() {
        let list = acl(&[
            ace(
                ACCESS_ALLOWED,
                OBJECT_INHERIT | CONTAINER_INHERIT | INHERIT_ONLY,
                MODIFY,
                &USERS,
            ),
            ace(ACCESS_DENIED, 0, MODIFY, &AUTHENTICATED_USERS),
        ]);
        assert_eq!(other_writer(&list, &OWNER, FILE_CHANGES), Ok(None));
        let mut broken = acl(&[ace(ACCESS_ALLOWED, 0, MODIFY, &USERS)]);
        broken[2..4].copy_from_slice(&200u16.to_le_bytes());
        assert!(other_writer(&broken, &OWNER, FILE_CHANGES).is_err());
    }

    /// #306 sec F1: SYSTEM, the administrators, TrustedInstaller and the engine's own user may
    /// own a program the engine starts; every user, another account and a container may not.
    #[test]
    fn only_the_system_the_administrators_trustedinstaller_or_the_user_may_own_a_program() {
        for owner in [&SYSTEM[..], &ADMINISTRATORS, &TRUSTED_INSTALLER, &OWNER] {
            assert!(trusted_owner(owner, &OWNER), "{owner:?}");
        }
        for owner in [&USERS[..], &OTHER_ACCOUNT, &CONTAINER, &OWNER_RIGHTS] {
            assert!(!trusted_owner(owner, &OWNER), "{owner:?}");
        }
    }
}
