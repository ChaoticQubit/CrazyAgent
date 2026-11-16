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
