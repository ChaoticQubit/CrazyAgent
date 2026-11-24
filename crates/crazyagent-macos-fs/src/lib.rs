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

/// A process's unique id and the unique id of the process it was forked from, as the kernel
/// keeps them: unique ids are never reused, and the second stays the forking parent's after
/// that parent exits and the process is handed to launchd.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIds {
    pub unique: u64,
    pub forked_from: u64,
}

/// The `proc_pidinfo` flavor that reads a process's unique ids, `PROC_PIDUNIQIDENTIFIERINFO`
/// in xnu's private proc_info header; the macOS SDK leaves it out.
#[cfg(target_os = "macos")]
const PID_UNIQUE_IDS: libc::c_int = 17;

/// The kernel's `proc_uniqidentifierinfo`: the main program's UUID, the unique id, the forking
/// parent's unique id, the pid version, the forking parent's pid version and two reserved words.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct UniqueIdsInfo {
    _program: [u8; 16],
    unique: u64,
    forked_from: u64,
    _version: i32,
    _forked_from_version: i32,
    _reserved: [u64; 2],
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<UniqueIdsInfo>() == 56);

/// Reads a process's [`ProcessIds`] via `proc_pidinfo`. A process that ended, one of another
/// user, or an answer that is not a whole record with a forking parent older than the process
/// reads as none. Safety contract of its one unsafe call: `info` is a local of exactly the
/// size passed, so the kernel writes inside it, and it outlives the call. Input: the process
/// id. Output: its ids, or none.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn process_ids(pid: i32) -> Option<ProcessIds> {
    let mut info = UniqueIdsInfo::default();
    let size = libc::c_int::try_from(std::mem::size_of::<UniqueIdsInfo>()).ok()?;
    let read = unsafe { libc::proc_pidinfo(pid, PID_UNIQUE_IDS, 0, (&raw mut info).cast(), size) };
    if read != size || info.unique == 0 || info.forked_from >= info.unique {
        return None;
    }
    Some(ProcessIds {
        unique: info.unique,
        forked_from: info.forked_from,
    })
}

/// The `proc_pidfdinfo` flavor that reads one socket descriptor's `socket_fdinfo`,
/// `PROC_PIDFDSOCKETINFO` in the SDK's sys/proc_info.h; libc 0.2.191 leaves it out.
#[cfg(target_os = "macos")]
const FD_SOCKET_INFO: libc::c_int = 3;

/// The size of the SDK's `socket_fdinfo`, the same on arm64 and x86_64 (SDK 27.2).
#[cfg(target_os = "macos")]
const SOCKET_RECORD_SIZE: usize = 792;

/// Where `psi.soi_kind` sits in a `socket_fdinfo`: which protocol record the union holds.
#[cfg(target_os = "macos")]
const AT_KIND: usize = 256;

/// Where `psi.soi_proto.pri_tcp.tcpsi_ini.insi_lport` sits: the local port, in network
/// byte order, in the low half of an int.
#[cfg(target_os = "macos")]
const AT_LOCAL_PORT: usize = 268;

/// Where the one-byte `insi_vflag` sits: [`INI_IPV4`] and [`INI_IPV6`].
#[cfg(target_os = "macos")]
const AT_FLAGS: usize = 288;

/// Where the 16-byte IPv6 local address `insi_laddr.ina_6` sits.
#[cfg(target_os = "macos")]
const AT_ADDRESS_V6: usize = 312;

/// Where the 4-byte IPv4 local address `insi_laddr.ina_46.i46a_addr4` sits.
#[cfg(target_os = "macos")]
const AT_ADDRESS_V4: usize = 324;

/// Where `psi.soi_proto.pri_tcp.tcpsi_state` sits: the TCP state.
#[cfg(target_os = "macos")]
const AT_STATE: usize = 344;

/// `SOCKINFO_TCP`: the `soi_kind` of a TCP socket.
#[cfg(target_os = "macos")]
const KIND_TCP: i32 = 2;

/// `TSI_S_LISTEN`: the `tcpsi_state` of a listening socket.
#[cfg(target_os = "macos")]
const STATE_LISTEN: i32 = 1;

/// `INI_IPV4`: the socket's address is the IPv4 one, also on an IPv6 socket that takes IPv4.
#[cfg(target_os = "macos")]
const INI_IPV4: u8 = 1;

/// `INI_IPV6`: the socket's address is the IPv6 one.
#[cfg(target_os = "macos")]
const INI_IPV6: u8 = 2;

/// One `socket_fdinfo` as the kernel writes it, read at the fixed SDK offsets above. libc has
/// no type for it, and only seven fields are read. The size, alignment and offsets are
/// checked here at build time and against the SDK header by a test that builds a C probe.
#[cfg(target_os = "macos")]
#[repr(C, align(8))]
struct SocketRecord([u8; SOCKET_RECORD_SIZE]);

#[cfg(target_os = "macos")]
const _: () = assert!(
    std::mem::size_of::<SocketRecord>() == SOCKET_RECORD_SIZE
        && std::mem::align_of::<SocketRecord>() == 8
);

#[cfg(target_os = "macos")]
const _: () = assert!(
    AT_KIND + 4 <= AT_LOCAL_PORT
        && AT_LOCAL_PORT + 4 <= AT_FLAGS
        && AT_FLAGS < AT_ADDRESS_V6
        && AT_ADDRESS_V6 + 16 <= AT_STATE
        && AT_ADDRESS_V4 + 4 <= AT_ADDRESS_V6 + 16
        && AT_STATE + 4 <= SOCKET_RECORD_SIZE
);

#[cfg(target_os = "macos")]
impl SocketRecord {
    /// Reads the native-endian int at `at`. Input: the offset. Output: the value.
    fn int(&self, at: usize) -> i32 {
        let mut word = [0; 4];
        word.copy_from_slice(&self.0[at..at + 4]);
        i32::from_ne_bytes(word)
    }

