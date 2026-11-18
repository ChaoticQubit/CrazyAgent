use crate::plan::{Access, READ_EXECUTE, Rights};
use std::ops::Range;

/// The SID `S-1-15-2-1` (ALL APPLICATION PACKAGES) as bytes. System folders such as
/// `C:\Windows` and `C:\Program Files` grant it read and run, so every container reads them.
pub const ALL_APPLICATION_PACKAGES: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 1, 0, 0, 0];

const ACL_HEADER_BYTES: usize = 8;
const ACE_HEADER_BYTES: usize = 8;
const ACL_REVISION: u8 = 2;
const ACCESS_ALLOWED: u8 = 0;
const ACCESS_DENIED: u8 = 1;
const MANDATORY_LABEL: u8 = 0x11;
const LABEL_AUTHORITY: [u8; 6] = [0, 0, 0, 0, 0, 16];
/// The entry types that only take rights away: the plain, object, callback and callback
/// object deny entries.
pub const DENY_TYPES: [u8; 4] = [ACCESS_DENIED, 6, 10, 12];
const OBJECT_INHERIT: u8 = 0x01;
const CONTAINER_INHERIT: u8 = 0x02;
const NO_PROPAGATE: u8 = 0x04;
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

/// The rights every app container needs on a fixed drive's root for Node.js to resolve a path
/// there: read attributes and synchronize, on the root itself (V244-WIN-ADMIN-STEP).
pub const DRIVE_ROOT_RIGHTS: u32 = 0x0010_0080;

/// Whether the list gives `sid` every right in `mask` on the object itself through one plain
/// allow entry, explicit or inherited, not inherit-only; callback, object and conditional
/// allow entries do not count. Inputs: the list, the SID and the rights. Output: true when it
/// does; false for a malformed list.
pub fn holds_allow(acl: &[u8], sid: &[u8], mask: u32) -> bool {
    assert_ne!(mask, 0);
    entries(acl).is_ok_and(|found| {
        found.iter().any(|entry| {
            entry.allows
                && entry.sid == sid
                && entry.flags & INHERIT_ONLY == 0
                && entry.mask & mask == mask
        })
    })
}

/// The size an access list's header gives it. Input: a list whose header [`spans`] checked.
/// Output: the size in bytes.
fn list_size(acl: &[u8]) -> usize {
    let size = usize::from(u16::from_le_bytes([acl[2], acl[3]]));
    assert!((ACL_HEADER_BYTES..=acl.len()).contains(&size));
    size
}

/// The list with one plain allow entry giving `sid` exactly `mask` on this object only (no
/// flags), placed after the last explicit entry and before the inherited ones; every other
/// byte, unused room at the end included, is kept. Inputs: the list, the SID and the rights.
/// Output: the new list, None when [`holds_allow`] already holds, or the reason it is
/// malformed or would pass an access list's size limit.
pub fn with_allow(acl: &[u8], sid: &[u8], mask: u32) -> Result<Option<Vec<u8>>, &'static str> {
    const TOO_BIG: &str = "the access list would grow past its limit";
    let spans = spans(acl)?;
    if holds_allow(acl, sid, mask) {
        return Ok(None);
    }
    let size = list_size(acl);
    let at = spans
        .iter()
        .rev()
        .find(|span| acl[span.range.start + 1] & INHERITED == 0)
        .map_or(ACL_HEADER_BYTES, |span| span.range.end);
    let entry = ace_bytes(ACCESS_ALLOWED, 0, mask, sid);
    let grown = u16::try_from(size + entry.len()).map_err(|_| TOO_BIG)?;
    let count = u16::try_from(spans.len() + 1).map_err(|_| TOO_BIG)?;
    let mut list = Vec::with_capacity(usize::from(grown));
    list.extend_from_slice(&acl[..2]);
    list.extend(grown.to_le_bytes());
    list.extend(count.to_le_bytes());
    list.extend_from_slice(&acl[6..at]);
    list.extend_from_slice(&entry);
    list.extend_from_slice(&acl[at..size]);
    assert_eq!(list.len(), usize::from(grown));
    assert_eq!(self::spans(&list)?.len(), spans.len() + 1);
    assert!(holds_allow(&list, sid, mask));
    Ok(Some(list))
}

/// The list without each entry byte-equal to the one [`with_allow`] adds for `sid` and
/// `mask`; every other entry, a wider one for the same SID included, and unused room at the
/// end are kept. Inputs: the list, the SID and the rights. Output: the new list, None when it
/// holds no such entry, or the reason it is malformed.
pub fn without_allow(acl: &[u8], sid: &[u8], mask: u32) -> Result<Option<Vec<u8>>, &'static str> {
    let spans = spans(acl)?;
    let entry = ace_bytes(ACCESS_ALLOWED, 0, mask, sid);
    let ours = |span: &Span<'_>| acl[span.range.clone()] == entry[..];
    let removed = spans.iter().filter(|span| ours(span)).count();
    if removed == 0 {
        return Ok(None);
    }
    let size = list_size(acl);
    let end = spans.last().map_or(ACL_HEADER_BYTES, |span| span.range.end);
    let shrunk = u16::try_from(size - removed * entry.len()).expect("a list only shrinks");
    let count = u16::try_from(spans.len() - removed).expect("a list only shrinks");
    let mut list = Vec::with_capacity(usize::from(shrunk));
    list.extend_from_slice(&acl[..2]);
    list.extend(shrunk.to_le_bytes());
    list.extend(count.to_le_bytes());
    list.extend_from_slice(&acl[6..ACL_HEADER_BYTES]);
    for span in spans.iter().filter(|span| !ours(span)) {
        list.extend_from_slice(&acl[span.range.clone()]);
    }
    list.extend_from_slice(&acl[end..size]);
    assert_eq!(list.len(), usize::from(shrunk));
    assert!(
        self::spans(&list)?
            .iter()
            .all(|span| list[span.range.clone()] != entry[..])
    );
    Ok(Some(list))
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

/// Whether the list lets `sid` read and run a folder and what lies below it: the folder itself
/// ([`reads`]), and its files and its subfolders through allow entries that pass read and run
/// on to every level, inherit-only or generic ones included, as `C:\Program Files` splits them
/// (rv-code4 #541 F1). Inputs: the access list bytes and the SID bytes. Output: true when it
/// does; false for a malformed list.
pub fn reads_tree(acl: &[u8], sid: &[u8]) -> bool {
    let passes = |inherit: u8| {
        entries(acl).is_ok_and(|found| {
            found.iter().any(|entry| {
                entry.allows
                    && entry.sid == sid
                    && entry.flags & inherit == inherit
                    && entry.flags & NO_PROPAGATE == 0
                    && specific(entry.mask) & READ_EXECUTE == READ_EXECUTE
            })
        })
    };
    reads(acl, sid) && passes(OBJECT_INHERIT) && passes(CONTAINER_INHERIT)
}

