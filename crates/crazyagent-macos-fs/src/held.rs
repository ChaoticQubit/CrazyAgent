use super::{
    AccessEntry, ENTRY_REPLY_BYTES, FILESEC_BYTES_MAX, VOLUME_REPLY_BYTES, device_at, eight_at,
    filesec_entries, malformed, root_pair, u32_at, volume_keeps_lists,
};
use std::ffi::CStr;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// The `vtype` of a folder in an `ATTR_CMN_OBJTYPE` reply.
const KIND_FOLDER: u32 = 2;

/// The `vtype` of a symbolic link in an `ATTR_CMN_OBJTYPE` reply.
const KIND_LINK: u32 = 5;

/// The common attributes every held entry reply must return: the device, the object type,
/// the change time, the owner, the group, the mode and the file id.
pub(super) const HELD_REQUIRED: libc::attrgroup_t = libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_CHGTIME
    | libc::ATTR_CMN_OWNERID
    | libc::ATTR_CMN_GRPID
    | libc::ATTR_CMN_ACCESSMASK
    | libc::ATTR_CMN_FILEID;

/// Where the filesec reference sits in a held entry reply; its offset counts from here.
const HELD_REPLY_SECURITY_AT: usize = 60;

/// Where the fixed attributes of a held entry reply end and the `kauth_filesec` may start:
/// the length, the returned set, the device, the object type, the change time, the owner,
/// the group, the mode, the filesec reference and the file id, each on a 4-byte boundary.
pub(super) const HELD_REPLY_FIXED: usize = 76;

const _: () = assert!(HELD_REPLY_FIXED == HELD_REPLY_SECURITY_AT + 8 + 8);
const _: () = assert!(HELD_REPLY_FIXED + FILESEC_BYTES_MAX <= ENTRY_REPLY_BYTES);

/// One entry as a single `getattrlist` call read it: its identity (device, file id, change
/// time), its type, owner, group and mode, and its extended access list, all from the same
/// object at the same moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldEntry {
    pub device: u64,
    pub inode: u64,
    pub changed: (i64, i64),
    pub kind: u32,
    pub owner: u32,
    pub group: u32,
    pub mode: u32,
    pub entries: Vec<AccessEntry>,
}

impl HeldEntry {
    /// Whether the entry is a folder. Output: the answer.
    pub fn folder(&self) -> bool {
        self.kind == KIND_FOLDER
    }

    /// Whether the entry is a symbolic link itself. Output: the answer.
    pub fn link(&self) -> bool {
        self.kind == KIND_LINK
    }
}

