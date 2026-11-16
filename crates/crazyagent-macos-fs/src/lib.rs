#![deny(unsafe_op_in_unsafe_fn)]
#![cfg_attr(not(target_os = "macos"), forbid(unsafe_code))]
#![cfg_attr(target_os = "macos", deny(unsafe_code))]

/// Sets the whole flag word of one entry named relative to an open folder, or to the working
/// folder through its `AT_FDCWD` handle, via `setattrlistat` with `ATTR_CMN_FLAGS` and
/// `FSOPT_NOFOLLOW`: a link at the last part is changed itself, and a name of one part walks
/// no path above the folder, so a folder swapped above it is never followed. It needs
/// ownership of the entry, not leave to read it. Safety contract of its one unsafe call:
/// `parent` is borrowed, so it stays open for the call; `name` is a `CStr`, so it ends in
/// NUL; the attribute list and the 4-byte value are locals that outlive the call, and the
/// kernel only reads them. Inputs: the folder, the entry's name there and the new flags.
/// Output: Ok, or the system error.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn set_flags_at(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
    flags: u32,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_FLAGS,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut value = flags;
    let done = unsafe {
        libc::setattrlistat(
            parent.as_raw_fd(),
            name.as_ptr(),
            (&raw mut list).cast(),
            (&raw mut value).cast(),
            std::mem::size_of::<u32>(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if done == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Where the `kauth_filesec` of a [`NoAcl`] starts, counted as the kernel counts it: from the
/// start of the `attrreference_t` that points to it, so right after that reference.
#[cfg(target_os = "macos")]
const FILESEC_AT: i32 = 8;

/// The size of a `kauth_filesec` with no ACL entries: its magic number, owner and group
/// GUIDs, entry count and ACL flags.
#[cfg(target_os = "macos")]
const FILESEC_LENGTH: u32 = 44;

/// The magic number the kernel requires at the start of a `kauth_filesec` it is given.
#[cfg(target_os = "macos")]
const FILESEC_MAGIC: u32 = 0x012c_c16d;

/// The ACL entry count, `KAUTH_FILESEC_NOACL`, by which a `kauth_filesec` given to the
/// kernel removes the entry's ACL; any other count would set one.
#[cfg(target_os = "macos")]
const FILESEC_NOACL: u32 = u32::MAX;

/// The value `setattrlistat` and `fsetattrlist` are given with `ATTR_CMN_EXTENDED_SECURITY`
/// to remove an extended ACL: an `attrreference_t` naming the `kauth_filesec` right after
/// it, then that filesec with its magic number, no owner or group and the entry count
/// [`FILESEC_NOACL`]. Its layout is the kernel's, checked against those sizes at build time;
/// only the kernel reads its fields.
#[cfg(target_os = "macos")]
#[repr(C)]
#[allow(dead_code)]
struct NoAcl {
    reference: libc::attrreference_t,
    magic: u32,
    owner: [u8; 16],
    group: [u8; 16],
    entry_count: u32,
    acl_flags: u32,
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::offset_of!(NoAcl, magic) == 8 && FILESEC_AT == 8);

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<NoAcl>() == 8 + 44 && FILESEC_LENGTH == 44);

#[cfg(target_os = "macos")]
impl NoAcl {
    /// The attribute list naming `ATTR_CMN_EXTENDED_SECURITY` alone, and the value that
    /// removes the ACL. Output: both.
    fn request() -> (libc::attrlist, Self) {
        let list = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: libc::ATTR_CMN_EXTENDED_SECURITY,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: 0,
        };
        let value = Self {
            reference: libc::attrreference_t {
                attr_dataoffset: FILESEC_AT,
                attr_length: FILESEC_LENGTH,
            },
            magic: FILESEC_MAGIC,
            owner: [0; 16],
            group: [0; 16],
            entry_count: FILESEC_NOACL,
            acl_flags: 0,
        };
        (list, value)
    }
}

/// Removes the whole extended ACL of one entry named relative to an open folder, or to the
/// working folder through its `AT_FDCWD` handle, via `setattrlistat` with
/// `ATTR_CMN_EXTENDED_SECURITY` and `FSOPT_NOFOLLOW`: a link at the last part loses its own
/// ACL while its target keeps its own, and a name of one part walks no path above the folder,
/// so a folder swapped above it is never followed. It needs ownership of the entry, not leave
/// to read or search it, and is refused while the entry is immutable. Safety contract of its
/// one unsafe call: `parent` is borrowed, so it stays open for the call; `name` is a `CStr`,
/// so it ends in NUL; the attribute list and the value are locals that outlive the call, and
/// the kernel only reads them. Inputs: the folder and the entry's name there. Output: Ok, or
/// the system error.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn clear_acl_at(
    parent: std::os::fd::BorrowedFd<'_>,
    name: &std::ffi::CStr,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let (mut list, mut value) = NoAcl::request();
    let done = unsafe {
        libc::setattrlistat(
            parent.as_raw_fd(),
            name.as_ptr(),
            (&raw mut list).cast(),
            (&raw mut value).cast(),
            std::mem::size_of::<NoAcl>(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if done == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Removes the whole extended ACL of the open entry `entry` itself via `fsetattrlist` with
/// `ATTR_CMN_EXTENDED_SECURITY`, so no name is looked up and a folder whose ACL denies
/// searching it still loses that ACL. It needs ownership of the entry, not leave to read it,
/// and is refused while the entry is immutable. Safety contract of its one unsafe call:
/// `entry` is borrowed, so it stays open for the call; the attribute list and the value are
/// locals that outlive the call, and the kernel only reads them. Input: the entry's handle.
/// Output: Ok, or the system error.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn clear_acl(entry: std::os::fd::BorrowedFd<'_>) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let (mut list, mut value) = NoAcl::request();
    let done = unsafe {
        libc::fsetattrlist(
            entry.as_raw_fd(),
            (&raw mut list).cast(),
            (&raw mut value).cast(),
            std::mem::size_of::<NoAcl>(),
            0,
        )
    };
    if done == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