/// Whether the list lets `sid` read a read root: a folder with what lies below it
/// ([`reads_tree`]), a file itself ([`reads`]) (V244-WIN-ROLE-READS-TREE). Inputs: the access
/// list bytes, the SID bytes and whether the root is a folder. Output: true when it does;
/// false for a malformed list.
pub fn reads_root(acl: &[u8], sid: &[u8], folder: bool) -> bool {
    if folder {
        reads_tree(acl, sid)
    } else {
        reads(acl, sid)
    }
}

/// Whether the boundary refuses a read root before its helper runs: neither ALL APPLICATION
/// PACKAGES nor the container reads the root ([`reads_root`]), so the helper would write the
/// container's entry, and this user cannot change the list, so that write fails. `changeable`
/// is asked only for such a root. Inputs: the list, the container SID, whether the root is a
/// folder and the change probe. Output: true to refuse.
pub fn read_root_refused(
    acl: &[u8],
    container: &[u8],
    folder: bool,
    changeable: impl FnOnce() -> bool,
) -> bool {
    !reads_root(acl, &ALL_APPLICATION_PACKAGES, folder)
        && !reads_root(acl, container, folder)
        && !changeable()
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
/// The SID `S-1-3-0` (CREATOR OWNER) as bytes, which an inherited entry turns into whoever
/// creates the child.
pub const CREATOR_OWNER: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0];

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
/// counted, as they only take rights away. An applying entry of any other type, such as a
/// conditional, object or compound allow entry this reader does not parse, is an error the
/// caller refuses on (#317 r2 F2). Inputs: the access list bytes, the user's SID bytes and
/// the rights. Output: that SID, none when only those may change the object, or the reason
/// the list is malformed or holds an entry the check cannot read.
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
    for span in spans(acl)? {
        let kind = acl[span.range.start];
        if acl[span.range.start + 1] & INHERIT_ONLY != 0 {
            continue;
        }
        match span.entry {
            Some(entry)
                if entry.allows && entry.mask & rights != 0 && !allowed.contains(&entry.sid) =>
            {
                return Ok(Some(entry.sid));
            }
            Some(_) => {}
            None if DENY_TYPES.contains(&kind) => {}
            None => return Err("the access list holds an allow entry this check cannot read"),
        }
    }
    Ok(None)
}

/// The first SID other than SYSTEM, the administrators, TrustedInstaller, CREATOR OWNER,
/// OWNER RIGHTS and the user that a folder's allow entry hands any right on what is made in
/// it (object or container inherit, inherit-only or not); an inheritable allow entry this
/// reader cannot parse is an error. Inputs: the list bytes and the user's SID bytes. Output:
/// that SID, none, or the reason the list is malformed or unreadable.
pub fn inherited_grantee<'a>(acl: &'a [u8], user: &[u8]) -> Result<Option<&'a [u8]>, &'static str> {
    assert!(user.len() >= ACE_HEADER_BYTES);
    let allowed = [
        &SYSTEM[..],
        &ADMINISTRATORS[..],
        &TRUSTED_INSTALLER[..],
        &CREATOR_OWNER[..],
        &OWNER_RIGHTS[..],
        user,
    ];
    for span in spans(acl)? {
        let kind = acl[span.range.start];
        if acl[span.range.start + 1] & (OBJECT_INHERIT | CONTAINER_INHERIT) == 0 {
            continue;
        }
        match span.entry {
            Some(entry) if entry.allows && entry.mask != 0 && !allowed.contains(&entry.sid) => {
                return Ok(Some(entry.sid));
            }
            Some(_) => {}
            None if DENY_TYPES.contains(&kind) => {}
            None => {
                return Err(
                    "the access list holds an inheritable allow entry this check cannot read",
                );
            }
        }
    }
    Ok(None)
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

/// Where an entry of a converged list came from: the container's own explicit entry, an entry
/// still inherited from the folder above, or any other entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Own,
    Inherited,
    Other,
}

/// The access list a path must hold for the container's role on it, built from its current
/// list. A role that cuts turns every inherited entry explicit; one that does not leaves
/// inherited entries as they are, and a root holding guards refuses an inherited allow that
/// gives the container more on the root itself than its role does (see [`reaches`]), since
/// no deny stops it there. A role that narrows others keeps every other package SID's
/// explicit allow within the role's rights, split into a part for the folder itself and a
/// part its files and folders inherit, and gone when nothing is left; it refuses a list
/// holding an entry type other than allow or deny, so no entry the boundary cannot read keeps
/// granting a container. The container's own explicit entries become the role's entries,
/// kept in place when they already are. Inputs: the current list, the container SID, the role
/// and whether the path is a folder. Output: the new list, None when the path already holds
/// it, or the reason the current list is refused.
pub fn converge(
    acl: &[u8],
    sid: &[u8],
    access: Access,
    folder: bool,
) -> Result<Option<Vec<u8>>, &'static str> {
    assert!(package(sid), "a container SID is a package SID");
    assert!(!access.cuts() || access.narrows_others());
    let target = own_entries(sid, access, folder);
    let mut body = parts(acl, sid, access, folder)?;
    let current: Vec<&[u8]> = body
        .iter()
        .filter(|(_, origin)| *origin == Origin::Own)
        .map(|(bytes, _)| bytes.as_slice())
        .collect();
    if current != target.iter().map(Vec::as_slice).collect::<Vec<_>>() {
        body.retain(|(_, origin)| *origin != Origin::Own);
        let at = body
            .iter()
            .position(|(_, origin)| *origin == Origin::Inherited)
            .unwrap_or(body.len());
        body.splice(
            at..at,
            target.iter().map(|bytes| (bytes.clone(), Origin::Own)),
        );
    }
    let aces: Vec<Vec<u8>> = body.into_iter().map(|(bytes, _)| bytes).collect();
    let list = list_from(&acl[..ACL_HEADER_BYTES], &aces)?;
    let found = entries(&list)?;
    assert!(!access.cuts() || found.iter().all(|entry| entry.flags & INHERITED == 0));
    assert_eq!(
        found
            .iter()
            .filter(|entry| entry.sid == sid && entry.flags & INHERITED == 0)
            .count(),
        target.len()
    );
    let size = usize::from(u16::from_le_bytes([acl[2], acl[3]]));
    Ok((list.as_slice() != &acl[..size]).then_some(list))
}