    /// Reads the record as a TCP listener on `port`. Inputs: the port. Output: its local
    /// address when it is a TCP socket in LISTEN bound to that port, IPv4 first as lsof reads
    /// a socket that takes both; none for any other socket; `InvalidData` for a listener there
    /// with neither address flag, so the caller fails closed.
    fn listening_on(&self, port: u16) -> std::io::Result<Option<std::net::IpAddr>> {
        assert_ne!(port, 0);
        if self.int(AT_KIND) != KIND_TCP || self.int(AT_STATE) != STATE_LISTEN {
            return Ok(None);
        }
        let low = u16::try_from(self.int(AT_LOCAL_PORT) & 0xffff).expect("masked to 16 bits");
        if u16::from_be(low) != port {
            return Ok(None);
        }
        let flags = self.0[AT_FLAGS];
        if flags & INI_IPV4 != 0 {
            let mut address = [0; 4];
            address.copy_from_slice(&self.0[AT_ADDRESS_V4..AT_ADDRESS_V4 + 4]);
            return Ok(Some(std::net::Ipv4Addr::from(address).into()));
        }
        if flags & INI_IPV6 != 0 {
            let mut address = [0; 16];
            address.copy_from_slice(&self.0[AT_ADDRESS_V6..AT_ADDRESS_V6 + 16]);
            return Ok(Some(std::net::Ipv6Addr::from(address).into()));
        }
        Err(std::io::Error::from(std::io::ErrorKind::InvalidData))
    }
}

/// How many times a list the kernel fills is asked for again, with more room, while it
/// comes back full.
#[cfg(target_os = "macos")]
const LIST_TRIES: usize = 4;

/// Room for this many entries past the kernel's own count, for processes or descriptors
/// that appear between the count and the read.
#[cfg(target_os = "macos")]
const LIST_SLACK: usize = 64;

/// The most process ids or descriptors one list may hold.
#[cfg(target_os = "macos")]
const LIST_MAX: usize = 1 << 20;

/// Lists every process id via `proc_listallpids`, asked again with more room while the answer
/// fills it. Safety contract of its two unsafe calls: the first passes no buffer, so nothing
/// is written; the second passes `pids`, exactly the byte size given, which outlives the call.
/// Output: the ids, the system error, or `Other` when the list kept growing.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn all_pids() -> std::io::Result<Vec<libc::pid_t>> {
    let counted = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let counted = usize::try_from(counted).map_err(|_| std::io::Error::last_os_error())?;
    let mut room = counted + LIST_SLACK;
    for _ in 0..LIST_TRIES {
        if room > LIST_MAX {
            break;
        }
        let mut pids: Vec<libc::pid_t> = vec![0; room];
        let bytes = libc::c_int::try_from(room * std::mem::size_of::<libc::pid_t>())
            .expect("bounded by LIST_MAX");
        let listed = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        let listed = usize::try_from(listed).map_err(|_| std::io::Error::last_os_error())?;
        assert!(listed <= room);
        if listed == 0 {
            return Err(std::io::Error::other("no process listed"));
        }
        if listed < room {
            pids.truncate(listed);
            return Ok(pids);
        }
        room *= 2;
    }
    Err(std::io::Error::other("the process list kept growing"))
}

/// Lists one process's socket descriptors via `proc_pidinfo` with `PROC_PIDLISTFDS`, asked
/// again with more room while the answer fills it; a process that ended or that this user may
/// not read has none, as lsof shows none. Safety contract of its two unsafe calls: the first
/// passes no buffer; the second passes `fds`, exactly the byte size given, which outlives the
/// call. Input: the process id. Output: the descriptors, none, or `Other` past the bound.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn socket_fds(pid: libc::pid_t) -> std::io::Result<Option<Vec<i32>>> {
    let entry = std::mem::size_of::<libc::proc_fdinfo>();
    let counted =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if counted <= 0 {
        return Ok(None);
    }
    let mut room = usize::try_from(counted).expect("positive") / entry + LIST_SLACK;
    let socket = u32::try_from(libc::PROX_FDTYPE_SOCKET).expect("a small constant");
    for _ in 0..LIST_TRIES {
        if room > LIST_MAX {
            break;
        }
        let empty = libc::proc_fdinfo {
            proc_fd: 0,
            proc_fdtype: 0,
        };
        let mut fds = vec![empty; room];
        let bytes = libc::c_int::try_from(room * entry).expect("bounded by LIST_MAX");
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                fds.as_mut_ptr().cast(),
                bytes,
            )
        };
        if read <= 0 {
            return Ok(None);
        }
        let read = usize::try_from(read).expect("positive");
        assert!(read.is_multiple_of(entry) && read <= room * entry);
        if read < room * entry {
            fds.truncate(read / entry);
            let sockets = fds.iter().filter(|fd| fd.proc_fdtype == socket);
            return Ok(Some(sockets.map(|fd| fd.proc_fd).collect()));
        }
        room *= 2;
    }
    Err(std::io::Error::other("the descriptor list kept growing"))
}

/// Reads one socket descriptor's record via `proc_pidfdinfo` with `PROC_PIDFDSOCKETINFO`; a
/// descriptor closed or reused for something else since it was listed reads as none. Safety
/// contract of its one unsafe call: `record` is a local of exactly the size passed, aligned as
/// the kernel's struct, and outlives the call. Inputs: the process and descriptor. Output: the
/// record, or none.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn socket_record(pid: libc::pid_t, fd: i32) -> Option<SocketRecord> {
    let mut record = SocketRecord([0; SOCKET_RECORD_SIZE]);
    let size = libc::c_int::try_from(SOCKET_RECORD_SIZE).expect("792 fits an int");
    let read =
        unsafe { libc::proc_pidfdinfo(pid, fd, FD_SOCKET_INFO, (&raw mut record).cast(), size) };
    (read == size).then_some(record)
}

