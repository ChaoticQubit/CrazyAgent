use crate::plan::{Access, READ_EXECUTE};

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
        if matches!(kind, ACCESS_ALLOWED | ACCESS_DENIED) {
            let mask = u32::from_le_bytes([acl[at + 4], acl[at + 5], acl[at + 6], acl[at + 7]]);
            let sid = sid_at(&acl[at + ACE_HEADER_BYTES..at + length])?;
            found.push(Entry {
                allows: kind == ACCESS_ALLOWED,
                flags,
                mask,
                sid,
            });
        }
        at += length;
    }
    assert!(at <= size);
    Ok(found)
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
/// and inherited by files and folders below when the path is a folder. Inputs: the access list
/// bytes, the SID bytes, the access and whether the path is a folder. Output: true when present;
/// false for a malformed list, so the entry is written again.
pub fn holds(acl: &[u8], sid: &[u8], access: Access, folder: bool) -> bool {
    let inherit = OBJECT_INHERIT | CONTAINER_INHERIT;
    entries(acl).is_ok_and(|found| {
        found.iter().any(|entry| {
            entry.allows != access.denies()
                && entry.sid == sid
                && entry.mask & access.mask() == access.mask()
                && entry.flags & (INHERITED | INHERIT_ONLY) == 0
                && (!folder || entry.flags & inherit == inherit)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{DENY_WRITE, MODIFY};

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
}