/// The entries of the current list as the role keeps them, before the container's own entries
/// are placed: see [`converge`].
fn parts(
    acl: &[u8],
    sid: &[u8],
    access: Access,
    folder: bool,
) -> Result<Vec<(Vec<u8>, Origin)>, &'static str> {
    let spans = spans(acl)?;
    let (cut, narrow) = (access.cuts(), access.narrows_others());
    let mut body: Vec<(Vec<u8>, Origin)> = Vec::with_capacity(2 * spans.len());
    for span in &spans {
        let mut bytes = acl[span.range.clone()].to_vec();
        let inherited = bytes[1] & INHERITED != 0;
        let Some(entry) = span.entry else {
            if narrow {
                return Err("an access list holds an entry the boundary does not read");
            }
            let origin = if inherited {
                Origin::Inherited
            } else {
                Origin::Other
            };
            body.push((bytes, origin));
            continue;
        };
        if inherited && !cut {
            if narrow && grants_beyond(&entry, sid, access.rights().this) {
                return Err("a root inherits more for containers than its role gives");
            }
            body.push((bytes, Origin::Inherited));
            continue;
        }
        let entry = Entry {
            flags: entry.flags & !INHERITED,
            ..entry
        };
        bytes[1] = entry.flags;
        if entry.sid == sid {
            if !inherited {
                body.push((bytes, Origin::Own));
            }
            continue;
        }
        if narrow && entry.allows && package(entry.sid) {
            let kept = narrowed(&entry, access.rights(), folder);
            body.extend(kept.into_iter().map(|bytes| (bytes, Origin::Other)));
            continue;
        }
        body.push((bytes, Origin::Other));
    }
    assert!(body.len() <= 2 * spans.len());
    assert!(!cut || body.iter().all(|(_, origin)| *origin != Origin::Inherited));
    Ok(body)
}

/// Whether an allow for `other` counts in the container's access check: the container's own
/// SID, ALL APPLICATION PACKAGES or ALL RESTRICTED APPLICATION PACKAGES (`S-1-15-2-1` and
/// `S-1-15-2-2`), or a capability (`S-1-15-3-*`), which the container may hold; another
/// container's SID does not. Inputs: the entry's SID and the container SID. Output: the answer.
fn reaches(other: &[u8], sid: &[u8]) -> bool {
    assert!(package(sid), "a container SID is a package SID");
    assert!(other.len() <= SID_BYTES_MAX);
    if other == sid {
        return true;
    }
    if !package(other) {
        return false;
    }
    let part = |at: usize| {
        other
            .get(at..at + 4)
            .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    match part(8) {
        Some(3) => true,
        _ => other[1] == 2 && matches!(part(12), Some(1 | 2)),
    }
}

/// Whether an inherited allow gives the container, on the object itself, a right outside
/// `this`, the role's rights there. An inherit-only entry gives the object nothing. Inputs:
/// the entry, the container SID and the role's rights on the object. Output: the answer.
fn grants_beyond(entry: &Entry<'_>, sid: &[u8], this: u32) -> bool {
    assert_eq!(this & !crate::plan::MODIFY, 0);
    let beyond = specific(entry.mask) & !this;
    entry.allows && entry.flags & INHERIT_ONLY == 0 && beyond != 0 && reaches(entry.sid, sid)
}

/// The entries the container gets for its role: on a file, the role's rights on the file; on
/// a folder, one entry its files and folders inherit when the rights match, else one for the
/// folder itself and an inherit-only one for what is below. An entry with no rights is left
/// out, so a hidden path holds none. Inputs: the container SID, the role and whether the path
/// is a folder. Output: the entries' bytes in order.
fn own_entries(sid: &[u8], access: Access, folder: bool) -> Vec<Vec<u8>> {
    let Rights { this, below } = access.rights();
    let inherit = OBJECT_INHERIT | CONTAINER_INHERIT;
    let planned: Vec<(u8, u32)> = if !folder {
        vec![(0, this)]
    } else if this == below {
        vec![(inherit, this)]
    } else {
        vec![(0, this), (inherit | INHERIT_ONLY, below)]
    };
    let own: Vec<Vec<u8>> = planned
        .into_iter()
        .filter(|(_, mask)| *mask != 0)
        .map(|(flags, mask)| ace_bytes(ACCESS_ALLOWED, flags, mask, sid))
        .collect();
    assert!(own.len() <= 2);
    assert_eq!(own.is_empty(), this == 0 && (!folder || below == 0));
    own
}

/// Another package's allow entry narrowed to a role's rights: on a file or where the rights
/// match, one entry with its flags and the rights it keeps; on a folder where they differ, a
/// part for the folder itself unless the entry is inherit-only, and an inherit-only part when
/// it is inherited below, each with what it keeps of its side's rights. A part left with no
/// rights goes. Inputs: the entry with its inherited flag cleared, the rights and whether the
/// path is a folder. Output: the parts' bytes in order.
fn narrowed(entry: &Entry<'_>, rights: Rights, folder: bool) -> Vec<Vec<u8>> {
    assert!(entry.allows);
    assert_eq!(entry.flags & INHERITED, 0);
    let mask = specific(entry.mask);
    let inherit = OBJECT_INHERIT | CONTAINER_INHERIT;
    let mut parts: Vec<(u8, u32)> = Vec::with_capacity(2);
    if !folder || rights.this == rights.below {
        parts.push((entry.flags, mask & rights.this));
    } else {
        if entry.flags & INHERIT_ONLY == 0 {
            parts.push((0, mask & rights.this));
        }
        if entry.flags & inherit != 0 {
            let flags = (entry.flags & (inherit | NO_PROPAGATE)) | INHERIT_ONLY;
            parts.push((flags, mask & rights.below));
        }
    }
    parts
        .into_iter()
        .filter(|(_, kept)| *kept != 0)
        .map(|(flags, kept)| ace_bytes(ACCESS_ALLOWED, flags, kept, entry.sid))
        .collect()
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

/// An access list of allow entries giving `mask` to each SID in order, all with `flags`, in
/// the layout Windows reads. Inputs: the SIDs, the entry flags and the rights. Output: the
/// list bytes.
pub fn allowing(sids: &[&[u8]], flags: u8, mask: u32) -> Vec<u8> {
    assert!(!sids.is_empty() && sids.len() <= 16);
    let aces: Vec<Vec<u8>> = sids
        .iter()
        .map(|sid| ace_bytes(ACCESS_ALLOWED, flags, mask, sid))
        .collect();
    let list = list_from(&[ACL_REVISION, 0, 0, 0, 0, 0, 0, 0], &aces).expect("a few SIDs fit");
    assert_eq!(entries(&list).map(|found| found.len()), Ok(sids.len()));
    list
}

/// A system access list holding one mandatory label entry with `flags`: integrity `level`,
/// the label SID's last part, and the `policy` bits, in the layout Windows reads. Inputs:
/// the level, the policy and the entry flags. Output: the list bytes.
pub fn labelled(level: u32, policy: u32, flags: u8) -> Vec<u8> {
    let mut sid = vec![1, 1];
    sid.extend(LABEL_AUTHORITY);
    sid.extend(level.to_le_bytes());
    let ace = ace_bytes(MANDATORY_LABEL, flags, policy, &sid);
    let list = list_from(&[ACL_REVISION, 0, 0, 0, 0, 0, 0, 0], &[ace]).expect("one label fits");
    let applies = flags & INHERIT_ONLY == 0;
    assert_eq!(label(&list), Ok(applies.then_some((level, policy))));
    list
}

/// The mandatory label a system access list holds for the object itself: the integrity
/// level, the label SID's last part, and the policy bits. Inherit-only labels are skipped.
/// Input: the list bytes Windows copied out with the label asked for. Output: the level and
/// policy, None when no label applies, or the reason the list or a label is malformed.
pub fn label(sacl: &[u8]) -> Result<Option<(u32, u32)>, &'static str> {
    only_label(sacl, |flags| flags & INHERIT_ONLY == 0)
}

/// The mandatory label a folder's system access list hands what is made in it, an entry with
/// object or container inherit, inherit-only or not, read as [`label`] reads its own.
/// Input: the list bytes. Output: the level and policy, None when no label is handed down,
/// or the reason the list or a label is malformed.
pub fn inherited_label(sacl: &[u8]) -> Result<Option<(u32, u32)>, &'static str> {
    only_label(sacl, |flags| {
        flags & (OBJECT_INHERIT | CONTAINER_INHERIT) != 0
    })
}