/// Lists every TCP listener on `port` held by a process this user may read, as `lsof -iTCP:P
/// -sTCP:LISTEN` does with no process spawned: every process and socket descriptor, so each
/// process sharing one listening socket is named (rv-sa241 LEAD-2); what cannot be read is
/// skipped, as lsof skips it. Input: the port. Output: each holder's id with the listener's
/// local address, or an error, `InvalidData` for a listener with no address family.
#[cfg(target_os = "macos")]
pub fn tcp_listeners(port: u16) -> std::io::Result<Vec<(i32, std::net::IpAddr)>> {
    assert_ne!(port, 0);
    let mut found = Vec::new();
    for pid in all_pids()? {
        if pid <= 0 {
            continue;
        }
        let Some(fds) = socket_fds(pid)? else {
            continue;
        };
        for fd in fds {
            let Some(record) = socket_record(pid, fd) else {
                continue;
            };
            if let Some(address) = record.listening_on(port)? {
                found.push((pid, address));
            }
        }
    }
    assert!(found.iter().all(|(pid, _)| *pid > 0));
    Ok(found)
}

/// The largest `kauth_filesec` an entry can carry: its 44-byte head and
/// `KAUTH_ACL_MAX_ENTRIES`, 128, entries of 24 bytes.
#[cfg(target_os = "macos")]
const FILESEC_BYTES_MAX: usize = 44 + 128 * ACCESS_ENTRY_BYTES;

/// The bytes of one `kauth_ace`: the principal's GUID, its flags and its rights.
#[cfg(target_os = "macos")]
const ACCESS_ENTRY_BYTES: usize = 24;

/// Where the fixed attributes of an entry reply end and the `kauth_filesec` may start: the
/// length, the returned set, the device, the change time, the filesec reference and the
/// file id, each packed on a 4-byte boundary.
#[cfg(target_os = "macos")]
const ENTRY_REPLY_FIXED: usize = 60;

/// The buffer an entry reply is read into, room for the fixed attributes and the largest
/// filesec.
#[cfg(target_os = "macos")]
const ENTRY_REPLY_BYTES: usize = 4096;

#[cfg(target_os = "macos")]
const _: () = assert!(ENTRY_REPLY_FIXED + FILESEC_BYTES_MAX <= ENTRY_REPLY_BYTES);

/// Where the filesec reference sits in an entry reply; its offset counts from here.
#[cfg(target_os = "macos")]
const ENTRY_REPLY_SECURITY_AT: usize = 44;

/// The common attributes every entry reply must return: the device, the change time and the
/// file id.
#[cfg(target_os = "macos")]
const ENTRY_REQUIRED: libc::attrgroup_t =
    libc::ATTR_CMN_DEVID | libc::ATTR_CMN_CHGTIME | libc::ATTR_CMN_FILEID;

/// The bytes of a volume reply: the length, the device and the volume capabilities.
#[cfg(target_os = "macos")]
const VOLUME_REPLY_BYTES: usize = 4 + 4 + 32;

/// One entry of an extended access list as the kernel keeps it: the principal's GUID, the
/// `KAUTH_ACE_*` flags, whose low four bits name allow or deny, and the `KAUTH_VNODE_*`
/// rights.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessEntry {
    pub principal: [u8; 16],
    pub flags: u32,
    pub rights: u32,
}

/// An entry's extended access list with the identity of the object it was read from: the
/// device, the file id and the change time in seconds and nanoseconds, all from the one call
/// that read the list.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessList {
    pub device: u64,
    pub inode: u64,
    pub changed: (i64, i64),
    pub entries: Vec<AccessEntry>,
}

/// Reads one entry's extended access list with `getattrlist` and `FSOPT_NOFOLLOW`, so a
/// link's own list is read and never its target's, and the list comes with the device, file
/// id and change time of the object it was read from. A second `getattrlist` of the same path,
/// also not following a link, must name a volume that keeps access lists, since an entry on a
/// volume without them returns none, as an entry with no list does, and the entry's own
/// device or, as the sealed system volume reads, the device pair `/` itself reads
/// ([`root_pair`]). No right on the entry itself is needed, only search on the folders above
/// it. Input: the path. Output: the list, or an error for a call that failed, a reply cut short
/// or out of shape, or a volume without access lists or another device.
#[cfg(target_os = "macos")]
pub fn access_list(path: &std::ffi::CStr) -> std::io::Result<AccessList> {
    let read = entry_read(path)?;
    let volume = volume_read(path)?;
    if !volume_keeps_lists(&volume, read.device, root_pair()) {
        return Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
    }
    Ok(read)
}

/// The first call of [`access_list`]: the entry's access list, device, file id and change
/// time, read without following a link. Safety contract of its unsafe call: the path is a
/// valid C string, and the attribute list and reply buffer are locals that outlive the call,
/// whose length is passed as the buffer's own. Input: the path. Output: the list, or an error
/// for a call that failed or a reply cut short or out of shape.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn entry_read(path: &std::ffi::CStr) -> std::io::Result<AccessList> {
    let mut list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ENTRY_REQUIRED
            | libc::ATTR_CMN_EXTENDED_SECURITY,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = [0u8; ENTRY_REPLY_BYTES];
    let done = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut list).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            libc::FSOPT_NOFOLLOW | libc::FSOPT_PACK_INVAL_ATTRS,
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error());
    }
    entry_reply(&reply)
}

/// The second call of [`access_list`]: the device and capabilities of the volume the path
/// sits on, read without following a link. Safety contract of its unsafe call: the path is a
/// valid C string, and the attribute list and reply buffer are locals that outlive the call,
/// whose length is passed as the buffer's own. Input: the path. Output: the raw reply, or the
/// error of a call that failed.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn volume_read(path: &std::ffi::CStr) -> std::io::Result<[u8; VOLUME_REPLY_BYTES]> {
    let mut volume = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_DEVID,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_CAPABILITIES,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = [0u8; VOLUME_REPLY_BYTES];
    let done = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&raw mut volume).cast(),
            reply.as_mut_ptr().cast(),
            reply.len(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if done != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(reply)
}

/// The device `/` itself reads in an entry call and in a volume call, read once. On a sealed
/// system volume the two differ, and every entry there reads that same pair; elsewhere they
/// are equal and the pair adds nothing. It is read from `/` only, never from a path under
/// test. Output: the pair, or none when either read of `/` failed or came back out of shape,
/// so no mismatch is ever let through.
#[cfg(target_os = "macos")]
fn root_pair() -> Option<(u64, u64)> {
    static PAIR: std::sync::OnceLock<Option<(u64, u64)>> = std::sync::OnceLock::new();
    *PAIR.get_or_init(|| {
        let entry = entry_read(c"/").ok()?;
        let volume = volume_read(c"/").ok()?;
        if u32_at(&volume, 0)? as usize != VOLUME_REPLY_BYTES {
            return None;
        }
        Some((entry.device, device_at(&volume, 4)?))
    })
}