/// Opens one folder for search only (`O_SEARCH`), named by one part relative to an open
/// folder, or `/` and other absolute names with no folder. With `follow` false a link at the
/// name is refused (`O_NOFOLLOW`, `ELOOP`) and never walked. Search on the folder is enough;
/// no leave to read it is needed. Safety contract of its unsafe calls: `parent` is borrowed,
/// so it stays open for the call; `name` is a `CStr`, so it ends in NUL; a descriptor the
/// call returns is owned by nothing else. Inputs: the folder, the name, whether to follow a
/// link. Output: the handle, or the system error.
#[allow(unsafe_code)]
pub fn open_search(
    parent: Option<BorrowedFd<'_>>,
    name: &CStr,
    follow: bool,
) -> std::io::Result<OwnedFd> {
    let flags = libc::O_SEARCH | libc::O_CLOEXEC | if follow { 0 } else { libc::O_NOFOLLOW };
    let folder = parent.map_or(libc::AT_FDCWD, |parent| parent.as_raw_fd());
    let fd = unsafe { libc::openat(folder, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    assert!(fd >= 0);
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The attribute list of a held entry read: the returned set, [`HELD_REQUIRED`] and the
/// extended access list. Output: the list.
fn held_list() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | HELD_REQUIRED
            | libc::ATTR_CMN_EXTENDED_SECURITY,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

/// Reads an open entry with one `fgetattrlist`, so no name is looked up again ([`HeldEntry`]).
/// Safety contract of its unsafe call: `entry` is borrowed, so it stays open for the call;
/// the attribute list and reply buffer are locals that outlive the call, whose length is
/// passed as the buffer's own. Input: the entry's handle. Output: the read, or an error for a
/// call that failed or a reply cut short or out of shape.
#[allow(unsafe_code)]
pub fn held_entry(entry: BorrowedFd<'_>) -> std::io::Result<HeldEntry> {
    let mut list = held_list();
    let mut reply = [0u8; ENTRY_REPLY_BYTES];
    let done = unsafe {
        libc::fgetattrlist(
            entry.as_raw_fd(),
            (&raw mut list).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            libc::FSOPT_PACK_INVAL_ATTRS,
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error());
    }
    held_reply(&reply)
}

/// Reads one entry named by one part relative to an open folder, or by an absolute name with
/// no folder, with one `getattrlistat` that follows no link, so a link there is read itself
/// ([`HeldEntry`]). Safety contract of its unsafe call: `parent` is borrowed, so it stays open
/// for the call; `name` is a `CStr`; the attribute list and reply buffer are locals that
/// outlive the call, whose length is passed as the buffer's own. Inputs: the folder or none,
/// and the name. Output: the read, or an error for a call that failed or a reply out of shape.
#[allow(unsafe_code)]
pub fn held_entry_at(parent: Option<BorrowedFd<'_>>, name: &CStr) -> std::io::Result<HeldEntry> {
    assert!(parent.is_some() || name.to_bytes().first() == Some(&b'/'));
    let mut list = held_list();
    let mut reply = [0u8; ENTRY_REPLY_BYTES];
    let done = unsafe {
        libc::getattrlistat(
            parent.map_or(libc::AT_FDCWD, |parent| parent.as_raw_fd()),
            name.as_ptr(),
            (&raw mut list).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            libc::c_ulong::from(libc::FSOPT_NOFOLLOW | libc::FSOPT_PACK_INVAL_ATTRS),
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error());
    }
    held_reply(&reply)
}

/// The volume attribute list of a held volume read: the device and the capabilities.
/// Output: the list.
fn volume_list() -> libc::attrlist {
    libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_DEVID,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

/// Whether the volume an open entry sits on keeps extended access lists and names the
/// entry's device ([`volume_keeps_lists`]), read with one `fgetattrlist`. Safety contract of
/// its unsafe call: as [`held_entry`]. Inputs: the handle and the device the entry read.
/// Output: the answer, or the error of a call that failed.
#[allow(unsafe_code)]
pub fn held_volume_keeps_lists(entry: BorrowedFd<'_>, device: u64) -> std::io::Result<bool> {
    let mut list = volume_list();
    let mut reply = [0u8; VOLUME_REPLY_BYTES];
    let done = unsafe {
        libc::fgetattrlist(
            entry.as_raw_fd(),
            (&raw mut list).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            0,
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(volume_keeps_lists(&reply, device, root_pair()))
}

/// [`held_volume_keeps_lists`] for one entry named by one part relative to an open folder, or
/// by an absolute name with no folder, read with one `getattrlistat` that follows no link.
/// Safety contract of its unsafe call: as [`held_entry_at`]. Inputs: the folder or none, the
/// name and the device the entry read. Output: the answer, or the error of a call that failed.
#[allow(unsafe_code)]
pub fn held_volume_keeps_lists_at(
    parent: Option<BorrowedFd<'_>>,
    name: &CStr,
    device: u64,
) -> std::io::Result<bool> {
    assert!(parent.is_some() || name.to_bytes().first() == Some(&b'/'));
    let mut list = volume_list();
    let mut reply = [0u8; VOLUME_REPLY_BYTES];
    let done = unsafe {
        libc::getattrlistat(
            parent.map_or(libc::AT_FDCWD, |parent| parent.as_raw_fd()),
            name.as_ptr(),
            (&raw mut list).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            libc::c_ulong::from(libc::FSOPT_NOFOLLOW),
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(volume_keeps_lists(&reply, device, root_pair()))
}

/// Reads a held entry reply packed with `FSOPT_PACK_INVAL_ATTRS`: the length the kernel
/// needed, which must fit the reply and hold the fixed part, the returned set, which must
/// hold every [`HELD_REQUIRED`] attribute, then those in bit order and the filesec reference,
/// whose bytes must lie inside the length. A filesec that is not returned is an empty list.
/// Input: the reply. Output: the read, or an error for a reply cut short or out of shape.
pub(super) fn held_reply(reply: &[u8]) -> std::io::Result<HeldEntry> {
    let length = u32_at(reply, 0).ok_or_else(|| malformed("no length"))? as usize;
    if length < HELD_REPLY_FIXED || length > reply.len() {
        return Err(malformed("the reply is cut short"));
    }
    let reply = &reply[..length];
    let returned = u32_at(reply, 4).ok_or_else(|| malformed("no returned set"))?;
    if returned & HELD_REQUIRED != HELD_REQUIRED {
        return Err(malformed("the identity was not returned"));
    }
    let word = |at: usize, what: &str| u32_at(reply, at).ok_or_else(|| malformed(what));
    let device = device_at(reply, 24).ok_or_else(|| malformed("no device"))?;
    let kind = word(28, "no object type")?;
    let seconds = eight_at(reply, 32).ok_or_else(|| malformed("no change time"))?;
    let nanos = eight_at(reply, 40).ok_or_else(|| malformed("no change time"))?;
    let owner = word(48, "no owner")?;
    let group = word(52, "no group")?;
    let mode = word(56, "no mode")?;
    let inode = eight_at(reply, 68).ok_or_else(|| malformed("no file id"))?;
    let mut entries = Vec::new();
    if returned & libc::ATTR_CMN_EXTENDED_SECURITY != 0 {
        let past = || malformed("a reference past the reply");
        let offset = u32_at(reply, HELD_REPLY_SECURITY_AT).ok_or_else(past)?;
        let size = u32_at(reply, HELD_REPLY_SECURITY_AT + 4).ok_or_else(past)? as usize;
        if size > 0 {
            let offset = usize::try_from(i32::from_ne_bytes(offset.to_ne_bytes()))
                .map_err(|_| malformed("a negative reference"))?;
            let start = HELD_REPLY_SECURITY_AT
                .checked_add(offset)
                .ok_or_else(past)?;
            let end = start.checked_add(size).ok_or_else(past)?;
            if start < HELD_REPLY_FIXED {
                return Err(malformed("a reference inside the fixed attributes"));
            }
            entries = filesec_entries(reply.get(start..end).ok_or_else(past)?)?;
        }
    }
    assert!(entries.len() <= 128);
    assert!((HELD_REPLY_FIXED..=ENTRY_REPLY_BYTES).contains(&length));
    Ok(HeldEntry {
        device,
        inode: u64::from_ne_bytes(inode),
        changed: (i64::from_ne_bytes(seconds), i64::from_ne_bytes(nanos)),
        kind,
        owner,
        group,
        mode,
        entries,
    })
}

#[cfg(test)]
#[path = "held_tests.rs"]
mod tests;
