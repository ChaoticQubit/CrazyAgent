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
const SID_BYTES_MAX: usize = 68;

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

/// The file rights each generic right stands for, as Windows maps them for files and folders:
/// read, write, execute and all.
const GENERIC_FILE_RIGHTS: [(u32, u32); 4] = [
    (0x8000_0000, 0x0012_0089),
    (0x4000_0000, 0x0012_0116),
    (0x2000_0000, 0x0012_00a0),
    (0x1000_0000, 0x001f_01ff),
];
const GENERIC_ALL_BITS: u32 = 0xf000_0000;

const _: () = assert!(GENERIC_FILE_RIGHTS[0].1 | GENERIC_FILE_RIGHTS[2].1 == READ_EXECUTE);

/// The rights a mask grants on a file or folder once its generic rights are replaced by the
/// file rights Windows maps them to, so an inheritable `GENERIC_READ | GENERIC_EXECUTE` entry
/// compares with read and run. Input: an access mask. Output: the mask without generic bits.
pub fn specific(mask: u32) -> u32 {
    let mapped = GENERIC_FILE_RIGHTS
        .iter()
        .filter(|(generic, _)| mask & generic != 0)
        .fold(mask & !GENERIC_ALL_BITS, |rights, (_, file)| rights | file);
    assert_eq!(mapped & GENERIC_ALL_BITS, 0);
    assert_eq!(mapped & mask & !GENERIC_ALL_BITS, mask & !GENERIC_ALL_BITS);
    mapped
}

/// Whether a SID names an app container or a capability (`S-1-15-2-*` or `S-1-15-3-*`), the
/// SIDs a container's access check counts: its own, a teammate's, ALL APPLICATION PACKAGES and
/// capabilities such as internetClient. Input: SID bytes. Output: true when it is one.
pub fn package(sid: &[u8]) -> bool {
    sid.len() >= 12
        && sid[0] == 1
        && sid[1] >= 1
        && sid[2..8] == [0, 0, 0, 0, 0, 15]
        && matches!(
            u32::from_le_bytes([sid[8], sid[9], sid[10], sid[11]]),
            2 | 3
        )
}

/// The access list a guarded path must hold, built from its current list: every inherited
/// entry turns explicit, since the path's inheritance is cut; entries of other SIDs keep
/// their rights; every allow entry of a package SID keeps at most `rights`, and goes when
/// nothing is left; the container's own entries are replaced by one allow of `rights`,
/// inherited by files and folders below when the path is a folder, or by none when `rights`
/// is empty. A list holding an entry type other than allow or deny is refused, so no entry
/// the boundary cannot read keeps granting a container. Inputs: the current list, the
/// container SID, the rights (read and run for a read-only path, none for a hidden one) and
/// whether the path is a folder. Output: the new list, None when the path already holds it,
/// or the reason the current list is refused.
pub fn converge(
    acl: &[u8],
    sid: &[u8],
    rights: u32,
    folder: bool,
) -> Result<Option<Vec<u8>>, &'static str> {
    assert!(package(sid), "a container SID is a package SID");
    assert!(rights == READ_EXECUTE || rights == 0);
    let spans = spans(acl)?;
    let own_flags = if folder {
        OBJECT_INHERIT | CONTAINER_INHERIT
    } else {
        0
    };
    let own = ace_bytes(ACCESS_ALLOWED, own_flags, rights, sid);
    let mut own_kept = rights == 0;
    let mut kept: Vec<Vec<u8>> = Vec::with_capacity(spans.len() + 1);
    for span in &spans {
        let entry = span
            .entry
            .ok_or("an access list holds an entry the boundary does not read")?;
        let mut bytes = acl[span.range.clone()].to_vec();
        bytes[1] = entry.flags & !INHERITED;
        if entry.sid == sid {
            if !own_kept && bytes == own {
                own_kept = true;
                kept.push(bytes);
            }
            continue;
        }
        if entry.allows && package(entry.sid) {
            let narrowed = specific(entry.mask) & rights;
            if narrowed == 0 {
                continue;
            }
            bytes[4..8].copy_from_slice(&narrowed.to_le_bytes());
        }
        kept.push(bytes);
    }
    if !own_kept {
        kept.push(own);
    }
    let list = list_from(&acl[..ACL_HEADER_BYTES], &kept)?;
    let size = usize::from(u16::from_le_bytes([acl[2], acl[3]]));
    assert!(entries(&list).is_ok_and(|found| found.iter().all(|e| e.flags & INHERITED == 0)));
    Ok((list.as_slice() != &acl[..size]).then_some(list))
}

/// One allow or deny entry's bytes. Inputs: the type, flags, rights and SID bytes. Output:
/// the entry.
fn ace_bytes(kind: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
    assert!(sid.len() <= SID_BYTES_MAX);
    let length = u16::try_from(ACE_HEADER_BYTES + sid.len()).expect("a SID is short");
    let mut bytes = Vec::with_capacity(usize::from(length));
    bytes.extend([kind, flags]);
    bytes.extend(length.to_le_bytes());
    bytes.extend(mask.to_le_bytes());
    bytes.extend_from_slice(sid);
    assert_eq!(bytes.len(), usize::from(length));
    bytes
}