/// The native-endian `u32` at `at`, if the bytes hold it. Inputs: the bytes and the offset.
/// Output: the value.
#[cfg(target_os = "macos")]
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_ne_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// The native-endian 8 bytes at `at`, if the bytes hold them. Inputs: the bytes and the
/// offset. Output: the bytes.
#[cfg(target_os = "macos")]
fn eight_at(bytes: &[u8], at: usize) -> Option<[u8; 8]> {
    let end = at.checked_add(8)?;
    bytes.get(at..end)?.try_into().ok()
}

/// The `dev_t` at `at`, widened as `st_dev` is widened into a `u64`, sign and all, so it
/// equals what `lstat` reads of the same device. Inputs: the bytes and the offset. Output:
/// the device.
#[cfg(target_os = "macos")]
fn device_at(bytes: &[u8], at: usize) -> Option<u64> {
    let device = i32::from_ne_bytes(u32_at(bytes, at)?.to_ne_bytes());
    Some(u64::from_ne_bytes(i64::from(device).to_ne_bytes()))
}

/// The error of a reply that is cut short or out of shape. Input: what is wrong. Output: the
/// error.
#[cfg(target_os = "macos")]
fn malformed(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_owned())
}

/// Reads an entry reply of `getattrlist` packed with `FSOPT_PACK_INVAL_ATTRS`: the length
/// the kernel needed, which must fit the reply, the returned set, which must hold the device,
/// change time and file id, then those and the filesec reference, whose bytes must lie
/// inside the length. A filesec that is not returned is an empty list. Input: the reply.
/// Output: the list, or an error for a reply cut short or out of shape.
#[cfg(target_os = "macos")]
fn entry_reply(reply: &[u8]) -> std::io::Result<AccessList> {
    let length = u32_at(reply, 0).ok_or_else(|| malformed("no length"))? as usize;
    if length < ENTRY_REPLY_FIXED || length > reply.len() {
        return Err(malformed("the reply is cut short"));
    }
    let reply = &reply[..length];
    let returned = u32_at(reply, 4).ok_or_else(|| malformed("no returned set"))?;
    if returned & ENTRY_REQUIRED != ENTRY_REQUIRED {
        return Err(malformed("the identity was not returned"));
    }
    let device = device_at(reply, 24).ok_or_else(|| malformed("no device"))?;
    let seconds = eight_at(reply, 28).ok_or_else(|| malformed("no change time"))?;
    let nanos = eight_at(reply, 36).ok_or_else(|| malformed("no change time"))?;
    let inode = eight_at(reply, 52).ok_or_else(|| malformed("no file id"))?;
    let mut entries = Vec::new();
    if returned & libc::ATTR_CMN_EXTENDED_SECURITY != 0 {
        let past = || malformed("a reference past the reply");
        let offset = u32_at(reply, ENTRY_REPLY_SECURITY_AT).ok_or_else(past)?;
        let size = u32_at(reply, ENTRY_REPLY_SECURITY_AT + 4).ok_or_else(past)? as usize;
        if size > 0 {
            let offset = usize::try_from(i32::from_ne_bytes(offset.to_ne_bytes()))
                .map_err(|_| malformed("a negative reference"))?;
            let start = ENTRY_REPLY_SECURITY_AT
                .checked_add(offset)
                .ok_or_else(past)?;
            let end = start.checked_add(size).ok_or_else(past)?;
            entries = filesec_entries(reply.get(start..end).ok_or_else(past)?)?;
        }
    }
    assert!(entries.len() <= 128);
    Ok(AccessList {
        device,
        inode: u64::from_ne_bytes(inode),
        changed: (i64::from_ne_bytes(seconds), i64::from_ne_bytes(nanos)),
        entries,
    })
}

/// Reads a `kauth_filesec`: the magic number, owner and group GUIDs, the entry count and the
/// ACL flags, then exactly that many `kauth_ace` entries. A count of `KAUTH_FILESEC_NOACL`
/// is an empty list. Input: the filesec's bytes. Output: the entries, or an error for a wrong
/// magic number, a count past `KAUTH_ACL_MAX_ENTRIES` or a size that does not hold exactly
/// the counted entries.
#[cfg(target_os = "macos")]
fn filesec_entries(filesec: &[u8]) -> std::io::Result<Vec<AccessEntry>> {
    let head = FILESEC_LENGTH as usize;
    if u32_at(filesec, 0) != Some(FILESEC_MAGIC) {
        return Err(malformed("not a filesec"));
    }
    let count = u32_at(filesec, 36).ok_or_else(|| malformed("no entry count"))?;
    if count == FILESEC_NOACL {
        return if filesec.len() == head {
            Ok(Vec::new())
        } else {
            Err(malformed("a filesec with no list carries entries"))
        };
    }
    let count = count as usize;
    if count > 128 || filesec.len() != head + count * ACCESS_ENTRY_BYTES {
        return Err(malformed("the entry count does not match the filesec"));
    }
    let (whole, rest) = filesec[head..].as_chunks::<ACCESS_ENTRY_BYTES>();
    assert!(
        rest.is_empty(),
        "the length check leaves only whole entries"
    );
    let entries: Vec<AccessEntry> = whole
        .iter()
        .map(|entry| AccessEntry {
            principal: entry[..16]
                .try_into()
                .expect("a 24-byte entry holds a GUID"),
            flags: u32::from_ne_bytes(entry[16..20].try_into().expect("a flag word")),
            rights: u32::from_ne_bytes(entry[20..24].try_into().expect("a rights word")),
        })
        .collect();
    assert_eq!(entries.len(), count);
    Ok(entries)
}

