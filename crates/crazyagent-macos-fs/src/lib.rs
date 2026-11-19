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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{
        AT_ADDRESS_V4, AT_ADDRESS_V6, AT_FLAGS, AT_KIND, AT_LOCAL_PORT, AT_STATE, FD_SOCKET_INFO,
        INI_IPV4, INI_IPV6, KIND_TCP, SOCKET_RECORD_SIZE, STATE_LISTEN, SocketRecord, process_ids,
        tcp_listeners,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

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