/// An access list holding `aces` in order, with the revision and reserved bytes of `header`.
/// Inputs: the old list's header and the entries. Output: the list, or the refusal when it
/// would not fit an access list's size field.
fn list_from(header: &[u8], aces: &[Vec<u8>]) -> Result<Vec<u8>, &'static str> {
    assert_eq!(header.len(), ACL_HEADER_BYTES);
    const TOO_BIG: &str = "the access list would grow past its limit";
    let body: usize = aces.iter().map(Vec::len).sum();
    let size = u16::try_from(ACL_HEADER_BYTES + body).map_err(|_| TOO_BIG)?;
    let count = u16::try_from(aces.len()).map_err(|_| TOO_BIG)?;
    let mut list = Vec::with_capacity(usize::from(size));
    list.extend_from_slice(&header[..2]);
    list.extend(size.to_le_bytes());
    list.extend(count.to_le_bytes());
    list.extend_from_slice(&header[6..]);
    for ace in aces {
        list.extend_from_slice(ace);
    }
    assert_eq!(list.len(), usize::from(size));
    Ok(list)
}

/// The explicit part of an access list: every entry not inherited from the folder above, in
/// order, the part Windows keeps when the path's inheritance is turned back on. Input: the
/// list. Output: the explicit list, or the reason the list is malformed.
pub fn explicit(acl: &[u8]) -> Result<Vec<u8>, &'static str> {
    let spans = spans(acl)?;
    let kept: Vec<Vec<u8>> = spans
        .iter()
        .filter(|span| acl[span.range.start + 1] & INHERITED == 0)
        .map(|span| acl[span.range.clone()].to_vec())
        .collect();
    assert!(kept.len() <= spans.len());
    let list = list_from(&acl[..ACL_HEADER_BYTES], &kept)?;
    assert!(list.len() <= acl.len());
    Ok(list)
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

    const SYSTEM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
    const OWNER: [u8; 28] = [
        1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xe9, 3, 0, 0,
    ];
    const TEAMMATE: [u8; 20] = [1, 3, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 8, 0, 0, 0, 4, 0, 0, 0];
    const INTERNET_CLIENT: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 3, 0, 0, 0, 1, 0, 0, 0];
    const FULL: u32 = 0x001f_01ff;
    const GENERIC_READ_EXECUTE: u32 = 0xa000_0000;
    const OICI: u8 = OBJECT_INHERIT | CONTAINER_INHERIT;

    fn inherited_project_list() -> Vec<u8> {
        acl(&[
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &OWNER),
            ace(ACCESS_ALLOWED, OICI | INHERITED, MODIFY, &CONTAINER),
            ace(ACCESS_ALLOWED, OICI | INHERITED, MODIFY, &TEAMMATE),
            ace(
                ACCESS_ALLOWED,
                OICI | INHERIT_ONLY | INHERITED,
                GENERIC_READ_EXECUTE,
                &ALL_APPLICATION_PACKAGES,
            ),
        ])
    }

    #[test]
    fn package_sids_are_app_containers_and_capabilities_only() {
        assert!(package(&CONTAINER));
        assert!(package(&TEAMMATE));
        assert!(package(&ALL_APPLICATION_PACKAGES));
        assert!(package(&INTERNET_CLIENT));
        assert!(!package(&SYSTEM));
        assert!(!package(&OWNER));
        assert!(!package(&[1, 1, 0, 0, 0, 0, 0, 15, 1, 0, 0, 0]));
        assert!(!package(&[1, 0, 0, 0, 0, 0, 0, 15]));
        assert!(!package(&[]));
    }

    #[test]
    fn generic_rights_map_to_the_file_rights_windows_gives_them() {
        assert_eq!(specific(GENERIC_READ_EXECUTE), READ_EXECUTE);
        assert_eq!(specific(0x1000_0000), FULL);
        assert_eq!(specific(0x4000_0000), 0x0012_0116);
        assert_eq!(specific(MODIFY), MODIFY);
        assert_eq!(specific(0x8000_0002), 0x0012_008b);
    }

    #[test]
    fn a_read_only_folder_is_cut_and_every_package_keeps_read_and_run_at_most() {
        let cut = converge(&inherited_project_list(), &CONTAINER, READ_EXECUTE, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            cut,
            acl(&[
                ace(ACCESS_ALLOWED, OICI, FULL, &SYSTEM),
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                ace(ACCESS_ALLOWED, OICI, READ_EXECUTE, &TEAMMATE),
                ace(
                    ACCESS_ALLOWED,
                    OICI | INHERIT_ONLY,
                    READ_EXECUTE,
                    &ALL_APPLICATION_PACKAGES,
                ),
                ace(ACCESS_ALLOWED, OICI, READ_EXECUTE, &CONTAINER),
            ]),
            "inherited entries turn explicit, other SIDs stay as they were, a teammate's modify \
             narrows to read and run, and the container's own entry is replaced"
        );
        assert_eq!(
            converge(&cut, &CONTAINER, READ_EXECUTE, true),
            Ok(None),
            "a path already at its target is left alone"
        );
        let teammate = converge(&cut, &TEAMMATE, READ_EXECUTE, true).unwrap();
        assert_eq!(
            teammate, None,
            "a teammate guarding the same path finds the list it would set"
        );
    }

    #[test]
    fn a_read_only_file_gets_an_entry_that_inherits_nothing() {
        let file = acl(&[
            ace(ACCESS_ALLOWED, INHERITED, FULL, &OWNER),
            ace(ACCESS_ALLOWED, INHERITED, MODIFY, &CONTAINER),
        ]);
        assert_eq!(
            converge(&file, &CONTAINER, READ_EXECUTE, false)
                .unwrap()
                .unwrap(),
            acl(&[
                ace(ACCESS_ALLOWED, 0, FULL, &OWNER),
                ace(ACCESS_ALLOWED, 0, READ_EXECUTE, &CONTAINER),
            ])
        );
    }

    #[test]
    fn a_hidden_path_keeps_no_allow_entry_of_any_package() {
        let list = acl(&[
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &OWNER),
            ace(ACCESS_ALLOWED, OICI | INHERITED, MODIFY, &CONTAINER),
            ace(ACCESS_ALLOWED, OICI | INHERITED, MODIFY, &TEAMMATE),
            ace(
                ACCESS_ALLOWED,
                OICI | INHERIT_ONLY | INHERITED,
                GENERIC_READ_EXECUTE,
                &ALL_APPLICATION_PACKAGES,
            ),
            ace(ACCESS_ALLOWED, INHERITED, 0x2, &INTERNET_CLIENT),
            ace(ACCESS_DENIED, INHERITED, FULL, &TEAMMATE),
        ]);
        assert_eq!(
            converge(&list, &CONTAINER, 0, true).unwrap().unwrap(),
            acl(&[
                ace(ACCESS_ALLOWED, OICI, FULL, &SYSTEM),
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                ace(ACCESS_DENIED, 0, FULL, &TEAMMATE),
            ]),
            "every package allow goes, the owner's entries and a deny stay explicit"
        );
    }

    #[test]
    fn an_allow_narrowed_to_nothing_goes_and_a_deny_of_a_package_stays() {
        let list = acl(&[
            ace(ACCESS_DENIED, OICI | INHERITED, 0x2, &TEAMMATE),
            ace(ACCESS_ALLOWED, OICI | INHERITED, 0x2 | 0x4, &TEAMMATE),
            ace(
                ACCESS_ALLOWED,
                OICI | INHERITED,
                0x1000_0000,
                &INTERNET_CLIENT,
            ),
        ]);
        assert_eq!(
            converge(&list, &CONTAINER, READ_EXECUTE, true)
                .unwrap()
                .unwrap(),
            acl(&[
                ace(ACCESS_DENIED, OICI, 0x2, &TEAMMATE),
                ace(ACCESS_ALLOWED, OICI, READ_EXECUTE, &INTERNET_CLIENT),
                ace(ACCESS_ALLOWED, OICI, READ_EXECUTE, &CONTAINER),
            ])
        );
    }

    #[test]
    fn a_list_the_boundary_cannot_read_whole_is_refused() {
        let object_ace = {
            let mut bytes = vec![5, 0];
            bytes.extend(16u16.to_le_bytes());
            bytes.extend([0; 12]);
            bytes
        };
        let unknown = acl(&[ace(ACCESS_ALLOWED, 0, FULL, &OWNER), object_ace]);
        assert!(converge(&unknown, &CONTAINER, READ_EXECUTE, true).is_err());
        let good = inherited_project_list();
        assert!(converge(&good[..7], &CONTAINER, READ_EXECUTE, true).is_err());
        let mut slack = good.clone();
        slack.extend([0; 8]);
        let size = u16::try_from(slack.len()).unwrap();
        slack[2..4].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            converge(&slack, &CONTAINER, READ_EXECUTE, true),
            converge(&good, &CONTAINER, READ_EXECUTE, true),
            "unused room at the end of a list does not change the target"
        );
    }

    #[test]
    fn the_explicit_part_keeps_only_entries_not_inherited() {
        let list = acl(&[
            ace(ACCESS_DENIED, 0, 0x2, &TEAMMATE),
            ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
            ace(ACCESS_ALLOWED, INHERITED, MODIFY, &CONTAINER),
        ]);
        assert_eq!(
            explicit(&list).unwrap(),
            acl(&[
                ace(ACCESS_DENIED, 0, 0x2, &TEAMMATE),
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
            ])
        );
        assert_eq!(explicit(&acl(&[])).unwrap(), acl(&[]));
        assert!(explicit(&list[..7]).is_err());
    }
}