/// Whether a volume reply of `getattrlist` names the entry's device, or the one other device
/// `/` itself pairs with it ([`root_pair`]), and a volume that keeps extended access lists:
/// the length must be the reply's, and the capability both valid and set. Any other device
/// reads as not keeping them. Inputs: the reply, the device the entry was read on, and the
/// root's pair, none when it could not be read. Output: the answer, false for a reply out of
/// shape.
#[cfg(target_os = "macos")]
fn volume_keeps_lists(reply: &[u8], device: u64, root: Option<(u64, u64)>) -> bool {
    let interfaces = libc::VOL_CAPABILITIES_INTERFACES * 4;
    let (Some(length), Some(on), Some(set), Some(valid)) = (
        u32_at(reply, 0),
        device_at(reply, 4),
        u32_at(reply, 8 + interfaces),
        u32_at(reply, 8 + 16 + interfaces),
    ) else {
        return false;
    };
    length as usize == VOLUME_REPLY_BYTES
        && (on == device || root == Some((device, on)))
        && set & valid & libc::VOL_CAP_INT_EXTENDED_SECURITY != 0
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn mbr_uid_to_uuid(uid: libc::uid_t, uuid: *mut u8) -> std::ffi::c_int;
    fn mbr_gid_to_uuid(gid: libc::gid_t, uuid: *mut u8) -> std::ffi::c_int;
}

/// The GUID access lists name a user by, from `mbr_uid_to_uuid`. Safety contract of its
/// unsafe call: the output is a local 16-byte buffer, the size of a `uuid_t`. Input: the user
/// id. Output: the GUID, or the error the lookup returned.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn user_guid(uid: u32) -> std::io::Result<[u8; 16]> {
    let mut guid = [0u8; 16];
    let failed = unsafe { mbr_uid_to_uuid(uid, guid.as_mut_ptr()) };
    match failed {
        0 => Ok(guid),
        error => Err(std::io::Error::from_raw_os_error(error)),
    }
}