/// The one mandatory label entry whose flags `wanted` accepts, as its level and policy. A
/// second such entry is refused, not skipped, since the reader cannot tell which of the two
/// Windows enforces (#568 rv-code F1). Inputs: the system list bytes and the flag test.
/// Output: the level and policy, None when no such label exists, or the reason the list or a
/// label is malformed or a second one is there.
fn only_label(sacl: &[u8], wanted: fn(u8) -> bool) -> Result<Option<(u32, u32)>, &'static str> {
    let mut found = None;
    for span in spans(sacl)? {
        let at = span.range.start;
        if sacl[at] != MANDATORY_LABEL || !wanted(sacl[at + 1]) {
            continue;
        }
        if found.is_some() {
            return Err("a system list holds a second mandatory label for the same objects");
        }
        let policy = u32::from_le_bytes([sacl[at + 4], sacl[at + 5], sacl[at + 6], sacl[at + 7]]);
        let sid = sid_at(&sacl[at + ACE_HEADER_BYTES..span.range.end])?;
        if sid.len() != 12 || sid[2..8] != LABEL_AUTHORITY {
            return Err("a mandatory label holds a SID that is not an integrity level");
        }
        found = Some((
            u32::from_le_bytes([sid[8], sid[9], sid[10], sid[11]]),
            policy,
        ));
    }
    Ok(found)
}

/// Whether only the trusted accounts may change an object: one of them owns it
/// ([`trusted_owner`]) and its access list gives no other SID any of `rights`
/// ([`other_writer`]). Inputs: the owner, the list (None when it has none), the user's SID
/// and the rights. Output: Ok, or the reason another account can change it.
pub fn held_only_by(
    owner: &[u8],
    list: Option<&[u8]>,
    user: &[u8],
    rights: u32,
) -> Result<(), &'static str> {
    if !trusted_owner(owner, user) {
        return Err("another account owns the program or a folder above it");
    }
    let Some(list) = list else {
        return Err(
            "the program or a folder above it has no access list, so everyone can change it",
        );
    };
    match other_writer(list, user, rights)? {
        None => Ok(()),
        Some(_) => Err("another account can change the program or a folder above it"),
    }
}

/// One part of a path as [`first_changeable_part`] reads it: missing, unreadable, or its
/// owner SID, its access list (None when it has none) and whether it is a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Missing,
    Unreadable,
    Found {
        owner: Vec<u8>,
        list: Option<Vec<u8>>,
        folder: bool,
    },
}