/// The GUID access lists name a group by, from `mbr_gid_to_uuid`. Safety contract of its
/// unsafe call: the output is a local 16-byte buffer, the size of a `uuid_t`. Input: the
/// group id. Output: the GUID, or the error the lookup returned.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn group_guid(gid: u32) -> std::io::Result<[u8; 16]> {
    let mut guid = [0u8; 16];
    let failed = unsafe { mbr_gid_to_uuid(gid, guid.as_mut_ptr()) };
    match failed {
        0 => Ok(guid),
        error => Err(std::io::Error::from_raw_os_error(error)),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{
        AT_ADDRESS_V4, AT_ADDRESS_V6, AT_FLAGS, AT_KIND, AT_LOCAL_PORT, AT_STATE, FD_SOCKET_INFO,
        INI_IPV4, INI_IPV6, KIND_TCP, SOCKET_RECORD_SIZE, STATE_LISTEN, SocketRecord, process_ids,
        tcp_listeners,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    /// A fresh folder under the system temp folder, removed by the caller. Input: a tag.
    /// Output: the folder.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let folder = std::env::temp_dir().join(format!(
            "macos-fs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        folder
    }

    /// The path as a C string. Input: the path. Output: the string.
    fn c_path(path: &std::path::Path) -> std::ffi::CString {
        std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap()
    }

    /// Runs `/bin/chmod` with the arguments on the entry and requires it to succeed. Inputs:
    /// the arguments and the entry.
    fn chmod(args: &[&str], entry: &std::path::Path) {
        let status = std::process::Command::new("/bin/chmod")
            .args(args)
            .arg(entry)
            .status()
            .unwrap();
        assert!(status.success(), "chmod {args:?} {entry:?}");
    }

    /// The GUID `uuid` text names, as its 16 bytes. Input: the text. Output: the bytes.
    fn guid(text: &str) -> [u8; 16] {
        let hex: String = text.chars().filter(|c| *c != '-').collect();
        assert_eq!(hex.len(), 32);
        let mut bytes = [0u8; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        bytes
    }

    /// A list is read with the device and file id `lstat` reads of the same entry: none for
    /// a file without one, the entry `chmod +a` adds, with its principal's GUID, allow kind and
    /// write right, still read once the file's mode is 000, and an error for a file whose
    /// folder denies search.
    #[test]
    fn an_access_list_is_read_with_the_identity_of_the_object_read() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let folder = scratch("acl");
        let file = folder.join("file");
        std::fs::write(&file, "x").unwrap();
        let read = super::access_list(&c_path(&file)).unwrap();
        let stat = std::fs::symlink_metadata(&file).unwrap();
        assert_eq!((read.device, read.inode), (stat.dev(), stat.ino()));
        assert_eq!(read.changed, (stat.ctime(), stat.ctime_nsec()));
        assert!(read.entries.is_empty());
        chmod(&["+a", "group:everyone allow write"], &file);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        let read = super::access_list(&c_path(&file)).unwrap();
        assert_eq!(read.inode, stat.ino());
        assert_eq!(
            read.entries,
            vec![super::AccessEntry {
                principal: guid("ABCDEFAB-CDEF-ABCD-EFAB-CDEF0000000C"),
                flags: 1,
                rights: 1 << 2,
            }]
        );
        let shut = folder.join("shut");
        std::fs::create_dir(&shut).unwrap();
        std::fs::write(shut.join("inside"), "x").unwrap();
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
        let unread = super::access_list(&c_path(&shut.join("inside")));
        std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(
            unread.map_err(|error| error.raw_os_error()),
            Err(Some(libc::EACCES))
        );
    }

    /// A link's own list is read, with the link's own file id, and never its target's: a
    /// target with a list behind a link without one reads empty, and a link given its own list
    /// with `chmod -h` reads that list.
    #[test]
    fn a_link_s_own_list_is_read_and_never_its_target_s() {
        use std::os::unix::fs::MetadataExt;
        let folder = scratch("acl-link");
        let target = folder.join("target");
        std::fs::write(&target, "x").unwrap();
        chmod(&["+a", "group:everyone allow write"], &target);
        let link = folder.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let read = super::access_list(&c_path(&link)).unwrap();
        assert_eq!(read.inode, std::fs::symlink_metadata(&link).unwrap().ino());
        assert_ne!(read.inode, std::fs::metadata(&target).unwrap().ino());
        assert!(read.entries.is_empty(), "{read:?}");
        chmod(&["-h", "+a", "group:staff allow delete"], &link);
        let read = super::access_list(&c_path(&link)).unwrap();
        assert_eq!(
            read.entries,
            vec![super::AccessEntry {
                principal: guid("ABCDEFAB-CDEF-ABCD-EFAB-CDEF00000014"),
                flags: 1,
                rights: 1 << 4,
            }]
        );
        let target_read = super::access_list(&c_path(&target)).unwrap();
        assert_eq!(target_read.entries[0].rights, 1 << 2);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    /// A `kauth_filesec` with the magic number, a 44-byte head and the given count field,
    /// then the entries. Inputs: the count field and the entries. Output: the bytes.
    fn filesec(count: u32, entries: &[([u8; 16], u32, u32)]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x012c_c16du32.to_ne_bytes());
        bytes.extend_from_slice(&[0u8; 32]);
        bytes.extend_from_slice(&count.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        for (principal, flags, rights) in entries {
            bytes.extend_from_slice(principal);
            bytes.extend_from_slice(&flags.to_ne_bytes());
            bytes.extend_from_slice(&rights.to_ne_bytes());
        }
        bytes
    }

    /// A filesec reads only in its exact shape: the counted entries, none for a count of
    /// `KAUTH_FILESEC_NOACL` or zero, and an error for a wrong magic number, a head cut short,
    /// a count past 128 or a count that does not match the bytes, so a cut list is never read
    /// as a shorter one.
    #[test]
    fn a_filesec_reads_only_in_its_exact_shape() {
        let one = ([7u8; 16], 1, 1 << 2);
        let read = super::filesec_entries(&filesec(1, &[one])).unwrap();
        assert_eq!(
            read,
            vec![super::AccessEntry {
                principal: [7; 16],
                flags: 1,
                rights: 1 << 2,
            }]
        );
        assert!(super::filesec_entries(&filesec(0, &[])).unwrap().is_empty());
        assert!(
            super::filesec_entries(&filesec(u32::MAX, &[]))
                .unwrap()
                .is_empty()
        );
        let mut wrong = filesec(1, &[one]);
        wrong[0] ^= 1;
        let refused = [
            wrong,
            filesec(1, &[one])[..40].to_vec(),
            filesec(2, &[one]),
            filesec(1, &[one, one]),
            filesec(0, &[one]),
            filesec(u32::MAX, &[one]),
            filesec(129, &[one; 129]),
            filesec(1, &[one])[..60].to_vec(),
        ];
        for (index, bytes) in refused.iter().enumerate() {
            assert!(super::filesec_entries(bytes).is_err(), "case {index}");
        }
    }

    /// An entry reply packed as `getattrlist` packs it: the length field, the returned set,
    /// the device, the change time, the filesec reference and file id, then the filesec.
    /// Inputs: the length field, the returned set and the filesec, if any. Output: the bytes.
    fn entry_reply(length: Option<u32>, returned: u32, filesec: Option<&[u8]>) -> Vec<u8> {
        let mut bytes = vec![0u8; super::ENTRY_REPLY_FIXED];
        bytes[4..8].copy_from_slice(&returned.to_ne_bytes());
        bytes[24..28].copy_from_slice(&5i32.to_ne_bytes());
        bytes[28..36].copy_from_slice(&100i64.to_ne_bytes());
        bytes[36..44].copy_from_slice(&7i64.to_ne_bytes());
        bytes[52..60].copy_from_slice(&9u64.to_ne_bytes());
        if let Some(filesec) = filesec {
            let offset = i32::try_from(super::ENTRY_REPLY_FIXED - 44).unwrap();
            bytes[44..48].copy_from_slice(&offset.to_ne_bytes());
            bytes[48..52].copy_from_slice(&u32::try_from(filesec.len()).unwrap().to_ne_bytes());
            bytes.extend_from_slice(filesec);
        }
        let own = u32::try_from(bytes.len()).unwrap();
        bytes[0..4].copy_from_slice(&length.unwrap_or(own).to_ne_bytes());
        bytes
    }

    /// An entry reply reads its identity and list only when whole: a length past the reply,
    /// as a buffer too small for the list leaves it, a missing device, change time or file id,
    /// or a reference past the length is an error, never an empty list.
    #[test]
    fn an_entry_reply_reads_only_when_whole() {
        let full = 0x8000_0000 | super::ENTRY_REQUIRED | libc::ATTR_CMN_EXTENDED_SECURITY;
        let list = filesec(1, &[([3u8; 16], 1, 1 << 4)]);
        let read = super::entry_reply(&entry_reply(None, full, Some(&list))).unwrap();
        assert_eq!((read.device, read.inode, read.changed), (5, 9, (100, 7)));
        assert_eq!(read.entries.len(), 1);
        let none = super::entry_reply(&entry_reply(
            None,
            full & !libc::ATTR_CMN_EXTENDED_SECURITY,
            None,
        ));
        assert!(none.unwrap().entries.is_empty());
        let whole = entry_reply(None, full, Some(&list));
        let cut = u32::try_from(whole.len() + 1).unwrap();
        assert!(super::entry_reply(&entry_reply(Some(cut), full, Some(&list))).is_err());
        assert!(super::entry_reply(&whole[..whole.len() - 1]).is_err());
        for missing in [
            libc::ATTR_CMN_DEVID,
            libc::ATTR_CMN_CHGTIME,
            libc::ATTR_CMN_FILEID,
        ] {
            let reply = entry_reply(None, full & !missing, Some(&list));
            assert!(super::entry_reply(&reply).is_err(), "{missing:#x}");
        }
        let mut past = whole.clone();
        past[48..52].copy_from_slice(&u32::try_from(list.len() + 24).unwrap().to_ne_bytes());
        assert!(super::entry_reply(&past).is_err());
        let mut negative = whole;
        negative[44..48].copy_from_slice(&(-4i32).to_ne_bytes());
        assert!(super::entry_reply(&negative).is_err());
    }

    /// A volume reply as `getattrlist` packs it. Inputs: the length field, the device, the
    /// interface capabilities set and the ones marked valid. Output: the bytes.
    fn volume_reply(length: u32, device: i32, set: u32, valid: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; super::VOLUME_REPLY_BYTES];
        bytes[0..4].copy_from_slice(&length.to_ne_bytes());
        bytes[4..8].copy_from_slice(&device.to_ne_bytes());
        bytes[12..16].copy_from_slice(&set.to_ne_bytes());
        bytes[28..32].copy_from_slice(&valid.to_ne_bytes());
        bytes
    }

    /// A volume keeps access lists for an entry only when the volume reply names the entry's
    /// own device and its capability is both valid and set: a device that moved between the
    /// two calls, a volume without the capability or one that does not say, and a reply out
    /// of shape all read as not keeping them.
    #[test]
    fn a_volume_keeps_lists_only_on_the_entry_s_device_with_the_capability() {
        let lists = libc::VOL_CAP_INT_EXTENDED_SECURITY;
        let length = u32::try_from(super::VOLUME_REPLY_BYTES).unwrap();
        assert!(super::volume_keeps_lists(
            &volume_reply(length, 5, lists, lists),
            5,
            None
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 6, lists, lists),
            5,
            None
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 5, 0, lists),
            5,
            None
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 5, lists, 0),
            5,
            None
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length - 4, 5, lists, lists),
            5,
            None
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 5, lists, lists)[..20],
            5,
            None
        ));
        let negative = volume_reply(length, -2, lists, lists);
        assert!(super::volume_keeps_lists(&negative, u64::MAX - 1, None));
    }

    /// On a sealed system volume the volume reply names another device than the entry's, as
    /// `/` itself reads: that one pair, read from `/`, keeps lists when the capability is set,
    /// and any other device the volume reply names still reads as not keeping them.
    #[test]
    fn a_volume_keeps_lists_for_the_root_s_own_device_pair_and_no_other() {
        let lists = libc::VOL_CAP_INT_EXTENDED_SECURITY;
        let length = u32::try_from(super::VOLUME_REPLY_BYTES).unwrap();
        let root = Some((5, 9));
        assert!(super::volume_keeps_lists(
            &volume_reply(length, 9, lists, lists),
            5,
            root
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 7, lists, lists),
            5,
            root
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 9, lists, lists),
            6,
            root
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 9, 0, lists),
            5,
            root
        ));
        assert!(!super::volume_keeps_lists(
            &volume_reply(length, 9, lists, lists),
            5,
            None
        ));
    }

    /// The GUIDs of root, wheel, daemon and admin are the well-known ones, and this user's
    /// is neither of them.
    #[test]
    fn the_well_known_guids_are_read_for_root_and_the_trusted_groups() {
        assert_eq!(
            super::user_guid(0).unwrap(),
            guid("FFFFEEEE-DDDD-CCCC-BBBB-AAAA00000000")
        );
        for (gid, text) in [
            (0, "ABCDEFAB-CDEF-ABCD-EFAB-CDEF00000000"),
            (1, "ABCDEFAB-CDEF-ABCD-EFAB-CDEF00000001"),
            (80, "ABCDEFAB-CDEF-ABCD-EFAB-CDEF00000050"),
        ] {
            assert_eq!(super::group_guid(gid).unwrap(), guid(text), "{gid}");
        }
        let me = super::user_guid(own_uid()).unwrap();
        assert_ne!(me, super::user_guid(0).unwrap());
    }

    /// This process's real user id, read from `id -u`.
    /// Output: the id.
    fn own_uid() -> u32 {
        let output = std::process::Command::new("/usr/bin/id")
            .arg("-u")
            .output()
            .unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// The C probe that prints, from the SDK header itself, every size, offset and constant
    /// the listener check reads, one `name value` line each, in the order the test lists them.
    const PROBE: &str = r#"#include <libproc.h>
#include <stddef.h>
#include <stdio.h>
#include <sys/proc_info.h>
int main(void) {
    printf("size %zu\n", sizeof(struct socket_fdinfo));
    printf("align %zu\n", _Alignof(struct socket_fdinfo));
    printf("kind %zu\n", offsetof(struct socket_fdinfo, psi.soi_kind));
    printf("local_port %zu\n", offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_lport));
    printf("flags %zu\n", offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_vflag));
    printf("address_v6 %zu\n", offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_laddr.ina_6));
    printf("address_v4 %zu\n", offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_ini.insi_laddr.ina_46.i46a_addr4));
    printf("state %zu\n", offsetof(struct socket_fdinfo, psi.soi_proto.pri_tcp.tcpsi_state));
    printf("sizes %zu %zu %zu %zu\n", sizeof(((struct socket_info *)0)->soi_kind), sizeof(((struct in_sockinfo *)0)->insi_lport), sizeof(((struct in_sockinfo *)0)->insi_vflag), sizeof(((struct tcp_sockinfo *)0)->tcpsi_state));
    printf("fd_socket_info %d\n", PROC_PIDFDSOCKETINFO);
    printf("kind_tcp %d\n", SOCKINFO_TCP);
    printf("state_listen %d\n", TSI_S_LISTEN);
    printf("ini_ipv4 %d\n", INI_IPV4);
    printf("ini_ipv6 %d\n", INI_IPV6);
    printf("fd_list %d %d %zu\n", PROC_PIDLISTFDS, PROX_FDTYPE_SOCKET, sizeof(struct proc_fdinfo));
    return 0;
}
"#;

    /// A record of a TCP socket in `state` on `port` with the address flags and one local
    /// address, written where its family sits: the IPv4 bytes are the last four of the IPv6
    /// ones, as in the kernel's union. Inputs: the state, port, flags and address. Output: the
    /// record.
    fn record(state: i32, port: u16, flags: u8, address: IpAddr) -> SocketRecord {
        let mut record = SocketRecord([0; SOCKET_RECORD_SIZE]);
        record.0[AT_KIND..AT_KIND + 4].copy_from_slice(&KIND_TCP.to_ne_bytes());
        record.0[AT_STATE..AT_STATE + 4].copy_from_slice(&state.to_ne_bytes());
        record.0[AT_LOCAL_PORT..AT_LOCAL_PORT + 4]
            .copy_from_slice(&i32::from(port.to_be()).to_ne_bytes());
        record.0[AT_FLAGS] = flags;
        match address {
            IpAddr::V4(v4) => {
                record.0[AT_ADDRESS_V4..AT_ADDRESS_V4 + 4].copy_from_slice(&v4.octets());
            }
            IpAddr::V6(v6) => {
                record.0[AT_ADDRESS_V6..AT_ADDRESS_V6 + 16].copy_from_slice(&v6.octets());
            }
        }
        record
    }

    /// Every size, offset and constant the check reads equals the SDK header's: a C probe is
    /// built with `xcrun clang` against the SDK and run, and its lines must equal this
    /// crate's, so an SDK that moves a field fails here and never reads a wrong one.
    #[test]
    fn the_socket_record_layout_and_constants_are_the_sdk_headers() {
        let folder = std::env::temp_dir().join(format!(
            "crazyagent-macos-fs-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        let source = folder.join("probe.c");
        let binary = folder.join("probe");
        std::fs::write(&source, PROBE).unwrap();
        let built = std::process::Command::new("/usr/bin/xcrun")
            .args(["clang", "-Wall", "-Werror", "-o"])
            .arg(&binary)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "the probe builds against the SDK: {}",
            String::from_utf8_lossy(&built.stderr)
        );
        let ran = std::process::Command::new(&binary).output().unwrap();
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(ran.status.success(), "the probe runs");
        let printed = String::from_utf8(ran.stdout).unwrap();
        let ours = [
            format!("size {SOCKET_RECORD_SIZE}"),
            format!("align {}", std::mem::align_of::<SocketRecord>()),
            format!("kind {AT_KIND}"),
            format!("local_port {AT_LOCAL_PORT}"),
            format!("flags {AT_FLAGS}"),
            format!("address_v6 {AT_ADDRESS_V6}"),
            format!("address_v4 {AT_ADDRESS_V4}"),
            format!("state {AT_STATE}"),
            "sizes 4 4 1 4".to_owned(),
            format!("fd_socket_info {FD_SOCKET_INFO}"),
            format!("kind_tcp {KIND_TCP}"),
            format!("state_listen {STATE_LISTEN}"),
            format!("ini_ipv4 {INI_IPV4}"),
            format!("ini_ipv6 {INI_IPV6}"),
            format!(
                "fd_list {} {} {}",
                libc::PROC_PIDLISTFDS,
                libc::PROX_FDTYPE_SOCKET,
                std::mem::size_of::<libc::proc_fdinfo>()
            ),
        ];
        assert_eq!(printed.lines().collect::<Vec<_>>(), ours, "{printed}");
    }

    /// A record reads as a listener only when it is TCP, in LISTEN and on the port; a socket
    /// that takes both families reads as its IPv4 address, an IPv6 one as its IPv6 address,
    /// and a listener with neither flag fails closed instead of reading as no listener.
    #[test]
    fn a_record_is_a_listener_only_in_listen_on_the_port_and_one_with_no_family_fails_closed() {
        let port = 41_234;
        let v4 = IpAddr::from(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::from(Ipv6Addr::LOCALHOST);
        let read =
            |state, flags, address, asked| record(state, port, flags, address).listening_on(asked);
        assert_eq!(read(STATE_LISTEN, INI_IPV4, v4, port).unwrap(), Some(v4));
        assert_eq!(
            read(STATE_LISTEN, INI_IPV4 | INI_IPV6, v4, port).unwrap(),
            Some(v4),
            "a socket taking both families reads as its IPv4 address"
        );
        assert_eq!(read(STATE_LISTEN, INI_IPV6, v6, port).unwrap(), Some(v6));
        assert_eq!(read(STATE_LISTEN, INI_IPV4, v4, port + 1).unwrap(), None);
        assert_eq!(
            read(STATE_LISTEN + 3, INI_IPV4, v4, port).unwrap(),
            None,
            "an established socket on the port is no listener"
        );
        let mut udp = record(STATE_LISTEN, port, INI_IPV4, v4);
        udp.0[AT_KIND..AT_KIND + 4].copy_from_slice(&1_i32.to_ne_bytes());
        assert_eq!(udp.listening_on(port).unwrap(), None);
        assert_eq!(
            read(STATE_LISTEN, 0, v4, port).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    /// This process's listeners on one port are named with their addresses, 127.0.0.1 and
    /// [::1]; the connection the first accepted on that port is not a listener; once both
    /// close, none of this process's is named.
    #[test]
    fn this_process_listeners_are_named_and_an_accepted_or_closed_socket_is_not() {
        let own = i32::try_from(std::process::id()).unwrap();
        let mine = |port: u16| {
            let mut found: Vec<IpAddr> = tcp_listeners(port)
                .unwrap()
                .into_iter()
                .filter(|(pid, _)| *pid == own)
                .map(|(_, address)| address)
                .collect();
            found.sort_unstable();
            found
        };
        let (v4, v6, port) = (0..20)
            .find_map(|_| {
                let v4 = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
                let port = v4.local_addr().unwrap().port();
                let v6 = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).ok()?;
                Some((v4, v6, port))
            })
            .expect("a port free on both loopbacks");
        let client = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let (accepted, _) = v4.accept().unwrap();
        assert_eq!(accepted.local_addr().unwrap().port(), port);
        assert_eq!(
            mine(port),
            vec![
                IpAddr::from(Ipv4Addr::LOCALHOST),
                IpAddr::from(Ipv6Addr::LOCALHOST)
            ]
        );
        drop((accepted, client, v4, v6));
        assert_eq!(mine(port), Vec::<IpAddr>::new());
    }

    /// A running child's ids name this process as its forking parent, and an ended, reaped
    /// child reads as none.
    #[test]
    fn a_child_names_this_process_as_the_one_it_was_forked_from_and_an_ended_one_has_no_ids() {
        let own =
            process_ids(i32::try_from(std::process::id()).unwrap()).expect("this process has ids");
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let ids = process_ids(pid).expect("a running child has ids");
        assert_eq!(ids.forked_from, own.unique, "{ids:?} {own:?}");
        assert!(ids.unique > own.unique, "{ids:?} {own:?}");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(
            process_ids(pid),
            None,
            "an ended and reaped child has no ids"
        );
    }
}