/// The first part, the entry first, that someone other than the trusted accounts can change
/// ([`held_only_by`]): a file entry with [`FILE_CHANGES`], a folder entry, a file's own folder
/// and a folder above a missing part with [`OWN_FOLDER_CHANGES`], any other folder with
/// [`ANCESTOR_CHANGES`]. A missing part is never named, an unreadable one always. Inputs: the
/// parts from the entry up and the user's SID. Output: the part's index, or none.
pub fn first_changeable_part(parts: &[Part], user: &[u8]) -> Option<usize> {
    let folder_entry = matches!(parts.first(), Some(Part::Found { folder: true, .. }));
    let mut below_exists = false;
    for (depth, part) in parts.iter().enumerate() {
        let rights = match depth {
            0 if folder_entry => OWN_FOLDER_CHANGES,
            0 => FILE_CHANGES,
            1 if !folder_entry => OWN_FOLDER_CHANGES,
            _ if !below_exists => OWN_FOLDER_CHANGES,
            _ => ANCESTOR_CHANGES,
        };
        let changeable = match part {
            Part::Missing => false,
            Part::Unreadable => true,
            Part::Found { owner, list, .. } => {
                held_only_by(owner, list.as_deref(), user, rights).is_err()
            }
        };
        if changeable {
            assert!(*part != Part::Missing);
            return Some(depth);
        }
        below_exists = *part != Part::Missing;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ANCHOR, KEEP_ENTRIES, MODIFY};

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
            ace(ACCESS_DENIED, 0x13, 0x000d_0156, &ALL_APPLICATION_PACKAGES),
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
        assert!(converge(&sid_count, &CONTAINER, Access::Modify, true).is_err());
        assert!(!reads(&revision, &CONTAINER));
        assert!(entries(&acl(&[])).unwrap().is_empty());
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

    const EVERYONE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0];

    fn drive_entry() -> Vec<u8> {
        ace(
            ACCESS_ALLOWED,
            0,
            DRIVE_ROOT_RIGHTS,
            &ALL_APPLICATION_PACKAGES,
        )
    }

    fn with_slack(list: &[u8], slack: usize) -> Vec<u8> {
        let mut bytes = list.to_vec();
        bytes.extend(std::iter::repeat_n(0, slack));
        let size = u16::try_from(bytes.len()).unwrap();
        bytes[2..4].copy_from_slice(&size.to_le_bytes());
        bytes
    }

    fn drive_root() -> [Vec<u8>; 4] {
        [
            ace(ACCESS_DENIED, 0, WRITE_DAC, &EVERYONE),
            ace(ACCESS_ALLOWED, 0x03, 0x001f_01ff, &ADMINISTRATORS),
            ace(ACCESS_ALLOWED, 0x03, 0x001f_01ff, &SYSTEM),
            ace(ACCESS_ALLOWED, 0x03, READ_EXECUTE, &USERS),
        ]
    }

    #[test]
    fn with_allow_adds_one_24_byte_entry_after_the_explicit_entries_and_keeps_every_other_byte() {
        let entry = drive_entry();
        assert_eq!(entry.len(), 24);
        assert_eq!(entry[..4], [0, 0, 24, 0]);
        let root = drive_root();
        let added = with_allow(&acl(&root), &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS)
            .unwrap()
            .unwrap();
        let mut expected = root.to_vec();
        expected.push(entry.clone());
        assert_eq!(added, acl(&expected));
        let inherited = ace(ACCESS_ALLOWED, 0x13, READ_EXECUTE, &USERS);
        let mut mixed = root.to_vec();
        mixed.push(inherited.clone());
        let added = with_allow(&acl(&mixed), &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS)
            .unwrap()
            .unwrap();
        let mut expected = root.to_vec();
        expected.extend([entry.clone(), inherited]);
        assert_eq!(
            added,
            acl(&expected),
            "it goes before the inherited entries"
        );
        let added = with_allow(&acl(&[]), &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS)
            .unwrap()
            .unwrap();
        assert_eq!(added, acl(std::slice::from_ref(&entry)));
        let roomy = with_slack(&acl(&root), 12);
        let added = with_allow(&roomy, &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS)
            .unwrap()
            .unwrap();
        let mut expected = root.to_vec();
        expected.push(entry);
        assert_eq!(
            added,
            with_slack(&acl(&expected), 12),
            "unused room at the end is kept"
        );
        let mut revision = acl(&root);
        revision[0] = 4;
        let added = with_allow(&revision, &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS)
            .unwrap()
            .unwrap();
        assert_eq!(added[0], 4, "the list's own revision is kept");
    }

    #[test]
    fn holds_allow_counts_a_plain_allow_on_the_object_itself_that_covers_the_rights() {
        let root = drive_root();
        let holding = |extra: Vec<u8>| {
            let mut list = root.to_vec();
            list.push(extra);
            acl(&list)
        };
        let package = &ALL_APPLICATION_PACKAGES;
        for (extra, holds, why) in [
            (drive_entry(), true, "the exact entry"),
            (
                ace(ACCESS_ALLOWED, 0x03, READ_EXECUTE, package),
                true,
                "a wider inheritable one",
            ),
            (
                ace(ACCESS_ALLOWED, 0x10, DRIVE_ROOT_RIGHTS, package),
                true,
                "an inherited one on the object itself",
            ),
            (
                ace(ACCESS_ALLOWED, 0x0b, READ_EXECUTE, package),
                false,
                "an inherit-only one",
            ),
            (
                ace(9, 0, DRIVE_ROOT_RIGHTS, package),
                false,
                "a callback allow",
            ),
            (
                ace(ACCESS_DENIED, 0, DRIVE_ROOT_RIGHTS, package),
                false,
                "a deny",
            ),
            (
                ace(ACCESS_ALLOWED, 0, 0x0000_0080, package),
                false,
                "read attributes without synchronize",
            ),
            (
                ace(ACCESS_ALLOWED, 0, DRIVE_ROOT_RIGHTS, &CONTAINER),
                false,
                "another SID",
            ),
        ] {
            let list = holding(extra);
            assert_eq!(
                holds_allow(&list, package, DRIVE_ROOT_RIGHTS),
                holds,
                "{why}"
            );
            assert_eq!(
                with_allow(&list, package, DRIVE_ROOT_RIGHTS)
                    .unwrap()
                    .is_none(),
                holds,
                "{why}"
            );
        }
        assert!(!holds_allow(&acl(&root)[..7], package, DRIVE_ROOT_RIGHTS));
        assert!(with_allow(&acl(&root)[..7], package, DRIVE_ROOT_RIGHTS).is_err());
    }

    #[test]
    fn without_allow_takes_out_only_the_entries_byte_equal_to_the_added_one() {
        let package = &ALL_APPLICATION_PACKAGES;
        let kept = [
            ace(ACCESS_ALLOWED, 0x03, READ_EXECUTE, package),
            ace(ACCESS_ALLOWED, 0x08, DRIVE_ROOT_RIGHTS, package),
            ace(ACCESS_ALLOWED, 0, DRIVE_ROOT_RIGHTS, &CONTAINER),
            ace(ACCESS_ALLOWED, 0x10, DRIVE_ROOT_RIGHTS, package),
        ];
        let mut list = drive_root().to_vec();
        list.insert(1, drive_entry());
        list.extend(kept.iter().cloned());
        list.push(drive_entry());
        let mut expected = drive_root().to_vec();
        expected.extend(kept.iter().cloned());
        assert_eq!(
            without_allow(&acl(&list), package, DRIVE_ROOT_RIGHTS).unwrap(),
            Some(acl(&expected))
        );
        assert_eq!(
            without_allow(&acl(&expected), package, DRIVE_ROOT_RIGHTS).unwrap(),
            None
        );
        assert_eq!(
            without_allow(&with_slack(&acl(&list), 8), package, DRIVE_ROOT_RIGHTS).unwrap(),
            Some(with_slack(&acl(&expected), 8)),
            "unused room at the end is kept"
        );
        assert!(without_allow(&acl(&list)[..7], package, DRIVE_ROOT_RIGHTS).is_err());
    }

    #[test]
    fn with_allow_then_without_allow_gives_back_the_original_bytes() {
        let package = &ALL_APPLICATION_PACKAGES;
        let mut inherited = drive_root().to_vec();
        inherited.push(ace(ACCESS_ALLOWED, 0x13, READ_EXECUTE, &USERS));
        for original in [
            acl(&drive_root()),
            acl(&inherited),
            acl(&[]),
            with_slack(&acl(&drive_root()), 20),
        ] {
            let added = with_allow(&original, package, DRIVE_ROOT_RIGHTS)
                .unwrap()
                .unwrap();
            assert_eq!(added.len(), original.len() + 24);
            assert_eq!(
                without_allow(&added, package, DRIVE_ROOT_RIGHTS).unwrap(),
                Some(original)
            );
        }
    }

    #[test]
    fn with_allow_refuses_a_list_that_would_pass_the_size_limit() {
        let mut long_sid = vec![1, 15, 0, 0, 0, 0, 0, 5];
        long_sid.extend([7; 60]);
        assert_eq!(long_sid.len(), SID_BYTES_MAX);
        let big: Vec<Vec<u8>> = (0..862)
            .map(|_| ace(ACCESS_ALLOWED, 0, 1, &long_sid))
            .collect();
        let list = acl(&big);
        assert_eq!(list.len(), 65_520);
        assert!(with_allow(&list, &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS).is_err());
        let fits = acl(&big[..861]);
        assert!(
            with_allow(&fits, &ALL_APPLICATION_PACKAGES, DRIVE_ROOT_RIGHTS)
                .unwrap()
                .is_some()
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

    /// rv-code4 #541 F1: a folder is read with what lies below it only when entries for the
    /// SID also pass read and run on to files and subfolders at every level, split or whole.
    #[test]
    fn reads_tree_needs_read_and_run_passed_on_to_files_and_subfolders() {
        const AAP: [u8; 16] = ALL_APPLICATION_PACKAGES;
        let program_files = acl(&[
            ace(ACCESS_ALLOWED, 0x0b, 0xa000_0000, &AAP),
            ace(ACCESS_ALLOWED, 0x10, READ_EXECUTE, &AAP),
        ]);
        assert!(reads_tree(&program_files, &AAP));
        let inherited = acl(&[ace(ACCESS_ALLOWED, 0x13, READ_EXECUTE, &CONTAINER)]);
        assert!(reads_tree(&inherited, &CONTAINER));
        let halves = acl(&[
            ace(ACCESS_ALLOWED, 0x00, READ_EXECUTE, &AAP),
            ace(ACCESS_ALLOWED, 0x09, READ_EXECUTE, &AAP),
            ace(ACCESS_ALLOWED, 0x0a, 0x1000_0000, &AAP),
        ]);
        assert!(reads_tree(&halves, &AAP));
        let folder_only = acl(&[ace(ACCESS_ALLOWED, 0x00, READ_EXECUTE, &AAP)]);
        assert!(reads(&folder_only, &AAP) && !reads_tree(&folder_only, &AAP));
        for flags in [0x01, 0x02, 0x07] {
            let short = acl(&[ace(ACCESS_ALLOWED, flags, READ_EXECUTE, &AAP)]);
            assert!(!reads_tree(&short, &AAP), "{flags:#04x}");
        }
        let other = acl(&[
            ace(ACCESS_ALLOWED, 0x00, READ_EXECUTE, &AAP),
            ace(ACCESS_ALLOWED, 0x0b, READ_EXECUTE, &CONTAINER),
        ]);
        assert!(!reads_tree(&other, &AAP));
        let write_only = acl(&[
            ace(ACCESS_ALLOWED, 0x00, READ_EXECUTE, &AAP),
            ace(ACCESS_ALLOWED, 0x0b, 0x4000_0000, &AAP),
        ]);
        assert!(!reads_tree(&write_only, &AAP));
        assert!(!reads_tree(&program_files[..7], &AAP));
    }

    /// A read root is refused before the helper runs only when no app container may read it
    /// and this user cannot change its list, and the change probe runs only for such a root.
    #[test]
    fn a_read_root_is_refused_only_when_no_package_reads_it_and_this_user_cannot_change_it() {
        let packages = acl(&[ace(
            ACCESS_ALLOWED,
            0x10,
            READ_EXECUTE,
            &ALL_APPLICATION_PACKAGES,
        )]);
        let unprobed = || -> bool { panic!("a root every package reads was probed") };
        assert!(!read_root_refused(&packages, &CONTAINER, false, unprobed));
        let admins_only = acl(&[
            ace(ACCESS_ALLOWED, 0x03, 0x001f_01ff, &ADMINISTRATORS),
            ace(ACCESS_ALLOWED, 0x03, 0x001f_01ff, &SYSTEM),
        ]);
        let inherit_only = acl(&[ace(
            ACCESS_ALLOWED,
            0x0b,
            READ_EXECUTE,
            &ALL_APPLICATION_PACKAGES,
        )]);
        let mut malformed = packages.clone();
        malformed[16] = 2;
        for list in [&admins_only, &inherit_only, &malformed] {
            for folder in [false, true] {
                assert!(read_root_refused(list, &CONTAINER, folder, || false));
                assert!(!read_root_refused(list, &CONTAINER, folder, || true));
            }
        }
    }

    /// V244-WIN-NPM r2 F1: a read root the container's own entry already reads is left to the
    /// helper, which names the root if it still has to write there, so the pre-check neither
    /// probes nor refuses it; another container's entry does not count.
    #[test]
    fn a_read_root_the_container_already_reads_is_left_to_the_helper() {
        let own = acl(&[
            ace(ACCESS_ALLOWED, 0x03, 0x001f_01ff, &ADMINISTRATORS),
            ace(ACCESS_ALLOWED, 0x00, READ_EXECUTE, &CONTAINER),
        ]);
        let unprobed = || -> bool { panic!("a root the container reads was probed") };
        assert!(!read_root_refused(&own, &CONTAINER, false, unprobed));
        assert!(read_root_refused(&own, &TEAMMATE, false, || false));
    }

    /// V244-WIN-ROLE-READS-TREE: a read root folder counts as read only through entries that
    /// pass read and run on to its files and subfolders, so one every package or the container
    /// reads for that folder only is refused when this user cannot change it, while the same
    /// list on a file, the `C:\Program Files` split and a whole inheritable grant are not.
    #[test]
    fn a_read_root_folder_read_only_for_itself_is_refused_when_this_user_cannot_change_it() {
        const AAP: [u8; 16] = ALL_APPLICATION_PACKAGES;
        let unprobed = || -> bool { panic!("a root that is read was probed") };
        for sid in [&AAP[..], &CONTAINER[..]] {
            let folder_only = acl(&[ace(ACCESS_ALLOWED, 0x00, READ_EXECUTE, sid)]);
            assert!(reads_root(&folder_only, sid, false));
            assert!(!reads_root(&folder_only, sid, true));
            assert!(!read_root_refused(
                &folder_only,
                &CONTAINER,
                false,
                unprobed
            ));
            assert!(read_root_refused(&folder_only, &CONTAINER, true, || false));
            assert!(!read_root_refused(&folder_only, &CONTAINER, true, || true));
        }
        let program_files = acl(&[
            ace(ACCESS_ALLOWED, 0x0b, 0xa000_0000, &AAP),
            ace(ACCESS_ALLOWED, 0x10, READ_EXECUTE, &AAP),
        ]);
        let granted = acl(&[ace(ACCESS_ALLOWED, 0x03, READ_EXECUTE, &CONTAINER)]);
        for list in [&program_files, &granted] {
            assert!(!read_root_refused(list, &CONTAINER, true, unprobed));
        }
        assert!(read_root_refused(&granted, &TEAMMATE, true, || false));
    }

    const TEAMMATE: [u8; 20] = [1, 3, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 8, 0, 0, 0, 4, 0, 0, 0];
    const INTERNET_CLIENT: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 3, 0, 0, 0, 1, 0, 0, 0];
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
        let cut = converge(
            &inherited_project_list(),
            &CONTAINER,
            Access::ReadOnly,
            true,
        )
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
            converge(&cut, &CONTAINER, Access::ReadOnly, true),
            Ok(None),
            "a path already at its target is left alone"
        );
        let teammate = converge(&cut, &TEAMMATE, Access::ReadOnly, true).unwrap();
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
            converge(&file, &CONTAINER, Access::ReadOnly, false)
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
            converge(&list, &CONTAINER, Access::Hidden, true)
                .unwrap()
                .unwrap(),
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
            converge(&list, &CONTAINER, Access::ReadOnly, true)
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
        assert!(converge(&unknown, &CONTAINER, Access::ReadOnly, true).is_err());
        let good = inherited_project_list();
        assert!(converge(&good[..7], &CONTAINER, Access::ReadOnly, true).is_err());
        let mut slack = good.clone();
        slack.extend([0; 8]);
        let size = u16::try_from(slack.len()).unwrap();
        slack[2..4].copy_from_slice(&size.to_le_bytes());
        assert_eq!(
            converge(&slack, &CONTAINER, Access::ReadOnly, true),
            converge(&good, &CONTAINER, Access::ReadOnly, true),
            "unused room at the end of a list does not change the target"
        );
    }

    #[test]
    fn an_anchored_folder_splits_every_package_into_itself_and_below() {
        let anchored = converge(&inherited_project_list(), &CONTAINER, Access::Anchor, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            anchored,
            acl(&[
                ace(ACCESS_ALLOWED, OICI, FULL, &SYSTEM),
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                ace(ACCESS_ALLOWED, 0, ANCHOR, &TEAMMATE),
                ace(ACCESS_ALLOWED, OICI | INHERIT_ONLY, MODIFY, &TEAMMATE),
                ace(
                    ACCESS_ALLOWED,
                    OICI | INHERIT_ONLY,
                    READ_EXECUTE,
                    &ALL_APPLICATION_PACKAGES,
                ),
                ace(ACCESS_ALLOWED, 0, ANCHOR, &CONTAINER),
                ace(ACCESS_ALLOWED, OICI | INHERIT_ONLY, MODIFY, &CONTAINER),
            ]),
            "the folder itself keeps no delete for any package, while work below goes on"
        );
        assert_eq!(
            converge(&anchored, &CONTAINER, Access::Anchor, true),
            Ok(None)
        );
        assert_eq!(
            converge(&anchored, &TEAMMATE, Access::Anchor, true),
            Ok(None)
        );
        let kept = converge(&anchored, &CONTAINER, Access::KeepEntries, true)
            .unwrap()
            .unwrap();
        let found = entries(&kept).unwrap();
        assert!(
            found
                .iter()
                .filter(|entry| entry.flags & INHERIT_ONLY == 0 && package(entry.sid))
                .all(|entry| entry.mask & !KEEP_ENTRIES == 0),
            "a kept folder lets no package add an entry to it"
        );
        assert_eq!(
            converge(&kept, &CONTAINER, Access::KeepEntries, true),
            Ok(None)
        );
    }

    #[test]
    fn a_root_holding_guarded_paths_refuses_an_inherited_grant_beyond_its_role() {
        let refused = "a root inherits more for containers than its role gives";
        let restricted: [u8; 16] = [1, 2, 0, 0, 0, 0, 0, 15, 2, 0, 0, 0, 2, 0, 0, 0];
        let reaching: [&[u8]; 4] = [
            &ALL_APPLICATION_PACKAGES,
            &restricted,
            &INTERNET_CLIENT,
            &CONTAINER,
        ];
        for access in [Access::RootAnchor, Access::RootKeep] {
            for sid in reaching {
                let list = acl(&[
                    ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                    ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, sid),
                ]);
                assert_eq!(
                    converge(&list, &CONTAINER, access, true),
                    Err(refused),
                    "{access:?} {sid:?}"
                );
            }
            let kept = acl(&[
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &TEAMMATE),
                ace(
                    ACCESS_ALLOWED,
                    OICI | INHERITED,
                    GENERIC_READ_EXECUTE,
                    &ALL_APPLICATION_PACKAGES,
                ),
                ace(
                    ACCESS_ALLOWED,
                    OICI | INHERIT_ONLY | INHERITED,
                    FULL,
                    &ALL_APPLICATION_PACKAGES,
                ),
            ]);
            assert!(
                converge(&kept, &CONTAINER, access, true).is_ok(),
                "a teammate's grant, a read and an inherit-only grant give this root nothing \
                 more: {access:?}"
            );
        }
        let plain = acl(&[ace(
            ACCESS_ALLOWED,
            OICI | INHERITED,
            FULL,
            &ALL_APPLICATION_PACKAGES,
        )]);
        assert!(converge(&plain, &CONTAINER, Access::Modify, true).is_ok());
        let edges = [
            (Access::RootAnchor, MODIFY, false),
            (Access::RootAnchor, GENERIC_ALL, false),
            (Access::RootAnchor, ANCHOR, true),
            (Access::RootKeep, ANCHOR, false),
            (Access::RootKeep, KEEP_ENTRIES, true),
        ];
        for (access, mask, kept) in edges {
            let list = acl(&[ace(
                ACCESS_ALLOWED,
                OICI | INHERITED,
                mask,
                &ALL_APPLICATION_PACKAGES,
            )]);
            let converged = converge(&list, &CONTAINER, access, true);
            assert_eq!(converged.is_ok(), kept, "{access:?} {mask:#x}");
            assert!(kept || converged == Err(refused), "{access:?} {mask:#x}");
        }
    }

    #[test]
    fn a_root_holding_guarded_paths_narrows_explicit_entries_and_keeps_inherited_ones() {
        let list = acl(&[
            ace(ACCESS_DENIED, 0, KEEP_ENTRIES, &CONTAINER),
            ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
            ace(ACCESS_ALLOWED, OICI, MODIFY, &CONTAINER),
            ace(ACCESS_ALLOWED, OICI | NO_PROPAGATE, MODIFY, &TEAMMATE),
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
            ace(
                ACCESS_ALLOWED,
                OICI | INHERIT_ONLY | INHERITED,
                GENERIC_READ_EXECUTE,
                &ALL_APPLICATION_PACKAGES,
            ),
        ]);
        let anchored = converge(&list, &CONTAINER, Access::RootAnchor, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            anchored,
            acl(&[
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                ace(ACCESS_ALLOWED, 0, ANCHOR, &TEAMMATE),
                ace(
                    ACCESS_ALLOWED,
                    OICI | NO_PROPAGATE | INHERIT_ONLY,
                    MODIFY,
                    &TEAMMATE
                ),
                ace(ACCESS_ALLOWED, 0, ANCHOR, &CONTAINER),
                ace(ACCESS_ALLOWED, OICI | INHERIT_ONLY, MODIFY, &CONTAINER),
                ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
                ace(
                    ACCESS_ALLOWED,
                    OICI | INHERIT_ONLY | INHERITED,
                    GENERIC_READ_EXECUTE,
                    &ALL_APPLICATION_PACKAGES,
                ),
            ]),
            "an older deny of the container goes, its entries go before the inherited ones, and \
             the inherited ones stay as Windows wrote them"
        );
        assert_eq!(
            converge(&anchored, &CONTAINER, Access::RootAnchor, true),
            Ok(None)
        );
        let object_ace = {
            let mut bytes = vec![5, 0];
            bytes.extend(16u16.to_le_bytes());
            bytes.extend([0; 12]);
            bytes
        };
        let unknown = acl(&[object_ace, ace(ACCESS_ALLOWED, OICI, FULL, &OWNER)]);
        assert!(converge(&unknown, &CONTAINER, Access::RootKeep, true).is_err());
        assert!(converge(&unknown, &CONTAINER, Access::Modify, true).is_ok());
    }

    #[test]
    fn a_granted_root_changes_only_the_containers_own_entry() {
        let object_ace = {
            let mut bytes = vec![5, 0];
            bytes.extend(16u16.to_le_bytes());
            bytes.extend([0; 12]);
            bytes
        };
        let list = acl(&[
            ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
            ace(ACCESS_ALLOWED, OICI, READ_EXECUTE, &CONTAINER),
            object_ace.clone(),
            ace(ACCESS_ALLOWED, OICI, MODIFY, &TEAMMATE),
            ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
            ace(ACCESS_ALLOWED, OICI | INHERITED, MODIFY, &CONTAINER),
        ]);
        let granted = converge(&list, &CONTAINER, Access::Modify, true)
            .unwrap()
            .unwrap();
        assert_eq!(
            granted,
            acl(&[
                ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
                object_ace,
                ace(ACCESS_ALLOWED, OICI, MODIFY, &TEAMMATE),
                ace(ACCESS_ALLOWED, OICI, MODIFY, &CONTAINER),
                ace(ACCESS_ALLOWED, OICI | INHERITED, FULL, &SYSTEM),
                ace(ACCESS_ALLOWED, OICI | INHERITED, MODIFY, &CONTAINER),
            ]),
            "a plain grant narrows no teammate and leaves what it cannot read"
        );
        assert_eq!(
            converge(&granted, &CONTAINER, Access::Modify, true),
            Ok(None)
        );
        let first = acl(&[
            ace(ACCESS_ALLOWED, OICI, MODIFY, &CONTAINER),
            ace(ACCESS_ALLOWED, OICI, FULL, &OWNER),
        ]);
        assert_eq!(
            converge(&first, &CONTAINER, Access::Modify, true),
            Ok(None),
            "an own entry already in place stays where it is"
        );
        let file = acl(&[ace(ACCESS_ALLOWED, 0, FULL, &OWNER)]);
        assert_eq!(
            converge(&file, &CONTAINER, Access::ReadExecute, false)
                .unwrap()
                .unwrap(),
            acl(&[
                ace(ACCESS_ALLOWED, 0, FULL, &OWNER),
                ace(ACCESS_ALLOWED, 0, READ_EXECUTE, &CONTAINER),
            ])
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

    /// V243-WIN-CHANGEABLE-BY-OTHERS: a file's own folder is judged as `owner_only` judges
    /// it, a folder entry's parent only for swapping it, a folder above a missing part for
    /// adding one; a missing part is never named, and an unreadable part, another owner or
    /// no access list always is.
    #[test]
    fn the_first_changeable_part_reads_each_depth_with_its_own_rights() {
        let own = acl(&[ace(ACCESS_ALLOWED, INHERITED, FULL, &OWNER)]);
        let adds = acl(&[
            ace(ACCESS_ALLOWED, INHERITED, FULL, &OWNER),
            ace(ACCESS_ALLOWED, 0, ADD_SUBDIRECTORY, &AUTHENTICATED_USERS),
        ]);
        let found = |list: &[u8], folder: bool| Part::Found {
            owner: OWNER.to_vec(),
            list: Some(list.to_vec()),
            folder,
        };
        let (file, folder, open) = (found(&own, false), found(&own, true), found(&adds, true));
        let first = |parts: &[&Part]| {
            let parts: Vec<Part> = parts.iter().map(|part| (*part).clone()).collect();
            first_changeable_part(&parts, &OWNER)
        };
        assert_eq!(first(&[&file, &folder, &folder]), None);
        assert_eq!(first(&[&file, &open, &folder]), Some(1));
        assert_eq!(first(&[&file, &folder, &open]), None);
        assert_eq!(first(&[&folder, &open]), None);
        assert_eq!(first(&[&open, &folder]), Some(0));
        assert_eq!(first(&[&Part::Missing, &Part::Missing, &open]), Some(2));
        assert_eq!(first(&[&Part::Missing, &folder, &open]), None);
        assert_eq!(first(&[&file, &Part::Unreadable]), Some(1));
        let foreign = Part::Found {
            owner: OTHER_ACCOUNT.to_vec(),
            list: Some(own.clone()),
            folder: true,
        };
        assert_eq!(first(&[&file, &folder, &foreign]), Some(2));
        let bare = Part::Found {
            owner: OWNER.to_vec(),
            list: None,
            folder: true,
        };
        assert_eq!(first(&[&file, &folder, &bare]), Some(2));
        assert_eq!(first(&[]), None);
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

    /// #317 r2 F2: an allow entry of a type the reader does not parse, here a conditional
    /// entry giving another account full control, refuses instead of passing, while an
    /// inherit-only one and the deny kinds still name no one.
    #[test]
    fn an_allow_entry_the_check_cannot_read_refuses_and_deny_kinds_do_not() {
        const ALLOWED_CALLBACK: u8 = 9;
        for kind in [ALLOWED_CALLBACK, 4, 5, 11] {
            let list = acl(&[ace(kind, 0, FULL, &OTHER_ACCOUNT)]);
            assert!(other_writer(&list, &OWNER, FILE_CHANGES).is_err(), "{kind}");
            let later = acl(&[ace(
                kind,
                INHERIT_ONLY | CONTAINER_INHERIT,
                FULL,
                &OTHER_ACCOUNT,
            )]);
            assert_eq!(
                other_writer(&later, &OWNER, FILE_CHANGES),
                Ok(None),
                "{kind}"
            );
        }
        for kind in DENY_TYPES {
            let list = acl(&[ace(kind, 0, FULL, &OTHER_ACCOUNT)]);
            assert_eq!(
                other_writer(&list, &OWNER, FILE_CHANGES),
                Ok(None),
                "{kind}"
            );
        }
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
